#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 11-postgres.sh — part of the lunarwing-mt-admin library.
#
# Per-tenant PostgreSQL container lifecycle, password rotation, and pg_dump
# backup/restore with retention.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced.
#
# Extracted from the v2.0.8.0 monolith (goals item #9), keeping the library
# structure of ref2/sunburst/mtadmin-refactor-v2.0.6.0 per
# docs/proposals/MT-ADMIN-DECOMPOSITION.md. Pure line-range move: no function
# body was rewritten.


# ── PostgreSQL container ─────────────────────────────────────────────────────

# Rootless PG data is present when the container exists OR the exact-name data
# volume exists (Quadlet's --rm removes the container on every unit stop while
# keeping the named volume, so container-only would false-negative a migrated
# tenant's restart — RW-2/run-271). The volume probe compares `{{.Name}}` for
# equality because `volume inspect <name>` itself prefix-matches a longer
# volume (e.g. lunarwing-pg-<t> resolves to lunarwing-pg-<t>2), the same
# cross-object hazard `_ctr_inspect` removes for containers.
_rootless_pg_data_present() {  # <tenant> <container_name> -> 0 if data present
  local name="$1" container_name="$2" volume_name
  _ctr_inspect "$name" "$container_name" &>/dev/null && return 0
  volume_name="$(_ctr "$name" volume inspect --format '{{.Name}}' \
    "lunarwing-pg-${name}" 2>/dev/null || true)"
  [[ "$volume_name" == "lunarwing-pg-${name}" ]]
}

# The legacy v1.1.0 rootful PG lived in root's container store. Probe it with
# the typed subcommand so a root-store *volume* named lunarwing-pg-<t>-... can
# never satisfy the "root container still exists" arm (RW-5/run-271); the guard
# fails closed either way — a miss here only means "no root data to protect".
_root_store_pg_container_present() {  # <tenant> <container_name> -> 0 if present
  local name="$1" container_name="$2"
  _root_ctr container inspect "$container_name" &>/dev/null
}

start_tenant_postgres() {
  local name="$1"
  ensure_container_runtime

  local pg_port container_name
  pg_port="$(ports_get "$name" postgres)"
  container_name="lunarwing-pg-$name"

  # ── Rootful→rootless data-orphan guard ──────────────────────────────────────
  # v1.1.0 ran PG ROOTFUL (root's container store, data in the container writable
  # layer, no named volume); v1.1.4 defaults MT_ROOTLESS=true (rootless, tenant
  # store). If a legacy ROOT-store container still exists and the tenant has NO
  # rootless container yet, creating the rootless PG here would silently bring the
  # tenant up on an EMPTY database and orphan the v1.1.0 data. Refuse by default.
  # Fires ONLY in this exact window (rootless target + root container present +
  # rootless container absent), so fresh tenants and already-migrated tenants are
  # never affected. Escape hatches: migrate (upgrade-tenant.sh), keep rootful
  # (LUNARWING_MT_ROOTLESS=false), or acknowledge an intended fresh DB
  # (LUNARWING_MT_ACK_ROOTLESS_FLIP=<tenant> — set automatically by the migration
  # tool once it has a verified backup in hand). The ack is a comma-separated list
  # of tenant NAMES, not a global boolean, so a stray `export …=1` cannot silence
  # the guard for OTHER un-migrated tenants in the same shell/CI session.
  #
  # Coverage note: this fires only when the rootless container is ABSENT. The
  # "rootless exists but is EMPTY while a root orphan persists" case is deliberately
  # NOT caught here — the post-migration steady state (live rootless DB + a root
  # container kept as a rollback net) also has both present, and a start-time probe
  # can't tell them apart without the container running, so guarding it here would
  # break every legitimate restart. That case is surfaced by upgrade-preflight.sh
  # and blocked by `upgrade-tenant.sh --prune-old-root` (refuses to delete the root
  # copy while the rootless DB is empty).
  #
  # Rootless-data presence is "container OR exact-name volume", not container-only:
  # on systemd+Quadlet the user-generator runs PG with `--rm`, so every unit stop
  # removes the container while the named data volume `lunarwing-pg-<t>` stays.
  # A container-only probe would then see "no rootless data" on a legitimately
  # migrated tenant's next start-tenant and trip this guard on its kept root
  # rollback container. `volume inspect` compares `{{.Name}}` for equality, so the
  # prefix cross-match that motivated `_ctr_inspect` cannot re-enter here
  # (RW-2/run-271: a plain volume-name existence check also prefix-matches, e.g.
  # lunarwing-pg-<t> vs lunarwing-pg-<t>2).
  if [[ "$MT_ROOTLESS" == "true" && "$CONTAINER_RT" == "podman" ]] \
     && ! _rootless_pg_data_present "$name" "$container_name" \
     && _root_store_pg_container_present "$name" "$container_name"; then
    {
      say "############################################################"
      say "# DATA-ORPHAN GUARD — tenant '$name'"
      say "# A ROOTFUL (root-store) '$container_name' exists, but MT_ROOTLESS=true"
      say "# would create a NEW, EMPTY rootless database and orphan the existing"
      say "# v1.1.0 data (still recoverable from the root container until removed)."
      say "#"
      say "# Choose one:"
      say "#   migrate data:     sudo ic/scripts/upgrade-tenant.sh $name"
      say "#   keep rootful:     LUNARWING_MT_ROOTLESS=false <re-run this command>"
      say "#   intended fresh DB: LUNARWING_MT_ACK_ROOTLESS_FLIP=$name <re-run>"
      say "############################################################"
    } >&2
    if [[ ",${LUNARWING_MT_ACK_ROOTLESS_FLIP:-}," != *",$name,"* ]]; then
      die "refusing to create an empty rootless PG over existing root-store data for '$name' (see guard above)"
    fi
    say "LUNARWING_MT_ACK_ROOTLESS_FLIP lists '$name' — proceeding with a fresh rootless DB; root-store data left intact." >&2
  fi

  # systemd + rootless podman: a Quadlet .container owns the lifecycle (boot-
  # persistent, health-monitored, self-healable). Quadlet creates the container,
  # so skip the imperative `_ctr run` below; keep the pg_isready gate (via exec).
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    render_pg_quadlet "$name"
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if ! _systemctl_user "$name" start "lunarwing-pg-${name}.service" >/dev/null 2>&1; then
      say "WARNING: lunarwing-pg-${name}.service failed to start" >&2
      _systemctl_user "$name" status "lunarwing-pg-${name}.service" --no-pager >&2 || true
    fi
    # Gate on the container's own health (the Quadlet defines a pg_isready
    # HealthCmd) OR a direct pg_isready, rather than a bare `exec` loop: on a fresh
    # volume the initdb cycle (temp server up→down→restart) makes a single exec
    # probe flap, which previously exhausted the budget and `die`d — aborting the
    # WHOLE provision and leaving a half-baked tenant (F2). A slow first boot must
    # NOT strand the tenant: warn and continue (the unit has Restart=on-failure and
    # the host health pipeline/self-heal converge it), so add-tenant still renders
    # units and installs the pipeline.
    local q_attempts=0 q_ready=false q_health=""
    while (( q_attempts < 120 )); do
      q_health="$(_ctr_inspect "$name" -f '{{if .State.Health}}{{.State.Health.Status}}{{end}}' "$container_name" 2>/dev/null || true)"
      if [[ "$q_health" == healthy ]] || _ctr "$name" exec "$container_name" pg_isready -U lunarwing -q 2>/dev/null; then
        q_ready=true; break
      fi
      q_attempts=$((q_attempts + 1)); sleep 1
    done
    if [[ "$q_ready" == true ]]; then
      say "PostgreSQL ready via quadlet ($container_name, port $pg_port)"
    else
      say "WARNING: PostgreSQL for $name not confirmed ready after ${q_attempts}s (health=${q_health:-none}); continuing — pg has Restart=on-failure and the health pipeline will converge it." >&2
    fi
    return 0
  fi

  local -a healthcheck_args=()
  if _openrc_podman_uses_init_health; then
    _reconcile_openrc_podman_healthcheck "$name" "$container_name"
    healthcheck_args=(--no-healthcheck)
  fi

  if _ctr_inspect "$name" "$container_name" &>/dev/null; then
    if _ctr_inspect "$name" -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "PostgreSQL already running ($container_name, port $pg_port)"
      return 0
    fi
    say "starting existing PostgreSQL container $container_name"
    if ! _ctr "$name" start "$container_name" >/dev/null; then
      say "WARNING: could not start existing PostgreSQL container $container_name for $name; continuing — re-run start-tenant or let the health pipeline converge it." >&2
      return 0
    fi
  else
    say "creating PostgreSQL container $container_name on port $pg_port"
    # Named volume (not anonymous) so the data has a stable, inspectable,
    # exportable identity for backups; rootless podman auto-chowns it inside the
    # tenant user namespace. --restart is a no-op under rootless podman (no
    # daemon — OpenRC owns lifecycle), so only set it for rootful docker.
    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    # Pass POSTGRES_PASSWORD via a transient 0600 --env-file rather than `-e` so
    # the per-tenant secret never lands on the container-runtime argv (readable in
    # /proc/<pid>/cmdline by other local users). Removed right after creation; the
    # Quadlet path keeps it in its own 0600 unit file for the same reason.
    local pg_init_env pg_init_pw
    pg_init_env="$(tenant_env_dir "$name")/.pg-init.env"
    # WI#423 (run-166 LF-5): this was the last root `umask 077; printf >` plus
    # root `chown` inside the tenant-writable env dir — a pre-placed symlink
    # at .pg-init.env redirected both. Resolve the password fail-closed and
    # commit through the tenant-side atomic writer, exactly like pg.secret.
    if ! pg_init_pw="$(tenant_pg_password "$name")"; then
      die "could not resolve the PostgreSQL password for tenant '$name'"
    fi
    if ! write_tenant_file_atomic "$name" "$pg_init_env" 600 <<<"POSTGRES_PASSWORD=$pg_init_pw"; then
      die "could not safely write $pg_init_env as tenant '$name'"
    fi
    # O2: don't let a pg CREATE failure abort the whole provision (set -e). The F2
    # gate already downgrades a slow-readiness race to warn+continue; mirror that
    # for the create call (and the start above) so a transient runtime hiccup
    # doesn't strand the tenant half-baked (no units rendered, no health pipeline
    # installed). add-tenant is resumable (F4): fix the cause and re-run.
    if ! _ctr "$name" run -d \
      --name "$container_name" \
      "${healthcheck_args[@]}" \
      -e POSTGRES_USER=lunarwing \
      --env-file "$pg_init_env" \
      -e POSTGRES_DB=lunarwing \
      -p "127.0.0.1:${pg_port}:5432" \
      -v "lunarwing-pg-${name}:/var/lib/postgresql/data" \
      "${restart_arg[@]}" \
      "$PG_IMAGE" >/dev/null; then
      rm -f "$pg_init_env"
      # Remove the partial/failed container so a resumed start-tenant re-creates it
      # cleanly, rather than taking the "start existing" branch on a broken shell.
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
      say "WARNING: PostgreSQL container create failed for $name; add-tenant still renders units + installs the health pipeline (so exit 0 does NOT imply pg is up). Fix the cause and re-run start-tenant." >&2
      return 0
    fi
    rm -f "$pg_init_env"
  fi

  local attempts=0
  while ! _ctr "$name" exec "$container_name" pg_isready -U lunarwing -q 2>/dev/null; do
    attempts=$((attempts + 1))
    if (( attempts >= 90 )); then
      say "WARNING: PostgreSQL for $name not confirmed ready after ${attempts}s; continuing — the host health pipeline/self-heal will converge it." >&2
      return 0
    fi
    sleep 1
  done
  say "PostgreSQL ready ($container_name, port $pg_port)"
}

stop_tenant_postgres() {
  local name="$1"
  if _tenant_stop_skips_missing_account "$name" PostgreSQL; then
    return 0
  fi

  local container_name="lunarwing-pg-$name"
  ensure_init_system
  # Quadlet owns the container on systemd+rootless: stop the unit first, or
  # Restart=on-failure resurrects the container mid-shutdown (goal #10). The
  # container fallback covers a unit that is not loaded/active.
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] \
      && podman_supports_quadlet; then
    _systemctl_user "$name" stop "${container_name}.service" >/dev/null 2>&1 \
      || _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
  fi
  if _ctr_inspect "$name" "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "PostgreSQL stopped ($container_name)"
  fi
}

reset_tenant_postgres() {
  local name="$1"
  # WI#688: the data volume of a rootless tenant whose account is gone stays
  # in the store under its former home; mt-admin cannot reach it any more.
  if tenant_rootless_account_gone "$name"; then
    say "skipped PostgreSQL data removal for $name: the tenant account is gone; its data volume stays in the rootless store under $(tenant_home "$name")"
    return 0
  fi

  local container_name="lunarwing-pg-$name"
  stop_tenant_postgres "$name"
  _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
  # Remove the named data volume too so a subsequent create starts fresh (matches
  # the pre-named-volume behaviour where the anonymous volume was orphaned on rm).
  _ctr "$name" volume rm "lunarwing-pg-${name}" >/dev/null 2>&1 || true
  say "PostgreSQL removed ($container_name, data volume cleared)"
}

# Rotate an existing tenant's PG password to a fresh random one. Unlike a new
# tenant (where POSTGRES_PASSWORD seeds an empty datadir), an initialised DB needs
# an in-place ALTER ROLE, then the persisted secret + DATABASE_URL updated, then a
# daemon restart so it reconnects with the new credential. The new password is hex
# (URL/SQL-safe) and is never echoed.
rotate_tenant_pg_password() {
  local name="$1"
  ensure_container_runtime
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  local container_name="lunarwing-pg-$name"
  _ctr_inspect "$name" -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
    || die "PostgreSQL container $container_name is not running; start the tenant first"

  local envf pg_port new_pw secret_file
  envf="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$envf" ]] || die "tenant env not found: $envf"
  pg_port="$(ports_get "$name" postgres)"
  new_pw="$(generate_token | cut -c1-32)"

  # Change the live role password. psql connects over the container's local unix
  # socket (trust auth in the postgres image), and the new value is fed on stdin —
  # never on argv or in logs. A hex value carries no SQL-quoting hazard.
  if ! printf "ALTER ROLE lunarwing PASSWORD '%s';\n" "$new_pw" \
       | _ctr "$name" exec -i "$container_name" psql -v ON_ERROR_STOP=1 -U lunarwing -d lunarwing -q >/dev/null 2>&1; then
    die "failed to ALTER ROLE password inside $container_name (is the DB healthy?)"
  fi

  # Persist the new secret (source of truth). The refusal must be fatal
  # (WI#390, review IG-4): the role password is already changed, so failing
  # open here would leave pg.secret and DATABASE_URL disagreeing with the live
  # DB while reporting success. A refusal aborts the rotation loudly; re-running
  # rotate-pg-password is safe (it ALTERs to a fresh value and rewrites both).
  secret_file="$(tenant_env_dir "$name")/pg.secret"
  printf '%s\n' "$new_pw" | write_tenant_file_atomic "$name" "$secret_file" 600 \
    || die "could not safely write $secret_file"

  # ... and replace DATABASE_URL without reopening a tenant-writable temp as root.
  # WI#390 (IY-3): the replace must be fatal too — under a suppressed-errexit
  # caller an unchecked refusal would report "Rotated" while DATABASE_URL still
  # carries the old password.
  _tenant_env_replace "$name" "$envf" DATABASE_URL \
    "postgres://lunarwing:${new_pw}@127.0.0.1:${pg_port}/lunarwing" \
    || die "could not safely update DATABASE_URL in $envf (pg.secret already holds the new password; re-run rotate-pg-password)"

  say "Rotated PostgreSQL password for tenant '$name'."
  say "IMPORTANT: the running daemon still holds the old credential — restart to apply:"
  say "    lunarwing-mt-admin.sh restart-tenant $name"
}

# Run psql inside the tenant's own PostgreSQL container via `_ctr`, so the
# query hits the same store that owns the container on every init system and
# container runtime (rootless podman under the tenant user, or rootful
# docker). Read-only operator diagnostics (audits, health inspection) are the
# intended use: SELECTs and fingerprints, not content dumps — env/DB rows can
# be secret-bearing. Extra argv is passed through to psql verbatim.
# Usage: tenant_psql <name> [psql args...]
tenant_psql() {
  local name="$1"; shift
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_container_runtime

  local container_name="lunarwing-pg-$name"
  _ctr_inspect "$name" -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
    || die "PostgreSQL container $container_name is not running; start the tenant first"

  _ctr "$name" exec -i "$container_name" psql -U lunarwing -d lunarwing "$@"
}

_pg_named_volume_present() {
  local name="$1" volume_name
  volume_name="$(_ctr "$name" volume inspect --format '{{.Name}}' \
    "lunarwing-pg-${name}" 2>/dev/null || true)"
  [[ "$volume_name" == "lunarwing-pg-${name}" ]]
}

_pg_container_uses_named_volume() {
  local name="$1" mount
  # A stopped Quadlet removes its container; its retained exact-name volume
  # is enough in that case. An existing container must actually use that volume.
  _ctr_inspect "$name" "lunarwing-pg-${name}" &>/dev/null || return 0
  mount="$(_ctr_inspect "$name" -f \
    '{{range .Mounts}}{{if eq .Destination "/var/lib/postgresql/data"}}{{.Type}}:{{.Name}}{{end}}{{end}}' \
    "lunarwing-pg-${name}" 2>/dev/null)" || return 1
  [[ "$mount" == "volume:lunarwing-pg-${name}" ]]
}

_pg_daemon_activity() {
  local name="$1" state rc=0
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    state="$(_systemctl_user "$name" is-active "lunarwing-${name}.service" 2>/dev/null)" || rc=$?
    case "$rc:$state" in
      0:active) printf 'active\n' ;;
      3:inactive|3:failed|4:inactive) printf 'stopped\n' ;;
      *) printf 'unknown\n' ;;
    esac
  else
    _openrc_service "lunarwing-${name}" status >/dev/null 2>&1 || rc=$?
    case "$rc" in
      0) printf 'active\n' ;;
      3) printf 'stopped\n' ;;
      *) printf 'unknown\n' ;;
    esac
  fi
}

_pg_stop_daemon_for_refresh() {
  local name="$1" activity="$2"
  [[ "$activity" == active ]] || return 0
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    _systemctl_user "$name" stop "lunarwing-${name}.service" || return 1
  else
    _openrc_service "lunarwing-${name}" stop || return 1
  fi
  [[ "$(_pg_daemon_activity "$name")" == stopped ]]
}

_pg_start_daemon_after_refresh() {
  local name="$1" activity="$2"
  [[ "$activity" == active ]] || return 0
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    _systemctl_user "$name" start "lunarwing-${name}.service" || return 1
  else
    _openrc_service "lunarwing-${name}" start || return 1
  fi
  [[ "$(_pg_daemon_activity "$name")" == active ]]
}

_pg_replace_container_for_refresh() {
  local name="$1" container_name="lunarwing-pg-$1" quadlet=false
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    quadlet=true
    render_pg_quadlet "$name" "$PG_IMAGE" || return 1
    _systemctl_user "$name" daemon-reload || return 1
    stop_tenant_postgres "$name" || return 1
  elif [[ "$INIT_SYSTEM" == "openrc" ]]; then
    _openrc_stop_service_if_installed "lunarwing-pg-${name}-sup" || return 1
    _openrc_stop_service_if_installed "lunarwing-pg-${name}" || return 1
  else
    stop_tenant_postgres "$name" || return 1
  fi
  if _ctr_inspect "$name" "$container_name" &>/dev/null; then
    _ctr "$name" rm -f "$container_name" >/dev/null || return 1
  fi
  if [[ "$quadlet" == true ]]; then
    _systemctl_user "$name" start "${container_name}.service" >/dev/null || return 1
  else
    start_tenant_postgres "$name" || return 1
    _ctr_inspect "$name" -f '{{.State.Running}}' "$container_name" 2>/dev/null \
      | grep -q true || return 1
    if [[ "$INIT_SYSTEM" == "openrc" ]]; then
      _openrc_service "lunarwing-pg-${name}" start || return 1
      _openrc_service "lunarwing-pg-${name}-sup" start \
        || say "WARNING: optional PostgreSQL supervisor for '$name' did not start; check it after refresh" >&2
    fi
  fi
}

_pg_refresh_version_allowed() {
  local name="$1" server_version="$2"
  [[ "$server_version" =~ ^[0-9]{6}$ ]] || return 1
  if (( 10#$server_version >= 160016 )); then return 0; fi
  if [[ "$server_version" == 16* && "${LUNARWING_MT_PG_ALLOW_UNPATCHED:-}" == "$name" ]]; then
    say "WARNING: explicit unpatched PostgreSQL rollback allowed for '$name' ($server_version)" >&2
    return 0
  fi
  say "PostgreSQL server_version_num=$server_version is below security floor 160016" >&2
  return 1
}

_pg_check_refresh_image_version() {
  local name="$1" version_output major minor
  # Use the pulled image in the tenant's store, not a possibly different host
  # binary. No tenant data/secrets are mounted and the image entrypoint is bypassed.
  version_output="$(_ctr "$name" run --rm --pull=never --network none --read-only \
    --cap-drop ALL --security-opt no-new-privileges --no-healthcheck \
    --entrypoint postgres "$PG_IMAGE" --version)" || return 1
  # PostgreSQL 10+ encodes major*10000 + minor. Reject unrecognized/prerelease
  # output and bound both components before shell arithmetic.
  if [[ "$version_output" =~ ^postgres[[:space:]]+\(PostgreSQL\)[[:space:]]+([1-9][0-9])\.([0-9]{1,4})([[:space:]].*)?$ ]]; then
    major="${BASH_REMATCH[1]}"
    minor="${BASH_REMATCH[2]}"
    _pg_refresh_version_allowed "$name" "$((10#$major * 10000 + 10#$minor))"
  else
    say "could not parse PostgreSQL image version for '$name'" >&2
    return 1
  fi
}

_pg_wait_for_refresh_health() {
  local name="$1" container_name="lunarwing-pg-$1" attempts=0 vector_version server_version
  while (( attempts < 120 )); do
    if _ctr "$name" exec "$container_name" pg_isready -U lunarwing -q 2>/dev/null; then
      vector_version="$(_ctr "$name" exec "$container_name" \
        psql -v ON_ERROR_STOP=1 -U lunarwing -d lunarwing -Atc \
        "SELECT extversion FROM pg_extension WHERE extname = 'vector'" 2>/dev/null || true)"
      if [[ -n "$vector_version" ]] && _ctr "$name" exec "$container_name" \
          psql -v ON_ERROR_STOP=1 -U lunarwing -d lunarwing -Atc \
          "SELECT '[1,2,3]'::vector" >/dev/null 2>&1; then
        server_version="$(_ctr "$name" exec "$container_name" \
          psql -v ON_ERROR_STOP=1 -U lunarwing -d lunarwing -Atc \
          'SHOW server_version_num' 2>/dev/null)" || return 1
        _pg_refresh_version_allowed "$name" "$server_version"
        return $?
      fi
    fi
    attempts=$((attempts + 1))
    sleep 1
  done
  return 1
}

tenant_postgres_image_status() {
  local name="$1" container_name image_ref=missing image_id=missing version=unavailable present=true
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_container_runtime
  container_name="lunarwing-pg-$name"
  if _ctr_inspect "$name" "$container_name" &>/dev/null; then
    image_ref="$(_ctr_inspect "$name" -f '{{.Config.Image}}' "$container_name" 2>/dev/null || printf 'unknown')"
    image_id="$(_ctr_inspect "$name" -f '{{.Image}}' "$container_name" 2>/dev/null || printf 'unknown')"
    version="$(_ctr "$name" exec "$container_name" psql -U lunarwing -d lunarwing -Atc \
      'SELECT version()' 2>/dev/null || printf 'unavailable')"
  else
    present=false
  fi
  printf 'tenant=%s\nimage_reference=%s\nimage_id=%s\nserver_version=%s\n' \
    "$name" "$image_ref" "$image_id" "$version"
  [[ "$present" == true ]]
}

refresh_tenant_postgres_image() {
  local name="$1" activity
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_container_runtime
  ensure_init_system
  _pg_named_volume_present "$name" \
    || die "PostgreSQL refresh requires the named volume lunarwing-pg-${name}; migrate legacy container-local data first"
  _pg_container_uses_named_volume "$name" \
    || die "PostgreSQL container does not mount lunarwing-pg-${name} at /var/lib/postgresql/data; refresh refused before stop"
  tenant_postgres_image_status "$name" || true
  if ! _ctr "$name" pull "$PG_IMAGE"; then
    die "PostgreSQL image pull failed for '$name'; no service was stopped"
  fi
  _pg_check_refresh_image_version "$name" \
    || die "PostgreSQL image version preflight failed for '$name'; no service was stopped"
  activity="$(_pg_daemon_activity "$name")"
  [[ "$activity" != unknown ]] \
    || die "cannot determine whether the '$name' daemon is running; refresh refused before stop"
  _pg_stop_daemon_for_refresh "$name" "$activity" \
    || die "failed to stop the '$name' daemon; PostgreSQL refresh refused"
  if ! _pg_replace_container_for_refresh "$name"; then
    die "PostgreSQL container replacement failed for '$name'; data volume retained and daemon left stopped"
  fi
  if ! _pg_wait_for_refresh_health "$name"; then
    die "replacement PostgreSQL for '$name' failed readiness/pgvector checks; data volume retained and daemon left stopped"
  fi
  _pg_start_daemon_after_refresh "$name" "$activity" \
    || die "PostgreSQL is healthy but the '$name' daemon did not return to its prior active state"
  say "PostgreSQL image refreshed for '$name' (named data volume retained)."
  tenant_postgres_image_status "$name"
}

# ── PostgreSQL backup / restore ──────────────────────────────────────────────

# pg_dump a tenant's database to a timestamped custom-format file under
# $BACKUP_DIR/<tenant>/. Runs pg_dump inside the tenant's pg container via _ctr,
# so it is init- and runtime-agnostic (rootless podman or rootful docker). Safe
# on a live database (MVCC snapshot). Prunes to the most recent $BACKUP_KEEP.
backup_tenant_postgres() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_container_runtime
  local container_name="lunarwing-pg-$name"

  _ctr_inspect "$name" -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
    || die "PostgreSQL not running for '$name' ($container_name) — start the tenant first"

  local dir ts dest tmp
  dir="$BACKUP_DIR/$name"
  mkdir -p "$dir"
  chmod 0700 "$BACKUP_DIR" "$dir" 2>/dev/null || true
  ts="$(date +%Y%m%d%H%M%S)"
  dest="$dir/${name}-${ts}.dump"
  tmp="$dest.partial"

  say "backing up '$name' -> $dest"
  # -Fc: compressed custom format (restore via pg_restore). umask 077 so the
  # .partial is never world-readable, even mid-dump; rename on success so an
  # interrupted dump never looks complete.
  if ( umask 077; _ctr "$name" exec "$container_name" pg_dump -U lunarwing -Fc lunarwing > "$tmp" ); then
    mv "$tmp" "$dest"
    say "backup complete: $dest ($(du -h "$dest" 2>/dev/null | cut -f1))"
  else
    rm -f "$tmp"
    die "pg_dump failed for '$name'"
  fi
  _prune_tenant_backups "$name"
}

# Keep only the most recent $BACKUP_KEEP dumps for a tenant (0/unset = keep all).
_prune_tenant_backups() {
  local name="$1" dir="$BACKUP_DIR/$1"
  [[ "${BACKUP_KEEP:-0}" =~ ^[0-9]+$ && "$BACKUP_KEEP" -gt 0 ]] || return 0
  local -a dumps=("$dir"/*.dump)
  [[ -e "${dumps[0]:-}" ]] || return 0          # glob did not match -> nothing to prune
  local n=${#dumps[@]} i
  (( n > BACKUP_KEEP )) || return 0
  for (( i=0; i < n - BACKUP_KEEP; i++ )); do   # glob is ascending (timestamp) = oldest first
    rm -f "${dumps[$i]}"
    say "pruned old backup: ${dumps[$i]}"
  done
}

# List existing backups for one tenant or all.
list_tenant_backups() {
  local filter="${1:-}" names
  if [[ -n "$filter" ]]; then names="$(sanitize_name "$filter")"; else names="$(all_tenant_names)"; fi
  [[ -n "$names" ]] || { say "no tenants"; return 0; }
  say "=== Backups (under $BACKUP_DIR) ==="
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    say ""
    say "$name:"
    if compgen -G "$BACKUP_DIR/$name/*.dump" >/dev/null 2>&1; then
      ls -1sh "$BACKUP_DIR/$name"/*.dump 2>/dev/null | sed 's/^/  /'
    else
      say "  (no backups)"
    fi
  done <<< "$names"
}

# Restore a tenant's database from a custom-format dump. DESTRUCTIVE: pg_restore
# --clean --if-exists DROPs and recreates objects. Requires the tenant daemon to
# be stopped (no concurrent writes) and an explicit --yes.
restore_tenant_postgres() {
  local name="$1" file="$2" confirmed="${3:-false}"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  [[ -f "$file" ]] || die "backup file not found: $file"
  # Reject anything that is not a custom-format archive before touching the DB
  # (custom-format pg_dump files begin with the magic "PGDMP").
  [[ "$(head -c5 "$file" 2>/dev/null)" == "PGDMP" ]] \
    || die "not a custom-format pg_dump archive (missing PGDMP header): $file"
  [[ "$confirmed" == "true" ]] || die "restore DROPs and recreates the database for '$name'. Re-run with --yes to confirm."
  ensure_container_runtime
  ensure_init_system
  local container_name="lunarwing-pg-$name"

  _ctr_inspect "$name" -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
    || die "PostgreSQL not running for '$name' — start the tenant's pg container first"

  # Refuse unless we can POSITIVELY confirm the daemon is stopped (concurrent
  # writes corrupt a restore). Fail closed: if the service tool is missing or the
  # state is indeterminate, never assume "stopped".
  local daemon_state="unknown"
  if [[ "$INIT_SYSTEM" == "systemd" ]] && command -v systemctl >/dev/null 2>&1; then
    # systemctl's exit codes are per-state, not per-liveness: rc 3 covers
    # inactive AND activating/deactivating, so a crash-looping Restart=always
    # unit reads rc 3 while it restarts every few seconds. Capture the printed
    # state and classify rc+state pairs; only positively-stopped states count
    # (EQ-8 rc 4 = no such unit; FL-2: anything else is indeterminate and must
    # fail closed, never read as "stopped").
    local is_active_rc=0 is_active_out=""
    is_active_out="$(_systemctl_user "$name" is-active "lunarwing-${name}.service" 2>/dev/null)" \
      || is_active_rc=$?
    case "$is_active_rc:$is_active_out" in
      0:*) daemon_state="active" ;;
      3:inactive|3:failed|4:inactive) daemon_state="stopped" ;;
      *) daemon_state="unknown" ;;
    esac
  elif [[ "$INIT_SYSTEM" == "openrc" ]] && command -v rc-service >/dev/null 2>&1; then
    local status_rc=0
    _openrc_service "lunarwing-${name}" status >/dev/null 2>&1 || status_rc=$?
    case "$status_rc" in
      0) daemon_state="active" ;;
      3) daemon_state="stopped" ;;
      125) die "OpenRC proxy migration required; run: $0 render-units --all-proxies (restore refused)" ;;
      *) daemon_state="unknown" ;;
    esac
  fi
  case "$daemon_state" in
    stopped) : ;;
    active)  die "stop the daemon first: $0 stop-tenant $name  (restart after restore)" ;;
    *)       die "cannot confirm the '$name' daemon is stopped (no $INIT_SYSTEM service tool?); stop it manually, then re-run" ;;
  esac

  say "restoring '$name' from $file (single transaction, DROP + recreate) ..."
  # --single-transaction: all-or-nothing. A mid-restore failure rolls the whole
  # DROP+recreate back, so a failed restore never leaves the DB half-dropped.
  if _ctr "$name" exec -i "$container_name" pg_restore -U lunarwing -d lunarwing --single-transaction --clean --if-exists < "$file"; then
    say "restore complete for '$name'. Restart the tenant: $0 start-tenant $name"
  else
    die "pg_restore failed for '$name' — rolled back (single transaction); the database is unchanged"
  fi
}
