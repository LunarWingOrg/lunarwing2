"""Backend auto-selection by probing."""

import unittest

import cu_support  # noqa: F401
from cu_support import FakeBackend

from lunarwing_computer.backends import Selector
from lunarwing_computer.control import DesktopError

WLR_GLOBALS = {"wl_seat": 9, "wl_output": 4, "zwlr_virtual_pointer_manager_v1": 2,
               "zwp_virtual_keyboard_manager_v1": 1, "zxdg_output_manager_v1": 3}


def selector(mode="auto", wayland=None, portal=None):
    made = []

    def wayland_probe(_env):
        if isinstance(wayland, Exception):
            raise wayland
        return {"socket": "/run/user/1/wayland-1", "globals": dict(wayland or {})}

    def portal_probe(_env):
        if isinstance(portal, Exception):
            raise portal
        return portal or {"available": False, "reason": "org.freedesktop.portal.RemoteDesktop is not offered"}

    def factory(kind):
        def make(env, options):
            backend = FakeBackend()
            backend.name = kind
            made.append(kind)
            return backend
        return make

    sel = Selector(mode, {}, wayland_probe=wayland_probe, portal_probe=portal_probe,
                   wlr_factory=factory("wlr"), portal_factory=factory("portal"))
    return sel, made


class SelectionTests(unittest.TestCase):
    def test_auto_prefers_wlr_when_both_virtual_input_managers_exist(self):
        sel, made = selector(wayland=WLR_GLOBALS, portal={"available": True, "version": 2})
        self.assertEqual(sel.select().name, "wlr")
        self.assertEqual(made, ["wlr"])

    def test_auto_uses_the_portal_when_wlr_protocols_are_missing(self):
        globals_ = {k: v for k, v in WLR_GLOBALS.items() if k != "zwlr_virtual_pointer_manager_v1"}
        sel, _made = selector(wayland=globals_, portal={"available": True, "version": 2})
        self.assertEqual(sel.select().name, "portal")
        sel, _made = selector(wayland=DesktopError("no wayland-N socket"), portal={"available": True, "version": 1})
        self.assertEqual(sel.select().name, "portal")

    def test_auto_error_lists_what_was_probed(self):
        sel, made = selector(wayland={"wl_seat": 9}, portal={"available": False, "reason": "PyGObject missing"})
        with self.assertRaises(DesktopError) as caught:
            sel.select()
        message = str(caught.exception)
        self.assertIn("zwlr_virtual_pointer_manager_v1", message)
        self.assertIn("zwp_virtual_keyboard_manager_v1", message)
        self.assertIn("PyGObject missing", message)
        self.assertEqual(made, [])

    def test_explicit_modes_skip_probing(self):
        sel, _made = selector("wlr", wayland=DesktopError("unused"), portal=DesktopError("unused"))
        self.assertEqual(sel.select().name, "wlr")
        sel, _made = selector("portal", wayland=DesktopError("unused"), portal=DesktopError("unused"))
        self.assertEqual(sel.select().name, "portal")
        sel, _made = selector("x11")
        with self.assertRaises(DesktopError):
            sel.select()

    def test_probe_report_never_raises_and_follows_the_mode(self):
        sel, _made = selector(wayland=DesktopError("no socket"), portal=DesktopError("no bus"))
        report = sel.probe()
        self.assertEqual(report["wayland"], {"error": "no socket"})
        self.assertEqual(report["portal"], {"error": "no bus"})
        sel, _made = selector("wlr", wayland=WLR_GLOBALS)
        report = sel.probe()
        self.assertTrue(report["wayland"]["wlr_protocols"])
        self.assertNotIn("portal", report)


if __name__ == "__main__":
    unittest.main()
