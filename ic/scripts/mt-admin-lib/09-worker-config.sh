render_worker_openrc_unit() {
  local name="$1" worker="$2" health_port="${3:-8443}"
  ensure_container_runtime
  local runtime_bin="" container="lunarwing-${worker}-${name}" uid home
  [[ -n "${CONTAINER_RT:-}" ]] && runtime_bin="$(command -v "$CONTAINER_RT" 2>/dev/null || true)"
  uid="$(id -u "$name" 2>/dev/null || echo "")"
  home="$(tenant_home "$name")"

  cat >"/etc/init.d/${container}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing ${worker} worker ($name)"

: "\${wk_runtime:=$runtime_bin}"
: "\${wk_container:=$container}"
: "\${wk_rootless:=$MT_ROOTLESS}"
: "\${wk_user:=$name}"
: "\${wk_home:=$home}"
: "\${wk_uid:=$uid}"
: "\${wk_health_port:=$health_port}"
: "\${wk_wait:=60}"

depend() {
    need net localmount
    after firewall lunarwing-${name}
}

# Run the container runtime as the owning user (rootless) or root (rootful).
_wk() {
    if [ "\${wk_rootless}" = "true" ]; then
        sudo -u "\${wk_user}" env HOME="\${wk_home}" XDG_RUNTIME_DIR="/run/user/\${wk_uid}" "\${wk_runtime}" "\$@"
    else
        "\${wk_runtime}" "\$@"
    fi
}

# Healthy = container running AND its internal /health endpoint answers.
_wk_healthy() {
    [ "\$(_wk inspect -f '{{.State.Running}}' "\${wk_container}" 2>/dev/null)" = "true" ] || return 1
    _wk exec "\${wk_container}" curl -sf -o /dev/null --max-time 3 "http://127.0.0.1:\${wk_health_port}/health" 2>/dev/null
}

start() {
    [ -n "\${wk_runtime}" ] && [ -x "\${wk_runtime}" ] || { ewarn "no container runtime; skipping ${worker} for $name"; return 0; }
    ebegin "Starting ${worker} worker (\${wk_container})"
    if [ "\${wk_rootless}" = "true" ]; then
        checkpath -d -m 0700 -o "\${wk_user}:\${wk_user}" "/run/user/\${wk_uid}"
    fi
    _wk start "\${wk_container}" >/dev/null 2>&1 || { eend 1 "container start failed"; return 1; }
    _w=0
    while ! _wk_healthy; do
        _w=\$((_w + 1))
        [ "\$_w" -lt "\${wk_wait}" ] || { eend 1 "${worker} worker not healthy after \${wk_wait}s"; return 1; }
        sleep 1
    done
    eend 0
}

stop() {
    [ -n "\${wk_runtime}" ] && [ -x "\${wk_runtime}" ] || return 0
    ebegin "Stopping ${worker} worker (\${wk_container})"
    _wk stop --time 30 "\${wk_container}" >/dev/null 2>&1
    eend 0
}

status() {
    # Standard OpenRC started/stopped wording so health-openrc.sh classifies it.
    if _wk_healthy; then
        einfo "\${wk_container}: started"; return 0
    fi
    einfo "\${wk_container}: stopped"; return 3
}
INITEOF
  chmod 0755 "/etc/init.d/${container}"
}

# Promote a (rootless) worker container to a dedicated OpenRC unit so it is
# health-monitored, self-healed, and boot-persistent. No-op unless OpenRC.
_register_worker_unit() {
  local name="$1" worker="$2"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  render_worker_openrc_unit "$name" "$worker"
  rc-update add "lunarwing-${worker}-${name}" default >/dev/null 2>&1 || true
  rc-service "lunarwing-${worker}-${name}" start >/dev/null 2>&1 || true
  say "registered OpenRC unit lunarwing-${worker}-${name} (health-monitored, boot-persistent)"
  local container="lunarwing-${worker}-${name}"
  local uid home
  uid="$(id -u "$name" 2>/dev/null || echo "")"
  home="$(tenant_home "$name")"
  _register_babysitter "$name" "$worker" "$container" "$uid" "$home"
}

# Tear down a worker's OpenRC unit (boot-disable + remove the init script).
_deregister_worker_unit() {
  local name="$1" worker="$2"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  [[ -f "/etc/init.d/lunarwing-${worker}-${name}" ]] || return 0
  _deregister_babysitter "lunarwing-${worker}-${name}"
  rc-service "lunarwing-${worker}-${name}" stop >/dev/null 2>&1 || true
  rc-update del "lunarwing-${worker}-${name}" default >/dev/null 2>&1 || true
  rm -f "/etc/init.d/lunarwing-${worker}-${name}" "/etc/conf.d/lunarwing-${worker}-${name}"
}

# --- Batch B: external worker config (upgrade/ensure/remove) ---
upgrade_external_worker_timeout() {
  local name="$1" worker="$2" config_path="$3"
  local awk_status temp_path

  if [[ "$MIGRATE_LEGACY_EXTERNAL_WORKER_TIMEOUTS" != "true" ]] ||
     [[ "$DEFAULT_EXTERNAL_WORKER_TIMEOUT_MS" == "$LEGACY_EXTERNAL_WORKER_TIMEOUT_MS" ]]; then
    return 1
  fi

  if ! temp_path="$(mktemp "${config_path}.tmp.XXXXXX")"; then
    return 2
  fi

  if awk \
    -v worker="$worker" \
    -v old_timeout="$LEGACY_EXTERNAL_WORKER_TIMEOUT_MS" \
    -v new_timeout="$DEFAULT_EXTERNAL_WORKER_TIMEOUT_MS" '
      BEGIN {
        in_external_worker = 0
        target_worker = 0
        changed = 0
      }
      /^\[\[sandbox\.external_workers\]\][[:space:]]*$/ {
        in_external_worker = 1
        target_worker = 0
        print
        next
      }
      /^\[/ {
        in_external_worker = 0
        target_worker = 0
      }
      in_external_worker && $0 == "name = \"" worker "\"" {
        target_worker = 1
      }
      in_external_worker && target_worker && $0 == "timeout_ms = " old_timeout {
        print "timeout_ms = " new_timeout
        changed = 1
        next
      }
      { print }
      END { if (!changed) exit 42 }
    ' "$config_path" >"$temp_path"; then
    if ! chown "$name:$name" "$temp_path" ||
       ! chmod 600 "$temp_path" ||
       ! mv -f -- "$temp_path" "$config_path"; then
      rm -f -- "$temp_path"
      return 2
    fi
    say "upgraded external worker '$worker' timeout to ${DEFAULT_EXTERNAL_WORKER_TIMEOUT_MS}ms"
    return 0
  else
    awk_status=$?
    rm -f -- "$temp_path"
    if [[ "$awk_status" -eq 42 ]]; then
      return 1
    fi
    return 2
  fi
}

ensure_external_worker_config() {
  local name="$1"      # tenant name
  local worker="$2"    # logical worker name, matches create_job mode
  local port_key="$3"  # ports-registry key for its WSS port

  local state_dir env_path config_path wss_port auth_token

  if [[ "$worker" == "kimi" ]] && ! tenant_worker_enabled "$name" kimi; then
    say "kimi worker not selected for $name (skipping external-worker config)"
    return 0
  fi
  state_dir="$(tenant_state_dir "$name")"
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  config_path="$state_dir/config.toml"

  wss_port="$(ports_get "$name" "$port_key")"
  if [[ -z "$wss_port" ]]; then
    say "no $port_key port allocated for $name (skipping $worker external-worker config)"
    return 0
  fi

  if [[ ! "$DEFAULT_EXTERNAL_WORKER_TIMEOUT_MS" =~ ^[1-9][0-9]*$ ]]; then
    die "LUNARWING_MT_EXTERNAL_WORKER_TIMEOUT_MS must be a positive integer"
  fi

  # Already configured? Upgrade only the exact legacy timeout, then skip so
  # re-runs / patch-env are idempotent. The daemon's
  # TOML writer emits `name = "<worker>"` identically, so this also matches a
  # file that has been load-modify-saved by `/model`.
  if [[ -f "$config_path" ]] && grep -q "name = \"$worker\"" "$config_path" 2>/dev/null; then
    local upgrade_status=0
    upgrade_external_worker_timeout "$name" "$worker" "$config_path" || upgrade_status=$?
    case "$upgrade_status" in
      0) ;;
      1) say "external worker '$worker' already configured in $config_path (skipping)" ;;
      *) die "failed to upgrade external worker '$worker' timeout in $config_path" ;;
    esac
    return 0
  fi

  auth_token="$(_read_env_value "$env_path" GATEWAY_AUTH_TOKEN)"

  # add-tenant runs before the daemon ever starts, so config.toml usually does
  # not exist yet — create it with a header. Appending a fresh
  # `[[sandbox.external_workers]]` array-of-tables to an existing file is valid
  # TOML because we only ever append when no entry for this array exists yet.
  if [[ ! -f "$config_path" ]]; then
    sudo -u "$name" mkdir -p "$state_dir"
    (
      umask 077
      cat >"$config_path" <<'HDR'
# LunarWing tenant configuration (auto-generated by lunarwing-mt-admin.sh).
#
# Priority: env var > this file > database settings > defaults.
# The external-worker blocks below wire create_job(mode: "<name>") to the
# per-tenant worker containers. Hand edits outside these blocks are preserved.
HDR
    )
  fi

  {
    printf '\n# External worker: %s — create_job(mode: "%s")\n' "$worker" "$worker"
    printf '[[sandbox.external_workers]]\n'
    printf 'name = "%s"\n' "$worker"
    printf 'url = "ws://127.0.0.1:%s/ws/agent"\n' "$wss_port"
    if [[ -n "$auth_token" ]]; then
      printf 'auth_token = "%s"\n' "$auth_token"
    fi
    printf 'timeout_ms = %s\n' "$DEFAULT_EXTERNAL_WORKER_TIMEOUT_MS"
  } >>"$config_path"

  if [[ -z "$auth_token" ]]; then
    say "WARNING: GATEWAY_AUTH_TOKEN not found in $env_path;" \
        "'$worker' worker config written WITHOUT auth_token (connections will fail until set)"
  fi

  chown "$name:$name" "$config_path"
  chmod 600 "$config_path"
  say "wrote $worker external-worker config to $config_path (ws://127.0.0.1:$wss_port/ws/agent)"
}

remove_external_worker_config() {
  local name="$1" worker="$2" config_path temp_path backup_path rc=0
  config_path="$(tenant_state_dir "$name")/config.toml"
  [[ -f "$config_path" ]] || return 0
  [[ ! -L "$config_path" ]] || { say "refusing symlinked tenant config: $config_path" >&2; return 1; }
  require_cmd python3
  temp_path="$(mktemp "${config_path}.tmp.XXXXXX")"
  backup_path="${config_path}.pre-${worker}-retirement"

  python3 - "$config_path" "$temp_path" "$backup_path" "$worker" <<'PY' || rc=$?
import os
import re
import stat
import sys
import tomllib
from pathlib import Path

source, target, backup = map(Path, sys.argv[1:4])
worker = sys.argv[4]
source_fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
source_stat = os.fstat(source_fd)
if not stat.S_ISREG(source_stat.st_mode):
    os.close(source_fd)
    raise RuntimeError("tenant config is not a regular file")
with os.fdopen(source_fd, encoding="utf-8") as handle:
    text = handle.read()
tomllib.loads(text)
lines = text.splitlines(keepends=True)
root = "sandbox.external_workers"
header_re = re.compile(r"^\s*(\[\[?)([^\]]+)(\]\]?)\s*(?:#.*)?$")
name_re = re.compile(rf"^\s*name\s*=\s*(['\"]){re.escape(worker)}\1\s*(?:#.*)?$")

def header(line: str):
    match = header_re.match(line.rstrip("\r\n"))
    if not match or len(match.group(1)) != len(match.group(3)):
        return None
    return len(match.group(1)), "".join(match.group(2).split())

ranges = []
index = 0
while index < len(lines):
    info = header(lines[index])
    if info != (2, root):
        index += 1
        continue
    end = len(lines)
    scalar_end = len(lines)
    for cursor in range(index + 1, len(lines)):
        child = header(lines[cursor])
        if child is None:
            continue
        if scalar_end == len(lines):
            scalar_end = cursor
        if child[1] == root or not child[1].startswith(root + "."):
            end = cursor
            break
    if any(name_re.match(line.rstrip("\r\n")) for line in lines[index + 1:scalar_end]):
        start = index
        cursor = index - 1
        if cursor >= 0 and re.match(
            rf"^\s*#\s*External worker:\s*{re.escape(worker)}\b",
            lines[cursor],
            re.IGNORECASE,
        ):
            start = cursor
            if start > 0 and not lines[start - 1].strip():
                start -= 1
        ranges.append((start, end))
    index = end

if not ranges:
    raise SystemExit(3)

kept = []
cursor = 0
for start, end in ranges:
    kept.extend(lines[cursor:start])
    cursor = end
kept.extend(lines[cursor:])
result = "".join(kept)
parsed = tomllib.loads(result)
workers = parsed.get("sandbox", {}).get("external_workers", [])
if any(isinstance(item, dict) and item.get("name") == worker for item in workers):
    raise RuntimeError(f"failed to remove external worker {worker!r}")

def write_secure(path: Path, content: str, flags: int) -> None:
    fd = os.open(path, flags | os.O_NOFOLLOW, 0o600)
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise RuntimeError(f"unsafe retirement output: {path}")
        os.fchmod(fd, 0o600)
        os.fchown(fd, source_stat.st_uid, source_stat.st_gid)
        with os.fdopen(fd, "w", encoding="utf-8", closefd=False) as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
    finally:
        os.close(fd)

if backup.exists() or backup.is_symlink():
    backup_stat = os.lstat(backup)
    if not stat.S_ISREG(backup_stat.st_mode):
        raise RuntimeError(f"unsafe retirement backup: {backup}")
else:
    write_secure(backup, text, os.O_WRONLY | os.O_CREAT | os.O_EXCL)
write_secure(target, result, os.O_WRONLY | os.O_TRUNC)
os.replace(target, source)
PY
  if [[ "$rc" -eq 3 ]]; then
    rm -f "$temp_path"
    return 0
  fi
  if [[ "$rc" -ne 0 ]]; then
    rm -f "$temp_path"
    say "failed to remove external worker '$worker' from $config_path" >&2
    return 1
  fi
  say "removed retired '$worker' route from $config_path"
}
render_worker_quadlet() {
  local name="$1" worker="$2" health_port="${3:-8443}"
  case "$worker" in
    pebble|opencode|kimi) ;;
    *) die "unsupported external worker '$worker'" ;;
  esac
  local qdir wss_port workspace_dir env_dir tenant_env_path worker_env_path host_health_port
  local runtime_env_path="" runtime_env_hash="" health_path="health"
  qdir="$(tenant_quadlet_dir "$name")"
  wss_port="$(ports_get "$name" "${worker}_wss")"
  host_health_port="$(ports_get "$name" "${worker}_health")"
  [[ -n "$wss_port" ]] || return 0
  env_dir="$(tenant_env_dir "$name")"
  tenant_env_path="$env_dir/lunarwing.env"
  worker_env_path="$env_dir/${worker}.env"
  workspace_dir="$(tenant_lw_root "$name")/${worker}-workspace"
  mkdir -p "$workspace_dir"
  if [[ "$worker" == "kimi" || "$worker" == "opencode" ]]; then
    workspace_dir="$(prepare_worker_workspace "$name" "$worker")"
  else
    chown "$name:$name" "$workspace_dir"
  fi
  [[ "$worker" == "kimi" || "$worker" == "opencode" ]] || chmod 777 "$workspace_dir"
  [[ "$worker" != "kimi" ]] || runtime_env_path="$(write_kimi_runtime_env "$name")"
  [[ -z "$runtime_env_path" ]] \
    || runtime_env_hash="$(sha256sum "$runtime_env_path" | cut -d' ' -f1)"
  [[ "$worker" != "kimi" ]] || health_path="ready"
  mkdir -p "$qdir"

  local agent_token tz_key
  agent_token="$(_read_env_value "$tenant_env_path" GATEWAY_AUTH_TOKEN)"
  tz_key="$(_read_env_value "$tenant_env_path" LLM_API_KEY)"
  # systemd treats % as a unit specifier; escape so a token containing % survives.
  agent_token="${agent_token//%/%%}"
  tz_key="${tz_key//%/%%}"

  # SSH agent socket: the gateway creates an SSH agent server at startup,
  # with its socket at <tenant_home>/lunarwing/run/ssh-agent.sock (NOT /tmp,
  # because the daemon runs with PrivateTmp=true — a /tmp socket would be
  # invisible to podman containers). Always include the volume mount + env var
  # in the quadlet, even if the socket doesn't exist yet at render time — the
  # daemon creates it before the worker needs it (workers only use SSH when a
  # create_job with SSH commands is invoked, well after startup). If SSH is
  # not configured for the tenant, the socket simply won't exist and SSH
  # commands from the worker will fail with a clear "no agent" error.
  local ssh_agent_socket
  ssh_agent_socket="$(tenant_run_dir "$name")/ssh-agent.sock"
  local study_notes_home="" study_notes_state_home="" study_notes_allow_write=""
  if [[ "$worker" == "opencode" ]]; then
    study_notes_home="$(tenant_study_notes_home "$name")"
    [[ -z "$study_notes_home" ]] || validate_study_notes_home "$study_notes_home"
    if [[ -n "$study_notes_home" ]]; then
      study_notes_state_home="$(tenant_study_notes_state_home "$name")"
      study_notes_allow_write="$(tenant_study_notes_allow_write "$name")"
      if study_notes_write_enabled "$study_notes_allow_write"; then
        [[ -n "$study_notes_state_home" ]] \
          || die "Study Notes write mode requires LW2STUDYMCP_STATE_HOME"
      fi
      [[ -z "$study_notes_state_home" ]] \
        || validate_study_notes_state_home "$study_notes_state_home"
    fi
  fi

  {
    cat <<EOF
[Unit]
Description=LunarWing ${worker} worker ($name)
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Container]
ContainerName=lunarwing-${worker}-${name}
Image=lunarwing-worker-${worker}:latest
PublishPort=127.0.0.1:${wss_port}:${wss_port}
Volume=${workspace_dir}:/workspace:z
Environment=LUNARWING_WORKER_ID=worker-${worker}-${name}
Environment=WS_PORT=${wss_port}
Environment=HEALTH_PORT=${health_port}
Environment=WS_BIND_HOST=0.0.0.0
Environment=WS_PATH=/ws/agent
EOF
    if [[ -n "$study_notes_home" ]]; then
      printf 'Volume=%s:%s:ro,z\n' "$study_notes_home" "$STUDY_NOTES_CONTAINER_HOME"
      printf 'Environment=LW2STUDYMCP_HOME=%s\n' "$STUDY_NOTES_CONTAINER_HOME"
      if [[ -n "$study_notes_state_home" ]]; then
        printf 'Volume=%s:%s:ro,z\n' \
          "$study_notes_state_home" "$STUDY_NOTES_CONTAINER_STATE_HOME"
        printf 'Environment=LW2STUDYMCP_STATE_HOME=%s\n' "$STUDY_NOTES_CONTAINER_STATE_HOME"
      fi
    fi
    if [[ "$worker" == "opencode" ]]; then
      printf 'UserNS=%s\n' "$OPENCODE_USERNS"
    elif [[ "$worker" == "kimi" ]]; then
      printf 'UserNS=%s\n' "$KIMI_USERNS"
    fi
    # SSH agent socket mount + env (always included — the daemon creates the
    # socket at startup; if SSH isn't configured, the socket won't exist and
    # SSH commands from the worker will fail with a clear "no agent" error).
    # :z label so SELinux (Enforcing on Fedora) permits container_t access to
    # the tenant-home-labeled socket.
    printf 'Volume=%s:/tmp/ssh-agent.sock:z\n' "$ssh_agent_socket"
    printf 'Environment=SSH_AUTH_SOCK=/tmp/ssh-agent.sock\n'
    # Publish the per-tenant dedicated health port (v8) -> container's 8443, so
    # the host self-heal pipeline can probe the worker directly. Container still
    # listens on HEALTH_PORT=8443 internally (matches image healthcheck).
    [[ -n "$host_health_port" ]] && printf 'PublishPort=127.0.0.1:%s:8443\n' "$host_health_port"
    if [[ "$worker" == "pebble" ]]; then
      printf 'Environment=PEBBLE_MODE=websocket\n'
    elif [[ "$worker" == "opencode" ]]; then
      printf 'Environment=OPENCODE_MODE=websocket\n'
      printf 'Environment=WS_ROLE=server\n'
    elif [[ "$worker" == "kimi" ]]; then
      printf 'Environment=KIMI_MODE=websocket\n'
      printf 'Environment=WS_ROLE=server\n'
    fi
    [[ "$worker" == "kimi" || -z "$agent_token" ]] \
      || printf 'Environment=AGENT_AUTH_TOKEN=%s\n' "$agent_token"
    [[ "$worker" == "opencode" && -n "$tz_key" ]] && printf 'Environment=TENSORZERO_API_KEY=%s\n' "$tz_key"
    local oc_model oc_base_url
    oc_model="$(_read_env_value "$tenant_env_path" OPENCODE_MODEL)"
    oc_base_url="$(_read_env_value "$tenant_env_path" OPENCODE_BASE_URL)"
    [[ "$worker" == "opencode" && -n "$oc_model" ]] && printf 'Environment=OPENCODE_MODEL=%s\n' "$oc_model"
    [[ "$worker" == "opencode" && -n "$oc_base_url" ]] && printf 'Environment=OPENCODE_BASE_URL=%s\n' "$oc_base_url"
    if [[ -n "$runtime_env_path" ]]; then
      printf '# RuntimeEnvHash=%s\n' "$runtime_env_hash"
      printf 'EnvironmentFile=%s\n' "$runtime_env_path"
    fi
    # Operator override file (optional). EnvironmentFile= has existed since the
    # Quadlet 4.4 debut, so it is safe at our >= 4.6 floor.
    [[ -f "$worker_env_path" ]] && printf 'EnvironmentFile=%s\n' "$worker_env_path"
    [[ "$worker" == "opencode" || "$worker" == "kimi" ]] && printf 'Exec=--mode websocket\n'
    cat <<EOF
HealthCmd=curl -sf http://127.0.0.1:${health_port}/${health_path} || exit 1
HealthInterval=15s
HealthTimeout=5s
HealthRetries=3
HealthStartPeriod=30s

[Service]
Restart=on-failure
RestartSec=5
TimeoutStartSec=120

[Install]
WantedBy=default.target
EOF
  } >"$qdir/lunarwing-${worker}-${name}.container"
  chmod 0600 "$qdir/lunarwing-${worker}-${name}.container"
  chown -R "$name:$name" "$(tenant_home "$name")/.config/containers"
}
require_worker_name() {
  case "${1:-}" in
    nanocode) die "Nanocode worker is retired" ;;
    pebble|opencode|kimi) ;;
    *) die "unknown worker '${1:-}'; expected pebble, opencode, or kimi" ;;
  esac
}

prepare_worker_lifecycle() {
  local name worker
  name="$(sanitize_name "$1")"
  worker="$2"
  require_worker_name "$worker"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  printf '%s %s\n' "$name" "$worker"
}

configure_pebble() {
  local name="$1"
  local nanogpt_api_key="$2"
  local model="$3"

  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  local env_dir env_path
  env_dir="$(tenant_env_dir "$name")"
  env_path="$env_dir/pebble.env"

  mkdir -p "$env_dir"

  : >"$env_path"

  if [[ -n "$nanogpt_api_key" ]]; then
    printf 'NANOGPT_API_KEY=%s\n' "$nanogpt_api_key" >>"$env_path"
  fi

  if [[ -n "$model" ]]; then
    printf 'PEBBLE_MODEL=%s\n' "$model" >>"$env_path"
  fi

  chown "$name:$name" "$env_path"
  chmod 600 "$env_path"
  say "pebble configured for tenant '$name' at $env_path"

  local container_name="lunarwing-pebble-$name"
  if _ctr "$name" inspect "$container_name" &>/dev/null 2>&1; then
    say "note: restart the pebble worker to pick up new config:"
    say "  sudo $0 stop-tenant $name && sudo $0 start-tenant $name"
  fi
}

# ── OpenCode worker container ──────────────────────────────────────────────────

snapshot_sqlite_database() {  # <source> <destination>
  require_cmd python3
  python3 - "$1" "$2" <<'PY'
import sqlite3
import sys
from pathlib import Path

source_path = Path(sys.argv[1]).resolve(strict=True)
destination_path = Path(sys.argv[2])
source = sqlite3.connect(f"{source_path.as_uri()}?mode=ro", uri=True)
destination = sqlite3.connect(destination_path)
try:
    source.backup(destination)
    result = destination.execute("PRAGMA quick_check").fetchall()
    if result != [("ok",)]:
        raise RuntimeError(f"SQLite quick_check failed: {result}")
finally:
    destination.close()
    source.close()
PY
}

initialize_study_notes_state() {  # <tenant> <code home> <state home>
  local name="$1" home="$2" state_home="$3"
  validate_study_notes_path "$state_home" "Study Notes state home"

  local home_real state_real
  home_real="$(readlink -f -- "$home")"
  state_real="$(readlink -m -- "$state_home")"
  [[ "$state_real" != "$home_real" && "$state_real" != "$home_real/"* \
      && "$home_real" != "$state_real/"* ]] \
    || die "Study Notes code and state directories must not overlap"

  if [[ -e "$state_home" ]]; then
    [[ -d "$state_home" ]] || die "Study Notes state home is not a directory: $state_home"
    if [[ -e "$state_home/agent-notes.sqlite3" \
        || -e "$state_home/agent-notes-by-agent" ]]; then
      validate_study_notes_state_home "$state_home"
      return 0
    fi
    [[ -z "$(find "$state_home" -mindepth 1 -maxdepth 1 -print -quit)" ]] \
      || die "Study Notes state home is non-empty but uninitialized: $state_home"
  fi

  local state_parent staged
  state_parent="$(dirname -- "$state_home")"
  mkdir -p "$state_parent"
  staged="$(mktemp -d "$state_parent/.study-notes-state.XXXXXX")"
  if ! snapshot_sqlite_database \
      "$home/agent-notes.sqlite3" "$staged/agent-notes.sqlite3" \
      || ! chmod u+rw,go-rwx "$staged/agent-notes.sqlite3" \
      || ! cp -a "$home/agent-notes-by-agent" "$staged/agent-notes-by-agent" \
      || ! chown -R "$name:$name" "$staged" \
      || ! chmod u+rwx "$staged" \
      || ! find "$staged/agent-notes-by-agent" -type d -exec chmod u+rwx {} +; then
    rm -rf -- "$staged"
    die "failed to seed Study Notes state: $state_home"
  fi
  validate_study_notes_state_home "$staged"
  if [[ -d "$state_home" ]] && ! rmdir -- "$state_home"; then
    rm -rf -- "$staged"
    die "Study Notes state home changed while seeding: $state_home"
  fi
  if ! mv -T -- "$staged" "$state_home"; then
    rm -rf -- "$staged"
    die "failed to install seeded Study Notes state: $state_home"
  fi
}

validate_study_notes_tenant_access() {  # <tenant> <code home> [state home] <write bool>
  local name="$1" home="$2" state_home="$3" allow_write="$4"
  local required
  for required in \
    "$home/study_notes_mcp.py" \
    "$home/memorydb.py" \
    "$home/agent-notes.sqlite3" \
    "$home/skills/study-notes/SKILL.md"; do
    sudo -u "$name" test -r "$required" \
      || die "tenant '$name' cannot read Study Notes file: $required"
  done
  sudo -u "$name" test -x "$home/agent-notes-by-agent" \
    || die "tenant '$name' cannot traverse Study Notes directory: $home/agent-notes-by-agent"

  [[ -n "$state_home" ]] || return 0
  sudo -u "$name" test -r "$state_home/agent-notes.sqlite3" \
    || die "tenant '$name' cannot read Study Notes state database"
  sudo -u "$name" test -x "$state_home/agent-notes-by-agent" \
    || die "tenant '$name' cannot traverse Study Notes state notes"
  [[ "$allow_write" == "true" ]] || return 0
  sudo -u "$name" test -w "$state_home" \
    || die "tenant '$name' cannot write Study Notes state home: $state_home"
  local unwritable
  unwritable="$(sudo -u "$name" find "$state_home/agent-notes-by-agent" \
    -type d ! -writable -print -quit)"
  [[ -z "$unwritable" ]] \
    || die "tenant '$name' cannot write Study Notes state directory: $unwritable"
  if [[ -e "$state_home/agent-notes-by-agent/.study-notes.lock" ]]; then
    sudo -u "$name" test -w "$state_home/agent-notes-by-agent/.study-notes.lock" \
      || die "tenant '$name' cannot write Study Notes lock file"
  fi
}

configure_study_mcp() {  # <tenant> <code home> <allow-write bool> [state home]
  local name="$1" home="$2" allow_write_raw="$3" state_home="${4:-}"

  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  validate_study_notes_home "$home"

  local allow_write=false
  if study_notes_write_enabled "$allow_write_raw"; then
    allow_write=true
  fi
  if [[ -z "$state_home" ]]; then
    state_home="$(tenant_study_notes_state_home "$name")"
  fi
  if [[ "$allow_write" == "true" && -z "$state_home" ]]; then
    state_home="$(tenant_state_dir "$name")/study-notes"
  fi
  if [[ -n "$state_home" ]]; then
    initialize_study_notes_state "$name" "$home" "$state_home"
  fi
  validate_study_notes_tenant_access "$name" "$home" "$state_home" "$allow_write"

  # Rootless Podman cannot relabel a root-owned shared checkout itself.
  if command -v selinuxenabled >/dev/null 2>&1 && selinuxenabled; then
    require_cmd chcon
    chcon -R -t container_file_t -- "$home" \
      || die "failed to label Study Notes checkout for container access: $home"
    if [[ -n "$state_home" ]]; then
      chcon -R -t container_file_t -- "$state_home" \
        || die "failed to label Study Notes state for container access: $state_home"
    fi
  fi

  local env_path tmp
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path (run add-tenant first)"
  tmp="$(mktemp)"
  grep -vE '^(# Shared .*Study Notes MCP|LW2STUDYMCP_(HOME|STATE_HOME|ALLOW_WRITE)=)' \
    "$env_path" > "$tmp" || true
  printf '\n# Shared LunarWing Study Notes MCP\nLW2STUDYMCP_HOME=%s\n' \
    "$home" >> "$tmp"
  [[ -z "$state_home" ]] \
    || printf 'LW2STUDYMCP_STATE_HOME=%s\n' "$state_home" >> "$tmp"
  [[ "$allow_write" != "true" ]] \
    || printf 'LW2STUDYMCP_ALLOW_WRITE=1\n' >> "$tmp"
  cat "$tmp" > "$env_path"
  rm -f "$tmp"
  chown "$name:$name" "$env_path"
  chmod 600 "$env_path"

  say "Study Notes MCP configured for tenant '$name' from $home (write=$allow_write)"
  say "restart tenant to seed core MCP config and recreate opencode worker mount:"
  say "  sudo $0 restart-tenant $name"
}

# ── Kimi Code worker container ────────────────────────────────────────────────

prepare_worker_workspace() {
  local name="$1" worker="$2" workspace_dir container_uid container_gid
  workspace_dir="$(tenant_lw_root "$name")/${worker}-workspace"
  mkdir -p "$workspace_dir"

  case "$worker" in
    opencode)
      container_uid="$OPENCODE_CONTAINER_UID"
      container_gid="$OPENCODE_CONTAINER_GID"
      ;;
    kimi)
      container_uid="$KIMI_CONTAINER_UID"
      container_gid="$KIMI_CONTAINER_GID"
      ;;
    *) die "unsupported private worker workspace: $worker" ;;
  esac

  if [[ "$MT_ROOTLESS" != "true" ]]; then
    # Rootful runtimes do not apply keep-id, so bind mounts must use image IDs.
    chown -R "$container_uid:$container_gid" "$workspace_dir"
  else
    chown -R "$name:$name" "$workspace_dir"
  fi
  chmod 2770 "$workspace_dir"
  printf '%s\n' "$workspace_dir"
}

write_kimi_runtime_env() {
  local name="$1" source_path target_path temporary key value
  source_path="$(tenant_env_dir "$name")/lunarwing.env"
  target_path="$(tenant_env_dir "$name")/kimi-runtime.env"
  temporary="$(mktemp "${target_path}.tmp.XXXXXX")"
  chmod 600 "$temporary"

  for key in KIMI_MODEL KIMI_BASE_URL KIMI_API_KEY; do
    value="$(_read_env_value "$source_path" "$key")"
    [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] \
      || { rm -f "$temporary"; die "$key cannot contain a newline"; }
    [[ -z "$value" ]] || printf '%s=%s\n' "$key" "$value" >>"$temporary"
  done

  value="$(_read_env_value "$source_path" GATEWAY_AUTH_TOKEN)"
  [[ -n "$value" ]] \
    || { rm -f "$temporary"; die "Kimi worker requires GATEWAY_AUTH_TOKEN"; }
  # Worker bridge expects AGENT_AUTH_TOKEN, not daemon's gateway variable name.
  printf 'AGENT_AUTH_TOKEN=%s\n' "$value" >>"$temporary"
  chown "$name:$name" "$temporary"
  chmod 600 "$temporary"
  mv -f "$temporary" "$target_path"
  printf '%s\n' "$target_path"
}

configure_kimi() {
  local name="$1"
  local model="${2:-}"
  local base_url="${3:-}"
  local api_key="${4:-}"

  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  [[ -n "$model" || -n "$base_url" || -n "$api_key" ]] \
    || die "configure-kimi: pass --model <model>, --base-url <url>, or --api-key-file <path>"

  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path (run add-tenant first)"

  local tmp existing_model existing_base_url existing_api_key
  existing_model="$(_read_env_value "$env_path" KIMI_MODEL)"
  existing_base_url="$(_read_env_value "$env_path" KIMI_BASE_URL)"
  existing_api_key="$(_read_env_value "$env_path" KIMI_API_KEY)"
  [[ -n "$model" ]] || model="$existing_model"
  [[ -n "$base_url" ]] || base_url="$existing_base_url"
  [[ -n "$api_key" ]] || api_key="$existing_api_key"
  tmp="$(mktemp)"
  grep -v -e '^KIMI_MODEL=' -e '^KIMI_BASE_URL=' -e '^KIMI_API_KEY=' "$env_path" >"$tmp" || true
  [[ -n "$model" ]]    && printf 'KIMI_MODEL=%s\n'    "$model"    >>"$tmp"
  [[ -n "$base_url" ]] && printf 'KIMI_BASE_URL=%s\n' "$base_url" >>"$tmp"
  [[ -n "$api_key" ]] \
    && printf 'KIMI_API_KEY=%s\n' "$(_quote_env_value "$api_key")" >>"$tmp"
  cat "$tmp" >"$env_path"
  rm -f "$tmp"
  chown "$name:$name" "$env_path"
  chmod 600 "$env_path"
  say "kimi LLM overrides written to $env_path for tenant '$name'"

  local container_name="lunarwing-kimi-$name"
  if _ctr "$name" inspect "$container_name" &>/dev/null 2>&1; then
    say "note: restart the kimi worker to pick up the new config:"
    say "  sudo $0 stop-tenant $name && sudo $0 start-tenant $name"
  fi
}

start_tenant_kimi() {
  local name="$1"
  ensure_container_runtime

  local kimi_launch_contract=""
  local -a kimi_userns_args=()
  if [[ "$CONTAINER_RT" == "podman" && "$MT_ROOTLESS" == "true" ]]; then
    kimi_launch_contract="$KIMI_ROOTLESS_LAUNCH_CONTRACT"
    kimi_userns_args=(--userns "$KIMI_USERNS")
  fi

  local wss_port container_name kimi_dir
  wss_port="$(ports_get "$name" kimi_wss)" || true
  container_name="lunarwing-kimi-$name"
  kimi_dir="${LUNARWING_ROOT}/kimicode4lunarwing"

  if [[ -z "$wss_port" ]]; then
    say "no kimi_wss port allocated for $name (skipping kimi worker)"
    return 0
  fi

  if ! _ensure_tenant_image "$name" lunarwing-worker-kimi:latest; then
    say "kimi worker image not available; run 'build-kimi-worker' first (skipping)"
    return 0
  fi
  local image_user
  image_user="$(_ctr "$name" image inspect --format '{{.Config.User}}' \
    lunarwing-worker-kimi:latest 2>/dev/null || true)"
  case "$image_user" in
    "$KIMI_CONTAINER_UID"|"$KIMI_CONTAINER_UID:$KIMI_CONTAINER_GID") ;;
    *)
      die "kimi worker image has user '${image_user:-unset}', expected $KIMI_CONTAINER_UID:$KIMI_CONTAINER_GID; rebuild it"
      ;;
  esac

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local kimi_service="lunarwing-kimi-${name}.service"
    local quadlet_file
    quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-kimi-${name}.container"
    render_worker_quadlet "$name" kimi 8443
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if _container_config_changed "$name" "lunarwing-kimi-${name}" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "$kimi_service" "lunarwing-kimi-${name}"
    fi
    _systemctl_user "$name" reset-failed "$kimi_service" >/dev/null 2>&1 || true
    if _systemctl_user "$name" start "$kimi_service" >/dev/null 2>&1; then
      _store_container_hash "$name" "lunarwing-kimi-${name}" "$quadlet_file"
      say "kimi worker ready via quadlet ($kimi_service, WSS port $wss_port)"
    else
      say "WARNING: $kimi_service failed to start" >&2
      _systemctl_user "$name" status "$kimi_service" --no-pager --full >&2 || true
    fi
    return 0
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    local env_file_for_hash
    env_file_for_hash="$(tenant_env_dir "$name")/lunarwing.env"
    if _container_config_changed \
        "$name" "$container_name" "$env_file_for_hash" "$kimi_launch_contract"; then
      say "config changed for $container_name; force-recreating"
      _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
    elif _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "kimi worker already running ($container_name, WSS port $wss_port)"
      _store_container_hash \
        "$name" "$container_name" "$env_file_for_hash" "$kimi_launch_contract"
      _register_worker_unit "$name" kimi
      return 0
    else
      say "starting existing kimi worker container $container_name"
      _ctr "$name" start "$container_name" >/dev/null
      _store_container_hash \
        "$name" "$container_name" "$env_file_for_hash" "$kimi_launch_contract"
      _register_worker_unit "$name" kimi
      say "kimi worker ready ($container_name, WSS port $wss_port)"
      return 0
    fi
  fi

  if ! _ctr "$name" inspect "$container_name" &>/dev/null; then
    say "creating kimi worker container $container_name on WSS port $wss_port"

    local kimi_env_path
    kimi_env_path="$(tenant_env_dir "$name")/kimi.env"

    local runtime_env_path
    runtime_env_path="$(write_kimi_runtime_env "$name")"
    local env_flags=(--env-file "$runtime_env_path")

    if [[ -f "$kimi_env_path" ]]; then
      env_flags+=(--env-file "$kimi_env_path")
    fi

    local workspace_dir
    workspace_dir="$(prepare_worker_workspace "$name" kimi)"

    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    local -a health_publish=()
    local host_health_port
    host_health_port="$(ports_get "$name" kimi_health)" || true
    [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:8443")
    local ssh_agent_socket
    ssh_agent_socket="$(tenant_run_dir "$name")/ssh-agent.sock"
    local -a ssh_mount=(-v "${ssh_agent_socket}:/tmp/ssh-agent.sock:z" -e SSH_AUTH_SOCK=/tmp/ssh-agent.sock)
    _ctr "$name" run -d \
      --name "$container_name" \
      "${kimi_userns_args[@]}" \
      -e LUNARWING_WORKER_ID="worker-kimi-${name}" \
      -e WS_PORT="$wss_port" \
      -e HEALTH_PORT="8443" \
      -e KIMI_MODE=websocket \
      -e WS_ROLE=server \
      -e WS_BIND_HOST=0.0.0.0 \
      -e WS_PATH=/ws/agent \
      "${env_flags[@]}" \
      "${ssh_mount[@]}" \
      -p "127.0.0.1:${wss_port}:${wss_port}" \
      "${health_publish[@]}" \
      -v "$workspace_dir:/workspace:z" \
      "${restart_arg[@]}" \
      lunarwing-worker-kimi:latest \
      --mode websocket >/dev/null
    _store_container_hash \
      "$name" "$container_name" "$(tenant_env_dir "$name")/lunarwing.env" \
      "$kimi_launch_contract"
  fi

  _register_worker_unit "$name" kimi
  say "kimi worker ready ($container_name, WSS port $wss_port)"
}

stop_tenant_kimi() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-kimi-$name"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container_name}" ]]; then
    _deregister_babysitter "$container_name"
    rc-service "$container_name" stop >/dev/null 2>&1 || true
    say "kimi worker stopped ($container_name)"
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "kimi worker stopped ($container_name)"
  fi
}

restart_worker() {
  stop_worker "$1" "$2"
  start_worker "$1" "$2"
}

# Cargo builds can refresh tracked lockfiles in nested standalone crates. Restore
# them only when they are the sole tracked changes; otherwise preserve everything
# and fail. Untracked tenant runtime directories do not block checkout.

openrc_worker_stop_disable() {
  local name="$1" worker="$2" container svc
  container="lunarwing-${worker}-${name}"
  for svc in "${container}-sup" "$container"; do
    if [[ -x "$OPENRC_INITD_DIR/$svc" ]]; then
      rc-service "$svc" stop >/dev/null 2>&1 \
        || { say "failed to stop OpenRC service $svc" >&2; return 1; }
    fi
    openrc_service_remove_runlevels "$svc" || return 1
  done
}

start_worker() {
  local prepared name worker
  prepared="$(prepare_worker_lifecycle "$1" "$2")" || return
  read -r name worker <<<"$prepared"
  tenant_worker_enabled "$name" "$worker" \
    || die "worker '$worker' is not enabled for tenant '$name'"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" ]]; then
    ports_set_worker_desired_running "$name" "$worker" true
  fi
  "start_tenant_${worker}" "$name"
}

stop_worker() {
  local prepared name worker container service
  prepared="$(prepare_worker_lifecycle "$1" "$2")" || return
  read -r name worker <<<"$prepared"
  container="lunarwing-${worker}-${name}"
  service="${container}.service"
  ensure_container_runtime
  ensure_init_system

  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] \
      && podman_supports_quadlet; then
    _systemctl_user "$name" stop "$service" >/dev/null 2>&1 \
      || _ctr "$name" stop "$container" >/dev/null 2>&1 \
      || die "failed to stop $service"
  elif [[ "$INIT_SYSTEM" == "openrc" ]]; then
    ports_set_worker_desired_running "$name" "$worker" false
    openrc_worker_stop_disable "$name" "$worker" \
      || die "failed to stop and disable OpenRC worker $worker for tenant '$name'"
    if [[ ! -x "$OPENRC_INITD_DIR/$container" ]] \
        && _ctr "$name" inspect "$container" &>/dev/null; then
      _ctr "$name" stop "$container" >/dev/null 2>&1 \
        || die "failed to stop container $container"
    fi
  elif _ctr "$name" inspect "$container" &>/dev/null; then
    _ctr "$name" stop "$container" >/dev/null 2>&1 \
      || die "failed to stop container $container"
  fi
  say "$worker worker stopped for $name"
}

kill_worker() {
  local prepared name worker container service
  prepared="$(prepare_worker_lifecycle "$1" "$2")" || return
  read -r name worker <<<"$prepared"
  container="lunarwing-${worker}-${name}"
  service="${container}.service"
  ensure_container_runtime
  ensure_init_system

  # Stop supervision before the zero-grace container stop. Otherwise
  # Restart=on-failure/supervise-daemon can resurrect the worker immediately.
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] \
      && podman_supports_quadlet; then
    _systemctl_user "$name" stop --no-block "$service" >/dev/null 2>&1 || true
  elif [[ "$INIT_SYSTEM" == "openrc" ]]; then
    ports_set_worker_desired_running "$name" "$worker" false
    rc-service "${container}-sup" stop >/dev/null 2>&1 || true
    openrc_service_remove_runlevels "${container}-sup" \
      || die "failed to disable OpenRC supervisor ${container}-sup"
    openrc_service_remove_runlevels "$container" \
      || die "failed to disable OpenRC service $container"
  fi

  if _ctr "$name" inspect "$container" &>/dev/null; then
    _ctr "$name" stop --time 0 "$container" >/dev/null 2>&1 \
      || die "failed to kill container $container"
  fi

  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] \
      && podman_supports_quadlet; then
    _systemctl_user "$name" stop "$service" >/dev/null 2>&1 || true
  elif [[ "$INIT_SYSTEM" == "openrc" && -x "$OPENRC_INITD_DIR/${container}" ]]; then
    rc-service "$container" stop >/dev/null 2>&1 || true
  fi
  say "$worker worker killed and left stopped for $name"
}
