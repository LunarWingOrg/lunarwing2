#!/usr/bin/env python3
"""WI#807: the bounded-turn probe reads a turn record, never an impression.

The probe's only job is to turn the daemon's `Turn finished` log line into a
verdict on a task with a known step count. These tests use a fake gateway, so
they never touch a tenant.
"""

import importlib.util
import json
import pathlib
import re
import sys
import unittest
import urllib.error
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[2]
PROBE_PATH = ROOT / "ic" / "scripts" / "bounded-turn-probe.py"
MODULE_NAME = "bounded_turn_probe"


def load_probe_module():
    spec = importlib.util.spec_from_file_location(MODULE_NAME, PROBE_PATH)
    module = importlib.util.module_from_spec(spec)
    sys.modules[MODULE_NAME] = module
    spec.loader.exec_module(module)
    return module


probe = load_probe_module()

THREAD = "5f0e4a52-6a3e-4a8f-9a1e-2b9d6c1e7a10"


def record_line(
    thread_id=THREAD,
    phase="foreground",
    tool_calls=8,
    budget_secs=610,
    exhausted="false",
    elapsed_ms=240_000,
    outcome="completed",
):
    return (
        f"Turn finished thread_id={thread_id} phase={phase} tool_calls={tool_calls} "
        f"budget_secs={budget_secs} budget_exhausted={exhausted} "
        f"elapsed_ms={elapsed_ms} outcome={outcome}"
    )


class TurnRecordParsingTests(unittest.TestCase):
    def test_parses_the_daemon_turn_finished_line(self):
        record = probe.parse_turn_finished(record_line())
        self.assertEqual(
            record,
            {
                "thread_id": THREAD,
                "phase": "foreground",
                "tool_calls": 8,
                "budget_secs": 610,
                "budget_exhausted": False,
                "elapsed_ms": 240_000,
                "outcome": "completed",
            },
        )

    def test_reads_the_late_phase_and_an_exhausted_budget(self):
        record = probe.parse_turn_finished(
            record_line(phase="late", budget_secs=1_320, exhausted="true", outcome="failed")
        )
        self.assertEqual(record["phase"], "late")
        self.assertEqual(record["budget_secs"], 1_320)
        self.assertTrue(record["budget_exhausted"])

    def test_ignores_a_line_missing_a_measurement_field(self):
        line = record_line()
        without_outcome = line.rsplit(" ", 1)[0]
        self.assertIsNone(probe.parse_turn_finished(without_outcome))

    def test_ignores_other_log_messages(self):
        self.assertIsNone(
            probe.parse_turn_finished(
                "Turn wait budget exhausted; transferring delivery to late watcher "
                "thread_id=x tool_calls=0 late_budget_secs=600"
            )
        )
        self.assertIsNone(probe.parse_turn_finished("Late turn budget exhausted; stopping thread"))

    def test_a_non_numeric_field_is_not_a_record(self):
        self.assertIsNone(probe.parse_turn_finished(record_line(elapsed_ms="soon")))

    def test_selects_only_the_probed_thread(self):
        messages = [
            record_line(thread_id="other-thread"),
            record_line(thread_id=THREAD, tool_calls=3),
        ]
        records = probe.records_for_thread(messages, THREAD)
        self.assertEqual([record["tool_calls"] for record in records], [3])


class VerdictTests(unittest.TestCase):
    def test_a_bounded_task_inside_its_budget_passes(self):
        reading, code = probe.verdict(
            probe.parse_turn_finished(record_line()), expected_steps=8
        )
        self.assertEqual(code, 0)
        self.assertIn("FITS: 8 tool calls inside 610 s", reading)

    def test_an_exhausted_budget_is_the_finding(self):
        reading, code = probe.verdict(
            probe.parse_turn_finished(
                record_line(phase="late", budget_secs=1_320, exhausted="true")
            ),
            expected_steps=8,
        )
        self.assertEqual(code, 3)
        self.assertIn("EXHAUSTED", reading)

    def test_a_short_step_count_is_reported_as_a_mismatch(self):
        reading, code = probe.verdict(
            probe.parse_turn_finished(record_line(tool_calls=5)), expected_steps=8
        )
        self.assertEqual(code, 4)
        self.assertIn("5 of 8 requested tool calls", reading)

    def test_exhaustion_outranks_a_step_mismatch(self):
        _, code = probe.verdict(
            probe.parse_turn_finished(record_line(tool_calls=2, exhausted="true")),
            expected_steps=8,
        )
        self.assertEqual(code, 3)


class FakeGateway:
    """Serves the contract endpoints, a history body and a scripted stream."""

    def __init__(self, history_body=""):
        self.base_url = "http://127.0.0.1:1"
        self.token = "token"
        self.timeout = 60
        self.history_body = history_body
        self.sent = []

    def raw_get(self, path):
        if path == "/api/chat/capabilities":
            return 200, json.dumps({"api_version": 1}).encode()
        raise AssertionError(f"unexpected GET {path}")

    def get_text(self, path):
        if path == "/api/logs/download":
            return self.history_body
        raise AssertionError(f"unexpected GET {path}")

    def post_json(self, path, payload):
        if path == "/api/chat/thread/new":
            return {"id": THREAD}
        if path == "/api/chat/send":
            self.sent.append(payload)
            return {"message_id": "0b8a6c1e-0000-0000-0000-000000000000", "status": "accepted"}
        raise AssertionError(f"unexpected POST {path}")


def log_body(*messages):
    entries = [
        {"level": "INFO", "target": "lunarwing", "message": message, "timestamp": "t"}
        for message in messages
    ]
    return "\n".join(json.dumps(entry) for entry in entries)


def entry(message):
    return {"level": "INFO", "target": "lunarwing", "message": message, "timestamp": "t"}


class ContractTests(unittest.TestCase):
    """A refused token and a legacy daemon must not read as the same failure."""

    class StatusGateway:
        def __init__(self, status):
            self.status = status

        def raw_get(self, _path):
            return self.status, b"body"

    def test_a_healthy_contract_passes(self):
        probe.require_contract(self.StatusGateway(200))

    def test_a_refused_token_is_named_as_a_token_problem(self):
        for status in (401, 403):
            with self.assertRaises(probe.ProbeError) as caught:
                probe.require_contract(self.StatusGateway(status))
            self.assertIn("refused the token", str(caught.exception))

    def test_a_missing_capabilities_route_is_a_legacy_daemon(self):
        with self.assertRaises(probe.ProbeError) as caught:
            probe.require_contract(self.StatusGateway(404))
        self.assertIn("legacy daemon", str(caught.exception))


class WaitTests(unittest.TestCase):
    def test_a_record_already_in_history_is_the_answer(self):
        gateway = FakeGateway(history_body=log_body(record_line(tool_calls=6)))

        def no_stream(_gateway, _timeout):
            raise AssertionError("a history hit must not open the stream")

        with mock.patch.object(probe, "stream_logs", no_stream):
            record = probe.wait_for_record(gateway, THREAD, timeout=60)
        self.assertEqual(record["tool_calls"], 6)

    def test_the_live_stream_supplies_a_record_the_history_missed(self):
        # The churning ring buffer no longer holds the record: only the stream
        # (history replay plus live tail) sees it.
        gateway = FakeGateway(history_body=log_body("unrelated"))

        def stream(_gateway, _timeout):
            yield entry(record_line(thread_id="other-thread"))
            yield entry(record_line(tool_calls=8, phase="late"))

        with mock.patch.object(probe, "stream_logs", stream):
            record = probe.wait_for_record(gateway, THREAD, timeout=60)
        self.assertEqual(record["phase"], "late")

    def test_gives_up_when_the_stream_ends_without_a_record(self):
        gateway = FakeGateway(history_body=log_body("unrelated"))
        with mock.patch.object(probe, "stream_logs", lambda _g, _t: iter(())):
            self.assertIsNone(probe.wait_for_record(gateway, THREAD, timeout=60))


class StreamTests(unittest.TestCase):
    """A closed window is a verdict, a broken transport is a harness error."""

    def test_an_idle_stream_that_times_out_yields_no_entries(self):
        gateway = FakeGateway()
        with mock.patch.object(
            probe.urllib.request, "urlopen", side_effect=TimeoutError("idle")
        ):
            self.assertEqual(list(probe.stream_logs(gateway, timeout=1)), [])

    def test_a_refused_connection_is_a_harness_error(self):
        gateway = FakeGateway()
        with mock.patch.object(
            probe.urllib.request, "urlopen", side_effect=urllib.error.URLError("refused")
        ):
            with self.assertRaises(probe.ProbeError):
                list(probe.stream_logs(gateway, timeout=1))

    def test_data_lines_parse_and_comments_are_skipped(self):
        gateway = FakeGateway()
        payload = (
            b": keep-alive\n\n"
            b'data: {"message": "Turn finished thread_id=x"}\n\n'
        )

        class FakeResponse:
            def __enter__(self):
                return iter(payload.splitlines(keepends=True))

            def __exit__(self, *_exc):
                return False

        with mock.patch.object(probe.urllib.request, "urlopen", return_value=FakeResponse()):
            entries = list(probe.stream_logs(gateway, timeout=5))
        self.assertEqual([entry["message"] for entry in entries], ["Turn finished thread_id=x"])


class SendTests(unittest.TestCase):
    def test_the_prompt_names_the_exact_step_count_and_no_other_tool(self):
        gateway = FakeGateway([log_body()])
        probe.send_bounded_task(gateway, THREAD, steps=8)
        content = gateway.sent[0]["content"]
        self.assertIn("exactly 8 times", content)
        self.assertIn("Do not use any other tool", content)
        self.assertEqual(gateway.sent[0]["thread_id"], THREAD)


class TargetTests(unittest.TestCase):
    def test_a_tenant_resolves_to_its_gateway_port_and_token(self):
        args = probe.parse_args(["--tenant", "antelope"])
        with mock.patch.object(
            probe,
            "tenant_gateway",
            return_value=("http://127.0.0.1:10000", "token"),
        ):
            self.assertEqual(
                probe.resolve_target(args), ("http://127.0.0.1:10000", "token")
            )

    def test_explicit_url_and_token_file_need_no_tenant(self):
        args = probe.parse_args(["--base-url", "http://127.0.0.1:1", "--token-file", "-"])
        with mock.patch.object(probe, "open", mock.mock_open(read_data="tok\n"), create=True):
            self.assertEqual(
                probe.resolve_target(args), ("http://127.0.0.1:1", "tok")
            )

    def test_a_missing_target_is_a_harness_error(self):
        args = probe.parse_args(["--steps", "8"])
        with self.assertRaises(probe.ProbeError):
            probe.resolve_target(args)

    def test_zero_steps_is_a_harness_error(self):
        args = probe.parse_args(
            ["--base-url", "http://127.0.0.1:1", "--token-file", "-", "--steps", "0"]
        )
        with mock.patch.object(probe, "open", mock.mock_open(read_data="tok\n"), create=True):
            with self.assertRaises(probe.ProbeError):
                probe.resolve_target(args)


class SourceContractTests(unittest.TestCase):
    """The record is rendered by Rust and parsed here; drift must fail a test.

    `log_turn_finished` is the one writer of the line, and these tests are the
    one reader outside it. If a field is renamed or dropped on either side the
    probe would silently find nothing, so the names are pinned against the
    source rather than against a fixture.
    """

    OUTCOME_RS = ROOT / "ic" / "src" / "bridge" / "router" / "outcome.rs"

    def turn_finished_block(self):
        source = self.OUTCOME_RS.read_text(encoding="utf-8")
        body = source.split("fn log_turn_finished", 1)[1]
        block = body.split("tracing::info!(", 1)[1]
        return block.split("\n        );", 1)[0]

    def test_every_field_the_probe_requires_is_emitted(self):
        block = self.turn_finished_block()
        for name in probe.REQUIRED_FIELDS:
            assigned = re.search(rf"\b{name}\s*=", block)
            shorthand = re.search(rf"(?:^|[\s(,])\b{name}\s*,", block, re.MULTILINE)
            self.assertTrue(
                assigned or shorthand,
                f"log_turn_finished no longer emits the field '{name}'",
            )

    def test_no_field_the_probe_reads_is_renamed_away(self):
        self.assertEqual(
            set(probe.REQUIRED_FIELDS),
            {
                "thread_id",
                "phase",
                "tool_calls",
                "budget_secs",
                "budget_exhausted",
                "elapsed_ms",
                "outcome",
            },
        )


if __name__ == "__main__":
    unittest.main()
