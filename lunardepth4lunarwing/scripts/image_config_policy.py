#!/usr/bin/env python3
"""Validate the complete LunarDepth image configuration out of band."""

import argparse
import json
import sys

EXPECTED_ENTRYPOINT = ["/usr/bin/tini", "--", "/app/entrypoint.sh"]
EXPECTED_CMD = ["--mode", "websocket"]
EXPECTED_HEALTHCHECK = ["CMD-SHELL", "/usr/bin/curl -sf http://127.0.0.1:8443/ready || exit 1"]
EXPECTED_SHELL = ["/bin/bash", "-o", "pipefail", "-c"]
ALLOWED_ENV = {
    "NODE_VERSION", "YARN_VERSION", "PATH", "LUNARDEPTH_IMAGE_REVISION", "LUNARDEPTH_IMAGE_VERSION",
    "LUNARDEPTH_PROFILE_SCHEMA_VERSION", "LUNARDEPTH_TASK_UID", "LUNARDEPTH_TASK_GID",
    "LUNARDEPTH_BRIDGE_UID", "LUNARDEPTH_CLOSURE_ROOT", "LUNARDEPTH_TEMPLATE_ROOT",
    "LUNARWING_APP_DIR", "DSH_TELEMETRY_DISABLED", "WS_NO_BUFFER_UTIL", "WS_NO_UTF_8_VALIDATE",
}


def environment(config: dict) -> dict[str, str]:
    values = {}
    for entry in config.get("Env") or []:
        if not isinstance(entry, str) or "=" not in entry:
            raise ValueError("image environment entry is malformed")
        key, value = entry.split("=", 1)
        values[key] = value
    return values


def validate_config(document: dict) -> None:
    config = document.get("Config", document.get("config", document))
    if not isinstance(config, dict):
        raise ValueError("image config is not an object")
    if config.get("User") != "10005:10005":
        raise ValueError("final image User must be 10005:10005")
    if config.get("Entrypoint") != EXPECTED_ENTRYPOINT or config.get("Cmd") != EXPECTED_CMD:
        raise ValueError("image entrypoint or Cmd drifted")
    if config.get("WorkingDir") != "/app":
        raise ValueError("image WorkingDir must be /app")
    for key in ("OnBuild", "Volumes"):
        if config.get(key):
            raise ValueError(f"image {key} must be empty")
    if config.get("Shell") not in (None, [], EXPECTED_SHELL):
        raise ValueError("image Shell must be empty or the reviewed bash pipefail form")
    exposed = config.get("ExposedPorts") or {}
    if set(exposed) != {"8443/tcp", "9090/tcp"}:
        raise ValueError("only bridge health and WebSocket ports may be exposed")
    healthcheck = config.get("Healthcheck") or document.get("Healthcheck") or {}
    if healthcheck.get("Test") != EXPECTED_HEALTHCHECK:
        raise ValueError("image healthcheck must use bounded bridge /ready")
    env = environment(config)
    unexpected = set(env) - ALLOWED_ENV
    if unexpected:
        raise ValueError(f"unexpected image environment: {sorted(unexpected)}")
    if env.get("LUNARDEPTH_IMAGE_REVISION") != "da06c33b2091546dc38c61b95452ce27a4ce32d5":
        raise ValueError("LunarDepth source revision drifted")
    if env.get("LUNARDEPTH_PROFILE_SCHEMA_VERSION") != "unversioned":
        raise ValueError("profile schema metadata drifted")
    if env.get("LUNARDEPTH_TASK_UID") != "10005" or env.get("LUNARDEPTH_BRIDGE_UID") != "10105":
        raise ValueError("split identity metadata drifted")
    if env.get("PATH") != "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin":
        raise ValueError("image PATH must contain only root-owned system directories")
    expected_paths = {
        "LUNARDEPTH_TASK_GID": "10005",
        "LUNARDEPTH_IMAGE_VERSION": "0.2.1-alpha.1",
        "LUNARDEPTH_CLOSURE_ROOT": "/opt/lunardepth",
        "LUNARDEPTH_TEMPLATE_ROOT": "/opt/lunardepth-templates",
        "LUNARWING_APP_DIR": "/app",
    }
    for key, expected in expected_paths.items():
        if env.get(key) != expected:
            raise ValueError(f"runtime path metadata drifted: {key}")
    if env.get("DSH_TELEMETRY_DISABLED") != "1" or env.get("WS_NO_BUFFER_UTIL") != "1" or env.get("WS_NO_UTF_8_VALIDATE") != "1":
        raise ValueError("launch hardening environment drifted")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--image-config", required=True)
    args = parser.parse_args()
    with open(args.image_config, encoding="utf-8") as handle:
        validate_config(json.load(handle))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"lunardepth-image-policy: {error}", file=sys.stderr)
        sys.exit(1)
