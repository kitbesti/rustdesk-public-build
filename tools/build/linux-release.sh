#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/_lib.sh"

usage() {
  cat <<'EOF'
Usage: tools/build/linux-release.sh --arch amd64|arm64|all [--clean]

Build Flutter desktop Debian release packages for RustDesk.
Outputs:
  dist/linux/rustdesk-<version>-amd64.deb
  dist/linux/rustdesk-<version>-arm64.deb
EOF
}

ensure_image() {
  local image="$1"
  local label="$2"
  if docker image inspect "${image}" >/dev/null 2>&1; then
    rd_log "Using cached ${label} image"
    return
  fi
  rd_log "Pulling ${label} builder image"
  docker pull "${image}" >/dev/null
}

ARCH="all"
CLEAN=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --arch)
      ARCH="$2"
      shift 2
      ;;
    --clean)
      CLEAN=1
      shift
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

case "${ARCH}" in
  amd64|arm64|all) ;;
  *) rd_fail "Unsupported --arch value: ${ARCH}" ;;
esac

rd_require_cmd git
rd_require_cmd tar
rd_require_cmd docker
rd_require_cmd python3
rd_require_cmd dpkg-deb
rd_require_cmd file

docker version >/dev/null 2>&1 || rd_fail "Docker daemon is not available"

rd_require_submodules

free_kb="$(df -Pk "${RD_REPO_ROOT}" | awk 'NR==2 { print $4 }')"
min_kb=$((35 * 1024 * 1024))
if (( free_kb < min_kb )); then
  rd_fail "At least 35GB of free disk space is required"
fi

mem_kb="$(awk '/MemTotal/ { print $2 }' /proc/meminfo)"
swap_kb="$(awk '/SwapTotal/ { print $2 }' /proc/meminfo)"
if (( mem_kb + swap_kb < 12 * 1024 * 1024 )); then
  rd_warn "Less than 12GiB of combined RAM+swap detected; arm64 builds may fail under QEMU"
fi

mkdir -p "${RD_BUILD_ROOT}" "${RD_DIST_ROOT}"

arches=()
case "${ARCH}" in
  amd64) arches=(amd64) ;;
  arm64) arches=(arm64) ;;
  all) arches=(amd64 arm64) ;;
esac

version="$(rd_version)"
base_snapshot="$(rd_named_build_dir base-linux-release)"
bridge_snapshot="$(rd_named_build_dir bridge-linux-release)"
bridge_cache_root="$(rd_named_cache_root bridge-linux-release)"

if (( CLEAN == 1 )); then
  rd_log "Cleaning linux-release workspaces for ${ARCH}"
  rm -rf "${base_snapshot}" "${bridge_snapshot}"
  for item in "${arches[@]}"; do
    rm -rf "$(rd_snapshot_dir "${item}")"
    rm -f "$(rd_arch_output_path "${version}" "${item}")"
  done
fi

rd_log "Snapshotting current worktree into ${base_snapshot}"
rd_snapshot_worktree "${base_snapshot}"

ensure_image "${RD_AMD64_IMAGE}" "amd64"

if [[ "${ARCH}" == "arm64" || "${ARCH}" == "all" ]]; then
  rd_log "Registering arm64 binfmt handlers"
  docker run --privileged --rm tonistiigi/binfmt --install arm64 >/dev/null
  ensure_image "${RD_ARM64_IMAGE}" "arm64"
fi

"${SCRIPT_DIR}/_bridge.sh" \
  --base-snapshot "${base_snapshot}" \
  --bridge-snapshot "${bridge_snapshot}" \
  --cache-root "${bridge_cache_root}"

for item in "${arches[@]}"; do
  output_path="$(rd_arch_output_path "${version}" "${item}")"
  rm -f "${output_path}"
  "${SCRIPT_DIR}/_package.sh" --arch "${item}" --base-snapshot "${base_snapshot}" --output "${output_path}"
  "${SCRIPT_DIR}/_verify.sh" --package "${output_path}" --arch "${item}"
done

rd_log "Finished. Packages are in ${RD_DIST_ROOT}"
