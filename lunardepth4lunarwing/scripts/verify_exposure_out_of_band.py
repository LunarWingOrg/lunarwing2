#!/usr/bin/env python3
"""Verify an exported LunarDepth rootfs and image config using checkout-owned inputs."""

import argparse
import hashlib
import importlib.util
import json
import stat
import sys
import tarfile
from pathlib import Path, PurePosixPath

# mt-admin runs this file with `python3 -I`, which leaves the script's own
# directory off the import path. Name it explicitly; nothing else is added.
sys.path.insert(0, str(Path(__file__).resolve().parent))

from image_config_policy import validate_config  # noqa: E402

CHECKED_CLOSURE_PREFIXES = ("app/", "opt/lunardepth")


def digest_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def digest_file(path: Path) -> str:
    return digest_bytes(path.read_bytes())


def normalized(name: str) -> str:
    path = PurePosixPath(name)
    parts = list(path.parts)
    if parts and parts[0] in ("/", "."):
        parts = parts[1:]
    return "/".join(parts)


def source_inventory(root: Path, compiled_root: Path | None = None) -> dict[str, Path]:
    files = {
        "app/entrypoint.sh": root / "entrypoint.sh",
        "app/entrypoint-split.sh": root / "entrypoint-split.sh",
        "app/entrypoint-task-setup.sh": root / "entrypoint-task-setup.sh",
        "app/health_server.py": root / "health_server.py",
        "app/agent_comm_protocol.json": root / "agent_comm_protocol.json",
        "usr/local/bin/dsh": root / "scripts/dsh-exposure-guard.sh",
    }
    for path in (root / "config").glob("*"):
        if path.is_file():
            files[f"app/config/{path.name}"] = path
    scripts = root / "scripts"
    compiled_scripts = compiled_root if compiled_root is not None else scripts
    for path in scripts.glob("*.py"):
        if not path.name.endswith("_test.py"):
            files[f"app/scripts/{path.name}"] = path
    for path in scripts.glob("*.sh"):
        files[f"app/scripts/{path.name}"] = path
    for path in scripts.glob("*.ts"):
        if not path.name.endswith("_test.ts"):
            files[f"app/scripts/{path.with_suffix('.js').name}"] = compiled_scripts / path.with_suffix(".js").name
    files["app/scripts/package.json"] = scripts / "package.json"
    return files


def check_closure_member(member: tarfile.TarInfo, name: str, closure_regular: set[str]) -> None:
    """Apply the application-closure rules to one archive member.

    A tar hard-link member is a second name for an inode of the same archive.
    `podman export` and BuildKit both write the first path of a shared inode as
    a regular member and every later path as a link to it, so a hard link
    cannot redirect a read or a write outside the carried tree the way a
    symlink can: it names another member, and its own owner, mode, and name are
    checked here like any other. It is accepted only when the target is an
    earlier regular file under the checked roots, so the closure gains no path
    whose bytes are not already present in it. Symlinks, links to anything
    outside the checked roots, links to a member that is not a regular file,
    and links that name a path not yet seen stay rejected.
    """
    if member.uid != 0 or member.gid != 0 or member.mode & 0o222:
        raise ValueError(f"writable or non-root-owned image input: {name}")
    if member.mode & 0o6000:
        raise ValueError(f"setuid/setgid image input: {name}")
    if member.issym():
        raise ValueError(f"symlink in application closure: {name} -> {member.linkname}")
    if member.islnk():
        target = normalized(member.linkname)
        if not target.startswith(CHECKED_CLOSURE_PREFIXES):
            raise ValueError(
                f"link in application closure leaves the checked roots: {name} -> {member.linkname}"
            )
        if target not in closure_regular:
            raise ValueError(
                "link in application closure is not a second name for an earlier "
                f"regular closure file: {name} -> {member.linkname}"
            )


def validate_archive(
    archive_path: Path,
    config_path: Path,
    lane_root: Path,
    compiled_root: Path | None = None,
) -> None:
    document = json.loads(config_path.read_text(encoding="utf-8"))
    if isinstance(document, list):
        if len(document) != 1 or not isinstance(document[0], dict):
            raise ValueError("image inspect must contain exactly one image")
        document = document[0]
    validate_config(document)
    inventory = source_inventory(lane_root, compiled_root)
    seen = set()
    closure_regular = set()
    with tarfile.open(archive_path, "r:*") as archive:
        for member in archive:
            name = normalized(member.name)
            if not name:
                continue
            if member.isreg():
                data = archive.extractfile(member)
                if data is None:
                    raise ValueError(f"unreadable archive member {name}")
                content = data.read()
            else:
                content = b""
            if name in inventory:
                if not member.isreg():
                    raise ValueError(f"lane input is not a regular file: {name}")
                if digest_bytes(content) != digest_file(inventory[name]):
                    raise ValueError(f"checkout/image content drift: {name}")
                seen.add(name)
            if name.startswith(CHECKED_CLOSURE_PREFIXES):
                check_closure_member(member, name, closure_regular)
                if member.isreg():
                    closure_regular.add(name)
            lowered = name.lower()
            lane_owned = name.startswith(CHECKED_CLOSURE_PREFIXES)
            if lane_owned and any(part in lowered for part in ("/.git/", "/__tests__/", "/tests/", "/.cache/", "/.npm/")):
                raise ValueError(f"build residue in image: {name}")
            if name.startswith(("root/.cache/", "root/.npm/", "home/lunardepth/.cache/", "home/lunardepth/.npm/")):
                raise ValueError(f"build residue in image: {name}")
            if lane_owned and not name.startswith("app/scripts/smoke_test.") and lowered.endswith(
                ("_test.ts", "_test.js", "_test.py", "pnpm-lock.yaml", "app/scripts/package-lock.json")
            ):
                raise ValueError(f"test/package-manager residue in image: {name}")
            if lowered.endswith(("local-auth-token", "agent_auth_token", ".lunardepth-sandbox-escape")):
                raise ValueError(f"smoke credential or state residue in image: {name}")
            if name == "tmp/lunardepth_ws_state.json" or name.startswith("tmp/dsh-spill-"):
                raise ValueError(f"smoke credential or state residue in image: {name}")
    missing = sorted(set(inventory) - seen)
    if missing:
        raise ValueError(f"missing or divergent lane inputs: {', '.join(missing)}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--lane-root", type=Path, required=True)
    parser.add_argument("--compiled-root", type=Path)
    args = parser.parse_args()
    compiled_root = args.compiled_root.resolve() if args.compiled_root is not None else None
    validate_archive(args.archive, args.config, args.lane_root.resolve(), compiled_root)


if __name__ == "__main__":
    main()
