#!/usr/bin/env python3
"""Regression checks for the required gates that failed in pipeline #4679."""

import copy
import datetime
import hashlib
import importlib.util
import io
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

    def test_dropping_cron_retention_from_postgres_lane_is_rejected(self):
        self.merged["rust-test-postgres-integration"]["script"] = [
            command.replace(" --test postgres_cron_retention", "")
            for command in self.merged["rust-test-postgres-integration"]["script"]
        ]
        errors = []
        config.validate_test_shapes(self.merged, errors)
        self.assertTrue(
            any("rust-test-postgres-integration" in error for error in errors),
            errors,
        )

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
            f"ARG OPENCODE_VERSION=v2.1.2\nARG OPENCODE_REF={self.OPENCODE2_REF}\n",
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
        # HN-7 fixtures: the DEBIAN_SNAPSHOT build-override guard also reads
        # the mt-admin tenant build script and the harness compose file.
        mt_admin_build = self.root / "ic/scripts/mt-admin-lib/05-tenant-build.sh"
        mt_admin_build.parent.mkdir(parents=True)
        mt_admin_build.write_text("# tenant image builds\n", encoding="utf-8")
        harness_compose = self.root / "tests/docker-compose.test.yml"
        harness_compose.parent.mkdir(parents=True)
        harness_compose.write_text("services: {}\n", encoding="utf-8")
        # IF-3 fixture: the kimi worker's own compose file is the fourth
        # guarded build entrypoint (MR !1665 review).
        kimi_compose = self.root / "kimicode4lunarwing/docker-compose.yml"
        kimi_compose.parent.mkdir(parents=True)
        kimi_compose.write_text("services: {}\n", encoding="utf-8")
        self.write_build_script(
            pebble_ref=self.PEBBLE_REF,
            opencode_ref=self.OPENCODE_REF,
            opencode2_ref=self.OPENCODE2_REF,
            opencode2_version="v2.1.2",
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
            opencode2_version="v2.1.2",
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
            opencode2_version="v2.1.2",
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

    def test_snapshot_token_outside_the_kimi_block_is_caught(self):
        # MR !1644 review HN-7: the guard covered one block of one file — a
        # build-arg in the shared post-case build_args or inside
        # build_image() passed unseen; the whole file is now guarded.
        text = self.build_script.read_text(encoding="utf-8")
        self.build_script.write_text(
            text + '    build_args="$build_args\nDEBIAN_SNAPSHOT=20260906T000000Z"\n',
            encoding="utf-8",
        )
        self.assertTrue(any("DEBIAN_SNAPSHOT" in error for error in self.errors()))

    def test_snapshot_token_in_mt_admin_build_is_caught(self):
        # HN-7: mt-admin builds the tenant worker images too, so its build
        # script is guarded the same way.
        path = self.root / "ic/scripts/mt-admin-lib/05-tenant-build.sh"
        path.write_text(
            'podman build --build-arg DEBIAN_SNAPSHOT=20260906T000000Z kimi\n',
            encoding="utf-8",
        )
        self.assertTrue(any("05-tenant-build.sh" in error for error in self.errors()))

    def test_snapshot_token_in_harness_compose_is_caught(self):
        # HN-7: the harness compose file is the third build entrypoint.
        path = self.root / "tests/docker-compose.test.yml"
        path.write_text(
            "services:\n  kimi:\n    build:\n      args:\n        DEBIAN_SNAPSHOT: x\n",
            encoding="utf-8",
        )
        self.assertTrue(any("docker-compose.test.yml" in error for error in self.errors()))

    def test_snapshot_token_in_kimi_compose_is_caught(self):
        # MR !1665 review IF-3 (P8): the kimi worker's own docker-compose.yml
        # builds the image with build.args — a DEBIAN_SNAPSHOT entry there is
        # the same build-time override the HN-7 guard exists to refuse.
        path = self.root / "kimicode4lunarwing/docker-compose.yml"
        path.write_text(
            "services:\n  lunarwing-worker:\n    build:\n      args:\n"
            "        DEBIAN_SNAPSHOT: 20200101T000000Z\n",
            encoding="utf-8",
        )
        self.assertTrue(any("kimicode4lunarwing/docker-compose.yml" in error for error in self.errors()))

    def test_missing_build_entrypoint_fails_closed(self):
        # HN-7: a vanished build entrypoint must error, not skip the guard.
        (self.root / "ic/scripts/mt-admin-lib/05-tenant-build.sh").unlink()
        self.assertTrue(any("05-tenant-build.sh missing" in error for error in self.errors()))


class WorkerInputManifestTests(unittest.TestCase):
    """Item 35: the kimi worker-inputs manifest must cover every build-read
    input (CY-4 precedent — the builder stage compiles and executes every
    scripts/*.ts and the in-build test gate reads entrypoint.sh, hooks/ and
    config/), and every listed entry in every lane must be a safe relative
    path that exists under the repo root, so a stale or escaping entry fails
    at ci-config time instead of the manual protected candidate build."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.kimi_files = sorted(config.KIMI_BUILD_INPUT_CORE) + [
            "kimicode4lunarwing/scripts/lunarwing_bridge.ts",
            "kimicode4lunarwing/scripts/lunarwing_bridge_test.ts",
        ]
        for relative in self.kimi_files:
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("placeholder\n", encoding="utf-8")
        pebble_dockerfile = self.root / "pebble4lunarwing/Dockerfile"
        pebble_dockerfile.parent.mkdir(parents=True, exist_ok=True)
        pebble_dockerfile.write_text("FROM scratch\n", encoding="utf-8")
        self.build_script = self.root / "build-worker-image.sh"
        self.write_build_script(kimi_files=self.kimi_files)

    def write_build_script(self, *, kimi_files, pebble_files=("pebble4lunarwing/Dockerfile",)):
        kimi_list = "\n".join(kimi_files)
        pebble_list = "\n".join(pebble_files)
        self.build_script.write_text(
            "case \"$1\" in\n"
            "    pebble)\n"
            f"        input_files='{pebble_list}'\n"
            "        ;;\n"
            "    kimi)\n"
            f"        input_files='{kimi_list}'\n"
            "        ;;\n"
            "esac\n",
            encoding="utf-8",
        )

    def errors(self):
        errors = []
        with mock.patch.multiple(
            config, WORKER_BUILD_SCRIPT=self.build_script, ROOT=self.root
        ):
            config.validate_worker_input_manifest(errors)
        return errors

    def test_complete_manifest_passes(self):
        self.assertEqual(self.errors(), [])

    def test_missing_listed_file_is_caught(self):
        (self.root / "pebble4lunarwing/Dockerfile").unlink()
        self.assertTrue(
            any("pebble" in error and "does not exist" in error for error in self.errors())
        )

    def test_unsafe_input_path_is_caught(self):
        self.write_build_script(kimi_files=self.kimi_files, pebble_files=("../outside",))
        self.assertTrue(
            any("pebble" in error and "safe relative" in error for error in self.errors())
        )

    def test_lane_without_input_files_is_caught(self):
        self.build_script.write_text(
            "case \"$1\" in\n"
            "    pebble)\n"
            "        source_ref=abc\n"
            "        ;;\n"
            "esac\n",
            encoding="utf-8",
        )
        self.assertTrue(
            any("pebble" in error and "input_files" in error for error in self.errors())
        )

    def test_kimi_core_input_omission_is_caught(self):
        reduced = [f for f in self.kimi_files if not f.endswith("entrypoint.sh")]
        self.write_build_script(kimi_files=reduced)
        self.assertTrue(
            any(
                "omits build-read input" in error and "entrypoint.sh" in error
                for error in self.errors()
            )
        )

    def test_kimi_runtime_input_omissions_are_caught(self):
        # IL-1: literal paths keep this independent of the core tuple used
        # by the fixture; dropping either required entry must fail this pin.
        for filename in ("health_server.py", "agent_comm_protocol.json"):
            with self.subTest(filename=filename):
                relative = f"kimicode4lunarwing/{filename}"
                self.write_build_script(
                    kimi_files=[path for path in self.kimi_files if path != relative]
                )
                self.assertTrue(any(
                    "omits build-read input" in error and relative in error
                    for error in self.errors()
                ))

    def test_second_input_files_assignment_is_caught(self):
        # IL-3: the shell uses the last assignment, even though the parser
        # can still see the complete first list. Prefixes/+= count too.
        before, after = self.build_script.read_text(encoding="utf-8").rsplit(
            "        ;;", 1
        )
        for assignment in (
            "input_files=kimicode4lunarwing/Dockerfile",
            "input_files='kimicode4lunarwing/Dockerfile'",
            'input_files="kimicode4lunarwing/Dockerfile"',
            "export input_files=kimicode4lunarwing/Dockerfile",
            "declare -x input_files=kimicode4lunarwing/Dockerfile",
            "readonly input_files=kimicode4lunarwing/Dockerfile",
            "input_files+=x",
            "export input_files+=x",
        ):
            with self.subTest(assignment=assignment):
                self.build_script.write_text(
                    f"{before}        {assignment}\n        ;;{after}", encoding="utf-8"
                )
                self.assertTrue(any(
                    "kimi block" in error and "exactly once" in error
                    for error in self.errors()
                ))

    def test_input_files_assignment_outside_worker_blocks_is_caught(self):
        original = self.build_script.read_text(encoding="utf-8")
        assignment = "input_files=kimicode4lunarwing/Dockerfile\n"
        for text in (assignment + original, original + assignment):
            with self.subTest(text=text):
                self.build_script.write_text(text, encoding="utf-8")
                self.assertTrue(any("exactly once" in error for error in self.errors()))

    def test_assignment_counts_cannot_balance_across_workers(self):
        # A global count alone would miss a duplicate in kimi balanced by a
        # missing declaration in pebble. Check each block independently.
        text = self.build_script.read_text(encoding="utf-8").replace(
            "        input_files='pebble4lunarwing/Dockerfile'\n", ""
        ).replace(
            "    kimi)\n", "    kimi)\n        input_files=kimicode4lunarwing/Dockerfile\n"
        )
        self.build_script.write_text(text, encoding="utf-8")
        errors = self.errors()
        for worker in ("pebble", "kimi"):
            with self.subTest(worker=worker):
                self.assertTrue(any(
                    f"{worker} block" in error and "exactly once" in error for error in errors
                ))

    def test_commented_input_files_assignments_are_ignored(self):
        text = self.build_script.read_text(encoding="utf-8").replace(
            "    kimi)\n",
            "    kimi)\n        # input_files=old\n        # export input_files+=old\n",
        )
        self.build_script.write_text(text, encoding="utf-8")
        self.assertEqual(self.errors(), [])

    def test_unlisted_kimi_bridge_source_is_caught(self):
        # A new scripts/*.ts appears on disk (the Dockerfile's COPY glob picks
        # it up) but never joins the manifest.
        new_source = self.root / "kimicode4lunarwing/scripts/new_bridge.ts"
        new_source.write_text("// placeholder\n", encoding="utf-8")
        self.assertTrue(any("new_bridge.ts" in error for error in self.errors()))

    def test_missing_kimi_block_defers_to_the_version_check(self):
        self.build_script.write_text(
            "case \"$1\" in\n"
            "    pebble)\n"
            "        input_files='pebble4lunarwing/Dockerfile'\n"
            "        ;;\n"
            "esac\n",
            encoding="utf-8",
        )
        self.assertEqual(self.errors(), [])

    def test_real_manifest_matches_the_real_tree(self):
        # Binds the check to the checked-in build script and worker tree, so
        # a build-worker-image.sh edit without a manifest update fails here
        # (and in ci-config via main()).
        errors = []
        config.validate_worker_input_manifest(errors)
        self.assertEqual(errors, [])


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
        # Sibling fixture: the opencode2 bun tie (MR !1669 review IK-2) also
        # runs here; patch it too so the tests never read the real file
        # (whose digest happens to equal BUN_DIGEST and would pass silently).
        self.opencode2_dockerfile = self.root / "Dockerfile.opencode2"
        self.opencode2_dockerfile.write_text(
            f"FROM docker.io/oven/bun:debian@sha256:{self.BUN_DIGEST} AS builder\n",
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
            OPENCODE2_DOCKERFILE=self.opencode2_dockerfile,
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

    def test_opencode2_bun_drift_is_caught(self):
        # MR !1669 review IK-2: the worker-opencode2-ts lane runs on
        # CI_BUN_IMAGE, so the opencode2 Dockerfile's bun digest must be
        # cross-checked too.
        self.opencode2_dockerfile.write_text(
            f"FROM docker.io/oven/bun:debian@sha256:{self.OTHER_DIGEST} AS builder\n",
            encoding="utf-8",
        )
        self.assertTrue(
            any(
                "CI_BUN_IMAGE digest must match" in error and "opencode2.4lunarwing" in error
                for error in self.errors()
            )
        )

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
        # Sibling fixture: the opencode2 bun tie (MR !1669 review IK-2) also
        # runs here; patch it too so the tests never read the real file.
        self.opencode2_dockerfile = self.root / "Dockerfile.opencode2"
        self.opencode2_dockerfile.write_text(
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
            OPENCODE2_DOCKERFILE=self.opencode2_dockerfile,
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

    def test_short_form_and_mirror_node_from_are_tied(self):
        # MR !1669 review IK-1: the tie follows the repository's last path
        # segment, so a drifted runtime stage is caught however the node
        # image is spelled — docker.io/node, a mirror path, or bare node.
        for runtime_from in (
            f"FROM docker.io/node:22-bookworm-slim@sha256:{self.OTHER_DIGEST} AS runtime\n",
            f"FROM registry.example/mirror/node:22-bookworm-slim@sha256:{self.OTHER_DIGEST} AS runtime\n",
            f"FROM node:22-bookworm-slim@sha256:{self.OTHER_DIGEST} AS runtime\n",
        ):
            with self.subTest(runtime_from=runtime_from):
                self.write_dockerfile(
                    f"FROM docker.io/library/node:22-bookworm-slim@sha256:{self.NODE_DIGEST} AS builder\n"
                    + runtime_from
                )
                self.assertTrue(
                    any("CI_NODE_IMAGE digest must match" in error for error in self.errors()),
                    self.errors(),
                )

    def test_missing_dockerfile_is_caught(self):
        self.assertTrue(any("Dockerfile missing" in error for error in self.errors()))

    def test_no_node_from_is_caught(self):
        self.write_dockerfile(f"FROM docker.io/library/python:3.12@sha256:{self.OTHER_DIGEST}\n")
        self.assertTrue(any("no library/node FROM" in error for error in self.errors()))

    def test_nodejs_named_from_is_caught(self):
        # A non-node image whose name merely starts with "node" must fail
        # closed as "no library/node FROM line", not silently match the tie.
        self.write_dockerfile(f"FROM docker.io/library/nodejs:24@sha256:{self.NODE_DIGEST}\n")
        self.assertTrue(any("no library/node FROM" in error for error in self.errors()))

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
        # Sibling fixture: the opencode2 bun tie (MR !1669 review IK-2) also
        # runs here; patch it too so the tests never read the real file.
        self.opencode2_dockerfile = self.root / "Dockerfile.opencode2"
        self.opencode2_dockerfile.write_text(
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
            OPENCODE2_DOCKERFILE=self.opencode2_dockerfile,
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

    def test_env_override_after_arg_is_caught(self):
        # MR !1644 review HN-6: ENV overrides ARG at build time, so an ENV
        # after the builder's ARG silently detaches the tie from the image.
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + f"ENV DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
        )
        self.assertTrue(
            any("DEBIAN_SNAPSHOT references" in error for error in self.errors())
        )

    def test_run_level_snapshot_assignment_is_caught(self):
        # HN-6: a RUN-level shell assignment re-points that RUN's URLs
        # without touching the ARG the tie binds.
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + "RUN set -eu; DEBIAN_SNAPSHOT=20261001T000000Z; apt-get update\n"
        )
        self.assertTrue(
            any("DEBIAN_SNAPSHOT references" in error for error in self.errors())
        )

    def test_multi_name_arg_is_caught(self):
        # HN-6: a multi-name ARG line hides the pin from the valued-ARG match.
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + f"ARG OTHER=1 DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
        )
        self.assertTrue(
            any("DEBIAN_SNAPSHOT references" in error for error in self.errors())
        )

    def test_shell_substituted_snapshot_url_is_caught(self):
        # HN-6: a URL assembled with shell substitution is not a literal
        # ${DEBIAN_SNAPSHOT} reference.
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + "RUN printf '%s\\n' "
            + '"deb http://snapshot.debian.org/archive/debian/$(echo ${DEBIAN_SNAPSHOT})/ bookworm main" '
            + "> /etc/apt/sources.list\n"
        )
        self.assertTrue(
            any("DEBIAN_SNAPSHOT references" in error for error in self.errors())
        )

    def test_uppercase_snapshot_host_hardcoded_stamp_is_caught(self):
        # HN-6: the host match is case-insensitive, so an upper-case
        # SNAPSHOT.debian.org URL cannot smuggle a hardcoded timestamp past
        # the tie.
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + "RUN printf '%s\\n' "
            + f'"deb http://SNAPSHOT.debian.org/archive/debian/{self.SNAPSHOT}/ bookworm main" '
            + "> /etc/apt/sources.list\n"
        )
        self.assertTrue(
            any("must use ${DEBIAN_SNAPSHOT}" in error for error in self.errors())
        )

    def test_live_mirror_deb_source_is_caught(self):
        # HN-6: a builder moved back to the live deb.debian.org mirror made
        # the tie vacuous — every apt deb source must be a snapshot URL.
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + "RUN printf '%s\\n' "
            + '"deb http://deb.debian.org/debian bookworm main" '
            + "> /etc/apt/sources.list\n"
        )
        self.assertTrue(
            any("apt deb sources" in error for error in self.errors())
        )

    def test_bracketed_option_list_source_is_caught(self):
        # MR !1665 review IF-3 (P6): the rule matched only `deb` followed
        # directly by the URL, so an options list smuggled the live mirror
        # past it.
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + "RUN echo 'deb [trusted=yes] http://deb.debian.org/debian bookworm main' "
            + ">> /etc/apt/sources.list\n"
        )
        self.assertTrue(
            any("apt deb sources" in error for error in self.errors())
        )

    def test_deb_src_source_is_caught(self):
        # IF-3: a deb-src entry is an apt source too.
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + "RUN echo 'deb-src http://deb.debian.org/debian bookworm main' "
            + ">> /etc/apt/sources.list\n"
        )
        self.assertTrue(
            any("apt deb sources" in error for error in self.errors())
        )

    def test_deb822_uris_source_is_caught(self):
        # IF-3: the Dockerfile deletes debian.sources, but a RUN can write a
        # new deb822 stanza — its URIs line is an apt source.
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + "RUN printf '%s\\n' 'Types: deb' "
            + "'URIs: http://deb.debian.org/debian' 'Suites: bookworm' "
            + "> /etc/apt/sources.list.d/debian.sources\n"
        )
        self.assertTrue(
            any("apt deb sources" in error for error in self.errors())
        )

    def test_snapshot_sources_with_option_lists_and_deb822_pass(self):
        # IF-3 pass-side pin: the same shapes pointing at the pinned snapshot
        # must stay clean, and a ${DEBIAN_SNAPSHOT} reference inside a
        # bracketed or deb822 URL still counts as a literal reference.
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + "RUN printf '%s\\n' "
            + '"deb [arch=amd64] http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ bookworm main" '
            + "> /etc/apt/sources.list\n"
            + "RUN printf '%s\\n' 'Types: deb deb-src' "
            + "'URIs: http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/' "
            + "'Suites: bookworm' > /etc/apt/sources.list.d/debian.sources\n"
        )
        self.assertEqual(self.errors(), [])

    def test_live_mirror_beside_a_snapshot_source_is_caught(self):
        # MR !1680 review IP-2 (A9): the per-line check excused the whole
        # line once one source was a snapshot, so one RUN line could carry
        # the pinned snapshot AND a live mirror.
        self.write_dockerfile(
            f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n"
            + self.node_from()
            + "RUN echo 'deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ bookworm main' > /a.list "
            + "&& echo 'deb http://deb.debian.org/debian bookworm main' >> /a.list\n"
        )
        self.assertTrue(
            any("apt deb sources" in error for error in self.errors())
        )

    def test_other_apt_schemes_and_spellings_are_caught(self):
        # MR !1680 review IP-2 (A3-A8): the tie only accepted https?://
        # immediately after a case-sensitive `deb`. apt also honours
        # mirror+http, tor+https and file:, the instruction keyword is
        # case-insensitive, deb822 field names are case-insensitive, and a
        # printf-escaped newline separates tokens in the written file.
        newline = chr(92) + "n"
        shapes = {
            "mirror+http": "RUN echo 'deb mirror+http://deb.debian.org/debian bookworm main' > /a.list\n",
            "tor+https": "RUN echo 'deb tor+https://deb.debian.org/debian bookworm main' > /a.list\n",
            "file:": "RUN echo 'deb [trusted=yes] file:/opt/repo ./' > /a.list\n",
            "uppercase DEB": "RUN echo 'DEB http://deb.debian.org/debian bookworm main' > /a.list\n",
            "lowercase deb822 uris:": "RUN echo 'uris: http://deb.debian.org/debian' >> /a.sources\n",
            "printf deb822": (
                "RUN printf 'Types: deb" + newline + "URIs: http://deb.debian.org/debian"
                + newline + "Suites: bookworm' > /a.sources\n"
            ),
            "deb822 snapshot plus live": (
                "RUN echo 'URIs: http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/"
                " http://deb.debian.org/debian' >> /a.sources\n"
            ),
        }
        for shape, line in shapes.items():
            with self.subTest(shape=shape):
                self.write_dockerfile(
                    f"ARG DEBIAN_SNAPSHOT={self.SNAPSHOT}\n" + self.node_from() + line
                )
                self.assertTrue(
                    any("apt deb sources" in error for error in self.errors()),
                    f"{shape} produced no apt-source error",
                )


class YamlParseErrorTests(unittest.TestCase):
    """MR !1647 review HP-5: a `!reference` tag anywhere in the CI YAML made
    yaml.safe_load raise a ConstructorError traceback — fail-closed (rc 1)
    but unreadable. The validator now reports one clean ERROR line."""

    def test_load_yaml_reports_unsupported_tags_cleanly(self):
        temporary = tempfile.TemporaryDirectory(dir=ROOT)
        self.addCleanup(temporary.cleanup)
        probe = pathlib.Path(temporary.name) / "probe.yml"
        probe.write_text("job:\n  script: !reference [build, script]\n", encoding="utf-8")
        with self.assertRaises(ValueError) as caught:
            config.load_yaml(probe)
        message = str(caught.exception)
        self.assertIn("probe.yml: cannot parse YAML", message)
        self.assertIn("!reference", message)

    def test_load_yaml_still_requires_a_mapping(self):
        temporary = tempfile.TemporaryDirectory(dir=ROOT)
        self.addCleanup(temporary.cleanup)
        probe = pathlib.Path(temporary.name) / "probe.yml"
        probe.write_text("- just\n- a\n- list\n", encoding="utf-8")
        with self.assertRaises(ValueError) as caught:
            config.load_yaml(probe)
        self.assertIn("must contain a YAML mapping", str(caught.exception))

    def test_cli_returns_a_clean_error_without_a_traceback(self):
        original_load_yaml = config.load_yaml

        # Drive cli() with load_yaml raising the real ValueError shape.
        def raising_load_yaml(path):
            if path == ROOT / ".gitlab" / "ci" / "validate.yml":
                raise ValueError(
                    ".gitlab/ci/validate.yml: cannot parse YAML: could not "
                    "determine a constructor for the tag '!reference'"
                )
            return original_load_yaml(path)

        with (
            mock.patch.object(config, "load_yaml", side_effect=raising_load_yaml),
            mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            self.assertEqual(config.cli(), 1)
        output = stderr.getvalue()
        self.assertIn("ERROR: .gitlab/ci/validate.yml: cannot parse YAML", output)
        self.assertIn("!reference", output)
        self.assertNotIn("Traceback", output)


class AdvisoryGateOptionalTests(unittest.TestCase):
    """MR !1658 review IA-9: the release audit's advisory gates must not be
    allow_failure — a root-file `cargo-deny: {allow_failure: true}` turned
    the gate advisory-only while every script pin here stayed green.
    MR !1665 review IF-1/IF-2: the same applies one level down (rule-level
    allow_failure, replaced rules, a job-level when, a deleted or retagged
    gate, a job/rule-level runner tag variable), so the fixture models the
    real shape — .rust-job carrying the pinned tag and the pinned bootstrap
    before_script, .full-validation-context carrying the if-rules plus a
    final when: never, each gate extending both and running exactly its
    pinned script (MR !1680 review IP-1)."""

    GATES = (
        "cargo-deny",
        "satellite-advisory-sweep",
        "lockfile-only-advisory-guard",
        "default-graph-advisory-guard",
    )

    def merged(self) -> dict:
        return {
            ".rust-job": {
                "tags": ["$CI_RUST_RUNNER_TAG"],
                "before_script": list(config.ADVISORY_GATE_BEFORE_SCRIPT),
                "image": copy.deepcopy(config.ADVISORY_GATE_PINNED_VALUES["image"]),
                "cache": copy.deepcopy(config.ADVISORY_GATE_PINNED_VALUES["cache"]),
            },
            ".full-validation-context": {
                "rules": [{"if": "$CI_COMMIT_BRANCH"}, {"when": "never"}]
            },
            **{
                name: {
                    "extends": [".rust-job", ".full-validation-context"],
                    "script": list(config.ADVISORY_GATE_SCRIPTS[name]),
                    "needs": copy.deepcopy(config.ADVISORY_GATE_PINNED_VALUES["needs"]),
                }
                for name in self.GATES
            },
        }

    def errors(self, merged: dict) -> list[str]:
        errors: list[str] = []
        config.validate_advisory_gates_not_optional(merged, errors)
        return errors

    def test_real_shape_passes(self):
        self.assertEqual(self.errors(self.merged()), [])

    def test_gate_inputs_are_pinned(self):
        # IU-1: string-form needs imports both artifacts and dotenv; other
        # jobs, images or caches can replace the checker without editing it.
        shapes = (
            ("needs", ["cargo-metadata"]),
            ("needs", [*config.ADVISORY_GATE_PINNED_VALUES["needs"], "hollow-prep"]),
            ("image", {"name": "docker.io/attacker/rust@sha256:" + "a" * 64}),
            ("cache", [*config.ADVISORY_GATE_PINNED_VALUES["cache"], {
                "key": "hollow-checkers", "paths": [".gitlab/scripts/"], "policy": "pull",
            }]),
        )
        for job_name in self.GATES:
            for key, value in shapes:
                with self.subTest(job=job_name, key=key, value=value):
                    merged = self.merged()
                    merged[job_name][key] = copy.deepcopy(value)
                    self.assertTrue(any(
                        error.startswith(f"{job_name} {key} must be exactly")
                        for error in self.errors(merged)
                    ))

    def test_allow_failure_is_caught_on_every_gate(self):
        for job_name in self.GATES:
            for value in (True, {"exit_codes": [1]}):
                with self.subTest(job=job_name, value=value):
                    merged = self.merged()
                    merged[job_name]["allow_failure"] = value
                    self.assertTrue(
                        any(
                            f"{job_name} must not set allow_failure" in error
                            for error in self.errors(merged)
                        )
                    )

    def test_inherited_allow_failure_is_caught(self):
        # The extends-resolved job is checked: .observational-job carries
        # allow_failure: true (defaults.yml), so extending it is refused too.
        merged = self.merged()
        merged[".observational-job"] = {"allow_failure": True}
        merged["cargo-deny"]["extends"].append(".observational-job")
        self.assertTrue(
            any("cargo-deny must not set allow_failure" in error for error in self.errors(merged))
        )

    def test_gate_shape_variants_are_caught(self):
        # MR !1665 review IF-1/IF-2: each of the 4 gates x 5 shapes left the
        # head's job-level-only guard green (the review's P1-P5 probes).
        for job_name in self.GATES:
            with self.subTest(job=job_name, shape="rule-level allow_failure"):
                merged = self.merged()
                rules = copy.deepcopy(merged[".full-validation-context"]["rules"])
                for rule in rules:
                    if "if" in rule:
                        rule["allow_failure"] = True
                merged[job_name]["rules"] = rules
                self.assertTrue(
                    any(
                        f"{job_name} rules must be exactly" in error
                        for error in self.errors(merged)
                    )
                )
            with self.subTest(job=job_name, shape="rules replaced with when: never"):
                merged = self.merged()
                merged[job_name]["rules"] = [{"when": "never"}]
                self.assertTrue(
                    any(
                        f"{job_name} rules must be exactly" in error
                        for error in self.errors(merged)
                    )
                )
            with self.subTest(job=job_name, shape="gate deleted"):
                merged = self.merged()
                del merged[job_name]
                self.assertTrue(
                    any(
                        f"{job_name} is missing" in error
                        for error in self.errors(merged)
                    )
                )
            with self.subTest(job=job_name, shape="retagged"):
                merged = self.merged()
                merged[job_name]["tags"] = ["untrusted-shell"]
                self.assertTrue(
                    any(
                        f"{job_name} tags must be exactly" in error
                        for error in self.errors(merged)
                    )
                )
            with self.subTest(job=job_name, shape="job-level runner variable"):
                merged = self.merged()
                merged[job_name]["variables"] = {"CI_RUST_RUNNER_TAG": "untrusted-shell"}
                self.assertTrue(
                    any(
                        f"{job_name} must not override a runner tag variable" in error
                        for error in self.errors(merged)
                    )
                )

    def test_job_level_when_is_caught(self):
        merged = self.merged()
        merged["cargo-deny"]["when"] = "manual"
        self.assertTrue(
            any("cargo-deny must not set a job-level when" in error for error in self.errors(merged))
        )

    def test_template_rule_carrying_more_than_if_is_caught(self):
        merged = self.merged()
        merged[".full-validation-context"]["rules"][0]["allow_failure"] = True
        errors = self.errors(merged)
        self.assertTrue(
            any(".full-validation-context rules may carry only `if`" in error for error in errors)
        )

    def test_template_without_rules_fails_closed(self):
        merged = self.merged()
        del merged[".full-validation-context"]["rules"]
        self.assertTrue(
            any(".full-validation-context must define rules" in error for error in self.errors(merged))
        )

    def test_rule_level_runner_tag_variable_is_caught(self):
        # IF-2: a rules:variables entry also overrides the pipeline variable
        # for the job that matches the rule — pinned on a non-gate job so
        # only the variable refusal fires.
        merged = self.merged()
        merged["some-job"] = {
            "script": ["true"],
            "rules": [
                {"if": "$CI_COMMIT_BRANCH", "variables": {"CI_LINT_RUNNER_TAG": "untrusted-shell"}}
            ],
        }
        errors = self.errors(merged)
        self.assertTrue(
            any("some-job must not override a runner tag variable" in error for error in errors)
        )

    def test_benign_job_variables_pass(self):
        # Only the runner tag names are refused at job/rule level; other
        # job-level variables on a non-gate job (the real tree sets several)
        # stay fine.
        merged = self.merged()
        merged["worker-scan-lane"] = {"variables": {"TRIVY_NO_PROGRESS": "true"}}
        self.assertEqual(self.errors(merged), [])
        # MR !1680 review IP-1: the gates themselves carry NO job-level
        # variables — a variable can hollow out a gate, so the same benign
        # variable on a gate is refused by the key-set pin.
        merged = self.merged()
        merged["cargo-deny"]["variables"] = {"TRIVY_NO_PROGRESS": "true"}
        self.assertTrue(
            any(
                "cargo-deny must not carry job keys" in error
                for error in self.errors(merged)
            )
        )

    def test_gate_that_never_runs_its_checker_is_caught(self):
        # MR !1680 review IP-1 (run-135): a gate that never runs its checker
        # is as optional as an allow_failure one. 4 gates x 6 shapes, each
        # left the head's mention-only script pin green (probes Q1-Q5).
        for job_name in self.GATES:
            checker = config.ADVISORY_GATE_SCRIPTS[job_name][-1]
            shapes = {
                "job before_script exit 0": lambda merged, name: merged[name].update(
                    {"before_script": ["exit 0"]}
                ),
                "exit 0 as first script line": lambda merged, name: merged[name].update(
                    {"script": ["exit 0", *config.ADVISORY_GATE_SCRIPTS[name]]}
                ),
                "checker || true": lambda merged, name: merged[name].update(
                    {
                        "script": [
                            *config.ADVISORY_GATE_SCRIPTS[name][:-1],
                            f"{checker} || true",
                        ]
                    }
                ),
                "echoed checker": lambda merged, name: merged[name].update(
                    {
                        "script": [
                            *config.ADVISORY_GATE_SCRIPTS[name][:-1],
                            f"echo {checker}",
                        ]
                    }
                ),
                "template before_script exit 0": lambda merged, name: merged[
                    ".rust-job"
                ].update(
                    {"before_script": [*config.ADVISORY_GATE_BEFORE_SCRIPT, "exit 0"]}
                ),
                "job-level hooks": lambda merged, name: merged[name].update(
                    {"hooks": {"pre_get_sources_script": ["exit 0"]}}
                ),
            }
            for shape, mutate in shapes.items():
                with self.subTest(job=job_name, shape=shape):
                    merged = self.merged()
                    mutate(merged, job_name)
                    self.assertTrue(
                        any(
                            error.startswith(f"{job_name} ")
                            for error in self.errors(merged)
                        ),
                        f"{job_name} / {shape} produced no gate-named error",
                    )

    def test_root_file_cargo_deny_override_is_caught(self):
        # IA-9: the review's exact probe — one root-file line makes the
        # release audit's gate advisory-only.
        original_load_yaml = config.load_yaml

        def load_with_root_override(path):
            loaded = original_load_yaml(path)
            if path == ROOT / ".gitlab-ci.yml":
                loaded["cargo-deny"] = {"allow_failure": True}
            return loaded

        with (
            mock.patch.object(config, "load_yaml", side_effect=load_with_root_override),
            mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            self.assertEqual(config.main(), 1)
        self.assertIn("cargo-deny must not set allow_failure", stderr.getvalue())


class CacheTrustTests(unittest.TestCase):
    """IU-3: exercise cache policy against the actual merged CI definitions."""

    def setUp(self):
        self.merged = {}
        for include in config.EXPECTED_INCLUDES:
            self.merged = config.merge_ci_hashes(
                self.merged, config.load_yaml(ROOT / include.lstrip("/"))
            )
        self.merged = config.merge_ci_hashes(self.merged, config.load_yaml(config.ROOT_CONFIG))

    def errors(self, merged):
        errors = []
        config.validate_cache_trust(merged, errors)
        return errors

    def test_real_cache_policy_passes(self):
        self.assertEqual(self.errors(self.merged), [])

    def test_validation_jobs_cannot_write_caches(self):
        for name in ("rust-check-postgres", "rust-check-libsql", "rust-test-lib"):
            for policy in ("push", "pull-push", "$CACHE_POLICY", None):
                with self.subTest(job=name, policy=policy):
                    merged = copy.deepcopy(self.merged)
                    cache = {"key": "rust-1.96.1-sccache", "paths": [".ci-cache/sccache/"]}
                    if policy is not None:
                        cache["policy"] = policy
                    merged[name]["cache"] = cache
                    self.assertIn(
                        f"{name} validation caches must be fixed pull-only", self.errors(merged)
                    )

    def test_inherited_writer_is_caught_on_the_effective_job(self):
        self.merged[".extra-cache"] = {"cache": {
            "key": "rust-1.96.1-sccache", "paths": [".ci-cache/sccache/"], "policy": "push",
        }}
        self.merged["rust-check-postgres"]["extends"].append(".extra-cache")
        self.assertIn(
            "rust-check-postgres validation caches must be fixed pull-only",
            self.errors(self.merged),
        )

    def test_default_and_deprecated_global_writers_are_caught(self):
        cache = {"key": "shared", "paths": [".ci-cache/sccache/"], "policy": "push"}
        self.merged["default"]["cache"] = cache
        self.assertIn("default validation caches must be fixed pull-only", self.errors(self.merged))
        del self.merged["default"]["cache"]
        self.merged["cache"] = cache
        self.assertIn(
            "top-level cache is not allowed; declare caches on job templates",
            self.errors(self.merged),
        )

    def test_cache_namespaces_cannot_be_unprotected(self):
        for name in (".rust-job", "rust-cache-refresh"):
            for value in (True, "$UNPROTECT_CACHE"):
                with self.subTest(job=name, value=value):
                    merged = copy.deepcopy(self.merged)
                    merged[name]["cache"][-1]["unprotect"] = value
                    self.assertIn(
                        f"{name} must not use cache:unprotect; keep protected caches separate",
                        self.errors(merged),
                    )

    def test_writer_predicate_cannot_add_an_mr_route(self):
        # These still contain every fragment the old substring guard required.
        condition = self.merged["rust-cache-refresh"]["rules"][0]["if"]
        for bypass in (
            condition + ' || $CI_PIPELINE_SOURCE == "merge_request_event"',
            condition.replace(" && ", " || "),
        ):
            with self.subTest(condition=bypass):
                merged = copy.deepcopy(self.merged)
                merged["rust-cache-refresh"]["rules"][0]["if"] = bypass
                merged["rust-check-default"]["rules"][0]["if"] = bypass
                self.assertIn(
                    "rust-cache-refresh rules must be exactly the protected default-branch push gate",
                    self.errors(merged),
                )

    def test_writer_variables_cannot_spoof_the_push_context(self):
        self.merged["rust-cache-refresh"]["variables"] = {"CI_PIPELINE_SOURCE": "push"}
        self.assertIn(
            "rust-cache-refresh must not override variables used by its protected push gate",
            self.errors(self.merged),
        )

    def test_writer_must_exist_with_fixed_policies(self):
        for policy in ("pull", "$CACHE_POLICY"):
            with self.subTest(policy=policy):
                merged = copy.deepcopy(self.merged)
                merged["rust-cache-refresh"]["cache"][-1]["policy"] = policy
                self.assertIn(
                    "rust-cache-refresh must declare fixed pull-push caches", self.errors(merged)
                )
        del self.merged["rust-cache-refresh"]
        self.assertIn(
            "rust-cache-refresh must declare fixed pull-push caches", self.errors(self.merged)
        )

    def test_truncated_cache_inheritance_fails_closed(self):
        self.merged["cache-probe"] = {"extends": ".cache-parent-0", "script": ["true"]}
        for index in range(config.EXTENDS_DEPTH_BUDGET + 1):
            self.merged[f".cache-parent-{index}"] = {"extends": f".cache-parent-{index + 1}"}
        self.assertIn(
            "cache-probe cache trust cannot validate a truncated extends chain",
            self.errors(self.merged),
        )

    def test_validation_can_disable_caching(self):
        for value in ([], None):
            with self.subTest(cache=value):
                self.merged["rust-check-postgres"]["cache"] = value
                self.assertEqual(self.errors(self.merged), [])


class WorkerTsLaneTests(unittest.TestCase):
    """MR !1601 review GH-2: the worker TS lanes must be pinned — existence,
    the merge_request_event rule, the tied image variable, and (MR !1644
    review HN-5) the exact script, taken from config.WORKER_TS_LANE_SCRIPTS
    so the fixtures always match the pinned tuple; the real-tree binding is
    main() running this validator over the checked-in validate.yml."""

    LANES = ("worker-kimi-ts", "worker-opencode-ts")

    def lane(self, job_name: str) -> dict:
        image_variable, script = config.WORKER_TS_LANE_SCRIPTS[job_name]
        return {
            "stage": "validate",
            "needs": [],
            "image": {"name": image_variable, "entrypoint": [""]},
            "tags": ["$CI_LINT_RUNNER_TAG"],
            "rules": [
                {"if": '$CI_PIPELINE_SOURCE == "merge_request_event"'},
                {"if": '$CI_PIPELINE_SOURCE == "push" && $CI_COMMIT_BRANCH'},
                {"when": "never"},
            ],
            "script": list(script),
        }

    def variables(self, extra: dict | None = None) -> dict:
        # The canonical runner tag values (IA-9): every top-level variables
        # fixture must carry them so the tag pin sees the real-tree shape.
        values = {"CI_LINT_RUNNER_TAG": "lint", "CI_RUST_RUNNER_TAG": "rust-x86_64"}
        if extra:
            values.update(extra)
        return values

    def merged(self) -> dict:
        # Every pinned TS lane gets the same fixture shape, so the per-lane
        # loops below cover a newly pinned lane without a test edit.
        merged = {name: self.lane(name) for name in config.WORKER_TS_LANE_SCRIPTS}
        merged["variables"] = self.variables()
        return merged

    def errors(self, merged: dict) -> list[str]:
        errors: list[str] = []
        config.validate_worker_ts_lanes(merged, errors)
        return errors

    def test_valid_shape_passes(self):
        self.assertEqual(self.errors(self.merged()), [])

    def test_missing_needs_is_caught(self):
        # IX-2 (MR !1691 run-137): a lane moved to a later stage without
        # needs: [] receives every earlier-stage job's dotenv variables
        # (NODE_OPTIONS, BUN_OPTIONS) and artifacts.
        for job_name in config.WORKER_TS_LANE_SCRIPTS:
            with self.subTest(job=job_name):
                merged = self.merged()
                del merged[job_name]["needs"]
                merged[job_name]["stage"] = "check"
                self.assertTrue(
                    any(
                        job_name in error and "needs: []" in error
                        for error in self.errors(merged)
                    ),
                    self.errors(merged),
                )

    def test_job_level_execution_overrides_are_caught(self):
        # HP-1: the script can remain byte-identical while its environment
        # skips tests, exits Node immediately, or replaces npm's shell.
        overrides = (
            ("variables", {"NODE_OPTIONS": "--test-skip-pattern=."}),
            ("variables", {"NODE_OPTIONS": "--import=data:text/javascript,process.exit(0)"}),
            ("variables", {"npm_config_script_shell": "/bin/true"}),
            ("parallel", {"matrix": [{"NODE_OPTIONS": ["--test-only"]}]}),
            ("hooks", {"pre_get_sources_script": ["git config --global core.hooksPath /tmp/hooks"]}),
        )
        for job_name in self.LANES:
            for key, value in overrides:
                for inherited in (False, True):
                    with self.subTest(job=job_name, key=key, value=value, inherited=inherited):
                        merged = self.merged()
                        override = {key: copy.deepcopy(value)}
                        if inherited:
                            merged[".execution-parent"] = override
                            merged[job_name]["extends"] = ".execution-parent"
                        else:
                            merged[job_name].update(override)
                        self.assertTrue(any(
                            f"{job_name} must not set job keys ['{key}']" in error
                            for error in self.errors(merged)
                        ))

    def test_other_job_keywords_require_review(self):
        # The allowlist deliberately requires review even for currently
        # harmless values; it is not a blacklist of known variable names.
        for job_name in self.LANES:
            for key, value in (
                ("variables", {}), ("variables", None), ("hooks", {}),
                ("after_script", ["true"]), ("timeout", "1m"),
                ("retry", 1), ("inherit", {"variables": False}),
            ):
                with self.subTest(job=job_name, key=key, value=value):
                    merged = self.merged()
                    merged[job_name][key] = value
                    self.assertTrue(any(
                        f"{job_name} must not set job keys ['{key}']" in error
                        for error in self.errors(merged)
                    ))

    def test_runner_tags_must_match_in_both_lanes(self):
        for job_name in self.LANES:
            for override in (
                {}, {"tags": None}, {"tags": []},
                {"tags": ["unavailable-runner"]},
                {"tags": ["$CI_LINT_RUNNER_TAG", "extra-runner-tag"]},
                {"tags": "$CI_LINT_RUNNER_TAG"},
            ):
                for inherited in (False, True):
                    with self.subTest(job=job_name, override=override, inherited=inherited):
                        merged = self.merged()
                        del merged[job_name]["tags"]
                        if inherited:
                            merged[".runner-parent"] = copy.deepcopy(override)
                            merged[job_name]["extends"] = ".runner-parent"
                        else:
                            merged[job_name].update(override)
                        self.assertTrue(any(
                            f"{job_name} tags must be exactly ['$CI_LINT_RUNNER_TAG']" in error
                            for error in self.errors(merged)
                        ))

    def test_allowed_job_keys_can_be_inherited(self):
        # Inspect the resolved job, not the raw `extends` keyword; allowed
        # defaults and the canonical runner tag remain valid in a parent.
        for job_name in self.LANES:
            with self.subTest(job=job_name):
                merged = self.merged()
                merged[".worker-template"] = merged[job_name]
                merged[".worker-template"].update({
                    "before_script": [], "allow_failure": False, "when": "on_success",
                })
                merged[job_name] = {"extends": ".worker-template"}
                self.assertEqual(self.errors(merged), [])

    def test_main_rejects_job_level_execution_override(self):
        original_load_yaml = config.load_yaml
        for job_name in self.LANES:
            with self.subTest(job=job_name):
                def load_with_override(path):
                    loaded = original_load_yaml(path)
                    if path == ROOT / ".gitlab/ci/validate.yml":
                        loaded[job_name]["variables"] = {"NODE_OPTIONS": "--test-only"}
                    return loaded

                with (
                    mock.patch.object(config, "load_yaml", side_effect=load_with_override),
                    mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
                ):
                    self.assertEqual(config.main(), 1)
                self.assertIn(
                    f"{job_name} must not set job keys ['variables']", stderr.getvalue()
                )

    def test_missing_kimi_lane_is_caught(self):
        merged = self.merged()
        del merged["worker-kimi-ts"]
        self.assertTrue(any("worker-kimi-ts job missing" in error for error in self.errors(merged)))

    def test_missing_opencode_lane_is_caught(self):
        merged = self.merged()
        del merged["worker-opencode-ts"]
        self.assertTrue(any("worker-opencode-ts job missing" in error for error in self.errors(merged)))

    def test_missing_opencode2_lane_is_caught(self):
        merged = self.merged()
        del merged["worker-opencode2-ts"]
        self.assertTrue(any("worker-opencode2-ts job missing" in error for error in self.errors(merged)))

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

    def test_trailing_space_inside_mr_predicate_quotes_is_caught(self):
        # MR !1644 review HN-1: the squash-and-compare check deleted
        # whitespace inside the quoted literal, so `"merge_request_event "`
        # passed while GitLab compares the literal exactly — the rule then
        # never matches an MR pipeline and the lane silently drops out.
        merged = self.merged()
        merged["worker-kimi-ts"]["rules"][0]["if"] = (
            '$CI_PIPELINE_SOURCE == "merge_request_event "'
        )
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_space_inside_mr_predicate_literal_is_caught(self):
        # HN-1: same hole, whitespace in the middle of the literal.
        merged = self.merged()
        merged["worker-kimi-ts"]["rules"][0]["if"] = (
            '$CI_PIPELINE_SOURCE == "merge_request _event"'
        )
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_nbsp_inside_mr_predicate_quotes_is_caught(self):
        # HN-1: a non-breaking space inside the quotes is literal too.
        merged = self.merged()
        merged["worker-kimi-ts"]["rules"][0]["if"] = (
            '$CI_PIPELINE_SOURCE == "merge_request_event "'
        )
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

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
        # A cyclic extends chain must terminate (depth-bounded), surface
        # the parent's gating keys, and — since HP-2/HN-3 — carry the
        # truncation sentinel so the lane fails closed.
        merged = self.merged()
        merged[".loopy"] = {"extends": ".loopy", "allow_failure": True}
        merged["worker-kimi-ts"]["extends"] = ".loopy"
        errors = self.errors(merged)
        self.assertTrue(any("merge_request_event" in error for error in errors))
        self.assertTrue(any("depth budget" in error for error in errors))

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

    def test_trailing_args_change_the_pinned_script(self):
        # MR !1644 review HN-5 (inverts the HE-2 acceptance): the fragment
        # pin accepted `npm test -- --test-name-pattern=NO_SUCH_TEST`, which
        # runs nothing. The script must equal the pinned tuple line for
        # line, so trailing args now fail.
        merged = self.merged()
        merged["worker-kimi-ts"]["script"] = [
            "npm test -- --grep worker" if command == "npm test" else command
            for command in merged["worker-kimi-ts"]["script"]
        ]
        self.assertTrue(any("exactly the pinned commands" in error for error in self.errors(merged)))

    def test_folded_multi_line_command_is_caught(self):
        # HN-5 (inverts the HE-2 acceptance): folding `npm test` into the
        # previous line's YAML string changes the effective script.
        merged = self.merged()
        script = merged["worker-kimi-ts"]["script"]
        index = script.index("npm test")
        script[index - 1] = script[index - 1] + "\nnpm test"
        del script[index]
        self.assertTrue(any("exactly the pinned commands" in error for error in self.errors(merged)))

    def test_prepended_exit_0_is_caught(self):
        # HN-5: `- exit 0` as the first script line has the same effect as
        # the banned before_script — the pinned commands never run.
        merged = self.merged()
        merged["worker-kimi-ts"]["script"].insert(0, "exit 0")
        self.assertTrue(any("exactly the pinned commands" in error for error in self.errors(merged)))

    def test_backgrounded_npm_test_is_caught(self):
        # HN-5: `npm test &` runs the suite concurrently and cannot gate.
        merged = self.merged()
        merged["worker-kimi-ts"]["script"] = [
            "npm test &" if command == "npm test" else command
            for command in merged["worker-kimi-ts"]["script"]
        ]
        self.assertTrue(any("exactly the pinned commands" in error for error in self.errors(merged)))

    def test_set_plus_e_and_trailing_true_is_caught(self):
        # HN-5: `set +e` up front and a trailing `true` neutralise failures.
        merged = self.merged()
        script = merged["worker-kimi-ts"]["script"]
        script.insert(0, "set +e")
        script.append("true")
        self.assertTrue(any("exactly the pinned commands" in error for error in self.errors(merged)))

    def test_dropped_sources_removal_line_is_caught(self):
        # HN-5: deleting the debian.sources removal lets the live mirror's
        # newer python3 win, silently undoing the GV-4 snapshot pin.
        merged = self.merged()
        merged["worker-kimi-ts"]["script"] = [
            command
            for command in merged["worker-kimi-ts"]["script"]
            if "debian.sources" not in command
        ]
        self.assertTrue(any("exactly the pinned commands" in error for error in self.errors(merged)))

    def test_extra_live_mirror_deb_entry_is_caught(self):
        # HN-5: an extra live-mirror deb entry in the printf re-adds the
        # live mirror alongside the snapshot pins.
        merged = self.merged()
        script = merged["worker-kimi-ts"]["script"]
        index = next(
            i for i, command in enumerate(script) if command.startswith("printf")
        )
        script[index] = script[index].replace(
            " > /etc/apt/sources.list",
            ' "deb http://deb.debian.org/debian bookworm main" > /etc/apt/sources.list',
        )
        self.assertTrue(any("exactly the pinned commands" in error for error in self.errors(merged)))

    def test_dropped_opencode_bun_test_lines_are_caught(self):
        # HN-5 (pre-existing gap): the opencode lane's five bun self-test
        # lines could be deleted without tripping the fragment pins.
        merged = self.merged()
        merged["worker-opencode-ts"]["script"] = [
            command
            for command in merged["worker-opencode-ts"]["script"]
            if not command.startswith("bun scripts/")
        ]
        self.assertTrue(any("exactly the pinned commands" in error for error in self.errors(merged)))

    def test_real_lanes_match_the_pinned_scripts(self):
        # HN-5: the pinned tuples must equal the checked-in validate.yml
        # lanes, so a lane edit without a tuple update fails here (and in
        # ci-config via main()).
        merged = {}
        for include in config.EXPECTED_INCLUDES:
            merged.update(config.load_yaml(ROOT / include.lstrip("/")))
        self.assertEqual(self.errors(merged), [])

    def test_missing_apt_retries_are_caught(self):
        # HE-4/HN-8: reverting the snapshot fetches to retry-less apt-get
        # drops the pinned provisioning fragment — on update and install
        # together, or on the install half alone.
        merged = self.merged()
        merged["worker-kimi-ts"]["script"] = [
            command.replace(" -o Acquire::Retries=5 -o Acquire::http::Timeout=60", "")
            for command in merged["worker-kimi-ts"]["script"]
        ]
        self.assertTrue(any("Acquire::Retries" in error for error in self.errors(merged)))
        merged = self.merged()
        merged["worker-kimi-ts"]["script"] = [
            command.replace(
                "apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=60 install",
                "apt-get install",
            )
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
        # live deb.debian.org mirror must fail the (now exact-script) pin.
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

    def test_reordered_script_lines_are_caught(self):
        # HP-3: an order-insensitive comparison would accept a reorder, but
        # the snapshot sources must be rewritten BEFORE apt-get update runs.
        for job_name in self.LANES:
            with self.subTest(job=job_name):
                merged = self.merged()
                script = merged[job_name]["script"]
                script[0], script[1] = script[1], script[0]
                self.assertTrue(
                    any("exactly the pinned commands" in error for error in self.errors(merged))
                )

    def test_script_as_a_single_string_is_caught(self):
        # HP-3: GitLab also accepts a string script; dropping the
        # isinstance(script, list) guard would compare it against the
        # list. A non-iterable script (e.g. `script: 5`) must fail with a
        # validator error, not crash the guard-less comparison.
        for job_name in self.LANES:
            for script in ("\n".join(self.lane(job_name)["script"]), 5):
                with self.subTest(job=job_name, script=script):
                    merged = self.merged()
                    merged[job_name]["script"] = script
                    self.assertTrue(
                        any(
                            "exactly the pinned commands" in error
                            for error in self.errors(merged)
                        )
                    )

    def test_other_variable_in_mr_predicate_is_caught(self):
        # HP-3: only $CI_PIPELINE_SOURCE is the MR predicate's left
        # operand; dropping that check accepts any variable compared
        # against the "merge_request_event" literal.
        for job_name in self.LANES:
            with self.subTest(job=job_name):
                merged = self.merged()
                merged[job_name]["rules"][0] = {
                    "if": '$CI_MERGE_REQUEST_IID == "merge_request_event"'
                }
                self.assertTrue(
                    any("must run on merge_request_event" in error for error in self.errors(merged))
                )

    def test_root_file_job_override_is_merged_last(self):
        # HP-2: GitLab merges the root file OVER the includes, so a root
        # worker-kimi-ts entry is the effective job. Root-first merging
        # hid the override behind validate.yml's pinned block.
        original_load_yaml = config.load_yaml

        def load_with_root_override(path):
            loaded = original_load_yaml(path)
            if path == ROOT / ".gitlab-ci.yml":
                loaded["worker-kimi-ts"] = {"script": ["exit 0"]}
            return loaded

        with (
            mock.patch.object(config, "load_yaml", side_effect=load_with_root_override),
            mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            self.assertEqual(config.main(), 1)
        self.assertIn("worker-kimi-ts script must be exactly the pinned commands", stderr.getvalue())

    def test_root_file_variables_override_is_caught(self):
        # HP-2: the root file's top-level variables win over the includes
        # in GitLab; collect_variables must apply the same precedence so a
        # root-file CI_NODE_IMAGE override fails the digest pin instead of
        # passing unseen.
        original_load_yaml = config.load_yaml

        def load_with_root_variables(path):
            loaded = original_load_yaml(path)
            if path == ROOT / ".gitlab-ci.yml":
                loaded.setdefault("variables", {})["CI_NODE_IMAGE"] = (
                    "docker.io/library/node:22-bookworm-slim"
                )
            return loaded

        with (
            mock.patch.object(config, "load_yaml", side_effect=load_with_root_variables),
            mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            self.assertEqual(config.main(), 1)
        self.assertIn("CI_NODE_IMAGE", stderr.getvalue())

    def test_default_before_script_is_caught(self):
        # HP-2: a default before_script/hooks is inherited by every job —
        # including both lanes — and shares the pinned script's shell.
        for key, value in (
            ("before_script", ["export NODE_OPTIONS=--test-only"]),
            ("hooks", {"pre_get_sources_script": ["true"]}),
        ):
            with self.subTest(key=key):
                merged = self.merged()
                merged["default"] = {key: value}
                self.assertTrue(any(f"default.{key}" in error for error in self.errors(merged)))

    def test_global_lane_bypass_variables_are_caught(self):
        # Run-121 HP-2 reminder: a top-level NODE_OPTIONS or npm_config_*
        # reaches every job, including both lanes; npm reads the prefix
        # case-insensitively.
        merged = self.merged()
        merged["variables"] = self.variables({"GIT_STRATEGY": "fetch"})
        self.assertEqual(self.errors(merged), [])
        for name in ("NODE_OPTIONS", "npm_config_script_shell", "NPM_CONFIG_SCRIPT_SHELL"):
            with self.subTest(name=name):
                merged = self.merged()
                merged["variables"] = self.variables({name: "--test-only"})
                self.assertTrue(
                    any("top-level variables" in error for error in self.errors(merged))
                )

    def test_runner_tag_variable_override_is_caught(self):
        # MR !1658 review IA-9: the lanes select their runner through
        # $CI_LINT_RUNNER_TAG; a variable override reroutes every pinned
        # lint job with the tags lists themselves untouched.
        for name, value in (
            ("CI_LINT_RUNNER_TAG", "other-runner"),
            ("CI_RUST_RUNNER_TAG", "other-runner"),
            ("CI_LINT_RUNNER_TAG", {"value": "other-runner", "description": "hash form"}),
        ):
            with self.subTest(name=name, value=value):
                merged = self.merged()
                merged["variables"][name] = value
                self.assertTrue(
                    any(f"top-level {name}" in error for error in self.errors(merged))
                )

    def test_root_file_runner_tag_override_is_caught(self):
        # IA-9: the review's probe — a root-file variables entry reroutes
        # the lanes with validate.yml untouched.
        original_load_yaml = config.load_yaml

        def load_with_root_tag_override(path):
            loaded = original_load_yaml(path)
            if path == ROOT / ".gitlab-ci.yml":
                loaded.setdefault("variables", {})["CI_LINT_RUNNER_TAG"] = "other-runner"
            return loaded

        with (
            mock.patch.object(config, "load_yaml", side_effect=load_with_root_tag_override),
            mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            self.assertEqual(config.main(), 1)
        self.assertIn("CI_LINT_RUNNER_TAG", stderr.getvalue())

    def test_nine_level_extends_chain_is_resolved(self):
        # HP-2/HN-3: the budget must cover chains deeper than 8; a gating
        # key on the ninth parent was silently invisible at depth 8.
        merged = self.merged()
        merged[".p9"] = {"allow_failure": True}
        for index in range(8, 0, -1):
            merged[f".p{index}"] = {"extends": f".p{index + 1}"}
        merged["worker-kimi-ts"]["extends"] = ".p1"
        errors = self.errors(merged)
        self.assertTrue(any("merge_request_event" in error for error in errors))
        self.assertFalse(any("depth budget" in error for error in errors))

    def test_extends_chain_at_the_budget_boundary_is_resolved(self):
        # IA-8 (mutant M16): the budget is exactly EXTENDS_DEPTH_BUDGET —
        # moving the leaf check below the depth check silently cut it to
        # ten. A chain of exactly the budgeted depth must still resolve.
        merged = self.merged()
        depth = config.EXTENDS_DEPTH_BUDGET
        merged[f".p{depth}"] = {"allow_failure": True}
        for index in range(depth - 1, 0, -1):
            merged[f".p{index}"] = {"extends": f".p{index + 1}"}
        merged["worker-kimi-ts"]["extends"] = ".p1"
        errors = self.errors(merged)
        self.assertTrue(any("merge_request_event" in error for error in errors))
        self.assertFalse(any("depth budget" in error for error in errors))

    def test_lane_bypass_variable_prefix_and_case(self):
        # IA-8 (mutant M10): the predicate matches the whole npm_config_*
        # family case-insensitively (npm lowercases every npm_config_<x>
        # into its config), not just npm_config_script_shell.
        for name in ("NODE_OPTIONS", "npm_config_script_shell", "npm_config_foo",
                     "NPM_CONFIG_SCRIPT_SHELL", "NPM_CONFIG_FOO"):
            with self.subTest(name=name, expected=True):
                self.assertTrue(config.lane_bypass_variable(name))
        for name in ("node_options", "NODE_OPTIONSS", "npm_configurations",
                     "npm_config", "BUN_OPTIONS", None, 5):
            with self.subTest(name=name, expected=False):
                self.assertFalse(config.lane_bypass_variable(name))

    def test_extends_chain_beyond_budget_fails_closed(self):
        # HP-2/HN-3: a chain longer than the budget cannot be certified —
        # the truncation sentinel must fail the lane even though the
        # dropped parent's gating key is never seen.
        merged = self.merged()
        depth = config.EXTENDS_DEPTH_BUDGET + 1
        merged[f".p{depth}"] = {"allow_failure": True}
        for index in range(depth - 1, 0, -1):
            merged[f".p{index}"] = {"extends": f".p{index + 1}"}
        merged["worker-kimi-ts"]["extends"] = ".p1"
        self.assertTrue(any("depth budget" in error for error in self.errors(merged)))

    def test_global_variables_outside_allowlist_are_caught(self):
        # MR !1658 review IA-5: a name denylist cannot enumerate every
        # variable that hollows a lane out (BUN_OPTIONS preloads a file
        # before bun's main script; PATH and GIT_CHECKOUT need one more
        # artifact), so every global variable must be allowlisted.
        merged = self.merged()
        merged["variables"] = self.variables(
            {
                name: "x"
                for name in config.GLOBAL_VARIABLE_ALLOWLIST
                if name not in config.EXPECTED_RUNNER_TAGS
            }
        )
        self.assertEqual(self.errors(merged), [])
        for name in ("BUN_OPTIONS", "PATH", "GIT_CHECKOUT", "SOME_NEW_VARIABLE"):
            with self.subTest(name=name):
                merged = self.merged()
                merged["variables"] = self.variables({name: "x"})
                self.assertTrue(
                    any("GLOBAL_VARIABLE_ALLOWLIST" in error for error in self.errors(merged))
                )

    def test_workflow_rule_variables_are_caught(self):
        # IA-1: workflow:rules:variables become pipeline-wide variables on
        # the pipelines the rule matches, overriding same-named globals, so
        # a NODE_OPTIONS or an unpinned CI_NODE_IMAGE there reaches both
        # lanes while their scripts and the global variables stay pinned.
        for variables in (
            {"NODE_OPTIONS": "--import=data:text/javascript,process.exit(0)"},
            {"CI_NODE_IMAGE": "docker.io/library/node:22-bookworm-slim"},
        ):
            with self.subTest(variables=variables):
                merged = self.merged()
                merged["workflow"] = {
                    "rules": [
                        {
                            "if": '$CI_PIPELINE_SOURCE == "merge_request_event"',
                            "variables": variables,
                        },
                    ]
                }
                self.assertTrue(
                    any(
                        "workflow:rules must not set variables" in error
                        for error in self.errors(merged)
                    )
                )

    def test_deprecated_global_before_script_is_caught(self):
        # IA-3: the deprecated top-level before_script is still honoured by
        # GitLab (equivalent to default:before_script) and inherited by
        # both lanes, which define none.
        merged = self.merged()
        merged["before_script"] = ["exit 0"]
        self.assertTrue(
            any("top-level before_script" in error for error in self.errors(merged))
        )

    def test_lane_rule_variables_are_caught(self):
        # IA-6: rules:variables on any lane rule reach the pinned script on
        # the pipelines that rule matches (push and tag pipelines too, not
        # only the first, MR rule).
        for job_name in self.LANES:
            for index in (0, 1):
                with self.subTest(job=job_name, rule=index):
                    merged = self.merged()
                    merged[job_name]["rules"][index]["variables"] = {
                        "NODE_OPTIONS": "--test-only"
                    }
                    self.assertTrue(
                        any(
                            f"{job_name} rules must not set variables" in error
                            for error in self.errors(merged)
                        )
                    )

    def test_hash_form_root_image_variable_is_caught(self):
        # IA-2: GitLab's `value:`/`description:` hash form defines the same
        # variable; the root file is merged last, so a hash-form override
        # of a pinned image variable must fail the digest pin exactly like
        # the string form in test_root_file_variables_override_is_caught.
        self.assertEqual(
            config.collect_variables(
                [
                    (
                        ROOT / "x.yml",
                        {
                            "variables": {
                                "A": {"value": "v", "description": "d"},
                                "B": "w",
                                "C": {"description": "no value"},
                            }
                        },
                    )
                ]
            ),
            {"A": "v", "B": "w"},
        )
        original_load_yaml = config.load_yaml

        def load_with_hash_form(path):
            loaded = original_load_yaml(path)
            if path == ROOT / ".gitlab-ci.yml":
                loaded.setdefault("variables", {})["CI_NODE_IMAGE"] = {
                    "value": "docker.io/library/node:22-bookworm-slim",
                    "description": "unpinned",
                }
            return loaded

        with (
            mock.patch.object(config, "load_yaml", side_effect=load_with_hash_form),
            mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            self.assertEqual(config.main(), 1)
        self.assertIn("CI_NODE_IMAGE", stderr.getvalue())

    def test_nested_and_conditional_includes_are_caught(self):
        # IA-4/IA-7: GitLab merges includes declared inside included files
        # and honours include:rules on root entries; the validator reads
        # exactly the seven root entries, so both shapes are refused.
        original_load_yaml = config.load_yaml

        def load_with_nested_include(path):
            loaded = original_load_yaml(path)
            if path == ROOT / ".gitlab" / "ci" / "containers.yml":
                loaded = {"include": [{"local": "/.gitlab/ci/evil.yml"}], **loaded}
            return loaded

        with (
            mock.patch.object(config, "load_yaml", side_effect=load_with_nested_include),
            mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            self.assertEqual(config.main(), 1)
        self.assertIn("containers.yml must not include further files", stderr.getvalue())

        def load_with_conditional_include(path):
            loaded = original_load_yaml(path)
            if path == ROOT / ".gitlab-ci.yml":
                for item in loaded["include"]:
                    if item.get("local") == "/.gitlab/ci/validate.yml":
                        item["rules"] = [{"if": '$CI_PIPELINE_SOURCE == "push"'}]
            return loaded

        with (
            mock.patch.object(config, "load_yaml", side_effect=load_with_conditional_include),
            mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            self.assertEqual(config.main(), 1)
        self.assertIn("root include entries must be plain local includes", stderr.getvalue())

    def test_root_workflow_override_is_checked(self):
        # IA-1: a root-file `workflow:` replaces the include's rules array
        # in GitLab; the workflow check must read the merged view, not
        # workflow.yml alone.
        original_load_yaml = config.load_yaml

        def load_with_root_workflow(path):
            loaded = original_load_yaml(path)
            if path == ROOT / ".gitlab-ci.yml":
                loaded["workflow"] = {"rules": [{"if": "$CI_COMMIT_BRANCH"}]}
            return loaded

        with (
            mock.patch.object(config, "load_yaml", side_effect=load_with_root_workflow),
            mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            self.assertEqual(config.main(), 1)
        self.assertIn("workflow must enable MR pipelines", stderr.getvalue())

    def test_root_default_is_deep_merged_over_include_default(self):
        # Pins the deep merge (IA-8 mutant M2): a harmless root `default:`
        # key must not hide an include-level default.before_script by
        # replacing the whole block.
        original_load_yaml = config.load_yaml

        def load_with_split_default(path):
            loaded = original_load_yaml(path)
            if path == ROOT / ".gitlab" / "ci" / "defaults.yml":
                loaded.setdefault("default", {})["before_script"] = ["exit 0"]
            if path == ROOT / ".gitlab-ci.yml":
                loaded["default"] = {"interruptible": True}
            return loaded

        with (
            mock.patch.object(config, "load_yaml", side_effect=load_with_split_default),
            mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            self.assertEqual(config.main(), 1)
        self.assertIn("default.before_script", stderr.getvalue())

    def test_later_include_overrides_earlier_include(self):
        # Pins the include order (IA-8 mutant M4): containers.yml comes
        # after validate.yml, so its worker-kimi-ts entry is the effective
        # job; merging in reverse would certify validate.yml's pinned block.
        original_load_yaml = config.load_yaml

        def load_with_later_override(path):
            loaded = original_load_yaml(path)
            if path == ROOT / ".gitlab" / "ci" / "containers.yml":
                loaded["worker-kimi-ts"] = {"script": ["exit 0"]}
            return loaded

        with (
            mock.patch.object(config, "load_yaml", side_effect=load_with_later_override),
            mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            self.assertEqual(config.main(), 1)
        self.assertIn(
            "worker-kimi-ts script must be exactly the pinned commands", stderr.getvalue()
        )

    def test_extends_parent_image_keys_are_deep_merged(self):
        # Pins the recursive merge (IA-8 mutant M18): nested hashes merge
        # key-wise at every level, so a job's partial `image:` hash keeps
        # the parent's name while overriding the entrypoint.
        self.assertEqual(
            config.merge_ci_hashes({"a": {"b": {"c": 1}}}, {"a": {"b": {"d": 2}}}),
            {"a": {"b": {"c": 1, "d": 2}}},
        )
        merged = self.merged()
        merged[".base"] = {"image": {"name": "$CI_NODE_IMAGE", "entrypoint": [""]}}
        merged["worker-kimi-ts"]["extends"] = ".base"
        merged["worker-kimi-ts"]["image"] = {"entrypoint": ["/bin/sh"]}
        effective = config.resolve_extends(merged["worker-kimi-ts"], merged)
        self.assertEqual(
            effective["image"], {"name": "$CI_NODE_IMAGE", "entrypoint": ["/bin/sh"]}
        )
        self.assertTrue(any("entrypoint" in error for error in self.errors(merged)))


class PebbleRustLaneTests(unittest.TestCase):
    """WI#375 DR-3: the worker-pebble-rust lane (pebble's Rust suite in MR
    pipelines) is pinned by config.validate_pebble_rust_lane — existence,
    the .rust-job extends, the merge_request_event rule, the resolved
    $CI_RUST_IMAGE image and rust runner tag, and the exact script from
    config.PEBBLE_RUST_LANE_SCRIPT; the real-tree binding is main() running
    the validator over the checked-in validate.yml."""

    RUST_JOB = {
        "image": {"name": "$CI_RUST_IMAGE"},
        "tags": ["$CI_RUST_RUNNER_TAG"],
        "before_script": [
            'df -h "$CI_PROJECT_DIR"',
            "bash .gitlab/scripts/bootstrap-rust.sh",
            "bash .gitlab/scripts/bootstrap-sccache.sh",
        ],
    }

    def merged(self) -> dict:
        return {
            ".rust-job": copy.deepcopy(self.RUST_JOB),
            "worker-pebble-rust": {
                "extends": ".rust-job",
                "stage": "validate",
                "needs": [],
                "rules": [
                    {"if": '$CI_PIPELINE_SOURCE == "merge_request_event"'},
                    {"if": '$CI_PIPELINE_SOURCE == "push" && $CI_COMMIT_BRANCH'},
                    {"when": "never"},
                ],
                "script": list(config.PEBBLE_RUST_LANE_SCRIPT),
            },
        }

    def errors(self, merged: dict) -> list[str]:
        errors: list[str] = []
        config.validate_pebble_rust_lane(merged, errors)
        return errors

    def test_valid_shape_passes(self):
        self.assertEqual(self.errors(self.merged()), [])

    def test_real_lane_matches_the_pin(self):
        # The pinned tuple must equal the checked-in validate.yml lane, so a
        # lane edit without a tuple update fails here (and in ci-config via
        # main()). The real .rust-job template comes from defaults.yml.
        merged = {}
        for include in config.EXPECTED_INCLUDES:
            merged.update(config.load_yaml(ROOT / include.lstrip("/")))
        self.assertEqual(self.errors(merged), [])

    def test_missing_lane_is_caught(self):
        merged = self.merged()
        del merged["worker-pebble-rust"]
        self.assertTrue(any("worker-pebble-rust job missing" in error for error in self.errors(merged)))

    def test_not_extending_rust_job_is_caught(self):
        for override in ({}, [], ".other-template"):
            with self.subTest(override=override):
                merged = self.merged()
                if override:
                    merged["worker-pebble-rust"]["extends"] = override
                else:
                    del merged["worker-pebble-rust"]["extends"]
                self.assertTrue(any(
                    "must extend .rust-job" in error for error in self.errors(merged)
                ))

    def test_job_level_keys_are_caught(self):
        # A job-level before_script/after_script replaces the audited
        # .rust-job bootstrap; variables/hooks/parallel bypass the pinned
        # script without touching it.
        for key, value in (
            ("variables", {"CARGO_TERM_COLOR": "never"}),
            ("hooks", {"pre_get_sources_script": ["true"]}),
            ("parallel", {"matrix": [{"CARGO_TERM_COLOR": ["never"]}]}),
            ("before_script", ["true"]),
            ("after_script", ["true"]),
            ("image", {"name": "$CI_RUST_IMAGE"}),
            ("tags", ["$CI_RUST_RUNNER_TAG"]),
        ):
            with self.subTest(key=key):
                merged = self.merged()
                merged["worker-pebble-rust"][key] = value
                self.assertTrue(any(
                    f"must not set job keys ['{key}']" in error for error in self.errors(merged)
                ))

    def test_template_image_drift_is_caught(self):
        merged = self.merged()
        merged[".rust-job"]["image"] = {"name": "$CI_BUN_IMAGE"}
        self.assertTrue(any("CI_RUST_IMAGE" in error for error in self.errors(merged)))

    def test_template_image_entrypoint_is_caught(self):
        # IX-1 (MR !1691 run-137): an entrypoint inherited from .rust-job can
        # end the job before the pinned script runs, with the image name
        # unchanged.
        merged = self.merged()
        merged[".rust-job"]["image"]["entrypoint"] = ["/bin/true"]
        self.assertTrue(any("CI_RUST_IMAGE" in error for error in self.errors(merged)))

    def test_missing_needs_is_caught(self):
        # IX-2 (MR !1691 run-137): without needs: [] a later-stage lane
        # inherits every earlier-stage dotenv report (variables) and
        # artifact.
        merged = self.merged()
        del merged["worker-pebble-rust"]["needs"]
        merged["worker-pebble-rust"]["stage"] = "check"
        self.assertTrue(any("needs: []" in error for error in self.errors(merged)))

    def test_template_tag_drift_is_caught(self):
        merged = self.merged()
        merged[".rust-job"]["tags"] = ["$CI_LINT_RUNNER_TAG"]
        self.assertTrue(any("CI_RUST_RUNNER_TAG" in error for error in self.errors(merged)))

    def test_template_bootstrap_exit_0_is_caught(self):
        # MR !1669 review IK-8: .rust-job is shared — an `exit 0` appended to
        # its before_script ends the job green before the pinned script runs.
        merged = self.merged()
        merged[".rust-job"]["before_script"].append("exit 0")
        self.assertTrue(any("before_script" in error for error in self.errors(merged)))

    def test_template_level_bypass_keys_are_caught(self):
        # MR !1669 review IK-8: template-level variables/hooks/inherit/
        # parallel survive extends resolution and bypass the pinned script.
        for key, value in (
            ("variables", {"RUSTC_WRAPPER": "/bin/true"}),
            ("hooks", {"pre_get_sources_script": ["echo x"]}),
            ("inherit", {"variables": False}),
            ("parallel", {"matrix": [{"RUSTFLAGS": ["--cfg x"]}]}),
        ):
            with self.subTest(key=key):
                merged = self.merged()
                merged[".rust-job"][key] = value
                self.assertTrue(
                    any(f"'{key}'" in error for error in self.errors(merged)),
                    self.errors(merged),
                )

    def test_never_rules_are_caught(self):
        merged = self.merged()
        merged["worker-pebble-rust"]["rules"] = [{"when": "never"}]
        self.assertTrue(any("merge_request_event" in error for error in self.errors(merged)))

    def test_rules_variables_are_caught(self):
        merged = self.merged()
        merged["worker-pebble-rust"]["rules"][0]["variables"] = {"RUSTFLAGS": ""}
        self.assertTrue(any("rules must not set variables" in error for error in self.errors(merged)))

    def test_prepended_exit_0_is_caught(self):
        merged = self.merged()
        merged["worker-pebble-rust"]["script"] = (
            ["exit 0"] + merged["worker-pebble-rust"]["script"]
        )
        self.assertTrue(any("exactly the pinned commands" in error for error in self.errors(merged)))

    def test_dropped_locked_flag_is_caught(self):
        merged = self.merged()
        merged["worker-pebble-rust"]["script"] = [
            command.replace(" --locked", "")
            for command in merged["worker-pebble-rust"]["script"]
        ]
        self.assertTrue(any("exactly the pinned commands" in error for error in self.errors(merged)))

    def test_main_runs_the_pebble_lane_validator(self):
        # Same class as the GV-2 pin for the TS lanes: deleting the main()
        # call must not pass the suite.
        with mock.patch.object(config, "validate_pebble_rust_lane") as lane_check:
            self.assertEqual(config.main(), 0)
        lane_check.assert_called_once()


class TrivyIgnoreTests(unittest.TestCase):
    """WI#361: .gitlab/trivy-ignore entries must use Trivy's real `exp:`
    syntax — getExpirationDate (pkg/result/ignore.go) only CutPrefix's
    `exp:`, so the former `expiring:` lines were permanent suppressions —
    and no entry may expire more than six months out."""

    TODAY = datetime.date(2026, 10, 17)
    VALID = "CVE-2025-7458 exp:2027-04-17"

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.ignore = pathlib.Path(temporary.name) / "trivy-ignore"

    def errors(self, text: str | None) -> list[str]:
        if text is not None:
            self.ignore.write_text(text, encoding="utf-8")
        errors = []
        with mock.patch.object(config, "TRIVY_IGNORE", self.ignore):
            config.validate_trivy_ignore(errors, today=self.TODAY)
        return errors

    def test_valid_file_passes(self):
        text = (
            "# header comment\n"
            "\n"
            "# rationale comment\n"
            f"{self.VALID}\n"
            "CVE-2026-60002 exp:2027-04-17\n"
        )
        self.assertEqual(self.errors(text), [])

    def test_expiring_spelling_is_caught(self):
        # The exact WI#361 defect shape: parses silently as no expiry.
        errors = self.errors("CVE-2026-13221 expiring:2027-03-06\n")
        self.assertTrue(any("must match" in error for error in errors))

    def test_bare_cve_is_caught(self):
        errors = self.errors("CVE-2025-7458\n")
        self.assertTrue(any("must match" in error for error in errors))

    def test_non_cve_id_is_caught(self):
        # Plain-file IDs apply to every scanner (vuln, secret, misconfig,
        # license); only CVE IDs belong here (run-143 JP-2). The second ID
        # has CVE-shaped digits, so a regex that only relaxes the prefix
        # (GO-, PYSEC-, RUSTSEC-) is caught too (MR !1750 review).
        for entry in ("GHSA-abcd-efgh-ijkl exp:2027-04-17", "RUSTSEC-2026-0001 exp:2027-04-17"):
            with self.subTest(entry):
                errors = self.errors(entry + chr(10))
                self.assertTrue(any("must match" in error for error in errors))

    def test_extra_field_is_caught(self):
        errors = self.errors(f"{self.VALID} # trailing note\n")
        self.assertTrue(any("must match" in error for error in errors))

    def test_impossible_date_is_caught(self):
        errors = self.errors("CVE-2025-7458 exp:2027-13-40\n")
        self.assertTrue(any("not a real calendar date" in error for error in errors))

    def test_far_future_expiry_is_caught(self):
        errors = self.errors("CVE-2025-7458 exp:2027-04-18\n")
        self.assertTrue(any("more than" in error and "months out" in error for error in errors))

    def test_horizon_edge_passes(self):
        # Exactly six calendar months out is still allowed.
        self.assertEqual(self.errors("CVE-2025-7458 exp:2027-04-17\n"), [])

    def test_missing_file_fails_closed(self):
        errors = self.errors(None)
        self.assertTrue(any("missing" in error for error in errors))

    def test_real_repo_file_passes(self):
        # Real date, not TODAY: the next re-verification moves every expiry
        # past TODAY + 6 months, which a fixed TODAY would then reject
        # (run-143 JP-2). The committed file holds its own re-verified
        # horizon at whatever date it was written.
        errors = []
        config.validate_trivy_ignore(errors)
        self.assertEqual(errors, [])

    def test_main_runs_the_ignore_validator(self):
        # GV-2 precedent: every test above calls the validator directly, so
        # deleting the main() call passed the whole suite (run-143 JP-2).
        self.ignore.write_text("CVE-2026-13221 expiring:2027-03-06", encoding="utf-8")
        with (
            mock.patch.object(config, "TRIVY_IGNORE", self.ignore),
            mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            self.assertEqual(config.main(), 1)
        self.assertIn("trivy-ignore:1 must match", stderr.getvalue())


class TrivyGateInputTests(unittest.TestCase):
    """WI#361 run-143 JP-1: the ignore-file guard only holds if the gate
    reads nothing else. The scan must run the pinned command (with
    --config /dev/null) and no un-allowlisted TRIVY_* variable may reach
    it from any GitLab variable source."""

    def setUp(self):
        self.merged = {}
        for include in config.EXPECTED_INCLUDES:
            self.merged = config.merge_ci_hashes(
                self.merged, config.load_yaml(ROOT / include.lstrip("/"))
            )
        self.merged = config.merge_ci_hashes(self.merged, config.load_yaml(config.ROOT_CONFIG))

    def errors(self, merged):
        errors = []
        config.validate_trivy_gate_inputs(merged, errors)
        return errors

    def test_real_tree_passes(self):
        self.assertEqual(self.errors(self.merged), [])

    def test_gate_command_is_pinned(self):
        merged = copy.deepcopy(self.merged)
        script = merged[".worker-scan"]["script"]
        merged[".worker-scan"]["script"] = [
            line.replace("--config /dev/null ", "") for line in script
        ]
        self.assertTrue(any("must run the CRITICAL gate" in error for error in self.errors(merged)))

    def test_trivy_variable_in_template_is_caught(self):
        merged = copy.deepcopy(self.merged)
        merged[".worker-scan"]["variables"] = dict(
            merged[".worker-scan"].get("variables", {}), TRIVY_IGNORE_UNFIXED="true"
        )
        self.assertTrue(any("set TRIVY_IGNORE_UNFIXED" in error for error in self.errors(merged)))

    def test_trivy_variable_in_scan_job_is_caught(self):
        merged = copy.deepcopy(self.merged)
        job = "worker-pebble-scan"
        self.assertIn(job, merged)
        merged[job]["variables"] = dict(
            merged[job].get("variables", {}), TRIVY_IGNORE_UNFIXED="true"
        )
        self.assertTrue(any(f"{job} variables set TRIVY_IGNORE_UNFIXED" in error for error in self.errors(merged)))

    def test_trivy_variable_in_scan_job_rule_is_caught(self):
        # A job's own rules replace the template's, so the job case is not
        # the template case (MR !1750 review).
        for job in (".worker-scan", "worker-kimi-scan"):
            with self.subTest(job):
                merged = copy.deepcopy(self.merged)
                rules = copy.deepcopy(merged[".worker-scan"]["rules"])
                rules[0]["variables"] = {"TRIVY_IGNORE_UNFIXED": "true"}
                merged[job]["rules"] = rules
                self.assertTrue(any(f"{job} rule variables set TRIVY_IGNORE_UNFIXED" in error for error in self.errors(merged)))

    def test_trivy_variable_in_workflow_rule_is_caught(self):
        merged = copy.deepcopy(self.merged)
        workflow = merged.setdefault("workflow", {"rules": [{}]})
        workflow["rules"][0]["variables"] = {"TRIVY_SKIP_UPDATE": "true"}
        self.assertTrue(any("workflow rule variables set TRIVY_SKIP_UPDATE" in error for error in self.errors(merged)))

    def test_trivy_variable_at_top_level_is_caught(self):
        merged = copy.deepcopy(self.merged)
        merged["variables"] = dict(merged.get("variables", {}), TRIVY_IGNORE_UNFIXED="true")
        self.assertTrue(any("top-level variables set TRIVY_IGNORE_UNFIXED" in error for error in self.errors(merged)))

    def test_allowlisted_trivy_variable_value_is_pinned(self):
        # The cache dir decides which vulnerability DB the gate trusts, so
        # an allowlisted key is pinned by value too (MR !1750 review).
        merged = copy.deepcopy(self.merged)
        merged[".worker-scan"]["variables"] = dict(
            merged[".worker-scan"].get("variables", {}), TRIVY_CACHE_DIR="/tmp/trivy"
        )
        self.assertTrue(any(".worker-scan variables must be exactly" in error for error in self.errors(merged)))

    def test_gate_line_suffix_or_extra_line_is_caught(self):
        # MR !1750 review: the startswith() pin let every one of these pass,
        # and each makes trivy (or the job) exit 0 on an unfixed CRITICAL.
        # The gate call is wrapped in trivy_with_retry (item 28), so locate it
        # by the gate command itself, not by the line prefix.
        script = list(config.WORKER_SCAN_SCRIPT)
        gate = next(
            index
            for index, line in enumerate(script)
            if config.TRIVY_GATE_COMMAND in line
        )
        cases = {
            "--ignore-unfixed": script[:gate] + [script[gate] + " --ignore-unfixed"] + script[gate + 1:],
            "--exit-code 0": script[:gate] + [script[gate] + " --exit-code 0"] + script[gate + 1:],
            "or-true": script[:gate] + [script[gate] + " || true"] + script[gate + 1:],
            "exit-0-first": script[:gate] + ["exit 0"] + script[gate:],
            "export": script[:gate] + ["export TRIVY_IGNORE_UNFIXED=true"] + script[gate:],
        }
        for label, mutated in cases.items():
            with self.subTest(label):
                merged = copy.deepcopy(self.merged)
                merged[".worker-scan"]["script"] = mutated
                self.assertTrue(any(".worker-scan must run the CRITICAL gate" in error for error in self.errors(merged)))

    def test_every_trivy_call_ignores_checkout_config_and_cache(self):
        # The first trivy call fills the cache the gate reuses, and a
        # committed .trivy-cache DB with a future NextUpdate is never
        # re-downloaded (MR !1750 review).
        for label, line in (
            ("report call config", next(line for line in config.WORKER_SCAN_SCRIPT if "gitlab.tpl" in line)),
            ("cache reset", 'rm -rf "$TRIVY_CACHE_DIR"'),
        ):
            with self.subTest(label):
                merged = copy.deepcopy(self.merged)
                script = merged[".worker-scan"]["script"]
                index = script.index(line)
                if label == "cache reset":
                    del script[index]
                else:
                    script[index] = line.replace("--config /dev/null ", "")
                self.assertTrue(any("must run the CRITICAL gate" in error for error in self.errors(merged)))

    def test_scan_job_may_only_wire_extends_and_needs(self):
        # GitLab replaces the template's script and merges variables from
        # the job, and a job allow_failure applies when the tag rule sets
        # none; the template-only view saw none of them (MR !1750 review).
        overrides = {
            "script": ["echo skipped"],
            "before_script": ["export TRIVY_IGNORE_UNFIXED=true"],
            "after_script": ["cp clean.json worker-vulnerabilities.json"],
            "allow_failure": True,
            "variables": {"PATH": "$CI_PROJECT_DIR/fakebin:/usr/bin:/bin"},
            "image": {"name": "docker.io/aquasec/trivy:0.74.0@sha256:" + "0" * 64, "entrypoint": [""]},
            "timeout": "1h",
        }
        for key, value in overrides.items():
            with self.subTest(key):
                merged = copy.deepcopy(self.merged)
                merged["worker-kimi-scan"][key] = value
                self.assertTrue(any(
                    f"worker-kimi-scan must not set job keys ['{key}']" in error
                    for error in self.errors(merged)
                ))

    def test_scan_job_image_override_is_caught_by_value(self):
        merged = copy.deepcopy(self.merged)
        merged["worker-kimi-scan"]["image"] = {"name": "docker.io/aquasec/trivy:0.74.0@sha256:" + "0" * 64, "entrypoint": [""]}
        self.assertTrue(any("worker-kimi-scan image must be exactly" in error for error in self.errors(merged)))

    def test_scan_retry_pin_is_enforced(self):
        # Item 70: the bounded scan auto-retry (transient runner kills like
        # pipeline 5263's exit-137 SIGKILL mid SBOM) is a deliberate policy,
        # so it is pinned by value like the image and tags.
        mutations = {
            "removed": lambda template: template.pop("retry"),
            "wider": lambda template: template.update(retry={"max": 2, "when": ["always"]}),
            "narrower": lambda template: template.update(
                retry={"max": 1, "when": ["runner_system_failure"]}
            ),
        }
        for label, mutate in mutations.items():
            with self.subTest(label):
                merged = copy.deepcopy(self.merged)
                mutate(merged[".worker-scan"])
                self.assertTrue(any(
                    ".worker-scan retry must be exactly" in error
                    for error in self.errors(merged)
                ))

    def test_scan_job_retry_override_is_caught(self):
        # A job-level retry would replace the template policy wholesale.
        merged = copy.deepcopy(self.merged)
        merged["worker-kimi-scan"]["retry"] = {"max": 5}
        self.assertTrue(any(
            "worker-kimi-scan must not set job keys ['retry']" in error
            for error in self.errors(merged)
        ))

    def test_second_extends_parent_is_caught(self):
        for label, parent in (
            ("variables", {"variables": {"TRIVY_IGNORE_UNFIXED": "true"}}),
            ("harmless", {"timeout": "1h"}),
        ):
            with self.subTest(label):
                merged = copy.deepcopy(self.merged)
                merged[".scan-tuning"] = parent
                merged["worker-kimi-scan"]["extends"] = [".worker-scan", ".scan-tuning"]
                self.assertTrue(any("worker-kimi-scan must extend exactly .worker-scan" in error for error in self.errors(merged)))

    def test_template_hooks_and_allow_failure_are_caught(self):
        for key, value in (
            ("before_script", ["export TRIVY_IGNORE_UNFIXED=true"]),
            ("after_script", ["true"]),
            ("allow_failure", True),
            ("hooks", {"pre_get_sources_script": ["true"]}),
        ):
            with self.subTest(key):
                merged = copy.deepcopy(self.merged)
                merged[".worker-scan"][key] = value
                self.assertTrue(any(f".worker-scan must not carry job keys ['{key}']" in error for error in self.errors(merged)))

    def test_template_extends_is_caught(self):
        merged = copy.deepcopy(self.merged)
        merged[".scan-base"] = {"timeout": "1h"}
        merged[".worker-scan"]["extends"] = ".scan-base"
        self.assertTrue(any(".worker-scan must not set ['extends']" in error for error in self.errors(merged)))

    def test_release_tag_rule_cannot_be_advisory(self):
        for key, value in (("allow_failure", True), ("when", "manual"), ("variables", {"X": "1"})):
            with self.subTest(key):
                merged = copy.deepcopy(self.merged)
                rule = next(rule for rule in merged[".worker-scan"]["rules"] if "CI_COMMIT_TAG" in rule.get("if", ""))
                rule[key] = value
                self.assertTrue(any("release-tag rule carrying only `if`" in error for error in self.errors(merged)))

    def test_rule_variables_are_refused(self):
        # Any rules:variables entry (PATH as well as TRIVY_*) reaches the gate.
        merged = copy.deepcopy(self.merged)
        rule = next(rule for rule in merged[".worker-scan"]["rules"] if "CI_DEFAULT_BRANCH" in rule.get("if", ""))
        rule["variables"] = {"PATH": "/x"}
        self.assertTrue(any(".worker-scan rules must not set variables" in error for error in self.errors(merged)))

    def test_main_runs_the_gate_input_validator(self):
        # GV-2 precedent: deleting the main() call must not pass the suite.
        with mock.patch.object(config, "validate_trivy_gate_inputs") as gate_check:
            self.assertEqual(config.main(), 0)
        gate_check.assert_called_once()


class ProxyToolSetSyncTests(unittest.TestCase):
    """MR !1780 review LM-5 (run 171): nothing kept mt-admin's
    EXTERNAL_WORKER_PROXY_TOOLS (09-worker-config.sh) in sync with the
    daemon's PROXY_TOOL_ALLOWLIST (ic/src/orchestrator/external_worker.rs),
    so a daemon change drifted silently — a widened set is refused at the
    operator boundary, a narrowed set is written and then stripped at load.
    config.validate_proxy_tool_set_sync closes both drifts and fails closed
    on a renamed constant or a missing file."""

    SHELL_TOOLS = (
        "memory_read memory_write knowledge_search knowledge_submit_note "
        "knowledge_get_topic gitlab_tool gitlab-tool"
    )
    DAEMON_TOOLS = (
        "memory_read",
        "memory_write",
        "knowledge_search",
        "knowledge_submit_note",
        "knowledge_get_topic",
        "gitlab_tool",
        "gitlab-tool",
    )

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.worker_config = self.root / "09-worker-config.sh"
        self.write_shell(self.SHELL_TOOLS)
        self.daemon_source = self.root / "external_worker.rs"
        self.write_daemon(*self.DAEMON_TOOLS)

    def write_shell(self, tools: str):
        self.worker_config.write_text(
            f'EXTERNAL_WORKER_PROXY_TOOLS="{tools}"\n', encoding="utf-8"
        )

    def write_daemon(self, *names: str, comment: str | None = None):
        lines = [f'    "{name}",' for name in names]
        if comment is not None:
            lines.append(f"    // {comment}")
        self.daemon_source.write_text(
            "const PROXY_TOOL_ALLOWLIST: &[&str] = &[\n" + "\n".join(lines) + "\n];\n",
            encoding="utf-8",
        )

    def errors(self):
        errors = []
        with mock.patch.multiple(
            config,
            MT_ADMIN_WORKER_CONFIG=self.worker_config,
            EXTERNAL_WORKER_SOURCE=self.daemon_source,
        ):
            config.validate_proxy_tool_set_sync(errors)
        return errors

    def test_matching_sets_pass(self):
        self.assertEqual(self.errors(), [])

    def test_real_tree_lists_match(self):
        # The review's ask, verbatim: extract both lists from the checked-in
        # sources and assert they are equal (main() runs the same validator in
        # the ci-config job).
        errors = []
        config.validate_proxy_tool_set_sync(errors)
        self.assertEqual(errors, [])

    def test_daemon_set_widened_without_the_shell_is_caught(self):
        # The shell list lost a daemon tool: a widened PROXY_TOOL_ALLOWLIST
        # would be refused at the operator boundary (grant never issuable).
        self.write_shell("memory_read memory_write")
        self.assertTrue(any("missing daemon tools" in error for error in self.errors()))

    def test_daemon_set_narrowed_without_the_shell_is_caught(self):
        # The daemon dropped tools the shell still grants: written as a grant,
        # then stripped at load (the "lying config" shape).
        self.write_daemon("memory_read", "gitlab_tool", "gitlab-tool")
        self.assertTrue(any("stripped at daemon load" in error for error in self.errors()))

    def test_missing_declaration_or_file_is_caught(self):
        # Fail closed: no vacuous pass when a constant is renamed away or a
        # file goes missing.
        self.worker_config.write_text(
            "readonly OTHER_TOOLS='memory_read memory_write'\n", encoding="utf-8"
        )
        self.assertTrue(any("EXTERNAL_WORKER_PROXY_TOOLS" in error for error in self.errors()))
        self.write_shell(self.SHELL_TOOLS)
        self.daemon_source.write_text("const OTHER_ALLOWLIST: &[&str] = &[];\n", encoding="utf-8")
        self.assertTrue(any("PROXY_TOOL_ALLOWLIST" in error for error in self.errors()))
        self.daemon_source.unlink()
        self.assertTrue(any("external_worker.rs missing" in error for error in self.errors()))

    def test_shell_duplicate_entries_are_caught(self):
        duplicated = self.SHELL_TOOLS.replace(
            " knowledge_search", " knowledge_search memory_read", 1
        )
        self.write_shell(duplicated)
        self.assertTrue(any("duplicate entries" in error for error in self.errors()))

    def test_daemon_comment_strings_do_not_count_as_tools(self):
        # A quoted name inside the const body's prose must not widen the
        # extracted set (dropping the comment strip fails this pin).
        self.write_daemon(*self.DAEMON_TOOLS, comment='mentioning "shell_exec" here')
        self.assertEqual(self.errors(), [])

    def test_main_runs_the_sync_validator(self):
        # GV-2 precedent: deleting the main() call must not pass the suite.
        with mock.patch.object(config, "validate_proxy_tool_set_sync") as sync_check:
            self.assertEqual(config.main(), 0)
        sync_check.assert_called_once()


if __name__ == "__main__":
    unittest.main()
