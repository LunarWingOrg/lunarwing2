#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 04-tenant-lifecycle.sh — part of the lunarwing-mt-admin library.
#
# High-level tenant verbs: add/remove/start/stop/restart/upgrade,
# status/list/tokens/doctor, unit-render dispatch, migration + upgrade
# boundary helpers, WeeChat enablement, and tenant writer (DarkIRC) lifecycle.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced.
#
# Extracted from the v2.0.8.0 monolith (goals item #9), keeping the library
# structure of ref2/sunburst/mtadmin-refactor-v2.0.6.0 per
# docs/proposals/MT-ADMIN-DECOMPOSITION.md. Pure line-range move: no function
# body was rewritten.


cleanup_legacy_weechat() {
  local name="$1"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_init_system
  case "$INIT_SYSTEM" in
    systemd) cleanup_legacy_weechat_systemd "$name" ;;
    openrc) cleanup_legacy_weechat_openrc "$name" ;;
    *) die "unsupported init system: $INIT_SYSTEM" ;;
  esac
}

restart_weechat_adapter() {
  local name="$1"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_init_system
  case "$INIT_SYSTEM" in
    systemd)
      _systemctl_user "$name" restart "lunarwing-weechat-adapter-${name}.service"
      ;;
    openrc)
      rc-service "lunarwing-weechat-adapter-${name}" restart
      ;;
    *) die "unsupported init system: $INIT_SYSTEM" ;;
  esac
}

darkirc_service_uses_release_selector() {
  local name="$1" service_path expected command_prefix fragment drop_ins
  ensure_init_system
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    service_path="$(tenant_home "$name")/.config/systemd/user/lunarwing-darkirc-${name}.service"
    expected="ExecStart=$DARKIRC_RELEASE_DAEMON --config $(tenant_state_dir "$name")/darkirc/darkirc_config.toml"
    command_prefix='ExecStart='
  else
    service_path="$OPENRC_INITD_DIR/lunarwing-darkirc-${name}"
    expected="darkirc_command=\"$DARKIRC_RELEASE_DAEMON\""
    command_prefix='darkirc_command='
  fi
  [[ -f "$service_path" && ! -L "$service_path" ]] || return 1
  [[ "$(grep -Fxc -- "$expected" "$service_path")" -eq 1 \
    && "$(grep -Fc -- "$command_prefix" "$service_path")" -eq 1 ]] || return 1
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    _systemctl_user "$name" daemon-reload >/dev/null || return 1
    fragment="$(_systemctl_user "$name" show --value -p FragmentPath \
      "lunarwing-darkirc-${name}.service" 2>/dev/null)" || return 1
    drop_ins="$(_systemctl_user "$name" show --value -p DropInPaths \
      "lunarwing-darkirc-${name}.service" 2>/dev/null)" || return 1
    # Ignore global admin drop-ins (/usr/lib/systemd/user/service.d/*.conf),
    # which apply to every user service on the host; only tenant/unit-specific
    # drop-ins make the unit stale or unsafe.
    local drop_in
    for drop_in in $drop_ins; do
      case "$drop_in" in
        /usr/lib/systemd/user/service.d/*.conf) ;;
        *) return 1 ;;
      esac
    done
    [[ "$fragment" == "$service_path" ]]
  fi
}

# ISSUE-006: enable_weechat is authoritative for lifecycle, not just health
# classification. disable-weechat stops the optional weechat units and removes
# their boot persistence (ending any systemd restart loop); enable-weechat
# re-enables and starts them. Both are idempotent and tolerate never-rendered
# or missing units on either init system.
weechat_lifecycle_stop_disable() {
  local name="$1"
  case "$INIT_SYSTEM" in
    systemd)
      if ! id -u "$name" >/dev/null 2>&1; then
        say "tenant user '$name' absent; cannot stop/disable WeeChat" >&2
        return 1
      fi
      local user_unit_dir
      user_unit_dir="$(tenant_home "$name")/.config/systemd/user"
      _systemctl_user "$name" show-environment >/dev/null || return 1
      local units=() unit load_state
      for unit in "lunarwing-weechat-adapter-${name}.service" "lunarwing-weechat-${name}.service"; do
        if ! load_state="$(_systemctl_user "$name" show --value -p LoadState "$unit" 2>/dev/null)"; then
          return 1
        fi
        if [[ -f "$user_unit_dir/$unit" || ( -n "$load_state" && "$load_state" != "not-found" ) ]]; then
          units+=("$unit")
        fi
      done
      if ((${#units[@]} > 0)); then
        _systemctl_user "$name" stop "${units[@]}" || return 1
        _systemctl_user "$name" disable "${units[@]}" || return 1
      fi
      say "stopped + disabled lunarwing-weechat{,-adapter}-${name}.service (systemd)"
      ;;
    openrc)
      local svc link runlevel
      for svc in "lunarwing-weechat-adapter-${name}" "lunarwing-weechat-${name}"; do
        if [[ -x "$OPENRC_INITD_DIR/$svc" ]]; then
          rc-service "$svc" stop || return 1
        fi
        for link in "$OPENRC_RUNLEVELS_DIR"/*/"$svc"; do
          [[ -e "$link" || -L "$link" ]] || continue
          runlevel="$(basename "$(dirname "$link")")"
          rc-update del "$svc" "$runlevel" || return 1
        done
      done
      say "stopped + removed from runlevels: lunarwing-weechat{,-adapter}-${name} (OpenRC)"
      ;;
    *)
      say "weechat lifecycle: unsupported INIT_SYSTEM=$INIT_SYSTEM" >&2
      return 1
      ;;
  esac
}

weechat_lifecycle_enable_start() {
  local name="$1"
  case "$INIT_SYSTEM" in
    systemd)
      if ! id -u "$name" >/dev/null 2>&1; then
        say "tenant user '$name' absent; cannot enable/start WeeChat" >&2
        return 1
      fi
      local user_unit_dir
      user_unit_dir="$(tenant_home "$name")/.config/systemd/user"
      if [[ ! -f "$user_unit_dir/lunarwing-weechat-${name}.service" \
        || ! -f "$user_unit_dir/lunarwing-weechat-adapter-${name}.service" ]]; then
        say "weechat units not rendered for '$name'" >&2
        return 1
      fi
      _systemctl_user "$name" daemon-reload || return 1
      _systemctl_user "$name" enable "lunarwing-weechat-${name}.service" "lunarwing-weechat-adapter-${name}.service" || return 1
      _systemctl_user "$name" start "lunarwing-weechat-${name}.service" || return 1
      _systemctl_user "$name" start "lunarwing-weechat-adapter-${name}.service" || return 1
      say "enabled + started lunarwing-weechat{,-adapter}-${name}.service (systemd)"
      ;;
    openrc)
      if [[ ! -x "$OPENRC_INITD_DIR/lunarwing-weechat-${name}" \
        || ! -x "$OPENRC_INITD_DIR/lunarwing-weechat-adapter-${name}" ]]; then
        say "weechat units not rendered for '$name'" >&2
        return 1
      fi
      rc-update add "lunarwing-weechat-${name}" default || return 1
      rc-update add "lunarwing-weechat-adapter-${name}" default || return 1
      rc-service "lunarwing-weechat-${name}" start || return 1
      rc-service "lunarwing-weechat-adapter-${name}" start || return 1
      say "enabled + started lunarwing-weechat{,-adapter}-${name} (OpenRC)"
      ;;
    *)
      say "weechat lifecycle: unsupported INIT_SYSTEM=$INIT_SYSTEM" >&2
      return 1
      ;;
  esac
}

weechat_reconcile_units() {
  local name="$1"
  case "$INIT_SYSTEM" in
    systemd)
      (render_tenant_systemd_units "$name") || return 1
      _systemctl_user "$name" daemon-reload || return 1
      ;;
    openrc)
      (render_tenant_openrc_units "$name") || return 1
      ;;
    *)
      say "weechat lifecycle: unsupported INIT_SYSTEM=$INIT_SYSTEM" >&2
      return 1
      ;;
  esac
}

set_tenant_weechat_enabled() {
  local name="$1" enabled="$2"
  with_tenant_lifecycle_lock "$name" _set_tenant_weechat_enabled_locked "$name" "$enabled"
}

_set_tenant_weechat_enabled_locked() {
  local name="$1" enabled="$2"
  case "$enabled" in true|false) ;; *) return 2 ;; esac

  ensure_init_system || return 1
  if [[ "$enabled" == "false" ]]; then
    # Stop first: health must not mask a unit until init confirms that its
    # process and boot persistence are gone.
    weechat_lifecycle_stop_disable "$name" || return 1
    ports_set_weechat "$name" false || return 1
    if ! weechat_reconcile_units "$name"; then
      # Reconciliation failed after the flag changed. Restore fail-loud health
      # semantics and make a best effort to restore the prior unit graph.
      say "disabled WeeChat unit reconciliation failed; restoring enabled flag" >&2
      ports_set_weechat "$name" true \
        || say "WARNING: failed to restore enable_weechat=true for '$name'" >&2
      weechat_reconcile_units "$name" \
        || say "WARNING: failed to restore enabled WeeChat units for '$name'" >&2
      return 1
    fi
    return 0
  fi

  # Enabled is fail-loud: health remains critical if rendering or startup fails.
  ports_set_weechat "$name" true || return 1
  weechat_reconcile_units "$name" || return 1
  weechat_lifecycle_enable_start "$name" || return 1
}

# Stop only services that can mutate a migration snapshot. PostgreSQL remains
# running so export can take a consistent pg_dump after quiescence.
tenant_writer_services() {
  local name="$1"
  printf '%s\n' "lunarwing-${name}" "xmpp-bridge-${name}"
  tenant_proxy_enabled "$name" && printf '%s\n' "lunarwing-proxy-${name}"
  if tenant_weechat_enabled "$name"; then
    printf '%s\n' "lunarwing-weechat-adapter-${name}" "lunarwing-weechat-${name}"
  fi
  if tenant_darkirc_enabled "$name"; then
    printf '%s\n' "lunarwing-darkirc-adapter-${name}" "lunarwing-darkirc-${name}"
  fi
  [[ "$INIT_SYSTEM" != openrc ]] || printf '%s\n' "lunarwing-vision-${name}-sup"
  printf '%s\n' "lunarwing-vision-${name}"
  local worker
  for worker in pebble opencode kimi; do
    if tenant_worker_enabled "$name" "$worker"; then
      printf '%s\n' "lunarwing-${worker}-${name}"
      [[ "$INIT_SYSTEM" != openrc ]] || printf '%s\n' "lunarwing-${worker}-${name}-sup"
    fi
  done
}

tenant_writer_containers() {
  local name="$1" worker
  for worker in pebble opencode kimi vision; do
    printf 'lunarwing-%s-%s\n' "$worker" "$name"
  done
}

container_name_list_has() {
  local names="$1" expected="$2" current
  while IFS= read -r current; do
    [[ "$current" == "$expected" ]] && return 0
  done <<<"$names"
  return 1
}

stop_tenant_writers() {
  local name="$1" service state rc worker container names running writer_rc
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    while IFS= read -r service; do
      rc=0
      state="$(_systemctl_user "$name" is-active "${service}.service" 2>/dev/null)" || rc=$?
      case "$state" in
        active|activating|reloading|deactivating)
          _systemctl_user "$name" stop "${service}.service" >/dev/null \
            || die "failed to stop writer ${service}.service" ;;
        inactive|dead|failed|maintenance) ;;
        unknown) [[ "$rc" -eq 4 ]] || die "could not inspect writer ${service}.service" ;;
        *) [[ "$rc" -eq 4 ]] || die "could not classify writer ${service}.service state '$state'" ;;
      esac
    done < <(tenant_writer_services "$name")
  else
    while IFS= read -r service; do
      [[ -x "$OPENRC_INITD_DIR/$service" ]] || continue
      rc-service "$service" stop >/dev/null 2>&1 \
        || rc-service "$service" status 2>&1 | grep -Eqi 'stopped|inactive|dead' \
        || die "failed to stop writer $service"
    done < <(tenant_writer_services "$name")
  fi

  for worker in pebble opencode kimi; do
    stop_worker "$name" "$worker"
  done
  stop_tenant_vision "$name"

  ensure_container_runtime
  names="$(_ctr "$name" ps -a --format '{{.Names}}' 2>/dev/null)" \
    || die "could not inspect tenant container store"
  while IFS= read -r container; do
    container_name_list_has "$names" "$container" || continue
    running="$(_ctr "$name" inspect -f '{{.State.Running}}' "$container" 2>/dev/null)" \
      || die "could not inspect writer container $container"
    case "$running" in
      true) _ctr "$name" stop --time 0 "$container" >/dev/null \
        || die "failed to stop writer container $container" ;;
      false) ;;
      *) die "could not classify writer container $container" ;;
    esac
  done < <(tenant_writer_containers "$name")

  writer_rc=0
  tenant_writers_active "$name" || writer_rc=$?
  case "$writer_rc" in
    0) die "tenant '$name' still has active writers after stop" ;;
    1) ;;
    *) die "could not verify writer quiescence for tenant '$name'" ;;
  esac
  say "tenant writers stopped for $name (PostgreSQL left running)"
}

# Returns 0 when any writer is active, 1 when all are stopped/absent, and 2
# when state cannot be classified safely.
tenant_writers_active() {
  local name="$1" service state rc container running names
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || return 2
  ensure_init_system || return 2
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    while IFS= read -r service; do
      rc=0
      state="$(_systemctl_user "$name" is-active "${service}.service" 2>/dev/null)" || rc=$?
      case "$state" in
        active|activating|reloading|deactivating) return 0 ;;
        inactive|dead|failed|maintenance) ;;
        unknown) [[ "$rc" -eq 4 ]] || return 2 ;;
        *) [[ "$rc" -eq 4 ]] || return 2 ;;
      esac
    done < <(tenant_writer_services "$name")
  else
    while IFS= read -r service; do
      [[ -x "$OPENRC_INITD_DIR/$service" ]] || continue
      rc=0
      state="$(rc-service "$service" status 2>&1)" || rc=$?
      case "$state" in
        *stopped*|*inactive*|*dead*) ;;
        *started*|*running*|*active*) return 0 ;;
        *) [[ "$rc" -eq 3 ]] || return 2 ;;
      esac
    done < <(tenant_writer_services "$name")
  fi

  ensure_container_runtime || return 2
  names="$(_ctr "$name" ps -a --format '{{.Names}}' 2>/dev/null)" || return 2
  while IFS= read -r container; do
    container_name_list_has "$names" "$container" || continue
    running="$(_ctr "$name" inspect -f '{{.State.Running}}' "$container" 2>/dev/null)" \
      || return 2
    [[ "$running" != true ]] || return 0
    [[ "$running" == false ]] || return 2
  done < <(tenant_writer_containers "$name")
  return 1
}

tenant_writers_probe_status() {
  local rc=0
  tenant_writers_active "$1" || rc=$?
  case "$rc" in
    0) return 0 ;;
    1) return 10 ;;
    *) return 2 ;;
  esac
}

# Diagnostic-only in Stage A. Both init-managed services and authenticated
# adapter connectivity must be healthy; this does not activate an exchange.
darkirc_health_strict() {
  local name="$1" attempts="${2:-10}" quiet="${3:-false}"
  local adapter_port env_path secret response attempt
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || return 1
  tenant_darkirc_enabled "$name" || return 1
  ensure_init_system

  if [[ "$INIT_SYSTEM" == systemd ]]; then
    _systemctl_user "$name" is-active --quiet "lunarwing-darkirc-${name}.service" \
      || return 1
    _systemctl_user "$name" is-active --quiet "lunarwing-darkirc-adapter-${name}.service" \
      || return 1
  else
    rc-service "lunarwing-darkirc-${name}" status >/dev/null 2>&1 || return 1
    rc-service "lunarwing-darkirc-adapter-${name}" status >/dev/null 2>&1 || return 1
  fi

  adapter_port="$(ports_get "$name" darkirc_adapter 2>/dev/null || true)"
  [[ "$adapter_port" =~ ^[0-9]+$ ]] || return 1
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  secret="$(read_tenant_env_value \
    "$name" "$env_path" DARKIRC_ADAPTER_SECRET 2>/dev/null)" || return 1
  [[ -n "$secret" ]] || return 1
  require_cmd curl
  require_cmd jq

  for ((attempt = 1; attempt <= attempts; attempt++)); do
    response="$(printf 'Authorization: Bearer %s\n' "$secret" | \
      curl -fsS --max-time 2 --header @- \
        "http://127.0.0.1:${adapter_port}/health" 2>/dev/null || true)"
    if jq -e '(.status == "ok") and (.irc_connected == true)' <<<"$response" \
      >/dev/null 2>&1; then
      [[ "$quiet" == true ]] || say "strict DarkIRC health passed for $name"
      return 0
    fi
    [[ "$attempt" -lt "$attempts" ]] && sleep 1
  done
  say "strict DarkIRC health failed for $name" >&2
  return 1
}

add_tenant() {
  local name="$1"
  local docker_group="${2:-false}"
  local xmpp_jid="${3:-}"   # resolved below (preserve an existing one on re-run)
  local xmpp_password="${4:-}"
  local tensorzero_url="${5:-$DEFAULT_TENSORZERO_URL}"
  local gotify_url="${6:-$DEFAULT_GOTIFY_URL}"
  local gotify_title="${7:-$DEFAULT_GOTIFY_TITLE}"
  local llm_api_key="${8:-}"
  local llm_base_url="${9:-$DEFAULT_LLM_BASE_URL}"
  local enable_darkirc="${10:-false}"
  local enable_proxy="${11:-false}"
  local llm_model="${12:-}"
  local gateway_host="${13:-}"
  local xmpp_allow_from="${14:-}"
  local opencode_model="${15:-}"
  local opencode_base_url="${16:-}"
  # External worker selection (default off). Persisted into the port registry so
  # start_tenant_<worker> starts only what this tenant chose, not every worker
  # whose shared host image happens to exist. See PER_TENANT_WORKER_GATING.md.
  local with_pebble="${17:-false}"
  local with_opencode="${18:-false}"
  local with_kimi="${19:-false}"
  local kimi_model="${20:-}"
  local kimi_base_url="${21:-}"
  local kimi_api_key="${22:-}"
  local darkirc_scope_override="${23:-}"
  local with_study_mcp="${24:-false}"
  local study_mcp_home="${25:-}"

  name="$(sanitize_name "$name")"
  [[ -n "$name" ]] || die "invalid tenant name"
  # A tenant whose name begins with a reserved per-service prefix would make its
  # primary daemon unit (lunarwing-<name>) collide with another tenant's
  # per-service unit — e.g. tenant 'pebble-1' -> lunarwing-pebble-1, byte-identical
  # to tenant '1's pebble worker unit (lunarwing-pebble-1). That collision is
  # undisambiguatable downstream (health-openrc.sh's started-gate would mis-key the
  # unit and could mask a real outage as `skipped`), so forbid such names at the
  # source. (weechat-* also covers weechat-adapter-*.)
  case "$name" in
    pg-*|proxy-*|nanocode-*|pebble-*|opencode-*|kimi-*|weechat-*)
      die "tenant name '$name' collides with a reserved per-service unit prefix (pg-/proxy-/nanocode-/pebble-/opencode-/kimi-/weechat-/weechat-adapter-); choose another name" ;;
  esac

  # Resolve after sanitize_name so a preserved/default JID keys off the real name.
  local xmpp_jid_prev
  xmpp_jid_prev="$(_env_existing "$(tenant_env_dir "$name")/lunarwing.env" XMPP_JID)"
  xmpp_jid="$(resolve_tenant_xmpp_jid "$name" "$xmpp_jid")"

  say "=== Adding tenant: $name ==="
  say ""
  if [[ -n "$xmpp_jid_prev" && "$xmpp_jid" == "$xmpp_jid_prev" ]]; then
    say "preserving existing XMPP JID: $xmpp_jid"
  elif [[ -n "$xmpp_jid_prev" && "$xmpp_jid" != "$xmpp_jid_prev" ]]; then
    say "WARNING: changing XMPP JID: $xmpp_jid_prev -> $xmpp_jid (bridge restart required)" >&2
  fi

  ports_registry_init
  local base_port workers_json
  workers_json="$(printf '{"nanocode":false,"pebble":%s,"opencode":%s,"kimi":%s}' \
    "$with_pebble" "$with_opencode" "$with_kimi")"
  base_port="$(ports_allocate "$name" "$enable_darkirc" "$enable_proxy" "$workers_json" "$darkirc_scope_override")"
  say ""

  create_tenant_user "$name" "$docker_group"
  say ""

  warn_if_adapter_deps_missing "$name"

  clone_tenant_repo "$name"
  say ""

  retire_tenant_nanocode "$name" \
    || die "failed to retire legacy Nanocode resources for tenant '$name'"
  say ""

  say "--- Generating environment files ---"
  write_tenant_vision_env "$name" >/dev/null
  write_tenant_voice_env "$name" >/dev/null
  write_tenant_lunarwing_env "$name" "$xmpp_jid" "$xmpp_password" "$tensorzero_url" "$llm_api_key" "$llm_base_url" "$llm_model" "$gateway_host" "$xmpp_allow_from" "$opencode_model" "$opencode_base_url" "$kimi_model" "$kimi_base_url" "$kimi_api_key"
  write_tenant_bridge_env "$name" "$xmpp_jid" "$xmpp_password" "$xmpp_allow_from"
  if [[ "$enable_proxy" == "true" ]]; then
    write_tenant_proxy_env "$name" "$tensorzero_url"
  fi
  if [[ "$enable_darkirc" == "true" ]]; then
    ensure_darkirc_scope_id "$name" >/dev/null
    write_tenant_darkirc_adapter_env "$name"
    if darkirc_resolve_release; then
      generate_darkirc_config "$name"
    else
      say "DarkIRC config deferred until build-darkirc installs the attested helper"
    fi
  fi
  write_tenant_gotify_config "$name" "$gotify_url" "$gotify_title"
  ensure_external_worker_config "$name" "pebble" "pebble_wss"
  ensure_external_worker_config "$name" "opencode" "opencode_wss"
  ensure_external_worker_config "$name" "kimi" "kimi_wss"

  # WeeChat relay auto-bootstrap: write a dedicated minimal weechat.env, then
  # attempt a one-shot relay configuration. Non-fatal: a degraded WeeChat setup
  # must not strand the base tenant. Recovery: configure-weechat-relay <name>.
  local weechat_bootstrap_ok="configured"
  if [[ "$WEECHAT_BOOTSTRAP_OPT_OUT" == "true" ]]; then
    local relay_password
    relay_password="$(_read_env_value "$(tenant_env_dir "$name")/lunarwing.env" RELAY_PASSWORD)"
    if [[ -n "$relay_password" ]] && _write_weechat_env "$name" "$relay_password"; then
      weechat_bootstrap_ok="disabled (--no-weechat-bootstrap)"
    else
      weechat_bootstrap_ok="disabled (credential env setup failed)"
      say "WARNING: WeeChat bootstrap was disabled, but the minimal credential env could not be written." >&2
    fi
  elif ! configure_weechat_relay "$name" 2>&1; then
    weechat_bootstrap_ok="needs recovery"
    say ""
    say "WARNING: WeeChat relay auto-bootstrap failed for tenant '$name'."
    say "         WeeChat will start but the relay is not configured."
    say "         Recovery: sudo $0 configure-weechat-relay $name"
    say ""
  fi

  # SSH harness: config.toml [[ssh.hosts]] block + ed25519 key pair.
  # Enabled by default; opt out with --no-ssh or LUNARWING_MT_SSH_ENABLED=false.
  if [[ "$DEFAULT_SSH_ENABLED" == "true" && "$SSH_OPT_OUT" != "true" ]]; then
    say "--- Provisioning SSH harness ---"
    ensure_ssh_config "$name"
    provision_tenant_ssh_key "$name"
    warn_if_sshd_unreachable "$name"
  else
    say "SSH harness: disabled (enabled=$DEFAULT_SSH_ENABLED, opt-out=$SSH_OPT_OUT)"
  fi

  # Study Notes MCP (lw2studymcp): opt-in self-knowledge bootstrap. The shared
  # host checkout is always synced to the latest upstream before wiring, so the
  # new tenant starts with current knowledge of the LunarWing codebase.
  # Non-fatal: a sync/checkout problem must not strand the base tenant.
  local study_mcp_bootstrap_ok="disabled (pass --with-study-mcp to enable)"
  if [[ "$with_study_mcp" == "true" ]]; then
    say "--- Configuring Study Notes MCP (latest lw2studymcp) ---"
    if provision_tenant_study_mcp "$name" "$study_mcp_home"; then
      study_mcp_bootstrap_ok="configured (latest lw2studymcp checkout)"
    else
      study_mcp_bootstrap_ok="needs recovery"
      say "WARNING: Study Notes MCP auto-configuration failed for tenant '$name'." >&2
      say "         Recovery: sudo $0 configure-study-mcp $name --sync" >&2
    fi
    say ""
  fi

  if ! "$CONTAINER_RT" image inspect "$VISION_SIDECAR_IMAGE" &>/dev/null; then
    build_vision_sidecar_image || true
  fi
  say ""

  say "--- Starting PostgreSQL ---"
  start_tenant_postgres "$name"
  say ""

  ensure_init_system
  say "--- Rendering $INIT_SYSTEM services ---"
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    render_tenant_systemd_units "$name"
  else
    render_tenant_openrc_units "$name"
  fi
  say ""

  # Pre-delegated cgroup v2 subtree (docs/ops/PREDELEGATED_SUBTREE_CGROUPS.md):
  # contains tenant login sessions and hosts the per-build cgroup. Warn-only:
  # the build path bounds without it, and `setup-cgroups` can repair it later.
  if [[ "$INIT_SYSTEM" != systemd* ]]; then
    say "--- Setting up pre-delegated cgroup v2 subtree ---"
    if ! setup_tenant_cgroups "$name"; then
      say "WARNING: cgroup subtree setup failed for '$name' (recovery: sudo $0 setup-cgroups)" >&2
    fi
    say ""
  fi

  # Host-global health-check + self-heal pipeline (auto-covers every tenant).
  if [[ "$DEFAULT_HEALTH_ENABLED" == "true" && "$HEALTH_OPT_OUT" != "true" ]]; then
    ensure_health_pipeline
  else
    say "health pipeline: disabled (enabled=$DEFAULT_HEALTH_ENABLED, opt-out=$HEALTH_OPT_OUT)"
  fi
  say ""

  say "=== Tenant '$name' added ==="
  say ""
  say "Port block: $base_port-$((base_port + PORT_BLOCK_SIZE - 1))"
  say "  gateway:          $(ports_get "$name" gateway)"
  say "  http:             $(ports_get "$name" http)"
  say "  bridge:           $(ports_get "$name" bridge)"
  say "  postgres:         $(ports_get "$name" postgres)"
  say "  proxy:            $(ports_get "$name" proxy)"
  say "  weechat:          $(ports_get "$name" weechat)"
  say "  orchestrator:     $(ports_get "$name" orchestrator)"
  say "  nanocode_wss:     $(ports_get "$name" nanocode_wss) (reserved legacy slot)"
  say "  pebble_wss:       $(ports_get "$name" pebble_wss)"
  say "  opencode_wss:     $(ports_get "$name" opencode_wss)"
  say "  kimi_wss:         $(ports_get "$name" kimi_wss)"
  say "  weechat_adapter:  $(ports_get "$name" weechat_adapter)"
  say "  darkirc:          $( [[ "$enable_darkirc" == "true" ]] && echo "enabled" || echo "disabled (pass --enable-darkirc to enable)" )"
  say "  proxy:            $( [[ "$enable_proxy" == "true" ]] && echo "enabled" || echo "disabled (pass --enable-proxy to enable)" )"
  say "  workers:          $( _selected="$( [[ "$with_pebble" == "true" ]] && printf 'pebble '; [[ "$with_opencode" == "true" ]] && printf 'opencode '; [[ "$with_kimi" == "true" ]] && printf 'kimi ' )"; [[ -n "$_selected" ]] && echo "${_selected% }" || echo "none (pass --with-pebble/--with-opencode/--with-kimi to select)" )"
  say "  ssh:              $( [[ "$DEFAULT_SSH_ENABLED" == "true" && "$SSH_OPT_OUT" != "true" ]] && echo "enabled (key upload + activation handled by start-tenant)" || echo "disabled (pass --no-ssh)" )"
  say "  weechat relay:    $weechat_bootstrap_ok"
  say "  study mcp:        $study_mcp_bootstrap_ok"
  say ""
  say "Next steps:"
  say "  sudo $0 build-tenant $name --with-wasm"
  say "  sudo $0 start-tenant $name"
}

add_tenants() {
  local names_csv="$1"
  shift

  local IFS=','
  local names_array
  read -ra names_array <<< "$names_csv"

  for raw_name in "${names_array[@]}"; do
    local name
    name="$(sanitize_name "$(echo "$raw_name" | xargs)")"
    [[ -n "$name" ]] || continue
    say ""
    add_tenant "$name" "$@"
  done
}

remove_tenant() {
  local name="$1"
  local purge="${2:-false}"

  name="$(sanitize_name "$name")"
  say "=== Removing tenant: $name ==="
  say ""

  retire_tenant_nanocode "$name" \
    || die "failed to retire legacy Nanocode resources for tenant '$name'; refusing removal"

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    stop_tenant_systemd "$name"
    uninstall_tenant_systemd "$name"
  else
    stop_tenant_openrc "$name"
    uninstall_tenant_openrc "$name"
  fi
  say ""

  stop_tenant_postgres "$name"
  if [[ "$purge" == "true" ]]; then
    reset_tenant_postgres "$name"
  fi
  say ""

  ports_deallocate "$name"
  say ""

  remove_tenant_user "$name" "$purge"
  say ""

  # Drop the pre-delegated cgroup subtree and re-render the boot-time setup
  # without the removed tenant (only when the host assets were ever installed).
  if [[ "$INIT_SYSTEM" != systemd* ]]; then
    _openrc_tenant_subtree_remove "$name" || true
    if [[ -f "$(_openrc_cgroups_locald_path)" ]]; then
      _openrc_cgroups_locald_render \
        || say "WARNING: could not re-render boot-time cgroup setup" >&2
    fi
    say ""
  fi

  # If that was the last tenant, retire the host-global health pipeline schedule.
  if [[ -z "$(all_tenant_names)" ]]; then
    remove_health_pipeline
    say ""
  fi

  say "=== Tenant '$name' removed ==="
}

# Re-render a tenant's service units from the current generator WITHOUT touching
# secrets/env or restarting anything — for applying a generator change (e.g. an
# updated init-script status()) to an already-provisioned tenant. The command
# daemon-reloads once after all systemd assets are written; OpenRC reads scripts
# per invocation. A status()/health change takes effect immediately; a change to
# the run command needs a restart.
render_tenant_units() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  with_tenant_lifecycle_lock "$name" _render_tenant_units_locked "$name" \
    || die "failed to re-render units for tenant '$name'"
}

_render_tenant_units_locked() {
  local name="$1"
  _retire_tenant_nanocode "$name" || return 1
  ensure_init_system
  say "--- Re-rendering $INIT_SYSTEM units for $name ---"
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    render_voice_systemd_assets "$name" || return 1
    render_tenant_systemd_units "$name" || return 1
    _systemctl_user "$name" daemon-reload >/dev/null || return 1
  else
    render_voice_openrc_assets "$name" || return 1
    render_tenant_openrc_units "$name"
  fi
  say "units re-rendered for '$name' (services NOT restarted)."
  say "run-command changes need a restart to apply: $0 restart-tenant $name"
}

# Print a post-start SSH readiness block sourced from the live API. Warn-only:
# a missing/failed API must never fail start-tenant.
_ssh_ready_summary() {
  local name="$1" http_port status keys hosts
  hosts="$(_ssh_hosts_from_config "$name" | paste -sd, -)"
  [[ -n "$hosts" ]] || return 0  # SSH not configured for this tenant

  http_port="$(ports_get "$name" http)"
  status="$(curl -sf --max-time 3 "http://127.0.0.1:${http_port}/agent/status" 2>/dev/null)" || status=""
  keys="$(jq -r '.data.keys_loaded // "?"' <<<"$status" 2>/dev/null)" || keys="?"

  say ""
  say "--- SSH readiness ---"
  say "  hosts:        $hosts"
  if [[ "$keys" =~ ^[0-9]+$ ]] && ((keys >= 1)); then
    say "  agent:        running, $keys key(s) loaded — ssh/ssh_git tools ready"
  else
    say "  agent:        keys_loaded=$keys — if a key upload just failed, re-run '$0 start-tenant $name'"
  fi
  if [[ -f "$(tenant_state_dir "$name")/tools/ssh-tool.wasm" ]]; then
    say "  wasm ssh:     installed (activate it in the web panel: Settings → Extensions → ssh)"
  fi
  say "  verify:       curl -s http://127.0.0.1:${http_port}/agent/status | jq"
}

# Poll the authenticated tenant gateway until reachable (up to ~30s).
_wait_tenant_gateway() {
  local name="$1" env_path gateway_port gateway_token auth_header i=0
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  gateway_port="$(ports_get "$name" gateway)"
  gateway_token="$(_read_env_value "$env_path" GATEWAY_AUTH_TOKEN)"
  [[ -n "$gateway_token" ]] || return 1
  auth_header="Authorization: Bearer ${gateway_token}"
  while ! curl -sf --max-time 2 \
    -K <(printf 'header = "%s"\n' "$auth_header") \
    "http://127.0.0.1:${gateway_port}/api/gateway/status" >/dev/null 2>&1; do
    i=$((i + 1))
    [[ $i -lt 15 ]] || return 1
    sleep 2
  done
  return 0
}

_start_tenant_init_services() {
  local name="$1"
  ensure_init_system || return 1
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    start_tenant_systemd "$name"
  else
    start_tenant_openrc "$name"
  fi
}

# Restart ONLY the lunarwing daemon unit for a tenant (not the full stack).
# Used by start_tenant to make a freshly-uploaded SSH key signable: the agent
# loads keys from the secrets store at startup only ("runtime key add is
# status-only" — see docs/architecture/SSH_AGENT_HARNESS.md §7).
_restart_tenant_daemon() {
  local name="$1"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    _systemctl_user "$name" restart "lunarwing-${name}.service"
  else
    rc-service "lunarwing-${name}" restart
  fi
}

start_tenant() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  say "=== Starting tenant: $name ==="

  local darkirc_config
  darkirc_config="$(tenant_state_dir "$name")/darkirc/darkirc_config.toml"
  if tenant_darkirc_enabled "$name"; then
    darkirc_load_compatibility \
      || die "attested DarkIRC release is unavailable; run ensure-darkirc-release $name"
    darkirc_service_uses_release_selector "$name" \
      || die "DarkIRC service unit is stale or unsafe; run render-units $name"
    if [[ ! -f "$darkirc_config" || -L "$darkirc_config" ]]; then
      say "finalizing deferred DarkIRC config before start ..."
      generate_darkirc_config "$name"
    fi
  fi

  retire_tenant_nanocode "$name" \
    || die "failed to retire legacy Nanocode resources for tenant '$name'; refusing start"

  # Safety net: if the tenant has LUNARWING_OWNER_ID set but the DB still has
  # orphaned 'default'-scoped rows (e.g. an upgrade that didn't run patch-env),
  # migrate them before starting the daemon so no data is orphaned.
  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  if grep -q '^LUNARWING_OWNER_ID=' "$env_path" 2>/dev/null && _owner_scope_needs_migration "$name"; then
    say "found orphaned 'default'-scoped DB data; auto-migrating to '$name' scope"
    migrate_owner_scope "$name" || say "WARNING: owner-scope migration failed (run 'migrate-owner-scope $name' manually)"
  fi

  # Pre-create the SSH agent socket path so podman can bind-mount it into
  # worker containers. The daemon creates the actual Unix socket here at
  # startup; without a pre-existing path, podman would create it as a
  # directory (breaking the daemon's socket bind). A touch-file is safe —
  # the daemon removes it and binds the real socket.
  local ssh_socket_path
  ssh_socket_path="$(tenant_run_dir "$name")/ssh-agent.sock"
  if [[ ! -S "$ssh_socket_path" ]]; then
    sudo -u "$name" mkdir -p "$(tenant_run_dir "$name")" 2>/dev/null || true
    sudo -u "$name" touch "$ssh_socket_path" 2>/dev/null || true
  fi

  # Start the daemon BEFORE the workers so the SSH agent socket exists when
  # the worker containers are created (podman bind-mounts the file at creation
  # time; if the socket doesn't exist yet, the mount is a stale touch-file).
  # The daemon's SSH agent creates the real Unix socket at
  # <run_dir>/ssh-agent.sock, which the workers bind-mount.
  start_tenant_postgres "$name"
  start_tenant_vision "$name"
  start_tenant_voice "$name"

  with_tenant_lifecycle_lock "$name" _start_tenant_init_services "$name" \
    || die "failed to start init services for tenant '$name'"

  # Upload the staged SSH key to the secrets store (if one was provisioned by
  # add-tenant but not yet uploaded), BEFORE the workers start. If a key was
  # actually ingested, bounce the daemon once so the agent loads it (keys are
  # only read from the secrets store at startup); the workers then bind-mount
  # the post-bounce socket inode, so they are never left on a stale socket.
  local staged_key
  staged_key="$(tenant_env_dir "$name")/ssh_key_staged"
  warn_if_sshd_unreachable "$name"
  if [[ -f "$staged_key" ]]; then
    upload_tenant_ssh_key "$name" || true
    if [[ ! -f "$staged_key" ]]; then
      # Upload succeeded (upload_tenant_ssh_key deletes the staged file).
      say "restarting lunarwing-${name} so the SSH agent loads the new key ..."
      if _restart_tenant_daemon "$name"; then
        if _wait_tenant_gateway "$name"; then
          say "lunarwing-${name} restarted; SSH key active"
        else
          say "WARNING: gateway not reachable after SSH-key restart (check 'status $name')" >&2
        fi
      else
        say "WARNING: daemon restart failed after SSH key upload; run '$0 restart-tenant $name' manually" >&2
      fi
    fi
  fi

  # Seed the optional DarkIRC adapter secret into the encrypted secrets store
  # so the WASM channel authenticates outbound adapter requests. Best-effort:
  # a failed upload warns and continues; the WASM channel refreshes the credential
  # live from the store, so no daemon restart is required.
  upload_tenant_darkirc_secret "$name" || true

  # Patch WeeChat channel caps with the tenant's allocated ports. This ensures
  # the ports are correct even if install-wasm wasn't run or the caps were
  # manually edited. Runs before workers start so the channel connects immediately.
  patch_weechat_caps "$name"

  # Workers start AFTER the daemon (and after any SSH-key bounce) so the SSH
  # agent socket is already a real, current Unix socket when podman bind-mounts it.
  #
  # Gate each worker on the tenant's persisted selection (add-tenant --with-*).
  # Without this, start_tenant_<worker> would start every worker whose SHARED
  # host image happens to exist — regardless of what this tenant chose — because
  # its only guards are "port allocated" (always true) and "image present"
  # (host-wide). See tenant_worker_enabled / PER_TENANT_WORKER_GATING.md.
  local _w
  for _w in pebble opencode kimi; do
    if tenant_worker_enabled "$name" "$_w" \
        && { [[ "$INIT_SYSTEM" != "openrc" ]] || tenant_worker_desired_running "$name" "$_w"; }; then
      "start_tenant_${_w}" "$name"
    elif [[ "$INIT_SYSTEM" == "openrc" ]] && tenant_worker_enabled "$name" "$_w"; then
      say "$_w worker intentionally stopped for $name (skipping; use 'start-worker $name $_w')"
    else
      say "$_w worker not selected for $name (skipping; enable with 'add-tenant $name --with-$_w')"
    fi
  done
  _ssh_ready_summary "$name" || true
}

stop_tenant() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  say "=== Stopping tenant: $name ==="

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    stop_tenant_systemd "$name"
  else
    stop_tenant_openrc "$name"
  fi

  stop_tenant_vision "$name"
  stop_tenant_voice "$name"
  stop_tenant_pebble "$name"
  stop_tenant_opencode "$name"
  stop_tenant_kimi "$name"
  stop_tenant_postgres "$name"

  # Shutdown must not be blocked by malformed legacy config or an unsafe
  # rollback path. Retire Nanocode only after every supported service is down.
  retire_tenant_nanocode "$name" \
    || die "tenant '$name' stopped, but legacy Nanocode retirement failed"
}

# External upgrade wrappers use this boundary so migration readiness cannot race
# another DarkIRC writer between the check and the completed tenant stop.
stop_tenant_for_upgrade() {
  local name="$1" scope_id result=0 writer_lock_owned=false
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  if tenant_darkirc_enabled "$name"; then
    if [[ "$DARKIRC_WRITER_LOCK_HELD" != true ]]; then
      darkirc_writer_lock "$name"
      writer_lock_owned=true
    fi
    scope_id="$(darkirc_scope_id "$name")"
    validate_darkirc_scope_id "$scope_id"
    run_darkirc_key_helper "$name" migration-ready \
      --tenant "$name" --scope-id "$scope_id" --json >/dev/null \
      || die "DarkIRC migration state is not settled; refusing upgrade"
  fi

  stop_tenant "$name" || result=$?
  if [[ "$writer_lock_owned" == true ]]; then
    darkirc_writer_unlock
  fi
  return "$result"
}

# Machine export runs as a child so its existing passphrase and archive handling
# stay in export-tenant.sh. Both inherited lock descriptors remain open until that
# child has validated readiness, stopped/verified writers, pinned DarkIRC state,
# and sealed the final bundle.
migration_export_child() {
  local name="$1"
  shift
  local -a inherited_env=(
    "LUNARWING_MT_EXPORT_BOUNDARY_TENANT=$name"
    "LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_FD=$LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD"
    "LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_TENANT=$name"
  )
  if [[ "$DARKIRC_WRITER_LOCK_HELD" == true ]]; then
    inherited_env+=(
      "LUNARWING_MT_INHERITED_DARKIRC_LOCK_FD=$DARKIRC_WRITER_LOCK_FD"
      "LUNARWING_MT_INHERITED_DARKIRC_LOCK_SCOPE=$DARKIRC_WRITER_SCOPE"
    )
  fi
  env "${inherited_env[@]}" "$SCRIPT_DIR/export-tenant.sh" "$name" "$@"
}

migration_export_darkirc_locked() {
  local name="$1" scope_id
  shift
  scope_id="$(darkirc_scope_id "$name")"
  validate_darkirc_scope_id "$scope_id"
  run_darkirc_key_helper "$name" migration-ready \
    --tenant "$name" --scope-id "$scope_id" --json >/dev/null \
    || die "DarkIRC migration state is not settled; refusing export"
  migration_export_child "$name" "$@"
}

migration_export_boundary_locked() {
  local name="$1"
  shift
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  if tenant_darkirc_enabled "$name"; then
    with_darkirc_writer_lock "$name" migration_export_darkirc_locked "$name" "$@"
  else
    migration_export_child "$name" "$@"
  fi
}

migration_export_boundary() {
  local name="$1"
  shift
  name="$(sanitize_name "$name")"
  with_tenant_lifecycle_lock "$name" migration_export_boundary_locked "$name" "$@"
}

upgrade_wrapper_child() {
  local name="$1"
  shift
  local -a inherited_env=(
    "LUNARWING_MT_UPGRADE_BOUNDARY_TENANT=$name"
    "LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_FD=$LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD"
    "LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_TENANT=$name"
    "LUNARWING_MT_INHERITED_UPGRADE_LOCK_FD=$LUNARWING_MT_ACTIVE_UPGRADE_LOCK_FD"
  )
  if [[ "$DARKIRC_WRITER_LOCK_HELD" == true ]]; then
    inherited_env+=(
      "LUNARWING_MT_INHERITED_DARKIRC_LOCK_FD=$DARKIRC_WRITER_LOCK_FD"
      "LUNARWING_MT_INHERITED_DARKIRC_LOCK_SCOPE=$DARKIRC_WRITER_SCOPE"
    )
  fi
  env "${inherited_env[@]}" "$SCRIPT_DIR/upgrade-tenant.sh" "$@"
}

upgrade_wrapper_boundary_locked() {
  local name="$1" upgrade_lock_fd result=0
  shift
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  exec {upgrade_lock_fd}>"${BUILD_LOCK}.${name}.upgrade" \
    || die "failed to open tenant upgrade lock"
  flock -x "$upgrade_lock_fd" || die "failed to acquire tenant upgrade lock"
  LUNARWING_MT_ACTIVE_UPGRADE_LOCK_FD="$upgrade_lock_fd"
  if tenant_darkirc_enabled "$name"; then
    with_darkirc_writer_lock "$name" upgrade_wrapper_child "$name" "$@" || result=$?
  else
    upgrade_wrapper_child "$name" "$@" || result=$?
  fi
  unset LUNARWING_MT_ACTIVE_UPGRADE_LOCK_FD
  flock -u "$upgrade_lock_fd" || result=1
  exec {upgrade_lock_fd}>&-
  return "$result"
}

upgrade_wrapper_boundary() {
  local name="$1"
  shift
  name="$(sanitize_name "$name")"
  with_tenant_lifecycle_lock "$name" upgrade_wrapper_boundary_locked "$name" "$@"
}

validate_upgrade_wrapper_boundary_locked() {
  local name="$1" upgrade_fd="${LUNARWING_MT_INHERITED_UPGRADE_LOCK_FD:-}"
  [[ "${LUNARWING_MT_UPGRADE_BOUNDARY_TENANT:-}" == "$name" ]] \
    || die "upgrade wrapper boundary tenant marker mismatch"
  inherited_lock_fd_is_held "$upgrade_fd" "${BUILD_LOCK}.${name}.upgrade" \
    || die "invalid inherited tenant upgrade lock"
  if tenant_darkirc_enabled "$name"; then
    with_darkirc_writer_lock "$name" true
  elif [[ -n "${LUNARWING_MT_INHERITED_DARKIRC_LOCK_FD:-}${LUNARWING_MT_INHERITED_DARKIRC_LOCK_SCOPE:-}" ]]; then
    die "unexpected inherited DarkIRC writer lock for disabled tenant '$name'"
  fi
}

validate_upgrade_wrapper_boundary() {
  local name="$1"
  name="$(sanitize_name "$name")"
  with_tenant_lifecycle_lock "$name" validate_upgrade_wrapper_boundary_locked "$name"
}

validate_migration_export_boundary_locked() {
  local name="$1"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  if tenant_darkirc_enabled "$name"; then
    with_darkirc_writer_lock "$name" true
  elif [[ -n "${LUNARWING_MT_INHERITED_DARKIRC_LOCK_FD:-}${LUNARWING_MT_INHERITED_DARKIRC_LOCK_SCOPE:-}" ]]; then
    die "unexpected inherited DarkIRC writer lock for disabled tenant '$name'"
  fi
}

validate_migration_export_boundary() {
  local name="$1"
  name="$(sanitize_name "$name")"
  [[ "${LUNARWING_MT_EXPORT_BOUNDARY_TENANT:-}" == "$name" ]] \
    || die "migration export boundary tenant marker mismatch"
  with_tenant_lifecycle_lock "$name" validate_migration_export_boundary_locked "$name"
}

restart_tenant() {
  local name="$1"
  stop_tenant "$name"
  start_tenant "$name"
}

darkirc_managed_state_exists() {
  local state_root
  state_root="$(tenant_state_dir "$1")/darkirc/key-exchange"
  [[ -e "$state_root" || -L "$state_root" ]]
}

upgrade_tenant_darkirc_locked() {
  local name="$1" scope_id
  scope_id="$(darkirc_scope_id "$name")"
  validate_darkirc_scope_id "$scope_id"
  run_darkirc_key_helper "$name" migration-ready \
    --tenant "$name" --scope-id "$scope_id" --json >/dev/null \
    || die "DarkIRC managed state is not settled; upgrade refused"
  # The caller retains the DarkIRC writer lock while upgrade_tenant_locked stops
  # the tenant, closing the readiness-check/stop race.
  upgrade_tenant_locked "$@"
}

# In-place tenant upgrade: composes the verified sequence (backup -> fetch ->
# clean-tree gate -> stop -> checkout as the tenant user -> rebuild with WASM ->
# re-render units -> patch env -> start). The verbs it calls dispatch
# OpenRC/systemd themselves, so this works on any supported init system. See
# docs/ops/TENANT-RENAME-MIGRATION-1.1.9.md for the 1.1.9 rename specifics.
upgrade_tenant() {
  local name="$1"
  shift
  name="$(sanitize_name "$name")"
  with_tenant_lifecycle_lock "$name" _upgrade_tenant_lifecycle_locked "$name" "$@"
}

_upgrade_tenant_lifecycle_locked() {
  local name="$1" upgrade_lock_fd
  say "acquiring tenant upgrade lock ..."
  exec {upgrade_lock_fd}>"${BUILD_LOCK}.${name}.upgrade" \
    || die "failed to open tenant upgrade lock"
  flock -x "$upgrade_lock_fd" || die "failed to acquire tenant upgrade lock"
  if tenant_darkirc_enabled "$name" && darkirc_managed_state_exists "$name"; then
    with_darkirc_writer_lock "$name" upgrade_tenant_darkirc_locked "$@"
  else
    upgrade_tenant_locked "$@"
  fi
  flock -u "$upgrade_lock_fd" || die "failed to release tenant upgrade lock"
  exec {upgrade_lock_fd}>&-
}

upgrade_tenant_locked() {
  local name="$1" target="$2" source_repo="$3" do_backup="$4" do_render="$5"
  local lw_root repo
  lw_root="$(tenant_lw_root "$name")"
  repo="$(tenant_repo "$name")"

  [[ -d "$lw_root/.git" ]] || die "no git checkout at $lw_root; run add-tenant first"

  local tgit=(sudo -u "$name" git -c safe.directory="$lw_root" -C "$lw_root")

  if [[ -n "$source_repo" ]]; then
    [[ -d "$source_repo/.git" || -f "$source_repo/HEAD" ]] || die "--source-repo $source_repo is not a git repository"
    say "pointing origin at $source_repo ..."
    "${tgit[@]}" remote set-url origin "$source_repo" || die "failed to retarget origin"
  fi

  local before
  before="$("${tgit[@]}" describe --tags --always 2>/dev/null || echo unknown)"
  say "upgrading tenant '$name': $before -> $target"

  if [[ "$do_backup" == "true" ]]; then
    backup_tenant_postgres "$name"
  else
    say "skipping pre-upgrade backup (--no-backup)"
  fi

  say "fetching tags + refs from origin (as $name) ..."
  "${tgit[@]}" fetch --tags --prune origin || die "git fetch failed"
  "${tgit[@]}" rev-parse --verify --quiet "${target}^{commit}" >/dev/null 2>&1 \
    || "${tgit[@]}" rev-parse --verify --quiet "origin/${target}^{commit}" >/dev/null 2>&1 \
    || die "target ref '$target' not found after fetch (need a branch, tag, or commit reachable from origin)"

  local checkout_ref target_commit
  checkout_ref="$target"
  if "${tgit[@]}" rev-parse --verify --quiet "refs/remotes/origin/$target" >/dev/null 2>&1; then
    checkout_ref="origin/$target"
  fi
  target_commit="$("${tgit[@]}" rev-parse --verify "${checkout_ref}^{commit}")" \
    || die "failed to resolve target ref '$target'"

  local build_lock_fd migrations_type previous_commit
  say "acquiring build lock for checkout ..."
  exec {build_lock_fd}>"$BUILD_LOCK" || die "failed to open build lock for checkout"
  flock -x "$build_lock_fd" || die "failed to acquire build lock for checkout"
  prepare_tenant_checkout "$name" "$lw_root" "$target_commit"
  migrations_type="$("${tgit[@]}" cat-file -t "${target_commit}:ic/migrations" 2>/dev/null || true)"
  [[ "$migrations_type" == "tree" ]] \
    || die "target ref '$target' has no ic/migrations directory (tenant was not stopped)"

  previous_commit="$("${tgit[@]}" rev-parse --verify HEAD)" \
    || die "failed to record current tenant commit"
  stop_tenant "$name"

  say "checking out $target at $target_commit (as $name) ..."
  if [[ "$checkout_ref" != "$target" ]]; then
    # Branch on origin: (re)create the local branch on the pinned fetched
    # commit so concurrent ref updates cannot change the deployed source.
    "${tgit[@]}" checkout -B "$target" "$target_commit" \
      || recover_failed_tenant_checkout "$name" "$lw_root" "$target" "$previous_commit"
  else
    "${tgit[@]}" checkout "$target_commit" \
      || recover_failed_tenant_checkout "$name" "$lw_root" "$target" "$previous_commit"
  fi
  [[ -d "$repo/migrations" ]] || die "ic/migrations missing after checkout — wrong ref?"
  say "  now at: $("${tgit[@]}" describe --tags --always 2>/dev/null)"
  flock -u "$build_lock_fd" || die "failed to release build lock after checkout"
  exec {build_lock_fd}>&-

  local with_opencode="false"
  if tenant_worker_enabled "$name" opencode; then
    with_opencode="true"
  fi
  local with_kimi="false"
  if tenant_worker_enabled "$name" kimi; then
    with_kimi="true"
  fi
  build_tenant "$name" "true" "false" "$with_opencode" "$with_kimi" "false" "true" \
    || die "build failed for $name; aborting upgrade (tenant left stopped at $target_commit; fix the build and re-run upgrade-tenant)"

  if [[ "$do_render" == "true" ]]; then
    render_tenant_units "$name"
  else
    say "skipping render-units (--skip-render); unit files keep their embedded paths"
  fi

  patch_tenant_env "$name"
  start_tenant "$name"

  # Pre-1.1.9 units embed renamed adapter paths that only keep working through
  # the 1.1.9-only compat symlinks — warn (don't fail) if any remain.
  local stale
  stale="$(grep -rlE 'ironclaw_weechat_wss|darkirc_channel_for_ironclaw' \
    /etc/init.d /etc/conf.d "$(tenant_home "$name")/.config/systemd/user" 2>/dev/null \
    | grep -F -- "$name" || true)"
  if [[ -n "$stale" ]]; then
    say "WARNING: these units still embed pre-rename paths (run render-units before v2.0.0):"
    say "$stale"
  fi

  say ""
  say "tenant '$name' upgraded: $before -> $("${tgit[@]}" describe --tags --always 2>/dev/null)"
  say "verify with: $0 status $name"
}

status_tenant() {
  local name="$1"
  name="$(sanitize_name "$name")"

  if ! tenant_exists_in_registry "$name"; then
    say "tenant '$name' not found in registry"
    return 1
  fi

  say "=== Tenant: $name ==="
  say ""
  say "Ports:"
  say "  gateway:          $(ports_get "$name" gateway)"
  say "  http:             $(ports_get "$name" http)"
  say "  bridge:           $(ports_get "$name" bridge)"
  say "  postgres:         $(ports_get "$name" postgres)"
  say "  proxy:            $(ports_get "$name" proxy)"
  say "  weechat:          $(ports_get "$name" weechat)"
  say "  orchestrator:     $(ports_get "$name" orchestrator)"
  say "  nanocode_wss:     $(ports_get "$name" nanocode_wss) (reserved legacy slot)"
  say "  pebble_wss:       $(ports_get "$name" pebble_wss)"
  say "  opencode_wss:     $(ports_get "$name" opencode_wss)"
  say "  kimi_wss:         $(ports_get "$name" kimi_wss)"
  say "  vision_service:   $(ports_get "$name" vision_service)"
  say "  vision_health:    $(ports_get "$name" vision_health)"
  say "  voice_service:    $(ports_get "$name" voice_service)"
  say "  voice_health:     $(ports_get "$name" voice_health)"
  say "  weechat_adapter:  $(ports_get "$name" weechat_adapter)"
  say ""

  ensure_container_runtime
  local container_name="lunarwing-pg-$name"
  if _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
    say "PostgreSQL: running ($container_name)"
  else
    say "PostgreSQL: stopped ($container_name)"
  fi

  local nanocode_container="lunarwing-nanocode-$name"
  if _ctr "$name" inspect -f '{{.State.Running}}' "$nanocode_container" 2>/dev/null | grep -q true; then
    say "Nanocode (retired): WARNING still running ($nanocode_container)"
  elif _ctr "$name" inspect "$nanocode_container" &>/dev/null; then
    say "Nanocode (retired): stopped container preserved ($nanocode_container)"
  else
    say "Nanocode (retired): no container in active runtime store"
  fi

  local pebble_container="lunarwing-pebble-$name"
  if _ctr "$name" inspect -f '{{.State.Running}}' "$pebble_container" 2>/dev/null | grep -q true; then
    say "Pebble worker: running ($pebble_container, WSS port $(ports_get "$name" pebble_wss))"
  elif _ctr "$name" inspect "$pebble_container" &>/dev/null; then
    say "Pebble worker: stopped ($pebble_container)"
  else
    say "Pebble worker: not created"
  fi

  local opencode_container="lunarwing-opencode-$name"
  if _ctr "$name" inspect -f '{{.State.Running}}' "$opencode_container" 2>/dev/null | grep -q true; then
    say "OpenCode worker: running ($opencode_container, WSS port $(ports_get "$name" opencode_wss))"
  elif _ctr "$name" inspect "$opencode_container" &>/dev/null; then
    say "OpenCode worker: stopped ($opencode_container)"
  else
    say "OpenCode worker: not created"
  fi

  local kimi_container="lunarwing-kimi-$name"
  if _ctr "$name" inspect -f '{{.State.Running}}' "$kimi_container" 2>/dev/null | grep -q true; then
    say "Kimi worker: running ($kimi_container, WSS port $(ports_get "$name" kimi_wss))"
  elif _ctr "$name" inspect "$kimi_container" &>/dev/null; then
    say "Kimi worker: stopped ($kimi_container)"
  else
    say "Kimi worker: not created"
  fi

  local vision_container="lunarwing-vision-$name"
  if _vision_container_healthy "$name" "$vision_container"; then
    say "LunarVision: healthy ($vision_container, API/health ports $(ports_get "$name" vision_service)/$(ports_get "$name" vision_health))"
  elif _ctr "$name" inspect -f '{{.State.Running}}' "$vision_container" 2>/dev/null | grep -q true; then
    say "LunarVision: running but unhealthy ($vision_container)"
  elif _ctr "$name" inspect "$vision_container" &>/dev/null; then
    say "LunarVision: stopped ($vision_container)"
  else
    say "LunarVision: not created"
  fi

  local voice_container="lunarwing-voice-$name"
  if _ctr "$name" inspect -f '{{.State.Running}}' "$voice_container" 2>/dev/null | grep -q true; then
    say "LunarVoice: running ($voice_container, API port $(ports_get "$name" voice_service))"
  elif _ctr "$name" inspect "$voice_container" &>/dev/null; then
    say "LunarVoice: stopped ($voice_container)"
  else
    say "LunarVoice: not created (enabled=$(tenant_voice_enabled "$name" && echo true || echo false))"
  fi

  ensure_init_system
  say ""
  say "Services ($INIT_SYSTEM):"
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    local svcs=("lunarwing-${name}" "xmpp-bridge-${name}" "lunarwing-proxy-${name}" \
                "lunarwing-weechat-${name}" "lunarwing-weechat-adapter-${name}")
    if tenant_darkirc_enabled "$name"; then
      svcs+=("lunarwing-darkirc-${name}" "lunarwing-darkirc-adapter-${name}")
    fi
    # LunarVision is supervised for every rootless Podman systemd deployment:
    # Quadlet where supported, otherwise the managed podman-wait fallback unit.
    if [[ "$MT_ROOTLESS" == "true" && "$CONTAINER_RT" == "podman" ]]; then
      svcs+=("lunarwing-vision-${name}")
    fi
    # PostgreSQL and workers use Quadlet only when the Podman version supports it.
    if [[ "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
      svcs+=("lunarwing-pg-${name}" "lunarwing-pebble-${name}" "lunarwing-opencode-${name}" "lunarwing-kimi-${name}")
      tenant_voice_enabled "$name" && svcs+=("lunarwing-voice-${name}")
    fi
    local svc state
    for svc in "${svcs[@]}"; do
      state="$(_systemctl_user "$name" is-active "${svc}.service" 2>/dev/null || echo "inactive")"
      say "  ${svc}.service: $state"
    done
  else
    local rc_svcs=("lunarwing-pg-${name}" "lunarwing-pg-${name}-sup" "lunarwing-${name}" "xmpp-bridge-${name}" "lunarwing-proxy-${name}" \
                   "lunarwing-weechat-${name}" "lunarwing-weechat-adapter-${name}" \
                   "lunarwing-vision-${name}" "lunarwing-vision-${name}-sup")
    if tenant_darkirc_enabled "$name"; then
      rc_svcs+=("lunarwing-darkirc-${name}" "lunarwing-darkirc-adapter-${name}")
    fi
    rc_svcs+=("lunarwing-pebble-${name}" "lunarwing-pebble-${name}-sup" \
              "lunarwing-opencode-${name}" "lunarwing-opencode-${name}-sup" \
              "lunarwing-kimi-${name}" "lunarwing-kimi-${name}-sup")
    tenant_voice_enabled "$name" && rc_svcs+=("lunarwing-voice-${name}")
    local svc state
    for svc in "${rc_svcs[@]}"; do
      state="$(rc-service "$svc" status 2>/dev/null | grep -oE 'started|stopped|crashed' || echo "unknown")"
      say "  $svc: $state"
    done
  fi
}

list_tenants() {
  ports_registry_init
  say "=== Registered tenants ==="
  say ""

  local names
  names="$(all_tenant_names)"
  if [[ -z "$names" ]]; then
    say "no tenants registered"
    return 0
  fi

  printf '%-15s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s\n' \
    "TENANT" "GATEWAY" "HTTP" "BRIDGE" "PG" "PROXY" "WEECHAT" "WS_ADPT" "ORCH" "NC_RSVD" "PEBBLE" "OPENCODE" "KIMI"
  printf '%-15s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s\n' \
    "------" "-------" "----" "------" "--" "-----" "-------" "-------" "----" "--------" "------" "--------" "----"

  while IFS= read -r name; do
    printf '%-15s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s\n' \
      "$name" \
      "$(ports_get "$name" gateway)" \
      "$(ports_get "$name" http)" \
      "$(ports_get "$name" bridge)" \
      "$(ports_get "$name" postgres)" \
      "$(ports_get "$name" proxy)" \
      "$(ports_get "$name" weechat)" \
      "$(ports_get "$name" weechat_adapter)" \
      "$(ports_get "$name" orchestrator)" \
      "$(ports_get "$name" nanocode_wss)" \
      "$(ports_get "$name" pebble_wss)" \
      "$(ports_get "$name" opencode_wss)" \
      "$(ports_get "$name" kimi_wss)"
  done <<< "$names"
}

show_tokens() {
  local filter="${1:-}"

  say "=== Gateway auth tokens ==="
  say ""

  local names
  if [[ -n "$filter" ]]; then
    names="$(sanitize_name "$filter")"
  else
    names="$(all_tenant_names)"
  fi

  if [[ -z "$names" ]]; then
    say "no tenants found"
    return 0
  fi

  while IFS= read -r name; do
    local env_path gateway_port token
    env_path="$(tenant_env_dir "$name")/lunarwing.env"
    gateway_port="$(ports_get "$name" gateway)"
    token="$(_read_env_value "$env_path" GATEWAY_AUTH_TOKEN)"
    say "$name (port $gateway_port): ${token:-<not set>}"
  done <<< "$names"
}

doctor() {
  say "=== Multi-tenant doctor ==="
  say ""

  local pass=0 fail=0
  _check() {
    local label="$1"; shift
    if "$@" >/dev/null 2>&1; then
      printf '[PASS] %s\n' "$label"
      pass=$((pass + 1))
    else
      printf '[FAIL] %s\n' "$label"
      fail=$((fail + 1))
    fi
  }

  _check "running as root" test "${EUID}" -eq 0
  _check "jq installed" command -v jq
  _check "git installed" command -v git
  _check "python3 installed" command -v python3
  _check "cargo installed" command -v cargo
  _check "rustup installed" command -v rustup

  if command -v docker >/dev/null 2>&1; then
    _check "docker available" docker info
  fi
  if command -v podman >/dev/null 2>&1; then
    _check "podman available" podman info
  fi

  # Informational: which runtime commands will use, and why. Single call,
  # protected by the if: detect_container_runtime dies (exits) on a box with
  # neither runtime, and only a $( ) subshell can contain that exit. The
  # source label is re-derived here with the same precedence the function
  # uses (env > saved file > auto-detect).
  local _rt_resolved _rt_source
  if _rt_resolved="$(detect_container_runtime 2>/dev/null)"; then
    if [[ -n "${LUNARWING_CONTAINER_RUNTIME:-}" ]]; then
      _rt_source="env"
    elif [[ "$(_load_saved_container_runtime 2>/dev/null)" == "$_rt_resolved" ]]; then
      _rt_source="saved — $RUNTIME_STATE_FILE"
    else
      _rt_source="auto-detected"
    fi
    printf '[info] container runtime: %s (%s)\n' "$_rt_resolved" "$_rt_source"
  else
    printf '[info] container runtime: unresolved (install docker or podman, or set LUNARWING_CONTAINER_RUNTIME)\n'
  fi

  _check "sshd listening on 127.0.0.1:22 (needed for loopback SSH tenants)" \
    bash -c 'timeout 2 bash -c "exec 3<>/dev/tcp/127.0.0.1/22"'

  ensure_container_runtime
  if [[ "$MT_ROOTLESS" == "true" ]]; then
    _check "rootless: newuidmap setuid" bash -c '[ -u "$(command -v newuidmap 2>/dev/null)" ]'
    _check "rootless: newgidmap setuid" bash -c '[ -u "$(command -v newgidmap 2>/dev/null)" ]'
    _check "rootless: /etc/subuid populated" test -s /etc/subuid
    _check "rootless: /etc/subgid populated" test -s /etc/subgid
    # Every per-tenant container publishes 127.0.0.1:<port>:…, which under rootless
    # needs a userspace port-forwarder (pasta or slirp4netns).
    _check "rootless: pasta or slirp4netns (port-forward)" \
      bash -c 'command -v pasta >/dev/null 2>&1 || command -v slirp4netns >/dev/null 2>&1'
  fi

  ensure_init_system
  _check "init system detected ($INIT_SYSTEM)" true

  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    _check "loginctl available" command -v loginctl
    if [[ "$MT_ROOTLESS" == "true" ]]; then
      _check "podman >= 4.6 (Quadlet supervision)" podman_supports_quadlet
    fi
    # Per-tenant linger keeps /run/user/<uid> + the systemd --user manager alive
    # across reboot — boot-persistent Quadlet/user units depend on it. (Highest-
    # value rootless-on-systemd check.)
    local _dt _du _duid
    while IFS=$'\t' read -r _dt _du; do
      [[ -n "$_du" ]] || continue
      _duid="$(id -u "$_du" 2>/dev/null || echo "")"
      [[ -n "$_duid" ]] || continue
      _check "tenant $_dt: linger enabled" \
        bash -c "loginctl show-user '$_du' -p Linger --value 2>/dev/null | grep -qx yes"
      _check "tenant $_dt: /run/user/$_duid present" test -d "/run/user/$_duid"
    done < <(jq -r '.tenants // {} | to_entries[] | "\(.key)\t\(.value.user)"' "$PORTS_REGISTRY" 2>/dev/null || true)
  else
    _check "rc-service available" command -v rc-service
    _check "rc-update available" command -v rc-update
  fi

  _check "port registry exists" test -f "$PORTS_REGISTRY"
  _check "source repo exists" test -d "$SOURCE_REPO/ic"
  _check "proxy script exists" test -f "$SOURCE_REPO/tensorzero-proxy-configurations/lunarwing-proxy.py"
  _check "health-check orchestrator present" test -x "$HEALTH_SRC_DIR/infrastructure-health-check.sh"
  _check "self-heal script present" test -x "$HEALTH_SRC_DIR/lunarwing-self-heal.sh"
  _check "curl installed (self-heal/gotify)" command -v curl
  _check "flock installed (self-heal lock)" command -v flock
  if [[ "$DEFAULT_HEALTH_ENABLED" == "true" ]]; then
    _check "health pipeline scheduled" bash -c 'systemctl is-enabled lunarwing-mt-health.timer >/dev/null 2>&1 || crontab -l 2>/dev/null | grep -q lunarwing-mt-health || { command -v fcrontab >/dev/null 2>&1 && fcrontab -l 2>/dev/null | grep -q lunarwing-mt-health; }'
  fi
  _check "pebble worker dir exists" test -d "$LUNARWING_ROOT/pebble4lunarwing"
  _check "pebble worker Dockerfile exists" test -f "$LUNARWING_ROOT/pebble4lunarwing/Dockerfile"
  _check "opencode worker dir exists" test -d "$LUNARWING_ROOT/opencode4lunarwing"
  _check "opencode worker Dockerfile exists" test -f "$LUNARWING_ROOT/opencode4lunarwing/Dockerfile"
  _check "kimi worker dir exists" test -d "$LUNARWING_ROOT/kimicode4lunarwing"
  _check "kimi worker Dockerfile exists" test -f "$LUNARWING_ROOT/kimicode4lunarwing/Dockerfile"

  # Check if worker images are built
  if command -v docker >/dev/null 2>&1; then
    _check "pebble worker image exists" docker image inspect lunarwing-worker-pebble:latest
    _check "opencode worker image exists" docker image inspect lunarwing-worker-opencode:latest
    _check "kimi worker image exists" docker image inspect lunarwing-worker-kimi:latest
    _check "vision sidecar image exists" docker image inspect "$VISION_SIDECAR_IMAGE"
  elif command -v podman >/dev/null 2>&1; then
    _check "pebble worker image exists" podman image inspect lunarwing-worker-pebble:latest
    _check "opencode worker image exists" podman image inspect lunarwing-worker-opencode:latest
    _check "kimi worker image exists" podman image inspect lunarwing-worker-kimi:latest
    _check "vision sidecar image exists" podman image inspect "$VISION_SIDECAR_IMAGE"
  fi

  say ""
  say "passed: $pass, failed: $fail"
  [[ $fail -eq 0 ]]
}
