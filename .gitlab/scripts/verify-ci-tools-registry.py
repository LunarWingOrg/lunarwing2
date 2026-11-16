#!/usr/bin/env python3
"""Verify the publication contract for the pinned CI tool archives.

The pins are read from the installers that consume them, so this never drifts
from what CI checks. Each archive is downloaded anonymously from the project's
generic package registry, the way a job downloads it, and its SHA256 must equal
the pin. With --token-file the package protection rule is checked too: it is
what keeps non-Maintainers from publishing to `lunarwing-ci-tools`. The token
goes to the configured API host only: over https (plain http is accepted for a
loopback address), and never to a redirect target.

Exit codes: 0 ok, 1 checksum mismatch or unreadable pin, 2 archive absent (404)
or registry unreachable, 3 protection rule missing, weaker than Maintainer or
not checkable, 4 any other HTTP status on a download. The highest one wins.
"""

import argparse
import hashlib
import ipaddress
import json
import os
import pathlib
import re
import sys
import urllib.error
import urllib.parse
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[2]
PACKAGE = "lunarwing-ci-tools"
OK = 0
MISMATCH = 1
MISSING = 2
UNPROTECTED = 3
REGISTRY_ERROR = 4
TIMEOUT = 120
PUSH_LEVELS = ("maintainer", "owner", "admin")
RULE_PAGES = 50
TOOLS = (
    {
        "name": "sccache",
        "script": ".gitlab/scripts/bootstrap-sccache.sh",
        "version": "SCCACHE_VERSION",
        "sha256": "SCCACHE_SHA256",
        "archive": "SCCACHE_ARCHIVE",
    },
    {
        "name": "shellcheck",
        "script": ".gitlab/scripts/install-shellcheck.sh",
        "version": "version",
        "sha256": "checksum",
        "archive": "archive",
    },
)


def assignment(text: str, name: str) -> str:
    match = re.search(
        rf'^(?:readonly\s+|local\s+)?{re.escape(name)}="([^"]*)"', text, re.MULTILINE
    )
    if match is None:
        raise ValueError(f"no {name} assignment")
    return match.group(1)


def expand(template: str, values: dict[str, str]) -> str:
    def substitute(match: re.Match[str]) -> str:
        key = match.group(1)
        if key not in values:
            raise ValueError(f"unknown placeholder ${{{key}}}")
        return values[key]

    return re.sub(r"\$\{(\w+)\}", substitute, template)


def load_pins(root: pathlib.Path) -> list[dict[str, str]]:
    pins = []
    for tool in TOOLS:
        text = (root / tool["script"]).read_text(encoding="utf-8")
        version = assignment(text, tool["version"])
        pins.append(
            {
                "name": tool["name"],
                "version": version,
                "sha256": assignment(text, tool["sha256"]),
                "archive": expand(
                    assignment(text, tool["archive"]),
                    {"version": version, "SCCACHE_VERSION": version},
                ),
            }
        )
    return pins


def fetch_sha256(url: str) -> str:
    request = urllib.request.Request(
        url, headers={"User-Agent": "verify-ci-tools-registry"}
    )
    with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
        digest = hashlib.sha256()
        while chunk := response.read(1 << 20):
            digest.update(chunk)
    return digest.hexdigest()


def read_token(path: pathlib.Path) -> str:
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        header, separator, value = line.partition(":")
        if separator and header.strip().upper() == "PRIVATE-TOKEN":
            return value.strip()
        return line
    raise ValueError(f"{path} holds no token")


class RefuseRedirect(urllib.request.HTTPRedirectHandler):
    """urllib would repeat the token header to the redirect target, on any host."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise urllib.error.HTTPError(
            req.full_url, code, f"refusing the redirect to {newurl}", headers, fp
        )


def token_opener(url: str) -> urllib.request.OpenerDirector:
    parts = urllib.parse.urlsplit(url)
    if parts.scheme == "https":
        return urllib.request.build_opener(RefuseRedirect)
    # A name such as localhost is whatever the resolver says, so only a
    # literal loopback address may take the token unencrypted.
    try:
        loopback = ipaddress.ip_address(parts.hostname or "").is_loopback
    except ValueError:
        loopback = False
    if parts.scheme != "http" or not loopback:
        raise ValueError(
            f"refusing to send the token to {parts.scheme}://{parts.hostname}: "
            "use https (plain http only for a loopback address such as 127.0.0.1)"
        )
    # Unencrypted, so no proxy from the environment may relay it either.
    return urllib.request.build_opener(RefuseRedirect, urllib.request.ProxyHandler({}))


def get_json(url: str, token: str) -> tuple[object, str]:
    request = urllib.request.Request(url, headers={"PRIVATE-TOKEN": token})
    with token_opener(url).open(request, timeout=TIMEOUT) as response:
        return json.load(response), (response.headers.get("X-Next-Page") or "").strip()


def list_rules(api_url: str, project_id: str, token: str) -> list[object]:
    rules: list[object] = []
    page = "1"
    # GitLab names the next page in X-Next-Page; an unpaged list sends none.
    for _ in range(RULE_PAGES):
        payload, page = get_json(
            f"{api_url}/projects/{project_id}/packages/protection/rules"
            f"?per_page=100&page={page}",
            token,
        )
        if not isinstance(payload, list):
            raise ValueError("the rules answer is not a list")
        rules.extend(payload)
        if not page:
            return rules
        if not page.isdecimal():
            raise ValueError(f"unexpected X-Next-Page {page!r}")
    raise ValueError(f"more than {RULE_PAGES} pages of rules")


def covers_package(pattern: object) -> bool:
    # GitLab turns only `*` into a wildcard (Gitlab::SQL::Glob), so `?` and `[`
    # are literal there. It also ignores case; staying exact errs to exit 3.
    if not isinstance(pattern, str):
        return False
    expression = ".*".join(re.escape(part) for part in pattern.split("*"))
    return re.fullmatch(expression, PACKAGE) is not None


def check_protection(api_url: str, project_id: str, token: str) -> int:
    user, _ = get_json(f"{api_url}/user", token)
    if isinstance(user, dict) and user.get("is_admin"):
        # GitLab admits admins everywhere, so no rule level can constrain them.
        print(
            "WARNING: this token belongs to an instance admin; admins bypass package "
            "protection levels, so this rule cannot stop that account publishing "
            "(WI#598)."
        )
    matching = []
    for rule in list_rules(api_url, project_id, token):
        if not isinstance(rule, dict) or rule.get("package_type") != "generic":
            continue
        if covers_package(rule.get("package_name_pattern")):
            matching.append(rule)
    # GitLab refuses a push when any matching rule asks for more than the
    # pusher's level (Packages::Protection::Rule.for_action_exists?), so one
    # Maintainer rule protects the package whatever the other rules say.
    strong = [
        rule for rule in matching if rule.get("minimum_access_level_for_push") in PUSH_LEVELS
    ]
    for rule in strong or matching:
        level = rule.get("minimum_access_level_for_push")
        found = f"protection: rule {rule.get('id')} matches {rule.get('package_name_pattern')}"
        if strong:
            print(f"{found} with push level {level}")
        else:
            print(f"{found} but push level {level!r} is weaker than Maintainer")
    if not matching:
        print(f"protection: no generic rule covers {PACKAGE}")
    return OK if strong else UNPROTECTED


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--api-url",
        default=os.environ.get("CI_API_V4_URL", "https://git.lunarwing.org/api/v4"),
    )
    parser.add_argument(
        "--project-id", default=os.environ.get("CI_PROJECT_ID", "35")
    )
    parser.add_argument("--repo-root", type=pathlib.Path, default=ROOT)
    parser.add_argument(
        "--token-file",
        type=pathlib.Path,
        help="raw token or a PRIVATE-TOKEN header line; enables the rule check",
    )
    args = parser.parse_args()
    api_url = args.api_url.rstrip("/")

    try:
        pins = load_pins(args.repo_root)
    except (OSError, ValueError) as error:
        print(f"ERROR: cannot read pins: {error}", file=sys.stderr)
        return MISMATCH

    status = OK
    for pin in pins:
        url = (
            f"{api_url}/projects/{args.project_id}/packages/generic/{PACKAGE}"
            f"/{pin['version']}/{pin['archive']}"
        )
        try:
            actual = fetch_sha256(url)
        except urllib.error.HTTPError as error:
            if error.code == 404:
                print(f"{pin['name']} {pin['version']}: ABSENT (HTTP 404) {url}")
                status = max(status, MISSING)
            else:
                # Not proof of absence: an overloaded edge answers 503 for a
                # published archive, and a second upload would duplicate it.
                print(f"{pin['name']} {pin['version']}: ERROR (HTTP {error.code}) {url}")
                status = max(status, REGISTRY_ERROR)
            continue
        except urllib.error.URLError as error:
            print(f"{pin['name']} {pin['version']}: UNREACHABLE ({error.reason}) {url}")
            status = max(status, MISSING)
            continue
        if actual != pin["sha256"]:
            print(
                f"{pin['name']} {pin['version']}: MISMATCH registry {actual} "
                f"pin {pin['sha256']}"
            )
            status = max(status, MISMATCH)
        else:
            print(f"{pin['name']} {pin['version']}: ok {actual}")

    if args.token_file is None:
        print("protection: skipped (pass --token-file to check the rule)")
    else:
        try:
            token = read_token(args.token_file)
            status = max(status, check_protection(api_url, args.project_id, token))
        except (OSError, ValueError, urllib.error.URLError) as error:
            print(f"ERROR: protection check failed: {error}", file=sys.stderr)
            status = max(status, UNPROTECTED)

    return status


if __name__ == "__main__":
    raise SystemExit(main())
