#!/usr/bin/env python3
"""Exercise the real CI bootstrap with a minimal-image Rustup/proxy fixture."""

import os
import pathlib
import subprocess
import tempfile
import textwrap
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]


class RustBootstrapTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}", FIXTURE=str(self.root))
        # No package management, host file deletion, toolchain installation, or
        # backoff sleeps escape this fixture. Rustup proxies exist but don't
        # imply that the corresponding component is installed.
        for name in ("apt-get", "rm"):
            self.script(name, "exit 0\n")
        for name in ("rustfmt", "cargo-clippy"):
            self.script(name, "exit 1\n")
        self.script("rustc", 'echo "rustc ${FIXTURE_RUST_VERSION:-1.96.1} (fixture)"\n')
        self.script("sleep", 'echo "$*" >> "$FIXTURE/sleeps"\n')
        self.script("rustup", textwrap.dedent("""\
            import os
            import pathlib
            import sys

            root = pathlib.Path(os.environ["FIXTURE"])
            args = sys.argv[1:]
            with (root / "calls").open("a", encoding="utf-8") as log:
                log.write(" ".join(args) + "\\n")
            if args[:2] == ["run", "1.96.1"] and args[-1:] == ["--version"]:
                component = {"rustfmt": "rustfmt", "cargo-clippy": "clippy"}[args[2]]
                sys.exit(0 if (root / component).exists() else 1)
            if args[:4] != ["component", "add", "--toolchain", "1.96.1"]:
                sys.exit("unexpected rustup command")
            component = args[4]
            remaining = root / "failures"
            count = int(remaining.read_text()) if remaining.exists() else 0
            if count:
                remaining.write_text(str(count - 1))
                sys.exit("Temporary failure in name resolution")
            if not (root / "broken-install").exists():
                (root / component).touch()
            """), python=True)

    def script(self, name, content, *, python=False):
        path = self.bin / name
        interpreter = "python3" if python else "bash"
        path.write_text(f"#!/usr/bin/env {interpreter}\n{content}", encoding="utf-8")
        path.chmod(0o755)

    def run_bootstrap(self):
        return subprocess.run(
            ["bash", str(ROOT / ".gitlab/scripts/bootstrap-rust.sh")],
            cwd=ROOT, env=self.env, text=True, capture_output=True, timeout=15,
        )

    def lines(self, name):
        path = self.root / name
        return path.read_text(encoding="utf-8").splitlines() if path.exists() else []

    def installs(self):
        return [call for call in self.lines("calls") if call.startswith("component add")]

    def test_minimal_image_proxies_do_not_skip_missing_components(self):
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.installs(), [
            "component add --toolchain 1.96.1 rustfmt",
            "component add --toolchain 1.96.1 clippy",
        ])
        self.assertTrue((self.root / "rustfmt").exists())
        self.assertTrue((self.root / "clippy").exists())

    def test_installed_components_need_no_download(self):
        for component in ("rustfmt", "clippy"):
            (self.root / component).touch()
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.installs(), [])
        self.assertEqual(self.lines("calls"), [
            "run 1.96.1 rustfmt --version", "run 1.96.1 cargo-clippy --version",
        ])

    def test_only_missing_component_is_installed(self):
        (self.root / "rustfmt").touch()
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.installs(), ["component add --toolchain 1.96.1 clippy"])

    def test_transient_dns_failure_recovers_with_bounded_backoff(self):
        (self.root / "failures").write_text("2", encoding="utf-8")
        (self.root / "clippy").touch()
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.installs(), ["component add --toolchain 1.96.1 rustfmt"] * 3)
        self.assertEqual(self.lines("sleeps"), ["5", "10"])

    def test_persistent_failure_stops_before_cargo_instead_of_passing(self):
        (self.root / "failures").write_text("100", encoding="utf-8")
        result = self.run_bootstrap()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("after 3 attempts", result.stderr)
        self.assertEqual(self.installs(), ["component add --toolchain 1.96.1 rustfmt"] * 3)
        self.assertEqual(self.lines("sleeps"), ["5", "10"])

    def test_successful_install_must_make_component_usable(self):
        (self.root / "broken-install").touch()
        result = self.run_bootstrap()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("after 3 attempts", result.stderr)
        self.assertEqual(len(self.installs()), 3)

    def test_wrong_image_version_is_still_rejected(self):
        self.env["FIXTURE_RUST_VERSION"] = "1.95.0"
        result = self.run_bootstrap()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("expected rustc 1.96.1", result.stderr)
        self.assertEqual(self.lines("calls"), [])


if __name__ == "__main__":
    unittest.main()
