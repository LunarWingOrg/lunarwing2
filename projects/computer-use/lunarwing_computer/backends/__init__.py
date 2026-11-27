"""Backend interface and backend selection.

A backend owns one connection to the desktop session. All coordinates passed
to a backend are compositor logical coordinates relative to the origin of the
given output; mapping from screenshot pixels happens in the controller.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Mapping, NamedTuple, Optional

from ..control import Chord, DesktopError, Rect

BUTTON_CODES = {"left": 0x110, "right": 0x111, "middle": 0x112}  # linux/input-event-codes.h BTN_*

WLR_REQUIRED = ("wl_seat", "zwlr_virtual_pointer_manager_v1", "zwp_virtual_keyboard_manager_v1")
PORTAL_INTERFACE = "org.freedesktop.portal.RemoteDesktop"


@dataclass(frozen=True)
class Output:
    """One monitor as the compositor lays it out.

    ``x``, ``y``, ``width`` and ``height`` are in logical (layout) coordinates.
    ``scale`` is physical pixels per logical unit (1.5 for a 2560x1600 panel
    shown as 1707x1067), used to avoid capturing above native resolution.
    """

    name: str
    x: int
    y: int
    width: int
    height: int
    scale: float = 1.0
    physical_width: Optional[int] = None
    physical_height: Optional[int] = None
    description: str = ""

    def as_dict(self) -> Dict[str, Any]:
        data: Dict[str, Any] = {
            "name": self.name,
            "logical": {"x": self.x, "y": self.y, "width": self.width, "height": self.height},
            "scale": round(self.scale, 4),
        }
        if self.physical_width and self.physical_height:
            data["physical"] = {"width": self.physical_width, "height": self.physical_height}
        if self.description:
            data["description"] = self.description
        return data


class Capture(NamedTuple):
    data: bytes
    mime: str
    width: int
    height: int


class Backend:
    """Interface every desktop backend implements."""

    name = "base"

    def __init__(self) -> None:
        self.generation = 0

    @property
    def connected(self) -> bool:
        raise NotImplementedError

    def connect(self) -> None:
        """Open the session (for the portal this shows the consent dialog)."""
        raise NotImplementedError

    def pump(self) -> None:
        """Handle pending events without blocking (output changes, revocation)."""

    def outputs(self) -> List[Output]:
        raise NotImplementedError

    def max_capture_scale(self, output: Output) -> float:
        """Largest useful image pixels per logical unit for this output."""
        return 1.0

    def capture(self, output: Output, max_edge: int, fmt: str, region: Optional[Rect] = None,
                jpeg_quality: int = 80) -> Capture:
        """Capture ``region`` (logical, output-relative; whole output if None)."""
        raise NotImplementedError

    def pointer_move_abs(self, output: Output, x: float, y: float) -> None:
        raise NotImplementedError

    def button(self, button: str, pressed: bool) -> None:
        raise NotImplementedError

    def scroll(self, dx: int, dy: int) -> None:
        """Scroll by wheel steps; positive dy scrolls down, positive dx right."""
        raise NotImplementedError

    def key_chord(self, chord: Chord, repeat: int = 1) -> None:
        """Press the chord's modifiers in order, then its key, and release all in reverse."""
        raise NotImplementedError

    def type_text(self, text: str) -> None:
        raise NotImplementedError

    def disconnect(self) -> None:
        raise NotImplementedError

    def describe(self) -> Dict[str, Any]:
        return {"backend": self.name, "connected": self.connected}


@dataclass
class BackendOptions:
    restore_token_file: Optional[str] = None
    include_cursor: bool = True


# ---------------------------------------------------------------------------
# Probing and selection


def probe_wayland(env: Mapping[str, str]) -> Dict[str, Any]:
    """Read-only registry listing: which wlroots-style globals the compositor offers."""
    from .. import session_env, wayland_wire

    path = session_env.resolve_wayland_socket(env)
    try:
        found = wayland_wire.list_globals(path, timeout=3.0)
    except wayland_wire.WaylandError as error:
        raise DesktopError(str(error)) from None
    versions: Dict[str, int] = {}
    for interface, version in found.values():
        versions[interface] = max(version, versions.get(interface, 0))
    return {"socket": path, "globals": versions}


def probe_portal(env: Mapping[str, str]) -> Dict[str, Any]:
    """Check that the RemoteDesktop portal interface exists (read-only property read)."""
    from .portal import probe_remote_desktop

    return probe_remote_desktop(env)


@dataclass
class Selector:
    """Pick a backend for ``--backend auto|wlr|portal``.

    The probes and factories are injectable so selection can be tested
    without a compositor or a session bus.
    """

    mode: str
    env: Mapping[str, str]
    options: BackendOptions = field(default_factory=BackendOptions)
    wayland_probe: Callable[[Mapping[str, str]], Dict[str, Any]] = probe_wayland
    portal_probe: Callable[[Mapping[str, str]], Dict[str, Any]] = probe_portal
    wlr_factory: Optional[Callable[..., Backend]] = None
    portal_factory: Optional[Callable[..., Backend]] = None

    def _make_wlr(self) -> Backend:
        if self.wlr_factory is not None:
            return self.wlr_factory(env=self.env, options=self.options)
        from .wlr import WlrBackend

        return WlrBackend(env=self.env, options=self.options)

    def _make_portal(self) -> Backend:
        if self.portal_factory is not None:
            return self.portal_factory(env=self.env, options=self.options)
        from .portal import PortalBackend

        return PortalBackend(env=self.env, options=self.options)

    def probe(self) -> Dict[str, Any]:
        """Report what auto-selection would see. Never raises."""
        report: Dict[str, Any] = {"mode": self.mode}
        if self.mode in ("auto", "wlr"):
            self._probe_wayland_into(report)
        if self.mode in ("auto", "portal"):
            try:
                report["portal"] = self.portal_probe(self.env)
            except DesktopError as error:
                report["portal"] = {"error": str(error)}
        return report

    def _probe_wayland_into(self, report: Dict[str, Any]) -> None:
        try:
            wayland = self.wayland_probe(self.env)
            missing = [name for name in WLR_REQUIRED if name not in wayland["globals"]]
            report["wayland"] = {
                "socket": wayland["socket"],
                "wlr_protocols": not missing,
                "missing": missing,
                "versions": {k: v for k, v in sorted(wayland["globals"].items()) if k in _INTERESTING_GLOBALS},
            }
        except DesktopError as error:
            report["wayland"] = {"error": str(error)}

    def select(self) -> Backend:
        if self.mode == "wlr":
            return self._make_wlr()
        if self.mode == "portal":
            return self._make_portal()
        if self.mode != "auto":
            raise DesktopError(f"unknown backend {self.mode!r}")
        findings: List[str] = []
        try:
            wayland = self.wayland_probe(self.env)
            missing = [name for name in WLR_REQUIRED if name not in wayland["globals"]]
            if not missing:
                return self._make_wlr()
            findings.append(f"Wayland socket {wayland['socket']} lacks {', '.join(missing)}")
        except DesktopError as error:
            findings.append(f"Wayland: {error}")
        try:
            portal = self.portal_probe(self.env)
            if portal.get("available"):
                return self._make_portal()
            findings.append(f"portal: {portal.get('reason', PORTAL_INTERFACE + ' not offered')}")
        except DesktopError as error:
            findings.append(f"portal: {error}")
        raise DesktopError("no usable desktop backend. Probed: " + "; ".join(findings))


_INTERESTING_GLOBALS = {
    "wl_seat", "wl_output", "zxdg_output_manager_v1", "zwlr_virtual_pointer_manager_v1",
    "zwp_virtual_keyboard_manager_v1", "zwlr_screencopy_manager_v1", "ext_image_copy_capture_manager_v1",
}
