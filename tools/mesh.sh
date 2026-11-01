#!/usr/bin/env bash
# Rust builds for tallow, from pinned container images. Nothing is installed on
# the host. The mesh cargo cache lands in gitignored firmware/mesh/. The cam
# image and its ESP-IDF tools land in gitignored firmware/cam/.
#
# Usage:
#   tools/mesh.sh check [--fast]       every offline gate; --fast skips the firmware builds
#   tools/mesh.sh fmt                  format all Rust in place
#   tools/mesh.sh test                 mesh core tests, on this machine
#   tools/mesh.sh daemon check         fmt, clippy, and tests for host/
#   tools/mesh.sh daemon build         release build of tallowd
#                                      (both run in tallow-rust-host, built from
#                                      tools/rust-host.Dockerfile: the pinned image plus libdbus)
#   tools/mesh.sh daemon run /dev/ttyUSB0 [tallowd run arguments]
#                                      run the release build on the root's port, such as
#                                      --out data/mesh/map.json, or --trial --out data/mesh/trial.json.
#                                      The container gets the port by --device, as host does, and
#                                      writes only data/mesh. tools/setup_mesh_host.sh runs it as a user service.
#   tools/mesh.sh key                  write the gitignored firmware/mesh/mesh.key once
#   tools/mesh.sh ota-key              write the gitignored ota.key (private) and tracked ota.pub, once each
#   tools/mesh.sh build                release image for a classic ESP32
#   tools/mesh.sh size                 image size against the OTA slot
#   tools/mesh.sh cam-build            release image for an ESP32-S3 cam
#   tools/mesh.sh cam-size             cam image size against its OTA slot
#   tools/mesh.sh ports                list attached boards; opens no port, resets nothing
#   tools/mesh.sh flash /dev/ttyUSB0   write the image to one classic ESP32
#   tools/mesh.sh monitor /dev/ttyUSB0 [seconds]   print a board's serial output
#   tools/mesh.sh bench /dev/ttyUSB0 [--deny <id-hex>]... [--seconds N]   refuse root ids, print its @ lines
#   tools/mesh.sh backup /dev/ttyUSB0  save a board's whole flash to data/firmware-backups/
#   tools/mesh.sh host /dev/ttyUSB0 [mesh_host.py arguments]
#
# Opening a CP2102 port resets that board, so flash, monitor, backup, and host all
# reboot it. These four only touch a port that firmware/mesh/boards.tsv lists as a
# classic ESP32: opening an S3 cam's port can drop it into download mode, and its
# CH340 port is a ttyUSB too. Every command exits non-zero when its step fails.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="espressif/idf-rust:esp32_1.95.0.0"
CAM_IMAGE="espressif/idf-rust:esp32s3_1.95.0.0"
MESH="${ROOT}/firmware/mesh"
CAM="${ROOT}/firmware/cam"
CAM_ELF="target/xtensa-esp32s3-espidf/release/tallow-cam"
# The mesh node's already-checked-out IDF, so the cam build does not clone it.
CAM_IDF="${MESH}/node/.embuild/espressif/esp-idf/v5.5.5"
KEY="${MESH}/mesh.key"
# The OTA signing pair. ota.key is the private seed and is gitignored. ota.pub is
# the public key, tracked, which firmware bakes in at build time.
OTA_KEY="${MESH}/ota.key"
OTA_PUB="${MESH}/ota.pub"
BOARDS="${MESH}/boards.tsv"
HOST_IMAGE="ghcr.io/esphome/esphome:2026.9.1"
# The host daemon's image: the pinned Rust image plus libdbus for bluer. It is built
# locally from the Dockerfile, under its own tag, and no other command uses it.
RUST_HOST_IMAGE="tallow-rust-host:esp32_1.95.0.0"
RUST_HOST_DOCKERFILE="${ROOT}/tools/rust-host.Dockerfile"
ELF="target/xtensa-esp32-espidf/release/tallow-node"
FLASH_TRIES=3

die() { printf '%s\n' "$*" >&2; exit 1; }

run_image() { run_image_from "$IMAGE" "$@"; }

run_image_from() {
  local image="$1" workdir="$2"; shift 2
  mkdir -p "${MESH}/.cache/registry" "${MESH}/.cache/git"
  local -a args=(--rm --user "$(id -u):$(id -g)" -w "$workdir"
    -v "${MESH}:/mesh"
    -v "${MESH}:/repo/firmware/mesh"
    -v "${ROOT}/host:/repo/host"
    -v "${ROOT}/app:/repo/app:ro"
    -v "${MESH}/.cache/registry:/home/esp/.cargo/registry"
    -v "${MESH}/.cache/git:/home/esp/.cargo/git"
    -e HOME=/home/esp)
  [[ -r "$KEY" ]] && args+=(-v "${KEY}:/run/mesh-key:ro")
  if [[ -n "${MESH_DEVICE:-}" ]]; then
    args+=(--device "${MESH_DEVICE}:${MESH_DEVICE}" --group-add "$(stat -c %g "$MESH_DEVICE")")
  fi
  docker run "${args[@]}" "$image" "$@"
}

# Runs a shell line in the image. A failure anywhere in a pipe fails the step.
in_image() {
  local workdir="$1"; shift
  run_image "$workdir" bash -lc "set -euo pipefail; $*"
}

in_image_from() {
  local image="$1" workdir="$2"; shift 2
  run_image_from "$image" "$workdir" bash -lc "set -euo pipefail; $*"
}

# The cam crate is a separate image and a separate tree. It does not mount the
# mesh key and it does not write into the mesh crates.
run_cam_image() {
  local workdir="$1"; shift
  mkdir -p "${MESH}/.cache/registry" "${MESH}/.cache/git"
  local -a args=(--rm --user "$(id -u):$(id -g)" -w "$workdir"
    -v "${CAM}:/cam"
    -v "${MESH}/.cache/registry:/home/esp/.cargo/registry"
    -v "${MESH}/.cache/git:/home/esp/.cargo/git"
    -e HOME=/home/esp)
  if [[ -d "$CAM_IDF" ]]; then
    args+=(-v "${CAM_IDF}:/idf:ro" -e IDF_PATH=/idf)
  fi
  docker run "${args[@]}" "$CAM_IMAGE" "$@"
}

in_cam() {
  run_cam_image /cam bash -lc "set -euo pipefail; $*"
}

need_key() {
  [[ -r "$KEY" ]] || die "No mesh key. Run: tools/mesh.sh key"
}

# The USB path of a port, as firmware/mesh/boards.tsv records it. Opens no port.
usb_path() {
  udevadm info -q property -n "$1" | sed -n 's/^ID_PATH=.*usb-0:\([0-9.]*\):.*/\1/p'
}

# The boards.tsv column for a USB path: 1 is the name, 3 the chip.
board_field() {
  awk -F'\t' -v p="$1" -v f="$2" '!/^#/ && $4 == p {print $f}' "$BOARDS"
}

# The mesh image is built for the classic ESP32. An S3 cam shows up as ttyACM on
# its native USB, and as a ttyUSB on its CH340, so the name alone proves nothing.
need_classic_port() {
  local port="$1" path chip
  [[ -e "$port" ]] || die "${port} is not there. Run: tools/mesh.sh ports"
  [[ "$port" == /dev/ttyUSB* ]] || die "${port} is not a classic ESP32 port. The cams are not mesh nodes."
  path="$(usb_path "$port")"
  chip="$(board_field "$path" 3)"
  [[ -n "$chip" ]] || die "${port} (USB path ${path:-unknown}) is not in firmware/mesh/boards.tsv. Read the board with tools/esphome.sh mac and add it first."
  [[ "$chip" == esp32 ]] || die "${port} is $(board_field "$path" 1), a ${chip}. The cams are not mesh nodes."
}

show_ports() {
  local port path name
  shopt -s nullglob
  printf '%-14s %-12s %-10s %s\n' PORT BOARD USB-PATH DEVICE
  for port in /dev/ttyUSB* /dev/ttyACM*; do
    path="$(usb_path "$port")"
    name="$(board_field "$path" 1)"
    printf '%-14s %-12s %-10s %s\n' "$port" "${name:-unknown}" "${path:-?}" \
      "$(udevadm info -q property -n "$port" | sed -n 's/^ID_MODEL=//p')"
  done
}

# Older boards drop out at high speed, so a failed read falls back to a slow one.
run_backup() {
  local port="$1" out="${ROOT}/data/firmware-backups" mac name baud
  local -a esptool=(docker run --rm --user "$(id -u):$(id -g)" --device "${port}:${port}"
    --group-add "$(stat -c %g "$port")" -v "${out}:/out" --entrypoint esptool "$HOST_IMAGE" --port "$port")
  # A mesh board's flash holds the mesh key, so a backup is as private as the key.
  mkdir -p "$out"
  chmod 700 "$out"
  mac="$("${esptool[@]}" read-mac 2>&1 | sed -n 's/^MAC: *//p' | head -1 | tr -d ':')"
  [[ -n "$mac" ]] || die "Could not read a MAC from ${port}."
  name="${mac}-$(date +%F).bin"
  [[ -e "${out}/${name}" ]] && die "${out}/${name} already exists."
  for baud in 460800 115200; do
    if "${esptool[@]}" --baud "$baud" read-flash 0 ALL "/out/${name}" >/dev/null 2>&1; then
      chmod 600 "${out}/${name}"
      echo "Saved ${out}/${name} ($(stat -c %s "${out}/${name}") bytes)."
      return 0
    fi
    rm -f "${out}/${name}"
    echo "Backup at ${baud} failed on ${port}." >&2
  done
  die "Backup failed on ${port}. Do not flash this board until one succeeds."
}

# The built image embeds the mesh key, and cargo makes it world-readable.
hide_node_target() {
  [[ -d "${MESH}/node/target" ]] && chmod go= "${MESH}/node/target"
  return 0
}

# Builds the daemon's image from the Dockerfile on stdin. Cached layers make this quick.
# The base image's apt step needs the network the first time only.
build_rust_host_image() {
  docker build -q -t "$RUST_HOST_IMAGE" - <"$RUST_HOST_DOCKERFILE" >/dev/null \
    || die "Could not build ${RUST_HOST_IMAGE} from ${RUST_HOST_DOCKERFILE}."
}

# Runs the release build of tallowd on one classic board's port. The image is the
# one that built it, so its libdbus is there. Only data/mesh is writable: host/ is
# mounted read-only and the mesh key is not mounted at all. The port reaches the
# container by --device, with the device's group added, as `host` does, so this
# works for a user outside dialout. The udev rule from tools/setup_mesh_host.sh
# also gives this user the port by its ACL.
run_daemon_run() {
  local port="$1"; shift
  need_classic_port "$port"
  [[ -x "${ROOT}/host/target/release/tallowd" ]] || die "No release build of tallowd. Run: tools/mesh.sh daemon build"
  mkdir -p "${ROOT}/data/mesh"
  build_rust_host_image
  docker run --rm --user "$(id -u):$(id -g)" -w /repo \
    --device "${port}:${port}" --group-add "$(stat -c %g "$port")" \
    -v "${ROOT}/host:/repo/host:ro" -v "${ROOT}/data/mesh:/repo/data/mesh" \
    "$RUST_HOST_IMAGE" /repo/host/target/release/tallowd run --port "$port" "$@"
}

run_daemon() {
  case "${1:-}" in
    check)
      build_rust_host_image
      in_image_from "$RUST_HOST_IMAGE" /repo/host 'cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test 2>&1'
      ;;
    build)
      build_rust_host_image
      in_image_from "$RUST_HOST_IMAGE" /repo/host 'cargo build --release 2>&1 | tail -20'
      ;;
    run)
      shift
      [[ -n "${1:-}" ]] || die "Name the port: tools/mesh.sh daemon run /dev/ttyUSBn [arguments]"
      run_daemon_run "$1" "${@:2}"
      ;;
    *) die "Usage: tools/mesh.sh daemon check|build|run <port> [arguments]" ;;
  esac
}

# Makes the OTA signing pair. The private seed is made and read inside a container
# and comes back to this shell only as hex, so it is never printed. It is written
# to $1 as mode 0600, and the public key to $2. Both paths must not exist yet.
make_ota_key() {
  local key="$1" pub="$2" pair seed public
  [[ -e "$key" ]] && die "${key} already exists."
  [[ -e "$pub" ]] && die "${pub} already exists."
  pair="$(docker run --rm --user "$(id -u):$(id -g)" --entrypoint bash "$IMAGE" -c '
    set -euo pipefail
    umask 077
    dir="$(mktemp -d)"
    openssl genpkey -algorithm ED25519 -outform DER -out "$dir/seed.der"
    openssl pkey -inform DER -in "$dir/seed.der" -pubout -outform DER -out "$dir/pub.der"
    tail -c 32 "$dir/seed.der" | od -An -tx1 -v | tr -d " \n"; echo
    tail -c 32 "$dir/pub.der" | od -An -tx1 -v | tr -d " \n"; echo
  ')"
  seed="$(sed -n 1p <<<"$pair")"
  public="$(sed -n 2p <<<"$pair")"
  [[ "$seed" =~ ^[0-9a-f]{64}$ && "$public" =~ ^[0-9a-f]{64}$ ]] \
    || die "openssl did not return a 32-byte seed and public key. Nothing was written."
  mkdir -p "$(dirname "$key")"
  ( umask 077; set -C; printf '%s\n' "$seed" >"$key" )
  printf '%s\n' "$public" >"$pub"
  echo "Wrote ${key} (mode 0600, never commit it) and ${pub} (tracked)."
}

run_check() {
  local fast="${1:-}"
  echo "== core: fmt, clippy, tests"
  in_image /mesh/core 'cargo fmt --check && cargo clippy --all-targets --target x86_64-unknown-linux-gnu -- -D warnings && cargo test --target x86_64-unknown-linux-gnu 2>&1 | grep -E "^test result|FAILED|panicked|error"'
  echo "== daemon: fmt, clippy, tests"
  local output
  output="$(run_daemon check 2>&1)" || { tail -30 <<<"$output" >&2; die "daemon check failed"; }
  grep -E '^test result' <<<"$output"
  echo "== host tool: unit tests"
  ( cd "$ROOT" && .venv/bin/python -m unittest tools/tests/test_mesh_host.py tools/tests/test_mesh_bench.py tools/tests/test_map_fixture.py 2>&1 ) | tail -1
  echo "== panel: unit tests, HTTP contract"
  ( cd "$ROOT" && .venv/bin/python -m unittest tools/tests/test_h7020_panel.py tools/tests/test_h7020_library.py \
    tools/tests/test_cams.py tools/tests/test_cams_page.py tools/tests/test_panel_broker.py \
    tools/tests/test_library_fixture.py tools/tests/test_devices_page.py 2>&1 ) | tail -1
  ( cd "$ROOT" && .venv/bin/python tools/tests/panel_contract.py 2>&1 ) | head -1
  echo "== node and cam: fmt"
  in_image /mesh/node 'cargo fmt --check'
  in_cam 'cargo fmt --check'
  if [[ "$fast" != "--fast" ]]; then
    need_key
    echo "== node: firmware build"
    in_image /mesh/node 'cargo build --release 2>&1 | grep -E "^(warning: unused|error)|Finished"'
    hide_node_target
    echo "== cam: firmware build"
    in_cam 'cargo build --release 2>&1 | grep -E "^(warning: unused|error)|Finished"'
    echo "== tallowd: release build, same HTTP contract as the Python"
    run_daemon build | tail -1
    ( cd "$ROOT" && .venv/bin/python tools/tests/panel_contract.py --rust-only --panel-command 'host/target/release/tallowd panel --preview' 2>&1 ) | head -1
  fi
  echo "check passed"
}

case "${1:-}" in
  "" | -h | --help)
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
    ;;
  key)
    [[ -e "$KEY" ]] && die "${KEY} already exists."
    ( umask 077; mkdir -p "$(dirname "$KEY")"; od -An -tx1 -N32 /dev/urandom | tr -d ' \n' >"$KEY"; echo >>"$KEY" )
    echo "Wrote ${KEY} (mode 0600). Every node built with it can join."
    ;;
  ota-key)
    make_ota_key "$OTA_KEY" "$OTA_PUB"
    ;;
  check)
    run_check "${2:-}"
    ;;
  fmt)
    in_image /mesh/core 'cargo fmt'
    in_image /mesh/node 'cargo fmt'
    in_image /repo/host 'cargo fmt --all'
    in_cam 'cargo fmt'
    ;;
  test)
    in_image /mesh/core 'cargo test --target x86_64-unknown-linux-gnu -- --nocapture 2>&1'
    ;;
  daemon)
    run_daemon "${@:2}"
    ;;
  build)
    need_key
    in_image /mesh/node 'cargo build --release 2>&1 | tail -40'
    hide_node_target
    ;;
  size)
    in_image /mesh/node "espflash save-image --chip esp32 --flash-size 4mb --partition-table partitions.csv ${ELF} /tmp/app.bin 2>&1 | grep -E 'App/part'"
    ;;
  cam-build)
    in_cam 'cargo build --release 2>&1 | tail -40'
    ;;
  cam-size)
    in_cam "espflash save-image --chip esp32s3 --flash-size 16mb --partition-table partitions.csv ${CAM_ELF} /tmp/app.bin 2>&1 | grep -E 'App/part'"
    ;;
  ports)
    show_ports
    ;;
  flash)
    port="${2:?Name the port}"
    need_classic_port "$port"
    export MESH_DEVICE="$port"
    for try in $(seq 1 "$FLASH_TRIES"); do
      if output="$(run_image /mesh/node bash -lc "espflash flash --non-interactive --chip esp32 --port ${port} --partition-table partitions.csv --flash-size 4mb ${ELF}" 2>&1)" \
        && grep -q 'Flashing has completed' <<<"$output"; then
        echo "Flashed ${port}."
        exit 0
      fi
      echo "Flash try ${try} of ${FLASH_TRIES} failed on ${port}." >&2
      tail -3 <<<"$output" >&2
      sleep 2
    done
    die "Flash failed on ${port}. The board may hold a partial image. Fix the cable or port before retrying."
    ;;
  monitor)
    port="${2:?Name the port}"
    seconds="${3:-20}"
    need_classic_port "$port"
    docker run --rm --device "${port}:${port}" --group-add "$(stat -c %g "$port")" \
      -v "${ROOT}/tools:/tools:ro" --entrypoint python3 "$HOST_IMAGE" \
      /tools/mesh_monitor.py "$port" "$seconds"
    ;;
  bench)
    port="${2:?Name the port}"
    shift 2
    need_classic_port "$port"
    docker run --rm --device "${port}:${port}" --group-add "$(stat -c %g "$port")" \
      -v "${ROOT}/tools:/tools:ro" --entrypoint python3 "$HOST_IMAGE" \
      /tools/mesh_bench.py "$port" "$@"
    ;;
  backup)
    port="${2:?Name the port}"
    need_classic_port "$port"
    run_backup "$port"
    ;;
  host)
    device="${2:?Name the port}"
    shift 2
    need_classic_port "$device"
    mkdir -p "${ROOT}/data/mesh"
    docker run --rm --user "$(id -u):$(id -g)" --device "${device}:${device}" --group-add "$(stat -c %g "$device")" \
      -v "${ROOT}/tools:/tools:ro" -v "${ROOT}/data/mesh:/data" --entrypoint python3 "$HOST_IMAGE" \
      /tools/mesh_host.py --port "$device" --out /data/map.json "$@"
    ;;
  *)
    die "Unknown command ${1}. Run: tools/mesh.sh --help"
    ;;
esac
