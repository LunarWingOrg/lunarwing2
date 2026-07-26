#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# postgres.sh — part of the lunarwing-mt-admin library.
#
# Per-tenant PostgreSQL container lifecycle plus the pg_dump/pg_restore
# backup surface: start/stop, reset, password rotation, backup, prune,
# list, and restore.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced, so this file only defines functions.

# ── PostgreSQL container ─────────────────────────────────────────────────────

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
  if [[ "$MT_ROOTLESS" == "true" && "$CONTAINER_RT" == "podman" ]] \
     && ! _ctr "$name" inspect "$container_name" &>/dev/null \
     && "$CONTAINER_RT" inspect "$container_name" &>/dev/null; then
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
      q_health="$(_ctr "$name" inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{end}}' "$container_name" 2>/dev/null || true)"
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

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    if _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
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
    local pg_init_env
    pg_init_env="$(tenant_env_dir "$name")/.pg-init.env"
    ( umask 077; printf 'POSTGRES_PASSWORD=%s\n' "$(tenant_pg_password "$name")" >"$pg_init_env" )
    chown "$name:$name" "$pg_init_env" 2>/dev/null || true
    # O2: don't let a pg CREATE failure abort the whole provision (set -e). The F2
    # gate already downgrades a slow-readiness race to warn+continue; mirror that
    # for the create call (and the start above) so a transient runtime hiccup
    # doesn't strand the tenant half-baked (no units rendered, no health pipeline
    # installed). add-tenant is resumable (F4): fix the cause and re-run.
    if ! _ctr "$name" run -d \
      --name "$container_name" \
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
  ensure_container_runtime

  local container_name="lunarwing-pg-$name"
  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "PostgreSQL stopped ($container_name)"
  fi
}

reset_tenant_postgres() {
  local name="$1"
  ensure_container_runtime

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
  _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
    || die "PostgreSQL container $container_name is not running; start the tenant first"

  local envf pg_port new_pw secret_file tmp
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

  # Persist the new secret (source of truth) ...
  secret_file="$(tenant_env_dir "$name")/pg.secret"
  ( umask 077; printf '%s\n' "$new_pw" > "$secret_file" )
  chown "$name:$name" "$secret_file" 2>/dev/null || true

  # ... and rewrite DATABASE_URL in place (preserving the env file's owner/perms).
  # Temp lives beside the env file (0600), not in shared /tmp, so the cleartext
  # password isn't briefly exposed there — matching the convention used elsewhere.
  tmp="$(mktemp "$envf.tmp.XXXXXX")"
  sed "s#^DATABASE_URL=.*#DATABASE_URL=postgres://lunarwing:${new_pw}@127.0.0.1:${pg_port}/lunarwing#" "$envf" >"$tmp"
  cat "$tmp" >"$envf"
  rm -f "$tmp"

  say "Rotated PostgreSQL password for tenant '$name'."
  say "IMPORTANT: the running daemon still holds the old credential — restart to apply:"
  say "    lunarwing-mt-admin.sh restart-tenant $name"
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

  _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
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

  _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
    || die "PostgreSQL not running for '$name' — start the tenant's pg container first"

  # Refuse unless we can POSITIVELY confirm the daemon is stopped (concurrent
  # writes corrupt a restore). Fail closed: if the service tool is missing or the
  # state is indeterminate, never assume "stopped".
  local daemon_state="unknown"
  if [[ "$INIT_SYSTEM" == "systemd" ]] && command -v systemctl >/dev/null 2>&1; then
    if _systemctl_user "$name" is-active --quiet "lunarwing-${name}.service" 2>/dev/null; then daemon_state="active"; else daemon_state="stopped"; fi
  elif [[ "$INIT_SYSTEM" == "openrc" ]] && command -v rc-service >/dev/null 2>&1; then
    if rc-service "lunarwing-${name}" status >/dev/null 2>&1; then daemon_state="active"; else daemon_state="stopped"; fi
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

