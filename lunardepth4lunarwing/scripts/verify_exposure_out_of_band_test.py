#!/usr/bin/env python3

import importlib.util
import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path

from image_config_policy_test import valid_config
from verify_exposure_out_of_band import source_inventory, validate_archive


class OutOfBandTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.archive_path = root / "rootfs.tar"
        self.config_path = root / "config.json"
        self.config_path.write_text(json.dumps([valid_config()]), encoding="utf-8")
        self.lane_root = Path(__file__).parents[1]
        self.compiled_root = self.lane_root / "scripts"
        expected_js = [
            path.with_suffix(".js")
            for path in self.compiled_root.glob("*.ts")
            if not path.name.endswith("_test.ts")
        ]
        if any(not path.is_file() for path in expected_js):
            self.compiled_root = root / "compiled"
            self.compiled_root.mkdir()
            for source in expected_js:
                target = self.compiled_root / source.name
                target.write_bytes(source.with_suffix(".ts").read_bytes())
        self.make_archive(self.archive_path)

    def tearDown(self):
        self.temp.cleanup()

    def make_archive(self, path):
        with tarfile.open(path, "w") as archive:
            for name, source in source_inventory(self.lane_root, self.compiled_root).items():
                data = source.read_bytes()
                info = tarfile.TarInfo(name)
                info.size = len(data)
                info.mode = 0o555 if name.endswith((".sh", "dsh")) or name == "app/health_server.py" else 0o444
                info.uid = info.gid = 0
                archive.addfile(info, io.BytesIO(data))
            for name in ("opt/lunardepth/worker-inputs.sha256", "opt/lunardepth-templates/profiles/lunarwing-worker/package.json"):
                data = b"{}\n"
                info = tarfile.TarInfo(name)
                info.size = len(data)
                info.mode = 0o444
                info.uid = info.gid = 0
                archive.addfile(info, io.BytesIO(data))

    def test_valid_archive_and_config(self):
        validate_archive(self.archive_path, self.config_path, self.lane_root, self.compiled_root)

    def test_writable_application_file_fails(self):
        with tarfile.open(self.archive_path, "a") as archive:
            info = tarfile.TarInfo("app/planted")
            data = b"bad"
            info.size = len(data)
            info.mode = 0o666
            info.uid = info.gid = 0
            archive.addfile(info, io.BytesIO(data))
        with self.assertRaisesRegex(ValueError, "writable"):
            validate_archive(self.archive_path, self.config_path, self.lane_root, self.compiled_root)

    def test_vendor_tests_are_allowed_but_lane_test_residue_fails(self):
        with tarfile.open(self.archive_path, "a") as archive:
            info = tarfile.TarInfo("usr/local/lib/node_modules/npm/node_modules/node-gyp/gyp/pylib/gyp/MSVSSettings_test.py")
            data = b"base-image fixture"
            info.size = len(data)
            info.mode = 0o444
            info.uid = info.gid = 0
            archive.addfile(info, io.BytesIO(data))
        validate_archive(self.archive_path, self.config_path, self.lane_root, self.compiled_root)
        with tarfile.open(self.archive_path, "a") as archive:
            info = tarfile.TarInfo("app/scripts/planted_test.js")
            data = b"lane test residue"
            info.size = len(data)
            info.mode = 0o444
            info.uid = info.gid = 0
            archive.addfile(info, io.BytesIO(data))
        with self.assertRaisesRegex(ValueError, "test/package-manager residue"):
            validate_archive(self.archive_path, self.config_path, self.lane_root, self.compiled_root)

    def test_smoke_secret_and_checkout_drift_fail(self):
        with tarfile.open(self.archive_path, "a") as archive:
            for name in ("run/lunardepth-state/local-auth-token", "app/entrypoint.sh"):
                data = b"drift"
                info = tarfile.TarInfo(name)
                info.size = len(data)
                info.mode = 0o400 if "token" in name else 0o555
                info.uid = 10005 if "token" in name else 0
                info.gid = 10005 if "token" in name else 0
                archive.addfile(info, io.BytesIO(data))
        with self.assertRaisesRegex(ValueError, "residue|drift"):
            validate_archive(self.archive_path, self.config_path, self.lane_root, self.compiled_root)


if __name__ == "__main__":
    unittest.main()
