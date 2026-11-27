"""Command line entry point: ``python3 -m lunarwing_computer`` or ``computer-use.py``.

Nothing desktop-related is touched before the first tool call, so --help and
the MCP handshake work without PyGObject, a compositor or a session bus.
"""

from __future__ import annotations

import argparse
import logging
import os
import signal
import sys
from typing import BinaryIO, List, Optional

from . import __version__
from .control import DesktopError


def _bounded_int(low: int, high: int):
    def parse(text: str) -> int:
        try:
            value = int(text)
        except ValueError:
            raise argparse.ArgumentTypeError(f"expected an integer from {low} to {high}") from None
        if not low <= value <= high:
            raise argparse.ArgumentTypeError(f"expected an integer from {low} to {high}")
        return value
    return parse


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="lunarwing-computer",
        description=(
            "LunarWing computer-use connector: a stdio MCP server that controls one existing Wayland "
            "desktop through wlroots virtual-input protocols or the XDG RemoteDesktop portal. "
            "It opens no network listener."
        ),
    )
    parser.add_argument("--backend", choices=("auto", "wlr", "portal"), default="auto",
                        help="auto (default) uses wlroots protocols when the compositor offers virtual pointer "
                             "and keyboard, else the RemoteDesktop portal")
    parser.add_argument("--output", metavar="NAME", help="default output (monitor) name for screenshots")
    parser.add_argument("--max-edge", type=_bounded_int(256, 4096), default=1280, metavar="PIXELS",
                        help="longest screenshot edge in pixels (default 1280)")
    parser.add_argument("--format", choices=("png", "jpeg"), default="png", help="screenshot format (default png)")
    parser.add_argument("--jpeg-quality", type=_bounded_int(10, 95), default=80, metavar="Q",
                        help="JPEG quality 10-95 (default 80)")
    parser.add_argument("--config", metavar="FILE",
                        help="private JSON file (mode 0600) with vision_url and vision_token; enables desktop_observe")
    parser.add_argument("--restore-token-file", metavar="FILE",
                        help="portal backend only: keep the portal restore token in this 0600 file so later "
                             "connects can skip the consent dialog (default: memory only)")
    parser.add_argument("--log-level", choices=("debug", "info", "warning", "error"), default="warning",
                        help="stderr log level (default warning)")
    parser.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
    return parser


def _protect_stdout() -> BinaryIO:
    """Keep a private handle on the real stdout and point fd 1 at stderr.

    Stray output from a library or a child process could otherwise corrupt
    the MCP stream.
    """
    sys.stdout.flush()
    mcp_out = os.fdopen(os.dup(1), "wb", buffering=0)
    os.dup2(2, 1)
    sys.stdout = sys.stderr
    return mcp_out


def main(argv: Optional[List[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    logging.basicConfig(stream=sys.stderr, level=getattr(logging, args.log_level.upper()),
                        format="lunarwing-computer %(levelname)s %(name)s: %(message)s")

    # Imports that pull in the rest of the package happen after argument parsing.
    from .backends import BackendOptions, Selector
    from .server import Server
    from .tools import Desktop, Options

    vision = None
    if args.config:
        from .vision import Vision

        try:
            vision = Vision.from_config(args.config)
        except DesktopError as error:
            print(f"lunarwing-computer: {error}", file=sys.stderr)
            return 2
    if args.restore_token_file and args.backend == "wlr":
        print("lunarwing-computer: --restore-token-file only applies to the portal backend", file=sys.stderr)

    env = dict(os.environ)
    selector = Selector(args.backend, env, BackendOptions(restore_token_file=args.restore_token_file))
    options = Options(backend=args.backend, output=args.output, max_edge=args.max_edge, fmt=args.format,
                      jpeg_quality=args.jpeg_quality)
    desktop = Desktop(selector, options, vision=vision)
    server = Server(desktop)

    def stop(_signum, _frame):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGHUP, stop)
    output = _protect_stdout()
    try:
        server.serve(sys.stdin.buffer, output)
    except (KeyboardInterrupt, BrokenPipeError):
        pass
    finally:
        desktop.shutdown()
    return 0


if __name__ == "__main__":
    sys.exit(main())
