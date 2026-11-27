"""Keymap generation for the wlroots virtual keyboard."""

import re
import unittest

import cu_support  # noqa: F401

from lunarwing_computer import xkb
from lunarwing_computer.control import parse_chord, text_keysyms


def parse_keymap(text):
    """Return {evdev keycode: (keysyms...)} and {modifier: [evdev codes]} from generated keymap text."""
    assert text.endswith(b"\0") and text.count(b"\0") == 1
    body = text[:-1].decode("ascii")
    names = {name: int(code) for name, code in re.findall(r"<(K\d+)> = (\d+);", body)}
    keys = {}
    for name, symbols in re.findall(r"key <(K\d+)> \{ \[ ([^\]]+) \] \};", body):
        keys[names[name] - 8] = tuple(int(s.strip(), 16) for s in symbols.split(","))
    modmap = {}
    for modifier, name in re.findall(r"modifier_map (\w+) \{ <(K\d+)> \};", body):
        modmap.setdefault(modifier, []).append(names[name] - 8)
    maximum = int(re.search(r"maximum = (\d+);", body).group(1))
    return keys, modmap, maximum, body


class KeymapTests(unittest.TestCase):
    def test_structure(self):
        builder = xkb.KeymapBuilder()
        self.assertEqual(builder.add([0xFFE3], modifier="Control"), 1)
        self.assertEqual(builder.add([0x61, 0x41]), 2)
        self.assertEqual(builder.add([0x61, 0x41]), 2)  # deduplicated
        keymap = builder.build()
        keys, modmap, maximum, body = parse_keymap(keymap.text)
        self.assertEqual(keys, {1: (0xFFE3,), 2: (0x61, 0x41)})
        self.assertEqual(modmap, {"Control": [1]})
        self.assertEqual(maximum, 10)
        self.assertIn('xkb_types "lunarwing" { include "complete" };', body)
        self.assertIn('xkb_compatibility "lunarwing" { include "complete" };', body)
        self.assertTrue(body.startswith("xkb_keymap {"))
        self.assertIn("minimum = 8;", body)

    def test_rejects_keysyms_below_ten_and_too_many_keys(self):
        with self.assertRaises(ValueError):
            xkb.KeymapBuilder().add([5])
        builder = xkb.KeymapBuilder()
        for index in range(xkb.MAX_KEYS):
            builder.add([0x01000100 + index])
        with self.assertRaises(ValueError):
            builder.add([0x01009999])
        _keys, _modmap, maximum, _body = parse_keymap(builder.build().text)
        self.assertEqual(maximum, 255)

    def test_chord_plan(self):
        plan = xkb.plan_chord(parse_chord(["ctrl", "shift", "t"]))
        keys, modmap, _maximum, _body = parse_keymap(plan.keymap.text)
        ctrl, shift = plan.modifier_codes
        self.assertEqual(keys[ctrl], (0xFFE3,))
        self.assertEqual(keys[shift], (0xFFE1,))
        self.assertEqual(keys[plan.key_code], (0x74, 0x54))  # two levels: t, T
        self.assertEqual(modmap, {"Control": [ctrl], "Shift": [shift]})
        plan = xkb.plan_chord(parse_chord(["super"]))
        self.assertIsNone(plan.key_code)
        plan = xkb.plan_chord(parse_chord(["Enter"]))
        self.assertEqual(parse_keymap(plan.keymap.text)[0][plan.key_code], (0xFF0D,))

    def test_text_plan_reconstructs_the_text(self):
        text = "Hello, wörld λ €\n\tok"
        chunks = xkb.plan_text(text_keysyms(text))
        self.assertEqual(len(chunks), 1)
        keys, _modmap, _maximum, _body = parse_keymap(chunks[0].keymap.text)
        typed = [keys[code] for code in chunks[0].codes]
        self.assertTrue(all(len(levels) == 1 for levels in typed))
        self.assertEqual([levels[0] for levels in typed], text_keysyms(text))
        self.assertEqual(len(keys), len(set(text_keysyms(text))))

    def test_text_with_many_distinct_characters_is_chunked(self):
        text = "".join(chr(0x4E00 + i) for i in range(256))
        keysyms = text_keysyms(text)
        chunks = xkb.plan_text(keysyms)
        self.assertEqual([len(c.codes) for c in chunks], [200, 56])
        rebuilt = []
        for chunk in chunks:
            keys, _modmap, maximum, _body = parse_keymap(chunk.keymap.text)
            self.assertLessEqual(maximum, 255)
            self.assertLessEqual(len(keys), xkb.TEXT_CHUNK_KEYS)
            rebuilt += [keys[code][0] for code in chunk.codes]
        self.assertEqual(rebuilt, keysyms)


if __name__ == "__main__":
    unittest.main()
