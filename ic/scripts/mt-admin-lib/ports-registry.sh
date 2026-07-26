#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# ports-registry.sh — part of the lunarwing-mt-admin library.
#
# The /etc/lunarwing/ports.json data layer: schema init, the v2..v11
# migration ladder, allocation/deallocation, feature enablement, and
# registry queries. No init-system or container awareness.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced, so this file only defines functions.

# ── Port registry ────────────────────────────────────────────────────────────

ports_registry_init() {
  local registry_version="" registry_needs_repair="false"
  if [[ -f "$PORTS_REGISTRY" ]]; then
    registry_version="$(jq -r '.version // 0' "$PORTS_REGISTRY" 2>/dev/null || true)"
    if jq -e '.tenants | to_entries[] | select(.value.extended_ports | has("darkirc_adapter") | not)' \
        "$PORTS_REGISTRY" >/dev/null 2>&1; then
      registry_needs_repair="true"
    fi
  fi
  if [[ ! -f "$PORTS_REGISTRY" || "$registry_version" != "11" || "$registry_needs_repair" == "true" ]]; then
    acquire_ports_lock
  fi

  if [[ ! -d /etc/lunarwing ]]; then
    mkdir -p /etc/lunarwing
    chmod 0755 /etc/lunarwing
  fi

  if [[ ! -f "$PORTS_REGISTRY" ]]; then
    local tmp
    tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    cat >"$tmp" <<'ENDJSON'
{
  "version": 3,
  "range": { "start": 10000, "end": 19999 },
  "block_size": 10,
  "tenants": {}
}
ENDJSON
    chmod 0644 "$tmp"
    mv "$tmp" "$PORTS_REGISTRY"
    say "initialized port registry: $PORTS_REGISTRY"
  fi

  ports_migrate
  release_ports_lock
}

ports_migrate_v2() {
  say "migrating port registry v1 -> v2 (reserved_0 -> orchestrator) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 2 |
    .tenants |= with_entries(
      .value.ports |= (
        if .reserved_0 then
          .orchestrator = .reserved_0 | del(.reserved_0)
        else
          .
        end
      )
    )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v2"
}

ports_migrate_v3() {
  say "migrating port registry v2 -> v3 (reserved_1 -> nanocode_wss) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 3 |
    .tenants |= with_entries(
      .value.ports |= (
        if .reserved_1 then
          .nanocode_wss = .reserved_1 | del(.reserved_1)
        else
          . + { nanocode_wss: (.orchestrator + 1) }
        end
      )
    )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v3"
}

ports_migrate_v4() {
  say "migrating port registry v3 -> v4 (reserved_2 -> pebble_wss) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 4 |
    .tenants |= with_entries(
      .value.ports |= (
        if .reserved_2 then
          .pebble_wss = .reserved_2 | del(.reserved_2)
        else
          . + { pebble_wss: (.orchestrator + 2) }
        end
      )
    )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v4"
}

ports_migrate_v5() {
  say "migrating port registry v4 -> v5 (reserved_3 -> weechat_adapter) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 5 |
    .tenants |= with_entries(
      .value.ports |= (
        if .reserved_3 then
          .weechat_adapter = .reserved_3 | del(.reserved_3)
        else
          . + { weechat_adapter: (.orchestrator + 3) }
        end
      )
    )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v5"
}

ports_migrate_v6() {
  say "migrating port registry v${current_version} -> v6 (add extended port range for overflow services) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 6
    | .extended_range = { "start": 20000, "end": 29999 }
    | .extended_block_size = (.block_size // 10)
    | ( .range.start // 10000 ) as $rstart
    | ( .extended_range.start ) as $estart
    | ( .extended_block_size ) as $bs
    | .tenants |= with_entries(
        .value |= (
          if .base_port then
            ( .base_port - $rstart + $estart ) as $eb
            | .extended_base = $eb
            | .extended_ports = (
                reduce range(0; $bs) as $i ({}; . + { ("reserved_\($i)"): ($eb + $i) })
              )
          else . end
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v6"
}

ports_migrate_v6_1() {
  # v6.1: assign darkirc_adapter to existing tenants from extended_ports[0].
  # Idempotent: detected by the absence of darkirc_adapter in extended_ports.
  if ! jq -e '.tenants | to_entries[] | select(.value.extended_ports | has("darkirc_adapter") | not)' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    return 0
  fi

  say "migrating port registry -> v6.1 (assign darkirc_adapter from extended slot 0) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .tenants |= with_entries(
      .value |= (
        if (.extended_ports | has("darkirc_adapter") | not) and (.extended_ports | type == "object") then
          .extended_ports.darkirc_adapter = .extended_ports.reserved_0
          | if (.extended_ports.reserved_0) then .extended_ports |= del(.reserved_0) else . end
        else . end
      )
    )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v6.1 (darkirc_adapter added)"
}

ports_migrate_v7() {
  say "migrating port registry v${current_version} -> v7 (assign darkirc_irc + darkirc_rpc from extended slots) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 7
    | .tenants |= with_entries(
        .value.extended_ports |= (
          if type == "object" then
            .darkirc_irc = ((.darkirc_irc) // (.reserved_1) // (.darkirc_adapter + 1))
            | .darkirc_rpc = ((.darkirc_rpc) // (.reserved_2) // (.darkirc_adapter + 2))
            | del(.reserved_1, .reserved_2)
          else . end
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  current_version=7
  say "port registry migrated to v7 (darkirc_irc + darkirc_rpc added)"
}

ports_migrate_v8() {
  say "migrating port registry v${current_version} -> v8 (dedicate nanocode_health + pebble_health) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 8
    | .tenants |= with_entries(
        .value |= (
          .extended_base as $eb
          | .extended_ports |= (
              if type == "object" then
                .nanocode_health = ((.nanocode_health) // (.reserved_3) // ($eb + 3))
                | .pebble_health = ((.pebble_health) // (.reserved_4) // ($eb + 4))
                | del(.reserved_3, .reserved_4)
              else . end
            )
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  current_version=8
}

ports_migrate() {
  [[ -f "$PORTS_REGISTRY" ]] || return 0
  require_cmd jq

  local current_version
  current_version="$(jq -r '.version // 0' "$PORTS_REGISTRY")"

  if [[ "$current_version" -lt 2 ]]; then ports_migrate_v2; current_version=2; fi
  if [[ "$current_version" -lt 3 ]]; then ports_migrate_v3; current_version=3; fi
  if [[ "$current_version" -lt 4 ]]; then ports_migrate_v4; current_version=4; fi
  if [[ "$current_version" -lt 5 ]]; then ports_migrate_v5; current_version=5; fi
  if [[ "$current_version" -lt 6 ]]; then ports_migrate_v6; current_version=6; fi
  ports_migrate_v6_1
  if [[ "$current_version" -lt 7 ]]; then ports_migrate_v7; fi
  if [[ "$current_version" -lt 8 ]]; then ports_migrate_v8; fi
  if [[ "$current_version" -lt 9 ]]; then ports_migrate_v9; fi
  if [[ "$current_version" -lt 10 ]]; then ports_migrate_v10; fi
  if [[ "$current_version" -lt 11 ]]; then ports_migrate_v11; fi
}

# v8 -> v9: rename extended_ports.reserved_5 -> vision_service.
ports_migrate_v9() {
  if jq -e '.tenants | to_entries[] | select(.value.extended_ports | has("vision_service") | not) | select(.value.extended_ports | has("reserved_5"))' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    say "migrating port registry -> v9 (assign vision_service from reserved_5 slot) ..."
    local tmp
    tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    jq '
      .tenants |= with_entries(
          .value |= (
            if (.extended_ports | type == "object") then
              .extended_ports |= (
                .vision_service = ((.vision_service) // (.reserved_5) // ((.extended_base // 0) + 5))
                | del(.reserved_5)
              )
            else . end
          )
        )
    ' "$PORTS_REGISTRY" >"$tmp"
    chmod 0644 "$tmp"
    mv "$tmp" "$PORTS_REGISTRY"
    say "port registry migrated to v9 (vision_service dedicated at extended_base+5)"
  fi

  # Always bump the version when the dispatcher calls v9, even if there were
  # no tenants to migrate (e.g. empty registry) — otherwise .version stays
  # stale and ports_allocate writes v9-shaped tenants into a v8-labeled file.
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '.version = 9' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
}

# v9 -> v10: dedicate extended_ports.reserved_6 -> vision_health.
# The vision sidecar now listens on two internal ports: 8088 (OCR_PORT) for
# API traffic and 8089 (OCR_HEALTH_PORT) for /health only. This dedicates a
# host-side port (extended_base+6) mapped to 8089 so the host self-heal
# pipeline can probe /health independently of OCR traffic — mirroring the
# nanocode_health / pebble_health pattern (v8).
ports_migrate_v10() {
  if jq -e '.tenants | to_entries[] | select(.value.extended_ports | has("vision_health") | not) | select(.value.extended_ports | has("reserved_6"))' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    say "migrating port registry -> v10 (assign vision_health from reserved_6 slot) ..."
    local tmp
    tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    jq '
      .tenants |= with_entries(
          .value |= (
            if (.extended_ports | type == "object") then
              .extended_ports |= (
                .vision_health = ((.vision_health) // (.reserved_6) // ((.extended_base // 0) + 6))
                | del(.reserved_6)
              )
            else . end
          )
        )
    ' "$PORTS_REGISTRY" >"$tmp"
    chmod 0644 "$tmp"
    mv "$tmp" "$PORTS_REGISTRY"
    say "port registry migrated to v10 (vision_health dedicated at extended_base+6)"
  fi

  # Always bump the version when the dispatcher calls v10, even if there were
  # no tenants to migrate (e.g. empty registry) — otherwise .version stays
  # stale and ports_allocate writes v10-shaped tenants into a v9-labeled file.
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '.version = 10' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
}

# v10 -> v11: dedicate extended_ports.reserved_7 -> opencode_wss,
# extended_ports.reserved_8 -> opencode_health.
ports_migrate_v11() {
  if jq -e '.tenants | to_entries[] | select(.value.extended_ports | has("opencode_wss") | not) | select(.value.extended_ports | has("reserved_7"))' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    say "migrating port registry -> v11 (assign opencode_wss from reserved_7 slot) ..."
    local tmp
    tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    jq '
      .tenants |= with_entries(
          .value |= (
            if (.extended_ports | type == "object") then
              .extended_ports |= (
                .opencode_wss = ((.opencode_wss) // (.reserved_7) // ((.extended_base // 0) + 7))
                | .opencode_health = ((.opencode_health) // (.reserved_8) // ((.extended_base // 0) + 8))
                | del(.reserved_7, .reserved_8)
              )
            else . end
          )
        )
    ' "$PORTS_REGISTRY" >"$tmp"
    chmod 0644 "$tmp"
    mv "$tmp" "$PORTS_REGISTRY"
    say "port registry migrated to v11 (opencode_wss + opencode_health dedicated at extended_base+7/+8)"
  fi

  # Always bump the version when the dispatcher calls v11, even if there were
  # no tenants to migrate (e.g. empty registry) — otherwise .version stays
  # stale and ports_allocate writes v11-shaped tenants into a v10-labeled file.
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '.version = 11' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
}

ports_allocate() {
  local name="$1"
  local enable_darkirc="${2:-false}"
  local enable_proxy="${3:-false}"
  # Selected external workers, as a JSON object literal
  # (e.g. '{"nanocode":true,"pebble":false,"opencode":false}'). Persisted so
  # start_tenant_<worker> can gate on the tenant's choice; see
  # tenant_worker_enabled. NOTE: assign in two steps — `${4:-{}}` mis-parses
  # (bash closes the ${...} at the first `}`, appending a stray `}` and
  # corrupting the JSON), so default explicitly instead.
  local workers_json="${4:-}"
  [[ -n "$workers_json" ]] || workers_json='{}'
  require_cmd jq
  acquire_ports_lock

  # Resumable (F4): if this tenant already has a block, reuse it (echo its
  # base_port) instead of dying — so re-running add-tenant after a mid-flow failure
  # resumes cleanly (clone/env/pg/render/pipeline are idempotent, and
  # tenant_pg_password is stable). Use `remove-tenant` to truly start over.
  local existing
  existing="$(jq -r ".tenants[\"$name\"].base_port // empty" "$PORTS_REGISTRY" 2>/dev/null || true)"
  if [[ -n "$existing" ]]; then
    say "tenant '$name' already has ports allocated (base $existing); reusing for resume" >&2
    # Reconcile the darkirc flag on resume (H2): add_tenant()'s env/unit writers
    # (write_tenant_lunarwing_env, render_tenant_*_units, start_tenant_*) gate on
    # tenant_darkirc_enabled (a registry read), but the adapter env/TOML writers
    # at add_tenant() use the in-memory flag. Without this flip, re-running
    # `add-tenant <existing> --enable-darkirc` leaves the tenant half-configured.
    # One-directional (false -> true): disabling darkirc post-provision is a
    # manual teardown (see docs/ops/DARKIRC-MULTITENANT.md).
    [[ "$enable_darkirc" == "true" ]] && ports_enable_darkirc "$name"
    [[ "$enable_proxy" == "true" ]] && ports_enable_proxy "$name"
    # Reconcile worker selection on resume, same one-directional false->true rule
    # as darkirc/proxy: re-running `add-tenant <existing> --with-<worker>` turns a
    # worker on; omitting the flag never turns one off (disabling is manual).
    local _w
    for _w in nanocode pebble opencode; do
      if [[ "$(jq -r --arg w "$_w" '.[$w] // false' <<<"$workers_json" 2>/dev/null)" == "true" ]]; then
        ports_enable_worker "$name" "$_w"
      fi
    done
    printf '%s' "$existing"
    release_ports_lock
    return 0
  fi

  local base=-1
  local p
  for ((p = PORT_RANGE_START; p <= PORT_RANGE_END - PORT_BLOCK_SIZE + 1; p += PORT_BLOCK_SIZE)); do
    if ! jq -e ".tenants | to_entries[] | select(.value.base_port == $p)" "$PORTS_REGISTRY" >/dev/null 2>&1; then
      base=$p
      break
    fi
  done

  [[ $base -ge 0 ]] || die "no free port blocks in range $PORT_RANGE_START-$PORT_RANGE_END"

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  local darkirc_json="false"
  [[ "$enable_darkirc" == "true" ]] && darkirc_json="true"
  local proxy_json="false"
  [[ "$enable_proxy" == "true" ]] && proxy_json="true"
  # Normalize the workers spec to a full {nanocode,pebble,opencode} bool map so
  # every tenant object has a stable, explicit shape (missing/garbage -> all off).
  local workers_norm
  workers_norm="$(jq -c '{
      nanocode: (.nanocode // false),
      pebble:   (.pebble   // false),
      opencode: (.opencode // false)
    }' <<<"$workers_json" 2>/dev/null || echo '{"nanocode":false,"pebble":false,"opencode":false}')"
  jq --arg name "$name" --argjson base "$base" --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson darkirc "$darkirc_json" --argjson proxy "$proxy_json" --argjson workers "$workers_norm" '
    ( .range.start // 10000 ) as $rstart
    | ( .extended_range.start // 20000 ) as $estart
    | ( .extended_block_size // 10 ) as $ebs
    | ( $base - $rstart + $estart ) as $ebase
    | .tenants[$name] = {
        base_port: $base,
        user: $name,
        created_at: $ts,
        enable_darkirc: $darkirc,
        enable_proxy: $proxy,
        workers: $workers,
        ports: {
          gateway:          ($base + 0),
          http:             ($base + 1),
          bridge:           ($base + 2),
          postgres:         ($base + 3),
          proxy:            ($base + 4),
          weechat:          ($base + 5),
          orchestrator:     ($base + 6),
          nanocode_wss:     ($base + 7),
          pebble_wss:       ($base + 8),
          weechat_adapter:  ($base + 9)
        },
        extended_base: $ebase,
        extended_ports: (
          {
            darkirc_adapter: $ebase,
            darkirc_irc: ($ebase + 1),
            darkirc_rpc: ($ebase + 2),
            nanocode_health: ($ebase + 3),
            pebble_health: ($ebase + 4),
            vision_service: ($ebase + 5),
            vision_health: ($ebase + 6),
            opencode_wss: ($ebase + 7),
            opencode_health: ($ebase + 8)
          }
          + (reduce range(9; $ebs) as $i ({}; . + { ("reserved_\($i)"): ($ebase + $i) }))
        )
      }
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"

  say "allocated port block $base-$((base + PORT_BLOCK_SIZE - 1)) for tenant '$name'" >&2
  printf '%s' "$base"
  release_ports_lock
}

# Mark darkirc enabled for a tenant in the registry (one-directional: false -> true).
# Idempotent: no-op if already enabled. Used by ports_allocate()'s resume path so
# `add-tenant <existing> --enable-darkirc` flips the registry (H2). Does NOT tear
# down darkirc (disabling is a manual operation; see DARKIRC-MULTITENANT.md).
ports_enable_darkirc() {
  local name="$1"
  require_cmd jq
  local current
  current="$(jq -r ".tenants[\"$name\"].enable_darkirc // false" "$PORTS_REGISTRY" 2>/dev/null || true)"
  [[ "$current" == "true" ]] && return 0

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" '.tenants[$name].enable_darkirc = true' \
      "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    die "failed to enable darkirc flag for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "darkirc for tenant '$name': disabled -> enabled" >&2
}

# Mark proxy enabled for a tenant in the registry (one-directional: false -> true).
# Idempotent. Used by ports_allocate()'s resume path and by the --enable-proxy flag.
ports_enable_proxy() {
  local name="$1"
  require_cmd jq
  local current
  current="$(jq -r ".tenants[\"$name\"].enable_proxy // false" "$PORTS_REGISTRY" 2>/dev/null || true)"
  [[ "$current" == "true" ]] && return 0

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" '.tenants[$name].enable_proxy = true' \
      "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    die "failed to enable proxy flag for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "proxy for tenant '$name': disabled -> enabled" >&2
}

# Mark a single external worker (nanocode|pebble|opencode) enabled for a tenant
# (one-directional: false -> true). Idempotent. Used by ports_allocate()'s resume
# path so `add-tenant <existing> --with-<worker>` flips the registry, mirroring
# ports_enable_darkirc/proxy. Ensures a .workers object exists on older tenant
# entries that predate the workers map.
ports_enable_worker() {
  local name="$1" worker="$2"
  require_cmd jq
  case "$worker" in
    nanocode|pebble|opencode) ;;
    *) die "ports_enable_worker: unknown worker '$worker'" ;;
  esac
  local current
  current="$(jq -r ".tenants[\"$name\"].workers[\"$worker\"] // false" "$PORTS_REGISTRY" 2>/dev/null || true)"
  [[ "$current" == "true" ]] && return 0

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" --arg w "$worker" '
      .tenants[$name].workers = ((.tenants[$name].workers // {}) + { ($w): true })
    ' "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    die "failed to enable worker '$worker' for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "worker '$worker' for tenant '$name': disabled -> enabled" >&2
}

ports_deallocate() {
  local name="$1"
  require_cmd jq
  acquire_ports_lock

  if ! jq -e ".tenants[\"$name\"]" "$PORTS_REGISTRY" >/dev/null 2>&1; then
    say "tenant '$name' not in port registry (already removed?)"
    release_ports_lock
    return 0
  fi

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq --arg name "$name" 'del(.tenants[$name])' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"

  say "deallocated ports for tenant '$name'"
  release_ports_lock
}

ports_get() {
  local name="$1" port_name="$2"
  require_cmd jq
  local port
  port="$(jq -r ".tenants[\"$name\"].ports.$port_name // empty" "$PORTS_REGISTRY")"
  if [[ -n "$port" ]]; then
    printf '%s' "$port"
    return 0
  fi
  port="$(jq -r ".tenants[\"$name\"].extended_ports.$port_name // empty" "$PORTS_REGISTRY")"
  if [[ -n "$port" ]]; then
    printf '%s' "$port"
    return 0
  fi
  # Back-compat: pre-v9 registries had `reserved_5` where `vision_service` now
  # lives. Read either name; new tenants only ever carry `vision_service`.
  if [[ "$port_name" == "vision_service" ]]; then
    port="$(jq -r ".tenants[\"$name\"].extended_ports.reserved_5 // empty" "$PORTS_REGISTRY")"
    [[ -n "$port" ]] && { printf '%s' "$port"; return 0; }
  fi
  # Back-compat: pre-v11 registries had `reserved_7`/`reserved_8` where
  # `opencode_wss`/`opencode_health` now live (renamed by ports_migrate_v11).
  # Read either name so an unmigrated tenant's opencode worker still resolves
  # its port instead of being skipped — mirrors the vision_service case above.
  if [[ "$port_name" == "opencode_wss" ]]; then
    port="$(jq -r ".tenants[\"$name\"].extended_ports.reserved_7 // empty" "$PORTS_REGISTRY")"
    [[ -n "$port" ]] && { printf '%s' "$port"; return 0; }
  fi
  if [[ "$port_name" == "opencode_health" ]]; then
    port="$(jq -r ".tenants[\"$name\"].extended_ports.reserved_8 // empty" "$PORTS_REGISTRY")"
    [[ -n "$port" ]] && { printf '%s' "$port"; return 0; }
  fi
  return 1
}

ports_list() {
  require_cmd jq
  if [[ ! -f "$PORTS_REGISTRY" ]]; then
    say "no port registry found; run add-tenant first"
    return 0
  fi
  jq -r '.tenants | to_entries[] | "\(.key)\t\(.value.ports.gateway)\t\(.value.ports.http)\t\(.value.ports.bridge)\t\(.value.ports.postgres)\t\(.value.ports.proxy)\t\(.value.ports.weechat)\t\(.value.ports.weechat_adapter // "-")\t\(.value.extended_ports.darkirc_adapter // "-")\t\(.value.extended_ports.darkirc_irc // "-")\t\(.value.extended_ports.darkirc_rpc // "-")\t\(.value.ports.orchestrator)\t\(.value.ports.nanocode_wss // "-")\t\(.value.ports.pebble_wss // "-")\t\(.value.extended_ports.vision_service // "-")\t\(.value.extended_ports.vision_health // "-")"' "$PORTS_REGISTRY" \
    | column -t -N "TENANT,GATEWAY,HTTP,BRIDGE,PG,PROXY,WEECHAT,WS_ADPT,DARKIRC_ADPT,DARKIRC_IRC,DARKIRC_RPC,ORCH,NANOCODE,PEBBLE,VISION_SVC,VISION_HLTH"
}

tenant_exists_in_registry() {
  local name="$1"
  require_cmd jq
  jq -e ".tenants[\"$name\"]" "$PORTS_REGISTRY" >/dev/null 2>&1
}

all_tenant_names() {
  require_cmd jq
  jq -r '.tenants | keys[]' "$PORTS_REGISTRY" 2>/dev/null
}

