#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/_lib.sh"

usage() {
  cat <<'EOF'
Usage: tools/build/linux-release-arm64-cross.sh [--clean]

Build the Flutter desktop arm64 Debian package on an amd64 host using
host-side cross compilation for the heavy Rust/C/C++ steps.
Output:
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

ensure_arm64_binfmt() {
  rd_log "Registering arm64 binfmt handlers"
  docker run --privileged --rm tonistiigi/binfmt --install arm64 >/dev/null
}

normalize_arm64_sysroot_symlinks() {
  local sysroot_dir="$1"
  local link target resolved relative

  while IFS= read -r -d '' link; do
    target="$(readlink "${link}")"
    [[ "${target}" == /* ]] || continue
    resolved="${sysroot_dir}${target}"
    [[ -e "${resolved}" ]] || continue
    relative="$(realpath --relative-to="$(dirname "${link}")" "${resolved}")"
    ln -snf "${relative}" "${link}"
  done < <(find "${sysroot_dir}" -type l -print0)
}

prepare_arm64_sysroot() {
  local cache_root="$1"
  local sysroot_dir="${cache_root}/sdk/sysroot"
  local ready_file="${sysroot_dir}/.rustdesk-sysroot-ready"
  local sysroot_version="v4"

  if [[ -f "${ready_file}" ]] && [[ "$(cat "${ready_file}")" == "${sysroot_version}" ]]; then
    normalize_arm64_sysroot_symlinks "${sysroot_dir}"
    rd_log "Using cached arm64 sysroot"
    return
  fi

  rd_log "Preparing arm64 sysroot with target development packages"
  rm -rf "${sysroot_dir}"
  mkdir -p "${sysroot_dir}"

  ensure_arm64_binfmt

  local container_name="rustdesk-arm64-sysroot-prep"
  docker rm -f "${container_name}" >/dev/null 2>&1 || true

  docker run \
    --name "${container_name}" \
    --platform linux/arm64 \
    --network host \
    -v /etc/resolv.conf:/etc/resolv.conf:ro \
    -v /etc/hosts:/etc/hosts:ro \
    -v /etc/nsswitch.conf:/etc/nsswitch.conf:ro \
    "${RD_ARM64_IMAGE}" \
    bash -lc 'set -euo pipefail
      apt-get update -y
      apt-get install -y \
        libc6-dev \
        libayatana-appindicator3-dev \
        libasound2-dev \
        libgstreamer1.0-dev \
        libgstreamer-plugins-base1.0-dev \
        libgtk-3-dev \
        linux-libc-dev \
        libpam0g-dev \
        libpulse-dev \
        libva-dev \
        libxcb-randr0-dev \
        libxcb-shape0-dev \
        libxcb-xfixes0-dev \
        libxdo-dev \
        libxfixes-dev
      apt-get clean
      rm -rf /var/lib/apt/lists/*'

  docker export "${container_name}" | tar -C "${sysroot_dir}" -xf -
  docker rm -f "${container_name}" >/dev/null 2>&1 || true
  normalize_arm64_sysroot_symlinks "${sysroot_dir}"

  printf '%s\n' "${sysroot_version}" > "${ready_file}"
}

CLEAN=0

while [[ $# -gt 0 ]]; do
  case "$1" in
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

rd_require_cmd git
rd_require_cmd tar
rd_require_cmd docker
rd_require_cmd python3
rd_require_cmd dpkg-deb
rd_require_cmd file

docker version >/dev/null 2>&1 || rd_fail "Docker daemon is not available"

rd_require_submodules

free_kb="$(df -Pk "${RD_REPO_ROOT}" | awk 'NR==2 { print $4 }')"
min_kb=$((20 * 1024 * 1024))
if (( free_kb < min_kb )); then
  rd_fail "At least 20GB of free disk space is required"
fi

mkdir -p "${RD_BUILD_ROOT}" "${RD_DIST_ROOT}"

base_snapshot="$(rd_named_build_dir base-cross-arm64)"
bridge_snapshot="$(rd_named_build_dir bridge-arm64-cross)"
workspace="$(rd_named_build_dir arm64-cross)"
cache_root="$(rd_cache_root arm64-cross)"
bridge_cache_root="$(rd_named_cache_root bridge-arm64-cross)"

if (( CLEAN == 1 )); then
  rd_log "Cleaning cross-build workspace"
  rm -rf "${base_snapshot}" "${bridge_snapshot}" "${workspace}"
fi

version="$(rd_version)"
output_path="$(rd_arch_output_path "${version}" "arm64")"

rd_log "Snapshotting current worktree into ${base_snapshot}"
rd_snapshot_worktree "${base_snapshot}"

ensure_image "${RD_AMD64_IMAGE}" "amd64"
ensure_image "${RD_ARM64_IMAGE}" "arm64"
ensure_arm64_binfmt
prepare_arm64_sysroot "${cache_root}"

"${SCRIPT_DIR}/_bridge.sh" \
  --base-snapshot "${base_snapshot}" \
  --bridge-snapshot "${bridge_snapshot}" \
  --cache-root "${bridge_cache_root}"

rm -f "${output_path}"
"${SCRIPT_DIR}/_package_arm64_cross.sh" --base-snapshot "${base_snapshot}" --output "${output_path}"
"${SCRIPT_DIR}/_verify.sh" --package "${output_path}" --arch arm64

rd_log "Finished. Package is in ${output_path}"
