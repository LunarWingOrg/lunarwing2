#!/usr/bin/env python3

import json
import pathlib


ROOT = pathlib.Path(__file__).resolve().parents[2]
REGISTRY = ROOT / "ic" / "registry"


def main() -> int:
    files = sorted(REGISTRY.rglob("*.json"))
    if not files:
        raise RuntimeError(f"no registry JSON files found under {REGISTRY}")

    for path in files:
        with path.open(encoding="utf-8") as stream:
            json.load(stream)

    print(f"Validated {len(files)} registry JSON files.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
