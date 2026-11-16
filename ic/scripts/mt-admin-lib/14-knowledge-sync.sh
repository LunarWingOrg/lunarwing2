#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions are reached through the root dispatcher
#
# Host-side source publication and operator controls for additive knowledge sync.
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly.

install_knowledge_sync_admin_snapshot() {
  local root release base bump=1 current_tmp file source_commit
  root="$(knowledge_sync_admin_root)"
  local releases="$root/releases"
  [[ "$root" == /* && "$root" =~ ^/[A-Za-z0-9._/-]+$ ]] \
    || die "knowledge sync admin root must be an absolute safe path"
  # The contract is copied into a root-owned release that root's hourly job
  # sources. A source the load guard refuses (WI#722, WI#725) must not get
  # there: as a root-owned copy it would pass every later check.
  require_safe_installed_health_source \
    || die "refusing to stage the Study Notes sync snapshot from a refused health contract source; the reason is printed above"
  install -d -m 0755 -- "$releases"
  # A same-second re-arm (configure-study-mcp followed by an upgrade, a test
  # loop) must publish a NEW release directory: reusing the live one would leave
  # the stamp naming the previous commit while the files were overwritten.
  base="$releases/$(date -u '+%Y%m%d%H%M%S')-$$"
  release="$base"
  while [[ -e "$release" ]]; do
    release="$base-$bump"
    bump=$((bump + 1))
  done
  install -d -m 0755 -- "$release/mt-admin-lib" "$release/ic-infrastructure-health-check"
  install -m 0755 -- "$SCRIPT_DIR/lunarwing-mt-admin.sh" "$release/lunarwing-mt-admin.sh"
  for file in "$SCRIPT_DIR"/mt-admin-lib/*.sh; do
    install -m 0755 -- "$file" "$release/mt-admin-lib/$(basename -- "$file")"
  done
  install -m 0755 -- "$HEALTH_CONTRACT_RESOLVED_DIR/openrc-proxy-unit-contract.sh" \
    "$release/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh"
  # WI#715: record what this frozen copy was staged from. Root's hourly timer
  # executes this release until something restages it, so the commit is the
  # only way for an operator to tell a fresh copy from one frozen at the last
  # configure-study-mcp --enable-knowledge-sync.
  source_commit="$(knowledge_sync_checkout_commit)"
  {
    printf 'commit=%s\n' "${source_commit:-unknown}"
    printf 'source=%s\n' "$SCRIPT_DIR"
    printf 'staged_at=%s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  } >"$release/STAGED-FROM"
  chmod 0644 -- "$release/STAGED-FROM"
  current_tmp="$root/.current.$$"
  ln -s -- "$release" "$current_tmp"
  mv -fT -- "$current_tmp" "$root/current"
  _prune_knowledge_sync_snapshots "$releases" "$release"
  printf '%s/lunarwing-mt-admin.sh\n' "$root/current"
}

# Retain only the newest releases (the live one plus a small rollback margin).
# Timestamped names sort lexically newest-last; pruning after the symlink flip
# means the just-published release is always retained. Best-effort: an
# unpurgeable stale release must never fail a scheduler install.
_prune_knowledge_sync_snapshots() {  # <releases dir> <retained release path>
  local releases="$1" retained="$2" count
  [[ -d "$releases" ]] || return 0
  count="$(find "$releases" -mindepth 1 -maxdepth 1 -type d | wc -l)"
  ((count <= KNOWLEDGE_SYNC_SNAPSHOT_KEEP + 1)) && return 0
  while IFS= read -r file; do
    [[ "$file" != "$retained" ]] || continue
    rm -rf -- "$file" 2>/dev/null \
      || say "WARNING: could not prune old knowledge sync snapshot: $file"
  done < <(find "$releases" -mindepth 1 -maxdepth 1 -type d | sort | head -n -"$KNOWLEDGE_SYNC_SNAPSHOT_KEEP")
  return 0
}

# ── WI#715: snapshot provenance and upgrade restaging ────────────────────────
#
# Root's hourly Study Notes refresh runs from the release under `current`, not
# from this checkout. Before WI#715 that snapshot was only staged by
# `configure-study-mcp --enable-knowledge-sync` (and add-tenant's Study MCP
# path), so an upgrade delivered a new daemon but left the root-side sync — and
# its security fixes, including the WI#445 trust gate — frozen at the day the
# opt-in was last enabled. `upgrade-tenant` now calls
# refresh_deployed_knowledge_sync_scheduler (after the health-pipeline refresh)
# when, and only when, a scheduler is already installed.

knowledge_sync_admin_root() {
  printf '%s\n' "${LUNARWING_MT_KNOWLEDGE_SYNC_ADMIN_ROOT:-/usr/local/lib/lunarwing-study-notes-sync}"
}

# Commit of the operator checkout this mt-admin was read from. Empty when the
# host has no git metadata (a tarball deploy); callers then record "unknown"
# and restage on upgrade rather than compare two unavailable values.
knowledge_sync_checkout_commit() {
  git -C "$SCRIPT_DIR" rev-parse HEAD 2>/dev/null || true
}

# Commit recorded in the installed release. Empty for a snapshot staged before
# this stamp existed (WI#715).
knowledge_sync_staged_commit() {  # <admin root>
  local stamp="$1/current/STAGED-FROM" key value
  [[ -r "$stamp" ]] || return 0
  while IFS='=' read -r key value; do
    if [[ "$key" == "commit" ]]; then
      printf '%s' "$value"
      return 0
    fi
  done <"$stamp"
}

knowledge_sync_scheduler_installed() {
  local root
  root="$(knowledge_sync_admin_root)"
  [[ -x "$root/current/lunarwing-mt-admin.sh" ]]
}

# Restage an existing scheduler after an upgrade. NEVER creates the scheduler:
# a host without one must keep receiving no opt-in from an upgrade. A restage
# failure warns instead of failing the upgrade, whose tenant already restarted;
# the hourly job then keeps running the previous staged copy.
refresh_deployed_knowledge_sync_scheduler() {
  local root checkout_commit staged_commit restage_rc=0
  root="$(knowledge_sync_admin_root)"
  if ! knowledge_sync_scheduler_installed; then
    say "knowledge sync scheduler: not installed; skipping upgrade refresh"
    return 0
  fi
  checkout_commit="$(knowledge_sync_checkout_commit)"
  staged_commit="$(knowledge_sync_staged_commit "$root")"
  if [[ -n "$checkout_commit" && "$staged_commit" == "$checkout_commit" ]]; then
    say "knowledge sync scheduler: snapshot already staged from $checkout_commit"
    return 0
  fi
  # Contain die(): ensure_knowledge_source_scheduler exits on a missing
  # systemctl/crontab, and that must not turn a completed upgrade into a
  # failure. Full reinstall also repairs a timer/cron block an OS update lost.
  ( set -e; ensure_knowledge_source_scheduler ) || restage_rc=$?
  if (( restage_rc != 0 )); then
    say "WARNING: could not restage the root Study Notes sync snapshot (exit $restage_rc); the hourly job keeps running the previous staged copy"
    return 0
  fi
  say "knowledge sync scheduler: restaged from ${checkout_commit:-an unknown checkout}"
}

# One operator-visible line for doctor: what the hourly root-side sync is
# actually running, and whether it lags this checkout.
report_knowledge_sync_scheduler_status() {
  local root staged_commit checkout_commit
  root="$(knowledge_sync_admin_root)"
  if ! knowledge_sync_scheduler_installed; then
    printf '[info] knowledge sync scheduler: not installed (opt-in via configure-study-mcp --enable-knowledge-sync)\n'
    return 0
  fi
  staged_commit="$(knowledge_sync_staged_commit "$root")"
  if [[ -z "$staged_commit" ]]; then
    staged_commit="unknown (staged before WI#715 provenance stamping)"
  fi
  printf '[info] knowledge sync scheduler: staged from %s (%s/current)\n' \
    "$staged_commit" "$root"
  checkout_commit="$(knowledge_sync_checkout_commit)"
  [[ -n "$checkout_commit" ]] || return 0
  if [[ "$staged_commit" == "$checkout_commit" ]]; then
    return 0
  fi
  printf '[WARN] knowledge sync scheduler is stale: staged %s, this checkout %s; restage with upgrade-tenant <tenant>\n' \
    "$staged_commit" "$checkout_commit"
}

_install_knowledge_source_systemd_timer() {  # <root-owned mt-admin entrypoint>
  local entrypoint="$1"
  local unit_dir="${LUNARWING_MT_SYSTEMD_UNIT_DIR:-/etc/systemd/system}"
  local service="$unit_dir/lunarwing-study-notes-sync.service"
  local timer="$unit_dir/lunarwing-study-notes-sync.timer"
  command -v systemctl >/dev/null 2>&1 \
    || die "systemctl not found; cannot install Study Notes source scheduler"
  mkdir -p "$unit_dir"
  cat >"$service" <<EOF
[Unit]
Description=Publish fresh Study Notes checkout generations for LunarWing tenants
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=$entrypoint sync-knowledge-sources
TimeoutStartSec=30min
EOF
  cat >"$timer" <<'EOF'
[Unit]
Description=Refresh LunarWing Study Notes source generations hourly

[Timer]
OnBootSec=11min
OnCalendar=hourly
RandomizedDelaySec=10min
Persistent=true
Unit=lunarwing-study-notes-sync.service

[Install]
WantedBy=timers.target
EOF
  systemctl daemon-reload >/dev/null \
    || die "failed to reload systemd after installing Study Notes source timer"
  systemctl enable --now lunarwing-study-notes-sync.timer >/dev/null \
    || die "failed to enable Study Notes source timer"
  say "Study Notes source refresh scheduled with systemd (hourly)"
}

_install_knowledge_source_cron() {  # <root-owned mt-admin entrypoint>
  local entrypoint="$1"
  local command="" begin="# BEGIN lunarwing-study-notes-sync managed block"
  local end="# END lunarwing-study-notes-sync managed block" tmp
  command -v fcrontab >/dev/null 2>&1 && command="fcrontab"
  [[ -n "$command" ]] || { command -v crontab >/dev/null 2>&1 && command="crontab"; }
  [[ -n "$command" ]] \
    || die "fcrontab/crontab not found; cannot schedule Study Notes source refresh"
  tmp="$(mktemp)"
  "$command" -l 2>/dev/null | sed "/^${begin}$/,/^${end}$/d" >"$tmp" || true
  {
    printf '%s\n' "$begin"
    printf '17 * * * * %s sync-knowledge-sources\n' "$entrypoint"
    printf '%s\n' "$end"
  } >>"$tmp"
  if ! "$command" "$tmp"; then
    rm -f "$tmp"
    die "failed to install Study Notes source schedule with $command"
  fi
  rm -f "$tmp"
  _ensure_cron_runlevel "$([[ "$command" == "fcrontab" ]] && echo fcron)"
  say "Study Notes source refresh scheduled with $command (hourly at :17)"
}

ensure_knowledge_source_scheduler() {
  local entrypoint
  entrypoint="$(install_knowledge_sync_admin_snapshot)"
  ensure_init_system
  case "$INIT_SYSTEM" in
    systemd) _install_knowledge_source_systemd_timer "$entrypoint" ;;
    openrc) _install_knowledge_source_cron "$entrypoint" ;;
    *) die "Study Notes source scheduling is unsupported for INIT_SYSTEM=$INIT_SYSTEM" ;;
  esac
}

# WI#445: both root-side sync entrypoints take LW2STUDYMCP_HOME from the
# tenant's own lunarwing.env, and the tenant account can choose that value.
# Root filesystem operations under it (git clone, install -d, the lock file,
# the manifest publish) are only safe when the home is the operator default
# or its deepest existing ancestor chain is root-owned, symlink-free, and not
# group/other-writable — the darkirc lane's root-follows-tenant-path rules.
study_notes_sync_home_trusted() {  # <checkout home>
  local home="$1" anchor
  [[ "$home" == "$STUDY_NOTES_DEFAULT_HOME" ]] && return 0
  [[ "$home" =~ ^/[A-Za-z0-9._/-]+$ ]] || return 1
  # The walk validates only the deepest existing ancestor; a "." or ".."
  # component in the missing tail would let the literal path the sync then
  # uses resolve outside the validated chain (run-174 LV-7).
  [[ "/$home/" != */../* && "/$home/" != */./* ]] || return 1
  anchor="$home"
  while [[ ! -e "$anchor" && ! -L "$anchor" ]]; do anchor="$(dirname -- "$anchor")"; done
  darkirc_validate_root_owned_directory "$anchor"
}

sync_enabled_study_notes_sources() {
  require_root
  ports_registry_require_readonly
  local name env_path enabled home
  local -A seen=()
  while IFS= read -r name; do
    env_path="$(tenant_env_dir "$name")/lunarwing.env"
    [[ -f "$env_path" ]] || continue
    enabled="$(_env_existing "$env_path" KNOWLEDGE_SYNC_ENABLED)"
    # Mirror the daemon's parse_bool_env contract exactly: case-insensitive
    # true/1 enable, while unset/false/0 use the daemon's default of false.
    # Other hand-edited values (yes/on) never leave the host scheduler
    # refreshing a source the daemon refuses to start with.
    enabled="${enabled,,}"
    case "$enabled" in
      1|true) ;;
      ""|false|0) continue ;;
      *)
        say "WARNING: tenant '$name' has unsupported KNOWLEDGE_SYNC_ENABLED='$enabled' (expected true/1/false/0); skipping"
        continue
        ;;
    esac
    home="$(_env_existing "$env_path" LW2STUDYMCP_HOME)"
    [[ -n "$home" ]] \
      || { say "WARNING: tenant '$name' enables knowledge sync without LW2STUDYMCP_HOME"; continue; }
    # WI#445: a tenant-chosen home must be trusted before root-side sync
    # operations run under it; an untrusted or malformed value skips only
    # this tenant instead of aborting the whole hourly run.
    study_notes_sync_home_trusted "$home" \
      || { say "WARNING: tenant '$name' LW2STUDYMCP_HOME is not trusted for the root-side sync: $home (expected the operator default or a root-owned directory); skipping"; continue; }
    seen["$home"]=1
  done < <(jq -r '.tenants | keys[]' "$PORTS_REGISTRY")

  if [[ "${#seen[@]}" -eq 0 ]]; then
    say "no enabled tenant Study Notes sources to refresh"
    return 0
  fi
  for home in "${!seen[@]}"; do
    sync_study_notes_checkout "$home"
  done
}

run_tenant_knowledge_refresh() {  # <tenant> <sync source true|false>
  local name="$1" sync_source="$2" env_path home repo binary helper
  name="$(sanitize_name "$name")"
  ports_registry_require_readonly
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path"
  home="$(_env_existing "$env_path" LW2STUDYMCP_HOME)"
  [[ -n "$home" ]] || die "Study Notes MCP is not configured for tenant '$name'"
  if [[ "$sync_source" == "true" ]]; then
    # WI#445: an explicit operator request dies on an untrusted home rather
    # than running the root-side sync under a tenant-chosen path.
    study_notes_sync_home_trusted "$home" \
      || die "tenant '$name' LW2STUDYMCP_HOME is not trusted for the root-side sync: $home (expected the operator default or a root-owned directory)"
    sync_study_notes_checkout "$home"
  fi
  [[ -r "$home/.git/lunarwing-study-notes-generation.json" ]] \
    || die "Study Notes source has no published generation; rerun with --sync-source"

  repo="$(tenant_repo "$name")"
  binary="$repo/target/${PROFILE}/lunarwing"
  [[ -x "$binary" ]] || die "tenant LunarWing binary missing: $binary"
  helper="$OPENRC_ENV_EXEC"
  [[ -x "$helper" ]] || helper="$OPENRC_ENV_EXEC_SRC"
  [[ -x "$helper" ]] || die "safe tenant environment loader missing: $helper"
  sudo -u "$name" "$helper" --env-file "$env_path" -- \
    "$binary" --no-onboard refresh-knowledge --json
}
