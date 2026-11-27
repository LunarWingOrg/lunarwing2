"""LunarVision text fallback (desktop_observe) without any network access."""

import io
import json
import os
import tempfile
import unittest
from unittest.mock import patch

import cu_support  # noqa: F401
from cu_support import Clock, FakeBackend, FakeSelector

from lunarwing_computer.control import DesktopError
from lunarwing_computer.tools import Desktop, Options
from lunarwing_computer.vision import NoRedirect, Vision

FRESH = {"mode_used": "describe", "ocr": {"full_text": "", "blocks": [], "avg_confidence": 0},
         "vision": {"description": "A text editor; Save button at (40, 12)", "prompt_answer": "A text editor; Save button at (40, 12)"},
         "meta": {}}


class VisionTests(unittest.TestCase):
    def test_endpoint_rules(self):
        for url in ("http://example.com", "https://user:pw@example.com", "https://example.com/path",
                    "https://example.com?x=1", "file:///etc/passwd", "ftp://example.com"):
            with self.subTest(url=url), self.assertRaises(DesktopError):
                Vision(url, "test-token")
        self.assertEqual(Vision("http://127.0.0.1:8188", "test-token").url, "http://127.0.0.1:8188/v1/vision/analyze")
        self.assertEqual(Vision("https://vision.example/", "test-token").url, "https://vision.example/v1/vision/analyze")
        with self.assertRaises(DesktopError):
            Vision("https://vision.example", "a\nb")

    def test_redirects_are_refused(self):
        with self.assertRaises(DesktopError):
            NoRedirect().redirect_request(None, None, 302, "", {}, "https://elsewhere.example")

    def test_config_file_must_be_private(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "config.json")
            with open(path, "w") as handle:
                json.dump({"vision_url": "https://vision.example", "vision_token": "test-token"}, handle)
            os.chmod(path, 0o644)
            with self.assertRaisesRegex(DesktopError, "0600"):
                Vision.from_config(path)
            os.chmod(path, 0o600)
            self.assertEqual(Vision.from_config(path).url, "https://vision.example/v1/vision/analyze")
            with open(path, "w") as handle:
                json.dump({"vision_url": "https://vision.example", "vision_token": "t", "extra": 1}, handle)
            with self.assertRaisesRegex(DesktopError, "exactly"):
                Vision.from_config(path)

    def test_fresh_and_cached_responses_have_the_same_result(self):
        vision = Vision("https://vision.example", "test-token")
        for body in (FRESH, {"cached": True, "response": FRESH}):
            with self.subTest(cached="cached" in body):
                with patch.object(vision.opener, "open", return_value=io.BytesIO(json.dumps(body).encode())) as opened:
                    result = vision.describe(b"\x89PNG", 800, 600, "Find the Save button")
                self.assertEqual(result, {"description": FRESH["vision"]["description"]})
                request = opened.call_args.args[0]
                payload = json.loads(request.data)
                self.assertEqual(payload["mode"], "describe")
                self.assertIn("800 by 600", payload["prompt"])
                self.assertEqual(request.get_header("Authorization"), "Bearer test-token")

    def test_failures(self):
        vision = Vision("https://vision.example", "test-token")
        for body in (b"{}", json.dumps({"vision": None}).encode(), b" " * (1024 * 1024 + 1), b"not json"):
            with self.subTest(body=body[:20]):
                with patch.object(vision.opener, "open", return_value=io.BytesIO(body)):
                    with self.assertRaises(DesktopError):
                        vision.describe(b"", 1, 1, "Describe")
        with patch.object(vision.opener, "open", side_effect=OSError("refused")):
            with self.assertRaisesRegex(DesktopError, "request failed"):
                vision.describe(b"", 1, 1, "Describe")


class ObserveToolTests(unittest.TestCase):
    def test_observe_returns_text_and_a_usable_frame(self):
        class StubVision:
            def describe(self, image, width, height, prompt):
                self.seen = (image[:4], width, height, prompt)
                return {"description": "button at (10, 10)"}

        vision = StubVision()
        backend = FakeBackend()
        clock = Clock()
        desktop = Desktop(FakeSelector(backend), Options(), vision=vision, clock=clock, sleep=clock.sleep)
        desktop.call("desktop_connect", {})
        result = desktop.call("desktop_observe", {"prompt": "Where is OK?"})
        self.assertFalse(result["isError"], result)
        self.assertEqual([b["type"] for b in result["content"]], ["text"])
        info = json.loads(result["content"][0]["text"])
        self.assertEqual(info["observation"], {"description": "button at (10, 10)"})
        self.assertEqual(vision.seen, (b"\x89PNG", 1280, 960, "Where is OK?"))
        clicked = desktop.call("desktop_click", {"frame_id": info["frame_id"], "x": 10, "y": 10})
        self.assertFalse(clicked["isError"])

    def test_observe_is_unknown_without_config(self):
        from lunarwing_computer.tools import UnknownTool

        desktop = Desktop(FakeSelector(FakeBackend()), Options())
        with self.assertRaises(UnknownTool):
            desktop.call("desktop_observe", {})


if __name__ == "__main__":
    unittest.main()
