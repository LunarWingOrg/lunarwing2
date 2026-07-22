from __future__ import annotations

import os
import tempfile
import unittest

from lunarwing_mt_onboard import import_tenant
from lunarwing_mt_onboard.import_tenant import ImportConfig


class TestImportConfigDefaults(unittest.TestCase):
    def test_defaults_are_dry_run_stage_only_and_noninteractive(self) -> None:
        cfg = ImportConfig(bundle="/tmp/tenant.tar")

        self.assertFalse(cfg.apply)
        self.assertFalse(cfg.start)
        self.assertFalse(cfg.old_stopped)
        self.assertTrue(cfg.auto_yes)


class TestImportConfigSerialization(unittest.TestCase):
    def test_roundtrip_json_preserves_all_fields(self) -> None:
        cfg = ImportConfig(
            bundle="/tmp/alpha.tar",
            name="alpha-new",
            start=True,
            old_stopped=True,
            with_nanocode=True,
            with_pebble=True,
            with_opencode=True,
            with_toolchains=True,
            with_vision=True,
            docker_group=True,
            owner_scope="legacy-alpha",
            apply=True,
            force=True,
            auto_yes=False,
            passphrase="not-written-to-disk",
        )
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "cfg.json")
            cfg.to_json(path)
            loaded = ImportConfig.from_json(path)

            mode = os.stat(path).st_mode & 0o777

        self.assertEqual(loaded, cfg)
        self.assertEqual(loaded.passphrase, "")
        self.assertEqual(mode, 0o600)

    def test_passphrase_is_not_serialized_or_represented(self) -> None:
        secret = "legacy archive password"
        cfg = ImportConfig(bundle="/tmp/alpha.7z", passphrase=secret)

        self.assertNotIn("passphrase", cfg.to_dict())
        self.assertNotIn(secret, repr(cfg))

    def test_to_dict_roundtrip_uses_safe_defaults(self) -> None:
        cfg = ImportConfig.from_dict(
            {
                "bundle": "/tmp/beta.tar",
                "name": True,
                "start": "yes",
                "auto_yes": False,
            }
        )

        self.assertEqual(cfg.bundle, "/tmp/beta.tar")
        self.assertEqual(cfg.name, "")
        self.assertFalse(cfg.start)
        self.assertFalse(cfg.auto_yes)
        self.assertEqual(ImportConfig.from_dict(cfg.to_dict()), cfg)


class TestImportConfigValidation(unittest.TestCase):
    def test_valid_bundle_without_name(self) -> None:
        self.assertIsNone(ImportConfig(bundle="/tmp/alpha.tar").validate())

    def test_empty_bundle_rejected(self) -> None:
        self.assertEqual(
            ImportConfig(bundle="  ").validate(), "bundle path is required"
        )

    def test_invalid_optional_name_rejected(self) -> None:
        self.assertIsNotNone(
            ImportConfig(bundle="/tmp/alpha.tar", name="BAD NAME").validate()
        )

    def test_multiline_passphrase_rejected(self) -> None:
        error = ImportConfig(
            bundle="/tmp/alpha.7z", passphrase="first\nsecond"
        ).validate()
        self.assertIn("line breaks", error or "")


class TestEnsureImportScript(unittest.TestCase):
    def test_missing_script_raises(self) -> None:
        previous = import_tenant.IMPORT_SCRIPT
        import_tenant.IMPORT_SCRIPT = "/nonexistent/import.sh"
        try:
            with self.assertRaises(FileNotFoundError):
                import_tenant.ensure_import_script()
        finally:
            import_tenant.IMPORT_SCRIPT = previous

    def test_override_is_used_when_executable(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            script = _write_script(tmp, "import-tenant.sh", "echo ok")
            previous = import_tenant.IMPORT_SCRIPT
            import_tenant.IMPORT_SCRIPT = script
            try:
                found = import_tenant.ensure_import_script()
            finally:
                import_tenant.IMPORT_SCRIPT = previous

        self.assertEqual(found, script)


class TestImportArgs(unittest.TestCase):
    def test_default_is_bundle_first_dry_run_and_yes(self) -> None:
        args = _build_args(ImportConfig(bundle="/tmp/alpha.tar"))

        self.assertEqual(args[1], "/tmp/alpha.tar")
        self.assertIn("--dry-run", args)
        self.assertIn("--yes", args)
        self.assertNotIn("--start", args)
        self.assertNotIn("--old-stopped", args)

    def test_apply_stage_omits_dry_run_and_start(self) -> None:
        args = _build_args(ImportConfig(bundle="/tmp/alpha.tar", apply=True))

        self.assertNotIn("--dry-run", args)
        self.assertNotIn("--start", args)
        self.assertIn("--yes", args)

    def test_start_and_old_stopped_are_independent_flags(self) -> None:
        start_only = _build_args(
            ImportConfig(bundle="/tmp/alpha.tar", apply=True, start=True)
        )
        both = _build_args(
            ImportConfig(
                bundle="/tmp/alpha.tar", apply=True, start=True, old_stopped=True
            )
        )

        self.assertIn("--start", start_only)
        self.assertNotIn("--old-stopped", start_only)
        self.assertIn("--start", both)
        self.assertIn("--old-stopped", both)

    def test_all_optional_flags_are_forwarded(self) -> None:
        args = _build_args(
            ImportConfig(
                bundle="/tmp/alpha.tar",
                name="alpha-new",
                with_nanocode=True,
                with_pebble=True,
                with_opencode=True,
                with_toolchains=True,
                with_vision=True,
                docker_group=True,
                owner_scope="legacy-alpha",
                apply=True,
                force=True,
            )
        )

        self.assertIn("--name", args)
        self.assertIn("alpha-new", args)
        self.assertIn("--with-nanocode", args)
        self.assertIn("--with-pebble", args)
        self.assertIn("--with-opencode", args)
        self.assertIn("--with-toolchains", args)
        self.assertIn("--with-vision", args)
        self.assertIn("--docker-group", args)
        self.assertIn("--owner-scope", args)
        self.assertIn("legacy-alpha", args)
        self.assertIn("--force", args)

    def test_auto_yes_can_be_disabled_for_non_web_callers(self) -> None:
        args = _build_args(ImportConfig(bundle="/tmp/alpha.tar", auto_yes=False))

        self.assertNotIn("--yes", args)


class TestImportPhaseNames(unittest.TestCase):
    def test_run_import_uses_import_phase_name(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            script = _write_script(tmp, "import-tenant.sh", "echo ok\nexit 0")
            previous = import_tenant.IMPORT_SCRIPT
            import_tenant.IMPORT_SCRIPT = script
            try:
                result = import_tenant.run_import(
                    ImportConfig(bundle="/tmp/alpha.tar", apply=True)
                )
            finally:
                import_tenant.IMPORT_SCRIPT = previous

        self.assertTrue(result.ok)
        self.assertEqual(result.phases[0].name, "import")

    @unittest.skipUnless(os.name == "posix", "pass_fds requires POSIX")
    def test_run_import_supplies_passphrase_through_fd(self) -> None:
        passphrase = "archive password"
        body = (
            'fd="$KAWARIMI_PASS_FD"\n'
            'eval "IFS= read -r supplied <&${fd}"\n'
            f'[ "$supplied" = "{passphrase}" ]\n'
            'case "$*" in *"archive password"*) exit 9;; esac\n'
            "exit 0"
        )
        with tempfile.TemporaryDirectory() as tmp:
            script = _write_script(tmp, "import-tenant.sh", body)
            previous = import_tenant.IMPORT_SCRIPT
            import_tenant.IMPORT_SCRIPT = script
            try:
                result = import_tenant.run_import(
                    ImportConfig(
                        bundle="/tmp/alpha.7z",
                        apply=True,
                        passphrase=passphrase,
                    )
                )
            finally:
                import_tenant.IMPORT_SCRIPT = previous

        self.assertTrue(result.ok)


def _build_args(cfg: ImportConfig) -> list[str]:
    with tempfile.TemporaryDirectory() as tmp:
        script = _write_script(tmp, "import-tenant.sh", "echo ok")
        previous = import_tenant.IMPORT_SCRIPT
        import_tenant.IMPORT_SCRIPT = script
        try:
            return import_tenant.build_import_args(cfg)
        finally:
            import_tenant.IMPORT_SCRIPT = previous


def _write_script(directory: str, name: str, body: str) -> str:
    path = os.path.join(directory, name)
    with open(path, "w") as f:
        f.write(f"#!/bin/sh\n{body}\n")
    os.chmod(path, 0o700)
    return path


if __name__ == "__main__":
    unittest.main()
