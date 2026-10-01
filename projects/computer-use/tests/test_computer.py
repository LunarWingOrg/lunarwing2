import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from lunarwing_computer.control import Desktop, DesktopError, Frame
from lunarwing_computer.server import Server, TOOLS
from lunarwing_computer.vision import NoRedirect, Vision


class FakePortal:
    def __init__(self):
        self.active = False
        self.events = []
        self.revoked = False

    def pump(self):
        if self.revoked:
            self.close()

    def start(self):
        self.active = True

    def close(self):
        self.active = False

    def capture(self):
        if not self.active:
            raise DesktopError("Not connected")
        return Frame(b"fake-png", 800, 600, 1600, 1200)

    def move(self, x, y):
        self.events.append(("move", x, y))

    def button(self, name, pressed):
        self.events.append(("button", name, pressed))

    def key(self, symbol, pressed):
        self.events.append(("key", symbol, pressed))

    def scroll(self, axis, steps):
        self.events.append(("scroll", axis, steps))


class FakeVision:
    def describe(self, frame, prompt):
        return {"description": f"A desktop: {frame.width}x{frame.height}"}


class DesktopTests(unittest.TestCase):
    def setUp(self):
        self.portal = FakePortal()
        self.now = 100
        self.desktop = Desktop(self.portal, FakeVision(), clock=lambda: self.now)
        self.desktop.connect()

    def frame_id(self):
        return self.desktop.observe()["frame_id"]

    def test_scaled_coordinates_and_consumed_frame(self):
        frame_id = self.frame_id()
        self.desktop.click(frame_id, 400, 300)
        self.assertEqual(self.portal.events, [
            ("move", 800, 600), ("button", "left", True), ("button", "left", False),
        ])
        with self.assertRaises(DesktopError):
            self.desktop.click(frame_id, 400, 300)
        self.assertEqual(len(self.portal.events), 3)

    def test_stale_unknown_and_revoked_frames_never_send_input(self):
        frame_id = self.frame_id()
        for invalid in ("wrong", "", None):
            with self.assertRaises(DesktopError):
                self.desktop.click(invalid, 10, 10)
        self.now += 61
        with self.assertRaises(DesktopError):
            self.desktop.click(frame_id, 10, 10)
        frame_id = self.frame_id()
        self.portal.revoked = True
        with self.assertRaises(DesktopError):
            self.desktop.click(frame_id, 10, 10)
        self.assertEqual(self.portal.events, [])

    def test_bad_coordinates_never_move(self):
        frame_id = self.frame_id()
        for x, y in [(-1, 0), (800, 0), (0, 600), (float("nan"), 0),
                     (0, float("inf")), (True, 0), ("0", 0)]:
            with self.subTest(x=x, y=y), self.assertRaises(DesktopError):
                self.desktop.click(frame_id, x, y)
        self.assertEqual(self.portal.events, [])

    def test_chord_releases_in_reverse_order(self):
        self.desktop.key(self.frame_id(), ["Ctrl", "l"])
        self.assertEqual(self.portal.events, [
            ("key", 0xFFE3, True), ("key", ord("l"), True),
            ("key", ord("l"), False), ("key", 0xFFE3, False),
        ])

    def test_interrupted_chord_releases_keys(self):
        original = self.portal.key

        def fail(symbol, pressed):
            original(symbol, pressed)
            if symbol == ord("l") and pressed:
                raise DesktopError("Interrupted")

        self.portal.key = fail
        with self.assertRaises(DesktopError):
            self.desktop.key(self.frame_id(), ["Ctrl", "l"])
        self.assertEqual(self.portal.events[-2:], [("key", ord("l"), False), ("key", 0xFFE3, False)])

    def test_text_and_chord_validated_before_any_input(self):
        frame_id = self.frame_id()
        for text in ("abc\x00", "a" * 257, ""):
            with self.assertRaises(DesktopError):
                self.desktop.type_text(frame_id, text)
        for keys in (["Ctrl", "unknown"], ["Ctrl", "Ctrl"], [], [1]):
            with self.assertRaises(DesktopError):
                self.desktop.key(frame_id, keys)
        self.assertEqual(self.portal.events, [])

    def test_unicode_text_and_newlines(self):
        self.desktop.type_text(self.frame_id(), "Aλ\n")
        self.assertEqual(self.portal.events, [
            ("key", 65, True), ("key", 65, False),
            ("key", 0x010003BB, True), ("key", 0x010003BB, False),
            ("key", 0xFF0D, True), ("key", 0xFF0D, False),
        ])

    def test_failed_observation_invalidates_old_frame(self):
        old = self.frame_id()
        self.desktop.vision = None
        with self.assertRaises(DesktopError):
            self.desktop.observe()
        with self.assertRaises(DesktopError):
            self.desktop.click(old, 10, 10)

    def test_revocation_during_vision_discards_observation(self):
        def revoke(_frame, _prompt):
            self.portal.revoked = True
            return {"description": "old screenshot"}
        self.desktop.vision.describe = revoke
        with self.assertRaises(DesktopError):
            self.desktop.observe()
        self.assertIsNone(self.desktop.frame_id)

    def test_scroll_and_move(self):
        self.desktop.scroll(self.frame_id(), -2)
        self.desktop.move(self.frame_id(), 100, 50)
        self.assertEqual(self.portal.events, [("scroll", "vertical", -2), ("move", 200, 100)])

    def test_reconnect_discards_frame(self):
        old = self.frame_id()
        self.desktop.disconnect()
        self.desktop.connect()
        with self.assertRaises(DesktopError):
            self.desktop.click(old, 0, 0)


class ProtocolTests(unittest.TestCase):
    def setUp(self):
        self.portal = FakePortal()
        self.server = Server(Desktop(self.portal, FakeVision()))
        self.send("initialize", {"protocolVersion": "2024-11-05"})

    def send(self, method, params):
        return self.server.dispatch({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})

    def test_complete_mcp_observe_act_roundtrip(self):
        self.send("tools/call", {"name": "desktop_connect"})
        result = self.send("tools/call", {"name": "desktop_observe"})["result"]
        observation = json.loads(result["content"][0]["text"])
        self.assertFalse(result["isError"])
        self.assertEqual(observation["provenance"], "untrusted_desktop_content")
        args = {"frame_id": observation["frame_id"], "x": 25, "y": 30}
        clicked = self.send("tools/call", {"name": "desktop_click", "arguments": args})
        self.assertFalse(clicked["result"]["isError"])
        again = self.send("tools/call", {"name": "desktop_click", "arguments": args})
        self.assertTrue(again["result"]["isError"])
        self.assertEqual(len(self.portal.events), 3)

    def test_screenshot_standard_mcp_image(self):
        self.portal.start()
        result = self.send("tools/call", {"name": "desktop_screenshot"})["result"]
        self.assertEqual(result["content"][1], {"type": "image", "mimeType": "image/png", "data": "ZmFrZS1wbmc="})
        self.assertNotIn("image", json.loads(result["content"][0]["text"]))

    def test_notification_never_executes_action(self):
        result = self.server.dispatch({"jsonrpc": "2.0", "method": "tools/call",
                                       "params": {"name": "desktop_connect"}})
        self.assertIsNone(result)
        self.assertFalse(self.portal.active)

    def test_bad_arguments_return_tool_errors(self):
        for name, args in [("desktop_click", {}), ("desktop_connect", {"url": "http://evil"}),
                           ("desktop_observe", {"prompt": 3}), ("desktop_key", []),
                           ("desktop_click", {"frame_id": "a" * 32, "x": True, "y": 1})]:
            result = self.send("tools/call", {"name": name, "arguments": args})
            self.assertTrue(result["result"]["isError"])
        self.assertEqual(self.portal.events, [])

    def test_eof_closes_session(self):
        self.portal.start()
        self.server.serve(io.BytesIO(b''), io.StringIO())
        self.assertFalse(self.portal.active)

    def test_oversize_request_closes_without_executing_tail(self):
        self.portal.start()
        output = io.StringIO()
        self.server.serve(io.BytesIO(b' ' * 65537 + b'\n'), output)
        self.assertEqual(json.loads(output.getvalue())["error"]["code"], -32600)
        self.assertFalse(self.portal.active)

    def test_invalid_json_and_nan_are_protocol_errors(self):
        output = io.StringIO()
        self.server.serve(io.BytesIO(b'{\n{"jsonrpc":"2.0","id":NaN}\n'), output)
        self.assertEqual([json.loads(line)["error"]["code"] for line in output.getvalue().splitlines()],
                         [-32700, -32700])

    def test_input_tools_declare_side_effects(self):
        for definition in TOOLS:
            if definition["name"] in ("desktop_click", "desktop_key", "desktop_type_text", "desktop_scroll", "desktop_move"):
                self.assertTrue(definition["annotations"]["destructiveHint"])
                self.assertFalse(definition["annotations"]["readOnlyHint"])

    def test_backend_error_is_redacted_and_disconnects(self):
        self.portal.start()
        self.portal.capture = lambda: (_ for _ in ()).throw(RuntimeError("SECRET"))
        result = self.send("tools/call", {"name": "desktop_observe"})
        self.assertTrue(result["result"]["isError"])
        self.assertNotIn("SECRET", json.dumps(result))
        self.assertFalse(self.portal.active)


class VisionTests(unittest.TestCase):
    def test_endpoint_validation(self):
        for url in ("http://example.com", "https://user:password@example.com",
                    "https://example.com/path", "https://example.com?x=1", "file:///etc/passwd"):
            with self.subTest(url=url), self.assertRaises(DesktopError):
                Vision(url, "test-token")
        self.assertEqual(Vision("http://127.0.0.1:8088", "token").url,
                         "http://127.0.0.1:8088/v1/vision/analyze")

    def test_redirects_are_rejected(self):
        with self.assertRaises(DesktopError):
            NoRedirect().redirect_request(None, None, 302, "", {}, "https://elsewhere.test")

    def test_config_permissions(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.json"
            path.write_text(json.dumps({"vision_url": "https://vision.example", "vision_token": "token"}))
            path.chmod(0o644)
            with self.assertRaises(DesktopError):
                Vision.from_config(path)
            path.chmod(0o600)
            self.assertEqual(Vision.from_config(path).url, "https://vision.example/v1/vision/analyze")

    def test_lunarvision_payload_and_response_bounds(self):
        vision = Vision("https://vision.example", "private-token")
        body = json.dumps({"vision": {"description": "button at (10,20)"}}).encode()
        with patch.object(vision.opener, "open", return_value=io.BytesIO(body)) as opened:
            result = vision.describe(Frame(b"png", 800, 600, 1600, 1200), "Find the button")
        request = opened.call_args.args[0]
        payload = json.loads(request.data)
        self.assertEqual(payload["mode"], "describe")
        self.assertIn("800 by 600", payload["prompt"])
        self.assertEqual(request.get_header("Authorization"), "Bearer private-token")
        self.assertEqual(result, {"description": "button at (10,20)"})
        with patch.object(vision.opener, "open", return_value=io.BytesIO(b' ' * 32769)):
            with self.assertRaises(DesktopError):
                vision.describe(Frame(b"png", 1, 1, 1, 1), "Describe")


if __name__ == "__main__":
    unittest.main()
