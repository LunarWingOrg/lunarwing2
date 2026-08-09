#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 04-tenant-lifecycle.sh — part of the lunarwing-mt-admin library.
#
# High-level tenant verbs: add/remove tenant, start/stop/restart, unit
# re-render, checkout preparation and recovery, upgrade, status, list.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced, so this file only defines functions.

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
  say ""
  say "Next steps:"
  say "  sudo $0 build-tenant $name --with-wasm"
  say "  sudo $0 start-tenant $name"
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
