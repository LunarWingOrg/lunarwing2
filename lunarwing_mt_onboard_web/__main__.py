"""Entry point: ``python3 -m lunarwing_mt_onboard_web [--demo] [--port N]``.

Prints a prominent banner with the browser URL (including the session token),
then serves the app on 127.0.0.1.
"""

from __future__ import annotations

import argparse
import ipaddress
import os
import sys

from . import __version__
from .security import generate_token

_BANNER = r'''
       .   *      .     *        .      .    *
    *      (  o  )      L U N A R W I N G
    .     ^ v^v ^       Multi-Tenant Onboarding  v{ver}
       .        .  *    ~ bats and moons ~
'''


def _banner(url: str, demo: bool, needs_sudo_warning: bool) -> None:
    mode = (
        "DEMO  -  simulated run, no sudo, no system changes"
        if demo
        else "REAL  -  drives lunarwing-mt-admin.sh (run with sudo)"
    )
    line = "=" * 68
    print(_BANNER.format(ver=__version__))
    print(line)
    print(f"  Mode : {mode}")
    print("  Open your browser to:")
    print(f"      {url}")
    print(line)
    if needs_sudo_warning:
        print(
            "  NOTE: real provisioning creates OS users/containers and needs root.\n"
            "        Re-run with:  sudo .venv/bin/python -m lunarwing_mt_onboard_web\n"
            "        (or use --demo to try the UI safely first).",
            file=sys.stderr,
        )
    print("  Press Ctrl+C to stop.\n")
    sys.stdout.flush()


def _is_loopback_host(host: str) -> bool:
    if host.lower() == "localhost":
        return True
    try:
        return ipaddress.ip_address(host.strip("[]")).is_loopback
    except ValueError:
        return False


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="lunarwing_mt_onboard_web",
        description="Localhost web UI for LunarWing multi-tenant onboarding.",
    )
    parser.add_argument("--host", default="127.0.0.1", help="Bind host (default: 127.0.0.1).")
    parser.add_argument(
        "--port",
        type=int,
        default=int(os.environ.get("LUNARWING_ONBOARD_WEB_PORT", "1969")),
        help="Bind port (default: 1969).",
    )
    parser.add_argument(
        "--demo",
        action="store_true",
        help="Run against bundled fake scripts: no sudo, no system changes.",
    )
    parser.add_argument(
        "--log-dir",
        default=os.environ.get("LUNARWING_ONBOARD_WEB_LOG_DIR"),
        help="Directory for audit logs (default: /var/log/... or package logs/).",
    )
    parser.add_argument(
        "--no-token",
        action="store_true",
        help="Disable the session token (NOT recommended).",
    )
    parser.add_argument(
        "--allow-insecure-remote",
        action="store_true",
        help="Allow real mode to bind outside loopback without TLS (DANGEROUS).",
    )
    args = parser.parse_args(argv)

    loopback = _is_loopback_host(args.host)
    if not args.demo and not loopback and not args.allow_insecure_remote:
        parser.error(
            "real mode refuses a non-loopback bind without --allow-insecure-remote; "
            "use an SSH tunnel instead"
        )

    # Ensure the banner (with the URL + token) appears immediately even when
    # stdout is piped/redirected, not just on a TTY.
    try:
        sys.stdout.reconfigure(line_buffering=True)
    except Exception:
        pass

    # Activate demo BEFORE importing the app so the parent modules' import-time
    # script-path globals get patched to the fake scripts.
    if args.demo:
        from . import demo

        demo.enable()

    token = "" if args.no_token else generate_token()

    from .app import create_app

    try:
        import uvicorn
    except ImportError:
        print(
            "error: uvicorn is not installed. Create a venv and install deps:\n"
            "  python3 -m venv lunarwing_mt_onboard_web/.venv\n"
            "  lunarwing_mt_onboard_web/.venv/bin/pip install -r "
            "lunarwing_mt_onboard_web/requirements.txt",
            file=sys.stderr,
        )
        return 1

    app = create_app(token=token, demo=args.demo, log_dir=args.log_dir)

    url = f"http://{args.host}:{args.port}/"
    if token:
        url += f"?token={token}"

    is_root = hasattr(os, "geteuid") and os.geteuid() == 0
    _banner(url, args.demo, needs_sudo_warning=(not args.demo and not is_root))

    if not loopback:
        print(
            "WARNING: insecure non-loopback bind exposes tenant provisioning "
            "and submitted secrets to your network without TLS.",
            file=sys.stderr,
        )

    uvicorn.run(app, host=args.host, port=args.port, log_level="warning")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
