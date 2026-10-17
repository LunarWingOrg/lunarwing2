#!/usr/bin/env python3
"""Regression checks for the default-graph advisory guard (WI#331, v2.1.6.0).

The guard pins the WI#331 mitigation: libsql's remote/replication/tls surface
lives behind the non-default `libsql-remote-replica` feature, so the legacy
chain (rustls 0.22.4, rustls-webpki 0.102.8, h2 0.3.27) must never resolve in a
default-feature production graph. The ID-keyed cargo-deny ignores cannot see
that regression because every deny gate queries --workspace --all-features
(MR !1513 review CZ-1).

These tests drive the real script with stub git/taskset/cargo shims: safe
resolved graphs pass, unsafe graphs and failed resolution fail closed, and
missing pins fail as stale. Both roots must use their vendored, locked graph.
WI#418 also pins the shipped build commands: a clean default graph cannot
detect a build explicitly enabling the opt-in chain.
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
SHIPPED_BUILDS = (
    "ic/Dockerfile",
    "ic/Dockerfile.worker",
    "ic/scripts/mt-admin-lib/05-tenant-build.sh",
    "scripts/build-lunarwing.sh",
)

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

# Emulates the forward default graph from `cargo tree`.
# FAKE_REACHABLE lists crate@version pins visible in the default-feature
# graph. The default graph includes newer versions of the guarded names,
# ensuring the script checks exact pins instead of rejecting safe versions.
FAKE_CARGO = """#!/usr/bin/env bash
printf '%s|%s\\n' "$PWD" "$*" >> "$FAKE_ROOT/cargo-calls"
if [[ -n "$FAKE_ERROR" ]]; then
    echo "$FAKE_ERROR" >&2
    exit 101
fi
echo "fixture v0.1.0 ($PWD)"
echo "h2 v0.4.20"
echo "rustls v0.23.46"
echo "rustls-webpki v0.103.15"
if [[ -z "$FAKE_REACHABLE_ROOT" || "$PWD" == "$FAKE_ROOT/$FAKE_REACHABLE_ROOT" ]]; then
    IFS=, read -ra pins <<< "$FAKE_REACHABLE"
    for pin in "${pins[@]}"; do
        echo "${pin%@*} v${pin#*@}"
    done
fi
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
        for build in SHIPPED_BUILDS:
            path = self.root / build
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("cargo build --release --bin lunarwing\n", encoding="utf-8")

    def write_ic_lock(self, entries):
        (self.root / "ic" / "Cargo.lock").write_text(
            LOCK_TEMPLATE.format(entries=entries), encoding="utf-8"
        )

    def run_guard(self, reachable=(), error="", reachable_root=""):
        env = dict(os.environ)
        env["PATH"] = f"{self.root / 'bin'}{os.pathsep}{env['PATH']}"
        env["FAKE_ROOT"] = str(self.root)
        env["FAKE_REACHABLE"] = ",".join(reachable)
        env["FAKE_ERROR"] = error
        env["FAKE_REACHABLE_ROOT"] = reachable_root
        return subprocess.run(
            ["bash", str(self.root / ".gitlab" / "scripts" / SCRIPT.name)],
            capture_output=True,
            text=True,
            env=env,
            cwd=self.root,
            timeout=60,
        )

    def real_ic_lock_pins(self):
        """Match the real tree: all three legacy pins live in ic/Cargo.lock."""
        self.write_ic_lock(
            '[[package]]\nname = "h2"\nversion = "0.3.27"\n'
            '[[package]]\nname = "h2"\nversion = "0.4.20"\n'
            '[[package]]\nname = "rustls"\nversion = "0.22.4"\n'
            '[[package]]\nname = "rustls"\nversion = "0.23.46"\n'
            '[[package]]\nname = "rustls-webpki"\nversion = "0.102.8"\n'
            '[[package]]\nname = "rustls-webpki"\nversion = "0.103.15"\n'
        )

    def test_pinned_and_absent_from_default_graph_passes(self):
        self.real_ic_lock_pins()
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("default-graph advisory guard: ok", result.stdout)

    def test_each_root_uses_locked_default_graph_and_own_cargo_config(self):
        self.real_ic_lock_pins()
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = (self.root / "cargo-calls").read_text().splitlines()
        self.assertEqual(len(calls), 2)
        for call, root in zip(calls, ("ic", "ic/bridges/xmpp-bridge")):
            cwd, args = call.split("|", 1)
            self.assertEqual(cwd, str(self.root / root))
            self.assertIn("--locked", args)
            self.assertNotIn("--offline", args)  # Pinned git sources need fetching on cold CI caches.
            self.assertIn("-e normal --target all", args)
            self.assertIn(f"--manifest-path {cwd}/Cargo.toml", args)
            self.assertNotIn("--all-features", args)
            self.assertNotIn("--no-default-features", args)

    def test_shipped_build_enabling_the_opt_in_feature_fails(self):
        # IS-2: both default graphs remain clean while a shipped build opts
        # in explicitly, either by feature name or via --all-features.
        self.real_ic_lock_pins()
        for build in SHIPPED_BUILDS:
            for flag in ("--features libsql-remote-replica", "--all-features"):
                with self.subTest(build=build, flag=flag):
                    path = self.root / build
                    clean = path.read_text(encoding="utf-8")
                    try:
                        path.write_text(
                            f"cargo build --release --bin lunarwing {flag}\n", encoding="utf-8"
                        )
                        result = self.run_guard()
                    finally:
                        path.write_text(clean, encoding="utf-8")
                    self.assertEqual(result.returncode, 1, result.stderr)
                    self.assertIn(build, result.stderr)
                    self.assertIn("WI#418 condition 1", result.stderr)

    def test_missing_shipped_build_path_fails(self):
        self.real_ic_lock_pins()
        for build in SHIPPED_BUILDS:
            with self.subTest(build=build):
                path = self.root / build
                clean = path.read_text(encoding="utf-8")
                try:
                    path.unlink()
                    result = self.run_guard()
                finally:
                    path.write_text(clean, encoding="utf-8")
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(build, result.stderr)
                self.assertIn("condition-1 list", result.stderr)

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

    def test_rustls_legacy_in_either_default_graph_fails(self):
        self.real_ic_lock_pins()
        for root in ("ic", "ic/bridges/xmpp-bridge"):
            with self.subTest(root=root):
                result = self.run_guard(reachable=("rustls@0.22.4",), reachable_root=root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("rustls@0.22.4 is reachable", result.stderr)
                self.assertIn("RUSTSEC-2026-0346/0349", result.stderr)
                self.assertIn(f"{root}'s DEFAULT-feature", result.stderr)

    def test_rustls_legacy_pin_missing_from_lock_is_stale_failure(self):
        self.real_ic_lock_pins()
        lock = self.root / "ic" / "Cargo.lock"
        lock.write_text(
            lock.read_text(encoding="utf-8").replace(
                '[[package]]\nname = "rustls"\nversion = "0.22.4"\n', ""
            ),
            encoding="utf-8",
        )
        result = self.run_guard()
        self.assertEqual(result.returncode, 1)
        self.assertIn("rustls@0.22.4' is stale", result.stderr)

    def test_pin_missing_from_lock_is_stale_failure(self):
        # If the chain is ever really cleared from ic/Cargo.lock, the guard
        # must fail loudly so the entry and the deny.toml ignores are removed
        # together — a vacuous pass would hide the drift.
        self.write_ic_lock("")
        result = self.run_guard()
        self.assertEqual(result.returncode, 1)
        self.assertIn("stale", result.stderr)
        self.assertIn("h2@0.3.27", result.stderr)

    def test_bridge_only_regression_fails(self):
        self.real_ic_lock_pins()
        result = self.run_guard(reachable=("h2@0.3.27",), reachable_root="ic/bridges/xmpp-bridge")
        self.assertEqual(result.returncode, 1)
        self.assertIn("h2@0.3.27 is reachable", result.stderr)
        self.assertIn("xmpp-bridge's DEFAULT-feature", result.stderr)

    def test_resolver_failure_is_not_an_absent_package(self):
        self.real_ic_lock_pins()
        result = self.run_guard(error="error: lock file needs to be updated but --locked was passed")
        self.assertEqual(result.returncode, 1)
        self.assertIn("failed to inspect", result.stderr)
        self.assertIn("lock file needs to be updated", result.stderr)
        self.assertNotIn("guard: ok", result.stdout)

    def test_error_containing_absence_marker_still_fails(self):
        self.real_ic_lock_pins()
        result = self.run_guard(error="error: failed to resolve graph: nothing to print")
        self.assertEqual(result.returncode, 1)
        self.assertIn("failed to inspect", result.stderr)


if __name__ == "__main__":
    unittest.main()
