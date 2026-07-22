"""Unit tests for config validation, serialization, and secrets helpers."""

from __future__ import annotations

import json
import os
import tempfile
import unittest
from unittest.mock import call, patch

from lunarwing_mt_onboard import provisioner
from lunarwing_mt_onboard.config import TenantConfig, WorkerType
from lunarwing_mt_onboard.secrets import (
    generate_master_key,
    is_valid_master_key,
    mask_secret,
)


class TestTenantNameValidation(unittest.TestCase):
    def test_valid_simple_name(self):
        self.assertIsNone(TenantConfig.validate_name("ruffles"))

    def test_valid_hyphenated_name(self):
        self.assertIsNone(TenantConfig.validate_name("my-tenant"))

    def test_empty_name_rejected(self):
        self.assertEqual(TenantConfig.validate_name(""), "tenant name is required")

    def test_uppercase_rejected(self):
        err = TenantConfig.validate_name("Ruffles")
        self.assertIsNotNone(err)

    def test_underscore_rejected(self):
        err = TenantConfig.validate_name("my_tenant")
        self.assertIsNotNone(err)

    def test_reserved_prefix_rejected(self):
        for prefix in ("pg-", "proxy-", "nanocode-", "pebble-", "opencode-", "weechat-"):
            with self.subTest(prefix=prefix):
                err = TenantConfig.validate_name(f"{prefix}1")
                self.assertIsNotNone(err)


class TestConfigSerialization(unittest.TestCase):
    def test_roundtrip_json(self):
        config = TenantConfig(
            name="alpha",
            gateway_host="0.0.0.0",
            xmpp_enabled=True,
            xmpp_jid="alpha@xmpp.localhost",
            workers=[WorkerType.NANOCODE, WorkerType.OPENCODE],
            secrets_master_key="ab" * 32,
        )
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "cfg.json")
            config.to_json(path)
            loaded = TenantConfig.from_json(path)
        self.assertEqual(loaded.name, "alpha")
        self.assertEqual(loaded.gateway_host, "0.0.0.0")
        self.assertTrue(loaded.xmpp_enabled)
        self.assertEqual(loaded.workers, [WorkerType.NANOCODE, WorkerType.OPENCODE])
        self.assertEqual(loaded.secrets_master_key, "ab" * 32)

    def test_current_llm_and_worker_overrides_round_trip(self):
        config = TenantConfig(
            name="alpha",
            llm_base_url="http://127.0.0.1:4000/openai/v1",
            nanocode_model="nano-model",
            nanocode_base_url="http://nano.test/v1",
            opencode_model="open-model",
            opencode_base_url="http://open.test/v1",
        )
        self.assertEqual(TenantConfig.from_dict(config.to_dict()), config)

    def test_to_dict_serializes_workers_as_strings(self):
        config = TenantConfig(
            name="beta",
            workers=[WorkerType.PEBBLE],
        )
        data = config.to_dict()
        self.assertEqual(data["workers"], ["pebble"])

    def test_from_dict_accepts_string_workers(self):
        data = {
            "name": "gamma",
            "workers": ["nanocode", "opencode"],
        }
        config = TenantConfig.from_dict(data)
        self.assertEqual(config.workers, [WorkerType.NANOCODE, WorkerType.OPENCODE])

    def test_json_file_permissions(self):
        config = TenantConfig(name="perms")
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "cfg.json")
            config.to_json(path)
            mode = os.stat(path).st_mode & 0o777
            self.assertEqual(mode, 0o600)

    def test_json_is_valid_format(self):
        config = TenantConfig(name="json-check")
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "cfg.json")
            config.to_json(path)
            with open(path) as f:
                data = json.loads(f.read())
            self.assertIn("name", data)
            self.assertEqual(data["name"], "json-check")

    def test_weechat_bootstrap_defaults_false(self):
        config = TenantConfig(name="alpha")
        self.assertFalse(config.no_weechat_bootstrap)

    def test_weechat_bootstrap_opt_out_round_trips(self):
        config = TenantConfig(name="alpha", no_weechat_bootstrap=True)
        restored = TenantConfig.from_dict(config.to_dict())
        self.assertTrue(restored.no_weechat_bootstrap)

    def test_old_json_without_weechat_field_defaults_false(self):
        old_data = {
            "name": "legacy",
            "no_ssh": False,
            "no_health": False,
        }
        config = TenantConfig.from_dict(old_data)
        self.assertFalse(config.no_weechat_bootstrap)


class TestProvisionerArgs(unittest.TestCase):
    def test_custom_gateway_host_is_forwarded_to_mt_admin(self):
        with tempfile.TemporaryDirectory() as tmp:
            script = os.path.join(tmp, "lunarwing-mt-admin.sh")
            with open(script, "w") as f:
                f.write("#!/bin/sh\n")
            os.chmod(script, 0o700)

            previous = provisioner.MT_ADMIN_SCRIPT
            provisioner.MT_ADMIN_SCRIPT = script
            try:
                config = TenantConfig(name="alpha", gateway_host="10.0.0.25")
                args = provisioner.build_add_tenant_args(config)
            finally:
                provisioner.MT_ADMIN_SCRIPT = previous

        self.assertIn("--gateway-host", args)
        self.assertIn("10.0.0.25", args)

    def test_custom_xmpp_jid_is_forwarded_to_mt_admin(self):
        with tempfile.TemporaryDirectory() as tmp:
            script = os.path.join(tmp, "lunarwing-mt-admin.sh")
            with open(script, "w") as f:
                f.write("#!/bin/sh\n")
            os.chmod(script, 0o700)

            previous = provisioner.MT_ADMIN_SCRIPT
            provisioner.MT_ADMIN_SCRIPT = script
            try:
                config = TenantConfig(
                    name="alpha",
                    xmpp_enabled=True,
                    xmpp_jid="alpha@chat.example.net",
                )
                args = provisioner.build_add_tenant_args(config)
            finally:
                provisioner.MT_ADMIN_SCRIPT = previous

        self.assertIn("--xmpp-jid", args)
        self.assertIn("alpha@chat.example.net", args)

    def test_darkirc_flag_is_forwarded_to_mt_admin(self):
        with tempfile.TemporaryDirectory() as tmp:
            script = os.path.join(tmp, "lunarwing-mt-admin.sh")
            with open(script, "w") as f:
                f.write("#!/bin/sh\n")
            os.chmod(script, 0o700)

            previous = provisioner.MT_ADMIN_SCRIPT
            provisioner.MT_ADMIN_SCRIPT = script
            try:
                config = TenantConfig(name="alpha", enable_darkirc=True)
                args = provisioner.build_add_tenant_args(config)
            finally:
                provisioner.MT_ADMIN_SCRIPT = previous

        self.assertIn("--enable-darkirc", args)

    def test_build_darkirc_uses_tenant_context(self):
        with tempfile.TemporaryDirectory() as tmp:
            script = os.path.join(tmp, "lunarwing-mt-admin.sh")
            with open(script, "w") as f:
                f.write("#!/bin/sh\n")
            os.chmod(script, 0o700)

            previous = provisioner.MT_ADMIN_SCRIPT
            provisioner.MT_ADMIN_SCRIPT = script
            try:
                config = TenantConfig(name="alpha", enable_darkirc=True)
                args = provisioner.build_darkirc_args(config)
            finally:
                provisioner.MT_ADMIN_SCRIPT = previous

        self.assertEqual(args, [script, "build-darkirc", "--tenant", "alpha"])

    def test_lunarwing_model_is_forwarded_to_mt_admin(self):
        with tempfile.TemporaryDirectory() as tmp:
            script = os.path.join(tmp, "lunarwing-mt-admin.sh")
            with open(script, "w") as f:
                f.write("#!/bin/sh\n")
            os.chmod(script, 0o700)

            previous = provisioner.MT_ADMIN_SCRIPT
            provisioner.MT_ADMIN_SCRIPT = script
            try:
                config = TenantConfig(
                    name="alpha",
                    llm_model="tensorzero::function_name::lunarwing",
                )
                args = provisioner.build_add_tenant_args(config)
            finally:
                provisioner.MT_ADMIN_SCRIPT = previous

        self.assertIn("--llm-model", args)
        self.assertIn("tensorzero::function_name::lunarwing", args)

    def test_current_llm_and_worker_overrides_are_forwarded(self):
        with tempfile.TemporaryDirectory() as tmp:
            script = os.path.join(tmp, "lunarwing-mt-admin.sh")
            with open(script, "w") as f:
                f.write("#!/bin/sh\n")
            os.chmod(script, 0o700)
            previous = provisioner.MT_ADMIN_SCRIPT
            provisioner.MT_ADMIN_SCRIPT = script
            try:
                args = provisioner.build_add_tenant_args(
                    TenantConfig(
                        name="alpha",
                        llm_base_url="http://llm.test/v1",
                        nanocode_model="nano-model",
                        nanocode_base_url="http://nano.test/v1",
                        opencode_model="open-model",
                        opencode_base_url="http://open.test/v1",
                    )
                )
            finally:
                provisioner.MT_ADMIN_SCRIPT = previous

        for flag, value in (
            ("--llm-base-url", "http://llm.test/v1"),
            ("--nanocode-model", "nano-model"),
            ("--nanocode-base-url", "http://nano.test/v1"),
            ("--opencode-model", "open-model"),
            ("--opencode-base-url", "http://open.test/v1"),
        ):
            self.assertIn(flag, args)
            self.assertIn(value, args)

    def test_worker_selection_is_forwarded_to_add_tenant(self):
        # start-tenant gates workers on the per-tenant registry flag persisted at
        # add-tenant, so build_add_tenant_args must emit --with-* for selected
        # workers (not only build_build_tenant_args). Unselected workers omitted.
        with tempfile.TemporaryDirectory() as tmp:
            script = os.path.join(tmp, "lunarwing-mt-admin.sh")
            with open(script, "w") as f:
                f.write("#!/bin/sh\n")
            os.chmod(script, 0o700)

            previous = provisioner.MT_ADMIN_SCRIPT
            provisioner.MT_ADMIN_SCRIPT = script
            try:
                config = TenantConfig(
                    name="alpha",
                    workers=[WorkerType.NANOCODE, WorkerType.OPENCODE],
                )
                add_args = provisioner.build_add_tenant_args(config)
                build_args = provisioner.build_build_tenant_args(config)
            finally:
                provisioner.MT_ADMIN_SCRIPT = previous

        # Selected workers appear on add-tenant (the persisted selection).
        self.assertIn("--with-nanocode", add_args)
        self.assertIn("--with-opencode", add_args)
        # Unselected worker is omitted from add-tenant.
        self.assertNotIn("--with-pebble", add_args)
        # build-tenant still gets the same selection (to build the images).
        self.assertIn("--with-nanocode", build_args)
        self.assertIn("--with-opencode", build_args)
        self.assertNotIn("--with-pebble", build_args)

    def test_no_workers_selected_emits_no_worker_flags(self):
        with tempfile.TemporaryDirectory() as tmp:
            script = os.path.join(tmp, "lunarwing-mt-admin.sh")
            with open(script, "w") as f:
                f.write("#!/bin/sh\n")
            os.chmod(script, 0o700)

            previous = provisioner.MT_ADMIN_SCRIPT
            provisioner.MT_ADMIN_SCRIPT = script
            try:
                config = TenantConfig(name="alpha")  # no workers
                add_args = provisioner.build_add_tenant_args(config)
            finally:
                provisioner.MT_ADMIN_SCRIPT = previous

        for flag in ("--with-nanocode", "--with-pebble", "--with-opencode"):
            self.assertNotIn(flag, add_args)

    def test_weechat_bootstrap_opt_out_is_forwarded(self):
        with tempfile.TemporaryDirectory() as tmp:
            script = os.path.join(tmp, "lunarwing-mt-admin.sh")
            with open(script, "w") as f:
                f.write("#!/bin/sh\n")
            os.chmod(script, 0o700)
            previous = provisioner.MT_ADMIN_SCRIPT
            provisioner.MT_ADMIN_SCRIPT = script
            try:
                opted_out = provisioner.build_add_tenant_args(
                    TenantConfig(name="alpha", no_weechat_bootstrap=True)
                )
                default = provisioner.build_add_tenant_args(
                    TenantConfig(name="beta")
                )
            finally:
                provisioner.MT_ADMIN_SCRIPT = previous
        self.assertIn("--no-weechat-bootstrap", opted_out)
        self.assertNotIn("--no-weechat-bootstrap", default)

    def test_xmpp_password_is_written_to_daemon_and_bridge_envs(self):
        config = TenantConfig(name="alpha", xmpp_password="shared-password")
        with patch.object(provisioner.os.path, "isfile", return_value=True), patch.object(
            provisioner, "_write_env_values"
        ) as write_values:
            provisioner._inject_secrets(config)

        self.assertEqual(
            write_values.call_args_list,
            [
                call(
                    "/home/alpha/lunarwing/env/lunarwing.env",
                    {"XMPP_PASSWORD": "shared-password"},
                ),
                call(
                    "/home/alpha/lunarwing/env/xmpp-bridge.env",
                    {"XMPP_PASSWORD": "shared-password"},
                ),
            ],
        )

    def test_skip_build_also_skips_start(self):
        phase = provisioner.PhaseResult(name="add-tenant", returncode=0)
        with patch.object(provisioner, "build_add_tenant_args", return_value=["mt", "add"]), patch.object(
            provisioner, "_run", return_value=phase
        ) as run, patch.object(provisioner, "_inject_secrets"):
            result = provisioner.provision(
                TenantConfig(name="alpha"),
                skip_build=True,
                skip_start=False,
            )

        self.assertTrue(result.ok)
        run.assert_called_once()

    def test_unrelated_subprocess_does_not_inherit_kawarimi_secrets(self):
        with tempfile.TemporaryDirectory() as tmp:
            script = os.path.join(tmp, "check-env.sh")
            with open(script, "w") as script_file:
                script_file.write(
                    "#!/bin/sh\n"
                    "test -z \"${KAWARIMI_PASS:-}\"\n"
                    "test -z \"${KAWARIMI_PASS_FILE:-}\"\n"
                    "test -z \"${KAWARIMI_PASS_FD:-}\"\n"
                )
            os.chmod(script, 0o700)
            with patch.dict(
                os.environ,
                {
                    "KAWARIMI_PASS": "secret",
                    "KAWARIMI_PASS_FILE": "/root/secret",
                    "KAWARIMI_PASS_FD": "9",
                },
            ):
                result = provisioner.run_command([script], phase_name="env-check")

        self.assertTrue(result.ok)


class TestSecretsHelpers(unittest.TestCase):
    def test_generate_master_key_length(self):
        key = generate_master_key()
        self.assertEqual(len(key), 64)

    def test_generate_master_key_is_hex(self):
        key = generate_master_key()
        self.assertTrue(is_valid_master_key(key))

    def test_generate_master_key_randomness(self):
        key1 = generate_master_key()
        key2 = generate_master_key()
        self.assertNotEqual(key1, key2)

    def test_is_valid_master_key_rejects_short(self):
        self.assertFalse(is_valid_master_key("abc"))

    def test_is_valid_master_key_rejects_non_hex(self):
        self.assertFalse(is_valid_master_key("z" * 64))

    def test_mask_secret_long(self):
        self.assertEqual(mask_secret("abcdefghij"), "******ghij")

    def test_mask_secret_short(self):
        self.assertEqual(mask_secret("ab"), "**")

    def test_mask_secret_empty(self):
        self.assertEqual(mask_secret(""), "")


def load_tests(
    loader: unittest.TestLoader,
    standard_tests: unittest.TestSuite,
    pattern: str | None,
) -> unittest.TestSuite:
    """Include focused feature modules in the package test entrypoint."""
    _ = pattern
    from lunarwing_mt_onboard import import_tenant_tests

    standard_tests.addTests(loader.loadTestsFromModule(import_tenant_tests))
    return standard_tests


if __name__ == "__main__":
    unittest.main()
