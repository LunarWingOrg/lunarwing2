#!/usr/bin/env python3
"""Run the connector from a checkout without installing a Python package."""

import os
import sys

# Under "python3 -I" the script directory is not added to sys.path, so add it
# explicitly. This is the same directory Python would add by default.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from lunarwing_computer.__main__ import main  # noqa: E402

if __name__ == "__main__":
    raise SystemExit(main())
