#!/usr/bin/env python3

import argparse
import dataclasses
import json
import os
import pathlib
import re
import shlex
import subprocess
import sys
import tempfile


COMMIT = re.compile(r"^[0-9a-f]{40}$")
HEADER = """# WI#590 / WI#575: reviewed Gitleaks v8.30.1 historical findings.
# Exact commit:path:rule:line fingerprints only; never add path/rule-only pins.
# Refresh from a non-shallow full mirror (branches, tags and merge-request refs)
# with .gitlab/scripts/refresh-secret-history-baseline.py.
"""


@dataclasses.dataclass(frozen=True)
class RefreshPlan:
    pinned: list[str]
    vendor: set[str]
    accepted: set[str]
    unclassified: set[str]
    pending: set[str]


def git(repo: pathlib.Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def validate_mirror(repo: pathlib.Path) -> None:
    if git(repo, "rev-parse", "--is-shallow-repository") == "true":
        raise ValueError("baseline generation requires a non-shallow repository")
    refs = git(repo, "for-each-ref", "--format=%(refname)").splitlines()
    if not any(ref.startswith(("refs/heads/", "refs/remotes/")) for ref in refs):
        raise ValueError("baseline generation requires branch refs")
    if not any(ref.startswith("refs/tags/") for ref in refs):
        raise ValueError("baseline generation requires tag refs")
    if not any(ref.startswith("refs/merge-requests/") for ref in refs):
        raise ValueError("baseline generation requires merge-request refs")


def validate_fingerprint(fingerprint: str) -> str:
    commit, separator, remainder = fingerprint.partition(":")
    if not separator or not COMMIT.fullmatch(commit):
        raise ValueError("finding lacks a commit-bound fingerprint")
    path, rule, line = remainder.rsplit(":", 2)
    if not path or path.startswith("/") or not rule or not line.isdigit():
        raise ValueError("finding has an invalid fingerprint")
    return fingerprint


def fingerprint_path(fingerprint: str) -> str:
    return fingerprint.split(":", 1)[1].rsplit(":", 2)[0]


def vendor_crate(fingerprint: str) -> str:
    parts = fingerprint_path(fingerprint).split("/")
    if len(parts) < 3 or parts[:2] != ["ic", "vendor"]:
        raise ValueError("vendor fingerprint has no crate component")
    return parts[2]


def read_fingerprint_file(path: pathlib.Path) -> set[str]:
    if not path.is_file():
        raise ValueError(f"fingerprint file is missing: {path}")
    pinned = set()
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        entry = raw_line.strip()
        if not entry or entry.startswith("#"):
            continue
        pinned.add(validate_fingerprint(entry))
    return pinned


def fingerprints(report_path: pathlib.Path) -> list[str]:
    findings = json.loads(report_path.read_text(encoding="utf-8"))
    if not isinstance(findings, list):
        raise ValueError("Gitleaks report is not a finding list")
    pinned = set()
    for finding in findings:
        if finding.get("Secret") != "REDACTED":
            raise ValueError("Gitleaks report contains an unredacted secret")
        pinned.add(validate_fingerprint(finding.get("Fingerprint", "")))
    return sorted(pinned)


def plan_refresh(
    existing: set[str],
    scanned: list[str],
    accepted: set[str],
    pending: set[str],
) -> RefreshPlan:
    if existing & pending:
        raise ValueError("a pending fingerprint is already present in the baseline")
    candidates = set(scanned) - existing
    blocked = candidates & pending
    vendor = {
        fingerprint
        for fingerprint in candidates - blocked
        if fingerprint_path(fingerprint).startswith("ic/vendor/")
    }
    non_vendor = candidates - blocked - vendor
    approved = non_vendor & accepted
    unclassified = non_vendor - accepted
    return RefreshPlan(
        pinned=sorted(existing | vendor | approved),
        vendor=vendor,
        accepted=approved,
        unclassified=unclassified,
        pending=blocked,
    )


def write_baseline(output: pathlib.Path, pinned: list[str]) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(dir=output.parent, text=True)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            stream.write(HEADER)
            for fingerprint in pinned:
                stream.write(f"{fingerprint}\n")
        os.chmod(temporary, 0o644)
        os.replace(temporary, output)
    except BaseException:
        os.unlink(temporary)
        raise


def scan(repo: pathlib.Path, command: list[str]) -> list[str]:
    with tempfile.TemporaryDirectory() as temporary:
        scan_root = pathlib.Path(temporary) / "scan"
        scan_root.mkdir()
        git_dir = git(repo, "rev-parse", "--absolute-git-dir")
        (scan_root / ".git").write_text(f"gitdir: {git_dir}\n", encoding="utf-8")
        empty_ignore = pathlib.Path(temporary) / "empty.ignore"
        empty_ignore.touch()
        report = pathlib.Path(temporary) / "report.json"
        result = subprocess.run(
            [
                *command,
                "git",
                "--redact=100",
                "--no-banner",
                "--no-color",
                "--report-format",
                "json",
                "--report-path",
                str(report),
                "--exit-code=0",
                "--gitleaks-ignore-path",
                str(empty_ignore),
                "--log-opts=--all",
                ".",
            ],
            cwd=scan_root,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0 or not report.is_file():
            raise ValueError(f"redacted Gitleaks scan failed with status {result.returncode}")
        return fingerprints(report)


def print_refresh(plan: RefreshPlan, output: pathlib.Path) -> None:
    print(f"wrote {len(plan.pinned)} exact historical fingerprints to {output}")
    for crate in sorted({vendor_crate(fingerprint) for fingerprint in plan.vendor}):
        print(f"new vendor fingerprints for {crate}:")
        for fingerprint in sorted(
            fingerprint for fingerprint in plan.vendor if vendor_crate(fingerprint) == crate
        ):
            print(f"  {fingerprint}")
    for fingerprint in sorted(plan.accepted):
        print(f"accepted classified non-vendor fingerprint: {fingerprint}")
    for fingerprint in sorted(plan.unclassified):
        print(f"unclassified non-vendor fingerprint not pinned: {fingerprint}", file=sys.stderr)
    for fingerprint in sorted(plan.pending):
        print(f"pending fingerprint refused even if accepted: {fingerprint}", file=sys.stderr)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=pathlib.Path, default=pathlib.Path.cwd())
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--accept", type=pathlib.Path)
    parser.add_argument("--pending", type=pathlib.Path)
    parser.add_argument(
        "--gitleaks",
        default=os.environ.get("GITLEAKS_BIN", "gitleaks"),
        help="scanner command, split like a shell command line",
    )
    args = parser.parse_args()
    repo = args.repo.resolve()
    output = args.output or repo / ".gitlab/gitleaks-history-ignore"
    pending_path = args.pending or repo / ".gitlab/gitleaks-history-pending"
    existing = read_fingerprint_file(output)
    pending = read_fingerprint_file(pending_path)
    accepted = read_fingerprint_file(args.accept) if args.accept else set()
    command = shlex.split(args.gitleaks)
    validate_mirror(repo)
    version = subprocess.run(
        [*command, "version"], check=True, capture_output=True, text=True
    ).stdout.strip()
    if version != "v8.30.1":
        raise ValueError("baseline generation requires Gitleaks v8.30.1")

    plan = plan_refresh(existing, scan(repo, command), accepted, pending)
    write_baseline(output, plan.pinned)
    print_refresh(plan, output)
    return 1 if plan.unclassified or plan.pending else 0


if __name__ == "__main__":
    raise SystemExit(main())
