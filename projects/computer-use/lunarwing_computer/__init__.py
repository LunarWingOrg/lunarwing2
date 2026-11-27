"""Wayland desktop connector for LunarWing (MCP over stdio).

The package is a stdio MCP server. It controls one existing Wayland desktop
session through either the wlroots virtual-input protocols (Hyprland, Sway,
labwc, river, Wayfire, niri) or the XDG RemoteDesktop and ScreenCast portals
(GNOME, KDE, COSMIC). It never opens a network listener.
"""

__version__ = "0.2.0"
SERVER_NAME = "lunarwing-computer"
