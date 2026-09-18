#!/usr/bin/env python3
"""
Generate an OpenWrt .ipk (opkg) package without relying on host `ar`.

Modern GNU ar appends '/' to member names and uses "!" extended format
entries that older opkg/libarchive versions choke on ("Malformed package
file").  This script writes the ar archive by hand to guarantee the
classic format opkg expects.

Usage:
    python3 scripts/mkpkg.py --control <dir> --data <tar.gz> --out <file.ipk>

Both tarballs must already exist.  This script only wraps them into the
ipk ar envelope (debian-binary + control.tar.gz + data.tar.gz).

Alternatively, to build everything from scratch:
    python3 scripts/mkpkg.py --from-scratch \\
        --pkg-name campus-auth-guardian \\
        --pkg-version 0.2.0 --pkg-release 1 --pkg-arch aarch64_cortex-a53 \\
        --root <repo-root> --out <file.ipk>
"""

from __future__ import annotations

import argparse
import io
import os
import struct
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

# ---------- constants -------------------------------------------------------

IPK_MAGIC = b"!<arch>\n"
# Each ar member header is exactly 60 bytes:
#   name[16] mtime[12] owner[6] group[6] mode[8] size[10] "`\n"
HDR_FMT = "<16s12s6s6s8s10s2s"
HDR_SIZE = 60


# ---------- ar writer -------------------------------------------------------


def _ar_field(raw: bytes, width: int) -> bytes:
    """Pad or truncate a field to exactly `width` bytes using SPACES (not NUL).

    Python's struct.pack('Ns', ...) pads with \\x00, but ar headers must be
    space-padded.  libarchive/opkg chokes on NUL bytes in header fields.
    """
    return raw[:width].ljust(width, b" ")


def _ar_member(name: str, data: bytes) -> bytes:
    """Build a single ar member (header + data + padding)."""
    if len(name) > 16:
        raise ValueError(f"ar member name too long ({len(name)} > 16): {name}")
    # size field = original data length (before any padding)
    # ar spec: each member's data must be 2-byte aligned.
    # Padding byte goes AFTER the data, is NOT counted in the size field.
    header = (
        _ar_field(name.encode("ascii"), 16)       # name
        + _ar_field(b"0", 12)                      # mtime
        + _ar_field(b"0", 6)                       # uid
        + _ar_field(b"0", 6)                       # gid
        + _ar_field(b"100644", 8)                  # mode
        + _ar_field(str(len(data)).encode(), 10)   # size (original, unpadded)
        + b"`\n"                                    # terminator
    )
    assert len(header) == HDR_SIZE, f"header is {len(header)} bytes, expected {HDR_SIZE}"
    padding = b"\n" if len(data) % 2 else b""
    return header + data + padding


def write_ipk(
    debian_binary: bytes,
    control_tar: bytes,
    data_tar: bytes,
    out_path: str | Path,
) -> None:
    """Write a three-member ipk ar archive."""
    with open(out_path, "wb") as f:
        f.write(IPK_MAGIC)
        f.write(_ar_member("debian-binary", debian_binary))
        f.write(_ar_member("control.tar.gz", control_tar))
        f.write(_ar_member("data.tar.gz", data_tar))


# ---------- from-scratch builder -------------------------------------------


def build_from_scratch(
    root: Path,
    pkg_name: str,
    pkg_version: str,
    pkg_release: str,
    pkg_arch: str,
    pkg_maintainer: str,
    out_path: Path,
) -> None:
    """Full build: prepare files, create tarballs, assemble ipk."""

    # Permissions are set in the TAR archive directly, not via filesystem chmod,
    # so the staging directory can be anywhere (including Windows/9p mounts).
    with tempfile.TemporaryDirectory(prefix="cag-ipk-") as stage:
        stage = Path(stage)
        data_dir = stage / "data"
        ctrl_dir = stage / "control"

        # --- prepare data tree ---
        data_dir.mkdir()
        bin_dir = data_dir / "usr" / "bin"
        bin_dir.mkdir(parents=True)

        bin_src = root / "target" / "aarch64-unknown-linux-musl" / "release" / pkg_name
        if not bin_src.exists():
            print(f"错误：找不到二进制 {bin_src}", file=sys.stderr)
            print("先跑一次 cargo build --release --target aarch64-unknown-linux-musl", file=sys.stderr)
            sys.exit(1)

        # Verify it's actually aarch64
        try:
            result = subprocess.run(
                ["file", str(bin_src)], capture_output=True, text=True, timeout=5
            )
            if "aarch64" not in result.stdout.lower() and "arm" not in result.stdout.lower():
                # Not fatal if `file` isn't available; continue
                pass
        except FileNotFoundError:
            pass  # `file` not available on Windows; skip check

        _copy_file(bin_src, bin_dir / pkg_name)

        # Copy openwrt/files/ tree
        files_root = root / "openwrt" / "files"
        if files_root.exists():
            _copy_tree(files_root, data_dir)

        # NOTE: Permissions are set in the TAR archive (_make_tarball), not on
        # disk, because on Windows/WSL 9p mounts chmod is a no-op.

        # --- prepare control tree ---
        ctrl_dir.mkdir()

        size_kb = _dir_size_kb(data_dir)

        control_src = root / "openwrt" / "control"
        control_text = control_src.read_text(encoding="utf-8")
        control_text = control_text.replace("@VERSION@", f"{pkg_version}-{pkg_release}")
        control_text = control_text.replace("@ARCH@", pkg_arch)
        control_text = control_text.replace("@SIZE@", str(size_kb))
        control_text = control_text.replace("@MAINTAINER@", pkg_maintainer)
        (ctrl_dir / "control").write_text(control_text, encoding="utf-8")

        for name in ("postinst", "prerm", "conffiles"):
            src = root / "openwrt" / name
            if src.exists():
                _copy_file(src, ctrl_dir / name)

        # --- create tarballs with ustar format (best compatibility) ---
        debian_binary = b"2.0\n"

        ctrl_tar = _make_tarball(ctrl_dir, stage)
        data_tar = _make_tarball(data_dir, stage)

        # --- assemble ipk ---
        out_path.parent.mkdir(parents=True, exist_ok=True)
        write_ipk(debian_binary, ctrl_tar, data_tar, out_path)

        print(f"\n>>> 完成: {out_path}")
        try:
            sz = out_path.stat().st_size
            print(f"    大小: {sz:,} bytes ({sz // 1024} KB)")
        except OSError:
            pass


def _copy_file(src: Path, dst: Path) -> None:
    """Copy file content only; do NOT preserve source permissions (9p mount)."""
    import shutil
    shutil.copyfile(str(src), str(dst))
    dst.chmod(0o644)  # default; caller overrides for executables


def _copy_tree(src: Path, dst: Path) -> None:
    """Recursively copy src into dst. Only copies content, not permissions."""
    import shutil
    for item in sorted(src.rglob("*")):
        rel = item.relative_to(src)
        target = dst / rel
        if item.is_dir():
            target.mkdir(parents=True, exist_ok=True)
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(str(item), str(target))
            target.chmod(0o644)  # default; _set_perms overrides later


def _set_perms(data_dir: Path, pkg_name: str) -> None:
    """Set correct permissions for all files and directories."""
    # All directories: 0755
    for d in [p for p in data_dir.rglob("*") if p.is_dir()]:
        d.chmod(0o755)
    data_dir.chmod(0o755)

    # All files: 0644 by default
    for f in [p for p in data_dir.rglob("*") if p.is_file()]:
        f.chmod(0o644)

    # Executables: 0755
    executables = [
        f"usr/bin/{pkg_name}",
        f"etc/init.d/{pkg_name}",
        f"etc/hotplug.d/iface/90-{pkg_name}",
        f"usr/libexec/rpcd/{pkg_name}",
    ]
    for rel in executables:
        p = data_dir / rel
        if p.exists():
            p.chmod(0o755)

    # Config file: 0600 (has passwords)
    cfg = data_dir / "etc" / "config" / pkg_name
    if cfg.exists():
        cfg.chmod(0o600)


def _verify_perms(data_dir: Path, pkg_name: str) -> None:
    """Verify critical file permissions."""
    ok = True
    for rel in [
        f"usr/bin/{pkg_name}",
        f"etc/init.d/{pkg_name}",
        f"etc/hotplug.d/iface/90-{pkg_name}",
        f"usr/libexec/rpcd/{pkg_name}",
    ]:
        p = data_dir / rel
        if p.exists():
            mode = oct(p.stat().st_mode)[-3:]
            if mode != "755":
                print(f"权限错误：{rel} 是 {mode}，应为 755", file=sys.stderr)
                ok = False

    cfg = data_dir / "etc" / "config" / pkg_name
    if cfg.exists():
        mode = oct(cfg.stat().st_mode)[-3:]
        if mode != "600":
            print(f"权限错误：etc/config/{pkg_name} 是 {mode}，应为 600", file=sys.stderr)
            ok = False

    if not ok:
        sys.exit(1)


def _make_tarball(root_dir: Path, stage: Path) -> bytes:
    """Create a .tar.gz with ustar format from root_dir. Returns bytes.

    IMPORTANT: We override permissions in TarInfo directly because on Windows/WSL
    the filesystem permissions are meaningless (chmod is a no-op on 9p mounts).
    The permissions in the tar archive are what actually get installed on the router.
    """
    tar_path = stage / (root_dir.name + ".tar.gz")

    # Build a permission map: arcname → mode
    # This must match _set_perms() logic — we set perms in the TAR, not on disk.
    perm_map: dict[str, int] = {}

    # Detect package name from the presence of usr/bin/<name>
    pkg_name = None
    bin_dir = root_dir / "usr" / "bin"
    if bin_dir.exists():
        bins = [f for f in bin_dir.iterdir() if f.is_file()]
        if bins:
            pkg_name = bins[0].name

    # Directories: 0755
    for d in root_dir.rglob("*"):
        if d.is_dir():
            rel = "./" + str(d.relative_to(root_dir)).replace("\\", "/")
            perm_map[rel] = 0o755

    # Default files: 0644
    for f in root_dir.rglob("*"):
        if f.is_file():
            rel = "./" + str(f.relative_to(root_dir)).replace("\\", "/")
            perm_map[rel] = 0o644

    # Executables: 0755
    if pkg_name:
        for rel in [
            f"./usr/bin/{pkg_name}",
            f"./etc/init.d/{pkg_name}",
            f"./etc/hotplug.d/iface/90-{pkg_name}",
            f"./usr/libexec/rpcd/{pkg_name}",
        ]:
            if rel in perm_map:
                perm_map[rel] = 0o755

        # Config file: 0600 (contains passwords)
        cfg_key = f"./etc/config/{pkg_name}"
        if cfg_key in perm_map:
            perm_map[cfg_key] = 0o600

    # Control tar: postinst/prerm must be executable (opkg runs them)
    for ctrl_script in ("./postinst", "./prerm"):
        if ctrl_script in perm_map:
            perm_map[ctrl_script] = 0o755

    with tarfile.open(str(tar_path), "w:gz", format=tarfile.USTAR_FORMAT) as tf:
        # Add all items with "./" prefix (OpenWrt convention)
        for item in sorted(root_dir.rglob("*")):
            arcname = "./" + str(item.relative_to(root_dir)).replace("\\", "/")
            info = tf.gettarinfo(str(item), arcname=arcname)
            # Override mode from our map (ignore filesystem permissions)
            if arcname in perm_map:
                info.mode = perm_map[arcname]
            info.uid = 0
            info.gid = 0
            info.uname = ""
            info.gname = ""
            if info.isfile():
                with open(str(item), "rb") as fobj:
                    tf.addfile(info, fobj)
            else:
                tf.addfile(info)
    return tar_path.read_bytes()


def _dir_size_kb(path: Path) -> int:
    """Approximate directory size in KB (matches du -sk)."""
    total = 0
    for f in path.rglob("*"):
        if f.is_file():
            total += f.stat().st_size
    return max(1, total // 1024)


# ---------- simple mode: just wrap existing tarballs -----------------------


def build_from_existing(
    control_tar_path: str,
    data_tar_path: str,
    out_path: str | Path,
) -> None:
    """Wrap existing control.tar.gz and data.tar.gz into an ipk."""
    debian_binary = b"2.0\n"
    control_tar = Path(control_tar_path).read_bytes()
    data_tar = Path(data_tar_path).read_bytes()

    out_path = Path(out_path)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    write_ipk(debian_binary, control_tar, data_tar, out_path)

    sz = out_path.stat().st_size
    print(f"ipk written: {out_path}  ({sz:,} bytes)")


# ---------- CLI -------------------------------------------------------------


def main() -> None:
    p = argparse.ArgumentParser(description="Build an OpenWrt .ipk package")
    p.add_argument("--control", help="Path to existing control.tar.gz")
    p.add_argument("--data", help="Path to existing data.tar.gz")
    p.add_argument("--out", required=True, help="Output .ipk path")

    # From-scratch mode
    p.add_argument("--from-scratch", action="store_true", help="Build everything from scratch")
    p.add_argument("--root", help="Repo root (for --from-scratch)")
    p.add_argument("--pkg-name", default="campus-auth-guardian")
    p.add_argument("--pkg-version", default="0.2.0")
    p.add_argument("--pkg-release", default="1")
    p.add_argument("--pkg-arch", default="aarch64_cortex-a53")
    p.add_argument("--pkg-maintainer", default="Your Name <you@example.com>")

    args = p.parse_args()

    if args.from_scratch:
        if not args.root:
            p.error("--root is required with --from-scratch")
        # 自动从 Cargo.toml 读版本号
        if args.pkg_version == "0.2.0":  # 仅在未手动指定时自动读取
            cargo_toml = Path(args.root) / "Cargo.toml"
            if cargo_toml.exists():
                import re
                m = re.search(r'^version\s*=\s*"([^"]+)"', cargo_toml.read_text(), re.M)
                if m:
                    args.pkg_version = m.group(1)
        build_from_scratch(
            root=Path(args.root),
            pkg_name=args.pkg_name,
            pkg_version=args.pkg_version,
            pkg_release=args.pkg_release,
            pkg_arch=args.pkg_arch,
            pkg_maintainer=args.pkg_maintainer,
            out_path=Path(args.out),
        )
    else:
        if not args.control or not args.data:
            p.error("--control and --data are required (or use --from-scratch)")
        build_from_existing(args.control, args.data, args.out)


if __name__ == "__main__":
    main()
