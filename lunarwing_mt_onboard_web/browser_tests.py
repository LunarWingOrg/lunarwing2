"""Browser end-to-end tests for the MT onboarding web UI.

These tests are intentionally separate from ``web_tests`` because they require
Playwright and a downloaded Chromium binary. See README.md for setup and run
commands.
"""

from __future__ import annotations

import os
import re
import socket
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.error
import urllib.request
from pathlib import Path

PACKAGE_DIR = Path(__file__).resolve().parent
REPO_ROOT = PACKAGE_DIR.parent
os.environ.setdefault(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(PACKAGE_DIR / ".venv" / "playwright-browsers"),
)

try:
    from playwright.sync_api import Browser, Page, sync_playwright
except ModuleNotFoundError:
    Browser = Page = None  # type: ignore[assignment,misc]
    sync_playwright = None


def _unused_loopback_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def _wait_for_server(url: str, process: subprocess.Popen[str]) -> None:
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"demo server exited with status {process.returncode}")
        try:
            with urllib.request.urlopen(url, timeout=0.5) as response:
                if response.status == 200:
                    return
        except (OSError, urllib.error.URLError):
            time.sleep(0.05)
    raise RuntimeError(f"demo server did not become ready at {url}")


@unittest.skipIf(sync_playwright is None, "install requirements-browser-tests.txt")
class OnboardBrowserTests(unittest.TestCase):
    browser: Browser

    @classmethod
    def setUpClass(cls) -> None:
        cls._logs = tempfile.TemporaryDirectory(prefix="lw-onboard-browser-")
        cls._server_output = tempfile.TemporaryFile(mode="w+", encoding="utf-8")
        cls.port = _unused_loopback_port()
        cls.base_url = f"http://127.0.0.1:{cls.port}"
        env = os.environ.copy()
        env["LUNARWING_DEMO_SLEEP"] = "0.01"
        cls._server = subprocess.Popen(
            [
                sys.executable,
                "-m",
                "lunarwing_mt_onboard_web",
                "--demo",
                "--no-token",
                "--port",
                str(cls.port),
                "--log-dir",
                cls._logs.name,
            ],
            cwd=REPO_ROOT,
            env=env,
            stdout=cls._server_output,
            stderr=subprocess.STDOUT,
            text=True,
        )
        try:
            _wait_for_server(f"{cls.base_url}/api/config", cls._server)
            cls._playwright = sync_playwright().start()
            cls.browser = cls._playwright.chromium.launch(headless=True)
        except Exception:
            cls._stop_server()
            raise

    @classmethod
    def tearDownClass(cls) -> None:
        if hasattr(cls, "browser"):
            cls.browser.close()
        if hasattr(cls, "_playwright"):
            cls._playwright.stop()
        cls._stop_server()

    @classmethod
    def _stop_server(cls) -> None:
        if hasattr(cls, "_server") and cls._server.poll() is None:
            cls._server.terminate()
            try:
                cls._server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                cls._server.kill()
                cls._server.wait(timeout=5)
        if hasattr(cls, "_server_output"):
            cls._server_output.close()
        if hasattr(cls, "_logs"):
            cls._logs.cleanup()

    def setUp(self) -> None:
        self.console_errors: list[str] = []
        self.request_failures: list[str] = []
        self.page = self.browser.new_page(viewport={"width": 1440, "height": 1000})
        self.page.on(
            "console",
            lambda message: self.console_errors.append(message.text)
            if message.type == "error"
            else None,
        )
        self.page.on("pageerror", lambda error: self.console_errors.append(str(error)))
        self.page.on(
            "requestfailed",
            lambda request: self.request_failures.append(
                f"{request.method} {request.url}: {request.failure}"
            ),
        )
        self.page.add_init_script(
            "localStorage.setItem('lw-onboard-splash', 'false')"
        )
        response = self.page.goto(self.base_url, wait_until="networkidle")
        self.assertIsNotNone(response)
        self.assertTrue(response.ok if response is not None else False)
        self.page.locator("#splash-screen").wait_for(state="detached")
        self.page.locator("#mode-badge:not(.hidden)").wait_for()

    def tearDown(self) -> None:
        self.page.close()
        self.assertEqual(self.console_errors, [])
        self.assertEqual(self.request_failures, [])

    def _field(self, label: str):
        label_node = self.page.locator(".field > label").filter(
            has_text=re.compile(f"^{re.escape(label)}")
        )
        self.assertEqual(label_node.count(), 1, f"field not unique: {label}")
        return label_node.locator("..").locator("input, select").first

    def _checkbox(self, label: str):
        row = self.page.locator(".check-row").filter(has_text=label)
        self.assertEqual(row.count(), 1, f"checkbox not unique: {label}")
        return row.locator('input[type="checkbox"]')

    def _open_mode(self, mode: str) -> None:
        self.page.locator(f'.mode-card[data-mode="{mode}"]').click()
        self.page.locator("#view-wizard:not(.hidden)").wait_for()

    def _back_to_picker(self) -> None:
        self.page.locator(".back-link").click()
        self.page.locator("#view-picker:not(.hidden)").wait_for()

    def _start_another(self) -> None:
        self.page.get_by_role("button", name="Start another").click()
        self.page.locator("#view-picker:not(.hidden)").wait_for()

    def _wait_for_result(self, *, success: bool = True) -> str:
        verdict = ".verdict-ok" if success else ".verdict-fail"
        self.page.locator(verdict).wait_for(timeout=15_000)
        result = self.page.locator("#result-summary").inner_text()
        self.assertIn("Success" if success else "Failed", result)
        return result

    def _advance_provision(self, tenant: str) -> None:
        self._open_mode("provision")
        self._field("Tenant name").fill(tenant)
        for _ in range(5):
            self.page.get_by_role("button", name="Next").click()
        self.page.get_by_role("button", name="Provision").click()

    def test_picker_assets_settings_and_no_token_mode(self) -> None:
        self.assertEqual(self.page.locator("#mode-badge").inner_text(), "DEMO")
        self.assertEqual(self.page.locator(".mode-card").count(), 5)
        self.assertTrue(
            self.page.locator("#token-error").evaluate(
                "element => element.classList.contains('hidden')"
            )
        )
        self.assertGreater(
            self.page.locator(".logo").evaluate("element => element.naturalWidth"),
            0,
        )
        self.page.locator("#settings-btn").click()
        # Mascot defaults to hidden; enabling it reveals Lunar's dock.
        self.assertTrue(
            self.page.locator("#bat-host").evaluate(
                "element => element.classList.contains('hidden')"
            )
        )
        self.page.locator("#set-mascot").check()
        self.assertFalse(
            self.page.locator("#bat-host").evaluate(
                "element => element.classList.contains('hidden')"
            )
        )

    def test_client_side_validation_for_all_operations(self) -> None:
        self._open_mode("provision")
        self._field("Tenant name").fill("Bad Name")
        self.page.get_by_role("button", name="Next").click()
        self.assertIn("lowercase alphanumeric", self.page.locator(".field-error").inner_text())
        self._back_to_picker()

        self._open_mode("secrets")
        self._field("Tenant").fill("sphinx")
        self._field("Secret name").fill("gotify_app_token")
        self._field("Secret value").fill("first")
        self._field("Confirm value").fill("second")
        self.page.get_by_role("button", name="Store secret").click()
        self.assertIn("values do not match", self.page.locator(".field-error").inner_text())
        self._back_to_picker()

        self._open_mode("upgrade")
        self._field("Tenant").fill("sphinx")
        self._field("Target branch").fill("v2.1.0.0")
        self.page.get_by_role("button", name="Run upgrade").click()
        self.assertIn("confirm the upgrade", self.page.locator(".field-error").inner_text())
        self._back_to_picker()

        self._open_mode("export")
        self._field("Tenant").fill("sphinx")
        self._checkbox("Apply").check()
        self._field("Encryption passphrase").fill("short")
        self.page.get_by_role("button", name="Run export").click()
        self.assertIn("at least 12", self.page.locator(".field-error").inner_text())
        self._back_to_picker()

        self._open_mode("import")
        self._field("Migration bundle path").fill("/tmp/sphinx.tar")
        self._checkbox("Start restored tenant").check()
        self.page.get_by_role("button", name="Run import").click()
        self.assertIn("old host is stopped", self.page.locator(".field-error").inner_text())

    def test_provision_success_and_failure_stream_to_completion(self) -> None:
        self._advance_provision("sphinx")
        self._wait_for_result()
        self.assertTrue(
            self.page.locator('[data-cred="master_key"] code').inner_text()
        )
        self.assertTrue(
            self.page.locator('[data-cred="gateway_auth_token"] code').inner_text()
        )
        self.assertIn("PASS", self.page.locator("#result-summary").inner_text())

        self._start_another()
        self._advance_provision("failme")
        result = self._wait_for_result(success=False)
        self.assertIn("FAIL", result)

    def test_simple_demo_operations_stream_to_completion(self) -> None:
        self._open_mode("secrets")
        self._field("Tenant").fill("sphinx")
        self._field("Secret name").fill("gotify_app_token")
        self._field("Secret value").fill("browser-test-secret")
        self._field("Confirm value").fill("browser-test-secret")
        self.page.get_by_role("button", name="Store secret").click()
        self._wait_for_result()
        self.assertIn(
            "secret stored: gotify_app_token",
            self.page.locator("#log-console").inner_text(),
        )
        self._start_another()

        self._open_mode("upgrade")
        self._field("Tenant").fill("sphinx")
        self._field("Target branch").fill("v2.1.0.0")
        self._checkbox("I understand").check()
        self.page.get_by_role("button", name="Run upgrade").click()
        self._wait_for_result()
        self._start_another()

        self._open_mode("export")
        self._field("Tenant").fill("sphinx")
        self.page.get_by_role("button", name="Run export").click()
        self._wait_for_result()
        self._start_another()

        self._open_mode("import")
        self._field("Migration bundle path").fill("/tmp/chimera.tar")
        self.page.get_by_role("button", name="Run import").click()
        self._wait_for_result()


if __name__ == "__main__":
    unittest.main()
