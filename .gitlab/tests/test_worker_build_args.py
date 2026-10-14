#!/usr/bin/env python3
"""Exercise the CI wrapper digest contract without building/publishing images."""

import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".gitlab/scripts/build-worker-image.sh"


class WorkerBuildArgsTests(unittest.TestCase):
    def setUp(self):
        # PATH shims need an exec-capable filesystem (some hosts mount /tmp noexec).
        temporary = tempfile.TemporaryDirectory(dir=ROOT)
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        # Match the slim ci-config tool surface; neither git nor awk is
        # installed there. Keep the PATH closed so this cannot contact a real
        # builder or registry even if the production script changes.
        for name in ("sh", "sha256sum", "cut", "tr", "base64", "mkdir"):
            tool = shutil.which(name)
            self.assertIsNotNone(tool, f"required tool {name!r} not found")
            (self.bin / name).symlink_to(tool)
        self.log = self.root / "build-calls.jsonl"
        self.script = self.root / SCRIPT.name
        shutil.copy(SCRIPT, self.script)
        shutil.copytree(
            ROOT / "opencode2.4lunarwing", self.root / "opencode2.4lunarwing",
            ignore=shutil.ignore_patterns("node_modules", "__pycache__"),
        )
        self.wrapper = self.root / "opencode2.4lunarwing/scripts/opencode-exposure-guard.sh"
        self.write_shim("git", "#!/bin/sh\nprintf '%s\\n' 1700000000\n")
        # Only metadata_digest() uses awk; emulate that extraction in Python.
        self.write_shim("awk", f"""#!{sys.executable}
import json
import sys
with open(sys.argv[-1]) as source:
    print(json.load(source)["containerimage.digest"])
""")
        self.write_shim("buildctl-daemonless.sh", f"""#!{sys.executable}
import json
import os
import pathlib
import sys
args = sys.argv[1:]
with open(os.environ["BUILD_CALLS"], "a") as log:
    log.write(json.dumps(args) + "\\n")
metadata = pathlib.Path(args[args.index("--metadata-file") + 1])
metadata.write_text(json.dumps({{"containerimage.digest": "sha256:" + "0" * 64}}))
""")
        self.env = {
            **os.environ,
            "PATH": str(self.bin),
            "HOME": str(self.root / "home"),
            "BUILD_CALLS": str(self.log),
            "CI_COMMIT_SHA": "1" * 40,
            "CI_JOB_ID": "2",
            "CI_PIPELINE_ID": "3",
            "CI_PROJECT_DIR": str(self.root),
            "CI_REGISTRY": "registry.invalid",
            "CI_REGISTRY_IMAGE": "registry.invalid/example/worker",
            "CI_REGISTRY_USER": "fixture",
            "CI_REGISTRY_PASSWORD": "fixture",
        }

    def write_shim(self, name, source):
        path = self.bin / name
        path.unlink(missing_ok=True)
        path.write_text(source, encoding="utf-8")
        path.chmod(0o755)

    def run_build(self):
        self.log.unlink(missing_ok=True)
        return subprocess.run(
            ["sh", str(self.script), "opencode2"], cwd=self.root, env=self.env,
            capture_output=True, text=True, timeout=30,
        )

    def test_checkout_digest_reaches_candidate_and_clean_rebuild(self):
        # Changing the checkout catches hardcoded/stale digests; inspect the
        # actual buildctl argv, so dropping the build-arg forwarding is caught.
        for revision in ("first", "second"):
            with self.subTest(revision=revision):
                self.wrapper.write_text(f"#!/bin/sh\n# {revision} wrapper\n", encoding="utf-8")
                digest = hashlib.sha256(self.wrapper.read_bytes()).hexdigest()
                result = self.run_build()
                self.assertEqual(result.returncode, 0, result.stderr)
                calls = [json.loads(line) for line in self.log.read_text().splitlines()]
                self.assertEqual(len(calls), 2)
                self.assertNotIn("--no-cache", calls[0])
                self.assertIn("--no-cache", calls[1])
                for args in calls:
                    options = [args[i + 1] for i, arg in enumerate(args) if arg == "--opt"]
                    anchors = [arg for arg in options if arg.startswith("build-arg:LUNARWING_WRAPPER_SHA256=")]
                    self.assertEqual(anchors, [f"build-arg:LUNARWING_WRAPPER_SHA256={digest}"])

    def test_missing_wrapper_fails_before_build(self):
        self.wrapper.unlink()
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot hash", result.stderr)
        self.assertFalse(self.log.exists())

    def test_failed_hash_command_fails_even_with_valid_looking_output(self):
        self.write_shim("sha256sum", "#!/bin/sh\nprintf '%064d  -\\n' 0\nexit 1\n")
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot hash", result.stderr)
        self.assertFalse(self.log.exists())

    def test_malformed_wrapper_digest_fails_before_build(self):
        for digest in ("", "a" * 63, "a" * 65, "g" * 64):
            with self.subTest(digest=digest):
                self.write_shim("sha256sum", f"#!/bin/sh\nprintf '%s  -\\n' '{digest}'\n")
                result = self.run_build()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("invalid wrapper sha256", result.stderr)
                self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
