#!/usr/bin/env python3
"""Regression checks for the default-graph advisory guard (WI#331, v2.1.6.0).

The guard pins the WI#331 mitigation: libsql's remote/replication/tls surface
lives behind the non-default `libsql-remote-replica` feature, so the legacy
chain (rustls-webpki 0.102.8, h2 0.3.27) must never resolve in a
default-feature production graph. The ID-keyed cargo-deny ignores cannot see
that regression because every deny gate queries --workspace --all-features
(MR !1513 review CZ-1).

These tests drive the real script with stub git/taskset/cargo shims so all
three contract branches stay pinned: pinned-in-lock + absent-from-default-
graph passes; reachable-in-default-graph fails; pin-missing-from-lock fails
as stale instead of passing vacuously.
"""

import os
import pathlib
import shutil
import stat
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".gitlab" / "scripts" / "check-default-graph-advisories.sh"

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

# Emulates `cargo tree --locked -e normal --target all -i crate@version`.
# FAKE_REACHABLE lists crate@version pins visible in the default-feature
# graph; anything else prints cargo's real absent marker so the script's
# stdout inspection is what decides.
FAKE_CARGO = """#!/usr/bin/env bash
pin=""
prev=""
for arg in "$@"; do
    if [[ "$prev" == "-i" ]]; then
        pin="$arg"
    fi
    prev="$arg"
done
if [[ -n "$pin" && ",$FAKE_REACHABLE," == *",$pin,"* ]]; then
    echo "$pin v0.0.0"
    echo "└── fixture-parent v0.0.0"
    exit 0
fi
echo "error: package ID specification \\`$pin\\` did not match any packages" >&2
exit 101
"""


class DefaultGraphAdvisoryGuardTests(unittest.TestCase):
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

    def write_ic_lock(self, entries):
        (self.root / "ic" / "Cargo.lock").write_text(
            LOCK_TEMPLATE.format(entries=entries), encoding="utf-8"
        )

    def run_guard(self, reachable=()):
        env = dict(os.environ)
        env["PATH"] = f"{self.root / 'bin'}{os.pathsep}{env['PATH']}"
        env["FAKE_ROOT"] = str(self.root)
        env["FAKE_REACHABLE"] = ",".join(reachable)
        return subprocess.run(
            ["bash", str(self.root / ".gitlab" / "scripts" / SCRIPT.name)],
            capture_output=True,
            text=True,
            env=env,
            cwd=self.root,
            timeout=60,
        )

    def real_ic_lock_pins(self):
        """Match the real tree: both legacy pins live in ic/Cargo.lock."""
        self.write_ic_lock(
            '[[package]]\nname = "h2"\nversion = "0.3.27"\n'
            '[[package]]\nname = "h2"\nversion = "0.4.20"\n'
            '[[package]]\nname = "rustls-webpki"\nversion = "0.102.8"\n'
            '[[package]]\nname = "rustls-webpki"\nversion = "0.103.15"\n'
        )

    def test_pinned_and_absent_from_default_graph_passes(self):
        self.real_ic_lock_pins()
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("default-graph advisory guard: ok", result.stdout)

    def test_legacy_chain_in_default_graph_fails(self):
        # The CZ-1 regression: something re-enables libsql's remote surface
        # (or a new hyper-rustls 0.25 dependent appears) and the chain is
        # back in default-feature builds.
        self.real_ic_lock_pins()
        result = self.run_guard(reachable=("rustls-webpki@0.102.8",))
        self.assertEqual(result.returncode, 1)
        self.assertIn("rustls-webpki@0.102.8 is reachable", result.stderr)
        self.assertIn("RUSTSEC-2026-0049/0098/0099/0104", result.stderr)

    def test_h2_legacy_in_default_graph_fails(self):
        self.real_ic_lock_pins()
        result = self.run_guard(reachable=("h2@0.3.27",))
        self.assertEqual(result.returncode, 1)
        self.assertIn("h2@0.3.27 is reachable", result.stderr)
        self.assertIn("RUSTSEC-2026-0258", result.stderr)

    def test_pin_missing_from_lock_is_stale_failure(self):
        # If the chain is ever really cleared from ic/Cargo.lock, the guard
        # must fail loudly so the entry and the deny.toml ignores are removed
        # together — a vacuous pass would hide the drift.
        self.write_ic_lock("")
        result = self.run_guard()
        self.assertEqual(result.returncode, 1)
        self.assertIn("stale", result.stderr)
        self.assertIn("h2@0.3.27", result.stderr)


if __name__ == "__main__":
    unittest.main()
