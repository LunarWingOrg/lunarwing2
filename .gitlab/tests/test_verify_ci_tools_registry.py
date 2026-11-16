#!/usr/bin/env python3
"""WI#598: the registry verifier must honour pins, absence, and the push rule."""

import hashlib
import http.server
import json
import pathlib
import subprocess
import sys
import tempfile
import threading
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".gitlab/scripts/verify-ci-tools-registry.py"
SCCACHE = b"sccache archive fixture"
SHELLCHECK = b"shellcheck archive fixture"
SCCACHE_ARCHIVE = "sccache-v9.9.9-x86_64-unknown-linux-musl.tar.gz"
SHELLCHECK_ARCHIVE = "shellcheck-v9.9.9.linux.x86_64.tar.xz"
PRESENT = {
    f"/api/v4/projects/35/packages/generic/lunarwing-ci-tools/9.9.9/{SCCACHE_ARCHIVE}": SCCACHE,
    f"/api/v4/projects/35/packages/generic/lunarwing-ci-tools/9.9.9/{SHELLCHECK_ARCHIVE}": SHELLCHECK,
}


def digest(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


class RegistryFixture(http.server.ThreadingHTTPServer):
    """Serve the two archives plus the rule and user endpoints from memory."""

    def __init__(self, routes: dict[str, bytes]) -> None:
        self.routes = routes
        super().__init__(("127.0.0.1", 0), RegistryHandler)

    @property
    def api_url(self) -> str:
        return f"http://127.0.0.1:{self.server_address[1]}/api/v4"


class RegistryHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        body = self.server.routes.get(self.path)
        if body is None:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args: object) -> None:
        """Keep discovery output clean."""


class VerifyRegistryTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        scripts = self.root / ".gitlab/scripts"
        scripts.mkdir(parents=True)
        (scripts / "bootstrap-sccache.sh").write_text(
            'SCCACHE_VERSION="9.9.9"\n'
            f'SCCACHE_SHA256="{digest(SCCACHE)}"\n'
            f'SCCACHE_ARCHIVE="{SCCACHE_ARCHIVE}"\n',
            encoding="utf-8",
        )
        (scripts / "install-shellcheck.sh").write_text(
            'readonly version="9.9.9"\n'
            f'readonly archive="{SHELLCHECK_ARCHIVE}"\n'
            f'readonly checksum="{digest(SHELLCHECK)}"\n',
            encoding="utf-8",
        )
        self.token = self.root / "header"
        self.token.write_text("PRIVATE-TOKEN: test-token\n", encoding="utf-8")

    def serve(self, routes: dict[str, bytes]) -> RegistryFixture:
        server = RegistryFixture(routes)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        return server

    def run_verifier(self, server: RegistryFixture) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--api-url",
                server.api_url,
                "--repo-root",
                str(self.root),
                "--token-file",
                str(self.token),
            ],
            capture_output=True,
            text=True,
            check=False,
        )

    def rule(self, level: str | None) -> bytes:
        return json.dumps(
            [
                {
                    "id": 7,
                    "package_name_pattern": "lunarwing-ci-tools",
                    "package_type": "generic",
                    "minimum_access_level_for_push": level,
                }
            ]
        ).encode()

    def routes(self, level: str | None = "maintainer", admin: bool = False) -> dict[str, bytes]:
        return {
            **PRESENT,
            "/api/v4/projects/35/packages/protection/rules": self.rule(level),
            "/api/v4/user": json.dumps({"is_admin": admin}).encode(),
        }

    def test_published_archives_and_rule_pass(self) -> None:
        result = self.run_verifier(self.serve(self.routes()))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("sccache 9.9.9: ok", result.stdout)
        self.assertIn("shellcheck 9.9.9: ok", result.stdout)
        self.assertIn("push level maintainer", result.stdout)
        self.assertNotIn("WARNING", result.stdout)

    def test_absent_archive_is_reported(self) -> None:
        routes = {
            key: value for key, value in self.routes().items() if SHELLCHECK_ARCHIVE not in key
        }
        result = self.run_verifier(self.serve(routes))
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("shellcheck 9.9.9: ABSENT", result.stdout)

    def test_corrupt_archive_fails_the_pin(self) -> None:
        routes = {
            **self.routes(),
            f"/api/v4/projects/35/packages/generic/lunarwing-ci-tools/9.9.9/{SCCACHE_ARCHIVE}": b"tampered",
        }
        result = self.run_verifier(self.serve(routes))
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("sccache 9.9.9: MISMATCH", result.stdout)

    def test_weak_rule_and_admin_token_are_flagged(self) -> None:
        result = self.run_verifier(self.serve(self.routes(level="developer", admin=True)))
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertIn("weaker than Maintainer", result.stdout)
        self.assertIn("admins bypass package protection levels", result.stdout)


if __name__ == "__main__":
    unittest.main()
