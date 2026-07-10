"""Loopback session token.

This app can create OS users and run privileged setup scripts, so it must not
be reachable by other local users or cross-site requests. We bind to 127.0.0.1
and require a random per-session token (printed in the launch URL) on every
API and WebSocket request.
"""

from __future__ import annotations

import secrets


def generate_token() -> str:
    """Return a fresh URL-safe session token."""
    return secrets.token_urlsafe(24)


def token_matches(expected: str, provided: str | None) -> bool:
    """Constant-time compare; an empty *expected* disables the check."""
    if not expected:
        return True
    if not provided:
        return False
    return secrets.compare_digest(expected, provided)
