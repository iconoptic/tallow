#!/usr/bin/env bash
# Build the panel and the broker, install them as user services, and restart both.
#
# The binaries that the services run are host/target/live/. daemon build writes
# host/target/release/, and this script copies the new files into place with mv
# so a running process keeps its old inode until the restart.
#
# The access-point unit tallow-iot-dnsmasq stays with tools/setup_iot_ap.sh.
# This script does not restart it. The combo Bluetooth-off unit and the survey
# timer are retired by that script.
#
# The panel unit starts through `sg dialout`. A user service does not receive
# the login's groups, SupplementaryGroups= fails with status 216/GROUP, and the
# root board's port (/dev/ttyUSB*, group dialout) stays closed without sg.
# This login still has to be in dialout, or sg cannot add the group.
#
# Usage, from a login session, as the broker user (not root):
#   tools/deploy_panel.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LIVE_DIR="${ROOT}/host/target/live"
UNIT_DIR="${HOME}/.config/systemd/user"
PANEL_UNIT=tallow-panel.service
BROKER_UNIT=tallow-panel-broker-rust.service

die() { printf '%s\n' "$*" >&2; exit 1; }

[[ "$(id -u)" -ne 0 ]] || die "Run this as the broker user, not root. A root panel locks the agent socket."
[[ "$ROOT" == "${HOME}/Projects/tallow" ]] \
  || die "The units run from ${HOME}/Projects/tallow, but this checkout is ${ROOT}."
id -nG | tr ' ' '\n' | grep -qx dialout \
  || die "This login is not in the dialout group, so the panel cannot open the root board. Log in again after: sudo usermod -aG dialout ${USER}"

# A process whose command is tallowd, and one argument is the word panel or run.
tallowd_pids() {
  local pid arg
  for pid in $(ps -u "$(id -u)" -o pid=); do
    [[ -r "/proc/${pid}/cmdline" ]] || continue
    arg=""
    IFS= read -r -d '' arg <"/proc/${pid}/cmdline" || true
    [[ "${arg}" == "tallowd" || "${arg}" == */tallowd ]] || continue
    printf '%s\n' "${pid}"
  done
}

has_arg() {
  local pid="$1" want="$2" arg
  [[ -r "/proc/${pid}/cmdline" ]] || return 1
  while IFS= read -r -d '' arg; do
    [[ "${arg}" == "${want}" ]] && return 0
  done <"/proc/${pid}/cmdline"
  return 1
}

proc_alive() {
  local state
  [[ -r "/proc/$1/status" ]] || return 1
  state="$(awk '/^State:/ {print $2; exit}' "/proc/$1/status")"
  [[ -n "${state}" && "${state}" != "Z" ]]
}

# True when pid is root, or a process root started.
under_pid() {
  local pid="$1" root="$2" ppid
  [[ "${root}" =~ ^[0-9]+$ && "${root}" -gt 1 ]] || return 1
  while [[ "${pid}" =~ ^[0-9]+$ && "${pid}" -gt 1 ]]; do
    [[ "${pid}" == "${root}" ]] && return 0
    ppid="$(awk '/^PPid:/ {print $2; exit}' "/proc/${pid}/status" 2>/dev/null || echo 0)"
    pid="${ppid}"
  done
  return 1
}

stage_bin() {
  local src="$1" dest="$2" tmp
  [[ -x "${src}" ]] || die "Missing ${src}. The release build did not produce it."
  mkdir -p "$(dirname "${dest}")"
  tmp="${dest}.new.$$"
  cp -a "${src}" "${tmp}"
  mv -f "${tmp}" "${dest}"
}

install_unit() {
  local src="$1" dest="${UNIT_DIR}/$(basename "$1")"
  mkdir -p "${UNIT_DIR}"
  if cmp -s "${src}" "${dest}" 2>/dev/null; then
    printf 'user unit %s: already current\n' "${dest}"
    return 0
  fi
  install -m 0644 "${src}" "${dest}"
  printf 'user unit: wrote %s\n' "${dest}"
}

# `systemd-run --user --unit=tallow-panel.service` leaves a transient unit at
# /run/user/<uid>/systemd/transient/. That name wins over the file we install,
# and systemctl enable refuses it. Stopping it lets CollectMode drop the name.
drop_transient() {
  local unit="$1" transient waits=0
  transient="$(systemctl --user show -p Transient --value "${unit}" 2>/dev/null || true)"
  [[ "${transient}" == "yes" ]] || return 0
  printf 'stopping transient %s so the installed unit can take the name\n' "${unit}"
  systemctl --user stop "${unit}" || true
  while [[ "${waits}" -lt 20 ]]; do
    transient="$(systemctl --user show -p Transient --value "${unit}" 2>/dev/null || true)"
    [[ "${transient}" == "yes" ]] || return 0
    sleep 0.5
    waits=$((waits + 1))
  done
  die "Transient ${unit} is still loaded. Stop it with: systemctl --user stop ${unit}"
}

# A panel this user started by hand. The systemd panel, and the mesh child it
# spawned, are left for systemctl restart.
stop_hand_panel() {
  local main pid
  main="$(systemctl --user show -p MainPID --value "${PANEL_UNIT}" 2>/dev/null || echo 0)"
  [[ "${main}" =~ ^[0-9]+$ ]] || main=0
  local found=0
  for pid in $(tallowd_pids); do
    under_pid "${pid}" "${main}" && continue
    has_arg "${pid}" panel || continue
    printf 'stopping hand-started panel %s\n' "${pid}"
    kill -TERM "${pid}" || true
    found=1
  done
  [[ "${found}" -eq 1 ]] || return 0
  local waits=0
  while [[ "${waits}" -lt 20 ]]; do
    local left=0
    for pid in $(tallowd_pids); do
      under_pid "${pid}" "${main}" && continue
      if has_arg "${pid}" panel || has_arg "${pid}" run; then
        proc_alive "${pid}" || continue
        left=1
      fi
    done
    [[ "${left}" -eq 0 ]] && return 0
    sleep 0.5
    waits=$((waits + 1))
  done
  die "A hand-started tallowd is still running. Stop it, then run this again."
}

"${ROOT}/tools/mesh.sh" daemon build

stage_bin "${ROOT}/host/target/release/tallowd" "${LIVE_DIR}/tallowd"
stage_bin "${ROOT}/host/target/release/tallow-broker" "${LIVE_DIR}/tallow-broker"

install_unit "${ROOT}/tools/tallow-panel.service"
install_unit "${ROOT}/tools/tallow-panel-broker-rust.service"
systemctl --user daemon-reload \
  || die "systemctl --user daemon-reload failed. Run this from a login session."

drop_transient "${PANEL_UNIT}"
drop_transient "${BROKER_UNIT}"
systemctl --user daemon-reload \
  || die "systemctl --user daemon-reload failed. Run this from a login session."

stop_hand_panel

systemctl --user enable "${PANEL_UNIT}" "${BROKER_UNIT}"
systemctl --user restart "${PANEL_UNIT}"
systemctl --user restart "${BROKER_UNIT}"

systemctl --user is-active --quiet "${PANEL_UNIT}" || die "The panel service did not stay active."
systemctl --user is-active --quiet "${BROKER_UNIT}" || die "The broker service did not stay active."
printf 'panel: %s\n' "$(systemctl --user is-active "${PANEL_UNIT}")"
printf 'broker: %s\n' "$(systemctl --user is-active "${BROKER_UNIT}")"
printf 'The panel listens on port 8765. Logs: journalctl --user -u %s -u %s -f\n' \
  "${PANEL_UNIT}" "${BROKER_UNIT}"
printf 'These units start at login. For boot, without a login: loginctl enable-linger %s\n' "${USER}"
