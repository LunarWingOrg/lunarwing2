#!/usr/bin/env python3
"""Check the reviewed Pebble lock override before a locked upstream build."""

import argparse
import hashlib
import pathlib
import re
import sys
import tomllib


ALLOWED_PACKAGES = {"rustls", "rustls-webpki"}


def package_records(lock: dict) -> dict[tuple[str, str, str], dict]:
    records = {}
    packages = lock.get("package")
    if not isinstance(packages, list) or not packages:
        raise ValueError("Cargo.lock must contain package records")
    for package in packages:
        if not isinstance(package, dict):
            raise ValueError("invalid Cargo.lock package record")
        name, version, source = (
            package.get("name"), package.get("version"), package.get("source", "")
        )
        if not all(isinstance(field, str) for field in (name, version, source)):
            raise ValueError("invalid Cargo.lock package identity")
        identity = (name, version, source)
        if identity in records:
            raise ValueError(f"duplicate Cargo.lock package: {identity}")
        records[identity] = package
    return records


def check_package_delta(upstream: dict, patched: dict) -> None:
    # Compare the entire record, not textual version-line hunks. Additions,
    # removals and changes to checksums/sources/dependencies count equally.
    before = package_records(upstream)
    after = package_records(patched)
    changed = {
        identity[0]
        for identity in before.keys() | after.keys()
        if before.get(identity) != after.get(identity)
    }
    if not changed:
        raise ValueError("Cargo.lock override is a no-op — the pinned upstream lock no longer needs patching")
    extra = changed - ALLOWED_PACKAGES
    if extra:
        raise ValueError(f"Cargo.lock delta outside allowlist: {sorted(extra)}")
    before_metadata = {key: value for key, value in upstream.items() if key != "package"}
    after_metadata = {key: value for key, value in patched.items() if key != "package"}
    if before_metadata != after_metadata:
        raise ValueError("Cargo.lock metadata changed outside package allowlist")
    for name in sorted(changed):
        print(f"override: {name}")


def verify(upstream: pathlib.Path, patched: pathlib.Path, expected_digest: str) -> None:
    if not re.fullmatch(r"[0-9a-f]{64}", expected_digest):
        raise ValueError("expected lock SHA256 must be 64 lowercase hex characters")
    patched_bytes = patched.read_bytes()
    if hashlib.sha256(patched_bytes).hexdigest() != expected_digest:
        raise ValueError("patched Pebble Cargo.lock SHA256 does not match the reviewed pin")
    check_package_delta(
        tomllib.loads(upstream.read_text(encoding="utf-8")),
        tomllib.loads(patched_bytes.decode("utf-8")),
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("upstream", type=pathlib.Path)
    parser.add_argument("patched", type=pathlib.Path)
    parser.add_argument("expected_digest")
    args = parser.parse_args()
    try:
        verify(args.upstream, args.patched, args.expected_digest)
    except (OSError, UnicodeError, ValueError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    print("Verified Pebble lock SHA256 and rustls/rustls-webpki-only package delta.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
