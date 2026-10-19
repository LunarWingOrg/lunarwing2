#!/usr/bin/env python3
"""WI#363: unit tests for the ic/deny.toml accepted-advisory guards.

Offline half: validate_config.validate_deny_toml_advisories (ci-config lane)
requires every [advisories].ignore reason to name a "clearing owner WI#N" and
every cited repo-relative path to be tracked. Online half:
check_deny_toml_owners (the cargo-deny job's final script step) requires each
named owner to be an open issue-type work item — tested here with a stubbed
fetch, no network. Registered in .gitlab/scripts/python_tests.py as well, so
the suite also runs in python-unit.
"""

import importlib.util
import io
import json
import pathlib
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]


def load_module(name: str, relative: str):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


config = load_module("validate_config", ".gitlab/scripts/validate_config.py")
owners = load_module("check_deny_toml_owners", ".gitlab/scripts/check_deny_toml_owners.py")


class OwnerPatternDriftTests(unittest.TestCase):
    """The online script is stdlib-only (the .rust-job image has no PyYAML),
    so the owner regex is duplicated with a keep-in-sync note — pinned here."""

    def test_owner_patterns_match(self):
        self.assertEqual(config.DENY_OWNER_PATTERN.pattern, owners.OWNER_PATTERN.pattern)


class DenyTomlAdvisoryGuardTests(unittest.TestCase):
    """Offline half (WI#363 fix item 1): reason shape + cited-path existence."""

    TRACKED = {
        "ic/deny.toml",
        ".gitlab/README.md",
        "docs/ops/EXISTS.md",
        "ic/src/main.rs",
        ".gitlab/scripts/check-default-graph-advisories.sh",
    }
    DENY_OK = """[advisories]
unmaintained = "workspace"
# Audit trail: docs/ops/EXISTS.md and the module tree under ic/src/.
# Prose fragments like opencode/opencode2/pebble or 2026-0098/0099 are not paths.
ignore = [
    { id = "RUSTSEC-0000-0001", reason = "demo; clearing owner WI#363" },
    { id = "RUSTSEC-0000-0002", reason = "demo two; decision record WI#331 (closed), clearing owner WI#367" },
]
"""
    README_OK = (
        "# CI\n"
        "\n"
        f"{config.README_DENY_GUARD_BEGIN}\n"
        "- cargo-deny gate; see docs/ops/EXISTS.md and docs/ops/EXISTS.md:12-34.\n"
        f"{config.README_DENY_GUARD_END}\n"
        "\n"
        "- outside the markers, docs/ops/GONE.md is not checked.\n"
    )

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)

    def errors(self, deny_text: str | None, readme_text: str | None) -> list[str]:
        if deny_text is not None:
            deny_dir = self.root / "ic"
            deny_dir.mkdir(parents=True, exist_ok=True)
            (deny_dir / "deny.toml").write_text(deny_text, encoding="utf-8")
        if readme_text is not None:
            readme_dir = self.root / ".gitlab"
            readme_dir.mkdir(parents=True, exist_ok=True)
            (readme_dir / "README.md").write_text(readme_text, encoding="utf-8")
        errors: list[str] = []
        with mock.patch.object(config, "ROOT", self.root):
            config.validate_deny_toml_advisories(errors, tracked=set(self.TRACKED))
        return errors

    def ok(self) -> list[str]:
        return self.errors(self.DENY_OK, self.README_OK)

    def test_real_tree_passes(self):
        # The checked-in ic/deny.toml + .gitlab/README.md must satisfy the
        # guard (uses the real git ls-files, not a fixture set).
        errors: list[str] = []
        config.validate_deny_toml_advisories(errors)
        self.assertEqual(errors, [])

    def test_fixture_passes(self):
        self.assertEqual(self.ok(), [])

    def test_entry_without_reason_fails(self):
        deny = '[advisories]\nignore = [ { id = "RUSTSEC-0000-0001" } ]\n'
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(any("RUSTSEC-0000-0001 has no reason" in error for error in errors))

    def test_empty_reason_fails(self):
        deny = '[advisories]\nignore = [ { id = "RUSTSEC-0000-0001", reason = "  " } ]\n'
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(any("has no reason" in error for error in errors))

    def test_reason_without_wi_fails(self):
        deny = '[advisories]\nignore = [ { id = "RUSTSEC-0000-0001", reason = "no reference" } ]\n'
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(any("names no WI#N work item" in error for error in errors))

    def test_reason_with_wi_but_no_clearing_owner_fails(self):
        # A reason naming only a (closed) historical record has no owner the
        # online check can poll — refused offline (WI#363).
        deny = (
            '[advisories]\nignore = [ { id = "RUSTSEC-0000-0001", '
            'reason = "decision record WI#331" } ]\n'
        )
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(any("names no 'clearing owner WI#N'" in error for error in errors))

    def test_non_table_entry_fails(self):
        deny = '[advisories]\nignore = [ "RUSTSEC-0000-0001" ]\n'
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(any("entry 0 must be a table" in error for error in errors))

    def test_ignore_not_a_list_fails(self):
        deny = '[advisories]\nignore = "RUSTSEC-0000-0001"\n'
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(any("[advisories].ignore must be a list" in error for error in errors))

    def test_unparseable_toml_fails(self):
        errors = self.errors("not [ toml", self.README_OK)
        self.assertTrue(any("does not parse" in error for error in errors))

    def test_missing_deny_toml_fails(self):
        errors = self.errors(None, self.README_OK)
        self.assertTrue(any("cannot read ic/deny.toml" in error for error in errors))

    def test_missing_path_in_deny_comment_fails(self):
        deny = self.DENY_OK.replace("docs/ops/EXISTS.md", "docs/ops/GONE.md", 1)
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(
            any(
                "ic/deny.toml cites docs/ops/GONE.md" in error for error in errors
            )
        )

    def test_missing_path_in_readme_section_fails(self):
        readme = self.README_OK.replace("docs/ops/EXISTS.md:12-34", "docs/ops/GONE.md:12-34")
        errors = self.errors(self.DENY_OK, readme)
        self.assertTrue(
            any(
                ".gitlab/README.md cargo-deny section cites docs/ops/GONE.md" in error
                for error in errors
            )
        )

    def test_directory_citation_must_exist(self):
        deny = self.DENY_OK.replace("ic/src/", "ic/nope/")
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(any("ic/nope" in error for error in errors))

    def test_missing_readme_markers_fail(self):
        errors = self.errors(self.DENY_OK, "# CI\n- no markers here\n")
        self.assertTrue(any("marker pair" in error for error in errors))

    def test_prose_fragments_are_not_citations(self):
        # The fixture comments carry opencode/opencode2/pebble, 2026-0098/0099
        # and WI#331/WI#409-shaped fragments; none may be flagged.
        self.assertEqual(self.ok(), [])


class DenyTomlNoGitFallbackTests(unittest.TestCase):
    """The ci-config lint image (python:3.12-slim) ships no git binary, so
    `git ls-files` raises FileNotFoundError there (pipeline 5929, job
    117472). The guard must fall back to checkout existence, not crash."""

    def no_git(self):
        return mock.patch.object(
            config.subprocess, "run", side_effect=FileNotFoundError("git")
        )

    def test_real_tree_passes_without_git(self):
        errors: list[str] = []
        with self.no_git():
            config.validate_deny_toml_advisories(errors)
        self.assertEqual(errors, [])

    def test_missing_path_still_fails_without_git(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = pathlib.Path(temporary.name)
        (root / "docs" / "ops").mkdir(parents=True)
        (root / "docs" / "ops" / "EXISTS.md").write_text("x", encoding="utf-8")
        (root / "ic" / "src").mkdir(parents=True)
        (root / "ic" / "src" / "main.rs").write_text("", encoding="utf-8")
        (root / ".gitlab").mkdir()
        (root / ".gitlab" / "README.md").write_text(
            DenyTomlAdvisoryGuardTests.README_OK, encoding="utf-8"
        )
        deny = (
            DenyTomlAdvisoryGuardTests.DENY_OK
            + "# stale citation: docs/ops/GONE.md"
            + chr(10)
        )
        (root / "ic" / "deny.toml").write_text(deny, encoding="utf-8")
        errors: list[str] = []
        with mock.patch.object(config, "ROOT", root), self.no_git():
            config.validate_deny_toml_advisories(errors)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("docs/ops/GONE.md", errors[0])

    def test_citation_escaping_the_checkout_fails_without_git(self):
        # git mode refuses all three citations (none is a tracked path); the
        # fallback must not accept them just because the target exists
        # (run-157 KF-1).
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        outer = pathlib.Path(temporary.name)
        (outer / "outside.md").write_text("x", encoding="utf-8")
        root = outer / "repo"
        (root / "ic" / "src").mkdir(parents=True)
        (root / "ic" / "src" / "main.rs").write_text("", encoding="utf-8")
        (root / "docs" / "ops").mkdir(parents=True)
        (root / "docs" / "ops" / "EXISTS.md").write_text("x", encoding="utf-8")
        (root / "docs" / "escape").symlink_to(outer, target_is_directory=True)
        (root / ".gitlab").mkdir()
        (root / ".gitlab" / "README.md").write_text(
            DenyTomlAdvisoryGuardTests.README_OK, encoding="utf-8"
        )
        deny = (
            DenyTomlAdvisoryGuardTests.DENY_OK
            + "# traversal: ic/../../outside.md"
            + chr(10)
            + "# symlink: docs/escape/outside.md"
            + chr(10)
            + "# in-repo traversal: ic/../ic/src/main.rs"
            + chr(10)
        )
        (root / "ic" / "deny.toml").write_text(deny, encoding="utf-8")
        errors: list[str] = []
        with mock.patch.object(config, "ROOT", root), self.no_git():
            config.validate_deny_toml_advisories(errors)
        self.assertEqual(len(errors), 3, errors)
        joined = chr(10).join(errors)
        self.assertIn("ic/../../outside.md", joined)
        self.assertIn("docs/escape/outside.md", joined)
        self.assertIn("ic/../ic/src/main.rs", joined)

    def test_git_failure_is_an_error_not_a_fallback(self):
        # A `git ls-files` that fails while git is present (for example,
        # dubious ownership) is an error, not a silent fallback (run-157
        # KF-2, mutant M5).
        failed = config.subprocess.CompletedProcess(
            ["git", "ls-files"], 128, "", "fatal: not a git repository"
        )
        errors: list[str] = []
        with mock.patch.object(config.subprocess, "run", return_value=failed):
            config.validate_deny_toml_advisories(errors)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("git ls-files failed", errors[0])

    def test_citation_through_a_directory_symlink_fails_without_git(self):
        # git mode lists a directory symlink itself, never the paths beneath
        # it (e.g. ic/channels-src/darkirc -> ../../darkirc_channel_for_lunarwing/
        # darkirc), so the fallback must refuse such citations even when the
        # target resolves inside the checkout (run-161 KN-2). A symlink at
        # the final component stays accepted because git lists leaf symlinks.
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = pathlib.Path(temporary.name)
        (root / "ic" / "src").mkdir(parents=True)
        (root / "ic" / "src" / "main.rs").write_text("", encoding="utf-8")
        (root / "docs" / "ops").mkdir(parents=True)
        (root / "docs" / "ops" / "EXISTS.md").write_text("x", encoding="utf-8")
        (root / "ic" / "realdir").mkdir()
        (root / "ic" / "realdir" / "file.md").write_text("x", encoding="utf-8")
        (root / "ic" / "linkdir").symlink_to("realdir", target_is_directory=True)
        (root / "ic" / "realfile.md").write_text("x", encoding="utf-8")
        (root / "ic" / "leaf.md").symlink_to("realfile.md")
        (root / ".gitlab").mkdir()
        (root / ".gitlab" / "README.md").write_text(
            DenyTomlAdvisoryGuardTests.README_OK, encoding="utf-8"
        )
        deny = (
            DenyTomlAdvisoryGuardTests.DENY_OK
            + "# through a directory symlink: ic/linkdir/file.md"
            + chr(10)
            + "# leaf symlink: ic/leaf.md"
            + chr(10)
            + "# plain file: ic/realfile.md"
            + chr(10)
        )
        (root / "ic" / "deny.toml").write_text(deny, encoding="utf-8")
        errors: list[str] = []
        with mock.patch.object(config, "ROOT", root), self.no_git():
            config.validate_deny_toml_advisories(errors)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("ic/linkdir/file.md", errors[0])


class DenyTomlMainWiringTests(unittest.TestCase):
    """The ci-config lane runs validate_config.py's main(), so the offline
    guard only protects anything while main() calls it."""

    def test_main_runs_the_deny_toml_guard(self):
        calls = []

        def record(errors, tracked=None):
            calls.append(tracked)
            errors.append("WI#363 wiring probe")

        with (
            mock.patch.object(
                config, "validate_deny_toml_advisories", side_effect=record
            ),
            mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            self.assertEqual(config.main(), 1)
        self.assertEqual(calls, [None])
        self.assertIn("WI#363 wiring probe", stderr.getvalue())


class DenyTomlOwnerCheckTests(unittest.TestCase):
    """Online half (WI#363 fix item 2): every named owner must be open.

    fetch is stubbed; no network. The failure mode is deliberately a hard
    error (merging is not CI-gated, so the red gate buys visibility).
    """

    DENY = """[advisories]
ignore = [
    { id = "RUSTSEC-0000-0001", reason = "a; clearing owner WI#363" },
    { id = "RUSTSEC-0000-0002", reason = "b; clearing owner WI#363" },
    { id = "RUSTSEC-0000-0003", reason = "c; clearing owner WI#331, decision record WI#304" },
]
"""

    def collect(self, text: str = DENY):
        return owners.collect_owners(text)

    def test_collect_maps_each_advisory_to_its_owner(self):
        owner_map, missing = self.collect()
        self.assertEqual(missing, [])
        self.assertEqual(
            owner_map,
            {
                363: ["RUSTSEC-0000-0001", "RUSTSEC-0000-0002"],
                331: ["RUSTSEC-0000-0003"],
            },
        )

    def test_open_owners_pass_and_fetch_once_per_distinct_owner(self):
        calls = []

        def fetch(number):
            calls.append(number)
            return "opened"

        owner_map, missing = self.collect()
        self.assertEqual(owners.check_owners(owner_map, missing, fetch), [])
        self.assertEqual(sorted(calls), [331, 363])

    def test_closed_owner_fails_naming_wi_and_advisories(self):
        def fetch(number):
            return "closed" if number == 331 else "opened"

        owner_map, missing = self.collect()
        errors = owners.check_owners(owner_map, missing, fetch)
        self.assertEqual(len(errors), 1)
        self.assertIn("WI#331", errors[0])
        self.assertIn("'closed'", errors[0])
        self.assertIn("RUSTSEC-0000-0003", errors[0])

    def test_404_fails_with_task_type_message_and_is_not_retried(self):
        calls = []

        def fetch(number):
            calls.append(number)
            raise owners.OwnerNotFound(str(number))

        owner_map, missing = self.collect()
        errors = owners.check_owners(owner_map, missing, fetch, sleep=lambda seconds: None)
        self.assertEqual(len(errors), 2)
        for error in errors:
            self.assertIn("404", error)
            self.assertIn("issue-type", error)
        self.assertEqual(sorted(calls), [331, 363])

    def test_transient_failure_retries_then_recovers(self):
        attempts = []

        def fetch(number):
            attempts.append(number)
            if len(attempts) == 1:
                raise owners.OwnerLookupError("boom")
            return "opened"

        owner_map, missing = self.collect()
        errors = owners.check_owners(owner_map, missing, fetch, sleep=lambda seconds: None)
        self.assertEqual(errors, [])
        self.assertEqual(attempts, [331, 331, 363])

    def test_transient_failure_exhaustion_fails_closed(self):
        def fetch(number):
            raise owners.OwnerLookupError("boom")

        owner_map, missing = self.collect()
        errors = owners.check_owners(
            owner_map, missing, fetch, attempts=3, sleep=lambda seconds: None
        )
        self.assertEqual(len(errors), 2)
        for error in errors:
            self.assertIn("after 3 attempts", error)
            self.assertIn("Failing closed", error)

    def test_entry_without_owner_fails(self):
        deny = '[advisories]\nignore = [ { id = "RUSTSEC-0000-0009", reason = "WI#331" } ]\n'
        owner_map, missing = self.collect(deny)
        self.assertEqual(owner_map, {})
        self.assertEqual(missing, ["RUSTSEC-0000-0009"])
        errors = owners.check_owners(owner_map, missing, lambda number: "opened")
        self.assertEqual(len(errors), 1)
        self.assertIn("RUSTSEC-0000-0009", errors[0])
        self.assertIn("clearing owner WI#N", errors[0])

    def test_empty_ignore_list_has_nothing_to_check(self):
        owner_map, missing = self.collect("[advisories]\nignore = []\n")
        self.assertEqual((owner_map, missing), ({}, []))
        self.assertEqual(owners.check_owners(owner_map, missing, lambda number: "opened"), [])


class FetchIssueStateTests(unittest.TestCase):
    """The live fetch layer (urlopen stubbed): 404 is definitive — task-type
    work items 404 on the anonymous single-issue endpoint (WI#274 precedent),
    so it maps to OwnerNotFound and is never retried; 429/5xx and network
    errors map to the retried OwnerLookupError."""

    def stub_urlopen(self, behavior):
        return mock.patch.object(owners.urllib.request, "urlopen", behavior)

    def test_open_state_returned(self):
        response = mock.MagicMock()
        response.__enter__.return_value = response
        response.read.return_value = json.dumps({"state": "opened"}).encode()
        with self.stub_urlopen(mock.MagicMock(return_value=response)):
            self.assertEqual(owners.fetch_issue_state(363), "opened")

    def test_404_raises_owner_not_found(self):
        error = owners.urllib.error.HTTPError("url", 404, "Not Found", {}, None)
        with self.stub_urlopen(mock.MagicMock(side_effect=error)):
            with self.assertRaises(owners.OwnerNotFound):
                owners.fetch_issue_state(99999)

    def test_429_and_5xx_raise_transient_lookup_error(self):
        for status in (429, 500, 502, 503, 504):
            with self.subTest(status=status):
                error = owners.urllib.error.HTTPError("url", status, "x", {}, None)
                with self.stub_urlopen(mock.MagicMock(side_effect=error)):
                    with self.assertRaises(owners.OwnerLookupError):
                        owners.fetch_issue_state(363)

    def test_network_error_raises_transient_lookup_error(self):
        error = owners.urllib.error.URLError("no route to host")
        with self.stub_urlopen(mock.MagicMock(side_effect=error)):
            with self.assertRaises(owners.OwnerLookupError):
                owners.fetch_issue_state(363)


class DenyOwnerWiringTests(unittest.TestCase):
    """The cargo-deny job runs the owner check, and the exact-script pin in
    validate_config.py (ADVISORY_GATE_SCRIPTS, MR !1680 review IP-1) matches
    the checked-in job — the WI#363 'required fragments' wiring."""

    def test_cargo_deny_job_runs_the_owner_check_and_matches_the_pin(self):
        security = config.load_yaml(ROOT / ".gitlab" / "ci" / "security.yml")
        script = security["cargo-deny"]["script"]
        self.assertIn("python3 .gitlab/scripts/check_deny_toml_owners.py", script)
        self.assertEqual(script, config.ADVISORY_GATE_SCRIPTS["cargo-deny"])

    def test_owner_check_script_is_stdlib_only(self):
        # The .rust-job image installs bare python3 (bootstrap-rust.sh); the
        # script must not import PyYAML or other non-stdlib modules.
        text = (ROOT / ".gitlab" / "scripts" / "check_deny_toml_owners.py").read_text(
            encoding="utf-8"
        )
        self.assertNotIn("yaml", text)
        self.assertNotIn("import requests", text)


if __name__ == "__main__":
    unittest.main()
