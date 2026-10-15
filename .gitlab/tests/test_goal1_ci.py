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

    def test_kimi_block_must_not_pass_debian_snapshot(self):
        # MR !1627 review HE-7: a DEBIAN_SNAPSHOT build-arg would override
        # the Dockerfile default at build time and silently detach the
        # CI_DEBIAN_SNAPSHOT tie from what the image really builds with.
        text = self.build_script.read_text(encoding="utf-8")
        self.build_script.write_text(
            text.replace(
                '        build_args="KIMI_CODE_VERSION=2.1.1\n',
                '        build_args="KIMI_CODE_VERSION=2.1.1\nDEBIAN_SNAPSHOT=20260906T000000Z\n',
            ),
            encoding="utf-8",
        )
        self.assertTrue(any("DEBIAN_SNAPSHOT" in error for error in self.errors()))


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
        # Sibling fixture: the CI_NODE_IMAGE and CI_DEBIAN_SNAPSHOT ties also
        # run here, so pin a matching kimi Dockerfile to keep the bun cases
        # hermetic.
        self.kimi_dockerfile = self.root / "Dockerfile.kimi"
        self.kimi_dockerfile.write_text(
            "ARG DEBIAN_SNAPSHOT=20260906T000000Z\n"
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
            "CI_DEBIAN_SNAPSHOT": "20260906T000000Z",
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

    def test_two_stage_drift_is_caught(self):
        # Builder matches the variable but the runtime stage drifts: only a
        # set-equality comparison catches this; an any-match check would not.
        self.write_dockerfile(
            f"FROM docker.io/oven/bun:debian@sha256:{self.BUN_DIGEST} AS builder\n"
            f"FROM docker.io/oven/bun:debian@sha256:{self.OTHER_DIGEST} AS runtime\n"
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
    SNAPSHOT = "20260906T000000Z"

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
            "CI_DEBIAN_SNAPSHOT": self.SNAPSHOT,
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
        # The CI_DEBIAN_SNAPSHOT tie also runs here; every fixture carries a
        # matching valued ARG so only the node assertions vary.
        self.dockerfile.write_text(f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n" + text, encoding="utf-8")

    def errors(self):
        errors = []
        with mock.patch.multiple(
            config,
            OPENCODE_DOCKERFILE=self.opencode_dockerfile,
            KIMI_DOCKERFILE=self.dockerfile,
        ):
            config.validate_ci_tool_image_pins(self.merged, self.variables, errors)
        return errors

    def test_tag_only_ci_node_image_is_caught(self):
        # MR !1618 review GV-3: the trust-sensitive entry is the only check
        # that rejects a tag-only CI_NODE_IMAGE — the node tie above runs
        # only when a digest is present, so a tag silently skips it.
        self.variables["CI_NODE_IMAGE"] = "docker.io/library/node:22-bookworm-slim"
        self.write_dockerfile(
            f"FROM docker.io/library/node:22-bookworm-slim@sha256:{self.NODE_DIGEST} AS builder\n"
        )
        self.assertTrue(
            any(
                "CI_NODE_IMAGE" in error and "digest-pinned" in error
                for error in self.errors()
            )
        )

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

    def test_two_stage_drift_is_caught(self):
        # MR !1601 review GH-3: builder matches the variable but the runtime
        # stage drifts — only set equality catches this; an any-match
        # comparison (`var_digest in dockerfile_digests`) would accept it.
        self.write_dockerfile(
            f"FROM docker.io/library/node:22-bookworm-slim@sha256:{self.NODE_DIGEST} AS builder\n"
            f"FROM docker.io/library/node:22-bookworm-slim@sha256:{self.OTHER_DIGEST} AS runtime\n"
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


class DebianSnapshotTieTests(unittest.TestCase):
    """MR !1618 review GV-4 ties CI_DEBIAN_SNAPSHOT to the valued
    ARG DEBIAN_SNAPSHOT in the kimi worker Dockerfile, so the worker-kimi-ts
    lane's python3 comes from the same pinned snapshot the image builds
    with — failing closed like the image ties."""

    NODE_DIGEST = "83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5"
    BUN_DIGEST = "9114c058aeae42162ee16dd5084b95fe9473970bb6bcb5b232ab1630f0546895"
    OTHER_DIGEST = "0" * 64
    SNAPSHOT = "20260906T000000Z"

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.dockerfile = self.root / "Dockerfile"
        # Sibling fixture: the CI_BUN_IMAGE tie also runs here, so pin a
        # matching opencode Dockerfile to keep the snapshot cases hermetic.
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
            "CI_DEBIAN_SNAPSHOT": self.SNAPSHOT,
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

    def node_from(self) -> str:
        return f"FROM docker.io/library/node:22-bookworm-slim@sha256:{self.NODE_DIGEST} AS builder\n"

    def errors(self):
        errors = []
        with mock.patch.multiple(
            config,
            OPENCODE_DOCKERFILE=self.opencode_dockerfile,
            KIMI_DOCKERFILE=self.dockerfile,
        ):
            config.validate_ci_tool_image_pins(self.merged, self.variables, errors)
        return errors

    def test_matching_snapshot_passes(self):
        # The realistic builder-stage shape: valued ARG, bare redeclaration,
        # and RUN lines referencing ${DEBIAN_SNAPSHOT} (HE-3 pass-side pin).
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + "ARG DEBIAN_SNAPSHOT\n"
            + "RUN printf '%s\\n' \\\n"
            + '      "deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT} bookworm main" \\\n'
            + '      "deb http://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT} bookworm-security main" \\\n'
            + "      > /etc/apt/sources.list\n"
        )
        self.assertEqual(self.errors(), [])

    def test_snapshot_mismatch_is_caught(self):
        self.write_dockerfile("ARG DEBIAN_SNAPSHOT=20261001T000000Z\n" + self.node_from())
        self.assertTrue(any("CI_DEBIAN_SNAPSHOT must match" in error for error in self.errors()))

    def test_malformed_snapshot_is_caught(self):
        self.variables["CI_DEBIAN_SNAPSHOT"] = "latest"
        self.write_dockerfile(f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n" + self.node_from())
        self.assertTrue(
            any(
                "CI_DEBIAN_SNAPSHOT" in error and "timestamp" in error
                for error in self.errors()
            )
        )

    def test_missing_arg_is_caught(self):
        self.write_dockerfile(self.node_from())
        self.assertTrue(any("no valued ARG DEBIAN_SNAPSHOT" in error for error in self.errors()))

    def test_two_stage_drift_is_caught(self):
        # A second stage pinning a different snapshot must fail the tie even
        # when the first valued ARG matches (same set-equality hole as GH-3).
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + "ARG DEBIAN_SNAPSHOT=20261001T000000Z\n"
            + self.node_from()
        )
        self.assertTrue(any("CI_DEBIAN_SNAPSHOT must match" in error for error in self.errors()))

    def test_lowercase_arg_instruction_is_caught(self):
        # MR !1627 review HE-3: Dockerfile instructions are case-insensitive,
        # so a lowercase stage-level `arg` still steers the build.
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + "arg DEBIAN_SNAPSHOT=20270101T000000Z\n"
        )
        self.assertTrue(any("CI_DEBIAN_SNAPSHOT must match" in error for error in self.errors()))
        # …while a lowercase ARG carrying the correct value must pass.
        self.write_dockerfile(f"arg DEBIAN_SNAPSHOT={self.SNAPSHOT}\n" + self.node_from())
        self.assertEqual(self.errors(), [])

    def test_indented_arg_is_caught(self):
        # HE-3: an indented divergent ARG was invisible to the column-0 match.
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + "  ARG DEBIAN_SNAPSHOT=20270101T000000Z\n"
        )
        self.assertTrue(any("CI_DEBIAN_SNAPSHOT must match" in error for error in self.errors()))

    def test_quoted_arg_value_passes(self):
        # HE-3: a quoted correct value must not false-error as a mismatch.
        self.write_dockerfile(
            f'ARG DEBIAN_SNAPSHOT="{self.SNAPSHOT}"\n' + self.node_from()
        )
        self.assertEqual(self.errors(), [])

    def test_hardcoded_snapshot_url_is_caught(self):
        # HE-3: the tie checked the ARG value but not its use — a RUN that
        # hardcodes a snapshot timestamp builds from it regardless of the
        # ARG, even when the hardcoded timestamp equals the pinned value.
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + "RUN printf '%s\\n' "
            + f'"deb http://snapshot.debian.org/archive/debian/{self.SNAPSHOT}/ bookworm main" '
            + "> /etc/apt/sources.list\n"
        )
        self.assertTrue(
            any("must use ${DEBIAN_SNAPSHOT}" in error for error in self.errors())
        )

    def test_renamed_snapshot_variable_is_caught(self):
        # HE-3: a URL referencing a different variable is equally detached.
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + "RUN printf '%s\\n' "
            + '"deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOTS}/ bookworm main" '
            + "> /etc/apt/sources.list\n"
        )
        self.assertTrue(
            any("must use ${DEBIAN_SNAPSHOT}" in error for error in self.errors())
        )


class WorkerTsLaneTests(unittest.TestCase):
    """MR !1601 review GH-2: both worker TS lanes must be pinned — existence,
    the merge_request_event rule, the tied image variable, and the core
    script fragments (including the kimi python3 provisioning, GH-1)."""

    def lane(self, image_variable: str, commands: list[str]) -> dict:
        return {
            "stage": "validate",
            "image": {"name": image_variable, "entrypoint": [""]},
            "rules": [
                {"if": '$CI_PIPELINE_SOURCE == "merge_request_event"'},
                {"if": '$CI_PIPELINE_SOURCE == "push" && $CI_COMMIT_BRANCH'},
                {"when": "never"},
            ],
            "script": commands,
        }

    def merged(self) -> dict:
        return {
            "worker-kimi-ts": self.lane(
                "$CI_NODE_IMAGE",
                [
                    "rm -f /etc/apt/sources.list.d/debian.sources",
                    'printf \'%s\\n\' "deb http://snapshot.debian.org/archive/debian/${CI_DEBIAN_SNAPSHOT}/ bookworm main" > /etc/apt/sources.list',
                    "apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=5 -o Acquire::http::Timeout=60 update && apt-get install -y --no-install-recommends python3",
                    "cd kimicode4lunarwing/scripts",
                    "npm ci --ignore-scripts",
                    "npx tsc -p tsconfig.json",
                    "npm test",
                ],
            ),
            "worker-opencode-ts": self.lane(
                "$CI_BUN_IMAGE",
                [
                    "cd opencode4lunarwing",
                    "bun install --frozen-lockfile",
                    "bun scripts/path_expand_test.ts",
                    "./node_modules/.bin/tsc --noEmit -p scripts/tsconfig.json",
                ],
            ),
        }

    def errors(self, merged: dict) -> list[str]:
        errors: list[str] = []
        config.validate_worker_ts_lanes(merged, errors)
        return errors

    def test_valid_shape_passes(self):
        self.assertEqual(self.errors(self.merged()), [])

    def test_missing_kimi_lane_is_caught(self):
        merged = self.merged()
        del merged["worker-kimi-ts"]
        self.assertTrue(any("worker-kimi-ts job missing" in error for error in self.errors(merged)))

    def test_missing_opencode_lane_is_caught(self):
        merged = self.merged()
        del merged["worker-opencode-ts"]
        self.assertTrue(any("worker-opencode-ts job missing" in error for error in self.errors(merged)))

    def test_never_rules_are_caught(self):
        merged = self.merged()
        merged["worker-kimi-ts"]["rules"] = [{"when": "never"}]
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_mr_rule_when_never_is_caught(self):
        # MR !1618 review GV-1: the MR rule exists but disables the lane.
        merged = self.merged()
        merged["worker-kimi-ts"]["rules"][0]["when"] = "never"
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_mr_rule_when_manual_is_caught(self):
        # GV-1: a manual MR rule runs the lane only on demand — it cannot gate.
        merged = self.merged()
        merged["worker-kimi-ts"]["rules"][0]["when"] = "manual"
        merged["worker-kimi-ts"]["rules"][0]["allow_failure"] = False
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_job_level_allow_failure_is_caught(self):
        # GV-1: an allowed-to-fail lane runs on MRs but cannot gate.
        merged = self.merged()
        merged["worker-kimi-ts"]["allow_failure"] = True
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_shadowing_unconditional_never_is_caught(self):
        # GV-1: first match wins — an unconditional rule before the MR rule
        # shadows it, so the lane never runs on MRs.
        merged = self.merged()
        merged["worker-kimi-ts"]["rules"].insert(0, {"when": "never"})
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_job_level_when_manual_is_caught(self):
        # MR !1627 review HE-1 (R1): the MR rule carries no `when`, so a
        # job-level `when: manual` inherits into it — the lane never runs
        # unless clicked, even with allow_failure: false.
        merged = self.merged()
        merged["worker-kimi-ts"]["when"] = "manual"
        merged["worker-kimi-ts"]["allow_failure"] = False
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_job_level_when_never_is_caught(self):
        merged = self.merged()
        merged["worker-kimi-ts"]["when"] = "never"
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_catch_all_shadow_rule_is_caught(self):
        # HE-1 (R2): an always-true `if` rule placed first shadows the MR
        # rule; the substring check could not see it.
        merged = self.merged()
        merged["worker-kimi-ts"]["rules"].insert(
            0, {"if": "$CI_PIPELINE_SOURCE", "when": "never"}
        )
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_exists_shadow_rule_is_caught(self):
        # HE-1 (R2): same shadow via exists: — .gitlab-ci.yml always exists.
        merged = self.merged()
        merged["worker-kimi-ts"]["rules"].insert(
            0, {"exists": [".gitlab-ci.yml"], "when": "never"}
        )
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_changes_shadow_rule_is_caught(self):
        # HE-1 (R2): same shadow via changes: — **/* matches every push/MR.
        merged = self.merged()
        merged["worker-kimi-ts"]["rules"].insert(
            0, {"changes": ["**/*"], "when": "never"}
        )
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_inverted_mr_predicate_is_caught(self):
        # HE-1 (R3): the substring "merge_request_event" is still present,
        # but the lane then runs on everything EXCEPT merge requests.
        merged = self.merged()
        merged["worker-kimi-ts"]["rules"][0]["if"] = (
            '$CI_PIPELINE_SOURCE != "merge_request_event"'
        )
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_narrowed_mr_predicate_is_caught(self):
        # HE-1 (R3): an &&-narrowed predicate no longer matches every MR.
        merged = self.merged()
        merged["worker-kimi-ts"]["rules"][0]["if"] = (
            '$CI_PIPELINE_SOURCE == "merge_request_event" '
            '&& $CI_MERGE_REQUEST_TARGET_BRANCH_NAME == "no-such"'
        )
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_mr_rule_extra_keys_are_caught(self):
        # HE-1: the first rule must be the plain MR predicate; an extra
        # changes:/exists: key can narrow or shadow it.
        merged = self.merged()
        merged["worker-kimi-ts"]["rules"][0]["changes"] = ["kimicode4lunarwing/**"]
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_mr_predicate_spacing_and_quotes_normalised(self):
        # Formatting variants of the canonical predicate still pass.
        merged = self.merged()
        merged["worker-kimi-ts"]["rules"][0]["if"] = (
            "$CI_PIPELINE_SOURCE=='merge_request_event'"
        )
        self.assertEqual(self.errors(merged), [])

    def test_only_except_are_caught(self):
        # HE-1: only/except bypass the rules walk entirely.
        merged = self.merged()
        merged["worker-kimi-ts"]["only"] = ["merge_requests"]
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_extends_observational_job_is_caught(self):
        # HE-1 (R4): extending .observational-job (defaults.yml) inherits
        # allow_failure: true — the lane runs on MRs but cannot gate.
        merged = self.merged()
        merged[".observational-job"] = {
            "allow_failure": True,
            "rules": [{"if": '$CI_PIPELINE_SOURCE == "schedule"'}, {"when": "never"}],
        }
        merged["worker-kimi-ts"]["extends"] = ".observational-job"
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_extends_inherited_when_manual_is_caught(self):
        merged = self.merged()
        merged[".manual-parent"] = {"when": "manual"}
        merged["worker-kimi-ts"]["extends"] = ".manual-parent"
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_extends_job_keys_win(self):
        # GitLab merge semantics: the job's own keys override the parent's,
        # so an explicit allow_failure: false undoes the inherited one.
        merged = self.merged()
        merged[".observational-job"] = {"allow_failure": True}
        merged["worker-kimi-ts"]["extends"] = ".observational-job"
        merged["worker-kimi-ts"]["allow_failure"] = False
        self.assertEqual(self.errors(merged), [])

    def test_last_listed_parent_wins(self):
        # GitLab multi-parent merge is a reverse deep merge: keys from the
        # last listed parent override earlier parents.
        merged = self.merged()
        merged[".gating"] = {"allow_failure": False}
        merged[".observational"] = {"allow_failure": True}
        merged["worker-kimi-ts"]["extends"] = [".gating", ".observational"]
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))
        merged["worker-kimi-ts"]["extends"] = [".observational", ".gating"]
        self.assertEqual(self.errors(merged), [])

    def test_extends_self_cycle_still_applies_parent_keys(self):
        # A cyclic extends chain must terminate (depth-bounded) and still
        # surface the parent's gating keys.
        merged = self.merged()
        merged[".loopy"] = {"extends": ".loopy", "allow_failure": True}
        merged["worker-kimi-ts"]["extends"] = ".loopy"
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_needs_dependency_is_caught(self):
        # HE-1/HE-2: a non-empty needs can hold the lane behind a manual or
        # absent job; the lanes must keep needs: [].
        merged = self.merged()
        merged["worker-kimi-ts"]["needs"] = [{"job": "ci-config", "artifacts": False}]
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_before_script_is_caught(self):
        # HE-2: a before_script can neutralise anything before the pinned
        # commands run (e.g. before_script: [exit 0]).
        merged = self.merged()
        merged["worker-kimi-ts"]["before_script"] = ["exit 0"]
        self.assertTrue(any("before_script" in error for error in self.errors(merged)))

    def test_image_entrypoint_override_is_caught(self):
        # HE-2: image.entrypoint: ["/bin/true"] would run nothing at all;
        # [""] (the real tree's value) stays accepted.
        merged = self.merged()
        merged["worker-kimi-ts"]["image"]["entrypoint"] = ["/bin/true"]
        self.assertTrue(any("entrypoint" in error for error in self.errors(merged)))

    def test_commented_out_fragment_is_caught(self):
        # HE-2: the substring check accepted a fragment inside a comment.
        merged = self.merged()
        merged["worker-kimi-ts"]["script"] = [
            "# npm test" if command == "npm test" else command
            for command in merged["worker-kimi-ts"]["script"]
        ]
        self.assertTrue(any("npm test" in error for error in self.errors(merged)))

    def test_true_or_prefixed_fragment_is_caught(self):
        # HE-2: `true || npm test` contains the fragment but never runs it.
        merged = self.merged()
        merged["worker-kimi-ts"]["script"] = [
            "true || npm test" if command == "npm test" else command
            for command in merged["worker-kimi-ts"]["script"]
        ]
        self.assertTrue(any("npm test" in error for error in self.errors(merged)))

    def test_or_true_suffixed_fragment_is_caught(self):
        # HE-2: `npm test || true` runs the suite but cannot fail the lane.
        merged = self.merged()
        merged["worker-kimi-ts"]["script"] = [
            "npm test || true" if command == "npm test" else command
            for command in merged["worker-kimi-ts"]["script"]
        ]
        self.assertTrue(any("npm test" in error for error in self.errors(merged)))

    def test_echoed_fragment_is_caught(self):
        # HE-2: `echo npm test` contains the fragment but runs nothing.
        merged = self.merged()
        merged["worker-kimi-ts"]["script"] = [
            "echo npm test" if command == "npm test" else command
            for command in merged["worker-kimi-ts"]["script"]
        ]
        self.assertTrue(any("npm test" in error for error in self.errors(merged)))

    def test_fragment_with_trailing_args_counts(self):
        # `npm test -- --grep x` still starts with the pinned command.
        merged = self.merged()
        merged["worker-kimi-ts"]["script"] = [
            "npm test -- --grep worker" if command == "npm test" else command
            for command in merged["worker-kimi-ts"]["script"]
        ]
        self.assertEqual(self.errors(merged), [])

    def test_fragment_on_later_line_of_one_command_counts(self):
        # A fragment at the start of a later physical line inside one
        # multi-line YAML string still counts as a command start.
        merged = self.merged()
        script = merged["worker-kimi-ts"]["script"]
        index = script.index("npm test")
        script[index - 1] = script[index - 1] + "\nnpm test"
        del script[index]
        self.assertEqual(self.errors(merged), [])

    def test_missing_apt_retries_are_caught(self):
        # HE-4: reverting the snapshot fetch to a retry-less apt-get update
        # drops the pinned provisioning fragment.
        merged = self.merged()
        merged["worker-kimi-ts"]["script"] = [
            command.replace(" -o Acquire::Retries=5 -o Acquire::http::Timeout=60", "")
            for command in merged["worker-kimi-ts"]["script"]
        ]
        self.assertTrue(any("Acquire::Retries" in error for error in self.errors(merged)))

    def test_main_runs_the_lane_validator(self):
        # MR !1618 review GV-2: every unit test above calls the validator
        # directly, so deleting the main() call passed the whole suite.
        # MR !1627 review HE-7: also assert main()'s return value so an
        # erroring validator cannot pass this pin unnoticed.
        with mock.patch.object(config, "validate_worker_ts_lanes") as lane_check:
            self.assertEqual(config.main(), 0)
        lane_check.assert_called_once()

    def test_wrong_image_variable_is_caught(self):
        merged = self.merged()
        merged["worker-kimi-ts"]["image"]["name"] = "$CI_BUN_IMAGE"
        self.assertTrue(any("CI_NODE_IMAGE" in error for error in self.errors(merged)))

    def test_missing_python3_provisioning_is_caught(self):
        merged = self.merged()
        merged["worker-kimi-ts"]["script"] = [
            command
            for command in merged["worker-kimi-ts"]["script"]
            if "python3" not in command
        ]
        self.assertTrue(any("python3" in error for error in self.errors(merged)))

    def test_missing_snapshot_provisioning_is_caught(self):
        # MR !1618 review GV-4: reverting the lane's python3 source to the
        # live deb.debian.org mirror must fail the fragment check.
        merged = self.merged()
        merged["worker-kimi-ts"]["script"] = [
            command
            for command in merged["worker-kimi-ts"]["script"]
            if "snapshot.debian.org" not in command
        ]
        self.assertTrue(any("snapshot.debian.org" in error for error in self.errors(merged)))

    def test_missing_npm_test_fragment_is_caught(self):
        merged = self.merged()
        merged["worker-kimi-ts"]["script"] = [
            command for command in merged["worker-kimi-ts"]["script"] if command != "npm test"
        ]
        self.assertTrue(any("npm test" in error for error in self.errors(merged)))

    def test_missing_bun_install_fragment_is_caught(self):
        merged = self.merged()
        merged["worker-opencode-ts"]["script"] = [
            command
            for command in merged["worker-opencode-ts"]["script"]
            if "bun install" not in command
        ]
        self.assertTrue(any("bun install" in error for error in self.errors(merged)))


if __name__ == "__main__":
    unittest.main()
