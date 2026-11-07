#!/usr/bin/env bash
# Host Bluetooth seed on the Realtek combo (0bda:d723), the same stick as
# the tallow-iot access point. Powers that controller on and leaves it not
# discoverable and not pairable. No pairing agent is registered. Does not
# rfkill it. Refuses up unless that access point is up.
#
# Does not load a kernel LE whitelist. An empty whitelist would hide every
# advertiser from tools/ble_discover.py. secrets/iot-ble/accept_macs is the
# outbound allow list for the future controller. allow and revoke edit only
# that file.
#
# The TP-Link UB500 (2357:0604) is not selected. Leave it plugged in or
# unplug it; this script does not power it and does not require it.
#
# Idempotent. Re-running up leaves an existing allow file in place.
#
# Usage:
#   sudo ./tools/setup_iot_ble.sh
#   sudo ./tools/setup_iot_ble.sh status
#   sudo ./tools/setup_iot_ble.sh allow aa:bb:cc:dd:ee:ff
#   sudo ./tools/setup_iot_ble.sh revoke aa:bb:cc:dd:ee:ff
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$(readlink -f "${BASH_SOURCE[0]}")"
AP_SCRIPT="$(readlink -f "${ROOT}/tools/setup_iot_ap.sh")"
SECRET_DIR="${ROOT}/secrets/iot-ble"
RUN=/run/tallow

# Bind by USB id. HCI indexes move. The seed and the access point are one stick.
SEED_VID=0bda
SEED_PID=d723
AP_VID=0bda
AP_PID=d723

usage() {
  cat <<EOF
Usage: setup_iot_ble.sh <command>

  up                 power on the combo and leave it not pairable (default)
  status             show the access point, the seed, and the allow list
  allow MAC          add a MAC to secrets/iot-ble/accept_macs
  revoke MAC         remove a MAC from that file

up refuses unless the access point from tools/setup_iot_ap.sh is up on this stick.
allow and revoke only edit the allow file. They do not change BlueZ policy.

The allow list starts empty. Inbound pairing stays closed either way.
EOF
}

log() {
  printf '==> %s\n' "$*"
}

die() {
  printf 'setup_iot_ble: %s\n' "$*" >&2
  exit 1
}

need_root() {
  if [[ "${EUID}" -ne 0 ]]; then
    die "run as root: sudo ${SCRIPT} ${*:-up}"
  fi
}

require_fedora() {
  local id=""
  if [[ -r /etc/os-release ]]; then
    id="$(. /etc/os-release && printf '%s' "${ID:-}")"
  fi
  if [[ "$id" != "fedora" ]]; then
    die "This script is for Fedora (dnf)."
  fi
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing command: $1"
}

# Print "vid pid" for the USB device that owns this sysfs path.
usb_ids_from() {
  local path="$1" i
  path="$(readlink -f "$path")"
  for i in 1 2 3 4 5 6 7 8; do
    if [[ -f "${path}/idVendor" && -f "${path}/idProduct" ]]; then
      printf '%s %s\n' \
        "$(tr -d '[:space:]' <"${path}/idVendor")" \
        "$(tr -d '[:space:]' <"${path}/idProduct")"
      return 0
    fi
    [[ "$path" == "/" ]] && break
    path="$(dirname "$path")"
  done
  return 1
}

# Name of the single netdev on a USB device.
# Returns 1 when the device has no netdev. Returns 2 when it has several.
usb_net_iface() {
  local vid="$1" pid="$2" dev net base real
  local -A seen=()
  local found=()
  local shopts
  shopts="$(shopt -p nullglob || true)"
  shopt -s nullglob
  for dev in /sys/bus/usb/devices/*; do
    [[ -f "${dev}/idVendor" && -f "${dev}/idProduct" ]] || continue
    [[ "$(tr -d '[:space:]' <"${dev}/idVendor")" == "$vid" ]] || continue
    [[ "$(tr -d '[:space:]' <"${dev}/idProduct")" == "$pid" ]] || continue
    base="$(basename "$dev")"
    # Interface directories only (4-6:1.0). firmware_node, driver, and
    # subsystem are symlinks back to the same netdev, and driver/ also
    # reaches other USB devices.
    for net in "${dev}/${base}":*/net/*; do
      [[ -d "$net" ]] || continue
      real="$(readlink -f "$net")"
      [[ -n "${seen[$real]:-}" ]] && continue
      seen["$real"]=1
      found+=("$(basename "$real")")
    done
  done
  eval "$shopts"
  if [[ "${#found[@]}" -eq 0 ]]; then
    return 1
  fi
  if [[ "${#found[@]}" -gt 1 ]]; then
    printf 'setup_iot_ble: USB %s:%s has more than one netdev: %s\n' \
      "$vid" "$pid" "${found[*]}" >&2
    return 2
  fi
  printf '%s\n' "${found[0]}"
}

# Print hci names whose parent USB device matches vid:pid.
usb_hcis() {
  local vid="$1" pid="$2" h ids hv hp
  local shopts
  shopts="$(shopt -p nullglob || true)"
  shopt -s nullglob
  for h in /sys/class/bluetooth/hci*; do
    [[ -d "$h" ]] || continue
    ids="$(usb_ids_from "${h}/device")" || continue
    read -r hv hp <<<"$ids"
    if [[ "$hv" == "$vid" && "$hp" == "$pid" ]]; then
      basename "$h"
    fi
  done
  eval "$shopts"
  return 0
}

# Set $1 to the only matching hci name. Return 1 when none are present.
# Exit when several match, so the caller does not treat that as "missing".
assign_one_hci() {
  local __name="$1" vid="$2" pid="$3"
  local -a found=()
  mapfile -t found < <(usb_hcis "$vid" "$pid")
  if [[ "${#found[@]}" -gt 1 ]]; then
    die "USB ${vid}:${pid} has more than one Bluetooth controller: ${found[*]}"
  fi
  if [[ "${#found[@]}" -eq 0 ]]; then
    printf -v "$__name" '%s' ""
    return 1
  fi
  printf -v "$__name" '%s' "${found[0]}"
  return 0
}

# Print the access-point netdev. Return 1 when it is missing, 2 when it is ambiguous.
ap_iface() {
  local __value status
  set +e
  __value="$(usb_net_iface "$AP_VID" "$AP_PID")"
  status=$?
  set -e
  if [[ "$status" -eq 2 ]]; then
    return 2
  fi
  if [[ "$status" -ne 0 || -z "$__value" ]]; then
    return 1
  fi
  printf '%s\n' "$__value"
}

require_ap() {
  local live="" conf="" state="" status=0
  [[ -x "$AP_SCRIPT" ]] || die "missing ${AP_SCRIPT}"
  [[ -f /etc/systemd/system/hostapd.service.d/tallow.conf ]] || \
    die "access point is not installed. Run: sudo ${AP_SCRIPT}"
  if ! systemctl is-active --quiet hostapd; then
    die "access point is not up. Run: sudo ${AP_SCRIPT}"
  fi
  set +e
  live="$(ap_iface)"
  status=$?
  set -e
  if [[ "$status" -eq 2 ]]; then
    exit 1
  fi
  if [[ "$status" -ne 0 || -z "$live" ]]; then
    die "access point (USB ${AP_VID}:${AP_PID}) is not present"
  fi
  [[ -r /etc/tallow/hostapd.conf ]] || die "missing /etc/tallow/hostapd.conf"
  conf="$(awk -F= '$1 == "interface" { print $2; exit }' /etc/tallow/hostapd.conf | tr -d '[:space:]')"
  [[ "$live" == "$conf" ]] || die "hostapd is bound to ${conf:-nothing}, not ${live}"
  [[ -d "/sys/class/net/${live}" ]] || die "access point interface ${live} is not present"
  state="$(tr -d '[:space:]' <"/sys/class/net/${live}/operstate")"
  if [[ "$state" == "down" || -z "$state" ]]; then
    die "access point interface ${live} is down"
  fi
}

refuse_always_pairable() {
  local v=""
  [[ -f /etc/bluetooth/main.conf ]] || return 0
  v="$(awk '
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*AlwaysPairable[[:space:]]*=/ {
      val = $0
      sub(/^[^=]*=[[:space:]]*/, "", val)
      gsub(/[[:space:]]/, "", val)
      print tolower(val)
      exit
    }
  ' /etc/bluetooth/main.conf)"
  if [[ "$v" == "true" ]]; then
    die "AlwaysPairable=true in /etc/bluetooth/main.conf; inbound pairing would stay open"
  fi
}

adapter_prop() {
  local hci="$1" prop="$2" val
  val="$(busctl get-property org.bluez "/org/bluez/${hci}" org.bluez.Adapter1 "$prop" 2>/dev/null || true)"
  printf '%s\n' "${val:-missing}"
}

plain_bool() {
  case "$1" in
    "b true") printf 'true' ;;
    "b false") printf 'false' ;;
    *) printf '%s' "${1:-missing}" ;;
  esac
}

plain_string() {
  local v="$1"
  v="${v#s }"
  v="${v#\"}"
  v="${v%\"}"
  printf '%s' "$v"
}

hci_soft() {
  local hci="$1" soft
  soft="$(rfkill -r -n -o DEVICE,SOFT 2>/dev/null | awk -v h="$hci" '$1 == h { print $2; exit }' || true)"
  printf '%s\n' "${soft:-unknown}"
}

hci_hard() {
  local hci="$1" hard
  hard="$(rfkill -r -n -o DEVICE,HARD 2>/dev/null | awk -v h="$hci" '$1 == h { print $2; exit }' || true)"
  printf '%s\n' "${hard:-unknown}"
}

# Unblock one controller. Never "rfkill unblock bluetooth".
unblock_hci() {
  local hci="$1" r
  local shopts
  shopts="$(shopt -p nullglob || true)"
  shopt -s nullglob
  for r in /sys/class/rfkill/rfkill*; do
    [[ "$(tr -d '[:space:]' <"${r}/name")" == "$hci" ]] || continue
    [[ "$(tr -d '[:space:]' <"${r}/type")" == "bluetooth" ]] || continue
    if [[ "$(tr -d '[:space:]' <"${r}/hard")" == "1" ]]; then
      eval "$shopts"
      die "seed ${hci} is hard-blocked"
    fi
    printf '0\n' >"${r}/soft" || true
  done
  eval "$shopts"
  rfkill unblock "$hci" || true
}

power_seed() {
  local hci="" i powered="" disc="" pair=""
  assign_one_hci hci "$SEED_VID" "$SEED_PID" || die "seed (USB ${SEED_VID}:${SEED_PID}) is not present"
  if [[ "$(hci_hard "$hci")" == "blocked" ]]; then
    die "seed ${hci} is hard-blocked"
  fi
  for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    assign_one_hci hci "$SEED_VID" "$SEED_PID" || die "seed (USB ${SEED_VID}:${SEED_PID}) is not present"
    unblock_hci "$hci"
    # Close pairing before power-on, then again once the adapter is up.
    # This does not register a pairing agent.
    busctl set-property org.bluez "/org/bluez/${hci}" org.bluez.Adapter1 Discoverable b false || true
    busctl set-property org.bluez "/org/bluez/${hci}" org.bluez.Adapter1 Pairable b false || true
    busctl set-property org.bluez "/org/bluez/${hci}" org.bluez.Adapter1 Powered b true || true
    busctl set-property org.bluez "/org/bluez/${hci}" org.bluez.Adapter1 Discoverable b false || true
    busctl set-property org.bluez "/org/bluez/${hci}" org.bluez.Adapter1 Pairable b false || true
    sleep 0.4
    powered="$(adapter_prop "$hci" Powered)"
    disc="$(adapter_prop "$hci" Discoverable)"
    pair="$(adapter_prop "$hci" Pairable)"
    if [[ "$powered" == "b true" && "$disc" == "b false" && "$pair" == "b false" ]]; then
      log "seed ${hci} powered on, discoverable=false, pairable=false"
      return 0
    fi
  done
  die "seed ${hci} did not settle (powered=${powered:-?} discoverable=${disc:-?} pairable=${pair:-?})"
}

lock_secrets() {
  install -d -m 0755 "$RUN"
  exec 9>>"${RUN}/iot-ble.lock"
  flock 9
}

unlock_secrets() {
  flock -u 9 || true
  exec 9>&-
}

ensure_secret_dir() {
  if [[ -n "${SUDO_UID:-}" ]]; then
    install -d -m 0700 -o "$SUDO_UID" -g "${SUDO_GID:-$SUDO_UID}" "$SECRET_DIR"
  else
    install -d -m 0700 "$SECRET_DIR"
  fi
  chmod 0700 "$SECRET_DIR"
}

# stdin -> atomic replace. Caller holds the secrets lock.
write_secret() {
  local dest="$1" tmp
  tmp="$(mktemp "${SECRET_DIR}/.tmp.XXXXXX")"
  if ! cat >"$tmp"; then
    rm -f "$tmp"
    die "failed to write ${dest}"
  fi
  chmod 0600 "$tmp"
  chown --reference="$SECRET_DIR" "$tmp"
  mv -f "$tmp" "$dest"
}

ensure_allow_file() {
  ensure_secret_dir
  if [[ ! -f "${SECRET_DIR}/accept_macs" ]]; then
    printf '' | write_secret "${SECRET_DIR}/accept_macs"
    log "wrote ${SECRET_DIR}/accept_macs"
  else
    chmod 0600 "${SECRET_DIR}/accept_macs"
    chown --reference="$SECRET_DIR" "${SECRET_DIR}/accept_macs"
  fi
}

norm_mac() {
  local mac="${1,,}"
  if [[ ! "$mac" =~ ^([0-9a-f]{2}:){5}[0-9a-f]{2}$ ]]; then
    die "MAC must look like aa:bb:cc:dd:ee:ff"
  fi
  if [[ "$mac" == "00:00:00:00:00:00" || "$mac" == "ff:ff:ff:ff:ff:ff" ]]; then
    die "refusing ${mac}"
  fi
  printf '%s\n' "$mac"
}

valid_macs() {
  local src="$1"
  [[ -f "$src" ]] || return 0
  awk 'BEGIN { IGNORECASE = 1 }
    /^([0-9a-f]{2}:){5}[0-9a-f]{2}$/ { print tolower($0) }' "$src" | sort -u
}

allow_mac() {
  local mac="$1" tmp
  ensure_secret_dir
  if [[ ! -f "${SECRET_DIR}/accept_macs" ]]; then
    printf '' | write_secret "${SECRET_DIR}/accept_macs"
  fi
  if valid_macs "${SECRET_DIR}/accept_macs" | grep -qx "$mac"; then
    log "${mac} is already allowed"
    return 0
  fi
  tmp="$(mktemp)"
  {
    valid_macs "${SECRET_DIR}/accept_macs"
    printf '%s\n' "$mac"
  } | sort -u >"$tmp"
  write_secret "${SECRET_DIR}/accept_macs" <"$tmp"
  rm -f "$tmp"
  log "allowed ${mac}"
}

revoke_mac() {
  local mac="$1" tmp
  ensure_secret_dir
  [[ -f "${SECRET_DIR}/accept_macs" ]] || {
    log "${mac} was not on the allow list"
    return 0
  }
  if ! valid_macs "${SECRET_DIR}/accept_macs" | grep -qx "$mac"; then
    log "${mac} was not on the allow list"
    return 0
  fi
  tmp="$(mktemp)"
  valid_macs "${SECRET_DIR}/accept_macs" | grep -vx "$mac" >"$tmp" || true
  write_secret "${SECRET_DIR}/accept_macs" <"$tmp"
  rm -f "$tmp"
  log "revoked ${mac}"
}

cmd_up() {
  local hci=""
  need_root up
  require_fedora
  [[ "$#" -eq 0 ]] || die "up takes no arguments"
  for c in busctl systemctl rfkill awk install flock mktemp tr; do
    need_cmd "$c"
  done
  require_ap
  refuse_always_pairable
  assign_one_hci hci "$SEED_VID" "$SEED_PID" || die "seed (USB ${SEED_VID}:${SEED_PID}) is not present"
  if [[ "$(systemctl is-enabled bluetooth.service 2>/dev/null || true)" == "masked" ]]; then
    die "bluetooth.service is masked"
  fi
  systemctl enable bluetooth.service
  if ! systemctl is-active --quiet bluetooth.service; then
    systemctl start bluetooth.service
  fi
  power_seed
  lock_secrets
  ensure_allow_file
  unlock_secrets
  log "allow list: ${SECRET_DIR}/accept_macs"
  if [[ -z "$(valid_macs "${SECRET_DIR}/accept_macs")" ]]; then
    log "allow list is empty"
  fi
}

print_powered_controllers() {
  local h name ids hv hp powered n=0
  local shopts
  shopts="$(shopt -p nullglob || true)"
  shopt -s nullglob
  for h in /sys/class/bluetooth/hci*; do
    [[ -d "$h" ]] || continue
    name="$(basename "$h")"
    powered="$(adapter_prop "$name" Powered)"
    [[ "$powered" == "b true" ]] || continue
    hv="?"
    hp="?"
    ids="$(usb_ids_from "${h}/device" || true)"
    if [[ -n "$ids" ]]; then
      read -r hv hp <<<"$ids"
    fi
    printf 'powered controller: %s USB %s:%s\n' "$name" "$hv" "$hp"
    n=$((n + 1))
  done
  eval "$shopts"
  if [[ "$n" -eq 0 ]]; then
    printf 'powered controller: none\n'
  fi
}

cmd_status() {
  local hci="" live="" status=0 n=0 addr powered disc pair soft
  need_root status
  [[ "$#" -eq 0 ]] || die "status takes no arguments"
  if systemctl is-active --quiet hostapd; then
    set +e
    live="$(ap_iface)"
    status=$?
    set -e
    if [[ "$status" -eq 2 ]]; then
      printf 'AP: hostapd active, USB %s:%s has more than one netdev\n' "$AP_VID" "$AP_PID"
    elif [[ "$status" -eq 0 && -n "$live" ]]; then
      printf 'AP: active %s USB %s:%s\n' "$live" "$AP_VID" "$AP_PID"
    else
      printf 'AP: hostapd active, USB %s:%s missing\n' "$AP_VID" "$AP_PID"
    fi
  else
    printf 'AP: down\n'
  fi
  print_powered_controllers
  if assign_one_hci hci "$SEED_VID" "$SEED_PID"; then
    addr="$(plain_string "$(adapter_prop "$hci" Address)")"
    powered="$(plain_bool "$(adapter_prop "$hci" Powered)")"
    disc="$(plain_bool "$(adapter_prop "$hci" Discoverable)")"
    pair="$(plain_bool "$(adapter_prop "$hci" Pairable)")"
    soft="$(hci_soft "$hci")"
    printf 'seed: %s USB %s:%s address %s powered=%s discoverable=%s pairable=%s soft=%s\n' \
      "$hci" "$SEED_VID" "$SEED_PID" "$addr" "$powered" "$disc" "$pair" "$soft"
  else
    printf 'seed: not present (USB %s:%s)\n' "$SEED_VID" "$SEED_PID"
  fi
  if [[ -f "${SECRET_DIR}/accept_macs" ]]; then
    n="$(valid_macs "${SECRET_DIR}/accept_macs" | awk 'END { print NR + 0 }')"
    printf 'allow list (%s):\n' "$n"
    valid_macs "${SECRET_DIR}/accept_macs" | sed 's/^/  /' || true
    if [[ "$n" -eq 0 ]]; then
      printf '  (empty)\n'
    fi
  else
    printf 'allow list: missing\n'
  fi
}

cmd_allow() {
  local mac
  need_root allow
  require_fedora
  [[ "$#" -eq 1 ]] || die "usage: ${SCRIPT} allow aa:bb:cc:dd:ee:ff"
  mac="$(norm_mac "$1")"
  lock_secrets
  allow_mac "$mac"
  unlock_secrets
}

cmd_revoke() {
  local mac
  need_root revoke
  require_fedora
  [[ "$#" -eq 1 ]] || die "usage: ${SCRIPT} revoke aa:bb:cc:dd:ee:ff"
  mac="$(norm_mac "$1")"
  lock_secrets
  revoke_mac "$mac"
  unlock_secrets
}

main() {
  local cmd="${1:-up}"
  if [[ "$#" -gt 0 ]]; then
    shift
  fi
  case "$cmd" in
    -h | --help | help)
      usage
      ;;
    up) cmd_up "$@" ;;
    status) cmd_status "$@" ;;
    allow) cmd_allow "$@" ;;
    revoke) cmd_revoke "$@" ;;
    *)
      usage >&2
      exit 1
      ;;
  esac
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
