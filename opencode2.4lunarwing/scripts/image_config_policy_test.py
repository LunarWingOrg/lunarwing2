#!/usr/bin/env python3
"""Mutation-focused tests for the opencode2 image-config policy (WI#425)."""

from __future__ import annotations

import copy
import pathlib
import unittest

from image_config_policy import EXPECTED_ENV, EXPECTED_FIELDS, config_failures


def clean_config() -> dict:
    env = [f"{key}={value}" for key, value in EXPECTED_ENV.items()]
    env.insert(3, "OPENCODE_IMAGE_REF=v2.1.2@" + "a" * 40)
    return {"config": {"Env": env, **copy.deepcopy(EXPECTED_FIELDS), "Shell": None}}


class ImageConfigPolicyTests(unittest.TestCase):
    def test_clean_config_passes(self):
        self.assertEqual(config_failures(clean_config()), [])

    def test_retargeted_binaries_are_refused(self):
        for key in ("OPENCODE_BIN", "LUNARWING_REAL_OPENCODE"):
            with self.subTest(key=key):
                config = clean_config()
                config["config"]["Env"].append(f"{key}=/tmp/evil")
                self.assertTrue(any("retargets" in failure for failure in config_failures(config)))

    def test_every_required_environment_value_is_pinned(self):
        for key in EXPECTED_ENV:
            with self.subTest(key=key):
                config = clean_config()
                config["config"]["Env"] = [
                    entry.replace(f"{key}=", f"{key}=changed-", 1)
                    if entry.startswith(f"{key}=") else entry
                    for entry in config["config"]["Env"]
                ]
                self.assertTrue(any(f"changed {key}" in failure for failure in config_failures(config)))

    def test_missing_environment_key_is_refused(self):
        config = clean_config()
        config["config"]["Env"].pop(0)
        self.assertTrue(config_failures(config))

    def test_unknown_and_loader_environment_keys_are_refused(self):
        for key in ("LD_PRELOAD", "LD_LIBRARY_PATH", "UNSURPRISING"):
            with self.subTest(key=key):
                config = clean_config()
                config["config"]["Env"].append(f"{key}=anything")
                self.assertTrue(any(f"unapproved key {key}" in failure
                                    for failure in config_failures(config)))

    def test_duplicate_environment_key_is_refused(self):
        config = clean_config()
        config["config"]["Env"].append("OPENCODE_DISABLE_AUTOUPDATE=1")
        self.assertTrue(any("duplicates" in failure for failure in config_failures(config)))

    def test_malformed_image_ref_is_refused(self):
        config = clean_config()
        config["config"]["Env"] = [
            "OPENCODE_IMAGE_REF=unreviewed" if entry.startswith("OPENCODE_IMAGE_REF=") else entry
            for entry in config["config"]["Env"]
        ]
        self.assertTrue(any("OPENCODE_IMAGE_REF" in failure for failure in config_failures(config)))

    def test_runtime_fields_are_pinned(self):
        for key in EXPECTED_FIELDS:
            with self.subTest(key=key):
                config = clean_config()
                config["config"][key] = "0:0" if key == "User" else ["bad"]
                self.assertTrue(any(key in failure for failure in config_failures(config)))

    def test_shell_override_is_refused(self):
        config = clean_config()
        config["config"]["Shell"] = ["/bin/false"]
        self.assertTrue(any("Shell override" in failure for failure in config_failures(config)))

    def test_docker_inspect_shape_is_accepted(self):
        config = clean_config()["config"]
        self.assertEqual(config_failures(config), [])


if __name__ == "__main__":
    unittest.main()
