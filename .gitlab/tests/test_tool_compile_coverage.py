#!/usr/bin/env python3
"""WI#809: a production WASM tool must name a blocking job that compiles it.

The job's crate list is derived from `ci` in .gitlab/deliverables.json, so the
validator and the runner must agree on the same contract. These tests pin the
rule, its failure modes, and the real tree's wiring.
"""

import importlib.util
import json
import pathlib
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("validate_config", ROOT / ".gitlab/scripts/validate_config.py")
config = importlib.util.module_from_spec(spec)
spec.loader.exec_module(config)


def deliverable(name="tool-x", status="production", manifest="ic/tools-src/x/Cargo.toml",
                ci=("standalone-deliverables", "wasm-tools-check")):
    return {
        "name": name, "status": status, "owner": "tools", "manifest": manifest,
        "lockfile": manifest.replace("Cargo.toml", "Cargo.lock"),
        "targets": [name], "ci": list(ci), "triggers": ["ic/tools-src/x/**"],
    }


class ToolCompileCoverageTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.registry = self.root / "registry"
        self.registry.mkdir()
        self.source = self.root / "ic" / "tools-src" / "x"
        self.source.mkdir(parents=True)
        (self.source / "Cargo.toml").write_text("[package]\n", encoding="utf-8")
        self.write_registry({"name": "x", "source": {"dir": "tools-src/x", "crate_name": "x-tool"}})
        self.inventory = self.root / "deliverables.json"
        self.write_inventory([deliverable()])
        self.jobs = {
            "ci-config": {"script": ["python3 .gitlab/scripts/validate_config.py"]},
            "standalone-deliverables": {"script": ["python3 .gitlab/scripts/standalone-deliverables.py"]},
            "wasm-tools-check": {"script": ["rustup target add wasm32-wasip2",
                                            "python3 .gitlab/scripts/wasm-tools-check.py"]},
            "builds-x": {"script": ["bash .gitlab/scripts/cargo-ci.sh build --manifest-path tools-src/x/Cargo.toml"]},
        }

    def write_registry(self, entry):
        (self.registry / "x.json").write_text(json.dumps(entry), encoding="utf-8")

    def write_inventory(self, entries):
        self.inventory.write_text(
            json.dumps({"schema": 1, "deliverables": entries}), encoding="utf-8"
        )

    def errors(self):
        return self.run_validator()

    def run_validator(self):
        errors = []
        with mock.patch.multiple(
            config, ROOT=self.root, DELIVERABLES=self.inventory, WASM_TOOL_REGISTRY=self.registry
        ):
            config.validate_tool_compile_coverage(self.jobs, errors)
        return errors

    def test_plan_runner_covers_the_tool(self):
        self.assertEqual(self.errors(), [])

    def test_directory_building_job_covers_the_tool(self):
        self.jobs["wasm-tools-check"]["script"] = ["echo hollow"]
        self.write_inventory([deliverable(ci=["standalone-deliverables", "builds-x"])])
        self.assertEqual(self.errors(), [])

    def test_resolution_only_job_does_not_cover(self):
        self.write_inventory([deliverable(ci=["standalone-deliverables"])])
        errors = self.errors()
        self.assertTrue(any("names no blocking CI job" in error for error in errors), errors)

    def test_allow_failure_job_does_not_gate(self):
        self.jobs["wasm-tools-check"]["allow_failure"] = True
        self.assertTrue(self.errors())

    def test_template_and_unknown_jobs_do_not_cover(self):
        self.jobs[".wasm-tools-check"] = self.jobs.pop("wasm-tools-check")
        self.assertTrue(self.errors())

    def test_job_inheriting_allow_failure_does_not_gate(self):
        self.jobs[".observational-job"] = {"allow_failure": True}
        self.jobs["wasm-tools-check"]["extends"] = ".observational-job"
        self.assertTrue(self.errors())

    def test_unbuilt_status_is_skipped(self):
        for status in ("retired", "fixture", "staging", "experimental", "mirror"):
            with self.subTest(status=status):
                self.write_inventory([deliverable(status=status, ci=[])])
                self.assertEqual(self.errors(), [])

    def test_registry_tool_without_deliverable_fails(self):
        self.write_inventory([deliverable(name="other", manifest="ic/tools-src/other/Cargo.toml")])
        errors = self.errors()
        self.assertTrue(any("has no .gitlab/deliverables.json entry" in error for error in errors), errors)

    def test_registry_entry_without_source_dir_fails(self):
        self.write_registry({"name": "x"})
        self.assertTrue(any("source.dir" in error for error in self.errors()))

    def test_manifest_outside_the_registry_directory_does_not_count(self):
        other = self.root / "ic" / "tools-src" / "other"
        other.mkdir(parents=True)
        (other / "Cargo.toml").write_text("[package]\n", encoding="utf-8")
        self.write_inventory([deliverable(manifest="ic/tools-src/other/Cargo.toml")])
        self.assertTrue(self.errors())

    def test_real_tree_production_tools_are_covered(self):
        """The committed job must actually run the plan-driven runner."""
        merged = {}
        for path in (ROOT / ".gitlab" / "ci" / "defaults.yml", ROOT / ".gitlab" / "ci" / "integration.yml"):
            merged = config.merge_ci_hashes(merged, config.load_yaml(path))
        errors = []
        config.validate_tool_compile_coverage(merged, errors)
        self.assertEqual(errors, [])


if __name__ == "__main__":
    unittest.main()
