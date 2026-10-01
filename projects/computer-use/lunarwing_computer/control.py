"""Desktop operations independent of the portal/transport implementation."""

import base64
import math
import secrets
import time
from dataclasses import dataclass


class DesktopError(Exception):
    """An actionable error safe to return to the caller."""


@dataclass(frozen=True)
class Frame:
    png: bytes
    width: int
    height: int
    logical_width: int
    logical_height: int


KEYS = {
    "Enter": 0xFF0D, "Tab": 0xFF09, "Escape": 0xFF1B,
    "Backspace": 0xFF08, "Delete": 0xFFFF, "Space": 0x20,
    "Left": 0xFF51, "Up": 0xFF52, "Right": 0xFF53, "Down": 0xFF54,
    "Home": 0xFF50, "End": 0xFF57, "PageUp": 0xFF55, "PageDown": 0xFF56,
    "Shift": 0xFFE1, "Ctrl": 0xFFE3, "Alt": 0xFFE9, "Super": 0xFFEB,
    **{f"F{i}": 0xFFBD + i for i in range(1, 13)},
}


def keysym(key):
    if key in KEYS:
        return KEYS[key]
    if len(key) == 1 and key.isprintable():
        code = ord(key)
        return code if code < 256 else 0x01000000 | code
    raise DesktopError("Unknown key; use a printable character or a documented key name")


class Desktop:
    def __init__(self, portal, vision=None, clock=time.monotonic):
        self.portal = portal
        self.vision = vision
        self.clock = clock
        self.frame = None
        self.frame_id = None
        self.frame_time = 0

    def invalidate(self):
        self.frame = None
        self.frame_id = None

    def status(self):
        self.portal.pump()
        if not self.portal.active:
            self.invalidate()
        return {"connected": self.portal.active,
                "vision_configured": self.vision is not None,
                "coordinate_space": "pixels in the latest observed screenshot"}

    def connect(self):
        self.invalidate()
        self.portal.start()
        return self.status()

    def disconnect(self):
        self.invalidate()
        self.portal.close()
        return {"connected": False}

    def observe(self, prompt="Describe the desktop and locate relevant controls.", image=False):
        self.invalidate()
        if not image and self.vision is None:
            raise DesktopError("Configure LunarVision in the connector config to use desktop_observe")
        self.portal.pump()
        frame = self.portal.capture()
        if not (0 < frame.width <= 4096 and 0 < frame.height <= 4096
                and frame.logical_width > 0 and frame.logical_height > 0):
            raise DesktopError("Invalid screenshot dimensions")
        if len(frame.png) > 8 * 1024 * 1024:
            raise DesktopError("Screenshot exceeds 8 MiB")
        captured = self.clock()
        description = None if image else self.vision.describe(frame, prompt)
        self.portal.pump()
        if not self.portal.active:
            raise DesktopError("Desktop sharing ended; connect again")
        self.frame = frame
        self.frame_id = secrets.token_hex(16)
        self.frame_time = captured
        result = {"frame_id": self.frame_id, "width": frame.width, "height": frame.height,
                  "coordinate_space": "screenshot pixels, origin top-left",
                  "expires_in_seconds": max(0, 60 - (self.clock() - captured)),
                  "provenance": "untrusted_desktop_content"}
        if description is not None:
            result["observation"] = description
        if image:
            result["image"] = base64.b64encode(frame.png).decode("ascii")
        return result

    def require_frame(self, frame_id):
        self.portal.pump()
        if not self.portal.active:
            self.invalidate()
            raise DesktopError("Desktop sharing ended; connect again")
        if (self.frame is None or frame_id != self.frame_id
                or self.clock() - self.frame_time > 60):
            raise DesktopError("Missing, stale or consumed frame_id; observe the desktop again")
        return self.frame

    def point(self, frame, x, y):
        if (isinstance(x, bool) or isinstance(y, bool)
                or not isinstance(x, (int, float)) or not isinstance(y, (int, float))
                or not math.isfinite(x) or not math.isfinite(y)
                or not 0 <= x < frame.width or not 0 <= y < frame.height):
            raise DesktopError("Coordinates must be within the observed screenshot")
        return (x * frame.logical_width / frame.width,
                y * frame.logical_height / frame.height)

    def click(self, frame_id, x, y, button="left", count=1):
        frame = self.require_frame(frame_id)
        position = self.point(frame, x, y)
        if button not in ("left", "middle", "right") or type(count) is not int or count not in (1, 2):
            raise DesktopError("Invalid button or click count")
        self.invalidate()
        self.portal.move(*position)
        for _ in range(count):
            try:
                self.portal.button(button, True)
            finally:
                self.portal.button(button, False)
        return {"sent": True, "next": "Observe again to verify the result"}

    def move(self, frame_id, x, y):
        position = self.point(self.require_frame(frame_id), x, y)
        self.invalidate()
        self.portal.move(*position)
        return {"sent": True}

    def scroll(self, frame_id, steps, axis="vertical"):
        self.require_frame(frame_id)
        if type(steps) is not int or not -20 <= steps <= 20 or steps == 0:
            raise DesktopError("steps must be a nonzero integer from -20 to 20")
        if axis not in ("vertical", "horizontal"):
            raise DesktopError("axis must be vertical or horizontal")
        self.invalidate()
        self.portal.scroll(axis, steps)
        return {"sent": True}

    def key(self, frame_id, keys):
        self.require_frame(frame_id)
        if not isinstance(keys, list) or not 1 <= len(keys) <= 5 or not all(isinstance(k, str) for k in keys):
            raise DesktopError("keys must contain 1 to 5 key names")
        symbols = [keysym(k) for k in keys]
        if len(set(symbols)) != len(symbols):
            raise DesktopError("Duplicate keys in chord")
        self.invalidate()
        held = []
        try:
            for symbol in symbols:
                held.append(symbol)
                self.portal.key(symbol, True)
        finally:
            for symbol in reversed(held):
                try:
                    self.portal.key(symbol, False)
                except Exception:
                    self.portal.close()
                    raise DesktopError("Input interrupted; session closed to release held keys") from None
        return {"sent": True}

    def type_text(self, frame_id, text):
        self.require_frame(frame_id)
        if not isinstance(text, str) or not 1 <= len(text) <= 256:
            raise DesktopError("text must contain 1 to 256 characters")
        symbols = [keysym({"\n": "Enter", "\t": "Tab"}.get(c, c)) for c in text]
        self.invalidate()
        for symbol in symbols:
            try:
                self.portal.key(symbol, True)
            finally:
                self.portal.key(symbol, False)
        return {"sent": True, "next": "Observe again; application text entry is not guaranteed"}
