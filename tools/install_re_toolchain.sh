#!/usr/bin/env bash
# Install Arch host prerequisites for REA (rea-agents) lanes used by this repo:
# Android/JADX, Ghidra, ADB, TShark, binwalk/unblob, Frida, mitmproxy.
# Idempotent. Does not edit .cursor/mcp.json (verify paths against it at the end).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOME_DIR="${HOME}"
REA_SHARE="${HOME_DIR}/.local/share/rea"
JADX_DIR="${REA_SHARE}/android"
JADX_JAR="${JADX_DIR}/jadx-headless-mcp-0.7.1-all.jar"
JADX_URL="https://github.com/1013503897/jadx-headless-mcp/releases/download/v0.7.1/jadx-headless-mcp-0.7.1-all.jar"
JADX_SHA256="6e5eacf500b64292bfb73c49797c1958f6ee44646e43e868039ae7feb573ff75"
REA_VERSION="6.7.0"
JAVA_HOME_EXPECTED="/usr/lib/jvm/java-21-openjdk"
GHIDRA_DIR_EXPECTED="/opt/ghidra"
WITH_SIGROK=0

usage() {
  cat <<EOF
Usage: $0 [--with-sigrok]

Install REA reverse-engineering prerequisites on Arch Linux.
  --with-sigrok   also install sigrok-cli (optional signal-recording lane)
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --with-sigrok) WITH_SIGROK=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage >&2; exit 1 ;;
  esac
done

need_cmd() {
  command -v "$1" >/dev/null 2>&1
}

echo "==> pacman packages"
PACMAN_PKGS=(
  jdk21-openjdk
  ghidra
  mitmproxy
  android-tools
  wireshark-cli
  binwalk
  e2fsprogs
  p7zip
  lz4
  squashfs-tools
)
if [[ "${WITH_SIGROK}" -eq 1 ]]; then
  PACMAN_PKGS+=(sigrok-cli)
fi
# apktool is AUR-only on Arch (android-apktool); installed after pacman.

PACMAN_OK=0
if need_cmd sudo; then
  if sudo -n true 2>/dev/null; then
    sudo pacman -S --needed --noconfirm "${PACMAN_PKGS[@]}"
    PACMAN_OK=1
  elif [[ -t 0 ]]; then
    sudo pacman -S --needed --noconfirm "${PACMAN_PKGS[@]}"
    PACMAN_OK=1
  else
    echo "WARN: sudo needs a password (non-interactive session)."
    echo "      Run in a terminal:"
    echo "      sudo pacman -S --needed ${PACMAN_PKGS[*]}"
    echo "      then re-run this script."
  fi
else
  echo "WARN: sudo not available; install manually:"
  echo "      pacman -S --needed ${PACMAN_PKGS[*]}"
fi

echo
echo "==> apktool (AUR: android-apktool / android-apktool-bin)"
if need_cmd apktool; then
  echo "    already on PATH: $(command -v apktool)"
elif [[ "${PACMAN_OK}" -eq 1 ]] && need_cmd paru; then
  paru -S --needed --noconfirm android-apktool-bin \
    || paru -S --needed --noconfirm android-apktool \
    || echo "WARN: AUR apktool install failed; optional for REA resource decode"
elif [[ -t 0 ]] && need_cmd paru; then
  paru -S --needed --noconfirm android-apktool-bin \
    || echo "WARN: AUR apktool install failed; optional for REA resource decode"
else
  echo "    apktool not installed. Optional for REA resource lane:"
  echo "      paru -S android-apktool-bin"
fi

echo
echo "==> Java note"
echo "    REA uses JAVA_HOME=${JAVA_HOME_EXPECTED} (do not change system default)."
echo "    Current default java: $(java -version 2>&1 | head -1 || true)"
if [[ -x "${JAVA_HOME_EXPECTED}/bin/java" ]]; then
  echo "    JDK 21: $("${JAVA_HOME_EXPECTED}/bin/java" -version 2>&1 | head -1)"
else
  echo "    ERROR: ${JAVA_HOME_EXPECTED}/bin/java missing" >&2
  exit 1
fi

echo
echo "==> uv tool installs (unblob, frida-tools)"
if ! need_cmd uv; then
  echo "uv not on PATH; install uv first (https://docs.astral.sh/uv/)" >&2
  exit 1
fi
# Ensure ~/.local/bin is where uv tools land
export UV_TOOL_BIN_DIR="${HOME_DIR}/.local/bin"
mkdir -p "${UV_TOOL_BIN_DIR}"
uv tool install --quiet unblob || uv tool upgrade unblob
uv tool install --quiet frida-tools || uv tool upgrade frida-tools

UNBLOB_BIN="${UV_TOOL_BIN_DIR}/unblob"
FRIDA_BIN="${UV_TOOL_BIN_DIR}/frida"
if [[ ! -x "${UNBLOB_BIN}" ]]; then
  echo "ERROR: unblob not found at ${UNBLOB_BIN}" >&2
  exit 1
fi
if [[ ! -x "${FRIDA_BIN}" ]]; then
  echo "WARN: frida CLI not found at ${FRIDA_BIN} (frida-tools may use a different entrypoint)"
fi

echo
echo "==> jadx-headless-mcp 0.7.1 (pinned SHA-256)"
mkdir -p "${JADX_DIR}"
if [[ -f "${JADX_JAR}" ]]; then
  CURRENT="$(sha256sum "${JADX_JAR}" | awk '{print $1}')"
  if [[ "${CURRENT}" == "${JADX_SHA256}" ]]; then
    echo "    already present and verified: ${JADX_JAR}"
  else
    echo "    hash mismatch; re-downloading"
    rm -f "${JADX_JAR}"
  fi
fi
if [[ ! -f "${JADX_JAR}" ]]; then
  curl -fL -o "${JADX_JAR}" "${JADX_URL}"
  echo "${JADX_SHA256}  ${JADX_JAR}" | sha256sum -c -
fi

echo
echo "==> Ghidra path"
if [[ -x "${GHIDRA_DIR_EXPECTED}/ghidraRun" ]]; then
  echo "    GHIDRA_INSTALL_DIR=${GHIDRA_DIR_EXPECTED}"
else
  echo "    WARN: ${GHIDRA_DIR_EXPECTED}/ghidraRun not found; check pacman -Ql ghidra"
fi

echo
echo "==> Wireshark group (live capture only; file inspection works without it)"
if id -nG | grep -qw wireshark; then
  echo "    already in wireshark group"
else
  echo "    optional: sudo usermod -aG wireshark \"$(id -un)\" && re-login"
fi

echo
echo "==> Expected .cursor/mcp.json env values (verify against ${ROOT}/.cursor/mcp.json)"
cat <<EOF
    JAVA_HOME=${JAVA_HOME_EXPECTED}
    GHIDRA_INSTALL_DIR=${GHIDRA_DIR_EXPECTED}
    REA_JADX_MCP_JAR=${JADX_JAR}
    REA_TSHARK_COMMAND=/usr/bin/tshark
    REA_CAPINFOS_COMMAND=/usr/bin/capinfos
    REA_BINWALK_COMMAND=/usr/bin/binwalk
    REA_UNBLOB_COMMAND=${UNBLOB_BIN}
EOF

echo
echo "==> rea doctor (${REA_VERSION})"
export JAVA_HOME="${JAVA_HOME_EXPECTED}"
export GHIDRA_INSTALL_DIR="${GHIDRA_DIR_EXPECTED}"
export REA_JADX_MCP_JAR="${JADX_JAR}"
export REA_TSHARK_COMMAND=/usr/bin/tshark
export REA_CAPINFOS_COMMAND=/usr/bin/capinfos
export REA_BINWALK_COMMAND=/usr/bin/binwalk
export REA_UNBLOB_COMMAND="${UNBLOB_BIN}"

DOCTOR_OUT="$(mktemp)"
DOCTOR_ERR="$(mktemp)"
set +e
npx -y "rea-agents@${REA_VERSION}" doctor --json >"${DOCTOR_OUT}" 2>"${DOCTOR_ERR}"
DOCTOR_RC=$?
set -e
if [[ -s "${DOCTOR_OUT}" ]]; then
  echo "    doctor wrote JSON to ${DOCTOR_OUT} (exit ${DOCTOR_RC}; healthy=false is OK until pacman deps land)"
  # Summarize key provider readiness from doctor JSON when python is available
  python - "${DOCTOR_OUT}" <<'PY' 2>/dev/null || true
import json, sys
data = json.load(open(sys.argv[1]))
for p in data.get("providerInspections") or []:
    print(f"    provider {p.get('id')}: available={p.get('available')} configured={p.get('configured')}")
print(f"    jadx check in scope: {[c for c in data.get('scope_checks',[]) if c.get('name')=='jadx']}")
PY
else
  echo "    doctor produced no JSON (exit ${DOCTOR_RC}); stderr:"
  cat "${DOCTOR_ERR}" || true
fi

# Lightweight lane readiness from PATH / files (doctor schema varies by release)
echo
echo "==> Lane readiness (host checks)"
check() {
  local name="$1" ok="$2"
  if [[ "${ok}" == "1" ]]; then
    printf "    %-12s ready\n" "${name}"
  else
    printf "    %-12s MISSING\n" "${name}"
  fi
}
[[ -x "${JAVA_HOME_EXPECTED}/bin/java" && -f "${JADX_JAR}" ]] && JADX_OK=1 || JADX_OK=0
[[ -x "${GHIDRA_DIR_EXPECTED}/ghidraRun" ]] && GHIDRA_OK=1 || GHIDRA_OK=0
need_cmd adb && ADB_OK=1 || ADB_OK=0
need_cmd tshark && TSHARK_OK=1 || TSHARK_OK=0
need_cmd binwalk && [[ -x "${UNBLOB_BIN}" ]] && FW_OK=1 || FW_OK=0
need_cmd frida || [[ -x "${FRIDA_BIN}" ]] && FRIDA_OK=1 || FRIDA_OK=0
need_cmd mitmdump && MITM_OK=1 || MITM_OK=0
need_cmd apktool && APKTOOL_OK=1 || APKTOOL_OK=0

check "android/jadx" "${JADX_OK}"
check "ghidra" "${GHIDRA_OK}"
check "adb" "${ADB_OK}"
check "tshark" "${TSHARK_OK}"
check "firmware" "${FW_OK}"
check "frida" "${FRIDA_OK}"
check "mitmproxy" "${MITM_OK}"
check "apktool" "${APKTOOL_OK}"

echo
if [[ "${PACMAN_OK}" -ne 1 ]]; then
  echo "Remaining (needs your sudo password in a terminal):"
  echo "  sudo pacman -S --needed ${PACMAN_PKGS[*]}"
  echo "  paru -S android-apktool-bin   # optional"
  echo "Then re-run: ${ROOT}/tools/install_re_toolchain.sh"
  echo
fi
echo "Done. Restart Cursor so it reloads .cursor/mcp.json, then confirm REA tools appear."
echo "CLI fallback: npx -y rea-agents@${REA_VERSION} <command>"
echo "Clone REA docs: ${ROOT}/tools/clone_references.sh → references/re-tooling/rea/"
