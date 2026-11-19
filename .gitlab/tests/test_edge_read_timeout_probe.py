#!/usr/bin/env python3
"""WI#706: the edge read-timeout probe must classify a cut correctly.

The probe distinguishes "the edge closed the connection while the request was
still being read" (the 60-second Traefik ``readTimeout``) from "the request
stayed readable for the whole hold and the upstream answered". These tests use
a fake transport so they never touch the network.
"""

import importlib.util
import io
import pathlib
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[2]
PROBE_PATH = ROOT / ".gitlab" / "scripts" / "edge-read-timeout-probe.py"
MODULE_NAME = "edge_read_timeout_probe"
RESPONSE = b"HTTP/1.1 302 Found\r\nlocation: /users/sign_in\r\n\r\n"


def load_probe_module():
    spec = importlib.util.spec_from_file_location(MODULE_NAME, PROBE_PATH)
    module = importlib.util.module_from_spec(spec)
    sys.modules[MODULE_NAME] = module
    spec.loader.exec_module(module)
    return module


class FakeClock:
    def __init__(self) -> None:
        self.now = 0.0

    def __call__(self) -> float:
        return self.now

    def advance(self, seconds: float) -> None:
        self.now += seconds


class FakeTransport:
    """Scripted peer: EOF at ``close_at`` seconds, an answer once headers complete."""

    def __init__(
        self,
        clock: FakeClock,
        *,
        close_at: float | None = None,
        fail_send_at: int | None = None,
    ) -> None:
        self.clock = clock
        self.close_at = close_at
        self.fail_send_at = fail_send_at
        self.sends: list[bytes] = []
        self.closed = False
        self.complete = False

    def send(self, data: bytes) -> None:
        if self.fail_send_at is not None and len(self.sends) >= self.fail_send_at:
            raise OSError("connection reset by peer")
        self.sends.append(data)
        if data == b"\r\n":
            self.complete = True

    def receive(self, timeout: float) -> bytes:
        if self.complete:
            return RESPONSE
        if self.close_at is not None:
            self.clock.advance(min(timeout, max(0.0, self.close_at - self.clock.now)))
            if self.clock.now >= self.close_at:
                return b""
            raise TimeoutError("no application data")
        self.clock.advance(timeout)
        raise TimeoutError("no application data")

    def close(self) -> None:
        self.closed = True


class EarlyResponseTransport(FakeTransport):
    def receive(self, timeout: float) -> bytes:
        return RESPONSE


class ProbeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.probe = load_probe_module()

    def run_probe(self, transport, clock: FakeClock, hold: float = 90.0):
        return self.probe.probe_host(
            "git.example.test",
            hold=hold,
            transport_factory=lambda _host: transport,
            clock=clock,
        )

    def test_cut_at_sixty_seconds_is_a_failure(self) -> None:
        clock = FakeClock()
        transport = FakeTransport(clock, close_at=60.0)
        outcome = self.run_probe(transport, clock)
        self.assertFalse(outcome.survived)
        self.assertAlmostEqual(outcome.elapsed, 60.0)
        self.assertEqual(outcome.detail, "closed mid-request")
        self.assertTrue(transport.closed)

    def test_surviving_the_hold_reports_the_upstream_status(self) -> None:
        clock = FakeClock()
        outcome = self.run_probe(FakeTransport(clock), clock)
        self.assertTrue(outcome.survived)
        self.assertAlmostEqual(outcome.elapsed, 90.0)
        self.assertEqual(outcome.detail, "HTTP/1.1 302 Found")

    def test_send_failure_mid_request_is_a_cut(self) -> None:
        clock = FakeClock()
        outcome = self.run_probe(FakeTransport(clock, fail_send_at=3), clock)
        self.assertFalse(outcome.survived)
        self.assertIn("closed mid-request", outcome.detail)
        self.assertAlmostEqual(outcome.elapsed, 15.0)

    def test_early_response_is_not_a_pass(self) -> None:
        clock = FakeClock()
        outcome = self.run_probe(EarlyResponseTransport(clock), clock)
        self.assertFalse(outcome.survived)
        self.assertIn("early response", outcome.detail)

    def test_main_fails_when_any_host_is_cut(self) -> None:
        cut = self.probe.Outcome("a.test", False, 60.07, "closed mid-request")
        opened = self.probe.Outcome("b.test", True, 90.0, "HTTP/1.1 401")
        with mock.patch.object(self.probe, "probe_host", side_effect=[cut, opened]):
            with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                self.assertEqual(self.probe.main(["--hold", "90"]), 1)

    def test_main_passes_when_every_host_survives(self) -> None:
        opened = self.probe.Outcome("a.test", True, 90.0, "HTTP/1.1 401")
        with mock.patch.object(self.probe, "probe_host", return_value=opened):
            with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                self.assertEqual(self.probe.main(["--host", "a.test"]), 0)

    def test_main_rejects_an_unbounded_hold(self) -> None:
        with redirect_stderr(io.StringIO()):
            self.assertEqual(self.probe.main(["--hold", "0"]), 2)


if __name__ == "__main__":
    unittest.main()
