#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="${1:-${SCRIPT_DIR}/public-build.env}"
TMP_ENV="$(mktemp)"

cleanup() {
  rm -f "${TMP_ENV}"
}
trap cleanup EXIT

cat "${ENV_FILE}" > "${TMP_ENV}"
cat >> "${TMP_ENV}" <<'EOF'
WINDOWS_AMD64=0
MACOS_AMD64=0
MACOS_ARM64=0
ANDROID_ARM64=0
IOS_UNSIGNED=1
EOF

bash "${SCRIPT_DIR}/create-and-run-public-build.sh" "${TMP_ENV}"
