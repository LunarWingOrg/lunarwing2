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

  # If that was the last tenant, retire the host-global health pipeline schedule.
  if [[ -z "$(all_tenant_names)" ]]; then
    remove_health_pipeline
    say ""
  fi

  say "=== Tenant '$name' removed ==="
}

_restart_tenant_daemon() {
  local name="$1"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    _systemctl_user "$name" restart "lunarwing-${name}.service"
  else
    rc-service "lunarwing-${name}" restart
  fi
}

restart_tenant() {
  local name="$1"
  stop_tenant "$name"
  start_tenant "$name"
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

upgrade_tenant() {
  local name="$1"
  shift
  name="$(sanitize_name "$name")"
  with_tenant_lifecycle_lock "$name" _upgrade_tenant_lifecycle_locked "$name" "$@"
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
  build_tenant "$name" "true" "false" "$with_opencode" "$with_kimi" "false" "true"

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
    # pg + workers are Quadlet units only on rootless podman; on rootful docker
    # they run as plain containers (shown above), not systemd units.
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
                   "lunarwing-weechat-${name}" "lunarwing-weechat-adapter-${name}")
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
