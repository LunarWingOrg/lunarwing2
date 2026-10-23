#!/usr/bin/env python3
"""WI#467: fail the opencode image build if the pinned upstream build stops
honouring the models.dev snapshot inputs, or if the snapshot is not strict JSON.

At the v1.18.25 pin, packages/opencode/script/generate.ts reads the models.dev
snapshot from the file named by MODELS_DEV_API_JSON when set; otherwise it
fetches ${OPENCODE_MODELS_URL:-https://models.dev}/api.json live and the build
compiles whatever the service returns at that minute into the binary (which is
what kept failing the clean-rebuild gate). The lane pins a committed snapshot,
sets MODELS_DEV_API_JSON in the build RUN and points OPENCODE_MODELS_URL at a
closed loopback port, so a live fetch fails the build. If a future upstream
bump drops or renames either env var, this assert turns that drift into a
build failure before `bun run build` runs.

script/build.ts passes the snapshot text to Bun.build as the
OPENCODE_MODELS_DEV define, so the snapshot is compiled in as code. It must
therefore be strict JSON: one non-empty top-level object, no duplicate keys
and no NaN/Infinity constants.
"""

import json
import pathlib
import sys


REQUIRED_FRAGMENTS = (
    "process.env.MODELS_DEV_API_JSON",
    "process.env.OPENCODE_MODELS_URL",
)


def _reject_duplicate_keys(pairs):
    seen = set()
    for key, _ in pairs:
        if key in seen:
            raise ValueError(f"duplicate key {key!r}")
        seen.add(key)
    return dict(pairs)


def _reject_constant(name):
    raise ValueError(f"non-standard JSON constant {name}")


def check_generate(path: pathlib.Path) -> None:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        raise SystemExit(f"cannot read upstream generate.ts at {path}: {error}") from None
    missing = [fragment for fragment in REQUIRED_FRAGMENTS if fragment not in text]
    if missing:
        raise SystemExit(
            "upstream generate.ts no longer reads the models.dev snapshot inputs "
            f"(missing {missing!r}); the build could fetch models.dev live. "
            "Review the bump and re-pin the snapshot input (WI#467)."
        )


def check_snapshot(path: pathlib.Path) -> None:
    try:
        data = json.loads(
            path.read_bytes(),
            object_pairs_hook=_reject_duplicate_keys,
            parse_constant=_reject_constant,
        )
    except (OSError, ValueError) as error:
        raise SystemExit(f"models.dev snapshot {path} is not strict JSON: {error}") from None
    if not isinstance(data, dict) or not data:
        raise SystemExit(f"models.dev snapshot {path} must be a non-empty JSON object")


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit(
            "usage: check_models_dev_snapshot.py "
            "<upstream packages/opencode/script/generate.ts> <models.dev snapshot>"
        )
    check_generate(pathlib.Path(sys.argv[1]))
    check_snapshot(pathlib.Path(sys.argv[2]))


if __name__ == "__main__":
    main()
