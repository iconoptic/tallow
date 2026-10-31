#!/usr/bin/env bash
# ESPHome for the nodes and their skews, from a pinned container image.
# Nothing is installed on the host. Builds and toolchains land in the
# gitignored firmware/esphome/.esphome/.
#
# The container uses host networking so it can reach nodes on tallow-iot
# (API 6053, OTA 3232). Serial goes through only when ESPHOME_DEVICE is set,
# which also covers a first flash without joining dialout.
#
# Usage:
#   tools/esphome.sh secrets                  write firmware/esphome/secrets.yaml once
#   tools/esphome.sh config  node-mb0200.yaml
#   tools/esphome.sh compile toby-cam-1.yaml
#   ESPHOME_DEVICE=/dev/ttyACM0 tools/esphome.sh mac   MAC, chip, and flash size
#   ESPHOME_DEVICE=/dev/ttyACM0 tools/esphome.sh run node-mb0200.yaml --device /dev/ttyACM0
#   ESPHOME_DEVICE=/dev/ttyUSB0 tools/esphome.sh run node-1.yaml --device /dev/ttyUSB0
#   tools/esphome.sh run toby-cam-1.yaml --device 10.87.0.20   first OTA from the bare node build
#   tools/esphome.sh run toby-cam-1.yaml      later OTAs, to the node's own address
#   tools/esphome.sh logs toby-cam-1.yaml
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="ghcr.io/esphome/esphome:2026.9.1"
CONFIG="${ROOT}/firmware/esphome"
SECRETS="${CONFIG}/secrets.yaml"
AP_SECRETS="${ROOT}/secrets/iot-ap"

log() { printf '%s\n' "$*" >&2; }
die() { log "$*"; exit 1; }

usage() {
  sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
}

write_secrets() {
  [[ -e "$SECRETS" ]] && die "${SECRETS} already exists. Edit it by hand or move it aside."
  [[ -r "${AP_SECRETS}/ssid" && -r "${AP_SECRETS}/passphrase" ]] \
    || die "No ${AP_SECRETS}/ssid or passphrase. Bring the AP up first: sudo ./tools/setup_iot_ap.sh"
  command -v openssl >/dev/null || die "openssl is missing"
  local ssid pass api
  ssid="$(<"${AP_SECRETS}/ssid")"
  pass="$(<"${AP_SECRETS}/passphrase")"
  api="$(openssl rand -base64 32)"
  (
    umask 077
    {
      printf '# Written by tools/esphome.sh secrets. One API/OTA key for every node on tallow-iot.\n'
      printf 'wifi_ssid: "%s"\n' "$ssid"
      printf 'wifi_password: "%s"\n' "$pass"
      printf 'api_key: "%s"\n' "$api"
    } >"$SECRETS"
  )
  log "Wrote ${SECRETS} (mode 0600)."
}

run_image() {
  command -v docker >/dev/null || die "docker is missing"
  mkdir -p "${CONFIG}/.esphome/home"
  local -a args=(--rm --network host --user "$(id -u):$(id -g)"
    -e HOME=/config/.esphome/home -v "${CONFIG}:/config")
  [[ -t 0 && -t 1 ]] && args+=(-it)
  if [[ -n "${ESPHOME_DEVICE:-}" ]]; then
    [[ -e "$ESPHOME_DEVICE" ]] || die "${ESPHOME_DEVICE} is not there."
    # The node keeps its host group (dialout) inside the container.
    args+=(--device "${ESPHOME_DEVICE}:${ESPHOME_DEVICE}" --group-add "$(stat -c %g "$ESPHOME_DEVICE")")
  fi
  docker run "${args[@]}" "$@"
}

case "${1:-}" in
  "" | -h | --help)
    usage
    ;;
  secrets)
    write_secrets
    ;;
  mac)
    [[ -n "${ESPHOME_DEVICE:-}" ]] || die "Set ESPHOME_DEVICE, e.g. ESPHOME_DEVICE=/dev/ttyACM0"
    # An RTS reset over USB-Serial/JTAG leaves an S3 waiting in download mode.
    # A classic ESP32 has no watchdog reset; esptool falls back to RTS there.
    run_image --entrypoint esptool "$IMAGE" --port "$ESPHOME_DEVICE" --after watchdog-reset flash-id
    ;;
  *)
    if [[ ! -e "$SECRETS" ]]; then
      die "No ${SECRETS}. Run: tools/esphome.sh secrets"
    fi
    run_image "$IMAGE" "$@"
    ;;
esac
