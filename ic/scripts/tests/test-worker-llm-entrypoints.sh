#!/usr/bin/env bash
# WI#330: execute each worker's real startup configuration/guard in an isolated
# filesystem. No container, CLI binary, network listener, or build is needed;
# this runs in the MR shell-harnesses lane as well as on developer hosts.
set -euo pipefail
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
python3 - "$SCRIPT_DIR/../../.." <<'PY'
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib

repo = Path(sys.argv[1]).resolve()
endpoint = "https://gateway.example.test/openai/v1"
workers = {
    "opencode": ("opencode4lunarwing", "opencode.json"),
    "opencode2": ("opencode2.4lunarwing", "opencode2.json"),
    "kimi": ("kimicode4lunarwing", "config.toml"),
}


def run_config(worker, overrides=None, mounted=False, custom=None):
    directory, filename = workers[worker]
    source = repo / directory
    entrypoint = (source / "entrypoint.sh").read_text()
    boundary = "# ── always start the health server"
    assert boundary in entrypoint, f"{worker}: startup boundary changed"
    with tempfile.TemporaryDirectory(prefix=f"{worker}-llm-") as temp:
        root = Path(temp)
        home, app, workspace = root / "home", root / "app", root / "workspace"
        for path in (home, app / "config", workspace):
            path.mkdir(parents=True)
        baked = (source / "config" / filename).read_text()
        placeholder = f"http://set-{worker}-base-url.invalid/openai/v1"
        if worker != "kimi":
            placeholder += "/"
        assert placeholder in baked, f"{worker}: expected baked placeholder"
        config = app / "config" / filename
        config.write_text(baked.replace(placeholder, endpoint) if mounted else baked)
        env = {
            "PATH": os.environ["PATH"], "HOME": str(home), "TMPDIR": temp,
            "WORKSPACE_ROOT": str(workspace), "KIMI_CODE_HOME": str(home / ".kimi-code"),
            "KIMI_CONFIG_SOURCE": str(config),
            **(overrides or {}),
        }
        if custom is not None:
            custom_path = root / "custom.json"
            custom_path.write_text(baked.replace(placeholder, custom))
            env["OPENCODE_CONFIG"] = str(custom_path)
        # Relocate only container paths. Run the production shell/Python code,
        # including its real baked config, and stop before any service starts.
        script = entrypoint.split(boundary, 1)[0].replace("/app/", str(app) + "/")
        for user in ("opencode", "kimi"):
            script = script.replace(f"/home/{user}/", str(home) + "/")
        result = subprocess.run(["bash"], input=script, text=True, env=env,
                                capture_output=True, timeout=15)
        target = (home / ".kimi-code/config.toml" if worker == "kimi"
                  else workspace / ".opencode" / filename)
        generated = target.read_text() if target.exists() else ""
        return result, generated


def expect_ready(worker, **kwargs):
    result, text = run_config(worker, **kwargs)
    assert result.returncode == 0, f"{worker}: {result.stderr}"
    if worker == "kimi":
        config = tomllib.loads(text)
        actual = config["providers"]["tensorzero"]["base_url"]
    else:
        config = json.loads(text)
        providers, settings = (("provider", "options") if worker == "opencode"
                               else ("providers", "settings"))
        actual = next(iter(config[providers].values()))[settings]["baseURL"]
    assert actual == endpoint, f"{worker}: effective base URL {actual!r}"
    return config


for worker in workers:
    key = f"{worker.upper()}_BASE_URL"
    for label, overrides in (
        ("unset", {}), ("empty", {key: ""}), ("whitespace", {key: " \t "}),
        ("model-only", {f"{worker.upper()}_MODEL": "test-model"}),
    ):
        result, _ = run_config(worker, overrides)
        assert result.returncode != 0, f"{worker}: {label} override bypassed guard"
        assert f"{key} is unset and the baked config is a placeholder" in result.stderr, result.stderr
        print(f"PASS: {worker} refuses {label} URL before starting services")
    expect_ready(worker, overrides={key: f" {endpoint} "})
    print(f"PASS: {worker} materializes endpoint override")
    expect_ready(worker, mounted=True)
    print(f"PASS: {worker} accepts operator-mounted endpoint without an override")

result, _ = run_config("opencode2", custom=endpoint)
assert result.returncode == 0, result.stderr
print("PASS: opencode2 checks custom OPENCODE_CONFIG instead of unused placeholder")
result, _ = run_config("opencode2", {"OPENCODE2_BASE_URL": endpoint},
                       custom="http://set-opencode2-base-url.invalid/openai/v1")
assert result.returncode != 0 and "baked config is a placeholder" in result.stderr, result.stderr
print("PASS: opencode2 refuses effective custom placeholder despite env override")

api_key = ' key"\\with spaces\t '
config = expect_ready("kimi", overrides={"KIMI_BASE_URL": endpoint,
                                         "KIMI_MODEL": " new-model ", "KIMI_API_KEY": api_key})
assert config["default_model"] == "new-model"
assert config["models"]["new-model"]["model"] == "new-model"
assert config["providers"]["tensorzero"]["api_key"] == api_key
print("PASS: Kimi normalizes model/URL consistently while preserving API-key bytes")
PY
