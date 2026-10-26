#!/usr/bin/env python3
"""Checkout-owned image-config policy for the opencode2 worker (WI#425)."""

from __future__ import annotations

import json
import re
import sys

EXPECTED_ENV = {
    "PATH": "/home/opencode2/.cargo/bin:/usr/local/cargo/bin:/usr/local/go/bin:"
    "/home/opencode2/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:"
    "/sbin:/bin:/usr/local/bun-node-fallback-bin",
    "BUN_RUNTIME_TRANSPILER_CACHE_PATH": "0",
    "BUN_INSTALL_BIN": "/usr/local/bin",
    "OPENCODE_DISABLE_AUTOUPDATE": "1",
    "RUSTUP_HOME": "/usr/local/rustup",
    "GOROOT": "/usr/local/go",
    "GOPATH": "/home/opencode2/go",
    "CARGO_HOME": "/home/opencode2/.cargo",
}
EXPECTED_FIELDS = {
    "Entrypoint": ["/usr/bin/tini", "--", "/app/entrypoint.sh"],
    "Cmd": ["--mode", "websocket"],
    "User": "10003:10003",
    "WorkingDir": "/app",
    "Healthcheck": {
        "Test": ["CMD-SHELL", "curl -sf http://127.0.0.1:8443/health || exit 1"],
        "Interval": 30_000_000_000,
        "Timeout": 5_000_000_000,
        "StartPeriod": 30_000_000_000,
        "Retries": 3,
    },
    "ExposedPorts": {"8443/tcp": {}, "9090/tcp": {}},
    "StopSignal": "SIGTERM",
}
EMPTY_FIELDS = {
    "OnBuild": (None, []),
    "Volumes": (None, {}),
    "Shell": (None, []),
}
IMAGE_REF_PATTERN = re.compile(r"^v\d+\.\d+\.\d+@[0-9a-f]{40}$")


def _config_section(image_config: dict) -> dict:
    for key in ("config", "Config"):
        section = image_config.get(key)
        if isinstance(section, dict) and "Env" in section:
            return section
    return image_config


def _environment_failures(entries: object) -> list[str]:
    if not isinstance(entries, list) or any(not isinstance(entry, str) for entry in entries):
        return ["image config Env must be a list of key=value strings"]
    parsed: dict[str, str] = {}
    failures: list[str] = []
    for entry in entries:
        if "=" not in entry:
            failures.append("image config Env contains an entry without '='")
            continue
        key, value = entry.split("=", 1)
        if key in parsed:
            failures.append(f"image config Env duplicates {key}")
        parsed[key] = value

    for key in sorted(set(parsed) - set(EXPECTED_ENV) - {"OPENCODE_IMAGE_REF"}):
        if key in {"OPENCODE_BIN", "LUNARWING_REAL_OPENCODE"}:
            failures.append(f"image config Env retargets {key}")
        else:
            failures.append(f"image config Env has unapproved key {key}")
    for key, expected in EXPECTED_ENV.items():
        if key not in parsed:
            failures.append(f"image config Env lost {key}={expected}")
        elif parsed[key] != expected:
            failures.append(f"image config Env changed {key} to {parsed[key]!r}")
    image_ref = parsed.get("OPENCODE_IMAGE_REF", "")
    if not IMAGE_REF_PATTERN.fullmatch(image_ref):
        failures.append("image config Env has a missing or malformed OPENCODE_IMAGE_REF")
    return failures


def config_failures(image_config: dict) -> list[str]:
    if not isinstance(image_config, dict):
        return ["image config must be a JSON object"]
    section = _config_section(image_config)
    failures = _environment_failures(section.get("Env"))
    for key, expected in EXPECTED_FIELDS.items():
        if section.get(key) != expected:
            failures.append(f"image config {key} must be {expected!r}, got {section.get(key)!r}")
    for key, empty_values in EMPTY_FIELDS.items():
        value = section.get(key)
        if value in empty_values:
            continue
        if key == "Shell":
            failures.append(f"image config carries a Shell override {value!r}")
        else:
            failures.append(f"image config {key} must be empty, got {value!r}")
    return failures


def main() -> int:
    try:
        image_config = json.load(sys.stdin)
    except (OSError, ValueError) as error:
        print(f"ERROR: cannot parse image config JSON: {error}", file=sys.stderr)
        return 1
    failures = config_failures(image_config)
    for failure in failures:
        print(f"ERROR: {failure} (WI#425)", file=sys.stderr)
    if failures:
        return 1
    print("out-of-band image-config verify: full Env and runtime config clean")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
