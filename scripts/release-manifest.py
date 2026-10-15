#!/usr/bin/env python3
"""Builds the `wrapper update` assets of a release.

From the platform packages in DIR (wrapper-<tag>-<platform>.tar.gz/.zip) it
cuts out the components and writes them next to the packages:

    wrapper-<platform>[.exe]    the executable
    guest.tar.gz                qemu/ kernel + initramfs (one per release
                                unless platforms differ)
    qemu-<platform>.tar.gz      qemu/bin/
    rootfs-<platform>.tar.gz    rootfs/ without data/

plus manifest.json describing them (see crates/host/src/update.rs). Content
digests come from `wrapper update --digest` of a package that runs here, so
CI and the updater share one implementation.

    python3 scripts/release-manifest.py --tag v3.1.0 --repo owner/name release/
"""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile

GUEST_FILES = ["vmlinuz-lite-qemu", "lite-initramfs.cpio.gz"]
ROOTFS_KEEP = {"data"}


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 16), b""):
            h.update(block)
    return h.hexdigest()


def unpack(archive, dest):
    if archive.endswith(".zip"):
        with zipfile.ZipFile(archive) as z:
            z.extractall(dest)
    else:
        with tarfile.open(archive) as t:
            t.extractall(dest, filter="tar" if hasattr(tarfile, "data_filter") else None)
    (top,) = os.listdir(dest)  # packages have a single wrapper-<tag>-<platform>/ dir
    return os.path.join(dest, top)


def make_tar(out, root, names=None, exclude=()):
    """tar.gz of root's contents (or just `names`), owned by root:root."""

    def owner(info):
        info.uid = info.gid = 0
        info.uname = info.gname = "root"
        return info

    with tarfile.open(out, "w:gz") as t:
        for name in sorted(names or os.listdir(root)):
            if name not in exclude:
                t.add(os.path.join(root, name), arcname=name, filter=owner)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", required=True)
    ap.add_argument("--repo", required=True)
    ap.add_argument("--tool", help="wrapper binary for digests [default: the linux-x86_64 package's]")
    ap.add_argument("dir")
    args = ap.parse_args()

    out = os.path.abspath(args.dir)
    prefix = f"wrapper-{args.tag}-"
    packages = {}
    for name in sorted(os.listdir(out)):
        for ext in (".tar.gz", ".zip"):
            if name.startswith(prefix) and name.endswith(ext):
                packages[name[len(prefix) : -len(ext)]] = os.path.join(out, name)
    if not packages:
        sys.exit(f"no {prefix}* packages in {out}")

    work = tempfile.mkdtemp(prefix="wrapper-release-")
    try:
        trees = {plat: unpack(pkg, os.path.join(work, plat)) for plat, pkg in packages.items()}
        tool = args.tool or os.path.join(trees["linux-x86_64"], "wrapper")

        def digest(component, path):
            cmd = [tool, "update", "--digest", component, path]
            return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout.strip()

        def entry(asset, component, installed):
            path = os.path.join(out, asset)
            return {
                "asset": asset,
                "size": os.path.getsize(path),
                "sha256": sha256(path),
                "digest": digest(component, installed),
            }

        platforms = {}
        guest_assets = {}  # digest -> asset name, to share identical guests
        for plat, tree in trees.items():
            comps = {}
            exe = "wrapper.exe" if plat.startswith("windows") else "wrapper"
            bin_asset = f"wrapper-{plat}" + (".exe" if exe.endswith(".exe") else "")
            shutil.copy2(os.path.join(tree, exe), os.path.join(out, bin_asset))
            comps["bin"] = entry(bin_asset, "bin", os.path.join(tree, exe))

            qemu = os.path.join(tree, "qemu")
            if all(os.path.isfile(os.path.join(qemu, f)) for f in GUEST_FILES):
                d = digest("guest", qemu)
                if d not in guest_assets:
                    guest_assets[d] = "guest.tar.gz" if not guest_assets else f"guest-{plat}.tar.gz"
                    make_tar(os.path.join(out, guest_assets[d]), qemu, names=GUEST_FILES)
                comps["guest"] = entry(guest_assets[d], "guest", qemu)

            qbin = os.path.join(qemu, "bin")
            if os.path.isdir(qbin) and os.listdir(qbin):
                make_tar(os.path.join(out, f"qemu-{plat}.tar.gz"), qbin)
                comps["qemu"] = entry(f"qemu-{plat}.tar.gz", "qemu", qbin)

            rootfs = os.path.join(tree, "rootfs")
            if os.path.isfile(os.path.join(rootfs, "system/bin/lite")):
                make_tar(os.path.join(out, f"rootfs-{plat}.tar.gz"), rootfs, exclude=ROOTFS_KEEP)
                comps["rootfs"] = entry(f"rootfs-{plat}.tar.gz", "rootfs", rootfs)
            platforms[plat] = comps
    finally:
        shutil.rmtree(work, ignore_errors=True)

    base = args.repo if "://" in args.repo else f"https://github.com/{args.repo}/releases"
    manifest = {
        "schema": 1,
        "version": args.tag.removeprefix("v"),
        "tag": args.tag,
        "notes": f"{base}/tag/{args.tag}",
        "platforms": platforms,
    }
    with open(os.path.join(out, "manifest.json"), "w", newline="\n") as f:
        json.dump(manifest, f, indent=2, sort_keys=True)
        f.write("\n")
    for plat, comps in platforms.items():
        print(plat, ", ".join(f"{c} {v['size'] // 1024} KiB" for c, v in comps.items()))


if __name__ == "__main__":
    main()
