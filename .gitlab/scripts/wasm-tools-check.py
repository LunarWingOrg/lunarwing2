#!/usr/bin/env python3
"""Compile, test and lint every production WASM tool crate (WI#809).

The crate list is not written here. It is the set of production deliverables
whose ``ci`` list names this job (``wasm-tools-check``), resolved through
``ic/registry/tools/*.json`` to the source directory a tenant build compiles
(``ic/scripts/build-wasm-extensions.sh``). ``--list`` prints that plan; the
default mode runs, for each tool:

* ``cargo build --locked --release --target wasm32-wasip2`` — what ships,
* ``cargo test --locked`` on the host,
* ``cargo clippy --locked --all-targets -- -D warnings`` on the host.

A tool that is classified production but names no compiling job fails
``ci-config`` (``validate_config.validate_tool_compile_coverage``), so a new
tool cannot silently fall out of this run.

Exit codes: 0 = every crate passed, 1 = at least one cargo command failed,
2 = the plan itself is invalid (bad manifest, unknown registry directory).
"""

import argparse
import json
import os
import pathlib
import shlex
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
IC = ROOT / "ic"
DELIVERABLES = ROOT / ".gitlab" / "deliverables.json"
REGISTRY_TOOLS = IC / "registry" / "tools"
CARGO_CI = ROOT / ".gitlab" / "scripts" / "cargo-ci.sh"
JOB_NAME = "wasm-tools-check"
DEFAULT_TARGET_DIR = ROOT / ".ci-cache" / "wasm-tools-target"


class PlanError(Exception):
    """The job's crate list cannot be resolved safely."""


def load_deliverables() -> list[dict]:
    try:
        data = json.loads(DELIVERABLES.read_text(encoding="utf-8"))
    except OSError as error:
        raise PlanError(f"cannot read {DELIVERABLES}: {error}") from error
    except json.JSONDecodeError as error:
        raise PlanError(f"{DELIVERABLES} is not valid JSON: {error}") from error
    entries = data.get("deliverables")
    if not isinstance(entries, list) or not entries:
        raise PlanError(f"{DELIVERABLES} has no deliverables list")
    return [entry for entry in entries if isinstance(entry, dict)]


def registry_sources() -> dict[pathlib.Path, dict]:
    """Map each registry tool's resolved source directory to its metadata.

    Tenant builds compile ``source.dir`` relative to ``ic/`` (see
    ``ic/scripts/build-wasm-extensions.sh``); resolution follows the tracked
    ``ic/tools-src/gotify`` symlink so the job builds the copy that ships.
    """
    if not REGISTRY_TOOLS.is_dir():
        raise PlanError(f"registry directory missing: {REGISTRY_TOOLS}")
    sources: dict[pathlib.Path, dict] = {}
    for path in sorted(REGISTRY_TOOLS.glob("*.json")):
        try:
            entry = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise PlanError(f"{path} is not valid JSON: {error}") from error
        source = entry.get("source")
        if not isinstance(source, dict):
            raise PlanError(f"{path} has no source object")
        directory = source.get("dir")
        crate = source.get("crate_name")
        if not isinstance(directory, str) or not directory:
            raise PlanError(f"{path} has no source.dir")
        if not isinstance(crate, str) or not crate:
            raise PlanError(f"{path} has no source.crate_name")
        resolved = (IC / directory).resolve()
        if not resolved.is_dir():
            raise PlanError(f"{path} source.dir {directory!r} does not exist under ic/")
        if resolved in sources:
            raise PlanError(
                f"{path} and {sources[resolved]['registry_path']} share source dir {directory!r}"
            )
        sources[resolved] = {
            "registry_name": entry.get("name") or path.stem,
            "registry_path": path,
            "source_dir": directory,
            "crate": crate,
        }
    if not sources:
        raise PlanError(f"no tool entries found in {REGISTRY_TOOLS}")
    return sources


def load_plan(job: str = JOB_NAME) -> list[dict]:
    """Resolve every deliverable that names ``job`` to the directory it builds."""
    entries = load_deliverables()
    selected = [entry for entry in entries if job in (entry.get("ci") or [])]
    if not selected:
        raise PlanError(f"no deliverable names the {job!r} CI job")
    sources = registry_sources()
    plan: list[dict] = []
    seen: dict[pathlib.Path, str] = {}
    for entry in selected:
        name = entry.get("name")
        if entry.get("status") != "production":
            raise PlanError(f"deliverable {name!r} names {job!r} but is not production")
        manifest = entry.get("manifest")
        if not isinstance(manifest, str) or not manifest:
            raise PlanError(f"deliverable {name!r} has no manifest")
        manifest_path = ROOT / manifest
        if not manifest_path.is_file():
            raise PlanError(f"deliverable {name!r} manifest {manifest!r} does not exist")
        directory = manifest_path.resolve().parent
        source = sources.get(directory)
        if source is None:
            raise PlanError(
                f"deliverable {name!r} manifest {manifest!r} is not a registry tool source "
                "directory, so a tenant build would not compile it"
            )
        if directory in seen:
            raise PlanError(f"deliverables {seen[directory]!r} and {name!r} build {directory}")
        seen[directory] = name
        plan.append(
            {
                "deliverable": name,
                "registry": source["registry_name"],
                "source_dir": source["source_dir"],
                "crate": source["crate"],
                "manifest": f"{source['source_dir']}/Cargo.toml",
            }
        )
    return plan


def cargo_commands(unit: dict) -> list[tuple[str, list[str]]]:
    manifest = unit["manifest"]
    return [
        (
            "build wasm32-wasip2",
            [
                "bash", str(CARGO_CI), "build", "--locked", "--release",
                "--target", "wasm32-wasip2", "--manifest-path", manifest,
            ],
        ),
        (
            "test host",
            ["bash", str(CARGO_CI), "test", "--locked", "--manifest-path", manifest],
        ),
        (
            "clippy host -D warnings",
            [
                "bash", str(CARGO_CI), "clippy", "--locked", "--all-targets",
                "--manifest-path", manifest, "--", "-D", "warnings",
            ],
        ),
    ]


def print_plan(plan: list[dict], target_dir: pathlib.Path) -> None:
    print(f"WASM tool crates selected by .gitlab/deliverables.json ci: [{JOB_NAME}]")
    for unit in plan:
        print(
            f"  {unit['deliverable']}: registry {unit['registry']} -> "
            f"ic/{unit['source_dir']} ({unit['crate']})"
        )
    print(f"Shared target directory: {target_dir}")


def run_crate(unit: dict, env: dict[str, str]) -> list[str]:
    failures: list[str] = []
    print(f"=== {unit['deliverable']}: {unit['crate']} from ic/{unit['source_dir']} ===", flush=True)
    for label, command in cargo_commands(unit):
        rendered = " ".join(shlex.quote(part) for part in command)
        print(f"--- {label}: {rendered}", flush=True)
        result = subprocess.run(command, cwd=ROOT, env=env, check=False)
        if result.returncode != 0:
            failures.append(f"{unit['deliverable']}: {label} failed ({result.returncode})")
            break
    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--list", action="store_true", help="print the plan and exit")
    parser.add_argument(
        "--target-dir",
        type=pathlib.Path,
        default=None,
        help=f"shared CARGO_TARGET_DIR (default: {DEFAULT_TARGET_DIR})",
    )
    args = parser.parse_args(argv)
    try:
        plan = load_plan()
    except PlanError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 2

    target_dir = args.target_dir or pathlib.Path(
        os.environ.get("CARGO_TARGET_DIR") or DEFAULT_TARGET_DIR
    )
    print_plan(plan, target_dir)
    if args.list:
        return 0

    if "wasm32-wasip2" not in subprocess.run(
        ["rustup", "target", "list", "--installed"],
        capture_output=True,
        text=True,
        check=False,
    ).stdout.split():
        print(
            "ERROR: wasm32-wasip2 is not installed for the active toolchain; run "
            "`rustup target add --toolchain 1.96.1 wasm32-wasip2`",
            file=sys.stderr,
        )
        return 2

    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(target_dir)
    failures: list[str] = []
    for unit in plan:
        failures.extend(run_crate(unit, env))

    if failures:
        print(f"ERROR: {len(failures)} WASM tool check(s) failed:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1
    print(f"All {len(plan)} production WASM tool crate(s) built, tested and linted clean.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
