#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions are reached through the root dispatcher
#
# Host-side source publication and operator controls for additive knowledge sync.
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly.

install_knowledge_sync_admin_snapshot() {
  local root="${LUNARWING_MT_KNOWLEDGE_SYNC_ADMIN_ROOT:-/usr/local/lib/lunarwing-study-notes-sync}"
  local releases="$root/releases" release current_tmp file
  [[ "$root" == /* && "$root" =~ ^/[A-Za-z0-9._/-]+$ ]] \
    || die "knowledge sync admin root must be an absolute safe path"
  install -d -m 0755 -- "$releases"
  release="$releases/$(date -u '+%Y%m%d%H%M%S')-$$"
  install -d -m 0755 -- "$release/mt-admin-lib"
  install -m 0755 -- "$SCRIPT_DIR/lunarwing-mt-admin.sh" "$release/lunarwing-mt-admin.sh"
  for file in "$SCRIPT_DIR"/mt-admin-lib/*.sh; do
    install -m 0755 -- "$file" "$release/mt-admin-lib/$(basename -- "$file")"
  done
  current_tmp="$root/.current.$$"
  ln -s -- "$release" "$current_tmp"
  mv -fT -- "$current_tmp" "$root/current"
  printf '%s/lunarwing-mt-admin.sh\n' "$root/current"
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

sync_enabled_study_notes_sources() {
  require_root
  ports_registry_require_readonly
  local name env_path enabled home
  local -A seen=()
  while IFS= read -r name; do
    env_path="$(tenant_env_dir "$name")/lunarwing.env"
    [[ -f "$env_path" ]] || continue
    enabled="$(_env_existing "$env_path" KNOWLEDGE_SYNC_ENABLED)"
    # Mirror the daemon's parse_bool_env contract exactly (case-insensitive
    # true/1 only) so a hand-edited value can never leave the host scheduler
    # refreshing a source the daemon refuses to start with (yes/on).
    enabled="${enabled,,}"
    case "$enabled" in
      1|true) ;;
      "")
        ;;
      *)
        say "WARNING: tenant '$name' has unsupported KNOWLEDGE_SYNC_ENABLED='$enabled' (expected true/1/false/0); skipping"
        continue
        ;;
    esac
    home="$(_env_existing "$env_path" LW2STUDYMCP_HOME)"
    [[ -n "$home" ]] \
      || { say "WARNING: tenant '$name' enables knowledge sync without LW2STUDYMCP_HOME"; continue; }
    validate_study_notes_path "$home" "Study Notes home"
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
