# Linux Release Build

Linux `.deb` packaging in depth. For the full target matrix, local verification
without root, and the Windows/macOS/Android/iOS flows, see [`BUILD.md`](BUILD.md).

## What it builds

- `linux/amd64` Debian package
- `linux/arm64` Debian package

Output goes to `dist/linux/rustdesk-<version>-<arch>.deb`.

## Host requirements

- Linux amd64 host
- Docker
- Network access to GitHub, `ghcr.io`, `storage.googleapis.com`, `pub.dev`, and the
  crate registries
- Roughly 35 GB free disk

`--arch arm64` builds through Docker + binfmt/QEMU. The separate
`linux-release-arm64-cross.sh` path instead cross-compiles the heavy Rust/C/C++ steps
on the amd64 host, which is considerably faster.

## Commands

```bash
# amd64
bash tools/build/linux-release.sh --arch amd64

# arm64 through QEMU
bash tools/build/linux-release.sh --arch arm64

# arm64 through host cross-compilation (preferred on an amd64 host)
bash tools/build/linux-release-arm64-cross.sh

# both architectures at once
bash tools/build/linux-release.sh --arch all --clean
```

### Parallel build

```bash
bash tools/build/linux-release.sh --arch amd64 --clean > /tmp/rustdesk-amd64.log 2>&1 &
bash tools/build/linux-release-arm64-cross.sh --clean  > /tmp/rustdesk-arm64.log 2>&1 &
wait
```

The two commands use isolated build workspaces, isolated bridge workspaces, and
isolated bridge caches, so `--clean` is safe in parallel for this `amd64 + arm64-cross`
pair. They share the `dist/linux/` output directory but each only removes or overwrites
its own architecture's package file.

### Re-package without rebuilding Rust

```bash
RD_ARM64_CROSS_REUSE_WORKSPACE=1 RD_ARM64_CROSS_SKIP_RUST=1 \
bash tools/build/_package_arm64_cross.sh \
  --base-snapshot .build/linux/base-cross-arm64 \
  --output dist/linux/rustdesk-1.4.6-arm64.deb
```

## Build environment cache

Cached under `~/.build/linux/environment-pkg` (override with `RD_ENV_CACHE_ROOT`):

- Rust/Cargo artifacts (`cache-amd64/cargo`, `cache-amd64/rustup`)
- Flutter and other SDKs (`cache-amd64/sdk`)
- vcpkg checkout (`cache-amd64/vcpkg`)
- APT caches (`cache-*/apt-cache`, `cache-*/apt-lists`)

The arm64 sysroot is cached at `cache-arm64-cross/sdk/sysroot` with the version marker
`.rustdesk-sysroot-ready`.

Cached toolchain and dependency outputs are preferred; anything missing is downloaded
on demand and persisted for the next run. `--clean` removes only the local
`.build/linux` workspace snapshots — environment caches are kept unless deleted by
hand. Delete `~/.build/linux/environment-pkg` to force a full environment rebuild.

## How the flow works

- The script snapshots the current worktree into `.build/linux/` and mutates only
  files inside that snapshot.
- Generated bridge files are produced in a temporary bridge snapshot and copied into
  the shared base snapshot before the package builds start.
- The flow mirrors the upstream GitHub Actions Linux Flutter pipeline and the upstream
  bridge generation pipeline (`bridge.yml`, `flutter-build.yml`).
- Builder images come from GitHub Container Registry:
  - `ghcr.io/rustdesk/rustdesk/run-on-arch-rustdesk-rustdesk-full-flutter-ci-x86-64-ubuntu18-04:latest`
  - `ghcr.io/rustdesk/rustdesk/run-on-arch-rustdesk-rustdesk-full-flutter-ci-aarch64-ubuntu18-04:latest`
