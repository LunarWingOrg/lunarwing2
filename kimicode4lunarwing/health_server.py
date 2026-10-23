#!/usr/bin/env python3
"""
health_server.py — Lightweight HTTP health-check endpoint (port 8443).

Endpoints:
  GET /health  →  200 {"status":"ok", ...}
  GET /ready   →  200 {"ready": true}  or  503 {"ready": false}
  GET /        →  redirect to /health
"""

import argparse
import json
import os
import sys
import time
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, HTTPServer

START_TIME = time.time()
WS_STATE_FILE = os.environ.get("WS_STATE_FILE", "/tmp/lunarwing_ws_state.json")
# The runner writes a heartbeat every 10 s; three missed beats is dead.
HEARTBEAT_MAX_AGE_SECONDS = 30.0
# NL-6 (!1873 run 200): a stamp further in the future than this clock skew
# is not a heartbeat (a forged or corrupt state file would otherwise keep
# /ready green long after the runner died).
HEARTBEAT_MAX_FUTURE_SKEW_SECONDS = 5.0

def _uptime() -> float:
    return round(time.time() - START_TIME, 2)

def _build_health() -> dict:
    return {
        "status": "ok",
        "uptime_seconds": _uptime(),
        "mode": os.environ.get("KIMI_MODE", "websocket"),
        "version": os.environ.get("KIMI_VERSION", "kimi-worker-1.0.0"),
        "worker_id": os.environ.get("LUNARWING_WORKER_ID", "worker-kimi-01"),
        # Upstream kimi-code release this image was built with
        # (Dockerfile KIMI_CODE_VERSION). "unknown" when running outside the image.
        "kimi_code_version": os.environ.get("KIMI_CODE_IMAGE_VERSION", "unknown"),
    }

def _load_ws_state() -> dict | None:
    try:
        with open(WS_STATE_FILE, encoding="utf-8") as handle:
            return json.load(handle)
    except FileNotFoundError:
        return None
    except json.JSONDecodeError:
        return {"ready": False, "error": "invalid websocket state file"}

def _load_state_file(path: str) -> dict | None:
    try:
        with open(path, encoding="utf-8") as handle:
            return json.load(handle)
    except FileNotFoundError:
        return None
    except ValueError:
        # Invalid JSON or invalid UTF-8 (NL-6: never an exception in /ready).
        return {"ready": False, "error": "invalid runner state file"}
    except OSError:
        return {"ready": False, "error": "runner state file unreadable"}

def _build_ready() -> tuple[dict, int]:
    mode = os.environ.get("KIMI_MODE", "websocket")
    body = {"ready": True, "uptime_seconds": _uptime(), "mode": mode}

    if mode != "websocket":
        return body, 200

    ws_state = _load_ws_state()
    if ws_state is None:
        body["ready"] = False
        body["websocket"] = {"ready": False, "error": "state unavailable"}
        return body, 503

    ready = bool(ws_state.get("ready"))
    body["ready"] = ready
    body["websocket"] = ws_state

    # NB-17 (!1861 run 195): the task runner runs unsupervised from the
    # health server's point of view unless its liveness is included. When
    # delegation is configured (the runner socket env is set), a missing or
    # not-ready runner state file makes /ready fail so the orchestrator
    # restarts the container instead of accepting tasks the runner will
    # refuse with connection errors.
    runner_socket = os.environ.get("LUNARWING_TASK_RUNNER_SOCKET", "")
    if runner_socket:
        runner_ready, runner_body = _runner_liveness(_load_runner_state())
        body["task_runner"] = runner_body
        if not runner_ready:
            body["ready"] = False
            status = 503
            return body, status

    status = 200 if ready else 503
    return body, status


def _parse_iso(ts: str) -> float:
    """Parse an ISO 8601 UTC timestamp to epoch seconds.

    ``datetime.fromisoformat`` has accepted the trailing ``Z`` since Python
    3.11; it also keeps any fractional seconds that ``Date.toISOString()``
    emits (``2026-10-23T03:29:05.739Z``), which a strptime with a bare
    ``%S`` rejects. The result is timezone-aware, so the offset is applied
    explicitly instead of being reinterpreted as local time (NI-2).
    """
    parsed = datetime.fromisoformat(ts.strip().replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.timestamp()

def _heartbeat_age(ts: str) -> float | None:
    """Seconds since *ts* in UTC, or None when it cannot be parsed."""
    if not isinstance(ts, str):
        return None
    try:
        return time.time() - _parse_iso(ts)
    except (TypeError, ValueError, OverflowError):
        return None

def _runner_liveness(state: object) -> tuple[bool, dict]:
    """(ready, body) for the runner's heartbeat state file.

    NC-7: a stale heartbeat means the runner is dead. The runner writes every
    10 s; anything older than 30 s (three missed beats) is not ready, so a
    SIGKILLed runner, which leaves a ready:true file behind, is caught.
    NL-6 (!1873 run 200): the heartbeat is REQUIRED. A state file that is
    not a JSON object, is not ready:true, or whose heartbeat_at is missing,
    empty, not a string, unparseable or in the future (beyond a small clock
    skew) is not ready; before, a missing or empty stamp passed and a
    numeric one raised AttributeError.
    NL-8: the body carries liveness only (ready, heartbeat_at, error). /ready
    is served off-container, so the socket path and pid are not published.
    """
    if state is None:
        return False, {"ready": False, "error": "state unavailable"}
    if not isinstance(state, dict):
        return False, {"ready": False, "error": "invalid runner state file"}
    if state.get("error") and state.get("ready") is not True:
        return False, {"ready": False, "error": str(state.get("error"))[:128]}
    if state.get("ready") is not True:
        return False, {"ready": False, "error": "runner not ready"}
    heartbeat_at = state.get("heartbeat_at")
    if not isinstance(heartbeat_at, str) or not heartbeat_at.strip():
        return False, {"ready": False, "error": "heartbeat missing"}
    body = {"ready": False, "heartbeat_at": heartbeat_at[:64]}
    age = _heartbeat_age(heartbeat_at)
    if age is None:
        return False, {**body, "error": "heartbeat unparseable"}
    if age > HEARTBEAT_MAX_AGE_SECONDS:
        return False, {**body, "error": "heartbeat stale"}
    if age < -HEARTBEAT_MAX_FUTURE_SKEW_SECONDS:
        return False, {**body, "error": "heartbeat in the future"}
    return True, {**body, "ready": True}

def _load_runner_state():
    # NC-7: use the same default as the runner (task_runner.ts main()):
    # <socket dir>/task-runner-state.json. The entrypoint exports both
    # LUNARWING_TASK_RUNNER_SOCKET and LUNARWING_TASK_RUNNER_STATE_FILE before
    # starting the health server so this path resolves to the runner's state.
    path = os.environ.get("LUNARWING_TASK_RUNNER_STATE_FILE", "")
    if not path:
        socket_path = os.environ.get("LUNARWING_TASK_RUNNER_SOCKET", "")
        if socket_path:
            import os.path as _osp
            path = _osp.join(_osp.dirname(socket_path), "task-runner-state.json")
    if not path:
        return None
    return _load_state_file(path)

class HealthHandler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def _send_json(self, status: int, body: dict) -> None:
        payload = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        path = self.path.split("?")[0]

        if path in ("/", ""):
            self.send_response(302)
            self.send_header("Location", "/health")
            self.end_headers()

        elif path == "/health":
            self._send_json(200, _build_health())

        elif path == "/ready":
            body, status = _build_ready()
            self._send_json(status, body)

        else:
            self._send_json(404, {"error": "not found", "path": path})

def main() -> None:
    parser = argparse.ArgumentParser(description="Kimi Code worker health server")
    parser.add_argument("--port", type=int, default=8443, help="Listen port (default 8443)")
    parser.add_argument("--host", default="0.0.0.0", help="Bind address (default 0.0.0.0)")
    args = parser.parse_args()

    server = HTTPServer((args.host, args.port), HealthHandler)
    print(f"[health_server] listening on {args.host}:{args.port}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("[health_server] shutting down", flush=True)
        server.server_close()
        sys.exit(0)

if __name__ == "__main__":
    main()
