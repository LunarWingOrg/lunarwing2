import argparse
import signal
import sys

from .control import Desktop, DesktopError
from .portal import Portal
from .server import Server
from .vision import Vision


def main():
    parser = argparse.ArgumentParser(description="LunarWing Wayland computer-use MCP connector")
    parser.add_argument("--config", help="Private JSON file containing vision_url and vision_token")
    args = parser.parse_args()
    portal = None

    def stop(_signal, _frame):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, stop)
    try:
        vision = Vision.from_config(args.config) if args.config else None
        portal = Portal()
        Server(Desktop(portal, vision)).serve(sys.stdin.buffer, sys.stdout)
    except DesktopError as error:
        print(str(error), file=sys.stderr)
        return 1
    except (KeyboardInterrupt, BrokenPipeError):
        return 0
    except Exception:
        print("Desktop connector failed; check session bus, dependencies and private config", file=sys.stderr)
        return 1
    finally:
        if portal is not None:
            portal.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
