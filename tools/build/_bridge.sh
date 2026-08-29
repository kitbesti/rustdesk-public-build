#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/_lib.sh"

usage() {
  cat <<'EOF'
Usage: tools/build/_bridge.sh --base-snapshot PATH [--bridge-snapshot PATH] [--cache-root PATH]
EOF
}

BASE_SNAPSHOT=""
BRIDGE_SNAPSHOT="$(rd_snapshot_dir bridge)"
CACHE_ROOT="$(rd_cache_root bridge)"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base-snapshot)
      BASE_SNAPSHOT="$2"
      shift 2
      ;;
    --bridge-snapshot)
      BRIDGE_SNAPSHOT="$2"
      shift 2
      ;;
    --cache-root)
      CACHE_ROOT="$2"
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

[[ -n "${BASE_SNAPSHOT}" ]] || rd_fail "--base-snapshot is required"
[[ -d "${BASE_SNAPSHOT}" ]] || rd_fail "Base snapshot not found: ${BASE_SNAPSHOT}"

rd_prepare_cache_dirs "${CACHE_ROOT}"
rd_copy_tree "${BASE_SNAPSHOT}" "${BRIDGE_SNAPSHOT}"

rd_log "Generating flutter_rust_bridge artifacts in ${BRIDGE_SNAPSHOT}"

BRIDGE_COMMAND=$(cat <<EOF
set -euo pipefail

export PATH="\${CARGO_HOME}/bin:\${PATH}"
git config --global --add safe.directory "*"

apt-get update -y
apt-get install -y \
  ca-certificates \
  clang \
  cmake \
  curl \
  gcc \
  g++ \
  git \
  libclang-dev \
  libgtk-3-dev \
  llvm-dev \
  nasm \
  ninja-build \
  pkg-config \
  wget

if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain ${RD_RUST_VERSION}
fi

export PATH="\${HOME}/.cargo/bin:\${CARGO_HOME}/bin:\${PATH}"
rustup default ${RD_RUST_VERSION}
rustup component add rustfmt

if [[ ! -d /cache/sdk/flutter-${RD_BRIDGE_FLUTTER_VERSION} ]]; then
  mkdir -p /cache/sdk
  pushd /cache/sdk >/dev/null
  wget -q -O flutter.tar.xz https://storage.googleapis.com/flutter_infra_release/releases/stable/linux/flutter_linux_${RD_BRIDGE_FLUTTER_VERSION}-stable.tar.xz
  rm -rf flutter-${RD_BRIDGE_FLUTTER_VERSION}
  tar xf flutter.tar.xz
  mv flutter flutter-${RD_BRIDGE_FLUTTER_VERSION}
  rm -f flutter.tar.xz
  popd >/dev/null
fi

export PATH="/cache/sdk/flutter-${RD_BRIDGE_FLUTTER_VERSION}/bin:\${PATH}"

pushd /workspace/flutter >/dev/null
sed -i -e 's/extended_text: 14.0.0/extended_text: 13.0.0/g' pubspec.yaml
flutter pub get
popd >/dev/null

cargo install cargo-expand --version ${RD_CARGO_EXPAND_VERSION} --locked
cargo install flutter_rust_bridge_codegen --version ${RD_FRB_VERSION} --features uuid --locked

pushd /workspace >/dev/null
flutter_rust_bridge_codegen \
  --rust-input ./src/flutter_ffi.rs \
  --dart-output ./flutter/lib/generated_bridge.dart \
  --c-output ./flutter/macos/Runner/bridge_generated.h
cp ./flutter/macos/Runner/bridge_generated.h ./flutter/ios/Runner/bridge_generated.h
popd >/dev/null
EOF
)

rd_docker_run "amd64" "${RD_AMD64_IMAGE}" "${BRIDGE_SNAPSHOT}" "${CACHE_ROOT}" "${BRIDGE_COMMAND}"

for path in "${RD_BRIDGE_OUTPUTS[@]}"; do
  [[ -f "${BRIDGE_SNAPSHOT}/${path}" ]] || rd_fail "Bridge output missing: ${path}"
  mkdir -p "${BASE_SNAPSHOT}/$(dirname "${path}")"
  cp -f "${BRIDGE_SNAPSHOT}/${path}" "${BASE_SNAPSHOT}/${path}"
done

rd_log "Bridge artifacts copied into ${BASE_SNAPSHOT}"
