#!/usr/bin/env python3
"""WI#363 (offline half, scope-narrowed 2026-10-20): unit tests for the
ic/deny.toml accepted-advisory guard.

validate_config.validate_deny_toml_advisories (ci-config lane) requires
every [advisories].ignore reason to name a "clearing owner" that is a
repo-relative document, and that document to list the advisory it owns
as the first cell of a table row
(WI#779 doc-only owner model: the four work-item owners the acceptances
cited — WI#367, WI#418, WI#749, WI#781 — were all closed with their pins
still locked). It also requires every cited repo-relative path to be
tracked, with a no-git checkout-existence fallback for the git-less
ci-config image. The online owner-liveness half of MR !1795 was dropped by
the scope decision: no owner-liveness job or network call exists, and the
doc-only owner form removes the liveness question instead of polling it.

WI#524 also pins the Wasmtime 36.x security floor and the advisory-fixed
cache/zstd selection in both affected lockfiles.
"""

import importlib.util
import io
import pathlib
import tempfile
import tomllib
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]


def load_module(name: str, relative: str):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


config = load_module("validate_config", ".gitlab/scripts/validate_config.py")


class WasmtimeSecurityFloorTests(unittest.TestCase):
    """WI#524: fix both graphs without undoing the advisory-fixed cache patch."""

    def test_policy_bans_vulnerable_36_x_not_the_fixed_release(self):
        policy = tomllib.loads((ROOT / "ic/deny.toml").read_text(encoding="utf-8"))
        self.assertIn(
            {"name": "wasmtime", "version": ">=36.0.0, <36.0.20"},
            policy["bans"]["deny"],
        )
        self.assertNotIn(
            "RUSTSEC-2026-0424",
            [entry["id"] for entry in policy["advisories"]["ignore"]],
        )

    def test_policy_bans_wasi_releases_below_the_sandbox_escape_fix(self):
        # RUSTSEC-2026-0587 names wasmtime-wasi and is fixed in 36.0.22.
        policy = tomllib.loads((ROOT / "ic/deny.toml").read_text(encoding="utf-8"))
        self.assertIn(
            {"name": "wasmtime-wasi", "version": ">=36.0.0, <36.0.22"},
            policy["bans"]["deny"],
        )
        self.assertNotIn(
            "RUSTSEC-2026-0587",
            [entry["id"] for entry in policy["advisories"]["ignore"]],
        )

    def test_both_lockfiles_keep_the_patched_cache_and_zstd_chain(self):
        for relative in ("ic/Cargo.lock", "ic/bridges/xmpp-bridge/Cargo.lock"):
            with self.subTest(lockfile=relative):
                packages = tomllib.loads(
                    (ROOT / relative).read_text(encoding="utf-8")
                )["package"]
                for name, floor in (
                    ("wasmtime", (36, 0, 20)),
                    ("wasmtime-wasi", (36, 0, 22)),
                    ("zstd", (0, 14, 3)),
                    ("zstd-safe", (8, 2, 0)),
                ):
                    selected = [p for p in packages if p["name"] == name]
                    self.assertTrue(selected, name)
                    for package in selected:
                        version = tuple(map(int, package["version"].split(".")))
                        self.assertGreaterEqual(version, floor, package)
                wasmtime = next(p for p in packages if p["name"] == "wasmtime")
                caches = [p for p in packages if p["name"] == "wasmtime-internal-cache"]
                self.assertEqual(len(caches), 1)
                self.assertEqual(caches[0]["version"], wasmtime["version"])
                self.assertNotIn("source", caches[0], "registry cache bypasses our patch")


class PostcardGrandparentPinTests(unittest.TestCase):
    """WI#788: the heapless acceptance is only as narrow as postcard's own
    [bans] pin. If a new crate takes postcard with its default features, it
    becomes a second heapless parent and inherits all nine acceptances with
    no lockfile change beyond the new parent itself — so the policy guard
    must fail offline until the new parent is reviewed and listed."""

    def policy(self) -> dict:
        return tomllib.loads((ROOT / "ic/deny.toml").read_text(encoding="utf-8"))

    def lockfile_parents(self) -> dict[str, set[str]]:
        parents: dict[str, set[str]] = {}
        for lock in sorted(ROOT.glob("ic/**/Cargo.lock")):
            relative = lock.relative_to(ROOT)
            if {"vendor", "target"} & set(relative.parts):
                continue
            packages = tomllib.loads(lock.read_text(encoding="utf-8")).get(
                "package", []
            )
            if not any(package.get("name") == "postcard" for package in packages):
                continue
            for package in packages:
                for dependency in package.get("dependencies", []):
                    if dependency.split()[0] == "postcard":
                        parents.setdefault(package["name"], set()).add(str(relative))
        return parents

    def test_postcard_ban_lists_exactly_the_parents_in_the_lockfiles(self):
        parents = self.lockfile_parents()
        self.assertTrue(parents, "no lockfile depends on postcard; drop the ban")
        entries = [
            entry
            for entry in self.policy()["bans"]["deny"]
            if entry.get("name") == "postcard"
        ]
        self.assertEqual(len(entries), 1, entries)
        self.assertEqual(entries[0]["version"], "<2.0.0")
        self.assertEqual(set(entries[0]["wrappers"]), set(parents), parents)

    def test_heapless_acceptance_stays_behind_both_pins(self):
        deny = self.policy()["bans"]["deny"]
        self.assertIn(
            {"name": "heapless", "version": "<0.9.4", "wrappers": ["postcard"]},
            deny,
        )
        postcard = [entry for entry in deny if entry.get("name") == "postcard"]
        self.assertEqual(len(postcard), 1, "heapless is one hop from postcard")
        self.assertIn("monty", postcard[0]["wrappers"], "the only default-feature parent")


class DenyTomlAdvisoryGuardTests(unittest.TestCase):
    """Offline half (WI#363 fix item 1): reason shape + cited-path existence."""

    TRACKED = {
        "ic/deny.toml",
        ".gitlab/README.md",
        "docs/ops/EXISTS.md",
        # Tracked but deliberately never written to disk: the unreadable
        # doc-only owner case. A citation check passes it (tracked), so the
        # owner-read check is what must fail.
        "docs/ops/UNWRITTEN.md",
        "ic/src/main.rs",
        ".gitlab/scripts/check-default-graph-advisories.sh",
    }
    # The doc-only clearing owner must name every advisory it owns
    # (WI#779). docs/ops/EXISTS.md stands in for the real standing record
    # docs/ops/v2.2.2.0_STANDING_ADVISORY_OWNERS.md in these fixtures.
    OWNER_DOC_TEXT = (
        "# Standing advisory ownership record\n"
        "\n"
        "| Advisory | Note |\n"
        "|---|---|\n"
        "| RUSTSEC-0000-0001 | demo |\n"
        "| RUSTSEC-0000-0002 | demo two |\n"
        "\n"
        "Superseded owners: | WI#331 | RUSTSEC-0000-0003 | closed |\n"
    )
    DENY_OK = """[advisories]
unmaintained = "workspace"
# Audit trail: docs/ops/EXISTS.md and the module tree under ic/src/.
# Prose fragments like opencode/opencode2/pebble or 2026-0098/0099 are not paths.
ignore = [
    { id = "RUSTSEC-0000-0001", reason = "demo; historical record WI#331; clearing owner docs/ops/EXISTS.md" },
    { id = "RUSTSEC-0000-0002", reason = "demo two; decision record WI#331 (closed), clearing owner docs/ops/EXISTS.md" },
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
        self.write_owner_doc(self.OWNER_DOC_TEXT)
        errors: list[str] = []
        with mock.patch.object(config, "ROOT", self.root):
            config.validate_deny_toml_advisories(errors, tracked=set(self.TRACKED))
        return errors

    def write_owner_doc(self, text: str) -> None:
        owner_dir = self.root / "docs" / "ops"
        owner_dir.mkdir(parents=True, exist_ok=True)
        (owner_dir / "EXISTS.md").write_text(text, encoding="utf-8")

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
        # A reason naming only a (closed) historical decision record names
        # no owner to re-verify the acceptance against — refused offline
        # (WI#363). Since WI#779 the owner must be a document, never a work
        # item: every work-item owner the acceptances cited was closed with
        # the pins still locked.
        deny = (
            '[advisories]\nignore = [ { id = "RUSTSEC-0000-0001", '
            'reason = "decision record WI#331" } ]\n'
        )
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(
            any("names no 'clearing owner <repo-relative>.md'" in error for error in errors)
        )

    def test_work_item_clearing_owner_fails(self):
        # The pre-WI#779 form is refused: a work item can close while its
        # acceptances stay standing, which is the defect WI#779 exists to
        # remove.
        deny = (
            '[advisories]\nignore = [ { id = "RUSTSEC-0000-0001", '
            'reason = "demo; clearing owner WI#363" } ]\n'
        )
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(
            any("names no 'clearing owner <repo-relative>.md'" in error for error in errors)
        )

    def test_historical_work_item_phrase_beside_document_owner_passes(self):
        # WI#820: the guard requires a document owner, not the absence of
        # work-item words. A historical `clearing owner WI#N` phrase kept
        # next to the document owner is allowed; the real RUSTSEC-2021-0145
        # reason records both forms.
        deny = (
            '[advisories]\nignore = [ { id = "RUSTSEC-0000-0001", '
            'reason = "demo; historical clearing owner WI#331 (closed); '
            'clearing owner docs/ops/EXISTS.md" } ]\n'
        )
        self.assertEqual(self.errors(deny, self.README_OK), [])

    def test_unreadable_owner_document_fails(self):
        # Tracked but absent from disk: the citation check passes, the owner
        # read must still fail so a deleted decision record is not a valid
        # owner.
        deny = (
            '[advisories]\nignore = [ { id = "RUSTSEC-0000-0001", '
            'reason = "demo; clearing owner docs/ops/UNWRITTEN.md" } ]\n'
        )
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(
            any(
                "clearing owner docs/ops/UNWRITTEN.md cannot be read" in error
                for error in errors
            ),
            errors,
        )

    def test_owner_document_must_name_the_advisory(self):
        # A document that is a valid tracked path but does not record the
        # advisory it owns is refused; otherwise a split or renamed record
        # could silently keep owning an acceptance.
        deny = (
            '[advisories]\nignore = [ { id = "RUSTSEC-0000-0003", '
            'reason = "demo; clearing owner docs/ops/EXISTS.md" } ]\n'
        )
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(
            any(
                "clearing owner docs/ops/EXISTS.md does not list RUSTSEC-0000-0003"
                in error
                for error in errors
            ),
            errors,
        )

    def test_advisory_in_a_later_table_cell_does_not_count(self):
        # WI#779 review AIO-1: the standing record's superseded-owner table
        # names IDs in full; such a row must not keep an acceptance owned
        # once its accepted-table row is gone.
        self.OWNER_DOC_TEXT = self.OWNER_DOC_TEXT + "| WI#781 | 2026-11-20 | RUSTSEC-0000-0004 | closed |\n"
        deny = (
            '[advisories]\nignore = [ { id = "RUSTSEC-0000-0004", '
            'reason = "demo; clearing owner docs/ops/EXISTS.md" } ]\n'
        )
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(
            any("does not list RUSTSEC-0000-0004 as the first cell" in error for error in errors),
            errors,
        )

    def test_owner_document_must_stay_inside_the_checkout(self):
        deny = (
            '[advisories]\nignore = [ { id = "RUSTSEC-0000-0001", '
            'reason = "demo; clearing owner docs/../outside.md" } ]\n'
        )
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(
            any(
                "clearing owner docs/../outside.md must resolve inside the checkout"
                in error
                for error in errors
            ),
            errors,
        )

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
        (root / "docs" / "ops" / "EXISTS.md").write_text(
            DenyTomlAdvisoryGuardTests.OWNER_DOC_TEXT, encoding="utf-8"
        )
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
        (root / "docs" / "ops" / "EXISTS.md").write_text(
            DenyTomlAdvisoryGuardTests.OWNER_DOC_TEXT, encoding="utf-8"
        )
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
        (root / "docs" / "ops" / "EXISTS.md").write_text(
            DenyTomlAdvisoryGuardTests.OWNER_DOC_TEXT, encoding="utf-8"
        )
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


if __name__ == "__main__":
    unittest.main()
