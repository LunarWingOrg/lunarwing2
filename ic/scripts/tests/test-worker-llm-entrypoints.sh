#!/usr/bin/env bash
# WI#330: execute each worker's real startup configuration/guard in an isolated
# filesystem. No container, CLI binary, or build is needed; the WI#373 phases
# bind short-lived ephemeral 127.0.0.1 listeners for their stub servers. This
# runs in the MR shell-harnesses lane as well as on developer hosts.
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


def free_port():
    import socket
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def execable_dir(root):
    """Temp dirs can be mounted noexec on dev VMs; probe for a directory
    where the stub binaries can actually run. root/bin is probed first; the
    HOME fallback is created only when that fails, and is rmtree'd at exit
    so a run never leaves directories behind (MR !1586 run-98 FY-2)."""
    import atexit
    import shutil

    def probe_executable(candidate):
        try:
            candidate.mkdir(parents=True, exist_ok=True)
            probe = candidate / ".exec-probe.sh"
            try:
                probe.write_text("#!/bin/sh\nexit 0\n")
                probe.chmod(0o755)
                return subprocess.run([str(probe)], capture_output=True).returncode == 0
            finally:
                probe.unlink(missing_ok=True)
        except OSError:
            return False

    if probe_executable(root / "bin"):
        return root / "bin"

    home = os.environ.get("HOME")
    if home and Path(home).is_dir():
        fallback = Path(tempfile.mkdtemp(prefix="worker-ws373-", dir=home))
        atexit.register(shutil.rmtree, fallback, ignore_errors=True)
        if probe_executable(fallback):
            return fallback
    raise AssertionError("no executable directory found for worker stubs")


def run_websocket_startup(worker):
    """WI#373: run the FULL entrypoint in websocket mode with stub binaries.
    The opencode serve stub records whether the bridge's hub-handshake
    credential (AGENT_AUTH_TOKEN) reached the server process env — the server
    runs all task code, so it must NOT see it — and the bun stub records the
    same for the bridge process, which MUST keep it for the handshake."""
    directory, filename = workers[worker]
    source = repo / directory
    entrypoint = (source / "entrypoint.sh").read_text()
    boundary = "# ── always start the health server"
    assert boundary in entrypoint, f"{worker}: startup boundary changed"
    with tempfile.TemporaryDirectory(prefix=f"{worker}-ws373-") as temp:
        root = Path(temp)
        home, app, workspace = root / "home", root / "app", root / "workspace"
        bin_dir = execable_dir(root)
        for path in (home, app / "config", workspace / ".opencode"):
            path.mkdir(parents=True)
        baked = (source / "config" / filename).read_text()
        placeholder = f"http://set-{worker}-base-url.invalid/openai/v1"
        if worker != "kimi":
            placeholder += "/"
        assert placeholder in baked, f"{worker}: expected baked placeholder"
        (app / "config" / filename).write_text(baked.replace(placeholder, endpoint))
        # Backgrounded by the entrypoint; exits immediately so nothing holds
        # the temp directory open.
        (app / "health_server.py").write_text("import time\ntime.sleep(0.2)\n")
        serve_mark = root / "serve-env.txt"
        bridge_mark = root / "bridge-env.txt"
        serve_port, health_port = free_port(), free_port()
        serve_stub = bin_dir / "opencode"
        serve_stub.write_text(
            "#!/usr/bin/env python3\n"
            "import os\n"
            "from http.server import BaseHTTPRequestHandler, HTTPServer\n"
            f"with open({str(serve_mark)!r}, 'w') as fh:\n"
            "    fh.write('PRESENT' if os.environ.get('AGENT_AUTH_TOKEN')"
            " else 'ABSENT')\n"
            "class Handler(BaseHTTPRequestHandler):\n"
            "    def do_GET(self):\n"
            "        self.send_response(200)\n"
            "        self.end_headers()\n"
            "        self.wfile.write(b'ok')\n"
            "    def log_message(self, *args):\n"
            "        pass\n"
            f"HTTPServer(('127.0.0.1', {serve_port}), Handler).handle_request()\n"
        )
        serve_stub.chmod(0o755)
        bun_stub = bin_dir / "bun"
        bun_stub.write_text(
            "#!/bin/sh\n"
            f"if [ -n \"$AGENT_AUTH_TOKEN\" ]; then echo PRESENT > {bridge_mark};"
            f" else echo ABSENT > {bridge_mark}; fi\n"
            "exit 0\n"
        )
        bun_stub.chmod(0o755)
        script = entrypoint.replace("/app/", str(app) + "/")
        for user in ("opencode", "kimi"):
            script = script.replace(f"/home/{user}/", str(home) + "/")
        env = {
            "PATH": f"{bin_dir}:{os.environ['PATH']}",
            "HOME": str(home), "TMPDIR": temp,
            "WORKSPACE_ROOT": str(workspace),
            "OPENCODE_BIN": str(serve_stub),
            f"{worker.upper()}_BASE_URL": endpoint,
            "OPENCODE_SERVE_PORT": str(serve_port),
            "OPENCODE_SERVE_HOST": "127.0.0.1",
            "HEALTH_PORT": str(health_port),
            "WS_STATE_FILE": str(root / "ws-state.json"),
            "AGENT_AUTH_TOKEN": "bridge-secret-64hex",
        }
        result = subprocess.run(["bash"], input=script, text=True, env=env,
                                capture_output=True, timeout=60)
        assert result.returncode == 0, f"{worker}: entrypoint failed: {result.stderr}"
        assert serve_mark.exists() and bridge_mark.exists(), \
            f"{worker}: stubs did not run: {result.stderr}"
        assert serve_mark.read_text().strip() == "ABSENT", \
            f"{worker}: hub-handshake token reached the serve process env (WI#373)"
        assert bridge_mark.read_text().strip() == "PRESENT", \
            f"{worker}: bridge lost its handshake credential (WI#373)"


def run_oneshot_startup(worker, mode):
    """WI#373 (one-shot lanes): cli/acp modes exec a single task process —
    the same untrusted-prompt boundary — so the exec'd process must not
    inherit the hub-handshake credential either."""
    directory, filename = workers[worker]
    source = repo / directory
    entrypoint = (source / "entrypoint.sh").read_text()
    with tempfile.TemporaryDirectory(prefix=f"{worker}-cli373-") as temp:
        root = Path(temp)
        home, app, workspace = root / "home", root / "app", root / "workspace"
        bin_dir = execable_dir(root)
        for path in (home, app / "config", workspace / ".opencode"):
            path.mkdir(parents=True)
        baked = (source / "config" / filename).read_text()
        placeholder = f"http://set-{worker}-base-url.invalid/openai/v1"
        if worker != "kimi":
            placeholder += "/"
        assert placeholder in baked, f"{worker}: expected baked placeholder"
        (app / "config" / filename).write_text(baked.replace(placeholder, endpoint))
        (app / "health_server.py").write_text("import time\ntime.sleep(0.2)\n")
        mark = root / "oneshot-env.txt"
        stub = bin_dir / ("kimi" if worker == "kimi" else "opencode")
        stub.write_text(
            "#!/bin/sh\n"
            f"if [ -n \"$AGENT_AUTH_TOKEN\" ]; then echo PRESENT > {mark};"
            f" else echo ABSENT > {mark}; fi\n"
            "exit 0\n"
        )
        stub.chmod(0o755)
        script = entrypoint.replace("/app/", str(app) + "/")
        for user in ("opencode", "kimi"):
            script = script.replace(f"/home/{user}/", str(home) + "/")
        env = {
            "PATH": f"{bin_dir}:{os.environ['PATH']}",
            "HOME": str(home), "TMPDIR": temp,
            "WORKSPACE_ROOT": str(workspace),
            "KIMI_CODE_HOME": str(home / ".kimi-code"),
            "KIMI_CONFIG_SOURCE": str(app / "config" / filename),
            f"{worker.upper()}_BASE_URL": endpoint,
            "HEALTH_PORT": str(free_port()),
            "WS_STATE_FILE": str(root / "ws-state.json"),
            "AGENT_AUTH_TOKEN": "bridge-secret-64hex",
        }
        if worker != "kimi":
            env["OPENCODE_BIN"] = str(stub)
        env["KIMI_MODE" if worker == "kimi" else "OPENCODE_MODE"] = mode
        result = subprocess.run(["bash", "-s", "--", "probe prompt"],
                                input=script, text=True, env=env,
                                capture_output=True, timeout=60)
        assert result.returncode == 0, \
            f"{worker} {mode}: entrypoint failed: {result.stderr}"
        assert mark.exists(), f"{worker} {mode}: stub did not run: {result.stderr}"
        assert mark.read_text().strip() == "ABSENT", \
            f"{worker} {mode}: hub-handshake token reached the one-shot task env (WI#373)"


def run_pebble_cli_startup():
    """WI#373 (pebble cli mode): same one-shot boundary; the pebble process
    must not inherit the hub-handshake credential."""
    entrypoint = (repo / "pebble4lunarwing" / "entrypoint.sh").read_text()
    with tempfile.TemporaryDirectory(prefix="pebble-cli373-") as temp:
        root = Path(temp)
        bin_dir = execable_dir(root)
        mark = root / "pebble-cli-env.txt"
        stub = bin_dir / "pebble"
        stub.write_text(
            "#!/bin/sh\n"
            f"if [ -n \"$AGENT_AUTH_TOKEN\" ]; then echo PRESENT > {mark};"
            f" else echo ABSENT > {mark}; fi\n"
            "exit 0\n"
        )
        stub.chmod(0o755)
        result = subprocess.run(
            ["bash", "-s", "--", "probe prompt"],
            input=entrypoint, text=True,
            env={"PATH": f"{bin_dir}:{os.environ['PATH']}",
                 "HOME": str(root), "TMPDIR": temp,
                 "PEBBLE_MODE": "cli",
                 "AGENT_AUTH_TOKEN": "bridge-secret-64hex"},
            capture_output=True, timeout=60)
        assert result.returncode == 0, f"pebble cli: entrypoint failed: {result.stderr}"
        assert mark.exists(), f"pebble cli: stub did not run: {result.stderr}"
        assert mark.read_text().strip() == "ABSENT", \
            "pebble cli: hub-handshake token reached the one-shot task env (WI#373)"


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

for worker in ("opencode", "opencode2"):
    run_websocket_startup(worker)
    print(f"PASS: {worker} serve process runs without the hub-handshake token (WI#373)")
for worker in ("kimi", "opencode", "opencode2"):
    modes = ("cli",) if worker == "kimi" else ("cli", "acp")
    for mode in modes:
        run_oneshot_startup(worker, mode)
        print(f"PASS: {worker} {mode} one-shot runs without the hub-handshake token (WI#373)")
run_pebble_cli_startup()
print("PASS: pebble cli one-shot runs without the hub-handshake token (WI#373)")
PY
