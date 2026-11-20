#!/usr/bin/env python3
"""Check how long the GitLab edge reads an unfinished HTTP request head.

The edge reads the request head before selecting a router. Raising the
entry-point deadline can turn CUT into OPEN, but also lets unauthenticated
connections occupy edge resources longer. A router-only body-timeout fix
can leave CUT unchanged. Verify uploads with a successful rate-limited
transfer on the affected route; see ``docs/ops/REGISTRY_PUSH_499.md``.

The probe never authenticates, uploads no body, and opens one connection per
host. Do not run it in a loop; it is an operator check, not a load test.
"""

from __future__ import annotations

import argparse
import socket
import ssl
import sys
import time
from dataclasses import dataclass
from typing import Callable, Protocol

DEFAULT_HOSTS = ("git.lunarwing.org", "registry.git.lunarwing.org")
DEFAULT_HOLD_SECONDS = 90.0
DRIBBLE_INTERVAL_SECONDS = 5.0
SOCKET_TIMEOUT_SECONDS = 30.0
FINAL_RESPONSE_TIMEOUT_SECONDS = 15.0
MAX_HOLD_SECONDS = 1800.0
OBSERVED_BOUNDARY_SECONDS = 60.0
READ_CHUNK = 4096


@dataclass(frozen=True)
class Outcome:
    host: str
    survived: bool
    elapsed: float
    detail: str


class Transport(Protocol):
    def send(self, data: bytes) -> None: ...

    def receive(self, timeout: float) -> bytes:
        """Return available bytes, b"" on EOF, or raise TimeoutError."""
        ...

    def close(self) -> None: ...


class TlsTransport:
    """One TLS connection to ``host:443`` with per-call read timeouts."""

    def __init__(self, host: str, timeout: float = SOCKET_TIMEOUT_SECONDS) -> None:
        raw = socket.create_connection((host, 443), timeout=timeout)
        self._socket = ssl.create_default_context().wrap_socket(
            raw, server_hostname=host
        )

    def send(self, data: bytes) -> None:
        self._socket.settimeout(SOCKET_TIMEOUT_SECONDS)
        self._socket.sendall(data)

    def receive(self, timeout: float) -> bytes:
        self._socket.settimeout(timeout)
        try:
            return self._socket.recv(READ_CHUNK)
        except (TimeoutError, ssl.SSLWantReadError):
            raise TimeoutError("no application data") from None
        except ssl.SSLEOFError:
            return b""

    def close(self) -> None:
        try:
            self._socket.close()
        except OSError:
            pass


def request_head(host: str, path: str) -> bytes:
    """The start of an HTTP/1.1 request, deliberately left incomplete."""
    return (
        f"GET {path} HTTP/1.1\r\n"
        f"Host: {host}\r\n"
        "User-Agent: lunarwing-edge-read-timeout-probe/1\r\n"
        "Accept: */*\r\n"
    ).encode()


def status_line(response: bytes) -> str:
    """First line of an HTTP response, bounded and printable for the report."""
    line = response.split(b"\r\n", 1)[0]
    return line.decode("latin-1", "replace")[:120] or "empty response"


def probe_host(
    host: str,
    *,
    hold: float,
    path: str = "/",
    transport_factory: Callable[[str], Transport] | None = None,
    clock: Callable[[], float] = time.monotonic,
    interval: float = DRIBBLE_INTERVAL_SECONDS,
) -> Outcome:
    """Leave the request head unfinished for the hold and report what happened."""
    factory = transport_factory or TlsTransport
    transport = factory(host)
    start = clock()
    try:
        try:
            transport.send(request_head(host, path))
        except OSError as error:
            return Outcome(host, False, clock() - start, f"send failed: {error}")

        while True:
            remaining = hold - (clock() - start)
            if remaining <= 0:
                break
            try:
                data = transport.receive(min(interval, remaining))
            except TimeoutError:
                data = None
            except OSError as error:
                return Outcome(host, False, clock() - start, f"closed mid-request: {error}")
            if data is not None:
                if not data:
                    return Outcome(host, False, clock() - start, "closed mid-request")
                return Outcome(
                    host, False, clock() - start, f"early response: {status_line(data)}"
                )
            if clock() - start >= hold:
                break
            try:
                transport.send(b"X-Probe-Keepalive: 1\r\n")
            except OSError as error:
                return Outcome(host, False, clock() - start, f"closed mid-request: {error}")

        elapsed = clock() - start
        try:
            transport.send(b"\r\n")
            response = transport.receive(FINAL_RESPONSE_TIMEOUT_SECONDS)
        except TimeoutError:
            return Outcome(host, True, elapsed, "no upstream response within 15s")
        except OSError as error:
            return Outcome(host, False, elapsed, f"closed at end of hold: {error}")
        if not response:
            return Outcome(host, False, elapsed, "closed at end of hold")
        return Outcome(host, True, elapsed, status_line(response))
    finally:
        transport.close()


def report(outcome: Outcome, hold: float) -> None:
    if outcome.survived:
        print(
            f"OPEN  {outcome.host}: probe reached the hold with the head unfinished "
            f"after {outcome.elapsed:.2f}s (hold {hold:g}s); {outcome.detail}"
        )
    else:
        print(
            f"CUT   {outcome.host}: unfinished-head check stopped after "
            f"{outcome.elapsed:.2f}s; {outcome.detail}"
        )


def parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Check the GitLab edge's read deadline for an unfinished HTTP "
            "request head, not whether it accepts long uploads."
        )
    )
    parser.add_argument(
        "--host",
        action="append",
        dest="hosts",
        metavar="HOST",
        help=f"host to probe; repeatable (default: {' '.join(DEFAULT_HOSTS)})",
    )
    parser.add_argument(
        "--hold",
        type=float,
        default=DEFAULT_HOLD_SECONDS,
        help=f"seconds to leave the request head unfinished (default: {DEFAULT_HOLD_SECONDS:g})",
    )
    parser.add_argument(
        "--path",
        default="/",
        help="request path; the response is ignored (default: /)",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if not 0 < args.hold <= MAX_HOLD_SECONDS:
        print(f"ERROR: --hold must be in (0, {MAX_HOLD_SECONDS:g}]", file=sys.stderr)
        return 2

    hosts = args.hosts or list(DEFAULT_HOSTS)
    failures = 0
    for host in hosts:
        try:
            outcome = probe_host(host, hold=args.hold, path=args.path)
        except OSError as error:
            print(f"ERROR {host}: {error}", file=sys.stderr)
            failures += 1
            continue
        report(outcome, args.hold)
        if not outcome.survived:
            failures += 1

    if failures:
        print(
            f"Could not complete the unfinished-head check on {failures} host(s). "
            "A router-only upload fix can leave CUT unchanged. Verify with a "
            "successful rate-limited upload; see docs/ops/REGISTRY_PUSH_499.md.",
            file=sys.stderr,
        )
        return 1
    if args.hold <= OBSERVED_BOUNDARY_SECONDS:
        print(
            f"HEAD CHECK ONLY (short hold): the probe reached {args.hold:g}s on "
            "every host, but that does not cross the observed 60 s boundary. "
            "Verify uploads separately."
        )
        return 0
    print(
        "HEAD CHECK ONLY: the probe reached the full hold on every host. "
        "Raising the entry-point deadline can turn CUT into OPEN, but lets "
        "unauthenticated connections occupy edge resources longer. A router-only "
        "change does not affect this check. Verify uploads with a successful "
        "rate-limited transfer; see docs/ops/REGISTRY_PUSH_499.md."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
