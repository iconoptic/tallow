#!/usr/bin/env bash
# Sets up tallowd as the mesh host on this machine, for the root node's port.
# Idempotent. It does not enable or start the service, and it does not run tallowd.
#
# The root is a board name in firmware/mesh/boards.tsv (node-0 by default). Its
# USB path picks the port, and a board whose chip is not esp32 is refused, so a
# cam's port is never named. This script changes:
#   /etc/udev/rules.d/99-tallow-mesh-root.rules   (sudo) gives this user that port
#                                                 by USB path, and links it as /dev/tallow-root
#   ~/.config/systemd/user/tallowd.service        copied from tools/tallowd.service
# and reloads udev and systemd --user. The service needs no root at runtime.
#
# Usage: tools/setup_mesh_host.sh [board]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BOARDS="${ROOT}/firmware/mesh/boards.tsv"
UNIT_SOURCE="${ROOT}/tools/tallowd.service"
RULE_PATH="/etc/udev/rules.d/99-tallow-mesh-root.rules"
UNIT_DIR="${HOME}/.config/systemd/user"
UNIT_PATH="${UNIT_DIR}/tallowd.service"
DEFAULT_BOARD="node-0"

usage() {
  cat <<EOF
Usage: $0 [board]

Sets up tallowd as the mesh host for the root board (default ${DEFAULT_BOARD}).
It changes ${RULE_PATH} (with sudo) and ${UNIT_PATH},
then reloads udev and systemd --user. It does not enable or start the service.
EOF
}

die() { printf '%s\n' "$*" >&2; exit 1; }

case "${1:-}" in
  -h|--help) usage; exit 0 ;;
esac
[[ $# -le 1 ]] || { usage >&2; exit 2; }
BOARD="${1:-$DEFAULT_BOARD}"

# The unit runs from %h/Projects/tallow, so it must be installed from there.
[[ "$ROOT" == "${HOME}/Projects/tallow" ]] \
  || die "The unit runs from ${HOME}/Projects/tallow, but this checkout is ${ROOT}. Move it, or edit tools/tallowd.service."
[[ -r "$BOARDS" ]] || die "No ${BOARDS}."

# The board's chip and USB path, by name. The path is the only thing the rule names.
row="$(awk -F'\t' -v name="$BOARD" '!/^#/ && $1 == name {print $3 "\t" $4; exit}' "$BOARDS")"
[[ -n "$row" ]] || die "No board named ${BOARD} in firmware/mesh/boards.tsv."
chip="${row%%$'\t'*}"
usb_path="${row#*$'\t'}"
[[ "$chip" == esp32 ]] \
  || die "${BOARD} is a ${chip}, not a classic ESP32. Only a classic board runs tallowd. The cams are not mesh nodes."
[[ "$usb_path" =~ ^[0-9.]+$ ]] || die "The USB path for ${BOARD} is not a plain path: ${usb_path}."

rule="$(cat <<EOF
# Written by tools/setup_mesh_host.sh from firmware/mesh/boards.tsv (${BOARD}, USB path ${usb_path}).
# Gives the user running tallowd this one port, and links it as /dev/tallow-root.
SUBSYSTEM=="tty", KERNEL=="ttyUSB*", ENV{ID_PATH}=="*-usb-0:${usb_path}:*", TAG+="uaccess", SYMLINK+="tallow-root"
EOF
)"

rule_changed=0
if [[ -f "$RULE_PATH" && "$(cat "$RULE_PATH")" == "$rule" ]]; then
  echo "udev rule ${RULE_PATH}: already current."
else
  tmp="$(mktemp)"
  printf '%s\n' "$rule" >"$tmp"
  echo "udev rule: writing ${RULE_PATH} (sudo)."
  sudo install -m 0644 "$tmp" "$RULE_PATH"
  rm -f "$tmp"
  sudo udevadm control --reload-rules
  rule_changed=1
fi

mkdir -p "$UNIT_DIR"
if cmp -s "$UNIT_SOURCE" "$UNIT_PATH" 2>/dev/null; then
  echo "user unit ${UNIT_PATH}: already current."
else
  install -m 0644 "$UNIT_SOURCE" "$UNIT_PATH"
  echo "user unit: wrote ${UNIT_PATH}."
  systemctl --user daemon-reload || echo "systemctl --user daemon-reload failed. Run it from a login session."
fi

if [[ ! -x "${ROOT}/host/target/release/tallowd" ]]; then
  echo "No release build of tallowd yet. Run: tools/mesh.sh daemon build"
fi

replug_note=""
if (( rule_changed )); then
  replug_note=" The rule changed, so replug the board now."
fi

cat <<EOF

Root: ${BOARD}, USB path ${usb_path}. Its port is /dev/tallow-root once the board
is replugged or the machine reboots.${replug_note}

The service holds that port while it runs. While it runs, tools/mesh.sh flash,
monitor, backup, bench, and host on the root's port fail. Stop it first.
  Stop:    systemctl --user stop tallowd.service
  Start:   systemctl --user enable --now tallowd.service   (also starts at each login)
  Log:     journalctl --user -u tallowd.service -f
Each start resets the root board. Restarts wait 10 s.

To run it by hand instead, without the service:
  tools/mesh.sh daemon run "\$(readlink -f /dev/tallow-root)" --out data/mesh/map.json
EOF
