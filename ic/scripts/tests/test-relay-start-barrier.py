#!/usr/bin/env python3
"""Real-process tests for the relay launch/rotation exclusion boundary."""

import fcntl
import os
from pathlib import Path
import pwd
import shutil
import subprocess
import sys
import tempfile
import unittest


class RelayStartBarrierTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env_dir = self.root / "env"
        self.env_dir.mkdir(mode=0o700)
        self.launcher = self.root / "relay-exec.py"
        shutil.copyfile(Path(__file__).resolve().parents[1] / "lunarwing-relay-env-exec.py", self.launcher)
        self.launcher.chmod(0o755)
        self.lock = self.env_dir / ".relay-start.lock"
        self.lock.touch(mode=0o600)
        self.env_file = self.env_dir / "weechat.env"
        self.user_prefix = []
        self.uid = os.geteuid()
        if self.uid == 0:
            self.uid = pwd.getpwnam("nobody").pw_uid
            self.user_prefix = ["runuser", "-u", "nobody", "--"]
            self.root.chmod(0o755)
            os.chown(self.env_dir, self.uid, -1)
            os.chown(self.lock, self.uid, -1)
        self.write_password("fixture-old-password")

    def write_password(self, value):
        self.env_file.write_text(f"RELAY_PASSWORD={value}\n", encoding="utf-8")
        self.env_file.chmod(0o600)
        os.chown(self.env_file, self.uid, -1)

    def command(self, code="pass"):
        return [*self.user_prefix, sys.executable, str(self.launcher),
                "--env-file", str(self.env_file), "--", sys.executable, "-c", code]

    def run_guard(self, code="pass", **kwargs):
        return subprocess.run(self.command(code), capture_output=True, text=True,
                              timeout=5, **kwargs)

    def test_writer_blocks_external_start_without_waiting(self):
        with self.lock.open() as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            result = self.run_guard()
        self.assertEqual(result.returncode, 75)
        self.assertNotIn("fixture-old-password", result.stderr)
        self.assertEqual(self.run_guard().returncode, 0)

    def test_marker_survives_writer_exit_and_reboot(self):
        marker = self.env_dir / ".relay-rotation-blocked"
        marker.mkdir()
        self.assertEqual(self.run_guard().returncode, 75)
        marker.rmdir()
        # Broken symlink is also an inhibition, not an absent marker.
        marker.symlink_to("missing")
        self.assertEqual(self.run_guard().returncode, 75)

    def test_reload_overrides_stale_init_or_tmux_environment_without_argv(self):
        self.write_password("fixture-new-password")
        result = self.run_guard(
            "import os; from pathlib import Path; "
            "print(os.environ['RELAY_PASSWORD']); "
            "print(Path('/proc/self/cmdline').read_bytes())",
            env={**os.environ, "RELAY_PASSWORD": "fixture-old-password"})
        self.assertEqual(result.returncode, 0, result.stderr)
        value, argv = result.stdout.splitlines()
        self.assertEqual(value, "fixture-new-password")
        self.assertNotIn(value, argv)

    def test_literal_password_is_not_evaluated(self):
        self.write_password('"$(touch /must-not-run); `false`"')
        result = self.run_guard("import os; print(os.environ['RELAY_PASSWORD'])")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "$(touch /must-not-run); `false`")

    def test_missing_unsafe_or_duplicate_file_fails_closed(self):
        for mode in (0o644, 0o400):
            self.env_file.chmod(mode)
            self.assertEqual(self.run_guard().returncode, 75)
        self.env_file.chmod(0o600)
        with self.env_file.open("a") as out:
            out.write("RELAY_PASSWORD=second\n")
        self.assertEqual(self.run_guard().returncode, 75)
        self.write_password("valid")
        self.lock.unlink()
        self.assertEqual(self.run_guard().returncode, 75)
        self.lock.symlink_to(self.env_file)
        self.assertEqual(self.run_guard().returncode, 75)

    def test_admitted_reader_blocks_rotation_until_exec_and_does_not_leak_lock(self):
        # Pause the actual launcher inside password loading: the writer must
        # wait for this admitted start. No timing sleeps or production hooks.
        driver = self.root / "paused-reader.py"
        driver.write_text(
            "import importlib.util, sys\n"
            f"spec = importlib.util.spec_from_file_location('guard', {str(self.launcher)!r})\n"
            "guard = importlib.util.module_from_spec(spec)\n"
            "spec.loader.exec_module(guard)\n"
            "read = guard.relay_password\n"
            "def paused(path):\n"
            "    print('admitted', flush=True)\n"
            "    sys.stdin.readline()\n"
            "    return read(path)\n"
            "guard.relay_password = paused\n"
            "guard.main()\n", encoding="utf-8")
        driver.chmod(0o755)
        cmd = self.command(
            "import fcntl, os; "
            f"fd = os.open({str(self.lock)!r}, os.O_RDONLY); "
            "fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)")
        cmd[len(self.user_prefix) + 1] = str(driver)
        with subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True) as child:
            self.assertEqual(child.stdout.readline().strip(), "admitted")
            with self.lock.open() as writer:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(writer, fcntl.LOCK_EX | fcntl.LOCK_NB)
            _, stderr = child.communicate("continue\n", timeout=5)
            self.assertEqual(child.returncode, 0, stderr)


if __name__ == "__main__":
    unittest.main()
