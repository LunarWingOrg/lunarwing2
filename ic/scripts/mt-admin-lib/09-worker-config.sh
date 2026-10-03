#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 09-worker-config.sh — part of the lunarwing-mt-admin library.
#
# External worker configuration and lifecycle (opencode/kimi/pebble, retired
# nanocode), worker unit registration, workspace prep, and the study-notes MCP
# wiring.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced.
#
# Extracted from the v2.0.8.0 monolith (goals item #9), keeping the library
# structure of ref2/sunburst/mtadmin-refactor-v2.0.6.0 per
# docs/proposals/MT-ADMIN-DECOMPOSITION.md. Pure line-range move: no function
# body was rewritten.


# Render a dedicated OpenRC unit for a tenant's supported external worker,
# modeled on the lunarwing-pg-<t> unit. Health-aware status() checks the worker's
# /health endpoint (curl is present in both worker images) so the host self-heal
# pipeline — which auto-discovers /etc/init.d/lunarwing-* units — can detect and
# remediate a crashed OR hung worker, and so it survives reboot. OpenRC only.
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

# ── External-worker config.toml generation ────────────────────────────────────
#
# External workers speak the lunarwing-agent-v1 WebSocket
# protocol and are routed by the agent's `create_job(mode: "<worker>")` tool.
# The daemon discovers them from `[[sandbox.external_workers]]` blocks in
# `config.toml` under the tenant's LUNARWING_BASE_DIR (the state dir). Without
# this block the agent has nothing to route `create_job(mode: "<worker>")` to
# and never logs `External workers configured: <worker>` on startup.
#
# The worker container binds 127.0.0.1:<wss_port> (path /ws/agent) and is
# launched with AGENT_AUTH_TOKEN set to the tenant's WORKER_AUTH_TOKEN, so the
# daemon side must present that same token as the WebSocket bearer — hence
# auth_token mirrors WORKER_AUTH_TOKEN here. This credential is scoped to the
# worker handshake only; it is never accepted by the gateway API, so a leaked
# `podman inspect` value no longer yields the tenant's dashboard surface
# (WI#205).
#
# Idempotent: a tenant entry is written once and skipped on re-runs. Exact legacy
# 5-minute deadlines migrate to the current default. Set
# LUNARWING_MT_MIGRATE_LEGACY_EXTERNAL_WORKER_TIMEOUTS=false to preserve an
# intentional 5-minute value. Other explicit values remain untouched.

# Scoped bearer for the daemon<->external-worker WebSocket handshake (WI#205).
#
# The worker is the WS *server*: it validates an inbound bearer on the upgrade
# and the daemon presents the same value as `auth_token`. Both sides MUST
# derive the value through this helper — a mismatch shows up only as a failed
# handshake ("worker connection refused"), never as a config error.
#
# Falls back to GATEWAY_AUTH_TOKEN for tenants provisioned before
# WORKER_AUTH_TOKEN existed; those converge on the next lifecycle pass, which
# regenerates the env file, rewrites the worker units, and upgrades the
# config.toml entry together.
#
# WI#210: the fallback mints the scoped token on first use instead of leaving
# the gateway bearer in place. `ensure_worker_auth_token` writes
# WORKER_AUTH_TOKEN into the tenant env file (0600, same file the daemon
# reads), so an upgrade-path tenant gets a real scoped credential immediately
# and never exposes the full gateway bearer to `podman inspect` or the
# tenant-readable unit file. The gateway-bearer substitution is kept ONLY as a
# last resort for read-only/unwritable env files, and is always loudly warned.
resolve_worker_auth_token() {
  local env_path="$1" token
  token="$(_read_env_value "$env_path" WORKER_AUTH_TOKEN)"
  if [[ -n "$token" ]]; then
    printf '%s' "$token"
    return 0
  fi

  # Mint and persist the scoped token. Preserve the tenant's ownership when
  # replacing the file: mt-admin runs as root, and a root-owned 0600 replacement
  # cannot be read by the tenant's systemd user services.
  if [[ -w "$env_path" ]]; then
    local fresh
    fresh="$(generate_token)"
    if [[ -n "$fresh" ]]; then
      if _env_set "$env_path" WORKER_AUTH_TOKEN "$fresh"; then
        echo "WARNING: WORKER_AUTH_TOKEN was missing in $env_path; generated and persisted a scoped worker credential (WI#210)." >&2
        printf '%s' "$fresh"
        return 0
      fi
    fi
  fi

  # Last resort: fail open ONLY after an explicit, loud warning. Never silent.
  echo "WARNING: could not persist WORKER_AUTH_TOKEN for $env_path; falling back to GATEWAY_AUTH_TOKEN (WI#210 mitigations do NOT apply to this worker until the env file is written)." >&2
  token="$(_read_env_value "$env_path" GATEWAY_AUTH_TOKEN)"
  printf '%s' "$token"
}

# Rewrite one worker's `auth_token` when it drifts from the scoped credential.
#
# Return contract mirrors upgrade_external_worker_timeout: 0 upgraded,
# 1 no change needed, 2 failure.
upgrade_external_worker_auth_token() {
  local name="$1" worker="$2" config_path="$3" env_path="$4"
  local awk_status temp_path new_token
  new_token="$(resolve_worker_auth_token "$env_path")"
  [[ -n "$new_token" ]] || return 1

  if ! temp_path="$(mktemp "${config_path}.tmp.XXXXXX")"; then
    return 2
  fi

  if awk \
    -v worker="$worker" \
    -v new_token="$new_token" '
      # Two-pass shape: first pass records whether the target block already has
      # a matching token (nothing to do) and which lines to rewrite; second pass
      # emits. Doing it in one pass risks inserting a duplicate ahead of the
      # existing key, which TOML rejects as a repeated key.
      FNR == NR {
        if ($0 ~ /^\[\[sandbox\.external_workers\]\][[:space:]]*$/) {
          in_block = 1
          target = 0
          next
        }
        if ($0 ~ /^\[/) {
          in_block = 0
          target = 0
          next
        }
        if (in_block && $0 == "name = \"" worker "\"") {
          target = 1
          next
        }
        if (!(in_block && target)) next
        if ($0 == "auth_token = \"" new_token "\"") { satisfied = 1 }
        if ($0 ~ /^auth_token[[:space:]]*=/) { has_token = 1 }
        next
      }
      {
        if ($0 ~ /^\[\[sandbox\.external_workers\]\][[:space:]]*$/) {
          in_block = 1
          target = 0
          print
          next
        }
        if ($0 ~ /^\[/) {
          in_block = 0
          target = 0
          print
          next
        }
        if (in_block && $0 == "name = \"" worker "\"") {
          target = 1
          print
          next
        }
        if (in_block && target) {
          if ($0 ~ /^auth_token[[:space:]]*=/) {
            if (!satisfied) { print "auth_token = \"" new_token "\""; changed = 1 }
            else { print }
            next
          }
          if (!has_token && $0 ~ /^url[[:space:]]*=/) {
            print
            print "auth_token = \"" new_token "\""
            changed = 1
            next
          }
        }
        print
      }
      END {
        if (satisfied) exit 42
        if (!changed && !has_token) exit 43
      }
    ' "$config_path" "$config_path" >"$temp_path"; then
    if ! chown "$name:$name" "$temp_path" ||
       ! chmod 600 "$temp_path" ||
       ! mv -f -- "$temp_path" "$config_path"; then
      rm -f -- "$temp_path"
      return 2
    fi
    say "scoped worker credential applied for '$worker' (WI#205)"
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

  local state_dir env_path config_path wss_port auth_token default_timeout_ms

  if [[ "$worker" == "kimi" ]] && ! tenant_worker_enabled "$name" kimi; then
    say "kimi worker not selected for $name (skipping external-worker config)"
    return 0
  fi
  if [[ "$worker" == "opencode2" ]] && ! tenant_worker_enabled "$name" opencode2; then
    say "opencode2 worker not selected for $name (skipping external-worker config)"
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
  default_timeout_ms="$DEFAULT_EXTERNAL_WORKER_TIMEOUT_MS"
  # Keep the other worker lanes at their existing default. An explicitly set
  # fleet-wide timeout still takes precedence for newly generated entries.
  if [[ "$worker" == "kimi" && -z "${LUNARWING_MT_EXTERNAL_WORKER_TIMEOUT_MS:-}" ]]; then
    default_timeout_ms=14400000
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
    # Migrate a pre-WI#205 entry (gateway bearer) onto the scoped credential.
    # Runs on the skip path too, so an existing tenant converges without
    # re-provisioning; the worker units pick up the same value on their next
    # render because both read it through resolve_worker_auth_token().
    local auth_status=0
    upgrade_external_worker_auth_token "$name" "$worker" "$config_path" "$env_path" \
      || auth_status=$?
    case "$auth_status" in
      0 | 1) ;;
      *) die "failed to apply the scoped worker credential for '$worker' in $config_path" ;;
    esac
    return 0
  fi

  # Scoped worker credential (WI#205). Derived through the shared resolver so
  # the value can never drift from what the worker container's
  # AGENT_AUTH_TOKEN will be; tenants provisioned before WORKER_AUTH_TOKEN
  # existed fall back to the gateway bearer until their next lifecycle pass
  # migrates both sides together.
  auth_token="$(resolve_worker_auth_token "$env_path")"

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
    printf 'timeout_ms = %s\n' "$default_timeout_ms"
  } >>"$config_path"

  if [[ -z "$auth_token" ]]; then
    say "WARNING: GATEWAY_AUTH_TOKEN not found in $env_path;" \
        "'$worker' worker config written WITHOUT auth_token (connections will fail until set)"
  fi

  chown "$name:$name" "$config_path"
  chmod 600 "$config_path"
  say "wrote $worker external-worker config to $config_path (ws://127.0.0.1:$wss_port/ws/agent)"
}

# Remove one external-worker array entry plus any nested child tables. Preserve
# unrelated TOML text/comments, validate before and after, and retain one secure
# rollback copy. Returns success when no Nanocode entry exists.
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

_nanocode_openrc_service_known() {
  local svc="$1" link
  [[ -e "$OPENRC_INITD_DIR/$svc" || -e "$OPENRC_CONFD_DIR/$svc" ]] && return 0
  for link in "$OPENRC_RUNLEVELS_DIR"/*/"$svc"; do
    [[ -e "$link" || -L "$link" ]] && return 0
  done
  return 1
}

_stop_nanocode_supervision() {
  local name="$1"
  local unit="lunarwing-nanocode-${name}.service"
  local container="lunarwing-nanocode-${name}" load_state="" user_unit_dir
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    user_unit_dir="$(tenant_home "$name")/.config/systemd/user"
    if id -u "$name" >/dev/null 2>&1; then
      load_state="$(_systemctl_user "$name" show --value -p LoadState "$unit" 2>/dev/null || true)"
      if [[ -e "$(tenant_quadlet_dir "$name")/${container}.container" \
          || -e "$user_unit_dir/$unit" || "$load_state" == "loaded" ]]; then
        _systemctl_user "$name" stop "$unit" >/dev/null 2>&1 \
          || { say "failed to stop legacy $unit" >&2; return 1; }
        if _systemctl_user "$name" is-active --quiet "$unit" 2>/dev/null; then
          say "legacy $unit remained active after stop" >&2
          return 1
        fi
      fi
    elif [[ -e "$(tenant_quadlet_dir "$name")/${container}.container" \
        || -e "$user_unit_dir/$unit" ]]; then
      say "tenant user '$name' absent; cannot stop legacy $unit" >&2
      return 1
    fi
    return 0
  fi

  local svc
  for svc in "${container}-sup" "$container"; do
    _nanocode_openrc_service_known "$svc" || continue
    rc-service "$svc" stop >/dev/null 2>&1 \
      || { say "failed to stop legacy OpenRC service $svc" >&2; return 1; }
  done
}

_stop_nanocode_container_store() {
  local label="$1" container="$2"
  shift 2
  local -a runtime=("$@")
  "${runtime[@]}" inspect "$container" >/dev/null 2>&1 || return 0
  if ! "${runtime[@]}" stop --time 30 "$container" >/dev/null 2>&1; then
    say "failed to stop legacy $container in $label" >&2
    return 1
  fi
  if [[ "$("${runtime[@]}" inspect -f '{{.State.Running}}' "$container" 2>/dev/null || true)" == "true" ]]; then
    say "legacy $container still running in $label" >&2
    return 1
  fi
  say "stopped legacy $container in $label"
}

_stop_nanocode_containers() {
  local name="$1"
  local container="lunarwing-nanocode-${name}" uid home
  if command -v docker >/dev/null 2>&1; then
    _stop_nanocode_container_store "root Docker store" "$container" docker || return 1
  fi
  if command -v podman >/dev/null 2>&1; then
    _stop_nanocode_container_store "root Podman store" "$container" podman || return 1
    if id -u "$name" >/dev/null 2>&1; then
      uid="$(id -u "$name")"
      home="$(getent passwd "$name" | cut -d: -f6)"
      _stop_nanocode_container_store "tenant $name rootless Podman store" "$container" \
        sudo -u "$name" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" podman || return 1
    fi
  fi
}

_remove_nanocode_supervision() {
  local name="$1"
  local container="lunarwing-nanocode-${name}" svc link runlevel
  rm -f -- \
    "$(tenant_quadlet_dir "$name")/${container}.container" \
    "$(tenant_home "$name")/.config/systemd/user/${container}.service" \
    "$(tenant_home "$name")/.config/systemd/user/default.target.wants/${container}.service" \
    "$(_hash_dir "$name")/${container}.hash"
  if [[ "$INIT_SYSTEM" == "systemd" ]] && id -u "$name" >/dev/null 2>&1; then
    _systemctl_user "$name" daemon-reload >/dev/null 2>&1 \
      || { say "WARNING: could not reload tenant user manager after Nanocode retirement (non-fatal)" >&2; true; }
  fi

  for svc in "${container}-sup" "$container"; do
    for link in "$OPENRC_RUNLEVELS_DIR"/*/"$svc"; do
      [[ -e "$link" || -L "$link" ]] || continue
      runlevel="$(basename "$(dirname "$link")")"
      if command -v rc-update >/dev/null 2>&1; then
        rc-update del "$svc" "$runlevel" >/dev/null 2>&1 \
          || { say "failed to remove $svc from OpenRC runlevel $runlevel" >&2; return 1; }
      else
        rm -f -- "$link"
      fi
    done
    rm -f -- "$OPENRC_INITD_DIR/$svc" "$OPENRC_CONFD_DIR/$svc"
  done
}

_archive_nanocode_env() {
  local name="$1" env_dir env_path legacy archive backup temp rc=0
  env_dir="$(tenant_env_dir "$name")"
  env_path="$env_dir/lunarwing.env"
  legacy="$env_dir/nanocode.env"
  require_cmd python3
  if [[ -e "$env_path" || -L "$env_path" ]]; then
    backup="$env_path.pre-nanocode-retirement"
    temp="$(mktemp "${env_path}.tmp.XXXXXX")"
    python3 - "$env_path" "$temp" "$backup" <<'PY' || rc=$?
import os
import stat
import sys
from pathlib import Path

source, target, backup = map(Path, sys.argv[1:4])
source_fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
source_stat = os.fstat(source_fd)
if not stat.S_ISREG(source_stat.st_mode):
    os.close(source_fd)
    raise RuntimeError("tenant env is not a regular file")
with os.fdopen(source_fd, encoding="utf-8") as handle:
    text = handle.read()
lines = text.splitlines(keepends=True)
if not any(line.startswith("NANOCODE_") for line in lines):
    raise SystemExit(3)
result = "".join(line for line in lines if not line.startswith("NANOCODE_"))

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
    if not stat.S_ISREG(os.lstat(backup).st_mode):
        raise RuntimeError(f"unsafe retirement backup: {backup}")
else:
    write_secure(backup, text, os.O_WRONLY | os.O_CREAT | os.O_EXCL)
write_secure(target, result, os.O_WRONLY | os.O_TRUNC)
os.replace(target, source)
PY
    if [[ "$rc" -eq 3 ]]; then
      rm -f -- "$temp"
    elif [[ "$rc" -ne 0 ]]; then
      rm -f -- "$temp"
      say "failed to remove retired NANOCODE_* settings from $env_path" >&2
      return 1
    else
      say "removed retired NANOCODE_* settings from $env_path"
    fi
  fi
  if [[ -e "$legacy" || -L "$legacy" ]]; then
    archive="$legacy.retired"
    [[ ! -e "$archive" ]] || archive="$archive.$(date -u +%Y%m%dT%H%M%SZ).$$"
    python3 - "$legacy" "$archive" <<'PY' || return 1
import os
import stat
import sys
from pathlib import Path

source, archive = map(Path, sys.argv[1:3])
fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
try:
    source_stat = os.fstat(fd)
    if not stat.S_ISREG(source_stat.st_mode):
        raise RuntimeError("legacy env is not a regular file")
    os.fchmod(fd, 0o600)
    current = os.lstat(source)
    if (current.st_dev, current.st_ino) != (source_stat.st_dev, source_stat.st_ino):
        raise RuntimeError("legacy env changed during retirement")
    os.link(source, archive, follow_symlinks=False)
    os.unlink(source)
finally:
    os.close(fd)
PY
    say "archived legacy Nanocode env as $archive"
  fi
}

_retire_tenant_nanocode() {
  local name="$1"
  _stop_nanocode_supervision "$name" || return 1
  _stop_nanocode_containers "$name" || return 1
  _remove_nanocode_supervision "$name" || return 1
  remove_external_worker_config "$name" nanocode || return 1
  _archive_nanocode_env "$name" || return 1
  ports_retire_nanocode "$name" || return 1
  say "Nanocode retired for tenant '$name' (data, stopped containers, and images preserved)"
}

retire_tenant_nanocode() {
  local name="$1"
  with_tenant_lifecycle_lock "$name" _retire_tenant_nanocode "$name"
}

# ── Pebble worker configuration ──────────────────────────────────────────────

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

configure_opencode() {
  local name="$1"
  local model="${2:-}"
  local base_url="${3:-}"

  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  [[ -n "$model" || -n "$base_url" ]] \
    || die "configure-opencode: pass --model <model> and/or --base-url <url>"

  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path (run add-tenant first)"

  local tmp
  tmp="$(mktemp)"
  grep -v -e '^OPENCODE_MODEL=' -e '^OPENCODE_BASE_URL=' "$env_path" >"$tmp" || true
  [[ -n "$model" ]]    && printf 'OPENCODE_MODEL=%s\n'    "$model"    >>"$tmp"
  [[ -n "$base_url" ]] && printf 'OPENCODE_BASE_URL=%s\n' "$base_url" >>"$tmp"
  cat "$tmp" >"$env_path"
  rm -f "$tmp"
  chown "$name:$name" "$env_path"
  chmod 600 "$env_path"
  say "opencode LLM overrides written to $env_path for tenant '$name'"

  local container_name="lunarwing-opencode-$name"
  if _ctr "$name" inspect "$container_name" &>/dev/null 2>&1; then
    say "note: restart the opencode worker to pick up the new config:"
    say "  sudo $0 stop-tenant $name && sudo $0 start-tenant $name"
  fi
}

configure_opencode2() {
  local name="$1"
  local model="${2:-}"
  local base_url="${3:-}"

  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  [[ -n "$model" || -n "$base_url" ]] \
    || die "configure-opencode2: pass --model <model> and/or --base-url <url>"

  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path (run add-tenant first)"

  local tmp existing_model existing_base_url
  existing_model="$(_read_env_value "$env_path" OPENCODE2_MODEL)"
  existing_base_url="$(_read_env_value "$env_path" OPENCODE2_BASE_URL)"
  [[ -n "$model" ]] || model="$existing_model"
  [[ -n "$base_url" ]] || base_url="$existing_base_url"
  tmp="$(mktemp)"
  grep -v -e '^OPENCODE2_MODEL=' -e '^OPENCODE2_BASE_URL=' "$env_path" >"$tmp" || true
  [[ -n "$model" ]]    && printf 'OPENCODE2_MODEL=%s\n'    "$model"    >>"$tmp"
  [[ -n "$base_url" ]] && printf 'OPENCODE2_BASE_URL=%s\n' "$base_url" >>"$tmp"
  cat "$tmp" >"$env_path"
  rm -f "$tmp"
  chown "$name:$name" "$env_path"
  chmod 600 "$env_path"
  say "opencode2 LLM overrides written to $env_path for tenant '$name'"

  local container_name="lunarwing-opencode2-$name"
  if _ctr "$name" inspect "$container_name" &>/dev/null 2>&1; then
    say "note: restart the opencode2 worker to pick up the new config:"
    say "  sudo $0 stop-tenant $name && sudo $0 start-tenant $name"
  fi
}

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

# Arm the one-shot knowledge seed for a NEWLY created tenant: the daemon
# imports the synced lw2studymcp checkout into the tenant's private knowledge
# store on first boot, then never runs again (state-file marker, existing
# notes skipped). Written only here — the add-tenant path — so existing
# tenants upgraded to a seed-capable binary are never populated, and a later
# configure-study-mcp re-run never (re)arms the seed.
arm_tenant_knowledge_seed() {  # <tenant>
  local name="$1" env_path
  name="$(sanitize_name "$name")"
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path"
  grep -qE '^KNOWLEDGE_SEED_FROM_STUDY_MCP=' "$env_path" && return 0
  printf '\n# One-shot knowledge seed from the synced Study Notes checkout (new tenants only)\nKNOWLEDGE_SEED_FROM_STUDY_MCP=1\n' >> "$env_path"
  chown "$name:$name" "$env_path"
  chmod 600 "$env_path"
}

set_tenant_knowledge_sync() {  # <tenant> <true|false>
  local name="$1" enabled="$2" env_path
  name="$(sanitize_name "$name")"
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path"
  [[ -n "$(_env_existing "$env_path" LW2STUDYMCP_HOME)" ]] \
    || die "configure Study Notes MCP before enabling knowledge sync for '$name'"
  case "$enabled" in
    true|false) : ;;
    *) die "knowledge sync state must be true or false" ;;
  esac
  if [[ "$enabled" == "true" ]]; then
    # Install the root-owned source scheduler before committing the tenant
    # opt-in, so a failed scheduler install cannot leave a silently static source.
    ensure_knowledge_source_scheduler
    [[ -n "$(_env_existing "$env_path" KNOWLEDGE_SYNC_CADENCE_HOURS)" ]] \
      || _env_set "$env_path" KNOWLEDGE_SYNC_CADENCE_HOURS 6
    [[ -n "$(_env_existing "$env_path" KNOWLEDGE_SYNC_MAX_RECORDS_PER_RUN)" ]] \
      || _env_set "$env_path" KNOWLEDGE_SYNC_MAX_RECORDS_PER_RUN 500
    [[ -n "$(_env_existing "$env_path" KNOWLEDGE_SYNC_MAX_BYTES_PER_RUN)" ]] \
      || _env_set "$env_path" KNOWLEDGE_SYNC_MAX_BYTES_PER_RUN 10485760
  fi
  _env_set "$env_path" KNOWLEDGE_SYNC_ENABLED "$enabled"
  say "additive knowledge sync for tenant '$name': $enabled"
}

provision_tenant_study_mcp() {  # <tenant> [checkout home]
  # Auto-configuration used by add-tenant --with-study-mcp: always sync the
  # shared checkout to the latest upstream first, then wire it in read-only
  # (core writes stay opt-in via configure-study-mcp --allow-write).
  # Runs in a subshell so a die() inside sync/configure is contained and the
  # caller can treat auto-configuration failure as non-fatal for the tenant.
  local name="$1" home="${2:-}"
  [[ -n "$home" ]] || home="$STUDY_NOTES_DEFAULT_HOME"
  (
    sync_study_notes_checkout "$home"
    configure_study_mcp "$name" "$home" false ""
    arm_tenant_knowledge_seed "$name"
    set_tenant_knowledge_sync "$name" true
  )
}

start_tenant_opencode() {
  local name="$1"
  ensure_container_runtime

  # Record the mount policy even for rootful/OpenRC imperative containers.
  local opencode_launch_contract="ssh-agent-dir-ro"
  local -a opencode_userns_args=()
  if [[ "$CONTAINER_RT" == "podman" && "$MT_ROOTLESS" == "true" ]]; then
    opencode_launch_contract="$OPENCODE_ROOTLESS_LAUNCH_CONTRACT"
    opencode_userns_args=(--userns "$OPENCODE_USERNS")
  fi

  local wss_port container_name opencode_dir
  # `|| true`: ports_get returns nonzero for a genuinely unallocated key; under
  # `set -euo pipefail` a bare assignment would abort the whole start-tenant run
  # before the `[[ -z "$wss_port" ]]` skip below (mirrors start_tenant_vision).
  wss_port="$(ports_get "$name" opencode_wss)" || true
  container_name="lunarwing-opencode-$name"
  opencode_dir="${LUNARWING_ROOT}/opencode4lunarwing"

  if [[ -z "$wss_port" ]]; then
    say "no opencode_wss port allocated for $name (skipping opencode worker)"
    return 0
  fi

  if ! _ensure_tenant_image "$name" lunarwing-worker-opencode:latest; then
    say "opencode worker image not available; run 'build-opencode-worker' first (skipping)"
    return 0
  fi
  local image_user
  image_user="$(_ctr "$name" image inspect --format '{{.Config.User}}' \
    lunarwing-worker-opencode:latest 2>/dev/null || true)"
  case "$image_user" in
    "$OPENCODE_CONTAINER_UID"|"$OPENCODE_CONTAINER_UID:$OPENCODE_CONTAINER_GID") ;;
    *)
      die "opencode worker image has user '${image_user:-unset}', expected $OPENCODE_CONTAINER_UID:$OPENCODE_CONTAINER_GID; rebuild it"
      ;;
  esac

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local opencode_service="lunarwing-opencode-${name}.service"
    local quadlet_file
    quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-opencode-${name}.container"
    render_worker_quadlet "$name" opencode 8443
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if _container_config_changed "$name" "lunarwing-opencode-${name}" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "$opencode_service" "lunarwing-opencode-${name}"
    fi
    # A bad pre-upgrade image can exhaust StartLimitBurst. Loading a fixed image
    # does not clear that systemd lockout, so reset it before the explicit retry.
    _systemctl_user "$name" reset-failed "$opencode_service" >/dev/null 2>&1 || true
    if _systemctl_user "$name" start "$opencode_service" >/dev/null 2>&1; then
      _store_container_hash "$name" "lunarwing-opencode-${name}" "$quadlet_file"
      say "opencode worker ready via quadlet ($opencode_service, WSS port $wss_port)"
    else
      say "WARNING: $opencode_service failed to start" >&2
      _systemctl_user "$name" status "$opencode_service" --no-pager --full >&2 || true
    fi
    return 0
  fi

  local -a healthcheck_args=()
  if _openrc_podman_uses_init_health; then
    _reconcile_openrc_podman_healthcheck "$name" "$container_name"
    healthcheck_args=(--no-healthcheck)
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    local env_file_for_hash
    env_file_for_hash="$(tenant_env_dir "$name")/lunarwing.env"
    if _container_config_changed \
        "$name" "$container_name" "$env_file_for_hash" "$opencode_launch_contract"; then
      say "config changed for $container_name; force-recreating"
      _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
    elif _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "opencode worker already running ($container_name, WSS port $wss_port)"
      _store_container_hash \
        "$name" "$container_name" "$env_file_for_hash" "$opencode_launch_contract"
      _register_worker_unit "$name" opencode
      return 0
    else
      say "starting existing opencode worker container $container_name"
      _ctr "$name" start "$container_name" >/dev/null
      _store_container_hash \
        "$name" "$container_name" "$env_file_for_hash" "$opencode_launch_contract"
      _register_worker_unit "$name" opencode
      say "opencode worker ready ($container_name, WSS port $wss_port)"
      return 0
    fi
  fi

  if ! _ctr "$name" inspect "$container_name" &>/dev/null; then
    say "creating opencode worker container $container_name on WSS port $wss_port"

    local tenant_env_path
    tenant_env_path="$(tenant_env_dir "$name")/lunarwing.env"

    local opencode_env_path
    opencode_env_path="$(tenant_env_dir "$name")/opencode.env"

    local env_flags=()
    if [[ -f "$tenant_env_path" ]]; then
      local agent_token
      agent_token="$(resolve_worker_auth_token "$tenant_env_path")"
      [[ -n "$agent_token" ]] && env_flags+=(-e "AGENT_AUTH_TOKEN=$agent_token")

      local llm_api_key
      llm_api_key="$(_read_env_value "$tenant_env_path" LLM_API_KEY)"
      [[ -n "$llm_api_key" ]] && env_flags+=(-e "TENSORMOON_API_KEY=$llm_api_key")
      # Pre-fork name: worker images built before the TensorMoon switch read it.
      [[ -n "$llm_api_key" ]] && env_flags+=(-e "TENSORZERO_API_KEY=$llm_api_key")

      local opencode_model opencode_base_url
      opencode_model="$(_read_env_value "$tenant_env_path" OPENCODE_MODEL)"
      [[ -n "$opencode_model" ]] && env_flags+=(-e "OPENCODE_MODEL=$opencode_model")
      opencode_base_url="$(_read_env_value "$tenant_env_path" OPENCODE_BASE_URL)"
      [[ -n "$opencode_base_url" ]] && env_flags+=(-e "OPENCODE_BASE_URL=$opencode_base_url")
    fi

    if [[ -f "$opencode_env_path" ]]; then
      env_flags+=(--env-file "$opencode_env_path")
    fi

    local study_notes_home study_notes_state_home study_notes_allow_write
    local -a study_notes_mount=()
    study_notes_home="$(tenant_study_notes_home "$name")"
    if [[ -n "$study_notes_home" ]]; then
      validate_study_notes_home "$study_notes_home"
      study_notes_mount=(
        -v "${study_notes_home}:${STUDY_NOTES_CONTAINER_HOME}:ro,z"
        -e "LW2STUDYMCP_HOME=${STUDY_NOTES_CONTAINER_HOME}"
      )
      study_notes_state_home="$(tenant_study_notes_state_home "$name")"
      study_notes_allow_write="$(tenant_study_notes_allow_write "$name")"
      if study_notes_write_enabled "$study_notes_allow_write"; then
        [[ -n "$study_notes_state_home" ]] \
          || die "Study Notes write mode requires LW2STUDYMCP_STATE_HOME"
      fi
      if [[ -n "$study_notes_state_home" ]]; then
        validate_study_notes_state_home "$study_notes_state_home"
        study_notes_mount+=(
          -v "${study_notes_state_home}:${STUDY_NOTES_CONTAINER_STATE_HOME}:ro,z"
          -e "LW2STUDYMCP_STATE_HOME=${STUDY_NOTES_CONTAINER_STATE_HOME}"
        )
      fi
    fi

    local workspace_dir
    workspace_dir="$(prepare_worker_workspace "$name" opencode)"

    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    local -a health_publish=()
    local host_health_port
    host_health_port="$(ports_get "$name" opencode_health)" || true
    [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:8443")
    local ssh_agent_dir
    ssh_agent_dir="$(tenant_run_dir "$name")/ssh-agent"
    local -a ssh_mount=(-v "${ssh_agent_dir}:/tmp/ssh-agent:ro,z" -e SSH_AUTH_SOCK=/tmp/ssh-agent/ssh-agent.sock)
    _ctr "$name" run -d \
      --name "$container_name" \
      "${healthcheck_args[@]}" \
      "${opencode_userns_args[@]}" \
      -e LUNARWING_WORKER_ID="worker-opencode-${name}" \
      -e WS_PORT="$wss_port" \
      -e HEALTH_PORT="8443" \
      -e OPENCODE_MODE=websocket \
      -e WS_ROLE=server \
      -e WS_BIND_HOST=0.0.0.0 \
      -e WS_PATH=/ws/agent \
      "${env_flags[@]}" \
      "${ssh_mount[@]}" \
      "${study_notes_mount[@]}" \
      -p "127.0.0.1:${wss_port}:${wss_port}" \
      "${health_publish[@]}" \
      -v "$workspace_dir:/workspace:z" \
      "${restart_arg[@]}" \
      lunarwing-worker-opencode:latest \
      --mode websocket >/dev/null
    _store_container_hash \
      "$name" "$container_name" "$(tenant_env_dir "$name")/lunarwing.env" \
      "$opencode_launch_contract"
  fi

  _register_worker_unit "$name" opencode
  say "opencode worker ready ($container_name, WSS port $wss_port)"
}

stop_tenant_opencode() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-opencode-$name"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container_name}" ]]; then
    _deregister_babysitter "$container_name"
    rc-service "$container_name" stop >/dev/null 2>&1 || true
    say "opencode worker stopped ($container_name)"
  elif [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] \
      && podman_supports_quadlet; then
    # Quadlet owns the container: stop the unit, not the container, or
    # Restart=on-failure resurrects it (goal #10; pebble parity from WI#128).
    if _systemctl_user "$name" stop "${container_name}.service" >/dev/null 2>&1 \
        || _ctr "$name" stop "$container_name" >/dev/null 2>&1; then
      say "opencode worker stopped ($container_name)"
    fi
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "opencode worker stopped ($container_name)"
  fi
}

start_tenant_opencode2() {
  local name="$1"
  ensure_container_runtime

  # Record the mount policy even for rootful/OpenRC imperative containers.
  local launch_contract="ssh-agent-dir-ro"
  local -a userns_args=()
  if [[ "$CONTAINER_RT" == "podman" && "$MT_ROOTLESS" == "true" ]]; then
    launch_contract="$OPENCODE2_ROOTLESS_LAUNCH_CONTRACT"
    userns_args=(--userns "$OPENCODE2_USERNS")
  fi

  local wss_port container_name worker_dir
  wss_port="$(ports_get "$name" opencode2_wss)" || true
  container_name="lunarwing-opencode2-$name"
  worker_dir="${LUNARWING_ROOT}/opencode2.4lunarwing"

  if [[ -z "$wss_port" ]]; then
    say "no opencode2_wss port allocated for $name (skipping opencode2 worker)"
    return 0
  fi

  if ! _ensure_tenant_image "$name" lunarwing-worker-opencode2:latest; then
    say "opencode2 worker image not available; run 'build-opencode2-worker' first (skipping)"
    return 0
  fi
  local image_user
  image_user="$(_ctr "$name" image inspect --format '{{.Config.User}}' \
    lunarwing-worker-opencode2:latest 2>/dev/null || true)"
  case "$image_user" in
    "$OPENCODE2_CONTAINER_UID"|"$OPENCODE2_CONTAINER_UID:$OPENCODE2_CONTAINER_GID") ;;
    *)
      die "opencode2 worker image has user '${image_user:-unset}', expected $OPENCODE2_CONTAINER_UID:$OPENCODE2_CONTAINER_GID; rebuild it"
      ;;
  esac

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local service="lunarwing-opencode2-${name}.service"
    local quadlet_file
    quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-opencode2-${name}.container"
    render_worker_quadlet "$name" opencode2 8443
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if _container_config_changed "$name" "lunarwing-opencode2-${name}" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "$service" "lunarwing-opencode2-${name}"
    fi
    _systemctl_user "$name" reset-failed "$service" >/dev/null 2>&1 || true
    if _systemctl_user "$name" start "$service" >/dev/null 2>&1; then
      _store_container_hash "$name" "lunarwing-opencode2-${name}" "$quadlet_file"
      say "opencode2 worker ready via quadlet ($service, WSS port $wss_port)"
    else
      say "WARNING: $service failed to start" >&2
      _systemctl_user "$name" status "$service" --no-pager --full >&2 || true
    fi
    return 0
  fi

  local -a healthcheck_args=()
  if _openrc_podman_uses_init_health; then
    _reconcile_openrc_podman_healthcheck "$name" "$container_name"
    healthcheck_args=(--no-healthcheck)
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    local env_file_for_hash
    env_file_for_hash="$(tenant_env_dir "$name")/lunarwing.env"
    if _container_config_changed \
        "$name" "$container_name" "$env_file_for_hash" "$launch_contract"; then
      say "config changed for $container_name; force-recreating"
      _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
    elif _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "opencode2 worker already running ($container_name, WSS port $wss_port)"
      _store_container_hash \
        "$name" "$container_name" "$env_file_for_hash" "$launch_contract"
      _register_worker_unit "$name" opencode2
      return 0
    else
      say "starting existing opencode2 worker container $container_name"
      _ctr "$name" start "$container_name" >/dev/null
      _store_container_hash \
        "$name" "$container_name" "$env_file_for_hash" "$launch_contract"
      _register_worker_unit "$name" opencode2
      say "opencode2 worker ready ($container_name, WSS port $wss_port)"
      return 0
    fi
  fi

  if ! _ctr "$name" inspect "$container_name" &>/dev/null; then
    say "creating opencode2 worker container $container_name on WSS port $wss_port"

    local tenant_env_path
    tenant_env_path="$(tenant_env_dir "$name")/lunarwing.env"

    local worker_env_path
    worker_env_path="$(tenant_env_dir "$name")/opencode2.env"

    local -a env_flags=()
    if [[ -f "$tenant_env_path" ]]; then
      local agent_token
      agent_token="$(resolve_worker_auth_token "$tenant_env_path")"
      [[ -n "$agent_token" ]] && env_flags+=(-e "AGENT_AUTH_TOKEN=$agent_token")

      local llm_api_key
      llm_api_key="$(_read_env_value "$tenant_env_path" LLM_API_KEY)"
      [[ -n "$llm_api_key" ]] && env_flags+=(-e "TENSORMOON_API_KEY=$llm_api_key")
      # Pre-fork name: worker images built before the TensorMoon switch read it.
      [[ -n "$llm_api_key" ]] && env_flags+=(-e "TENSORZERO_API_KEY=$llm_api_key")

      local oc2_model oc2_base_url
      oc2_model="$(_read_env_value "$tenant_env_path" OPENCODE2_MODEL)"
      [[ -n "$oc2_model" ]] && env_flags+=(-e "OPENCODE2_MODEL=$oc2_model")
      oc2_base_url="$(_read_env_value "$tenant_env_path" OPENCODE2_BASE_URL)"
      [[ -n "$oc2_base_url" ]] && env_flags+=(-e "OPENCODE2_BASE_URL=$oc2_base_url")
    fi

    if [[ -f "$worker_env_path" ]]; then
      env_flags+=(--env-file "$worker_env_path")
    fi

    local workspace_dir
    workspace_dir="$(prepare_worker_workspace "$name" opencode2)"

    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    local -a health_publish=()
    local host_health_port
    host_health_port="$(ports_get "$name" opencode2_health)" || true
    [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:8443")
    local ssh_agent_dir
    ssh_agent_dir="$(tenant_run_dir "$name")/ssh-agent"
    local -a ssh_mount=(-v "${ssh_agent_dir}:/tmp/ssh-agent:ro,z" -e SSH_AUTH_SOCK=/tmp/ssh-agent/ssh-agent.sock)
    _ctr "$name" run -d \
      --name "$container_name" \
      "${healthcheck_args[@]}" \
      "${userns_args[@]}" \
      -e LUNARWING_WORKER_ID="worker-opencode2-${name}" \
      -e WS_PORT="$wss_port" \
      -e HEALTH_PORT="8443" \
      -e OPENCODE_MODE=websocket \
      -e WS_ROLE=server \
      -e WS_BIND_HOST=0.0.0.0 \
      -e WS_PATH=/ws/agent \
      "${env_flags[@]}" \
      "${ssh_mount[@]}" \
      "${study_notes_mount[@]}" \
      -p "127.0.0.1:${wss_port}:${wss_port}" \
      "${health_publish[@]}" \
      -v "$workspace_dir:/workspace:z" \
      "${restart_arg[@]}" \
      lunarwing-worker-opencode2:latest \
      --mode websocket >/dev/null
    _store_container_hash \
      "$name" "$container_name" "$(tenant_env_dir "$name")/lunarwing.env" \
      "$launch_contract"
  fi

  _register_worker_unit "$name" opencode2
  say "opencode2 worker ready ($container_name, WSS port $wss_port)"
}

stop_tenant_opencode2() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-opencode2-$name"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container_name}" ]]; then
    _deregister_babysitter "$container_name"
    rc-service "$container_name" stop >/dev/null 2>&1 || true
    say "opencode2 worker stopped ($container_name)"
  elif [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] \
      && podman_supports_quadlet; then
    # Quadlet owns the container: stop the unit, not the container, or
    # Restart=on-failure resurrects it (goal #10; pebble parity from WI#128).
    if _systemctl_user "$name" stop "${container_name}.service" >/dev/null 2>&1 \
        || _ctr "$name" stop "$container_name" >/dev/null 2>&1; then
      say "opencode2 worker stopped ($container_name)"
    fi
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "opencode2 worker stopped ($container_name)"
  fi
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
    opencode2)
      container_uid="$OPENCODE2_CONTAINER_UID"
      container_gid="$OPENCODE2_CONTAINER_GID"
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
  local name="$1" source_path target_path key value staged
  source_path="$(tenant_env_dir "$name")/lunarwing.env"
  target_path="$(tenant_env_dir "$name")/kimi-runtime.env"
  # Generate the complete runtime env in root-private staging first: a failed
  # source read or token resolution must leave any existing target untouched
  # instead of publishing the partial stream (die() only exits this subshell).
  staged="$(mktemp)" || return 1
  if ! (
    for key in KIMI_MODEL KIMI_BASE_URL KIMI_API_KEY; do
      value="$(read_tenant_env_value "$name" "$source_path" "$key")" || exit 1
      [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] \
        || die "$key cannot contain a newline"
      [[ -z "$value" ]] || printf '%s=%s\n' "$key" "$value"
    done

    value="$(resolve_worker_auth_token "$source_path")" || exit 1
    [[ -n "$value" ]] \
      || die "Kimi worker requires WORKER_AUTH_TOKEN (or GATEWAY_AUTH_TOKEN)"
    # Worker bridge expects AGENT_AUTH_TOKEN, not the daemon's env variable name.
    printf 'AGENT_AUTH_TOKEN=%s\n' "$value"
  ) >"$staged" || ! write_tenant_file_atomic "$name" "$target_path" 600 <"$staged"; then
    rm -f -- "$staged"
    return 1
  fi
  rm -f -- "$staged"
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

  local -a healthcheck_args=()
  if _openrc_podman_uses_init_health; then
    _reconcile_openrc_podman_healthcheck "$name" "$container_name"
    healthcheck_args=(--no-healthcheck)
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
    # WI#206 remediation 3: the kimi lane no longer mounts the tenant
    # SSH-agent socket. The worker is an auto-approved headless task executor —
    # forwarded host credentials turn prompt injection into credential use.
    # Tenants needing SSH from worker tasks should use a scoped credential
    # broker, not the shared tenant agent. Other worker lanes keep the mount.
    _ctr "$name" run -d \
      --name "$container_name" \
      "${healthcheck_args[@]}" \
      "${kimi_userns_args[@]}" \
      --security-opt no-new-privileges \
      -e LUNARWING_WORKER_ID="worker-kimi-${name}" \
      -e WS_PORT="$wss_port" \
      -e HEALTH_PORT="8443" \
      -e KIMI_MODE=websocket \
      -e WS_ROLE=server \
      -e WS_BIND_HOST=0.0.0.0 \
      -e WS_PATH=/ws/agent \
      "${env_flags[@]}" \
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
  elif [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] \
      && podman_supports_quadlet; then
    # Quadlet owns the container: stop the unit, not the container, or
    # Restart=on-failure resurrects it (goal #10; pebble parity from WI#128).
    if _systemctl_user "$name" stop "${container_name}.service" >/dev/null 2>&1 \
        || _ctr "$name" stop "$container_name" >/dev/null 2>&1; then
      say "kimi worker stopped ($container_name)"
    fi
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "kimi worker stopped ($container_name)"
  fi
}

# ── Pebble worker container ──────────────────────────────────────────────────

start_tenant_pebble() {
  local name="$1"
  ensure_container_runtime

  local wss_port container_name
  wss_port="$(ports_get "$name" pebble_wss)"
  container_name="lunarwing-pebble-$name"

  if [[ -z "$wss_port" ]]; then
    say "no pebble_wss port allocated for $name (skipping pebble worker)"
    return 0
  fi

  if ! _ensure_tenant_image "$name" lunarwing-worker-pebble:latest; then
    say "pebble worker image not available; run 'build-pebble-worker' first (skipping)"
    return 0
  fi

  # systemd + rootless podman: Quadlet .container owns the lifecycle (the unit's
  # [Container] spec creates+runs it), so skip the imperative `_ctr run` below.
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local quadlet_file pebble_service
    pebble_service="lunarwing-pebble-${name}.service"
    quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-pebble-${name}.container"
    render_worker_quadlet "$name" pebble 8443
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if _container_config_changed "$name" "lunarwing-pebble-${name}" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "$pebble_service" "lunarwing-pebble-${name}"
    fi
    _systemctl_user "$name" reset-failed "$pebble_service" >/dev/null 2>&1 || true
    if _systemctl_user "$name" start "$pebble_service" >/dev/null 2>&1; then
      _store_container_hash "$name" "lunarwing-pebble-${name}" "$quadlet_file"
      say "pebble worker ready via quadlet ($pebble_service, WSS port $wss_port)"
    else
      say "WARNING: $pebble_service failed to start" >&2
      _systemctl_user "$name" status "$pebble_service" --no-pager >&2 || true
    fi
    return 0
  fi

  local -a healthcheck_args=()
  if _openrc_podman_uses_init_health; then
    _reconcile_openrc_podman_healthcheck "$name" "$container_name"
    healthcheck_args=(--no-healthcheck)
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    local env_file_for_hash
    env_file_for_hash="$(tenant_env_dir "$name")/lunarwing.env"
    if _container_config_changed "$name" "$container_name" "$env_file_for_hash" "$PEBBLE_ROOTLESS_LAUNCH_CONTRACT"; then
      say "config changed for $container_name; force-recreating"
      _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
    elif _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "pebble worker already running ($container_name, WSS port $wss_port)"
      _store_container_hash "$name" "$container_name" "$env_file_for_hash" "$PEBBLE_ROOTLESS_LAUNCH_CONTRACT"
      _register_worker_unit "$name" pebble
      return 0
    else
      say "starting existing pebble worker container $container_name"
      _ctr "$name" start "$container_name" >/dev/null
      _store_container_hash "$name" "$container_name" "$env_file_for_hash" "$PEBBLE_ROOTLESS_LAUNCH_CONTRACT"
      _register_worker_unit "$name" pebble
      say "pebble worker ready ($container_name, WSS port $wss_port)"
      return 0
    fi
  fi

  if ! _ctr "$name" inspect "$container_name" &>/dev/null; then
    say "creating pebble worker container $container_name on WSS port $wss_port"

    local tenant_env_path
    tenant_env_path="$(tenant_env_dir "$name")/lunarwing.env"

    local pebble_env_path
    pebble_env_path="$(tenant_env_dir "$name")/pebble.env"

    local env_flags=()
    if [[ -f "$tenant_env_path" ]]; then
      local agent_token
      agent_token="$(resolve_worker_auth_token "$tenant_env_path")"
      [[ -n "$agent_token" ]] && env_flags+=(-e "AGENT_AUTH_TOKEN=$agent_token")
    fi

    if [[ -f "$pebble_env_path" ]]; then
      env_flags+=(--env-file "$pebble_env_path")
    fi

    local workspace_dir
    workspace_dir="$(tenant_lw_root "$name")/pebble-workspace"
    mkdir -p "$workspace_dir"
    chown "$name:$name" "$workspace_dir"
    chmod 777 "$workspace_dir"

    # HEALTH_PORT=8443 matches the image's baked HEALTHCHECK (in-container).
    # v8: also publish the tenant's dedicated pebble_health port -> container
    # 8443, so the host self-heal pipeline can probe /health directly.
    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    local -a health_publish=()
    local host_health_port
    host_health_port="$(ports_get "$name" pebble_health)" || true
    [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:8443")
    # SSH agent socket (always included — daemon creates it at startup).
    # Directory mount so daemon restarts self-heal (fresh socket inode is
    # resolved at connect time). :ro blocks socket replacement by workers;
    # :z shares the SELinux label across worker lanes.
    local -a ssh_mount=()
    local ssh_agent_dir
    ssh_agent_dir="$(tenant_run_dir "$name")/ssh-agent"
    ssh_mount=(-v "${ssh_agent_dir}:/tmp/ssh-agent:ro,z" -e SSH_AUTH_SOCK=/tmp/ssh-agent/ssh-agent.sock)
    _ctr "$name" run -d \
      --name "$container_name" \
      "${healthcheck_args[@]}" \
      -e LUNARWING_WORKER_ID="worker-pebble-${name}" \
      -e WS_PORT="$wss_port" \
      -e HEALTH_PORT="8443" \
      -e PEBBLE_MODE=websocket \
      -e WS_BIND_HOST=0.0.0.0 \
      -e WS_PATH=/ws/agent \
      "${env_flags[@]}" \
      "${ssh_mount[@]}" \
      -p "127.0.0.1:${wss_port}:${wss_port}" \
      "${health_publish[@]}" \
      -v "$workspace_dir:/workspace:z" \
      "${restart_arg[@]}" \
      lunarwing-worker-pebble:latest >/dev/null
    _store_container_hash "$name" "$container_name" "$(tenant_env_dir "$name")/lunarwing.env" "$PEBBLE_ROOTLESS_LAUNCH_CONTRACT"
  fi

  _register_worker_unit "$name" pebble
  say "pebble worker ready ($container_name, WSS port $wss_port)"
}

stop_tenant_pebble() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-pebble-$name"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container_name}" ]]; then
    _deregister_babysitter "$container_name"
    rc-service "$container_name" stop >/dev/null 2>&1 || true
    say "pebble worker stopped ($container_name)"
  elif [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] \
      && podman_supports_quadlet; then
    if _systemctl_user "$name" stop "${container_name}.service" >/dev/null 2>&1 \
        || _ctr "$name" stop "$container_name" >/dev/null 2>&1; then
      say "pebble worker stopped ($container_name)"
    fi
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "pebble worker stopped ($container_name)"
  fi
}

require_worker_name() {
  case "${1:-}" in
    nanocode) die "Nanocode worker is retired" ;;
    pebble|opencode|kimi|opencode2) ;;
    *) die "unknown worker '${1:-}'; expected pebble, opencode, kimi, or opencode2" ;;
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

# Stop both worker units and remove every boot path. Init scripts stay rendered
# so an explicit start-worker can re-enable them without reprovisioning.
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

restart_worker() {
  stop_worker "$1" "$2"
  start_worker "$1" "$2"
}
