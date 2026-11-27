"""Shared test fakes. No display, D-Bus, network or PyGObject is needed.

Importing this module also puts the project directory on sys.path, so the
suite works under ``python3 -I -m unittest discover -s tests``.
"""

import logging
import os
import socket
import struct
import sys
import threading
import zlib

PROJECT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if PROJECT not in sys.path:
    sys.path.insert(0, PROJECT)

# Tests trigger error paths on purpose; keep their log records off the console.
logging.getLogger("lunarwing_computer").addHandler(logging.NullHandler())
logging.getLogger("lunarwing_computer").propagate = False

from lunarwing_computer.backends import Backend, Capture, Output  # noqa: E402
from lunarwing_computer.control import DesktopError  # noqa: E402

NATIVE = "<" if sys.byteorder == "little" else ">"


# ---------------------------------------------------------------------------
# Images


def make_png(width, height, padding=0):
    """A valid PNG (one grey row per line); ``padding`` adds an ancillary chunk."""
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0)
    raw = b"".join(b"\0" + b"\x80" * width for _ in range(height))
    body = chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(raw))
    if padding:
        body += chunk(b"tEXt", b"pad\0" + b"x" * padding)
    return b"\x89PNG\r\n\x1a\n" + body + chunk(b"IEND", b"")


def make_jpeg(width, height, padding=0):
    """JPEG-shaped bytes with a real SOI, APP0, SOF0 and EOI (not decodable pixels)."""
    app0 = b"\xff\xe0" + struct.pack(">H", 16) + b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"
    sof0 = b"\xff\xc0" + struct.pack(">HBHHB", 11, 8, height, width, 1) + b"\x01\x11\x00"
    filler = b""
    if padding:
        filler = b"\xff\xfe" + struct.pack(">H", 2 + min(padding, 65000)) + b"c" * min(padding, 65000)
    return b"\xff\xd8" + app0 + filler + sof0 + b"\xff\xd9"


# ---------------------------------------------------------------------------
# A recording backend


class FakeBackend(Backend):
    name = "fake"

    def __init__(self, outputs=None, image_size=None, big_png=False):
        super().__init__()
        self._outputs = outputs or [Output("DP-1", 0, 0, 1600, 1200, 1.0, 1600, 1200)]
        self.events = []
        self.captures = []
        self.active = False
        self.image_size = image_size
        self.big_png = big_png
        self.revoke_on_pump = False
        self.fail_on = None

    @property
    def connected(self):
        return self.active

    def connect(self):
        self.active = True
        self.generation += 1

    def pump(self):
        if self.revoke_on_pump:
            self.active = False

    def outputs(self):
        if not self.active:
            raise DesktopError("not connected")
        return list(self._outputs)

    def set_outputs(self, outputs):
        self._outputs = outputs
        self.generation += 1

    def max_capture_scale(self, output):
        return output.scale

    def capture(self, output, max_edge, fmt, region=None, jpeg_quality=80):
        if not self.active:
            raise DesktopError("not connected")
        self.captures.append((output.name, max_edge, fmt, region, jpeg_quality))
        from lunarwing_computer import control

        rect = region or control.full_rect(output.width, output.height)
        scale = control.capture_scale(rect, max_edge, self.max_capture_scale(output))
        width, height = self.image_size or (int(rect.width * scale), int(rect.height * scale))
        if fmt == "png":
            data = make_png(width, height, padding=3_000_000 if self.big_png else 0)
            return Capture(data, "image/png", width, height)
        return Capture(make_jpeg(width, height), "image/jpeg", width, height)

    def _record(self, *event):
        if self.fail_on is not None and event[0] == self.fail_on:
            raise DesktopError(f"injected failure in {event[0]}")
        self.events.append(event)

    def pointer_move_abs(self, output, x, y):
        self._record("move", output.name, round(x, 3), round(y, 3))

    def button(self, button, pressed):
        self._record("button", button, pressed)

    def scroll(self, dx, dy):
        self._record("scroll", dx, dy)

    def key_chord(self, chord, repeat=1):
        self._record("key", chord.label, repeat)

    def type_text(self, text):
        self._record("type", text)

    def disconnect(self):
        if self.active:
            self.generation += 1
        self.active = False


class FakeSelector:
    def __init__(self, backend, mode="auto"):
        self.backend = backend
        self.mode = mode
        self.selected = 0

    def select(self):
        self.selected += 1
        return self.backend

    def probe(self):
        return {"wayland": {"error": "fake"}, "portal": {"available": False, "reason": "fake"}}


class Clock:
    def __init__(self, now=1000.0):
        self.now = now

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


# ---------------------------------------------------------------------------
# A fake Wayland compositor over a socketpair

# Request tables written independently of the client, from the protocol XML.
REQUESTS = {
    "wl_display": {0: ("sync", "n"), 1: ("get_registry", "n")},
    "wl_registry": {0: ("bind", "usun")},
    "wl_seat": {3: ("release", "")},
    "wl_output": {0: ("release", "")},
    "zxdg_output_manager_v1": {0: ("destroy", ""), 1: ("get_xdg_output", "no")},
    "zxdg_output_v1": {0: ("destroy", "")},
    "zwlr_virtual_pointer_manager_v1": {
        0: ("create_virtual_pointer", "on"), 1: ("destroy", ""), 2: ("create_virtual_pointer_with_output", "oon"),
    },
    "zwlr_virtual_pointer_v1": {
        0: ("motion", "uff"), 1: ("motion_absolute", "uuuuu"), 2: ("button", "uuu"), 3: ("axis", "uuf"),
        4: ("frame", ""), 5: ("axis_source", "u"), 6: ("axis_stop", "uu"), 7: ("axis_discrete", "uufi"),
        8: ("destroy", ""),
    },
    "zwp_virtual_keyboard_manager_v1": {0: ("create_virtual_keyboard", "on")},
    "zwp_virtual_keyboard_v1": {0: ("keymap", "uhu"), 1: ("key", "uuu"), 2: ("modifiers", "uuuu"), 3: ("destroy", "")},
}
DESTRUCTORS = {
    ("wl_seat", "release"), ("wl_output", "release"), ("zxdg_output_manager_v1", "destroy"),
    ("zxdg_output_v1", "destroy"), ("zwlr_virtual_pointer_manager_v1", "destroy"),
    ("zwlr_virtual_pointer_v1", "destroy"), ("zwp_virtual_keyboard_v1", "destroy"),
}


class FakeOutput:
    def __init__(self, name, x, y, logical_width, logical_height, mode_width, mode_height,
                 scale=1, transform=0, description=""):
        self.name = name
        self.x, self.y = x, y
        self.logical_width, self.logical_height = logical_width, logical_height
        self.mode_width, self.mode_height = mode_width, mode_height
        self.scale = scale
        self.transform = transform
        self.description = description


def _pad(n):
    return (n + 3) & ~3


def _encode(signature, args):
    out = b""
    for code, value in zip(signature, args):
        if code == "i":
            out += struct.pack(NATIVE + "i", value)
        elif code in "uon":
            out += struct.pack(NATIVE + "I", value)
        elif code == "s":
            data = value.encode() + b"\0"
            out += struct.pack(NATIVE + "I", len(data)) + data + b"\0" * (_pad(len(data)) - len(data))
    return out


class FakeCompositor:
    """Serves one client over socket.socketpair and records every request.

    Client object ids are checked as libwayland-server does
    (wl_map_reserve_new): a new id may be at most the current map size and must
    not be in use, so an id the client allocated but never sent shows up as an
    "invalid new id" protocol error on the next object it creates.
    """

    def __init__(self, outputs, vp_version=2, with_keyboard=True, with_pointer=True, xdg_version=3,
                 fail_on=None, seat_version=9):
        self.client_socket, self.server_socket = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        self.objects = {1: ("wl_display", 1)}
        # wl_client_create inserts a NULL entry 0, then the display takes id 1.
        self.id_count = 2
        self.live_ids = {1}
        self.globals = {}
        self.output_globals = {}
        self.requests = []
        self.request_targets = []  # object id of each entry in self.requests
        self.keymaps = []
        self.fail_on = fail_on
        self.lock = threading.Lock()
        self.errors_sent = []
        self.closed = threading.Event()
        name = 1
        self.globals[name] = ("wl_seat", seat_version)
        if xdg_version:
            name += 1
            self.globals[name] = ("zxdg_output_manager_v1", xdg_version)
        if with_pointer:
            name += 1
            self.globals[name] = ("zwlr_virtual_pointer_manager_v1", vp_version)
        if with_keyboard:
            name += 1
            self.globals[name] = ("zwp_virtual_keyboard_manager_v1", 1)
        name += 1
        self.globals[name] = ("wl_compositor", 6)
        for output in outputs:
            name += 1
            self.globals[name] = ("wl_output", 4)
            self.output_globals[name] = output
        self._registry_ids = []
        self._buffer = b""
        self._fds = []
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()

    # -- helpers for tests -------------------------------------------------------

    def requests_for(self, interface, name=None):
        with self.lock:
            return [r for r in self.requests if r[0] == interface and (name is None or r[1] == name)]

    def requests_on(self, object_id):
        """Requests sent to one object, as (name, *args)."""
        with self.lock:
            return [r[1:] for r, target in zip(self.requests, self.request_targets) if target == object_id]

    def id_errors(self):
        with self.lock:
            return [e for e in self.errors_sent if "invalid new id" in e[2]]

    def add_output(self, output):
        with self.lock:
            name = max(self.globals) + 1
            self.globals[name] = ("wl_output", 4)
            self.output_globals[name] = output
            for registry in self._registry_ids:
                self._send(registry, 0, "usu", (name, "wl_output", 4))
        return name

    def remove_global(self, name):
        with self.lock:
            self.globals.pop(name, None)
            self.output_globals.pop(name, None)
            for registry in self._registry_ids:
                self._send(registry, 1, "u", (name,))

    def close(self):
        try:
            self.server_socket.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.thread.join(2)
        self.server_socket.close()
        self.client_socket.close()
        for fd in self._fds:
            os.close(fd)

    # -- server loop ---------------------------------------------------------------

    @staticmethod
    def _message(object_id, opcode, signature, args):
        body = _encode(signature, args)
        return struct.pack(NATIVE + "II", object_id, ((8 + len(body)) << 16) | opcode) + body

    def _send(self, object_id, opcode, signature, args):
        self.server_socket.sendall(self._message(object_id, opcode, signature, args))

    def _run(self):
        try:
            while True:
                data, ancdata, _flags, _addr = self.server_socket.recvmsg(65536, socket.CMSG_SPACE(16 * 4))
                for level, kind, payload in ancdata:
                    if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                        count = len(payload) // 4
                        self._fds.extend(struct.unpack(f"{count}i", payload[: count * 4]))
                if not data:
                    break
                self._buffer += data
                while len(self._buffer) >= 8:
                    object_id, word = struct.unpack_from(NATIVE + "II", self._buffer)
                    size, opcode = word >> 16, word & 0xFFFF
                    if len(self._buffer) < size:
                        break
                    body, self._buffer = self._buffer[8:size], self._buffer[size:]
                    with self.lock:
                        self._handle(object_id, opcode, body)
        except OSError:
            pass
        finally:
            self.closed.set()

    def _decode(self, signature, body):
        values, offset = [], 0
        for code in signature:
            if code in "uon":
                values.append(struct.unpack_from(NATIVE + "I", body, offset)[0])
                offset += 4
            elif code == "i":
                values.append(struct.unpack_from(NATIVE + "i", body, offset)[0])
                offset += 4
            elif code == "f":
                values.append(struct.unpack_from(NATIVE + "i", body, offset)[0] / 256.0)
                offset += 4
            elif code == "s":
                length = struct.unpack_from(NATIVE + "I", body, offset)[0]
                offset += 4
                values.append(body[offset:offset + length - 1].decode())
                offset += _pad(length)
            elif code == "h":
                values.append(self._fds.pop(0))
        return values

    def _protocol_error(self, object_id, code, message):
        self.errors_sent.append((object_id, code, message))
        self._send(1, 0, "uus", (object_id, code, message))

    def _reserve_new_id(self, new_id):
        """libwayland-server's wl_map_reserve_new for a client-side id."""
        if new_id > self.id_count or (new_id < self.id_count and new_id in self.live_ids):
            self._protocol_error(1, 1, f"invalid new id {new_id} (count {self.id_count})")
            return False
        if new_id == self.id_count:
            self.id_count += 1
        self.live_ids.add(new_id)
        return True

    def _handle(self, object_id, opcode, body):
        entry = self.objects.get(object_id, ("?", 0))
        interface, version = entry[0], entry[1]
        name, signature = REQUESTS.get(interface, {}).get(opcode, ("?", ""))
        args = self._decode(signature, body)
        self.request_targets.append(object_id)
        if interface == "zwp_virtual_keyboard_v1" and name == "keymap":
            fmt, fd, size = args
            content = os.pread(fd, size + 16, 0)
            os.close(fd)
            self.keymaps.append((fmt, size, content))
            self.requests.append((interface, name, fmt, size))
            self.objects[object_id] = (interface, version, "keymap")
            return
        self.requests.append((interface, name, *args))
        for code, value in zip(signature, args):
            if code == "n" and not self._reserve_new_id(value):
                return
        if self.fail_on == (interface, name):
            self._protocol_error(object_id, 0, f"injected failure in {name}")
            return
        if interface == "zwp_virtual_keyboard_v1" and name in ("key", "modifiers"):
            if len(self.objects[object_id]) < 3:
                self._protocol_error(object_id, 0, "No keymap was set")
                return
        if interface == "wl_display" and name == "sync":
            (callback,) = args
            self.live_ids.discard(callback)
            # done and delete_id in one write, as libwayland flushes them
            # together, so a roundtrip also sees the id being freed.
            self.server_socket.sendall(self._message(callback, 0, "u", (7,)) + self._message(1, 1, "u", (callback,)))
        elif interface == "wl_display" and name == "get_registry":
            (registry,) = args
            self.objects[registry] = ("wl_registry", 1)
            self._registry_ids.append(registry)
            for global_name, (iface, iface_version) in sorted(self.globals.items()):
                self._send(registry, 0, "usu", (global_name, iface, iface_version))
        elif interface == "wl_registry" and name == "bind":
            global_name, iface, bound_version, new_id = args
            self.objects[new_id] = (iface, bound_version)
            if iface == "wl_output":
                self._send_output(new_id, bound_version, self.output_globals[global_name])
                self.objects[new_id] = (iface, bound_version, global_name)
        elif interface == "zxdg_output_manager_v1" and name == "get_xdg_output":
            new_id, output_id = args
            self.objects[new_id] = ("zxdg_output_v1", version)
            output = self.output_globals[self.objects[output_id][2]]
            self._send(new_id, 0, "ii", (output.x, output.y))
            self._send(new_id, 1, "ii", (output.logical_width, output.logical_height))
            if version >= 2:
                self._send(new_id, 3, "s", (output.name,))
            if version < 3:
                self._send(new_id, 2, "", ())
        elif interface == "zwlr_virtual_pointer_manager_v1" and name == "create_virtual_pointer":
            self.objects[args[1]] = ("zwlr_virtual_pointer_v1", version)
        elif interface == "zwlr_virtual_pointer_manager_v1" and name == "create_virtual_pointer_with_output":
            self.objects[args[2]] = ("zwlr_virtual_pointer_v1", version)
        elif interface == "zwp_virtual_keyboard_manager_v1" and name == "create_virtual_keyboard":
            self.objects[args[1]] = ("zwp_virtual_keyboard_v1", 1)
        if (interface, name) in DESTRUCTORS:
            self.live_ids.discard(object_id)
            self.objects.pop(object_id, None)
            self._send(1, 1, "u", (object_id,))

    def _send_output(self, object_id, version, output):
        self._send(object_id, 0, "iiiiissi", (output.x, output.y, 600, 340, 0, "Make", "Model", output.transform))
        self._send(object_id, 1, "uiii", (0x3, output.mode_width, output.mode_height, 60000))
        if version >= 2:
            self._send(object_id, 3, "i", (output.scale,))
        if version >= 4:
            self._send(object_id, 4, "s", (output.name,))
            self._send(object_id, 5, "s", (output.description or output.name,))
        if version >= 2:
            self._send(object_id, 2, "", ())


def wlr_backend_for(compositor, runner=None, **kwargs):
    """A WlrBackend wired to ``compositor`` with no real socket, grim or delays."""
    from lunarwing_computer.backends.wlr import WlrBackend
    from lunarwing_computer.wayland_wire import Connection

    def connector(_path):
        return Connection(compositor.client_socket, timeout=2.0)

    kwargs.setdefault("sleep", lambda _s: None)
    kwargs.setdefault("which", lambda _name: "/usr/bin/grim")
    return WlrBackend(env={"XDG_RUNTIME_DIR": "/run/user/test"}, runner=runner or FakeRunner(),
                      connector=connector, socket_path="/run/user/test/wayland-9", **kwargs)


class FakeRunner:
    """Stands in for subprocess: records argv/env and returns canned images."""

    def __init__(self, png_size=(1280, 800), png_padding=0, jpeg_size=None, code=0, stderr=b"", exc=None):
        self.calls = []
        self.png_size = png_size
        self.png_padding = png_padding
        self.jpeg_size = jpeg_size or png_size
        self.code = code
        self.stderr = stderr
        self.exc = exc

    def __call__(self, argv, env, timeout):
        self.calls.append((list(argv), dict(env), timeout))
        if self.exc is not None:
            raise self.exc
        if self.code:
            return self.code, b"", self.stderr
        if "jpeg" in argv:
            return 0, make_jpeg(*self.jpeg_size), b""
        return 0, make_png(*self.png_size, padding=self.png_padding), b""
