#!/usr/bin/env python3
"""Regression checks for the required gates that failed in pipeline #4679."""

import copy
import hashlib
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


class CaveponyReleaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.files = {
            "ic/registry/tools/cavepony-tool.json": json.dumps({"version": "0.4.3"}),
            "ic/tools-src/cavepony/cavepony-tool.capabilities.json": json.dumps({"version": "0.4.3"}),
            "ic/tools-src/cavepony/Cargo.toml": '[package]\nname = "cavepony-tool"\nversion = "0.4.3"\n',
            "ic/tools-src/cavepony/Cargo.lock": 'version = 4\n[[package]]\nname = "cavepony-tool"\nversion = "0.4.3"\n',
        }
        for relative, text in self.files.items():
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        self.jobs = {"cavepony-wasm": {"variables": {"CAVEPONY_EXPECTED_VERSION": "0.4.3"}}}

    def errors(self):
        errors = []
        with mock.patch.object(config, "ROOT", self.root):
            config.validate_cavepony_version(self.jobs, errors)
        return errors

    def test_consistent_release_passes(self):
        self.assertEqual(self.errors(), [])

    def test_each_stale_version_fails(self):
        for relative, text in self.files.items():
            with self.subTest(file=relative):
                path = self.root / relative
                path.write_text(text.replace("0.4.3", "0.4.2"), encoding="utf-8")
                self.assertTrue(any(relative in error for error in self.errors()))
                path.write_text(text, encoding="utf-8")
        self.jobs["cavepony-wasm"]["variables"]["CAVEPONY_EXPECTED_VERSION"] = "0.4.2"
        self.assertEqual(len(self.errors()), 4)

    def test_missing_or_malformed_metadata_fails(self):
        for relative, text in self.files.items():
            with self.subTest(file=relative):
                path = self.root / relative
                path.unlink()
                self.assertTrue(any(relative in error for error in self.errors()))
                path.write_text("not valid JSON or TOML", encoding="utf-8")
                self.assertTrue(any(relative in error for error in self.errors()))
                path.write_text(text, encoding="utf-8")

    def test_missing_or_invalid_ci_expectation_fails(self):
        for version in (None, "", "0.4", 43):
            with self.subTest(version=version):
                self.jobs["cavepony-wasm"]["variables"]["CAVEPONY_EXPECTED_VERSION"] = version
                self.assertTrue(self.errors())

    def test_missing_or_duplicate_local_package_fails(self):
        relative = "ic/tools-src/cavepony/Cargo.lock"
        path = self.root / relative
        text = self.files[relative]
        for invalid in (text.replace("cavepony-tool", "other-tool"), text + text[text.index("[[package]]"):]):
            with self.subTest(lock=invalid):
                path.write_text(invalid, encoding="utf-8")
                self.assertTrue(self.errors())


class FullValidationTests(unittest.TestCase):
    def setUp(self):
        self.defaults = config.load_yaml(ROOT / ".gitlab/ci/defaults.yml")
        self.merged = {}
        for include in config.EXPECTED_INCLUDES:
            self.merged.update(config.load_yaml(ROOT / include.lstrip("/")))

    def test_checked_in_rules_and_jobs_match_context_contract(self):
        errors = []
        config.validate_release_gates(self.merged, self.defaults, errors)
        config.validate_cavepony_version(self.merged, errors)
        config.validate_test_shapes(self.merged, errors)
        self.assertEqual(errors, [])

    def test_versioned_integration_mrs_get_full_validation(self):
        for target in ("master", "aragents/kimil00p/v2.1.4.0"):
            with self.subTest(target=target):
                self.assertTrue(config.matches_full_validation_context({"source": "merge_request_event", "target": target}))
        self.assertFalse(config.matches_full_validation_context({"source": "merge_request_event", "target": "feature/other"}))

    def test_scratch_branch_full_validation_is_opt_in(self):
        context = {"source": "push", "branch": "cmc/g1/v2.1.4.0"}
        self.assertFalse(config.matches_full_validation_context(context))
        self.assertTrue(config.matches_full_validation_context({**context, "full_validation": "1"}))
        self.assertFalse(config.matches_full_validation_context({**context, "full_validation": "0"}))

    def test_removing_integration_mr_rule_is_rejected(self):
        defaults = copy.deepcopy(self.defaults)
        rules = defaults[".full-validation-context"]["rules"]
        rules[:] = [rule for rule in rules if "CI_MERGE_REQUEST_TARGET_BRANCH_NAME =~" not in rule.get("if", "")]
        errors = []
        config.validate_release_gates(self.merged, defaults, errors)
        self.assertTrue(any("canonical predicate" in error for error in errors))

    def test_unlocked_cavepony_build_is_rejected(self):
        self.merged["cavepony-wasm"]["script"] = [command.replace(" --locked", "") for command in self.merged["cavepony-wasm"]["script"]]
        errors = []
        config.validate_test_shapes(self.merged, errors)
        self.assertTrue(any("cavepony-wasm" in error for error in errors))

    def test_full_validation_does_not_widen_worker_publication(self):
        errors = []
        config.validate_worker_publication(self.merged, errors)
        self.assertEqual(errors, [])
        for template in (".worker-candidate", ".worker-scan", ".worker-promotion"):
            for condition in config.rule_conditions(self.merged[template]):
                with self.subTest(template=template, condition=condition):
                    if condition:
                        self.assertIn('CI_COMMIT_REF_PROTECTED == "true"', condition)
                        self.assertIn('CI_PIPELINE_SOURCE == "push"', condition)
                        self.assertNotIn("LUNARWING_CI_FULL_VALIDATION", condition)


class WorkerVersionPinTests(unittest.TestCase):
    """The worker pin cross-check must bind values to each worker's own case
    block (opencode and opencode2 share the OPENCODE_REF/OPENCODE_VERSION arg
    names) and must also verify the provenance source_ref, not just the
    build_args lines (WI#303 review)."""

    PEBBLE_REF = "1" * 40
    OPENCODE_REF = "2" * 40
    OPENCODE2_REF = "3" * 40

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.build_script = self.root / "build-worker-image.sh"
        self.dockerfiles = {
            "PEBBLE_DOCKERFILE": self.root / "pebble.Dockerfile",
            "OPENCODE_DOCKERFILE": self.root / "opencode.Dockerfile",
            "OPENCODE2_DOCKERFILE": self.root / "opencode2.Dockerfile",
            "KIMI_DOCKERFILE": self.root / "kimi.Dockerfile",
        }
        lock = self.root / "pebble4lunarwing/upstream/Cargo.lock"
        lock.parent.mkdir(parents=True)
        lock.write_text('version = 4\n[[package]]\nname = "worker"\n', encoding="utf-8")
        lock_digest = hashlib.sha256(lock.read_bytes()).hexdigest()
        self.dockerfiles["PEBBLE_DOCKERFILE"].write_text(
            f"ARG PEBBLE_REF={self.PEBBLE_REF}\nARG PEBBLE_LOCK_SHA256={lock_digest}\n",
            encoding="utf-8",
        )
        self.dockerfiles["OPENCODE_DOCKERFILE"].write_text(
            f"ARG OPENCODE_VERSION=v1.18.25\nARG OPENCODE_REF={self.OPENCODE_REF}\n",
            encoding="utf-8",
        )
        self.dockerfiles["OPENCODE2_DOCKERFILE"].write_text(
            f"ARG OPENCODE_VERSION=v2.0.24\nARG OPENCODE_REF={self.OPENCODE2_REF}\n",
            encoding="utf-8",
        )
        self.dockerfiles["KIMI_DOCKERFILE"].write_text(
            "ARG KIMI_CODE_VERSION=2.1.1\n", encoding="utf-8"
        )
        kimi_lock = self.root / "kimi-cli/package-lock.json"
        kimi_lock.parent.mkdir(parents=True)
        kimi_lock.write_text(
            json.dumps({"packages": {"": {"dependencies": {"@moonshot-ai/kimi-code": "2.1.1"}}}}),
            encoding="utf-8",
        )
        self.kimi_lock = kimi_lock
        self.write_build_script(
            pebble_ref=self.PEBBLE_REF,
            opencode_ref=self.OPENCODE_REF,
            opencode2_ref=self.OPENCODE2_REF,
            opencode2_version="v2.0.24",
            kimi_version="2.1.1",
        )

    def write_build_script(self, *, pebble_ref, opencode_ref, opencode2_ref, opencode2_version, kimi_version):
        self.build_script.write_text(
            "case \"$1\" in\n"
            "    pebble)\n"
            f"        source_ref={pebble_ref}\n"
            f"        build_args=\"PEBBLE_REF={pebble_ref}\n"
            "SOURCE_REVISION=$CI_COMMIT_SHA\"\n"
            "        ;;\n"
            "    opencode)\n"
            f"        source_ref={opencode_ref}\n"
            "        build_args=\"OPENCODE_VERSION=v1.18.25\n"
            f"OPENCODE_REF={opencode_ref}\n"
            "SOURCE_REVISION=$CI_COMMIT_SHA\"\n"
            "        ;;\n"
            "    opencode2)\n"
            f"        source_ref={opencode2_ref}\n"
            f"        build_args=\"OPENCODE_VERSION={opencode2_version}\n"
            f"OPENCODE_REF={opencode2_ref}\n"
            "SOURCE_REVISION=$CI_COMMIT_SHA\"\n"
            "        ;;\n"
            "    kimi)\n"
            f"        source_ref={kimi_version}\n"
            f"        build_args=\"KIMI_CODE_VERSION={kimi_version}\n"
            "SOURCE_REVISION=$CI_COMMIT_SHA\"\n"
            "        ;;\n"
            "esac\n",
            encoding="utf-8",
        )

    def errors(self):
        errors = []
        with mock.patch.multiple(
            config,
            WORKER_BUILD_SCRIPT=self.build_script,
            KIMI_CLI_LOCK=self.kimi_lock,
            ROOT=self.root,
            **self.dockerfiles,
        ):
            config.validate_worker_versions(errors)
        return errors

    def test_consistent_pins_pass(self):
        self.assertEqual(self.errors(), [])

    def test_cross_lane_ref_swap_is_caught(self):
        # opencode2's build_args REF swapped to the v1 lane's SHA: a
        # whole-script search would still find a match in the opencode block.
        self.write_build_script(
            pebble_ref=self.PEBBLE_REF,
            opencode_ref=self.OPENCODE_REF,
            opencode2_ref=self.OPENCODE_REF,
            opencode2_version="v2.0.24",
            kimi_version="2.1.1",
        )
        self.assertTrue(any("opencode2" in error for error in self.errors()))

    def test_stale_source_ref_is_caught(self):
        # The build_args match the Dockerfile but the provenance source_ref
        # still points at the previous pin.
        self.write_build_script(
            pebble_ref=self.PEBBLE_REF,
            opencode_ref=self.OPENCODE_REF,
            opencode2_ref=self.OPENCODE2_REF,
            opencode2_version="v2.0.24",
            kimi_version="2.1.1",
        )
        text = self.build_script.read_text(encoding="utf-8").replace(
            f"        source_ref={self.OPENCODE2_REF}", f"        source_ref={self.OPENCODE_REF}"
        )
        self.build_script.write_text(text, encoding="utf-8")
        errors = self.errors()
        self.assertTrue(any("source_ref" in error and "opencode2" in error for error in errors))

    def test_missing_worker_block_is_caught(self):
        text = self.build_script.read_text(encoding="utf-8")
        start = text.index("    kimi)\n")
        end = text.index("        ;;\n", start) + len("        ;;\n")
        self.build_script.write_text(text[:start] + text[end:], encoding="utf-8")
        self.assertTrue(any("kimi" in error for error in self.errors()))


class BunImageTieTests(unittest.TestCase):
    """MR !1552 ER-5 ties CI_BUN_IMAGE to the worker Dockerfile's oven/bun
    FROM digest(s); MR !1566 EZ-3 makes that cross-check fail closed."""

    BUN_DIGEST = "9114c058aeae42162ee16dd5084b95fe9473970bb6bcb5b232ab1630f0546895"
    OTHER_DIGEST = "0" * 64

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.dockerfile = self.root / "Dockerfile"
        # Sibling fixture: the CI_NODE_IMAGE tie also runs here, so pin a
        # matching kimi Dockerfile to keep the bun cases hermetic.
        self.kimi_dockerfile = self.root / "Dockerfile.kimi"
        self.kimi_dockerfile.write_text(
            f"FROM docker.io/library/node:22-bookworm-slim@sha256:{self.OTHER_DIGEST} AS builder\n",
            encoding="utf-8",
        )
        pin = f"docker.io/oven/bun:debian@sha256:{self.BUN_DIGEST}"
        self.variables = {
            "CI_LINT_IMAGE": f"docker.io/library/python:3.12@sha256:{self.OTHER_DIGEST}",
            "CI_RUST_IMAGE": f"docker.io/library/rust:1.96.1@sha256:{self.OTHER_DIGEST}",
            "CI_GITLEAKS_IMAGE": f"docker.io/zricethezav/gitleaks:v8.30.1@sha256:{self.OTHER_DIGEST}",
            "CI_BUN_IMAGE": pin,
            "CI_NODE_IMAGE": f"docker.io/library/node:22-bookworm-slim@sha256:{self.OTHER_DIGEST}",
        }
        self.merged = {
            ".worker-candidate": {"image": "docker.io/moby/buildkit:v0.25.1-rootless@sha256:0c0ce4929976ace509f7a53878303504b2de8857829aea4932af9f7325a30f51"},
            ".worker-scan": {"image": "docker.io/aquasec/trivy:0.74.0@sha256:62b1e65e8869bc4b4c6aa4fa2b21595256c7c2f6018a9d9ad61caf87187c1969"},
            ".worker-promotion": {"image": "docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b"},
            "rust-test-postgres-integration": {
                "services": [{"name": f"docker.io/pgvector/pgvector:pg16@sha256:{self.OTHER_DIGEST}"}],
            },
        }

    def write_dockerfile(self, text: str) -> None:
        self.dockerfile.write_text(text, encoding="utf-8")

    def errors(self):
        errors = []
        with mock.patch.multiple(
            config,
            OPENCODE_DOCKERFILE=self.dockerfile,
            KIMI_DOCKERFILE=self.kimi_dockerfile,
        ):
            config.validate_ci_tool_image_pins(self.merged, self.variables, errors)
        return errors

    def test_matching_digests_pass(self):
        self.write_dockerfile(
            f"FROM docker.io/oven/bun:debian@sha256:{self.BUN_DIGEST} AS builder\n"
            f"FROM docker.io/oven/bun:debian@sha256:{self.BUN_DIGEST} AS runtime\n"
        )
        self.assertEqual(self.errors(), [])

    def test_digest_mismatch_is_caught(self):
        self.write_dockerfile(
            f"FROM docker.io/oven/bun:debian@sha256:{self.BUN_DIGEST[:-1]}0 AS builder\n"
        )
        self.assertTrue(any("CI_BUN_IMAGE digest must match" in error for error in self.errors()))

    def test_missing_dockerfile_is_caught(self):
        self.assertTrue(any("Dockerfile missing" in error for error in self.errors()))

    def test_no_bun_from_is_caught(self):
        self.write_dockerfile(f"FROM docker.io/library/python:3.12@sha256:{self.OTHER_DIGEST}\n")
        self.assertTrue(any("no oven/bun FROM" in error for error in self.errors()))

    def test_tag_only_bun_from_is_caught(self):
        self.write_dockerfile("FROM docker.io/oven/bun:debian AS builder\n")
        self.assertTrue(any("not pinned" in error for error in self.errors()))

    def test_uppercase_digest_bun_from_is_caught(self):
        self.write_dockerfile(
            f"FROM docker.io/oven/bun:debian@sha256:{self.BUN_DIGEST.upper()} AS builder\n"
        )
        self.assertTrue(any("not pinned" in error for error in self.errors()))


class NodeImageTieTests(unittest.TestCase):
    """WI#324 (MR !1590 review GB-2) ties CI_NODE_IMAGE to the kimi worker
    Dockerfile's node FROM digest(s), failing closed like the bun tie."""

    NODE_DIGEST = "83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5"
    BUN_DIGEST = "9114c058aeae42162ee16dd5084b95fe9473970bb6bcb5b232ab1630f0546895"
    OTHER_DIGEST = "0" * 64

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.dockerfile = self.root / "Dockerfile"
        # Sibling fixture: the CI_BUN_IMAGE tie also runs here, so pin a
        # matching opencode Dockerfile to keep the node cases hermetic.
        self.opencode_dockerfile = self.root / "Dockerfile.opencode"
        self.opencode_dockerfile.write_text(
            f"FROM docker.io/oven/bun:debian@sha256:{self.BUN_DIGEST} AS builder\n",
            encoding="utf-8",
        )
        self.variables = {
            "CI_LINT_IMAGE": f"docker.io/library/python:3.12@sha256:{self.OTHER_DIGEST}",
            "CI_RUST_IMAGE": f"docker.io/library/rust:1.96.1@sha256:{self.OTHER_DIGEST}",
            "CI_GITLEAKS_IMAGE": f"docker.io/zricethezav/gitleaks:v8.30.1@sha256:{self.OTHER_DIGEST}",
            "CI_BUN_IMAGE": f"docker.io/oven/bun:debian@sha256:{self.BUN_DIGEST}",
            "CI_NODE_IMAGE": f"docker.io/library/node:22-bookworm-slim@sha256:{self.NODE_DIGEST}",
        }
        self.merged = {
            ".worker-candidate": {"image": "docker.io/moby/buildkit:v0.25.1-rootless@sha256:0c0ce4929976ace509f7a53878303504b2de8857829aea4932af9f7325a30f51"},
            ".worker-scan": {"image": "docker.io/aquasec/trivy:0.74.0@sha256:62b1e65e8869bc4b4c6aa4fa2b21595256c7c2f6018a9d9ad61caf87187c1969"},
            ".worker-promotion": {"image": "docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b"},
            "rust-test-postgres-integration": {
                "services": [{"name": f"docker.io/pgvector/pgvector:pg16@sha256:{self.OTHER_DIGEST}"}],
            },
        }

    def write_dockerfile(self, text: str) -> None:
        self.dockerfile.write_text(text, encoding="utf-8")

    def errors(self):
        errors = []
        with mock.patch.multiple(
            config,
            OPENCODE_DOCKERFILE=self.opencode_dockerfile,
            KIMI_DOCKERFILE=self.dockerfile,
        ):
            config.validate_ci_tool_image_pins(self.merged, self.variables, errors)
        return errors

    def test_matching_digests_pass(self):
        # The real Dockerfile has two node stages (builder + runtime).
        self.write_dockerfile(
            f"FROM docker.io/library/node:22-bookworm-slim@sha256:{self.NODE_DIGEST} AS builder\n"
            f"FROM docker.io/library/node:22-bookworm-slim@sha256:{self.NODE_DIGEST} AS runtime\n"
        )
        self.assertEqual(self.errors(), [])

    def test_digest_mismatch_is_caught(self):
        self.write_dockerfile(
            f"FROM docker.io/library/node:22-bookworm-slim@sha256:{self.NODE_DIGEST[:-1]}0 AS builder\n"
        )
        self.assertTrue(any("CI_NODE_IMAGE digest must match" in error for error in self.errors()))

    def test_missing_dockerfile_is_caught(self):
        self.assertTrue(any("Dockerfile missing" in error for error in self.errors()))

    def test_no_node_from_is_caught(self):
        self.write_dockerfile(f"FROM docker.io/library/python:3.12@sha256:{self.OTHER_DIGEST}\n")
        self.assertTrue(any("no node FROM" in error for error in self.errors()))

    def test_nodejs_named_from_is_caught(self):
        # A non-node image whose name merely starts with "node" must fail
        # closed as "no node FROM line", not silently match the tie.
        self.write_dockerfile(f"FROM docker.io/library/nodejs:24@sha256:{self.NODE_DIGEST}\n")
        self.assertTrue(any("no node FROM" in error for error in self.errors()))

    def test_tag_only_node_from_is_caught(self):
        self.write_dockerfile("FROM docker.io/library/node:22-bookworm-slim AS builder\n")
        self.assertTrue(any("not pinned" in error for error in self.errors()))

    def test_uppercase_digest_node_from_is_caught(self):
        self.write_dockerfile(
            f"FROM docker.io/library/node:22-bookworm-slim@sha256:{self.NODE_DIGEST.upper()} AS builder\n"
        )
        self.assertTrue(any("not pinned" in error for error in self.errors()))


if __name__ == "__main__":
    unittest.main()
