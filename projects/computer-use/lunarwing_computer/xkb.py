"""Generate small XKB keymaps for the wlroots virtual keyboard.

zwp_virtual_keyboard_v1 requires the client to upload its own keymap. Like
wtype, the connector writes a fresh keymap for each call that assigns every
keysym it needs to a free keycode, so any Unicode text and any named key can
be typed regardless of the user's own keyboard layout.

Keycodes are written in xkb numbering (evdev + 8) and kept at or below 255 so
clients that only handle 8-bit keycodes (Xwayland) still work. Keysyms are
written as hexadecimal numbers; xkbcommon resolves an integer keysym of 10 or
more through xkb_keysym_from_name("0x..."), so no keysym-name table is needed.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Dict, List, Optional, Sequence, Tuple

from .control import Chord, is_ascii_lower_letter

FIRST_XKB_KEYCODE = 9  # evdev keycode 1; xkb keycode = evdev keycode + 8
MAX_XKB_KEYCODE = 255
MAX_KEYS = MAX_XKB_KEYCODE - FIRST_XKB_KEYCODE + 1  # 247
TEXT_CHUNK_KEYS = 200
EVDEV_OFFSET = 8
KEYMAP_FORMAT_XKB_V1 = 1  # wl_keyboard.keymap_format.xkb_v1


@dataclass(frozen=True)
class Keymap:
    """A keymap text (NUL-terminated, as sent over the wire) and its key assignment."""

    text: bytes
    levels: Tuple[Tuple[int, ...], ...]  # per key, in keycode order

    def evdev_code(self, index: int) -> int:
        return FIRST_XKB_KEYCODE + index - EVDEV_OFFSET


class KeymapBuilder:
    def __init__(self) -> None:
        self._keys: List[Tuple[int, ...]] = []
        self._index: Dict[Tuple[int, ...], int] = {}
        self._modmap: Dict[int, str] = {}

    def __len__(self) -> int:
        return len(self._keys)

    def add(self, levels: Sequence[int], modifier: Optional[str] = None) -> int:
        """Add a key (deduplicated) and return its evdev keycode."""
        entry = tuple(levels)
        if not entry:
            raise ValueError("a key needs at least one keysym")
        for keysym in entry:
            if not 10 <= keysym <= 0x1FFFFFFF:
                raise ValueError(f"keysym 0x{keysym:x} is outside the range this keymap writer supports")
        index = self._index.get(entry)
        if index is None:
            if len(self._keys) >= MAX_KEYS:
                raise ValueError("too many distinct keys for one keymap")
            index = len(self._keys)
            self._keys.append(entry)
            self._index[entry] = index
        if modifier is not None:
            self._modmap[index] = modifier
        return FIRST_XKB_KEYCODE + index - EVDEV_OFFSET

    def build(self) -> Keymap:
        if not self._keys:
            raise ValueError("empty keymap")
        last = FIRST_XKB_KEYCODE + len(self._keys) - 1
        lines = [
            "xkb_keymap {",
            'xkb_keycodes "lunarwing" {',
            "minimum = 8;",
            f"maximum = {max(last, FIRST_XKB_KEYCODE)};",
        ]
        for index in range(len(self._keys)):
            lines.append(f"<K{index + 1}> = {FIRST_XKB_KEYCODE + index};")
        lines += [
            "};",
            'xkb_types "lunarwing" { include "complete" };',
            'xkb_compatibility "lunarwing" { include "complete" };',
            'xkb_symbols "lunarwing" {',
        ]
        for index, levels in enumerate(self._keys):
            symbols = ", ".join(f"0x{keysym:x}" for keysym in levels)
            lines.append(f"key <K{index + 1}> {{ [ {symbols} ] }};")
        for index, modifier in sorted(self._modmap.items()):
            lines.append(f"modifier_map {modifier} {{ <K{index + 1}> }};")
        lines += ["};", "};", ""]
        text = "\n".join(lines).encode("ascii") + b"\0"
        return Keymap(text, tuple(self._keys))


@dataclass(frozen=True)
class ChordPlan:
    keymap: Keymap
    modifier_codes: Tuple[int, ...]  # evdev codes, same order as chord.modifiers
    key_code: Optional[int]


def plan_chord(chord: Chord) -> ChordPlan:
    builder = KeymapBuilder()
    modifier_codes = tuple(builder.add([m.keysym], modifier=m.xkb_name) for m in chord.modifiers)
    key_code = None
    if chord.key is not None:
        if is_ascii_lower_letter(chord.key):
            # Two levels like a real keyboard, so shift+letter yields the capital.
            levels: Tuple[int, ...] = (chord.key, chord.key - 0x20)
        else:
            levels = (chord.key,)
        key_code = builder.add(levels)
    return ChordPlan(builder.build(), modifier_codes, key_code)


@dataclass(frozen=True)
class TextChunk:
    keymap: Keymap
    codes: Tuple[int, ...]  # evdev code for each character of the chunk, in order


def plan_text(keysyms: Sequence[int], chunk_keys: int = TEXT_CHUNK_KEYS) -> List[TextChunk]:
    """Split text into chunks whose distinct keysyms fit one keymap.

    Each character gets a single-level key, so it is typed without any
    modifier and the result does not depend on shift or lock state.
    """
    chunks: List[TextChunk] = []
    builder = KeymapBuilder()
    codes: List[int] = []
    seen = set()
    for keysym in keysyms:
        if keysym not in seen and len(seen) >= chunk_keys:
            chunks.append(TextChunk(builder.build(), tuple(codes)))
            builder, codes, seen = KeymapBuilder(), [], set()
        seen.add(keysym)
        codes.append(builder.add([keysym]))
    if codes:
        chunks.append(TextChunk(builder.build(), tuple(codes)))
    return chunks
