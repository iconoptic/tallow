#!/usr/bin/env bash
# Install Fedora host prerequisites for REA (rea-agents) lanes used by this repo:
# Android/JADX, Ghidra, ADB, TShark, binwalk/unblob, Frida, mitmproxy, apktool.
# Idempotent. Does not edit .cursor/mcp.json (verify paths against it at the end).
#
# Fedora 44 ships OpenJDK 25 only. JDK 21 comes from the Adoptium repo
# (temurin-21-jdk). A symlink at /usr/lib/jvm/java-21-openjdk keeps the
# JAVA_HOME already written in .cursor/mcp.json working.
# Ghidra is not packaged; this downloads the pinned NSA public build into /opt.
#
# Usage: install_re_toolchain_fedora.sh [--with-sigrok]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOME_DIR="${HOME}"
REA_SHARE="${HOME_DIR}/.local/share/rea"
JADX_DIR="${REA_SHARE}/android"
JADX_JAR="${JADX_DIR}/jadx-headless-mcp-0.7.1-all.jar"
JADX_URL="https://github.com/1013503897/jadx-headless-mcp/releases/download/v0.7.1/jadx-headless-mcp-0.7.1-all.jar"
JADX_SHA256="6e5eacf500b64292bfb73c49797c1958f6ee44646e43e868039ae7feb573ff75"
APKTOOL_DIR="${REA_SHARE}/apktool"
APKTOOL_JAR="${APKTOOL_DIR}/apktool.jar"
APKTOOL_WRAPPER="${APKTOOL_DIR}/apktool"
APKTOOL_JAR_URL="https://github.com/iBotPeaches/Apktool/releases/download/v3.0.3/apktool_3.0.3.jar"
APKTOOL_JAR_SHA256="dbf930b076c6b9be08d57c449cacefc3bdd6b71ebd59b3066fc0e1f5b14f9423"
APKTOOL_WRAPPER_URL="https://raw.githubusercontent.com/iBotPeaches/Apktool/v3.0.3/scripts/linux/apktool"
APKTOOL_WRAPPER_SHA256="493a9c98df968a25c373ca9df871ca77730c82954be4959b74e90adaf91a0e77"
GHIDRA_ZIP="ghidra_12.2_PUBLIC_20261016.zip"
GHIDRA_URL="https://github.com/NationalSecurityAgency/ghidra/releases/download/Ghidra_12.2_build/${GHIDRA_ZIP}"
GHIDRA_SHA256="2c2cd776db3c1f6a097ba418922378ac3649cadd5e844a6f5f088ae628a7be67"
GHIDRA_EXTRACT="ghidra_12.2_PUBLIC"
REA_VERSION="6.7.0"
ADOPTIUM_REPO="adoptium-temurin-java-repository"
TEMURIN_HOME="/usr/lib/jvm/java-21-temurin-jdk"
JAVA_HOME_EXPECTED="/usr/lib/jvm/java-21-openjdk"
GHIDRA_DIR_EXPECTED="/opt/ghidra"
WITH_SIGROK=0
DNF_OK=0

usage() {
  cat <<EOF
Usage: $0 [--with-sigrok]

Install REA reverse-engineering prerequisites on Fedora.
  --with-sigrok   also install sigrok-cli (optional signal-recording lane)

Requires the Adoptium Temurin repo (enabled only for this transaction):
  https://adoptium.net/installation/linux/
On this machine that repo file is present and disabled, which is enough.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --with-sigrok) WITH_SIGROK=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage >&2; exit 1 ;;
  esac
done

if [[ -r /etc/os-release ]]; then
  # shellcheck disable=SC1091
  source /etc/os-release
  if [[ "${ID:-}" != "fedora" ]]; then
    echo "This script is for Fedora (dnf)." >&2
    exit 1
  fi
fi

need_cmd() {
  command -v "$1" >/dev/null 2>&1
}

# dnf prunes each installonly package name down to installonly_limit on every
# transaction, not only kernel installs. When more kernels are installed than
# that limit, the prune erases one. Here that erase cannot finish: zfs and
# zfs-dkms are protected, and the kernel guard excludes the packages the
# solver would use to replace the dependency. Prints a higher limit to pass
# as installonly_limit for this transaction, or nothing when the cap is fine.
installonly_hold_limit() {
  local dump configured specs high
  dump="$(dnf -C --dump-main-config 2>/dev/null || true)"
  configured="$(printf '%s\n' "${dump}" | awk -F' = ' '/^installonly_limit /{print $2; exit}')"
  specs="$(printf '%s\n' "${dump}" | awk -F' = ' '/^installonlypkgs /{print $2; exit}')"
  [[ "${configured}" =~ ^[0-9]+$ ]] || configured=3
  # 0 already means unlimited.
  if [[ "${configured}" -eq 0 ]]; then
    return 0
  fi
  if [[ -z "${specs}" ]]; then
    specs="kernel,kernel-PAE,installonlypkg(kernel),installonlypkg(kernel-module),installonlypkg(vm),multiversion(kernel)"
  fi
  # One package can provide several of these names. Count each NEVRA once.
  # rpm writes "no package provides …" to stdout when a name matches nothing.
  high="$(
    {
      IFS=',' read -r -a _specs <<< "${specs}"
      for spec in "${_specs[@]}"; do
        spec="${spec//[[:space:]]/}"
        [[ -n "${spec}" ]] || continue
        rpm -q --whatprovides "${spec}" --qf '%{NEVRA} %{NAME}\n' 2>/dev/null || true
      done
    } | awk '
      $1 ~ /^no$/ { next }
      NF >= 2 && !seen[$1]++ { c[$2]++ }
      END { m=0; for (n in c) if (c[n] > m) m=c[n]; print m+0 }
    '
  )"
  if [[ "${high}" =~ ^[0-9]+$ && "${high}" -gt "${configured}" ]]; then
    printf '%s\n' "${high}"
  fi
}

download_verified() {
  local url="$1" dest="$2" sha="$3"
  if [[ -f "${dest}" ]]; then
    local current
    current="$(sha256sum "${dest}" | awk '{print $1}')"
    if [[ "${current}" == "${sha}" ]]; then
      echo "    already present and verified: ${dest}"
      return 0
    fi
    echo "    hash mismatch; re-downloading ${dest}"
    rm -f "${dest}"
  fi
  curl -fL --retry 3 -o "${dest}" "${url}"
  echo "${sha}  ${dest}" | sha256sum -c -
}

echo "==> dnf packages"
DNF_PKGS=(
  temurin-21-jdk
  android-tools
  wireshark-cli
  binwalk
  7zip
  lz4
  e2fsprogs
  squashfs-tools
  unzip
  curl
)
if [[ "${WITH_SIGROK}" -eq 1 ]]; then
  DNF_PKGS+=(sigrok-cli)
fi

DNF_EXTRA=()
hold_limit="$(installonly_hold_limit)"
if [[ -n "${hold_limit}" ]]; then
  DNF_EXTRA+=(--setopt=installonly_limit="${hold_limit}")
  echo "    holding installonly_limit at ${hold_limit} so this install does not erase a kernel"
fi

if need_cmd sudo && { sudo -n true 2>/dev/null || [[ -t 0 ]]; }; then
  sudo dnf install -y --enablerepo="${ADOPTIUM_REPO}" "${DNF_EXTRA[@]}" "${DNF_PKGS[@]}"
  DNF_OK=1
elif need_cmd sudo; then
  echo "WARN: sudo needs a password (non-interactive session)."
  echo "      Run in a terminal:"
  echo "      sudo dnf install -y --enablerepo=${ADOPTIUM_REPO} ${DNF_EXTRA[*]} ${DNF_PKGS[*]}"
  echo "      then re-run this script."
else
  echo "WARN: sudo not available; install manually:"
  echo "      dnf install -y --enablerepo=${ADOPTIUM_REPO} ${DNF_EXTRA[*]} ${DNF_PKGS[*]}"
fi

echo
echo "==> JDK 21 (Temurin, symlinked to the path in .cursor/mcp.json)"
if [[ -x "${JAVA_HOME_EXPECTED}/bin/java" ]]; then
  echo "    JDK 21: $("${JAVA_HOME_EXPECTED}/bin/java" -version 2>&1 | head -1)"
elif [[ "${DNF_OK}" -ne 1 ]]; then
  echo "    skipped until the dnf step succeeds"
elif [[ ! -x "${TEMURIN_HOME}/bin/java" ]]; then
  echo "ERROR: ${TEMURIN_HOME}/bin/java missing after temurin-21-jdk install" >&2
  exit 1
elif [[ -e "${JAVA_HOME_EXPECTED}" && ! -L "${JAVA_HOME_EXPECTED}" ]]; then
  echo "ERROR: ${JAVA_HOME_EXPECTED} exists and is not a symlink. Move it aside and re-run." >&2
  exit 1
else
  sudo ln -sfn java-21-temurin-jdk "${JAVA_HOME_EXPECTED}"
  echo "    ${JAVA_HOME_EXPECTED} -> java-21-temurin-jdk"
  echo "    JDK 21: $("${JAVA_HOME_EXPECTED}/bin/java" -version 2>&1 | head -1)"
fi

echo
echo "==> Ghidra 12.2 -> ${GHIDRA_DIR_EXPECTED}"
if [[ -x "${GHIDRA_DIR_EXPECTED}/ghidraRun" ]]; then
  echo "    already present: ${GHIDRA_DIR_EXPECTED}/ghidraRun"
elif [[ "${DNF_OK}" -ne 1 ]]; then
  echo "    skipped until sudo works (installs under /opt)"
elif [[ -e "${GHIDRA_DIR_EXPECTED}" && ! -L "${GHIDRA_DIR_EXPECTED}" ]]; then
  echo "ERROR: ${GHIDRA_DIR_EXPECTED} exists and is not a symlink. Move it aside and re-run." >&2
  exit 1
else
  ghidra_tmp="$(mktemp -d)"
  cleanup_ghidra() { rm -rf "${ghidra_tmp}"; }
  trap cleanup_ghidra EXIT
  echo "    downloading ${GHIDRA_ZIP} (about 544 MiB)"
  curl -fL --retry 3 -o "${ghidra_tmp}/${GHIDRA_ZIP}" "${GHIDRA_URL}"
  echo "${GHIDRA_SHA256}  ${ghidra_tmp}/${GHIDRA_ZIP}" | sha256sum -c -
  sudo unzip -q -o "${ghidra_tmp}/${GHIDRA_ZIP}" -d /opt
  if [[ ! -x "/opt/${GHIDRA_EXTRACT}/ghidraRun" ]]; then
    echo "ERROR: /opt/${GHIDRA_EXTRACT}/ghidraRun missing after unzip" >&2
    exit 1
  fi
  sudo ln -sfn "${GHIDRA_EXTRACT}" "${GHIDRA_DIR_EXPECTED}"
  if need_cmd restorecon; then
    sudo restorecon -R "/opt/${GHIDRA_EXTRACT}" "${GHIDRA_DIR_EXPECTED}" || true
  fi
  cleanup_ghidra
  trap - EXIT
  echo "    ${GHIDRA_DIR_EXPECTED} -> /opt/${GHIDRA_EXTRACT}"
fi

echo
echo "==> uv tool installs (unblob, frida-tools, mitmproxy)"
if ! need_cmd uv; then
  echo "uv not on PATH; install uv first (https://docs.astral.sh/uv/)" >&2
  exit 1
fi
export UV_TOOL_BIN_DIR="${HOME_DIR}/.local/bin"
mkdir -p "${UV_TOOL_BIN_DIR}"
uv tool install --quiet unblob || uv tool upgrade unblob
uv tool install --quiet frida-tools || uv tool upgrade frida-tools
uv tool install --quiet mitmproxy || uv tool upgrade mitmproxy

UNBLOB_BIN="${UV_TOOL_BIN_DIR}/unblob"
FRIDA_BIN="${UV_TOOL_BIN_DIR}/frida"
MITM_BIN="${UV_TOOL_BIN_DIR}/mitmdump"
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
download_verified "${JADX_URL}" "${JADX_JAR}" "${JADX_SHA256}"

echo
echo "==> apktool 3.0.3 (pinned SHA-256)"
mkdir -p "${APKTOOL_DIR}"
download_verified "${APKTOOL_JAR_URL}" "${APKTOOL_JAR}" "${APKTOOL_JAR_SHA256}"
download_verified "${APKTOOL_WRAPPER_URL}" "${APKTOOL_WRAPPER}" "${APKTOOL_WRAPPER_SHA256}"
chmod +x "${APKTOOL_WRAPPER}"
ln -sfn "${APKTOOL_WRAPPER}" "${UV_TOOL_BIN_DIR}/apktool"

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
if [[ ! -x "${JAVA_HOME_EXPECTED}/bin/java" || ! -f "${JADX_JAR}" ]]; then
  echo "    skipped until JDK 21 and the JADX jar are installed"
else
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
    echo "    doctor wrote JSON to ${DOCTOR_OUT} (exit ${DOCTOR_RC}; healthy=false is OK until dnf deps land)"
    python3 - "${DOCTOR_OUT}" <<'PY' 2>/dev/null || true
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
fi

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
need_cmd mitmdump || [[ -x "${MITM_BIN}" ]] && MITM_OK=1 || MITM_OK=0
[[ -x "${UV_TOOL_BIN_DIR}/apktool" && -f "${APKTOOL_JAR}" ]] && APKTOOL_OK=1 || APKTOOL_OK=0

check "android/jadx" "${JADX_OK}"
check "ghidra" "${GHIDRA_OK}"
check "adb" "${ADB_OK}"
check "tshark" "${TSHARK_OK}"
check "firmware" "${FW_OK}"
check "frida" "${FRIDA_OK}"
check "mitmproxy" "${MITM_OK}"
check "apktool" "${APKTOOL_OK}"

echo
if [[ "${DNF_OK}" -ne 1 ]]; then
  echo "Remaining (needs your sudo password in a terminal):"
  echo "  sudo dnf install -y --enablerepo=${ADOPTIUM_REPO} ${DNF_EXTRA[*]} ${DNF_PKGS[*]}"
  echo "Then re-run: ${ROOT}/tools/install_re_toolchain_fedora.sh"
  echo
fi
echo "Pinned CLI is rea-agents@${REA_VERSION} (Node $(node -v 2>/dev/null || echo unknown) satisfies it)."
echo "The shallow clone under references/re-tooling/rea may be a newer release; keep the 6.7.0 pin until Node is >= 22.23.4."
echo "Done. Restart Cursor so it reloads .cursor/mcp.json, then confirm REA tools appear."
echo "CLI fallback: npx -y rea-agents@${REA_VERSION} <command>"
