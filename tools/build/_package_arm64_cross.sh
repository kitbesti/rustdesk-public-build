#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/_lib.sh"

usage() {
  cat <<'EOF'
Usage: tools/build/_package_arm64_cross.sh --base-snapshot PATH --output PATH
EOF
}

BASE_SNAPSHOT=""
OUTPUT_PATH=""

while [[ $# -gt 0 ]]; do
  case "$1" in
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

[[ -n "${BASE_SNAPSHOT}" ]] || rd_fail "--base-snapshot is required"
[[ -n "${OUTPUT_PATH}" ]] || rd_fail "--output is required"
[[ -d "${BASE_SNAPSHOT}" ]] || rd_fail "Base snapshot not found: ${BASE_SNAPSHOT}"

WORKSPACE="${RD_BUILD_ROOT}/arm64-cross"
CACHE_ROOT="$(rd_cache_root arm64-cross)"
SYSROOT_DIR="${CACHE_ROOT}/sdk/sysroot"
CROSS_TOOLS_DIR="${WORKSPACE}/cross-tools"
REUSE_WORKSPACE="${RD_ARM64_CROSS_REUSE_WORKSPACE:-0}"
SKIP_RUST="${RD_ARM64_CROSS_SKIP_RUST:-0}"

[[ -d "${SYSROOT_DIR}/usr" ]] || rd_fail "Cross-build sysroot not found: ${SYSROOT_DIR}"

rd_prepare_cache_dirs "${CACHE_ROOT}"
if [[ "${REUSE_WORKSPACE}" == "1" ]]; then
  [[ -d "${WORKSPACE}" ]] || rd_fail "Workspace not found for reuse: ${WORKSPACE}"
else
  rd_copy_tree "${BASE_SNAPSHOT}" "${WORKSPACE}"
fi

mkdir -p "${CROSS_TOOLS_DIR}/cmake" "${CROSS_TOOLS_DIR}/vcpkg-triplets"
cp -f "${RD_REPO_ROOT}/tools/build/_assemble_flutter_deb.py" "${CROSS_TOOLS_DIR}/_assemble_flutter_deb.py"
cp -f "${RD_REPO_ROOT}/tools/build/cmake/aarch64-linux-gnu.cmake" "${CROSS_TOOLS_DIR}/cmake/aarch64-linux-gnu.cmake"
cp -f "${RD_REPO_ROOT}/tools/build/vcpkg-triplets/arm64-linux-cross.cmake" "${CROSS_TOOLS_DIR}/vcpkg-triplets/arm64-linux-cross.cmake"
chmod +x "${CROSS_TOOLS_DIR}/_assemble_flutter_deb.py"

JOBS_FLAG="--jobs ${RD_BUILD_JOBS:-24}"

RUST_COMMAND=$(cat <<EOF
set -euo pipefail

export PATH="/usr/local/bin:\${CARGO_HOME}/bin:\${PATH}"
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
  ca-certificates \
  clang \
  cmake \
  crossbuild-essential-arm64 \
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

ensure_modern_cmake() {
  local wanted_version="3.31.6"
  local install_root="/cache/sdk/cmake-\${wanted_version}"
  local tarball="/cache/sdk/cmake-\${wanted_version}-linux-x86_64.tar.gz"
  local current_version=""

  if command -v cmake >/dev/null 2>&1; then
    current_version="\$(cmake --version | awk 'NR==1 { print \$3 }')"
  fi

  if [[ -n "\${current_version}" ]] && dpkg --compare-versions "\${current_version}" ge 3.21; then
    return
  fi

  mkdir -p /cache/sdk
  if [[ ! -x "\${install_root}/bin/cmake" ]]; then
    rm -rf "\${install_root}"
    wget -q -O "\${tarball}" "https://github.com/Kitware/CMake/releases/download/v\${wanted_version}/cmake-\${wanted_version}-linux-x86_64.tar.gz"
    mkdir -p "\${install_root}"
    tar -xf "\${tarball}" -C "\${install_root}" --strip-components=1
  fi

  export PATH="\${install_root}/bin:\${PATH}"
  hash -r
}

ensure_modern_cmake

if ! command -v rustup >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain ${RD_RUST_VERSION}
fi
export PATH="\${CARGO_HOME}/bin:\${PATH}"
rustup toolchain install ${RD_RUST_VERSION} --profile minimal
rustup default ${RD_RUST_VERSION}
rustup target add aarch64-unknown-linux-gnu

mkdir -p /opt/artifacts
if [[ ! -d /opt/artifacts/vcpkg/.git ]]; then
  git init /opt/artifacts/vcpkg
  git -C /opt/artifacts/vcpkg remote remove origin >/dev/null 2>&1 || true
  git -C /opt/artifacts/vcpkg remote add origin https://github.com/microsoft/vcpkg
fi
pushd /opt/artifacts/vcpkg >/dev/null
if ! git rev-parse --verify ${RD_VCPKG_COMMIT}^{commit} >/dev/null 2>&1; then
  git fetch --all --tags
fi
git reset --hard ${RD_VCPKG_COMMIT}
if [[ ! -x ./vcpkg ]]; then
  ./bootstrap-vcpkg.sh -disableMetrics
fi
popd >/dev/null

export VCPKG_ROOT=/opt/artifacts/vcpkg
export VCPKG_FORCE_SYSTEM_BINARIES=1
export VCPKG_DEFAULT_HOST_TRIPLET=x64-linux
export RD_SYSROOT=/cache/sdk/sysroot
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="\${RD_SYSROOT}"
export PKG_CONFIG_LIBDIR="\${RD_SYSROOT}/usr/lib/aarch64-linux-gnu/pkgconfig:\${RD_SYSROOT}/usr/lib/pkgconfig:\${RD_SYSROOT}/usr/share/pkgconfig"
export PKG_CONFIG_PATH="\${PKG_CONFIG_LIBDIR}"
export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
export CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++
export AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar
export RANLIB_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ranlib
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc

pushd /workspace >/dev/null
if ! \$VCPKG_ROOT/vcpkg install \
  --overlay-ports=/workspace/res/vcpkg \
  --overlay-triplets=/workspace/cross-tools/vcpkg-triplets \
  --triplet arm64-linux-cross \
  --x-install-root="\$VCPKG_ROOT/installed"; then
  find "\${VCPKG_ROOT}/" -name "*.log" -print -exec sh -c 'echo "======"; cat "\$1"; echo "======"' sh {} \;
  exit 1
fi
popd >/dev/null

pushd /opt/artifacts/vcpkg >/dev/null
if ! \$VCPKG_ROOT/vcpkg install \
  --classic \
  --overlay-ports=/workspace/res/vcpkg \
  --overlay-triplets=/workspace/cross-tools/vcpkg-triplets \
  --triplet arm64-linux-cross \
  --x-install-root="\$VCPKG_ROOT/installed" \
  "ffmpeg[amf,nvcodec]:arm64-linux-cross"; then
  find "\${VCPKG_ROOT}/" -name "*.log" -print -exec sh -c 'echo "======"; cat "\$1"; echo "======"' sh {} \;
  exit 1
fi
popd >/dev/null

pushd /workspace >/dev/null
ln -sfn arm64-linux-cross "\$VCPKG_ROOT/installed/arm64-linux"
test -f "\$VCPKG_ROOT/installed/arm64-linux/include/libavcodec/avcodec.h"
test -f "\$VCPKG_ROOT/installed/arm64-linux/include/libavutil/attributes.h"
popd >/dev/null

target_pkg_config_path="\${RD_SYSROOT}/usr/lib/aarch64-linux-gnu/pkgconfig:\${RD_SYSROOT}/usr/lib/pkgconfig:\${RD_SYSROOT}/usr/share/pkgconfig"
unset PKG_CONFIG_SYSROOT_DIR PKG_CONFIG_LIBDIR PKG_CONFIG_PATH
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR_aarch64_unknown_linux_gnu="\${RD_SYSROOT}"
export PKG_CONFIG_LIBDIR_aarch64_unknown_linux_gnu="\${target_pkg_config_path}"
export PKG_CONFIG_PATH_aarch64_unknown_linux_gnu="\${target_pkg_config_path}"
export PKG_CONFIG_aarch64_unknown_linux_gnu=/usr/bin/pkg-config
target_clang_flags="--sysroot=\${RD_SYSROOT} -I\${RD_SYSROOT}/usr/include -I\${RD_SYSROOT}/usr/include/aarch64-linux-gnu"
target_link_flags="--sysroot=\${RD_SYSROOT} -Wl,-rpath-link,\${RD_SYSROOT}/lib/aarch64-linux-gnu -Wl,-rpath-link,\${RD_SYSROOT}/usr/lib/aarch64-linux-gnu"
export BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_gnu="\${target_clang_flags}"
export CFLAGS_aarch64_unknown_linux_gnu="\${target_clang_flags}"
export CXXFLAGS_aarch64_unknown_linux_gnu="\${target_clang_flags}"
export CPPFLAGS_aarch64_unknown_linux_gnu="\${target_clang_flags}"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-Clink-arg=--sysroot=\${RD_SYSROOT} -Clink-arg=-Wl,-rpath-link,\${RD_SYSROOT}/lib/aarch64-linux-gnu -Clink-arg=-Wl,-rpath-link,\${RD_SYSROOT}/usr/lib/aarch64-linux-gnu"

pushd /workspace >/dev/null
sed -i 's/crate-type = \["cdylib", "staticlib", "rlib"\]/crate-type = ["cdylib"]/g' Cargo.toml
cargo build --target aarch64-unknown-linux-gnu --lib ${JOBS_FLAG} --features hwcodec,flutter,unix-file-copy-paste --release
mkdir -p target/release
cp -f target/aarch64-unknown-linux-gnu/release/liblibrustdesk.so target/release/liblibrustdesk.so
rm -rf target/aarch64-unknown-linux-gnu/release/deps target/aarch64-unknown-linux-gnu/release/build
popd >/dev/null
EOF
)

FLUTTER_COMMAND=$(cat <<EOF
set -euo pipefail

export PATH="/usr/local/bin:\${PATH}"
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
  ca-certificates \
  clang \
  cmake \
  crossbuild-essential-arm64 \
  curl \
  gcc \
  g++ \
  git \
  libayatana-appindicator3-dev \
  libasound2-dev \
  libbz2-dev \
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
  ninja-build \
  pkg-config \
  python3 \
  rpm \
  unzip \
  wget \
  xz-utils \
  zlib1g-dev \
  zip

export RD_SYSROOT=/cache/sdk/sysroot
export QEMU_LD_PREFIX="\${RD_SYSROOT}"
target_pkg_config_path="\${RD_SYSROOT}/usr/lib/aarch64-linux-gnu/pkgconfig:\${RD_SYSROOT}/usr/lib/pkgconfig:\${RD_SYSROOT}/usr/share/pkgconfig"
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="\${RD_SYSROOT}"
export PKG_CONFIG_LIBDIR="\${target_pkg_config_path}"
export PKG_CONFIG_PATH="\${target_pkg_config_path}"

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
if ! git rev-parse --verify ${RD_PACKAGE_FLUTTER_VERSION}^{commit} >/dev/null 2>&1; then
  git fetch --all --tags
fi
git reset --hard ${RD_PACKAGE_FLUTTER_VERSION}
export PATH="/cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/bin:\${PATH}"
flutter-elinux doctor -v || true
flutter-elinux precache --linux
ensure_linux_arm64_release_artifacts() {
  local flutter_root="/cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/flutter"
  local engine_version
  local artifact_dir
  local artifact_zip
  local artifact_url

  engine_version="\$(cat "\${flutter_root}/bin/internal/engine.version")"
  artifact_dir="\${flutter_root}/bin/cache/artifacts/engine/linux-arm64-release"
  if [[ -x "\${artifact_dir}/gen_snapshot" ]] && [[ -f "\${artifact_dir}/libflutter_linux_gtk.so" ]]; then
    return
  fi

  mkdir -p /cache/sdk
  artifact_zip="/cache/sdk/flutter-linux-arm64-release-\${engine_version}.zip"
  artifact_url="https://storage.googleapis.com/flutter_infra_release/flutter/\${engine_version}/linux-arm64-release/linux-arm64-flutter-gtk.zip"
  wget -q -O "\${artifact_zip}" "\${artifact_url}"
  rm -rf "\${artifact_dir}"
  mkdir -p "\${artifact_dir}"
  unzip -qo "\${artifact_zip}" -d "\${artifact_dir}"
  chmod +x "\${artifact_dir}/gen_snapshot" || true
}

ensure_linux_arm64_release_artifacts

python3 - <<PY
from pathlib import Path
path = Path("/cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/flutter/packages/flutter_tools/lib/src/commands/build_linux.dart")
text = path.read_text(encoding="utf-8")
old = """    // TODO(fujino): https://github.com/flutter/flutter/issues/74929\n    if (_operatingSystemUtils.hostPlatform == HostPlatform.linux_x64 &&\n        targetPlatform == TargetPlatform.linux_arm64) {\n      throwToolExit(\n          'Cross-build from Linux x64 host to Linux arm64 target is not currently supported.');\n    }\n"""
new = "    // Enabled by RustDesk arm64-cross packaging.\\n"
if old in text:
    text = text.replace(old, new)
path.write_text(text, encoding="utf-8")
PY
rm -f \
  /cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/bin/cache/flutter-elinux.snapshot \
  /cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/bin/cache/flutter-elinux.stamp \
  /cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/flutter/bin/cache/flutter_tools.snapshot \
  /cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/flutter/bin/cache/flutter_tools.stamp
pushd flutter >/dev/null
apply_patch_once /workspace/${RD_DROPDOWN_PATCH}
popd >/dev/null
popd >/dev/null

mkdir -p /cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/flutter/bin/cache/artifacts/engine/linux-arm64
rm -rf /cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/flutter/bin/cache/artifacts/engine/linux-arm64/shader_lib
cp -R \
  /cache/sdk/flutter-${RD_PACKAGE_FLUTTER_VERSION}/bin/cache/artifacts/engine/linux-x64/shader_lib \
  /cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/flutter/bin/cache/artifacts/engine/linux-arm64/
cp -f \
  /cache/sdk/flutter-${RD_PACKAGE_FLUTTER_VERSION}/bin/cache/artifacts/engine/linux-x64/icudtl.dat \
  /cache/sdk/flutter-elinux-${RD_PACKAGE_FLUTTER_VERSION}/flutter/bin/cache/artifacts/engine/linux-arm64/icudtl.dat

pushd /workspace >/dev/null
sed -i 's|build/linux/x64/release/bundle/|build/linux/arm64/release/bundle/|g' build.py
python3 - <<'PY'
from pathlib import Path
path = Path("build.py")
text = path.read_text(encoding="utf-8")
old = "system2('flutter build linux --release')"
new = 'system2("flutter-elinux build linux --verbose --target-platform=linux-arm64 --target-sysroot=/cache/sdk/sysroot")'
if old in text:
    text = text.replace(old, new)
path.write_text(text, encoding="utf-8")
PY
export CARGO_INCREMENTAL=0
export DEB_ARCH=arm64
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

if [[ "${SKIP_RUST}" == "1" ]]; then
  rd_log "Reusing existing arm64 Rust artifacts in ${WORKSPACE}"
  [[ -f "${WORKSPACE}/target/aarch64-unknown-linux-gnu/release/liblibrustdesk.so" ]] || rd_fail "Missing reused arm64 library artifact"
  [[ -f "${WORKSPACE}/target/release/liblibrustdesk.so" ]] || rd_fail "Missing reused release library artifact"
else
  rd_log "Cross-compiling arm64 Rust artifacts in ${WORKSPACE}"
  docker run --rm \
    --platform linux/amd64 \
    --network host \
    -v /etc/resolv.conf:/etc/resolv.conf:ro \
    -v /etc/hosts:/etc/hosts:ro \
    -v /etc/nsswitch.conf:/etc/nsswitch.conf:ro \
    -e CARGO_HOME=/cache/cargo \
    -e RUSTUP_HOME=/cache/rustup \
    -e PUB_CACHE=/cache/pub-cache \
    -e DEBIAN_FRONTEND=noninteractive \
    -v "${WORKSPACE}:/workspace" \
    -v "${CACHE_ROOT}/cargo:/cache/cargo" \
    -v "${CACHE_ROOT}/rustup:/cache/rustup" \
    -v "${CACHE_ROOT}/pub-cache:/cache/pub-cache" \
    -v "${CACHE_ROOT}/sdk:/cache/sdk" \
    -v "${CACHE_ROOT}/vcpkg:/opt/artifacts/vcpkg" \
    -v "${CACHE_ROOT}/apt-cache:/var/cache/apt" \
    -v "${CACHE_ROOT}/apt-lists:/var/lib/apt/lists" \
    "${RD_AMD64_IMAGE}" \
    bash -lc "${RUST_COMMAND}"
fi

rd_log "Packaging arm64 Flutter bundle in ${WORKSPACE}"
docker run --rm \
  --platform linux/amd64 \
  --network host \
  -v /etc/resolv.conf:/etc/resolv.conf:ro \
  -v /etc/hosts:/etc/hosts:ro \
  -v /etc/nsswitch.conf:/etc/nsswitch.conf:ro \
  -e CARGO_HOME=/cache/cargo \
  -e RUSTUP_HOME=/cache/rustup \
  -e PUB_CACHE=/cache/pub-cache \
  -e DEBIAN_FRONTEND=noninteractive \
  -v "${WORKSPACE}:/workspace" \
    -v "${CACHE_ROOT}/cargo:/cache/cargo" \
    -v "${CACHE_ROOT}/rustup:/cache/rustup" \
    -v "${CACHE_ROOT}/pub-cache:/cache/pub-cache" \
    -v "${CACHE_ROOT}/sdk:/cache/sdk" \
    -v "${CACHE_ROOT}/vcpkg:/opt/artifacts/vcpkg" \
    -v "${CACHE_ROOT}/apt-cache:/var/cache/apt" \
    -v "${CACHE_ROOT}/apt-lists:/var/lib/apt/lists" \
    "${RD_AMD64_IMAGE}" \
    bash -lc "${FLUTTER_COMMAND}"

mkdir -p "$(dirname "${OUTPUT_PATH}")"
mv -f "${WORKSPACE}/output.deb" "${OUTPUT_PATH}"

rd_log "Package written to ${OUTPUT_PATH}"
