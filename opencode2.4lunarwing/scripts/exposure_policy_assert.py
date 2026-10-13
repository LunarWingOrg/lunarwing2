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
     hostname, and never invokes service mode (controls 2 + 4).
  5. scripts/opencode-exposure-guard.sh exists and refuses the service/pair
     subcommands (control 2, wrapper layer).
  6. --binary PATH: the built opencode binary contains no OpenTunnel client
     markers (controls 5 + 8).

v1 lane (--lane opencode4lunarwing): only the share-disabled and
entrypoint-loopback/password checks apply (v1.18.25 has no service mode or
tunnel; pairing is a v2 surface).
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
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
    if 'OPENCODE_SERVE_HOST="${OPENCODE_SERVE_HOST:-127.0.0.1}"' in text:
        check("entrypoint.sh defaults the server bind to loopback")
    else:
        fail(f"{entrypoint} lost its loopback OPENCODE_SERVE_HOST default")
    if re.search(r'"?\$OPENCODE_BIN"?\s+serve\s', code) and "--hostname" in code:
        check("entrypoint.sh starts the server with an explicit --hostname")
    else:
        fail(f"{entrypoint} must start the server via serve with --hostname")
    if is_v2:
        # v2 mandates Basic auth on serve; v1.18.25's serve has no auth
        # support, so the password invariant is v2-only.
        if "OPENCODE_PASSWORD" in text:
            check("entrypoint.sh sets a server password")
        else:
            fail(f"{entrypoint} must set OPENCODE_PASSWORD before serve")


def check_wrapper(lane: pathlib.Path) -> None:
    wrapper = lane / "scripts" / "opencode-exposure-guard.sh"
    try:
        text = wrapper.read_text(encoding="utf-8")
    except OSError:
        fail(f"{wrapper} is missing — the binary wrapper is the control-2 layer that "
             "permission rules cannot cover (bash -c, absolute paths)")
        return
    for subcommand in ("service", "pair"):
        if re.search(rf"^\s*{subcommand}\)", text, re.MULTILINE):
            check(f"wrapper refuses 'opencode {subcommand}'")
        else:
            fail(f"{wrapper} must refuse the '{subcommand}' subcommand")
    if "is_loopback" in text and "--hostname" in text:
        check("wrapper enforces loopback-only serve")
    else:
        fail(f"{wrapper} must refuse non-loopback serve --hostname")


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

    if failures:
        print(f"exposure policy: {len(failures)} check(s) FAILED for {lane}", file=sys.stderr)
        return 1
    print(f"exposure policy: all checks passed for {lane} (profile {profile})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
