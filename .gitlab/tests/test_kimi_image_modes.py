import os
import pathlib
import shutil
import stat
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
KIMI = ROOT / "kimicode4lunarwing"


def image_mode_step():
    dockerfile = (KIMI / "Dockerfile").read_text()
    steps = dockerfile.replace("\\\n", " ").split("\nRUN ")
    return next(step.split("\n\n", 1)[0] for step in steps
                if "chmod -R a+rX /app/config" in step)


class KimiImageModesTests(unittest.TestCase):
    def fixture(self, root, umask):
        app = root / "app"
        app.mkdir(mode=0o755)
        app.chmod(0o755)
        for directory in ("config", "hooks", "scripts", "scripts/nested directory"):
            path = app / directory
            path.mkdir()
            path.chmod(0o777 & ~umask)
        for source, target in (
            ("config/config.toml", "config/config.toml"),
            ("hooks/headless_command_guard.mjs", "hooks/headless_command_guard.mjs"),
            ("entrypoint.sh", "entrypoint.sh"),
            ("entrypoint-task-setup.sh", "entrypoint-task-setup.sh"),
            ("health_server.py", "health_server.py"),
            ("task_runner_bind.py", "task_runner_bind.py"),
            ("agent_comm_protocol.json", "agent_comm_protocol.json"),
        ):
            text = (KIMI / source).read_text().replace("/app", str(app))
            text = text.replace("/home/kimi", str(root / "task-home"))
            (app / target).write_text(text)
            (app / target).chmod(0o666 & ~umask)
        for name in ("scripts/task_runner.js", "scripts/nested directory/module.js"):
            (app / name).write_text("export const ready = true;\n")
            (app / name).chmod(0o666 & ~umask)
        subprocess.run(["chmod", "+x", str(app / "entrypoint.sh"),
                        str(app / "entrypoint-task-setup.sh"),
                        str(app / "health_server.py")], check=True)
        return app

    def run_step(self, app, *, probe):
        step = image_mode_step()
        if not probe:
            step = step.split('probe_dir="$(mktemp -d)"', 1)[0]
        return subprocess.run(["sh", "-c", step.replace("/app", str(app))],
                              env={"PATH": os.environ["PATH"]},
                              text=True, capture_output=True, timeout=30)

    def test_normal_and_restrictive_contexts_have_identical_readonly_modes(self):
        snapshots = []
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            for umask in (0o022, 0o077):
                case = root / str(umask)
                case.mkdir()
                app = self.fixture(case, umask)
                result = self.run_step(app, probe=False)
                self.assertEqual(result.returncode, 0, result.stderr)
                modes = {}
                for path in app.rglob("*"):
                    mode = stat.S_IMODE(path.stat().st_mode)
                    modes[str(path.relative_to(app))] = mode
                    self.assertEqual(mode & 0o222, 0, path)
                    self.assertEqual(mode & 0o444, 0o444, path)
                    if path.is_dir() or path.name in (
                            "entrypoint.sh", "entrypoint-task-setup.sh", "health_server.py"):
                        self.assertEqual(mode & 0o111, 0o111, path)
                snapshots.append(modes)
        self.assertEqual(*snapshots)

    def test_image_gate_drops_identity_and_cleans_credential_free_setup(self):
        step = image_mode_step()
        self.assertEqual(step.count("setpriv --reuid=10002 --regid=10002 --clear-groups"), 2)
        self.assertIn("-type f -exec cat -- {} +", step)
        self.assertIn(r"\( ! -uid 0 -o -perm /222 \)", step)
        self.assertIn("env -i", step)
        self.assertIn('trap \'rm -rf "$probe_dir"\' EXIT', step)
        self.assertIn('test -s "$probe_dir/state/config.toml"', step)
        self.assertIn('test -s "$probe_dir/state/mcp.json"', step)

    @unittest.skipUnless(os.geteuid() == 0 and shutil.which("setpriv"),
                         "requires root or a rootless user namespace with setpriv")
    def test_real_task_uid_reads_every_input_and_runs_setup(self):
        with tempfile.TemporaryDirectory(prefix="kimi-image-modes-") as temporary:
            root = pathlib.Path(temporary)
            root.chmod(0o755)
            app = self.fixture(root, 0o077)
            result = self.run_step(app, probe=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    @unittest.skipUnless(os.geteuid() == 0 and shutil.which("setpriv"),
                         "requires root or a rootless user namespace with setpriv")
    def test_task_uid_probe_refuses_unreadable_nested_inputs(self):
        with tempfile.TemporaryDirectory(prefix="kimi-image-modes-") as temporary:
            root = pathlib.Path(temporary)
            root.chmod(0o755)
            app = self.fixture(root, 0o077)
            step = image_mode_step().replace("/app", str(app))
            for target in (app / "hooks", app / "scripts/nested directory/module.js"):
                with self.subTest(target=target.name):
                    sabotaged = step.replace('probe_dir="$(mktemp -d)"',
                        f'chmod 0400 "{target}"; probe_dir="$(mktemp -d)"', 1)
                    result = subprocess.run(["sh", "-c", sabotaged],
                        env={"PATH": os.environ["PATH"]}, text=True,
                        capture_output=True, timeout=30)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("Permission denied", result.stderr)
                    target.chmod(0o555 if target.is_dir() else 0o444)


if __name__ == "__main__":
    unittest.main()
