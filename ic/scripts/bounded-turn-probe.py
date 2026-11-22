#!/usr/bin/env python3
"""Bounded-turn probe: did a known-step task finish inside its turn budget?

WI#807's `Turn finished` record answers "slow, thrashing, or doing a lot of
real work" only if the task has a known step count. This probe supplies one:
it asks the agent for exactly N discrete `echo` calls (default 8), waits for
the turn to end, then reads the record the daemon already writes and reports
the measurement instead of an impression.

The measurement is read from the daemon's own log stream
(`GET /api/logs/events`), which replays its recent history before the live
tail, so it works the same on systemd, OpenRC and launchd hosts and needs no
journal access.

Exit codes:
  0  the turn finished inside its budget with the requested step count
  1  harness/setup error (auth, contract, transport)
  2  no `Turn finished` record for the thread within --timeout
  3  the turn used its whole budget (`budget_exhausted=true`)
  4  the turn finished but completed a different number of tool calls

Usage:
  ic/scripts/bounded-turn-probe.py --tenant antelope
  ic/scripts/bounded-turn-probe.py --base-url http://127.0.0.1:10000 \\
      --token-file /run/lunarwing/probe-token --steps 8
"""

import argparse
import json
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

DEFAULT_STEPS = 8
DEFAULT_TIMEOUT = 2700
TURN_FINISHED = "Turn finished"
REQUIRED_FIELDS = (
    "thread_id",
    "phase",
    "tool_calls",
    "budget_secs",
    "budget_exhausted",
    "elapsed_ms",
    "outcome",
)
FIELD_RE = re.compile(r"([A-Za-z_][A-Za-z0-9_]*)=(\S+)")


class ProbeError(RuntimeError):
    """A harness-level failure: setup, auth or transport, never the measurement."""


def read_env_file(path):
    """Parse a tenant env file into a dict; last assignment wins."""
    values = {}
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            values[key.strip()] = value.strip().strip('"').strip("'")
    return values


def tenant_gateway(tenant):
    """Base URL and token for a tenant from its generated env file."""
    env_path = f"/home/{tenant}/lunarwing/env/lunarwing.env"
    try:
        env = read_env_file(env_path)
    except OSError as error:
        raise ProbeError(f"cannot read {env_path}: {error}") from error
    port = env.get("GATEWAY_PORT")
    token = env.get("GATEWAY_AUTH_TOKEN")
    if not port or not token:
        raise ProbeError(f"{env_path} has no GATEWAY_PORT/GATEWAY_AUTH_TOKEN")
    return f"http://127.0.0.1:{port}", token


class Gateway:
    """Minimal bearer-authenticated client for the chat contract."""

    def __init__(self, base_url, token, timeout=60):
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.timeout = timeout

    def _request(self, method, path, body=None):
        data = None
        headers = {"Authorization": f"Bearer {self.token}"}
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(
            f"{self.base_url}{path}", data=data, headers=headers, method=method
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                return response.status, response.read()
        except urllib.error.HTTPError as error:
            return error.code, error.read()
        except (urllib.error.URLError, OSError) as error:
            raise ProbeError(f"{method} {path} failed: {error}") from error

    def get_text(self, path):
        status, body = self._request("GET", path)
        if status != 200:
            raise ProbeError(f"GET {path} answered {status}: {body[:200]!r}")
        return body.decode("utf-8", "replace")

    def raw_get(self, path):
        """The status and body of a GET, for callers that classify the status."""
        return self._request("GET", path)

    def post_json(self, path, payload):
        status, body = self._request("POST", path, payload)
        if status not in (200, 202):
            raise ProbeError(f"POST {path} answered {status}: {body[:200]!r}")
        try:
            return json.loads(body)
        except json.JSONDecodeError as error:
            raise ProbeError(f"POST {path} did not answer JSON: {error}") from error


def require_contract(gateway):
    """A daemon without the chat contract has no turn lifecycle events.

    The status separates the two failures an operator hits first: a refused
    token is a bad `--token-file`, while a 404 is a daemon that predates the
    contract and has no turn lifecycle events at all.
    """
    status, body = gateway.raw_get("/api/chat/capabilities")
    if status in (401, 403):
        raise ProbeError(f"the gateway refused the token ({status}); check --token-file")
    if status == 404:
        raise ProbeError(
            "the daemon does not serve /api/chat/capabilities (legacy daemon?)"
        )
    if status != 200:
        raise ProbeError(f"GET /api/chat/capabilities answered {status}: {body[:200]!r}")


def create_thread(gateway):
    thread = gateway.post_json("/api/chat/thread/new", {})
    thread_id = thread.get("id")
    if not thread_id:
        raise ProbeError(f"thread/new answered no id: {thread}")
    return thread_id


def bounded_task_prompt(steps):
    return (
        f"Use the echo tool exactly {steps} times, one call per step. "
        f"Echo the text 'step 1' through 'step {steps}', one call each. "
        "Do not use any other tool. When all calls are done, reply with "
        "the single word DONE."
    )


def send_bounded_task(gateway, thread_id, steps):
    accepted = gateway.post_json(
        "/api/chat/send",
        {
            "content": bounded_task_prompt(steps),
            "thread_id": thread_id,
            "client_turn_id": f"wi807-probe-{int(time.time())}",
        },
    )
    message_id = accepted.get("message_id")
    if not message_id:
        raise ProbeError(f"chat/send answered no message_id: {accepted}")
    return message_id


def parse_turn_finished(message):
    """One `Turn finished` log line as a record, or None for anything else.

    The daemon writes `message` plus every field as `key=value` tokens
    (WebLogLayer's visitor). A line is a record only when every field the
    measurement depends on is present, so a truncated or foreign line cannot
    be read as a data point.
    """
    if not message.startswith(TURN_FINISHED):
        return None
    fields = dict(FIELD_RE.findall(message[len(TURN_FINISHED) :]))
    if any(name not in fields for name in REQUIRED_FIELDS):
        return None
    try:
        record = {
            "thread_id": fields["thread_id"],
            "phase": fields["phase"],
            "tool_calls": int(fields["tool_calls"]),
            "budget_secs": int(fields["budget_secs"]),
            "budget_exhausted": fields["budget_exhausted"] == "true",
            "elapsed_ms": int(fields["elapsed_ms"]),
            "outcome": fields["outcome"],
        }
    except ValueError:
        return None
    return record


def log_messages(entries):
    """The `message` field of each log entry, in order."""
    return [entry.get("message", "") for entry in entries if isinstance(entry, dict)]


def records_for_thread(messages, thread_id):
    records = []
    for message in messages:
        record = parse_turn_finished(message)
        if record is not None and record["thread_id"] == thread_id:
            records.append(record)
    return records


def download_logs(gateway):
    """The daemon's recent log entries, from its ring buffer (newline JSON)."""
    entries = []
    for line in gateway.get_text("/api/logs/download").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            entries.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return entries


def stream_logs(gateway, timeout):
    """Yield log entries from `/api/logs/events` until `timeout` runs out.

    The endpoint replays its recent history before the live tail, so opening it
    after the send still sees anything already logged. That matters on a tenant
    whose ring buffer churns: polling `/api/logs/download` can miss a record
    written and evicted between two snapshots, while a stream cannot.
    """
    request = urllib.request.Request(
        f"{gateway.base_url}/api/logs/events",
        headers={"Authorization": f"Bearer {gateway.token}", "Accept": "text/event-stream"},
    )
    deadline = time.monotonic() + timeout
    try:
        with urllib.request.urlopen(
            request, timeout=min(gateway.timeout, max(timeout, 1.0))
        ) as response:
            for raw in response:
                if time.monotonic() >= deadline:
                    return
                line = raw.decode("utf-8", "replace").strip()
                if not line.startswith("data:"):
                    continue
                try:
                    yield json.loads(line[len("data:") :].strip())
                except json.JSONDecodeError:
                    continue
    except TimeoutError:
        # The window closed while the stream was idle; that is "no record",
        # not a transport failure. The endpoint's 15-second keep-alive
        # normally ends the read first.
        return
    except (urllib.error.URLError, OSError) as error:
        raise ProbeError(f"GET /api/logs/events failed: {error}") from error


def wait_for_record(gateway, thread_id, timeout):
    """The turn's `Turn finished` record, or None once `timeout` runs out.

    A detached turn logs `phase=late` when the watcher ends it; a turn that
    fits the foreground wait logs `phase=foreground`. Either one means the
    turn is over, so the first record for the thread is the answer. The
    history snapshot is checked first because a turn that already ended
    before the stream opened is in it.
    """
    records = records_for_thread(log_messages(download_logs(gateway)), thread_id)
    if records:
        return records[0]
    for entry in stream_logs(gateway, timeout):
        record = parse_turn_finished(entry.get("message", ""))
        if record is not None and record["thread_id"] == thread_id:
            return record
    return None


def verdict(record, expected_steps):
    """The one-line reading of a record, and its exit code."""
    steps = record["tool_calls"]
    if record["budget_exhausted"]:
        reading = (
            f"EXHAUSTED: the turn used its whole {record['budget_secs']} s budget "
            f"after {steps} tool calls ({record['elapsed_ms']} ms, phase {record['phase']})"
        )
        return reading, 3
    if steps != expected_steps:
        reading = (
            f"STEP MISMATCH: {steps} of {expected_steps} requested tool calls, "
            f"finished inside {record['budget_secs']} s ({record['elapsed_ms']} ms, "
            f"phase {record['phase']}, outcome {record['outcome']})"
        )
        return reading, 4
    reading = (
        f"FITS: {steps} tool calls inside {record['budget_secs']} s "
        f"({record['elapsed_ms']} ms, phase {record['phase']}, outcome {record['outcome']})"
    )
    return reading, 0


def parse_args(argv):
    parser = argparse.ArgumentParser(
        description="Measure a known-step agent turn against its budget (WI#807).",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument("--tenant", help="derive the gateway URL and token from /home/<tenant>")
    parser.add_argument("--base-url", help="gateway base URL, e.g. http://127.0.0.1:10000")
    parser.add_argument("--token-file", help="file holding the gateway bearer token")
    parser.add_argument("--steps", type=int, default=DEFAULT_STEPS)
    parser.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT)
    return parser.parse_args(argv)


def resolve_target(args):
    base_url, token = args.base_url, None
    if args.tenant:
        tenant_url, token = tenant_gateway(args.tenant)
        base_url = base_url or tenant_url
    if args.token_file:
        try:
            with open(args.token_file, encoding="utf-8") as handle:
                token = handle.read().strip()
        except OSError as error:
            raise ProbeError(f"cannot read {args.token_file}: {error}") from error
    if not base_url or not token:
        raise ProbeError("need --tenant, or both --base-url and --token-file")
    if args.steps < 1:
        raise ProbeError("--steps must be at least 1")
    return base_url, token


def main(argv=None):
    args = parse_args(argv)
    try:
        base_url, token = resolve_target(args)
        gateway = Gateway(base_url, token)
        require_contract(gateway)
        thread_id = create_thread(gateway)
        message_id = send_bounded_task(gateway, thread_id, args.steps)
        print(
            f"probe: thread {thread_id} message {message_id}; "
            f"waiting up to {args.timeout:.0f} s for {args.steps} tool calls"
        )
        record = wait_for_record(gateway, thread_id, args.timeout)
    except ProbeError as error:
        print(f"probe: {error}", file=sys.stderr)
        return 1
    if record is None:
        print(
            f"probe: no 'Turn finished' record for thread {thread_id} within "
            f"{args.timeout:.0f} s",
            file=sys.stderr,
        )
        return 2
    print(json.dumps(record, sort_keys=True))
    reading, code = verdict(record, args.steps)
    print(f"probe: {reading}")
    return code


if __name__ == "__main__":
    sys.exit(main())
