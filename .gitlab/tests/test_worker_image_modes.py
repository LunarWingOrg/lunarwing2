"""Execute the RG-1 Dockerfile mode/gate blocks (MR !2059, RV-1/RV-3).

These fixtures check the real shell instructions, not a copy of their mode
arithmetic. Dropped-uid tests require root and do not replace image builds.
"""

import os
import pathlib
import shutil
import stat
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
ROOT_PROBE = os.geteuid() == 0 and shutil.which("setpriv") is not None


def image_step(lane, needle):
    text = (ROOT / lane / "Dockerfile").read_text().replace("\\\n", " ")
    matches = [line[4:] for line in text.splitlines() if line.startswith("RUN ") and needle in line]
    if len(matches) != 1:
        raise AssertionError(f"expected one {lane} RUN containing {needle!r}, got {len(matches)}")
    return matches[0]


class WorkerImageModesTests(unittest.TestCase):
    def fixture(self, lane, umask):
        temporary = tempfile.TemporaryDirectory(prefix="worker-image-modes-")
        self.addCleanup(temporary.cleanup)
        root = pathlib.Path(temporary.name)
        root.chmod(0o755)
        if lane == "pebble4lunarwing":
            files = {
                "usr/local/bin/entrypoint.sh": 0o777 & ~umask,
                "usr/local/bin/entrypoint-split.sh": 0o777 & ~umask,
                "usr/local/bin/pebble": 0o755,
                "usr/local/bin/pebble4lunarwing": 0o755,
                "etc/profile.d/lunarwing-toolchain.sh": 0o666 & ~umask,
                "usr/local/share/lunarwing/rustup-wrapper.sh": 0o666 & ~umask,
                "etc/lunarwing/pebble/settings.json": 0o666 & ~umask,
            }
            copied_dirs = ["etc/lunarwing/pebble"]
        else:
            files = {f"app/{name}": 0o666 & ~umask for name in (
                "config/opencode2.json", "scripts/nested directory/helper.js",
                "entrypoint.sh", "entrypoint-split.sh", "health_server.py", "agent_comm_protocol.json",
            )}
            copied_dirs = ["app/config", "app/scripts", "app/scripts/nested directory"]
        for relative, mode in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fixture input\n")
            path.chmod(mode)
        for path in root.rglob("*"):
            if path.is_dir():
                path.chmod(0o755)
        for relative in copied_dirs:
            (root / relative).chmod(0o777 & ~umask)
        if lane != "pebble4lunarwing":
            (root / "app/scripts/helper-link.js").symlink_to("nested directory/helper.js")
        return root

    def run_steps(self, root, *steps):
        script = "\n".join(steps)
        for prefix in ("/app", "/usr/local", "/etc/"):
            script = script.replace(prefix, str(root) + prefix)
        return subprocess.run(["sh", "-ec", script], env={"PATH": os.environ["PATH"]},
                              text=True, capture_output=True, timeout=30)

    def normalize(self, lane, root):
        if lane == "pebble4lunarwing":
            needles = ("chmod 0644 /etc/profile.d", "chmod 0555 /usr/local/bin/entrypoint.sh",
                       "chmod 0555 /etc/lunarwing/pebble")
        else:
            needles = ("chmod -R a+rX /app/config", "chmod +x /app/entrypoint.sh")
        result = self.run_steps(root, *(image_step(lane, needle) for needle in needles))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def gate(self, lane, root):
        needle = 'task_inputs="' if lane == "pebble4lunarwing" else 'test -z "$(find -L /app/scripts'
        return self.run_steps(root, image_step(lane, needle))

    def test_pebble_normal_and_restrictive_modes_match(self):
        snapshots = []
        for umask in (0o022, 0o077):
            root = self.fixture("pebble4lunarwing", umask)
            self.normalize("pebble4lunarwing", root)
            modes = {str(p.relative_to(root)): stat.S_IMODE(p.stat().st_mode)
                     for p in root.rglob("*")}
            self.assertEqual(modes["usr/local/bin/pebble"], 0o755)
            self.assertEqual(modes["etc/profile.d/lunarwing-toolchain.sh"], 0o644)
            self.assertEqual(modes["usr/local/bin/entrypoint.sh"], 0o555)
            snapshots.append(modes)
        self.assertEqual(*snapshots)

    def test_opencode2_normal_and_restrictive_modes_match(self):
        snapshots = []
        for umask in (0o022, 0o077):
            root = self.fixture("opencode2.4lunarwing", umask)
            self.normalize("opencode2.4lunarwing", root)
            modes = {}
            for path in (root / "app").rglob("*"):
                mode = stat.S_IMODE(path.stat().st_mode)
                self.assertEqual(mode & 0o222, 0, path)
                self.assertEqual(mode & 0o444, 0o444, path)
                if path.is_dir() or path.name in ("entrypoint.sh", "entrypoint-split.sh", "health_server.py"):
                    self.assertEqual(mode & 0o111, 0o111, path)
                modes[str(path.relative_to(root))] = mode
            snapshots.append(modes)
        self.assertEqual(*snapshots)

    @unittest.skipUnless(ROOT_PROBE, "requires root and setpriv for the real task uid")
    def test_pebble_gate_accepts_root_owner_write_in_both_contexts(self):
        for umask in (0o022, 0o077):
            with self.subTest(umask=oct(umask)):
                root = self.fixture("pebble4lunarwing", umask)
                self.normalize("pebble4lunarwing", root)
                result = self.gate("pebble4lunarwing", root)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    @unittest.skipUnless(ROOT_PROBE, "requires root and setpriv for the real task uid")
    def test_pebble_gate_refuses_group_and_other_write(self):
        root = self.fixture("pebble4lunarwing", 0o077)
        self.normalize("pebble4lunarwing", root)
        path = root / "usr/local/bin/pebble"
        for mode in (0o775, 0o757):
            with self.subTest(mode=oct(mode)):
                path.chmod(mode)
                self.assertNotEqual(self.gate("pebble4lunarwing", root).returncode, 0)
        path.chmod(0o755)

    @unittest.skipUnless(ROOT_PROBE, "requires root and setpriv for the real task uid")
    def test_pebble_gate_refuses_nonroot_ownership(self):
        root = self.fixture("pebble4lunarwing", 0o077)
        self.normalize("pebble4lunarwing", root)
        os.chown(root / "usr/local/bin/pebble", 10004, 10004)
        self.assertNotEqual(self.gate("pebble4lunarwing", root).returncode, 0)

    @unittest.skipUnless(ROOT_PROBE, "requires root and setpriv for the real task uid")
    def test_pebble_gate_really_reads_and_checks_execute_as_task_uid(self):
        for relative, mode in (("etc/profile.d/lunarwing-toolchain.sh", 0o600),
                               ("usr/local/bin/entrypoint.sh", 0o444)):
            with self.subTest(relative=relative):
                root = self.fixture("pebble4lunarwing", 0o077)
                self.normalize("pebble4lunarwing", root)
                (root / relative).chmod(mode)
                self.assertNotEqual(self.gate("pebble4lunarwing", root).returncode, 0)

    @unittest.skipUnless(ROOT_PROBE, "requires root and setpriv for the real task uid")
    def test_opencode2_gate_accepts_both_contexts(self):
        for umask in (0o022, 0o077):
            with self.subTest(umask=oct(umask)):
                root = self.fixture("opencode2.4lunarwing", umask)
                self.normalize("opencode2.4lunarwing", root)
                result = self.gate("opencode2.4lunarwing", root)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    @unittest.skipUnless(ROOT_PROBE, "requires root and setpriv for the real task uid")
    def test_opencode2_gate_refuses_unreadable_nested_inputs(self):
        root = self.fixture("opencode2.4lunarwing", 0o077)
        self.normalize("opencode2.4lunarwing", root)
        (root / "app/scripts/nested directory/helper.js").chmod(0o000)
        result = self.gate("opencode2.4lunarwing", root)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Permission denied", result.stderr)

    @unittest.skipUnless(ROOT_PROBE, "requires root and setpriv for the real task uid")
    def test_opencode2_gate_keeps_policy_inputs_root_owned_and_readonly(self):
        for sabotage in ("owner-write", "task-owner"):
            with self.subTest(sabotage=sabotage):
                root = self.fixture("opencode2.4lunarwing", 0o077)
                self.normalize("opencode2.4lunarwing", root)
                path = root / "app/config/opencode2.json"
                if sabotage == "owner-write":
                    path.chmod(0o644)
                else:
                    os.chown(path, 10003, 10003)
                self.assertNotEqual(self.gate("opencode2.4lunarwing", root).returncode, 0)


if __name__ == "__main__":
    unittest.main()
