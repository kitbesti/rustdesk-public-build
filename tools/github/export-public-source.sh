#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"
source "${REPO_ROOT}/tools/build/_lib.sh"

usage() {
  cat <<'EOF'
Usage: tools/github/export-public-source.sh --dest PATH [--with-local-bridge|--skip-local-bridge]

Create a clean public export from the current working tree:
- copies tracked files from the root repository
- vendors the current libs/hbb_common working tree
- removes unrelated workflows
- optionally generates bridge files locally for faster GitHub Actions runs
EOF
}

DEST=""
USE_LOCAL_BRIDGE=1
BRIDGE_CACHE_ROOT="$(rd_named_cache_root public-export-bridge)"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dest)
      DEST="$2"
      shift 2
      ;;
    --with-local-bridge)
      USE_LOCAL_BRIDGE=1
      shift
      ;;
    --skip-local-bridge)
      USE_LOCAL_BRIDGE=0
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

[[ -n "${DEST}" ]] || rd_fail "--dest is required"

rd_require_submodules
rd_reset_dir "${DEST}"

copy_root_files() {
  local dest_root="$1"
  git -C "${REPO_ROOT}" ls-files -z | while IFS= read -r -d '' path; do
    [[ "${path}" == "libs/hbb_common" ]] && continue
    [[ -e "${REPO_ROOT}/${path}" ]] || continue
    mkdir -p "${dest_root}/$(dirname "${path}")"
    cp -a "${REPO_ROOT}/${path}" "${dest_root}/${path}"
  done
}

copy_submodule_files() {
  local submodule_path="libs/hbb_common"
  git -C "${REPO_ROOT}/${submodule_path}" ls-files -z | while IFS= read -r -d '' path; do
    [[ -e "${REPO_ROOT}/${submodule_path}/${path}" ]] || continue
    mkdir -p "${DEST}/${submodule_path}/$(dirname "${path}")"
    cp -a "${REPO_ROOT}/${submodule_path}/${path}" "${DEST}/${submodule_path}/${path}"
  done
}

copy_explicit_path() {
  local relpath="$1"
  [[ -e "${REPO_ROOT}/${relpath}" ]] || return 0
  mkdir -p "${DEST}/$(dirname "${relpath}")"
  cp -a "${REPO_ROOT}/${relpath}" "${DEST}/${relpath}"
}

copy_root_files "${DEST}"
copy_submodule_files
copy_explicit_path ".github/workflows/public-free-build.yml"
copy_explicit_path "docs/PUBLIC-GITHUB-BUILD.md"

rm -f "${DEST}/.gitmodules"

if [[ -d "${DEST}/.github/workflows" ]]; then
  find "${DEST}/.github/workflows" \
    -maxdepth 1 \
    -type f \
    -name '*.yml' \
    ! -name 'public-free-build.yml' \
    -delete
fi

[[ -f "${DEST}/.github/workflows/public-free-build.yml" ]] || rd_fail "public-free-build.yml is missing from export"

if [[ "${USE_LOCAL_BRIDGE}" == "1" ]]; then
  # Prefer bridge artifacts already sitting in the working tree.
  #
  # The container step exists only to produce these files. If they are already
  # here - because flutter_rust_bridge_codegen was run on the host, see
  # docs/BUILD.md - regenerating them in Docker just to copy them across is
  # wasted work, and it lets the export run on a machine with no container
  # runtime at all. Falls back to the container when any of them is absent.
  bridge_missing=0
  for bridge_path in "${RD_BRIDGE_OUTPUTS[@]}"; do
    [[ -f "${REPO_ROOT}/${bridge_path}" ]] || bridge_missing=1
  done

  if [[ "${bridge_missing}" == "0" ]]; then
    rd_log "Reusing bridge artifacts from the working tree"
    for bridge_path in "${RD_BRIDGE_OUTPUTS[@]}"; do
      mkdir -p "${DEST}/$(dirname "${bridge_path}")"
      cp -a "${REPO_ROOT}/${bridge_path}" "${DEST}/${bridge_path}"
    done
  else
    rd_log "Bridge artifacts incomplete; generating them in a container"
    rd_require_cmd docker
    tmpdir="$(mktemp -d)"
    trap 'rm -rf "${tmpdir}"' EXIT
    "${REPO_ROOT}/tools/build/_bridge.sh" \
      --base-snapshot "${DEST}" \
      --bridge-snapshot "${tmpdir}/bridge" \
      --cache-root "${BRIDGE_CACHE_ROOT}"
  fi
fi

rd_log "Public export prepared at ${DEST}"
