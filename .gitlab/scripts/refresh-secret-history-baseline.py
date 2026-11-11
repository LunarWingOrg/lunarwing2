#!/usr/bin/env python3

import argparse
import dataclasses
import hashlib
import json
import os
import pathlib
import re
import shlex
import subprocess
import sys
import tempfile


COMMIT = re.compile(r"^[0-9a-f]{40}$")
CACHE_FORMAT = 1
HEADER = """# WI#590 / WI#575: reviewed Gitleaks v8.30.1 historical findings.
# Exact commit:path:rule:line fingerprints only; never add path/rule-only pins.
# Refresh from a non-shallow full mirror (branches, tags and merge-request refs)
# with .gitlab/scripts/refresh-secret-history-baseline.py.
"""
DRAFT_HEADER = """# Draft secret-history acceptance file; review every row before use.
# Delete every fingerprint whose disposition is not recorded in
# .gitlab/gitleaks-history-dispositions.md, then pass this file to --accept.
# No pending findings are included. Never add secret values or matched content.
"""


@dataclasses.dataclass(frozen=True)
class RefreshPlan:
    pinned: list[str]
    accepted_vendor: set[str]
    accepted: set[str]
    unclassified: set[str]
    pending: set[str]
    refused: set[str]
    unmatched_accepted: set[str]


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


def fingerprint_rule(fingerprint: str) -> str:
    return fingerprint.rsplit(":", 2)[1]


def vendor_crate(fingerprint: str) -> str:
    parts = fingerprint_path(fingerprint).split("/")
    if len(parts) < 3 or parts[:2] != ["ic", "vendor"]:
        raise ValueError("vendor fingerprint has no crate component")
    return parts[2]


def vendor_groups(fingerprints: set[str]) -> dict[tuple[str, str], list[str]]:
    groups = {}
    for fingerprint in fingerprints:
        if fingerprint_path(fingerprint).startswith("ic/vendor/"):
            key = (vendor_crate(fingerprint), fingerprint_rule(fingerprint))
            groups.setdefault(key, []).append(fingerprint)
    return groups


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
    approved = (candidates - blocked) & accepted
    accepted_vendor = {
        fingerprint
        for fingerprint in approved
        if fingerprint_path(fingerprint).startswith("ic/vendor/")
    }
    return RefreshPlan(
        pinned=sorted(existing | approved),
        accepted_vendor=accepted_vendor,
        accepted=approved - accepted_vendor,
        unclassified=candidates - blocked - accepted,
        pending=blocked,
        refused=blocked & accepted,
        unmatched_accepted=accepted - set(scanned),
    )


def write_text_atomic(output: pathlib.Path, content: str, mode: int) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(dir=output.parent, text=True)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            stream.write(content)
        os.chmod(temporary, mode)
        os.replace(temporary, output)
    except BaseException:
        os.unlink(temporary)
        raise


def fingerprint_file_content(header: str, pinned: list[str]) -> str:
    return header + "".join(f"{fingerprint}\n" for fingerprint in pinned)


def write_baseline(output: pathlib.Path, pinned: list[str]) -> None:
    write_text_atomic(output, fingerprint_file_content(HEADER, pinned), 0o644)


def write_draft(output: pathlib.Path, unclassified: list[str]) -> None:
    content = fingerprint_file_content(DRAFT_HEADER, unclassified)
    write_text_atomic(output, content, 0o600)


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


def mirror_refs_key(repo: pathlib.Path) -> str:
    refs = git(repo, "for-each-ref", "--format=%(refname) %(objectname)")
    head = git(repo, "rev-parse", "HEAD")
    return hashlib.sha256(f"HEAD {head}\n{refs}\n".encode()).hexdigest()


def default_cache_path(repo: pathlib.Path) -> pathlib.Path:
    git_dir = pathlib.Path(git(repo, "rev-parse", "--absolute-git-dir"))
    if git(repo, "rev-parse", "--is-bare-repository") == "true":
        return git_dir.parent / f"{git_dir.name}.secret-history-cache.json"
    return git_dir / "secret-history-baseline-cache.json"


def load_scan_cache(
    path: pathlib.Path,
    refs_key: str,
    command: list[str],
    version: str,
) -> list[str] | None:
    if not path.is_file():
        return None
    try:
        cached = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(cached, dict):
            raise ValueError("cache is not an object")
        expected = (CACHE_FORMAT, refs_key, command, version)
        actual = (
            cached.get("format"),
            cached.get("refs_sha256"),
            cached.get("command"),
            cached.get("gitleaks"),
        )
        if actual != expected:
            return None
        saved = cached.get("fingerprints")
        if not isinstance(saved, list) or not all(isinstance(row, str) for row in saved):
            raise ValueError("cache fingerprints are not a string list")
        return sorted({validate_fingerprint(row) for row in saved})
    except (OSError, TypeError, ValueError):
        print(f"ignoring invalid secret-history scan cache: {path}", file=sys.stderr)
        return None


def save_scan_cache(
    path: pathlib.Path,
    refs_key: str,
    command: list[str],
    version: str,
    scanned: list[str],
) -> None:
    cached = {
        "format": CACHE_FORMAT,
        "refs_sha256": refs_key,
        "command": command,
        "gitleaks": version,
        "fingerprints": scanned,
    }
    content = json.dumps(cached, indent=2, sort_keys=True) + "\n"
    write_text_atomic(path, content, 0o600)


def scan_with_cache(
    repo: pathlib.Path,
    command: list[str],
    version: str,
    cache_path: pathlib.Path,
) -> list[str]:
    refs_key = mirror_refs_key(repo)
    cached = load_scan_cache(cache_path, refs_key, command, version)
    if cached is not None:
        print(f"reusing {len(cached)} redacted fingerprints from unchanged mirror refs")
        print(f"secret-history scan cache: {cache_path}")
        return cached
    scanned = scan(repo, command)
    if mirror_refs_key(repo) != refs_key:
        raise ValueError("mirror refs changed during the history scan; rerun the helper")
    save_scan_cache(cache_path, refs_key, command, version, scanned)
    print(f"saved {len(scanned)} redacted fingerprints to scan cache: {cache_path}")
    return scanned


def print_vendor_groups(fingerprints: set[str], label: str, stream) -> None:
    groups = vendor_groups(fingerprints)
    if not groups:
        return
    count = sum(len(group) for group in groups.values())
    print(f"{label} ({count}):", file=stream)
    for (crate, rule), group in sorted(groups.items()):
        print(f"  {crate} / {rule} ({len(group)}):", file=stream)
        for fingerprint in sorted(group):
            print(f"    {fingerprint}", file=stream)


def print_refresh(plan: RefreshPlan, output: pathlib.Path) -> None:
    print(f"wrote {len(plan.pinned)} exact historical fingerprints to {output}")
    print_vendor_groups(plan.accepted_vendor, "accepted classified vendor fingerprints", sys.stdout)
    print_vendor_groups(
        plan.unclassified,
        "unclassified vendor fingerprints not pinned",
        sys.stderr,
    )
    for fingerprint in sorted(plan.accepted):
        print(f"accepted classified non-vendor fingerprint: {fingerprint}")
    for fingerprint in sorted(plan.unclassified):
        if not fingerprint_path(fingerprint).startswith("ic/vendor/"):
            print(f"unclassified non-vendor fingerprint not pinned: {fingerprint}", file=sys.stderr)
    for fingerprint in sorted(plan.pending - plan.refused):
        print(f"pending fingerprint left unpinned: {fingerprint}", file=sys.stderr)
    for fingerprint in sorted(plan.refused):
        print(f"pending fingerprint refused although accepted: {fingerprint}", file=sys.stderr)
    for fingerprint in sorted(plan.unmatched_accepted):
        print(f"warning: accepted fingerprint matched no scanned row: {fingerprint}", file=sys.stderr)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=pathlib.Path, default=pathlib.Path.cwd())
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--accept", type=pathlib.Path)
    parser.add_argument("--draft-accept", type=pathlib.Path)
    parser.add_argument("--pending", type=pathlib.Path)
    cache = parser.add_mutually_exclusive_group()
    cache.add_argument("--cache", type=pathlib.Path, help="override the scan cache path")
    cache.add_argument("--no-cache", action="store_true", help="force a fresh scan without caching")
    parser.add_argument(
        "--gitleaks",
        default=os.environ.get("GITLEAKS_BIN", "gitleaks"),
        help="scanner command, split like a shell command line",
    )
    args = parser.parse_args()
    repo = args.repo.resolve()
    output = args.output or repo / ".gitlab/gitleaks-history-ignore"
    pending_path = args.pending or repo / ".gitlab/gitleaks-history-pending"
    validate_mirror(repo)
    cache_path = None
    if not args.no_cache:
        cache_path = args.cache or default_cache_path(repo)
    named_paths = {
        "--output": output,
        "--pending": pending_path,
        "--accept": args.accept,
        "--draft-accept": args.draft_accept,
        "--cache": cache_path,
    }
    seen = {}
    for name, path in named_paths.items():
        if path is None:
            continue
        resolved = path.resolve()
        if resolved in seen:
            raise ValueError(f"{name} must name a different file from {seen[resolved]}")
        seen[resolved] = name

    existing = read_fingerprint_file(output)
    pending = read_fingerprint_file(pending_path)
    accepted = read_fingerprint_file(args.accept) if args.accept else set()
    command = shlex.split(args.gitleaks)
    version = subprocess.run(
        [*command, "version"], check=True, capture_output=True, text=True
    ).stdout.strip()
    if version != "v8.30.1":
        raise ValueError("baseline generation requires Gitleaks v8.30.1")

    scanned = scan(repo, command) if cache_path is None else scan_with_cache(
        repo, command, version, cache_path
    )
    plan = plan_refresh(existing, scanned, accepted, pending)
    write_baseline(output, plan.pinned)
    if args.draft_accept:
        write_draft(args.draft_accept, sorted(plan.unclassified))
        print(f"wrote {len(plan.unclassified)} unclassified fingerprints to draft: {args.draft_accept}")
    print_refresh(plan, output)
    # A pending row that nobody tried to accept is the known, tracked state:
    # reporting it must not hide whether anything new turned up.
    return 1 if plan.unclassified or plan.refused else 0


if __name__ == "__main__":
    raise SystemExit(main())
