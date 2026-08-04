#!/usr/bin/env python3
"""Secret-safe primitives for DarkIRC H3 live validation."""

from __future__ import annotations

import argparse
import json
import os
import pwd
import re
import stat
import sys
import time
from pathlib import Path
from typing import NoReturn

SECRET_KEYS = {"local_private", "my_dm_chacha_secret", "secret_key"}
OFFER_KEYS = {
    "schema", "version", "artifact_role", "offer_id", "sender_contact_id",
    "intended_peer_contact_id", "intended_peer_fingerprint", "generator_profile",
    "binary_sha256", "key_format", "source_revision", "public_key",
    "public_fingerprint", "generation", "created_at", "expires_at", "label",
    "previous_fingerprint",
}
RESPONSE_KEYS = OFFER_KEYS | {"in_reply_to"}
SECRET_RE = re.compile(
    rb'(?:local_private|my_dm_chacha_secret|secret_key)\s*[=:]\s*["\']([^"\'\r\n]{16,})'
)
MAX_FILE = 1024 * 1024


def fail(message: str) -> NoReturn:
    raise SystemExit(f"FAIL: {message}")


def require_root() -> None:
    if os.geteuid() != 0:
        fail("operation requires root")


def write_all(fd: int, data: bytes) -> None:
    view = memoryview(data)
    while view:
        written = os.write(fd, view)
        if written == 0:
            fail("short write")
        view = view[written:]


def secure_run_dir(path: Path) -> None:
    st = os.lstat(path)
    if not stat.S_ISDIR(st.st_mode) or stat.S_ISLNK(st.st_mode):
        fail("run directory is not a real directory")
    if st.st_uid != 0 or stat.S_IMODE(st.st_mode) != 0o700:
        fail("run directory must be root-owned mode 0700")


def regular_nofollow(path: Path, max_size: int = MAX_FILE) -> bytes:
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        st = os.fstat(fd)
        if not stat.S_ISREG(st.st_mode) or st.st_size > max_size:
            fail(f"unsafe or oversized input: {path.name}")
        chunks: list[bytes] = []
        remaining = max_size + 1
        while remaining:
            chunk = os.read(fd, min(remaining, 64 * 1024))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        data = b"".join(chunks)
        if len(data) > max_size:
            fail(f"oversized input: {path.name}")
        return data
    finally:
        os.close(fd)


def secret_values(value: object) -> list[str]:
    found: list[str] = []
    if isinstance(value, dict):
        for key, child in value.items():
            if key in SECRET_KEYS and isinstance(child, str) and len(child) >= 16:
                found.append(child)
            found.extend(secret_values(child))
    elif isinstance(value, list):
        for child in value:
            found.extend(secret_values(child))
    return found


def extract_file(path: Path) -> list[str]:
    data = regular_nofollow(path)
    values: list[str] = []
    try:
        values.extend(secret_values(json.loads(data)))
    except (json.JSONDecodeError, UnicodeDecodeError):
        pass
    values.extend(match.decode("utf-8") for match in SECRET_RE.findall(data))
    return values


def cmd_extract(args: argparse.Namespace) -> int:
    require_root()
    run_dir = Path(args.run_dir)
    secure_run_dir(run_dir)
    output = Path(args.output)
    if output.parent != run_dir:
        fail("canary output must be directly inside run directory")
    existing: list[str] = []
    if output.exists():
        existing = json.loads(regular_nofollow(output).decode("utf-8"))
    found = set(existing)
    for root_arg in args.state_dir:
        root = Path(root_arg)
        st = os.lstat(root)
        if not stat.S_ISDIR(st.st_mode) or stat.S_ISLNK(st.st_mode):
            fail("DarkIRC state root is unsafe")
        for directory, dirnames, filenames in os.walk(root, followlinks=False):
            dirnames[:] = [
                name
                for name in dirnames
                if name != "datastore" and not (Path(directory) / name).is_symlink()
            ]
            for name in filenames:
                path = Path(directory) / name
                try:
                    found.update(extract_file(path))
                except SystemExit:
                    raise
                except OSError as error:
                    fail(f"cannot extract canary from {path.name}: {error.strerror}")
    temporary = output.with_name(output.name + ".tmp")
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC, 0o600)
    try:
        write_all(fd, json.dumps(sorted(found)).encode("utf-8"))
        os.fsync(fd)
    finally:
        os.close(fd)
    os.replace(temporary, output)
    print(f"canary extraction: {len(found)} private values retained in root-only analyzer")
    return 0


def scan_paths(canaries: list[bytes], paths: list[Path]) -> tuple[int, int]:
    matches = 0
    checked = 0
    for root in paths:
        if not root.exists():
            fail(f"scan surface missing: {root}")
        candidates = root.rglob("*") if root.is_dir() else [root]
        for path in candidates:
            try:
                data = regular_nofollow(path, 16 * MAX_FILE)
            except IsADirectoryError:
                continue
            except OSError as error:
                fail(f"cannot scan surface file {path.name}: {error.strerror}")
            checked += 1
            matches += sum(data.count(canary) for canary in canaries)
    if checked == 0:
        fail("scan surface has no regular files")
    return checked, matches


def cmd_scan(args: argparse.Namespace) -> int:
    canary_path = Path(args.canaries)
    values = json.loads(regular_nofollow(canary_path).decode("utf-8"))
    if not isinstance(values, list) or any(not isinstance(value, str) for value in values):
        fail("invalid canary file")
    canaries = [value.encode("utf-8") for value in values if len(value) >= 16]
    if not canaries:
        fail("no private-key canaries found")
    failed = False
    for surface in args.surface:
        label, separator, raw_paths = surface.partition("=")
        if not separator or not label or not raw_paths:
            fail("surface must use LABEL=PATH[,PATH]")
        checked, matches = scan_paths(canaries, [Path(item) for item in raw_paths.split(",")])
        status = "PASS" if matches == 0 else "FAIL"
        print(f"surface={label} status={status} files={checked} matches={matches}")
        failed |= matches != 0
    return 1 if failed else 0


def public_shape_safe(value: object) -> bool:
    if isinstance(value, dict):
        return not (SECRET_KEYS & set(value)) and all(public_shape_safe(child) for child in value.values())
    if isinstance(value, list):
        return all(public_shape_safe(child) for child in value)
    return True


def public_artifact_safe(value: object) -> bool:
    if not isinstance(value, dict) or not public_shape_safe(value):
        return False
    role = value.get("artifact_role")
    expected = OFFER_KEYS if role == "initiator_offer" else RESPONSE_KEYS if role == "responder_response" else None
    return expected is not None and set(value) == expected


def write_new(dest: Path, data: bytes, uid: int, gid: int, mode: int) -> None:
    parent_st = os.lstat(dest.parent)
    if not stat.S_ISDIR(parent_st.st_mode) or stat.S_ISLNK(parent_st.st_mode):
        fail("destination parent is unsafe")
    if parent_st.st_uid != uid:
        fail("destination parent owner mismatch")
    fd = os.open(dest, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC, mode)
    try:
        os.fchown(fd, uid, gid)
        write_all(fd, data)
        os.fsync(fd)
    except BaseException:
        os.unlink(dest)
        raise
    finally:
        os.close(fd)


def cmd_copy_public(args: argparse.Namespace) -> int:
    require_root()
    source = Path(args.source)
    data = regular_nofollow(source)
    source_st = os.lstat(source)
    if source_st.st_uid != args.source_uid or stat.S_IMODE(source_st.st_mode) & 0o077:
        fail("public artifact source owner/mode is unsafe")
    try:
        artifact = json.loads(data)
    except (json.JSONDecodeError, UnicodeDecodeError):
        fail("public artifact is not JSON")
    if not public_artifact_safe(artifact):
        fail("artifact is not exact Stage A public schema")
    write_new(Path(args.dest), data, args.dest_uid, args.dest_gid, 0o600)
    print("public artifact copied: validated JSON, no secret-shaped fields")
    return 0


def cmd_capture_file(args: argparse.Namespace) -> int:
    require_root()
    data = regular_nofollow(Path(args.source), 16 * MAX_FILE)
    write_new(Path(args.dest), data, 0, 0, 0o600)
    return 0


def cmd_probe_peer(args: argparse.Namespace) -> int:
    require_root()
    account = pwd.getpwnam(args.user)
    pid = os.fork()
    if pid == 0:
        os.initgroups(account.pw_name, account.pw_gid)
        os.setgid(account.pw_gid)
        os.setuid(account.pw_uid)
        list_denied = open_denied = False
        try:
            os.listdir(args.path)
        except OSError:
            list_denied = True
        try:
            fd = os.open(args.path, os.O_RDONLY | os.O_NOFOLLOW)
            os.close(fd)
        except OSError:
            open_denied = True
        os._exit(0 if list_denied and open_denied else 1)
    _, status = os.waitpid(pid, 0)
    if os.waitstatus_to_exitcode(status) != 0:
        fail("cross-user peer path was listable/openable")
    print("cross-user peer secret path: PASS")
    return 0


def descendants(root_pid: int) -> set[int]:
    parent_map: dict[int, list[int]] = {}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            fields = (entry / "stat").read_text().split()
            parent_map.setdefault(int(fields[3]), []).append(int(entry.name))
        except (OSError, ValueError, IndexError):
            continue
    result = {root_pid}
    pending = [root_pid]
    while pending:
        children = parent_map.get(pending.pop(), [])
        result.update(children)
        pending.extend(children)
    return result


def cmd_watch_proc(args: argparse.Namespace) -> int:
    require_root()
    output = Path(args.output)
    ready = Path(args.ready)
    sampled = False
    with output.open("xb") as stream:
        while Path(f"/proc/{args.pid}").exists():
            for pid in descendants(args.pid):
                try:
                    executable = os.readlink(f"/proc/{pid}/exe").encode("utf-8", "surrogateescape")
                    stream.write(b"\nH3PROC pid=" + str(pid).encode("ascii") + b" exe=" + executable + b"\n")
                except OSError:
                    pass
                for leaf in ("cmdline", "environ"):
                    try:
                        stream.write(regular_nofollow(Path(f"/proc/{pid}/{leaf}"), 4 * MAX_FILE))
                        stream.write(b"\n")
                    except (OSError, SystemExit):
                        pass
            stream.flush()
            if not sampled:
                ready.touch(mode=0o600, exist_ok=False)
                sampled = True
            time.sleep(0.001)
    return 0


def cmd_verify_proc(args: argparse.Namespace) -> int:
    captures = [regular_nofollow(Path(path), 32 * MAX_FILE) for path in args.capture]
    combined = b"\n".join(captures)
    missing_cmdline = [
        marker for marker in args.expect_cmdline if marker.encode("utf-8") not in combined
    ]
    executable_names = {
        os.path.basename(os.fsdecode(line.partition(b" exe=")[2]))
        for line in combined.splitlines()
        if line.startswith(b"H3PROC pid=") and b" exe=" in line
    }
    missing_executable = [marker for marker in args.expect_exe if marker not in executable_names]
    if missing_cmdline or missing_executable:
        missing = missing_cmdline + [f"exe:{marker}" for marker in missing_executable]
        fail("proc capture missing required process markers: " + ", ".join(missing))
    marker_count = len(args.expect_cmdline) + len(args.expect_exe)
    print(f"proc coverage: PASS captures={len(captures)} markers={marker_count}")
    return 0


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    commands = root.add_subparsers(dest="command", required=True)
    extract = commands.add_parser("extract")
    extract.add_argument("--run-dir", required=True)
    extract.add_argument("--output", required=True)
    extract.add_argument("--state-dir", action="append", required=True)
    extract.set_defaults(func=cmd_extract)
    scan = commands.add_parser("scan")
    scan.add_argument("--canaries", required=True)
    scan.add_argument("--surface", action="append", required=True)
    scan.set_defaults(func=cmd_scan)
    copy = commands.add_parser("copy-public")
    copy.add_argument("--source", required=True)
    copy.add_argument("--dest", required=True)
    copy.add_argument("--source-uid", type=int, required=True)
    copy.add_argument("--dest-uid", type=int, required=True)
    copy.add_argument("--dest-gid", type=int, required=True)
    copy.set_defaults(func=cmd_copy_public)
    capture = commands.add_parser("capture-file")
    capture.add_argument("--source", required=True)
    capture.add_argument("--dest", required=True)
    capture.set_defaults(func=cmd_capture_file)
    probe = commands.add_parser("probe-peer")
    probe.add_argument("--user", required=True)
    probe.add_argument("--path", required=True)
    probe.set_defaults(func=cmd_probe_peer)
    watch = commands.add_parser("watch-proc")
    watch.add_argument("--pid", type=int, required=True)
    watch.add_argument("--output", required=True)
    watch.add_argument("--ready", required=True)
    watch.set_defaults(func=cmd_watch_proc)
    verify = commands.add_parser("verify-proc")
    verify.add_argument("--capture", action="append", required=True)
    verify.add_argument("--expect-cmdline", action="append", default=[])
    verify.add_argument("--expect-exe", action="append", default=[])
    verify.set_defaults(func=cmd_verify_proc)
    return root


def main() -> int:
    args = parser().parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
