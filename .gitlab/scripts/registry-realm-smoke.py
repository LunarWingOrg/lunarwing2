#!/usr/bin/env python3
"""Pre-flight the worker-promotion registry auth realm.

Runs at the start of worker promotion jobs so a broken or misconfigured
registry auth realm fails in seconds with a clear message, instead of
surfacing as a cryptic 'realm is not a trusted HTTPS origin' error only
after the multi-hour candidate and scan jobs have already run.
"""

import argparse
import os
import sys
import unittest
import urllib.error
import urllib.request
from unittest import mock

TIMEOUT_SECONDS = 15


def check_realm(registry: str) -> None:
    """GET the registry /v2/ endpoint and require a realm-bearing auth challenge.

    Raises ValueError with a diagnostic message when the realm cannot be
    confirmed, so the caller can fail the job before any manifest is touched.
    """
    url = f"https://{registry}/v2/"
    try:
        urllib.request.urlopen(urllib.request.Request(url), timeout=TIMEOUT_SECONDS)
        # A 200 means the registry demands no auth challenge; nothing to verify.
        return
    except urllib.error.HTTPError as exc:
        challenge = exc.headers.get("WWW-Authenticate", "")
        if "realm=" not in challenge:
            raise ValueError(f"registry auth challenge missing realm: {challenge!r}")
    except urllib.error.URLError as exc:
        raise ValueError(f"registry realm pre-flight failed: {exc}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(RealmSmokeTests)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        return 0 if result.wasSuccessful() else 1

    registry = os.environ.get("CI_REGISTRY")
    if not registry:
        print("ERROR: CI_REGISTRY is not set", file=sys.stderr)
        return 1

    try:
        check_realm(registry)
    except ValueError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    print("OK: registry auth realm pre-flight passed.")
    return 0


class RealmSmokeTests(unittest.TestCase):
    @staticmethod
    def _http_error(code: int, headers: dict[str, str]) -> urllib.error.HTTPError:
        return urllib.error.HTTPError("https://registry/v2/", code, "error", headers, None)

    def test_anonymous_registry_passes(self) -> None:
        with mock.patch("urllib.request.urlopen", return_value=object()):
            check_realm("registry")

    def test_realm_challenge_passes(self) -> None:
        error = self._http_error(
            401, {"WWW-Authenticate": 'Bearer realm="https://git.example.com/jwt/auth"'}
        )
        with mock.patch("urllib.request.urlopen", side_effect=error):
            check_realm("registry")

    def test_challenge_without_realm_fails(self) -> None:
        error = self._http_error(401, {"WWW-Authenticate": "Basic"})
        with mock.patch("urllib.request.urlopen", side_effect=error):
            with self.assertRaises(ValueError):
                check_realm("registry")

    def test_connection_failure_fails_cleanly(self) -> None:
        with mock.patch(
            "urllib.request.urlopen", side_effect=urllib.error.URLError("timed out")
        ):
            with self.assertRaises(ValueError):
                check_realm("registry")


if __name__ == "__main__":
    sys.exit(main())
