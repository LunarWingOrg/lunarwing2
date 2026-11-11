#!/usr/bin/env python3

import copy
import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("image_config_policy", Path(__file__).with_name("image_config_policy.py"))
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def valid_config():
    return {"Config": {
        "User": "10005:10005",
        "Entrypoint": MODULE.EXPECTED_ENTRYPOINT,
        "Cmd": MODULE.EXPECTED_CMD,
        "WorkingDir": "/app",
        "ExposedPorts": {"8443/tcp": {}, "9090/tcp": {}},
        "Healthcheck": {"Test": MODULE.EXPECTED_HEALTHCHECK},
        "Env": [
            "NODE_VERSION=22.23.4",
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            "LUNARDEPTH_IMAGE_REVISION=da06c33b2091546dc38c61b95452ce27a4ce32d5",
            "LUNARDEPTH_IMAGE_VERSION=0.2.1-alpha.1",
            "LUNARDEPTH_PROFILE_SCHEMA_VERSION=unversioned",
            "LUNARDEPTH_TASK_UID=10005",
            "LUNARDEPTH_TASK_GID=10005",
            "LUNARDEPTH_BRIDGE_UID=10105",
            "LUNARDEPTH_CLOSURE_ROOT=/opt/lunardepth",
            "LUNARDEPTH_TEMPLATE_ROOT=/opt/lunardepth-templates",
            "LUNARWING_APP_DIR=/app",
            "DSH_TELEMETRY_DISABLED=1",
            "WS_NO_BUFFER_UTIL=1",
            "WS_NO_UTF_8_VALIDATE=1",
        ],
    }}


class ImageConfigTests(unittest.TestCase):
    def test_valid_config(self):
        MODULE.validate_config(valid_config())

    def test_rejects_credentials_and_task_listener(self):
        for key, value in (("AGENT_AUTH_TOKEN", "secret"), ("LUNARDEPTH_API_KEY", "secret"), ("LUNARDEPTH_BASE_URL", "https://example.test")):
            document = copy.deepcopy(valid_config())
            document["Config"]["Env"].append(f"{key}={value}")
            with self.assertRaisesRegex(ValueError, "unexpected image environment"):
                MODULE.validate_config(document)
        document = copy.deepcopy(valid_config())
        document["Config"]["ExposedPorts"]["19091/tcp"] = {}
        with self.assertRaisesRegex(ValueError, "only bridge"):
            MODULE.validate_config(document)

    def test_rejects_identity_and_health_drift(self):
        document = copy.deepcopy(valid_config())
        document["Config"]["User"] = "0:0"
        with self.assertRaisesRegex(ValueError, "User"):
            MODULE.validate_config(document)
        document = copy.deepcopy(valid_config())
        document["Config"]["Healthcheck"]["Test"] = ["CMD", "true"]
        with self.assertRaisesRegex(ValueError, "healthcheck"):
            MODULE.validate_config(document)

    def test_runtime_paths_and_shell_are_pinned(self):
        document = copy.deepcopy(valid_config())
        document["Config"]["Shell"] = MODULE.EXPECTED_SHELL
        MODULE.validate_config(document)
        for key in ("LUNARDEPTH_CLOSURE_ROOT", "LUNARDEPTH_TEMPLATE_ROOT", "LUNARWING_APP_DIR"):
            document = copy.deepcopy(valid_config())
            document["Config"]["Env"] = [
                entry if not entry.startswith(f"{key}=") else f"{key}=/tmp/redirected"
                for entry in document["Config"]["Env"]
            ]
            with self.assertRaisesRegex(ValueError, "runtime path metadata"):
                MODULE.validate_config(document)
        document = copy.deepcopy(valid_config())
        document["Config"]["Shell"] = ["/bin/sh", "-c"]
        with self.assertRaisesRegex(ValueError, "image Shell"):
            MODULE.validate_config(document)

    def test_hardening_clears_inherited_setid_and_write_bits(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "closure"
            nested = root / "config"
            nested.mkdir(parents=True)
            payload = nested / "settings.json"
            payload.write_text("{}\n", encoding="utf-8")
            nested.chmod(0o2755)
            payload.chmod(0o4755)
            script = Path(__file__).with_name("harden_image_modes.sh")
            result = subprocess.run(
                ["bash", str(script), str(root)], capture_output=True, text=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(nested.stat().st_mode & 0o7777, 0o555)
            self.assertEqual(payload.stat().st_mode & 0o7777, 0o444)
            payload.chmod(0o644)
            result = subprocess.run(
                ["bash", str(script), str(root)], capture_output=True, text=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(payload.stat().st_mode & 0o7777, 0o444)


if __name__ == "__main__":
    unittest.main()
