"""Frame guard, coordinate mapping and key parsing (pure logic)."""

import json
import unittest

import cu_support  # noqa: F401
from cu_support import Clock, FakeBackend, FakeSelector

from lunarwing_computer.backends import Output
from lunarwing_computer.control import (
    DesktopError, FrameGuard, Rect, View, capture_scale, char_keysym, parse_chord, text_keysyms,
)
from lunarwing_computer.tools import Desktop, Options


def view(output="DP-1", width=800, height=600, rect=Rect(0, 0, 1600, 1200), generation=1):
    return View(output, width, height, rect, generation)


class FrameGuardTests(unittest.TestCase):
    def setUp(self):
        self.clock = Clock()
        self.guard = FrameGuard(self.clock)

    def test_single_use(self):
        frame = self.guard.issue(view())
        self.assertIs(self.guard.consume(frame.frame_id, 1), frame)
        with self.assertRaisesRegex(DesktopError, "already used"):
            self.guard.consume(frame.frame_id, 1)

    def test_expiry_with_injected_clock(self):
        frame = self.guard.issue(view())
        self.clock.now += 59.9
        self.assertEqual(self.guard.consume(frame.frame_id, 1).frame_id, frame.frame_id)
        frame = self.guard.issue(view())
        self.clock.now += 60.1
        with self.assertRaisesRegex(DesktopError, "expired"):
            self.guard.consume(frame.frame_id, 1)
        with self.assertRaisesRegex(DesktopError, "expired"):
            self.guard.consume(frame.frame_id, 1)

    def test_newer_screenshot_of_same_output_supersedes(self):
        first = self.guard.issue(view("DP-1"))
        other = self.guard.issue(view("HDMI-A-1"))
        second = self.guard.issue(view("DP-1"))
        with self.assertRaisesRegex(DesktopError, "newer screenshot"):
            self.guard.consume(first.frame_id, 1)
        # A refused frame sends no input, so the other frames stay usable.
        self.assertEqual(self.guard.consume(other.frame_id, 1).view.output, "HDMI-A-1")
        with self.assertRaisesRegex(DesktopError, "input action was sent after"):
            self.guard.consume(second.frame_id, 1)

    def test_an_input_retires_the_frames_of_every_output(self):
        dp = self.guard.issue(view("DP-1"))
        hdmi = self.guard.issue(view("HDMI-A-1"))
        self.assertIs(self.guard.consume(dp.frame_id, 1), dp)
        with self.assertRaisesRegex(DesktopError, "input action was sent after it was taken"):
            self.guard.consume(hdmi.frame_id, 1)
        with self.assertRaisesRegex(DesktopError, "already used"):
            self.guard.consume(dp.frame_id, 1)
        # The views survive, so a zoom region can still refer to the last images.
        self.assertIsNotNone(self.guard.latest_view("DP-1", 1))
        self.assertIsNotNone(self.guard.latest_view("HDMI-A-1", 1))
        fresh = self.guard.issue(view("HDMI-A-1"))
        self.assertIs(self.guard.consume(fresh.frame_id, 1), fresh)

    def test_retire_keeps_views_and_reset_drops_them(self):
        frame = self.guard.issue(view("DP-1"))
        self.guard.retire("desktop_wait ran after it was taken")
        with self.assertRaisesRegex(DesktopError, "desktop_wait"):
            self.guard.consume(frame.frame_id, 1)
        self.assertIsNotNone(self.guard.latest_view("DP-1", 1))
        self.guard.reset()
        self.assertIsNone(self.guard.latest_view("DP-1", 1))

    def test_generation_change_and_reset_invalidate(self):
        frame = self.guard.issue(view(generation=1))
        with self.assertRaisesRegex(DesktopError, "changed"):
            self.guard.consume(frame.frame_id, 2)
        frame = self.guard.issue(view())
        self.guard.reset("desktop_connect started a new session")
        with self.assertRaisesRegex(DesktopError, "new session"):
            self.guard.consume(frame.frame_id, 1)
        self.assertIsNone(self.guard.latest_view("DP-1", 1))

    def test_failed_check_keeps_the_frame(self):
        frame = self.guard.issue(view())

        def reject(_frame):
            raise DesktopError("bad coordinate")

        with self.assertRaisesRegex(DesktopError, "bad coordinate"):
            self.guard.consume(frame.frame_id, 1, reject)
        self.assertIs(self.guard.consume(frame.frame_id, 1), frame)

    def test_malformed_and_unknown_ids(self):
        self.guard.issue(view())
        for bad in (None, "", "abc", 12, "g" * 32):
            with self.assertRaises(DesktopError):
                self.guard.consume(bad, 1)
        with self.assertRaisesRegex(DesktopError, "unknown frame_id"):
            self.guard.consume("0" * 32, 1)


class MappingTests(unittest.TestCase):
    def test_points_are_validated_against_the_frame(self):
        v = view()
        for x, y in [(-1, 0), (800, 0), (0, 600), (True, 0), (1.5, 0), ("3", 0), (None, 0)]:
            with self.subTest(x=x, y=y), self.assertRaises(DesktopError):
                v.to_logical(x, y)
        self.assertEqual(v.to_logical(0, 0), (1.0, 1.0))
        self.assertEqual(v.to_logical(799, 599), (1599.0, 1199.0))

    def test_fractional_scale_2560x1600_at_1_5_captured_at_1280(self):
        # 2560x1600 physical at scale 1.5 is 1707x1067 logical (compositors round the logical size).
        output = Output("eDP-1", 0, 0, 1707, 1067, 2560 / 1707, 2560, 1600)
        scale = capture_scale(Rect(0, 0, 1707, 1067), 1280, output.scale)
        self.assertLess(scale, 0.75)
        self.assertLessEqual(int(1707 * scale), 1280)
        self.assertEqual((int(1707 * scale), int(1067 * scale)), (1279, 800))
        v = View("eDP-1", 1280, 800, Rect(0, 0, 1707, 1067), 1)
        x, y = v.to_logical(640, 400)
        self.assertAlmostEqual(x, 640.5 * 1707 / 1280)
        self.assertAlmostEqual(y, 400.5 * 1067 / 800)
        self.assertEqual((int(x), int(y)), (854, 534))

    def test_capture_never_exceeds_native_resolution(self):
        self.assertEqual(capture_scale(Rect(0, 0, 800, 600), 1280, 1.0), 1.0)
        self.assertEqual(capture_scale(Rect(0, 0, 200, 100), 1280, 2.0), 2.0)
        self.assertAlmostEqual(capture_scale(Rect(0, 0, 2560, 1440), 1280, 1.0), 0.5)

    def test_region_mapping_accounts_for_offset_and_zoom(self):
        full = View("DP-1", 1280, 720, Rect(0, 0, 2560, 1440), 1)
        rect = full.region_to_rect(640, 360, 320, 180, 2560, 1440)
        self.assertEqual(rect, Rect(1280, 720, 640, 360))
        zoomed = View("DP-1", 640, 360, rect, 1)
        self.assertEqual(zoomed.to_logical(0, 0), (1280.5, 720.5))
        self.assertEqual(zoomed.to_logical(639, 359), (1919.5, 1079.5))
        nested = zoomed.region_to_rect(320, 180, 10, 10, 2560, 1440)
        self.assertEqual(nested, Rect(1600, 900, 10, 10))
        for args in [(-1, 0, 5, 5), (0, 0, 0, 5), (1200, 0, 100, 5), (0, 0, 5.5, 5)]:
            with self.subTest(args=args), self.assertRaises(DesktopError):
                full.region_to_rect(*args, 2560, 1440)

    def test_region_rounds_outward_and_clamps(self):
        v = View("eDP-1", 1280, 800, Rect(0, 0, 1707, 1067), 1)
        rect = v.region_to_rect(1279, 799, 1, 1, 1707, 1067)
        self.assertEqual((rect.x + rect.width, rect.y + rect.height), (1707, 1067))
        self.assertGreaterEqual(rect.width, 1)


class DesktopFlowTests(unittest.TestCase):
    def setUp(self):
        self.clock = Clock()
        self.backend = FakeBackend(outputs=[
            Output("DP-1", 0, 0, 2560, 1440, 1.0, 2560, 1440),
            Output("HDMI-A-1", 2560, 0, 1280, 720, 1.5, 1920, 1080),
        ])
        self.desktop = Desktop(FakeSelector(self.backend), Options(), clock=self.clock, sleep=self.clock.sleep)
        self.desktop.call("desktop_connect", {})

    def shot(self, **args):
        result = self.desktop.call("desktop_screenshot", args)
        self.assertFalse(result["isError"], result)
        return json.loads(result["content"][0]["text"])

    def test_wrong_output_frame_and_outputs_change(self):
        first = self.shot()
        self.assertEqual(first["output"], "DP-1")
        hdmi = self.shot(output="HDMI-A-1")
        self.assertEqual((hdmi["width"], hdmi["height"]), (1280, 720))
        newer = self.shot()
        result = self.desktop.call("desktop_click", {"frame_id": first["frame_id"], "x": 1, "y": 1})
        self.assertTrue(result["isError"])
        self.assertIn("newer screenshot", result["content"][0]["text"])
        self.assertFalse(self.desktop.call("desktop_click", {"frame_id": hdmi["frame_id"], "x": 1, "y": 1})["isError"])
        fresh = self.shot()
        self.backend.set_outputs([Output("DP-1", 0, 0, 2560, 1440)])
        result = self.desktop.call("desktop_click", {"frame_id": fresh["frame_id"], "x": 1, "y": 1})
        self.assertTrue(result["isError"])
        self.assertIn("outputs or the desktop session changed", result["content"][0]["text"])
        self.assertTrue(self.desktop.call("desktop_screenshot", {"output": "HDMI-A-1"})["isError"])
        self.assertEqual([e[0] for e in self.backend.events], ["move", "button", "button"])

    def test_two_inputs_need_a_screenshot_in_between_even_across_outputs(self):
        dp = self.shot()
        hdmi = self.shot(output="HDMI-A-1")
        clicked = self.desktop.call("desktop_click", {"frame_id": dp["frame_id"], "x": 1, "y": 1})
        self.assertFalse(clicked["isError"], clicked)
        for name, args in [("desktop_type_text", {"text": "rm -rf ~\n"}), ("desktop_key", {"keys": ["Enter"]}),
                           ("desktop_click", {"x": 1, "y": 1})]:
            with self.subTest(tool=name):
                result = self.desktop.call(name, dict(args, frame_id=hdmi["frame_id"]))
                self.assertTrue(result["isError"])
                self.assertIn("input action was sent after it was taken", result["content"][0]["text"])
        self.assertEqual(self.backend.events, [("move", "DP-1", 3.0, 3.0), ("button", "left", True),
                                               ("button", "left", False)])
        # A screenshot taken after the click is accepted again.
        again = self.shot(output="HDMI-A-1")
        self.assertFalse(self.desktop.call("desktop_key", {"frame_id": again["frame_id"], "keys": ["Enter"]})["isError"])
        self.assertEqual(self.backend.events[-1], ("key", "enter", 1))

    def test_wait_retires_frames_but_a_region_can_still_refer_to_the_last_image(self):
        frame = self.shot()
        self.assertFalse(self.desktop.call("desktop_wait", {"seconds": 2})["isError"])
        result = self.desktop.call("desktop_click", {"frame_id": frame["frame_id"], "x": 1, "y": 1})
        self.assertTrue(result["isError"])
        self.assertIn("desktop_wait ran after it was taken", result["content"][0]["text"])
        self.assertEqual(self.backend.events, [])
        zoom = self.shot(region={"x": 0, "y": 0, "width": 100, "height": 100})
        self.assertEqual(zoom["logical_area"], {"x": 0, "y": 0, "width": 200, "height": 200})
        self.assertFalse(self.desktop.call("desktop_click", {"frame_id": zoom["frame_id"], "x": 1, "y": 1})["isError"])

    def test_out_of_range_coordinates_send_nothing_and_keep_the_frame(self):
        frame = self.shot()
        for x, y in [(1280, 0), (0, 720), (5000, 5000)]:
            result = self.desktop.call("desktop_click", {"frame_id": frame["frame_id"], "x": x, "y": y})
            self.assertTrue(result["isError"])
            self.assertIn("outside", result["content"][0]["text"])
        self.assertEqual(self.backend.events, [])
        result = self.desktop.call("desktop_click", {"frame_id": frame["frame_id"], "x": 1279, "y": 719})
        self.assertFalse(result["isError"])
        self.assertEqual(self.backend.events[0], ("move", "DP-1", 2559.0, 1439.0))

    def test_region_screenshot_maps_clicks_into_the_zoomed_area(self):
        self.shot()
        zoom = self.shot(region={"x": 640, "y": 360, "width": 320, "height": 180})
        self.assertEqual(zoom["logical_area"], {"x": 1280, "y": 720, "width": 640, "height": 360})
        self.assertTrue(zoom["zoomed"])
        self.assertEqual(self.backend.captures[-1][3], Rect(1280, 720, 640, 360))
        self.desktop.call("desktop_click", {"frame_id": zoom["frame_id"], "x": 0, "y": 0})
        self.assertEqual(self.backend.events[0], ("move", "DP-1", 1280.5, 720.5))

    def test_region_needs_a_previous_screenshot(self):
        result = self.desktop.call("desktop_screenshot", {"region": {"x": 0, "y": 0, "width": 10, "height": 10}})
        self.assertTrue(result["isError"])
        self.assertIn("full", result["content"][0]["text"])

    def test_connect_and_disconnect_invalidate_frames(self):
        frame = self.shot()
        self.desktop.call("desktop_connect", {})
        self.assertTrue(self.desktop.call("desktop_move", {"frame_id": frame["frame_id"], "x": 1, "y": 1})["isError"])
        frame = self.shot()
        self.desktop.call("desktop_disconnect", {})
        result = self.desktop.call("desktop_move", {"frame_id": frame["frame_id"], "x": 1, "y": 1})
        self.assertTrue(result["isError"])
        self.assertEqual(self.backend.events, [])

    def test_revoked_session_rejects_input(self):
        frame = self.shot()
        self.backend.revoke_on_pump = True
        result = self.desktop.call("desktop_click", {"frame_id": frame["frame_id"], "x": 1, "y": 1})
        self.assertTrue(result["isError"])
        self.assertIn("desktop_connect", result["content"][0]["text"])
        self.assertEqual(self.backend.events, [])

    def test_drag_scroll_key_and_text(self):
        frame = self.shot()
        self.desktop.call("desktop_drag", {"frame_id": frame["frame_id"], "from": {"x": 10, "y": 10}, "to": {"x": 20, "y": 10}})
        kinds = [e[0] for e in self.backend.events]
        self.assertEqual(kinds[:2], ["move", "button"])
        self.assertEqual(self.backend.events[-1], ("button", "left", False))
        self.assertEqual(self.backend.events[-2], ("move", "DP-1", 41.0, 21.0))
        self.backend.events.clear()
        frame = self.shot()
        self.assertTrue(self.desktop.call("desktop_scroll", {"frame_id": frame["frame_id"]})["isError"])
        self.assertTrue(self.desktop.call("desktop_scroll", {"frame_id": frame["frame_id"], "dy": 1, "x": 3})["isError"])
        self.desktop.call("desktop_scroll", {"frame_id": frame["frame_id"], "dy": -3, "x": 5, "y": 5})
        self.assertEqual(self.backend.events, [("move", "DP-1", 11.0, 11.0), ("scroll", 0, -3)])
        self.backend.events.clear()
        frame = self.shot()
        self.assertTrue(self.desktop.call("desktop_key", {"frame_id": frame["frame_id"], "keys": ["ctrl", "nope"]})["isError"])
        self.desktop.call("desktop_key", {"frame_id": frame["frame_id"], "keys": ["Ctrl", "L"], "repeat": 2})
        self.assertEqual(self.backend.events, [("key", "ctrl+shift+l", 2)])
        frame = self.shot()
        self.assertTrue(self.desktop.call("desktop_type_text", {"frame_id": frame["frame_id"], "text": "a\x1bb"})["isError"])
        self.desktop.call("desktop_type_text", {"frame_id": frame["frame_id"], "text": "Grüße λ\n"})
        self.assertEqual(self.backend.events[-1], ("type", "Grüße λ\n"))

    def test_click_releases_the_button_when_the_hold_is_interrupted(self):
        frame = self.shot()
        calls = {"n": 0}

        def interrupted_sleep(seconds):
            calls["n"] += 1
            if calls["n"] == 2:  # the sleep while the button is held
                raise KeyboardInterrupt

        self.desktop.sleep = interrupted_sleep
        with self.assertRaises(KeyboardInterrupt):
            self.desktop.call("desktop_click", {"frame_id": frame["frame_id"], "x": 1, "y": 1})
        self.assertEqual(self.backend.events[-2:], [("button", "left", True), ("button", "left", False)])


class KeyTests(unittest.TestCase):
    def test_chords(self):
        cases = {
            ("ctrl", "l"): ("ctrl+l", [0xFFE3], 0x6C),
            ("Control_L", "Return"): ("ctrl+return", [0xFFE3], 0xFF0D),
            ("Enter",): ("enter", [], 0xFF0D),
            ("alt", "Tab"): ("alt+tab", [0xFFE9], 0xFF09),
            ("super",): ("super", [0xFFEB], None),
            ("ctrl", "shift", "T"): ("ctrl+shift+t", [0xFFE3, 0xFFE1], 0x74),
            ("T", "shift"): ("shift+t", [0xFFE1], 0x74),
            ("cmd", "Page_Down"): ("super+pagedown", [0xFFEB], 0xFF56),
            ("F12",): ("f12", [], 0xFFC9),
            ("ctrl", "+"): ("ctrl++", [0xFFE3], 0x2B),
            ("é",): ("é", [], 0xE9),
            ("KP_Enter",): ("kpenter", [], 0xFF8D),
        }
        for keys, (label, modifiers, key) in cases.items():
            with self.subTest(keys=keys):
                chord = parse_chord(list(keys))
                self.assertEqual(chord.label, label)
                self.assertEqual([m.keysym for m in chord.modifiers], modifiers)
                self.assertEqual(chord.key, key)

    def test_string_chords(self):
        self.assertEqual(parse_chord("ctrl+l").label, "ctrl+l")
        self.assertEqual(parse_chord("ctrl++").key, 0x2B)
        self.assertEqual(parse_chord("+").key, 0x2B)
        self.assertEqual(parse_chord("Escape").key, 0xFF1B)

    def test_rejections(self):
        for keys in ([], ["ctrl", "ctrl"], ["a", "b"], ["ctrl", "Enter", "Tab"], ["CapsLock"],
                     ["unknownkey"], [""], ["a"] * 6, [1], ["\x07"], 5):
            with self.subTest(keys=keys), self.assertRaises(DesktopError):
                parse_chord(keys)

    def test_keysym_mapping(self):
        self.assertEqual(char_keysym("a"), 0x61)
        self.assertEqual(char_keysym("~"), 0x7E)
        self.assertEqual(char_keysym("\u00e9"), 0xE9)
        self.assertEqual(char_keysym("\u00a0"), 0xA0)
        self.assertEqual(char_keysym("\u03bb"), 0x010003BB)
        self.assertEqual(char_keysym("\u20ac"), 0x010020AC)
        self.assertEqual(char_keysym("\U0001F600"), 0x0101F600)
        self.assertEqual(char_keysym("\n"), 0xFF0D)
        self.assertEqual(char_keysym("\t"), 0xFF09)

    def test_text_validation(self):
        self.assertEqual(text_keysyms("a\tb\n"), [0x61, 0xFF09, 0x62, 0xFF0D])
        for bad in ("", "x" * 257, "a\rb", "\x00", "a\x7fb", "\x85", "\ud800", None, 5):
            with self.subTest(bad=bad), self.assertRaises(DesktopError):
                text_keysyms(bad)
        self.assertEqual(len(text_keysyms("x" * 256)), 256)
        self.assertEqual(text_keysyms("\u200d"), [0x0100200D])  # format characters are allowed


if __name__ == "__main__":
    unittest.main()
