#!/usr/bin/env python3
"""Live LunarDepth protocol, exposure, and policy smoke checks."""

import argparse
import json
import os
import socket
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, build_opener


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, file_pointer, code, message, headers, new_url):
        return None


NO_REDIRECTS = build_opener(NoRedirect())


def completion(model: str, stream: bool):
    if stream:
        chunks = [
            {"id": "smoke", "object": "chat.completion.chunk", "created": 0, "model": model,
             "choices": [{"index": 0, "delta": {"role": "assistant", "content": "LunarDepth smoke OK"}, "finish_reason": None}]},
            {"id": "smoke", "object": "chat.completion.chunk", "created": 0, "model": model,
             "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
        ]
        return "".join(f"data: {json.dumps(chunk)}\n\n" for chunk in chunks) + "data: [DONE]\n\n", "text/event-stream"
    body = {"id": "smoke", "object": "chat.completion", "created": 0, "model": model,
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "LunarDepth smoke OK"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}
    return json.dumps(body), "application/json"


class FakeModelHandler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def do_POST(self):
        if self.path != "/openai/v1/chat/completions":
            self.send_error(404)
            return
        length = int(self.headers.get("content-length", "0"))
        if length <= 0 or length > 1024 * 1024:
            self.send_error(400)
            return
        request = json.loads(self.rfile.read(length))
        text, content_type = completion(str(request.get("model", "")), bool(request.get("stream")))
        payload = text.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def serve_fake_model(port: int) -> None:
    server = ThreadingHTTPServer(("127.0.0.1", port), FakeModelHandler)
    server.serve_forever()


def loopback_url(url: str, allowed_schemes=("http", "ws")) -> None:
    parsed = urlsplit(url)
    if parsed.scheme not in allowed_schemes or parsed.hostname not in {"127.0.0.1", "localhost", "::1"}:
        raise ValueError("smoke endpoints must use loopback HTTP(S) or WS(S)")


def http_json(url: str) -> dict:
    loopback_url(url, ("http", "https"))
    with NO_REDIRECTS.open(url, timeout=5) as response:
        if response.status != 200:
            raise ValueError(f"probe returned HTTP {response.status}")
        return json.load(response)


def upgrade_status(url: str, token: str, subprotocol: str) -> int:
    loopback_url(url, ("ws", "wss"))
    parsed = urlsplit(url)
    port = parsed.port or (443 if parsed.scheme == "wss" else 80)
    with socket.create_connection((parsed.hostname, port), timeout=5) as connection:
        connection.settimeout(5)
        path = parsed.path + (f"?{parsed.query}" if parsed.query else "")
        request = (
            f"GET {path} HTTP/1.1\r\nHost: {parsed.hostname}:{port}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: dG9rZW4=\r\nSec-WebSocket-Version: 13\r\n"
            f"Sec-WebSocket-Protocol: {subprotocol}\r\nAuthorization: Bearer {token}\r\n\r\n"
        )
        connection.sendall(request.encode("ascii"))
        response = connection.recv(4096).decode("latin1")
    if not response.startswith("HTTP/1.1 "):
        raise ValueError("invalid upgrade response")
    return int(response.split(" ", 2)[1])


def run_node_smoke(script: Path, url: str, token: str, task: bool) -> None:
    env = dict(os.environ)
    env.update({"SMOKE_WS_URL": url, "SMOKE_TOKEN": token, "SMOKE_TASK": "1" if task else "0"})
    subprocess.run(["/usr/local/bin/node", str(script)], env=env, check=True, timeout=75)


def listeners(role: str) -> None:
    expected = {8443, 9090} if role in {"bridge", "combined"} else set()
    findings = []
    for name in ("tcp", "tcp6"):
        path = Path("/proc/net") / name
        if not path.exists():
            continue
        for line in path.read_text().splitlines()[1:]:
            fields = line.split()
            if fields[3] != "0A":
                continue
            address, port_hex = fields[1].rsplit(":", 1)
            port = int(port_hex, 16)
            if port == 19091:
                if (name == "tcp" and address != "0100007F") or (name == "tcp6" and address != "0" * 31 + "1"):
                    findings.append(f"task listener on non-loopback {name}:{port}")
                continue
            if port not in expected:
                findings.append(f"unexpected listener {name}:{port}")
    for name in ("udp", "udp6"):
        path = Path("/proc/net") / name
        if path.exists() and len(path.read_text().splitlines()) > 1:
            findings.append(f"unexpected UDP socket in {name}")
    if findings:
        raise ValueError("; ".join(findings))


def assert_fresh_policy(dsh_home: Path) -> None:
    files = sorted(dsh_home.glob("sessions/**/*.jsonl"), key=lambda path: path.stat().st_mtime, reverse=True)
    wanted = {
        "permission/preset": ("preset", "workspace-write"),
        "sandbox/mode": ("mode", "workspace-write"),
        "approval/policy": ("policy", "ask"),
    }
    for path in files:
        found = set()
        for line in path.read_text(encoding="utf-8").splitlines():
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if not isinstance(event, dict):
                continue
            event_type = event.get("type")
            data = event.get("data", {})
            if event_type in wanted and isinstance(data, dict):
                key, value = wanted[event_type]
                if data.get(key) == value:
                    found.add(event_type)
        if found == set(wanted):
            return
    raise ValueError("no fresh task session has the required permission/sandbox/approval events")


def remote(args) -> None:
    script = Path(__file__).with_name("smoke_test.js")
    hub_token = os.environ["SMOKE_HUB_TOKEN"]
    local_token = os.environ["SMOKE_LOCAL_TOKEN"]
    bridge_url = os.environ.get("SMOKE_BRIDGE_READY_URL", "http://127.0.0.1:8443/ready")
    task_url = os.environ.get("SMOKE_TASK_READY_URL", "http://127.0.0.1:19091/ready")
    if http_json(bridge_url).get("ready") is not True or http_json(task_url).get("ready") is not True:
        raise ValueError("worker is not ready")
    if http_json(os.environ.get("SMOKE_TASK_HEALTH_URL", "http://127.0.0.1:19091/health")).get("version") != "1":
        raise ValueError("task health does not report protocol version 1")
    if upgrade_status("ws://127.0.0.1:9090/ws/agent", "wrong", "lunarwing-agent-v1") != 401:
        raise ValueError("bridge accepted a wrong token")
    if upgrade_status("ws://127.0.0.1:19091/ws/agent", "wrong", "lunarwing-agent-v1") != 401:
        raise ValueError("harness accepted a wrong token")
    if upgrade_status("ws://127.0.0.1:9090/ws/agent", hub_token, "ironclaw-agent-v1") != 400:
        raise ValueError("bridge accepted the legacy subprotocol")
    if upgrade_status("ws://127.0.0.1:19091/ws/agent", local_token, "ironclaw-agent-v1") != 400:
        raise ValueError("harness accepted the legacy subprotocol")
    run_node_smoke(script, "ws://127.0.0.1:19091/ws/agent", local_token, False)
    run_node_smoke(script, "ws://127.0.0.1:9090/ws/agent", hub_token, True)
    listeners(os.environ.get("SMOKE_LISTENER_ROLE", "combined"))
    assert_fresh_policy(Path(os.environ.get("DSH_HOME", "/home/lunardepth/.dsh")))
    if os.environ.get("SMOKE_RUN_SANDBOX") == "1":
        subprocess.run(["/usr/local/bin/node", str(Path(__file__).with_name("sandbox_smoke.js"))], check=True, timeout=60)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--profile", choices=("remote", "fake-model"), default="remote")
    parser.add_argument("--port", type=int, default=18080)
    args = parser.parse_args()
    if args.profile == "fake-model":
        serve_fake_model(args.port)
    else:
        remote(args)


if __name__ == "__main__":
    try:
        main()
    except (KeyError, OSError, ValueError, subprocess.SubprocessError, json.JSONDecodeError) as error:
        print(f"lunardepth-runtime-smoke: {error}", file=sys.stderr)
        sys.exit(1)
