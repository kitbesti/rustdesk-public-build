#!/usr/bin/env bash

set -euo pipefail

RD_SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
RD_REPO_ROOT="$(cd -- "${RD_SCRIPT_DIR}/../.." && pwd)"
RD_ENV_CACHE_ROOT="${RD_ENV_CACHE_ROOT:-${HOME}/.build/linux/environment-pkg}"
RD_BUILD_ROOT="${RD_REPO_ROOT}/.build/linux"
RD_DIST_ROOT="${RD_REPO_ROOT}/dist/linux"

RD_RUST_VERSION="1.75.0"
RD_BRIDGE_FLUTTER_VERSION="3.22.3"
RD_PACKAGE_FLUTTER_VERSION="3.24.5"
RD_FRB_VERSION="1.80.1"
RD_CARGO_EXPAND_VERSION="1.0.95"
RD_VCPKG_COMMIT="120deac3062162151622ca4860575a33844ba10b"

RD_AMD64_IMAGE="ghcr.io/rustdesk/rustdesk/run-on-arch-rustdesk-rustdesk-full-flutter-ci-x86-64-ubuntu18-04:latest"
RD_ARM64_IMAGE="ghcr.io/rustdesk/rustdesk/run-on-arch-rustdesk-rustdesk-full-flutter-ci-aarch64-ubuntu18-04:latest"
RD_DROPDOWN_PATCH=".github/patches/flutter_3.24.4_dropdown_menu_enableFilter.diff"

RD_BRIDGE_OUTPUTS=(
  "src/bridge_generated.rs"
  "src/bridge_generated.io.rs"
  "flutter/lib/generated_bridge.dart"
  "flutter/lib/generated_bridge.freezed.dart"
  "flutter/macos/Runner/bridge_generated.h"
  "flutter/ios/Runner/bridge_generated.h"
)

rd_log() {
  printf '[rustdesk-linux-release] %s\n' "$*"
}

rd_warn() {
  printf '[rustdesk-linux-release][warn] %s\n' "$*" >&2
}

rd_fail() {
  printf '[rustdesk-linux-release][error] %s\n' "$*" >&2
  exit 1
}

rd_require_cmd() {
  command -v "$1" >/dev/null 2>&1 || rd_fail "Missing required command: $1"
}

rd_version() {
  awk -F'"' '/^version = / { print $2; exit }' "${RD_REPO_ROOT}/Cargo.toml"
}

rd_platform_for_arch() {
  case "$1" in
    amd64) printf 'linux/amd64\n' ;;
    arm64) printf 'linux/arm64\n' ;;
    *) rd_fail "Unsupported arch: $1" ;;
  esac
}

rd_image_for_arch() {
  case "$1" in
    amd64) printf '%s\n' "${RD_AMD64_IMAGE}" ;;
    arm64) printf '%s\n' "${RD_ARM64_IMAGE}" ;;
    *) rd_fail "Unsupported arch: $1" ;;
  esac
}

rd_container_arch() {
  case "$1" in
    amd64) printf 'x86_64\n' ;;
    arm64) printf 'aarch64\n' ;;
    *) rd_fail "Unsupported arch: $1" ;;
  esac
}

rd_target_triple() {
  case "$1" in
    amd64) printf 'x86_64-unknown-linux-gnu\n' ;;
    arm64) printf 'aarch64-unknown-linux-gnu\n' ;;
    *) rd_fail "Unsupported arch: $1" ;;
  esac
}

rd_vcpkg_triplet() {
  case "$1" in
    amd64) printf 'x64-linux\n' ;;
    arm64) printf 'arm64-linux\n' ;;
    *) rd_fail "Unsupported arch: $1" ;;
  esac
}

rd_snapshot_dir() {
  case "$1" in
    base) printf '%s\n' "${RD_BUILD_ROOT}/base" ;;
    bridge) printf '%s\n' "${RD_BUILD_ROOT}/bridge" ;;
    amd64) printf '%s\n' "${RD_BUILD_ROOT}/amd64" ;;
    arm64) printf '%s\n' "${RD_BUILD_ROOT}/arm64" ;;
    *) rd_fail "Unsupported snapshot kind: $1" ;;
  esac
}

rd_named_build_dir() {
  local name="$1"
  printf '%s/%s\n' "${RD_BUILD_ROOT}" "${name}"
}

rd_cache_root() {
  case "$1" in
    bridge) printf '%s\n' "${RD_ENV_CACHE_ROOT}/cache-bridge-amd64" ;;
    amd64) printf '%s\n' "${RD_ENV_CACHE_ROOT}/cache-amd64" ;;
    arm64) printf '%s\n' "${RD_ENV_CACHE_ROOT}/cache-arm64" ;;
    arm64-cross) printf '%s\n' "${RD_ENV_CACHE_ROOT}/cache-arm64-cross" ;;
    *) rd_fail "Unsupported cache key: $1" ;;
  esac
}

rd_named_cache_root() {
  local name="$1"
  printf '%s/cache-%s\n' "${RD_ENV_CACHE_ROOT}" "${name}"
}

rd_prepare_cache_dirs() {
  local cache_root="$1"
  mkdir -p \
    "${cache_root}/cargo" \
    "${cache_root}/rustup" \
    "${cache_root}/pub-cache" \
    "${cache_root}/sdk" \
    "${cache_root}/vcpkg" \
    "${cache_root}/apt-cache" \
    "${cache_root}/apt-lists"
}

rd_reset_dir() {
  rm -rf "$1"
  mkdir -p "$1"
}

rd_copy_tree() {
  local src="$1"
  local dest="$2"
  rm -rf "${dest}"
  mkdir -p "${dest}"
  tar -C "${src}" -cf - . | tar -C "${dest}" -xf -
}

rd_snapshot_worktree() {
  local dest="$1"
  rm -rf "${dest}"
  mkdir -p "${dest}"

  tar -C "${RD_REPO_ROOT}" \
    --exclude=".git" \
    --exclude=".build" \
    --exclude="dist" \
    --exclude="target" \
    --exclude="build" \
    --exclude="flutter/build" \
    --exclude="flutter/.dart_tool" \
    --exclude="flutter/.flutter-plugins" \
    --exclude="flutter/.flutter-plugins-dependencies" \
    --exclude="flutter/.packages" \
    --exclude="flutter/.pub-cache" \
    -cf - . | tar -C "${dest}" -xf -
}

rd_require_submodules() {
  local status
  status="$(git -C "${RD_REPO_ROOT}" submodule status --recursive)"
  if printf '%s\n' "${status}" | grep -q '^-'; then
    rd_fail "Submodules are not initialized. Run: git submodule update --init --recursive"
  fi
}

rd_arch_output_path() {
  local version="$1"
  local arch="$2"
  printf '%s/rustdesk-%s-%s.deb\n' "${RD_DIST_ROOT}" "${version}" "${arch}"
}

rd_docker_run() {
  local arch="$1"
  local image="$2"
  local workspace="$3"
  local cache_root="$4"
  local command="$5"

  docker run --rm \
    --platform "$(rd_platform_for_arch "${arch}")" \
    --network host \
    -e CARGO_HOME=/cache/cargo \
    -e RUSTUP_HOME=/cache/rustup \
    -e PUB_CACHE=/cache/pub-cache \
    -e DEBIAN_FRONTEND=noninteractive \
    -v "${workspace}:/workspace" \
    -v "${cache_root}/cargo:/cache/cargo" \
    -v "${cache_root}/rustup:/cache/rustup" \
    -v "${cache_root}/pub-cache:/cache/pub-cache" \
    -v "${cache_root}/sdk:/cache/sdk" \
    -v "${cache_root}/vcpkg:/opt/artifacts/vcpkg" \
    -v "${cache_root}/apt-cache:/var/cache/apt" \
    -v "${cache_root}/apt-lists:/var/lib/apt/lists" \
    "${image}" \
    bash -lc "${command}"
}
