#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 03-ports-registry.sh — part of the lunarwing-mt-admin library.
#
# The /etc/lunarwing/ports.json port registry: schema init, the v2..v16
# migration ladder, allocation/deallocation, feature slots, and registry
# queries. Pure data layer.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced.
#
# Extracted from the v2.0.8.0 monolith (goals item #9), keeping the library
# structure of ref2/sunburst/mtadmin-refactor-v2.0.6.0 per
# docs/proposals/MT-ADMIN-DECOMPOSITION.md. Pure line-range move: no function
# body was rewritten.


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
  if [[ ! -f "$PORTS_REGISTRY" || "$registry_version" != "17" || "$registry_needs_repair" == "true" ]]; then
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
  # ports_migrate may have bumped to the current version; verify and release.
  release_ports_lock
}

# Inspection commands must not create or migrate host state.
ports_registry_require_readonly() {
  require_cmd jq
  [[ -f "$PORTS_REGISTRY" && ! -L "$PORTS_REGISTRY" ]] \
    || die "port registry not found: $PORTS_REGISTRY"
  jq -e '.tenants | type == "object"' "$PORTS_REGISTRY" >/dev/null 2>&1 \
    || die "invalid port registry: $PORTS_REGISTRY"
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

# v7 -> v8: dedicate extended_ports.reserved_3 -> nanocode_health and
# reserved_4 -> pebble_health (pure rename). Mirrors the standalone
# scripts/migrate-ports-v8.sh's safety net (WI#379): back the live registry up
# first, then refuse to swap a candidate that leaves any numeric port assigned
# to more than one slot — on refusal the backup stays and the live registry is
# untouched.
ports_migrate_v8() {
  say "migrating port registry v${current_version} -> v8 (dedicate nanocode_health + pebble_health) ..."
  local backup tmp collisions
  backup="${PORTS_REGISTRY}.bak.$(date -u +%Y%m%d%H%M%S)"
  cp -p "$PORTS_REGISTRY" "$backup" \
    || die "failed to back up port registry to $backup; v8 migration aborted"
  say "backed up port registry to $backup"

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
  ' "$PORTS_REGISTRY" >"$tmp" \
    || { rm -f "$tmp"; die "failed to migrate port registry to v8; registry unchanged (backup: $backup)"; }

  collisions="$(ports_registry_collision_list "$tmp")"
  if [[ -n "$collisions" ]]; then
    rm -f "$tmp"
    die "v8 migration aborted: port(s) $collisions assigned to more than one slot; registry unchanged (backup: $backup)"
  fi

  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  current_version=8
  say "port registry migrated to v8 (nanocode_health + pebble_health dedicated)"
  say "rollback if needed: cp $backup $PORTS_REGISTRY"
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
  if [[ "$current_version" -lt 12 ]]; then ports_migrate_v12; fi
  if [[ "$current_version" -lt 13 ]]; then ports_migrate_v13; fi
  if [[ "$current_version" -lt 14 ]]; then ports_migrate_v14; fi
  if [[ "$current_version" -lt 15 ]]; then ports_migrate_v15; fi
  if [[ "$current_version" -lt 16 ]]; then ports_migrate_v16; fi
  if [[ "$current_version" -lt 17 ]]; then ports_migrate_v17; fi
}

# v15 -> v16: allocate a host-global `health_metrics` slot for the optional
# loopback-only LunarWing healthpoint metrics listener (proposal
# HEALTHPOINT_METRICS_EXPORTER.md Phase 2). Tenant-scoped listeners are
# explicitly NOT planned — one host-global exporter carries per-tenant labels —
# so this is a single top-level port, allocated above every existing range
# (voice ends at 49999) so no live tenant port can move.
ports_migrate_v16() {
  local tmp existing
  existing="$(jq -r '.health_metrics_port // empty' "$PORTS_REGISTRY" 2>/dev/null || true)"
  if [[ -n "$existing" ]] && jq -e --argjson p "$existing" '
      [ .tenants[]
        | ((.ports // {}) | to_entries[] | .value),
          ((.extended_ports // {}) | to_entries[] | .value),
          ((.kimi_ports // {}) | to_entries[] | .value),
          ((.voice_ports // {}) | to_entries[] | .value)
      ] | any(. == $p)
    ' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    # Pre-existing slot collides with a tenant port (hand-edited registry).
    # Refuse rather than silently moving a live port; operator must resolve.
    die "health_metrics_port $existing collides with a tenant port; fix $PORTS_REGISTRY manually"
  fi
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '.version = 16 | .health_metrics_port = ((.health_metrics_port) // 50000)' \
    "$PORTS_REGISTRY" >"$tmp" \
    || { rm -f "$tmp"; die "failed to migrate port registry to v16"; }
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v16 (host-global health_metrics slot: $(jq -r '.health_metrics_port' "$PORTS_REGISTRY"))"
}

# v16 -> v17: add a dedicated two-port range for the OpenCode 2 worker
# (opencode2_wss + opencode2_health). Every existing range is full (the
# extended block exhausted its slots at v13, kimi took 30000-39999 at v12,
# voice took 40000-49999 at v15, health_metrics took 50000 at v16), so a new
# range starting at 51000 keeps every live tenant port stable — same
# non-disruptive pattern as the kimi (v12) and voice (v15) migrations.
# v2 is opt-in per tenant; the worker stays disabled (.workers.opencode2 =
# false) until explicitly selected.
ports_migrate_v17() {
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 17
    | .opencode2_range = ((.opencode2_range) // {"start": 51000, "end": 51999})
    | .opencode2_block_size = ((.opencode2_block_size) // 2)
    | ( .range.start // 10000 ) as $rstart
    | ( .opencode2_range.start ) as $ostart
    | .tenants |= with_entries(
        .value |= (
          if .base_port then
            ( .base_port - $rstart + $ostart ) as $obase
            | .opencode2_base = ((.opencode2_base) // $obase)
            | .opencode2_ports = ((.opencode2_ports) // {
                opencode2_wss: ($obase + 0),
                opencode2_health: ($obase + 1)
              })
          else . end
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v17 (opencode2_wss + opencode2_health dedicated in range 51000-51999)"
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

# v11 -> v12: add a dedicated kimi port range (30000-39999, block_size 2)
# with per-tenant kimi_ports {kimi_wss, kimi_health}. The existing extended
# block (20000-29999, block_size 10) has only one free slot (reserved_9), which
# is insufficient for two ports. Using a separate range avoids re-spacing
# existing tenants' extended blocks (which would require re-render + restart
# of every running tenant) and is fully non-disruptive.
ports_migrate_v12() {
  if ! jq -e '.tenants | to_entries[] | select(.value.kimi_ports | has("kimi_wss") | not)' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    # No tenants need kimi_ports; just bump the version.
    local tmp
    tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    jq '.version = 12
      | .kimi_range = ((.kimi_range) // {"start": 30000, "end": 39999})
      | .kimi_block_size = ((.kimi_block_size) // 2)' "$PORTS_REGISTRY" >"$tmp"
    chmod 0644 "$tmp"
    mv "$tmp" "$PORTS_REGISTRY"
    return 0
  fi

  say "migrating port registry -> v12 (add kimi_wss + kimi_health from dedicated range) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 12
    | .kimi_range = ((.kimi_range) // {"start": 30000, "end": 39999})
    | .kimi_block_size = ((.kimi_block_size) // 2)
    | ( .range.start // 10000 ) as $rstart
    | ( .kimi_range.start ) as $kstart
    | .tenants |= with_entries(
        .value |= (
          if .base_port then
            ( .base_port - $rstart + $kstart ) as $kbase
            | .kimi_base = $kbase
            | .kimi_ports = {
                kimi_wss: ($kbase + 0),
                kimi_health: ($kbase + 1)
              }
          else . end
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v12 (kimi_wss + kimi_health dedicated in kimi_range 30000-39999)"
}

# v12 -> v13: dedicate the final extended slot to the per-tenant coordination
# admin server. Existing units are unaffected until coordination is enabled.
ports_migrate_v13() {
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 13
    | .tenants |= with_entries(
        .value |= (
          if (.extended_base | type) == "number" and ((.extended_ports | type) != "object" or (.extended_ports | has("coord_admin") | not)) then
            .extended_ports = (if (.extended_ports | type) == "object" then .extended_ports else {} end)
            | .extended_ports.coord_admin = (.extended_ports.reserved_9 // (.extended_base + 9))
            | .extended_ports |= del(.reserved_9)
          else . end
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v13 (coord_admin dedicated at extended_base+9)"
}

# v13 -> v14: stable non-secret identity for every DarkIRC-enabled tenant.
ports_migrate_v14() {
  local name tmp next scope
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  cp "$PORTS_REGISTRY" "$tmp"
  if ! jq -e '
      [(.tenants // {}) | to_entries[]
        | select(.value.enable_darkirc == true)
        | (.value.darkirc_scope_id // "")
        | select(length > 0)] as $ids
      | all($ids[]; test("^[0-9a-f]{32}$") and . != "00000000000000000000000000000000")
      and (($ids | unique | length) == ($ids | length))
    ' "$tmp" >/dev/null; then
    rm -f "$tmp"
    die "failed to migrate port registry to v14: invalid or duplicate scope ID"
  fi
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    scope="$(jq -r --arg name "$name" '.tenants[$name].darkirc_scope_id // empty' "$tmp")"
    [[ -z "$scope" ]] || continue
    while :; do
      scope="$(generate_darkirc_scope_id)"
      if ! jq -e --arg scope "$scope" \
        '.tenants // {} | to_entries | any(.value.darkirc_scope_id == $scope)' \
        "$tmp" >/dev/null; then
        break
      fi
    done
    next="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    jq --arg name "$name" --arg scope "$scope" \
      '.tenants[$name].darkirc_scope_id = $scope' "$tmp" >"$next" \
      || { rm -f "$tmp" "$next"; die "failed to assign DarkIRC scope ID"; }
    mv "$next" "$tmp"
  done < <(jq -r '.tenants // {} | to_entries[] | select(.value.enable_darkirc == true) | .key' "$tmp")
  next="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '.version = 14' "$tmp" >"$next" \
    || { rm -f "$tmp" "$next"; die "failed to migrate port registry to v14"; }
  mv "$next" "$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v14 (stable DarkIRC scope IDs)"
}

# v14 -> v15: add an isolated two-port range for LunarVoice. Existing base and
# extended ranges are full; a separate range keeps every live tenant port stable.
ports_migrate_v15() {
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 15
    | .voice_range = ((.voice_range) // {"start": 40000, "end": 49999})
    | .voice_block_size = ((.voice_block_size) // 2)
    | ( .range.start // 10000 ) as $rstart
    | ( .voice_range.start ) as $vstart
    | .tenants |= with_entries(
        .value |= (
          if .base_port then
            ( .base_port - $rstart + $vstart ) as $vbase
            | .enable_voice = ((.enable_voice) // false)
            | .voice_base = ((.voice_base) // $vbase)
            | .voice_ports = ((.voice_ports) // {
                voice_service: ($vbase + 0),
                voice_health: ($vbase + 1)
              })
          else . end
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v15 (LunarVoice service + health ports)"
}

ports_allocate() {
  local name="$1"
  local enable_darkirc="${2:-false}"
  local enable_proxy="${3:-false}"
  # Selected external workers, as a JSON object literal. Persisted so
  # start_tenant_<worker> can gate on the tenant's choice; see
  # tenant_worker_enabled. NOTE: assign in two steps — `${4:-{}}` mis-parses
  # (bash closes the ${...} at the first `}`, appending a stray `}` and
  # corrupting the JSON), so default explicitly instead.
  local workers_json="${4:-}"
  local requested_darkirc_scope="${5:-}"
  [[ -n "$workers_json" ]] || workers_json='{}'
  require_cmd jq
  acquire_ports_lock
  if [[ -n "$requested_darkirc_scope" ]]; then
    [[ "$enable_darkirc" == "true" ]] \
      || die "DarkIRC import scope requires --enable-darkirc"
    validate_darkirc_scope_id "$requested_darkirc_scope"
    jq -e --arg scope "$requested_darkirc_scope" --arg name "$name" \
      '.tenants // {} | to_entries | any(.key != $name and .value.darkirc_scope_id == $scope)' \
      "$PORTS_REGISTRY" >/dev/null 2>&1 \
      && die "DarkIRC import scope is already assigned"
  fi

  # Resumable (F4): if this tenant already has a block, reuse it (echo its
  # base_port) instead of dying — so re-running add-tenant after a mid-flow failure
  # resumes cleanly (clone/env/pg/render/pipeline are idempotent, and
  # tenant_pg_password is stable). Use `remove-tenant` to truly start over.
  local existing
  existing="$(jq -r ".tenants[\"$name\"].base_port // empty" "$PORTS_REGISTRY" 2>/dev/null || true)"
  if [[ -n "$existing" ]]; then
    if [[ -n "$requested_darkirc_scope" ]]; then
      local existing_scope
      existing_scope="$(jq -r --arg name "$name" '.tenants[$name].darkirc_scope_id // empty' "$PORTS_REGISTRY")"
      [[ "$existing_scope" == "$requested_darkirc_scope" ]] \
        || die "existing tenant has a different DarkIRC scope"
    fi
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
    for _w in pebble opencode kimi opencode2; do
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
  # Keep the retired nanocode field false so existing registry consumers retain
  # a stable shape and no historical port or worker slot gets reused.
  local workers_norm
  workers_norm="$(jq -c '{
      nanocode: false,
      pebble:   (.pebble   // false),
      opencode: (.opencode // false),
      kimi:     (.kimi     // false),
      opencode2: (.opencode2 // false)
    }' <<<"$workers_json" 2>/dev/null || echo '{"nanocode":false,"pebble":false,"opencode":false,"kimi":false,"opencode2":false}')"
  local darkirc_scope=""
  if [[ "$enable_darkirc" == true ]]; then
    darkirc_scope="${requested_darkirc_scope:-$(generate_unique_darkirc_scope_id)}"
  fi
  jq --arg name "$name" --argjson base "$base" --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson darkirc "$darkirc_json" --arg scope "$darkirc_scope" --argjson proxy "$proxy_json" --argjson workers "$workers_norm" '
    ( .range.start // 10000 ) as $rstart
    | ( .extended_range.start // 20000 ) as $estart
    | ( .extended_block_size // 10 ) as $ebs
    | ( .kimi_range.start // 30000 ) as $kstart
    | ( .voice_range.start // 40000 ) as $vstart
    | ( .opencode2_range.start // 51000 ) as $ostart
    | ( $base - $rstart + $estart ) as $ebase
    | ( $base - $rstart + $kstart ) as $kbase
    | ( $base - $rstart + $vstart ) as $vbase
    | ( $base - $rstart + $ostart ) as $obase
    | .tenants[$name] = {
        base_port: $base,
        user: $name,
        created_at: $ts,
        enable_darkirc: $darkirc,
        darkirc_scope_id: (if $darkirc then $scope else null end),
        enable_proxy: $proxy,
        enable_voice: false,
        enable_weechat: true,
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
            opencode_health: ($ebase + 8),
            coord_admin: ($ebase + 9)
          }
          + (reduce range(10; $ebs) as $i ({}; . + { ("reserved_\($i)"): ($ebase + $i) }))
        ),
        kimi_base: $kbase,
        kimi_ports: {
          kimi_wss: ($kbase + 0),
          kimi_health: ($kbase + 1)
        },
        voice_base: $vbase,
        voice_ports: {
          voice_service: ($vbase + 0),
          voice_health: ($vbase + 1)
        },
        opencode2_base: $obase,
        opencode2_ports: {
          opencode2_wss: ($obase + 0),
          opencode2_health: ($obase + 1)
        }
      }
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"

  say "allocated port block $base-$((base + PORT_BLOCK_SIZE - 1)) for tenant '$name'" >&2
  # ISSUE-65: the registry only tracks what mt-admin itself allocated, so a
  # fresh block can still collide with a live listener mt-admin does not know
  # about (stale container from a removed tenant, dev server, hand-started
  # service). Surface that now, as a warning, instead of later as an opaque
  # EADDRINUSE unit failure. Non-fatal by design.
  ports_warn_block_in_use "$name"
  printf '%s' "$base"
  release_ports_lock
}

# True if a process is accepting TCP connections on 127.0.0.1:<port>. Same
# /dev/tcp idiom as doctor's sshd check; on loopback a closed port refuses
# instantly, so the 2s timeout is only a defensive bound.
_port_in_use() {
  local port="$1"
  timeout 2 bash -c "exec 3<>/dev/tcp/127.0.0.1/$port" 2>/dev/null
}

# Warn (stderr, non-fatal) for every port assigned to tenant '$name' in the
# registry that is already accepting connections on 127.0.0.1. Reads all five
# per-tenant port maps so extended/kimi/voice/opencode2 slots are covered too.
# Only meaningful right after a fresh ports_allocate() — on a running tenant
# its own services legitimately hold its ports, so callers must not probe those.
ports_warn_block_in_use() {
  local name="$1"
  require_cmd jq
  [[ -f "$PORTS_REGISTRY" ]] || return 0
  local entry port in_use=""
  while IFS=$'\t' read -r entry port; do
    # Skip blank and non-numeric values (hand-edited registry): the port is
    # interpolated into a bash -c probe command line, so validate first.
    [[ "$port" =~ ^[0-9]+$ ]] || continue
    if _port_in_use "$port"; then
      in_use+="${in_use:+, }$entry=$port"
    fi
  done < <(jq -r --arg name "$name" '
    .tenants[$name] // {}
    | [ ((.ports // {}) | to_entries[]),
        ((.extended_ports // {}) | to_entries[]),
        ((.kimi_ports // {}) | to_entries[]),
        ((.voice_ports // {}) | to_entries[]),
        ((.opencode2_ports // {}) | to_entries[]) ]
    | sort_by(.value)[]
    | "\(.key)\t\(.value)"' "$PORTS_REGISTRY" 2>/dev/null)
  if [[ -n "$in_use" ]]; then
    say "WARNING: port collision for tenant '$name': freshly allocated port(s) already in use on 127.0.0.1: $in_use" >&2
    say "         another process is listening there (stale container, dev server, or a removed tenant's leftover);" >&2
    say "         units for '$name' will fail to bind until those ports are freed or the registry is repaired." >&2
  fi
  return 0
}

# List numeric port numbers assigned to more than one slot in <registry-file>
# (all per-tenant port maps plus the host-global health_metrics slot) as a
# comma-separated string; empty output means no collisions. Non-numeric hand
# edits are ignored. Shared by the post-allocation warning / doctor query and
# the v8 migration's pre-swap guard (WI#379) so both define "collision"
# identically.
ports_registry_collision_list() {  # <registry-file>
  require_cmd jq
  jq -r '
    [ .tenants[]
      | ((.ports // {}) | to_entries[] | .value),
        ((.extended_ports // {}) | to_entries[] | .value),
        ((.kimi_ports // {}) | to_entries[] | .value),
        ((.voice_ports // {}) | to_entries[] | .value),
        ((.opencode2_ports // {}) | to_entries[] | .value)
    ] + [(.health_metrics_port // empty)]
    | map(select(type == "number"))
    | group_by(.)
    | map(select(length > 1) | .[0] | tostring)
    | join(", ")
  ' "$1" 2>/dev/null || true
}

# List port numbers assigned to more than one slot across the whole registry.
# Empty output means no collisions. Pure read — used by doctor; also handy for
# operators debugging a hand-edited or migrated registry.
ports_registry_duplicate_ports() {
  [[ -f "$PORTS_REGISTRY" ]] || return 0
  ports_registry_collision_list "$PORTS_REGISTRY"
}

# Mark darkirc enabled for a tenant in the registry (one-directional: false -> true).
# Idempotent: no-op if already enabled. Used by ports_allocate()'s resume path so
# `add-tenant <existing> --enable-darkirc` flips the registry (H2). Does NOT tear
# down darkirc (disabling is a manual operation; see DARKIRC-MULTITENANT.md).
ports_enable_darkirc() {
  local name="$1"
  require_cmd jq
  # ISSUE-012 (follow-up): registry read-modify-rename must run under the shared
  # ports lock, same as ports_set_weechat. Reentrancy-safe: when called from
  # ports_allocate() (which already holds the lock) acquire is a no-op and we
  # must NOT release the caller's lock — only release if we acquired it here.
  local had_lock="$PORTS_LOCK_HELD"
  acquire_ports_lock
  if ! jq -e ".tenants[\"$name\"]" "$PORTS_REGISTRY" >/dev/null 2>&1; then
    [[ "$had_lock" == "true" ]] || release_ports_lock
    die "tenant '$name' not in port registry"
  fi
  local current
  current="$(jq -r ".tenants[\"$name\"].enable_darkirc // false" "$PORTS_REGISTRY" 2>/dev/null || true)"
  if [[ "$current" == "true" ]]; then
    ensure_darkirc_scope_id "$name" >/dev/null
    [[ "$had_lock" == "true" ]] || release_ports_lock
    return 0
  fi

  local tmp scope_id
  scope_id="$(generate_unique_darkirc_scope_id)"
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" --arg scope "$scope_id" \
      '.tenants[$name].enable_darkirc = true | .tenants[$name].darkirc_scope_id = $scope' \
      "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    [[ "$had_lock" == "true" ]] || release_ports_lock
    die "failed to enable darkirc flag for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  [[ "$had_lock" == "true" ]] || release_ports_lock
  say "darkirc for tenant '$name': disabled -> enabled" >&2
}

# Mark proxy enabled for a tenant in the registry (one-directional: false -> true).
# Idempotent. Used by ports_allocate()'s resume path and by the --enable-proxy flag.
ports_enable_proxy() {
  local name="$1"
  require_cmd jq
  # ISSUE-012 (follow-up): same locked read-modify-rename pattern as
  # ports_enable_darkirc — see comment there for the reentrancy contract.
  local had_lock="$PORTS_LOCK_HELD"
  acquire_ports_lock
  if ! jq -e ".tenants[\"$name\"]" "$PORTS_REGISTRY" >/dev/null 2>&1; then
    [[ "$had_lock" == "true" ]] || release_ports_lock
    die "tenant '$name' not in port registry"
  fi
  local current
  current="$(jq -r ".tenants[\"$name\"].enable_proxy // false" "$PORTS_REGISTRY" 2>/dev/null || true)"
  if [[ "$current" == "true" ]]; then
    [[ "$had_lock" == "true" ]] || release_ports_lock
    return 0
  fi

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" '.tenants[$name].enable_proxy = true' \
      "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    [[ "$had_lock" == "true" ]] || release_ports_lock
    die "failed to enable proxy flag for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  [[ "$had_lock" == "true" ]] || release_ports_lock
  say "proxy for tenant '$name': disabled -> enabled" >&2
}

ports_set_voice() {
  local name="$1" enabled="$2" tmp
  require_cmd jq
  case "$enabled" in true|false) ;; *) die "ports_set_voice: enabled must be true or false" ;; esac
  acquire_ports_lock
  if ! jq -e --arg name "$name" '.tenants[$name]' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    release_ports_lock
    die "tenant '$name' not in port registry"
  fi
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" --argjson enabled "$enabled" \
      '.tenants[$name].enable_voice = $enabled' "$PORTS_REGISTRY" >"$tmp"; then
    rm -f "$tmp"
    release_ports_lock
    die "failed to set LunarVoice flag for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  release_ports_lock
}

tenant_vision_mode() {
  jq -r --arg name "$1" '.tenants[$name].vision.mode // "local"' \
    "$PORTS_REGISTRY" 2>/dev/null
}

tenant_vision_external() {
  [[ "$(tenant_vision_mode "$1")" == "external" ]]
}

tenant_vision_external_url() {
  jq -r --arg name "$1" '.tenants[$name].vision.external_url // empty' \
    "$PORTS_REGISTRY" 2>/dev/null
}

# Select the tenant's LunarVision deployment. The external URL is operational
# metadata, not a credential; health tooling reads it from the registry while
# the bearer token remains only in the tenant's mode-0600 environment files.
# An absent vision object means local, preserving older registries.
ports_set_vision_mode() {
  local name="$1" mode="$2" external_url="${3:-}" tmp
  require_cmd jq
  case "$mode" in
    local) external_url="" ;;
    external) [[ -n "$external_url" ]] || die "external LunarVision URL is required" ;;
    *) die "ports_set_vision_mode: mode must be local or external" ;;
  esac
  acquire_ports_lock
  if ! jq -e --arg name "$name" '.tenants[$name]' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    release_ports_lock
    die "tenant '$name' not in port registry"
  fi
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" --arg mode "$mode" --arg url "$external_url" '
      .tenants[$name].vision = if $mode == "external" then
        {mode: "external", external_url: $url}
      else
        {mode: "local"}
      end
    ' "$PORTS_REGISTRY" >"$tmp"; then
    rm -f "$tmp"
    release_ports_lock
    die "failed to set LunarVision mode for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  release_ports_lock
}

# Set the per-tenant WeeChat health-gate flag in the registry (bidirectional).
# enable_weechat=false tells the infrastructure health checks (health-openrc.sh /
# health-systemd.sh) to classify the tenant's OPTIONAL lunarwing-weechat-<t> and
# lunarwing-weechat-adapter-<t> units as `skipped` when down, so an intentionally
# unused WeeChat backend no longer reads critical and self-heal does not
# restart-storm (flap) a unit that cannot come up. Absent flag == enabled, so
# tenants created before this field existed keep the old behavior.
# The flag is authoritative for lifecycle too (ISSUE-006): the
# disable-weechat / enable-weechat verbs stop/disable resp. enable/start the
# units after flipping it, and start-tenant respects it on both init systems.
# This function only mutates the registry (under the shared ports lock).
# ISSUE-012: runs under the shared ports lock like every other registry
# mutation, and revalidates tenant existence while holding it — two unlocked
# read-modify-rename writers could drop each other's updates, and a concurrent
# remove-tenant could be followed by recreation of a partial tenant object.
ports_set_weechat() {
  local name="$1" enabled="$2"
  require_cmd jq
  case "$enabled" in true|false) ;; *) die "ports_set_weechat: enabled must be true or false" ;; esac
  acquire_ports_lock
  local current
  if ! jq -e ".tenants[\"$name\"]" "$PORTS_REGISTRY" >/dev/null 2>&1; then
    release_ports_lock
    die "tenant '$name' not in port registry"
  fi
  # Explicit ==false test: jq's `//` treats false as empty, so
  # `.enable_weechat // true` would misread a disabled tenant as enabled and
  # `enable-weechat` could never flip it back.
  current="$(jq -r --arg name "$name" 'if .tenants[$name].enable_weechat == false then "false" else "true" end' "$PORTS_REGISTRY" 2>/dev/null || true)"
  if [[ "$current" == "$enabled" ]]; then
    say "weechat for tenant '$name': already $enabled" >&2
    release_ports_lock
    return 0
  fi
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" --argjson enabled "$enabled" '.tenants[$name].enable_weechat = $enabled' \
      "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    release_ports_lock
    die "failed to set weechat flag for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  release_ports_lock
  say "weechat for tenant '$name': $current -> $enabled" >&2
}

# Mark a supported external worker (pebble|opencode|kimi|opencode2) enabled for a tenant
# (one-directional: false -> true). Idempotent. Used by ports_allocate()'s resume
# path so `add-tenant <existing> --with-<worker>` flips the registry, mirroring
# ports_enable_darkirc/proxy. Ensures a .workers object exists on older tenant
# entries that predate the workers map.
ports_enable_worker() {
  local name="$1" worker="$2"
  require_cmd jq
    case "$worker" in
    pebble|opencode|kimi|opencode2) ;;
    *) die "ports_enable_worker: unknown worker '$worker'" ;;
  esac
  # ISSUE-012 (follow-up): same locked read-modify-rename pattern as
  # ports_enable_darkirc — see comment there for the reentrancy contract.
  local had_lock="$PORTS_LOCK_HELD"
  acquire_ports_lock
  if ! jq -e ".tenants[\"$name\"]" "$PORTS_REGISTRY" >/dev/null 2>&1; then
    [[ "$had_lock" == "true" ]] || release_ports_lock
    die "tenant '$name' not in port registry"
  fi
  local current
  current="$(jq -r ".tenants[\"$name\"].workers[\"$worker\"] // false" "$PORTS_REGISTRY" 2>/dev/null || true)"
  if [[ "$current" == "true" ]]; then
    [[ "$had_lock" == "true" ]] || release_ports_lock
    return 0
  fi

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" --arg w "$worker" '
      .tenants[$name].workers = ((.tenants[$name].workers // {}) + { ($w): true })
    ' "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    [[ "$had_lock" == "true" ]] || release_ports_lock
    die "failed to enable worker '$worker' for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  [[ "$had_lock" == "true" ]] || release_ports_lock
  say "worker '$worker' for tenant '$name': disabled -> enabled" >&2
}

# Persist OpenRC worker runtime intent without changing worker selection.
# Optional field: enabled workers without it retain historical auto-start.
ports_set_worker_desired_running() {
  local name="$1" worker="$2" desired="$3"
  require_cmd jq
  case "$worker" in
    pebble|opencode|kimi|opencode2) ;;
    *) die "ports_set_worker_desired_running: unknown worker '$worker'" ;;
  esac
  case "$desired" in true|false) ;; *) die "worker desired_running must be true or false" ;; esac

  acquire_ports_lock
  if ! jq -e --arg name "$name" '.tenants[$name]' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    release_ports_lock
    die "tenant '$name' not in port registry"
  fi

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" --arg worker "$worker" --argjson desired "$desired" '
      .tenants[$name].worker_runtime = (.tenants[$name].worker_runtime // {})
      | .tenants[$name].worker_runtime[$worker] =
          ((.tenants[$name].worker_runtime[$worker] // {}) + {desired_running: $desired})
    ' "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    release_ports_lock
    die "failed to set desired state for worker '$worker' on tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  release_ports_lock
}

# Preserve the legacy registry field while making retirement explicit. This runs
# only after supervision and containers are confirmed stopped.
ports_retire_nanocode() {
  local name="$1" tmp
  require_cmd jq
  acquire_ports_lock
  if ! jq -e --arg name "$name" '.tenants[$name]' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    release_ports_lock
    return 0
  fi
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" '
      .tenants[$name].workers = ((.tenants[$name].workers // {}) + {nanocode: false})
    ' "$PORTS_REGISTRY" >"$tmp"; then
    rm -f "$tmp"
    release_ports_lock
    return 1
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  release_ports_lock
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
  port="$(jq -r ".tenants[\"$name\"].kimi_ports.$port_name // empty" "$PORTS_REGISTRY")"
  if [[ -n "$port" ]]; then
    printf '%s' "$port"
    return 0
  fi
  port="$(jq -r ".tenants[\"$name\"].voice_ports.$port_name // empty" "$PORTS_REGISTRY")"
  if [[ -n "$port" ]]; then
    printf '%s' "$port"
    return 0
  fi
  port="$(jq -r ".tenants[\"$name\"].opencode2_ports.$port_name // empty" "$PORTS_REGISTRY")"
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
  # Back-compat: pre-v10 registries had `reserved_6` where `vision_health` now
  # lives. The migration normally renames it, but direct callers must remain safe.
  if [[ "$port_name" == "vision_health" ]]; then
    port="$(jq -r ".tenants[\"$name\"].extended_ports.reserved_6 // empty" "$PORTS_REGISTRY")"
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
  if [[ "$port_name" == "coord_admin" ]]; then
    port="$(jq -r ".tenants[\"$name\"].extended_ports.reserved_9 // empty" "$PORTS_REGISTRY")"
    [[ -n "$port" ]] && { printf '%s' "$port"; return 0; }
    port="$(jq -r ".tenants[\"$name\"].extended_base // empty" "$PORTS_REGISTRY")"
    [[ "$port" =~ ^[0-9]+$ ]] && { printf '%s' "$((port + 9))"; return 0; }
  fi
  return 1
}

ports_list() {
  require_cmd jq
  if [[ ! -f "$PORTS_REGISTRY" ]]; then
    say "no port registry found; run add-tenant first"
    return 0
  fi
  jq -r '.tenants | to_entries[] | "\(.key)\t\(.value.ports.gateway)\t\(.value.ports.http)\t\(.value.ports.bridge)\t\(.value.ports.postgres)\t\(.value.ports.proxy)\t\(.value.ports.weechat)\t\(.value.ports.weechat_adapter // "-")\t\(.value.extended_ports.darkirc_adapter // "-")\t\(.value.extended_ports.darkirc_irc // "-")\t\(.value.extended_ports.darkirc_rpc // "-")\t\(.value.ports.orchestrator)\t\(.value.ports.nanocode_wss // "-")\t\(.value.ports.pebble_wss // "-")\t\(.value.extended_ports.vision_service // "-")\t\(.value.extended_ports.vision_health // "-")\t\(.value.voice_ports.voice_service // "-")\t\(.value.voice_ports.voice_health // "-")\t\(.value.opencode2_ports.opencode2_wss // "-")\t\(.value.opencode2_ports.opencode2_health // "-")"' "$PORTS_REGISTRY" \
    | column -t -N "TENANT,GATEWAY,HTTP,BRIDGE,PG,PROXY,WEECHAT,WS_ADPT,DARKIRC_ADPT,DARKIRC_IRC,DARKIRC_RPC,ORCH,NC_RSVD,PEBBLE,VISION_SVC,VISION_HLTH,VOICE_SVC,VOICE_HLTH,OC2_WSS,OC2_HLTH"
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
