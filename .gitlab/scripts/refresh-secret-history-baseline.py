#!/usr/bin/env python3

import argparse
import json
import os
import pathlib
import re
import shlex
import subprocess
import tempfile


COMMIT = re.compile(r"^[0-9a-f]{40}$")
HEADER = """# WI#590 / WI#575: reviewed Gitleaks v8.30.1 historical findings.
# Exact commit:path:rule:line fingerprints only; never add path/rule-only pins.
# Refresh from a non-shallow full mirror (branches, tags and merge-request refs)
# with .gitlab/scripts/refresh-secret-history-baseline.py.
"""


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


def fingerprints(report_path: pathlib.Path) -> list[str]:
    findings = json.loads(report_path.read_text(encoding="utf-8"))
    if not isinstance(findings, list):
        raise ValueError("Gitleaks report is not a finding list")
    pinned = set()
    for finding in findings:
        if finding.get("Secret") != "REDACTED":
            raise ValueError("Gitleaks report contains an unredacted secret")
        fingerprint = finding.get("Fingerprint", "")
        commit, separator, remainder = fingerprint.partition(":")
        if not separator or not COMMIT.fullmatch(commit):
            raise ValueError("finding lacks a commit-bound fingerprint")
        path, rule, line = remainder.rsplit(":", 2)
        if not path or path.startswith("/") or not rule or not line.isdigit():
            raise ValueError("finding has an invalid fingerprint")
        pinned.add(fingerprint)
    return sorted(pinned)


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


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=pathlib.Path, default=pathlib.Path.cwd())
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument(
        "--gitleaks",
        default=os.environ.get("GITLEAKS_BIN", "gitleaks"),
        help="scanner command, split like a shell command line",
    )
    args = parser.parse_args()
    repo = args.repo.resolve()
    output = args.output or repo / ".gitlab/gitleaks-history-ignore"
    command = shlex.split(args.gitleaks)
    validate_mirror(repo)
    version = subprocess.run(
        [*command, "version"], check=True, capture_output=True, text=True
    ).stdout.strip()
    if version != "v8.30.1":
        raise ValueError("baseline generation requires Gitleaks v8.30.1")

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
        pinned = fingerprints(report)
    write_baseline(output, pinned)
    print(f"wrote {len(pinned)} exact historical fingerprints to {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
