#!/usr/bin/env python3
"""Non-sensitive bridge health and composite readiness."""

import argparse
import json
import os
import time
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, HTTPServer

START_TIME = time.time()
WS_STATE_FILE = os.environ.get("WS_STATE_FILE", "/tmp/lunardepth_ws_state.json")
RUNNER_STATE_FILE = os.environ.get("LUNARDEPTH_RUNNER_STATE_FILE", "/run/lunarwing-shared/runner/task-runner-state.json")
GATE_STATE_FILE = os.environ.get("LUNARDEPTH_GATE_RESULT", "/run/lunarwing-shared/runner/lunardepth-ready.json")
MAX_HEARTBEAT_AGE = 30.0
MAX_FUTURE_SKEW = 5.0


def load_json(path: str) -> dict | None:
    try:
        with open(path, encoding="utf-8") as handle:
            value = json.load(handle)
        return value if isinstance(value, dict) else None
    except FileNotFoundError:
        return None
    except (OSError, ValueError):
        return None


def heartbeat_fresh(state: dict | None) -> bool:
    if not state or state.get("ready") is not True or not isinstance(state.get("heartbeat_at"), str):
        return False
    try:
        stamp = datetime.fromisoformat(state["heartbeat_at"].replace("Z", "+00:00"))
        if stamp.tzinfo is None:
            stamp = stamp.replace(tzinfo=timezone.utc)
        age = time.time() - stamp.timestamp()
        return -MAX_FUTURE_SKEW <= age <= MAX_HEARTBEAT_AGE
    except (TypeError, ValueError, OverflowError):
        return False


def health() -> dict:
    return {
        "status": "ok",
        "uptime_seconds": round(time.time() - START_TIME, 2),
        "mode": os.environ.get("LUNARDEPTH_MODE", "websocket"),
        "version": os.environ.get("LUNARDEPTH_VERSION", "lunardepth-worker-1.0.0"),
        "worker_id": os.environ.get("LUNARWING_WORKER_ID", "lunardepth"),
        "source_revision": os.environ.get("LUNARDEPTH_IMAGE_REVISION", "unknown"),
    }


def readiness() -> tuple[dict, int]:
    websocket = load_json(WS_STATE_FILE)
    runner = load_json(RUNNER_STATE_FILE)
    gate = load_json(GATE_STATE_FILE)
    websocket_ready = bool(websocket and websocket.get("ready") is True and websocket.get("listening") is True)
    runner_ready = heartbeat_fresh(runner)
    gate_ready = heartbeat_fresh(gate) and all(
        gate.get(key) is True for key in ("composition", "sandbox", "task")
    ) if gate else False
    ready = websocket_ready and runner_ready and gate_ready
    return {
        "ready": ready,
        "uptime_seconds": round(time.time() - START_TIME, 2),
        "websocket": {"ready": websocket_ready},
        "task_runner": {"ready": runner_ready},
        "task_gate": {"ready": gate_ready},
    }, 200 if ready else 503


class HealthHandler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def send_json(self, status: int, body: dict) -> None:
        payload = json.dumps(body).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        path = self.path.split("?")[0]
        if path in ("", "/"):
            self.send_response(302)
            self.send_header("Location", "/health")
            self.end_headers()
        elif path == "/health":
            self.send_json(200, health())
        elif path == "/ready":
            body, status = readiness()
            self.send_json(status, body)
        else:
            self.send_json(404, {"error": "not found"})


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8443)
    parser.add_argument("--host", default="0.0.0.0")
    args = parser.parse_args()
    server = HTTPServer((args.host, args.port), HealthHandler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        server.server_close()


if __name__ == "__main__":
    main()
