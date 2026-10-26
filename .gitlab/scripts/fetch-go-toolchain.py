#!/usr/bin/env python3
"""Fetch a pinned Go toolchain for CI jobs that also need Python."""

from __future__ import annotations

import argparse
import hashlib
import io
import pathlib
import platform
import sys
import tarfile
import urllib.error
import urllib.request

GO_VERSION = "1.24.3"
ARCHIVES = {
    "x86_64": ("amd64", "3333f6ea53afa971e9078895eaa4ac7204a8c6b5c68c10e6bc9a33e8e391bdd8"),
    "aarch64": ("arm64", "a463cb59382bd7ae7d8f4c68846e73c4d589f223c589ac76871b66811ded7836"),
}
MAX_ARCHIVE_BYTES = 256 * 1024 * 1024


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", nargs="?", type=pathlib.Path, default="/tmp/lunarwing-go")
    args = parser.parse_args()
    archive = ARCHIVES.get(platform.machine())
    if archive is None:
        print(f"ERROR: no pinned Go archive for machine {platform.machine()!r}", file=sys.stderr)
        return 1
    architecture, expected_digest = archive
    url = f"https://go.dev/dl/go{GO_VERSION}.linux-{architecture}.tar.gz"
    try:
        with urllib.request.urlopen(url, timeout=120) as response:
            payload = response.read(MAX_ARCHIVE_BYTES + 1)
        if len(payload) > MAX_ARCHIVE_BYTES:
            raise ValueError("Go archive exceeds the 256 MiB limit")
        actual_digest = hashlib.sha256(payload).hexdigest()
        if actual_digest != expected_digest:
            raise ValueError(f"Go archive digest {actual_digest} does not match {expected_digest}")
        with tarfile.open(fileobj=io.BytesIO(payload), mode="r:gz") as bundle:
            bundle.extractall(args.destination, filter="data")
    except (OSError, ValueError, tarfile.TarError, urllib.error.URLError) as error:
        print(f"ERROR: cannot install the pinned Go toolchain: {error}", file=sys.stderr)
        return 1
    print(args.destination / "go" / "bin")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
