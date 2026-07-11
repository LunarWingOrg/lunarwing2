"""Post-start health verification for a freshly provisioned tenant.

Service checks go through ``lunarwing-mt-admin.sh status <tenant>`` so they
match operator tooling and stay init-agnostic (systemd --user on most hosts,
OpenRC on Gentoo). Bare ``systemctl is-active`` is wrong here: tenant units
live under the tenant's user manager, not the system bus, so that check
false-fails healthy tenants.
"""

from __future__ import annotations

import re
import socket
import subprocess
import time
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from typing import Any

DEFAULT_TIMEOUT = 90
POLL_INTERVAL = 3

# Primary daemon line shapes from ``status_tenant``:
#   systemd:  "  lunarwing-<t>.service: active"
#   openrc:   "  lunarwing-<t>: started"
_DAEMON_LINE_RE = re.compile(
    r"^\s*(?P<label>lunarwing-(?P<tenant>[A-Za-z0-9-]+)(?:\.service)?)\s*:\s*(?P<state>\S+)",
    re.IGNORECASE,
)

# States that mean the daemon is up under either init.
_UP_STATES = frozenset({"active", "started", "running"})


@dataclass
class CheckResult:
    label: str
    ok: bool
    detail: str = ""


def check_port(host: str, port: int, timeout: float = 5.0) -> CheckResult:
    """Return a CheckResult for a TCP port reachability test."""
    try:
        with socket.create_connection((host, int(port)), timeout=timeout):
            return CheckResult(f"port {port}", True)
    except OSError as exc:
        return CheckResult(f"port {port}", False, str(exc))


def parse_daemon_state(status_output: str, tenant: str) -> tuple[bool | None, str]:
    """Extract the primary daemon state from ``mt-admin status`` text.

    Returns ``(ok, detail)`` where ``ok`` is True/False when the daemon line is
    found, or ``None`` when the line is missing (caller should treat as fail).
    """
    tenant = tenant.strip()
    expected = {
        f"lunarwing-{tenant}".lower(),
        f"lunarwing-{tenant}.service".lower(),
    }
    for raw in status_output.splitlines():
        match = _DAEMON_LINE_RE.match(raw)
        if not match:
            continue
        if match.group("tenant") != tenant:
            continue
        if match.group("label").lower() not in expected:
            continue
        state = match.group("state").lower()
        return state in _UP_STATES, state
    return None, "daemon service line not found in mt-admin status"


def check_service_status(
    tenant: str,
    *,
    script: str | None = None,
    runner: Callable[[Sequence[str]], Any] | None = None,
) -> CheckResult:
    """Return a CheckResult for the tenant daemon via ``mt-admin status``.

    *script* overrides the resolved mt-admin path (tests). *runner* is an
    optional ``(argv) -> CompletedProcess-like`` hook for unit tests.
    """
    label = f"service lunarwing-{tenant}"
    try:
        if script is None:
            from lunarwing_mt_onboard.provisioner import ensure_mt_admin

            admin = ensure_mt_admin()
        else:
            admin = script
    except FileNotFoundError as exc:
        return CheckResult(label, False, str(exc))

    argv = [admin, "status", tenant]
    try:
        if runner is not None:
            result = runner(argv)
        else:
            result = subprocess.run(
                argv,
                capture_output=True,
                text=True,
                timeout=30,
            )
    except (OSError, subprocess.SubprocessError) as exc:
        return CheckResult(label, False, str(exc))

    text = result.stdout or ""
    if result.stderr:
        text = f"{text}\n{result.stderr}" if text else result.stderr

    if result.returncode != 0:
        detail = text.strip()[:300] or f"mt-admin status exit {result.returncode}"
        return CheckResult(label, False, detail)

    ok, detail = parse_daemon_state(text, tenant)
    if ok is None:
        return CheckResult(label, False, detail)
    return CheckResult(label, ok, detail)


def verify_tenant(
    tenant: str,
    host: str = "127.0.0.1",
    port: int = 0,
    *,
    timeout: int = DEFAULT_TIMEOUT,
    script: str | None = None,
) -> list[CheckResult]:
    """Poll optional TCP port + mt-admin status until healthy or *timeout*.

    Returns a list of CheckResult items for display.
    """
    results: list[CheckResult] = []
    deadline = time.monotonic() + timeout

    while time.monotonic() < deadline:
        results.clear()
        if port:
            results.append(check_port(host, port))
        results.append(check_service_status(tenant, script=script))
        if all(r.ok for r in results):
            return results
        time.sleep(POLL_INTERVAL)

    return results
