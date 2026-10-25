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
     self-exposure subcommands (control 2, config layer).
  4. entrypoint.sh starts the server with a password and a loopback default
     hostname, never invokes service mode, never passes --service, defaults
     OPENCODE_BIN to the exposure-guard wrapper (not the real binary), and
     disables implicit service startup and re-derives the runtime config
     from /app/config at boot (controls 2+4; expected startup-shape checks).
  5. scripts/opencode-exposure-guard.sh exists and scans every argv token:
     service/pair subcommands, the --service flag, and every --hostname
     occurrence (control 2, wrapper layer).
  6. --binary PATH: the built opencode binary contains no OpenTunnel client
     markers (controls 5 + 8).
  7. --image-filesystem: the installed boot files, scripts and config are
     root-owned and read-only (image builds only). For the v2 profile the
     installed wrapper is verified too: /usr/local/bin/opencode must be the
     byte-identical root-owned read-only executable exposure guard, PATH must
     resolve 'opencode' to it, the real binary is re-scanned for OpenTunnel
     markers (run-103 GD-2), LUNARWING_REAL_OPENCODE may not retarget the
     wrapper at build time, and the installed digest must match the
     LUNARWING_WRAPPER_SHA256 build-arg anchor (run-106 GJ-3). WI#425 GX-2
     adds the OPENCODE_BIN retarget refusal, the real binary's owner/mode,
     and the parent-directory checks; GX-3 requires all three execute bits.
     --image-root DIR (WI#425 out-of-band verification) points the
     installed-wrapper check at an unpacked BuildKit filesystem export
     instead of the live root: the PATH-resolution check is skipped there
     (no live environment) and the CI driver checks the image config
     Env/Shell separately.

v1 lane (--lane opencode4lunarwing): only the share-disabled and
entrypoint-loopback checks apply (v1.18.25 has no service mode or
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
        # v2 mandates Basic auth on serve; v1.18.25's serve has no auth
        # support, so the password invariant is v2-only. Structural: the
        # export must be executable code, not prose naming the variable
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


def check_image_filesystem(lane: pathlib.Path) -> None:
    """FB-4: check actual image metadata, not just Dockerfile text."""
    paths = [lane, *(lane / name for name in (
        "entrypoint.sh", "health_server.py", "agent_comm_protocol.json",
    ))]
    for name in ("scripts", "config"):
        tree = lane / name
        paths.extend((tree, *tree.rglob("*")))
    for path in paths:
        try:
            metadata = path.lstat()
        except OSError as error:
            fail(f"image policy file unreadable: {path}: {error}")
            continue
        # /app must prevent replacement of its children; its root-only write
        # bit is fine. All protected children must have no write bits at all.
        writable = metadata.st_mode & (0o022 if path == lane else 0o222)
        if metadata.st_uid != 0 or writable or path.is_symlink():
            fail(f"image policy path must be root-owned and read-only: {path}")
    if not failures:
        check("image boot files, scripts and config are root-owned and read-only")


def _check_real_binary_metadata(
    label: str, path: pathlib.Path, *, owner_uid: int
) -> bool:
    """WI#425 GX-2: the real binary must be root-owned and not group/other
    writable, otherwise the agent uid could replace it with a tunnel-bearing
    build that the wrapper would then exec. Missing/unreadable reports the
    real cause (GZ-2 shape)."""
    try:
        metadata = path.lstat()
    except OSError as error:
        fail(f"{label} {path} is missing or unreadable: {error}")
        return False
    mode = stat.S_IMODE(metadata.st_mode)
    if metadata.st_uid != owner_uid:
        fail(f"{label} {path} must be owned by uid {owner_uid} (got uid {metadata.st_uid})")
        return False
    if mode & 0o022:
        fail(f"{label} {path} must not be group- or other-writable (mode {mode:o})")
        return False
    return True


def _check_parent_directories(
    label: str, path: pathlib.Path, *, owner_uid: int
) -> bool:
    """WI#425 GX-2: every parent of the wrapper and the real binary must block
    entry replacement by a non-root uid, so the agent cannot swap either
    file by renaming it through a writable parent.

    A parent passes when it is owned by the wrapper owner or by root and
    carries no group/other write bit, or when it is sticky (the sticky bit
    already prevents non-owner rename/removal of the entries below it —
    the /tmp shape). Hardcoding root as an allowed owner matters for the
    test fixtures, whose paths live under root-owned ancestors (/home, /).
    """
    ok = True
    for parent in path.parents:
        try:
            metadata = parent.lstat()
        except OSError as error:
            fail(f"{label} parent {parent} is missing or unreadable: {error}")
            ok = False
            continue
        mode = stat.S_IMODE(metadata.st_mode)
        if mode & stat.S_ISVTX:
            continue
        if metadata.st_uid not in (owner_uid, 0):
            fail(f"{label} parent {parent} must be owned by uid {owner_uid} or root "
                 f"(got uid {metadata.st_uid})")
            ok = False
        if mode & 0o022:
            fail(f"{label} parent {parent} must not be group- or other-writable "
                 f"(mode {mode:o}) — a writable parent allows entry replacement")
            ok = False
    return ok


def check_installed_wrapper(
    lane: pathlib.Path,
    wrapper_path: pathlib.Path = pathlib.Path("/usr/local/bin/opencode"),
    binary_path: pathlib.Path = pathlib.Path("/usr/local/lib/opencode2/opencode"),
    *,
    owner_uid: int = 0,
    wrapper_sha256: str | None = None,
    check_path_resolution: bool = True,
) -> None:
    """Run-103 GD-2: verify the shipped wrapper, not the Dockerfile's claim.

    A RUN in the final stage between the wrapper-install scan and the
    --image-filesystem assertion can replace the wrapper (or the real
    binary) after the earlier scan, so inspect the installed files here.
    The wrapper must be the byte-identical, owner-only-writable,
    executable exposure guard, PATH must resolve 'opencode' to it, the real
    binary is re-scanned for OpenTunnel markers, and LUNARWING_REAL_OPENCODE
    may not retarget the wrapper at build time (run-106 GJ-3b/c). The
    installed digest must also match the LUNARWING_WRAPPER_SHA256 build-arg
    anchor (run-106 GJ-3a). This detects accidental drift; an adversarial
    Dockerfile edit can still defeat it by replacing the environment or
    checker (run-109 GW-3). Image builds only (v2 profile); the durable
    checkout-side out-of-band verification is WI#425 Ask 1 (the CI export +
    --image-root path and mt-admin's podman create/cp/cmp).

    WI#425 GX-2: OPENCODE_BIN may not retarget the wrapper at build time,
    the real binary's owner and mode are verified (root-owned, not
    group/other-writable), and every parent directory of both the wrapper
    and the real binary is root-owned and not group/other-writable.
    WI#425 GX-3: the executable-bit check requires all three execute bits
    (mode AND 0o111 == 0o111) — a root-owned 0744 wrapper bricks the
    runtime user (the agent uid cannot execute an owner-only-executable
    file), so owner-only exec no longer passes.
    """
    try:
        metadata = wrapper_path.lstat()
    except OSError as error:
        fail(f"installed wrapper {wrapper_path} is missing or unreadable: {error}")
        return
    if wrapper_path.is_symlink() or not stat.S_ISREG(metadata.st_mode):
        fail(f"installed wrapper {wrapper_path} must be a regular file, not a symlink")
        return
    if metadata.st_uid != owner_uid:
        fail(f"installed wrapper {wrapper_path} must be owned by uid {owner_uid}")
        return
    if metadata.st_mode & 0o022:
        fail(f"installed wrapper {wrapper_path} must not be group- or other-writable")
        return
    # Run-106 GJ-3(c) + WI#425 GX-3: a non-executable wrapper is skipped by
    # PATH lookup, and an owner-only-executable wrapper (0744) cannot be
    # executed by the runtime user — require every execute bit so the
    # agent uid the wrapper serves can run it.
    if (stat.S_IMODE(metadata.st_mode) & 0o111) != 0o111:
        fail(f"installed wrapper {wrapper_path} must be executable by user, group "
             f"and other (mode {stat.S_IMODE(metadata.st_mode):o}) — a root-owned "
             f"0744 wrapper bricks the runtime user (WI#425 GX-3)")
        return
    source = lane / "scripts" / "opencode-exposure-guard.sh"
    try:
        installed = wrapper_path.read_bytes()
        expected = source.read_bytes()
    except OSError as error:
        fail(f"installed wrapper comparison unreadable: {error}")
        return
    if installed != expected:
        fail(f"installed wrapper {wrapper_path} is not byte-identical to {source} — "
             "replaced after the scan (run-103 GD-2)")
        return
    # Run-106 GJ-3(a): {source} ships inside the image, so the same root RUN
    # can rewrite both files identically and pass the byte-identity check.
    # The build passes the checkout's wrapper sha256 as a build argument
    # (LUNARWING_WRAPPER_SHA256); compare the installed digest against that
    # anchor, and fail closed when it is absent — a dropped ARG or a builder
    # that does not forward the arg must fail the image build, not pass.
    # The Dockerfile still controls this environment and checker (GW-3).
    anchor = wrapper_sha256 if wrapper_sha256 is not None else os.environ.get(
        "LUNARWING_WRAPPER_SHA256", ""
    )
    installed_digest = hashlib.sha256(installed).hexdigest()
    if not anchor:
        fail("LUNARWING_WRAPPER_SHA256 (the checkout's wrapper digest) is missing — "
             "the installed wrapper cannot be anchored (run-106 GJ-3a)")
        return
    if anchor != installed_digest:
        fail(f"installed wrapper {wrapper_path} digest {installed_digest} does not "
             f"match the LUNARWING_WRAPPER_SHA256 build-arg anchor — rewritten "
             "together with the in-image reference (run-106 GJ-3a)")
        return
    # !1735 review KG-3 / !1699 JE-3: the updater ignores the lane config's
    # "update": "disable" pin (it reads only the global config dir, never
    # OPENCODE_CONFIG), so the image env must carry the kill switch the
    # updater actually honours (updater.ts inspect() gate).
    if os.environ.get("OPENCODE_DISABLE_AUTOUPDATE") != "1":
        fail("OPENCODE_DISABLE_AUTOUPDATE=1 must be set in the image environment — "
             "the updater ignores the lane config's update pin")
        return
    # Run-106 GJ-3(b): the wrapper honours LUNARWING_REAL_OPENCODE over the
    # default binary path, so an ENV pointing it elsewhere would exec that
    # path at runtime while this check scans only binary_path. Only the
    # default passes at build time; a foreign value fails AND is scanned so
    # the report says what sits at the override target.
    real_override = os.environ.get("LUNARWING_REAL_OPENCODE", "")
    if real_override and pathlib.Path(real_override) != binary_path:
        fail(f"LUNARWING_REAL_OPENCODE={real_override!r} is set at build time — the "
             f"wrapper would exec that instead of {binary_path} (run-106 GJ-3b)")
        check_binary(pathlib.Path(real_override))
    # WI#425 GX-2: the entrypoint defaults OPENCODE_BIN to the wrapper, so an
    # ENV pointing it at anything but the wrapper path makes the bridge spawn
    # the real binary directly, bypassing every argv scan. Only the wrapper
    # path (or an unset value) passes at build time; a foreign value fails
    # AND is scanned so the report says what sits at the override target.
    opencode_bin_override = os.environ.get("OPENCODE_BIN", "")
    if opencode_bin_override and pathlib.Path(opencode_bin_override) != wrapper_path:
        fail(f"OPENCODE_BIN={opencode_bin_override!r} is set at build time — the "
             f"bridge would spawn that instead of the wrapper {wrapper_path} "
             "(WI#425 GX-2)")
        check_binary(pathlib.Path(opencode_bin_override))
    # WI#425: in the --image-root (out-of-band export) mode there is no live
    # PATH to resolve — the exported tree is not the running environment —
    # so the resolution check is the CI script's job (it checks the image
    # config Env instead). In-image builds keep the original check.
    if check_path_resolution:
        resolved = shutil.which("opencode")
        if resolved is None or pathlib.Path(resolved) != wrapper_path:
            fail(f"'opencode' on PATH must resolve to the wrapper {wrapper_path} "
                 f"(got {resolved!r}) — an earlier PATH dir shadows the guard")
            return
    # WI#425 GX-2: the real binary must be root-owned and not group/other
    # writable like the wrapper; every parent directory of both paths must
    # block entry replacement so the agent uid cannot swap either file
    # through a writable parent (rename/swap attacks).
    _check_real_binary_metadata("real binary", binary_path, owner_uid=owner_uid)
    _check_parent_directories("wrapper", wrapper_path, owner_uid=owner_uid)
    _check_parent_directories("real binary", binary_path, owner_uid=owner_uid)
    check_binary(binary_path)
    if not failures:
        check(f"installed wrapper {wrapper_path} is the exposure guard "
              "and the real binary carries no tunnel markers")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--lane",
        type=pathlib.Path,
        default=LANE_ROOT,
        help="worker lane root (default: this script's lane)",
    )
    parser.add_argument(
        "--binary",
        type=pathlib.Path,
        default=None,
        help="built opencode binary to scan for OpenTunnel markers (image builds)",
    )
    parser.add_argument(
        "--image-filesystem",
        action="store_true",
        help="verify installed policy-file ownership/modes (image builds only)",
    )
    parser.add_argument(
        "--image-root",
        type=pathlib.Path,
        default=None,
        help="root of an unpacked image filesystem (WI#425 out-of-band verification "
        "in CI): the installed wrapper and real binary are verified under this "
        "root instead of / — the BuildKit local export carries no /proc, so "
        "nothing about the in-image environment is trusted",
    )
    parser.add_argument(
        "--config",
        default=None,
        help="lane config filename (default: opencode2.json for the v2 lane, opencode.json otherwise)",
    )
    parser.add_argument(
        "--profile",
        choices=("opencode2", "opencode-v1"),
        default=None,
        help="lane profile (default: infer from the lane directory name; "
        "required when the lane is unpacked under a generic name like /app)",
    )
    args = parser.parse_args()

    lane = args.lane
    if not lane.is_dir():
        print(f"FAIL: lane root {lane} is not a directory", file=sys.stderr)
        return 1

    profile = args.profile or ("opencode2" if lane.name.startswith("opencode2") else "opencode-v1")
    is_v2 = profile == "opencode2"

    config_name = args.config or ("opencode2.json" if is_v2 else "opencode.json")
    check_config(lane, config_name, is_v2)
    check_entrypoint(lane, is_v2)
    if is_v2:
        check_wrapper(lane)
    if args.binary is not None:
        check_binary(args.binary)
    if args.image_filesystem:
        check_image_filesystem(lane)
        if is_v2:
            if args.image_root is not None:
                check_installed_wrapper(
                    lane,
                    args.image_root / "usr/local/bin/opencode",
                    args.image_root / "usr/local/lib/opencode2/opencode",
                    check_path_resolution=False,
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
