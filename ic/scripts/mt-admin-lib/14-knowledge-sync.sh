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
  # Every step from here on is checked by hand (WI#736). Callers run this
  # function inside a command substitution, where bash drops errexit, and
  # inside `( … ) || rc=$?`, where bash ignores it: an unchecked failure used
  # to publish an incomplete release, or none, and still report success.
  install -d -m 0755 -- "$releases" \
    || die "cannot create the Study Notes sync release directory $releases"
  # A same-second re-arm (configure-study-mcp followed by an upgrade, a test
  # loop) must publish a NEW release directory: reusing the live one would leave
  # the stamp naming the previous commit while the files were overwritten.
  base="$releases/$(date -u '+%Y%m%d%H%M%S')-$$"
  release="$base"
  while [[ -e "$release" ]]; do
    release="$base-$bump"
    bump=$((bump + 1))
  done
  if ! _stage_knowledge_sync_release "$release"; then
    rm -rf -- "$release"
    die "could not stage a complete Study Notes sync release under $releases; $root/current was not changed"
  fi
  current_tmp="$root/.current.$$"
  if ! { ln -s -- "$release" "$current_tmp" && mv -fT -- "$current_tmp" "$root/current"; }; then
    rm -f -- "$current_tmp"
    rm -rf -- "$release"
    die "could not publish the Study Notes sync release $release; $root/current was not changed"
  fi
  _prune_knowledge_sync_snapshots "$releases" "$release"
  printf '%s/lunarwing-mt-admin.sh\n' "$root/current"
}

# Copy one release and prove that every file arrived whole. Returns non-zero at
# the first step that fails; the caller removes the partial directory and stops
# before `current` is switched, so root's hourly job never runs a release that
# lacks a file.
_stage_knowledge_sync_release() {  # <new release dir>
  local release="$1" file source_commit
  local contract="$HEALTH_CONTRACT_RESOLVED_DIR/openrc-proxy-unit-contract.sh"
  install -d -m 0755 -- "$release/mt-admin-lib" "$release/ic-infrastructure-health-check" || return 1
  install -m 0755 -- "$SCRIPT_DIR/lunarwing-mt-admin.sh" "$release/lunarwing-mt-admin.sh" || return 1
  for file in "$SCRIPT_DIR"/mt-admin-lib/*.sh; do
    install -m 0755 -- "$file" "$release/mt-admin-lib/$(basename -- "$file")" || return 1
  done
  install -m 0755 -- "$contract" \
    "$release/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh" || return 1
  # WI#715: record what this frozen copy was staged from. Root's hourly timer
  # executes this release until something restages it, so the commit is the
  # only way for an operator to tell a fresh copy from one frozen at the last
  # configure-study-mcp --enable-knowledge-sync.
  source_commit="$(knowledge_sync_checkout_commit)"
  {
    printf 'commit=%s\n' "${source_commit:-unknown}"
    printf 'source=%s\n' "$SCRIPT_DIR"
    printf 'staged_at=%s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  } >"$release/STAGED-FROM" || return 1
  chmod 0644 -- "$release/STAGED-FROM" || return 1
  [[ -x "$release/lunarwing-mt-admin.sh" ]] || return 1
  cmp -s -- "$SCRIPT_DIR/lunarwing-mt-admin.sh" "$release/lunarwing-mt-admin.sh" || return 1
  for file in "$SCRIPT_DIR"/mt-admin-lib/*.sh; do
    cmp -s -- "$file" "$release/mt-admin-lib/$(basename -- "$file")" || return 1
  done
  cmp -s -- "$contract" \
    "$release/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh" || return 1
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
# when, and only when, the hourly schedule itself is present on the host. It
# republishes the staged files and nothing else: an upgrade must neither create
# a schedule nor re-enable one an operator removed or disabled (WI#715 box 2,
# review ADV-2).

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

# Is the hourly root-side schedule live on this host? The staged snapshot and
# the schedule are separate facts: the snapshot outlives a timer an operator
# removed or disabled, and that removal is an operator decision an upgrade must
# not undo. On systemd, the enabled state is the standing intent (a stopped but
# enabled timer is repaired); on OpenRC, an entry in the managed crontab block
# that cron would run is: a block whose line an operator commented out is a
# schedule switched off. A host with no init manager detected reports "not
# live", so an upgrade only ever refreshes files there.
knowledge_sync_scheduler_live() {
  local init unit_dir command begin='# BEGIN lunarwing-study-notes-sync managed block'
  local end='# END lunarwing-study-notes-sync managed block' line inside=0 live=1
  init="${INIT_SYSTEM:-}"
  if [[ -z "$init" ]]; then
    init="$(detect_init_system 2>/dev/null)" || return 1
  fi
  case "$init" in
    systemd)
      unit_dir="${LUNARWING_MT_SYSTEMD_UNIT_DIR:-/etc/systemd/system}"
      [[ -f "$unit_dir/lunarwing-study-notes-sync.timer" ]] || return 1
      [[ -f "$unit_dir/lunarwing-study-notes-sync.service" ]] || return 1
      systemctl is-enabled --quiet lunarwing-study-notes-sync.timer 2>/dev/null
      ;;
    openrc)
      command=""
      command -v fcrontab >/dev/null 2>&1 && command="fcrontab"
      [[ -n "$command" ]] || { command -v crontab >/dev/null 2>&1 && command="crontab"; }
      [[ -n "$command" ]] || return 1
      while IFS= read -r line; do
        if [[ "$line" == "$begin" ]]; then inside=1; continue; fi
        if [[ "$line" == "$end" ]]; then inside=0; continue; fi
        ((inside)) || continue
        [[ "$line" =~ ^[[:space:]]*(#|$) ]] || live=0
      done < <("$command" -l 2>/dev/null)
      return "$live"
      ;;
    *) return 1 ;;
  esac
}

# Restage an existing scheduler after an upgrade. NEVER creates the scheduler
# and NEVER re-enables one: a host without a snapshot receives nothing, and a
# host whose hourly schedule was removed or disabled gets its staged files
# refreshed but keeps that removal (WI#715 box 2, review ADV-2). A restage
# failure warns instead of failing the upgrade, whose tenant already restarted;
# the hourly job then keeps running the previous staged copy.
refresh_deployed_knowledge_sync_scheduler() {
  local root checkout_commit staged_commit restaged_commit restage_rc=0
  local current_before current_after
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
  current_before="$(readlink -- "$root/current" 2>/dev/null || true)"
  if knowledge_sync_scheduler_live; then
    # Live schedule: restage the files and repair the unit/cron entry in one
    # install. Contain die(): the staging exits on a refused source, and
    # ensure_knowledge_source_scheduler on a missing systemctl/crontab; neither
    # may turn a completed upgrade into a failure. bash ignores errexit inside
    # this `||`, so both functions check their own steps and stop before any
    # unit or cron line is written (WI#736).
    ( set -e; ensure_knowledge_source_scheduler ) || restage_rc=$?
  else
    # The snapshot outlived its schedule: the unit files are gone, the timer is
    # disabled, or the managed cron block was removed. Republish the files so a
    # later re-arm runs fixed code, and touch nothing else — no unit is
    # written, nothing is enabled, no daemon-reload.
    say "knowledge sync scheduler: hourly schedule is not active on this host; refreshing the staged copy without re-arming it"
    ( set -e; install_knowledge_sync_admin_snapshot >/dev/null ) || restage_rc=$?
  fi
  if (( restage_rc != 0 )); then
    say "WARNING: could not restage the root Study Notes sync snapshot (exit $restage_rc); the reason is printed above. The schedule was left as it was, and the hourly job keeps running the previous staged copy"
    return 0
  fi
  # A restage that could not replace the release — a read-only releases
  # directory, a full disk — must not be reported as delivered (review ADV-1).
  restaged_commit="$(knowledge_sync_staged_commit "$root")"
  if [[ -n "$checkout_commit" && "$restaged_commit" != "$checkout_commit" ]]; then
    say "WARNING: the Study Notes sync snapshot still names '${restaged_commit:-nothing}' after restaging from $checkout_commit; check that $root/releases is writable"
    return 0
  fi
  # Every staging publishes a new release directory, so an unchanged link means
  # nothing was delivered. This also holds for a checkout without git metadata,
  # where there is no commit to compare.
  current_after="$(readlink -- "$root/current" 2>/dev/null || true)"
  if [[ -z "$current_after" || "$current_after" == "$current_before" ]]; then
    say "WARNING: the Study Notes sync snapshot was not replaced: $root/current still points at '${current_before:-nothing}'; check that $root/releases is writable"
    return 0
  fi
  say "knowledge sync scheduler: restaged from ${checkout_commit:-an unknown checkout}"
}

# One operator-visible block for doctor: what the hourly root-side sync is
# actually running, whether it lags this checkout, and — when the snapshot
# outlived its schedule — that an upgrade will refresh but not re-arm it.
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
  if ! knowledge_sync_scheduler_live; then
    printf '[info] knowledge sync scheduler: hourly schedule is not active (removed or disabled); upgrade-tenant refreshes the staged copy but does not re-arm it — re-arm with configure-study-mcp <tenant> --enable-knowledge-sync\n'
  fi
  checkout_commit="$(knowledge_sync_checkout_commit)"
  [[ -n "$checkout_commit" ]] || return 0
  if [[ "$staged_commit" == "$checkout_commit" ]]; then
    return 0
  fi
  printf '[WARN] knowledge sync scheduler is stale: staged %s, this checkout %s; restage with upgrade-tenant <tenant>\n' \
    "$staged_commit" "$checkout_commit"
}

# Root's schedule must name the absolute path of an existing executable. An
# empty or bare command would be looked up in the scheduler's own search path,
# and whoever can write a directory of that path would supply what root runs
# every hour (WI#736). Checked in each installer as well as in their caller, so
# the property does not depend on how they are reached.
_require_knowledge_sync_entrypoint() {  # <entrypoint>
  [[ "${1:-}" =~ ^/[A-Za-z0-9._/-]+$ && -x "$1" ]] \
    || die "refusing to schedule the Study Notes source refresh with '${1:-}': the command must be the absolute path of an existing executable"
}

_install_knowledge_source_systemd_timer() {  # <root-owned mt-admin entrypoint>
  local entrypoint="${1:-}"
  local unit_dir="${LUNARWING_MT_SYSTEMD_UNIT_DIR:-/etc/systemd/system}"
  local service="$unit_dir/lunarwing-study-notes-sync.service"
  local timer="$unit_dir/lunarwing-study-notes-sync.timer"
  _require_knowledge_sync_entrypoint "$entrypoint"
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
  local entrypoint="${1:-}"
  local command="" begin="# BEGIN lunarwing-study-notes-sync managed block"
  local end="# END lunarwing-study-notes-sync managed block" tmp
  _require_knowledge_sync_entrypoint "$entrypoint"
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
  # A refused or failed staging ends only this command substitution, and a
  # caller that contains this function with `||` runs it with errexit ignored
  # (WI#736). So the result is checked here, whatever the caller's errexit
  # state: without a staged entrypoint no unit is written, no cron line, and
  # nothing is enabled or reloaded.
  entrypoint="$(install_knowledge_sync_admin_snapshot)" \
    || die "could not stage the Study Notes sync snapshot (the reason is printed above); the hourly schedule was left as it was"
  _require_knowledge_sync_entrypoint "$entrypoint"
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
