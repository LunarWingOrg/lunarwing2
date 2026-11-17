#!/usr/bin/env python3
"""WI#745: apply the reviewed managed tool-policy patch, fail closed.

A managed allow neutralizes a lower layer's tool denial, as upstream's other
policy actions already do. It preserves the ordinary permission decision:
a managed allow never turns an "ask" into a grant. Only upstream-identified
managed documents activate the override.
"""

from __future__ import annotations

import hashlib
import pathlib
import subprocess
import sys

PATCH = pathlib.Path(__file__).resolve().parent.parent / "patches" / "wi745-managed-policy-precedence.patch"
PATCH_DIGEST = "af75e9900b53123bb4b95e6f3bbb978a052cf7ac485eb5770732bec1d307a58c"
SOURCES = {
    "packages/core/src/config/plugin/policy.ts":
        "125e1002d859d8b38e41655e7e8cc158d8df9149ef957c51d7383d475570533b",
    "packages/core/src/managed-policy.ts":
        "ca523aad4cbd12d7dc2d46ddf2f47a3fb0c2023e0790e41e895df0fbf3241715",
    "packages/core/test/config/policy.test.ts":
        "c064d3027e099b0b886e37f454c7561c84edb4f567641be59a779ea673f54abd",
}


def _run(command: list[str], cwd: pathlib.Path) -> None:
    try:
        subprocess.run(
            command,
            cwd=cwd,
            stdin=subprocess.DEVNULL,
            text=True,
            check=True,
            timeout=300,
        )
    except FileNotFoundError as error:
        raise SystemExit(f"managed-policy verification requires {command[0]}") from error
    except subprocess.CalledProcessError as error:
        raise SystemExit(f"managed-policy verification failed: {error.stdout or error}") from error
    except subprocess.TimeoutExpired as error:
        raise SystemExit(f"managed-policy verification timed out: {' '.join(command)}") from error


def apply(root: pathlib.Path, *, run_tests: bool = True) -> None:
    _run(["git", "apply", "--check", str(PATCH)], root)
    _run(["git", "apply", str(PATCH)], root)
    result = subprocess.run(
        ["git", "status", "--porcelain", "--untracked-files=no"],
        cwd=root,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    expected = {f" M {name}" for name in SOURCES}
    if result.returncode != 0 or set(result.stdout.splitlines()) != expected:
        raise SystemExit(f"managed-policy patch changed an unexpected source set: {result.stdout} {result.stderr}")
    if run_tests:
        _run(["bun", "run", "script/test.ts", "test/config/policy.test.ts"], root / "packages/core")


def _verify_inputs(root: pathlib.Path) -> None:
    for relative, expected in SOURCES.items():
        try:
            actual = hashlib.sha256((root / relative).read_bytes()).hexdigest()
        except OSError as error:
            raise SystemExit(f"managed-policy patch input {relative} is missing: {error}") from error
        if actual != expected:
            raise SystemExit(
                f"managed-policy patch input {relative} has sha256 {actual}, expected {expected}; "
                "re-read the upstream policy precedence before changing the patch"
            )
    try:
        actual = hashlib.sha256(PATCH.read_bytes()).hexdigest()
    except OSError as error:
        raise SystemExit(f"managed-policy patch is missing: {error}") from error
    if actual != PATCH_DIGEST:
        raise SystemExit(f"managed-policy patch content changed: {actual}; review its tests and update both digests")


def main(root: pathlib.Path) -> None:
    _verify_inputs(root)
    apply(root)
    print("managed-policy precedence: reviewed upstream patch applied and its tests passed")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: apply_managed_policy_precedence.py <upstream repo root>")
    main(pathlib.Path(sys.argv[1]).resolve())
