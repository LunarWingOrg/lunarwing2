#!/usr/bin/env python3
"""WI#598: the registry verifier must honour pins, absence, and the push rule."""

import hashlib
import http.server
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import threading
import unittest
import urllib.parse


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".gitlab/scripts/verify-ci-tools-registry.py"
SCCACHE = b"sccache archive fixture"
SHELLCHECK = b"shellcheck archive fixture"
SCCACHE_ARCHIVE = "sccache-v9.9.9-x86_64-unknown-linux-musl.tar.gz"
SHELLCHECK_ARCHIVE = "shellcheck-v9.9.9.linux.x86_64.tar.xz"
SCCACHE_PATH = f"/api/v4/projects/35/packages/generic/lunarwing-ci-tools/9.9.9/{SCCACHE_ARCHIVE}"
PRESENT = {
    SCCACHE_PATH: SCCACHE,
    f"/api/v4/projects/35/packages/generic/lunarwing-ci-tools/9.9.9/{SHELLCHECK_ARCHIVE}": SHELLCHECK,
}
RULES_PATH = "/api/v4/projects/35/packages/protection/rules"
USER_PATH = "/api/v4/user"
TOKEN = "test-token"


def digest(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


class RegistryFixture(http.server.ThreadingHTTPServer):
    """Serve the two archives to anyone, the rule and user endpoints to the token."""

    def __init__(self, routes: dict[str, bytes]) -> None:
        self.routes = routes
        self.statuses: dict[str, int] = {}
        self.redirects: dict[str, str] = {}
        self.seen: list[tuple[str, str | None]] = []
        super().__init__(("127.0.0.1", 0), RegistryHandler)

    @property
    def origin(self) -> str:
        return f"http://127.0.0.1:{self.server_address[1]}"

    @property
    def api_url(self) -> str:
        return f"{self.origin}/api/v4"

    def paths_given_the_token(self) -> set[str]:
        return {path for path, token in self.seen if token is not None}


class RegistryHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        # A client that takes this server for its proxy sends the whole URL.
        path = urllib.parse.urlsplit(self.path).path
        token = self.headers.get("PRIVATE-TOKEN")
        self.server.seen.append((path, token))
        if path in self.server.redirects:
            self.send_response(302)
            self.send_header("Location", self.server.redirects[path])
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if path in self.server.statuses:
            self.send_error(self.server.statuses[path])
            return
        # GitLab answers both API endpoints with 401 when no valid token comes.
        if path in (RULES_PATH, USER_PATH) and token != TOKEN:
            self.send_error(401)
            return
        body = self.server.routes.get(path)
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
        self.scripts = self.root / ".gitlab/scripts"
        self.scripts.mkdir(parents=True)
        (self.scripts / "bootstrap-sccache.sh").write_text(
            'SCCACHE_VERSION="9.9.9"\n'
            f'SCCACHE_SHA256="{digest(SCCACHE)}"\n'
            f'SCCACHE_ARCHIVE="{SCCACHE_ARCHIVE}"\n',
            encoding="utf-8",
        )
        (self.scripts / "install-shellcheck.sh").write_text(
            'readonly version="9.9.9"\n'
            f'readonly archive="{SHELLCHECK_ARCHIVE}"\n'
            f'readonly checksum="{digest(SHELLCHECK)}"\n',
            encoding="utf-8",
        )
        self.token = self.root / "header"
        self.token.write_text(f"PRIVATE-TOKEN: {TOKEN}\n", encoding="utf-8")

    def serve(self, routes: dict[str, bytes]) -> RegistryFixture:
        server = RegistryFixture(routes)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        return server

    def run_verifier(
        self, server: RegistryFixture, api_url: str | None = None, proxy: str | None = None
    ) -> subprocess.CompletedProcess[str]:
        # Only the proxy a test names: the runner's own settings must not route these.
        env = {
            key: value for key, value in os.environ.items() if not key.lower().endswith("_proxy")
        }
        if proxy is not None:
            env["http_proxy"] = proxy
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--api-url",
                server.api_url if api_url is None else api_url,
                "--repo-root",
                str(self.root),
                "--token-file",
                str(self.token),
            ],
            capture_output=True,
            text=True,
            check=False,
            env=env,
        )

    def rule(
        self,
        level: str | None = "maintainer",
        pattern: str = "lunarwing-ci-tools",
        package_type: str = "generic",
    ) -> dict[str, object]:
        return {
            "id": 7,
            "package_name_pattern": pattern,
            "package_type": package_type,
            "minimum_access_level_for_push": level,
        }

    def routes(
        self, rules: list[dict[str, object]] | None = None, admin: bool = False
    ) -> dict[str, bytes]:
        return {
            **PRESENT,
            RULES_PATH: json.dumps([self.rule()] if rules is None else rules).encode(),
            USER_PATH: json.dumps({"is_admin": admin}).encode(),
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
        routes = {**self.routes(), SCCACHE_PATH: b"tampered"}
        result = self.run_verifier(self.serve(routes))
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("sccache 9.9.9: MISMATCH", result.stdout)

    def test_missing_installer_fails(self) -> None:
        (self.scripts / "install-shellcheck.sh").unlink()
        result = self.run_verifier(self.serve(self.routes()))
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("cannot read pins", result.stderr)

    def test_installer_without_a_pin_fails(self) -> None:
        (self.scripts / "bootstrap-sccache.sh").write_text(
            f'SCCACHE_VERSION="9.9.9"\nSCCACHE_ARCHIVE="{SCCACHE_ARCHIVE}"\n', encoding="utf-8"
        )
        result = self.run_verifier(self.serve(self.routes()))
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("no SCCACHE_SHA256 assignment", result.stderr)

    def test_weak_rule_and_admin_token_are_flagged(self) -> None:
        routes = self.routes(rules=[self.rule(level="developer")], admin=True)
        result = self.run_verifier(self.serve(routes))
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertIn("weaker than Maintainer", result.stdout)
        self.assertIn("admins bypass package protection levels", result.stdout)

    def test_rule_for_another_package_type_does_not_count(self) -> None:
        routes = self.routes(rules=[self.rule(package_type="npm")])
        result = self.run_verifier(self.serve(routes))
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertIn("no generic rule covers lunarwing-ci-tools", result.stdout)

    def test_rule_for_another_package_name_does_not_count(self) -> None:
        routes = self.routes(rules=[self.rule(pattern="lunarwing-ci-tools-*")])
        result = self.run_verifier(self.serve(routes))
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertIn("no generic rule covers lunarwing-ci-tools", result.stdout)

    def test_no_rule_does_not_count(self) -> None:
        result = self.run_verifier(self.serve(self.routes(rules=[])))
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertIn("no generic rule covers lunarwing-ci-tools", result.stdout)

    def test_failed_rule_request_does_not_count(self) -> None:
        # What a Developer's token gets: the archives are fine, the rules are not shown.
        server = self.serve(self.routes())
        server.statuses[RULES_PATH] = 403
        result = self.run_verifier(server)
        self.assertEqual(result.returncode, 3, result.stdout)
        self.assertIn("protection check failed: HTTP Error 403", result.stderr)
        self.assertIn("shellcheck 9.9.9: ok", result.stdout)

    def test_token_is_not_sent_to_a_redirect_target(self) -> None:
        for path in (USER_PATH, RULES_PATH):
            with self.subTest(path=path):
                # The target would answer the token with a passing state.
                target = self.serve(self.routes())
                server = self.serve(self.routes())
                server.redirects[path] = f"{target.origin}{path}"
                result = self.run_verifier(server)
                self.assertEqual(target.paths_given_the_token(), set())
                self.assertIn((path, TOKEN), server.seen)
                self.assertEqual(result.returncode, 3, result.stdout)
                self.assertIn("refusing the redirect", result.stderr)

    def test_token_is_not_sent_over_http_to_another_host(self) -> None:
        # No request may leave for registry.invalid, so the fixture is the
        # proxy for it and sees whatever the verifier would have sent there.
        server = self.serve(self.routes())
        result = self.run_verifier(
            server, api_url="http://registry.invalid/api/v4", proxy=server.origin
        )
        self.assertEqual(server.paths_given_the_token(), set())
        self.assertIn("sccache 9.9.9: ok", result.stdout)
        self.assertEqual(result.returncode, 3, result.stdout)
        self.assertIn("refusing to send the token to http://registry.invalid", result.stderr)

    def test_token_is_not_sent_to_a_proxy_for_loopback_http(self) -> None:
        proxy = self.serve(self.routes())
        server = self.serve(self.routes())
        result = self.run_verifier(server, proxy=proxy.origin)
        self.assertEqual(proxy.paths_given_the_token(), set())
        self.assertEqual(server.paths_given_the_token(), {USER_PATH, RULES_PATH})
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_archive_downloads_follow_redirects_without_the_token(self) -> None:
        # GitLab redirects a package download to object storage.
        storage = self.serve(dict(PRESENT))
        server = self.serve(self.routes())
        server.redirects[SCCACHE_PATH] = f"{storage.origin}{SCCACHE_PATH}"
        result = self.run_verifier(server)
        self.assertEqual(storage.paths_given_the_token(), set())
        self.assertEqual(server.paths_given_the_token(), {USER_PATH, RULES_PATH})
        self.assertIn((SCCACHE_PATH, None), storage.seen)
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
