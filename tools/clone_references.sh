#!/usr/bin/env bash
# Shallow-clone curated smart-light reverse-engineering reference repos.
# Idempotent: skips directories that already exist.
# Records each clone's HEAD in tools/references.lock (tracked) so docs that cite
# reference code can be tied to a revision.
#
# Usage: clone_references.sh          clone missing repos, rewrite the lock
#        clone_references.sh --check  report clones that are missing or differ from the lock
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REF="${ROOT}/references"
LOCK="${ROOT}/tools/references.lock"
MODE="record"

case "${1:-}" in
  "") ;;
  --check) MODE="check" ;;
  *) echo "Usage: $0 [--check]" >&2; exit 1 ;;
esac

if [[ "${MODE}" == "record" ]]; then
  NEW_LOCK="$(mktemp)"
  trap 'rm -f "${NEW_LOCK}"' EXIT
  echo "# <path> <url> <HEAD sha> — written by tools/clone_references.sh" >"${NEW_LOCK}"
fi
DRIFT=0

clone_one() {
  local dest="$1"
  local url="$2"
  local rel="${dest#"${ROOT}/"}"
  local sha

  if [[ "${MODE}" == "check" ]]; then
    local locked
    locked="$(awk -v p="${rel}" '$1 == p {print $3}' "${LOCK}" 2>/dev/null || true)"
    if [[ ! -d "${dest}/.git" ]]; then
      echo "missing ${rel}"; DRIFT=1; return 0
    fi
    sha="$(git -C "${dest}" rev-parse HEAD)"
    if [[ -z "${locked}" ]]; then
      echo "unlocked ${rel} (${sha:0:12})"; DRIFT=1
    elif [[ "${sha}" != "${locked}" ]]; then
      echo "differs ${rel} (${sha:0:12} vs lock ${locked:0:12})"; DRIFT=1
    else
      echo "ok      ${rel}"
    fi
    return 0
  fi

  if [[ -d "${dest}/.git" ]]; then
    echo "skip  ${rel} (already cloned)"
  elif [[ -e "${dest}" ]]; then
    echo "error: ${rel} exists but is not a git clone; move it aside and re-run" >&2
    exit 1
  else
    echo "clone ${rel}"
    git clone --depth 1 --single-branch "${url}" "${dest}"
  fi
  sha="$(git -C "${dest}" rev-parse HEAD)"
  echo "${rel} ${url} ${sha}" >>"${NEW_LOCK}"
}

mkdir -p \
  "${REF}/ble-stack" \
  "${REF}/protocols-ble" \
  "${REF}/protocols-wifi" \
  "${REF}/firmware-liberation" \
  "${REF}/re-tooling"

# --- BLE stack & proxy infrastructure ---
clone_one "${REF}/ble-stack/bleak"            "https://github.com/hbldh/bleak.git"
clone_one "${REF}/ble-stack/bumble"           "https://github.com/google/bumble.git"
clone_one "${REF}/ble-stack/habluetooth"      "https://github.com/Bluetooth-Devices/habluetooth.git"
clone_one "${REF}/ble-stack/bleak-esphome"    "https://github.com/Bluetooth-Devices/bleak-esphome.git"

# --- Reverse-engineered BLE light protocols ---
clone_one "${REF}/protocols-ble/led-ble"      "https://github.com/Bluetooth-Devices/led-ble.git"
clone_one "${REF}/protocols-ble/elkbledom"    "https://github.com/dave-code-ruiz/elkbledom.git"
clone_one "${REF}/protocols-ble/lednetwf_ble" "https://github.com/8none1/lednetwf_ble.git"
clone_one "${REF}/protocols-ble/magichome2-ble" "https://github.com/rabidpaperclip/magichome2-ble.git"

# --- Local Wi-Fi light protocols ---
clone_one "${REF}/protocols-wifi/tinytuya"    "https://github.com/jasonacox/tinytuya.git"
clone_one "${REF}/protocols-wifi/python-kasa" "https://github.com/python-kasa/python-kasa.git"
clone_one "${REF}/protocols-wifi/flux_led"    "https://github.com/Danielhiversen/flux_led.git"
clone_one "${REF}/protocols-wifi/govee2mqtt"  "https://github.com/wez/govee2mqtt.git"

# --- Cloud-cutting & open firmware ---
clone_one "${REF}/firmware-liberation/tuya-cloudcutter" "https://github.com/tuya-cloudcutter/tuya-cloudcutter.git"
clone_one "${REF}/firmware-liberation/OpenBK7231T_App"  "https://github.com/openshwprojects/OpenBK7231T_App.git"

# --- AI / MCP reverse-engineering bridges ---
# Primary: REA (Android/JADX, Ghidra, ADB, Frida, tshark, firmware) — see docs/02
clone_one "${REF}/re-tooling/rea"             "https://github.com/morluto/rea.git"
# Fallbacks if REA MCP is unavailable in-session
clone_one "${REF}/re-tooling/GhidraMCP"       "https://github.com/LaurieWired/GhidraMCP.git"
clone_one "${REF}/re-tooling/jadx-ai-mcp"     "https://github.com/zinja-coder/jadx-ai-mcp.git"

if [[ "${MODE}" == "check" ]]; then
  exit "${DRIFT}"
fi

mv "${NEW_LOCK}" "${LOCK}"
trap - EXIT

echo
echo "Done. References live under ${REF}/"
echo "These clones are gitignored; re-run this script on a fresh checkout."
echo "Revisions recorded in ${LOCK#"${ROOT}/"}."
