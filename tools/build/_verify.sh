#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/_lib.sh"

usage() {
  cat <<'EOF'
Usage: tools/build/_verify.sh --package PATH --arch amd64|arm64
EOF
}

PACKAGE_PATH=""
ARCH=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --package)
      PACKAGE_PATH="$2"
      shift 2
      ;;
    --arch)
      ARCH="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      rd_fail "Unknown argument: $1"
      ;;
  esac
done

[[ -n "${PACKAGE_PATH}" ]] || rd_fail "--package is required"
[[ -n "${ARCH}" ]] || rd_fail "--arch is required"
[[ -f "${PACKAGE_PATH}" ]] || rd_fail "Package not found: ${PACKAGE_PATH}"

rd_require_cmd dpkg-deb
rd_require_cmd file
rd_require_cmd mktemp

expected_machine=""
case "${ARCH}" in
  amd64) expected_machine="x86-64" ;;
  arm64) expected_machine="ARM aarch64" ;;
  *) rd_fail "Unsupported arch: ${ARCH}" ;;
esac

pkg_arch="$(dpkg-deb -f "${PACKAGE_PATH}" Architecture)"
[[ "${pkg_arch}" == "${ARCH}" ]] || rd_fail "Package architecture mismatch: expected ${ARCH}, got ${pkg_arch}"

tmpdir="$(mktemp -d)"
trap 'rm -rf "${tmpdir}"' EXIT

dpkg-deb -x "${PACKAGE_PATH}" "${tmpdir}"

required_paths=(
  "usr/share/rustdesk/rustdesk"
  "usr/share/rustdesk/lib/librustdesk.so"
  "usr/share/rustdesk/files/systemd/rustdesk.service"
  "usr/share/applications/rustdesk.desktop"
  "usr/share/applications/rustdesk-link.desktop"
  "usr/share/icons/hicolor/256x256/apps/rustdesk.png"
  "usr/share/icons/hicolor/scalable/apps/rustdesk.svg"
  "etc/pam.d/rustdesk"
)

for relpath in "${required_paths[@]}"; do
  [[ -e "${tmpdir}/${relpath}" ]] || rd_fail "Package content missing: ${relpath}"
done

main_desc="$(file -b "${tmpdir}/usr/share/rustdesk/rustdesk")"
lib_desc="$(file -b "${tmpdir}/usr/share/rustdesk/lib/librustdesk.so")"

[[ "${main_desc}" == *"${expected_machine}"* ]] || rd_fail "Unexpected main executable architecture: ${main_desc}"
[[ "${lib_desc}" == *"${expected_machine}"* ]] || rd_fail "Unexpected librustdesk.so architecture: ${lib_desc}"

rd_log "Verified ${PACKAGE_PATH} (${ARCH})"
