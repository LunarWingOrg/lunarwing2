#!/usr/bin/env python3
"""IU-2: run the bootstrap with real hashing/extraction and local release fixtures."""

import hashlib
import io
import os
import pathlib
import re
import subprocess
import tarfile
import tempfile
import unittest

import yaml


ROOT = pathlib.Path(__file__).resolve().parents[2]
BOOTSTRAP = ROOT / ".gitlab/scripts/bootstrap-sccache.sh"
TRUSTED_BINARY = b'''#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FIXTURE/trusted-runs"
echo 'sccache 0.19.0'
'''
POISONED_BINARY = b'''#!/usr/bin/env bash
touch "$FIXTURE/payload-ran"
echo 'sccache 0.19.0'
'''


class SccacheBootstrapTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.project = self.root / "project with spaces"
        self.cached = self.project / ".ci-cache/bin/sccache"
        self.cached.parent.mkdir(parents=True)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.env = dict(
            os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
            FIXTURE=str(self.root), CI_PROJECT_DIR=str(self.project), TMPDIR=str(self.root),
        )
        self.env.pop("BASH_ENV", None)
        # The only intercepted command is the download. Hashing, tar, install
        # and execution use the real tools; no network is used by these tests.
        curl = self.bin / "curl"
        curl.write_text('''#!/usr/bin/env bash
set -euo pipefail
touch "$FIXTURE/downloaded"
if [[ "${DOWNLOAD_FAIL:-0}" == 1 ]]; then exit 22; fi
while [[ $# -gt 0 ]]; do
    if [[ "$1" == -o ]]; then
        cp "$FIXTURE/release.tar.gz" "$2"
        exit 0
    fi
    shift
done
exit 2
''')
        curl.chmod(0o755)
        self.release = self.root / "release.tar.gz"
        self.script = self.root / "bootstrap-sccache.sh"
        self.write_release(TRUSTED_BINARY)

    def write_release(self, binary):
        with tarfile.open(self.release, "w:gz") as archive:
            entry = tarfile.TarInfo("sccache-v0.19.0-x86_64-unknown-linux-musl/sccache")
            entry.size = len(binary)
            entry.mode = 0o755
            archive.addfile(entry, io.BytesIO(binary))
        # Substitute only the release pins in a scratch copy. The bootstrap's
        # control flow stays intact, including both integrity checks.
        source = BOOTSTRAP.read_text()
        for name, content in (
            ("SCCACHE_SHA256", self.release.read_bytes()),
            ("SCCACHE_BIN_SHA256", TRUSTED_BINARY),
        ):
            source, count = re.subn(
                rf'^{name}="[0-9a-f]{{64}}"$',
                f'{name}="{hashlib.sha256(content).hexdigest()}"', source, flags=re.M,
            )
            self.assertEqual(count, 1, f"missing {name} pin")
        self.script.write_text(source)

    def cache(self, binary, executable=True):
        self.cached.write_bytes(binary)
        self.cached.chmod(0o755 if executable else 0o644)

    def run_bootstrap(self, *args):
        return subprocess.run(
            ["bash", str(self.script), *args], env=self.env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10,
        )

    def assert_payload_not_run(self):
        self.assertFalse((self.root / "payload-ran").exists())

    def test_matching_version_payload_is_replaced_without_execution(self):
        self.cache(POISONED_BINARY)
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_payload_not_run()
        self.assertEqual(self.cached.read_bytes(), TRUSTED_BINARY)
        self.assertEqual((self.root / "trusted-runs").read_text(), "--version\n")

    def test_verified_cache_hit_neither_executes_nor_downloads(self):
        self.cache(TRUSTED_BINARY)
        self.env["DOWNLOAD_FAIL"] = "1"
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertFalse((self.root / "downloaded").exists())
        self.assertFalse((self.root / "trusted-runs").exists())

    def test_cold_install_verifies_and_executes_release(self):
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.cached.read_bytes(), TRUSTED_BINARY)
        self.assertTrue(os.access(self.cached, os.X_OK))
        self.assertEqual((self.root / "trusted-runs").read_text(), "--version\n")

    def test_nonexecutable_cache_entry_is_reinstalled(self):
        self.cache(TRUSTED_BINARY, executable=False)
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.root / "downloaded").exists())
        self.assertTrue(os.access(self.cached, os.X_OK))

    def test_download_failure_removes_payload_before_after_script(self):
        self.cache(POISONED_BINARY)
        self.env["DOWNLOAD_FAIL"] = "1"
        result = self.run_bootstrap()
        self.assertNotEqual(result.returncode, 0)
        self.assert_payload_not_run()
        self.assertFalse(self.cached.exists())

    def test_archive_checksum_failure_leaves_no_executable(self):
        self.cache(POISONED_BINARY)
        self.release.write_bytes(b"corrupt download")
        result = self.run_bootstrap()
        self.assertNotEqual(result.returncode, 0)
        self.assert_payload_not_run()
        self.assertFalse(self.cached.exists())
        self.assertFalse((self.root / "trusted-runs").exists())

    def test_wrong_binary_in_verified_archive_is_removed_without_execution(self):
        self.write_release(POISONED_BINARY)
        result = self.run_bootstrap()
        self.assertNotEqual(result.returncode, 0)
        self.assert_payload_not_run()
        self.assertFalse(self.cached.exists())

    def test_stats_runs_only_the_verified_binary_without_downloading(self):
        self.cache(TRUSTED_BINARY)
        result = self.run_bootstrap("--show-stats")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "trusted-runs").read_text(), "--show-stats\n")
        self.assertFalse((self.root / "downloaded").exists())

    def test_stats_rejects_payload_without_downloading(self):
        self.cache(POISONED_BINARY)
        result = self.run_bootstrap("--show-stats")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_payload_not_run()
        self.assertFalse(self.cached.exists())
        self.assertFalse((self.root / "downloaded").exists())

    def test_stats_skips_missing_binary_without_downloading(self):
        result = self.run_bootstrap("--show-stats")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.cached.exists())
        self.assertFalse((self.root / "downloaded").exists())

    def test_after_script_cannot_run_payload_when_earlier_bootstrap_fails(self):
        defaults = yaml.safe_load((ROOT / ".gitlab/ci/defaults.yml").read_text())
        for template in (".rust-job", ".rust-msrv-job"):
            for failed_step in ("df", "toolchain"):
                with self.subTest(template=template, failed_step=failed_step):
                    # No spaces here: the old unquoted after_script command
                    # must be able to execute its payload in the negative control.
                    project = self.root / f"runner-{template}-{failed_step}"
                    scripts = project / ".gitlab/scripts"
                    scripts.mkdir(parents=True)
                    (scripts / "bootstrap-sccache.sh").write_text(self.script.read_text())
                    for bootstrap in ("bootstrap-rust.sh", "bootstrap-msrv.sh"):
                        (scripts / bootstrap).write_text("#!/usr/bin/env bash\nexit 1\n")
                    cached = project / ".ci-cache/bin/sccache"
                    cached.parent.mkdir(parents=True)
                    cached.write_bytes(POISONED_BINARY)
                    cached.chmod(0o755)
                    env = dict(
                        self.env, CI_PROJECT_DIR=str(project), FIXTURE=str(project),
                        SCCACHE_DIR=str(project / ".ci-cache/sccache"),
                    )
                    before = list(defaults[template]["before_script"])
                    if failed_step == "df":
                        before[0] = "false"
                    result = subprocess.run(
                        ["bash", "-e", "-c", "\n".join(before)], cwd=project, env=env,
                        capture_output=True, text=True, timeout=10,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    # GitLab runs after_script in a fresh shell on script_failure.
                    result = subprocess.run(
                        ["bash", "-e", "-c", "\n".join(defaults[template]["after_script"])],
                        cwd=project, env=env, capture_output=True, text=True, timeout=10,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertFalse((project / "payload-ran").exists())
                    self.assertFalse((project / "downloaded").exists())
                    self.assertFalse(cached.exists())


if __name__ == "__main__":
    unittest.main()
