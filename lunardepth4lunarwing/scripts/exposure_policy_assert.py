#!/usr/bin/env python3
"""Static and image-filesystem LunarDepth exposure policy checks."""

import argparse
import os
import stat
import subprocess
from pathlib import Path

REQUIRED_FILES = {
    "AGENTS.md", "README.md", "Dockerfile", "docker-compose.yml", ".env.example", "agent_comm_protocol.json",
    "entrypoint.sh", "entrypoint-split.sh", "entrypoint-task-setup.sh", "health_server.py",
    "config/lunarwing-lane.patch.yml", "scripts/lunarwing_runtime.ts", "scripts/lunarwing_bridge.ts",
    "scripts/lunardepth_task_runner.ts", "scripts/task_runner_client.ts", "scripts/workspace_path.ts",
    "scripts/effective_config_assert.ts", "scripts/sandbox_smoke.ts", "scripts/exposure_policy_assert.py",
    "scripts/image_config_policy.py", "scripts/runtime_smoke.py", "scripts/runtime_smoke_entrypoint.sh",
    "scripts/dsh-exposure-guard.sh",
}


def require(text: str, needles: list[str], label: str) -> None:
    for needle in needles:
        if needle not in text:
            raise ValueError(f"{label}: missing {needle}")


def source_policy(root: Path) -> None:
    missing = sorted(path for path in REQUIRED_FILES if not (root / path).is_file())
    if missing:
        raise ValueError(f"missing lane files: {', '.join(missing)}")
    overlay = (root / "config/lunarwing-lane.patch.yml").read_text()
    require(overlay, [
        "host: 127.0.0.1", "port: 19091", "authToken: !!js process.env.LUNARDEPTH_LOCAL_AUTH_TOKEN",
        "defaultPreset: workspace-write", "runnerCommand: []", "runnerFailureSignatures: []",
        "mode: workspace-write", "policy: ask", "apiKeyEnv: LUNARDEPTH_API_KEY",
        "id: tool-fs-search\n  name: '@lunardepth/dsh-tool-fs-search'\n  disabled: true",
        "id: ptc-runtime\n  name: '@lunardepth/dsh-ptc-runtime-node'\n  disabled: true",
        "id: workflow-ptc\n  name: '@lunardepth/dsh-workflow-ptc'\n  disabled: true",
        "id: session-telemetry-otel\n  name: '@lunardepth/dsh-session-telemetry-otel'\n  disabled: true",
        "id: web-search-deepseek\n  name: '@lunardepth/dsh-web-search-deepseek'\n  disabled: true",
        "id: web-fetch-http\n  name: '@lunardepth/dsh-web-fetch-http'\n  disabled: true",
    ], "overlay")
    for forbidden in ("danger-full-access", "baseURLEnv:", "AGENT_AUTH_TOKEN", "LUNARDEPTH_API_KEY:", "dsh-bash-local"):
        if forbidden in overlay:
            raise ValueError(f"overlay: forbidden {forbidden}")
    dockerfile = (root / "Dockerfile").read_text()
    require(dockerfile, [
        "node:22.23.4-bookworm-slim@sha256:4b150b48b4116e2a22f2c39cc27025fbf4f153e2078fd3b14d159fdb5c7279da",
        "pnpm@11.7.0", "LUNARDEPTH_REF=da06c33b2091546dc38c61b95452ce27a4ce32d5",
        "LUNARDEPTH_COMPATIBILITY_REF=17aef23e05cc074c8621d38d159d77d25cb348ca",
        "LUNARDEPTH_MODEL_ROUTING_REF=8c610180c84a2f4c27fb2958e6e7dab9e48d476b",
        "LUNARDEPTH_PROFILE_SCHEMA_VERSION=unversioned", "DEBIAN_SNAPSHOT=20261105T082236Z",
        "build-exe-for-python-sdk.ts --deploy-to=/opt/lunardepth", "profile_template.js",
        "sed -i '/^  \"prunedAt\":/d' /opt/lunardepth/node_modules/.modules.yaml",
        "musl-tools", "pnpm exec tsx native/system/scripts/build.ts", "--probe | grep -q 'fully enforced'",
        "RUN --network=none set -eu", "USER 10005:10005",
    ], "Dockerfile")
    if "USER 0:0" in dockerfile or "pnpm@9" in dockerfile:
        raise ValueError("Dockerfile: forbidden runtime identity or package manager")
    setup = (root / "entrypoint-task-setup.sh").read_text()
    require(setup, [
        "export HOME=/home/lunardepth-home", "export DSH_HOME=/home/lunardepth/.dsh",
        "export TMPDIR=/tmp TMP=/tmp TEMP=/tmp", "export WS_NO_BUFFER_UTIL=1 WS_NO_UTF_8_VALIDATE=1",
        "sessions|storages|attachments", "cp -a \"$TEMPLATE_ROOT/profiles/lunarwing-worker\"",
        "secrets.token_hex(32)", "render_overlay.py", "unset NODE_OPTIONS",
        "LUNARDEPTH_EFFECTIVE_OVERLAY_SHA256", "effective overlay digest differs from its launch contract",
    ], "task setup")
    split = (root / "entrypoint-split.sh").read_text()
    require(split, [
        "10005:10005", "10105:10005", "post-ready", "LUNARWING_WORKSPACE_ID",
        ". \"$APP_DIR/entrypoint-task-setup.sh\"",
    ], "split entrypoint")
    bridge = (root / "scripts/lunarwing_bridge.ts").read_text()
    require(bridge, ["capabilities: []", "LUNARDEPTH_API_KEY", "LUNARDEPTH_LOCAL_AUTH_TOKEN"], "bridge")
    if "tool_proxy" in bridge or "tool-proxy" in bridge:
        raise ValueError("bridge: tool proxy is not supported")
    protocol = (root / "agent_comm_protocol.json").read_text()
    require(protocol, ["lunarwing-agent-v1", "\"supports_credentials\": false", "\"task_prompt\": false", "\"tool_proxy\": false"], "protocol")


def check_mode(path: Path, expected: int) -> None:
    actual = stat.S_IMODE(path.lstat().st_mode)
    if actual != expected:
        raise ValueError(f"{path}: mode {actual:o} != {expected:o}")


def image_policy(root: Path) -> None:
    for relative in ("app", "opt/lunardepth", "opt/lunardepth-templates"):
        base = root / relative
        if not base.is_dir():
            raise ValueError(f"missing image tree {base}")
        for path in base.rglob("*"):
            info = path.lstat()
            if info.st_uid != 0 or stat.S_IMODE(info.st_mode) & 0o222:
                raise ValueError(f"writable or non-root-owned image input: {path}")
    for relative in ("app/entrypoint.sh", "app/entrypoint-split.sh", "app/entrypoint-task-setup.sh",
                     "app/health_server.py", "usr/local/bin/dsh"):
        check_mode(root / relative, 0o555)
    check_mode(root / "run/lunarwing-shared", 0o3770)
    check_mode(root / "run/lunarwing-shared/runner", 0o2711)
    for relative in ("run/lunardepth-launch", "run/lunardepth-config", "home/lunardepth-home"):
        check_mode(root / relative, 0o555)
    for args in (["web"], ["--host", "0.0.0.0"], ["--share"], ["plugin", "install"]):
        result = subprocess.run([str(root / "usr/local/bin/dsh"), *args], capture_output=True, timeout=5)
        if result.returncode != 77:
            raise ValueError(f"exposure guard accepted {args}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path)
    parser.add_argument("--image-filesystem", type=Path)
    args = parser.parse_args()
    if (args.source is None) == (args.image_filesystem is None):
        parser.error("choose exactly one of --source or --image-filesystem")
    if args.source is not None:
        source_policy(args.source.resolve())
    else:
        image_policy(args.image_filesystem.resolve())


if __name__ == "__main__":
    main()
