#!/usr/bin/env python3
"""Validate every classified standalone deliverable from .gitlab/deliverables.json.

For each unit whose ``ci`` list names ``standalone-deliverables``, run a
locked metadata check inside the unit directory. A missing lockfile, a
lockfile out of sync with the manifest, or a stale classification fails the
job. Parsing is Python-only (no jq), so the job runs on the standard Rust
bootstrap image.

Exit codes: 0 = every selected unit verified, 1 = validation failure,
2 = manifest unreadable or structurally invalid.
"""

import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
MANIFEST = ROOT / ".gitlab" / "deliverables.json"
CARGO = ["cargo", "metadata", "--locked", "--format-version", "1"]
MAX_UNITS = 256


def load_manifest() -> tuple[dict, list[str]]:
    errors: list[str] = []
    if not MANIFEST.is_file():
        errors.append(".gitlab/deliverables.json missing")
        return {}, errors
    try:
        data = json.loads(MANIFEST.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        errors.append(f".gitlab/deliverables.json is not valid JSON: {error}")
        return {}, errors
    if data.get("schema") != 1:
        errors.append(".gitlab/deliverables.json schema must be 1")
        return {}, errors
    entries = data.get("deliverables")
    if not isinstance(entries, list) or not entries:
        errors.append(".gitlab/deliverables.json must contain a non-empty deliverables list")
        return {}, errors
    if len(entries) > MAX_UNITS:
        errors.append(f".gitlab/deliverables.json exceeds {MAX_UNITS} entries")
        return {}, errors
    return data, errors


def selected_units(data: dict) -> tuple[list[dict], list[str]]:
    errors: list[str] = []
    units: list[dict] = []
    seen: set[str] = set()
    for entry in data.get("deliverables", []):
        if not isinstance(entry, dict):
            errors.append("deliverable entry is not a JSON object")
            continue
        name = str(entry.get("name", "<unnamed>"))
        if name in seen:
            errors.append(f"duplicate deliverable name {name!r}")
            continue
        seen.add(name)
        ci = entry.get("ci")
        if not isinstance(ci, list):
            errors.append(f"deliverable {name!r} ci must be a list")
            continue
        if "standalone-deliverables" in ci:
            manifest_path = entry.get("manifest")
            if not isinstance(manifest_path, str) or not manifest_path:
                errors.append(f"deliverable {name!r} has no manifest path")
                continue
            units.append((name, manifest_path))
    return units, errors


def verify_unit(name: str, manifest_path: str) -> str | None:
    manifest = ROOT / manifest_path
    if not manifest.is_file():
        return f"{name}: manifest missing: {manifest_path}"
    try:
        subprocess.run(
            [*CARGO, "--manifest-path", str(manifest)],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
            timeout=900,
        )
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or error.stdout or "").strip().splitlines()
        tail = "\n".join(detail[-5:])
        return f"{name}: locked metadata failed: {tail}"
    except subprocess.TimeoutExpired:
        return f"{name}: locked metadata timed out"
    return None


def main() -> int:
    data, errors = load_manifest()
    units, unit_errors = selected_units(data)
    errors.extend(unit_errors)
    if errors:
        for error in errors:
            print(f"ERROR: {error}", file=sys.stderr)
        return 2
    if not units:
        print("ERROR: no deliverable selected standalone-deliverables validation", file=sys.stderr)
        return 1

    failures = 0
    for name, manifest_path in units:
        print(f"=== standalone deliverable: {name} ({manifest_path}) ===")
        failure = verify_unit(name, manifest_path)
        if failure:
            failures += 1
            print(f"ERROR: {failure}", file=sys.stderr)
        else:
            print("OK")
    if failures:
        print(f"ERROR: {failures} standalone deliverable(s) failed locked metadata", file=sys.stderr)
        return 1
    print(f"Standalone deliverables passed: {len(units)} unit(s), locked metadata verified.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())