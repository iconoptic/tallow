#!/usr/bin/env bash
# Stop the root-owned `tallowd panel`, give it the map and camera inventory,
# and start a detached copy as btgrant from host/target/live/tallowd.
#
# The broker unit is not signaled and is not restarted. SIGTERM goes only to
# a root process whose argv0 is tallowd and whose arguments include `panel`.
# /proc/<pid>/exe is not readable across users. The panel then removes
# data/panel/agent.sock and stops its mesh
# child. This login is not in the dialout group, so the new panel is started
# with `sg dialout`. The mesh child waits until data/mesh/map.json is a
# minute stale, then opens /dev/ttyUSB0.
#
# Usage, from anywhere:
#   tools/reclaim-panel.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT}"

RELEASE="${ROOT}/host/target/release/tallowd"
LIVE_DIR="${ROOT}/host/target/live"
LIVE="${LIVE_DIR}/tallowd"
MAP="${ROOT}/data/mesh/map.json"
CAMS="${ROOT}/data/panel/cams.json"
SOCK="${ROOT}/data/panel/agent.sock"
LOG=/tmp/tallow-panel.log
BROKER_UNIT=tallow-panel-broker-rust.service

if [[ "$(id -un)" != "btgrant" ]]; then
  echo "run this as btgrant" >&2
  exit 1
fi

exe_of() {
  readlink -f "/proc/$1/exe" 2>/dev/null || true
}

# A zombie still has a /proc directory. It has already dropped the socket and the port.
proc_alive() {
  local state
  [[ -r "/proc/$1/status" ]] || return 1
  state="$(awk '/^State:/ {print $2; exit}' "/proc/$1/status")"
  [[ -n "${state}" && "${state}" != "Z" ]]
}

euid_of() {
  awk '/^Uid:/ {print $3; exit}' "/proc/$1/status" 2>/dev/null || true
}

has_arg() {
  local pid="$1" want="$2" arg
  [[ -r "/proc/${pid}/cmdline" ]] || return 1
  while IFS= read -r -d '' arg; do
    [[ "${arg}" == "${want}" ]] && return 0
  done <"/proc/${pid}/cmdline"
  return 1
}

# argv0, not /proc/<pid>/exe: the exe symlink is unreadable for another user.
argv0_is_tallowd() {
  local arg=""
  [[ -r "/proc/$1/cmdline" ]] || return 1
  IFS= read -r -d '' arg <"/proc/$1/cmdline" || true
  [[ "${arg}" == "tallowd" || "${arg}" == */tallowd ]]
}

# Root tallowd whose argv contains `panel`. The sudo wrappers have argv0
# `sudo`, and the mesh child is `tallowd run`. The broker is tallow-broker.
root_panel_pids() {
  local pid
  local pids
  pids="$(ps -u root -o pid=)"
  for pid in ${pids}; do
    [[ "$(euid_of "${pid}")" == 0 ]] || continue
    argv0_is_tallowd "${pid}" || continue
    has_arg "${pid}" panel || continue
    printf '%s\n' "${pid}"
  done
}

mesh_child_pid() {
  local uid_want="$1" pid
  local pids
  pids="$(ps -u "${uid_want}" -o pid= 2>/dev/null || true)"
  for pid in ${pids}; do
    [[ "$(euid_of "${pid}")" == "${uid_want}" ]] || continue
    argv0_is_tallowd "${pid}" || continue
    has_arg "${pid}" run || continue
    has_arg "${pid}" /dev/ttyUSB0 || continue
    printf '%s\n' "${pid}"
    return 0
  done
  return 1
}

child_opened_tty() {
  local pid="$1" fd target
  for fd in /proc/"${pid}"/fd/*; do
    target="$(readlink "${fd}" 2>/dev/null || true)"
    [[ "${target}" == /dev/ttyUSB0 ]] && return 0
  done
  return 1
}

broker_pid="$(systemctl --user show -p MainPID --value "${BROKER_UNIT}")"
if [[ -z "${broker_pid}" || "${broker_pid}" == 0 || ! -d "/proc/${broker_pid}" ]]; then
  echo "broker unit ${BROKER_UNIT} has no running main process" >&2
  exit 1
fi
broker_exe="$(exe_of "${broker_pid}")"
if [[ "${broker_exe}" != */tallow-broker ]]; then
  echo "broker pid ${broker_pid} is ${broker_exe}, not tallow-broker" >&2
  exit 1
fi

[[ -x "${RELEASE}" ]] || { echo "missing ${RELEASE}" >&2; exit 1; }
[[ -f "${MAP}" && -f "${CAMS}" ]] || { echo "missing ${MAP} or ${CAMS}" >&2; exit 1; }
if [[ -e "${ROOT}/data/mesh/pause" ]]; then
  echo "data/mesh/pause is present, so the new panel will not open the port" >&2
  exit 1
fi

mapfile -t panel_pids < <(root_panel_pids)
if [[ "${#panel_pids[@]}" -eq 0 ]]; then
  echo "no root-owned tallowd panel" >&2
  exit 1
fi
for pid in "${panel_pids[@]}"; do
  if [[ "${pid}" == "${broker_pid}" ]]; then
    echo "refusing to signal the broker (${pid})" >&2
    exit 1
  fi
done

echo "broker ${broker_pid} stays up"
echo "SIGTERM root panel: ${panel_pids[*]}"
for pid in "${panel_pids[@]}"; do
  sudo kill -TERM "${pid}"
done

deadline=$((SECONDS + 15))
while (( SECONDS < deadline )); do
  still=0
  for pid in "${panel_pids[@]}"; do
    proc_alive "${pid}" && still=1
  done
  if mesh_child_pid 0 >/dev/null; then
    still=1
  fi
  if [[ "${still}" -eq 0 && ! -S "${SOCK}" ]]; then
    break
  fi
  sleep 0.2
done

still=0
for pid in "${panel_pids[@]}"; do
  if proc_alive "${pid}"; then
    echo "root panel ${pid} is still running after SIGTERM" >&2
    still=1
  fi
done
if mesh_child_pid 0 >/dev/null; then
  echo "root mesh child is still running after the panel was signaled" >&2
  still=1
fi
if [[ -S "${SOCK}" ]]; then
  echo "agent socket is still present after the panel exited" >&2
  still=1
fi
[[ "${still}" -eq 0 ]] || exit 1

if [[ "$(systemctl --user show -p MainPID --value "${BROKER_UNIT}")" != "${broker_pid}" ]]; then
  echo "broker pid changed; not starting a panel" >&2
  exit 1
fi

sudo chown btgrant:btgrant -- "${MAP}" "${CAMS}"
map_owner="$(stat -c '%U:%G' "${MAP}")"
cams_owner="$(stat -c '%U:%G' "${CAMS}")"
[[ "${map_owner}" == "btgrant:btgrant" && "${cams_owner}" == "btgrant:btgrant" ]]

mkdir -p "${LIVE_DIR}"
cp -a "${RELEASE}" "${LIVE}"
release_ino="$(stat -c '%i' "${RELEASE}")"
live_ino="$(stat -c '%i' "${LIVE}")"
if [[ "${release_ino}" == "${live_ino}" ]]; then
  echo "live binary is the same inode as the release binary" >&2
  exit 1
fi

: >"${LOG}"
sg dialout -c 'setsid host/target/live/tallowd panel >>/tmp/tallow-panel.log 2>&1 < /dev/null &'

panel_pid=""
child_pid=""
deadline=$((SECONDS + 120))
while (( SECONDS < deadline )); do
  if [[ -z "${panel_pid}" ]]; then
    panel_pid="$(
      for pid in /proc/[0-9]*; do
        pid="${pid#/proc/}"
        [[ "$(exe_of "${pid}")" == "${LIVE}" ]] || continue
        has_arg "${pid}" panel || continue
        printf '%s\n' "${pid}"
        break
      done
    )"
  elif ! proc_alive "${panel_pid}"; then
    echo "panel ${panel_pid} exited" >&2
    tail -n 40 "${LOG}" >&2 || true
    exit 1
  fi
  if [[ -f "${LOG}" ]] && grep -q 'Permission denied' "${LOG}"; then
    echo "mesh child could not open /dev/ttyUSB0" >&2
    tail -n 40 "${LOG}" >&2 || true
    exit 1
  fi
  if [[ -S "${SOCK}" ]]; then
    sock_owner="$(stat -c '%U:%G' "${SOCK}")"
    sock_mode="$(stat -c '%a' "${SOCK}")"
    if [[ "${sock_owner}" == "btgrant:btgrant" && "${sock_mode}" == "600" ]]; then
      child_pid="$(mesh_child_pid 1000 || true)"
      if [[ -n "${child_pid}" ]] && child_opened_tty "${child_pid}"; then
        break
      fi
    fi
  fi
  sleep 1
done

if [[ -z "${panel_pid}" ]] || ! proc_alive "${panel_pid}"; then
  echo "live panel did not stay up" >&2
  tail -n 40 "${LOG}" >&2 || true
  exit 1
fi
if [[ "$(stat -c '%U:%G %a' "${SOCK}")" != "btgrant:btgrant 600" ]]; then
  echo "socket is $(stat -c '%U:%G %a' "${SOCK}"), wanted btgrant:btgrant 600" >&2
  exit 1
fi
if [[ -z "${child_pid}" ]] || ! child_opened_tty "${child_pid}"; then
  echo "mesh child does not have /dev/ttyUSB0 open" >&2
  tail -n 40 "${LOG}" >&2 || true
  exit 1
fi
now_broker="$(systemctl --user show -p MainPID --value "${BROKER_UNIT}")"
if [[ "${now_broker}" != "${broker_pid}" ]] || ! systemctl --user is-active --quiet "${BROKER_UNIT}"; then
  echo "broker unit changed" >&2
  exit 1
fi

echo "panel ${panel_pid} exe $(exe_of "${panel_pid}")"
echo "socket $(stat -c '%U:%G %a' "${SOCK}") ${SOCK}"
echo "mesh child ${child_pid} has /dev/ttyUSB0 open"
echo "map ${map_owner} $(stat -c '%a' "${MAP}")"
echo "cams ${cams_owner} $(stat -c '%a' "${CAMS}")"
echo "broker ${broker_pid} still $(systemctl --user is-active "${BROKER_UNIT}")"
