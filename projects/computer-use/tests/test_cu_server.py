"""MCP framing, lifecycle, tool listing and result shapes."""

import base64
import io
import json
import unittest

import cu_support  # noqa: F401  (sets sys.path)
from cu_support import Clock, FakeBackend, FakeSelector

from lunarwing_computer import server as server_module
from lunarwing_computer.server import MAX_RESPONSE_LINE, Server
from lunarwing_computer.tools import IMAGE_BASE64_BUDGET, Desktop, Options

READ_ONLY_TOOLS = {"desktop_status", "desktop_list_outputs", "desktop_screenshot", "desktop_connect",
                   "desktop_disconnect", "desktop_wait"}
INPUT_TOOLS = {"desktop_move", "desktop_click", "desktop_drag", "desktop_scroll", "desktop_key", "desktop_type_text"}


class FakeVision:
    def describe(self, image, width, height, prompt):
        return {"description": f"a {width}x{height} desktop"}


def make_server(backend=None, vision=None, options=None):
    backend = backend or FakeBackend()
    clock = Clock()
    desktop = Desktop(FakeSelector(backend), options or Options(), vision=vision, clock=clock, sleep=clock.sleep)
    return Server(desktop), backend, clock


def request(server, method, params=None, request_id=1):
    message = {"jsonrpc": "2.0", "id": request_id, "method": method}
    if params is not None:
        message["params"] = params
    return server.dispatch(message)


def initialized(**kwargs):
    server, backend, clock = make_server(**kwargs)
    request(server, "initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t"}})
    return server, backend, clock


def call(server, name, arguments=None):
    params = {"name": name}
    if arguments is not None:
        params["arguments"] = arguments
    return request(server, "tools/call", params)["result"]


def payload(result):
    return json.loads(result["content"][0]["text"])


class LifecycleTests(unittest.TestCase):
    def test_version_negotiation(self):
        for requested, expected in [("2024-11-05", "2024-11-05"), ("2025-03-26", "2025-03-26"),
                                    ("2025-06-18", "2025-06-18"), ("2099-01-01", "2025-03-26"),
                                    (None, "2025-03-26")]:
            with self.subTest(requested=requested):
                server, _backend, _clock = make_server()
                params = {"capabilities": {}}
                if requested is not None:
                    params["protocolVersion"] = requested
                result = request(server, "initialize", params)["result"]
                self.assertEqual(result["protocolVersion"], expected)
                self.assertIn("tools", result["capabilities"])
                self.assertEqual(result["serverInfo"]["name"], "lunarwing-computer")
                self.assertIn("untrusted", result["instructions"])

    def test_ping_works_before_initialize_but_tools_do_not(self):
        server, _backend, _clock = make_server()
        self.assertEqual(request(server, "ping")["result"], {})
        self.assertEqual(request(server, "tools/list")["error"]["code"], -32002)

    def test_unknown_method_and_unknown_tool(self):
        server, _backend, _clock = initialized()
        self.assertEqual(request(server, "resources/list")["error"]["code"], -32601)
        response = request(server, "tools/call", {"name": "desktop_format_disk"})
        self.assertEqual(response["error"]["code"], -32602)
        self.assertEqual(request(server, "tools/call", {"arguments": {}})["error"]["code"], -32602)

    def test_notifications_are_ignored(self):
        server, backend, _clock = initialized()
        for method in ("notifications/initialized", "notifications/cancelled", "notifications/whatever",
                       "tools/call"):
            message = {"jsonrpc": "2.0", "method": method, "params": {"name": "desktop_connect"}}
            self.assertIsNone(server.dispatch(message))
        self.assertFalse(backend.active)

    def test_invalid_envelopes(self):
        server, _backend, _clock = initialized()  # 2024-11-05: no batches
        self.assertEqual(server.dispatch([{"jsonrpc": "2.0", "id": 1, "method": "ping"}])["error"]["code"], -32600)
        self.assertEqual(server.dispatch({"id": 1, "method": "ping"})["error"]["code"], -32600)
        for bad_id in (True, 1.5, None, {"a": 1}):
            response = server.dispatch({"jsonrpc": "2.0", "id": bad_id, "method": "ping"})
            self.assertEqual(response["error"]["code"], -32600)
            self.assertIsNone(response["id"])
        response = server.dispatch({"jsonrpc": "2.0", "id": 4, "method": "tools/list", "params": [1]})
        self.assertEqual(response["error"]["code"], -32602)
        self.assertIsNone(server.dispatch({"jsonrpc": "2.0", "id": 9, "result": {}}))


class BatchTests(unittest.TestCase):
    """JSON-RPC batches exist only in MCP 2025-03-26, which requires receiving them."""

    def negotiated(self, version, **kwargs):
        server, backend, clock = make_server(**kwargs)
        params = {"capabilities": {}}
        if version is not None:
            params["protocolVersion"] = version
        agreed = request(server, "initialize", params)["result"]["protocolVersion"]
        return server, backend, agreed

    def test_batch_is_answered_under_2025_03_26_and_its_fallback(self):
        for requested in ("2025-03-26", "2025-11-25", None):
            with self.subTest(requested=requested):
                server, _backend, agreed = self.negotiated(requested)
                self.assertEqual(agreed, "2025-03-26")
                response = server.dispatch([{"jsonrpc": "2.0", "id": 2, "method": "ping"},
                                            {"jsonrpc": "2.0", "method": "notifications/initialized"}])
                self.assertEqual(response, [{"jsonrpc": "2.0", "id": 2, "result": {}}])

    def test_batch_is_rejected_under_revisions_without_batches(self):
        for requested in ("2024-11-05", "2025-06-18"):
            with self.subTest(requested=requested):
                server, _backend, agreed = self.negotiated(requested)
                self.assertEqual(agreed, requested)
                response = server.dispatch([{"jsonrpc": "2.0", "id": 2, "method": "ping"}])
                self.assertEqual(response["error"]["code"], -32600)
                self.assertIsNone(response["id"])
        server, _backend, _clock = make_server()
        self.assertEqual(server.dispatch([{"jsonrpc": "2.0", "id": 1, "method": "ping"}])["error"]["code"], -32600)

    def test_batch_edge_cases(self):
        server, backend, _agreed = self.negotiated("2025-03-26")
        self.assertIsNone(server.dispatch([{"jsonrpc": "2.0", "method": "notifications/initialized"}]))
        empty = server.dispatch([])
        self.assertEqual((empty["id"], empty["error"]["code"]), (None, -32600))
        response = server.dispatch([
            1, [], {"jsonrpc": "2.0", "id": 3, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}},
            {"jsonrpc": "2.0", "id": 4, "method": "resources/list"},
            {"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "desktop_connect"}},
            {"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "desktop_status"}},
        ])
        self.assertEqual([(r["id"], r.get("error", {}).get("code")) for r in response],
                         [(None, -32600), (None, -32600), (3, -32600), (4, -32601), (5, None), (6, None)])
        self.assertIn("initialize", response[2]["error"]["message"])
        self.assertEqual(server.protocol_version, "2025-03-26")  # the batched initialize was not run
        self.assertTrue(backend.active)
        self.assertTrue(payload(response[5]["result"])["connected"])  # desktop_status after desktop_connect

    def test_frame_guard_applies_between_batched_calls(self):
        server, backend, _agreed = self.negotiated("2025-03-26")
        call(server, "desktop_connect")
        frame_id = payload(call(server, "desktop_screenshot"))["frame_id"]
        click = {"name": "desktop_click", "arguments": {"frame_id": frame_id, "x": 1, "y": 1}}
        response = server.dispatch([{"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": click},
                                    {"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": click}])
        self.assertEqual([r["result"]["isError"] for r in response], [False, True])
        self.assertEqual([e[0] for e in backend.events], ["move", "button", "button"])

    def test_batch_answer_is_one_line_and_stays_under_the_line_limit(self):
        server, _backend, _agreed = self.negotiated("2025-03-26")
        big = {"jsonrpc": "2.0", "id": 1, "result": {"content": [{"type": "text", "text": "x" * 2_000_000}],
                                                     "isError": False}}
        small = {"jsonrpc": "2.0", "id": 2, "result": {}}
        line = server.encode([big, dict(big, id=3), small])
        self.assertLess(len(line), MAX_RESPONSE_LINE)
        self.assertEqual(line.count(b"\n"), 1)
        decoded = json.loads(line)
        self.assertEqual([r["id"] for r in decoded], [1, 3, 2])
        self.assertTrue(decoded[0]["result"]["isError"])  # the first big result gave way
        self.assertFalse(decoded[1]["result"]["isError"])
        self.assertEqual(decoded[2], small)

    def test_serve_writes_a_batch_answer_as_one_array_line(self):
        server, _backend, _clock = make_server()
        init = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}
        batch = [{"jsonrpc": "2.0", "id": 2, "method": "ping"}, {"jsonrpc": "2.0", "id": 3, "method": "tools/list"}]
        data = (json.dumps(init) + "\n" + json.dumps(batch) + "\n").encode()
        output = io.BytesIO()
        server.serve(io.BytesIO(data), output)
        lines = output.getvalue().split(b"\n")
        self.assertEqual(len(lines), 3)
        answer = json.loads(lines[1])
        self.assertIsInstance(answer, list)
        self.assertEqual([r["id"] for r in answer], [2, 3])
        self.assertIn("tools", answer[1]["result"])


class FramingTests(unittest.TestCase):
    def serve(self, data):
        server, backend, _clock = make_server()
        output = io.BytesIO()
        server.serve(io.BytesIO(data), output)
        lines = output.getvalue().split(b"\n")
        self.assertEqual(lines[-1], b"")
        return [json.loads(line) for line in lines[:-1]], backend, output.getvalue()

    def test_malformed_lines_get_parse_errors_and_serving_continues(self):
        init = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}})
        data = (b"{\n" + b'{"jsonrpc":"2.0","id":2,"method":"ping","x":NaN}\n' + b"\xff\xfe\n"
                + b"[" * 100000 + b"\n" + init.encode() + b"\n" + b"\n"
                + b'{"jsonrpc":"2.0","id":3,"method":"ping"}\n')
        responses, _backend, raw = self.serve(data)
        self.assertEqual([r.get("error", {}).get("code") for r in responses[:4]], [-32700] * 4)
        self.assertTrue(all(r["id"] is None for r in responses[:4]))
        self.assertEqual(responses[4]["id"], 1)
        self.assertEqual(responses[5], {"jsonrpc": "2.0", "id": 3, "result": {}})
        raw.decode("ascii")  # pure ASCII on the wire

    def test_oversize_line_is_rejected_without_parsing_its_tail(self):
        tail = b'{"jsonrpc":"2.0","id":7,"method":"ping"}'
        data = b" " * (server_module.MAX_REQUEST_LINE + 10) + tail + b"\n" + b'{"jsonrpc":"2.0","id":8,"method":"ping"}\n'
        responses, _backend, _raw = self.serve(data)
        self.assertEqual(responses[0]["error"]["code"], -32600)
        self.assertEqual([r["id"] for r in responses], [None, 8])

    def test_eof_releases_the_session(self):
        server, backend, _clock = initialized()
        call(server, "desktop_connect")
        self.assertTrue(backend.active)
        server.serve(io.BytesIO(b""), io.BytesIO())
        self.assertFalse(backend.active)

    def test_each_response_is_one_line(self):
        server, _backend, _clock = initialized()
        call(server, "desktop_connect")
        line = server.encode(request(server, "tools/call", {"name": "desktop_screenshot"}))
        self.assertEqual(line.count(b"\n"), 1)
        self.assertTrue(line.endswith(b"\n"))


class ToolListTests(unittest.TestCase):
    def tools(self, **kwargs):
        server, _backend, _clock = initialized(**kwargs)
        return {tool["name"]: tool for tool in request(server, "tools/list")["result"]["tools"]}

    def test_annotations_follow_the_approval_contract(self):
        tools = self.tools()
        self.assertEqual(set(tools), READ_ONLY_TOOLS | INPUT_TOOLS)
        for name in READ_ONLY_TOOLS:
            self.assertEqual(tools[name]["annotations"], {"readOnlyHint": True, "destructiveHint": False}, name)
        for name in INPUT_TOOLS:
            self.assertEqual(tools[name]["annotations"], {"readOnlyHint": False, "destructiveHint": True,
                                                          "idempotentHint": False, "openWorldHint": True}, name)
            self.assertIn("frame_id", tools[name]["inputSchema"]["required"])

    def test_schemas_are_closed_and_descriptions_warn_about_screen_text(self):
        def closed(schema, where):
            if schema.get("type") == "object":
                self.assertIs(schema.get("additionalProperties"), False, where)
                for key, sub in schema.get("properties", {}).items():
                    closed(sub, f"{where}.{key}")
            if schema.get("type") == "array":
                closed(schema["items"], where + "[]")

        tools = self.tools()
        for name, tool in tools.items():
            closed(tool["inputSchema"], name)
            self.assertTrue(name.startswith("desktop_"))
        self.assertIn("untrusted", tools["desktop_screenshot"]["description"])
        self.assertIn("new desktop_screenshot", tools["desktop_click"]["description"])

    def test_observe_is_listed_only_with_a_vision_config(self):
        self.assertNotIn("desktop_observe", self.tools())
        tools = self.tools(vision=FakeVision())
        self.assertEqual(tools["desktop_observe"]["annotations"], {"readOnlyHint": True, "destructiveHint": False})


class ResultShapeTests(unittest.TestCase):
    def test_screenshot_returns_text_and_spec_image_block(self):
        server, _backend, _clock = initialized()
        connected = call(server, "desktop_connect")
        self.assertFalse(connected["isError"])
        result = call(server, "desktop_screenshot")
        self.assertIs(result["isError"], False)
        text, image = result["content"]
        self.assertEqual(text["type"], "text")
        self.assertEqual(set(image), {"type", "data", "mimeType"})
        self.assertEqual(image["mimeType"], "image/png")
        self.assertTrue(base64.b64decode(image["data"]).startswith(b"\x89PNG"))
        info = json.loads(text["text"])
        self.assertEqual(len(info["frame_id"]), 32)
        self.assertEqual((info["width"], info["height"]), (1280, 960))
        self.assertEqual(info["output"], "DP-1")
        self.assertIn("untrusted", info["untrusted"])
        self.assertIn("0..1279", info["coordinates"])

    def test_failures_are_is_error_results_with_text(self):
        server, backend, _clock = initialized()
        result = call(server, "desktop_screenshot")
        self.assertIs(result["isError"], True)
        self.assertEqual(result["content"][0]["type"], "text")
        self.assertIn("desktop_connect", result["content"][0]["text"])
        call(server, "desktop_connect")
        for name, args in [("desktop_click", {}), ("desktop_click", {"frame_id": "0" * 32, "x": True, "y": 1}),
                           ("desktop_connect", {"url": "http://example.invalid"}),
                           ("desktop_key", {"frame_id": "0" * 32, "keys": []}),
                           ("desktop_wait", {"seconds": 11}), ("desktop_click", {"frame_id": "0" * 32, "x": 1, "y": 1})]:
            with self.subTest(name=name, args=args):
                result = call(server, name, args)
                self.assertIs(result["isError"], True)
                self.assertTrue(result["content"][0]["text"])
        self.assertEqual(backend.events, [])

    def test_input_results_are_text_only_and_ask_for_a_new_screenshot(self):
        server, backend, _clock = initialized()
        call(server, "desktop_connect")
        frame_id = payload(call(server, "desktop_screenshot"))["frame_id"]
        result = call(server, "desktop_click", {"frame_id": frame_id, "x": 10, "y": 10})
        self.assertFalse(result["isError"])
        self.assertEqual([block["type"] for block in result["content"]], ["text"])
        self.assertIn("new desktop_screenshot", result["content"][0]["text"])
        again = call(server, "desktop_click", {"frame_id": frame_id, "x": 10, "y": 10})
        self.assertTrue(again["isError"])
        self.assertIn("already used", again["content"][0]["text"])
        self.assertEqual([e[0] for e in backend.events], ["move", "button", "button"])

    def test_integral_floats_are_accepted_for_integers(self):
        server, backend, _clock = initialized()
        call(server, "desktop_connect")
        frame_id = payload(call(server, "desktop_screenshot"))["frame_id"]
        result = call(server, "desktop_click", {"frame_id": frame_id, "x": 10.0, "y": 20, "count": 2.0})
        self.assertFalse(result["isError"], result)
        self.assertEqual(sum(1 for e in backend.events if e == ("button", "left", True)), 2)

    def test_unexpected_backend_exception_closes_the_session(self):
        server, backend, _clock = initialized()
        call(server, "desktop_connect")
        frame_id = payload(call(server, "desktop_screenshot"))["frame_id"]

        def broken(*_args):
            raise RuntimeError("private detail")

        backend.button = broken
        result = call(server, "desktop_click", {"frame_id": frame_id, "x": 1, "y": 1})
        self.assertTrue(result["isError"])
        self.assertNotIn("private detail", json.dumps(result))
        self.assertFalse(backend.active)

    def test_wait_is_bounded_and_sleeps(self):
        server, _backend, clock = initialized()
        before = clock.now
        self.assertFalse(call(server, "desktop_wait", {"seconds": 2.5})["isError"])
        self.assertAlmostEqual(clock.now - before, 2.5)
        self.assertTrue(call(server, "desktop_wait", {"seconds": 0.05})["isError"])


class SizeBudgetTests(unittest.TestCase):
    def test_large_png_falls_back_to_jpeg_within_budget(self):
        server, backend, _clock = initialized(backend=FakeBackend(big_png=True))
        call(server, "desktop_connect")
        response = request(server, "tools/call", {"name": "desktop_screenshot"})
        result = response["result"]
        self.assertFalse(result["isError"])
        image = result["content"][1]
        self.assertEqual(image["mimeType"], "image/jpeg")
        self.assertLessEqual(len(image["data"]), IMAGE_BASE64_BUDGET)
        self.assertIn("note", payload(result))
        self.assertEqual([c[2] for c in backend.captures], ["png", "jpeg"])
        self.assertLess(len(server.encode(response)), MAX_RESPONSE_LINE)

    def test_explicit_jpeg_format(self):
        server, backend, _clock = initialized()
        call(server, "desktop_connect")
        result = call(server, "desktop_screenshot", {"format": "jpeg"})
        self.assertEqual(result["content"][1]["mimeType"], "image/jpeg")
        self.assertEqual(backend.captures[-1][2], "jpeg")

    def test_capture_that_never_fits_is_an_error_without_frame(self):
        server, backend, _clock = initialized(options=Options(image_budget=10))
        call(server, "desktop_connect")
        result = call(server, "desktop_screenshot")
        self.assertTrue(result["isError"])
        self.assertIn("too large", result["content"][0]["text"])
        self.assertEqual(len(backend.captures), 5)
        self.assertEqual(server.desktop.guard._latest, {})

    def test_oversized_line_is_replaced_by_an_error_result(self):
        server, _backend, _clock = initialized()
        huge = {"jsonrpc": "2.0", "id": 5, "result": {"content": [{"type": "text", "text": "x" * MAX_RESPONSE_LINE}],
                                                      "isError": False}}
        line = server.encode(huge)
        self.assertLess(len(line), 1000)
        decoded = json.loads(line)
        self.assertEqual(decoded["id"], 5)
        self.assertTrue(decoded["result"]["isError"])


if __name__ == "__main__":
    unittest.main()
