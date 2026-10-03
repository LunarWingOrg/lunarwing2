#!/usr/bin/env python3
"""Exercise a real TensorZero/Tensormoon gateway with a local mock provider.

No database, external inference service, or provider credentials are required.
The gateway binary must already be built; this script does not build or install it.
"""

import argparse
import contextlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


TOOL = {
    "type": "function",
    "function": {
        "name": "echo",
        "description": "Echo the supplied text",
        "parameters": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
            "additionalProperties": False,
        },
    },
}
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def require(condition, message):
    if not condition:
        raise AssertionError(message)


class MockProvider(BaseHTTPRequestHandler):
    requests = []

    def log_message(self, *_args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.requests.append((self.path, body))
        if self.path != "/v1/chat/completions":
            self.send_error(404)
            return
        completed_tool = any(m["role"] == "tool" for m in body["messages"])
        call_tool = bool(body.get("tools")) and not completed_tool
        text = "tool completed" if completed_tool else "hello from mock"
        tool_call = {
            "id": "call_smoke_echo",
            "type": "function",
            "function": {"name": "echo", "arguments": '{"text":"hello"}'},
        }
        finish = "tool_calls" if call_tool else "stop"
        common = {"id": "chatcmpl-smoke", "created": 1, "model": "mock-model"}
        usage = {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        self.send_response(200)
        if body.get("stream"):
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            deltas = [{"role": "assistant"}]
            if call_tool:
                deltas += [
                    {"tool_calls": [{"index": 0, **tool_call,
                                     "function": {"name": "echo", "arguments": '{"text":'}}]},
                    {"tool_calls": [{"index": 0, "function": {"arguments": '"hello"}'}}]},
                ]
            else:
                deltas += [{"content": word} for word in [text[:6], text[6:]]]
            chunks = [
                {**common, "object": "chat.completion.chunk", "choices": [
                    {"index": 0, "delta": delta, "finish_reason": None}
                ]} for delta in deltas
            ]
            chunks.append({**common, "object": "chat.completion.chunk", "choices": [
                {"index": 0, "delta": {}, "finish_reason": finish}
            ], "usage": usage})
            for chunk in chunks:
                self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
                self.wfile.flush()
            self.wfile.write(b"data: [DONE]\n\n")
        else:
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            message = {"role": "assistant", "content": None if call_tool else text}
            if call_tool:
                message["tool_calls"] = [tool_call]
            response = {**common, "object": "chat.completion", "usage": usage,
                        "choices": [{"index": 0, "message": message, "finish_reason": finish}]}
            self.wfile.write(json.dumps(response).encode())


def post(base, payload):
    request = urllib.request.Request(
        base + "/openai/v1/chat/completions", json.dumps(payload).encode(),
        {"Content-Type": "application/json", "Authorization": "Bearer smoke-only"},
    )
    with OPENER.open(request, timeout=20) as response:
        return response.read().decode()


def check_usage(response):
    require(response["usage"]["prompt_tokens"] == 10, "prompt usage was lost")
    require(response["usage"]["completion_tokens"] == 5, "completion usage was lost")


def check_stream(raw, tool):
    events = [line[6:] for line in raw.splitlines() if line.startswith("data: ")]
    require(events and events[-1] == "[DONE]", "stream missing terminal [DONE]")
    chunks = [json.loads(event) for event in events[:-1]]
    choices = [choice for chunk in chunks for choice in chunk.get("choices", [])]
    finish = [choice["finish_reason"] for choice in choices if choice.get("finish_reason")]
    require(finish == ["tool_calls" if tool else "stop"], f"unexpected finish reasons: {finish}")
    if tool:
        calls = [call for choice in choices for call in choice["delta"].get("tool_calls", [])]
        require(any(call.get("function", {}).get("name") == "echo" for call in calls),
                "stream lost tool name")
        arguments = "".join(call.get("function", {}).get("arguments", "") for call in calls)
        require(json.loads(arguments) == {"text": "hello"}, "stream lost fragmented arguments")
    else:
        text = "".join(choice["delta"].get("content") or "" for choice in choices)
        require(text == "hello from mock", f"unexpected stream text: {text!r}")
    usage = [chunk for chunk in chunks if chunk.get("usage")]
    require(usage, "stream missing usage")
    check_usage(usage[-1])


def exercise(base, prefix, response_prefix):
    payload = {"model": f"{prefix}::function_name::lunarwing",
               "messages": [{"role": "user", "content": "hello"}],
               f"{prefix}::tags": {"test": "lunarwing-compat"}}
    response = json.loads(post(base, payload))
    require(response["choices"][0]["message"]["content"] == "hello from mock", "wrong text")
    require(response["model"].startswith(f"{response_prefix}::"), "unexpected response namespace")
    check_usage(response)
    print(f"PASS {prefix}: nonstreaming text and usage", flush=True)

    for tool in (False, True):
        streaming = {**payload, "stream": True, "stream_options": {"include_usage": True}}
        if tool:
            streaming.update(tools=[TOOL], tool_choice="auto")
        check_stream(post(base, streaming), tool)
        print(f"PASS {prefix}: streaming {'tool arguments' if tool else 'text'} and usage", flush=True)

    with_tools = {**payload, "tools": [TOOL], "tool_choice": "auto"}
    response = json.loads(post(base, with_tools))
    message = response["choices"][0]["message"]
    call = message["tool_calls"][0]
    require(call["function"]["name"] == "echo", "nonstreaming tool name lost")
    require(json.loads(call["function"]["arguments"]) == {"text": "hello"}, "tool arguments lost")
    followup = {**with_tools, "messages": payload["messages"] + [message, {
        "role": "tool", "tool_call_id": call["id"], "content": "hello",
    }]}
    response = json.loads(post(base, followup))
    require(response["choices"][0]["message"]["content"] == "tool completed", "tool round-trip failed")
    require(MockProvider.requests[-1][1]["messages"][-1]["tool_call_id"] == call["id"],
            "provider received a different tool call ID")
    print(f"PASS {prefix}: nonstreaming tool-call/result round-trip", flush=True)

    count = len(MockProvider.requests)
    try:
        post(base, {**payload, "model": f"{prefix}::function_name::missing"})
    except urllib.error.HTTPError as error:
        require(400 <= error.code < 500, f"missing function returned {error.code}")
    else:
        raise AssertionError("missing function unexpectedly succeeded")
    require(len(MockProvider.requests) == count, "missing function reached provider")
    print(f"PASS {prefix}: invalid function rejected before inference", flush=True)


def run(binary, kind):
    with tempfile.TemporaryDirectory(prefix="gateway-compat-") as temporary:
        root = Path(temporary)
        provider = ThreadingHTTPServer(("127.0.0.1", 0), MockProvider)
        thread = threading.Thread(target=provider.serve_forever, daemon=True)
        thread.start()
        with contextlib.closing(socket.socket()) as sock:
            sock.bind(("127.0.0.1", 0))
            gateway_port = sock.getsockname()[1]
        config = root / "gateway.toml"
        config.write_text(f'''[gateway]
bind_address = "127.0.0.1:{gateway_port}"
observability.enabled = false

[models.mock]
routing = ["local"]
[models.mock.providers.local]
type = "openai"
api_base = "http://127.0.0.1:{provider.server_port}/v1/"
model_name = "mock-model"
api_key_location = "none"

[functions.lunarwing]
type = "chat"
[functions.lunarwing.variants.mock]
type = "chat_completion"
model = "mock"
''')
        try:
            with (root / "gateway.log").open("w+") as log:
                process = subprocess.Popen(
                    [str(binary), "--config-file", str(config)], stdout=log, stderr=log,
                    env={"PATH": os.defpath, "HOME": str(root), "RUST_LOG": "warn"},
                )
                try:
                    base = f"http://127.0.0.1:{gateway_port}"
                    deadline = time.monotonic() + 60
                    while True:
                        require(process.poll() is None, "gateway exited during startup")
                        try:
                            with OPENER.open(base + "/health", timeout=1) as response:
                                require(response.status == 200, "gateway unhealthy")
                            break
                        except (urllib.error.URLError, TimeoutError):
                            require(time.monotonic() < deadline, "gateway readiness timed out")
                            time.sleep(0.1)
                    print(f"PASS {kind}: startup and health without a database", flush=True)
                    for prefix in (["tensorzero", "tensormoon"] if kind == "tensormoon" else [kind]):
                        exercise(base, prefix, kind)
                    require(all(path == "/v1/chat/completions" for path, _ in MockProvider.requests),
                            "unexpected upstream request path")
                    require(all(body["model"] == "mock-model" for _, body in MockProvider.requests),
                            "function routing did not select the mock model")
                except Exception:
                    log.flush()
                    log.seek(0)
                    print(log.read()[-12000:])
                    raise
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
        finally:
            provider.shutdown()
            provider.server_close()
            thread.join(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gateway-binary", type=Path, required=True)
    parser.add_argument("--gateway-kind", choices=["tensorzero", "tensormoon"], required=True)
    args = parser.parse_args()
    run(args.gateway_binary.resolve(strict=True), args.gateway_kind)


if __name__ == "__main__":
    main()
