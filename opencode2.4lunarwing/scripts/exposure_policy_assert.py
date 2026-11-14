#!/usr/bin/env python3
"""WI#357 worker exposure policy — static assertions for the opencode2 lane.

Runs in two places with the same rules:

- the image Dockerfile (control 5: every image build fails when the policy
  regresses), additionally checking the built binary for OpenTunnel client
  markers via --binary;
- the repo CI guard .gitlab/scripts/check-worker-exposure-policy.sh
  (control 4), which also runs the v1-lane checks.

Assertions (opencode2 lane):
  1. config/opencode2.json parses and pins "share": "disabled" (control 3).
  2. config/opencode2.json pins "update": "disable".
  3. config/opencode2.json carries the shell deny rules for the
     self-exposure subcommands (control 2, config layer), and sets neither
     experimental.console_watches nor experimental.all_flags (WI#609, !2398
     AAV-1: the Console event watches stay off).
  4. entrypoint.sh starts the server with a password and a loopback default
     hostname, never invokes service mode, never passes --service, defaults
     OPENCODE_BIN to the exposure-guard wrapper (not the real binary), and
     disables implicit service startup and re-derives the runtime config
     from /app/config at boot (controls 2+4; expected startup-shape checks).
  5. scripts/opencode-exposure-guard.sh exists and scans every argv token:
     service/pair subcommands, the --service flag, and every --hostname
     occurrence (control 2, wrapper layer). It also forces
     OPENCODE_FLAGS=-console_watches right before it execs the real binary
     (WI#609, !2398 AAV-1): those two statements must be the last two code
     lines, and no other code line may launch the binary or touch the
     variable.
  6. --binary PATH: the built opencode binary contains no OpenTunnel client
     markers (controls 5 + 8).
  7. --image-filesystem: the installed boot files, scripts and config are
     root-owned and read-only (image builds only); every node_modules entry
     must be a root-owned regular file/directory without setuid/setgid or
     group/other write bits. For the v2 profile the
     installed wrapper is verified too: /usr/local/bin/opencode must be the
     byte-identical root-owned read-only executable exposure guard, PATH must
     resolve 'opencode' to it, the real binary is re-scanned for OpenTunnel
     markers (run-103 GD-2), LUNARWING_REAL_OPENCODE may not retarget the
     wrapper at build time, and the installed digest must match the
     LUNARWING_WRAPPER_SHA256 build-arg anchor (run-106 GJ-3). WI#425 GX-2
     adds the OPENCODE_BIN retarget refusal, the real binary's owner/mode,
     and the parent-directory checks; GX-3 requires all three execute bits.
     --image-root DIR (WI#425 out-of-band verification) checks a safely
     materialized tar export using --image-metadata from its tar headers and
     --image-env-file from the actual image config. Host ownership and the CI
     job's own environment are never treated as image facts; the CI driver
     separately pins the complete runtime config, including PATH order.

v1 lane (--lane opencode4lunarwing): only the share-disabled and
entrypoint-loopback checks apply (v1 has no service mode or
tunnel; pairing is a v2 surface).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import shutil
import stat
import sys

LANE_ROOT = pathlib.Path(__file__).resolve().parent.parent

# Strings that prove the OpenTunnel client was compiled into the binary.
# Checked case-sensitively against the raw bytes; the lane's own stub message
# ("OpenTunnel remote access is compiled out ...") deliberately avoids them.
OPENTUNNEL_MARKERS = (b"opentunnel.xyz", b"@opentunnel/client", b"@opentunnel/protocol")

# Deny rules the lane config must carry (control 2, config layer). Kept in
# sync with sessionPermissions() in scripts/opencode_task_executor.ts.
REQUIRED_SHELL_DENIES = tuple(f"*opencode {sub} *" for sub in ("service", "pair", "serve"))

# WI#609, !2398 AAV-1: opencode v2.2.7 ships Console event watches, a remote
# steering channel through OpenCode Console, behind the feature flag
# `console_watches`. The lane keeps it off in two places. The lane config must
# not turn it on (`experimental.all_flags` turns every flag on). And the
# wrapper forces the flag off in OPENCODE_FLAGS, which upstream resolves before
# every config document, a project's own opencode.json included.
FORBIDDEN_EXPERIMENTAL_FLAGS = ("all_flags", "console_watches")
WRAPPER_FLAG_PIN = 'export OPENCODE_FLAGS="-console_watches"'
WRAPPER_EXEC = 'exec "$REAL_OPENCODE" "$@"'
REAL_BINARY_PATH = "/usr/local/lib/opencode2/opencode"
# SHA-256 over the wrapper's code lines (comments and blank lines stripped),
# so ANY code edit — including one the shape checks below do not name — needs
# a review of this checker in the same commit (WI#609 ABH-2).
WRAPPER_CODE_DIGEST = "cc551f2668957ba4c1354b79dd3723b7f3b8b97246f9f18ac7ca78481830ba0d"
# Shell features that can make the wrapper's tail unreachable or change what
# its last two lines mean. The wrapper needs none of them. Dot-sourcing is
# refused wherever it appears, not only at a line start.
WRAPPER_FORBIDDEN_RE = re.compile(r"\b(alias|eval|source)\b|<<|(^|[;&|\s])\.\s")

failures: list[str] = []


def fail(message: str) -> None:
    failures.append(message)
    print(f"FAIL: {message}", file=sys.stderr)


def check(message: str) -> None:
    print(f"ok: {message}")


def check_config(lane: pathlib.Path, config_name: str, is_v2: bool) -> None:
    config_path = lane / "config" / config_name
    try:
        config = json.loads(config_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"{config_path} does not parse: {error}")
        return
    if config.get("share") == "disabled":
        check(f"{config_name} pins share=disabled")
    else:
        fail(f"{config_path} must pin \"share\": \"disabled\" (got {config.get('share')!r})")
    if not is_v2:
        return
    if config.get("update") == "disable":
        check("opencode2.json pins update=disable")
    else:
        fail(f"{config_path} must pin \"update\": \"disable\" (got {config.get('update')!r})")
    denies = {
        rule.get("resource")
        for rule in config.get("permissions", [])
        if isinstance(rule, dict)
        and rule.get("action") == "shell"
        and rule.get("effect") == "deny"
    }
    missing = [resource for resource in REQUIRED_SHELL_DENIES if resource not in denies]
    if missing:
        fail(f"{config_path} permissions must deny shell resources {missing}")
    else:
        check("opencode2.json carries the self-exposure shell deny rules")
    # Only an explicit false is accepted beside an absent key. Upstream drops
    # a malformed value, which means off, but a lane config that carries one
    # says something its author did not mean, so it fails here.
    experimental = config.get("experimental", {})
    if not isinstance(experimental, dict):
        fail(f"{config_path} \"experimental\" must be an object (got {type(experimental).__name__})")
        return
    opted_in = [
        key for key in FORBIDDEN_EXPERIMENTAL_FLAGS if experimental.get(key, False) is not False
    ]
    if opted_in:
        fail(f"{config_path} must not set experimental flags {opted_in} — the Console "
             "event watches are a remote instruction channel and stay off (WI#609 AAV-1)")
    else:
        check("opencode2.json leaves the Console event watches flag off")


def serve_invocation_lines(code: str) -> list[str]:
    r"""Lines of the '$OPENCODE_BIN" serve' invocation, joining continuations.

    The serve command spans several shell lines (trailing backslashes), so a
    plain '--hostname in code' check could be satisfied by an unrelated line
    (run-79 EI-8). Returns [] when no serve invocation exists.
    """
    lines = code.splitlines()
    for index, line in enumerate(lines):
        if re.search(r'"?\$OPENCODE_BIN"?\s+serve\b', line):
            block = [line.rstrip("\\").rstrip()]
            cursor = index + 1
            while cursor < len(lines) and lines[cursor - 1].rstrip().endswith("\\"):
                block.append(lines[cursor].rstrip("\\").rstrip())
                cursor += 1
            return block
    return []


def check_entrypoint(lane: pathlib.Path, is_v2: bool) -> None:
    entrypoint = lane / "entrypoint.sh"
    try:
        text = entrypoint.read_text(encoding="utf-8")
    except OSError as error:
        fail(f"{entrypoint} unreadable: {error}")
        return
    # Service mode / pairing must never appear as an invocation. Strip comment
    # lines first: prose (like this policy's rationale) may name them.
    code = "\n".join(
        line for line in text.splitlines() if not line.lstrip().startswith("#")
    )
    for subcommand in ("service", "pair"):
        if re.search(rf'(?:opencode|OPENCODE_BIN)"?\s+{subcommand}\b', code):
            fail(f"{entrypoint} invokes 'opencode {subcommand}' — service mode is forbidden")
            break
    else:
        check("entrypoint.sh never invokes service mode or pairing")
    if "--service" in code:
        # run-79 EI-3: serve --service is service mode via a flag.
        fail(f"{entrypoint} passes --service — service mode is forbidden")
    else:
        check("entrypoint.sh never passes --service")
    if 'OPENCODE_SERVE_HOST="${OPENCODE_SERVE_HOST:-127.0.0.1}"' in text:
        check("entrypoint.sh defaults the server bind to loopback")
    else:
        fail(f"{entrypoint} lost its loopback OPENCODE_SERVE_HOST default")
    serve_lines = serve_invocation_lines(code)
    if serve_lines and any("--hostname" in line for line in serve_lines):
        check("entrypoint.sh starts the server with an explicit --hostname")
    else:
        fail(f"{entrypoint} must start the server via serve with --hostname")
    if is_v2:
        # run-79 EI-3: OPENCODE_BIN must resolve to the wrapper, not the real
        # binary at /usr/local/lib/opencode2/opencode.
        if 'OPENCODE_BIN="${OPENCODE_BIN:-/usr/local/bin/opencode}"' in text:
            check("entrypoint.sh runs opencode through the exposure-guard wrapper")
        else:
            fail(f"{entrypoint} must default OPENCODE_BIN to the wrapper at "
                 "/usr/local/bin/opencode (not the real binary)")
        # v2's serve authenticates; the password invariant is v2-only because
        # the v1 bridge sends no credential (v1's cli/cmd/serve.ts does honour
        # OPENCODE_SERVER_PASSWORD at v1.19.5, so a future v1 hardening could
        # adopt the same per-boot shared password this lane exports). Structural:
        # the export must be executable code, not prose naming the variable
        # (run-79 EI-8 — a comment could satisfy a substring check).
        if re.search(r'^\s*export OPENCODE_PASSWORD\b', code, re.MULTILINE):
            check("entrypoint.sh sets a server password")
        else:
            fail(f"{entrypoint} must export OPENCODE_PASSWORD before serve")
        # FB-1: implicit Service.ensure spawns the real binary, so argv checks
        # alone cannot stop it. Pin the boot-time default before mode dispatch.
        # FB-7: the reset must honour OPENCODE_CONFIG_DIR like upstream;
        # FB-6: it must remove an agent-planted FIFO/dir/symlink before the
        # write (a bare redirect hangs, crashes, or follows the symlink).
        boot = code.split('case "$MODE" in', 1)[0]
        service_setup = (
            'SERVICE_CONFIG_DIR="${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/opencode}"',
            'mkdir -p "$SERVICE_CONFIG_DIR"',
            "printf '%s\\n' '{\"disabled\": true}' > \"$SERVICE_CONFIG_DIR/service.json\"",
        )
        if all(line in boot.splitlines() for line in service_setup):
            check("entrypoint.sh disables implicit shared-service startup at boot")
        else:
            fail(f"{entrypoint} must write service.json with disabled=true before mode dispatch")
        # FB-6: without the removal a planted FIFO/dir/symlink at the reset
        # path hangs the boot, crashes it, or redirects the write.
        if 'rm -rf -- "$SERVICE_CONFIG_DIR/service.json"' in boot.splitlines():
            check("entrypoint.sh removes a planted service.json before the reset")
        else:
            fail(f"{entrypoint} must remove a planted service.json before the reset")
        # run-79 EI-5: the workspace config copy is untrusted — it must be
        # regenerated from the root-owned /app/config at every boot (copy or
        # symlink). The live copy remains agent-writable within a session.
        copies = 'cp /app/config/opencode2.json "$WORKSPACE_ROOT/.opencode/opencode2.json"' in code
        links = 'ln -sfn /app/config/opencode2.json "$WORKSPACE_ROOT/.opencode/opencode2.json"' in code
        if copies or links:
            check("entrypoint.sh re-derives the runtime config from /app/config at boot")
        else:
            fail(f"{entrypoint} must materialize the runtime config from "
                 "/app/config/opencode2.json (copy or symlink) at every boot")
        # WI#665: the hosted Console Library is an instruction channel whenever
        # its credential and egress are both present. Keep the credential gate
        # ahead of role dispatch so neither the bridge nor task role skips it.
        early, role_boundary, _ = code.partition(
            'if [[ "${LUNARWING_ROOTFUL_WORKSPACE:-}" == 1 ]]; then'
        )
        console_guard = (
            'case "${LUNARWING_ALLOW_OPENCODE_CONSOLE_CREDENTIAL:-0}" in',
            'if [ -n "${OPENCODE_API_KEY+x}" ]; then',
            'die "OPENCODE_API_KEY is present; refusing startup without '
            'LUNARWING_ALLOW_OPENCODE_CONSOLE_CREDENTIAL=1',
        )
        if role_boundary and all(fragment in early for fragment in console_guard):
            check("entrypoint.sh gates OpenCode Console credentials before role dispatch")
        else:
            fail(f"{entrypoint} must refuse an unacknowledged OPENCODE_API_KEY "
                 "before role dispatch (WI#665)")


def check_wrapper(lane: pathlib.Path) -> None:
    wrapper = lane / "scripts" / "opencode-exposure-guard.sh"
    try:
        text = wrapper.read_text(encoding="utf-8")
    except OSError:
        fail(f"{wrapper} is missing — the binary wrapper is the control-2 layer that "
             "permission rules cannot cover (bash -c, absolute paths)")
        return
    # run-79 EI-2/EI-3, run-84 EU-2: the wrapper must scan EVERY argv token —
    # a \$1-only dispatch misses global flags before the subcommand and
    # repeated --hostname values.
    required_fragments = (
        ('for arg in "$@"', "scan every argv token"),
        ("service) refuse", "refuse the 'service' subcommand"),
        ("pair) refuse", "refuse the 'pair' subcommand"),
        ("upgrade | update) refuse", "refuse the self-upgrade subcommand and its alias"),
        ("--service | --service=*) refuse", "refuse the --service flag"),
        ('--hostname=*) is_loopback', "check every --hostname=value occurrence"),
        ('[ "$want_host" = 0 ]', "refuse a dangling --hostname"),
    )
    missing = [what for fragment, what in required_fragments if fragment not in text]
    if missing:
        fail(f"{wrapper} lost required guard shape: {missing} — the wrapper must "
             "scan every argv token, not only $1")
    else:
        check("wrapper scans every argv token (service/pair/--service/--hostname)")
    # WI#609 AAV-1 / ABE-3 / ABH-2: the pin only holds if every start of the
    # real binary passes through it. This is a shape check on a reviewed
    # file, strict about the text instead of clever about the shell: the
    # last two code lines are the pin and the exec, unindented (not inside a
    # block or a function); nothing else in the code launches the binary,
    # names the variable, or uses a feature that could skip or redefine the
    # tail; and no line before the pin ends in a shell operator that
    # continues into it. On top of the named rules the whole code body must
    # match its reviewed digest, so any code edit fails until this checker is
    # reviewed in the same commit. The behaviour tests and the review cover
    # the committed file.
    raw_code = [line for line in text.splitlines() if line.strip() and not line.strip().startswith("#")]
    code = [line.strip() for line in raw_code]

    def lines_with(needle: str) -> int:
        return sum(1 for line in code if needle in line)

    problems = []
    if raw_code[-2:] != [WRAPPER_FLAG_PIN, WRAPPER_EXEC]:
        problems.append("the last two code lines are not the unindented pin and exec")
    if sum(1 for line in code if re.search(r"\bexec\b", line)) != 1:
        problems.append("the number of code lines using exec is not exactly one")
    if lines_with("REAL_OPENCODE") != 2:
        problems.append("REAL_OPENCODE is used outside its assignment and the final exec")
    if lines_with(REAL_BINARY_PATH) != 1:
        problems.append("the real binary path appears outside the REAL_OPENCODE default")
    if lines_with("OPENCODE_FLAGS") != 1:
        problems.append("another code line names OPENCODE_FLAGS")
    if any(WRAPPER_FORBIDDEN_RE.search(line) for line in code):
        problems.append("alias, eval, source, dot-sourcing or a here-document is used")
    if len(code) >= 3 and code[-3].endswith("\\"):
        problems.append("the line before the pin ends in a continuation")
    if len(code) >= 3 and code[-3].endswith(("&&", "||", "|", "()")):
        problems.append("the line before the pin ends in a shell operator that continues into it")
    if problems:
        fail(f"{wrapper} must end with exactly {WRAPPER_FLAG_PIN!r} then {WRAPPER_EXEC!r} and "
             f"launch the real binary nowhere else: {problems} — the Console event watches "
             "stay off (WI#609 AAV-1)")
        return
    digest = hashlib.sha256("\n".join(code).encode()).hexdigest()
    if digest != WRAPPER_CODE_DIGEST:
        fail(f"{wrapper} code lines differ from the reviewed digest ({digest[:12]}…) — any "
             "wrapper code edit must be reviewed and WRAPPER_CODE_DIGEST updated in the same "
             "commit (WI#609 ABH-2)")
        return
    check("wrapper forces OPENCODE_FLAGS=-console_watches before the real binary starts")


def check_binary(binary: pathlib.Path) -> None:
    try:
        data = binary.read_bytes()
    except OSError as error:
        fail(f"{binary} unreadable: {error}")
        return
    for marker in OPENTUNNEL_MARKERS:
        if marker in data:
            fail(f"{binary} contains OpenTunnel client marker {marker.decode()!r} — "
                 "compile the tunnel out (scripts/compile_out_remote_tunnel.py)")
            return
    check(f"{binary.name} contains no OpenTunnel client markers")


def _path_metadata(
    path: pathlib.Path,
    image_root: pathlib.Path | None,
    records: dict[str, dict[str, int]] | None,
) -> os.stat_result:
    if records is None:
        return path.lstat()
    if image_root is None:
        raise OSError("tar-header metadata requires an image root")
    try:
        relative = pathlib.Path(os.path.abspath(path)).relative_to(
            pathlib.Path(os.path.abspath(image_root))
        )
    except ValueError as error:
        raise OSError(f"path {path} escapes image root {image_root}") from error
    key = "" if relative == pathlib.Path(".") else relative.as_posix()
    record = records.get(key)
    if record is None:
        raise OSError(f"no tar-header metadata for {key or '/'}")
    return os.stat_result((
        record["mode"], 0, 0, 0, record["uid"], record["gid"], 0, 0, 0, 0,
    ))


def _dependency_paths(
    tree: pathlib.Path,
    image_root: pathlib.Path | None,
    records: dict[str, dict[str, int]] | None,
) -> list[pathlib.Path]:
    if records is None:
        return [tree, *tree.rglob("*")]
    if image_root is None:
        raise OSError("tar-header metadata requires an image root")
    try:
        relative = pathlib.Path(os.path.abspath(tree)).relative_to(
            pathlib.Path(os.path.abspath(image_root))
        )
    except ValueError as error:
        raise OSError(f"path {tree} escapes image root {image_root}") from error
    prefix = relative.as_posix()
    descendants = sorted(name for name in records if name.startswith(f"{prefix}/"))
    return [
        tree,
        *(image_root.joinpath(*pathlib.PurePosixPath(name).parts) for name in descendants),
    ]


def _check_dependency_tree(
    lane: pathlib.Path,
    image_root: pathlib.Path | None,
    image_metadata: dict[str, dict[str, int]] | None,
) -> None:
    tree = lane / "node_modules"
    try:
        paths = _dependency_paths(tree, image_root, image_metadata)
    except OSError as error:
        fail(f"dependency tree {tree} is unreadable: {error}")
        return
    for path in paths:
        try:
            metadata = _path_metadata(path, image_root, image_metadata)
        except OSError as error:
            fail(f"dependency path {path} is missing or unreadable: {error}")
            continue
        if not (stat.S_ISREG(metadata.st_mode) or stat.S_ISDIR(metadata.st_mode)):
            fail(f"dependency path {path} must be a regular file or directory, "
                 "not a symlink or special file")
            continue
        if metadata.st_uid != 0 or metadata.st_gid != 0:
            fail(f"dependency path {path} must be owned by root:root")
            continue
        mode = stat.S_IMODE(metadata.st_mode)
        if mode & 0o6022:
            fail(f"dependency path {path} has privilege or write bits {mode:o}")
    if not failures:
        check("image dependencies carry no privilege or group/other write bits")


def check_image_filesystem(
    lane: pathlib.Path,
    *,
    image_root: pathlib.Path | None = None,
    image_metadata: dict[str, dict[str, int]] | None = None,
) -> None:
    """FB-4: check image ownership/modes, from tar headers out of band."""
    paths = [lane, *(lane / name for name in (
        "entrypoint.sh", "entrypoint-split.sh", "health_server.py", "agent_comm_protocol.json",
    ))]
    for name in ("scripts", "config"):
        tree = lane / name
        paths.extend((tree, *tree.rglob("*")))
    for path in paths:
        try:
            metadata = _path_metadata(path, image_root, image_metadata)
        except OSError as error:
            fail(f"image policy file unreadable: {path}: {error}")
            continue
        writable = metadata.st_mode & (0o022 if path == lane else 0o222)
        if metadata.st_uid != 0 or writable or stat.S_ISLNK(metadata.st_mode):
            fail(f"image policy path must be root-owned and read-only: {path}")
    if not failures:
        check("image boot files, scripts and config are root-owned and read-only")
    _check_dependency_tree(lane, image_root, image_metadata)


def _check_real_binary_metadata(
    label: str,
    path: pathlib.Path,
    *,
    owner_uid: int,
    image_root: pathlib.Path | None = None,
    image_metadata: dict[str, dict[str, int]] | None = None,
) -> bool:
    try:
        metadata = _path_metadata(path, image_root, image_metadata)
    except OSError as error:
        fail(f"{label} {path} is missing or unreadable: {error}")
        return False
    if not stat.S_ISREG(metadata.st_mode):
        fail(f"{label} {path} must be a regular file, not a symlink or special file")
        return False
    mode = stat.S_IMODE(metadata.st_mode)
    if metadata.st_uid != owner_uid:
        fail(f"{label} {path} must be owned by uid {owner_uid} (got uid {metadata.st_uid})")
        return False
    if mode & 0o6022:
        fail(f"{label} {path} must not carry setuid/setgid and must not be "
             f"group- or other-writable (mode {mode:o})")
        return False
    return True


def _check_parent_directories(
    label: str,
    path: pathlib.Path,
    *,
    owner_uid: int,
    parent_root: pathlib.Path | None = None,
    image_root: pathlib.Path | None = None,
    image_metadata: dict[str, dict[str, int]] | None = None,
) -> bool:
    """Check parents through, but never above, the trusted filesystem root."""
    absolute_path = pathlib.Path(os.path.abspath(path))
    root = pathlib.Path(os.path.abspath(parent_root or absolute_path.anchor))
    if root not in absolute_path.parents:
        fail(f"{label} path {absolute_path} is outside the trusted parent root {root}")
        return False

    ok = True
    for parent in absolute_path.parents:
        try:
            metadata = _path_metadata(parent, image_root, image_metadata)
        except OSError as error:
            fail(f"{label} parent {parent} is missing or unreadable: {error}")
            ok = False
            if parent == root:
                break
            continue
        if not stat.S_ISDIR(metadata.st_mode):
            fail(f"{label} parent {parent} must be a directory")
            ok = False
        mode = stat.S_IMODE(metadata.st_mode)
        if metadata.st_uid not in (owner_uid, 0):
            fail(f"{label} parent {parent} must be owned by uid {owner_uid} or root "
                 f"(got uid {metadata.st_uid})")
            ok = False
        if mode & 0o022 and not (
            mode & stat.S_ISVTX and metadata.st_uid in (owner_uid, 0)
        ):
            fail(f"{label} parent {parent} must not be group- or other-writable "
                 f"(mode {mode:o}) — a writable parent allows entry replacement")
            ok = False
        if parent == root:
            break
    return ok


def _read_installed_wrapper(
    lane: pathlib.Path,
    wrapper_path: pathlib.Path,
    environment: dict[str, str],
    *,
    owner_uid: int,
    wrapper_sha256: str | None,
    image_root: pathlib.Path | None,
    image_metadata: dict[str, dict[str, int]] | None,
) -> bytes | None:
    try:
        metadata = _path_metadata(wrapper_path, image_root, image_metadata)
    except OSError as error:
        fail(f"installed wrapper {wrapper_path} is missing or unreadable: {error}")
        return None
    if not stat.S_ISREG(metadata.st_mode):
        fail(f"installed wrapper {wrapper_path} must be a regular file, not a symlink")
        return None
    if metadata.st_uid != owner_uid:
        fail(f"installed wrapper {wrapper_path} must be owned by uid {owner_uid}")
        return None
    if stat.S_IMODE(metadata.st_mode) & 0o6022:
        fail(f"installed wrapper {wrapper_path} must not carry setuid/setgid and "
             "must not be group- or other-writable")
        return None
    if (stat.S_IMODE(metadata.st_mode) & 0o111) != 0o111:
        fail(f"installed wrapper {wrapper_path} must be executable by user, group "
             f"and other (mode {stat.S_IMODE(metadata.st_mode):o}) — a root-owned "
             f"0744 wrapper bricks the runtime user (WI#425 GX-3)")
        return None

    source = lane / "scripts" / "opencode-exposure-guard.sh"
    try:
        installed = wrapper_path.read_bytes()
        expected = source.read_bytes()
    except OSError as error:
        fail(f"installed wrapper comparison unreadable: {error}")
        return None
    if installed != expected:
        fail(f"installed wrapper {wrapper_path} is not byte-identical to {source} — "
             "replaced after the scan (run-103 GD-2)")
        return None
    anchor = wrapper_sha256 if wrapper_sha256 is not None else environment.get(
        "LUNARWING_WRAPPER_SHA256", ""
    )
    installed_digest = hashlib.sha256(installed).hexdigest()
    if not anchor:
        fail("LUNARWING_WRAPPER_SHA256 (the checkout's wrapper digest) is missing — "
             "the installed wrapper cannot be anchored (run-106 GJ-3a)")
        return None
    if anchor != installed_digest:
        fail(f"installed wrapper {wrapper_path} digest {installed_digest} does not "
             "match the LUNARWING_WRAPPER_SHA256 build-arg anchor — rewritten "
             "together with the in-image reference (run-106 GJ-3a)")
        return None
    return installed


def _override_path(value: str, image_root: pathlib.Path | None) -> pathlib.Path | None:
    path = pathlib.Path(value)
    if image_root is None:
        return path
    if not path.is_absolute() or ".." in path.parts:
        return None
    return image_root.joinpath(*path.parts[1:])


def _check_wrapper_environment(
    environment: dict[str, str],
    wrapper_path: pathlib.Path,
    binary_path: pathlib.Path,
    image_root: pathlib.Path | None,
) -> None:
    if environment.get("OPENCODE_DISABLE_AUTOUPDATE") != "1":
        fail("OPENCODE_DISABLE_AUTOUPDATE=1 must be set in the image environment — "
             "the updater ignores the lane config's update pin")
    expected_wrapper = pathlib.Path("/usr/local/bin/opencode") if image_root else wrapper_path
    expected_binary = (
        pathlib.Path("/usr/local/lib/opencode2/opencode") if image_root else binary_path
    )
    overrides = (
        ("LUNARWING_REAL_OPENCODE", expected_binary, "run-106 GJ-3b"),
        ("OPENCODE_BIN", expected_wrapper, "WI#425 GX-2"),
    )
    for key, expected, reason in overrides:
        value = environment.get(key, "")
        if not value or pathlib.Path(value) == expected:
            continue
        fail(f"{key}={value!r} is set in the image environment — it must resolve to "
             f"{expected} ({reason})")
        mapped = _override_path(value, image_root)
        if mapped is not None:
            check_binary(mapped)


def check_installed_wrapper(
    lane: pathlib.Path,
    wrapper_path: pathlib.Path = pathlib.Path("/usr/local/bin/opencode"),
    binary_path: pathlib.Path = pathlib.Path("/usr/local/lib/opencode2/opencode"),
    *,
    owner_uid: int = 0,
    wrapper_sha256: str | None = None,
    check_path_resolution: bool = True,
    parent_root: pathlib.Path | None = None,
    environ: dict[str, str] | None = None,
    image_root: pathlib.Path | None = None,
    image_metadata: dict[str, dict[str, int]] | None = None,
) -> None:
    """Verify the installed wrapper and binary, in-image or from a tar export."""
    environment = os.environ if environ is None else environ
    installed = _read_installed_wrapper(
        lane,
        wrapper_path,
        environment,
        owner_uid=owner_uid,
        wrapper_sha256=wrapper_sha256,
        image_root=image_root,
        image_metadata=image_metadata,
    )
    if installed is None:
        return
    _check_wrapper_environment(environment, wrapper_path, binary_path, image_root)
    if check_path_resolution:
        resolved = shutil.which("opencode", path=environment.get("PATH"))
        if resolved is None or pathlib.Path(resolved) != wrapper_path:
            fail(f"'opencode' on PATH must resolve to the wrapper {wrapper_path} "
                 f"(got {resolved!r}) — an earlier PATH dir shadows the guard")
            return
    binary_ok = _check_real_binary_metadata(
        "real binary",
        binary_path,
        owner_uid=owner_uid,
        image_root=image_root,
        image_metadata=image_metadata,
    )
    for label, path in (("wrapper", wrapper_path), ("real binary", binary_path)):
        _check_parent_directories(
            label,
            path,
            owner_uid=owner_uid,
            parent_root=parent_root,
            image_root=image_root,
            image_metadata=image_metadata,
        )
    if binary_ok:
        check_binary(binary_path)
    if not failures:
        check(f"installed wrapper {wrapper_path} is the exposure guard "
              "and the real binary carries no tunnel markers")


def _json_object(path: pathlib.Path, label: str) -> dict:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise ValueError(f"cannot read {label} {path}: {error}") from error
    if not isinstance(value, dict):
        raise ValueError(f"{label} {path} must contain a JSON object")
    return value


def _out_of_band_inputs(args: argparse.Namespace) -> tuple[dict | None, dict | None]:
    if args.image_root is None:
        if args.image_metadata is not None or args.image_env_file is not None:
            raise ValueError("--image-metadata and --image-env-file require --image-root")
        return None, None
    if not args.image_filesystem:
        raise ValueError("--image-root requires --image-filesystem")
    if args.image_metadata is None or args.image_env_file is None:
        raise ValueError("--image-root requires --image-metadata and --image-env-file")

    metadata = _json_object(args.image_metadata, "image metadata")
    for key, record in metadata.items():
        path = pathlib.PurePosixPath(key)
        canonical = "" if key == "" else path.as_posix()
        if key != canonical or path.is_absolute() or ".." in path.parts:
            raise ValueError(f"image metadata has a non-canonical path {key!r}")
        if not isinstance(record, dict) or any(
            type(record.get(field)) is not int or record[field] < 0
            for field in ("mode", "uid", "gid")
        ):
            raise ValueError(f"image metadata for {key!r} needs integer mode/uid/gid")
    environment = _json_object(args.image_env_file, "image environment")
    allowed = {
        "LUNARWING_WRAPPER_SHA256", "OPENCODE_DISABLE_AUTOUPDATE",
        "LUNARWING_REAL_OPENCODE", "OPENCODE_BIN",
    }
    if any(not isinstance(key, str) or key not in allowed for key in environment):
        raise ValueError("image environment file carries an unapproved key")
    if any(not isinstance(value, str) for value in environment.values()):
        raise ValueError("image environment values must be strings")
    return metadata, environment


def _argument_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lane", type=pathlib.Path, default=LANE_ROOT)
    parser.add_argument("--binary", type=pathlib.Path, default=None)
    parser.add_argument("--image-filesystem", action="store_true")
    parser.add_argument("--image-root", type=pathlib.Path, default=None)
    parser.add_argument("--image-metadata", type=pathlib.Path, default=None)
    parser.add_argument("--image-env-file", type=pathlib.Path, default=None)
    parser.add_argument("--config", default=None)
    parser.add_argument("--profile", choices=("opencode2", "opencode-v1"), default=None)
    return parser


def main() -> int:
    args = _argument_parser().parse_args()
    lane = args.lane
    if not lane.is_dir():
        print(f"FAIL: lane root {lane} is not a directory", file=sys.stderr)
        return 1
    profile = args.profile or ("opencode2" if lane.name.startswith("opencode2") else "opencode-v1")
    is_v2 = profile == "opencode2"
    try:
        image_metadata, image_environment = _out_of_band_inputs(args)
    except ValueError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    if args.image_root is not None and not is_v2:
        print("FAIL: --image-root is supported only for the opencode2 profile", file=sys.stderr)
        return 1

    config_name = args.config or ("opencode2.json" if is_v2 else "opencode.json")
    check_config(lane, config_name, is_v2)
    check_entrypoint(lane, is_v2)
    if is_v2:
        check_wrapper(lane)
    if args.binary is not None:
        check_binary(args.binary)
    if args.image_filesystem:
        check_image_filesystem(
            lane, image_root=args.image_root, image_metadata=image_metadata
        )
        if is_v2:
            if args.image_root is not None:
                check_installed_wrapper(
                    lane,
                    args.image_root / "usr/local/bin/opencode",
                    args.image_root / "usr/local/lib/opencode2/opencode",
                    check_path_resolution=False,
                    parent_root=args.image_root,
                    environ=image_environment,
                    image_root=args.image_root,
                    image_metadata=image_metadata,
                )
            else:
                check_installed_wrapper(lane)

    if failures:
        print(f"exposure policy: {len(failures)} check(s) FAILED for {lane}", file=sys.stderr)
        return 1
    print(f"exposure policy: all checks passed for {lane} (profile {profile})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
