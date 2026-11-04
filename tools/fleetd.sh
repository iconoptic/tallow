#!/usr/bin/env bash
# USB flasher for the boards in firmware/mesh/devices.jsonl. Start it while
# attaching nodes or cams, and stop it with Ctrl-C. It is not a login
# service. A new port is read with esptool, and that chip's MAC, path, and
# flash manufacturer are written into the catalog and boards.tsv. A chip
# never seen before becomes the next node-N or toby-cam-N and is flashed. A later
# poll does not open a port whose read succeeded, or a CP2102 whose read
# failed. A USB serial that is a MAC is that chip: the board last recorded
# on the path is not, and a failed read of such a port is tried again, up to
# three times. A write is one `tools/mesh.sh flash` or `cam-flash`, which
# opens the port and resets it. Each poll contacts catalog cams on port 6053
# and adds the ones that answer to the panel page.
#
# Usage:
#   tools/fleetd.sh
#
# Debug lines go to stderr and fleetd.log in the repo root. A failed build or
# flash also writes fleetd-<utc>-<name>.log there. Both match /*.log in
# .gitignore. The journal of image ids already written is data/fleet/applied.jsonl.
# This command does not call daemon ota or cam-push.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
POLL_SECONDS="${POLL_SECONDS:-5}"
CATALOG="${ROOT}/firmware/mesh/devices.jsonl"
BOARDS="${ROOT}/firmware/mesh/boards.tsv"
JOURNAL="${ROOT}/data/fleet/applied.jsonl"
NODE_BOOT="${ROOT}/firmware/mesh/node/target/xtensa-esp32-espidf/release/bootloader.bin"
NODE_ELF="${ROOT}/firmware/mesh/node/target/xtensa-esp32-espidf/release/tallow-node"
CAM_BOOT="${ROOT}/firmware/cam/target/xtensa-esp32s3-espidf/release/bootloader.bin"
CAM_ELF="${ROOT}/firmware/cam/target/xtensa-esp32s3-espidf/release/tallow-cam"
NODE_IMAGE=""
CAM_IMAGE=""
# Paths flashed or refused for this presence. Cleared when the port goes away.
declare -A ATTEMPTED=()
# Paths whose chip was read for this presence. Cleared when the port goes away.
declare -A LEARNED=()
# Failed reads of a MAC-serial port. A CP2102 is not retried: opening it resets it.
declare -A LEARN_TRIES=()
# Paths whose chip read succeeded this presence. A path name is not used until then.
declare -A IDENTIFIED=()

log_debug() {
  local line
  line="$(date -u +%Y-%m-%dT%H:%M:%SZ) $*"
  printf '%s\n' "$line" | tee -a "${ROOT}/fleetd.log" >&2
}

# One failure file. The command output is the tool's own text. Key files are
# not copied in.
failure_log() {
  local name="$1" port="$2" path="$3" code="$4" output="$5" command="$6"
  local safe stamp file
  safe="$(printf '%s' "$name" | tr -c 'A-Za-z0-9._-' '_')"
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  file="${ROOT}/fleetd-${stamp}-${safe}.log"
  {
    printf 'time %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'command %s\n' "$command"
    printf 'port %s\n' "${port:-}"
    printf 'usb_path %s\n' "${path:-}"
    printf 'exit %s\n' "$code"
    printf 'output\n%s\n' "$output"
  } >"$file"
  log_debug "failure log ${file}"
}

# 0 when every catalog row's name, chip, usb path, and Wi-Fi MAC match
# boards.tsv, and the two files name the same boards. Opens nothing.
catalog_agrees() {
  python3 -c '
import json, sys
catalog, boards = sys.argv[1:]
rows = {}
with open(boards, encoding="utf-8") as handle:
    for line in handle:
        if not line.strip() or line.startswith("#"):
            continue
        parts = line.rstrip("\n").split("\t")
        if len(parts) < 4:
            sys.exit("short boards.tsv row")
        name, mac, chip, path = parts[0], parts[1], parts[2], parts[3]
        rows[name] = (chip, path, mac.lower())
seen = {}
with open(catalog, encoding="utf-8") as handle:
    for line in handle:
        if not line.strip():
            continue
        obj = json.loads(line)
        name = obj["name"]
        if name in seen:
            sys.exit(f"duplicate {name}")
        seen[name] = obj
        if name not in rows:
            sys.exit(f"{name} is not in boards.tsv")
        chip, path, mac = rows[name]
        got = (obj["chip"], obj["usb_path"], obj["wifi_mac"].lower())
        if got != (chip, path, mac):
            sys.exit(f"{name} disagrees with boards.tsv")
missing = sorted(set(rows) - set(seen))
if missing:
    sys.exit("missing from catalog: " + ", ".join(missing))
' "$1" "$2"
}

# 0 when no row sets push_ok. Opens nothing.
catalog_push_closed() {
  python3 -c '
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    for line in handle:
        if not line.strip():
            continue
        if json.loads(line).get("push_ok"):
            sys.exit(1)
' "$1"
}

# type, name, 1 or 0 for flash_ok, and the Wi-Fi MAC. Empty when this cable is not that chip.
# A USB serial that is a MAC is the chip. The path's previous row is not.
catalog_lookup() {
  python3 "${ROOT}/tools/fleet_catalog.py" lookup "$1" "$2" "${3:-}"
}

# 0 when the serial is a colon-separated MAC. Opens nothing.
serial_is_mac() {
  [[ "${1,,}" =~ ^([0-9a-f]{2}:){5}[0-9a-f]{2}$ ]]
}

# stdin is path, tab, port. 0 when every attached esp32-mesh row is on ttyUSB.
# A row with no path, or a path that is not plugged in, does not hold the others.
every_mesh_on_ttyusb() {
  python3 -c '
import json, sys
live = {}
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    path, port = line.split("\t", 1)
    live.setdefault(path, []).append(port)
with open(sys.argv[1], encoding="utf-8") as handle:
    for line in handle:
        if not line.strip():
            continue
        row = json.loads(line)
        if row.get("type") != "esp32-mesh":
            continue
        path = row.get("usb_path") or ""
        if not path or path not in live:
            continue
        if not any(port.startswith("/dev/ttyUSB") for port in live[path]):
            raise SystemExit(1)
' "$1"
}

# 0 when this board was already written with this image id. A row with a
# wifi_mac is that chip. An older row without one needs the path and the name.
# The path alone is not the board: a later chip on the same port is not skipped.
journal_has() {
  local file="$1" path="$2" image="$3" name="$4" mac="${5:-}"
  [[ -f "$file" && -n "$name" ]] || return 1
  python3 -c '
import json, sys
file, path, image, name, mac = sys.argv[1:]
mac = mac.lower()
with open(file, encoding="utf-8") as handle:
    for line in handle:
        if not line.strip():
            continue
        obj = json.loads(line)
        if obj.get("image_id") != image:
            continue
        recorded = str(obj.get("wifi_mac", "")).lower()
        if recorded:
            if mac and recorded == mac:
                raise SystemExit(0)
            continue
        if obj.get("usb_path") == path and obj.get("name") == name:
            raise SystemExit(0)
raise SystemExit(1)
' "$file" "$path" "$image" "$name" "$mac"
}

journal_add() {
  local file="$1" path="$2" name="$3" image="$4" command="$5" mac="${6:-}"
  mkdir -p "$(dirname "$file")"
  python3 -c '
import json, sys
file, path, name, image, command, mac = sys.argv[1:]
with open(file, "a", encoding="utf-8") as handle:
    handle.write(json.dumps({
        "usb_path": path,
        "name": name,
        "wifi_mac": mac.lower(),
        "image_id": image,
        "command": command,
    }) + "\n")
' "$file" "$path" "$name" "$image" "$command" "$mac"
}

# SHA-256 of the sha256sum lines for the bootloader and the app ELF.
image_id() {
  local boot="$1" elf="$2"
  [[ -f "$boot" && -f "$elf" ]] || return 1
  sha256sum "$boot" "$elf" | sha256sum | awk 'NR==1 {print $1}'
}

# One word: refuse, wait, skip, flash-node, or flash-cam. Opens nothing.
# Arguments: type, port, applied 0/1, mesh_ready 0/1, flash_ok 0/1, attempted 0/1.
fleet_decide() {
  local type="$1" port="$2" applied="$3" mesh_ready="$4" flash_ok="$5" attempted="${6:-0}"
  local action=refuse
  if [[ -z "$type" ]]; then
    printf 'refuse\n'
    return 0
  fi
  if [[ "$applied" == 1 ]]; then
    printf 'skip\n'
    return 0
  fi
  case "$type" in
    esp32-mesh)
      if [[ "$flash_ok" != 1 ]]; then
        action=refuse
      elif [[ "$port" == /dev/ttyUSB* && "$mesh_ready" == 1 ]]; then
        action=flash-node
      else
        action=wait
      fi
      ;;
    esp32s3-cam)
      if [[ "$flash_ok" == 1 ]]; then
        action=flash-cam
      else
        action=refuse
      fi
      ;;
  esac
  if [[ "$attempted" == 1 && ( "$action" == flash-node || "$action" == flash-cam ) ]]; then
    action=wait
  fi
  printf '%s\n' "$action"
}

# stdin is port, tab, usb path, tab, product string, one device per line.
# stdout starts with mesh_ready, then one decision line per device.
# Opens nothing.
fleet_plan() {
  local catalog="$1" journal="$2" node_image="$3" cam_image="${4:-}"
  local -a ports=() paths=() models=() serials=()
  local port path model serial mesh=0 live=""
  while IFS=$'\t' read -r port path model serial; do
    [[ -n "${port:-}" ]] || continue
    ports+=("$port")
    paths+=("$path")
    models+=("$model")
    serials+=("$serial")
    live+="${path}"$'\t'"${port}"$'\n'
  done
  if printf '%s' "$live" | every_mesh_on_ttyusb "$catalog"; then
    mesh=1
  fi
  printf 'mesh_ready\t%s\n' "$mesh"
  local i type="" name="" flash_ok="" mac="" applied image action lookup
  for i in "${!ports[@]}"; do
    port="${ports[$i]}"
    path="${paths[$i]}"
    model="${models[$i]}"
    serial="${serials[$i]}"
    type=""
    name=""
    flash_ok="0"
    mac=""
    lookup="$(catalog_lookup "$catalog" "$path" "$serial" || true)"
    if [[ -n "$lookup" ]]; then
      IFS=$'\t' read -r type name flash_ok mac <<<"$lookup"
    fi
    image="$node_image"
    [[ "$type" == esp32s3-cam ]] && image="$cam_image"
    applied=0
    if [[ -n "$image" ]] && journal_has "$journal" "$path" "$image" "$name" "$mac"; then
      applied=1
    fi
    action="$(fleet_decide "$type" "$port" "$applied" "$mesh" "$flash_ok" 0)"
    # A pipe keeps an empty name. Tab would collapse it and shift the log fields.
    printf '%s|%s|%s|%s|%s|%s|%s\n' "$port" "$path" "$name" "${model//|/}" "$action" "$serial" "$mac"
  done
}

# chip, MAC, and flash manufacturer from esptool flash-id text on stdin.
identity_from_esptool() {
  python3 "${ROOT}/tools/fleet_catalog.py" parse
}

# One seen chip. Rewrites the catalog and boards.tsv. Opens nothing.
catalog_learn() {
  python3 "${ROOT}/tools/fleet_catalog.py" learn "$1" "$2" "$3" "$4" "$5" "$6" "$7" "$8"
}

# USB path, the text after usb-0: in ID_PATH. Opens no tty.
usb_path() {
  udevadm info -q property -n "$1" | sed -n 's/^ID_PATH=.*usb-0:\([0-9.]*\):.*/\1/p'
}

usb_model() {
  udevadm info -q property -n "$1" | sed -n 's/^ID_MODEL=//p'
}

# idVendor:idProduct. Opens no tty.
usb_ids() {
  local props vendor product
  props="$(udevadm info -q property -n "$1")"
  vendor="$(sed -n 's/^ID_VENDOR_ID=//p' <<<"$props")"
  product="$(sed -n 's/^ID_MODEL_ID=//p' <<<"$props")"
  printf '%s:%s\n' "$vendor" "$product"
}

# Attached serial ports. udev only: this does not open the tty, so it does not
# reset a CP2102.
list_ports() {
  local port path model serial
  shopt -s nullglob
  for port in /dev/ttyUSB* /dev/ttyACM*; do
    path="$(usb_path "$port")"
    model="$(usb_model "$port")"
    serial="$(usb_serial "$port")"
    printf '%s\t%s\t%s\t%s\n' "$port" "$path" "$model" "$serial"
  done
}

usb_serial() {
  udevadm info -q property -n "$1" | sed -n 's/^ID_SERIAL_SHORT=//p'
}

run_build() {
  local name="$1"
  shift
  local output code
  log_debug "running $*"
  set +e
  output="$("$@" 2>&1)"
  code=$?
  set -e
  if [[ "$code" -ne 0 ]]; then
    failure_log "$name" "" "" "$code" "$output" "$*"
    exit 1
  fi
  log_debug "${name} finished"
}

# One cam build after startup, for a board that was not attached at the start.
# A failure stays in the failure log and does not stop the node poll.
ensure_cam_image() {
  local output code
  [[ -n "$CAM_IMAGE" ]] && return 0
  [[ "${CAM_BUILD_TRIED:-}" == 1 ]] && return 1
  CAM_BUILD_TRIED=1
  log_debug "running ${ROOT}/tools/mesh.sh cam-build"
  set +e
  output="$("${ROOT}/tools/mesh.sh" cam-build 2>&1)"
  code=$?
  set -e
  if [[ "$code" -ne 0 ]]; then
    failure_log cam-build "" "" "$code" "$output" "${ROOT}/tools/mesh.sh cam-build"
    return 1
  fi
  CAM_IMAGE="$(image_id "$CAM_BOOT" "$CAM_ELF")" || {
    failure_log cam-image "" "" 1 "no cam bootloader or ELF after build" "image_id"
    return 1
  }
  log_debug "cam image ${CAM_IMAGE}"
}

do_flash() {
  local kind="$1" name="$2" port="$3" path="$4" image="$5" mac="${6:-}"
  local output code
  local -a cmd
  if [[ "$kind" == cam && -z "$image" ]]; then
    ensure_cam_image || {
      ATTEMPTED["$path"]=1
      return 0
    }
    image="$CAM_IMAGE"
  fi
  if [[ -z "$image" ]]; then
    failure_log "$name" "$port" "$path" 1 "no image id" "image"
    ATTEMPTED["$path"]=1
    return 0
  fi
  if [[ "$kind" == cam ]]; then
    log_debug "Hold BOOT and tap RST first."
    cmd=("${ROOT}/tools/mesh.sh" cam-flash "$port")
  else
    cmd=("${ROOT}/tools/mesh.sh" flash "$port")
  fi
  log_debug "running ${cmd[*]}"
  set +e
  output="$("${cmd[@]}" 2>&1)"
  code=$?
  set -e
  if [[ "$code" -eq 0 ]]; then
    journal_add "$JOURNAL" "$path" "$name" "$image" "${cmd[*]}" "$mac"
    log_debug "wrote ${name} on ${port}"
  else
    failure_log "$name" "$port" "$path" "$code" "$output" "${cmd[*]}"
  fi
  ATTEMPTED["$path"]=1
}

# One esptool read per new port, then the catalog matches the chip on that path.
# A failed read of a CP2102 is final for this presence: another open resets it.
# A MAC serial that is not yet the catalog's chip is tried up to three times,
# because that cable is a different board and the path's name must not stick.
learn_failed() {
  local key="$1" serial="$2"
  if serial_is_mac "$serial" && [[ "${LEARN_TRIES[$key]:-0}" -lt 2 ]]; then
    LEARN_TRIES["$key"]=$(( ${LEARN_TRIES[$key]:-0} + 1 ))
    return 0
  fi
  LEARNED["$key"]=1
}

learn_attached() {
  local port path model serial key ids output code parsed chip mac mfr
  local -A present=()
  while IFS=$'\t' read -r port path model serial; do
    [[ -n "${port:-}" ]] || continue
    key="${path:-$port}"
    present["$key"]=1
    [[ -n "${LEARNED[$key]:-}" ]] && continue
    ids="$(usb_ids "$port")"
    set +e
    output="$("${ROOT}/tools/mesh.sh" identify "$port" 2>&1)"
    code=$?
    set -e
    if [[ "$code" -ne 0 ]]; then
      failure_log "identify-${key}" "$port" "$path" "$code" "$output" "tools/mesh.sh identify ${port}"
      learn_failed "$key" "$serial"
      continue
    fi
    if ! parsed="$(identity_from_esptool <<<"$output")"; then
      failure_log "identify-${key}" "$port" "$path" 1 "$output" "identity_from_esptool"
      learn_failed "$key" "$serial"
      continue
    fi
    IFS=$'\t' read -r chip mac mfr <<<"$parsed"
    catalog_learn "$CATALOG" "$BOARDS" "$path" "$mac" "$chip" "$mfr" "$model" "$ids"
    log_debug "learned path ${path:-unknown} chip ${chip} mac ${mac} flash ${mfr:-unknown}"
    LEARNED["$key"]=1
    IDENTIFIED["$key"]=1
  done < <(list_ports)
  for key in "${!LEARNED[@]}"; do
    [[ -n "${present[$key]+x}" ]] || unset 'LEARNED[$key]'
  done
  for key in "${!LEARN_TRIES[@]}"; do
    [[ -n "${present[$key]+x}" ]] || unset 'LEARN_TRIES[$key]'
  done
  for key in "${!IDENTIFIED[@]}"; do
    [[ -n "${present[$key]+x}" ]] || unset 'IDENTIFIED[$key]'
  done
}

poll_once() {
  local plan line port path name model action serial mac mesh_line
  local -A now=()
  local -a nodes=() cams=()
  learn_attached
  plan="$(list_ports | fleet_plan "$CATALOG" "$JOURNAL" "$NODE_IMAGE" "$CAM_IMAGE")"
  mesh_line="${plan%%$'\n'*}"
  log_debug "$mesh_line"
  plan="${plan#*$'\n'}"
  if [[ "$plan" == "$mesh_line" || -z "$plan" ]]; then
    log_debug "no serial ports"
    plan=""
  fi
  while IFS='|' read -r port path name model action serial mac; do
    [[ -n "${port:-}" ]] || continue
    now["$path"]=1
    # A CP2102 serial is shared, so the path's name is only the last chip read
    # there. Until this presence reads the chip, it is not that board.
    if [[ -z "${IDENTIFIED[${path:-$port}]:-}" ]] && ! serial_is_mac "$serial"; then
      name=""
      action=refuse
    fi
    if [[ -n "${ATTEMPTED[$path]:-}" && ( "$action" == flash-node || "$action" == flash-cam ) ]]; then
      action=wait
    fi
    log_debug "port ${port} path ${path:-unknown} name ${name:-unknown} model ${model:-unknown} decision ${action}"
    case "$action" in
      flash-node) nodes+=("${name}"$'\t'"${port}"$'\t'"${path}"$'\t'"${mac}") ;;
      flash-cam) cams+=("${name}"$'\t'"${port}"$'\t'"${path}"$'\t'"${mac}") ;;
    esac
  done <<<"$plan"
  for path in "${!ATTEMPTED[@]}"; do
    [[ -n "${now[$path]+x}" ]] || unset 'ATTEMPTED[$path]'
  done
  local sorted name port path mac
  if [[ "${#nodes[@]}" -gt 0 ]]; then
    sorted="$(printf '%s\n' "${nodes[@]}" | sort)"
    while IFS=$'\t' read -r name port path mac; do
      [[ -n "${name:-}" ]] || continue
      do_flash node "$name" "$port" "$path" "$NODE_IMAGE" "$mac"
    done <<<"$sorted"
  fi
  if [[ "${#cams[@]}" -gt 0 ]]; then
    for line in "${cams[@]}"; do
      IFS=$'\t' read -r name port path mac <<<"$line"
      do_flash cam "$name" "$port" "$path" "$CAM_IMAGE" "$mac"
    done
  fi
  sync_panel_cams
}

# Catalog cams that answer on port 6053 are written into the panel inventory.
# A failure here does not stop a flash. Opens no serial port.
sync_panel_cams() {
  local output code
  set +e
  output="$(python3 "${ROOT}/tools/sync_panel_cams.py" \
    --catalog "$CATALOG" \
    --reservations "${ROOT}/secrets/iot-ap/reservations" \
    --inventory "${ROOT}/data/panel/cams.json" 2>&1)"
  code=$?
  set -e
  if [[ "$code" -ne 0 ]]; then
    log_debug "cam page: ${output:-failed}"
    return 0
  fi
  [[ -z "$output" ]] || log_debug "cam page: ${output//$'\n'/; }"
}

main() {
  local err
  if ! err="$(catalog_agrees "$CATALOG" "$BOARDS" 2>&1)"; then
    failure_log catalog "" "" 1 "$err" "catalog_agrees"
    exit 1
  fi
  if ! catalog_push_closed "$CATALOG"; then
    failure_log catalog "" "" 1 "a catalog row sets push_ok" "catalog_push_closed"
    exit 1
  fi
  trap 'log_debug "stopped"; exit 0' INT TERM
  log_debug "fleetd start poll ${POLL_SECONDS}s"
  run_build "node-build" "${ROOT}/tools/mesh.sh" build
  NODE_IMAGE="$(image_id "$NODE_BOOT" "$NODE_ELF")" || {
    failure_log node-image "" "" 1 "no node bootloader or ELF after build" "image_id"
    exit 1
  }
  log_debug "node image ${NODE_IMAGE}"
  # Any cam may be plugged in later, so the cam image is built now. A failure
  # is logged and leaves cams unflashed; nodes still flash.
  ensure_cam_image || log_debug "no cam image: cams will not be flashed"
  while true; do
    poll_once
    sleep "$POLL_SECONDS"
  done
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
