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
    with temp_root(f"{worker}-llm-") as temp:
        root = Path(temp)
        home, app, workspace = root / "home", root / "app", root / "workspace"
        for path in (home, app / "config", workspace):
            path.mkdir(parents=True)
        stage_setup(source, app, home)
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
        hand_tree(root)
        result = subprocess.run(["bash"], input=script, text=True, env=env,
                                capture_output=True, timeout=15, **as_ci_user())
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


# WI#400: the worker entrypoints are root launchers that hand task-side setup
# to entrypoint-task-setup.sh (staged beside entrypoint.sh with the same path
# relocation). When the harness itself runs as root (the CI shape), the
# setup script refuses root by design, so the scratch tree is handed to an
# unprivileged uid and every entrypoint runs as that uid — the same
# unprivileged branch the entrypoints take on host dev runs.
SETUP_RUN_ID = 65534

def as_ci_user():
    """subprocess kwargs dropping to the unprivileged uid when run as root."""
    if os.geteuid() == 0:
        return {"user": SETUP_RUN_ID, "group": SETUP_RUN_ID, "extra_groups": []}
    return {}


_exec_temp_base = None


def _probe_exec(directory):
    """True when an executable stub runs from directory — probed AS the
    unprivileged uid when the harness is root, because the WI#400 entrypoint
    and its task-setup script then exec from inside the chosen tree."""
    probe = directory / f".exec-probe-{os.getpid()}.sh"
    try:
        directory.mkdir(parents=True, exist_ok=True)
        probe.write_text("#!/bin/sh\nexit 0\n")
        probe.chmod(0o755)
        return subprocess.run([str(probe)], capture_output=True,
                              timeout=15, **as_ci_user()).returncode == 0
    except OSError:
        return False
    finally:
        probe.unlink(missing_ok=True)


def temp_root(prefix):
    """A TemporaryDirectory rooted where scripts can actually EXECUTE.
    Dev VMs may mount /tmp noexec and the WI#400 entrypoints exec their
    task-setup script from inside the temp tree; probe the system temp dir
    first (the CI shape is unchanged), then /var/tmp, then a per-run base
    under $HOME that is removed at exit. When the harness runs as root the
    probe drops to the same unprivileged uid the entrypoints run as, so a
    0700 $HOME (e.g. /root) can never be chosen for a dropped run."""
    global _exec_temp_base
    if _exec_temp_base is None:
        import atexit
        import shutil
        system = Path(tempfile.gettempdir())
        if _probe_exec(system):
            _exec_temp_base = system
        else:
            for candidate in (Path("/var/tmp"),
                              Path(os.environ.get("HOME", "")) / f".worker-entrypoint-tmp-{os.getpid()}"):
                if str(candidate) in ("", "/") or not candidate.parent.exists():
                    continue
                if _probe_exec(candidate):
                    if candidate == Path("/var/tmp"):
                        _exec_temp_base = candidate
                    else:
                        atexit.register(shutil.rmtree, candidate, ignore_errors=True)
                        _exec_temp_base = candidate
                    break
            else:
                raise AssertionError(
                    "no executable directory found for the WI#400 entrypoint temp trees")
    return tempfile.TemporaryDirectory(
        prefix=prefix,
        dir=None if _exec_temp_base == Path(tempfile.gettempdir()) else _exec_temp_base,
    )


def hand_tree(path):
    """Recursively hand a scratch tree to the unprivileged uid (root only)."""
    if os.geteuid() != 0:
        return
    for dirpath, dirnames, filenames in os.walk(path):
        for name in dirnames + filenames:
            try:
                os.chown(os.path.join(dirpath, name), SETUP_RUN_ID, SETUP_RUN_ID,
                         follow_symlinks=False)
            except OSError:
                pass
    try:
        os.chown(path, SETUP_RUN_ID, SETUP_RUN_ID)
    except OSError:
        pass


def stage_setup(source, app, home):
    """Stage entrypoint-task-setup.sh (WI#400) with the entrypoint's own
    /app/ and /home/<user>/ relocation applied to its text."""
    setup = source / "entrypoint-task-setup.sh"
    if not setup.exists():
        return
    text = setup.read_text().replace("/app/", str(app) + "/")
    for user in ("opencode", "kimi"):
        text = text.replace(f"/home/{user}/", str(home) + "/")
    target = app / "entrypoint-task-setup.sh"
    target.write_text(text)
    target.chmod(0o755)


def relocate_entrypoint(entrypoint, app, home, bin_dir=None):
    """Apply the standard relocations to an entrypoint's text: /app/ and
    /home/<user>/ into the scratch tree, and (WI#454/ME-1) the stub bin_dir
    in front of the entrypoint's pinned root PATH — the pin is the ME-1 fix
    and must survive the harness run, so the stubs ride inside it instead
    of the inherited PATH the pin discards."""
    script = entrypoint.replace("/app/", str(app) + "/")
    for user in ("opencode", "kimi"):
        script = script.replace(f"/home/{user}/", str(home) + "/")
    if bin_dir is not None:
        script = script.replace(
            "export PATH=/usr/local/cargo/bin:/usr/local/go/bin",
            f"export PATH={bin_dir}:/usr/local/cargo/bin:/usr/local/go/bin",
            1,
        )
    return script


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
    with temp_root(f"{worker}-ws373-") as temp:
        root = Path(temp)
        home, app, workspace = root / "home", root / "app", root / "workspace"
        bin_dir = execable_dir(root)
        for path in (home, app / "config", workspace / ".opencode"):
            path.mkdir(parents=True)
        stage_setup(source, app, home)
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
        # WI#400 LP-1: bun loads bunfig.toml/.env from its cwd, so the root
        # bridge must be started from /app, never from the task workspace.
        bridge_cwd = root / "bridge-cwd.txt"
        bun_stub.write_text(
            "#!/bin/sh\n"
            f"pwd > {bridge_cwd}\n"
            f"if [ -n \"$AGENT_AUTH_TOKEN\" ]; then echo PRESENT > {bridge_mark};"
            "elif [ -n \"$AGENT_AUTH_TOKEN_FILE\" ] && [ \"$(cat"
            " \"$AGENT_AUTH_TOKEN_FILE\" 2>/dev/null)\" = bridge-secret-64hex ];"
            f" then echo PRESENT > {bridge_mark};"
            f" else echo ABSENT > {bridge_mark}; fi\n"
            "exit 0\n"
        )
        bun_stub.chmod(0o755)
        script = relocate_entrypoint(entrypoint, app, home, bin_dir).replace("cd /app\n", f"cd {app}\n")
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
            "LUNARWING_TOKEN_DIR": str(root / "run-lunarwing"),
            "AGENT_AUTH_TOKEN": "bridge-secret-64hex",
        }
        hand_tree(root)
        hand_tree(bin_dir)
        result = subprocess.run(["bash"], input=script, text=True, env=env,
                                capture_output=True, timeout=60, **as_ci_user())
        assert result.returncode == 0, f"{worker}: entrypoint failed: {result.stderr}"
        assert serve_mark.exists() and bridge_mark.exists(), \
            f"{worker}: stubs did not run: {result.stderr}"
        assert serve_mark.read_text().strip() == "ABSENT", \
            f"{worker}: hub-handshake token reached the serve process env (WI#373)"
        if worker == "opencode":  # opencode2's bridge is not root (USER 10003), so cwd is not a boundary there
            assert bridge_cwd.exists() and \
                Path(bridge_cwd.read_text().strip()).resolve() == app.resolve(), \
                f"{worker}: the bridge must be exec-ed from /app, not the workspace (WI#400 LP-1): {bridge_cwd.read_text() if bridge_cwd.exists() else 'no cwd mark'}"
        assert bridge_mark.read_text().strip() == "PRESENT", \
            f"{worker}: bridge lost its handshake credential (WI#373/WI#400 env-or-file)"


def run_oneshot_startup(worker, mode):
    """WI#373 (one-shot lanes): cli/acp modes exec a single task process —
    the same untrusted-prompt boundary — so the exec'd process must not
    inherit the hub-handshake credential either."""
    directory, filename = workers[worker]
    source = repo / directory
    entrypoint = (source / "entrypoint.sh").read_text()
    with temp_root(f"{worker}-cli373-") as temp:
        root = Path(temp)
        home, app, workspace = root / "home", root / "app", root / "workspace"
        bin_dir = execable_dir(root)
        for path in (home, app / "config", workspace / ".opencode"):
            path.mkdir(parents=True)
        stage_setup(source, app, home)
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
        script = relocate_entrypoint(entrypoint, app, home, bin_dir)
        env = {
            "PATH": f"{bin_dir}:{os.environ['PATH']}",
            "HOME": str(home), "TMPDIR": temp,
            "WORKSPACE_ROOT": str(workspace),
            "KIMI_CODE_HOME": str(home / ".kimi-code"),
            "KIMI_CONFIG_SOURCE": str(app / "config" / filename),
            f"{worker.upper()}_BASE_URL": endpoint,
            "HEALTH_PORT": str(free_port()),
            "WS_STATE_FILE": str(root / "ws-state.json"),
            "LUNARWING_TOKEN_DIR": str(root / "run-lunarwing"),
            "AGENT_AUTH_TOKEN": "bridge-secret-64hex",
        }
        if worker != "kimi":
            env["OPENCODE_BIN"] = str(stub)
        env["KIMI_MODE" if worker == "kimi" else "OPENCODE_MODE"] = mode
        hand_tree(root)
        hand_tree(bin_dir)
        result = subprocess.run(["bash", "-s", "--", "probe prompt"],
                                input=script, text=True, env=env,
                                capture_output=True, timeout=60, **as_ci_user())
        assert result.returncode == 0, \
            f"{worker} {mode}: entrypoint failed: {result.stderr}"
        assert mark.exists(), f"{worker} {mode}: stub did not run: {result.stderr}"
        assert mark.read_text().strip() == "ABSENT", \
            f"{worker} {mode}: hub-handshake token reached the one-shot task env (WI#373)"


def run_pebble_cli_startup():
    """WI#373 (pebble cli mode): same one-shot boundary; the pebble process
    must not inherit the hub-handshake credential."""
    entrypoint = (repo / "pebble4lunarwing" / "entrypoint.sh").read_text()
    with temp_root("pebble-cli373-") as temp:
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
        hand_tree(root)
        hand_tree(bin_dir)
        result = subprocess.run(
            ["bash", "-s", "--", "probe prompt"],
            input=entrypoint, text=True,
            env={"PATH": f"{bin_dir}:{os.environ['PATH']}",
                 "HOME": str(root), "TMPDIR": temp,
                 "PEBBLE_MODE": "cli",
                 "LUNARWING_TOKEN_DIR": str(root / "run-lunarwing"),
                 "AGENT_AUTH_TOKEN": "bridge-secret-64hex"},
            capture_output=True, timeout=60, **as_ci_user())
        assert result.returncode == 0, f"pebble cli: entrypoint failed: {result.stderr}"
        assert mark.exists(), f"pebble cli: stub did not run: {result.stderr}"
        assert mark.read_text().strip() == "ABSENT", \
            "pebble cli: hub-handshake token reached the one-shot task env (WI#373)"


def run_pebble_token_layout():
    """WI#400 KW-7c/LD-13: the pebble entrypoint's credential handoff layout
    — 0700 directory, 0400 file holding exactly the token, and BOTH
    AGENT_AUTH_TOKEN and AGENT_AUTH_TOKEN_FILE absent from the task env.
    (The in-container owner is root; ownership is asserted in-image — modes
    and stripping are pinned here.)"""
    import stat as statmod
    entrypoint = (repo / "pebble4lunarwing" / "entrypoint.sh").read_text()
    with temp_root("pebble-token400-") as temp:
        root = Path(temp)
        bin_dir = execable_dir(root)
        token_dir = root / "token"
        mark = root / "task-env.txt"
        stub = bin_dir / "pebble"
        stub.write_text(
            "#!/bin/sh\n"
            "{ echo \"token=${AGENT_AUTH_TOKEN:-unset}\"; "
            "echo \"file=${AGENT_AUTH_TOKEN_FILE:-unset}\"; } > " + str(mark) + "\n"
            "exit 0\n"
        )
        stub.chmod(0o755)
        hand_tree(root)
        hand_tree(bin_dir)
        result = subprocess.run(
            ["bash", "-s", "--", "probe prompt"],
            input=entrypoint, text=True,
            env={"PATH": f"{bin_dir}:{os.environ['PATH']}",
                 "HOME": str(root), "TMPDIR": temp,
                 "PEBBLE_MODE": "cli",
                 "LUNARWING_TOKEN_DIR": str(token_dir),
                 "AGENT_AUTH_TOKEN": "bridge-secret-64hex"},
            capture_output=True, timeout=60, **as_ci_user())
        assert result.returncode == 0, f"pebble token layout: entrypoint failed: {result.stderr}"
        token_file = token_dir / "agent_auth_token"
        assert token_dir.is_dir(), "token directory not created"
        assert statmod.S_IMODE(token_dir.stat().st_mode) == 0o700, \
            f"token directory mode {oct(statmod.S_IMODE(token_dir.stat().st_mode))} != 0o700"
        assert token_file.is_file(), "token file not created"
        assert statmod.S_IMODE(token_file.stat().st_mode) == 0o400, \
            f"token file mode {oct(statmod.S_IMODE(token_file.stat().st_mode))} != 0o400"
        assert token_file.read_text() == "bridge-secret-64hex", \
            "token file content must be exactly the credential"
        lines = mark.read_text().splitlines()
        assert lines == ["token=unset", "file=unset"], \
            f"task env must lose both the credential and its file pointer: {lines}"


def run_pebble_cli_drop_argv():
    """WI#400 KW-7c/LD-13: with the identity pinned and the harness running as
    root (the CI shape), the cli one-shot must exec setpriv with the exact
    privilege-dropping flags before pebble. Unprivileged hosts cannot take
    the root branch, so the pin is root-only."""
    if os.geteuid() != 0:
        return
    entrypoint = (repo / "pebble4lunarwing" / "entrypoint.sh").read_text()
    with temp_root("pebble-clidrop-") as temp:
        root = Path(temp)
        bin_dir = execable_dir(root)
        mark = root / "drop-argv.txt"
        stub = bin_dir / "pebble"
        stub.write_text("#!/bin/sh\necho RAN_PEBBLE > " + str(mark) + "\nexit 0\n")
        stub.chmod(0o755)
        setpriv_stub = bin_dir / "setpriv"
        setpriv_stub.write_text(
            "#!/bin/sh\nprintf '<%s>' \"$@\" > " + str(mark) + "\nprintf '\\n' >> " + str(mark) + "\nexit 0\n"
        )
        setpriv_stub.chmod(0o755)
        hand_tree(root)
        hand_tree(bin_dir)
        result = subprocess.run(
            ["bash", "-s", "--", "probe prompt"],
            input=entrypoint, text=True,
            env={"PATH": f"{bin_dir}:{os.environ['PATH']}",
                 "HOME": str(root), "TMPDIR": temp,
                 "PEBBLE_MODE": "cli",
                 "LUNARWING_TOKEN_DIR": str(root / "run-lunarwing"),
                 "PEBBLE_TASK_UID": "10004",
                 "PEBBLE_TASK_GID": "10004"},
            capture_output=True, timeout=60)
        assert result.returncode == 0, f"pebble cli drop: entrypoint failed: {result.stderr}"
        argv = mark.read_text().strip()
        assert argv == (
            "<--reuid=10004><--regid=10004><--clear-groups>"
            "<--ambient-caps=-all><--inh-caps=-all><--no-new-privs><-->"
            "<env><-u><AGENT_AUTH_TOKEN><-u><AGENT_AUTH_TOKEN_FILE>"
            "<pebble><--permission-mode><danger-full-access>"
            "<--model><openai/gpt-5.2><--output-format><text><probe prompt>"
        ), f"setpriv argv missing mandatory flags (WI#400 KW-2/LD-13): {argv}"


def run_task_drop_argv(worker):
    """WI#400 KW-2/LD-7: with the harness running as root (the CI shape), the
    kimi and opencode cli one-shots must exec setpriv with the exact
    privilege-dropping flags (the cap-clearing pair AND --no-new-privs)
    before the task binary. Unprivileged hosts cannot take the root branch,
    so the pin is root-only."""
    if os.geteuid() != 0:
        return
    directory, filename = workers[worker]
    source = repo / directory
    entrypoint = (source / "entrypoint.sh").read_text()
    user = "kimi" if worker == "kimi" else "opencode"
    task_uid = {"kimi": "10002", "opencode": "10001"}[worker]
    with temp_root(f"{worker}-dropargv-") as temp:
        root = Path(temp)
        bin_dir = execable_dir(root)
        mark = root / "drop-argv.txt"
        task_stub = bin_dir / user
        task_stub.write_text("#!/bin/sh\necho RAN_TASK > " + str(mark) + "\nexit 0\n")
        task_stub.chmod(0o755)
        setpriv_stub = bin_dir / "setpriv"
        setpriv_stub.write_text(
            "#!/bin/sh\nprintf '<%s>' \"$@\" > " + str(mark) + "\nprintf '\\n' >> " + str(mark) + "\nexit 0\n"
        )
        setpriv_stub.chmod(0o755)
        hand_tree(root)
        hand_tree(bin_dir)
        env = {
            "PATH": f"{bin_dir}:{os.environ['PATH']}",
            "HOME": str(root / "home"), "TMPDIR": temp,
            "WORKSPACE_ROOT": str(root / "workspace"),
            "LUNARWING_TOKEN_DIR": str(root / "run-lunarwing"),
            f"{worker.upper()}_MODE": "cli",
            **({"OPENCODE_BIN": str(task_stub)} if worker != "kimi" else {}),
        }
        (root / "home").mkdir()
        (root / "workspace").mkdir()
        (root / "app" / "config").mkdir(parents=True)
        result = subprocess.run(
            ["bash", "-s", "--", "probe prompt"],
            input=relocate_entrypoint(entrypoint, root / "app", root / "home", bin_dir),
            text=True, env=env, capture_output=True, timeout=60)
        assert result.returncode == 0, f"{worker} drop: entrypoint failed: {result.stderr}"
        argv = mark.read_text().strip()
        # The kimi/opencode SETPRIV_FLAGS arrays carry no bare `--`
        # separator (unlike pebble's entrypoint); the flags end at
        # --no-new-privs and the env command follows directly.
        flags = (
            f"<--reuid={task_uid}><--regid={task_uid}><--clear-groups>"
            "<--ambient-caps=-all><--inh-caps=-all><--no-new-privs>"
        )
        # WI#454/ME-1: the task toolchain PATH is handed over at the drop
        # (PATH="$TASK_PATH"), not inherited from the image PATH the root
        # launcher pins to root-owned directories. TASK_PATH appends the
        # PINNED root PATH (the launcher re-exports PATH before defining
        # TASK_PATH), not the test's inherited PATH.
        pinned_path = (
            f"{bin_dir}:"
            "/usr/local/cargo/bin:/usr/local/go/bin:/usr/local/sbin:/usr/local/bin"
            ":/usr/sbin:/usr/bin:/sbin:/bin"
        )
        # MV-2: the opencode entrypoint's root PATH appends
        # /usr/local/bun-node-fallback-bin (MI-2/WI#454), and TASK_PATH
        # inherits $PATH, so the setpriv argv carries it.  Kimi's
        # node:22 base does not need the fallback.
        if worker != "kimi":
            pinned_path = pinned_path + ":/usr/local/bun-node-fallback-bin"
        # The harness's /home/<user>/ relocation also rewrites the
        # TASK_PATH text, so the expected value follows the temp home.
        if worker == "kimi":
            task_path = (
                f"{root}/home/.cargo/bin:/usr/local/cargo/bin:/usr/local/go/bin"
                f":{root}/home/go/bin:" + pinned_path
            )
            expected = flags + (
                "<env><-u><AGENT_AUTH_TOKEN><-u><AGENT_AUTH_TOKEN_FILE>"
                "<HOME=/home/kimi><PATH=" + task_path + ">"
                "<kimi><-p><probe prompt><--output-format><stream-json>"
            )
        else:
            task_path = (
                f"{root}/home/.cargo/bin:/usr/local/cargo/bin:/usr/local/go/bin"
                f":{root}/home/go/bin:" + pinned_path
            )
            expected = flags + (
                "<env><-u><AGENT_AUTH_TOKEN><-u><AGENT_AUTH_TOKEN_FILE>"
                "<HOME=/home/opencode><PATH=" + task_path + ">"
                f"<{task_stub}><run><probe prompt>"
            )
        assert argv == expected, \
            f"{worker} setpriv argv missing mandatory flags (WI#400 KW-2/LD-7): {argv}"


def run_task_zero_uid_refusal(worker):
    """WI#400 LP-17: the kimi/opencode entrypoints must refuse a zero (or
    non-numeric) task id right after the defaults — a zero id makes the
    setpriv privilege drop a no-op while every caller keeps treating the
    split as active. The refusal fires before any service start, so the pin
    works in every uid shape."""
    directory, _ = workers[worker]
    entrypoint = (repo / directory / "entrypoint.sh").read_text()
    prefix = worker.upper()
    with temp_root(f"{worker}-zeroid-") as temp:
        root = Path(temp)
        for label, env_overrides in (
            ("zero uid", {f"{prefix}_TASK_UID": "0"}),
            ("zero gid", {f"{prefix}_TASK_GID": "0"}),
            ("non-numeric uid", {f"{prefix}_TASK_UID": "1000x"}),
        ):
            env = {
                "PATH": os.environ["PATH"], "HOME": str(root), "TMPDIR": temp,
                f"{prefix}_MODE": "cli",
                **env_overrides,
            }
            result = subprocess.run(
                ["bash", "-s", "--", "probe prompt"],
                input=entrypoint, text=True, env=env,
                capture_output=True, timeout=60, **as_ci_user())
            assert result.returncode != 0, \
                f"{worker} entrypoint accepted a {label} (WI#400 LP-17)"
            assert "must be non-zero numeric ids (a zero id would make the privilege drop a no-op)" in result.stderr, \
                f"{worker} {label} refusal must name the no-op-drop reason: {result.stderr}"


def run_pebble_websocket_inherit():
    """WI#400 x WI#422 (run-171): the rootful tenant-uid launch passes
    PEBBLE_TASK_UID/GID=inherit. That is the one sanctioned non-root shape:
    the websocket entrypoint must start (no LD-8 refusal), must NOT stage a
    root-only credential file (there is no root to own it), and must hand
    the bridge the credential in its environment, pointer-free."""
    entrypoint = (repo / "pebble4lunarwing" / "entrypoint.sh").read_text()
    with temp_root("pebble-wsinh-") as temp:
        root = Path(temp)
        bin_dir = execable_dir(root)
        token_dir = root / "token"
        mark = root / "bridge-env.txt"
        id_stub = bin_dir / "id"
        id_stub.write_text("#!/bin/sh\necho 1042\n")
        id_stub.chmod(0o755)
        bridge_stub = bin_dir / "pebble4lunarwing"
        bridge_stub.write_text(
            "#!/bin/sh\n"
            "{ echo \"token=${AGENT_AUTH_TOKEN:-unset}\"; "
            "echo \"file=${AGENT_AUTH_TOKEN_FILE:-unset}\"; } > " + str(mark) + "\n"
            "exit 0\n"
        )
        bridge_stub.chmod(0o755)
        hand_tree(root)
        hand_tree(bin_dir)
        result = subprocess.run(
            ["bash", "-s"],
            input=entrypoint, text=True,
            env={"PATH": f"{bin_dir}:{os.environ['PATH']}",
                 "HOME": str(root), "TMPDIR": temp,
                 "PEBBLE_MODE": "websocket",
                 "PEBBLE_TASK_UID": "inherit",
                 "PEBBLE_TASK_GID": "inherit",
                 "LUNARWING_TOKEN_DIR": str(token_dir),
                 "AGENT_AUTH_TOKEN": "bridge-secret-64hex"},
            capture_output=True, timeout=60, **as_ci_user())
        assert result.returncode == 0, \
            f"pebble websocket entrypoint refused the inherit pair as a non-root bridge: {result.stderr}"
        assert "no privilege split" in result.stdout, \
            f"inherit launch must say there is no split on this lane: {result.stdout}"
        assert not token_dir.exists(), \
            "inherit launch must not stage a credential file (no root to own it)"
        lines = mark.read_text().splitlines()
        assert lines == ["token=bridge-secret-64hex", "file=unset"], \
            f"inherit launch must hand the bridge the credential in env, pointer-free: {lines}"

def run_pebble_websocket_refusal():
    """WI#400 LD-8: when the bridge itself runs as the task uid (the rootless
    keep-id + USER-less-image failure mode), the websocket entrypoint must
    refuse to start. An `id` stub reports the task uid, so the pin works
    whatever uid the harness itself runs as."""
    entrypoint = (repo / "pebble4lunarwing" / "entrypoint.sh").read_text()
    with temp_root("pebble-wsref-") as temp:
        root = Path(temp)
        bin_dir = execable_dir(root)
        id_stub = bin_dir / "id"
        id_stub.write_text("#!/bin/sh\necho 10004\n")
        id_stub.chmod(0o755)
        hand_tree(root)
        hand_tree(bin_dir)
        result = subprocess.run(
            ["bash", "-s"],
            input=entrypoint, text=True,
            env={"PATH": f"{bin_dir}:{os.environ['PATH']}",
                 "HOME": str(root), "TMPDIR": temp,
                 "PEBBLE_MODE": "websocket",
                 "PEBBLE_TASK_UID": "10004",
                 "PEBBLE_TASK_GID": "10004"},
            capture_output=True, timeout=60, **as_ci_user())
        assert result.returncode != 0, \
            "pebble websocket entrypoint accepted running as the task uid (WI#400 LD-8)"
        assert "refusing to start" in result.stderr and "container root" in result.stderr, \
            f"refusal must explain the root-bridge requirement: {result.stderr}"


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
run_pebble_token_layout()
print("PASS: pebble entrypoint token handoff layout is 0700/0400 and fully stripped (WI#400 KW-7c)")
if os.geteuid() == 0:
    run_pebble_cli_drop_argv()
    print("PASS: pebble cli one-shot drops through setpriv with the mandatory flags (WI#400 KW-2/LD-13, root)")
    for worker in ("kimi", "opencode"):
        run_task_drop_argv(worker)
        print(f"PASS: {worker} cli one-shot drops through setpriv with the mandatory flags (WI#400 KW-2/LD-7, root)")
for worker in ("kimi", "opencode"):
    run_task_zero_uid_refusal(worker)
    print(f"PASS: {worker} entrypoint refuses zero/non-numeric task ids (WI#400 LP-17)")
run_pebble_websocket_refusal()
print("PASS: pebble websocket entrypoint refuses to run as the task uid (WI#400 LD-8)")
run_pebble_websocket_inherit()
print("PASS: pebble websocket entrypoint accepts the rootful inherit pair and keeps the credential in env (WI#400 x WI#422)")
PY
