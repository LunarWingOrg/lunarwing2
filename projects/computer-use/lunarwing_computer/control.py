"""Pure desktop-control logic shared by every backend.

Nothing in this module performs I/O. It holds the error type returned to the
model, the frame guard that ties every input action to a fresh screenshot, the
mapping from screenshot pixels to compositor logical coordinates, and the
parsing of key names, chords and text into X keysyms.
"""

from __future__ import annotations

import math
import secrets
import threading
import time
import unicodedata
from collections import OrderedDict
from dataclasses import dataclass
from typing import Callable, Dict, List, Optional, Sequence, Tuple, Union

FRAME_LIFETIME_SECONDS = 60.0
MAX_TEXT_CHARS = 256
MAX_CHORD_KEYS = 5
MAX_IMAGE_EDGE = 16384


class DesktopError(Exception):
    """An actionable error whose message is safe to return to the model."""


# ---------------------------------------------------------------------------
# Geometry


@dataclass(frozen=True)
class Rect:
    """Integer rectangle in compositor logical coordinates, relative to an output."""

    x: int
    y: int
    width: int
    height: int


@dataclass(frozen=True)
class View:
    """How the pixels of one screenshot map onto an output's logical space.

    ``rect`` is the logical rectangle (relative to the output origin) that the
    image covers; ``width`` and ``height`` are the image's real pixel size, read
    from the encoded image header rather than predicted from a scale factor.
    """

    output: str
    width: int
    height: int
    rect: Rect
    generation: int
    zoomed: bool = False

    @property
    def scale_x(self) -> float:
        """Screenshot pixels per logical unit on the x axis."""
        return self.width / self.rect.width

    @property
    def scale_y(self) -> float:
        return self.height / self.rect.height

    def to_logical(self, x: int, y: int) -> Tuple[float, float]:
        """Map the centre of screenshot pixel (x, y) to output-relative logical coordinates."""
        check_point(self, x, y)
        lx = self.rect.x + (x + 0.5) * self.rect.width / self.width
        ly = self.rect.y + (y + 0.5) * self.rect.height / self.height
        return lx, ly

    def region_to_rect(self, x, y, width, height, output_width: int, output_height: int) -> Rect:
        """Map a region given in this screenshot's pixels to an integer logical rectangle.

        The result is widened to whole logical units (floor of the start, ceil
        of the end) and clamped to the output, so the capture covers at least
        the requested pixels.
        """
        for name, value in (("x", x), ("y", y), ("width", width), ("height", height)):
            if not is_int(value):
                raise DesktopError(f"region.{name} must be an integer")
        if x < 0 or y < 0 or width < 1 or height < 1:
            raise DesktopError("region needs x, y >= 0 and width, height >= 1")
        if x + width > self.width or y + height > self.height:
            raise DesktopError(
                f"region must lie inside the latest {self.width}x{self.height} screenshot of {self.output}"
            )
        sx = self.rect.width / self.width
        sy = self.rect.height / self.height
        left = math.floor(self.rect.x + x * sx + 1e-6)
        top = math.floor(self.rect.y + y * sy + 1e-6)
        right = math.ceil(self.rect.x + (x + width) * sx - 1e-6)
        bottom = math.ceil(self.rect.y + (y + height) * sy - 1e-6)
        left = min(max(0, left), output_width - 1)
        top = min(max(0, top), output_height - 1)
        right = min(output_width, max(right, left + 1))
        bottom = min(output_height, max(bottom, top + 1))
        return Rect(left, top, right - left, bottom - top)


def is_int(value) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def check_point(view: View, x, y) -> None:
    """Reject anything that is not an integer pixel inside the screenshot."""
    if not is_int(x) or not is_int(y):
        raise DesktopError("x and y must be integers in screenshot pixels")
    if not (0 <= x < view.width and 0 <= y < view.height):
        raise DesktopError(
            f"({x}, {y}) is outside the {view.width}x{view.height} screenshot of {view.output}; "
            f"valid x is 0..{view.width - 1}, valid y is 0..{view.height - 1}"
        )


def capture_scale(rect: Rect, max_edge: int, max_scale: float) -> float:
    """Scale factor (image pixels per logical unit) for a capture of ``rect``.

    The long edge of the image is at most ``max_edge`` and the image is never
    sharper than ``max_scale`` (the output's physical pixels per logical unit),
    so a capture is never upscaled past the real screen resolution. The value
    is rounded down to six decimals so a tool that truncates the product cannot
    overshoot ``max_edge``.
    """
    if rect.width < 1 or rect.height < 1:
        raise DesktopError("capture area is empty")
    if max_edge < 1:
        raise DesktopError("max edge must be positive")
    scale = min(max_scale, max_edge / max(rect.width, rect.height))
    scale = math.floor(scale * 1_000_000) / 1_000_000
    return max(scale, 0.000001)


def full_rect(width: int, height: int) -> Rect:
    return Rect(0, 0, width, height)


# ---------------------------------------------------------------------------
# Frame guard


@dataclass(frozen=True)
class Frame:
    frame_id: str
    view: View
    issued_at: float


class FrameGuard:
    """Single-use, short-lived permission to act on the latest screenshot.

    Only the most recent screenshot of each output carries a usable frame id.
    The first input action that presents one consumes it and retires the
    frames of every other output too, so every input action needs a
    screenshot taken after the previous input action. Frames expire after
    ``lifetime`` seconds and are dropped wholesale on connect, disconnect,
    desktop_wait and any change of the output layout (``generation``).
    """

    _SPENT_MEMORY = 64

    def __init__(self, clock: Callable[[], float] = time.monotonic, lifetime: float = FRAME_LIFETIME_SECONDS):
        self._clock = clock
        self._lifetime = lifetime
        self._lock = threading.Lock()
        self._latest: Dict[str, Frame] = {}
        self._views: Dict[str, View] = {}
        self._spent: "OrderedDict[str, str]" = OrderedDict()

    def reset(self, reason: str = "the desktop session was reset") -> None:
        """Retire every frame and forget the views (the session itself changed)."""
        with self._lock:
            self._retire_all(reason)
            self._views.clear()

    def retire(self, reason: str) -> None:
        """Retire every live frame but keep the views.

        A zoom region can still be given in the pixels of the last image of an
        output; only acting on an old image is refused.
        """
        with self._lock:
            self._retire_all(reason)

    def _retire_all(self, reason: str) -> None:
        for frame in self._latest.values():
            self._remember(frame.frame_id, reason)
        self._latest.clear()

    def issue(self, view: View) -> Frame:
        with self._lock:
            previous = self._latest.get(view.output)
            if previous is not None:
                self._remember(previous.frame_id, "a newer screenshot of the same output replaced it")
            frame = Frame(secrets.token_hex(16), view, self._clock())
            self._latest[view.output] = frame
            self._views[view.output] = view
            return frame

    def latest_view(self, output: str, generation: int) -> Optional[View]:
        """Mapping of the latest screenshot of ``output`` (used to place a region).

        Consuming a frame does not remove its view: a zoom region can still be
        given in the pixels of the image the model last saw.
        """
        with self._lock:
            view = self._views.get(output)
            if view is None or view.generation != generation:
                return None
            return view

    def remaining(self, frame: Frame) -> float:
        return max(0.0, self._lifetime - (self._clock() - frame.issued_at))

    def consume(self, frame_id, generation: int, check: Optional[Callable[[Frame], None]] = None) -> Frame:
        """Validate and spend ``frame_id``.

        ``check`` runs before the frame is spent; if it raises (for example
        because a coordinate is out of range) every frame stays usable. Once
        the frame passes, it is spent and the frames of all other outputs are
        retired as well, because the input about to be sent may change what
        any of them showed.
        """
        if not isinstance(frame_id, str) or len(frame_id) != 32:
            raise DesktopError("frame_id must be the 32-character id from the latest desktop_screenshot")
        with self._lock:
            frame = None
            for candidate in self._latest.values():
                if secrets.compare_digest(candidate.frame_id, frame_id):
                    frame = candidate
                    break
            if frame is None:
                reason = self._spent.get(frame_id)
                if reason is not None:
                    raise DesktopError(f"frame_id is no longer valid ({reason}); take a new desktop_screenshot")
                raise DesktopError("unknown frame_id; take a new desktop_screenshot and use its frame_id")
            output = frame.view.output
            if self._clock() - frame.issued_at > self._lifetime:
                del self._latest[output]
                self._remember(frame_id, f"it expired after {int(self._lifetime)} seconds")
                raise DesktopError(
                    f"frame_id expired after {int(self._lifetime)} seconds; take a new desktop_screenshot"
                )
            if frame.view.generation != generation:
                del self._latest[output]
                self._remember(frame_id, "the outputs or the session changed after it was taken")
                raise DesktopError("the outputs or the desktop session changed; take a new desktop_screenshot")
            if check is not None:
                check(frame)
            del self._latest[output]
            self._remember(frame_id, "an input action already used it")
            self._retire_all("an input action was sent after it was taken")
            return frame

    def _remember(self, frame_id: str, reason: str) -> None:
        self._spent[frame_id] = reason
        self._spent.move_to_end(frame_id)
        while len(self._spent) > self._SPENT_MEMORY:
            self._spent.popitem(last=False)


# ---------------------------------------------------------------------------
# Keys


@dataclass(frozen=True)
class Modifier:
    name: str
    keysym: int
    # Bit in the xkb modifier mask. xkbcommon always assigns the eight real
    # modifiers the indices Shift=0, Lock=1, Control=2, Mod1..Mod5=3..7.
    mask: int
    xkb_name: str


MODIFIERS: Dict[str, Modifier] = {
    "shift": Modifier("shift", 0xFFE1, 1 << 0, "Shift"),  # Shift_L
    "ctrl": Modifier("ctrl", 0xFFE3, 1 << 2, "Control"),  # Control_L
    "alt": Modifier("alt", 0xFFE9, 1 << 3, "Mod1"),  # Alt_L
    "super": Modifier("super", 0xFFEB, 1 << 6, "Mod4"),  # Super_L
    "altgr": Modifier("altgr", 0xFE03, 1 << 7, "Mod5"),  # ISO_Level3_Shift
}

MODIFIER_ALIASES = {
    "shift": "shift", "lshift": "shift", "rshift": "shift", "shiftl": "shift", "shiftr": "shift",
    "ctrl": "ctrl", "control": "ctrl", "ctl": "ctrl", "lctrl": "ctrl", "rctrl": "ctrl",
    "controll": "ctrl", "controlr": "ctrl", "ctrll": "ctrl", "ctrlr": "ctrl",
    "alt": "alt", "lalt": "alt", "altl": "alt", "option": "alt", "opt": "alt", "mod1": "alt",
    "super": "super", "lsuper": "super", "rsuper": "super", "superl": "super", "superr": "super",
    "meta": "super", "win": "super", "windows": "super", "logo": "super", "cmd": "super",
    "command": "super", "mod4": "super",
    "altgr": "altgr", "ralt": "altgr", "altr": "altgr", "isolevel3shift": "altgr", "mod5": "altgr",
}

_NAMED: Dict[str, int] = {
    "enter": 0xFF0D, "return": 0xFF0D, "ret": 0xFF0D,
    "tab": 0xFF09,
    "escape": 0xFF1B, "esc": 0xFF1B,
    "backspace": 0xFF08, "bksp": 0xFF08,
    "delete": 0xFFFF, "del": 0xFFFF,
    "insert": 0xFF63, "ins": 0xFF63,
    "space": 0x20, "spacebar": 0x20,
    "left": 0xFF51, "arrowleft": 0xFF51, "leftarrow": 0xFF51,
    "up": 0xFF52, "arrowup": 0xFF52, "uparrow": 0xFF52,
    "right": 0xFF53, "arrowright": 0xFF53, "rightarrow": 0xFF53,
    "down": 0xFF54, "arrowdown": 0xFF54, "downarrow": 0xFF54,
    "home": 0xFF50, "end": 0xFF57,
    "pageup": 0xFF55, "pgup": 0xFF55, "prior": 0xFF55,
    "pagedown": 0xFF56, "pgdn": 0xFF56, "next": 0xFF56,
    "menu": 0xFF67, "contextmenu": 0xFF67, "apps": 0xFF67,
    "print": 0xFF61, "printscreen": 0xFF61, "prtsc": 0xFF61, "sysrq": 0xFF61,
    "pause": 0xFF13, "break": 0xFF13,
    "kpenter": 0xFF8D, "kpadd": 0xFFAB, "kpplus": 0xFFAB, "kpsubtract": 0xFFAD, "kpminus": 0xFFAD,
    "kpmultiply": 0xFFAA, "kpdivide": 0xFFAF, "kpdecimal": 0xFFAE,
    "plus": 0x2B, "minus": 0x2D, "equal": 0x3D, "equals": 0x3D, "comma": 0x2C, "period": 0x2E,
    "dot": 0x2E, "slash": 0x2F, "backslash": 0x5C, "semicolon": 0x3B, "colon": 0x3A,
    "apostrophe": 0x27, "quote": 0x27, "grave": 0x60, "backtick": 0x60, "bracketleft": 0x5B,
    "bracketright": 0x5D, "underscore": 0x5F, "asterisk": 0x2A, "at": 0x40, "hash": 0x23,
    "numbersign": 0x23, "dollar": 0x24, "percent": 0x25, "ampersand": 0x26, "exclam": 0x21,
    "question": 0x3F, "less": 0x3C, "greater": 0x3E, "bar": 0x7C, "pipe": 0x7C,
    "braceleft": 0x7B, "braceright": 0x7D, "parenleft": 0x28, "parenright": 0x29, "tilde": 0x7E,
    "asciitilde": 0x7E, "asciicircum": 0x5E, "caret": 0x5E, "quotedbl": 0x22,
    # XF86 media keys.
    "volumeup": 0x1008FF13, "audioraisevolume": 0x1008FF13,
    "volumedown": 0x1008FF11, "audiolowervolume": 0x1008FF11,
    "mute": 0x1008FF12, "volumemute": 0x1008FF12, "audiomute": 0x1008FF12,
    "playpause": 0x1008FF14, "audioplay": 0x1008FF14, "mediaplaypause": 0x1008FF14,
    "audiostop": 0x1008FF15, "mediastop": 0x1008FF15,
    "audioprev": 0x1008FF16, "mediaprevious": 0x1008FF16, "mediaprev": 0x1008FF16,
    "audionext": 0x1008FF17, "medianext": 0x1008FF17,
}
for _n in range(1, 25):
    _NAMED[f"f{_n}"] = 0xFFBE + _n - 1  # F1 = 0xffbe
for _n in range(10):
    _NAMED[f"kp{_n}"] = 0xFFB0 + _n  # KP_0 = 0xffb0

LOCK_KEYS = {"capslock", "numlock", "scrolllock", "shiftlock"}

NAMED_KEYS_HELP = (
    "Enter Tab Escape Backspace Delete Insert Space Left Right Up Down Home End PageUp PageDown "
    "Menu Print Pause F1-F24 KP_0-KP_9 KP_Enter and media keys (VolumeUp VolumeDown Mute PlayPause); "
    "modifiers ctrl shift alt super altgr; or any single character"
)


def normalize_key_name(name: str) -> str:
    return "".join(ch for ch in name.strip().lower() if ch not in "_- ")


def char_keysym(ch: str) -> int:
    """X keysym for one character.

    Latin-1 printable characters use their code point (the keysym values
    0x20-0x7e and 0xa0-0xff are identical to Latin-1); every other character
    uses the Unicode keysym range 0x01000000 + code point. Tab and newline
    map to the Tab and Return keys.
    """
    if ch == "\n":
        return 0xFF0D
    if ch == "\t":
        return 0xFF09
    code = ord(ch)
    if 0x20 <= code <= 0x7E or 0xA0 <= code <= 0xFF:
        return code
    return 0x01000000 + code


def text_keysyms(text) -> List[int]:
    """Validate text for desktop_type_text and convert it to keysyms."""
    if not isinstance(text, str):
        raise DesktopError("text must be a string")
    if not 1 <= len(text) <= MAX_TEXT_CHARS:
        raise DesktopError(f"text must contain 1 to {MAX_TEXT_CHARS} characters")
    for index, ch in enumerate(text):
        category = unicodedata.category(ch)
        if ch in "\t\n":
            continue
        if category == "Cc":
            raise DesktopError(
                f"text contains control character U+{ord(ch):04X} at position {index}; only tab and newline are allowed"
            )
        if category == "Cs":
            raise DesktopError(f"text contains an unpaired surrogate at position {index}")
    return [char_keysym(ch) for ch in text]


@dataclass(frozen=True)
class Chord:
    """Modifiers held (in press order) around at most one ordinary key."""

    modifiers: Tuple[Modifier, ...]
    key: Optional[int]
    label: str

    @property
    def mask(self) -> int:
        result = 0
        for modifier in self.modifiers:
            result |= modifier.mask
        return result


def _split_chord_string(spec: str) -> List[str]:
    if len(spec) <= 1 or "+" not in spec:
        return [spec]
    parts = spec.split("+")
    keys: List[str] = []
    for index, part in enumerate(parts):
        if part == "":
            # "ctrl++" means ctrl and the plus key; keep one "+" for the empty tail.
            if index == len(parts) - 1 and (not keys or keys[-1] != "+"):
                keys.append("+")
            continue
        keys.append(part)
    return keys


def parse_chord(keys: Union[str, Sequence[str]]) -> Chord:
    """Parse ["ctrl", "l"], ["Enter"], "ctrl+shift+t" and similar into a Chord.

    Modifier names are case-insensitive; an ASCII capital letter in a chord is
    sent as shift plus the lower-case letter, as a physical keyboard would.
    """
    if isinstance(keys, str):
        names = _split_chord_string(keys)
    elif isinstance(keys, (list, tuple)):
        names = list(keys)
    else:
        raise DesktopError("keys must be a list of key names such as [\"ctrl\", \"l\"]")
    if not 1 <= len(names) <= MAX_CHORD_KEYS:
        raise DesktopError(f"keys must contain 1 to {MAX_CHORD_KEYS} names")
    modifiers: List[Modifier] = []
    implicit_shift = False
    key: Optional[int] = None
    key_label = ""
    for name in names:
        if not isinstance(name, str) or name == "":
            raise DesktopError("every key must be a non-empty string")
        if len(name) == 1:
            ch = name
            if unicodedata.category(ch) in ("Cc", "Cs"):
                raise DesktopError("control characters are not key names; use Enter, Tab or Escape")
            if key is not None:
                raise DesktopError("a chord can contain only one non-modifier key; send keys in separate calls")
            if "A" <= ch <= "Z":
                if MODIFIERS["shift"] not in modifiers:
                    modifiers.append(MODIFIERS["shift"])
                    implicit_shift = True
                ch = ch.lower()
            key = char_keysym(ch)
            key_label = ch
            continue
        normalized = normalize_key_name(name)
        if normalized in MODIFIER_ALIASES:
            modifier = MODIFIERS[MODIFIER_ALIASES[normalized]]
            if modifier in modifiers:
                if modifier.name == "shift" and implicit_shift:
                    implicit_shift = False
                    continue
                raise DesktopError(f"modifier {modifier.name} appears twice in the chord")
            modifiers.append(modifier)
            continue
        if normalized in LOCK_KEYS:
            raise DesktopError("lock keys (Caps Lock, Num Lock, Scroll Lock) are not supported")
        if normalized not in _NAMED:
            raise DesktopError(f"unknown key name {name!r}; use one of: {NAMED_KEYS_HELP}")
        if key is not None:
            raise DesktopError("a chord can contain only one non-modifier key; send keys in separate calls")
        key = _NAMED[normalized]
        key_label = normalized
    label = "+".join([m.name for m in modifiers] + ([key_label] if key is not None else []))
    return Chord(tuple(modifiers), key, label)


def is_ascii_lower_letter(keysym: Optional[int]) -> bool:
    return keysym is not None and 0x61 <= keysym <= 0x7A
