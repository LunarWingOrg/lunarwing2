"""Find the desktop session's Wayland socket and D-Bus session bus.

LunarWing starts stdio MCP servers with a scrubbed environment:
XDG_RUNTIME_DIR, HOME and PATH are passed through, but WAYLAND_DISPLAY,
DBUS_SESSION_BUS_ADDRESS and DISPLAY are not unless the operator sets them
with ``--env``. These helpers fall back to the per-user runtime directory.
"""

from __future__ import annotations

import os
import re
import stat
from typing import List, Mapping, Optional

from .control import DesktopError

_WAYLAND_SOCKET_NAME = re.compile(r"^wayland-[0-9]+$")
_DBUS_SAFE = re.compile(rb"[-0-9A-Za-z_/.\\*]")


def runtime_dir(env: Mapping[str, str]) -> str:
    """XDG_RUNTIME_DIR, or /run/user/<uid> when it is unset."""
    value = env.get("XDG_RUNTIME_DIR")
    if value:
        return value
    return f"/run/user/{os.getuid()}"


def _is_socket(path: str) -> bool:
    try:
        return stat.S_ISSOCK(os.stat(path).st_mode)
    except OSError:
        return False


def wayland_socket_candidates(directory: str) -> List[str]:
    """Names in ``directory`` that look like compositor sockets.

    Only exact ``wayland-N`` names that are sockets count; lock files
    (``wayland-1.lock``) and helper sockets that merely start with the same
    prefix (for example ``wayland-1-swww-daemon..sock``) are ignored.
    """
    try:
        names = os.listdir(directory)
    except OSError:
        return []
    return sorted(
        name for name in names
        if _WAYLAND_SOCKET_NAME.match(name) and _is_socket(os.path.join(directory, name))
    )


def resolve_wayland_socket(env: Mapping[str, str]) -> str:
    """Absolute path of the compositor socket to use.

    WAYLAND_DISPLAY may be a name relative to the runtime directory or an
    absolute path (libwayland accepts both). When it is unset, the single
    ``wayland-N`` socket in the runtime directory is used; zero or several
    candidates are an error, because guessing could reach the wrong session.
    """
    directory = runtime_dir(env)
    display = env.get("WAYLAND_DISPLAY")
    if display:
        path = display if os.path.isabs(display) else os.path.join(directory, display)
        if not _is_socket(path):
            raise DesktopError(f"WAYLAND_DISPLAY points to {path}, which is not a Wayland socket")
        return path
    candidates = wayland_socket_candidates(directory)
    if not candidates:
        raise DesktopError(
            f"WAYLAND_DISPLAY is unset and no wayland-N socket exists in {directory}; "
            "run the connector as the desktop user or pass --env WAYLAND_DISPLAY=wayland-N"
        )
    if len(candidates) > 1:
        raise DesktopError(
            f"WAYLAND_DISPLAY is unset and {directory} holds several sockets ({', '.join(candidates)}); "
            "pass --env WAYLAND_DISPLAY=<name> to choose one"
        )
    return os.path.join(directory, candidates[0])


def wayland_child_env(env: Mapping[str, str], socket_path: str) -> dict:
    """Environment for a helper (grim) that must reach the same compositor."""
    child = dict(env)
    child["XDG_RUNTIME_DIR"] = os.path.dirname(socket_path)
    child["WAYLAND_DISPLAY"] = os.path.basename(socket_path)
    return child


def dbus_escape(value: str) -> str:
    """Escape a value for a D-Bus address (unescaped bytes per the D-Bus spec)."""
    out = []
    for byte in value.encode("utf-8"):
        char = bytes([byte])
        out.append(char.decode("ascii") if _DBUS_SAFE.match(char) else f"%{byte:02x}")
    return "".join(out)


def discover_dbus_address(env: Mapping[str, str]) -> Optional[str]:
    """DBUS_SESSION_BUS_ADDRESS, or the runtime directory's ``bus`` socket, or None."""
    value = env.get("DBUS_SESSION_BUS_ADDRESS")
    if value:
        return value
    path = os.path.join(runtime_dir(env), "bus")
    if _is_socket(path):
        return "unix:path=" + dbus_escape(path)
    return None
