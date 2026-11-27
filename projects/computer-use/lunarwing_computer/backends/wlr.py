"""Backend for wlroots-style compositors (Hyprland, Sway, labwc, river, Wayfire, niri).

Input uses two Wayland protocols through the connector's own minimal client:

* zwlr_virtual_pointer_manager_v1 / zwlr_virtual_pointer_v1
  (wlr-protocols, unstable/wlr-virtual-pointer-unstable-v1.xml). Requests on
  the pointer, by opcode: 0 motion, 1 motion_absolute(time, x, y, x_extent,
  y_extent), 2 button(time, button, state), 3 axis(time, axis, value),
  4 frame, 5 axis_source(axis_source), 6 axis_stop(time, axis),
  7 axis_discrete(time, axis, value, discrete), 8 destroy. Manager:
  0 create_virtual_pointer(seat, id), 1 destroy, and since version 2
  2 create_virtual_pointer_with_output(seat, output, id). The XML says of
  motion_absolute: "Value of x can range from 0 to x_extent, value of y can
  range from 0 to y_extent", and of frame: "Indicates the set of events that
  logically belong together". Hyprland and wlroots divide x by x_extent as a
  double and map the result into the bound output's logical box, or into the
  bounding box of all outputs when the pointer is not bound to an output, so
  positions are sent in 1/256 logical units for sub-unit precision.
* zwp_virtual_keyboard_manager_v1 / zwp_virtual_keyboard_v1
  (wayland-protocols misc, virtual-keyboard-unstable-v1.xml). Keyboard
  requests: 0 keymap(format, fd, size), 1 key(time, key, state),
  2 modifiers(depressed, latched, locked, group), 3 destroy. Manager:
  0 create_virtual_keyboard(seat, id). A key or modifiers request before a
  keymap is the protocol error no_keymap.

Screenshots come from grim (wlr-screencopy or ext-image-copy-capture),
written to a pipe and never to disk.
"""

from __future__ import annotations

import contextlib
import fcntl
import logging
import os
import shutil
import subprocess
import time
from typing import Any, Callable, Dict, Iterator, List, Mapping, Optional, Sequence, Tuple

from .. import control, imageinfo, session_env, xkb
from ..control import Chord, DesktopError, Rect
from ..wayland_wire import Connection, Proxy, WaylandError
from . import BUTTON_CODES, WLR_REQUIRED, Backend, BackendOptions, Capture, Output

log = logging.getLogger(__name__)

GRIM_TIMEOUT_SECONDS = 10.0
WHEEL_STEP = 15.0  # wl_pointer axis units per wheel detent (libinput degrees; Hyprland assumes 15)
AXIS_VERTICAL = 0  # wl_pointer.axis.vertical_scroll
AXIS_HORIZONTAL = 1  # wl_pointer.axis.horizontal_scroll
AXIS_SOURCE_WHEEL = 0  # wl_pointer.axis_source.wheel
BUTTON_RELEASED, BUTTON_PRESSED = 0, 1  # wl_pointer.button_state
KEY_RELEASED, KEY_PRESSED = 0, 1  # wl_keyboard.key_state
WL_OUTPUT_MODE_CURRENT = 0x1
# motion_absolute positions per logical unit; the same resolution as wl_fixed_t,
# in which clients receive pointer coordinates.
POINTER_SUBUNITS = 256
UINT32_MAX = 0xFFFFFFFF

SEAT_VERSION = 5
OUTPUT_VERSION = 4
XDG_OUTPUT_MANAGER_VERSION = 3
VIRTUAL_POINTER_MANAGER_VERSION = 2
VIRTUAL_KEYBOARD_MANAGER_VERSION = 1

Runner = Callable[[Sequence[str], Mapping[str, str], float], Tuple[int, bytes, bytes]]


def run_subprocess(argv: Sequence[str], env: Mapping[str, str], timeout: float) -> Tuple[int, bytes, bytes]:
    """Run a helper with an argv list (no shell); output stays in pipes."""
    completed = subprocess.run(
        list(argv), stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env=dict(env), timeout=timeout, check=False,
    )
    return completed.returncode, completed.stdout, completed.stderr


def grim_argv(grim: str, output: Output, region: Optional[Rect], scale: float, fmt: str,
              jpeg_quality: int, include_cursor: bool) -> List[str]:
    """grim command line writing one image to stdout ("-")."""
    argv = [grim]
    if include_cursor:
        argv.append("-c")
    argv += ["-t", "jpeg" if fmt == "jpeg" else "png"]
    if fmt == "jpeg":
        argv += ["-q", str(int(jpeg_quality))]
    argv += ["-s", f"{scale:.6f}"]
    if region is None or (region.x, region.y, region.width, region.height) == (0, 0, output.width, output.height):
        argv += ["-o", output.name]
    else:
        # grim -g takes "<x>,<y> <width>x<height>" in global layout coordinates.
        argv += ["-g", f"{output.x + region.x},{output.y + region.y} {region.width}x{region.height}"]
    argv.append("-")
    return argv


def absolute_axis(value: float, extent: int) -> Tuple[int, int]:
    """Position and extent of one motion_absolute axis.

    ``value`` is a logical coordinate in 0..extent. It is sent in
    POINTER_SUBUNITS steps per logical unit, rounded to the nearest step and
    clamped inside the area (the extent itself is the far edge, outside it).
    """
    if extent < 1:
        raise DesktopError("the output has no size")
    units = POINTER_SUBUNITS
    while units > 1 and extent * units > UINT32_MAX:
        units //= 2
    scaled_extent = extent * units
    position = min(max(round(value * units), 0), scaled_extent - 1)
    return position, scaled_extent


def keymap_fd(data: bytes) -> int:
    """An anonymous in-memory file holding the keymap (never on disk)."""
    flags = getattr(os, "MFD_CLOEXEC", 0) | getattr(os, "MFD_ALLOW_SEALING", 0)
    fd = os.memfd_create("lunarwing-keymap", flags)
    try:
        view = memoryview(data)
        while view:
            written = os.write(fd, view)
            view = view[written:]
        os.lseek(fd, 0, os.SEEK_SET)
        seals = 0
        for name in ("F_SEAL_SHRINK", "F_SEAL_GROW", "F_SEAL_WRITE", "F_SEAL_SEAL"):
            seals |= getattr(fcntl, name, 0)
        if seals and hasattr(fcntl, "F_ADD_SEALS"):
            try:
                fcntl.fcntl(fd, fcntl.F_ADD_SEALS, seals)
            except OSError:
                pass  # sealing is a hardening step, not required by the protocol
    except BaseException:
        os.close(fd)
        raise
    return fd


class _OutputState:
    """Accumulates wl_output and zxdg_output_v1 events for one output global."""

    def __init__(self, global_name: int, proxy: Proxy):
        self.global_name = global_name
        self.proxy = proxy
        self.xdg: Optional[Proxy] = None
        self.wl_name: Optional[str] = None
        self.xdg_name: Optional[str] = None
        self.description = ""
        self.geometry_x = 0
        self.geometry_y = 0
        self.transform = 0
        self.mode_width: Optional[int] = None
        self.mode_height: Optional[int] = None
        self.scale_factor = 1
        self.logical_x: Optional[int] = None
        self.logical_y: Optional[int] = None
        self.logical_width: Optional[int] = None
        self.logical_height: Optional[int] = None

    def on_output(self, event: str, args: Tuple[Any, ...]) -> None:
        if event == "geometry":
            self.geometry_x, self.geometry_y = args[0], args[1]
            self.transform = args[7]
        elif event == "mode":
            flags, width, height, _refresh = args
            if flags & WL_OUTPUT_MODE_CURRENT:
                self.mode_width, self.mode_height = width, height
        elif event == "scale":
            self.scale_factor = max(1, args[0])
        elif event == "name":
            self.wl_name = args[0]
        elif event == "description":
            self.description = args[0] or ""

    def on_xdg(self, event: str, args: Tuple[Any, ...]) -> None:
        if event == "logical_position":
            self.logical_x, self.logical_y = args
        elif event == "logical_size":
            self.logical_width, self.logical_height = args
        elif event == "name":
            self.xdg_name = args[0]
        elif event == "description" and not self.description:
            self.description = args[0] or ""

    def snapshot(self) -> Optional[Output]:
        physical_w, physical_h = self.mode_width, self.mode_height
        if physical_w is not None and physical_h is not None and self.transform in (1, 3, 5, 7):
            physical_w, physical_h = physical_h, physical_w  # 90/270 degree transforms swap axes
        if self.logical_width and self.logical_height:
            x, y = self.logical_x or 0, self.logical_y or 0
            width, height = self.logical_width, self.logical_height
        elif physical_w and physical_h:
            # No xdg-output: fall back to the integer wl_output scale.
            x, y = self.geometry_x, self.geometry_y
            width, height = physical_w // self.scale_factor, physical_h // self.scale_factor
        else:
            return None
        if physical_w:
            scale = physical_w / width
        else:
            scale = float(self.scale_factor)
        name = self.xdg_name or self.wl_name or f"output-{self.global_name}"
        return Output(name, x, y, width, height, scale, physical_w, physical_h, self.description)


class WlrBackend(Backend):
    name = "wlr"

    def __init__(self, env: Optional[Mapping[str, str]] = None, options: Optional[BackendOptions] = None,
                 runner: Optional[Runner] = None, which: Callable[[str], Optional[str]] = shutil.which,
                 connector: Optional[Callable[[str], Connection]] = None, socket_path: Optional[str] = None,
                 clock: Callable[[], float] = time.monotonic, sleep: Callable[[float], None] = time.sleep,
                 key_delay: float = 0.002, grim: str = "grim"):
        super().__init__()
        self.env = dict(os.environ if env is None else env)
        self.options = options or BackendOptions()
        self._runner = runner or run_subprocess
        self._which = which
        self._connector = connector or Connection.connect
        self._socket_override = socket_path
        self._clock = clock
        self._sleep = sleep
        self._key_delay = key_delay
        self._grim = grim
        self._reset_state()

    def _reset_state(self) -> None:
        self.conn: Optional[Connection] = None
        self.socket_path: Optional[str] = None
        self._globals: Dict[int, Tuple[str, int]] = {}
        self._ready = False
        self._broken: Optional[str] = None
        self._registry: Optional[Proxy] = None
        self._seat: Optional[Proxy] = None
        self._vp_manager: Optional[Proxy] = None
        self._vk_manager: Optional[Proxy] = None
        self._xdg_manager: Optional[Proxy] = None
        self._outputs: Dict[int, _OutputState] = {}
        self._pending_outputs: List[int] = []
        self._snapshot: List[Output] = []
        self._pointers: Dict[Optional[str], Proxy] = {}
        # The pointer that sent the latest motion. Buttons and scrolling go
        # through it only, never through a new pointer, so they cannot land at
        # a position the latest motion did not set.
        self._active_pointer: Optional[Proxy] = None
        # True when the active pointer was destroyed because its output went away.
        self._pointer_lost = False
        # Button name -> the pointer that pressed it; the release goes to the same pointer.
        self._held_buttons: Dict[str, Proxy] = {}
        self._keyboard_proxy: Optional[Proxy] = None

    # -- session ---------------------------------------------------------------

    @property
    def connected(self) -> bool:
        return self.conn is not None and not self.conn.closed

    def connect(self) -> None:
        if self.connected:
            return
        path = self._socket_override or session_env.resolve_wayland_socket(self.env)
        try:
            conn = self._connector(path)
        except WaylandError as error:
            raise DesktopError(str(error)) from None
        self.conn = conn
        self.socket_path = path
        try:
            self._registry = conn.get_registry(self._on_registry)
            conn.roundtrip()
            offered = {iface for iface, _version in self._globals.values()}
            missing = [name for name in WLR_REQUIRED if name not in offered]
            if missing:
                raise DesktopError(
                    "this compositor does not offer " + ", ".join(missing)
                    + "; on GNOME, KDE or COSMIC use --backend portal"
                )
            registry = self._registry_proxy()
            self._seat = self._bind_first(registry, "wl_seat", SEAT_VERSION)
            self._vp_manager = self._bind_first(registry, "zwlr_virtual_pointer_manager_v1", VIRTUAL_POINTER_MANAGER_VERSION)
            self._vk_manager = self._bind_first(registry, "zwp_virtual_keyboard_manager_v1", VIRTUAL_KEYBOARD_MANAGER_VERSION)
            if "zxdg_output_manager_v1" in offered:
                self._xdg_manager = self._bind_first(registry, "zxdg_output_manager_v1", XDG_OUTPUT_MANAGER_VERSION)
            for global_name, (interface, _version) in sorted(self._globals.items()):
                if interface == "wl_output":
                    self._bind_output(global_name)
            conn.roundtrip()
            self._ready = True
            self._snapshot = self._compute_outputs()
            if not self._snapshot:
                raise DesktopError("the compositor reports no outputs")
            self.generation += 1
        except WaylandError as error:
            self._drop()
            raise DesktopError(f"Wayland setup failed: {error}") from None
        except BaseException:
            self._drop()
            raise

    def _registry_proxy(self) -> Proxy:
        if self._registry is None:
            raise DesktopError("Wayland registry missing")
        return self._registry

    def _bind_first(self, registry: Proxy, interface: str, max_version: int) -> Proxy:
        assert self.conn is not None
        for global_name, (iface, version) in sorted(self._globals.items()):
            if iface == interface:
                return self.conn.bind(registry, global_name, interface, min(version, max_version))
        raise DesktopError(f"{interface} disappeared")

    def _bind_output(self, global_name: int) -> None:
        assert self.conn is not None
        _iface, version = self._globals[global_name]
        proxy = self.conn.bind(self._registry_proxy(), global_name, "wl_output", min(version, OUTPUT_VERSION))
        state = _OutputState(global_name, proxy)
        proxy.handler = state.on_output
        if self._xdg_manager is not None:
            xdg = self.conn.new_proxy("zxdg_output_v1", self._xdg_manager.version)
            xdg.handler = state.on_xdg
            self._xdg_manager.request(1, "no", xdg, proxy)  # get_xdg_output(id, output)
            state.xdg = xdg
        self._outputs[global_name] = state

    def _on_registry(self, event: str, args: Tuple[Any, ...]) -> None:
        if event == "global":
            global_name, interface, version = args
            self._globals[global_name] = (interface or "", version)
            if self._ready and interface == "wl_output":
                self._pending_outputs.append(global_name)
        elif event == "global_remove":
            (global_name,) = args
            interface, _version = self._globals.pop(global_name, ("", 0))
            state = self._outputs.pop(global_name, None)
            if state is not None:
                self._forget_output(state)
            elif interface in WLR_REQUIRED:
                self._broken = f"the compositor removed {interface}"

    def _forget_output(self, state: _OutputState) -> None:
        """Drop a removed output. Runs inside event dispatch, possibly mid-action."""
        snap = state.snapshot()
        if snap is not None:
            pointer = self._pointers.pop(snap.name, None)
            if pointer is not None:
                # A button held through this pointer (a drag) is released on
                # the same device before the device goes away.
                self._release_buttons(pointer)
                if pointer is self._active_pointer:
                    self._active_pointer = None
                    self._pointer_lost = True
                with contextlib.suppress(WaylandError):
                    pointer.destroy(8)  # zwlr_virtual_pointer_v1.destroy
        with contextlib.suppress(WaylandError):
            if state.xdg is not None:
                state.xdg.destroy(0)  # zxdg_output_v1.destroy
            if state.proxy.version >= 3:
                state.proxy.destroy(0)  # wl_output.release (since version 3)
            else:
                state.proxy.alive = False
        # Keep the layout current for the rest of a running action, and
        # invalidate every frame now rather than at the next pump.
        self._snapshot = self._compute_outputs()
        self.generation += 1

    def _release_buttons(self, pointer: Proxy) -> None:
        """Release every button ``pointer`` holds. Best effort; never raises."""
        for button, holder in list(self._held_buttons.items()):
            if holder is not pointer:
                continue
            del self._held_buttons[button]
            with contextlib.suppress(WaylandError):
                pointer.request(2, "uuu", self._time(), BUTTON_CODES[button], BUTTON_RELEASED)  # button
                pointer.request(4)  # frame

    def _output_state_by_name(self, name: str) -> Optional[_OutputState]:
        for state in self._outputs.values():
            snap = state.snapshot()
            if snap is not None and snap.name == name:
                return state
        return None

    def _compute_outputs(self) -> List[Output]:
        result = []
        for _global_name, state in sorted(self._outputs.items()):
            snap = state.snapshot()
            if snap is not None:
                result.append(snap)
        return result

    def pump(self) -> None:
        if not self.connected:
            return
        assert self.conn is not None
        try:
            self.conn.dispatch_pending()
            if self._pending_outputs:
                pending, self._pending_outputs = self._pending_outputs, []
                for global_name in pending:
                    if global_name in self._globals:
                        self._bind_output(global_name)
                self.conn.roundtrip()
        except WaylandError as error:
            log.warning("Wayland connection lost: %s", error)
            self._drop()
            return
        if self._broken:
            log.warning("%s; closing the session", self._broken)
            self._drop()
            return
        snapshot = self._compute_outputs()
        if snapshot != self._snapshot:
            self._snapshot = snapshot
            self.generation += 1

    def outputs(self) -> List[Output]:
        self.pump()
        self._require()
        return list(self._snapshot)

    def _require(self) -> Connection:
        if not self.connected or self.conn is None:
            raise DesktopError("the desktop session is not connected; call desktop_connect")
        return self.conn

    def _drop(self) -> None:
        conn = self.conn
        generation = self.generation
        self._reset_state()
        self.generation = generation + 1
        if conn is not None:
            conn.close()

    def disconnect(self) -> None:
        if self.conn is None:
            return
        try:
            for pointer in list(self._held_buttons.values()):
                self._release_buttons(pointer)
            for pointer in self._pointers.values():
                pointer.destroy(8)  # zwlr_virtual_pointer_v1.destroy
            if self._keyboard_proxy is not None:
                self._keyboard_proxy.destroy(3)  # zwp_virtual_keyboard_v1.destroy
            if self._vp_manager is not None:
                self._vp_manager.destroy(1)  # zwlr_virtual_pointer_manager_v1.destroy
            self.conn.roundtrip(timeout=1.0)
        except (WaylandError, DesktopError):
            pass
        finally:
            self._drop()

    @contextlib.contextmanager
    def _wire(self, action: str) -> Iterator[Connection]:
        """Translate Wayland failures into a DesktopError and drop the session."""
        conn = self._require()
        try:
            yield conn
        except WaylandError as error:
            self._drop()
            raise DesktopError(f"{action} failed and the session was closed: {error}") from None

    # -- capture -----------------------------------------------------------------

    def max_capture_scale(self, output: Output) -> float:
        return output.scale if output.scale > 0 else 1.0

    def capture(self, output: Output, max_edge: int, fmt: str, region: Optional[Rect] = None,
                jpeg_quality: int = 80) -> Capture:
        self._require()
        rect = region or control.full_rect(output.width, output.height)
        scale = control.capture_scale(rect, max_edge, self.max_capture_scale(output))
        argv = grim_argv(self._grim, output, region, scale, fmt, jpeg_quality, self.options.include_cursor)
        assert self.socket_path is not None
        env = session_env.wayland_child_env(self.env, self.socket_path)
        try:
            code, stdout, stderr = self._runner(argv, env, GRIM_TIMEOUT_SECONDS)
        except FileNotFoundError:
            raise DesktopError("grim is not installed; install the grim package to take screenshots") from None
        except subprocess.TimeoutExpired:
            raise DesktopError(
                f"grim did not finish within {GRIM_TIMEOUT_SECONDS:g} s; if the compositor shows a "
                "screen-capture permission prompt, allow it and retry"
            ) from None
        if code != 0:
            detail = stderr.decode("utf-8", "replace").strip().splitlines()
            reason = detail[-1][:200] if detail else "no error output"
            raise DesktopError(f"grim failed with exit status {code}: {reason}")
        try:
            mime, width, height = imageinfo.image_size(stdout)
        except ValueError as error:
            raise DesktopError(f"grim returned an unreadable image ({error})") from None
        expected = "image/jpeg" if fmt == "jpeg" else "image/png"
        if mime != expected:
            raise DesktopError(f"grim returned {mime} instead of {expected}")
        return Capture(stdout, mime, width, height)

    # -- pointer -------------------------------------------------------------------

    def _pointer_for(self, output: Optional[Output]) -> Proxy:
        conn = self._require()
        assert self._vp_manager is not None
        bound = output is not None and self._vp_manager.version >= 2
        key = output.name if bound and output is not None else None
        pointer = self._pointers.get(key)
        if pointer is not None and pointer.alive:
            return pointer
        state: Optional[_OutputState] = None
        if bound and output is not None:
            # Resolve the output before allocating an object id. An id that is
            # allocated but never sent leaves a gap, and libwayland-server
            # disconnects a client whose next new id skips one
            # (wl_map_reserve_new: "count < id" is EINVAL).
            state = self._output_state_by_name(output.name)
            if state is None:
                raise DesktopError(f"output {output.name} is gone; take a new desktop_screenshot")
        pointer = conn.new_proxy("zwlr_virtual_pointer_v1", self._vp_manager.version)
        if state is not None:
            # create_virtual_pointer_with_output(seat, output, id)
            self._vp_manager.request(2, "oon", self._seat, state.proxy, pointer)
        else:
            self._vp_manager.request(0, "on", self._seat, pointer)  # create_virtual_pointer(seat, id)
        self._pointers[key] = pointer
        return pointer

    def _layout_box(self) -> Rect:
        if not self._snapshot:
            raise DesktopError("no outputs known")
        left = min(o.x for o in self._snapshot)
        top = min(o.y for o in self._snapshot)
        right = max(o.x + o.width for o in self._snapshot)
        bottom = max(o.y + o.height for o in self._snapshot)
        return Rect(left, top, right - left, bottom - top)

    def _time(self) -> int:
        return int(self._clock() * 1000) & 0xFFFFFFFF

    def pointer_move_abs(self, output: Output, x: float, y: float) -> None:
        with self._wire("pointer motion") as conn:
            assert self._vp_manager is not None
            if self._output_state_by_name(output.name) is None:
                raise DesktopError(f"output {output.name} is gone; take a new desktop_screenshot")
            pointer = self._pointer_for(output)
            if self._vp_manager.version >= 2:
                px, extent_x = absolute_axis(x, output.width)
                py, extent_y = absolute_axis(y, output.height)
            else:
                box = self._layout_box()
                px, extent_x = absolute_axis(output.x - box.x + x, box.width)
                py, extent_y = absolute_axis(output.y - box.y + y, box.height)
            pointer.request(1, "uuuuu", self._time(), px, py, extent_x, extent_y)  # motion_absolute
            pointer.request(4)  # frame
            self._active_pointer = pointer
            self._pointer_lost = False
            # Wait until the compositor has handled the motion, so "sent" is
            # accurate and a protocol error is reported by this call.
            conn.roundtrip()

    def _motion_pointer(self) -> Optional[Proxy]:
        """The pointer that sent the latest motion, if it still exists.

        Raises if that pointer was destroyed because its output went away:
        a press or scroll through another pointer would land wherever the
        compositor left the cursor, on a screen the model has not seen.
        """
        pointer = self._active_pointer
        if pointer is not None and pointer.alive:
            return pointer
        if self._pointer_lost:
            raise DesktopError("the output under the pointer was removed; take a new desktop_screenshot")
        return None

    def button(self, button: str, pressed: bool) -> None:
        if button not in BUTTON_CODES:
            raise DesktopError("button must be left, middle or right")
        with self._wire("pointer button") as conn:
            if pressed:
                pointer = self._motion_pointer()
                if pointer is None:
                    raise DesktopError("move the pointer before pressing a button")
                state = BUTTON_PRESSED
            else:
                held = self._held_buttons.pop(button, None)
                if held is None:
                    # Not held: never pressed, or already released when the
                    # output of the pointer that pressed it was removed.
                    return
                pointer = held
                if not pointer.alive:
                    pointer = next((p for p in self._pointers.values() if p.alive), None) or self._pointer_for(None)
                state = BUTTON_RELEASED
            pointer.request(2, "uuu", self._time(), BUTTON_CODES[button], state)  # button
            pointer.request(4)  # frame
            if pressed:
                self._held_buttons[button] = pointer
            conn.roundtrip()

    def scroll(self, dx: int, dy: int) -> None:
        with self._wire("scroll") as conn:
            pointer = self._motion_pointer()
            if pointer is None:
                # No motion yet in this session: scroll wherever the cursor is,
                # through a pointer that is not bound to any output. It does not
                # become the motion pointer, so buttons still need a motion first.
                pointer = self._pointer_for(None)
            for axis, steps in ((AXIS_VERTICAL, dy), (AXIS_HORIZONTAL, dx)):
                sign = 1 if steps > 0 else -1
                for _ in range(abs(steps)):
                    # One detent per frame, as a physical wheel reports it:
                    # axis_source precedes the axis data, frame closes the group.
                    pointer.request(5, "u", AXIS_SOURCE_WHEEL)  # axis_source
                    pointer.request(7, "uufi", self._time(), axis, WHEEL_STEP * sign, sign)  # axis_discrete
                    pointer.request(4)  # frame
                    self._sleep(0.01)
            conn.roundtrip()

    # -- keyboard ------------------------------------------------------------------

    def _keyboard(self) -> Proxy:
        conn = self._require()
        if self._keyboard_proxy is None or not self._keyboard_proxy.alive:
            assert self._vk_manager is not None
            keyboard = conn.new_proxy("zwp_virtual_keyboard_v1", 1)
            self._vk_manager.request(0, "on", self._seat, keyboard)  # create_virtual_keyboard(seat, id)
            self._keyboard_proxy = keyboard
        return self._keyboard_proxy

    def _upload_keymap(self, keyboard: Proxy, keymap: xkb.Keymap) -> None:
        # Uploaded on every call, even if unchanged: a compositor may replace
        # the device keymap behind our back (Hyprland re-applies the user's
        # layout to every keyboard on a config reload).
        fd = keymap_fd(keymap.text)
        try:
            # keymap(format, fd, size); size counts the terminating NUL.
            keyboard.request(0, "uhu", xkb.KEYMAP_FORMAT_XKB_V1, fd, len(keymap.text))
        finally:
            os.close(fd)

    def _key(self, keyboard: Proxy, code: int, pressed: bool) -> None:
        keyboard.request(1, "uuu", self._time(), code, KEY_PRESSED if pressed else KEY_RELEASED)

    def _modifiers(self, keyboard: Proxy, depressed: int) -> None:
        keyboard.request(2, "uuuu", depressed, 0, 0, 0)

    def _pause(self) -> None:
        if self._key_delay > 0:
            self._sleep(self._key_delay)

    def _release_all(self, keyboard: Proxy, pressed: List[Tuple[int, int]], mods: int) -> None:
        """Best-effort release after a failure; never raises."""
        try:
            while pressed:
                code, mask = pressed.pop()
                self._key(keyboard, code, False)
                if mask:
                    mods &= ~mask
                    self._modifiers(keyboard, mods)
            if mods:
                self._modifiers(keyboard, 0)
        except (WaylandError, DesktopError):
            pass

    def key_chord(self, chord: Chord, repeat: int = 1) -> None:
        if not 1 <= repeat <= 10:
            raise DesktopError("repeat must be 1 to 10")
        plan = xkb.plan_chord(chord)
        with self._wire("key input") as conn:
            keyboard = self._keyboard()
            self._upload_keymap(keyboard, plan.keymap)
            pressed: List[Tuple[int, int]] = []
            mods = 0
            try:
                for _ in range(repeat):
                    for modifier, code in zip(chord.modifiers, plan.modifier_codes):
                        self._key(keyboard, code, True)
                        pressed.append((code, modifier.mask))
                        mods |= modifier.mask
                        self._modifiers(keyboard, mods)
                    if plan.key_code is not None:
                        self._key(keyboard, plan.key_code, True)
                        pressed.append((plan.key_code, 0))
                        self._pause()
                        self._key(keyboard, plan.key_code, False)
                        pressed.pop()
                    while pressed:
                        code, mask = pressed.pop()
                        self._key(keyboard, code, False)
                        if mask:
                            mods &= ~mask
                            self._modifiers(keyboard, mods)
                    self._pause()
            finally:
                if pressed or mods:
                    self._release_all(keyboard, pressed, mods)
            conn.roundtrip()

    def type_text(self, text: str) -> None:
        chunks = xkb.plan_text(control.text_keysyms(text))
        with self._wire("typing") as conn:
            keyboard = self._keyboard()
            pressed: List[Tuple[int, int]] = []
            try:
                for chunk in chunks:
                    self._upload_keymap(keyboard, chunk.keymap)
                    self._modifiers(keyboard, 0)
                    for code in chunk.codes:
                        self._key(keyboard, code, True)
                        pressed.append((code, 0))
                        self._pause()
                        self._key(keyboard, code, False)
                        pressed.pop()
                        self._pause()
            finally:
                if pressed:
                    self._release_all(keyboard, pressed, 0)
            conn.roundtrip()

    # -- status ----------------------------------------------------------------------

    def describe(self) -> Dict[str, Any]:
        info: Dict[str, Any] = {"backend": self.name, "connected": self.connected}
        if self.socket_path:
            info["wayland_socket"] = self.socket_path
        if self.connected:
            info["globals"] = {
                iface: version for iface, version in sorted(set(self._globals.values()))
                if iface in WLR_REQUIRED or iface in ("wl_output", "zxdg_output_manager_v1")
            }
            if self._vp_manager is not None:
                info["pointer_bound_to_output"] = self._vp_manager.version >= 2
        info["grim"] = self._which(self._grim) or "not found (screenshots will fail)"
        return info
