#!/usr/bin/env python3
"""Regression checks for the Pebble lock-only input and its build guards."""

import copy
import hashlib
import importlib.util
import json
import pathlib
import tempfile
import unittest
from unittest import mock

import yaml


ROOT = pathlib.Path(__file__).resolve().parents[2]


def load_module(name: str, path: pathlib.Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


config = load_module("validate_config", ROOT / ".gitlab/scripts/validate_config.py")
guard = load_module("verify_upstream_lock", ROOT / "pebble4lunarwing/scripts/verify_upstream_lock.py")


class LockDeltaTests(unittest.TestCase):
    def setUp(self):
        self.upstream = {
            "version": 4,
            "package": [
                {"name": "rustls", "version": "0.23.41", "source": "registry+example", "checksum": "tls-old"},
                {"name": "rustls-webpki", "version": "0.103.13", "source": "registry+example", "checksum": "webpki-old"},
                {"name": "unrelated", "version": "1.0.0", "source": "registry+example", "checksum": "original", "dependencies": ["rustls"]},
                {"name": "unrelated", "version": "2.0.0", "source": "registry+example", "checksum": "second-version"},
            ],
        }
        self.patched = copy.deepcopy(self.upstream)
        self.patched["package"][0].update(version="0.23.45", checksum="tls-new")
        self.patched["package"][1].update(version="0.103.15", checksum="webpki-new")
        # The real override also retires upstream's yanked chacha20 0.10.1 pin.
        for records in (self.upstream, self.patched):
            records["package"].append(
                {"name": "chacha20", "version": "0.10.1", "source": "registry+example", "checksum": "chacha-yanked"}
            )
        self.patched["package"][-1].update(version="0.10.2", checksum="chacha-new")

    def test_intended_tls_patch(self):
        guard.check_package_delta(self.upstream, self.patched)

    def test_noop_override_rejected(self):
        # Once upstream ships its own fixed lock the override becomes a no-op;
        # the guard must fail loudly so the stale input cannot linger.
        with self.assertRaisesRegex(ValueError, "no-op"):
            guard.check_package_delta(self.upstream, copy.deepcopy(self.upstream))

    def test_earlier_version_record_edit_rejected(self):
        # Multi-version crates (cpufeatures, getrandom, …) must be keyed by
        # (name, version, source): editing or dropping only an earlier record
        # of an allowlisted-name crate still fails the complete-record check.
        patched = copy.deepcopy(self.patched)
        for package in patched["package"]:
            if package["name"] == "unrelated" and package["version"] == "1.0.0":
                package["checksum"] = "tampered-earlier-record"
        with self.assertRaisesRegex(ValueError, "outside allowlist.*unrelated"):
            guard.check_package_delta(self.upstream, patched)

    def test_unrelated_record_fields(self):
        for field, value in (
            ("version", "1.0.1"), ("source", "registry+other"),
            ("checksum", "tampered"), ("dependencies", ["rustls", "new-dependency"]),
            ("replace", "another-package"),
        ):
            with self.subTest(field=field):
                patched = copy.deepcopy(self.patched)
                patched["package"][2][field] = value
                with self.assertRaisesRegex(ValueError, "outside allowlist.*unrelated"):
                    guard.check_package_delta(self.upstream, patched)

    def test_package_addition_and_removal(self):
        for add in (True, False):
            with self.subTest(add=add):
                patched = copy.deepcopy(self.patched)
                if add:
                    patched["package"].append({"name": "injected", "version": "1.0.0"})
                else:
                    patched["package"].pop(2)  # drop the "unrelated" 1.0.0 record
                with self.assertRaisesRegex(ValueError, "outside allowlist"):
                    guard.check_package_delta(self.upstream, patched)

    def test_duplicate_package_identity_rejected(self):
        self.patched["package"].append(copy.deepcopy(self.patched["package"][2]))
        with self.assertRaisesRegex(ValueError, "duplicate"):
            guard.check_package_delta(self.upstream, self.patched)

    def test_lock_metadata_change_rejected(self):
        self.patched["version"] = 3
        with self.assertRaisesRegex(ValueError, "metadata changed"):
            guard.check_package_delta(self.upstream, self.patched)

    def test_package_order_is_irrelevant(self):
        self.patched["package"].reverse()
        guard.check_package_delta(self.upstream, self.patched)

    def test_digest_gate_and_toml_parsing(self):
        with tempfile.TemporaryDirectory() as temporary:
            upstream = pathlib.Path(temporary) / "upstream.lock"
            patched = pathlib.Path(temporary) / "patched.lock"
            text = 'version = 4\n[[package]]\nname = "rustls"\nversion = "0.23.41"\n'
            upstream.write_text(text, encoding="utf-8")
            patched.write_text(text.replace("0.23.41", "0.23.45"), encoding="utf-8")
            digest = hashlib.sha256(patched.read_bytes()).hexdigest()
            guard.verify(upstream, patched, digest)
            for invalid in ("", "0" * 64):
                with self.subTest(digest=invalid), self.assertRaises(ValueError):
                    guard.verify(upstream, patched, invalid)
            patched.write_text("not valid TOML", encoding="utf-8")
            digest = hashlib.sha256(patched.read_bytes()).hexdigest()
            with self.assertRaises(ValueError):
                guard.verify(upstream, patched, digest)

    def test_dockerfile_has_reviewed_default_and_uses_guard(self):
        dockerfile = (ROOT / "pebble4lunarwing/Dockerfile").read_text(encoding="utf-8")
        digest = config.extract_arg_default(dockerfile, "PEBBLE_LOCK_SHA256")
        self.assertEqual(digest, hashlib.sha256((ROOT / "pebble4lunarwing/upstream/Cargo.lock").read_bytes()).hexdigest())
        self.assertIn('RUN python3 /tmp/verify-pebble-lock.py pebble/Cargo.lock', dockerfile)
        self.assertIn('"${PEBBLE_LOCK_SHA256}"', dockerfile)
        self.assertIn("git python3", dockerfile)
        build_script = (ROOT / ".gitlab/scripts/build-worker-image.sh").read_text(encoding="utf-8")
        self.assertIn("pebble4lunarwing/scripts/verify_upstream_lock.py'", build_script)
        self.assertNotIn("PEBBLE_LOCK_SHA256=", build_script)  # CI uses the reviewed default too.

    def test_matching_digest_does_not_authorize_package_addition(self):
        with tempfile.TemporaryDirectory() as temporary:
            upstream = pathlib.Path(temporary) / "upstream.lock"
            patched = pathlib.Path(temporary) / "patched.lock"
            text = 'version = 4\n[[package]]\nname = "rustls"\nversion = "0.23.41"\n'
            upstream.write_text(text, encoding="utf-8")
            patched.write_text(text + '\n[[package]]\nname = "injected"\nversion = "1.0.0"\n', encoding="utf-8")
            digest = hashlib.sha256(patched.read_bytes()).hexdigest()
            with self.assertRaisesRegex(ValueError, "outside allowlist.*injected"):
                guard.verify(upstream, patched, digest)

    def test_both_compose_services_use_the_canonical_dockerfile_default(self):
        compose = yaml.safe_load((ROOT / "pebble4lunarwing/docker-compose.yml").read_text(encoding="utf-8"))
        for name in ("pebble-worker", "pebble-cli"):
            with self.subTest(service=name):
                build = compose["services"][name]["build"]
                self.assertEqual(build["context"], "..")
                self.assertEqual(build["dockerfile"], "pebble4lunarwing/Dockerfile")
                self.assertNotIn("PEBBLE_LOCK_SHA256", build.get("args", {}))


class BuildInputInventoryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        for path in ("worker/Cargo.toml", "worker/Cargo.lock", "worker/upstream/Cargo.lock"):
            target = self.root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text("", encoding="utf-8")
        (self.root / "worker/Dockerfile").write_text("COPY worker/upstream/Cargo.lock /tmp/patched.lock\n", encoding="utf-8")
        self.inventory = self.root / "deliverables.json"
        self.data = {
            "schema": 1,
            "deliverables": [{"name": "worker", "status": "production", "owner": "workers",
                              "manifest": "worker/Cargo.toml", "lockfile": "worker/Cargo.lock",
                              "targets": ["worker"], "ci": ["ci-config"], "triggers": ["worker/**"]}],
            "build_inputs": [{"name": "patched-lock", "kind": "cargo-lock-override", "owner": "workers",
                              "consumer": "worker", "lockfile": "worker/upstream/Cargo.lock",
                              "dockerfile": "worker/Dockerfile", "ci": ["ci-config", "worker-candidate"],
                              "triggers": ["worker/**"]}],
        }
        self.jobs = {"ci-config": {}, "worker-candidate": {"extends": ".worker-candidate"}, "standalone-deliverables": {}}

    def errors(self):
        self.inventory.write_text(json.dumps(self.data), encoding="utf-8")
        errors = []
        with mock.patch.object(config, "ROOT", self.root), mock.patch.object(config, "DELIVERABLES", self.inventory):
            config.validate_deliverables_manifest(self.jobs, errors)
        return errors

    def test_lock_override_does_not_need_sibling_manifest(self):
        self.assertFalse((self.root / "worker/upstream/Cargo.toml").exists())
        self.assertEqual(self.errors(), [])

    def test_unclassified_lock_still_blocks_ci(self):
        self.data["build_inputs"] = []
        self.assertTrue(any("missing from" in error for error in self.errors()))

    def test_unrelated_lock_is_not_exempted(self):
        (self.root / "unclassified").mkdir()
        (self.root / "unclassified/Cargo.lock").touch()
        self.assertTrue(any("unclassified/Cargo.lock" in error for error in self.errors()))

    def test_build_input_coverage_is_required(self):
        for ci in (["ci-config"], ["worker-candidate"], ["ci-config", "missing-job"],
                   ["ci-config", "worker-candidate", "standalone-deliverables"]):
            with self.subTest(ci=ci):
                self.data["build_inputs"][0]["ci"] = ci
                self.assertTrue(self.errors())

    def test_missing_unsafe_or_unconsumed_input_rejected(self):
        for key, value in (("lockfile", "../outside/Cargo.lock"), ("dockerfile", "missing/Dockerfile"),
                           ("consumer", "missing"), ("kind", "ignore"), ("triggers", ["elsewhere/**"])):
            with self.subTest(key=key):
                original = self.data["build_inputs"][0][key]
                self.data["build_inputs"][0][key] = value
                self.assertTrue(self.errors())
                self.data["build_inputs"][0][key] = original
        (self.root / "worker/Dockerfile").write_text("COPY another/Cargo.lock /tmp/lock\n", encoding="utf-8")
        self.assertTrue(any("not copied" in error for error in self.errors()))

    def test_duplicate_classification_rejected(self):
        self.data["build_inputs"].append(copy.deepcopy(self.data["build_inputs"][0]))
        self.assertTrue(any("declared twice" in error for error in self.errors()))


if __name__ == "__main__":
    unittest.main()
