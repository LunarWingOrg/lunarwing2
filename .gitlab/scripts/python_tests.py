#!/usr/bin/env python3

import contextlib
import importlib.util
import os
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import junit_report as junit


ROOT = pathlib.Path(__file__).resolve().parents[2]
REPORT_PATH = ROOT / "junit-python.xml"
ROOT_SUITES = [
    "lunarwing_mt_onboard.tests",
    "lunarwing_mt_onboard.export_tests",
    "lunarwing_mt_onboard.verify_tests",
    "lunarwing_mt_onboard.secrets_tests",
    "lunarwing_mt_onboard.upgrade_tests",
    "lunarwing_mt_onboard.upgrade_runtime_tests",
    "lunarwing_mt_onboard_web.web_tests",
    "hermes_kawarimi.model_tests",
    "hermes_kawarimi.extract_tests",
    "hermes_kawarimi.mapper_tests",
    "hermes_kawarimi.config_tests",
    "hermes_kawarimi.loader_tests",
    "hermes_kawarimi.loader_populate_tests",
    "hermes_kawarimi.cli_tests",
]


@contextlib.contextmanager
def import_from(path: pathlib.Path):
    previous_cwd = pathlib.Path.cwd()
    sys.path.insert(0, str(path))
    os.chdir(path)
    try:
        yield
    finally:
        os.chdir(previous_cwd)
        sys.path.remove(str(path))


def run_suite(label: str, modules: list[str], records: list[dict]) -> tuple[bool, int, int]:
    suite = unittest.defaultTestLoader.loadTestsFromNames(modules)
    return run_loaded_suite(label, suite, records)


def run_loaded_suite(label: str, suite, records: list[dict]) -> tuple[bool, int, int]:
    count = suite.countTestCases()
    if count == 0:
        print(f"ERROR: {label} loaded zero tests", file=sys.stderr)
        return False, 0, 0

    print(f"\n=== {label}: {count} tests ===")
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    records.extend(
        junit.suite_records(
            label,
            result.failures + result.errors,
            result.skipped,
            count,
        )
    )
    return result.wasSuccessful(), count, len(result.skipped)


def load_file_suite(path: pathlib.Path):
    """WI#363: .gitlab/tests modules are not package-importable (the hidden
    directory name), so the deny.toml guard suite is loaded by path — the
    same importlib pattern the module itself uses for the scripts."""
    spec = importlib.util.spec_from_file_location(path.stem, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return unittest.defaultTestLoader.loadTestsFromModule(module)


def main() -> int:
    sys.path.insert(0, str(ROOT))
    records: list[dict] = []
    ok, total, skipped = run_suite("onboarding, web, and Kawarimi", ROOT_SUITES, records)

    adapter = ROOT / "darkirc_channel_for_lunarwing" / "darkirc" / "adapter"
    with import_from(adapter):
        adapter_ok, adapter_total, adapter_skipped = run_suite(
            "DarkIRC adapter",
            ["test_split", "test_adapter_integration", "test_outbox"],
            records,
        )

    # WI#363: the deny.toml advisory-guard suite is also discovered by the
    # ci-config lane's unittest discover; registering it here keeps it running
    # in python-unit too (the WI's explicit registration requirement).
    guard_ok, guard_total, guard_skipped = run_loaded_suite(
        "deny.toml advisory guards",
        load_file_suite(ROOT / ".gitlab" / "tests" / "test_deny_toml_guards.py"),
        records,
    )

    junit.write(REPORT_PATH, records)
    total += adapter_total + guard_total
    skipped += adapter_skipped + guard_skipped
    if skipped:
        print(f"ERROR: {skipped} Python test(s) skipped; CI prerequisites are incomplete", file=sys.stderr)
        return 1
    if not ok or not adapter_ok or not guard_ok:
        return 1

    print(f"Python test suites passed: {total} tests, no skips.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
