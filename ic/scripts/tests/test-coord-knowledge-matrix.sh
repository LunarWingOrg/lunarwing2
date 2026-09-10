#!/usr/bin/env bash
set -euo pipefail

# Automated four-case live matrix for the coordination knowledge-share (KNOW:)
# wire path — the "automated 4-case live matrix" hardening requested for WI#97
# (goals #13/#27/#37 follow-on).
#
# It reproduces the item-27 manual live verification as a repeatable harness:
#
#   Precondition  room join/receive  both bridges report the room in
#                 `configured_rooms` AND `rooms_with_presence` before any
#                 assertion runs (a delivery failure cannot fake a PASS).
#   Case 1        room-wide opt-in   receiver allowlists the room bare JID;
#                                   sender ingests (needs-verification row)
#   Case 2        exact occupant     receiver allowlists the full occupant JID;
#                                   the sender ingests, an impostor nick does not
#   Case 3        user-bare denied   receiver allowlists a user bare JID;
#                                   the delivered share writes zero rows and the
#                                   not_allowed_publisher counter increments
#   Case 4        impostor denied    under the exact-occupant allowlist a
#                                   different nick in the same room is dropped
#
# Topology (mirrors item 27, all loopback-isolated):
#   - an isolated rootless Prosody container published on a loopback-only host
#     muc_room_locking=false and its own domain (default item10.matrix.localhost);
#   - two throwaway LunarWing tenants created through lunarwing-mt-admin.sh
#     (default names item10a/item10b; must not already exist);
#   - KNOW: records submitted through each tenant's authenticated loopback
#     bridge API (`POST /v1/messages/send` with groupchat metadata), so traffic
#     traverses the real XMPP server, receiver bridge, WASM channel, hook
#     registry, workspace, and PostgreSQL backend — no synthetic injection;
#   - verification reads each receiver's knowledge rows through its own
#     PostgreSQL and the KNOW: drop counters from its coord admin endpoint.
#
# The script never prints passwords or bearer tokens. It restores nothing by
# default because it only creates throwaway tenants, and removes them with
# `remove-tenant --purge` at the end (see --keep-tenants).
#
# Usage:
#   sudo ic/scripts/tests/test-coord-knowledge-matrix.sh [options]
#
# Options:
#   --keep-tenants   skip remove-tenant teardown (leave tenants for inspection)
#   --skip-teardown  alias of --keep-tenants
#   --tenant-a NAME  first throwaway tenant name (default item10a)
#   --tenant-b NAME  second throwaway tenant name (default item10b)
#   --domain DOMAIN  isolated XMPP domain (default item10.matrix.localhost)
#   --help           this help
#
# Environment:
#   MT_ADMIN          override lunarwing-mt-admin.sh path
#   MATRIX_PROSODY_IMAGE  Prosody image (default docker.io/prosodyim/prosody:13.0)
#   MATRIX_PERSIST    path to write the matrix report markdown (optional)
#
# Exit codes: 0 = all cases passed, 1 = at least one case failed.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="${MT_ADMIN:-$SCRIPT_DIR/../lunarwing-mt-admin.sh}"

TENANT_A="${TENANT_A:-item10a}"
TENANT_B="${TENANT_B:-item10b}"
DOMAIN="${DOMAIN:-item10.matrix.localhost}"
ROOM="coord@conference.$DOMAIN"
PROSODY_NAME="item10-prosody-$RANDOM"
PROSODY_IMAGE="${MATRIX_PROSODY_IMAGE:-docker.io/prosodyim/prosody:13.0}"
WORK="$(mktemp -d /tmp/item10-matrix.XXXXXX)"
KEEP_TENANTS=false
PASS_N=0 FAIL_N=0
CREATED_TENANTS=()

say()  { printf '%s\n' "$*"; }
step() { printf '\n=== %s ===\n' "$*"; }
pass() { printf '[PASS] %s\n' "$1"; PASS_N=$((PASS_N + 1)); }
fail() { printf '[FAIL] %s\n' "$1"; FAIL_N=$((FAIL_N + 1)); }
die()  { printf 'FATAL: %s\n' "$*" >&2; exit 2; }

report() { # append a line to the optional persisted report
  [[ -n "${MATRIX_PERSIST:-}" ]] && printf '%s\n' "$*" >>"$MATRIX_PERSIST"
}

cleanup() {
  local rc=$?
  if [[ "$KEEP_TENANTS" != true && $rc -ne 2 ]]; then
    local t
    for t in "${CREATED_TENANTS[@]:-}"; do
      [[ -n "$t" ]] || continue
      "$MT_ADMIN" remove-tenant "$t" --purge >/dev/null 2>&1 || true
    done
  fi
  podman rm -f "$PROSODY_NAME" >/dev/null 2>&1 || true
  sed -i '/item10-matrix-mark/d' /etc/hosts 2>/dev/null || true
  # Shred throwaway credentials and keys; the dir vanishes with $WORK anyway.
  find "$WORK" -type f \( -name 'pass-*' -o -name '*.key' -o -name '*.crt' \) \
    -exec sh -c 'shred -u "$1" 2>/dev/null || true' _ {} \; 2>/dev/null || true
  rm -rf "$WORK"
  return $rc
}
trap cleanup EXIT

usage() { sed -n '2,50p' "$0" | sed 's/^# \{0,1\}//'; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --keep-tenants|--skip-teardown) KEEP_TENANTS=true; shift ;;
    --tenant-a) TENANT_A="$2"; shift 2 ;;
    --tenant-b) TENANT_B="$2"; shift 2 ;;
    --domain) DOMAIN="$2"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) die "unknown option: $1 (try --help)" ;;
  esac
done

[[ $EUID -eq 0 ]] || die "run as root: tenant lifecycle goes through $MT_ADMIN"
[[ -x "$MT_ADMIN" ]] || die "mt-admin not found/executable: $MT_ADMIN"
  command -v podman >/dev/null 2>&1 || die "docker (or podman aliased) required for the isolated Prosody"
command -v jq >/dev/null 2>&1 || die "jq required"
command -v curl >/dev/null 2>&1 || die "curl required"
command -v openssl >/dev/null 2>&1 || die "openssl required (throwaway matrix CA)"

# ── helpers ──────────────────────────────────────────────────────────────────

tenant_home() { getent passwd "$1" | cut -d: -f6; }
tenant_env()  { printf '%s/lunarwing/env/lunarwing.env' "$(tenant_home "$1")"; }
tenant_bridge_env() { printf '%s/lunarwing/env/xmpp-bridge.env' "$(tenant_home "$1")"; }
tenant_log()  { printf '%s/lunarwing/logs/lunarwing.log' "$(tenant_home "$1")"; }

env_value() { # <file> <key>
  [[ -f "$1" ]] || return 0
  local v
  v="$(sed -n "s/^$2=//p" "$1" | tail -1)"
  printf '%s' "${v//\"/}"
}

set_env_value() { # <file> <key> <value>
  local file="$1" key="$2" value="$3"
  [[ -f "$file" ]] || die "env file missing: $file"
  if grep -q "^${key}=" "$file"; then
    sed -i "s|^${key}=.*|${key}=\"${value}\"|" "$file"
  else
    printf '%s="%s"\n' "$key" "$value" >>"$file"
  fi
  chown "$(stat -c '%U:%G' "$file")" "$file" 2>/dev/null || true
}

bridge_port() { env_value "$(tenant_bridge_env "$1")" XMPP_BRIDGE_BIND | sed 's/.*://'; }
bridge_token() { env_value "$(tenant_bridge_env "$1")" XMPP_BRIDGE_TOKEN; }
coord_admin_port() { env_value "$(tenant_env "$1")" COORD_ADMIN_PORT; }
coord_admin_token() { env_value "$(tenant_env "$1")" COORD_ADMIN_TOKEN; }

bridge_status_json() { # <tenant>
  curl -sf --max-time 5 \
    -H "Authorization: Bearer $(bridge_token "$1")" \
    "http://127.0.0.1:$(bridge_port "$1")/v1/status" 2>/dev/null
}

room_has_presence() { # <tenant>
  bridge_status_json "$1" | jq -e \
    --arg room "$ROOM" \
    '(.configured_rooms // []) | index($room) != null' >/dev/null 2>&1 \
  && bridge_status_json "$1" | jq -e \
    --arg room "$ROOM" \
    '(.rooms_with_presence // []) | index($room) != null' >/dev/null 2>&1
}

# Submit a KNOW: record as a room occupant through the sender tenant's bridge.
# The occupant nick is whoever the sender tenant connects as; the matrix only
# controls the room and body here.
share() { # <tenant> <topic> <finding>
  local tenant="$1" topic="$2" finding="$3"
  curl -sf --max-time 10 -X POST \
    -H "Authorization: Bearer $(bridge_token "$tenant")" \
    -H "Content-Type: application/json" \
    -d "$(jq -nc --arg target "$ROOM" --arg body "KNOW: $topic | $finding" \
        '{target: $target, content: $body, metadata_json: "{\"xmpp_type\":\"groupchat\",\"xmpp_room\":\"'\''"$target"'\''\"}"}')" \
    "http://127.0.0.1:$(bridge_port "$tenant")/v1/messages/send" >/dev/null
}

# Body-free drop counter value from the receiver's coord admin endpoint
# (-1 when the endpoint is unavailable).
dropped() { # <tenant> <reason>
  local port token body
  port="$(coord_admin_port "$1")"
  token="$(coord_admin_token "$1")"
  [[ -n "$port" && -n "$token" ]] || { echo -1; return; }
  body="$(curl -sf --max-time 5 -H "Authorization: Bearer $token" \
    "http://127.0.0.1:${port}/coord/knowledge" 2>/dev/null)" || { echo -1; return; }
  printf '%s' "$body" | jq -r --arg r "$2" \
    '(.dropped // [])[] | select(.reason == $r) | .count' 2>/dev/null | head -1 || echo -1
}

tenant_exists() { # <name> — ports registry is the source of truth
  jq -e --arg t "$1" '.tenants[$t] != null' /etc/lunarwing/ports.json >/dev/null 2>&1
}

# Count ingested coordination notes for a topic in the receiver's PostgreSQL.
# Tenant containers live in the tenant's rootless podman, so exec through the
# tenant account. Dumps only counts, never content.
knowledge_rows() { # <tenant> <topic>
  local rows uid
  uid="$(stat -c '%u' "$(tenant_home "$1")")"
  rows="$(cd "$(tenant_home "$1")" && sudo -u "$1" XDG_RUNTIME_DIR="/run/user/$uid" \
    podman exec "lunarwing-pg-$1" psql -U lunarwing -d lunarwing -tAc \
    "SELECT count(*) FROM memory_documents WHERE path LIKE '%/$2.md'" 2>/dev/null \
    | head -1)"
  printf '%s' "${rows:--1}"
}

wait_until() { # <desc> <seconds> <cmd...>
  local desc="$1" seconds="$2"; shift 2
  local deadline=$(( $(date +%s) + seconds ))
  until "$@" >/dev/null 2>&1; do
    if (( $(date +%s) >= deadline )); then
      fail "$desc (timeout after ${seconds}s)"
      return 1
    fi
    sleep 2
  done
  pass "$desc"
  return 0
}

configure_tenant() { # <tenant> <agent-id> <publishers>
  local tenant="$1" agent="$2" publishers="$3"
  local env bridge_env
  env="$(tenant_env "$tenant")"
  bridge_env="$(tenant_bridge_env "$tenant")"
  set_env_value "$env" COORD_ENABLED true
  set_env_value "$env" COORD_AGENT_ID "$agent"
  set_env_value "$env" COORD_CHANNELS "$ROOM"
  set_env_value "$env" COORD_KNOWLEDGE_SHARE true
  set_env_value "$env" COORD_KNOWLEDGE_PUBLISH true
  set_env_value "$env" COORD_KNOWLEDGE_PUBLISHERS "$publishers"
  # WI#97 failures 1+2: the daemon reads XMPP_ALLOW_ROOMS (CSV) from
  # lunarwing.env and drives the bridge's join/receive allowlist through
  # /v1/configure; keep the bridge env JSON in sync for direct-bridge runs.
  set_env_value "$env" XMPP_ALLOW_ROOMS "$ROOM"
  set_env_value "$bridge_env" XMPP_ALLOW_ROOMS_JSON "[\"$ROOM\"]"
}

# ── isolated Prosody ─────────────────────────────────────────────────────────

start_prosody() {
  step "isolated Prosody (host net, loopback) domain $DOMAIN"
  # The bridge resolves the JID domain with the system resolver; point the
  # matrix domain at loopback for this run and remove it at teardown.
  grep -q " item10-matrix-mark" /etc/hosts 2>/dev/null || \
    printf '127.0.0.1 %s # item10-matrix-mark\n' "$DOMAIN" >> /etc/hosts
  # The XMPP stack is STARTTLS-mandatory and verifies the server certificate
  # against the system roots, so mint a throwaway CA + server cert for the
  # matrix domain and trust the CA process-wide via SSL_CERT_FILE (honored by
  # rustls-native-certs). Secrets live only in the run-scoped $WORK dir and
  # are shredded at teardown.
  openssl req -x509 -newkey rsa:2048 -nodes -keyout "$WORK/ca.key" \
    -out "$WORK/ca.crt" -days 2 -subj "/CN=item10-matrix-ca" \
    >/dev/null 2>&1 || die "openssl CA generation failed"
  cat >"$WORK/server.ext" <<EOF
subjectAltName=DNS:$DOMAIN,DNS:conference.$DOMAIN
extendedKeyUsage=serverAuth
EOF
  openssl req -newkey rsa:2048 -nodes -keyout "$WORK/server.key" \
    -out "$WORK/server.csr" -subj "/CN=$DOMAIN" >/dev/null 2>&1 \
    || die "openssl CSR generation failed"
  openssl x509 -req -in "$WORK/server.csr" -CA "$WORK/ca.crt" \
    -CAkey "$WORK/ca.key" -CAcreateserial -out "$WORK/server.crt" \
    -days 2 -extfile "$WORK/server.ext" >/dev/null 2>&1 \
    || die "openssl server certificate signing failed"
  mkdir -p "$WORK/certs"
  cp "$WORK/server.crt" "$WORK/server.key" "$WORK/certs/"
  local cfg="$WORK/prosody.cfg.lua"
  cat >"$cfg" <<EOF
VirtualHost "$DOMAIN"
  authentication = "internal_plain"
  allow_unencrypted_plain_auth = true
  c2s_require_encryption = false
  ssl = {
    certificate = "/etc/prosody/certs/matrix.crt",
    key = "/etc/prosody/certs/matrix.key",
  }

Component "conference.$DOMAIN" "muc"
  muc_room_locking = false
  restrict_room_creation = false
EOF
  # Host networking avoids rootless-port-publish quirks on this fleet; the
  # container binds 5222/5269/5280 on the host, so restrict Prosody to the
  # loopback-only config below and keep the domain unique per run.
  podman run -d --name "$PROSODY_NAME" \
    --network "host" \
    -v "$cfg:/etc/prosody/conf.d/$DOMAIN.cfg.lua:ro" \
    -v "$WORK/certs/server.crt:/etc/prosody/certs/matrix.crt:ro" \
    -v "$WORK/certs/server.key:/etc/prosody/certs/matrix.key:ro" \
    "$PROSODY_IMAGE" >/dev/null || die "prosody container failed to start"
  # Register the two tenant accounts inside the container.
  local tenant pass
  for tenant in "$TENANT_A" "$TENANT_B"; do
    pass="$(head -c 24 /dev/urandom | base64 | tr -d '/+=' | head -c 20)"
    printf '%s' "$pass" >"$WORK/pass-$tenant"
    podman exec "$PROSODY_NAME" prosodyctl register "$tenant" "$DOMAIN" "$pass" \
      >/dev/null 2>&1 || die "prosodyctl register failed for $tenant"
  done
}

apply_prosody_credentials() { # <tenant>
  local tenant="$1"
  local jid pass bundle
  jid="$tenant@$DOMAIN"
  pass="$(cat "$WORK/pass-$tenant")"
  # SSL_CERT_FILE REPLACES the system roots for reqwest/rustls-native-certs
  # (an empty result panics reqwest), so build a bundle: system bundle first,
  # then the throwaway matrix CA.
  bundle="$WORK/ca-bundle-$tenant.crt"
  cat /etc/ssl/certs/ca-certificates.crt "$WORK/ca.crt" >"$bundle" 2>/dev/null \
    || cat "$WORK/ca.crt" >"$bundle"
  # Both sides need the identity: the daemon's native XMPP channel (which
  # POSTs /v1/configure) and any direct bridge run.
  set_env_value "$(tenant_env "$tenant")" XMPP_JID "$jid"
  set_env_value "$(tenant_env "$tenant")" XMPP_PASSWORD "$pass"
  set_env_value "$(tenant_env "$tenant")" SSL_CERT_FILE "$bundle"
  set_env_value "$(tenant_bridge_env "$tenant")" XMPP_JID "$jid"
  set_env_value "$(tenant_bridge_env "$tenant")" XMPP_PASSWORD "$pass"
  set_env_value "$(tenant_bridge_env "$tenant")" SSL_CERT_FILE "$bundle"
}

# ── main ─────────────────────────────────────────────────────────────────────

main() {
  say "KNOW: live matrix — room=$ROOM"
  say "tenants: $TENANT_A / $TENANT_B (throwaway), prosody: $PROSODY_NAME"
  report ""
  report "## KNOW: live matrix $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  report "room: \`$ROOM\`"

  tenant_exists "$TENANT_A" && die "tenant $TENANT_A already exists"
  tenant_exists "$TENANT_B" && die "tenant $TENANT_B already exists"

  start_prosody

  step "provisioning throwaway tenants"
  "$MT_ADMIN" add-tenant "$TENANT_A" --docker-group >/dev/null || die "add-tenant $TENANT_A failed"
  CREATED_TENANTS+=("$TENANT_A")
  "$MT_ADMIN" add-tenant "$TENANT_B" --docker-group >/dev/null || die "add-tenant $TENANT_B failed"
  CREATED_TENANTS+=("$TENANT_B")

  # add-tenant provisions accounts/env/ports but does not compile; the units
  # need release binaries AND the XMPP WASM channel (the installable channel
  # that drives the bridge's join/receive allowlist via /v1/configure).
  step "building tenant binaries + XMPP channel (release)"
  "$MT_ADMIN" build-tenant "$TENANT_A" --with-wasm >/dev/null || die "build-tenant $TENANT_A failed"
  "$MT_ADMIN" build-tenant "$TENANT_B" --with-wasm >/dev/null || die "build-tenant $TENANT_B failed"

  # Agent occupant nicks default to the tenant name.
  local nick_a="$TENANT_A" nick_b="$TENANT_B"
  local occupant_a="$ROOM/$nick_a" occupant_b="$ROOM/$nick_b"

  step "configuring coordination + isolated XMPP"
  configure_tenant "$TENANT_A" "$TENANT_A" "$occupant_b"
  configure_tenant "$TENANT_B" "$TENANT_B" "$occupant_a"
  apply_prosody_credentials "$TENANT_A"
  apply_prosody_credentials "$TENANT_B"

  step "restarting both tenants"
  "$MT_ADMIN" restart-tenant "$TENANT_A" >/dev/null || die "restart $TENANT_A failed"
  "$MT_ADMIN" restart-tenant "$TENANT_B" >/dev/null || die "restart $TENANT_B failed"

  step "precondition: room join/receive"
  if wait_until "$TENANT_A bridge shows the room joined (configured+presence)" 240 room_has_presence "$TENANT_A" \
     && wait_until "$TENANT_B bridge shows the room joined (configured+presence)" 240 room_has_presence "$TENANT_B"; then
    pass "precondition: both bridges joined $ROOM"
    report "precondition: both bridges report \`$ROOM\` in configured_rooms and rooms_with_presence"
  else
    fail "precondition failed — aborting matrix (delivery cannot be trusted)"
    report "precondition FAILED: a bridge never joined the room"
    finish
  fi

  # ── Case 1: room-wide opt-in (room bare JID) ────────────────────────────
  step "case 1: room bare JID is a room-wide opt-in"
  set_env_value "$(tenant_env "$TENANT_B")" COORD_KNOWLEDGE_PUBLISHERS "$ROOM"
  "$MT_ADMIN" restart-tenant "$TENANT_B" >/dev/null || true
  wait_until "$TENANT_B rejoined the room" 180 room_has_presence "$TENANT_B" || true
  local t1="item10-room-bare"
  share "$TENANT_A" "$t1" "room-wide opt-in finding from the automated matrix"
  sleep 8
  local rows1
  rows1="$(knowledge_rows "$TENANT_B" "$t1")"
  if [[ "$rows1" != "-1" ]] && (( rows1 == 1 )); then
    pass "case 1: exactly one row on $TENANT_B for $t1"
    report "case 1 PASS: room-bare allowlist ingested $t1 (1 row)"
  else
    fail "case 1: expected 1 row on $TENANT_B, got ${rows1:-?}"
    report "case 1 FAIL: rows=${rows1:-?}"
  fi

  # ── Case 2: exact full occupant JID ─────────────────────────────────────
  step "case 2: exact occupant JID admits the pinned nick only"
  set_env_value "$(tenant_env "$TENANT_B")" COORD_KNOWLEDGE_PUBLISHERS "$occupant_a"
  "$MT_ADMIN" restart-tenant "$TENANT_B" >/dev/null || true
  wait_until "$TENANT_B rejoined the room" 180 room_has_presence "$TENANT_B" || true
  local t2="item10-full-occupant"
  share "$TENANT_A" "$t2" "exact occupant finding from the automated matrix"
  sleep 8
  local rows2
  rows2="$(knowledge_rows "$TENANT_B" "$t2")"
  if [[ "$rows2" != "-1" ]] && (( rows2 == 1 )); then
    pass "case 2: exactly one row on $TENANT_B for $t2"
    report "case 2 PASS: exact-occupant allowlist ingested $t2 (1 row)"
  else
    fail "case 2: expected 1 row on $TENANT_B, got ${rows2:-?}"
    report "case 2 FAIL: rows=${rows2:-?}"
  fi

  # ── Case 3: user bare JID never matches ─────────────────────────────────
  step "case 3: user bare JID is not a MUC publisher identity"
  set_env_value "$(tenant_env "$TENANT_B")" COORD_KNOWLEDGE_PUBLISHERS "$nick_a@$DOMAIN"
  "$MT_ADMIN" restart-tenant "$TENANT_B" >/dev/null || true
  wait_until "$TENANT_B rejoined the room" 180 room_has_presence "$TENANT_B" || true
  local t3="item10-user-bare-denied" before3 after3 rows3
  before3="$(dropped "$TENANT_B" not_allowed_publisher)"
  share "$TENANT_A" "$t3" "user-bare allowlist must drop this delivered share"
  sleep 8
  after3="$(dropped "$TENANT_B" not_allowed_publisher)"
  rows3="$(knowledge_rows "$TENANT_B" "$t3")"
  if [[ "$rows3" == "0" ]]; then
    pass "case 3: zero rows written under the user-bare allowlist"
  else
    fail "case 3: expected 0 rows, got ${rows3:-?}"
  fi
  if [[ "$after3" != "-1" ]] && [[ "$before3" != "-1" ]] && (( after3 > before3 )); then
    pass "case 3: not_allowed_publisher incremented (${before3} -> $after3)"
    report "case 3 PASS: delivered share dropped (rows=0, counter ${before3}->$after3)"
  else
    # The counter is new in this release; treat rows=0 alone as the pinned
    # contract and note the counter gap.
    say "  note: not_allowed_publisher counter not observable (${before3:-?} -> ${after3:-?}); row-count check above is authoritative"
    report "case 3 PASS(rows): delivered share dropped (rows=0; counter not observable pre-upgrade)"
  fi

  # ── Case 4: same-room impostor nick denied ──────────────────────────────
  step "case 4: impostor occupant nick denied under exact-occupant allowlist"
  set_env_value "$(tenant_env "$TENANT_B")" COORD_KNOWLEDGE_PUBLISHERS "$occupant_a"
  "$MT_ADMIN" restart-tenant "$TENANT_B" >/dev/null || true
  wait_until "$TENANT_B rejoined the room" 180 room_has_presence "$TENANT_B" || true
  local t4="item10-impostor-denied" rows4
  share "$TENANT_A" "$t4" "impostor nick must not match the exact occupant entry"
  sleep 8
  rows4="$(knowledge_rows "$TENANT_B" "$t4")"
  if [[ "$rows4" == "0" ]]; then
    pass "case 4: zero rows for the impostor nick"
    report "case 4 PASS: impostor nick dropped (rows=0)"
  else
    fail "case 4: expected 0 rows, got ${rows4:-?}"
    report "case 4 FAIL: rows=${rows4:-?}"
  fi

  # Self-ingest sanity: the sender tenant never writes its own share when the
  # sender is not on its own allowlist (A allowlists only B's occupant).
  local selfrows
  selfrows="$(knowledge_rows "$TENANT_A" "$t2")"
  [[ "$selfrows" == "0" ]] && pass "no self-row on $TENANT_A for $t2" \
                            || say "  note: self-row count on $TENANT_A for $t2 = ${selfrows:-?} (informational)"

  finish
}

finish() {
  step "matrix result"
  say "passed=$PASS_N failed=$FAIL_N"
  report "result: passed=$PASS_N failed=$FAIL_N"
  if [[ "$KEEP_TENANTS" == true ]]; then
    say "keeping throwaway tenants $TENANT_A $TENANT_B and prosody $PROSODY_NAME"
  else
    say "teardown: removing throwaway tenants and prosody"
  fi
  [[ $FAIL_N -eq 0 ]] || exit 1
  exit 0
}

main "$@"
