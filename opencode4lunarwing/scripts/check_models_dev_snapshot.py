#!/usr/bin/env python3
"""WI#467: fail the opencode image build if the pinned upstream build stops
honouring MODELS_DEV_API_JSON.

At the v1.18.25 pin, packages/opencode/script/generate.ts reads the models.dev
snapshot from the file named by MODELS_DEV_API_JSON when set; otherwise it
fetches https://models.dev/api.json live and the build compiles whatever the
service returns at that minute into the binary (which is what kept failing the
clean-rebuild gate). The lane pins a committed snapshot and sets the env var in
the build RUN, so if a future upstream bump drops or renames the env var the
live fetch returns silently. This assert runs in the Dockerfile builder stage
right before `bun run build` to turn that drift into a build failure.
"""

import pathlib
import sys


REQUIRED_FRAGMENTS = (
    "process.env.MODELS_DEV_API_JSON",
    "models.dev",
)


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: check_models_dev_snapshot.py <upstream packages/opencode/script/generate.ts>")
    path = pathlib.Path(sys.argv[1])
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        raise SystemExit(f"cannot read upstream generate.ts at {path}: {error}") from None
    missing = [fragment for fragment in REQUIRED_FRAGMENTS if fragment not in text]
    if missing:
        raise SystemExit(
            "upstream generate.ts no longer honours MODELS_DEV_API_JSON "
            f"(missing {missing!r}); the build would fetch models.dev live. "
            "Review the bump and re-pin the snapshot input (WI#467)."
        )


if __name__ == "__main__":
    main()
