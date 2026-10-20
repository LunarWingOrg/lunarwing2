#!/usr/bin/env python3
"""WI#363: fail CI when an ic/deny.toml clearing owner closes.

Every [advisories].ignore entry in ic/deny.toml must name a "clearing owner
WI#N" in its reason — the offline half (validate_deny_toml_advisories in
validate_config.py) requires the reference at ci-config time. This guard is the
online half and runs as the pinned script of its own cargo-deny-owners job
(run-164 KV-1c: keeping it out of cargo-deny means a red cargo-deny always
means a real advisory finding, instead of the owner red and an advisory red
sharing one badge): each distinct named owner is resolved through the
anonymous GitLab issue API and must be state == "opened".

Failure mode (deliberate, WI#363): this script exits nonzero on any failure
— the moment a named owner WI closes, the job goes red naming the WI and
its advisory IDs (the 2.1.5.0 reasons named WI#304/#305/#306, which were
closed without the migrations landing, and nobody noticed until a review;
on 2026-10-17 a bulk close orphaned WI#418/WI#368 the same way). While no
open clearing owner exists (maintainer decision pending, WI#363 note
#10157), the job carries allow_failure: WI#363's documented "warns" mode —
the red stays visible as a warning without drowning the advisory signal,
and only a maintainer names owners and then removes allow_failure in the
same change.
Merging is not CI-gated on this project, so the signal is visibility, not
a block. The maintenance step when closing an owner WI: re-point the
ic/deny.toml reasons to the new open owner first (see .gitlab/README.md).

Anonymous reads work on this public project for ISSUE-type work items.
Task-type work items 404 on the anonymous single-issue endpoint (WI#274
precedent), so a 404 fails with a message saying exactly that: clearing owners
must be issue-type work items.

Transient network failures (and 5xx/429) are retried with backoff like
cargo-deny-check.sh; an unreachable API after the retries fails closed — the
job already depends on the network for the advisory DB, and a check that warns
on outage silently stops checking (the index-failure silence class).

Stdlib only: this runs in the .rust-job image, which installs bare python3
(bootstrap-rust.sh) — no PyYAML. Keep the OWNER_PATTERN in sync with
validate_config.py's DENY_OWNER_PATTERN (pinned by .gitlab/tests).
"""

import json
import re
import sys
import time
import tomllib
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DENY_TOML = ROOT / "ic" / "deny.toml"
ISSUE_API = "https://git.lunarwing.org/api/v4/projects/lunarwing%2Flunarwing2/issues/{number}"
# Keep in sync with DENY_OWNER_PATTERN in .gitlab/scripts/validate_config.py
# (the drift is pinned by OwnerPatternDriftTests in
# .gitlab/tests/test_deny_toml_guards.py).
OWNER_PATTERN = re.compile(r"clearing owner:?\s*WI#(\d+)", re.IGNORECASE)
TIMEOUT_SECONDS = 60
ATTEMPTS = 3
BACKOFF_SECONDS = (5, 10)
RETRYABLE_STATUS = frozenset({429, 500, 502, 503, 504})


class OwnerNotFound(Exception):
    """The owner WI 404'd on the anonymous issue API (definitive: never
    retried). Task-type work items 404 on this endpoint (WI#274 precedent)."""


class OwnerLookupError(Exception):
    """A transient owner lookup failure (network, 5xx, 429): retried, then a
    hard failure — an unreachable check must not pass silently."""


def collect_owners(text: str) -> tuple[dict[int, list[str]], list[str]]:
    """Parse ic/deny.toml and map each named clearing owner WI number to the
    advisory IDs whose reasons name it. The second return lists entry labels
    whose reason names no owner (validate_config.py refuses that offline; this
    is the defense-in-depth copy so the job fails closed on the same shape)."""
    data = tomllib.loads(text)
    ignore = data.get("advisories", {}).get("ignore", [])
    if not isinstance(ignore, list):
        return {}, ["[advisories].ignore (not a list)"]
    owners: dict[int, list[str]] = {}
    missing: list[str] = []
    for index, entry in enumerate(ignore):
        if not isinstance(entry, dict):
            missing.append(f"entry {index} (not a table)")
            continue
        entry_id = entry.get("id")
        label = entry_id if isinstance(entry_id, str) and entry_id else f"entry {index}"
        reason = entry.get("reason")
        if not isinstance(reason, str):
            missing.append(label)
            continue
        found = [int(number) for number in OWNER_PATTERN.findall(reason)]
        if not found:
            missing.append(label)
        for number in found:
            owners.setdefault(number, []).append(label)
    return owners, missing


def fetch_issue_state(number: int) -> str:
    """Return the issue state ("opened"/"closed"/...) for WI#number via the
    anonymous GitLab issue API, or raise OwnerNotFound (404) /
    OwnerLookupError (transient)."""
    request = urllib.request.Request(
        ISSUE_API.format(number=number),
        headers={"User-Agent": "lunarwing-ci-deny-owner-check (WI#363)"},
    )
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
            payload = json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as error:
        if error.code == 404:
            raise OwnerNotFound(str(number)) from None
        if error.code in RETRYABLE_STATUS:
            raise OwnerLookupError(f"HTTP {error.code}") from None
        raise OwnerLookupError(f"HTTP {error.code} (not retried)") from None
    except (urllib.error.URLError, TimeoutError, OSError, json.JSONDecodeError) as error:
        raise OwnerLookupError(str(error)) from None
    state = payload.get("state")
    if not isinstance(state, str):
        raise OwnerLookupError("the API response carried no string state")
    return state


def check_owners(
    owners: dict[int, list[str]],
    missing: list[str],
    fetch,
    *,
    attempts: int = ATTEMPTS,
    backoff: tuple[int, ...] = BACKOFF_SECONDS,
    sleep=time.sleep,
) -> list[str]:
    """Verify every named clearing owner is open. `fetch(number) -> state` is
    injectable for tests. Every failure mode is a hard error (WI#363)."""
    errors: list[str] = []
    for label in missing:
        errors.append(
            f"ic/deny.toml {label} names no 'clearing owner WI#N' in its reason; "
            "every accepted advisory must name an open clearing owner work item"
        )
    for number in sorted(owners):
        ids = ", ".join(owners[number])
        state = None
        last_error: Exception | None = None
        for attempt in range(attempts):
            try:
                state = fetch(number)
                last_error = None
                break
            except OwnerNotFound:
                last_error = None
                break
            except OwnerLookupError as error:
                last_error = error
                if attempt < attempts - 1 and attempt < len(backoff):
                    sleep(backoff[attempt])
        if state is not None:
            if state != "opened":
                errors.append(
                    f"WI#{number} is {state!r}, not open, and is the named clearing "
                    f"owner of {ids}. Only a maintainer names a replacement owner: "
                    "report this red gate on the MR or on WI#363 and re-point the "
                    "ic/deny.toml reasons only to an owner the maintainer named; do not "
                    "file or reopen an owner work item (see .gitlab/README.md)."
                )
            continue
        if last_error is None:
            errors.append(
                f"WI#{number} returned 404 on the anonymous issue API. Task-type "
                "work items 404 on this endpoint (WI#274 precedent), so clearing "
                "owners must be issue-type work items (or the item was deleted). "
                f"Named by: {ids}."
            )
        else:
            errors.append(
                f"could not confirm WI#{number} is open after {attempts} attempts: "
                f"{last_error}. Failing closed (an unreachable check must not pass "
                "silently); on a git.lunarwing.org outage, retry the job. "
                f"Named by: {ids}."
            )
    return errors


def main() -> int:
    try:
        text = DENY_TOML.read_text(encoding="utf-8")
    except OSError as error:
        print(f"ERROR: cannot read ic/deny.toml: {error}", file=sys.stderr)
        return 1
    try:
        owners, missing = collect_owners(text)
    except tomllib.TOMLDecodeError as error:
        print(f"ERROR: ic/deny.toml does not parse: {error}", file=sys.stderr)
        return 1
    if not owners and not missing:
        print("ic/deny.toml names no accepted advisories; no clearing owners to check.")
        return 0
    errors = check_owners(owners, missing, fetch_issue_state)
    if errors:
        print("\n".join(f"ERROR: {error}" for error in errors), file=sys.stderr)
        return 1
    summary = ", ".join(
        f"WI#{number} ({len(ids)} advisor{'y' if len(ids) == 1 else 'ies'})"
        for number, ids in sorted(owners.items())
    )
    print(f"ic/deny.toml clearing owners all open: {summary}.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
