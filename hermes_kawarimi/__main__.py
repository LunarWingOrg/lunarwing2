"""``python3 -m hermes_kawarimi`` entry point."""

from __future__ import annotations

import sys

from hermes_kawarimi.cli import main

if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
