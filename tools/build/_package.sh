#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/_lib.sh"

usage() {
  cat <<'EOF'
Usage: tools/build/_package.sh --arch amd64|arm64 --base-snapshot PATH --output PATH
EOF
}

ARCH=""
BASE_SNAPSHOT=""
OUTPUT_PATH=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --arch)
      ARCH="$2"
      shift 2
      ;;
    --base-snapshot)
      BASE_SNAPSHOT="$2"
      shift 2
      ;;
    --output)
      OUTPUT_PATH="$2"
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

[[ -n "${ARCH}" ]] || rd_fail "--arch is required"
[[ -n "${BASE_SNAPSHOT}" ]] || rd_fail "--base-snapshot is required"
[[ -n "${OUTPUT_PATH}" ]] || rd_fail "--output is required"

case "${ARCH}" in
  amd64|arm64) ;;
  *) rd_fail "Unsupported arch: ${ARCH}" ;;
esac

[[ -d "${BASE_SNAPSHOT}" ]] || rd_fail "Base snapshot not found: ${BASE_SNAPSHOT}"

WORKSPACE="$(rd_snapshot_dir "${ARCH}")"
CACHE_ROOT="$(rd_cache_root "${ARCH}")"

rd_prepare_cache_dirs "${CACHE_ROOT}"
rd_copy_tree "${BASE_SNAPSHOT}" "${WORKSPACE}"

JOBS_FLAG="--jobs ${RD_BUILD_JOBS:-24}"

PACKAGE_COMMAND=$(cat <<EOF
set -euo pipefail

export PATH="\${CARGO_HOME}/bin:\${PATH}"
git config --global --add safe.directory "*"

apply_patch_once() {
  local patch_file="\$1"
  if git apply --check "\${patch_file}" >/dev/null 2>&1; then
    git apply "\${patch_file}"
    return
  fi
  if git apply --reverse --check "\${patch_file}" >/dev/null 2>&1; then
    return
  fi
  echo "Unable to apply patch: \${patch_file}" >&2
  exit 1
}

apt-get update -y
apt-get install -y \
  build-essential \
  clang \
  cmake \
  curl \
  gcc \
  g++ \
  git \
  libayatana-appindicator3-dev \
  libasound2-dev \
  libbz2-dev \
  libclang-10-dev \
  libffi-dev \
  libgdbm-dev \
  libgstreamer1.0-dev \
  libgstreamer-plugins-base1.0-dev \
  libgtk-3-dev \
  liblzma-dev \
  libncurses5-dev \
  libpam0g-dev \
  libpulse-dev \
  libreadline-dev \
  libsqlite3-dev \
  libssl-dev \
  libva-dev \
  libxcb-randr0-dev \
  libxcb-shape0-dev \
  libxcb-xfixes0-dev \
  libxdo-dev \
  libxfixes-dev \
  llvm-10-dev \
  nasm \
  ninja-build \
  pkg-config \
  python3 \
  rpm \
  unzip \
  wget \
  xz-utils \
  zlib1g-dev \
  zip
apt-get remove -y libopus-dev || true

ensure_modern_nasm() {
  local wanted_version="2.16.03"
  local install_root="/cache/sdk/nasm-\${wanted_version}"
  local tarball="/cache/sdk/nasm-\${wanted_version}.tar.xz"
  local build_dir="/cache/sdk/nasm-\${wanted_version}-src"
  local current_version=""

  if command -v nasm >/dev/null 2>&1; then
    current_version="\$(nasm -v | awk '{print \$3}')"
  fi

  if [[ -n "\${current_version}" ]] && dpkg --compare-versions "\${current_version}" ge 2.14; then
    return
  fi

  mkdir -p /cache/sdk
  if [[ ! -x "\${install_root}/bin/nasm" ]]; then
    rm -rf "\${build_dir}"
    wget -q -O "\${tarball}" "https://www.nasm.us/pub/nasm/releasebuilds/\${wanted_version}/nasm-\${wanted_version}.tar.xz"
    mkdir -p "\${build_dir}"
    tar -xf "\${tarball}" -C "\${build_dir}" --strip-components=1
    pushd "\${build_dir}" >/dev/null
    ./configure --prefix="\${install_root}"
    make -j"$(nproc)"
    make install
    popd >/dev/null
    rm -rf "\${build_dir}"
  fi

  export PATH="\${install_root}/bin:\${PATH}"
}

ensure_modern_nasm

ensure_modern_python() {
  local wanted_version="3.11.11"
  local install_root="/cache/sdk/python-\${wanted_version}"
  local tarball="/cache/sdk/Python-\${wanted_version}.tgz"
  local build_dir="/cache/sdk/Python-\${wanted_version}"

  if command -v python3 >/dev/null 2>&1 && python3 -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 7) else 1)'; then
    return
  fi

  mkdir -p /cache/sdk
  if [[ ! -x "\${install_root}/bin/python3" ]]; then
    rm -rf "\${build_dir}"
    wget -q -O "\${tarball}" "https://www.python.org/ftp/python/\${wanted_version}/Python-\${wanted_version}.tgz"
    tar -xf "\${tarball}" -C /cache/sdk
    pushd "\${build_dir}" >/dev/null
    ./configure --prefix="\${install_root}" --without-ensurepip
    make -j4
    make install
    popd >/dev/null
    rm -rf "\${build_dir}"
  fi

  export PATH="\${install_root}/bin:\${PATH}"
  hash -r
}

ensure_modern_python

mkdir -p /opt/artifacts
if [[ ! -d /opt/artifacts/vcpkg/.git ]]; then
  git init /opt/artifacts/vcpkg
  git -C /opt/artifacts/vcpkg remote remove origin >/dev/null 2>&1 || true
  git -C /opt/artifacts/vcpkg remote add origin https://github.com/microsoft/vcpkg
fi
pushd /opt/artifacts/vcpkg >/dev/null
git fetch --all --tags
git reset --hard ${RD_VCPKG_COMMIT}
if [[ ! -x ./vcpkg ]]; then
  ./bootstrap-vcpkg.sh -disableMetrics
fi
popd >/dev/null
export VCPKG_ROOT=/opt/artifacts/vcpkg

if ! command -v cargo >/dev/null 2>&1 || ! cargo --version | grep -q '${RD_RUST_VERSION}'; then
  pushd /cache/sdk >/dev/null
  wget -q -O rust.tar.gz https://static.rust-lang.org/dist/rust-${RD_RUST_VERSION}-$(rd_target_triple "${ARCH}").tar.gz
  rm -rf rust-${RD_RUST_VERSION}-$(rd_target_triple "${ARCH}")
  tar -zxf rust.tar.gz
  pushd rust-${RD_RUST_VERSION}-$(rd_target_triple "${ARCH}") >/dev/null
  ./install.sh
  popd >/dev/null
  rm -rf rust-${RD_RUST_VERSION}-$(rd_target_triple "${ARCH}") rust.tar.gz
  popd >/dev/null
fi
export PATH="/usr/local/bin:\${CARGO_HOME}/bin:\${PATH}"

pushd /workspace >/dev/null
if ! \$VCPKG_ROOT/vcpkg install --triplet $(rd_vcpkg_triplet "${ARCH}") --x-install-root="\$VCPKG_ROOT/installed"; then
  find "\${VCPKG_ROOT}/" -name "*.log" -print -exec sh -c 'echo "======"; cat "\$1"; echo "======"' sh {} \;
  exit 1
fi
popd >/dev/null

pushd /workspace >/dev/null
sed -i 's/crate-type = \["cdylib", "staticlib", "rlib"\]/crate-type = ["cdylib"]/g' Cargo.toml
cargo build --lib ${JOBS_FLAG} --features hwcodec,flutter,unix-file-copy-paste --release
rm -rf target/release/deps target/release/build
popd >/dev/null

case "${ARCH}" in
  amd64)
    if [[ ! -d /cache/sdk/flutter-${RD_PACKAGE_FLUTTER_VERSION} ]]; then
      mkdir -p /cache/sdk
      pushd /cache/sdk >/dev/null
      wget -q -O flutter.tar.xz https://storage.googleapis.com/flutter_infra_release/releases/stable/linux/flutter_linux_${RD_PACKAGE_FLUTTER_VERSION}-stable.tar.xz
      rm -rf flutter-${RD_PACKAGE_FLUTTER_VERSION}
      tar xf flutter.tar.xz
      mv flutter flutter-${RD_PACKAGE_FLUTTER_VERSION}
      rm -f flutter.tar.xz
      popd >/dev/null
    fi
    export PATH="/cache/sdk/flutter-${RD_PACKAGE_FLUTTER_VERSION}/bin:\${PATH}"
    pushd /cache/sdk/flutter-${RD_PACKAGE_FLUTTER_VERSION} >/dev/null
    flutter doctor -v
    apply_patch_once /workspace/${RD_DROPDOWN_PATCH}
    popd >/dev/null
    ;;
  arm64)
    if [[ ! -d /cache/sdk/flutter-${RD_PACKAGE_FLUTTER_VERSION} ]]; then
      mkdir -p /cache/sdk
      pushd /cache/sdk >/dev/null
      wget -q -O flutter.tar.xz https://storage.googleapis.com/flutter_infra_release/releases/stable/linux/flutter_linux_${RD_PACKAGE_FLUTTER_VERSION}-stable.tar.xz
      rm -rf flutter-${RD_PACKAGE_FLUTTER_VERSION}
      tar xf flutter.tar.xz
      mv flutter flutter-${RD_PACKAGE_FLUTTER_VERSION}
      rm -f flutter.tar.xz
      popd >/dev/null
    fi
    if [[ ! -d /cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/.git ]]; then
      rm -rf /cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}
      git clone https://github.com/sony/flutter-elinux.git /cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}
    fi
    pushd /cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION} >/dev/null
    git fetch --all --tags
    git reset --hard ${RD_PACKAGE_FLUTTER_VERSION}
    export PATH="/cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/bin:\${PATH}"
    flutter-elinux doctor -v
    flutter-elinux precache --linux
    pushd flutter >/dev/null
    apply_patch_once /workspace/${RD_DROPDOWN_PATCH}
    popd >/dev/null
    popd >/dev/null
    mkdir -p /cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/flutter/bin/cache/artifacts/engine/linux-arm64
    rm -rf /cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/flutter/bin/cache/artifacts/engine/linux-arm64/shader_lib
    cp -R \
      /cache/sdk/flutter-${RD_PACKAGE_FLUTTER_VERSION}/bin/cache/artifacts/engine/linux-x64/shader_lib \
      /cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/flutter/bin/cache/artifacts/engine/linux-arm64/
    pushd /workspace >/dev/null
    sed -i 's|build/linux/x64/release/bundle/|build/linux/arm64/release/bundle/|g' build.py
    sed -i 's|flutter build linux --release|flutter-elinux build linux --verbose|g' build.py
    popd >/dev/null
    ;;
esac

pushd /workspace >/dev/null
export CARGO_INCREMENTAL=0
export DEB_ARCH=${ARCH}
python3 ./build.py --flutter --skip-cargo
version=\$(python3 - <<'PY'
from pathlib import Path
for line in Path("Cargo.toml").read_text(encoding="utf-8").splitlines():
    if line.startswith("version"):
        print(line.split('"')[1])
        break
PY
)
mv "rustdesk-\${version}.deb" "/workspace/output.deb"
popd >/dev/null
EOF
)

rd_log "Building ${ARCH} package in ${WORKSPACE}"
rd_docker_run "${ARCH}" "$(rd_image_for_arch "${ARCH}")" "${WORKSPACE}" "${CACHE_ROOT}" "${PACKAGE_COMMAND}"

mkdir -p "$(dirname "${OUTPUT_PATH}")"
mv -f "${WORKSPACE}/output.deb" "${OUTPUT_PATH}"

rd_log "Package written to ${OUTPUT_PATH}"
