#!/usr/bin/env bash
# Isolated 2.4 GHz IoT access point on the Kootech USB stick (0bda:b812).
# The Realtek combo (0bda:d723) only surveys channels 1, 6, and 11 and writes
# advice. It does not restart hostapd. apply-channel is the retune command.
# The combo's Bluetooth controller is held off so a scan is not time-sliced.
#
# Idempotent. Re-running up does not rotate the passphrase and leaves hostapd
# running when the rendered config is unchanged.
#
# Usage:
#   sudo ./tools/setup_iot_ap.sh
#   sudo ./tools/setup_iot_ap.sh status
#   sudo ./tools/setup_iot_ap.sh allow aa:bb:cc:dd:ee:ff
#   sudo ./tools/setup_iot_ap.sh revoke aa:bb:cc:dd:ee:ff
#   sudo ./tools/setup_iot_ap.sh apply-channel
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$(readlink -f "${BASH_SOURCE[0]}")"
SECRET_DIR="${ROOT}/secrets/iot-ap"
ETC=/etc/tallow
STATE=/var/lib/tallow
RUN=/run/tallow

# Bind by USB id. Interface names and HCI indexes move.
AP_VID=0bda
AP_PID=b812
AP_DRIVER=rtw88_8822bu
COMBO_VID=0bda
COMBO_PID=d723
COMBO_DRIVER=rtw88_8723du

AP_CIDR=10.87.0.1/24
AP_ADDR=10.87.0.1
AP_NET=10.87.0.0/24
DHCP_RANGE="10.87.0.50,10.87.0.200,255.255.255.0,12h"
SSID_DEFAULT=tallow-iot

ZONE=tallow-iot
POLICY_FROM=tallow-iot-from
POLICY_TO=tallow-iot-to

CONF_CHANGED=0
ACCEPT_CHANGED=0
DNS_CHANGED=0
SCAN_ERROR=""

usage() {
  cat <<EOF
Usage: setup_iot_ap.sh <command>

  up                 bring up the isolated AP (default)
  status             show the AP, the accept list, and the latest advice
  allow MAC          add a station to the whitelist and reload hostapd
  revoke MAC         remove a station and reload hostapd
  apply-channel      copy the survey recommendation into the saved channel
                     and restart hostapd

SSID, passphrase, saved channel, survey advice, and accept_macs live in
secrets/iot-ap/ (directory 0700, files 0600). Re-running up does not rotate
the passphrase. The survey timer only records advice.

The accept list starts empty: the SSID is up and nothing can associate.
EOF
}

log() {
  printf '==> %s\n' "$*"
}

die() {
  printf 'setup_iot_ap: %s\n' "$*" >&2
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

refuse_lan() {
  case "$1" in
    eno1 | enp5s0f1 | br0 | lo | virbr* | docker* | br-* | tailscale* | tun* | veth* | vnet*)
      die "refusing to configure ${1}"
      ;;
  esac
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
    printf 'setup_iot_ap: USB %s:%s has more than one netdev: %s\n' \
      "$vid" "$pid" "${found[*]}" >&2
    return 2
  fi
  printf '%s\n' "${found[0]}"
}

# Set $1 to the netdev. Exit on an ambiguous device so the caller does not
# also report it as missing.
assign_usb_iface() {
  local __name="$1" vid="$2" pid="$3" missing="$4"
  local __value status
  set +e
  __value="$(usb_net_iface "$vid" "$pid")"
  status=$?
  set -e
  if [[ "$status" -eq 2 ]]; then
    exit 1
  fi
  if [[ "$status" -ne 0 || -z "$__value" ]]; then
    die "$missing"
  fi
  # $__name is the caller's variable. A local of that name would hide it.
  printf -v "$__name" '%s' "$__value"
}

usb_hci() {
  local vid="$1" pid="$2" h ids hv hp
  local shopts
  shopts="$(shopt -p nullglob || true)"
  shopt -s nullglob
  for h in /sys/class/bluetooth/hci*; do
    [[ -d "$h" ]] || continue
    ids="$(usb_ids_from "${h}/device")" || continue
    read -r hv hp <<<"$ids"
    if [[ "$hv" == "$vid" && "$hp" == "$pid" ]]; then
      eval "$shopts"
      basename "$h"
      return 0
    fi
  done
  eval "$shopts"
  return 1
}

driver_of() {
  local iface="$1" link
  link="$(readlink -f "/sys/class/net/${iface}/device/driver" 2>/dev/null || true)"
  [[ -n "$link" ]] || return 1
  basename "$link"
}

lock_secrets() {
  install -d -m 0755 "$RUN"
  exec 9>>"${RUN}/secrets.lock"
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

ensure_credentials() {
  ensure_secret_dir
  if [[ ! -s "${SECRET_DIR}/ssid" ]]; then
    printf '%s\n' "$SSID_DEFAULT" | write_secret "${SECRET_DIR}/ssid"
  fi
  if [[ ! -s "${SECRET_DIR}/passphrase" ]]; then
    openssl rand -hex 16 | write_secret "${SECRET_DIR}/passphrase"
    log "wrote a new WPA2 passphrase to ${SECRET_DIR}/passphrase"
  fi
  if [[ ! -f "${SECRET_DIR}/accept_macs" ]]; then
    printf '' | write_secret "${SECRET_DIR}/accept_macs"
  fi
}

channel_is_set() {
  local c=""
  [[ -f "${SECRET_DIR}/channel" ]] || return 1
  c="$(tr -d '[:space:]' <"${SECRET_DIR}/channel")"
  [[ "$c" =~ ^(1|6|11)$ ]]
}

current_channel() {
  local c=""
  if [[ -f "${SECRET_DIR}/channel" ]]; then
    c="$(tr -d '[:space:]' <"${SECRET_DIR}/channel")"
  fi
  if [[ "$c" =~ ^(1|6|11)$ ]]; then
    printf '%s\n' "$c"
  fi
}

write_channel() {
  local channel="$1"
  [[ "$channel" =~ ^(1|6|11)$ ]] || die "refusing channel ${channel}"
  printf '%s\n' "$channel" | write_secret "${SECRET_DIR}/channel"
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

publish_accept() {
  [[ -d "$ETC" ]] || return 0
  local tmp
  tmp="$(mktemp)"
  valid_macs "${SECRET_DIR}/accept_macs" >"$tmp"
  install -m 0640 -o root -g root "$tmp" "${ETC}/hostapd.accept"
  rm -f "$tmp"
}

parse_scan() {
  awk '
    function flush() {
      if (!have) return
      if (freq >= 3000) { have = 0; return }
      ch = 0
      if (ds != "") ch = ds + 0
      else if (ht != "") ch = ht + 0
      else if (freq == 2484) ch = 14
      else if (freq >= 2412 && freq <= 2472) ch = int((freq - 2407) / 5)
      if (ch >= 1 && ch <= 14) printf "%d %.2f\n", ch, sig
      have = 0
    }
    /^BSS / {
      flush()
      ds = ""
      ht = ""
      freq = 0
      sig = -100
      have = 1
      next
    }
    $1 == "freq:" { freq = $2 + 0; next }
    $1 == "signal:" { sig = $2 + 0; next }
    /DS Parameter set:/ { ds = $NF; next }
    /primary channel:/ { ht = $NF; next }
    END { flush() }
  '
}

# Lower score is quieter. Ties prefer the saved channel, then 6, then 1, then 11.
score_records() {
  local prefer="${1:-}"
  awk -v prefer="$prefer" '
    function rank(ch,    r) {
      r = 9
      if (ch == 11) r = 3
      if (ch == 1) r = 2
      if (ch == 6) r = 1
      if (prefer != "" && ch == prefer + 0) r = 0
      return r
    }
    BEGIN { t[1] = 1; t[2] = 6; t[3] = 11 }
    {
      ch = $1 + 0
      sig = $2 + 0
      strength = sig + 100
      if (strength < 0) strength = 0
      if (strength > 100) strength = 100
      strength = int(strength + 0.5)
      for (i = 1; i <= 3; i++) {
        target = t[i]
        d = ch - target
        if (d < 0) d = -d
        if (d >= 5) continue
        score[target] += (5 - d) * strength
        overlap[target]++
      }
    }
    END {
      best = -1
      bests = -1
      for (i = 1; i <= 3; i++) {
        target = t[i]
        s = score[target] + 0
        if (best < 0 || s < bests || (s == bests && rank(target) < rank(best))) {
          best = target
          bests = s
        }
      }
      printf "recommended=%d\n", best
      printf "score_1=%d\n", score[1] + 0
      printf "score_6=%d\n", score[6] + 0
      printf "score_11=%d\n", score[11] + 0
      printf "overlap_1=%d\n", overlap[1] + 0
      printf "overlap_6=%d\n", overlap[6] + 0
      printf "overlap_11=%d\n", overlap[11] + 0
    }
  '
}

reset_scores() {
  REC=""
  SCORE_1=""
  SCORE_6=""
  SCORE_11=""
  OVERLAP_1=""
  OVERLAP_6=""
  OVERLAP_11=""
}

load_scores() {
  local line key val
  reset_scores
  while IFS= read -r line; do
    [[ "$line" == *=* ]] || continue
    key="${line%%=*}"
    val="${line#*=}"
    case "$key" in
      recommended) REC="$val" ;;
      score_1) SCORE_1="$val" ;;
      score_6) SCORE_6="$val" ;;
      score_11) SCORE_11="$val" ;;
      overlap_1) OVERLAP_1="$val" ;;
      overlap_6) OVERLAP_6="$val" ;;
      overlap_11) OVERLAP_11="$val" ;;
    esac
  done
  [[ "$REC" =~ ^(1|6|11)$ ]] || return 1
  [[ "$SCORE_1" =~ ^[0-9]+$ && "$SCORE_6" =~ ^[0-9]+$ && "$SCORE_11" =~ ^[0-9]+$ ]] || return 1
  [[ "$OVERLAP_1" =~ ^[0-9]+$ && "$OVERLAP_6" =~ ^[0-9]+$ && "$OVERLAP_11" =~ ^[0-9]+$ ]] || return 1
}

advice_field() {
  local key="$1" file="${SECRET_DIR}/advice"
  [[ -f "$file" ]] || return 1
  awk -F= -v k="$key" '$1 == k { print substr($0, length(k) + 2); exit }' "$file"
}

write_advice() {
  local scanner="$1" iface="$2" recommended="$3" note="$4"
  local saved score1 score6 score11 ov1 ov6 ov11 bss
  saved="${5:-}"
  score1="${6:-}"
  score6="${7:-}"
  score11="${8:-}"
  ov1="${9:-}"
  ov6="${10:-}"
  ov11="${11:-}"
  bss="${12:-}"
  if [[ -z "$saved" ]]; then
    saved="$(current_channel || true)"
  fi
  note="${note//$'\n'/ }"
  {
    printf 'timestamp=%s\n' "$(date -Iseconds)"
    printf 'scanner=%s\n' "$scanner"
    printf 'scanner_iface=%s\n' "$iface"
    printf 'saved_channel=%s\n' "$saved"
    printf 'recommended=%s\n' "$recommended"
    printf 'score_1=%s\n' "$score1"
    printf 'score_6=%s\n' "$score6"
    printf 'score_11=%s\n' "$score11"
    printf 'overlap_1=%s\n' "$ov1"
    printf 'overlap_6=%s\n' "$ov6"
    printf 'overlap_11=%s\n' "$ov11"
    printf 'bss_seen=%s\n' "$bss"
    printf 'note=%s\n' "$note"
  } | write_secret "${SECRET_DIR}/advice"
}

unblock_wifi() {
  local iface="$1" phy
  phy="/sys/class/net/${iface}/phy80211/name"
  [[ -r "$phy" ]] || return 0
  rfkill unblock "$(tr -d '[:space:]' <"$phy")" || true
}

ensure_managed() {
  local iface="$1" typ
  refuse_lan "$iface"
  unblock_wifi "$iface"
  typ="$(iw dev "$iface" info | awk '$1 == "type" { print $2; exit }')"
  if [[ "$typ" != "managed" ]]; then
    ip link set dev "$iface" down
    iw dev "$iface" set type managed
  fi
  ip link set dev "$iface" up
}

scan_iface() {
  local iface="$1" try err_file out
  SCAN_ERROR=""
  err_file="$(mktemp)"
  for try in 1 2 3; do
    if out="$(timeout 25 iw dev "$iface" scan 2>"$err_file")"; then
      rm -f "$err_file"
      printf '%s\n' "$out"
      return 0
    fi
    SCAN_ERROR="$(tr '\n' ' ' <"$err_file" | cut -c1-200)"
    sleep 1
  done
  rm -f "$err_file"
  return 1
}

# Survey one radio. apply=yes copies the recommendation into the saved channel.
survey_iface() {
  local iface="$1" scanner="$2" note="$3" apply="$4"
  local scan parsed scores bss saved
  ensure_managed "$iface"
  sleep 1
  if ! scan="$(scan_iface "$iface")"; then
    return 1
  fi
  parsed="$(printf '%s\n' "$scan" | parse_scan)"
  saved="$(current_channel || true)"
  scores="$(printf '%s\n' "$parsed" | score_records "$saved")"
  load_scores <<<"$scores" || return 1
  bss="$(printf '%s\n' "$scan" | awk '/^BSS / { n++ } END { print n + 0 }')"
  lock_secrets
  if [[ "$apply" == "yes" ]]; then
    write_channel "$REC"
    saved="$REC"
  else
    saved="$(current_channel || true)"
  fi
  write_advice "$scanner" "$iface" "$REC" "$note" "$saved" \
    "$SCORE_1" "$SCORE_6" "$SCORE_11" \
    "$OVERLAP_1" "$OVERLAP_6" "$OVERLAP_11" "$bss"
  unlock_secrets
}

write_fallback_advice() {
  local note="$1" saved
  saved="$(current_channel || true)"
  lock_secrets
  if ! channel_is_set; then
    write_channel 6
    saved=6
  fi
  write_advice "missing" "" "" "$note" "$saved"
  unlock_secrets
}

ensure_initial_channel() {
  local iface
  if channel_is_set; then
    return 0
  fi
  log "no saved channel yet; surveying once before hostapd starts"
  if iface="$(usb_net_iface "$COMBO_VID" "$COMBO_PID")"; then
    if survey_iface "$iface" combo "initial survey on the combo radio" yes; then
      log "initial channel $(current_channel) from the combo survey"
      return 0
    fi
    log "combo scan failed (${SCAN_ERROR:-unknown}); saving channel 6"
    write_fallback_advice "combo scan failed (${SCAN_ERROR:-unknown}); saved channel 6"
    return 0
  fi
  if systemctl is-active --quiet hostapd; then
    log "combo radio is absent and hostapd is already up; saving channel 6 without scanning the AP radio"
    write_fallback_advice "combo survey radio ${COMBO_VID}:${COMBO_PID} is not present; saved channel 6 without scanning the AP radio"
    return 0
  fi
  assign_usb_iface iface "$AP_VID" "$AP_PID" \
    "Kootech AP (USB ${AP_VID}:${AP_PID}) is not present. Leave it in the USB port it is in."
  log "combo radio is absent; surveying the Kootech once before hostapd starts"
  if survey_iface "$iface" kootech "initial survey on the AP radio before hostapd started" yes; then
    log "initial channel $(current_channel) from the Kootech survey"
    return 0
  fi
  log "Kootech scan failed (${SCAN_ERROR:-unknown}); saving channel 6"
  write_fallback_advice "Kootech scan failed (${SCAN_ERROR:-unknown}); saved channel 6"
}

render_tree() {
  local iface="$1" dest="$2"
  local ssid pass channel
  ssid="$(tr -d '[:space:]' <"${SECRET_DIR}/ssid")"
  pass="$(tr -d '[:space:]' <"${SECRET_DIR}/passphrase")"
  channel="$(tr -d '[:space:]' <"${SECRET_DIR}/channel")"
  [[ "$ssid" =~ ^[[:graph:]]{1,32}$ ]] || die "SSID in ${SECRET_DIR}/ssid is not usable"
  [[ "$pass" =~ ^[[:graph:]]{8,63}$ ]] || die "passphrase must be 8-63 non-space characters"
  [[ "$channel" =~ ^(1|6|11)$ ]] || die "saved channel must be 1, 6, or 11"
  [[ "$iface" =~ ^[A-Za-z0-9._-]+$ ]] || die "unexpected interface name: ${iface}"
  refuse_lan "$iface"
  install -d -m 0700 "$dest"
  {
    printf '%s\n' '# Generated by tools/setup_iot_ap.sh. Overwritten on the next render.'
    printf '%s\n' '# 2.4 GHz HT20, WPA2-PSK only. No VHT, no HE, no SAE.'
    printf 'interface=%s\n' "$iface"
    printf '%s\n' 'driver=nl80211'
    printf 'ssid=%s\n' "$ssid"
    printf '%s\n' 'country_code=US'
    printf '%s\n' 'ieee80211d=1'
    printf '%s\n' 'hw_mode=g'
    printf 'channel=%s\n' "$channel"
    printf '%s\n' 'ieee80211n=1'
    printf '%s\n' 'ht_capab=[SHORT-GI-20]'
    printf '%s\n' 'wmm_enabled=1'
    printf '%s\n' 'macaddr_acl=1'
    printf 'accept_mac_file=%s\n' "${ETC}/hostapd.accept"
    printf '%s\n' 'auth_algs=1'
    printf '%s\n' 'ignore_broadcast_ssid=0'
    printf '%s\n' 'wpa=2'
    printf 'wpa_passphrase=%s\n' "$pass"
    printf '%s\n' 'wpa_key_mgmt=WPA-PSK'
    printf '%s\n' 'rsn_pairwise=CCMP'
    printf '%s\n' 'ap_isolate=1'
    printf '%s\n' 'disassoc_low_ack=0'
    printf '%s\n' 'ctrl_interface=/run/hostapd'
    printf '%s\n' 'ctrl_interface_group=0'
    printf '%s\n' 'logger_syslog=-1'
    printf '%s\n' 'logger_syslog_level=2'
  } >"${dest}/hostapd.conf"
  chmod 0600 "${dest}/hostapd.conf"
  valid_macs "${SECRET_DIR}/accept_macs" >"${dest}/hostapd.accept"
  chmod 0640 "${dest}/hostapd.accept"
  {
    printf '%s\n' '# Generated by tools/setup_iot_ap.sh. Overwritten on the next render.'
    printf '%s\n' '# DNS answers every name with 0.0.0.0. DHCP offers no default route.'
    printf 'interface=%s\n' "$iface"
    printf '%s\n' 'bind-interfaces'
    printf 'listen-address=%s\n' "$AP_ADDR"
    printf '%s\n' 'except-interface=lo'
    printf 'dhcp-range=%s\n' "$DHCP_RANGE"
    printf '%s\n' 'dhcp-option=option:router'
    printf 'dhcp-option=option:dns-server,%s\n' "$AP_ADDR"
    printf 'dhcp-option=option:ntp-server,%s\n' "$AP_ADDR"
    printf '%s\n' 'dhcp-authoritative'
    printf '%s\n' 'no-resolv'
    printf '%s\n' 'no-hosts'
    printf '%s\n' 'address=/#/0.0.0.0'
    printf '%s\n' 'filter-AAAA'
    printf '%s\n' 'domain-needed'
    printf '%s\n' 'bogus-priv'
    printf 'dhcp-leasefile=%s\n' "${STATE}/dnsmasq.leases"
    printf '%s\n' 'user=dnsmasq'
    printf '%s\n' 'group=dnsmasq'
  } >"${dest}/dnsmasq.conf"
  chmod 0644 "${dest}/dnsmasq.conf"
}

files_match() {
  [[ -f "$2" ]] && cmp -s "$1" "$2"
}

stage_and_install() {
  local iface="$1" stage
  install -d -m 0755 "$RUN"
  stage="$(mktemp -d "${RUN}/stage.XXXXXX")"
  render_tree "$iface" "$stage"
  CONF_CHANGED=0
  ACCEPT_CHANGED=0
  DNS_CHANGED=0
  files_match "${stage}/hostapd.conf" "${ETC}/hostapd.conf" || CONF_CHANGED=1
  files_match "${stage}/hostapd.accept" "${ETC}/hostapd.accept" || ACCEPT_CHANGED=1
  files_match "${stage}/dnsmasq.conf" "${ETC}/dnsmasq.conf" || DNS_CHANGED=1
  install -d -m 0700 "$ETC"
  install -m 0600 -o root -g root "${stage}/hostapd.conf" "${ETC}/hostapd.conf"
  install -m 0640 -o root -g root "${stage}/hostapd.accept" "${ETC}/hostapd.accept"
  install -m 0644 -o root -g root "${stage}/dnsmasq.conf" "${ETC}/dnsmasq.conf"
  rm -rf "$stage"
}

configure_iface() {
  local iface="$1" addr
  refuse_lan "$iface"
  unblock_wifi "$iface"
  ip link set dev "$iface" up
  if [[ -w "/proc/sys/net/ipv6/conf/${iface}/disable_ipv6" ]]; then
    printf '1\n' >"/proc/sys/net/ipv6/conf/${iface}/disable_ipv6"
  fi
  if ! ip -4 -o addr show dev "$iface" | awk '{ print $4 }' | grep -qx "$AP_CIDR"; then
    ip -4 addr flush dev "$iface" || true
    ip -4 addr add "$AP_CIDR" dev "$iface"
  fi
  while read -r addr; do
    [[ -n "$addr" ]] || continue
    [[ "$addr" == "$AP_CIDR" ]] && continue
    ip -4 addr del "$addr" dev "$iface" || true
  done < <(ip -4 -o addr show dev "$iface" | awk '{ print $4 }')
  getent passwd dnsmasq >/dev/null || die "user dnsmasq is missing"
  install -d -o dnsmasq -g dnsmasq -m 0750 "$STATE"
  if [[ ! -e "${STATE}/dnsmasq.leases" ]]; then
    install -o dnsmasq -g dnsmasq -m 0640 /dev/null "${STATE}/dnsmasq.leases"
  else
    chown dnsmasq:dnsmasq "${STATE}/dnsmasq.leases"
    chmod 0640 "${STATE}/dnsmasq.leases"
  fi
}

fw() {
  local mode="$1"
  shift
  if [[ "$mode" == "permanent" ]]; then
    firewall-cmd --permanent "$@"
  else
    firewall-cmd "$@"
  fi
}

zone_known() {
  local mode="$1"
  fw "$mode" --get-zones | tr ' ' '\n' | grep -qx "$ZONE"
}

policy_known() {
  local mode="$1" name="$2"
  fw "$mode" --get-policies | tr ' ' '\n' | grep -qx "$name"
}

ensure_zone_services() {
  local mode="$1" svc current port
  local -a services=()
  local -A want=([dhcp]=1 [dns]=1 [ntp]=1)
  current="$(fw "$mode" --zone="$ZONE" --list-services)"
  read -r -a services <<<"$current"
  for svc in "${services[@]}"; do
    [[ -n "$svc" ]] || continue
    if [[ -z "${want[$svc]:-}" ]]; then
      fw "$mode" --zone="$ZONE" --remove-service="$svc"
    fi
  done
  for svc in dhcp dns ntp; do
    if ! fw "$mode" --zone="$ZONE" --query-service="$svc"; then
      fw "$mode" --zone="$ZONE" --add-service="$svc"
    fi
  done
  if fw "$mode" --zone="$ZONE" --query-masquerade; then
    fw "$mode" --zone="$ZONE" --remove-masquerade
  fi
  while read -r port; do
    [[ -n "$port" ]] || continue
    fw "$mode" --zone="$ZONE" --remove-port="$port"
  done < <(fw "$mode" --zone="$ZONE" --list-ports | tr ' ' '\n')
  while read -r port; do
    [[ -n "$port" ]] || continue
    fw "$mode" --zone="$ZONE" --remove-forward-port="$port"
  done < <(fw "$mode" --zone="$ZONE" --list-forward-ports)
}

ensure_policy() {
  local name="$1" ingress="$2" egress="$3" mode
  # --new-policy, --set-target, and --set-priority are permanent-only.
  # The reload in configure_firewall copies them into the running firewall.
  if ! policy_known permanent "$name"; then
    fw permanent --new-policy="$name"
  fi
  for mode in permanent runtime; do
    if [[ "$mode" == runtime ]] && ! policy_known runtime "$name"; then
      continue
    fi
    if ! fw "$mode" --policy="$name" --query-ingress-zone="$ingress"; then
      fw "$mode" --policy="$name" --add-ingress-zone="$ingress"
    fi
    if ! fw "$mode" --policy="$name" --query-egress-zone="$egress"; then
      fw "$mode" --policy="$name" --add-egress-zone="$egress"
    fi
  done
  fw permanent --policy="$name" --set-target=DROP
  fw permanent --policy="$name" --set-priority=-100
}

# Runtime policy priority stays at firewalld's default until a reload.
# Return 0 when that reload is still required.
firewall_reload_needed() {
  local name pri
  [[ -f "${ETC}/firewall.priority" ]] || return 0
  zone_known runtime || return 0
  zone_known permanent || return 0
  for name in "$POLICY_FROM" "$POLICY_TO"; do
    policy_known runtime "$name" || return 0
    policy_known permanent "$name" || return 0
    pri="$(fw permanent --policy="$name" --get-priority 2>/dev/null || echo missing)"
    [[ "$pri" == "-100" ]] || return 0
  done
  return 1
}

configure_firewall() {
  local reload=0
  systemctl is-active --quiet firewalld || die "firewalld is not active; this script will not start it"
  if firewall_reload_needed; then
    reload=1
  fi
  # --new-zone and --set-target are permanent-only. A reload copies them
  # into the running firewall. Service edits can still be applied live.
  if ! zone_known permanent; then
    fw permanent --new-zone="$ZONE"
  fi
  fw permanent --zone="$ZONE" --set-target=DROP
  ensure_zone_services permanent
  if zone_known runtime; then
    ensure_zone_services runtime
  fi
  # -100 is ahead of firewalld's default policy priority (-1). 0 is reserved.
  # ANY is every zone except HOST, so this host can still open connections.
  ensure_policy "$POLICY_FROM" "$ZONE" ANY
  ensure_policy "$POLICY_TO" ANY "$ZONE"
  firewall-cmd --check-config
  if [[ "$reload" -eq 1 ]]; then
    log "reloading firewalld so the IoT zone and policies become active"
    firewall-cmd --reload
    install -d -m 0700 "$ETC"
    printf '%s\n' '-100' >"${ETC}/firewall.priority"
    chmod 0644 "${ETC}/firewall.priority"
  fi
}

bind_firewall_iface() {
  local iface="$1" old=""
  if [[ -f "${ETC}/ap.iface" ]]; then
    old="$(tr -d '[:space:]' <"${ETC}/ap.iface")"
  fi
  if [[ -n "$old" && "$old" != "$iface" ]]; then
    firewall-cmd --zone="$ZONE" --remove-interface="$old" || true
    firewall-cmd --permanent --zone="$ZONE" --remove-interface="$old" || true
  fi
  if [[ "$(firewall-cmd --get-zone-of-interface="$iface" 2>/dev/null || true)" != "$ZONE" ]]; then
    firewall-cmd --zone="$ZONE" --change-interface="$iface"
  fi
  if ! firewall-cmd --permanent --zone="$ZONE" --query-interface="$iface"; then
    firewall-cmd --permanent --zone="$ZONE" --add-interface="$iface"
  fi
  printf '%s\n' "$iface" >"${ETC}/ap.iface"
  chmod 0644 "${ETC}/ap.iface"
}

configure_chrony() {
  local snippet="/etc/chrony.d/tallow-iot.conf" tmp changed=0 include
  include="include ${snippet}"
  install -d -m 0755 /etc/chrony.d
  tmp="$(mktemp)"
  cat >"$tmp" <<EOF
# tallow IoT access point. NTP for ${AP_NET} only.
allow ${AP_NET}
EOF
  if [[ ! -f "$snippet" ]] || ! cmp -s "$tmp" "$snippet"; then
    install -m 0644 "$tmp" "$snippet"
    changed=1
  fi
  rm -f "$tmp"
  if ! grep -qxF "$include" /etc/chrony.conf; then
    printf '\n# tallow IoT access point (tools/setup_iot_ap.sh)\n%s\n' "$include" >>/etc/chrony.conf
    changed=1
  fi
  systemctl enable chronyd
  if [[ "$changed" -eq 1 ]]; then
    systemctl restart chronyd
    log "chrony now allows ${AP_NET}"
  else
    systemctl start chronyd
  fi
}

configure_nm() {
  local conf="/etc/NetworkManager/conf.d/tallow-iot-unmanaged.conf"
  local tmp line="" d iface drv pair state
  local -a drivers=("$AP_DRIVER" "$COMBO_DRIVER")
  for pair in "${AP_VID}:${AP_PID}" "${COMBO_VID}:${COMBO_PID}"; do
    iface="$(usb_net_iface "${pair%%:*}" "${pair##*:}")" || continue
    drv="$(driver_of "$iface" || true)"
    [[ -n "$drv" ]] && drivers+=("$drv")
  done
  while read -r d; do
    [[ "$d" =~ ^[A-Za-z0-9_.-]+$ ]] || continue
    if [[ -n "$line" ]]; then
      line+=";"
    fi
    line+="driver:${d}"
  done < <(printf '%s\n' "${drivers[@]}" | awk 'NF && !seen[$0]++')
  tmp="$(mktemp)"
  cat >"$tmp" <<EOF
# Installed by tools/setup_iot_ap.sh.
# NetworkManager must not run a shared hotspot on these radios or join a
# network on the survey stick.
[keyfile]
unmanaged-devices=${line}
EOF
  if [[ ! -f "$conf" ]] || ! cmp -s "$tmp" "$conf"; then
    install -m 0644 "$tmp" "$conf"
    nmcli general reload
  fi
  rm -f "$tmp"
  for pair in "${AP_VID}:${AP_PID}" "${COMBO_VID}:${COMBO_PID}"; do
    iface="$(usb_net_iface "${pair%%:*}" "${pair##*:}")" || continue
    state="$(nmcli -g GENERAL.STATE device show "$iface" 2>/dev/null || true)"
    case "$state" in
      *disconnected* | *unmanaged* | *unavailable* | "") ;;
      *) nmcli device disconnect "$iface" || true ;;
    esac
    nmcli device set "$iface" managed no || true
  done
}

install_units() {
  install -d -m 0755 /etc/systemd/system/hostapd.service.d
  # The Fedora unit points at /usr/sbin/hostapd. The package installs the
  # binary at /usr/bin/hostapd and does not add that symlink.
  cat >/etc/systemd/system/hostapd.service.d/tallow.conf <<EOF
[Unit]
After=NetworkManager.service
ConditionPathExists=${ETC}/hostapd.conf

[Service]
ExecStart=
ExecStart=/usr/bin/hostapd ${ETC}/hostapd.conf -P /run/hostapd.pid -B
ExecStartPre=${SCRIPT} prepare
ExecStartPost=${SCRIPT} prepare
ExecReload=/bin/kill -HUP \$MAINPID
EOF
  chmod 0644 /etc/systemd/system/hostapd.service.d/tallow.conf

  cat >/etc/systemd/system/tallow-iot-dnsmasq.service <<EOF
[Unit]
Description=tallow IoT DHCP and DNS sinkhole
After=hostapd.service
BindsTo=hostapd.service
PartOf=hostapd.service

[Service]
Type=simple
ExecStartPre=${SCRIPT} prepare
ExecStart=/usr/sbin/dnsmasq --keep-in-foreground --conf-file=${ETC}/dnsmasq.conf
Restart=on-failure
RestartSec=2s

[Install]
WantedBy=multi-user.target
EOF
  chmod 0644 /etc/systemd/system/tallow-iot-dnsmasq.service

  cat >/etc/systemd/system/tallow-iot-combo-bt-off.service <<EOF
[Unit]
Description=Power off the Realtek combo Bluetooth controller
After=bluetooth.service
PartOf=bluetooth.service

[Service]
Type=oneshot
ExecStart=${SCRIPT} combo-bt-off

[Install]
WantedBy=bluetooth.service multi-user.target
EOF
  chmod 0644 /etc/systemd/system/tallow-iot-combo-bt-off.service

  cat >/etc/systemd/system/tallow-iot-survey.service <<EOF
[Unit]
Description=Record 2.4 GHz channel advice from the Realtek combo
After=tallow-iot-combo-bt-off.service

[Service]
Type=oneshot
ExecStart=${SCRIPT} survey-timer
EOF
  chmod 0644 /etc/systemd/system/tallow-iot-survey.service

  cat >/etc/systemd/system/tallow-iot-survey.timer <<EOF
[Unit]
Description=Survey 2.4 GHz channels 1, 6, and 11 every 30 minutes

[Timer]
OnBootSec=1min
OnActiveSec=1min
OnUnitActiveSec=30min
AccuracySec=1min

[Install]
WantedBy=timers.target
EOF
  chmod 0644 /etc/systemd/system/tallow-iot-survey.timer

  cat >/etc/udev/rules.d/80-tallow-combo-bt.rules <<EOF
# Realtek combo Bluetooth (0bda:d723) shares the survey antenna. Keep it off.
ACTION=="add", SUBSYSTEM=="bluetooth", ATTRS{idVendor}=="${COMBO_VID}", ATTRS{idProduct}=="${COMBO_PID}", TAG+="systemd", ENV{SYSTEMD_WANTS}="tallow-iot-combo-bt-off.service"
EOF
  chmod 0644 /etc/udev/rules.d/80-tallow-combo-bt.rules

  cat >/etc/tmpfiles.d/tallow-iot.conf <<EOF
d ${RUN} 0755 root root -
d ${STATE} 0750 dnsmasq dnsmasq -
EOF
  chmod 0644 /etc/tmpfiles.d/tallow-iot.conf
  systemd-tmpfiles --create /etc/tmpfiles.d/tallow-iot.conf

  udevadm control --reload
  systemctl daemon-reload
}

cmd_prepare() {
  local iface drv
  need_root
  [[ "$#" -eq 0 ]] || die "prepare takes no arguments"
  assign_usb_iface iface "$AP_VID" "$AP_PID" \
    "Kootech AP (USB ${AP_VID}:${AP_PID}) is not present. Leave it in the USB port it is in."
  refuse_lan "$iface"
  drv="$(driver_of "$iface" || true)"
  if [[ -n "$drv" && "$drv" != "$AP_DRIVER" ]]; then
    log "AP driver is ${drv} (expected ${AP_DRIVER}); still binding by USB id"
  fi
  [[ -f "${SECRET_DIR}/channel" ]] || die "no saved channel; run ${SCRIPT} up"
  lock_secrets
  stage_and_install "$iface"
  unlock_secrets
  configure_iface "$iface"
  if systemctl is-active --quiet firewalld && zone_known runtime; then
    bind_firewall_iface "$iface"
  fi
}

block_combo_hci() {
  local hci="$1" shopts r
  shopts="$(shopt -p nullglob || true)"
  shopt -s nullglob
  for r in /sys/class/rfkill/rfkill*; do
    if [[ "$(tr -d '[:space:]' <"${r}/name")" == "$hci" && "$(tr -d '[:space:]' <"${r}/type")" == "bluetooth" ]]; then
      printf '1\n' >"${r}/soft" || true
    fi
  done
  eval "$shopts"
  rfkill block "$hci" || true
}

cmd_combo_bt_off() {
  local hci="" i powered="" soft=""
  need_root
  [[ "$#" -eq 0 ]] || die "combo-bt-off takes no arguments"
  hci="$(usb_hci "$COMBO_VID" "$COMBO_PID" || true)"
  if [[ -z "$hci" ]]; then
    log "combo Bluetooth controller is not present"
    return 0
  fi
  for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    hci="$(usb_hci "$COMBO_VID" "$COMBO_PID" || true)"
    [[ -n "$hci" ]] || return 0
    block_combo_hci "$hci"
    if systemctl is-active --quiet bluetooth; then
      busctl set-property org.bluez "/org/bluez/${hci}" org.bluez.Adapter1 Powered b false || true
    fi
    sleep 0.4
    powered="$(busctl get-property org.bluez "/org/bluez/${hci}" org.bluez.Adapter1 Powered 2>/dev/null || echo 'b false')"
    soft="$(rfkill -r -n -o DEVICE,SOFT | awk -v h="$hci" '$1 == h { print $2; exit }')"
    if [[ "$powered" == "b false" && "$soft" == "blocked" ]]; then
      log "combo Bluetooth ${hci} is off"
      return 0
    fi
    if ! systemctl is-active --quiet bluetooth && [[ "$soft" == "blocked" ]]; then
      log "combo Bluetooth ${hci} is rfkill-blocked"
      return 0
    fi
  done
  printf 'setup_iot_ap: combo Bluetooth %s stayed up (powered=%s soft=%s)\n' \
    "$hci" "${powered:-?}" "${soft:-?}" >&2
  return 1
}

cmd_survey_timer() {
  local iface
  need_root
  [[ "$#" -eq 0 ]] || die "survey-timer takes no arguments"
  if [[ ! -d "$SECRET_DIR" ]]; then
    log "secrets directory is missing; run up first"
    return 0
  fi
  if ! iface="$(usb_net_iface "$COMBO_VID" "$COMBO_PID")"; then
    lock_secrets
    ensure_secret_dir
    write_advice missing "" "" "combo survey radio ${COMBO_VID}:${COMBO_PID} is not present"
    unlock_secrets
    log "combo survey radio is not present"
    return 0
  fi
  if ! cmd_combo_bt_off; then
    lock_secrets
    ensure_secret_dir
    write_advice blocked "$iface" "" "combo Bluetooth stayed up; scan skipped"
    unlock_secrets
    return 0
  fi
  if ! survey_iface "$iface" combo "advice only; apply-channel restarts the AP on the recommended channel" no; then
    lock_secrets
    write_advice combo "$iface" "" "scan failed: ${SCAN_ERROR:-unknown}"
    unlock_secrets
    log "survey failed: ${SCAN_ERROR:-unknown}"
    return 0
  fi
  log "advice recorded; recommended channel $(advice_field recommended || true)"
}

allow_mac() {
  local mac="$1" tmp
  ensure_secret_dir
  if [[ ! -f "${SECRET_DIR}/accept_macs" ]]; then
    printf '' | write_secret "${SECRET_DIR}/accept_macs"
  fi
  if valid_macs "${SECRET_DIR}/accept_macs" | grep -qx "$mac"; then
    publish_accept
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
  publish_accept
  log "allowed ${mac}"
}

revoke_mac() {
  local mac="$1" tmp
  ensure_secret_dir
  [[ -f "${SECRET_DIR}/accept_macs" ]] || {
    log "${mac} was not on the accept list"
    return 0
  }
  if ! valid_macs "${SECRET_DIR}/accept_macs" | grep -qx "$mac"; then
    log "${mac} was not on the accept list"
    return 0
  fi
  tmp="$(mktemp)"
  valid_macs "${SECRET_DIR}/accept_macs" | grep -vx "$mac" >"$tmp" || true
  write_secret "${SECRET_DIR}/accept_macs" <"$tmp"
  rm -f "$tmp"
  publish_accept
  log "revoked ${mac}"
}

reload_hostapd_if_up() {
  if systemctl is-active --quiet hostapd; then
    systemctl reload hostapd
    log "hostapd reloaded"
  else
    log "hostapd is not running; the accept file is used on the next start"
  fi
}

cmd_allow() {
  local mac
  need_root
  require_fedora
  [[ "$#" -eq 1 ]] || die "usage: ${SCRIPT} allow aa:bb:cc:dd:ee:ff"
  mac="$(norm_mac "$1")"
  lock_secrets
  allow_mac "$mac"
  unlock_secrets
  reload_hostapd_if_up
}

cmd_revoke() {
  local mac
  need_root
  require_fedora
  [[ "$#" -eq 1 ]] || die "usage: ${SCRIPT} revoke aa:bb:cc:dd:ee:ff"
  mac="$(norm_mac "$1")"
  lock_secrets
  revoke_mac "$mac"
  unlock_secrets
  reload_hostapd_if_up
}

cmd_apply_channel() {
  local rec cur
  need_root
  require_fedora
  [[ "$#" -eq 0 ]] || die "apply-channel takes no arguments"
  [[ -f "${SECRET_DIR}/advice" ]] || die "no survey advice in ${SECRET_DIR}/advice"
  rec="$(advice_field recommended || true)"
  [[ "$rec" =~ ^(1|6|11)$ ]] || die "no channel recommendation to apply (see ${SECRET_DIR}/advice)"
  cur="$(current_channel || true)"
  if [[ "$cur" == "$rec" ]]; then
    log "saved channel is already ${rec}"
    return 0
  fi
  lock_secrets
  write_channel "$rec"
  unlock_secrets
  log "saved channel ${cur:-unset} -> ${rec}"
  if [[ ! -x /usr/bin/hostapd || ! -f /etc/systemd/system/hostapd.service.d/tallow.conf ]]; then
    log "channel saved; run up to start the AP"
    return 0
  fi
  log "restarting hostapd; associated stations will drop"
  systemctl restart hostapd
  if [[ -f /etc/systemd/system/tallow-iot-dnsmasq.service ]]; then
    systemctl restart tallow-iot-dnsmasq
  fi
}

install_hostapd() {
  if rpm -q hostapd >/dev/null 2>&1 && [[ -x /usr/bin/hostapd ]]; then
    return 0
  fi
  log "installing hostapd (installonly_limit=0 so this transaction does not prune kernels)"
  dnf install -y --setopt=installonly_limit=0 hostapd
  [[ -x /usr/bin/hostapd ]] || die "hostapd package did not provide /usr/bin/hostapd"
}

cmd_up() {
  local iface restarted=0 c
  need_root
  require_fedora
  [[ "$#" -eq 0 ]] || die "up takes no arguments"
  for c in iw firewall-cmd dnsmasq openssl systemctl rfkill busctl timeout install awk nmcli udevadm ip; do
    need_cmd "$c"
  done
  assign_usb_iface iface "$AP_VID" "$AP_PID" \
    "Kootech AP (USB ${AP_VID}:${AP_PID}) is not present. Leave it in the USB port it is in."
  refuse_lan "$iface"
  if systemctl is-active --quiet dnsmasq.service; then
    die "system dnsmasq.service is active. This AP uses tallow-iot-dnsmasq.service. Stop the system unit first."
  fi
  getent passwd dnsmasq >/dev/null || die "user dnsmasq is missing"
  install_hostapd
  chmod 0755 "$SCRIPT"
  lock_secrets
  ensure_credentials
  unlock_secrets
  configure_nm
  configure_firewall
  configure_chrony
  install_units
  systemctl enable --now tallow-iot-combo-bt-off.service
  ensure_initial_channel
  cmd_prepare
  systemctl enable hostapd.service
  if systemctl is-active --quiet hostapd && [[ "$CONF_CHANGED" -eq 0 ]]; then
    if [[ "$ACCEPT_CHANGED" -eq 1 ]]; then
      systemctl reload hostapd
      log "accept list updated; hostapd reloaded"
    else
      log "hostapd config unchanged; leaving the AP up"
    fi
  else
    if systemctl is-active --quiet hostapd; then
      log "hostapd config changed; restarting (stations will drop)"
      systemctl restart hostapd
    else
      systemctl start hostapd
    fi
    restarted=1
  fi
  systemctl enable tallow-iot-dnsmasq.service
  if [[ "$restarted" -eq 1 || "$DNS_CHANGED" -eq 1 ]] || ! systemctl is-active --quiet tallow-iot-dnsmasq; then
    systemctl restart tallow-iot-dnsmasq
  fi
  systemctl enable --now tallow-iot-survey.timer
  log "SSID $(tr -d '[:space:]' <"${SECRET_DIR}/ssid") on ${iface} channel $(current_channel) address ${AP_CIDR}"
  log "passphrase: ${SECRET_DIR}/passphrase"
  if [[ -z "$(valid_macs "${SECRET_DIR}/accept_macs")" ]]; then
    log "accept list is empty; nothing can join until: sudo ${SCRIPT} allow aa:bb:cc:dd:ee:ff"
  fi
  log "channel changes: sudo ${SCRIPT} apply-channel"
}

cmd_status() {
  local iface hci n soft powered from_pri to_pri from_target to_target zone_target zone_services
  need_root
  iface="$(usb_net_iface "$AP_VID" "$AP_PID" || true)"
  hci="$(usb_hci "$COMBO_VID" "$COMBO_PID" || true)"
  printf 'SSID file: %s\n' "${SECRET_DIR}/ssid"
  if [[ -f "${SECRET_DIR}/ssid" ]]; then
    printf 'SSID: %s\n' "$(tr -d '[:space:]' <"${SECRET_DIR}/ssid")"
  fi
  printf 'passphrase file: %s\n' "${SECRET_DIR}/passphrase"
  printf 'saved channel: %s\n' "$(current_channel || printf '%s' 'unset')"
  printf 'AP interface: %s\n' "${iface:-missing}"
  if [[ -n "$iface" ]]; then
    ip -4 -o addr show dev "$iface" | awk '{ print "address: " $4 }' || true
  fi
  printf 'hostapd: %s\n' "$(systemctl is-active hostapd 2>/dev/null || true)"
  printf 'dnsmasq: %s\n' "$(systemctl is-active tallow-iot-dnsmasq 2>/dev/null || true)"
  printf 'survey timer: %s\n' "$(systemctl is-active tallow-iot-survey.timer 2>/dev/null || true)"
  if [[ -f "${SECRET_DIR}/accept_macs" ]]; then
    n="$(valid_macs "${SECRET_DIR}/accept_macs" | awk 'END { print NR + 0 }')"
    printf 'accept list (%s):\n' "$n"
    valid_macs "${SECRET_DIR}/accept_macs" | sed 's/^/  /' || true
    if [[ "$n" -eq 0 ]]; then
      printf '  (empty; nothing can join)\n'
    fi
  else
    printf 'accept list: missing\n'
  fi
  if [[ -n "$hci" ]]; then
    soft="$(rfkill -r -n -o DEVICE,SOFT | awk -v h="$hci" '$1 == h { print $2; exit }')"
    powered="$(busctl get-property org.bluez "/org/bluez/${hci}" org.bluez.Adapter1 Powered 2>/dev/null || echo 'unknown')"
    printf 'combo Bluetooth: %s soft=%s powered=%s\n' "$hci" "${soft:-unknown}" "$powered"
  else
    printf 'combo Bluetooth: not present\n'
  fi
  if [[ -f "${SECRET_DIR}/advice" ]]; then
    printf 'advice:\n'
    sed 's/^/  /' "${SECRET_DIR}/advice"
  else
    printf 'advice: none\n'
  fi
  if systemctl is-active --quiet firewalld && zone_known runtime; then
    zone_target="$(firewall-cmd --permanent --zone="$ZONE" --get-target 2>/dev/null || echo '?')"
    zone_services="$(firewall-cmd --zone="$ZONE" --list-services 2>/dev/null || echo '?')"
    from_target="$(firewall-cmd --permanent --policy="$POLICY_FROM" --get-target 2>/dev/null || echo '?')"
    from_pri="$(firewall-cmd --permanent --policy="$POLICY_FROM" --get-priority 2>/dev/null || echo '?')"
    to_target="$(firewall-cmd --permanent --policy="$POLICY_TO" --get-target 2>/dev/null || echo '?')"
    to_pri="$(firewall-cmd --permanent --policy="$POLICY_TO" --get-priority 2>/dev/null || echo '?')"
    printf 'firewall zone %s target=%s services=%s\n' "$ZONE" "$zone_target" "$zone_services"
    printf 'policy %s target=%s priority=%s\n' "$POLICY_FROM" "$from_target" "$from_pri"
    printf 'policy %s target=%s priority=%s\n' "$POLICY_TO" "$to_target" "$to_pri"
  else
    printf 'firewall zone %s: not loaded\n' "$ZONE"
  fi
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
    apply-channel) cmd_apply_channel "$@" ;;
    prepare) cmd_prepare "$@" ;;
    combo-bt-off) cmd_combo_bt_off "$@" ;;
    survey-timer) cmd_survey_timer "$@" ;;
    *)
      usage >&2
      exit 1
      ;;
  esac
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
