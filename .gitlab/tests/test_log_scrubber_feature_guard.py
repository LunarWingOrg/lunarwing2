#!/usr/bin/env python3
"""WI#662: lunarwing_safety/test-support must stay dev-only."""

import importlib.util
import io
import pathlib
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "validate_config", ROOT / ".gitlab/scripts/validate_config.py"
)
config = importlib.util.module_from_spec(spec)
spec.loader.exec_module(config)

ROOT_MANIFEST = """[workspace]
members = [".", "crates/engine"]

[package]
name = "lunarwing"
version = "2.2.0"
rust-version = "1.96.1"

[dev-dependencies]
lunarwing_safety = { path = "crates/lunarwing_safety", features = ["test-support"] }

[target.'cfg(unix)'.dev-dependencies]
lunarwing_safety = { path = "crates/lunarwing_safety", features = ["test-support"] }
"""

ENGINE_MANIFEST = """[package]
name = "lunarwing_engine"
version = "2.2.0"
"""


class LogScrubberFeatureGuardTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.workspace = self.root / "ic" / "Cargo.toml"
        self.engine = self.root / "ic" / "crates" / "engine" / "Cargo.toml"
        self.engine.parent.mkdir(parents=True)
        self.write_root()
        self.write_engine()

    def write_root(self, extra=""):
        self.workspace.parent.mkdir(parents=True, exist_ok=True)
        self.workspace.write_text(ROOT_MANIFEST + extra, encoding="utf-8")

    def write_engine(self, extra=""):
        self.engine.write_text(ENGINE_MANIFEST + extra, encoding="utf-8")

    def errors(self):
        errors = []
        with mock.patch.multiple(config, ROOT=self.root, WORKSPACE_MANIFEST=self.workspace):
            config.validate_log_scrubber_test_support(errors)
        return errors

    def assert_forbidden(self, section):
        errors = self.errors()
        self.assertTrue(
            any(
                section in error and "lunarwing_safety/test-support" in error
                for error in errors
            ),
            errors,
        )

    def test_checked_in_manifests_pass(self):
        errors = []
        config.validate_log_scrubber_test_support(errors)
        self.assertEqual(errors, [])

    def test_regular_and_target_dev_dependencies_pass(self):
        self.assertEqual(self.errors(), [])

    def test_normal_and_build_dependency_features_fail(self):
        for section in ("dependencies", "build-dependencies"):
            with self.subTest(section=section):
                self.write_root(
                    f"\n[{section}]\n"
                    'lunarwing_safety = { path = "crates/lunarwing_safety", '
                    'features = ["test-support"] }\n'
                )
                self.assert_forbidden(f"[{section}]")

    def test_target_normal_and_build_dependency_features_fail(self):
        for section in ("dependencies", "build-dependencies"):
            with self.subTest(section=section):
                self.write_root(
                    f"\n[target.'cfg(unix)'.{section}]\n"
                    'lunarwing_safety = { path = "crates/lunarwing_safety", '
                    'features = ["test-support"] }\n'
                )
                self.assert_forbidden(f"target.cfg(unix).{section}")

    def test_another_workspace_member_cannot_enable_the_feature(self):
        self.write_engine(
            "\n[dependencies]\n"
            'lunarwing_safety = { path = "../lunarwing_safety", features = ["test-support"] }\n'
        )
        self.assert_forbidden("ic/crates/engine/Cargo.toml [dependencies]")

    def test_renamed_dependency_and_workspace_dependency_fail(self):
        for declaration in (
            '[dependencies]\nsafety = { package = "lunarwing_safety", '
            'path = "crates/lunarwing_safety", features = ["test-support"] }\n',
            '[workspace.dependencies]\nlunarwing_safety = '
            '{ path = "crates/lunarwing_safety", features = ["test-support"] }\n',
        ):
            with self.subTest(declaration=declaration):
                self.write_root("\n" + declaration)
                self.assert_forbidden("outside dev-dependencies")

    def test_feature_table_activation_fails(self):
        for dependency, activation in (
            ("lunarwing_safety", "lunarwing_safety/test-support"),
            ("lunarwing_safety", "lunarwing_safety?/test-support"),
            ("safety", "safety/test-support"),
        ):
            with self.subTest(activation=activation):
                package = 'package = "lunarwing_safety", ' if dependency == "safety" else ""
                self.write_root(
                    f"\n[dependencies]\n{dependency} = {{ {package}"
                    'path = "crates/lunarwing_safety" }\n'
                    f'\n[features]\ndefault = ["{activation}"]\n'
                )
                self.assert_forbidden("[features]")

    def test_workspace_alias_activation_in_member_fails(self):
        self.write_root(
            '\n[workspace.dependencies]\nsafety = { package = "lunarwing_safety", '
            'path = "crates/lunarwing_safety" }\n'
        )
        self.write_engine(
            "\n[dependencies]\nsafety = { workspace = true }\n"
            '\n[features]\ndefault = ["safety/test-support"]\n'
        )
        self.assert_forbidden("ic/crates/engine/Cargo.toml [features]")

    def test_other_crates_may_have_a_test_support_feature(self):
        self.write_engine(
            '\n[dependencies]\nother = { path = "../other" }\n'
            '\n[features]\ndefault = ["other/test-support"]\n'
        )
        self.assertEqual(self.errors(), [])

    def test_unreadable_member_manifest_fails_closed(self):
        self.engine.write_text("not valid TOML", encoding="utf-8")
        self.assertTrue(any("cannot inspect" in error for error in self.errors()))

    def test_ci_config_main_rejects_a_normal_dependency_edge(self):
        self.write_root(
            "\n[dependencies]\n"
            'lunarwing_safety = { path = "crates/lunarwing_safety", '
            'features = ["test-support"] }\n'
        )
        with (
            mock.patch.object(config, "WORKSPACE_MANIFEST", self.workspace),
            mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            result = config.main()
        self.assertEqual(result, 1)
        self.assertIn("outside dev-dependencies (WI#662)", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
