# RustDesk — Complete Cross-Platform Build Guide

This is the authoritative build document for this fork. It covers every target, what
can be produced on a local Linux host, and what must go through CI.

Companion documents:

- [`BUILD-LINUX-RELEASE.md`](BUILD-LINUX-RELEASE.md) — Linux `.deb` release flow in depth
- [`PUBLIC-GITHUB-BUILD.md`](PUBLIC-GITHUB-BUILD.md) — building Windows/macOS/Android/iOS on free GitHub runners

---

## 1. Target matrix

| Target | Where it can be built | Command |
|---|---|---|
| Rust lib `liblibrustdesk.so` (amd64) | **Local, no Docker** | `cargo build --release --lib --features flutter,hwcodec` |
| Flutter Linux desktop bundle (amd64) | **Local, no Docker** | `cd flutter && flutter build linux --release` |
| Sciter/default binaries (amd64) | **Local, no Docker** | `cargo build --release --features hwcodec` |
| Linux amd64 `.deb` | Local (Docker) | `bash tools/build/linux-release.sh --arch amd64` |
| Linux arm64 `.deb` | Local (Docker + QEMU) | `bash tools/build/linux-release.sh --arch arm64` |
| Linux arm64 `.deb` (fast) | Local (host cross-compile) | `bash tools/build/linux-release-arm64-cross.sh` |
| Windows amd64 | GitHub Actions | `bash tools/github/create-and-run-public-build.sh` |
| macOS amd64 / arm64 | GitHub Actions | same as above |
| Android arm64 | GitHub Actions | same as above |
| iOS (unsigned xcarchive) | GitHub Actions | `bash tools/github/create-and-run-public-ios-build.sh` |
| iOS `.ipa` | Real Mac only | `bash tools/ios/export-personal-team-ipa.sh` |

The first three rows need only §2's rootless setup — no container and no root. That
covers everything a Linux amd64 change can be compiled and smoke-tested against; only
*packaging* into a `.deb` still needs Docker.

Windows and macOS **cannot** be cross-compiled from Linux in this project: they need
platform SDKs (MSVC / Xcode) plus platform-specific vcpkg triplets and Flutter
desktop toolchains. Use the GitHub flow for those.

Android is not a toolchain limitation but a dependency one: it additionally needs a
JDK, the Android SDK and NDK, and — the real cost — the whole vcpkg set (libvpx,
libyuv, opus, aom) rebuilt for an `arm64-android` triplet. The GitHub flow is far
cheaper than assembling that locally.

---

## 2. Verifying Rust changes locally without root

Full packaging needs Docker. But to **type-check and unit-test Rust changes** you only
need a Rust toolchain, the vcpkg media libraries, and the Linux `-dev` headers. All of
that can be staged without `sudo`.

### 2.1 Media codecs (vcpkg)

`libs/scrap` links `libvpx`, `libyuv`, `opus`, `aom`, and (with `hwcodec`) FFmpeg.
The Linux release build leaves a populated vcpkg tree behind at:

```
.build/linux/cache/amd64/vcpkg
```

Point `VCPKG_ROOT` at it and no rebuild is needed. If it is absent, run a Linux
release build once (§3), or bootstrap vcpkg manually and
`vcpkg install libvpx libyuv opus aom`.

### 2.2 System headers, rootlessly

`apt-get download` and `dpkg-deb -x` do not require root, so the `-dev` packages can be
unpacked into a private sysroot:

```bash
SYSROOT=/tmp/rd-sysroot
mkdir -p "$SYSROOT/.debs" && cd "$SYSROOT/.debs"

ROOTS="libgtk-3-dev libdbus-1-dev libpulse-dev libx11-dev libxcb1-dev
       libxcb-randr0-dev libxcb-shm0-dev libxcb-xfixes0-dev libxcb-damage0-dev libxfixes-dev
       libxrandr-dev libxtst-dev libxinerama-dev libxi-dev libevdev-dev
       libasound2-dev libudev-dev libwayland-dev libxkbcommon-dev libglib2.0-dev
       libva-dev libdrm-dev libgbm-dev libegl1-mesa-dev libgl1-mesa-dev
       libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev
       libclang-18-dev libclang-common-18-dev clang-18"

apt-cache depends --recurse --no-recommends --no-suggests --no-conflicts \
  --no-breaks --no-replaces --no-enhances $ROOTS \
  | grep -v '^ ' | grep -v '^<' | sort -u | xargs -n 40 apt-get download

for d in *.deb; do dpkg-deb -x "$d" "$SYSROOT"; done

# Absolute symlinks inside the .debs point at the real root; retarget them.
find "$SYSROOT" -type l | while read -r l; do
  t=$(readlink "$l"); case "$t" in /*) ln -sf "$SYSROOT$t" "$l";; esac
done
```

`libclang` is required because `libs/scrap` runs `bindgen` over the vpx headers.
`gstreamer` is required by the camera capture path.

### 2.3 Environment

```bash
export PATH="$HOME/.cargo/bin:$SYSROOT/usr/lib/llvm-18/bin:$PATH"
export VCPKG_ROOT="$PWD/.build/linux/cache/amd64/vcpkg"

export PKG_CONFIG_SYSROOT_DIR="$SYSROOT"
export PKG_CONFIG_PATH="$SYSROOT/usr/lib/x86_64-linux-gnu/pkgconfig:$SYSROOT/usr/share/pkgconfig:$SYSROOT/usr/lib/pkgconfig"
export PKG_CONFIG_ALLOW_SYSTEM_LIBS=1
export PKG_CONFIG_ALLOW_SYSTEM_CFLAGS=1

# bindgen needs libclang *and* clang's own builtin headers (stddef.h et al).
export LIBCLANG_PATH="$SYSROOT/usr/lib/x86_64-linux-gnu"
export BINDGEN_EXTRA_CLANG_ARGS="-I$SYSROOT/usr/lib/llvm-18/lib/clang/18/include -I$SYSROOT/usr/include -I$SYSROOT/usr/include/x86_64-linux-gnu"

export RUSTFLAGS="-L $SYSROOT/usr/lib/x86_64-linux-gnu -L $SYSROOT/lib/x86_64-linux-gnu"
export LD_LIBRARY_PATH="$SYSROOT/usr/lib/x86_64-linux-gnu:$SYSROOT/lib/x86_64-linux-gnu:$LD_LIBRARY_PATH"
```

### 2.4 Check and test

```bash
cargo check -p hbb_common
cargo check -p scrap
cargo check --lib --features hwcodec
cargo test  --lib --features hwcodec video_qos
```

### 2.5 flutter_rust_bridge, without Docker

`--features flutter` needs `src/bridge_generated.rs`, produced by
`flutter_rust_bridge_codegen`. `tools/build/_bridge.sh` does this in a container, but it
runs just as well on the host. Versions are pinned in `tools/build/_lib.sh` — match them,
the codegen is version-sensitive:

```bash
# Flutter 3.22.3 is the version the bridge step pins (RD_BRIDGE_FLUTTER_VERSION).
cd ~/sdk
wget -O flutter.tar.xz https://storage.googleapis.com/flutter_infra_release/releases/stable/linux/flutter_linux_3.22.3-stable.tar.xz
tar xf flutter.tar.xz && mv flutter flutter-3.22.3
git config --global --add safe.directory ~/sdk/flutter-3.22.3
export PATH="$HOME/sdk/flutter-3.22.3/bin:$PATH"

cargo install flutter_rust_bridge_codegen --version 1.80.1 --features uuid --locked
cargo install cargo-expand --version 1.0.95 --locked
```

Then, from the repo root:

```bash
cd flutter
# Flutter 3.22.3 cannot resolve extended_text 14.0.0. The upstream script patches this
# in a throwaway snapshot; do it temporarily and revert afterwards - it is a build-time
# workaround, not a source change.
sed -i 's/extended_text: 14.0.0/extended_text: 13.0.0/' pubspec.yaml
flutter pub get
cd ..

SYSROOT=/tmp/rd-sysroot   # from §2.2
flutter_rust_bridge_codegen \
  --rust-input ./src/flutter_ffi.rs \
  --dart-output ./flutter/lib/generated_bridge.dart \
  --c-output ./flutter/macos/Runner/bridge_generated.h \
  --llvm-path "$SYSROOT/usr/lib/llvm-18" \
  "--llvm-compiler-opts=-I$SYSROOT/usr/lib/llvm-18/lib/clang/18/include"
cp ./flutter/macos/Runner/bridge_generated.h ./flutter/ios/Runner/bridge_generated.h

git checkout -- flutter/pubspec.yaml flutter/pubspec.lock
```

**Both LLVM flags are required, and getting them wrong fails silently.** The codegen
runs `ffigen` over the generated C header, and `libclang` does not find clang's own
builtin headers on its own. Without them `stdbool.h` is missing, so `bool` never
resolves and ffigen invents a placeholder:

```dart
typedef bool = ffi.NativeFunction<ffi.Int Function(ffi.Pointer<ffi.Int>)>;
```

That shadows `dart:core`'s `bool` for the whole library. Codegen still reports success,
and the Rust side still compiles — the damage only surfaces much later as a wall of
Dart errors like *"The argument type 'bool' can't be assigned to the parameter type
`Pointer<bool>`"* and *"Type 'Int' not found"* in `generated_bridge.freezed.dart`.
Deleting the typedef by hand is not a fix: the wire signatures were mis-generated too
(184 spurious `Pointer<bool>` parameters).

Note the `=` form on `--llvm-compiler-opts`. A space-separated value beginning with
`-I` is parsed as a flag and the command aborts with a usage error.

After generating, confirm both are clean before building:

```bash
grep -c '^typedef bool = ' flutter/lib/generated_bridge.dart   # must be 0
grep -c 'Pointer<bool>'    flutter/lib/generated_bridge.dart   # must be 0
```

That produces all six artifacts (all git-ignored):

```
src/bridge_generated.rs            flutter/lib/generated_bridge.dart
src/bridge_generated.io.rs         flutter/lib/generated_bridge.freezed.dart
flutter/macos/Runner/bridge_generated.h   flutter/ios/Runner/bridge_generated.h
```

After which the Flutter configuration builds and tests normally:

```bash
cargo check --lib --features flutter,hwcodec
cargo test  --lib --features flutter,hwcodec
```

### Known flaky test

`hbb_common`'s `config::tests::test_store_load` can fail under
`cargo test --workspace` with a permissions assertion at `config.rs`. It writes a peer
config to the shared user config directory and then asserts the file mode is `0o600`;
cargo runs each crate's test binary concurrently, so another crate's tests touching the
same path race it. It passes reliably on its own, in parallel or serial:

```bash
cargo test -p hbb_common --lib                      # 101 passed
cargo test -p hbb_common --lib -- --test-threads=1  # 101 passed
```

This is upstream test isolation, not a code defect. Re-run the workspace suite before
concluding anything from it.

Note the app bundle uses a *different* Flutter version from the bridge step
(`RD_PACKAGE_FLUTTER_VERSION` = 3.24.5); only the bridge is pinned to 3.22.3.

---

## 2.6 Reference: prepared non-Docker environment (VM 115)

> **Builds run on the local machine, not on the VM.** This section is a parts list: it
> records which toolchain versions a known-good non-Docker setup uses, so the local
> environment in §2.1–2.5 can be matched against it. Compile locally; consult this only
> when something local is missing or a version needs checking.

A Debian 12 VM has this toolchain installed natively rather than through Docker. Its
root filesystem is mounted on the Proxmox host `192.168.1.23` at `/mnt/vm-115-root`,
so it can be inspected from there.

| Component | Location under `/mnt/vm-115-root` | Version |
|---|---|---|
| OS | — | Debian GNU/Linux 12 (bookworm) |
| Rust | `root/.rustup`, `root/.cargo` | stable x86_64 |
| Flutter | `root/.build/linux/environment-pkg/cache-amd64/sdk` | 3.24.5 |
| Python | same | 3.11.11 |
| nasm | same | 2.16.03 |
| Android SDK | `root/android-sdk` | build-tools, cmdline-tools, platforms |
| vcpkg (plain) | `root/vcpkg/installed/x64-linux` | aom, jpeg, opus, turbojpeg, vpx, yuv |
| vcpkg (build cache) | `root/.build/linux/environment-pkg/cache-amd64/vcpkg` | the above **plus** FFmpeg + libmfx, i.e. `hwcodec` |
| Project checkout | `root/rustdesk` | — |
| System `-dev` libs | `usr/lib/x86_64-linux-gnu` | gtk-3, pulse, dbus, gstreamer, drm, xcb-*, clang-14 |

Note the two vcpkg trees differ: only the build-cache one carries FFmpeg, so
`--features hwcodec` needs `VCPKG_ROOT` pointed at
`.build/linux/environment-pkg/cache-amd64/vcpkg`, not `root/vcpkg`.

The VM's rootfs is a complete Debian install with all the `-dev` packages present, so
if the locally staged sysroot from §2.2 is ever unavailable it can serve as a source for
the missing headers and libraries — copy what is needed to the local machine and keep
building there. Do not move the build itself onto the VM.

## 3. Linux release packages

Requires Docker and roughly 35 GB free disk.

```bash
# amd64
bash tools/build/linux-release.sh --arch amd64

# arm64 via QEMU (slow, but a single code path)
bash tools/build/linux-release.sh --arch arm64

# arm64 via host cross-compilation (much faster on an amd64 host)
bash tools/build/linux-release-arm64-cross.sh
```

Both architectures in parallel — the two scripts use isolated workspaces, bridge
workspaces, and caches, so `--clean` is safe concurrently for this pair:

```bash
bash tools/build/linux-release.sh --arch amd64 --clean > /tmp/rd-amd64.log 2>&1 &
bash tools/build/linux-release-arm64-cross.sh --clean  > /tmp/rd-arm64.log 2>&1 &
wait
```

Output lands in `dist/linux/rustdesk-<version>-<arch>.deb`. Each command only touches
its own architecture's package file.

Re-package without redoing the Rust build:

```bash
RD_ARM64_CROSS_REUSE_WORKSPACE=1 RD_ARM64_CROSS_SKIP_RUST=1 \
bash tools/build/_package_arm64_cross.sh \
  --base-snapshot .build/linux/base-cross-arm64 \
  --output dist/linux/rustdesk-1.4.6-arm64.deb
```

### Build caches

Stored under `~/.build/linux/environment-pkg` (override with `RD_ENV_CACHE_ROOT`):
Cargo/rustup artifacts, Flutter and other SDKs, the vcpkg checkout, and APT caches.
The arm64 sysroot is cached at `cache-arm64-cross/sdk/sysroot` with a
`.rustdesk-sysroot-ready` marker. `--clean` only clears the local `.build/linux`
workspace snapshots; environment caches survive. Delete the directory to force a full
rebuild.

---

## 4. Windows, macOS, Android, iOS

These go through `.github/workflows/public-free-build.yml` on free GitHub-hosted
runners. Configure once:

```bash
cp tools/github/public-build.env.example tools/github/public-build.env
${EDITOR:-vi} tools/github/public-build.env   # GITHUB_OWNER / GITHUB_REPO / GITHUB_TOKEN
```

Use a classic PAT with `repo` and `workflow` scopes — the flow creates a repository,
pushes workflow files, and dispatches a run. Then:

```bash
bash tools/github/create-and-run-public-build.sh       # all CI targets
bash tools/github/create-and-run-public-ios-build.sh   # iOS xcarchive only
```

`USE_LOCAL_BRIDGE=1` (default) pre-generates the flutter_rust_bridge files on the
local Linux host, saving one Ubuntu job. Set `USE_LOCAL_BRIDGE=0` if Docker is
unavailable locally.

See [`PUBLIC-GITHUB-BUILD.md`](PUBLIC-GITHUB-BUILD.md) for artifact names, GitHub
storage limits, and the iOS signing procedure.

---

## 5. Feature flags

| Flag | Effect |
|---|---|
| `flutter` | Flutter UI (requires generated bridge files) |
| `hwcodec` | Hardware H.264/H.265 encode and decode via FFmpeg |
| `vram` | Windows-only GPU-resident encoding path |
| `mediacodec` | Android hardware codec |
| `unix-file-copy-paste` | File copy/paste on X11; delegates to `libs/clipboard` |
| `linux-pkg-config` | Resolve media codecs via system pkg-config instead of vcpkg |
| `screencapturekit` | macOS ScreenCaptureKit audio capture |
| `cli` / `inline` / `plugin_framework` | Sciter CLI, inline resources, plugin host |

---

## 6. Dependency notes

Some manifest entries are deliberately not referenced from source:

- **`openssl` (linux/android)** — declared only to force `openssl-sys`, pulled in
  transitively by reqwest's native-tls backend, to build a *vendored* OpenSSL rather
  than using the build host's. Removing it breaks the Ubuntu 18.04 release containers
  and the Android NDK build.
- **`libxdo-sys`** — patched to a local stub (`libs/libxdo-sys-stub`) via
  `[patch.crates-io]` so the project builds on hosts without libxdo, including
  Wayland-only systems.

Dependencies removed as genuinely unused (no reference anywhere in the tree):
`mac_address` (declared by `hbb_common`, which is where it is used), `objc_id`,
`async-process`, `hound`, and the root's optional `x11-clipboard` / `x11rb` /
`percent-encoding` / `once_cell` — the last four are declared by `libs/clipboard`,
which owns the implementation; this crate only feature-gates.

---

## 7. Repository layout in an editor

Opening this folder shows more than one Git repository, which can read as several
projects sharing one window. There are legitimately **two**, plus one that should never
have been listed:

1. **`rustdesk`** — this project.
2. **`libs/hbb_common`** — a real Git submodule (`.gitmodules` →
   `github.com/rustdesk/hbb_common`). It has its own history and its own commits, and a
   change there is recorded in the superproject as a gitlink, so it genuinely needs its
   own entry in Source Control. Check `git submodule status` to see which commit is
   pinned.
3. **`.build/linux/cache/amd64/vcpkg`** — an unrelated third-party checkout left behind
   by a release build. Repository scanning does not consult `.gitignore`, so this showed
   up even though `.build/` is ignored.

`.vscode/settings.json` (tracked, via a `.gitignore` exception) excludes the build and
output directories from repository scanning, search, and the file watcher, leaving the
two real repositories. Nothing is deleted and no history is touched — the settings only
change what the editor looks at.

To see everything again, remove the `git.repositoryScanIgnoredFolders` and
`files.exclude` entries from that file.

## 8. macOS: running an unsigned local build

```bash
xattr -dr com.apple.quarantine /Applications/RustDesk.app
codesign --force --deep --sign - /Applications/RustDesk.app
codesign --verify --deep --strict --verbose=2 /Applications/RustDesk.app
spctl --assess --type execute -vv /Applications/RustDesk.app
```

Then approve it under *System Settings → Privacy & Security → Open Anyway*.

### "The app is installed but will not open, with no error"

Symptom: `RustDesk.app` sits in `/Applications`, double-clicking does nothing, and no
window or error appears. Diagnose with:

```bash
codesign --verify --deep --strict /Applications/RustDesk.app
```

If it reports **`a sealed resource is missing or invalid`** — and
`--verbose=4` adds **`file added: .../Contents/MacOS/service`** — the bundle's
signature seal is broken. `build.py` copies `target/release/service` into
`Contents/MacOS/` *after* Xcode has ad-hoc signed the app, and from macOS 15 onward a
broken seal blocks launch outright rather than warning.

The `codesign --force --deep --sign -` line above repairs it in place; the app launches
immediately afterwards, no reinstall needed. `spctl` will still say `rejected` because
an ad-hoc signature is not notarized — that is expected and does not prevent launching
once the quarantine attribute is gone.

Builds produced by `public-free-build.yml` no longer need this: both macOS jobs re-seal
the bundle before the DMG is created.
