#!/usr/bin/env bash
# The post-WI#376 OpenRC proxy unit contract: the unit template, the
# interpreter resolver, the body renderer, the port extractor and the
# byte-for-byte safe-unit checker with its verdict cache.
#
# ONE implementation, three consumers (WI#410 review ID-2):
#   - ic/scripts/mt-admin-lib/08-units-openrc.sh sources this file from the
#     repo: the renderer writes units from it and the preflight guard
#     (require_safe_openrc_proxy_units) checks installed units against it;
#   - ic-infrastructure-health-check/health-openrc.sh and
#     lunarwing-self-heal.sh source the copy that ensure_health_pipeline
#     installs beside them (/usr/local/lib/lunarwing-health) and refuse to
#     drive rc-service as root past any installed unit that fails it.
# Because the cron reads the installed copy, a template change reaches the
# cron guard at the next health-lib sync (add-tenant, upgrade-tenant,
# enable-health-fleet.sh); until then the cron refuses re-rendered units and
# self-heal pages — fail closed, never accept.
#
# Sourceable with no dependencies under `set -euo pipefail`: pure functions,
# two memo variables and one cache. Function and variable names are
# mt-admin's (the transition harness drives them directly).

# Executed instead of sourced: nothing to do (ensure_health_pipeline marks
# every *.sh in the installed copy executable).
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  printf 'openrc-proxy-unit-contract.sh is a library: source it\n' >&2
  exit 2
fi

# Tenant layout + name helpers: mt-admin's 00-common.sh defines these before
# sourcing this file; the root health cron does not. The fallbacks must stay
# equal to 00-common.sh (tenant_home, tenant_lw_root, tenant_env_dir,
# tenant_log_dir, tenant_run_dir, sanitize_name) — a divergence changes the
# expected render and can only REFUSE installed units (fail closed, paged by
# lunarwing-self-heal.sh), never accept a tampered one.
if ! declare -F tenant_home >/dev/null 2>&1; then
  tenant_home() { printf '/home/%s' "$1"; }
fi
if ! declare -F tenant_lw_root >/dev/null 2>&1; then
  tenant_lw_root() { printf '%s/lunarwing' "$(tenant_home "$1")"; }
fi
if ! declare -F tenant_env_dir >/dev/null 2>&1; then
  tenant_env_dir() { printf '%s/env' "$(tenant_lw_root "$1")"; }
fi
if ! declare -F tenant_log_dir >/dev/null 2>&1; then
  tenant_log_dir() { printf '%s/logs' "$(tenant_lw_root "$1")"; }
fi
if ! declare -F tenant_run_dir >/dev/null 2>&1; then
  tenant_run_dir() { printf '%s/run' "$(tenant_lw_root "$1")"; }
fi
if ! declare -F sanitize_name >/dev/null 2>&1; then
  sanitize_name() {
    local raw="$1"
    printf '%s' "$raw" | tr '[:upper:]' '[:lower:]' | tr -cs 'a-z0-9-' '-' | sed 's/^-//;s/-$//'
  }
fi
# The env-exec launcher path is fixed; lunarwing-mt-admin.sh sets the same
# value before sourcing its libraries.
: "${OPENRC_ENV_EXEC:=/usr/local/libexec/lunarwing-openrc-env-exec}"

# Single source of truth for the post-WI#376 proxy unit shape. The renderer
# and the safe-unit contract both substitute concrete values for the @TOKENS@
# through _openrc_proxy_unit_body below, so the renderer and the contract
# render from the same text (WI#393 GR-1/HF-1). Tenant data lives only in the
# @TOKEN@ slots; everything else is fixed scaffolding.
_openrc_proxy_unit_template() {
  cat <<'INITEOF'
#!/sbin/openrc-run

description="LunarWing TensorZero proxy (@NAME@)"

: "${proxy_command:=@PYBIN@}"
: "${proxy_args:=@PROXYBIN@ --port @PORT@ --bind 127.0.0.1 --tensorzero-env}"
: "${proxy_user:=@NAME@}"
: "${proxy_group:=@NAME@}"
: "${proxy_pidfile:=@RUNDIR@/proxy.pid}"
: "${proxy_runtime_dir:=@RUNDIR@}"
: "${proxy_log_dir:=@LOGDIR@}"
: "${proxy_output_log:=${proxy_log_dir}/proxy.log}"
: "${proxy_error_log:=${proxy_log_dir}/proxy.err}"
: "${proxy_env_file:=@ENVDIR@/proxy.env}"
: "${proxy_openrc_env_exec:=@ENVEXEC@}"
: "${proxy_umask:=0077}"
: "${proxy_respawn_delay:=5}"
: "${proxy_respawn_max:=5}"
: "${proxy_respawn_period:=60}"
: "${proxy_retry:=SIGTERM/30/KILL/5}"

command="${proxy_openrc_env_exec}"
command_args="--env-file ${proxy_env_file} -- ${proxy_command} ${proxy_args}"
command_user="${proxy_user}:${proxy_group}"
pidfile="${proxy_pidfile}"
supervisor="supervise-daemon"
retry="${proxy_retry}"
respawn_delay="${proxy_respawn_delay}"
respawn_max="${proxy_respawn_max}"
respawn_period="${proxy_respawn_period}"
output_log="${proxy_output_log}"
error_log="${proxy_error_log}"
required_files="${proxy_openrc_env_exec} ${proxy_command} ${proxy_env_file}"

depend() {
    need net
    use dns
    after firewall
    before lunarwing-@NAME@
}

start_pre() {
    checkpath -d -m 0750 -o "${proxy_user}:${proxy_group}" "${proxy_runtime_dir}"
    checkpath -d -m 0750 -o "${proxy_user}:${proxy_group}" "${proxy_log_dir}"
    checkpath -f -m 0640 -o "${proxy_user}:${proxy_group}" "${output_log}"
    checkpath -f -m 0640 -o "${proxy_user}:${proxy_group}" "${error_log}"
    umask "${proxy_umask}"
}
INITEOF
}

# The one python3 spelling both the renderer and the safe-unit contract use
# (WI#393 HF-1). The unit's interpreter slot must not depend on the calling
# process's PATH: root login shells, sudo's secure_path, the onboarder's
# .venv and pyenv shims all resolve python3 differently, and a unit rendered
# under one spelling but checked under another is refused fleet-wide while
# the suggested repair re-renders it into the *other* spelling (ping-pong).
# LUNARWING_MT_PROXY_PYTHON3 pins an interpreter explicitly; otherwise the
# first python3 under the fixed /usr/local/bin:/usr/bin:/bin search path wins.
# `type -P` (not `command -v`) so a `python3` shell function in the caller's
# environment (exported via BASH_FUNC_python3%% or defined through BASH_ENV)
# cannot resolve to the bare word and fail the allowlist fleet-wide (HU-1).
# `type -P` does consult bash's hash table; it is the temporary PATH=
# assignment that flushes the table before the lookup, so a stale hashed
# python3 in the caller cannot reach the result either (pinned by the HF-1
# shadow-PATH test and the hash-entry pin in the transition harness). The
# result is allowlisted to plain path characters so it can never carry shell
# syntax — and so bash 5.2+ patsub_replacement has nothing to expand (HF-4).
# Any failure fails closed
# (the caller treats the unit as unsafe) and is not memoized, so a transient
# PATH problem recovers on the next call.
# The resolution is memoized per process (HF-2), keyed on the override value
# so a changed LUNARWING_MT_PROXY_PYTHON3 re-resolves instead of going stale.
_OPENRC_PROXY_PYTHON3_RESOLVED=""
_OPENRC_PROXY_PYTHON3_RESOLVED_FROM=$'\001'
_openrc_proxy_python3() {
  local override="${LUNARWING_MT_PROXY_PYTHON3:-}" pybin
  if [[ -z "$_OPENRC_PROXY_PYTHON3_RESOLVED" \
     || "$override" != "$_OPENRC_PROXY_PYTHON3_RESOLVED_FROM" ]]; then
    if [[ -n "$override" ]]; then
      pybin="$override"
    else
      pybin="$(PATH=/usr/local/bin:/usr/bin:/bin type -P python3)" || return 1
    fi
    [[ "$pybin" == /* && "$pybin" =~ ^[A-Za-z0-9._/+-]+$ ]] || return 1
    _OPENRC_PROXY_PYTHON3_RESOLVED="$pybin"
    _OPENRC_PROXY_PYTHON3_RESOLVED_FROM="$override"
  fi
  printf '%s' "$_OPENRC_PROXY_PYTHON3_RESOLVED"
}

# The unit body the renderer writes and the safe-unit contract expects, from
# the single template source above (WI#393 GR-1/HF-1). Shared so both sides
# substitute identically and cannot drift apart. Every replacement is quoted:
# with bash 5.2+ patsub_replacement an unquoted & in a value would expand to
# the matched token, writing a corrupted unit the checker then accepts (HF-4).
_openrc_proxy_unit_body() {
  local name="$1" proxy_port="$2" pybin body
  pybin="$(_openrc_proxy_python3)" || return 1
  body="$(_openrc_proxy_unit_template)" || return 1
  body="${body//@NAME@/"$name"}"
  body="${body//@PYBIN@/"$pybin"}"
  body="${body//@PROXYBIN@/"$(tenant_lw_root "$name")/tensorzero-proxy-configurations/lunarwing-proxy.py"}"
  body="${body//@PORT@/"$proxy_port"}"
  body="${body//@RUNDIR@/"$(tenant_run_dir "$name")"}"
  body="${body//@LOGDIR@/"$(tenant_log_dir "$name")"}"
  body="${body//@ENVDIR@/"$(tenant_env_dir "$name")"}"
  body="${body//@ENVEXEC@/"$OPENRC_ENV_EXEC"}"
  printf '%s\n' "$body"
}

# Extract the port from an installed unit's proxy_args line for the
# filename's tenant. The port is the one slot the checker cannot derive on
# its own: reading the ports registry does not belong in the OpenRC preflight
# path. The extracted value only feeds the expected-body render, so a wrong
# or missing extraction fails closed at the byte-for-byte comparison.
_openrc_proxy_unit_extract_port() {
  local path="$1" name="$2" line rest port=""
  # The port shape is ASCII digits only, in every locale (HF-6): a UTF-8
  # locale's [0-9] range can match non-ASCII digits that Python's int() would
  # also accept, smuggling a different value past a shape check.
  local LC_ALL=C
  local prefix port_re
  prefix=': "${proxy_args:='"$(tenant_lw_root "$name")"'/tensorzero-proxy-configurations/lunarwing-proxy.py --port '
  port_re='^([0123456789]{1,5}) --bind 127[.]0[.]0[.]1 --tensorzero-env[}]"$'
  local -a lines=()
  mapfile -t lines <"$path" || return 1
  for line in "${lines[@]}"; do
    [[ "$line" == "$prefix"* ]] || continue
    rest="${line#"$prefix"}"
    if [[ "$rest" =~ $port_re ]]; then
      port="${BASH_REMATCH[1]}"
      break
    fi
  done
  [[ -n "$port" ]] || return 1
  printf '%s\n' "$port"
}

# The post-WI#376 proxy unit contract: byte-for-byte equality with a fresh
# render from the shared template (WI#393 FL-6/GR-1, HF-1/HF-2). The installed
# unit must reproduce _openrc_proxy_unit_body for its filename's tenant
# exactly — same bytes in the same order and count, tenant name identical in
# every slot, the canonical interpreter/renderer paths, the unit's own port.
# Byte equality also covers NUL/CR bytes, the final newline and trailing
# lines, so no separate hygiene pass is needed. Any extra, missing, reordered,
# or payload-carrying content (a legacy inline --tensorzero flag, command
# substitution, a backtick, a prepended override, a replaced proxy_command)
# fails the contract, so a tampered unit is archived before replacement rather
# than passing on the strength of one safe-looking line. Shared by the
# preflight guard and the renderer, which archives only installed units that
# fail this check.
#
# Verdict cache (HF-2): the guard runs on every OpenRC service operation, so
# one `status` pass re-checks every installed proxy unit dozens of times.
# Only safe verdicts are cached, for the life of the process, keyed by path
# with the unit's dev:inode:mtime(ns):size signature plus the resolved
# interpreter — any replacement, edit or interpreter-resolution change forces
# a full re-check. The key is a stat signature, not a content hash: a
# same-size in-place rewrite that preserved dev:inode AND nanosecond mtime
# would keep the stale verdict (200 such rewrites produced no stale hit on
# ext4 or tmpfs, and only root writes init.d, so there is no tenant
# escalation either way).
declare -gA _OPENRC_PROXY_UNIT_SAFE_CACHE=()
openrc_proxy_unit_is_safe() {
  local path="$1" name sig port
  local LC_ALL=C
  [[ -f "$path" && ! -L "$path" ]] || return 1
  name="${path##*/lunarwing-proxy-}"
  [[ -n "$name" ]] || return 1
  sig="$(stat -c '%d:%i:%y:%s' -- "$path" 2>/dev/null)" || return 1
  [[ -n "$sig" ]] || return 1
  # Warm the interpreter memo with a DIRECT call (no command substitution), so
  # the resolution survives in this process and the cache comparison below is
  # subshell-free (HF-2). Subshell callers (e.g. `$(openrc_service_state …)`)
  # inherit the parent's warm memo and verdict cache at fork time.
  if [[ -z "$_OPENRC_PROXY_PYTHON3_RESOLVED" \
     || "${LUNARWING_MT_PROXY_PYTHON3:-}" != "$_OPENRC_PROXY_PYTHON3_RESOLVED_FROM" ]]; then
    _openrc_proxy_python3 >/dev/null || return 1
  fi
  # Fast path: a cache hit costs the one `$(stat …)` fork above — the
  # fork-heavy validations (name sanitize, body render, cmp) run only on a
  # miss, and only a fully verified unit is ever cached, so a hit can only
  # repeat a verdict this process already proved for this exact stat
  # signature under this exact interpreter resolution.
  if [[ "${_OPENRC_PROXY_UNIT_SAFE_CACHE[$path]:-}" == "$sig|$_OPENRC_PROXY_PYTHON3_RESOLVED" ]]; then
    return 0
  fi
  [[ "$name" == "$(sanitize_name "$name")" ]] || return 1
  port="$(_openrc_proxy_unit_extract_port "$path" "$name")" || return 1
  cmp -s -- "$path" <(_openrc_proxy_unit_body "$name" "$port") || return 1
  _OPENRC_PROXY_UNIT_SAFE_CACHE[$path]="$sig|$_OPENRC_PROXY_PYTHON3_RESOLVED"
  return 0
}

# Echo the first installed lunarwing-proxy-* unit under <initd dir> that fails
# the contract and return 1; return 0 with empty output when every installed
# unit is safe (a missing or empty directory is safe: nothing to drive). Used
# by the root health cron before any rc-service invocation (WI#410).
first_unsafe_openrc_proxy_unit() {
  local dir="${1:?initd dir}" path
  for path in "$dir"/lunarwing-proxy-*; do
    [[ -e "$path" || -L "$path" ]] || continue
    openrc_proxy_unit_is_safe "$path" || { printf '%s' "$path"; return 1; }
  done
  return 0
}
