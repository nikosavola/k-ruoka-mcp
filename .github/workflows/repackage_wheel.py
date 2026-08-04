#!/usr/bin/env python3
"""Repackage a wheel's binary as a standalone archive for `cargo binstall`.

The wheel built alongside this already holds a release binary for the platform, so the
archive is lifted out of it rather than cross-compiled a second time. Same bytes as PyPI
ships, including the manylinux2014 glibc floor on Linux.

The binary sits at the archive *root*, which is what `[package.metadata.binstall]` in
Cargo.toml promises cargo-binstall; the default it would otherwise assume is a
`<name>-<target>-v<version>/` directory. Naming and layout are therefore a contract
between this script and that table -- change one and the other has to follow.

Usage: python3 .github/workflows/repackage_wheel.py <wheel-dir> <target-triple> <out-dir>
"""

from __future__ import annotations

import gzip
import io
import sys
import tarfile
import zipfile
from pathlib import Path

BINARY = "k-ruoka-mcp"

if len(sys.argv) != 4:
    raise SystemExit(f"usage: {sys.argv[0]} <wheel-dir> <target-triple> <out-dir>")

wheel_dir, triple, out_dir = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])

wheels = sorted(wheel_dir.glob("*.whl"))
if len(wheels) != 1:
    raise SystemExit(f"expected exactly one wheel in {wheel_dir}, found {[w.name for w in wheels]}")
wheel = wheels[0]

windows = "windows" in triple
member_name = f"{BINARY}.exe" if windows else BINARY

with zipfile.ZipFile(wheel) as zf:
    # maturin's `bin` bindings put the executable in `<name>-<version>.data/scripts/`. The
    # version is in that path, so match on the suffix rather than reconstructing it.
    found = [n for n in zf.namelist() if n.endswith(f".data/scripts/{member_name}")]
    if len(found) != 1:
        raise SystemExit(f"expected one {member_name} in {wheel.name}, found {found}")
    payload = zf.read(found[0])

out_dir.mkdir(parents=True, exist_ok=True)

# Timestamps are pinned on both paths so that re-running this over one wheel produces a
# byte-identical archive, and `SHA256SUMS` can therefore be reproduced rather than just
# trusted. Neither library does that by default: `zipfile` dates an entry with the wall
# clock, and `tarfile.open(..., "w:gz")` stamps the gzip header with it even when the tar
# member's own mtime is 0. ZIP_EPOCH is the earliest a zip can express, and what maturin
# writes into the wheel this payload came out of.
ZIP_EPOCH = (1980, 1, 1, 0, 0, 0)

if windows:
    archive = out_dir / f"{BINARY}-{triple}.zip"
    with zipfile.ZipFile(archive, "w") as out:
        # A ZipInfo carries its own compress_type, defaulting to ZIP_STORED, and it wins
        # over whatever the ZipFile was opened with -- so the compression has to be named
        # here or the archive silently comes out three times the size.
        info = zipfile.ZipInfo(member_name, date_time=ZIP_EPOCH)
        info.compress_type = zipfile.ZIP_DEFLATED
        # 0 is MS-DOS, which is what this defaults to on the Windows runner that builds the
        # release asset. Pinned rather than defaulted because the default is 3 (Unix)
        # everywhere else, and that byte lands in the central directory: without this,
        # re-running on Linux to check a published checksum would not reproduce it.
        info.create_system = 0
        out.writestr(info, payload)
else:
    archive = out_dir / f"{BINARY}-{triple}.tar.gz"
    with (
        gzip.GzipFile(archive, "wb", mtime=0) as gz,
        tarfile.open(fileobj=gz, mode="w") as out,
    ):
        info = tarfile.TarInfo(member_name)
        info.size = len(payload)
        # A TarInfo defaults to 0o644, and a binary nobody can run is the one failure mode
        # that would survive every other check here.
        info.mode = 0o755
        out.addfile(info, io.BytesIO(payload))

# Read it back: the archive is the deliverable, and "wrote some bytes" is not evidence that
# it unpacks to one runnable executable at the root.
if windows:
    with zipfile.ZipFile(archive) as check:
        infos = check.infolist()
        entries, modes = [i.filename for i in infos], []
        # Not paranoia: a stored entry is how this went out at three times the size once.
        if infos and infos[0].compress_type != zipfile.ZIP_DEFLATED:
            raise SystemExit(f"{archive.name} is not compressed (method {infos[0].compress_type})")
else:
    with tarfile.open(archive) as check:
        members = check.getmembers()
        entries, modes = [m.name for m in members], [m.mode for m in members]

if entries != [member_name]:
    raise SystemExit(f"{archive.name} should hold exactly [{member_name!r}], holds {entries}")
# Empty for the zip, deliberately: a zip's permission bits mean nothing on Windows.
if modes and not modes[0] & 0o111:
    raise SystemExit(f"{member_name} in {archive.name} is not executable (mode {modes[0]:o})")

print(
    f"{archive.name}: {archive.stat().st_size:,} bytes, {len(payload):,} unpacked, from {wheel.name}"
)
