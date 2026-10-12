#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 10-owner-scope.sh — part of the lunarwing-mt-admin library.
#
# Owner-scope database migration (multi-tenant memory rekey).
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced.
#
# Extracted from the v2.0.8.0 monolith (goals item #9), keeping the library
# structure of ref2/sunburst/mtadmin-refactor-v2.0.6.0 per
# docs/proposals/MT-ADMIN-DECOMPOSITION.md. Pure line-range move: no function
# body was rewritten.


# ── Owner-scope DB migration ──────────────────────────────────────────────────
#
# Rekeys all user_id='default' (or a specified old scope) rows to user_id=<name>
# in the tenant's PostgreSQL. Used when a tenant that was originally created
# without LUNARWING_OWNER_ID (running as 'default') is upgraded to have its own
# owner_id scope. Handles unique-constraint collisions by preserving the
# existing tenant-scoped row and copying content from the old row if the new
# one is empty.
#
owner_scope_tables() {
  printf '%s\n' \
    settings conversations memory_documents routines agent_jobs api_tokens \
    heartbeat_state reflex_patterns user_identities secrets wasm_tools \
    tool_rate_limit_state secret_usage_log leak_detection_events wasm_channels
}

# Print aggregate owner-scope counts as: <user_id><tab><row_count>
owner_scope_rows() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_container_runtime

  local container_name="lunarwing-pg-$name"
  _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
    || die "PostgreSQL not running for '$name' — start the tenant's pg container first"

  local psql_cmd="psql -U lunarwing -d lunarwing"
  local selects="" sep="" tbl exists
  while IFS= read -r tbl; do
    exists="$(cd / && _ctr "$name" exec "$container_name" $psql_cmd -tAc \
      "SELECT 1 FROM information_schema.columns WHERE table_schema='public' AND table_name='$tbl' AND column_name='user_id'" 2>/dev/null || true)"
    [[ "$exists" == "1" ]] || continue
    selects+="${sep}SELECT user_id, count(*)::bigint AS row_count FROM $tbl GROUP BY user_id"
    sep=" UNION ALL "
  done < <(owner_scope_tables)

  [[ -n "$selects" ]] || return 0
  local sql
  sql="SELECT user_id, sum(row_count)::bigint FROM ($selects) s GROUP BY user_id ORDER BY user_id;"
  cd / && _ctr "$name" exec "$container_name" \
    psql -U lunarwing -d lunarwing -tA -F $'\t' -c "$sql"
}

# Check whether a tenant's DB has orphaned old-scope rows that need migration.
# Returns 0 (needs migration) or 1 (already clean / DB unreachable).
# Usage: _owner_scope_needs_migration <name> [old_scope]
_owner_scope_needs_migration() {
  local name="$1"
  local old_scope="${2:-default}"
  # Confirm a postgres port is allocated (tenant is provisioned); we do NOT connect
  # over it — psql runs INSIDE the pg container via the local socket (see the note in
  # migrate_owner_scope). A host-published port isn't reachable from inside the
  # container under rootless podman.
  ports_get "$name" postgres >/dev/null 2>&1 || return 1

  # If the PG container isn't running, can't check — assume clean.
  _ctr "$name" inspect -f '{{.State.Running}}' "lunarwing-pg-$name" 2>/dev/null | grep -q true || return 1

  local count
  count="$(cd / && _ctr "$name" exec lunarwing-pg-$name \
    psql -U lunarwing -d lunarwing -tAc "
    SELECT count(*) FROM (
      SELECT user_id FROM settings WHERE user_id='$old_scope'
      UNION ALL SELECT user_id FROM conversations WHERE user_id='$old_scope'
      UNION ALL SELECT user_id FROM memory_documents WHERE user_id='$old_scope'
      UNION ALL SELECT user_id FROM secrets WHERE user_id='$old_scope'
      UNION ALL SELECT user_id FROM agent_jobs WHERE user_id='$old_scope'
    ) AS t;" 2>/dev/null || echo 0)"

  [[ "$count" -gt 0 ]] 2>/dev/null
}

# Usage: migrate_owner_scope <name> [--from <old_scope>]
migrate_owner_scope() {
  local name="$1"
  local old_scope="${2:-default}"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  # Self-wipe guard: if the source scope already equals the target tenant name there
  # is nothing to rekey — and proceeding would be CATASTROPHIC. The collision dedup
  # DELETEs self-join the table (DELETE ... d USING ... t WHERE d.user_id=old_scope
  # AND t.user_id=name AND <key match>); when old_scope==name every row matches
  # itself and is deleted before the no-op UPDATE, wiping the table. The default-only
  # auto-callers never reach this (old_scope='default'), but an explicit
  # `--from <name>` (or a future scope-detection caller) could.
  if [[ "$old_scope" == "$name" ]]; then
    say "owner scope '$name' already matches the tenant; nothing to rekey"
    return 0
  fi

  local pg_port
  pg_port="$(ports_get "$name" postgres)" || die "no postgres port for $name"

  # Quick check: are there any old-scope rows at all?
  if ! _owner_scope_needs_migration "$name" "$old_scope"; then
    say "no '$old_scope' rows found for $name; owner scope already clean"
    return 0
  fi

  say "migrating owner scope: '$old_scope' -> '$name' (pg port $pg_port)"

  # Tables with a user_id column (base tables only, not views).
  # Discovered via information_schema — kept as a static list so the migration
  # is deterministic and doesn't break if a view is added/renamed.
  local tables
  tables="$(owner_scope_tables)"

  # Stop the daemon first so it doesn't re-create 'default' rows mid-migration.
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    _systemctl_user "$name" stop "lunarwing-${name}.service" 2>/dev/null || true
  else
    _openrc_service "lunarwing-${name}" stop \
      || die "stop failed; refusing owner-scope migration for '$name'"
  fi

  # Connect over the container's LOCAL SOCKET — psql runs INSIDE the pg container via
  # `_ctr exec`, so NOT `-h 127.0.0.1 -p $pg_port`: that host-published port is not
  # reachable from inside the container under rootless podman, which silently broke
  # the rekey (the needs-check saw 0 rows -> "already clean" no-op, so a real
  # migration never ran). Mirrors restore_tenant_postgres (pg_restore -U lunarwing
  # -d lunarwing, no -h/-p). Do not add -h/-p back.
  local psql_cmd
  psql_cmd="psql -U lunarwing -d lunarwing"

  # Run the migration via the tenant's PG container. Each table's statements run
  # in ONE transaction (-1) with ON_ERROR_STOP, so a unique-constraint collision
  # or FK error rolls that table back atomically instead of half-applying — and
  # the error is SURFACED and counted (not swallowed) rather than aborting the
  # whole migration. Every table with a UNIQUE/PK on (user_id, ...) first deletes
  # the old-scope rows that would collide with an existing tenant-scoped row
  # (keeping the tenant row; memory_documents also salvages content into an empty
  # tenant row first), then updates. Tables with no user_id-bearing unique key get
  # a straight update.
  local total_migrated=0 migrate_errors=0
  for tbl in $tables; do
    # Check if the table exists in this DB (some may not if migrations haven't run).
    local exists
    exists="$(cd / && _ctr "$name" exec lunarwing-pg-$name $psql_cmd -tAc \
      "SELECT 1 FROM information_schema.tables WHERE table_name='$tbl' AND table_schema='public'" 2>/dev/null || true)"
    [[ "$exists" == "1" ]] || continue

    local sql
    case "$tbl" in
      settings)  # PRIMARY KEY (user_id, key)
        sql="DELETE FROM settings d USING settings t
               WHERE d.user_id='$old_scope' AND t.user_id='$name' AND d.key=t.key;
             UPDATE settings SET user_id='$name' WHERE user_id='$old_scope';" ;;
      memory_documents)  # UNIQUE (user_id, path, agent_id) NULLS NOT DISTINCT (V21)
        sql="UPDATE memory_documents t SET content = d.content
               FROM memory_documents d
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.path=t.path AND (d.agent_id IS NOT DISTINCT FROM t.agent_id)
                 AND (t.content IS NULL OR t.content = '');
             DELETE FROM memory_documents d USING memory_documents t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.path=t.path AND (d.agent_id IS NOT DISTINCT FROM t.agent_id);
             UPDATE memory_documents SET user_id='$name' WHERE user_id='$old_scope';" ;;
      secrets)  # UNIQUE (user_id, name)
        sql="DELETE FROM secrets d USING secrets t
               WHERE d.user_id='$old_scope' AND t.user_id='$name' AND d.name=t.name;
             UPDATE secrets SET user_id='$name' WHERE user_id='$old_scope';" ;;
      wasm_tools)  # UNIQUE (user_id, name, version)
        sql="DELETE FROM wasm_tools d USING wasm_tools t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.name=t.name AND d.version=t.version;
             UPDATE wasm_tools SET user_id='$name' WHERE user_id='$old_scope';" ;;
      routines)  # UNIQUE (user_id, name)
        sql="DELETE FROM routines d USING routines t
               WHERE d.user_id='$old_scope' AND t.user_id='$name' AND d.name=t.name;
             UPDATE routines SET user_id='$name' WHERE user_id='$old_scope';" ;;
      reflex_patterns)  # UNIQUE (user_id, normalized_pattern)
        sql="DELETE FROM reflex_patterns d USING reflex_patterns t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.normalized_pattern=t.normalized_pattern;
             UPDATE reflex_patterns SET user_id='$name' WHERE user_id='$old_scope';" ;;
      wasm_channels)  # UNIQUE (user_id, name)
        sql="DELETE FROM wasm_channels d USING wasm_channels t
               WHERE d.user_id='$old_scope' AND t.user_id='$name' AND d.name=t.name;
             UPDATE wasm_channels SET user_id='$name' WHERE user_id='$old_scope';" ;;
      heartbeat_state)  # UNIQUE (user_id, agent_id) — plain (NULLs distinct), so a
                        # NULL agent_id never collides; only dedup non-NULL matches.
        sql="DELETE FROM heartbeat_state d USING heartbeat_state t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.agent_id IS NOT NULL AND d.agent_id = t.agent_id;
             UPDATE heartbeat_state SET user_id='$name' WHERE user_id='$old_scope';" ;;
      tool_rate_limit_state)  # UNIQUE (wasm_tool_id, user_id)
        sql="DELETE FROM tool_rate_limit_state d USING tool_rate_limit_state t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.wasm_tool_id=t.wasm_tool_id;
             UPDATE tool_rate_limit_state SET user_id='$name' WHERE user_id='$old_scope';" ;;
      conversations)  # partial UNIQUE idx (user_id, routine_id) + (user_id) heartbeat singleton (V11)
        sql="DELETE FROM conversations d USING conversations t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.metadata->>'routine_id' IS NOT NULL
                 AND d.metadata->>'routine_id' = t.metadata->>'routine_id';
             DELETE FROM conversations d USING conversations t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.metadata->>'thread_type'='heartbeat'
                 AND t.metadata->>'thread_type'='heartbeat';
             UPDATE conversations SET user_id='$name' WHERE user_id='$old_scope';" ;;
      *)  # agent_jobs, api_tokens, user_identities, secret_usage_log, leak_detection_events: no user_id-bearing unique key
        sql="UPDATE $tbl SET user_id='$name' WHERE user_id='$old_scope';" ;;
    esac

    local out rc=0
    out="$(cd / && _ctr "$name" exec lunarwing-pg-$name \
      $psql_cmd -1 -v ON_ERROR_STOP=1 -c "$sql" 2>&1)" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
      say "  WARNING: $tbl rekey failed (rc=$rc): ${out//$'\n'/ }"
      migrate_errors=$((migrate_errors + 1))
    fi

    local count
    count="$(cd / && _ctr "$name" exec lunarwing-pg-$name $psql_cmd -tAc \
      "SELECT count(*) FROM $tbl WHERE user_id='$name'" 2>/dev/null || echo 0)"
    say "  $tbl: $count rows now scoped to '$name'"
    total_migrated=$((total_migrated + count))
  done

  # Verify no old-scope rows remain — check ALL owner-scoped tables (not just 5)
  # and name the offenders so a swallowed collision can't hide a partial rekey.
  local remaining_total=0 offenders="" unchecked=""
  for tbl in $tables; do
    local exists2 r
    exists2="$(cd / && _ctr "$name" exec lunarwing-pg-$name $psql_cmd -tAc \
      "SELECT 1 FROM information_schema.tables WHERE table_name='$tbl' AND table_schema='public'" 2>/dev/null || true)"
    [[ "$exists2" == "1" ]] || continue
    r="$(cd / && _ctr "$name" exec lunarwing-pg-$name $psql_cmd -tAc \
      "SELECT count(*) FROM $tbl WHERE user_id='$old_scope'" 2>/dev/null || echo "?")"
    if [[ "$r" =~ ^[0-9]+$ ]]; then
      [[ "$r" -gt 0 ]] && { offenders+=" $tbl($r)"; remaining_total=$((remaining_total + r)); }
    else
      unchecked+=" $tbl"
    fi
  done

  say ""
  [[ "$migrate_errors" -gt 0 ]] && \
    say "WARNING: $migrate_errors table(s) errored during rekey — see the WARNINGs above"
  if [[ -n "$offenders" ]]; then
    say "WARNING: $remaining_total '$old_scope' row(s) still remain after migration:$offenders"
    say "  (usually a unique-constraint collision or a users-table FK gap — inspect before starting)"
  elif [[ -n "$unchecked" ]]; then
    say "migration done; could not re-verify:$unchecked (DB read failed)"
  else
    say "migration complete: no '$old_scope' rows remain in any owner-scoped table"
  fi
  [[ "$migrate_errors" -eq 0 && -z "$offenders" && -z "$unchecked" ]] || \
    say "review the WARNINGs above before running 'start-tenant'"
  say "restart the tenant: $0 start-tenant $name"
}
