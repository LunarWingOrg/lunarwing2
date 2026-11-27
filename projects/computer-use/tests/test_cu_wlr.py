"""wlroots backend against the fake compositor and a fake grim runner."""

import json
import re
import subprocess
import time
import unittest

import cu_support  # noqa: F401
from cu_support import Clock, FakeCompositor, FakeOutput, FakeRunner, FakeSelector, make_jpeg, wlr_backend_for

from lunarwing_computer.backends import Output
from lunarwing_computer.backends.wlr import absolute_axis, grim_argv
from lunarwing_computer.control import DesktopError, Rect, View, parse_chord
from lunarwing_computer.tools import Desktop, Options

LAPTOP = FakeOutput("eDP-1", 0, 0, 1707, 1067, 2560, 1600, scale=2)  # 2560x1600 panel at scale 1.5
SIDE = FakeOutput("DP-2", 1707, 0, 2560, 1440, 2560, 1440)
HIDPI = FakeOutput("DP-1", 0, 0, 1920, 1080, 3840, 2160, scale=2)
BTN_LEFT = 0x110


def wait_for(predicate, timeout=2.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.005)
    return False


class WlrTestCase(unittest.TestCase):
    outputs = (LAPTOP, SIDE)
    compositor_kwargs = {}

    def setUp(self):
        self.fake = FakeCompositor(list(self.outputs), **self.compositor_kwargs)
        self.runner = FakeRunner()
        self.backend = wlr_backend_for(self.fake, runner=self.runner)

    def tearDown(self):
        # No test may leave a gap in the client's object ids (libwayland-server
        # would have disconnected the client).
        id_errors = self.fake.id_errors()
        self.backend.disconnect()
        self.fake.close()
        self.assertEqual(id_errors, [])

    def pointer_requests(self):
        return [r[1:] for r in self.fake.requests_for("zwlr_virtual_pointer_v1")]

    def pointer_creates(self):
        return self.fake.requests_for("zwlr_virtual_pointer_manager_v1")

    def output_global(self, name):
        return next(g for g, output in self.fake.output_globals.items() if output.name == name)

    def keyboard_requests(self):
        return [r[1:] for r in self.fake.requests_for("zwp_virtual_keyboard_v1")]


class ConnectTests(WlrTestCase):
    def test_binds_explicit_versions_and_discovers_outputs(self):
        self.backend.connect()
        binds = {(r[3], r[4]) for r in self.fake.requests_for("wl_registry", "bind")}
        self.assertEqual(binds, {("wl_seat", 5), ("zwlr_virtual_pointer_manager_v1", 2),
                                 ("zwp_virtual_keyboard_manager_v1", 1), ("zxdg_output_manager_v1", 3),
                                 ("wl_output", 4)})
        outputs = self.backend.outputs()
        self.assertEqual([o.name for o in outputs], ["eDP-1", "DP-2"])
        laptop = outputs[0]
        self.assertEqual((laptop.x, laptop.y, laptop.width, laptop.height), (0, 0, 1707, 1067))
        self.assertAlmostEqual(laptop.scale, 2560 / 1707)
        self.assertEqual((laptop.physical_width, laptop.physical_height), (2560, 1600))
        self.assertEqual((outputs[1].x, outputs[1].scale), (1707, 1.0))
        info = self.backend.describe()
        self.assertTrue(info["pointer_bound_to_output"])
        self.assertEqual(info["globals"]["zwlr_virtual_pointer_manager_v1"], 2)
        # Connecting creates no virtual devices yet.
        self.assertEqual(self.fake.requests_for("zwlr_virtual_pointer_manager_v1"), [])
        self.assertEqual(self.fake.requests_for("zwp_virtual_keyboard_manager_v1"), [])

    def test_disconnect_destroys_devices_and_closes(self):
        self.backend.connect()
        self.backend.pointer_move_abs(self.backend.outputs()[0], 1, 1)
        self.backend.key_chord(parse_chord(["Enter"]))
        generation = self.backend.generation
        self.backend.disconnect()
        self.assertFalse(self.backend.connected)
        self.assertGreater(self.backend.generation, generation)
        self.assertTrue(self.fake.requests_for("zwlr_virtual_pointer_v1", "destroy"))
        self.assertTrue(self.fake.requests_for("zwp_virtual_keyboard_v1", "destroy"))
        self.assertTrue(wait_for(self.fake.closed.is_set))
        with self.assertRaisesRegex(DesktopError, "desktop_connect"):
            self.backend.outputs()


class MissingProtocolTests(WlrTestCase):
    compositor_kwargs = {"with_keyboard": False}

    def test_missing_virtual_keyboard_is_reported(self):
        with self.assertRaisesRegex(DesktopError, "zwp_virtual_keyboard_manager_v1"):
            self.backend.connect()
        self.assertFalse(self.backend.connected)


class NoXdgOutputTests(WlrTestCase):
    outputs = (FakeOutput("HDMI-A-1", 100, 50, 0, 0, 3840, 2160, scale=2),)
    compositor_kwargs = {"xdg_version": 0}

    def test_falls_back_to_integer_scale(self):
        self.backend.connect()
        output = self.backend.outputs()[0]
        self.assertEqual((output.name, output.x, output.y, output.width, output.height), ("HDMI-A-1", 100, 50, 1920, 1080))
        self.assertEqual(output.scale, 2.0)


class PointerTests(WlrTestCase):
    def test_absolute_motion_buttons_and_scroll(self):
        self.backend.connect()
        laptop, side = self.backend.outputs()
        self.backend.pointer_move_abs(laptop, 854.17, 534.9)
        self.backend.button("left", True)
        self.backend.button("left", False)
        self.backend.button("right", True)
        self.backend.button("right", False)
        self.backend.scroll(0, 2)
        self.backend.scroll(-1, 0)
        creates = self.fake.requests_for("zwlr_virtual_pointer_manager_v1")
        self.assertEqual(len(creates), 1)
        _iface, name, seat, output_id, _pointer = creates[0]
        self.assertEqual(name, "create_virtual_pointer_with_output")
        output_binds = [r for r in self.fake.requests_for("wl_registry", "bind") if r[3] == "wl_output"]
        self.assertEqual(output_id, output_binds[0][5])
        time_free = [(r[0],) + tuple(r[2:]) if r[0] in ("motion_absolute", "button", "axis_discrete") else r
                     for r in self.pointer_requests()]
        self.assertEqual(time_free, [
            # 1/256 logical units: 854.17 * 256 = 218667.52, 534.9 * 256 = 136934.4, extents 1707 and 1067 * 256.
            ("motion_absolute", 218668, 136934, 436992, 273152), ("frame",),
            ("button", 0x110, 1), ("frame",), ("button", 0x110, 0), ("frame",),
            ("button", 0x111, 1), ("frame",), ("button", 0x111, 0), ("frame",),
            ("axis_source", 0), ("axis_discrete", 0, 15.0, 1), ("frame",),
            ("axis_source", 0), ("axis_discrete", 0, 15.0, 1), ("frame",),
            ("axis_source", 0), ("axis_discrete", 1, -15.0, -1), ("frame",),
        ])
        # A second output gets its own pointer bound to it; coordinates are clamped into the output.
        self.backend.pointer_move_abs(side, 5000, -3)
        self.assertEqual(len(self.fake.requests_for("zwlr_virtual_pointer_manager_v1")), 2)
        self.assertEqual(self.pointer_requests()[-2][2:], (2560 * 256 - 1, 0, 2560 * 256, 1440 * 256))

    def test_invalid_button(self):
        self.backend.connect()
        with self.assertRaises(DesktopError):
            self.backend.button("back", True)

    def test_scroll_before_any_motion_uses_an_unbound_pointer(self):
        self.backend.connect()
        self.backend.scroll(0, 1)
        self.assertEqual([c[1] for c in self.pointer_creates()], ["create_virtual_pointer"])
        self.assertEqual([r[0] for r in self.pointer_requests()], ["axis_source", "axis_discrete", "frame"])
        self.backend.button("left", False)  # nothing is held, so nothing is sent
        with self.assertRaisesRegex(DesktopError, "move the pointer"):
            self.backend.button("left", True)
        self.assertNotIn("button", [r[0] for r in self.pointer_requests()])

    def test_disconnect_releases_a_held_button_first(self):
        self.backend.connect()
        laptop = self.backend.outputs()[0]
        self.backend.pointer_move_abs(laptop, 10, 10)
        self.backend.button("left", True)
        self.backend.disconnect()
        names = [(r[0], r[3]) if r[0] == "button" else (r[0],) for r in self.pointer_requests()]
        self.assertEqual(names[-5:], [("button", 1), ("frame",), ("button", 0), ("frame",), ("destroy",)])


class SubUnitPointerTests(WlrTestCase):
    outputs = (HIDPI,)

    def test_absolute_axis_rounds_and_clamps(self):
        self.assertEqual(absolute_axis(400.25, 1920), (102464, 491520))
        self.assertEqual(absolute_axis(-5.0, 1920), (0, 491520))
        self.assertEqual(absolute_axis(1920.0, 1920), (491519, 491520))
        position, extent = absolute_axis(10.0, 40_000_000)  # extent * 256 would overflow uint32
        self.assertLessEqual(extent, 0xFFFFFFFF)
        self.assertEqual(position / extent, 10.0 / 40_000_000)
        with self.assertRaises(DesktopError):
            absolute_axis(1.0, 0)

    def test_every_pixel_of_a_zoomed_frame_reaches_its_own_position(self):
        # 3840x2160 panel at scale 2 (1920x1080 logical); a 640x360 zoom captured at scale 2 is 1280x720.
        self.backend.connect()
        output = self.backend.outputs()[0]
        zoom = View("DP-1", 1280, 720, Rect(400, 200, 640, 360), self.backend.generation, zoomed=True)
        expected = []
        for pixel in range(6):
            x, y = zoom.to_logical(pixel, pixel)
            expected.append((x, y))
            self.backend.pointer_move_abs(output, x, y)
        motions = [r for r in self.pointer_requests() if r[0] == "motion_absolute"]
        self.assertEqual({(r[4], r[5]) for r in motions}, {(1920 * 256, 1080 * 256)})
        landed = [(r[2] / 256, r[3] / 256) for r in motions]
        self.assertEqual(landed, expected)  # pixel centres 400.25, 400.75, ... are exact in 1/256 units
        self.assertEqual(len(set(landed)), 6)


class UnboundPointerTests(WlrTestCase):
    compositor_kwargs = {"vp_version": 1}

    def test_version_1_uses_the_layout_bounding_box(self):
        self.backend.connect()
        _laptop, side = self.backend.outputs()
        self.backend.pointer_move_abs(side, 10.6, 20.2)
        create = self.fake.requests_for("zwlr_virtual_pointer_manager_v1")
        self.assertEqual([c[1] for c in create], ["create_virtual_pointer"])
        motion = self.pointer_requests()[0]
        # (1707 + 10.6) * 256 = 439705.6, 20.2 * 256 = 5171.2; the extent is the whole layout box.
        self.assertEqual(motion[2:], (439706, 5171, (1707 + 2560) * 256, 1440 * 256))
        self.assertFalse(self.backend.describe()["pointer_bound_to_output"])

    def test_held_button_survives_the_removal_of_another_output(self):
        self.backend.connect()
        laptop, side = self.backend.outputs()
        self.backend.pointer_move_abs(side, 10, 10)
        self.backend.button("left", True)
        self.fake.remove_global(self.output_global("eDP-1"))
        self.backend.pointer_move_abs(side, 20, 10)  # the removal arrives during this motion
        self.assertEqual([o.name for o in self.backend.outputs()], ["DP-2"])
        self.backend.pointer_move_abs(side, 30, 10)
        self.backend.button("left", False)  # its roundtrip makes the fake record everything sent so far
        motions = [r for r in self.pointer_requests() if r[0] == "motion_absolute"]
        # Once eDP-1 is gone the layout box is DP-2 alone, so positions are relative to DP-2's origin.
        self.assertEqual(motions[-1][2:], (30 * 256, 10 * 256, 2560 * 256, 1440 * 256))
        buttons = [r[2:] for r in self.pointer_requests() if r[0] == "button"]
        self.assertEqual(buttons, [(BTN_LEFT, 1), (BTN_LEFT, 0)])


class KeyboardTests(WlrTestCase):
    def keymap_text(self, index=-1):
        fmt, size, content = self.fake.keymaps[index]
        self.assertEqual(fmt, 1)
        self.assertEqual(len(content), size)  # size counts the trailing NUL and nothing more
        self.assertEqual(content[-1:], b"\0")
        return content[:-1].decode("ascii")

    def codes(self, text):
        names = dict(re.findall(r"<(K\d+)> = (\d+);", text))
        result = {}
        for name, symbols in re.findall(r"key <(K\d+)> \{ \[ ([^\]]+) \] \};", text):
            result[int(names[name]) - 8] = tuple(int(s, 16) for s in symbols.split(", "))
        return result

    def test_chord_presses_and_releases_in_reverse_with_modifier_state(self):
        self.backend.connect()
        self.backend.key_chord(parse_chord(["ctrl", "shift", "t"]))
        text = self.keymap_text()
        codes = self.codes(text)
        by_sym = {levels[0]: code for code, levels in codes.items()}
        ctrl, shift, t = by_sym[0xFFE3], by_sym[0xFFE1], by_sym[0x74]
        self.assertEqual(codes[t], (0x74, 0x54))
        self.assertIn("modifier_map Control", text)
        events = [(r[0],) + tuple(r[2:]) if r[0] == "key" else r for r in self.keyboard_requests()]
        self.assertEqual(events, [
            ("keymap", 1, len(self.fake.keymaps[-1][2])),
            ("key", ctrl, 1), ("modifiers", 4, 0, 0, 0),
            ("key", shift, 1), ("modifiers", 5, 0, 0, 0),
            ("key", t, 1), ("key", t, 0),
            ("key", shift, 0), ("modifiers", 4, 0, 0, 0),
            ("key", ctrl, 0), ("modifiers", 0, 0, 0, 0),
        ])
        self.assertEqual(self.fake.errors_sent, [])

    def test_repeat_and_keymap_uploaded_on_every_call(self):
        self.backend.connect()
        self.backend.key_chord(parse_chord(["Down"]), repeat=3)
        self.backend.key_chord(parse_chord(["Down"]))
        self.assertEqual(len(self.fake.keymaps), 2)
        self.assertEqual(self.fake.keymaps[0], self.fake.keymaps[1])
        self.assertEqual(len(self.fake.requests_for("zwp_virtual_keyboard_manager_v1")), 1)
        presses = [r for r in self.keyboard_requests() if r[0] == "key" and r[3] == 1]
        self.assertEqual(len(presses), 4)
        with self.assertRaises(DesktopError):
            self.backend.key_chord(parse_chord(["Down"]), repeat=11)

    def test_type_text_uses_its_own_keymap(self):
        self.backend.connect()
        text = "Hi λ\n€"
        self.backend.type_text(text)
        codes = self.codes(self.keymap_text())
        keys = [r for r in self.keyboard_requests() if r[0] == "key"]
        self.assertEqual([r[3] for r in keys], [1, 0] * len(text))
        typed = [codes[r[2]][0] for r in keys[::2]]
        self.assertEqual(typed, [0x48, 0x69, 0x20, 0x010003BB, 0xFF0D, 0x010020AC])
        self.assertTrue(all(len(levels) == 1 for levels in codes.values()))

    def test_interrupted_chord_releases_everything_in_reverse(self):
        self.backend.connect()
        pauses = {"n": 0}

        def interrupted(_seconds):
            pauses["n"] += 1
            raise KeyboardInterrupt  # e.g. SIGTERM while the key is held

        self.backend._sleep = interrupted
        with self.assertRaises(KeyboardInterrupt):
            self.backend.key_chord(parse_chord(["ctrl", "alt", "Delete"]))
        self.backend.conn.roundtrip()
        events = [(r[0], r[2], r[3]) if r[0] == "key" else (r[0], r[1]) for r in self.keyboard_requests()[1:]]
        pressed = [e[1] for e in events if e[0] == "key" and e[2] == 1]
        released = [e[1] for e in events if e[0] == "key" and e[2] == 0]
        self.assertEqual(released, list(reversed(pressed)))
        self.assertEqual(events[-1], ("modifiers", 0))

    def test_protocol_error_closes_the_session(self):
        self.fake.fail_on = ("zwp_virtual_keyboard_v1", "modifiers")
        self.backend.connect()
        with self.assertRaisesRegex(DesktopError, "session was closed"):
            self.backend.key_chord(parse_chord(["ctrl", "c"]))
        self.assertFalse(self.backend.connected)


class GrimTests(WlrTestCase):
    def test_full_output_argv_env_and_header_size(self):
        self.backend.connect()
        laptop = self.backend.outputs()[0]
        shot = self.backend.capture(laptop, 1280, "png")
        argv, env, timeout = self.runner.calls[0]
        self.assertEqual(argv, ["grim", "-c", "-t", "png", "-s", "0.749853", "-o", "eDP-1", "-"])
        self.assertEqual(env["WAYLAND_DISPLAY"], "wayland-9")
        self.assertEqual(env["XDG_RUNTIME_DIR"], "/run/user/test")
        self.assertEqual(timeout, 10.0)
        self.assertEqual((shot.mime, shot.width, shot.height), ("image/png", 1280, 800))

    def test_region_and_jpeg_argv(self):
        output = Output("DP-2", 1707, 0, 2560, 1440, 1.0)
        argv = grim_argv("grim", output, Rect(100, 200, 640, 360), 1.0, "jpeg", 70, False)
        self.assertEqual(argv, ["grim", "-t", "jpeg", "-q", "70", "-s", "1.000000", "-g", "1807,200 640x360", "-"])
        argv = grim_argv("grim", output, Rect(0, 0, 2560, 1440), 0.5, "png", 80, True)
        self.assertEqual(argv[-3:], ["-o", "DP-2", "-"])

    def test_grim_failures_are_clear(self):
        self.backend.connect()
        laptop = self.backend.outputs()[0]
        for runner, pattern in [
            (FakeRunner(exc=FileNotFoundError()), "grim is not installed"),
            (FakeRunner(exc=subprocess.TimeoutExpired("grim", 10)), "permission prompt"),
            (FakeRunner(code=1, stderr=b"warning\ncompositor doesn't support screencopy\n"), "screencopy"),
        ]:
            with self.subTest(pattern=pattern):
                self.backend._runner = runner
                with self.assertRaisesRegex(DesktopError, pattern):
                    self.backend.capture(laptop, 1280, "png")
        self.backend._runner = lambda argv, env, timeout: (0, make_jpeg(10, 10), b"")
        with self.assertRaisesRegex(DesktopError, "instead of image/png"):
            self.backend.capture(laptop, 1280, "png")
        self.backend._runner = lambda argv, env, timeout: (0, b"not an image", b"")
        with self.assertRaisesRegex(DesktopError, "unreadable"):
            self.backend.capture(laptop, 1280, "png")


class HotplugTests(WlrTestCase):
    def test_press_after_the_pointer_output_vanished_is_refused(self):
        self.backend.connect()
        laptop, side = self.backend.outputs()
        self.backend.pointer_move_abs(side, 10, 10)
        generation = self.backend.generation
        self.fake.remove_global(self.output_global("DP-2"))
        self.backend.pointer_move_abs(side, 20, 20)  # the removal arrives during this motion's roundtrip
        self.assertGreater(self.backend.generation, generation)  # frames are invalid at once
        # No fallback pointer on a surviving output: the click would land where nobody looked.
        with self.assertRaisesRegex(DesktopError, "removed; take a new desktop_screenshot"):
            self.backend.button("left", True)
        with self.assertRaisesRegex(DesktopError, "removed; take a new desktop_screenshot"):
            self.backend.scroll(0, 1)
        self.backend.conn.roundtrip()  # let the fake record everything sent so far
        self.assertEqual(len(self.pointer_creates()), 1)
        self.assertEqual([r for r in self.pointer_requests() if r[0] in ("button", "axis_discrete")], [])
        with self.assertRaisesRegex(DesktopError, "DP-2 is gone"):
            self.backend.pointer_move_abs(side, 30, 30)
        # A new motion on a remaining output makes buttons usable again.
        self.assertTrue(self.backend.connected)
        self.backend.pointer_move_abs(laptop, 5, 5)
        self.backend.button("left", True)
        self.backend.button("left", False)
        self.assertEqual(self.fake.errors_sent, [])

    def test_held_button_is_released_on_its_own_pointer_and_no_id_gap_is_left(self):
        self.backend.connect()
        laptop, side = self.backend.outputs()
        self.backend.pointer_move_abs(laptop, 10, 10)
        self.backend.button("left", True)
        pointer_id = self.pointer_creates()[0][-1]
        self.fake.remove_global(self.output_global("eDP-1"))
        self.backend.pointer_move_abs(laptop, 20, 20)  # a drag step; the removal arrives here
        conn = self.backend.conn
        ids = (conn._next_id, list(conn._free_ids))
        # Later drag steps are refused without allocating an object id. Several
        # attempts, so a leak would also use up the free list and skip a fresh id.
        for _ in range(5):
            with self.assertRaisesRegex(DesktopError, "eDP-1 is gone"):
                self.backend.pointer_move_abs(laptop, 30, 30)
        self.assertEqual((conn._next_id, list(conn._free_ids)), ids)
        self.backend.button("left", False)  # the drag's final release: already done, nothing to send
        self.backend.conn.roundtrip()  # let the fake record everything sent so far
        on_pointer = [r[0] if r[0] != "button" else (r[0], r[2], r[3]) for r in self.fake.requests_on(pointer_id)]
        self.assertEqual(on_pointer, [
            "motion_absolute", "frame", ("button", BTN_LEFT, 1), "frame",
            "motion_absolute", "frame", ("button", BTN_LEFT, 0), "frame", "destroy",
        ])
        self.assertEqual(len(self.fake.requests_for("zwlr_virtual_pointer_v1", "button")), 2)
        # The failed steps allocated no object id, so creating objects still works.
        self.backend.pointer_move_abs(side, 5, 5)
        self.backend.key_chord(parse_chord(["ctrl", "l"]))
        self.assertEqual(self.fake.errors_sent, [])
        self.assertTrue(self.backend.connected)

    def test_drag_through_the_desktop_reports_the_vanished_output(self):
        clock = Clock()
        desktop = Desktop(FakeSelector(self.backend), Options(), clock=clock, sleep=clock.sleep)
        desktop.call("desktop_connect", {"output": "eDP-1"})
        info = json.loads(desktop.call("desktop_screenshot", {})["content"][0]["text"])
        steps = {"n": 0}

        def sleep(seconds):
            clock.sleep(seconds)
            steps["n"] += 1
            if steps["n"] == 3:  # after the first drag step
                self.fake.remove_global(self.output_global("eDP-1"))

        desktop.sleep = sleep
        result = desktop.call("desktop_drag", {"frame_id": info["frame_id"], "from": {"x": 10, "y": 10},
                                               "to": {"x": 200, "y": 10}})
        self.assertTrue(result["isError"])
        self.assertIn("eDP-1 is gone", result["content"][0]["text"])
        self.backend.conn.roundtrip()  # let the fake record everything sent so far
        buttons = [r[2:] for r in self.pointer_requests() if r[0] == "button"]
        self.assertEqual(buttons, [(BTN_LEFT, 1), (BTN_LEFT, 0)])
        self.assertEqual(self.fake.errors_sent, [])
        desktop.shutdown()

    def test_output_added_and_removed(self):
        self.backend.connect()
        generation = self.backend.generation
        name = self.fake.add_output(FakeOutput("DP-5", 4267, 0, 1920, 1080, 1920, 1080))
        self.assertTrue(wait_for(lambda: len(self.backend.outputs()) == 3))
        self.assertGreater(self.backend.generation, generation)
        generation = self.backend.generation
        self.backend.pointer_move_abs(self.backend.outputs()[2], 1, 1)
        self.fake.remove_global(name)
        self.assertTrue(wait_for(lambda: len(self.backend.outputs()) == 2))
        self.assertGreater(self.backend.generation, generation)
        self.assertTrue(wait_for(lambda: self.fake.requests_for("zwlr_virtual_pointer_v1", "destroy")))


class EndToEndTests(WlrTestCase):
    outputs = (LAPTOP,)

    def make_desktop(self):
        clock = Clock()
        return Desktop(FakeSelector(self.backend), Options(), clock=clock, sleep=clock.sleep)

    def test_click_maps_frame_pixels_to_logical_coordinates_under_fractional_scale(self):
        desktop = self.make_desktop()
        self.assertFalse(desktop.call("desktop_connect", {})["isError"])
        result = desktop.call("desktop_screenshot", {})
        info = json.loads(result["content"][0]["text"])
        self.assertEqual((info["width"], info["height"]), (1280, 800))
        self.assertAlmostEqual(info["scale"], round(1280 / 1707, 4))
        clicked = desktop.call("desktop_click", {"frame_id": info["frame_id"], "x": 640, "y": 400})
        self.assertFalse(clicked["isError"], clicked)
        motion = self.pointer_requests()[0]
        self.assertEqual(motion[0], "motion_absolute")
        # Pixel (640, 400) maps to logical (640.5 * 1707 / 1280, 400.5 * 1067 / 800) = (854.167, 534.167),
        # sent in 1/256 units against extents of 1707 and 1067 logical units.
        self.assertEqual(motion[2:], (218667, 136747, 1707 * 256, 1067 * 256))
        self.assertAlmostEqual(motion[2] / 256, 640.5 * 1707 / 1280, delta=1 / 512)
        self.assertAlmostEqual(motion[3] / 256, 400.5 * 1067 / 800, delta=1 / 512)
        desktop.shutdown()

    def test_big_png_is_retried_as_jpeg(self):
        self.runner.png_padding = 3_000_000
        desktop = self.make_desktop()
        desktop.call("desktop_connect", {})
        result = desktop.call("desktop_screenshot", {})
        self.assertFalse(result["isError"])
        self.assertEqual(result["content"][1]["mimeType"], "image/jpeg")
        self.assertEqual([c[0][3] for c in self.runner.calls], ["png", "jpeg"])
        self.assertIn("-q", self.runner.calls[1][0])
        self.assertEqual(self.runner.calls[1][0][self.runner.calls[1][0].index("-q") + 1], "80")


if __name__ == "__main__":
    unittest.main()
