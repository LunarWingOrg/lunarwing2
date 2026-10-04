#!/usr/bin/env python3
"""Regression checks for the lockfile-only advisory guard (v2.1.5.0 item 3).

The guard exists for crates pinned in a committed Cargo.lock whose advisory
cargo-deny cannot see because the crate never enters the built graph (WI#255
pyo3 pattern). The v2.1.5.0 rework (MR !1371 review A2) fixed two false
premises: queries now run --workspace --all-features (the vendored
libsignal-protocol path deps are workspace members, so their dev-deps DO
resolve — atty is compiled for their tests), and an entry whose crate left
the lockfile is a stale-guard failure instead of a vacuous pass.

These tests drive the real script with stub git/taskset/cargo shims so both
branches of every contract stay pinned: present-in-lockfile + absent-from-
graph passes; present-in-graph fails; missing-from-lockfile fails.
"""

import os
import pathlib
import shutil
import stat
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".gitlab" / "scripts" / "check-lockfile-only-advisories.sh"

LOCK_TEMPLATE = """version = 4
[[package]]
name = "rootpkg"
version = "0.1.0"
{entries}"""

FAKE_GIT = """#!/usr/bin/env bash
if [[ "$*" == "rev-parse --show-toplevel" ]]; then
    echo "$FAKE_ROOT"
    exit 0
fi
echo "fake git: unexpected args: $*" >&2
exit 1
"""

FAKE_TASKSET = """#!/usr/bin/env bash
# Drop the "-c <list>" affinity arguments and exec the command verbatim:
# the guard's thread bound is irrelevant to what is being asserted.
shift 2
exec "$@"
"""

# Emulates `cargo tree` against a fixture graph. FAKE_REACHABLE lists crates
# visible in the full (dev-inclusive) graph; FAKE_REACHABLE_NO_DEV lists
# crates still visible with `-e no-dev`. Anything else prints cargo's real
# absent markers so the script's stdout inspection is what decides.
FAKE_CARGO = """#!/usr/bin/env bash
crate=""
no_dev=0
prev=""
for arg in "$@"; do
    if [[ "$prev" == "-i" ]]; then
        crate="$arg"
    fi
    if [[ "$arg" == "no-dev" ]]; then
        no_dev=1
    fi
    prev="$arg"
done
reachable="$FAKE_REACHABLE"
if ((no_dev)); then
    reachable="$FAKE_REACHABLE_NO_DEV"
fi
if [[ -n "$crate" && ",$reachable," == *",$crate,"* ]]; then
    echo "$crate v0.0.0"
    echo "└── fixture-parent v0.0.0"
    exit 0
fi
if [[ ",$FAKE_NO_MATCH," == *",$crate,"* ]]; then
    echo "error: package ID specification \\`$crate\\` did not match any packages" >&2
    exit 101
fi
echo "warning: nothing to print."
exit 0
"""


class LockfileOnlyAdvisoryGuardTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        script_dir = self.root / ".gitlab" / "scripts"
        script_dir.mkdir(parents=True)
        shutil.copy(SCRIPT, script_dir / SCRIPT.name)
        bridge = self.root / "ic" / "bridges" / "xmpp-bridge"
        bridge.mkdir(parents=True)
        for manifest in (self.root / "ic" / "Cargo.toml", bridge / "Cargo.toml"):
            manifest.write_text('[package]\nname = "fixture"\nversion = "0.1.0"\n', encoding="utf-8")
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        for name, text in (("git", FAKE_GIT), ("taskset", FAKE_TASKSET), ("cargo", FAKE_CARGO)):
            shim = bin_dir / name
            shim.write_text(text, encoding="utf-8")
            shim.chmod(shim.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    def write_locks(self, ic_entries, bridge_entries):
        (self.root / "ic" / "Cargo.lock").write_text(
            LOCK_TEMPLATE.format(entries=ic_entries), encoding="utf-8"
        )
        (self.root / "ic" / "bridges" / "xmpp-bridge" / "Cargo.lock").write_text(
            LOCK_TEMPLATE.format(entries=bridge_entries), encoding="utf-8"
        )

    def run_guard(self, reachable=(), reachable_no_dev=(), no_match=()):
        env = dict(os.environ)
        env["PATH"] = f"{self.root / 'bin'}{os.pathsep}{env['PATH']}"
        env["FAKE_ROOT"] = str(self.root)
        env["FAKE_REACHABLE"] = ",".join(reachable)
        env["FAKE_REACHABLE_NO_DEV"] = ",".join(reachable_no_dev)
        env["FAKE_NO_MATCH"] = ",".join(no_match)
        return subprocess.run(
            ["bash", str(self.root / ".gitlab" / "scripts" / SCRIPT.name)],
            capture_output=True,
            text=True,
            env=env,
            cwd=self.root,
            timeout=60,
        )

    def real_lock_pins(self):
        """Match the real tree: atty only in ic, pyo3 in both roots."""
        self.write_locks(
            '[[package]]\nname = "atty"\nversion = "0.2.14"\n'
            '[[package]]\nname = "pyo3"\nversion = "0.28.3"\n',
            '[[package]]\nname = "pyo3"\nversion = "0.28.3"\n',
        )

    def test_pinned_and_absent_from_all_graphs_passes(self):
        self.real_lock_pins()
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("lockfile-only advisory guard: ok", result.stdout)

    def test_dev_only_atty_passes(self):
        # The real v2.1.5.0 shape: atty resolves in the full workspace dev
        # graph (env_logger dev-dep of vendored libsignal-protocol) but the
        # no-dev production graph is clean. The guard must accept this.
        self.real_lock_pins()
        result = self.run_guard(reachable=("atty",))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_crate_in_production_graph_fails(self):
        self.real_lock_pins()
        result = self.run_guard(reachable=("atty",), reachable_no_dev=("atty",))
        self.assertEqual(result.returncode, 1)
        self.assertIn("atty is now reachable", result.stderr)
        self.assertIn("RUSTSEC-2021-0145", result.stderr)

    def test_absent_mode_crate_anywhere_in_graph_fails(self):
        # pyo3's mode is `absent`: even dev-only resolution must fail loudly,
        # because the hazard is the crate compiling at all (WI#255).
        self.real_lock_pins()
        result = self.run_guard(reachable=("pyo3",))
        self.assertEqual(result.returncode, 1)
        self.assertIn("pyo3 is now reachable", result.stderr)
        self.assertIn("RUSTSEC-2026-0176", result.stderr)

    def test_crate_missing_from_every_lockfile_is_stale_failure(self):
        # Misspelled, renamed, or removed pins must not pass vacuously on a
        # "did not match any packages" marker (MR !1371 review A2).
        self.write_locks("", "")
        result = self.run_guard(no_match=("atty", "pyo3"))
        self.assertEqual(result.returncode, 1)
        self.assertIn("stale", result.stderr)
        self.assertIn("atty", result.stderr)


if __name__ == "__main__":
    unittest.main()
