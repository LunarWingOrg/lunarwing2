#!/usr/bin/env python3
"""Verify an opencode2 candidate outside its image and Dockerfile (WI#425)."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import pathlib
import posixpath
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
LANE = REPO_ROOT / "opencode2.4lunarwing"
ASSERT = LANE / "scripts" / "exposure_policy_assert.py"
EXPORT_ARCHIVE_NAME = "opencode2-rootfs.tar"
BOOT_FILES = ("entrypoint.sh", "health_server.py", "agent_comm_protocol.json")
SOURCE_PREFIXES = ("app/scripts", "app/config")
INSTALLED_FILES = ("usr/local/bin/opencode", "usr/local/lib/opencode2/opencode")
CHECKOUT_ENV_KEYS = (
    "OPENCODE_DISABLE_AUTOUPDATE", "LUNARWING_REAL_OPENCODE", "OPENCODE_BIN",
)

sys.dont_write_bytecode = True
sys.path.insert(0, str(LANE / "scripts"))
from image_config_policy import config_failures  # noqa: E402


def die(message: str) -> None:
    print(f"ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


REGISTRY_RETRY_DELAYS = (2, 4, 8)
TRANSIENT_HTTP_STATUSES = frozenset((429, 500, 502, 503, 504))


def _registry_json(request: urllib.request.Request, *, timeout: int, description: str) -> dict:
    attempt = 0
    while True:
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            status = error.code
            error.close()
            if status not in TRANSIENT_HTTP_STATUSES or attempt == len(REGISTRY_RETRY_DELAYS):
                die(f"{description}: HTTP Error {status}: {error.reason}")
            delay = REGISTRY_RETRY_DELAYS[attempt]
            attempt += 1
            print(
                f"WARNING: {description}: HTTP {status}; retrying in {delay} seconds",
                file=sys.stderr,
            )
            time.sleep(delay)
        except (OSError, ValueError) as error:
            die(f"{description}: {error}")


def registry_token(repo: str) -> str:
    host = os.environ.get("CI_SERVER_HOST", "")
    user = os.environ.get("CI_REGISTRY_USER", "")
    password = os.environ.get("CI_REGISTRY_PASSWORD", "")
    if not (host and user and password and repo):
        die("CI_SERVER_HOST/CI_REGISTRY_USER/CI_REGISTRY_PASSWORD (or the image ref) "
            "are missing — the image-config half cannot fetch the config")
    auth = base64.b64encode(f"{user}:{password}".encode()).decode()
    url = (f"https://{host}/jwt/auth?service=container_registry&"
           f"scope=repository:{repo}:pull")
    request = urllib.request.Request(url, headers={"Authorization": f"Basic {auth}"})
    payload = _registry_json(
        request, timeout=60, description=f"cannot obtain a registry pull token for {repo}",
    )
    token = payload.get("token")
    if not token:
        die(f"registry auth response for {repo} carried no token")
    return token


def image_config(repo: str, digest: str, token: str) -> dict:
    registry = os.environ.get("CI_REGISTRY", "")
    if not registry:
        die("CI_REGISTRY is missing")
    url = f"https://{registry}/v2/{repo}/blobs/{digest}"
    request = urllib.request.Request(url, headers={"Authorization": f"Bearer {token}"})
    return _registry_json(
        request, timeout=120, description=f"cannot fetch image config {digest} from {repo}",
    )


def check_config_half(config: dict) -> None:
    failures = config_failures(config)
    for failure in failures:
        print(f"ERROR: {failure} (WI#425)", file=sys.stderr)
    if failures:
        raise SystemExit(1)
    print("out-of-band image-config verify: full Env and runtime config clean")


def _environment(config: dict) -> dict[str, str]:
    section = config.get("config", config)
    return dict(entry.split("=", 1) for entry in section["Env"])


def _member_name(name: str) -> str:
    normalized = posixpath.normpath(name)
    if normalized in ("", "."):
        return ""
    if normalized == ".." or normalized.startswith(("../", "/")):
        die(f"tar export carries unsafe member name {name!r}")
    if any(part.startswith(".wh.") for part in pathlib.PurePosixPath(normalized).parts):
        die(f"tar export carries an unapplied whiteout {name!r}")
    return normalized


def _members(archive: tarfile.TarFile) -> dict[str, tarfile.TarInfo]:
    members: dict[str, tarfile.TarInfo] = {}
    for member in archive:
        name = _member_name(member.name)
        if name in members:
            die(f"tar export carries duplicate member {name!r}")
        members[name] = member
    return members


def _kind(member: tarfile.TarInfo) -> int:
    if member.isdir():
        return stat.S_IFDIR
    if member.isreg():
        return stat.S_IFREG
    if member.issym():
        return stat.S_IFLNK
    return 0


def _has_capabilities(member: tarfile.TarInfo) -> bool:
    return any(
        key.lower() == "security.capability"
        or key.lower().endswith(".security.capability")
        for key in member.pax_headers
    )


def _check_root_member(member: tarfile.TarInfo, name: str, *, executable: bool = False) -> None:
    if not (member.isdir() or member.isreg()):
        die(f"tar export protected path {name} must be a regular file or directory")
    if member.uid != 0 or member.gid != 0:
        die(f"tar export protected path {name} must be owned by root:root")
    mode = member.mode & 0o7777
    if mode & 0o6022:
        die(f"tar export protected path {name} has privilege or write bits {mode:o}")
    if executable and (not member.isreg() or mode & 0o111 != 0o111):
        die(f"tar export protected executable {name} must be a regular 0755-style file")
    if _has_capabilities(member):
        die(f"tar export protected path {name} carries file capabilities")


def _resolve_export_path(path: str, members: dict[str, tarfile.TarInfo]) -> str:
    pending = [part for part in path.split("/") if part]
    resolved = ""
    for _ in range(40):
        if not pending:
            return resolved
        candidate = posixpath.join(resolved, pending.pop(0))
        member = members.get(candidate)
        if member is None or not member.issym():
            resolved = candidate
            continue
        base = "/" if member.linkname.startswith("/") else f"/{resolved}"
        target = posixpath.normpath(posixpath.join(base, member.linkname, *pending))
        if target == "/" or not target.startswith("/"):
            die(f"tar export path {path} resolves outside the image root")
        resolved = ""
        pending = [part for part in target.split("/") if part]
    die(f"tar export path {path} has too many symlink hops")
    return ""


def _check_path_shadows(members: dict[str, tarfile.TarInfo], path_value: str) -> None:
    if not path_value:
        die("image PATH is empty; cannot prove the wrapper is unshadowed")
    for directory in path_value.split(":"):
        if not directory.startswith("/") or posixpath.normpath(directory) != directory:
            die(f"image PATH carries a non-canonical directory {directory!r}")
        resolved = _resolve_export_path(directory.removeprefix("/"), members)
        shadow = posixpath.join(resolved, "opencode")
        if shadow != "usr/local/bin/opencode" and shadow in members:
            die(f"tar export carries an opencode PATH shadow at /{shadow}")


def _metadata(members: dict[str, tarfile.TarInfo]) -> dict[str, dict[str, int]]:
    records = {
        name: {
            "mode": _kind(member) | (member.mode & 0o7777),
            "uid": member.uid,
            "gid": member.gid,
        }
        for name, member in members.items()
    }
    # BuildKit's fs.Walk skips the root entry. Treat it as the structural
    # trust boundary; every emitted entry still comes from its tar header.
    records.setdefault("", {"mode": stat.S_IFDIR | 0o755, "uid": 0, "gid": 0})
    return records


def _is_source_path(name: str) -> bool:
    return (
        name in {f"app/{entry}" for entry in BOOT_FILES}
        or any(name == prefix or name.startswith(f"{prefix}/") for prefix in SOURCE_PREFIXES)
    )


def _checkout_entries() -> dict[str, pathlib.Path]:
    entries = {f"app/{name}": LANE / name for name in BOOT_FILES}
    for directory in ("scripts", "config"):
        tree = LANE / directory
        entries[f"app/{directory}"] = tree
        for path in tree.rglob("*"):
            if path.is_symlink() or not (path.is_file() or path.is_dir()):
                die(f"checkout source-owned path is not a regular file/directory: {path}")
            entries[f"app/{path.relative_to(LANE).as_posix()}"] = path
    return entries


def _compare_checkout_tree(
    archive: tarfile.TarFile,
    members: dict[str, tarfile.TarInfo],
    checkout: dict[str, pathlib.Path],
) -> None:
    actual = {name for name in members if _is_source_path(name)}
    if actual != set(checkout):
        missing = sorted(set(checkout) - actual)
        unexpected = sorted(actual - set(checkout))
        die(f"exported /app source tree differs from checkout; missing={missing}, "
            f"unexpected={unexpected}")
    for name, source in checkout.items():
        member = members[name]
        if source.is_dir():
            if not member.isdir():
                die(f"exported {name} must be a directory")
            continue
        if not member.isreg():
            die(f"exported {name} must be a regular file, not a link or special file")
        extracted = archive.extractfile(member)
        if extracted is None:
            die(f"cannot read exported regular file {name}")
        with extracted:
            exported_bytes = extracted.read()
        if exported_bytes != source.read_bytes():
            die(f"exported {name} differs from the checkout (WI#425)")


def _validate_members(members: dict[str, tarfile.TarInfo]) -> None:
    if "" in members and not members[""].isdir():
        die("tar export root entry is not a directory")
    required_dirs = {
        "app", "app/node_modules", "usr", "usr/bin", "usr/local", "usr/local/bin",
        "usr/local/lib", "usr/local/lib/opencode2", *SOURCE_PREFIXES,
    }
    for name in required_dirs:
        if name not in members or not members[name].isdir():
            die(f"tar export directory {name} is missing or not a directory")
    for name in (*INSTALLED_FILES, "usr/bin/tini"):
        if name not in members or not members[name].isreg():
            die(f"tar export installed file {name} must be a regular file, not a link")
        if _has_capabilities(members[name]):
            die(f"tar export protected path {name} carries file capabilities")
    _check_root_member(members["usr/bin"], "usr/bin")
    _check_root_member(members["usr/bin/tini"], "usr/bin/tini", executable=True)
    if "etc/ld.so.preload" in members:
        die("tar export carries /etc/ld.so.preload")
    for name, member in members.items():
        dependency = name == "app/node_modules" or name.startswith("app/node_modules/")
        if dependency:
            _check_root_member(member, name)
        elif _is_source_path(name) and _has_capabilities(member):
            die(f"tar export protected path {name} carries file capabilities")
        if name.startswith("app/") and not _is_source_path(name) and not (
            name == "app/results" or name.startswith("app/results/") or dependency
        ):
            die(f"tar export carries unexpected /app entry {name}")


def _copy_member(
    archive: tarfile.TarFile,
    member: tarfile.TarInfo,
    destination: pathlib.Path,
) -> None:
    source = archive.extractfile(member)
    if source is None:
        die(f"cannot extract regular file {member.name}")
    with source, destination.open("xb") as output:
        shutil.copyfileobj(source, output, length=1024 * 1024)


def _materialize(
    archive: tarfile.TarFile,
    members: dict[str, tarfile.TarInfo],
    export: pathlib.Path,
    checkout: dict[str, pathlib.Path],
) -> None:
    export.mkdir()
    for name in {*checkout, *INSTALLED_FILES}:
        member = members[name]
        destination = export.joinpath(*pathlib.PurePosixPath(name).parts)
        if member.isdir():
            destination.mkdir(parents=True, exist_ok=True)
            continue
        destination.parent.mkdir(parents=True, exist_ok=True)
        _copy_member(archive, member, destination)


def _run_assertion(
    export: pathlib.Path,
    workspace: pathlib.Path,
    members: dict[str, tarfile.TarInfo],
    image_environment: dict[str, str],
) -> None:
    metadata_path = workspace / "image-metadata.json"
    metadata_path.write_text(json.dumps(_metadata(members)), encoding="utf-8")
    wrapper = LANE / "scripts" / "opencode-exposure-guard.sh"
    environment = {key: image_environment.get(key, "") for key in CHECKOUT_ENV_KEYS}
    environment["LUNARWING_WRAPPER_SHA256"] = hashlib.sha256(wrapper.read_bytes()).hexdigest()
    environment_path = workspace / "image-environment.json"
    environment_path.write_text(json.dumps(environment), encoding="utf-8")
    result = subprocess.run(
        [
            sys.executable, "-I", str(ASSERT), "--lane", str(export / "app"),
            "--profile", "opencode2", "--image-filesystem", "--image-root", str(export),
            "--image-metadata", str(metadata_path), "--image-env-file", str(environment_path),
        ],
        capture_output=True,
        text=True,
        timeout=600,
        env={"PATH": os.defpath, "PYTHONDONTWRITEBYTECODE": "1"},
    )
    sys.stdout.write(result.stdout)
    sys.stderr.write(result.stderr)
    if result.returncode != 0:
        die("out-of-band filesystem verification FAILED (WI#425)")


def check_filesystem_half(
    archive_path: pathlib.Path,
    image_environment: dict[str, str],
    workspace_parent: pathlib.Path | None = None,
) -> None:
    try:
        if not stat.S_ISREG(archive_path.lstat().st_mode):
            die(f"filesystem export {archive_path} is not a regular tar file")
    except OSError as error:
        die(f"filesystem export {archive_path} is missing or unreadable: {error}")
    try:
        with tempfile.TemporaryDirectory(
            prefix=".opencode2-verify-", dir=workspace_parent or REPO_ROOT,
        ) as tmp, tarfile.open(archive_path, mode="r:*") as archive:
            members = _members(archive)
            _validate_members(members)
            _check_path_shadows(members, image_environment.get("PATH", ""))
            checkout = _checkout_entries()
            _compare_checkout_tree(archive, members, checkout)
            workspace = pathlib.Path(tmp)
            export = workspace / "rootfs"
            _materialize(archive, members, export, checkout)
            _run_assertion(export, workspace, members, image_environment)
    except (OSError, tarfile.TarError) as error:
        die(f"cannot inspect filesystem export {archive_path}: {error}")
    print("out-of-band filesystem verify: tar metadata and checkout-owned files clean")


def _fetch_candidate_config(image_ref: str, image_digest: str) -> dict:
    registry = os.environ.get("CI_REGISTRY", "")
    prefix = f"{registry}/" if registry else ""
    repo = image_ref[len(prefix):].rsplit(":", 1)[0] if image_ref.startswith(prefix) else image_ref.rsplit(":", 1)[0]
    token = registry_token(repo)
    manifest_url = f"https://{os.environ['CI_REGISTRY']}/v2/{repo}/manifests/{image_digest}"
    request = urllib.request.Request(
        manifest_url,
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.docker.distribution.manifest.v2+json",
        },
    )
    manifest = _registry_json(
        request, timeout=120, description=f"cannot fetch manifest {image_digest} from {repo}",
    )
    config_digest = manifest.get("config", {}).get("digest")
    if not config_digest:
        die(f"manifest {image_digest} carries no config digest")
    return image_config(repo, config_digest, token)


def _normalize_local_config(payload: object) -> dict:
    if isinstance(payload, dict):
        return payload
    if not isinstance(payload, list) or len(payload) != 1 or not isinstance(payload[0], dict):
        die("runtime image inspect must contain exactly one image object")
    inspected = payload[0]
    section = inspected.get("Config")
    if not isinstance(section, dict):
        die("runtime image inspect carries no Config object")
    config = dict(section)
    if "Healthcheck" in inspected:
        config["Healthcheck"] = inspected["Healthcheck"]
    return {"config": config}


def _local_config(path: pathlib.Path) -> dict:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        die(f"cannot read local image config {path}: {error}")
    return _normalize_local_config(payload)


def _argument_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=pathlib.Path)
    parser.add_argument("--config", type=pathlib.Path)
    parser.add_argument("--workspace-dir", type=pathlib.Path)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _argument_parser().parse_args(argv)
    if (args.archive is None) != (args.config is None):
        die("--archive and --config must be supplied together")
    if args.workspace_dir is not None and args.archive is None:
        die("--workspace-dir requires the local --archive/--config mode")
    if args.archive is not None:
        config = _local_config(args.config)
        check_config_half(config)
        check_filesystem_half(
            args.archive, _environment(config), workspace_parent=args.workspace_dir,
        )
        print(f"out-of-band verification PASSED for local export {args.archive} (WI#425)")
        return 0

    image_ref = os.environ.get("WORKER_IMAGE_REF", "")
    image_digest = os.environ.get("WORKER_IMAGE_DIGEST", "")
    if not (image_ref and re.fullmatch(r"sha256:[0-9a-f]{64}", image_digest)):
        die("WORKER_IMAGE_REF and a valid WORKER_IMAGE_DIGEST are required")
    config = _fetch_candidate_config(image_ref, image_digest)
    check_config_half(config)
    check_filesystem_half(REPO_ROOT / EXPORT_ARCHIVE_NAME, _environment(config))
    print(f"out-of-band verification PASSED for {image_ref}@{image_digest} (WI#425)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
