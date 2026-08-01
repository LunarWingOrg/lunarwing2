#!/usr/bin/env bash
# Standalone test harness for the add-tenant env-flag feature.
# Sources lunarwing-mt-admin.sh (the script's BASH_SOURCE guard skips main()
# when sourced, so no dispatch / require_root runs) and invokes the pure
# helpers + env-writer functions against fixtures.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

# shellcheck source=../lunarwing-mt-admin.sh
source "$ADMIN_SCRIPT"

failures=0
assert_eq() {  # <label> <actual> <expected>
  if [[ "$2" == "$3" ]]; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1"
    echo "        expected: [$3]"
    echo "        actual:   [$2]"
    failures=$((failures + 1))
  fi
}

echo "=== build_xmpp_allow_from tests ==="

# Owner JID only (no extras)
out="$(build_xmpp_allow_from "ruffles@xmpp.localhost" "")"
assert_eq "owner-only (csv)" "$out" "ruffles@xmpp.localhost"

# Owner + one extra
out="$(build_xmpp_allow_from "ruffles@xmpp.localhost" "admin@xmpp.org")"
assert_eq "owner+one (csv)" "$out" "ruffles@xmpp.localhost,admin@xmpp.org"

# Owner + multiple extras (comma-separated input)
out="$(build_xmpp_allow_from "ruffles@xmpp.localhost" "admin@xmpp.org,bob@xmpp.org")"
assert_eq "owner+two (csv)" "$out" "ruffles@xmpp.localhost,admin@xmpp.org,bob@xmpp.org"

# Dedupe: extra equals owner
out="$(build_xmpp_allow_from "ruffles@xmpp.localhost" "ruffles@xmpp.localhost")"
assert_eq "dedupe-owner" "$out" "ruffles@xmpp.localhost"

# Dedupe: duplicate extra
out="$(build_xmpp_allow_from "ruffles@xmpp.localhost" "admin@xmpp.org,admin@xmpp.org")"
assert_eq "dedupe-extra" "$out" "ruffles@xmpp.localhost,admin@xmpp.org"

# Whitespace trimmed around extras
out="$(build_xmpp_allow_from "ruffles@xmpp.localhost" " admin@xmpp.org , bob@xmpp.org ")"
assert_eq "trim-whitespace" "$out" "ruffles@xmpp.localhost,admin@xmpp.org,bob@xmpp.org"

echo "=== build_xmpp_allow_from_json tests ==="

out="$(build_xmpp_allow_from_json "ruffles@xmpp.localhost" "")"
assert_eq "json owner-only" "$out" '["ruffles@xmpp.localhost"]'

out="$(build_xmpp_allow_from_json "ruffles@xmpp.localhost" "admin@xmpp.org,bob@xmpp.org")"
assert_eq "json owner+two" "$out" '["ruffles@xmpp.localhost","admin@xmpp.org","bob@xmpp.org"]'

out="$(build_xmpp_allow_from_json "ruffles@xmpp.localhost" "ruffles@xmpp.localhost,admin@xmpp.org")"
assert_eq "json dedupe" "$out" '["ruffles@xmpp.localhost","admin@xmpp.org"]'

echo "=== write_tenant_lunarwing_env fixture tests ==="

# Fixture: stub the helpers the env writer depends on so it can run without
# root, a real tenant, or podman. We capture into a temp dir.
MT_FIXTURE="$(mktemp -d)"
trap 'rm -rf "$MT_FIXTURE"' EXIT

# Stub tenant_* path helpers to point at the fixture dir.
tenant_env_dir()    { echo "$MT_FIXTURE/env"; }
tenant_state_dir()  { echo "$MT_FIXTURE/state"; }
tenant_run_dir()    { echo "$MT_FIXTURE/run"; }
mkdir -p "$MT_FIXTURE/env" "$MT_FIXTURE/state" "$MT_FIXTURE/run"
chmod 0700 "$MT_FIXTURE/env" "$MT_FIXTURE/state" "$MT_FIXTURE/run"

# Stub ports_get to return deterministic ports. Restore the real implementation
# for registry migration/lookup tests below.
REAL_PORTS_GET_DEF="$(declare -f ports_get)"
ports_get() {  # <name> <service>
  case "$2" in
    gateway)         echo 10000 ;;
    http)            echo 10001 ;;
    bridge)          echo 10002 ;;
    postgres)        echo 10003 ;;
    proxy)           echo 10004 ;;
    weechat)         echo 10005 ;;
    orchestrator)    echo 10006 ;;
    nanocode_wss)    echo 10007 ;;
    pebble_wss)      echo 10008 ;;
    weechat_adapter) echo 10009 ;;
    coord_admin)     echo 20009 ;;
    *)               echo 0 ;;
  esac
}

# Stub tenant_pg_password, tenant_darkirc_enabled, generate_token, say, chown.
tenant_pg_password()      { echo "pgpass-fixture"; }
DARKIRC_FIXTURE_ENABLED=false
tenant_darkirc_enabled()  { [[ "$DARKIRC_FIXTURE_ENABLED" == "true" ]]; }
generate_token()          { echo "token-fixture-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; }
say()                     { :; }          # silence
chown()                   { :; }          # no-op (running as non-root in tests)
sudo() {
  [[ "${1:-}" == -u ]] && shift 2
  if [[ "${1:-}" == env ]]; then
    shift
    command env "$@"
  else
    "$@"
  fi
}

run_writer() {  # <args...> — invokes write_tenant_lunarwing_env, prints the file
  rm -f "$MT_FIXTURE/env/lunarwing.env"
  write_tenant_lunarwing_env "$@"
}

envval() {  # <key> — extract value from the last-written fixture env
  grep -m1 "^$1=" "$MT_FIXTURE/env/lunarwing.env" | cut -d= -f2-
}

# Case 1: defaults (no flags) on first write
out="$(run_writer fixture-tenant)"
assert_eq "default LLM_MODEL" "$(envval LLM_MODEL)" "tensorzero::function_name::lunarwing"
assert_eq "default GATEWAY_HOST" "$(envval GATEWAY_HOST)" "127.0.0.1"
assert_eq "default XMPP_ALLOW_FROM" "$(envval XMPP_ALLOW_FROM)" "fixture-tenant@xmpp.localhost"
assert_eq "default coordination disabled" "$(envval COORD_ENABLED)" "false"
assert_eq "default coordination agent ID" "$(envval COORD_AGENT_ID)" "fixture-tenant"
assert_eq "coordination admin token generated" \
  "$(envval COORD_ADMIN_TOKEN)" "token-fixture-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
assert_eq "coordination admin port allocated" "$(envval COORD_ADMIN_PORT)" "20009"

# Case 2: all three flags on first write
out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "" "" "" "" \
  "glm-5-air" "0.0.0.0" "admin@xmpp.org,bob@xmpp.org")"
assert_eq "flag LLM_MODEL" "$(envval LLM_MODEL)" "glm-5-air"
assert_eq "flag GATEWAY_HOST" "$(envval GATEWAY_HOST)" "0.0.0.0"
assert_eq "flag XMPP_ALLOW_FROM" "$(envval XMPP_ALLOW_FROM)" "fixture-tenant@xmpp.localhost,admin@xmpp.org,bob@xmpp.org"

# Case 3: idempotent re-run WITHOUT flags preserves operator-set values.
#   (run_writer deletes the file each call, so simulate a re-run by writing
#   once with flags, then a second time without — but keep the file between.)
rm -f "$MT_FIXTURE/env/lunarwing.env"
write_tenant_lunarwing_env fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "" "" "" "" \
  "glm-5-air" "0.0.0.0" "admin@xmpp.org"
sed -i \
  -e 's/^COORD_ENABLED=.*/COORD_ENABLED=true/' \
  -e 's/^COORD_AGENT_ID=.*/COORD_AGENT_ID=custom-agent/' \
  -e 's/^COORD_CHANNELS=.*/COORD_CHANNELS=ops@conference.example/' \
  -e 's/^COORD_ADMIN_TOKEN=.*/COORD_ADMIN_TOKEN=preserved-coord-token/' \
  "$MT_FIXTURE/env/lunarwing.env"
cat >>"$MT_FIXTURE/env/lunarwing.env" <<'EOF'
LW2STUDYMCP_HOME=/srv/lunarwing/lw2studymcp
LW2STUDYMCP_STATE_HOME=/srv/lunarwing/study-state
LW2STUDYMCP_ALLOW_WRITE=1
EOF
# Now re-run WITHOUT the new flags (pass empty for slots 9-11) — file exists.
write_tenant_lunarwing_env fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "" "" "" "" \
  "" "" ""
assert_eq "rerun preserves LLM_MODEL" "$(envval LLM_MODEL)" "glm-5-air"
assert_eq "rerun preserves GATEWAY_HOST" "$(envval GATEWAY_HOST)" "0.0.0.0"
assert_eq "rerun preserves XMPP_ALLOW_FROM" "$(envval XMPP_ALLOW_FROM)" "fixture-tenant@xmpp.localhost,admin@xmpp.org"
assert_eq "rerun preserves COORD_ENABLED" "$(envval COORD_ENABLED)" "true"
assert_eq "rerun preserves COORD_AGENT_ID" "$(envval COORD_AGENT_ID)" "custom-agent"
assert_eq "rerun preserves COORD_CHANNELS" "$(envval COORD_CHANNELS)" "ops@conference.example"
assert_eq "rerun preserves COORD_ADMIN_TOKEN" "$(envval COORD_ADMIN_TOKEN)" "preserved-coord-token"
assert_eq "rerun preserves LW2STUDYMCP_HOME" \
  "$(envval LW2STUDYMCP_HOME)" "/srv/lunarwing/lw2studymcp"
assert_eq "rerun preserves LW2STUDYMCP_STATE_HOME" \
  "$(envval LW2STUDYMCP_STATE_HOME)" "/srv/lunarwing/study-state"
assert_eq "rerun preserves LW2STUDYMCP_ALLOW_WRITE" \
  "$(envval LW2STUDYMCP_ALLOW_WRITE)" "1"

# Case 4: a DarkIRC-enabled re-render must preserve the existing adapter secret.
DARKIRC_FIXTURE_ENABLED=true
printf '\nDARKIRC_ADAPTER_SECRET=preserved-darkirc-fixture\n' >>"$MT_FIXTURE/env/lunarwing.env"
write_tenant_lunarwing_env fixture-tenant
assert_eq "rerun preserves DARKIRC_ADAPTER_SECRET" \
  "$(envval DARKIRC_ADAPTER_SECRET)" "preserved-darkirc-fixture"
DARKIRC_FIXTURE_ENABLED=false

echo "=== coordination env backfill tests ==="
cat >"$MT_FIXTURE/env/lunarwing.env" <<'EOF'
LUNARWING_OWNER_ID=fixture-tenant
EOF
patch_tenant_coordination_env fixture-tenant "$MT_FIXTURE/env/lunarwing.env"
assert_eq "backfill coordination disabled" "$(envval COORD_ENABLED)" "false"
assert_eq "backfill coordination agent ID" "$(envval COORD_AGENT_ID)" "fixture-tenant"
assert_eq "backfill coordination channels" "$(envval COORD_CHANNELS)" ""
assert_eq "backfill coordination aliases" "$(envval COORD_AGENT_ALIASES)" ""
assert_eq "backfill coordination admin users" "$(envval COORD_ADMIN_USERS)" ""
assert_eq "backfill coordination token" \
  "$(envval COORD_ADMIN_TOKEN)" "token-fixture-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
assert_eq "backfill coordination admin port" "$(envval COORD_ADMIN_PORT)" "20009"
sed -i 's/^COORD_ADMIN_TOKEN=.*/COORD_ADMIN_TOKEN=backfill-preserved/' \
  "$MT_FIXTURE/env/lunarwing.env"
patch_tenant_coordination_env fixture-tenant "$MT_FIXTURE/env/lunarwing.env"
assert_eq "backfill token remains stable" "$(envval COORD_ADMIN_TOKEN)" "backfill-preserved"
assert_eq "backfill env mode" \
  "$(stat -c '%a' "$MT_FIXTURE/env/lunarwing.env")" "600"
sed -i \
  -e 's/^COORD_ADMIN_TOKEN=.*/COORD_ADMIN_TOKEN=/' \
  -e 's/^COORD_ADMIN_PORT=.*/COORD_ADMIN_PORT=/' \
  "$MT_FIXTURE/env/lunarwing.env"
patch_tenant_coordination_env fixture-tenant "$MT_FIXTURE/env/lunarwing.env"
assert_eq "backfill repairs empty token" \
  "$(envval COORD_ADMIN_TOKEN)" "token-fixture-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
assert_eq "backfill repairs empty admin port" "$(envval COORD_ADMIN_PORT)" "20009"
assert_eq "backfill keeps one token key" \
  "$(grep -c '^COORD_ADMIN_TOKEN=' "$MT_FIXTURE/env/lunarwing.env")" "1"
assert_eq "backfill keeps one port key" \
  "$(grep -c '^COORD_ADMIN_PORT=' "$MT_FIXTURE/env/lunarwing.env")" "1"

echo "=== XMPP_JID preservation tests ==="

# Regression: re-running add-tenant without --xmpp-jid/--xmpp-domain must NOT
# reset a live tenant's JID to the <name>@xmpp.localhost default. Doing so
# repoints the daemon at a domain that resolves nowhere; the bridge then sits in
# an endless connect-retry while /v1/status still reports configured+running.
rm -f "$MT_FIXTURE/env/lunarwing.env"
write_tenant_lunarwing_env fixture-tenant "fixture-tenant@xmpp.sobe.world"
assert_eq "first write honours explicit JID" \
  "$(envval XMPP_JID)" "fixture-tenant@xmpp.sobe.world"

# Re-run with NO jid argument -> must preserve, not default to xmpp.localhost.
write_tenant_lunarwing_env fixture-tenant
assert_eq "rerun preserves XMPP_JID" \
  "$(envval XMPP_JID)" "fixture-tenant@xmpp.sobe.world"

# Re-run with an empty-string jid (how the dispatcher passes "flag absent").
write_tenant_lunarwing_env fixture-tenant ""
assert_eq "empty-arg rerun preserves XMPP_JID" \
  "$(envval XMPP_JID)" "fixture-tenant@xmpp.sobe.world"

# The bridge env writer must preserve from lunarwing.env too.
rm -f "$MT_FIXTURE/env/xmpp-bridge.env"
write_tenant_bridge_env fixture-tenant
assert_eq "bridge writer preserves XMPP_JID" \
  "$(grep -m1 '^XMPP_JID=' "$MT_FIXTURE/env/xmpp-bridge.env" | cut -d= -f2-)" \
  "fixture-tenant@xmpp.sobe.world"

# An explicit JID still wins over the stored one (intentional re-point).
write_tenant_lunarwing_env fixture-tenant "fixture-tenant@xmpp.example.net"
assert_eq "explicit JID overrides stored" \
  "$(envval XMPP_JID)" "fixture-tenant@xmpp.example.net"

# First-ever write with no stored value falls back to the localhost default.
rm -f "$MT_FIXTURE/env/lunarwing.env"
write_tenant_lunarwing_env fixture-tenant
assert_eq "first write defaults to xmpp.localhost" \
  "$(envval XMPP_JID)" "fixture-tenant@xmpp.localhost"

# resolve_tenant_xmpp_jid unit checks.
rm -f "$MT_FIXTURE/env/lunarwing.env"
assert_eq "resolver: no file -> default" \
  "$(resolve_tenant_xmpp_jid fixture-tenant "")" "fixture-tenant@xmpp.localhost"
printf 'XMPP_JID=stored@xmpp.sobe.world\n' >"$MT_FIXTURE/env/lunarwing.env"
assert_eq "resolver: stored wins over default" \
  "$(resolve_tenant_xmpp_jid fixture-tenant "")" "stored@xmpp.sobe.world"
assert_eq "resolver: explicit wins over stored" \
  "$(resolve_tenant_xmpp_jid fixture-tenant "cli@xmpp.example.net")" "cli@xmpp.example.net"

echo "=== write_tenant_bridge_env fixture tests ==="

# The bridge writer reads XMPP_BRIDGE_TOKEN / XMPP_PASSWORD from the daemon env.
# Ensure a daemon env exists with those keys for the fixture tenant.
cat > "$MT_FIXTURE/env/lunarwing.env" <<E
XMPP_BRIDGE_TOKEN=bridge-tok-fixture
XMPP_PASSWORD=xmpp-pass-fixture
E

run_bridge_writer() {  # <xmpp_allow_from_extras>
  rm -f "$MT_FIXTURE/env/xmpp-bridge.env"
  write_tenant_bridge_env fixture-tenant \
    "fixture-tenant@xmpp.localhost" "" "$1"
  cat "$MT_FIXTURE/env/xmpp-bridge.env"
}

bridgeval() {  # <key>
  grep -m1 "^$1=" "$MT_FIXTURE/env/xmpp-bridge.env" | cut -d= -f2-
}

# Case 1: no extras → owner JID only
out="$(run_bridge_writer "")"
assert_eq "bridge json owner-only" "$(bridgeval XMPP_ALLOW_FROM_JSON)" '["fixture-tenant@xmpp.localhost"]'

# Case 2: extras → owner + extras
out="$(run_bridge_writer "admin@xmpp.org,bob@xmpp.org")"
assert_eq "bridge json owner+two" "$(bridgeval XMPP_ALLOW_FROM_JSON)" '["fixture-tenant@xmpp.localhost","admin@xmpp.org","bob@xmpp.org"]'

echo "=== per-tenant worker gating (registry) tests ==="

# Exercise the registry readers/writers that gate start_tenant_<worker>.
# Point PORTS_REGISTRY at a fixture seeded with: an "old" tenant that predates
# the workers map (key absent) and a tenant with an explicit selection.
WORKERS_REG="$MT_FIXTURE/ports.json"
PORTS_REGISTRY="$WORKERS_REG"
cat > "$WORKERS_REG" <<JSON
{"tenants":{
  "oldt":{"base_port":10020},
  "sel":{"base_port":10030,"workers":{"nanocode":true,"pebble":false,"opencode":false}}
}}
JSON

# Capture a boolean helper's exit under `set -e` without aborting the run.
worker_state() {  # <tenant> <worker> -> prints "on" | "off"
  if tenant_worker_enabled "$1" "$2"; then echo on; else echo off; fi
}

# Absent workers key => every worker OFF (the bug this fixes: previously a
# tenant started every worker whose shared image existed).
assert_eq "old tenant nanocode OFF" "$(worker_state oldt nanocode)" "off"
assert_eq "old tenant pebble OFF"   "$(worker_state oldt pebble)"   "off"
assert_eq "old tenant opencode OFF" "$(worker_state oldt opencode)" "off"

# Explicit selection honored.
assert_eq "selected nanocode ON"  "$(worker_state sel nanocode)" "on"
assert_eq "unselected pebble OFF" "$(worker_state sel pebble)"   "off"

# Resume flip is one-directional (false->true), idempotent, and creates the
# workers map on a tenant that lacked one — without leaking other workers ON.
ports_enable_worker oldt opencode >/dev/null 2>&1
assert_eq "resume enable opencode"   "$(worker_state oldt opencode)" "on"
ports_enable_worker oldt opencode >/dev/null 2>&1   # idempotent
assert_eq "resume enable idempotent" "$(worker_state oldt opencode)" "on"
assert_eq "resume no leak nanocode"  "$(worker_state oldt nanocode)" "off"
if jq -e . "$WORKERS_REG" >/dev/null 2>&1; then
  echo "  PASS: registry remains valid JSON after enable"
else
  echo "  FAIL: registry corrupted"; failures=$((failures + 1))
fi

echo "=== coordination admin port migration tests ==="
eval "$REAL_PORTS_GET_DEF"
PORTS_REGISTRY="$MT_FIXTURE/ports-v12.json"
cat >"$PORTS_REGISTRY" <<'JSON'
{"version":12,"tenants":{"legacy":{"extended_base":20000,"extended_ports":{"reserved_9":20009}},"missing":{"extended_base":20010}}}
JSON
ports_migrate_v13
assert_eq "v13 registry version" "$(jq -r '.version' "$PORTS_REGISTRY")" "13"
assert_eq "v13 coord_admin rename" \
  "$(jq -r '.tenants.legacy.extended_ports.coord_admin' "$PORTS_REGISTRY")" "20009"
assert_eq "v13 removes reserved_9" \
  "$(jq -r '.tenants.legacy.extended_ports | has("reserved_9")' "$PORTS_REGISTRY")" "false"
assert_eq "ports_get resolves coord_admin" "$(ports_get legacy coord_admin)" "20009"
assert_eq "v13 repairs missing extended_ports" \
  "$(jq -r '.tenants.missing.extended_ports.coord_admin' "$PORTS_REGISTRY")" "20019"
jq '.tenants.unmigrated = {"extended_base":20020}' "$PORTS_REGISTRY" >"$PORTS_REGISTRY.tmp"
mv "$PORTS_REGISTRY.tmp" "$PORTS_REGISTRY"
assert_eq "ports_get derives coord_admin from extended_base" \
  "$(ports_get unmigrated coord_admin)" "20029"

BAD_ENV="$MT_FIXTURE/env/bad.env"
printf 'LUNARWING_OWNER_ID=broken\n' >"$BAD_ENV"
chmod 0644 "$BAD_ENV"
jq '.tenants.broken = {}' "$PORTS_REGISTRY" >"$PORTS_REGISTRY.tmp"
mv "$PORTS_REGISTRY.tmp" "$PORTS_REGISTRY"
if (patch_tenant_coordination_env broken "$BAD_ENV" >/dev/null 2>&1); then
  echo "  FAIL: missing coord_admin port accepted"; failures=$((failures + 1))
else
  echo "  PASS: missing coord_admin port rejected"
fi
assert_eq "failed backfill does not rewrite unsafe env" "$(stat -c '%a' "$BAD_ENV")" "644"
if grep -q '^COORD_ADMIN_TOKEN=' "$BAD_ENV"; then
  echo "  FAIL: failed backfill appended coordination token"; failures=$((failures + 1))
else
  echo "  PASS: failed backfill leaves token absent"
fi

# Unknown worker name is rejected.
if ( ports_enable_worker oldt bogus >/dev/null 2>&1 ); then
  echo "  FAIL: ports_enable_worker accepted an unknown worker"; failures=$((failures + 1))
else
  echo "  PASS: ports_enable_worker rejects unknown worker"
fi

echo "=== CLI dispatch parsing smoke check ==="

# Verify the dispatch block recognizes the new flags by invoking the script
# directly with a deliberately-bad tenant name AFTER the flags; if the flags
# parsed, the script reaches require_root (not "unknown flag"). As non-root,
# require_root exits with a permission message; we assert it does NOT mention
# "unknown flag".
err="$(bash "$ADMIN_SCRIPT" add-tenant \
  --llm-model glm-5-air \
  --gateway-host 0.0.0.0 \
  --xmpp-allow-from admin@xmpp.org \
  --with-nanocode --with-pebble --with-opencode \
  2>&1 || true)"
if echo "$err" | grep -q 'unknown flag'; then
  echo "  FAIL: a new flag was rejected as unknown"
  echo "        $err"
  failures=$((failures + 1))
else
  echo "  PASS: all new flags (incl. --with-nanocode/pebble/opencode) parsed"
fi

# Verify --no-weechat-bootstrap is accepted by add-tenant dispatch.
dispatch_output="$(bash "$ADMIN_SCRIPT" add-tenant \
  --no-weechat-bootstrap \
  2>&1 || true)"
if echo "$dispatch_output" | grep -q 'unknown flag: --no-weechat-bootstrap'; then
  echo "  FAIL: add-tenant rejected --no-weechat-bootstrap"
  echo "        $dispatch_output"
  failures=$((failures + 1))
else
  echo "  PASS: add-tenant accepts --no-weechat-bootstrap"
fi

# Verify --no-weechat-bootstrap is accepted by add-tenants dispatch.
batch_dispatch_output="$(bash "$ADMIN_SCRIPT" add-tenants \
  --no-weechat-bootstrap \
  2>&1 || true)"
if echo "$batch_dispatch_output" | grep -q 'unknown flag: --no-weechat-bootstrap'; then
  echo "  FAIL: add-tenants rejected --no-weechat-bootstrap"
  echo "        $batch_dispatch_output"
  failures=$((failures + 1))
else
  echo "  PASS: add-tenants accepts --no-weechat-bootstrap"
fi

echo ""
if [[ "$failures" -eq 0 ]]; then
  echo "ALL TESTS PASSED"
  exit 0
else
  echo "$failures TEST(S) FAILED"
  exit 1
fi
