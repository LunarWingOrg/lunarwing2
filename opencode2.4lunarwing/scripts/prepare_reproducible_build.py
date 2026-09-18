#!/usr/bin/env python3
"""Apply the worker's deterministic bundling profile to the pinned v2 build."""

import pathlib
import sys


def prepare(source: str) -> str:
    # Bun's split compiled bundles can retain per-build chunk keys. Avoid
    # modifying executable bytes: generate one source bundle without a
    # bytecode cache instead. Version defines and artifact verification stay
    # in the upstream build script.
    for option in ("bytecode", "splitting"):
        original = f"    {option}: true,"
        if source.count(original) != 1:
            raise ValueError(f"expected exactly one upstream {option}: true setting")
        source = source.replace(original, f"    {option}: false,")
    return source


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: prepare_reproducible_build.py <upstream script/build.ts>")
    path = pathlib.Path(sys.argv[1])
    # Validate both settings before writing; upstream drift fails the build.
    patched = prepare(path.read_text(encoding="utf-8"))
    path.write_text(patched, encoding="utf-8")


if __name__ == "__main__":
    main()
