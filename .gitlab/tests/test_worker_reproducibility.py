#!/usr/bin/env python3
"""Worker account dates and package snapshots must be reproducible."""

import datetime
import pathlib
import re
import shlex
import subprocess
import unittest

import yaml


ROOT = pathlib.Path(__file__).resolve().parents[2]
WORKERS = {
    "opencode2.4lunarwing": "opencode2",
    "opencode4lunarwing": "opencode",
    "kimicode4lunarwing": "kimi",
    "pebble4lunarwing": "pebbletask",
}


def instructions(text):
    return [
        line.strip()
        for line in re.sub(r"\\\n\s*", " ", text).splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]


class WorkerReproducibilityTests(unittest.TestCase):
    def test_source_epoch_is_in_scope_before_each_useradd(self):
        for lane in WORKERS:
            with self.subTest(lane=lane):
                epoch = None
                accounts = 0
                for instruction in instructions((ROOT / lane / "Dockerfile").read_text()):
                    if instruction.startswith("FROM "):
                        epoch = None
                    elif instruction.startswith("ARG SOURCE_DATE_EPOCH"):
                        epoch = instruction
                    elif instruction.startswith("RUN ") and "useradd " in instruction:
                        accounts += 1
                        self.assertEqual(epoch, "ARG SOURCE_DATE_EPOCH=0")
                self.assertEqual(accounts, 1)

    def test_shadow_day_assertion_gates_the_account_creation_layer(self):
        for lane, account in WORKERS.items():
            with self.subTest(lane=lane):
                runs = [
                    instruction
                    for instruction in instructions((ROOT / lane / "Dockerfile").read_text())
                    if instruction.startswith("RUN ") and "useradd " in instruction
                ]
                self.assertEqual(len(runs), 1)
                self.assertIn(
                    f'&& shadow_entry="$(getent shadow {account})" '
                    '&& shadow_day="$(printf \'%s\' "$shadow_entry" | cut -d: -f3)" '
                    '&& test "${shadow_day:-0}" = "$((SOURCE_DATE_EPOCH / 86400))"',
                    " ".join(runs[0].split()),
                )

    def shadow_check(self, lane, account, epoch, day):
        text = (ROOT / lane / "Dockerfile").read_text()
        guard = " && ".join(
            line.strip().removeprefix("&& ").removesuffix(" \\")
            for line in text.splitlines()
            if any(token in line for token in ('shadow_entry=', 'shadow_day=', 'test "${shadow_day'))
        )
        shadow = f"{account}:!:{day}:0:99999:7:::" if day is not None else ""
        script = (
            f"getent() {{ printf '%s\\n' {shlex.quote(shadow)}; return {int(day is None)}; }}; "
            + guard
        )
        return subprocess.run(
            ["/bin/sh", "-ec", script],
            env={"PATH": "/usr/bin:/bin", "SOURCE_DATE_EPOCH": str(epoch)},
            capture_output=True,
            text=True,
            check=False,
        )

    def test_shadow_checks_use_the_epoch_day_on_both_sides_of_midnight(self):
        for lane, account in WORKERS.items():
            for epoch in (0, 172799, 172800):
                with self.subTest(lane=lane, epoch=epoch):
                    result = self.shadow_check(lane, account, epoch, epoch // 86400)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout, "")

    def test_shadow_checks_accept_empty_only_in_epoch_day_zero(self):
        for lane, account in WORKERS.items():
            for epoch in (0, 1, 86399, 86400):
                with self.subTest(lane=lane, epoch=epoch):
                    result = self.shadow_check(lane, account, epoch, "")
                    self.assertEqual(result.returncode == 0, epoch < 86400, result.stderr)
                    self.assertEqual(result.stdout, "")

    def test_shadow_checks_refuse_wall_clock_days_and_missing_accounts(self):
        today = (datetime.date.today() - datetime.date(1970, 1, 1)).days
        for lane, account in WORKERS.items():
            for day in (today, 2, None):
                with self.subTest(lane=lane, day=day):
                    result = self.shadow_check(lane, account, 172799, day)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, "")
            self.assertNotEqual(self.shadow_check(lane, account, 0, None).returncode, 0)

    def test_all_worker_snapshots_match_ci_and_include_security_fixes(self):
        defaults = yaml.safe_load((ROOT / ".gitlab/ci/defaults.yml").read_text())
        snapshot = defaults["variables"]["CI_DEBIAN_SNAPSHOT"]
        self.assertRegex(snapshot, r"^\d{8}T\d{6}Z$")
        self.assertGreaterEqual(snapshot, "20261105T082236Z")
        for lane in WORKERS:
            with self.subTest(lane=lane):
                pins = re.findall(
                    r"^ARG DEBIAN_SNAPSHOT=(\S+)$",
                    (ROOT / lane / "Dockerfile").read_text(),
                    flags=re.MULTILINE,
                )
                self.assertEqual(pins, [snapshot])

    def test_worker_apt_uses_https_without_internal_retries(self):
        for lane in WORKERS:
            with self.subTest(lane=lane):
                text = (ROOT / lane / "Dockerfile").read_text()
                sources = re.findall(r'"deb (https?)://snapshot\.debian\.org/', text)
                self.assertEqual(len(sources), 6)
                self.assertEqual(set(sources), {"https"})
                self.assertNotIn('"deb http://', text)
                apt_calls = len(re.findall(r"\bapt-get\b", text))
                self.assertEqual(apt_calls, 6)
                self.assertEqual(text.count("Acquire::Retries=0"), apt_calls)
                self.assertEqual(text.count("Acquire::https::Timeout=60"), apt_calls)
                self.assertNotRegex(text, r"Acquire::Retries=[1-9]")
                self.assertEqual(text.count("--no-install-recommends apt"), 3)
                expected = "3.0.4" if lane.startswith("opencode") else "2.6.2"
                assertion = f'test "$(dpkg-query -W -f=\'${{Version}}\' apt)" = "{expected}";'
                self.assertEqual(text.count(assertion), 2)


if __name__ == "__main__":
    unittest.main()
