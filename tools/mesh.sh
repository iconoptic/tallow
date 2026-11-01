#!/usr/bin/env bash
# Rust builds for tallow, from pinned container images. Nothing is installed on
# the host. The mesh cargo cache lands in gitignored firmware/mesh/. The cam
# image and its ESP-IDF tools land in gitignored firmware/cam/.
#
# Usage:
#   tools/mesh.sh check [--fast]       every offline gate; --fast skips firmware clippy and the firmware builds
#   tools/mesh.sh mutants              mutation tests for the mesh core, the host senders
#                                      and broker, and the cam's push logic.
#                                      Slow, so check does not run them.
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
#   tools/mesh.sh daemon gatt /dev/ttyUSB0 [tallowd gatt arguments]
#                                      GATT on the root's port. The same container and port as
#                                      daemon run, then `tallowd gatt --port`. `subscribe <handle>
#                                      --listen <s>` keeps printing notifications for s seconds.
#   tools/mesh.sh key                  write the gitignored firmware/mesh/mesh.key once
#   tools/mesh.sh ota-key              write the gitignored ota.key (private) and tracked ota.pub, once each
#   tools/mesh.sh ota-image            write the node app image to firmware/mesh/node/target/ota.bin
#   tools/mesh.sh ota-sign <image> <node|cam0> <version>
#                                      sign <image> for that product and version with ota.key;
#                                      write the 69-byte <image>.sig: product, version, signature.
#                                      A board refuses a version below the highest it has confirmed.
#   tools/mesh.sh daemon ota /dev/ttyUSB0 --target <id> --image <file> --sig <file>
#                                      send that signed image; Begin takes the product and version
#                                      from the .sig. The container gets the port as daemon gatt
#                                      does, and the named image and signature only.
#   tools/mesh.sh build                release image for a classic ESP32
#   tools/mesh.sh size                 image size against the OTA slot
#   tools/mesh.sh cam-build            release image for an ESP32-S3 cam
#   tools/mesh.sh cam-size             cam image size against its OTA slot
#   tools/mesh.sh cam-image            write the cam app image to firmware/cam/target/ota.bin
#   tools/mesh.sh cam-flash <port>     write the cam image and this build's bootloader, and
#                                      erase otadata, on the port boards.tsv names toby-cam-0
#   tools/mesh.sh cam-backup <port>    save that cam's whole flash
#   tools/mesh.sh cam-frame            print one host-built multipart sample on stdout
#   tools/mesh.sh cam-push <address> <image>
#                                      send <image> and <image>.sig to 10.87.0.23:3232 and
#                                      wait for the cam's result. Exits non-zero on anything
#                                      but ok. Refuses every other address. Does not read ota.key.
#   tools/mesh.sh ports                list attached boards; opens no port, resets nothing
#   tools/mesh.sh flash /dev/ttyUSB0   write the image and this build's bootloader to one classic ESP32
#   tools/mesh.sh monitor /dev/ttyUSB0 [seconds]   print a board's serial output
#   tools/mesh.sh bench /dev/ttyUSB0 [--deny <id-hex>]... [--seconds N]   refuse root ids, print its @ lines
#   tools/mesh.sh backup /dev/ttyUSB0  save a board's whole flash to data/firmware-backups/
#
# Opening a CP2102 port resets that board, so flash, monitor, backup, bench,
# daemon run, daemon gatt, and daemon ota all reboot it. These seven only touch a
# port that firmware/mesh/boards.tsv lists as a classic ESP32: opening an S3 cam's
# port can drop it into download mode, and its CH340 port is a ttyUSB too. Every
# command exits non-zero when its step fails.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="docker.io/espressif/idf-rust:esp32_1.95.0.0"
CAM_IMAGE="docker.io/espressif/idf-rust:esp32s3_1.95.0.0"
MESH="${ROOT}/firmware/mesh"
CAM="${ROOT}/firmware/cam"
CAM_ELF="target/xtensa-esp32s3-espidf/release/tallow-cam"
# Wi-Fi credentials for the cam image. The build reads this one file. It is
# never printed. mesh.key is not mounted into the cam build.
CAM_SECRETS="${ROOT}/firmware/cam/secrets.yaml"
# The mesh node's already-checked-out IDF, so the cam build does not clone it.
CAM_IDF="${MESH}/node/.embuild/espressif/esp-idf/v5.5.5"
KEY="${MESH}/mesh.key"
# The OTA signing pair. ota.key is the private seed and is gitignored. ota.pub is
# the public key, tracked. The node build bakes in the mesh key and ota.pub.
# run_image_from mounts the directory that holds ota.key, so it covers that one
# file with /dev/null. Only ota-sign mounts the key itself, in its own container.
OTA_KEY="${MESH}/ota.key"
OTA_PUB="${MESH}/ota.pub"
BOARDS="${MESH}/boards.tsv"
HOST_IMAGE="ghcr.io/esphome/esphome:2026.9.1"
# The host daemon's image: the pinned Rust image plus libdbus for bluer. It is built
# locally from the Dockerfile, under its own tag, and no other command uses it.
RUST_HOST_IMAGE="localhost/tallow-rust-host:esp32_1.95.0.0"
RUST_HOST_DOCKERFILE="${ROOT}/tools/rust-host.Dockerfile"
ELF="target/xtensa-esp32-espidf/release/tallow-node"
FLASH_TRIES=3
HARDEN=(--cap-drop=all --security-opt=no-new-privileges)

die() { printf '%s\n' "$*" >&2; exit 1; }

run_image() { run_image_from "$IMAGE" "$@"; }

run_image_from() {
  local image="$1" workdir="$2"; shift 2
  mkdir -p "${MESH}/.cache/registry" "${MESH}/.cache/git"
  local -a args=(--rm "${HARDEN[@]}" --userns=keep-id --user "$(id -u):$(id -g)" -w "$workdir"
    -v "${MESH}:/mesh"
    -v "${MESH}:/repo/firmware/mesh"
    -v "${ROOT}/host:/repo/host"
    -v "${ROOT}/app:/repo/app:ro"
    -v "${MESH}/.cache/registry:/home/esp/.cargo/registry"
    -v "${MESH}/.cache/git:/home/esp/.cargo/git"
    -e HOME=/home/esp)
  # Masks only a file that exists: podman refuses a missing bind source.
  [[ -e "$OTA_KEY" ]] && args+=(-v /dev/null:/mesh/ota.key:ro -v /dev/null:/repo/firmware/mesh/ota.key:ro)
  [[ -r "$KEY" ]] && args+=(-v "${KEY}:/run/mesh-key:ro")
  [[ -r "$OTA_PUB" ]] && args+=(-v "${OTA_PUB}:/run/ota.pub:ro")
  if [[ -n "${MESH_DEVICE:-}" ]]; then
    args+=(--device "${MESH_DEVICE}:${MESH_DEVICE}" --group-add keep-groups)
  fi
  podman run "${args[@]}" "$image" "$@"
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
# mesh key, it does not mount firmware/mesh itself, and it does not write into
# the mesh crates. The two mesh mounts are the core crate and ota.pub.
# The cam build bakes wifi_ssid and wifi_password. A missing file stops here,
# before podman refuses a missing bind source.
need_cam_secrets() {
  [[ -f "$CAM_SECRETS" ]] || die "firmware/cam/secrets.yaml is missing. It must hold wifi_ssid and wifi_password."
}

need_cam_ota() {
  [[ -f "$OTA_PUB" ]] || die "firmware/mesh/ota.pub is missing. Run: tools/mesh.sh ota-key"
  [[ -d "${MESH}/core" ]] || die "firmware/mesh/core is not there"
}

run_cam_image() {
  need_cam_secrets
  cam_container "$CAM_SECRETS" "$@"
}

# The same container with no Wi-Fi secrets, for steps that build nothing.
run_cam_plain() {
  cam_container "" "$@"
}

cam_container() {
  local secrets="$1" workdir="$2"; shift 2
  need_cam_ota
  hide_cam_target
  mkdir -p "${MESH}/.cache/registry" "${MESH}/.cache/git"
  local -a args=(--rm "${HARDEN[@]}" --userns=keep-id --user "$(id -u):$(id -g)" -w "$workdir"
    -v "${CAM}:/cam"
    -v "${MESH}/.cache/registry:/home/esp/.cargo/registry"
    -v "${MESH}/.cache/git:/home/esp/.cargo/git"
    -v "${MESH}/core:/mesh/core:ro"
    -v "${OTA_PUB}:/run/ota.pub:ro"
    -e HOME=/home/esp)
  [[ -n "$secrets" ]] && args+=(-v "${secrets}:/run/secrets.yaml:ro")
  if [[ -d "$CAM_IDF" ]]; then
    args+=(-v "${CAM_IDF}:/idf:ro" -e IDF_PATH=/idf)
  fi
  if [[ -n "${CAM_DEVICE:-}" ]]; then
    args+=(--device "${CAM_DEVICE}:${CAM_DEVICE}" --group-add keep-groups)
  fi
  podman run "${args[@]}" "$CAM_IMAGE" "$@"
}

in_cam() {
  run_cam_image /cam bash -lc "set -euo pipefail; $*"
}

in_cam_plain() {
  run_cam_plain /cam bash -lc "set -euo pipefail; $*"
}

need_key() {
  [[ -r "$KEY" ]] || die "No mesh key. Run: tools/mesh.sh key"
}

# The USB path of a port, as firmware/mesh/boards.tsv records it. Opens no port.
usb_path() {
  udevadm info -q property -n "$1" | sed -n 's/^ID_PATH=.*usb-0:\([0-9.]*\):.*/\1/p'
}

# The boards.tsv column for a USB path: 1 is the name, 3 the chip. The empty
# strings make awk compare text: as numbers, path 1.10 would match a row 1.1.
board_field() {
  awk -F'\t' -v p="$1" -v f="$2" '!/^#/ && p != "" && $4"" == p"" {print $f}' "$BOARDS"
}

# The mesh image is built for the classic ESP32. An S3 cam shows up as ttyACM on
# its native USB, and as a ttyUSB on its CH340, so the name alone proves nothing.
need_classic_port() {
  local port="$1" path chip
  [[ -e "$port" ]] || die "${port} is not there. Run: tools/mesh.sh ports"
  [[ "$port" == /dev/ttyUSB* ]] || die "${port} is not a classic ESP32 port. The cams are not mesh nodes."
  path="$(usb_path "$port")"
  chip="$(board_field "$path" 3)"
  [[ -n "$chip" ]] || die "${port} (USB path ${path:-unknown}) is not in firmware/mesh/boards.tsv. Read the board with tools/mesh.sh ports and add it first."
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
  local -a esptool=(podman run --rm "${HARDEN[@]}" --userns=keep-id --user "$(id -u):$(id -g)" --device "${port}:${port}"
    --group-add keep-groups -v "${out}:/out" --entrypoint esptool "$HOST_IMAGE" --port "$port")
  # A mesh board's flash holds the mesh key, so a backup is as private as the key.
  mkdir -p "$out"
  chmod 700 "$out"
  local output
  output="$("${esptool[@]}" read-mac 2>&1)" || true
  mac="$(sed -n 's/^MAC: *//p' <<<"$output" | head -1 | tr -d ':')"
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

# The built image embeds the mesh key, and cargo makes it world-readable. Called
# before each build, so a failed first build never leaves a readable tree.
hide_node_target() {
  mkdir -p "${MESH}/node/target"
  chmod 700 "${MESH}/node/target"
}

# The cam image embeds the Wi-Fi password. run_cam_image calls this before each
# container, so a failed first build never leaves a readable tree.
hide_cam_target() {
  mkdir -p "${CAM}/target"
  chmod 700 "${CAM}/target"
}

# 0 when column 4 of the boards file is this USB path and column 1 is toby-cam-0.
# Opens nothing. A test passes a stub table.
cam_port_ok() {
  local boards="$1" path="$2" name
  name="$(awk -F'\t' -v p="$path" '!/^#/ && p != "" && $4"" == p"" { print $1; exit }' "$boards")"
  [[ "$name" == "toby-cam-0" ]]
}

# 0 only for toby-cam-0. cam-push opens no socket from a test of this predicate.
cam_allows_address() {
  [[ "${1:-}" == "10.87.0.23" ]]
}

# 0 when the argument is a device path. An address is refused.
cam_takes_port() {
  local arg="${1:-}"
  [[ -n "$arg" ]] || return 1
  [[ "$arg" == /dev/* ]] || return 1
  [[ "$arg" != *.*.*.* ]] || return 1
  return 0
}

# cam-flash and cam-backup: one port, and only the row named toby-cam-0.
need_cam0_port() {
  local port="$1" path
  echo "Hold BOOT and tap RST first."
  cam_takes_port "$port" || die "${port} is not a cam port. cam-flash and cam-backup take a port, not an address."
  [[ -e "$port" ]] || die "${port} is not there. Run: tools/mesh.sh ports"
  path="$(usb_path "$port")"
  cam_port_ok "$BOARDS" "$path" || die "${port} (USB path ${path:-unknown}) is not toby-cam-0. cam-flash and cam-backup refuse every other board."
}

# Builds the daemon's image from tools/rust-host.Dockerfile, with tools/ as the context.
# Cached layers make this quick. The base image's apt step needs the network the first time only.
build_rust_host_image() {
  podman build -q -t "$RUST_HOST_IMAGE" -f "$RUST_HOST_DOCKERFILE" "${ROOT}/tools" >/dev/null \
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
  podman run --rm "${HARDEN[@]}" --userns=keep-id --user "$(id -u):$(id -g)" -w /repo \
    --device "${port}:${port}" --group-add keep-groups \
    -v "${ROOT}/host:/repo/host:ro" -v "${ROOT}/data/mesh:/repo/data/mesh" \
    "$RUST_HOST_IMAGE" /repo/host/target/release/tallowd run --port "$port" "$@"
}

# GATT through the same container and port as `daemon run`. The arguments after
# the port are `tallowd gatt`'s, without repeating `--port`.
run_daemon_gatt() {
  local port="$1"; shift
  need_classic_port "$port"
  [[ -x "${ROOT}/host/target/release/tallowd" ]] || die "No release build of tallowd. Run: tools/mesh.sh daemon build"
  mkdir -p "${ROOT}/data/mesh"
  build_rust_host_image
  podman run --rm "${HARDEN[@]}" --userns=keep-id --user "$(id -u):$(id -g)" -w /repo \
    --device "${port}:${port}" --group-add keep-groups \
    -v "${ROOT}/host:/repo/host:ro" -v "${ROOT}/data/mesh:/repo/data/mesh" \
    "$RUST_HOST_IMAGE" /repo/host/target/release/tallowd gatt --port "$port" "$@"
}

# Sends one signed image through the same container and port as `daemon gatt`.
# The image and the signature are the two files named here. ota.key is not
# mounted, and neither is the rest of firmware/mesh.
run_daemon_ota() {
  local port="$1"; shift
  need_classic_port "$port"
  [[ -x "${ROOT}/host/target/release/tallowd" ]] || die "No release build of tallowd. Run: tools/mesh.sh daemon build"
  local image="" sig=""
  local -a rest=()
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --image)
        image="${2:?--image needs a file}"
        shift 2
        ;;
      --sig)
        sig="${2:?--sig needs a file}"
        shift 2
        ;;
      *)
        rest+=("$1")
        shift
        ;;
    esac
  done
  [[ -r "$image" ]] || die "Name a readable image: --image <file>"
  [[ -r "$sig" ]] || die "Name a readable signature: --sig <file>"
  image="$(readlink -f "$image")"
  sig="$(readlink -f "$sig")"
  mkdir -p "${ROOT}/data/mesh"
  build_rust_host_image
  podman run --rm "${HARDEN[@]}" --userns=keep-id --user "$(id -u):$(id -g)" -w /repo \
    --device "${port}:${port}" --group-add keep-groups \
    -v "${ROOT}/host:/repo/host:ro" -v "${ROOT}/data/mesh:/repo/data/mesh" \
    -v "${image}:/run/ota-image:ro" -v "${sig}:/run/ota-image.sig:ro" \
    "$RUST_HOST_IMAGE" /repo/host/target/release/tallowd ota \
    --port "$port" --image /run/ota-image --sig /run/ota-image.sig "${rest[@]}"
}

# Signs one image with firmware/mesh/ota.key. The message is what `signed_message`
# in firmware/mesh/core/src/ota.rs builds: "tallow-ota-v1", the product byte, the
# version little-endian, and the image's sha256. The result is 69 bytes: product,
# version, signature. The key is missing: this returns before mounting anything.
# The container sees that one key and the image, both read-only, and the DER copy
# of the seed stays on a tmpfs that dies with it. The seed is not an argument and
# is not printed. Nothing is left at <image>.sig unless every step worked.
sign_ota_image() {
  local image="$1" name="$2" version="$3" product sig partial
  case "$name" in
    node) product=1 ;;
    cam0) product=2 ;;
    *) die "Name the product: node or cam0" ;;
  esac
  [[ "$version" =~ ^[1-9][0-9]{0,9}$ ]] && (( version <= 4294967295 )) \
    || die "The version is a whole number from 1 to 4294967295, not ${version}"
  [[ -r "$image" ]] || die "Cannot read ${image}"
  image="$(readlink -f "$image")"
  sig="${image}.sig"
  partial="${sig}.partial"
  trap 'rm -f "$partial"' EXIT
  umask 077
  podman run --rm "${HARDEN[@]}" --userns=keep-id --user "$(id -u):$(id -g)" \
    --tmpfs "/tmp:mode=0700,uid=$(id -u),gid=$(id -g)" \
    -v "${OTA_KEY}:/run/ota.key:ro" -v "${image}:/run/image:ro" \
    --entrypoint bash "$IMAGE" -lc '
      set -euo pipefail
      umask 077
      dir="$(mktemp -d)"
      trap "rm -rf \"$dir\"" EXIT
      python3 - "$dir" "$1" "$2" << "PY"
import hashlib, pathlib, struct, sys
out, product, version = pathlib.Path(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3])
text = pathlib.Path("/run/ota.key").read_text().split()
try:
    raw = bytes.fromhex("".join(text))
except ValueError:
    sys.exit(1)
if len(raw) != 32:
    sys.exit(1)
prefix = bytes.fromhex("302e020100300506032b657004220420")
(out / "key.der").write_bytes(prefix + raw)
digest = hashlib.sha256(pathlib.Path("/run/image").read_bytes()).digest()
head = struct.pack("<BI", product, version)
(out / "head").write_bytes(head)
(out / "message").write_bytes(b"tallow-ota-v1" + head + digest)
PY
      openssl pkeyutl -sign -rawin -inkey "$dir/key.der" -keyform DER -in "$dir/message" -out "$dir/sig"
      test "$(wc -c < "$dir/sig")" -eq 64
      cat "$dir/head" "$dir/sig"
    ' sign "$product" "$version" >"$partial" || die "Signing failed. Nothing was kept."
  [[ "$(wc -c < "$partial")" -eq 69 ]] || die "Signing failed. Nothing was kept."
  mv -f "$partial" "$sig"
  trap - EXIT
  echo "Wrote ${sig}"
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
    gatt)
      shift
      [[ -n "${1:-}" ]] || die "Name the port: tools/mesh.sh daemon gatt /dev/ttyUSBn [arguments]"
      run_daemon_gatt "$1" "${@:2}"
      ;;
    ota)
      shift
      [[ -n "${1:-}" ]] || die "Name the port: tools/mesh.sh daemon ota /dev/ttyUSBn --target <id> --image <file> --sig <file>"
      run_daemon_ota "$1" "${@:2}"
      ;;
    *) die "Usage: tools/mesh.sh daemon check|build|run <port> [arguments]|gatt <port> [arguments]|ota <port> --target <id> --image <file> --sig <file>" ;;
  esac
}

# Makes the OTA signing pair. The private seed is made and read inside a container
# and comes back to this shell only as hex, so it is never printed. It is written
# to $1 as mode 0600, and the public key to $2. Both paths must not exist yet.
make_ota_key() {
  local key="$1" pub="$2" pair seed public
  [[ -e "$key" ]] && die "${key} already exists."
  [[ -e "$pub" ]] && die "${pub} already exists."
  pair="$(podman run --rm "${HARDEN[@]}" --userns=keep-id --user "$(id -u):$(id -g)" --entrypoint bash "$IMAGE" -c '
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
  ( cd "$ROOT" && .venv/bin/python -m unittest tools/tests/test_mesh_bench.py \
    tools/tests/test_mesh_tool.py tools/tests/test_parse_btsnoop.py \
    tools/tests/test_ble_discover.py 2>&1 ) | tail -1
  echo "== scripts: syntax"
  local sh
  for sh in "$ROOT"/tools/*.sh; do bash -n "$sh" || die "syntax error in ${sh}"; done
  ( cd "$ROOT" && .venv/bin/python -m py_compile tools/*.py ) || die "a Python file does not compile"
  echo "ok"
  echo "== cam push: signature"
  cam_push_test
  echo "== panel: page tests"
  ( cd "$ROOT" && .venv/bin/python -m unittest tools/tests/test_cams_page.py tools/tests/test_devices_page.py 2>&1 ) | tail -1
  echo "== node and cam: fmt"
  in_image /mesh/node 'cargo fmt --check'
  in_cam_plain 'cargo fmt --check'
  if [[ "$fast" != "--fast" ]]; then
    need_key
    hide_node_target
    echo "== node: clippy"
    in_image /mesh/node 'cargo clippy --release -- -D warnings'
    echo "== node: firmware build"
    in_image /mesh/node 'cargo build --release 2>&1 | grep -E "^(warning: unused|error)|Finished"'
    echo "== node: stack budget"
    "${ROOT}/.venv/bin/python" "${ROOT}/tools/tests/stack_budget.py" "${MESH}/node/${ELF}"
    echo "== cam: clippy"
    cam_logged clippy --release -- -D warnings
    hide_cam_target
    echo "== cam: firmware build"
    cam_logged build --release
    hide_cam_target
    scan_cam_image
    "${ROOT}/.venv/bin/python" "${ROOT}/tools/tests/stack_budget.py" "${CAM}/${CAM_ELF}" --chains "${ROOT}/tools/tests/stack_chains_cam.json"
    echo "== tallowd: release build, HTTP contract"
    run_daemon build | tail -1
    ( cd "$ROOT" && .venv/bin/python tools/tests/panel_contract.py 2>&1 ) | head -1
  fi
  echo "check passed"
}

# Runs one cam cargo command, keeps the log private, and refuses a log that
# contains a credential. The failure line does not quote the log.
cam_logged() {
  local log
  log="$(mktemp)"
  chmod 600 "$log"
  trap "rm -f '$log'" EXIT
  if ! in_cam "cargo $* 2>&1" >"$log"; then
    if ! "${ROOT}/.venv/bin/python" "${ROOT}/tools/cam_build_scan.py" log "$log" "$CAM_SECRETS"; then
      rm -f "$log"
      exit 1
    fi
    grep -E '^(error|warning: unused)|secrets.yaml' "$log" | head -40 >&2 || true
    rm -f "$log"
    die "cam cargo $* failed"
  fi
  if ! "${ROOT}/.venv/bin/python" "${ROOT}/tools/cam_build_scan.py" log "$log" "$CAM_SECRETS"; then
    rm -f "$log"
    exit 1
  fi
  grep -E '^(warning: unused|error)|Finished' "$log" || true
  rm -f "$log"
}

# strings the cam ELF for baked-in default credentials. Prints one fixed line.
scan_cam_image() {
  local elf="${CAM}/${CAM_ELF}"
  [[ -f "$elf" ]] || die "No cam image at firmware/cam/${CAM_ELF}."
  strings "$elf" | "${ROOT}/.venv/bin/python" "${ROOT}/tools/cam_build_scan.py" image
}

# The cam slot, app0 and app1. cam-push refuses a file outside this before it dials.
CAM_SLOT=8126464

# Host build of the signature tests. Copies push.rs in, mounts only mesh core.
cam_push_test() {
  local dir
  dir="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$dir'" RETURN
  mkdir -p "$dir/tools/tests" "$dir/firmware/mesh"
  cp -a "${ROOT}/tools/tests/cam_push" "$dir/tools/tests/cam_push"
  cp "${CAM}/src/push.rs" "${CAM}/src/serve.rs" "${CAM}/src/stream.rs" "$dir/tools/tests/cam_push/src/"
  mkdir -p "${MESH}/.cache/registry" "${MESH}/.cache/git"
  podman run --rm "${HARDEN[@]}" --userns=keep-id --user "$(id -u):$(id -g)" -w /work/tools/tests/cam_push \
    -v "${dir}:/work" \
    -v "${MESH}/core:/work/firmware/mesh/core:ro" \
    -v "${MESH}/.cache/registry:/home/esp/.cargo/registry" \
    -v "${MESH}/.cache/git:/home/esp/.cargo/git" \
    -e HOME=/home/esp -e CARGO_TARGET_DIR=/tmp/target \
    "$IMAGE" cargo test --target x86_64-unknown-linux-gnu
}

# Host build of the multipart sample. stdout is the bytes, nothing else.
cam_frame_bytes() {
  local dir
  dir="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$dir'" RETURN
  cp -a "${ROOT}/tools/tests/cam_frame/." "$dir/"
  cp "${CAM}/src/stream.rs" "$dir/src/stream.rs"
  podman run --rm "${HARDEN[@]}" --userns=keep-id --user "$(id -u):$(id -g)" -w /work \
    -v "${dir}:/work" -e HOME=/home/esp -e CARGO_TARGET_DIR=/tmp/target \
    "$IMAGE" cargo run --quiet --target x86_64-unknown-linux-gnu
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
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
  ota-image)
    need_key
    [[ -f "${MESH}/node/${ELF}" ]] || die "No node image. Run: tools/mesh.sh build"
    in_image /mesh/node "espflash save-image --chip esp32 --flash-size 4mb --partition-table partitions.csv ${ELF} target/ota.bin"
    chmod 0600 "${MESH}/node/target/ota.bin"
    echo "Wrote firmware/mesh/node/target/ota.bin"
    ;;
  ota-sign)
    [[ -r "$OTA_KEY" ]] || die "No OTA signing key. Run: tools/mesh.sh ota-key"
    sign_ota_image "${2:?Name the image}" "${3:?Name the product: node or cam0}" "${4:?Name the version}"
    ;;
  check)
    run_check "${2:-}"
    ;;
  mutants)
    echo "== core mutants"
    "${ROOT}/.venv/bin/python" "${ROOT}/tools/tests/mutants/core_mutants.py"
    echo "== host mutants"
    "${ROOT}/.venv/bin/python" "${ROOT}/tools/tests/mutants/host_mutants.py"
    echo "== cam mutants"
    "${ROOT}/.venv/bin/python" "${ROOT}/tools/tests/mutants/cam_mutants.py"
    ;;
  fmt)
    in_image /mesh/core 'cargo fmt'
    in_image /mesh/node 'cargo fmt'
    in_image /repo/host 'cargo fmt --all'
    in_cam_plain 'cargo fmt'
    ;;
  test)
    in_image /mesh/core 'cargo test --target x86_64-unknown-linux-gnu -- --nocapture 2>&1'
    ;;
  daemon)
    run_daemon "${@:2}"
    ;;
  build)
    need_key
    hide_node_target
    in_image /mesh/node 'cargo build --release 2>&1 | tail -40'
    ;;
  size)
    in_image /mesh/node "espflash save-image --chip esp32 --flash-size 4mb --partition-table partitions.csv ${ELF} /tmp/app.bin 2>&1 | grep -E 'App/part'"
    ;;
  cam-build)
    cam_logged build --release
    hide_cam_target
    ;;
  cam-flash)
    port="${2:?Name the port}"
    [[ -z "${3:-}" ]] || die "cam-flash takes a port, not an address."
    need_cam0_port "$port"
    bootloader="target/xtensa-esp32s3-espidf/release/bootloader.bin"
    [[ -f "${CAM}/${bootloader}" ]] || die "No bootloader at firmware/cam/${bootloader}. Run: tools/mesh.sh cam-build"
    export CAM_DEVICE="$port"
    # Rollback lives in the bootloader this build produced. espflash writes app0.
    # Otadata left by ESPHome could select app1 and keep the old image booting.
    # Blank otadata boots app0, and this bootloader writes it as valid.
    in_cam "espflash flash --non-interactive --chip esp32s3 --port ${port} --bootloader ${bootloader} --partition-table partitions.csv --erase-parts otadata --flash-size 16mb ${CAM_ELF}"
    echo "Flashed ${port}."
    ;;
  cam-backup)
    port="${2:?Name the port}"
    [[ -z "${3:-}" ]] || die "cam-backup takes a port, not an address."
    need_cam0_port "$port"
    run_backup "$port"
    ;;
  cam-frame)
    cam_frame_bytes
    ;;
  cam-push)
    address="${2:?Name the address: tools/mesh.sh cam-push <address> <image>}"
    image="${3:?Name the image: tools/mesh.sh cam-push <address> <image>}"
    [[ -z "${4:-}" ]] || die "cam-push takes an address and an image."
    cam_allows_address "$address" || die "${address} is not 10.87.0.23. cam-push refuses every other address."
    [[ -r "$image" ]] || die "Cannot read ${image}"
    sig="${image}.sig"
    [[ -r "$sig" ]] || die "No signature at ${sig}. Run: tools/mesh.sh ota-sign ${image} cam0 <version>"
    [[ "$(wc -c < "$sig")" -eq 69 ]] || die "${sig} is not 69 bytes. Sign it again with tools/mesh.sh ota-sign."
    bytes="$(wc -c < "$image")"
    if [[ "$bytes" -eq 0 || "$bytes" -gt "$CAM_SLOT" ]]; then
      die "${image} does not fit the cam slot."
    fi
    "${ROOT}/.venv/bin/python" "${ROOT}/tools/cam_push_send.py" "$address" "$image" "$sig"
    ;;
  cam-image)
    [[ -f "${CAM}/${CAM_ELF}" ]] || die "No cam image at firmware/cam/${CAM_ELF}. Run: tools/mesh.sh cam-build"
    in_cam "umask 077; espflash save-image --chip esp32s3 --flash-size 16mb --partition-table partitions.csv ${CAM_ELF} target/ota.bin 2>&1 | grep -E 'App/part'"
    chmod 0600 "${CAM}/target/ota.bin"
    echo "Wrote firmware/cam/target/ota.bin"
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
    # Rollback lives in the bootloader this build produced. espflash's own
    # bootloader does not have it. espflash writes app0, and otadata from an
    # earlier OTA could still select app1 and keep the old image running, so
    # otadata is erased. This bootloader boots ota_0 from a blank otadata, and
    # writes it as valid.
    bootloader="target/xtensa-esp32-espidf/release/bootloader.bin"
    [[ -f "${MESH}/node/${bootloader}" ]] || die "No bootloader at firmware/mesh/node/${bootloader}. Run: tools/mesh.sh build"
    export MESH_DEVICE="$port"
    for try in $(seq 1 "$FLASH_TRIES"); do
      if output="$(run_image /mesh/node bash -lc "espflash flash --non-interactive --chip esp32 --port ${port} --bootloader ${bootloader} --partition-table partitions.csv --erase-parts otadata --flash-size 4mb ${ELF}" 2>&1)" \
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
    podman run --rm "${HARDEN[@]}" --device "${port}:${port}" --group-add keep-groups \
      -v "${ROOT}/tools:/tools:ro" --entrypoint python3 "$HOST_IMAGE" \
      /tools/mesh_monitor.py "$port" "$seconds"
    ;;
  bench)
    port="${2:?Name the port}"
    shift 2
    need_classic_port "$port"
    podman run --rm "${HARDEN[@]}" --device "${port}:${port}" --group-add keep-groups \
      -v "${ROOT}/tools:/tools:ro" --entrypoint python3 "$HOST_IMAGE" \
      /tools/mesh_bench.py "$port" "$@"
    ;;
  backup)
    port="${2:?Name the port}"
    need_classic_port "$port"
    run_backup "$port"
    ;;
  *)
    die "Unknown command ${1}. Run: tools/mesh.sh --help"
    ;;
esac
fi
