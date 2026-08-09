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
