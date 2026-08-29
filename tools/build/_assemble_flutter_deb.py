#!/usr/bin/env python3

from __future__ import annotations

import argparse
import hashlib
import shutil
import subprocess
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Assemble a Flutter desktop Debian package.")
    parser.add_argument("--bundle-dir", required=True)
    parser.add_argument("--arch", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--output", required=True)
    return parser.parse_args()


def write_control_file(control_path: Path, version: str, arch: str) -> None:
    extra_depends = ", libatomic1" if arch == "armhf" else ""
    control_path.write_text(
        f"""Package: rustdesk
Section: net
Priority: optional
Version: {version}
Architecture: {arch}
Maintainer: rustdesk <info@rustdesk.com>
Homepage: https://rustdesk.com
Depends: libgtk-3-0, libxcb-randr0, libxdo3 | libxdo4, libxfixes3, libxcb-shape0, libxcb-xfixes0, libasound2, libsystemd0, curl, libva2, libva-drm2, libva-x11-2, libgstreamer-plugins-base1.0-0, libpam0g, gstreamer1.0-pipewire{extra_depends}
Recommends: libayatana-appindicator3-1
Description: A remote control software.

""",
        encoding="utf-8",
    )


def copy_tree_contents(src: Path, dst: Path) -> None:
    for item in src.iterdir():
        target = dst / item.name
        if item.is_dir():
            shutil.copytree(item, target, dirs_exist_ok=True)
        else:
            shutil.copy2(item, target)


def write_md5sums(tmpdeb: Path) -> None:
    md5sums_path = tmpdeb / "DEBIAN" / "md5sums"
    with md5sums_path.open("w", encoding="utf-8") as handle:
        files = sorted(
            path
            for path in tmpdeb.rglob("*")
            if path.is_file() and "DEBIAN" not in path.parts
        )
        for path in files:
            digest = hashlib.md5(path.read_bytes()).hexdigest()
            relpath = path.relative_to(tmpdeb).as_posix()
            handle.write(f"{digest}  /{relpath}\n")


def main() -> None:
    args = parse_args()

    workspace = Path.cwd()
    bundle_dir = Path(args.bundle_dir)
    if not bundle_dir.is_absolute():
      bundle_dir = workspace / bundle_dir
    bundle_dir = bundle_dir.resolve()
    if not bundle_dir.is_dir():
        raise SystemExit(f"Bundle directory not found: {bundle_dir}")

    output_path = Path(args.output)
    if not output_path.is_absolute():
        output_path = workspace / output_path
    output_path = output_path.resolve()
    output_path.parent.mkdir(parents=True, exist_ok=True)

    flutter_dir = workspace / "flutter"
    tmpdeb = flutter_dir / "tmpdeb"
    if tmpdeb.exists():
        shutil.rmtree(tmpdeb)

    (tmpdeb / "usr/bin").mkdir(parents=True)
    (tmpdeb / "usr/share/rustdesk").mkdir(parents=True)
    (tmpdeb / "usr/share/rustdesk/files/systemd").mkdir(parents=True)
    (tmpdeb / "usr/share/icons/hicolor/256x256/apps").mkdir(parents=True)
    (tmpdeb / "usr/share/icons/hicolor/scalable/apps").mkdir(parents=True)
    (tmpdeb / "usr/share/applications").mkdir(parents=True)
    (tmpdeb / "usr/share/polkit-1/actions").mkdir(parents=True)
    (tmpdeb / "etc/rustdesk").mkdir(parents=True)
    (tmpdeb / "etc/pam.d").mkdir(parents=True)
    (tmpdeb / "DEBIAN").mkdir(parents=True)

    copy_tree_contents(bundle_dir, tmpdeb / "usr/share/rustdesk")

    shutil.copy2(workspace / "res/rustdesk.service", tmpdeb / "usr/share/rustdesk/files/systemd/rustdesk.service")
    shutil.copy2(workspace / "res/128x128@2x.png", tmpdeb / "usr/share/icons/hicolor/256x256/apps/rustdesk.png")
    shutil.copy2(workspace / "res/scalable.svg", tmpdeb / "usr/share/icons/hicolor/scalable/apps/rustdesk.svg")
    shutil.copy2(workspace / "res/rustdesk.desktop", tmpdeb / "usr/share/applications/rustdesk.desktop")
    shutil.copy2(workspace / "res/rustdesk-link.desktop", tmpdeb / "usr/share/applications/rustdesk-link.desktop")
    shutil.copy2(workspace / "res/startwm.sh", tmpdeb / "etc/rustdesk/startwm.sh")
    shutil.copy2(workspace / "res/xorg.conf", tmpdeb / "etc/rustdesk/xorg.conf")
    shutil.copy2(workspace / "res/pam.d/rustdesk.debian", tmpdeb / "etc/pam.d/rustdesk")

    polkit_script = tmpdeb / "usr/share/rustdesk/files/polkit"
    polkit_script.write_text("#!/bin/sh\n", encoding="utf-8")
    polkit_script.chmod(0o755)

    debian_dir = workspace / "res/DEBIAN"
    for item in debian_dir.iterdir():
        if item.name == "control":
            continue
        target = tmpdeb / "DEBIAN" / item.name
        if item.is_dir():
            shutil.copytree(item, target, dirs_exist_ok=True)
        else:
            shutil.copy2(item, target)
    write_control_file(tmpdeb / "DEBIAN/control", args.version, args.arch)
    write_md5sums(tmpdeb)

    subprocess.run(["dpkg-deb", "-b", str(tmpdeb), str(output_path)], check=True)
    shutil.rmtree(tmpdeb)


if __name__ == "__main__":
    main()
