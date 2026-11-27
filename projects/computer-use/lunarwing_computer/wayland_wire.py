"""A minimal pure-Python Wayland client (wire protocol only).

It implements just what the wlroots backend needs: the wl_display and
wl_registry handshake, sync round trips, binding globals at explicit
versions, a request encoder and a decoder for a fixed set of events.

Wire format (wayland.xml, "Wire Format" in the Wayland book): every message
starts with the 32-bit object id and a 32-bit word whose upper 16 bits are
the message size in bytes (header included) and whose lower 16 bits are the
opcode, all in host byte order. Arguments are 32-bit aligned: int and uint
are 32 bits, fixed is a signed 24.8 number, string is a uint length that
counts the terminating NUL followed by the bytes padded to 4, array is a uint
byte length followed by padded data, object and new_id are uint ids, and an
fd travels out of band as SCM_RIGHTS ancillary data. An untyped new_id
(wl_registry.bind) is sent as interface string, version and id.
"""

from __future__ import annotations

import array
import collections
import errno
import os
import select
import socket
import struct
import sys
import time
from typing import Any, Callable, Deque, Dict, List, Optional, Sequence, Tuple

DISPLAY_ID = 1
MAX_MESSAGE_SIZE = 4096  # WL_MAX_MESSAGE_SIZE in libwayland
DEFAULT_TIMEOUT = 5.0
_NATIVE = "<" if sys.byteorder == "little" else ">"

# Event signatures for the interfaces this client listens to.
# i int, u uint, f fixed, s string, o object id, n new_id, a array, h fd.
EVENTS: Dict[str, Dict[int, Tuple[str, str]]] = {
    "wl_display": {0: ("error", "ous"), 1: ("delete_id", "u")},
    "wl_registry": {0: ("global", "usu"), 1: ("global_remove", "u")},
    "wl_callback": {0: ("done", "u")},
    "wl_seat": {0: ("capabilities", "u"), 1: ("name", "s")},
    "wl_output": {
        0: ("geometry", "iiiiissi"),
        1: ("mode", "uiii"),
        2: ("done", ""),
        3: ("scale", "i"),
        4: ("name", "s"),
        5: ("description", "s"),
    },
    "zxdg_output_v1": {
        0: ("logical_position", "ii"),
        1: ("logical_size", "ii"),
        2: ("done", ""),
        3: ("name", "s"),
        4: ("description", "s"),
    },
}

EventHandler = Callable[[str, Tuple[Any, ...]], None]


class WaylandError(Exception):
    """Connection-level failure (socket closed, timeout, malformed data)."""


class ProtocolError(WaylandError):
    """The compositor sent wl_display.error; the connection is now unusable."""

    def __init__(self, object_id: int, interface: str, code: int, message: str):
        self.object_id = object_id
        self.interface = interface
        self.code = code
        self.message = message
        super().__init__(f"Wayland protocol error on {interface}@{object_id} (code {code}): {message}")


class Proxy:
    """Client-side handle for one protocol object."""

    __slots__ = ("conn", "id", "interface", "version", "handler", "alive")

    def __init__(self, conn: "Connection", object_id: int, interface: str, version: int):
        self.conn = conn
        self.id = object_id
        self.interface = interface
        self.version = version
        self.handler: Optional[EventHandler] = None
        self.alive = True

    def request(self, opcode: int, signature: str = "", *args: Any) -> None:
        if not self.alive:
            raise WaylandError(f"request on destroyed {self.interface}@{self.id}")
        self.conn.send_request(self.id, opcode, signature, args)

    def destroy(self, opcode: Optional[int] = None) -> None:
        """Send the destructor request (if any) and stop dispatching events.

        The id is reused only after the compositor confirms with delete_id.
        """
        if not self.alive:
            return
        if opcode is not None:
            self.conn.send_request(self.id, opcode, "", ())
        self.alive = False

    def __repr__(self) -> str:
        return f"<{self.interface}@{self.id} v{self.version}>"


def fixed_from_float(value: float) -> int:
    """Encode a float as wl_fixed_t (signed 24.8)."""
    raw = int(round(value * 256.0))
    if not -0x80000000 <= raw <= 0x7FFFFFFF:
        raise ValueError("fixed value out of range")
    return raw


def fixed_to_float(raw: int) -> float:
    return raw / 256.0


def _pad(length: int) -> int:
    return (length + 3) & ~3


def encode_args(signature: str, args: Sequence[Any]) -> Tuple[bytes, List[int]]:
    """Encode request arguments; returns (body, fds)."""
    if len(signature) != len(args):
        raise ValueError(f"signature {signature!r} needs {len(signature)} arguments, got {len(args)}")
    parts: List[bytes] = []
    fds: List[int] = []
    for code, value in zip(signature, args):
        if code == "i":
            parts.append(struct.pack(_NATIVE + "i", int(value)))
        elif code == "u":
            value = int(value)
            if not 0 <= value <= 0xFFFFFFFF:
                raise ValueError("uint argument out of range")
            parts.append(struct.pack(_NATIVE + "I", value))
        elif code == "f":
            parts.append(struct.pack(_NATIVE + "i", fixed_from_float(float(value))))
        elif code == "s":
            if value is None:
                parts.append(struct.pack(_NATIVE + "I", 0))
            else:
                data = str(value).encode("utf-8") + b"\0"
                parts.append(struct.pack(_NATIVE + "I", len(data)) + data + b"\0" * (_pad(len(data)) - len(data)))
        elif code in "on":
            if value is None:
                object_id = 0
            elif isinstance(value, Proxy):
                object_id = value.id
            else:
                object_id = int(value)
            parts.append(struct.pack(_NATIVE + "I", object_id))
        elif code == "a":
            data = bytes(value)
            parts.append(struct.pack(_NATIVE + "I", len(data)) + data + b"\0" * (_pad(len(data)) - len(data)))
        elif code == "h":
            fds.append(int(value))
        else:
            raise ValueError(f"unknown signature code {code!r}")
    return b"".join(parts), fds


def encode_message(object_id: int, opcode: int, signature: str, args: Sequence[Any]) -> Tuple[bytes, List[int]]:
    body, fds = encode_args(signature, args)
    size = 8 + len(body)
    if size > MAX_MESSAGE_SIZE:
        raise ValueError("Wayland message too large")
    header = struct.pack(_NATIVE + "II", object_id, (size << 16) | opcode)
    return header + body, fds


def decode_args(signature: str, body: bytes, fds: Deque[int]) -> Tuple[Any, ...]:
    """Decode event arguments. Raises WaylandError on truncated data."""
    values: List[Any] = []
    offset = 0
    try:
        for code in signature:
            if code == "i":
                (value,) = struct.unpack_from(_NATIVE + "i", body, offset)
                offset += 4
            elif code in "uon":
                (value,) = struct.unpack_from(_NATIVE + "I", body, offset)
                offset += 4
            elif code == "f":
                (raw,) = struct.unpack_from(_NATIVE + "i", body, offset)
                value = fixed_to_float(raw)
                offset += 4
            elif code == "s":
                (length,) = struct.unpack_from(_NATIVE + "I", body, offset)
                offset += 4
                if length == 0:
                    value = None
                else:
                    if offset + length > len(body):
                        raise WaylandError("truncated string in Wayland event")
                    value = body[offset:offset + length - 1].decode("utf-8", "replace")
                    offset += _pad(length)
            elif code == "a":
                (length,) = struct.unpack_from(_NATIVE + "I", body, offset)
                offset += 4
                if offset + length > len(body):
                    raise WaylandError("truncated array in Wayland event")
                value = body[offset:offset + length]
                offset += _pad(length)
            elif code == "h":
                if not fds:
                    raise WaylandError("event expects a file descriptor that was not received")
                value = fds.popleft()
            else:
                raise WaylandError(f"unknown signature code {code!r}")
            values.append(value)
    except struct.error:
        raise WaylandError("truncated Wayland event") from None
    return tuple(values)


class Connection:
    """One client connection to a compositor socket."""

    def __init__(self, sock: socket.socket, timeout: float = DEFAULT_TIMEOUT, clock: Callable[[], float] = time.monotonic):
        self.sock = sock
        self.timeout = timeout
        self._clock = clock
        self.objects: Dict[int, Proxy] = {}
        self._next_id = DISPLAY_ID + 1
        self._free_ids: List[int] = []
        self._buffer = bytearray()
        self._fds: Deque[int] = collections.deque()
        self._closed = False
        self._error: Optional[WaylandError] = None
        self.display = Proxy(self, DISPLAY_ID, "wl_display", 1)
        self.objects[DISPLAY_ID] = self.display
        self.display.handler = self._display_event
        self.sock.setblocking(False)

    # -- connection management -------------------------------------------------

    @classmethod
    def connect(cls, path: str, timeout: float = DEFAULT_TIMEOUT) -> "Connection":
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM | socket.SOCK_CLOEXEC)
        sock.settimeout(timeout)
        try:
            sock.connect(path)
        except OSError as error:
            sock.close()
            raise WaylandError(f"cannot connect to Wayland socket {path}: {error.strerror or error}") from None
        return cls(sock, timeout=timeout)

    @property
    def closed(self) -> bool:
        return self._closed

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        try:
            self.sock.close()
        finally:
            while self._fds:
                try:
                    os.close(self._fds.popleft())
                except OSError:
                    pass

    def _check_usable(self) -> None:
        if self._error is not None:
            raise self._error
        if self._closed:
            raise WaylandError("Wayland connection is closed")

    # -- ids ---------------------------------------------------------------------

    def allocate_id(self) -> int:
        # Client-side ids run from 1 (wl_display) upward; ids freed by the
        # compositor's delete_id are reused, as libwayland does.
        if self._free_ids:
            return self._free_ids.pop()
        object_id = self._next_id
        if object_id >= 0xFF000000:
            raise WaylandError("client object ids exhausted")
        self._next_id += 1
        return object_id

    def new_proxy(self, interface: str, version: int) -> Proxy:
        proxy = Proxy(self, self.allocate_id(), interface, version)
        self.objects[proxy.id] = proxy
        return proxy

    def _display_event(self, name: str, args: Tuple[Any, ...]) -> None:
        if name == "error":
            object_id, code, message = args
            target = self.objects.get(object_id)
            interface = target.interface if target is not None else "unknown"
            self._error = ProtocolError(object_id, interface, code, message or "")
            raise self._error
        if name == "delete_id":
            (object_id,) = args
            proxy = self.objects.pop(object_id, None)
            if proxy is not None:
                proxy.alive = False
            if object_id > DISPLAY_ID and object_id not in self._free_ids:
                self._free_ids.append(object_id)

    # -- sending -----------------------------------------------------------------

    def send_request(self, object_id: int, opcode: int, signature: str, args: Sequence[Any]) -> None:
        self._check_usable()
        data, fds = encode_message(object_id, opcode, signature, args)
        self._send(data, fds)

    def _send(self, data: bytes, fds: Sequence[int]) -> None:
        view = memoryview(data)
        ancillary = []
        if fds:
            ancillary = [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", fds).tobytes())]
        deadline = self._clock() + self.timeout
        while view:
            try:
                sent = self.sock.sendmsg([view], ancillary)
            except (BlockingIOError, InterruptedError):
                self._wait(select.POLLOUT, deadline)
                continue
            except OSError as error:
                self._fail(WaylandError(f"Wayland connection lost while sending: {error.strerror or error}"))
                raise self._error from None
            view = view[sent:]
            ancillary = []  # the fds travel with the first chunk only

    def _wait(self, events: int, deadline: float) -> None:
        remaining = deadline - self._clock()
        if remaining <= 0:
            raise WaylandError(f"compositor did not respond within {self.timeout:g} s")
        poller = select.poll()
        poller.register(self.sock.fileno(), events)
        if not poller.poll(int(remaining * 1000) + 1):
            raise WaylandError(f"compositor did not respond within {self.timeout:g} s")

    def _fail(self, error: WaylandError) -> None:
        if self._error is None:
            self._error = error

    # -- receiving ---------------------------------------------------------------

    def _read(self, block_until: Optional[float]) -> bool:
        """Read available bytes; returns False if nothing arrived (non-blocking)."""
        while True:
            try:
                data, ancdata, _flags, _addr = self.sock.recvmsg(65536, socket.CMSG_SPACE(28 * 4))
            except (BlockingIOError, InterruptedError):
                if block_until is None:
                    return False
                self._wait(select.POLLIN, block_until)
                continue
            except OSError as error:
                if error.errno == errno.EAGAIN:
                    if block_until is None:
                        return False
                    self._wait(select.POLLIN, block_until)
                    continue
                self._fail(WaylandError(f"Wayland connection lost: {error.strerror or error}"))
                raise self._error from None
            for level, kind, payload in ancdata:
                if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                    received = array.array("i")
                    received.frombytes(payload[: len(payload) - (len(payload) % received.itemsize)])
                    for fd in received:
                        if len(self._fds) >= 28:
                            os.close(fd)
                        else:
                            self._fds.append(fd)
            if not data:
                self._fail(WaylandError("compositor closed the Wayland connection"))
                raise self._error
            self._buffer.extend(data)
            return True

    def _process(self) -> None:
        while len(self._buffer) >= 8:
            object_id, word = struct.unpack_from(_NATIVE + "II", self._buffer, 0)
            size = word >> 16
            opcode = word & 0xFFFF
            if size < 8 or size % 4:
                self._fail(WaylandError("malformed Wayland message from compositor"))
                raise self._error
            if len(self._buffer) < size:
                return
            body = bytes(self._buffer[8:size])
            del self._buffer[:size]
            proxy = self.objects.get(object_id)
            if proxy is None:
                continue  # object already destroyed on our side
            spec = EVENTS.get(proxy.interface, {}).get(opcode)
            if spec is None:
                continue  # an event this client does not use
            name, signature = spec
            args = decode_args(signature, body, self._fds)
            if proxy.handler is not None and (proxy.alive or proxy.interface == "wl_display"):
                proxy.handler(name, args)

    def dispatch(self, timeout: Optional[float] = None) -> None:
        """Block until at least one chunk of events arrives, then handle them."""
        self._check_usable()
        wait = self.timeout if timeout is None else timeout
        self._read(self._clock() + wait)
        self._process()

    def dispatch_pending(self) -> None:
        """Handle whatever events are already queued, without blocking."""
        self._check_usable()
        while self._read(None):
            pass
        self._process()

    # -- core requests -------------------------------------------------------------

    def get_registry(self, handler: Optional[EventHandler] = None) -> Proxy:
        registry = self.new_proxy("wl_registry", 1)
        registry.handler = handler
        self.send_request(DISPLAY_ID, 1, "n", (registry,))  # wl_display.get_registry
        return registry

    def sync(self) -> Tuple[Proxy, List[bool]]:
        callback = self.new_proxy("wl_callback", 1)
        done = [False]

        def on_done(name: str, _args: Tuple[Any, ...]) -> None:
            if name == "done":
                done[0] = True

        callback.handler = on_done
        self.send_request(DISPLAY_ID, 0, "n", (callback,))  # wl_display.sync
        return callback, done

    def roundtrip(self, timeout: Optional[float] = None) -> None:
        """Wait until the compositor has handled every request sent so far."""
        _callback, done = self.sync()
        deadline = self._clock() + (self.timeout if timeout is None else timeout)
        self._process()
        while not done[0]:
            remaining = deadline - self._clock()
            if remaining <= 0:
                raise WaylandError(f"compositor did not answer a sync within {self.timeout:g} s")
            self.dispatch(remaining)

    def bind(self, registry: Proxy, name: int, interface: str, version: int) -> Proxy:
        proxy = self.new_proxy(interface, version)
        # wl_registry.bind(name: uint, id: new_id) with an untyped new_id.
        self.send_request(registry.id, 0, "usun", (name, interface, version, proxy))
        return proxy


def list_globals(path: str, timeout: float = DEFAULT_TIMEOUT) -> Dict[int, Tuple[str, int]]:
    """Connect, read the registry, disconnect. Read-only; binds nothing."""
    conn = Connection.connect(path, timeout=timeout)
    found: Dict[int, Tuple[str, int]] = {}

    def on_registry(name: str, args: Tuple[Any, ...]) -> None:
        if name == "global":
            global_name, interface, version = args
            found[global_name] = (interface or "", version)
        elif name == "global_remove":
            found.pop(args[0], None)

    try:
        conn.get_registry(on_registry)
        conn.roundtrip()
    finally:
        conn.close()
    return found
