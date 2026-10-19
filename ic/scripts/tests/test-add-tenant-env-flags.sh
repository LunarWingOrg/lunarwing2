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

# WI#330: mt-admin no longer ships a built-in TensorZero default, so the env
# writer needs SOME endpoint. Stand in for the fleet env var exactly the way a
# real host's LUNARWING_MT_TENSORZERO_URL would.
DEFAULT_TENSORZERO_URL="http://tz-fleet:3030"

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
tenant_lw_root()    { echo "$MT_FIXTURE"; }
tenant_env_dir()    { echo "$MT_FIXTURE/env"; }
tenant_state_dir()  { echo "$MT_FIXTURE/state"; }
tenant_run_dir()    { echo "$MT_FIXTURE/run"; }
mkdir -p "$MT_FIXTURE/env" "$MT_FIXTURE/state" "$MT_FIXTURE/run"
chmod 700 "$MT_FIXTURE/env"

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
# The real tenant_pg_password definition is kept for the WI#423 pins below.
REAL_TENANT_PG_PASSWORD_DEF="$(declare -f tenant_pg_password)"
tenant_pg_password()      { echo "pgpass-fixture"; }
DARKIRC_FIXTURE_ENABLED=false
tenant_darkirc_enabled()  { [[ "$DARKIRC_FIXTURE_ENABLED" == "true" ]]; }
PROXY_FIXTURE_ENABLED=false
tenant_proxy_enabled()    { [[ "$PROXY_FIXTURE_ENABLED" == "true" ]]; }
VISION_MODE_FIXTURE=local
VISION_EXTERNAL_URL_FIXTURE=""
tenant_vision_mode() { printf '%s\n' "$VISION_MODE_FIXTURE"; }
tenant_vision_external_url() { printf '%s\n' "$VISION_EXTERNAL_URL_FIXTURE"; }
generate_token()          { echo "token-fixture-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; }
say()                     { :; }          # silence
chown()                   { :; }          # no-op (running as non-root in tests)

# WI#390: the env writers commit through the tenant-side atomic writer, which
# drops privileges with sudo -u <tenant>. This harness's tenant does not exist
# as an account, so map the fixture tenant onto the invoking user (the fixture
# env dir is owned by that user at mode 700, satisfying every tenant-side
# ownership/privacy check) and let sudo -u invocations run directly.
sudo() {
  [[ "${1:-}" == "-u" ]] || return 1
  shift 2
  "$@"
}
tenant_cat() { cat -- "$2"; }
original_writer="$(declare -f write_tenant_file_atomic)"
eval "${original_writer/write_tenant_file_atomic ()/original_write_tenant_file_atomic ()}"
write_tenant_file_atomic() {
  local name="$1"
  shift
  [[ "$name" != fixture-tenant ]] || name="$(id -un)"
  original_write_tenant_file_atomic "$name" "$@"
}

run_writer() {  # <args...> — invokes write_tenant_lunarwing_env, prints the file
  rm -f "$MT_FIXTURE/env/lunarwing.env"
  write_tenant_lunarwing_env "$@"
}

envval() {  # <key> — extract DECODED value from the last-written fixture env
  # WI#330: worker base URLs are written with _quote_env_value; read through
  # the same decoder production readers use.
  _read_env_value "$MT_FIXTURE/env/lunarwing.env" "$1"
}

# Case 1: defaults (no flags) on first write
out="$(run_writer fixture-tenant)"
assert_eq "default LLM_MODEL" "$(envval LLM_MODEL)" "tensorzero::function_name::lunarwing"
assert_eq "default GATEWAY_HOST" "$(envval GATEWAY_HOST)" "127.0.0.1"
assert_eq "default MASCOT_ENABLED" "$(envval MASCOT_ENABLED)" "false"
# WI#205: the sandbox must be told which tenant directories hold credentials.
# They are not derivable from LUNARWING_BASE_DIR (state/ is a child of the
# tenant root; env/ and run/ are siblings), so mt-admin names them explicitly.
# WI#287: logs/ too — raw unscrubbed OpenRC logs were mountable read-write.
assert_eq "sandbox protects env, state, run and logs under the tenant root" \
  "$(envval SANDBOX_PROTECTED_PATHS)" "$MT_FIXTURE/env:$MT_FIXTURE/state:$MT_FIXTURE/run:$MT_FIXTURE/logs"
assert_eq "default XMPP_JID" "$(envval XMPP_JID)" "fixture-tenant@xmpp.localhost"
assert_eq "default XMPP_ALLOW_FROM" "$(envval XMPP_ALLOW_FROM)" "user@xmpp.localhost"
write_tenant_bridge_env fixture-tenant
assert_eq "new tenant bridge matches daemon DM allowlist" \
  "$(_env_existing "$MT_FIXTURE/env/xmpp-bridge.env" XMPP_ALLOW_FROM_JSON)" \
  '["user@xmpp.localhost"]'
assert_eq "default AGENT_AUTO_APPROVE_TOOLS" "$(envval AGENT_AUTO_APPROVE_TOOLS)" "false"
assert_eq "no legacy AUTO_APPROVE_TOOLS key" \
  "$(grep -c '^AUTO_APPROVE_TOOLS=' "$MT_FIXTURE/env/lunarwing.env" || true)" "0"
assert_eq "default coordination disabled" "$(envval COORD_ENABLED)" "false"
assert_eq "default coordination agent ID" "$(envval COORD_AGENT_ID)" "fixture-tenant"
assert_eq "default coordination knowledge sharing disabled" \
  "$(envval COORD_KNOWLEDGE_SHARE)" "false"
assert_eq "default coordination knowledge publishing disabled" \
  "$(envval COORD_KNOWLEDGE_PUBLISH)" "false"
assert_eq "default coordination knowledge publishers empty" \
  "$(envval COORD_KNOWLEDGE_PUBLISHERS)" ""
assert_eq "default coordination knowledge hourly cap" \
  "$(envval COORD_KNOWLEDGE_MAX_PER_HOUR)" "30"
assert_eq "default coordination knowledge global hourly cap" \
  "$(envval COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR)" "300"
assert_eq "default coordination knowledge minimum length" \
  "$(envval COORD_KNOWLEDGE_MIN_CHARS)" "12"
assert_eq "coordination admin token generated" \
  "$(envval COORD_ADMIN_TOKEN)" "token-fixture-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
assert_eq "coordination admin port allocated" "$(envval COORD_ADMIN_PORT)" "20009"
assert_eq "default ENGINE_V2" "$(envval ENGINE_V2)" "true"
assert_eq "default ENGINE_V2_CHANNELS" "$(envval ENGINE_V2_CHANNELS)" "xmpp,weechat,darkirc"
assert_eq "default ORCHESTRATOR_SELF_MODIFY" \
  "$(envval ORCHESTRATOR_SELF_MODIFY)" "true"
# WI#291: new tenants get a lightweight-routine tool-round cap above the
# daemon default of 3.
assert_eq "default ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS" \
  "$(envval ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS)" "6"
# WI#291 review R2: the cap does not lift a routine's own budget, and Engine V2
# chats cannot edit v1 routines, so the rendered comment names the host CLI.
assert_eq "env comment names the per-routine CLI" \
  "$(grep -c 'routines edit --name <routine> --max-tool-rounds <n>' "$MT_FIXTURE/env/lunarwing.env" || true)" "1"
# WI#154: MT tenants default to kernel-enforced no-egress sandbox networking.
assert_eq "default SANDBOX_NETWORK" "$(envval SANDBOX_NETWORK)" "none"
# WI#223: MT tenants pin the podman runtime, the lean locally-built sandbox
# image, and no auto-pull (the tag exists only in local stores).
assert_eq "default SANDBOX_RUNTIME" "$(envval SANDBOX_RUNTIME)" "podman"
assert_eq "default SANDBOX_IMAGE" "$(envval SANDBOX_IMAGE)" "lunarwing-sandbox:latest"
assert_eq "default SANDBOX_AUTO_PULL" "$(envval SANDBOX_AUTO_PULL)" "false"
# WI#287: the sandbox-visible daemon log copy is capped at INFO by default.
assert_eq "default SANDBOX_TENANT_LOGS" "$(envval SANDBOX_TENANT_LOGS)" "info"
# Legacy IRONCLAW_* aliases must NOT be generated for new tenants (v2.0.8.0);
# the daemon still accepts them for existing tenants.
assert_eq "base dir written" "$(envval LUNARWING_BASE_DIR)" "$MT_FIXTURE/state"
assert_eq "socket written" "$(envval LUNARWING_SOCKET)" "$MT_FIXTURE/run/lunarwing.sock"
assert_eq "no legacy IRONCLAW lines generated" \
  "$(grep -c '^IRONCLAW_' "$MT_FIXTURE/env/lunarwing.env" || true)" "0"

# Case 2: all three flags on first write
out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "" "" \
  "glm-5-air" "0.0.0.0" "admin@xmpp.org,bob@xmpp.org")"
assert_eq "flag LLM_MODEL" "$(envval LLM_MODEL)" "glm-5-air"
assert_eq "flag GATEWAY_HOST" "$(envval GATEWAY_HOST)" "0.0.0.0"
assert_eq "flag XMPP_ALLOW_FROM" "$(envval XMPP_ALLOW_FROM)" "fixture-tenant@xmpp.localhost,admin@xmpp.org,bob@xmpp.org"

# Case 3: idempotent re-run WITHOUT flags preserves operator-set values.
#   (run_writer deletes the file each call, so simulate a re-run by writing
#   once with flags, then a second time without — but keep the file between.)
rm -f "$MT_FIXTURE/env/lunarwing.env"
write_tenant_lunarwing_env fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "" "" \
  "glm-5-air" "0.0.0.0" "admin@xmpp.org"
sed -i \
  -e 's/^COORD_ENABLED=.*/COORD_ENABLED=true/' \
  -e 's/^COORD_AGENT_ID=.*/COORD_AGENT_ID=custom-agent/' \
  -e 's/^COORD_RESPONDER_ROSTER=.*/COORD_RESPONDER_ROSTER=custom-agent,peer-agent/' \
  -e 's/^COORD_CHANNELS=.*/COORD_CHANNELS=ops@conference.example/' \
  -e 's/^COORD_KNOWLEDGE_SHARE=.*/COORD_KNOWLEDGE_SHARE=true/' \
  -e 's/^COORD_KNOWLEDGE_PUBLISH=.*/COORD_KNOWLEDGE_PUBLISH=true/' \
  -e 's/^COORD_KNOWLEDGE_PUBLISHERS=.*/COORD_KNOWLEDGE_PUBLISHERS=ops-bot@conf.example\/res/' \
  -e 's/^COORD_KNOWLEDGE_MAX_PER_HOUR=.*/COORD_KNOWLEDGE_MAX_PER_HOUR=77/' \
  -e 's/^COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR=.*/COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR=777/' \
  -e 's/^COORD_KNOWLEDGE_MIN_CHARS=.*/COORD_KNOWLEDGE_MIN_CHARS=24/' \
  -e 's/^COORD_ADMIN_TOKEN=.*/COORD_ADMIN_TOKEN=preserved-coord-token/' \
  -e 's/^MASCOT_ENABLED=.*/MASCOT_ENABLED=true/' \
  -e 's/^ENGINE_V2=.*/ENGINE_V2=false/' \
  -e 's/^ENGINE_V2_CHANNELS=.*/ENGINE_V2_CHANNELS=xmpp/' \
  -e 's/^ORCHESTRATOR_SELF_MODIFY=.*/ORCHESTRATOR_SELF_MODIFY=false/' \
  -e 's/^ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS=.*/ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS=4/' \
  -e 's/^SANDBOX_NETWORK=.*/SANDBOX_NETWORK=proxy/' \
  -e 's/^SANDBOX_RUNTIME=.*/SANDBOX_RUNTIME=docker/' \
  -e 's/^SANDBOX_IMAGE=.*/SANDBOX_IMAGE=custom-sandbox:v2/' \
  -e 's/^SANDBOX_AUTO_PULL=.*/SANDBOX_AUTO_PULL=true/' \
  -e 's/^SANDBOX_TENANT_LOGS=.*/SANDBOX_TENANT_LOGS=off/' \
  "$MT_FIXTURE/env/lunarwing.env"
cat >>"$MT_FIXTURE/env/lunarwing.env" <<'EOF'
LW2STUDYMCP_HOME=/srv/lunarwing/lw2studymcp
LW2STUDYMCP_STATE_HOME=/srv/lunarwing/study-state
LW2STUDYMCP_ALLOW_WRITE=1
EOF
# Now re-run WITHOUT the new flags (pass empty for slots 9-11) — file exists.
write_tenant_lunarwing_env fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "" "" \
  "" "" ""
assert_eq "rerun preserves LLM_MODEL" "$(envval LLM_MODEL)" "glm-5-air"
assert_eq "rerun preserves GATEWAY_HOST" "$(envval GATEWAY_HOST)" "0.0.0.0"
assert_eq "rerun preserves MASCOT_ENABLED" "$(envval MASCOT_ENABLED)" "true"
assert_eq "rerun preserves XMPP_ALLOW_FROM" "$(envval XMPP_ALLOW_FROM)" "fixture-tenant@xmpp.localhost,admin@xmpp.org"
assert_eq "rerun preserves COORD_ENABLED" "$(envval COORD_ENABLED)" "true"
assert_eq "rerun preserves COORD_AGENT_ID" "$(envval COORD_AGENT_ID)" "custom-agent"
assert_eq "rerun preserves COORD_RESPONDER_ROSTER" \
  "$(envval COORD_RESPONDER_ROSTER)" "custom-agent,peer-agent"
assert_eq "rerun preserves COORD_CHANNELS" "$(envval COORD_CHANNELS)" "ops@conference.example"
assert_eq "rerun preserves COORD_KNOWLEDGE_SHARE" \
  "$(envval COORD_KNOWLEDGE_SHARE)" "true"
assert_eq "rerun preserves COORD_KNOWLEDGE_PUBLISH" \
  "$(envval COORD_KNOWLEDGE_PUBLISH)" "true"
assert_eq "rerun preserves COORD_KNOWLEDGE_PUBLISHERS" \
  "$(envval COORD_KNOWLEDGE_PUBLISHERS)" "ops-bot@conf.example/res"
assert_eq "rerun preserves COORD_KNOWLEDGE_MAX_PER_HOUR" \
  "$(envval COORD_KNOWLEDGE_MAX_PER_HOUR)" "77"
assert_eq "rerun preserves COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR" \
  "$(envval COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR)" "777"
assert_eq "rerun preserves COORD_KNOWLEDGE_MIN_CHARS" \
  "$(envval COORD_KNOWLEDGE_MIN_CHARS)" "24"
assert_eq "rerun preserves COORD_ADMIN_TOKEN" "$(envval COORD_ADMIN_TOKEN)" "preserved-coord-token"
assert_eq "rerun preserves LW2STUDYMCP_HOME" \
  "$(envval LW2STUDYMCP_HOME)" "/srv/lunarwing/lw2studymcp"
assert_eq "rerun preserves LW2STUDYMCP_STATE_HOME" \
  "$(envval LW2STUDYMCP_STATE_HOME)" "/srv/lunarwing/study-state"
assert_eq "rerun preserves LW2STUDYMCP_ALLOW_WRITE" \
  "$(envval LW2STUDYMCP_ALLOW_WRITE)" "1"
assert_eq "rerun preserves ENGINE_V2" "$(envval ENGINE_V2)" "false"
assert_eq "rerun preserves ENGINE_V2_CHANNELS" "$(envval ENGINE_V2_CHANNELS)" "xmpp"
assert_eq "rerun preserves ORCHESTRATOR_SELF_MODIFY" \
  "$(envval ORCHESTRATOR_SELF_MODIFY)" "false"
assert_eq "rerun preserves ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS" \
  "$(envval ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS)" "4"
assert_eq "rerun preserves SANDBOX_NETWORK" "$(envval SANDBOX_NETWORK)" "proxy"
assert_eq "rerun preserves SANDBOX_RUNTIME" "$(envval SANDBOX_RUNTIME)" "docker"
assert_eq "rerun preserves SANDBOX_IMAGE" "$(envval SANDBOX_IMAGE)" "custom-sandbox:v2"
assert_eq "rerun preserves SANDBOX_AUTO_PULL" "$(envval SANDBOX_AUTO_PULL)" "true"
assert_eq "rerun preserves SANDBOX_TENANT_LOGS" "$(envval SANDBOX_TENANT_LOGS)" "off"

echo "=== approval setting migration tests ==="
# Canonical key must win even when a conflicting legacy value is present.
sed -i 's/^AGENT_AUTO_APPROVE_TOOLS=.*/AGENT_AUTO_APPROVE_TOOLS=true/' \
  "$MT_FIXTURE/env/lunarwing.env"
printf 'AUTO_APPROVE_TOOLS=false\n' >>"$MT_FIXTURE/env/lunarwing.env"
write_tenant_lunarwing_env fixture-tenant
assert_eq "rerun preserves canonical approval override" "$(envval AGENT_AUTO_APPROVE_TOOLS)" "true"
assert_eq "rerun drops legacy approval key" \
  "$(grep -c '^AUTO_APPROVE_TOOLS=' "$MT_FIXTURE/env/lunarwing.env" || true)" "0"
write_tenant_lunarwing_env fixture-tenant
assert_eq "second rerun preserves canonical approval override" "$(envval AGENT_AUTO_APPROVE_TOOLS)" "true"

# Older tenants may have only the alias; migrate its value without dropping it.
sed -i '/^AGENT_AUTO_APPROVE_TOOLS=/d' "$MT_FIXTURE/env/lunarwing.env"
printf 'AUTO_APPROVE_TOOLS=true\n' >>"$MT_FIXTURE/env/lunarwing.env"
write_tenant_lunarwing_env fixture-tenant
assert_eq "legacy approval override migrates" "$(envval AGENT_AUTO_APPROVE_TOOLS)" "true"
assert_eq "migration removes legacy approval key" \
  "$(grep -c '^AUTO_APPROVE_TOOLS=' "$MT_FIXTURE/env/lunarwing.env" || true)" "0"

# A canonical false must not be replaced by a conflicting legacy true.
sed -i 's/^AGENT_AUTO_APPROVE_TOOLS=.*/AGENT_AUTO_APPROVE_TOOLS=false/' \
  "$MT_FIXTURE/env/lunarwing.env"
printf 'AUTO_APPROVE_TOOLS=true\n' >>"$MT_FIXTURE/env/lunarwing.env"
write_tenant_lunarwing_env fixture-tenant
assert_eq "canonical false beats legacy true" "$(envval AGENT_AUTO_APPROVE_TOOLS)" "false"

# Case 3b: explicit add-tenant re-run — the full env writer fills missing keys
# from an older file. upgrade-tenant calls patch_tenant_env, not this writer.
sed -i \
  -e '/^MASCOT_ENABLED=/d' \
  -e '/^ENGINE_V2=/d' \
  -e '/^ENGINE_V2_CHANNELS=/d' \
  -e '/^ORCHESTRATOR_SELF_MODIFY=/d' \
  -e '/^ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS=/d' \
  -e '/^SANDBOX_NETWORK=/d' \
  -e '/^SANDBOX_RUNTIME=/d' \
  -e '/^SANDBOX_IMAGE=/d' \
  -e '/^SANDBOX_AUTO_PULL=/d' \
  -e '/^SANDBOX_TENANT_LOGS=/d' \
  "$MT_FIXTURE/env/lunarwing.env"
write_tenant_lunarwing_env fixture-tenant
assert_eq "re-run adds MASCOT_ENABLED default" "$(envval MASCOT_ENABLED)" "false"
assert_eq "re-run adds ENGINE_V2 default" "$(envval ENGINE_V2)" "true"
assert_eq "re-run adds ENGINE_V2_CHANNELS default" \
  "$(envval ENGINE_V2_CHANNELS)" "xmpp,weechat,darkirc"
assert_eq "re-run adds ORCHESTRATOR_SELF_MODIFY default" \
  "$(envval ORCHESTRATOR_SELF_MODIFY)" "true"
assert_eq "re-run adds ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS default" \
  "$(envval ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS)" "6"
assert_eq "re-run adds SANDBOX_NETWORK default" "$(envval SANDBOX_NETWORK)" "none"
assert_eq "re-run adds SANDBOX_RUNTIME default" "$(envval SANDBOX_RUNTIME)" "podman"
assert_eq "re-run adds SANDBOX_IMAGE default" "$(envval SANDBOX_IMAGE)" "lunarwing-sandbox:latest"
assert_eq "re-run adds SANDBOX_AUTO_PULL default" "$(envval SANDBOX_AUTO_PULL)" "false"
assert_eq "re-run adds SANDBOX_TENANT_LOGS default" "$(envval SANDBOX_TENANT_LOGS)" "info"

# Case 3c: external LunarVision mode must survive every full env re-render.
VISION_MODE_FIXTURE=external
VISION_EXTERNAL_URL_FIXTURE=https://vision.example.test:8443
_env_set "$MT_FIXTURE/env/lunarwing.env" VISION_AUTH_TOKEN 'external-fixture-token'
write_tenant_lunarwing_env fixture-tenant
assert_eq "rerun preserves external LunarVision URL" \
  "$(_env_existing "$MT_FIXTURE/env/lunarwing.env" VISION_SERVICE_URL)" \
  "https://vision.example.test:8443"
assert_eq "rerun preserves external LunarVision token" \
  "$(_env_existing "$MT_FIXTURE/env/lunarwing.env" VISION_AUTH_TOKEN)" \
  "external-fixture-token"
VISION_MODE_FIXTURE=local
VISION_EXTERNAL_URL_FIXTURE=""

# Case 4: a DarkIRC-enabled re-render must preserve the existing adapter secret.
DARKIRC_FIXTURE_ENABLED=true
printf '\nDARKIRC_ADAPTER_SECRET=preserved-darkirc-fixture\n' >>"$MT_FIXTURE/env/lunarwing.env"
write_tenant_lunarwing_env fixture-tenant
assert_eq "rerun preserves DARKIRC_ADAPTER_SECRET" \
  "$(envval DARKIRC_ADAPTER_SECRET)" "preserved-darkirc-fixture"
DARKIRC_FIXTURE_ENABLED=false

# Case 5: LLM_BASE_URL derivation (v2.1.0.0 item-23 rerun finding 1).
# Regression: a proxy-disabled tenant provisioned with a bare TensorZero root
# (--tensorzero-url http://host:3000) used to get LLM_BASE_URL=<bare root>, and
# the daemon's openai_compatible client then 404'd every turn on
# POST /chat/completions while /api/health stayed healthy. The derivation must
# append TensorZero's OpenAI-compatible surface (/openai/v1).
out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "" "" \
  "glm-5-air" "0.0.0.0" "admin@xmpp.org,bob@xmpp.org")"
assert_eq "bare tensorzero root gains /openai/v1" \
  "$(envval LLM_BASE_URL)" "http://tz:3030/openai/v1"
out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030/" "")"
assert_eq "trailing slash stripped before suffix" \
  "$(envval LLM_BASE_URL)" "http://tz:3030/openai/v1"
out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://192.0.2.1:3000/openai/v1" "")"
assert_eq "already-suffixed tensorzero URL unchanged" \
  "$(envval LLM_BASE_URL)" "http://192.0.2.1:3000/openai/v1"
out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "https://gw.example.test/tz" "")"
assert_eq "reverse-proxy prefix kept" \
  "$(envval LLM_BASE_URL)" "https://gw.example.test/tz/openai/v1"
# An explicit --llm-base-url is an operator contract: passed through verbatim,
# never suffixed (non-TensorZero providers use their own path shapes).
out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "" "http://custom:9000/v1")"
assert_eq "explicit llm-base-url preserved verbatim" \
  "$(envval LLM_BASE_URL)" "http://custom:9000/v1"
# Proxy enabled: the tenant dials its own loopback proxy on /v1; the upstream
# TensorZero URL is not consulted for LLM_BASE_URL.
PROXY_FIXTURE_ENABLED=true
out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "")"
assert_eq "proxy enabled dials local proxy /v1" \
  "$(envval LLM_BASE_URL)" "http://127.0.0.1:10004/v1"
out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "" "http://custom:9000/v2")"
assert_eq "proxy enabled + explicit llm-base-url wins" \
  "$(envval LLM_BASE_URL)" "http://custom:9000/v2"
PROXY_FIXTURE_ENABLED=false

# Case 6: WI#330 — unset worker base URLs inherit the tenant's LLM endpoint.
# The worker images ship a placeholder endpoint their entrypoints refuse to
# run, so write_tenant_lunarwing_env must always render OPENCODE_BASE_URL /
# KIMI_BASE_URL / OPENCODE2_BASE_URL when it has a container-reachable source:
# --llm-base-url wins, else the tenant's TensorZero upstream (path-aware),
# even when the daemon itself dials the loopback proxy (unreachable from
# worker containers).
out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "")"
assert_eq "worker default: opencode inherits tensorzero upstream" \
  "$(envval OPENCODE_BASE_URL)" "http://tz:3030/openai/v1"
assert_eq "worker default: kimi inherits tensorzero upstream" \
  "$(envval KIMI_BASE_URL)" "http://tz:3030/openai/v1"
assert_eq "worker default: opencode2 inherits tensorzero upstream" \
  "$(envval OPENCODE2_BASE_URL)" "http://tz:3030/openai/v1"
# Inherited values carry a provenance marker (re-derived on re-render, skipped
# by export-tenant) and are written with the production quoting.
assert_eq "worker default: opencode marked inherited" \
  "$(envval OPENCODE_BASE_URL_SOURCE)" "inherited"
assert_eq "worker default: kimi marked inherited" \
  "$(envval KIMI_BASE_URL_SOURCE)" "inherited"
assert_eq "worker default: opencode2 marked inherited" \
  "$(envval OPENCODE2_BASE_URL_SOURCE)" "inherited"
assert_eq "worker default: value written quoted" \
  "$(grep -m1 '^OPENCODE_BASE_URL=' "$MT_FIXTURE/env/lunarwing.env")" \
  'OPENCODE_BASE_URL="http://tz:3030/openai/v1"'

out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "" "http://custom:9000/v1")"
assert_eq "worker default: explicit llm-base-url wins over tensorzero" \
  "$(envval OPENCODE_BASE_URL)" "http://custom:9000/v1"
assert_eq "worker default: kimi inherits explicit llm-base-url" \
  "$(envval KIMI_BASE_URL)" "http://custom:9000/v1"
assert_eq "worker default: opencode2 inherits explicit llm-base-url" \
  "$(envval OPENCODE2_BASE_URL)" "http://custom:9000/v1"

PROXY_FIXTURE_ENABLED=true
out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "")"
assert_eq "worker default: daemon dials loopback proxy" \
  "$(envval LLM_BASE_URL)" "http://127.0.0.1:10004/v1"
assert_eq "worker default: proxy loopback NOT inherited by workers" \
  "$(envval OPENCODE_BASE_URL)" "http://tz:3030/openai/v1"
assert_eq "worker default: kimi gets upstream, not loopback" \
  "$(envval KIMI_BASE_URL)" "http://tz:3030/openai/v1"
PROXY_FIXTURE_ENABLED=false

# CX-6: an explicit loopback --llm-base-url is a legitimate daemon endpoint but
# must NOT be inherited by worker containers — fall through to the upstream.
out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "" "http://127.0.0.1:9000/v1")"
assert_eq "loopback llm-base-url: daemon keeps it" \
  "$(envval LLM_BASE_URL)" "http://127.0.0.1:9000/v1"
assert_eq "loopback llm-base-url: workers skip to tensorzero upstream" \
  "$(envval OPENCODE_BASE_URL)" "http://tz:3030/openai/v1"
assert_eq "loopback llm-base-url: kimi skips to tensorzero upstream" \
  "$(envval KIMI_BASE_URL)" "http://tz:3030/openai/v1"

# CX-6: an overlay MagicDNS endpoint is unusable from rootless containers.
out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "" \
  "http://node1.netbird.selfhosted:3000/openai/v1")"
assert_eq "MagicDNS llm-base-url: daemon keeps it" \
  "$(envval LLM_BASE_URL)" "http://node1.netbird.selfhosted:3000/openai/v1"
assert_eq "MagicDNS llm-base-url: workers skip to tensorzero upstream" \
  "$(envval OPENCODE_BASE_URL)" "http://tz:3030/openai/v1"

# CX-1: a MagicDNS TensorZero URL with no llm-base-url leaves workers with NO
# rendered endpoint (workers are disabled in this fixture, so nothing dies) —
# never a silent developer-network fallback.
out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://node1.netbird.selfhosted:3000" "")"
assert_eq "MagicDNS tensorzero: daemon still derives its endpoint" \
  "$(envval LLM_BASE_URL)" "http://node1.netbird.selfhosted:3000/openai/v1"
assert_eq "MagicDNS tensorzero: no opencode worker URL rendered" \
  "$(grep -c '^OPENCODE_BASE_URL=' "$MT_FIXTURE/env/lunarwing.env" || true)" "0"
assert_eq "MagicDNS tensorzero: no kimi worker URL rendered" \
  "$(grep -c '^KIMI_BASE_URL=' "$MT_FIXTURE/env/lunarwing.env" || true)" "0"
assert_eq "MagicDNS tensorzero: no opencode2 worker URL rendered" \
  "$(grep -c '^OPENCODE2_BASE_URL=' "$MT_FIXTURE/env/lunarwing.env" || true)" "0"

# Explicit per-worker flags beat the inherited default and carry no marker.
out="$(run_writer fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz:3030" "" "" \
  "" "" "" "" "http://oc:4000/v1" "" "http://kimi:4001/v1" "" "" "http://oc2:4002/v1")"
assert_eq "explicit opencode base url wins" "$(envval OPENCODE_BASE_URL)" "http://oc:4000/v1"
assert_eq "explicit kimi base url wins"     "$(envval KIMI_BASE_URL)"     "http://kimi:4001/v1"
assert_eq "explicit opencode2 base url wins" "$(envval OPENCODE2_BASE_URL)" "http://oc2:4002/v1"
assert_eq "explicit opencode url carries no inherited marker" \
  "$(grep -c '^OPENCODE_BASE_URL_SOURCE=' "$MT_FIXTURE/env/lunarwing.env" || true)" "0"

# Re-render: operator-set (unmarked) worker URLs survive; INHERITED values
# re-derive from the tenant's current endpoint (CX-3 — a TensorZero move must
# reach the workers without a per-worker configure-* call).
rm -f "$MT_FIXTURE/env/lunarwing.env"
write_tenant_lunarwing_env fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz-a:3030" "" "" \
  "" "" "" "" "http://oc:4000/v1"
assert_eq "first render: explicit opencode kept" \
  "$(envval OPENCODE_BASE_URL)" "http://oc:4000/v1"
assert_eq "first render: kimi inherited from tz A" \
  "$(envval KIMI_BASE_URL)" "http://tz-a:3030/openai/v1"
assert_eq "first render: kimi marked inherited" \
  "$(envval KIMI_BASE_URL_SOURCE)" "inherited"
write_tenant_lunarwing_env fixture-tenant \
  "fixture-tenant@xmpp.localhost" "" "http://tz-b:3030" ""
assert_eq "rerun: explicit opencode preserved" \
  "$(envval OPENCODE_BASE_URL)" "http://oc:4000/v1"
assert_eq "rerun: explicit opencode still unmarked" \
  "$(grep -c '^OPENCODE_BASE_URL_SOURCE=' "$MT_FIXTURE/env/lunarwing.env" || true)" "0"
assert_eq "rerun: inherited kimi re-derived to tz B" \
  "$(envval KIMI_BASE_URL)" "http://tz-b:3030/openai/v1"
assert_eq "rerun: kimi still marked inherited" \
  "$(envval KIMI_BASE_URL_SOURCE)" "inherited"
assert_eq "rerun: opencode2 inherited from tz B" \
  "$(envval OPENCODE2_BASE_URL)" "http://tz-b:3030/openai/v1"

echo "=== configure-worker inheritance across partial updates (WI#330) ==="
(
  starting_failures="$failures"
  tenant_exists_in_registry() { :; }
  _ctr() { return 1; }
  run_writer fixture-tenant "" "" "http://tz-a:3030"
  for worker in opencode kimi opencode2; do
    key="${worker^^}_BASE_URL"
    "configure_$worker" fixture-tenant "model-only" ""
    assert_eq "$worker model-only keeps URL" "$(envval "$key")" "http://tz-a:3030/openai/v1"
    assert_eq "$worker model-only keeps provenance" "$(envval "${key}_SOURCE")" "inherited"
  done
  configure_kimi fixture-tenant "" "" "fixture-api-key"
  assert_eq "kimi API-key-only keeps URL provenance" "$(envval KIMI_BASE_URL_SOURCE)" "inherited"
  write_tenant_lunarwing_env fixture-tenant "" "" "http://tz-b:3030"
  for worker in opencode kimi opencode2; do
    key="${worker^^}_BASE_URL"
    assert_eq "$worker follows upstream after partial configure" "$(envval "$key")" "http://tz-b:3030/openai/v1"
    # Even the same URL explicitly supplied is an intentional override.
    "configure_$worker" fixture-tenant "" "http://tz-b:3030/openai/v1"
    assert_eq "$worker explicit URL removes provenance" "$(envval "${key}_SOURCE")" ""
    assert_eq "$worker URL-only keeps model" "$(envval "${worker^^}_MODEL")" "model-only"
  done
  write_tenant_lunarwing_env fixture-tenant "" "" "http://tz-c:3030"
  for worker in opencode kimi opencode2; do
    assert_eq "$worker explicit URL survives upstream move" "$(envval "${worker^^}_BASE_URL")" "http://tz-b:3030/openai/v1"
    before="$(cat "$MT_FIXTURE/env/lunarwing.env")"
    if ("configure_$worker" fixture-tenant "" $'http://gw.example.test/\nSANDBOX_ALLOW_FULL_ACCESS=true') 2>/dev/null; then
      echo "  FAIL: $worker accepted a multiline URL"; failures=$((failures + 1))
    else
      echo "  PASS: $worker rejects a multiline URL"
    fi
    assert_eq "$worker invalid update leaves env intact" "$(cat "$MT_FIXTURE/env/lunarwing.env")" "$before"
  done
  [[ "$failures" -eq "$starting_failures" ]]
)

echo "=== WI#330 URL helper unit tests ==="

# validate_llm_url dies on bad shapes (die inside a subshell exits it).
if (validate_llm_url "http://gw.example.test:3000/openai/v1" "--tensorzero-url"); then
  echo "  PASS: validate accepts a normal https URL"
else
  echo "  FAIL: validate rejected a normal URL"; failures=$((failures + 1))
fi
for bad in "ftp://gw.example.test" "http://gw.example.test/has space" \
  "http://user:pw@gw.example.test/v1" "http://gw.example.test/100%/v1" \
  'http://gw.example.test/`command`' 'http://gw.example.test/"quote"' \
  'http://gw.example.test/back\slash' 'http://gw.example.test/${IFS}' \
  'http://gw.example.test/a|b' 'http://gw.example.test/<tag>' \
  'http://gw.example.test/nonascii-é' \
  "https:///no-host" $'http://gw.example.test/inject\nSANDBOX_ALLOW_FULL_ACCESS=true'; do
  if (validate_llm_url "$bad" "--tensorzero-url") 2>/dev/null; then
    echo "  FAIL: validate accepted [$bad]"; failures=$((failures + 1))
  else
    echo "  PASS: validate rejects [$bad]"
  fi
done

# RFC 3986 delimiters are valid data, including some shell metacharacters.
# These must never be relied on as shell-safe simply because validation passed.
for good in 'https://gw.example.test/a;v=1?q=one&other=two#fragment' \
  'https://gw.example.test/$(literal)' 'http://[::1]:3000/openai/v1' \
  "https://gw.example.test/one'two!three*four+five,six=~_-"; do
  if (validate_llm_url "$good" fixture); then
    echo "  PASS: validate accepts RFC URI delimiters"
  else
    echo "  FAIL: validate rejected URI delimiters"; failures=$((failures + 1))
  fi
done

# worker_llm_url_container_ok: loopback and overlay MagicDNS are unreachable.
for unreachable in "http://127.0.0.1:3000/v1" "http://localhost:3000" \
  "http://[::1]:3000/" "http://node1.netbird.selfhosted:3000" \
  "https://host.tail1234.ts.net/" "http://x.netbird.cloud"; do
  if worker_llm_url_container_ok "$unreachable"; then
    echo "  FAIL: container-ok accepted [$unreachable]"; failures=$((failures + 1))
  else
    echo "  PASS: container-ok rejects [$unreachable]"
  fi
done
for reachable in "http://10.0.0.5:3000/openai/v1" "https://gw.example.test/" \
  "http://tz:3030"; do
  if worker_llm_url_container_ok "$reachable"; then
    echo "  PASS: container-ok accepts [$reachable]"
  else
    echo "  FAIL: container-ok rejected [$reachable]"; failures=$((failures + 1))
  fi
done

# derive_worker_llm_base_url precedence + fall-through.
assert_eq "derive: llm-base-url wins" \
  "$(derive_worker_llm_base_url "http://custom:9000/v1" "http://tz:3030")" \
  "http://custom:9000/v1"
assert_eq "derive: loopback llm falls through to tensorzero" \
  "$(derive_worker_llm_base_url "http://127.0.0.1:10004/v1" "http://tz:3030")" \
  "http://tz:3030/openai/v1"
assert_eq "derive: MagicDNS tensorzero unusable -> empty" \
  "$(derive_worker_llm_base_url "" "http://node1.netbird.selfhosted:3000" || true)" ""
assert_eq "derive: nothing usable -> empty" \
  "$(derive_worker_llm_base_url "" "" || true)" ""

# require_tenant_llm_endpoint: fail loudly instead of rendering nothing.
if (require_tenant_llm_endpoint "" "" "false") 2>/dev/null; then
  echo "  FAIL: no endpoint accepted"; failures=$((failures + 1))
else
  echo "  PASS: no tensorzero + no llm-base-url rejected"
fi
if (require_tenant_llm_endpoint "" "http://custom:9000/v1" "false") 2>/dev/null; then
  echo "  PASS: llm-base-url alone suffices without the proxy"
else
  echo "  FAIL: llm-base-url-only tenant rejected"; failures=$((failures + 1))
fi
if (require_tenant_llm_endpoint "" "http://custom:9000/v1" "true") 2>/dev/null; then
  echo "  FAIL: proxy tenant without tensorzero accepted"; failures=$((failures + 1))
else
  echo "  PASS: proxy tenant without tensorzero rejected"
fi

echo "=== tensorzero_openai_base unit tests ==="
assert_eq "unit bare root suffixed" \
  "$(tensorzero_openai_base "http://tz:3030")" "http://tz:3030/openai/v1"
assert_eq "unit trailing slash stripped" \
  "$(tensorzero_openai_base "http://tz:3030/")" "http://tz:3030/openai/v1"
assert_eq "unit multi trailing slashes stripped" \
  "$(tensorzero_openai_base "http://tz:3030//")" "http://tz:3030/openai/v1"
assert_eq "unit already suffixed unchanged" \
  "$(tensorzero_openai_base "http://tz:3030/openai/v1")" "http://tz:3030/openai/v1"
assert_eq "unit /openai completed to /openai/v1" \
  "$(tensorzero_openai_base "http://tz:3030/openai")" "http://tz:3030/openai/v1"
assert_eq "unit path prefix preserved" \
  "$(tensorzero_openai_base "https://gw.example.test/tz")" "https://gw.example.test/tz/openai/v1"

echo "=== coordination env backfill tests ==="
cat >"$MT_FIXTURE/env/lunarwing.env" <<'EOF'
LUNARWING_OWNER_ID=fixture-tenant
EOF
chmod 600 "$MT_FIXTURE/env/lunarwing.env"
patch_tenant_coordination_env fixture-tenant "$MT_FIXTURE/env/lunarwing.env"
assert_eq "backfill coordination disabled" "$(envval COORD_ENABLED)" "false"
assert_eq "backfill coordination agent ID" "$(envval COORD_AGENT_ID)" "fixture-tenant"
assert_eq "backfill coordination channels" "$(envval COORD_CHANNELS)" ""
assert_eq "backfill coordination aliases" "$(envval COORD_AGENT_ALIASES)" ""
assert_eq "backfill coordination responder roster" "$(envval COORD_RESPONDER_ROSTER)" ""
assert_eq "backfill coordination knowledge disabled" \
  "$(envval COORD_KNOWLEDGE_SHARE)" "false"
assert_eq "backfill coordination knowledge hourly cap" \
  "$(envval COORD_KNOWLEDGE_MAX_PER_HOUR)" "30"
assert_eq "backfill coordination knowledge global hourly cap" \
  "$(envval COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR)" "300"
assert_eq "backfill coordination knowledge minimum length" \
  "$(envval COORD_KNOWLEDGE_MIN_CHARS)" "12"
assert_eq "backfill coordination admin users" "$(envval COORD_ADMIN_USERS)" ""
assert_eq "backfill coordination token" \
  "$(envval COORD_ADMIN_TOKEN)" "token-fixture-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
assert_eq "backfill coordination admin port" "$(envval COORD_ADMIN_PORT)" "20009"
sed -i \
  -e 's/^COORD_KNOWLEDGE_SHARE=.*/COORD_KNOWLEDGE_SHARE=true/' \
  -e 's/^COORD_KNOWLEDGE_MAX_PER_HOUR=.*/COORD_KNOWLEDGE_MAX_PER_HOUR=88/' \
  -e 's/^COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR=.*/COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR=888/' \
  -e 's/^COORD_KNOWLEDGE_MIN_CHARS=.*/COORD_KNOWLEDGE_MIN_CHARS=20/' \
  -e 's/^COORD_ADMIN_TOKEN=.*/COORD_ADMIN_TOKEN=backfill-preserved/' \
  "$MT_FIXTURE/env/lunarwing.env"
patch_tenant_coordination_env fixture-tenant "$MT_FIXTURE/env/lunarwing.env"
assert_eq "backfill knowledge opt-in remains stable" \
  "$(envval COORD_KNOWLEDGE_SHARE)" "true"
assert_eq "backfill knowledge hourly cap remains stable" \
  "$(envval COORD_KNOWLEDGE_MAX_PER_HOUR)" "88"
assert_eq "backfill knowledge global hourly cap remains stable" \
  "$(envval COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR)" "888"
assert_eq "backfill knowledge minimum remains stable" \
  "$(envval COORD_KNOWLEDGE_MIN_CHARS)" "20"
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
assert_eq "custom JID still defaults to local user allowlist" \
  "$(envval XMPP_ALLOW_FROM)" "user@xmpp.localhost"

# Re-run with NO jid argument -> must preserve, not default to xmpp.localhost.
write_tenant_lunarwing_env fixture-tenant
assert_eq "rerun preserves XMPP_JID" \
  "$(envval XMPP_JID)" "fixture-tenant@xmpp.sobe.world"

# Re-run with an empty-string jid (how the dispatcher passes "flag absent").
write_tenant_lunarwing_env fixture-tenant ""
assert_eq "empty-arg rerun preserves XMPP_JID" \
  "$(envval XMPP_JID)" "fixture-tenant@xmpp.sobe.world"
assert_eq "rerun preserves local user allowlist" \
  "$(envval XMPP_ALLOW_FROM)" "user@xmpp.localhost"

# The bridge env writer must preserve from lunarwing.env too.
rm -f "$MT_FIXTURE/env/xmpp-bridge.env"
write_tenant_bridge_env fixture-tenant
assert_eq "bridge writer preserves XMPP_JID" \
  "$(grep -m1 '^XMPP_JID=' "$MT_FIXTURE/env/xmpp-bridge.env" | cut -d= -f2-)" \
  "fixture-tenant@xmpp.sobe.world"
assert_eq "custom JID bridge still allows local user by default" \
  "$(_env_existing "$MT_FIXTURE/env/xmpp-bridge.env" XMPP_ALLOW_FROM_JSON)" \
  '["user@xmpp.localhost"]'

sed -i 's/^XMPP_ALLOW_FROM=.*/XMPP_ALLOW_FROM=operator@chat.example.net/' \
  "$MT_FIXTURE/env/lunarwing.env"
write_tenant_lunarwing_env fixture-tenant
write_tenant_bridge_env fixture-tenant
assert_eq "rerun preserves operator-only allowlist" \
  "$(envval XMPP_ALLOW_FROM)" "operator@chat.example.net"
assert_eq "bridge mirrors operator-only allowlist after rerun" \
  "$(_env_existing "$MT_FIXTURE/env/xmpp-bridge.env" XMPP_ALLOW_FROM_JSON)" \
  '["operator@chat.example.net"]'

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
chmod 600 "$MT_FIXTURE/env/lunarwing.env"
assert_eq "resolver: stored wins over default" \
  "$(resolve_tenant_xmpp_jid fixture-tenant "")" "stored@xmpp.sobe.world"
assert_eq "resolver: explicit wins over stored" \
  "$(resolve_tenant_xmpp_jid fixture-tenant "cli@xmpp.example.net")" "cli@xmpp.example.net"

echo "=== WI#390: env writers refuse symlinked/foreign targets ==="

# A pre-placed symlink at the destination must never be followed: the write
# fails closed and the attacker-chosen target keeps its content.
sensitive="$MT_FIXTURE/sensitive-target"
printf 'DO-NOT-TRUNCATE\n' >"$sensitive"

ln -s "$sensitive" "$MT_FIXTURE/env/proxy.env"
if ( write_tenant_proxy_env fixture-tenant ) >/dev/null 2>&1; then
  echo "  FAIL: proxy env writer followed a symlinked target"; failures=$((failures + 1))
else
  assert_eq "proxy.env symlink refusal preserves target" "$(cat "$sensitive")" "DO-NOT-TRUNCATE"
fi
assert_eq "proxy.env symlink left in place" \
  "$(if [[ -L "$MT_FIXTURE/env/proxy.env" ]]; then echo symlink; else echo replaced; fi)" "symlink"

rm -f "$MT_FIXTURE/env/lunarwing.env"
ln -s "$sensitive" "$MT_FIXTURE/env/lunarwing.env"
if ( write_tenant_lunarwing_env fixture-tenant ) >/dev/null 2>&1; then
  echo "  FAIL: lunarwing env writer followed a symlinked target"; failures=$((failures + 1))
else
  assert_eq "lunarwing.env symlink refusal preserves target" "$(cat "$sensitive")" "DO-NOT-TRUNCATE"
fi

rm -f "$MT_FIXTURE/env/xmpp-bridge.env"
ln -s "$sensitive" "$MT_FIXTURE/env/xmpp-bridge.env"
if ( write_tenant_bridge_env fixture-tenant ) >/dev/null 2>&1; then
  echo "  FAIL: bridge env writer followed a symlinked target"; failures=$((failures + 1))
else
  assert_eq "xmpp-bridge.env symlink refusal preserves target" "$(cat "$sensitive")" "DO-NOT-TRUNCATE"
fi

# A dangling symlink (target absent) must not be created/truncated either.
rm -f "$MT_FIXTURE/env/proxy.env"
ln -s "$MT_FIXTURE/env/.dangling-target" "$MT_FIXTURE/env/proxy.env"
if ( write_tenant_proxy_env fixture-tenant ) >/dev/null 2>&1; then
  echo "  FAIL: proxy env writer created a dangling symlink's target"; failures=$((failures + 1))
else
  echo "  PASS: proxy env writer refuses a dangling symlink"
fi
if [[ -e "$MT_FIXTURE/env/.dangling-target" ]]; then
  echo "  FAIL: dangling symlink target was created"; failures=$((failures + 1))
fi

# The append helper fails closed on a symlinked target too, and round-trips a
# clean one through the tenant-side atomic writer.
rm -f "$MT_FIXTURE/env/lunarwing.env"
printf 'LLM_BASE_URL=http://direct:9000/v1\n' >"$MT_FIXTURE/env/lunarwing.env"
chmod 600 "$MT_FIXTURE/env/lunarwing.env"
if ( _tenant_env_append fixture-tenant "$MT_FIXTURE/env/lunarwing.env" <<< 'PIN_OK=1' ); then
  assert_eq "_tenant_env_append round-trips through the atomic writer" "$(envval PIN_OK)" "1"
else
  echo "  FAIL: _tenant_env_append refused a clean target"; failures=$((failures + 1))
fi
rm -f "$MT_FIXTURE/env/lunarwing.env"
ln -s "$sensitive" "$MT_FIXTURE/env/lunarwing.env"
if ( printf 'EVIL=1\n' | _tenant_env_append fixture-tenant "$MT_FIXTURE/env/lunarwing.env" ) >/dev/null 2>&1; then
  echo "  FAIL: _tenant_env_append followed a symlinked target"; failures=$((failures + 1))
else
  assert_eq "_tenant_env_append symlink refusal preserves target" "$(cat "$sensitive")" "DO-NOT-TRUNCATE"
fi

# Leave no symlinks behind: later sections rewrite these fixtures with plain
# redirects, which would follow them.
rm -f "$MT_FIXTURE/env/lunarwing.env" "$MT_FIXTURE/env/xmpp-bridge.env" "$MT_FIXTURE/env/proxy.env"

echo "=== WI#390 (HH-5): 02-containers/09-worker-config symlink pins ==="

# 02-containers.sh: the vision env writer must refuse a symlinked vision.env
# (previously nothing in this harness pinned anything in 02-containers.sh).
ln -s "$sensitive" "$MT_FIXTURE/env/vision.env"
if ( write_tenant_vision_env fixture-tenant ) >/dev/null 2>&1; then
  echo "  FAIL: vision env writer followed a symlinked target"; failures=$((failures + 1))
else
  assert_eq "vision.env symlink refusal preserves target" "$(cat "$sensitive")" "DO-NOT-TRUNCATE"
fi
rm -f "$MT_FIXTURE/env/vision.env"

# 09-worker-config.sh: the external-worker config writer must refuse a
# symlinked config.toml in the tenant state dir.
printf 'WORKER_AUTH_TOKEN=fixture-worker-token\n' >"$MT_FIXTURE/env/lunarwing.env"
chmod 600 "$MT_FIXTURE/env/lunarwing.env"
ln -s "$sensitive" "$MT_FIXTURE/state/config.toml"
if ( ensure_external_worker_config fixture-tenant pebble pebble_wss ) >/dev/null 2>&1; then
  echo "  FAIL: external-worker config writer followed a symlinked target"; failures=$((failures + 1))
else
  assert_eq "config.toml symlink refusal preserves target" "$(cat "$sensitive")" "DO-NOT-TRUNCATE"
fi
rm -f "$MT_FIXTURE/state/config.toml"
# Clean control: a plain absent config.toml gets the worker block, proving the
# refusal above came from the symlink and not from an unrelated early failure.
if ( ensure_external_worker_config fixture-tenant pebble pebble_wss ) >/dev/null 2>&1; then
  assert_eq "external-worker config written for a clean target" \
    "$(grep -c 'name = "pebble"' "$MT_FIXTURE/state/config.toml")" "1"
else
  echo "  FAIL: external-worker config refused a clean target"; failures=$((failures + 1))
fi
rm -f "$MT_FIXTURE/state/config.toml"

# 09-worker-config.sh (HH-4): prepare_pebble_workspace must refuse a symlinked
# workspace and leave it in place — no root chown/chmod may follow it. The
# helper resolves the tenant uid, so run it as the invoking user (the harness's
# fixture-tenant is not a real account; tenant_lw_root is stubbed anyway).
hh4_tenant="$(id -un)"
mkdir -p "$MT_FIXTURE/real-workspace-victim"
ln -s "$MT_FIXTURE/real-workspace-victim" "$MT_FIXTURE/pebble-workspace"
if ( prepare_pebble_workspace "$hh4_tenant" ) >/dev/null 2>&1; then
  echo "  FAIL: prepare_pebble_workspace accepted a symlinked workspace"; failures=$((failures + 1))
else
  echo "  PASS: prepare_pebble_workspace refuses a symlinked workspace"
fi
assert_eq "pebble workspace symlink left in place" \
  "$(if [[ -L "$MT_FIXTURE/pebble-workspace" ]]; then echo symlink; else echo replaced; fi)" "symlink"
rm -f "$MT_FIXTURE/pebble-workspace"
# Clean control: an absent workspace is created tenant-private (0700, no 0777).
ws_out="$(prepare_pebble_workspace "$hh4_tenant")" || ws_out=""
assert_eq "pebble workspace created at the tenant path" "$ws_out" "$MT_FIXTURE/pebble-workspace"
assert_eq "pebble workspace is tenant-private (0700)" \
  "$(stat -c %a "$MT_FIXTURE/pebble-workspace")" "700"

echo "=== WI#390 (HH-2): refused pipe-form write aborts under suppressed errexit ==="

# Production runs add_tenant/upgrade_tenant through the lifecycle lock as
# `"$@" || result=$?`, which disables errexit for the whole body: a refused
# _tenant_env_append must rely on its `|| die` to abort. With lunarwing.env
# symlinked away, patch_tenant_coordination_env's first append is refused; the
# die must fire BEFORE the matching "added ..." say and make the body non-zero
# (dropping the || die lets every append fail open and the body complete).
# Pre-populate every coordination key except COORD_CHANNELS, so the ONLY write
# patch_tenant_coordination_env attempts is the COORD_CHANNELS append — the pin
# is then sensitive to a dropped || die on that site (the function otherwise
# dies on its first append and never reaches a later mutated one).
cat >"$sensitive" <<'SENS'
DO-NOT-TRUNCATE
COORD_ENABLED=false
COORD_AGENT_ID=fixture-tenant
COORD_AGENT_ALIASES=
COORD_RESPONDER_ROSTER=
COORD_KNOWLEDGE_SHARE=false
COORD_KNOWLEDGE_PUBLISH=false
COORD_KNOWLEDGE_PUBLISHERS=
COORD_KNOWLEDGE_MAX_PER_HOUR=30
COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR=300
COORD_KNOWLEDGE_MIN_CHARS=12
COORD_ADMIN_USERS=
COORD_ADMIN_TOKEN=fixture-token
COORD_ADMIN_PORT=20009
SENS
rm -f "$MT_FIXTURE/env/lunarwing.env"
ln -s "$sensitive" "$MT_FIXTURE/env/lunarwing.env"
: >"$MT_FIXTURE/say.log"
real_say="$(declare -f say)"
say() { printf '%s\n' "$*" >>"$MT_FIXTURE/say.log"; }
hh2_result=0
( patch_tenant_coordination_env fixture-tenant "$MT_FIXTURE/env/lunarwing.env" ) >/dev/null 2>&1 \
  || hh2_result=$?
eval "$real_say"
if [[ "$hh2_result" -eq 0 ]]; then
  echo "  FAIL: refused pipe-form write returned 0 under suppressed errexit"; failures=$((failures + 1))
elif grep -q 'COORD_CHANNELS' "$MT_FIXTURE/say.log"; then
  echo "  FAIL: body printed 'added COORD_CHANNELS' past a refused pipe-form write under suppressed errexit"
  failures=$((failures + 1))
else
  echo "  PASS: refused pipe-form write aborts the body under suppressed errexit"
fi
assert_eq "suppressed-errexit refusal preserves symlink target" "$(head -1 "$sensitive")" "DO-NOT-TRUNCATE"
rm -f "$MT_FIXTURE/env/lunarwing.env"

echo "=== WI#390 (IG-1): env value reads come from a tenant-side snapshot ==="

# The render/patch/backfill functions must take their value reads from ONE
# snapshot read as the tenant, so a lunarwing.env symlinked at a foreign env
# for the read window dies at the snapshot instead of carrying the foreign
# values into the tenant's own files. The tenant_cat stub below emulates the
# cross-tenant permission boundary (a foreign 0600 env in a 0700 dir is
# unreadable to the tenant-side read) by refusing any read through a symlink.
# On the pre-fix shape each case either returns 0 (coordination: every key
# present in the victim, nothing to append) or dies later with a different
# message ("could not safely ..."), so the "could not read ... as tenant"
# assertion fails when the snapshot is reverted to root reads.
ig1_victim="$MT_FIXTURE/victim.env"
cat >"$ig1_victim" <<'EOF'
COORD_ENABLED=false
COORD_AGENT_ID=victim-tenant
COORD_CHANNELS=
COORD_AGENT_ALIASES=
COORD_RESPONDER_ROSTER=
COORD_KNOWLEDGE_SHARE=false
COORD_KNOWLEDGE_PUBLISH=false
COORD_KNOWLEDGE_PUBLISHERS=
COORD_KNOWLEDGE_MAX_PER_HOUR=30
COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR=300
COORD_KNOWLEDGE_MIN_CHARS=12
COORD_ADMIN_USERS=
COORD_ADMIN_TOKEN=B-SECRET-COORD-ADMIN-TOKEN
COORD_ADMIN_PORT=20555
GATEWAY_AUTH_TOKEN=B-SECRET-GATEWAY-TOKEN
LLM_BASE_URL=http://b-llm:9000/v1
EOF
ig1_victim_before="$(cat "$ig1_victim")"
# shellcheck disable=SC2317  # invoked indirectly via the tenant_cat stub
ig1_deny_tenant_cat() {  # deny reads through a symlink (foreign-target stand-in)
  [[ ! -L "$2" ]] || return 1
  cat -- "$2"
}
tenant_cat() { ig1_deny_tenant_cat "$@"; }

# 1) coordination patch: dies at the snapshot read, before any append/sed.
ln -s "$ig1_victim" "$MT_FIXTURE/env/lunarwing.env"
: >"$MT_FIXTURE/say.log"
real_say="$(declare -f say)"
say() { printf '%s\n' "$*" >>"$MT_FIXTURE/say.log"; }
ig1_rc=0
ig1_err="$( ( patch_tenant_coordination_env fixture-tenant "$MT_FIXTURE/env/lunarwing.env" ) 2>&1 )" || ig1_rc=$?
eval "$real_say"
if [[ "$ig1_rc" -eq 0 ]]; then
  echo "  FAIL: coordination patch read a foreign-linked env without dying"; failures=$((failures + 1))
elif [[ "$ig1_err" != *"could not read $MT_FIXTURE/env/lunarwing.env as tenant 'fixture-tenant'"* ]]; then
  echo "  FAIL: coordination patch died without the tenant-read refusal: $ig1_err"; failures=$((failures + 1))
else
  echo "  PASS: coordination patch dies at the tenant-side snapshot read"
fi
assert_eq "coordination refusal wrote nothing" "$(wc -l <"$MT_FIXTURE/say.log")" "0"
assert_eq "coordination refusal leaves the foreign env untouched" "$(cat "$ig1_victim")" "$ig1_victim_before"
rm -f "$MT_FIXTURE/env/lunarwing.env"

# 2) full render (add-tenant re-run shape): same refusal before any write.
ln -s "$ig1_victim" "$MT_FIXTURE/env/lunarwing.env"
ig1_rc=0
ig1_err="$( ( write_tenant_lunarwing_env fixture-tenant ) 2>&1 )" || ig1_rc=$?
if [[ "$ig1_rc" -eq 0 ]]; then
  echo "  FAIL: env render read a foreign-linked env without dying"; failures=$((failures + 1))
elif [[ "$ig1_err" != *"could not read $MT_FIXTURE/env/lunarwing.env as tenant 'fixture-tenant'"* ]]; then
  echo "  FAIL: env render died without the tenant-read refusal: $ig1_err"; failures=$((failures + 1))
else
  echo "  PASS: env render dies at the tenant-side snapshot read"
fi
assert_eq "render refusal leaves the foreign env untouched" "$(cat "$ig1_victim")" "$ig1_victim_before"
assert_eq "render refusal leaves the symlink in place" \
  "$(if [[ -L "$MT_FIXTURE/env/lunarwing.env" ]]; then echo symlink; else echo replaced; fi)" "symlink"
rm -f "$MT_FIXTURE/env/lunarwing.env"

# 3) WI#330 backfill: same refusal before any append.
ln -s "$ig1_victim" "$MT_FIXTURE/env/lunarwing.env"
ig1_rc=0
ig1_err="$( ( backfill_tenant_worker_base_urls fixture-tenant ) 2>&1 )" || ig1_rc=$?
if [[ "$ig1_rc" -eq 0 ]]; then
  echo "  FAIL: worker-URL backfill read a foreign-linked env without dying"; failures=$((failures + 1))
elif [[ "$ig1_err" != *"could not read $MT_FIXTURE/env/lunarwing.env as tenant 'fixture-tenant'"* ]]; then
  echo "  FAIL: backfill died without the tenant-read refusal: $ig1_err"; failures=$((failures + 1))
else
  echo "  PASS: backfill dies at the tenant-side snapshot read"
fi
assert_eq "backfill refusal leaves the foreign env untouched" "$(cat "$ig1_victim")" "$ig1_victim_before"
rm -f "$MT_FIXTURE/env/lunarwing.env" "$ig1_victim"

# Restore the harness's plain pass-through cat for the remaining sections.
tenant_cat() { cat -- "$2"; }

echo "=== WI#390 (IG-3): HH-3 upgrade helpers refuse a symlinked existing config ==="

# The HH-3 conversion made the fresh-config branch's refusal explicit; the
# existing-config branch had no pin. A symlinked config.toml must fail with
# the contract's failure code (2) and leave the redirect target untouched —
# the tenant-side atomic writer refuses it at commit time. The redirect
# target carries a legacy pebble worker block so each helper's awk pass
# reaches the commit (a block-free target exits the no-change path first).
printf 'WORKER_AUTH_TOKEN=fixture-worker-token\n' >"$MT_FIXTURE/env/lunarwing.env"
chmod 600 "$MT_FIXTURE/env/lunarwing.env"
mk_ig3_target() {  # <path>
  cat >"$1" <<'EOF'
[[sandbox.external_workers]]
name = "pebble"
url = "ws://127.0.0.1:10008/ws/agent"
timeout_ms = 300000
auth_token = "stale-token"
EOF
}

mk_ig3_target "$sensitive"
ln -s "$sensitive" "$MT_FIXTURE/state/config.toml"
ig3_rc=0
( upgrade_external_worker_timeout fixture-tenant pebble "$MT_FIXTURE/state/config.toml" ) >/dev/null 2>&1 || ig3_rc=$?
assert_eq "timeout upgrade on a symlinked config returns 2" "$ig3_rc" "2"
assert_eq "timeout upgrade refusal preserves the redirect target" \
  "$(grep -c 'timeout_ms = 300000' "$sensitive")" "1"

mk_ig3_target "$sensitive"
ig3_rc=0
( upgrade_external_worker_auth_token fixture-tenant pebble "$MT_FIXTURE/state/config.toml" "$MT_FIXTURE/env/lunarwing.env" ) >/dev/null 2>&1 || ig3_rc=$?
assert_eq "auth-token upgrade on a symlinked config returns 2" "$ig3_rc" "2"
assert_eq "auth-token upgrade refusal preserves the redirect target" \
  "$(grep -c 'auth_token = "stale-token"' "$sensitive")" "1"
assert_eq "upgrade refusals leave the symlink in place" \
  "$(if [[ -L "$MT_FIXTURE/state/config.toml" ]]; then echo symlink; else echo replaced; fi)" "symlink"
rm -f "$MT_FIXTURE/state/config.toml"

# Clean controls: the same content as a REGULAR config.toml upgrades in place,
# so the refusals above came from the symlink, not the fixture content.
mk_ig3_target "$MT_FIXTURE/state/config.toml"
ig3_rc=0
( upgrade_external_worker_timeout fixture-tenant pebble "$MT_FIXTURE/state/config.toml" ) >/dev/null 2>&1 || ig3_rc=$?
assert_eq "timeout upgrade on a clean config succeeds" "$ig3_rc" "0"
assert_eq "clean timeout upgrade applied" \
  "$(grep -c 'timeout_ms = 3600000' "$MT_FIXTURE/state/config.toml")" "1"
ig3_rc=0
( upgrade_external_worker_auth_token fixture-tenant pebble "$MT_FIXTURE/state/config.toml" "$MT_FIXTURE/env/lunarwing.env" ) >/dev/null 2>&1 || ig3_rc=$?
assert_eq "auth-token upgrade on a clean config succeeds" "$ig3_rc" "0"
assert_eq "clean auth-token upgrade applied" \
  "$(grep -c 'auth_token = "fixture-worker-token"' "$MT_FIXTURE/state/config.toml")" "1"
rm -f "$MT_FIXTURE/state/config.toml"

echo "=== WI#390 (IG-4): darkirc adapter env write fails closed ==="

# The writer used to pipe straight into the tenant-side atomic writer with no
# || die, and both callers ignored the return. A symlinked target must now
# abort the function; a clean target still renders through the template.
# read_tenant_env_value is stubbed to a plain read: its own tenant-side safety
# checks are unchanged by this pin and the dev-VM shell's startup banner
# pollutes captured stdout otherwise (CI's bash is clean).
ig4_lock_saved="$DARKIRC_WRITER_LOCK_HELD"
DARKIRC_WRITER_LOCK_HELD=true   # skip the root-owned flock in the fixture
ig4_real_reader="$(declare -f read_tenant_env_value)"
read_tenant_env_value() { _read_env_value "$2" "$3"; }
printf 'DO-NOT-TRUNCATE\n' >"$sensitive"
printf 'DARKIRC_ADAPTER_SECRET=fixture-secret\n' >"$MT_FIXTURE/env/lunarwing.env"
chmod 600 "$MT_FIXTURE/env/lunarwing.env"
ln -s "$sensitive" "$MT_FIXTURE/env/darkirc-adapter.env"
ig4_rc=0
( write_tenant_darkirc_adapter_env fixture-tenant ) >/dev/null 2>&1 || ig4_rc=$?
if [[ "$ig4_rc" -eq 0 ]]; then
  echo "  FAIL: darkirc adapter env write followed a symlinked target"; failures=$((failures + 1))
else
  echo "  PASS: darkirc adapter env write refuses a symlinked target"
fi
assert_eq "darkirc refusal preserves the redirect target" "$(cat "$sensitive")" "DO-NOT-TRUNCATE"
rm -f "$MT_FIXTURE/env/darkirc-adapter.env"
ig4_rc=0
( write_tenant_darkirc_adapter_env fixture-tenant ) >/dev/null 2>&1 || ig4_rc=$?
assert_eq "darkirc adapter env writes for a clean target" "$ig4_rc" "0"
assert_eq "darkirc clean write carries the preserved secret" \
  "$(grep -c '^ADAPTER_SECRET=fixture-secret$' "$MT_FIXTURE/env/darkirc-adapter.env")" "1"
eval "$ig4_real_reader"
DARKIRC_WRITER_LOCK_HELD="$ig4_lock_saved"
rm -f "$MT_FIXTURE/env/darkirc-adapter.env" "$MT_FIXTURE/env/lunarwing.env"

echo "=== WI#390 (IY-4): configure-* value reads come from a tenant-side snapshot ==="

# The run-130 pass converted the configure-* WRITES but left the READS on the
# root path (_read_env_value / grep -v against the live lunarwing.env). With
# lunarwing.env symlinked away (the IG-1 deny stub), each of the four
# functions must die at the tenant-side snapshot read before writing
# anything. On the pre-fix shape the reads succeed as root and the refusal
# comes later from the atomic writer with a different message, so the
# "could not read ... as tenant" assertion fails when a function's snapshot
# conversion is reverted.
iy4_victim="$MT_FIXTURE/iy4-victim.env"
printf 'GATEWAY_AUTH_TOKEN=B-SECRET-GATEWAY-TOKEN\n' >"$iy4_victim"
iy4_victim_before="$(cat "$iy4_victim")"
iy4_real_registry="$(declare -f tenant_exists_in_registry)"
iy4_real_ctr="$(declare -f _ctr)"
iy4_real_vsh="$(declare -f validate_study_notes_home)"
iy4_real_isns="$(declare -f initialize_study_notes_state)"
iy4_real_vsnta="$(declare -f validate_study_notes_tenant_access)"
tenant_exists_in_registry() { :; }
_ctr() { return 1; }
validate_study_notes_home() { :; }
initialize_study_notes_state() { :; }
validate_study_notes_tenant_access() { :; }
selinuxenabled() { return 1; }  # skip the chcon block in the fixture
tenant_cat() { ig1_deny_tenant_cat "$@"; }

for iy4_fn in configure_opencode configure_opencode2 configure_kimi; do
  ln -s "$iy4_victim" "$MT_FIXTURE/env/lunarwing.env"
  iy4_rc=0
  iy4_err="$( ( "$iy4_fn" fixture-tenant "iy4-model" "" ) 2>&1 )" || iy4_rc=$?
  if [[ "$iy4_rc" -eq 0 ]]; then
    echo "  FAIL: $iy4_fn read a foreign-linked env without dying"; failures=$((failures + 1))
  elif [[ "$iy4_err" != *"could not read $MT_FIXTURE/env/lunarwing.env as tenant 'fixture-tenant'"* ]]; then
    echo "  FAIL: $iy4_fn died without the tenant-read refusal: $iy4_err"; failures=$((failures + 1))
  else
    echo "  PASS: $iy4_fn dies at the tenant-side snapshot read"
  fi
  assert_eq "$iy4_fn refusal leaves the foreign env untouched" \
    "$(cat "$iy4_victim")" "$iy4_victim_before"
  rm -f "$MT_FIXTURE/env/lunarwing.env"
done

ln -s "$iy4_victim" "$MT_FIXTURE/env/lunarwing.env"
iy4_rc=0
iy4_err="$( ( configure_study_mcp fixture-tenant /srv/lunarwing/lw2studymcp false ) 2>&1 )" || iy4_rc=$?
if [[ "$iy4_rc" -eq 0 ]]; then
  echo "  FAIL: configure_study_mcp read a foreign-linked env without dying"; failures=$((failures + 1))
elif [[ "$iy4_err" != *"could not read $MT_FIXTURE/env/lunarwing.env as tenant 'fixture-tenant'"* ]]; then
  echo "  FAIL: configure_study_mcp died without the tenant-read refusal: $iy4_err"; failures=$((failures + 1))
else
  echo "  PASS: configure_study_mcp dies at the tenant-side snapshot read"
fi
assert_eq "configure_study_mcp refusal leaves the foreign env untouched" \
  "$(cat "$iy4_victim")" "$iy4_victim_before"
rm -f "$MT_FIXTURE/env/lunarwing.env" "$iy4_victim"

# Clean controls: a plain lunarwing.env still renders — untouched keys are
# kept from the snapshot and the new values are written through.
tenant_cat() { cat -- "$2"; }
printf 'GATEWAY_AUTH_TOKEN=keep-me\nOPENCODE_MODEL=old-model\n' >"$MT_FIXTURE/env/lunarwing.env"
chmod 600 "$MT_FIXTURE/env/lunarwing.env"
configure_opencode fixture-tenant "new-model" ""
assert_eq "configure_opencode writes the new model" "$(envval OPENCODE_MODEL)" "new-model"
assert_eq "configure_opencode keeps untouched keys" "$(envval GATEWAY_AUTH_TOKEN)" "keep-me"
configure_study_mcp fixture-tenant /srv/lunarwing/lw2studymcp false
assert_eq "configure_study_mcp keeps untouched keys" "$(envval GATEWAY_AUTH_TOKEN)" "keep-me"
assert_eq "configure_study_mcp writes the study home" \
  "$(envval LW2STUDYMCP_HOME)" "/srv/lunarwing/lw2studymcp"
rm -f "$MT_FIXTURE/env/lunarwing.env"

# Restore the stubbed helpers for the remaining sections.
eval "$iy4_real_registry"
eval "$iy4_real_ctr"
eval "$iy4_real_vsh"
eval "$iy4_real_isns"
eval "$iy4_real_vsnta"
unset -f selinuxenabled

echo "=== write_tenant_bridge_env fixture tests ==="

# The bridge writer reads XMPP_BRIDGE_TOKEN / XMPP_PASSWORD from the daemon env.
# Ensure a daemon env exists with those keys for the fixture tenant.
cat > "$MT_FIXTURE/env/lunarwing.env" <<E
XMPP_BRIDGE_TOKEN=bridge-tok-fixture
XMPP_PASSWORD=xmpp-pass-fixture
E
chmod 600 "$MT_FIXTURE/env/lunarwing.env"

run_bridge_writer() {  # <xmpp_allow_from_extras>
  rm -f "$MT_FIXTURE/env/xmpp-bridge.env"
  write_tenant_bridge_env fixture-tenant \
    "fixture-tenant@xmpp.localhost" "" "$1"
  cat "$MT_FIXTURE/env/xmpp-bridge.env"
}

bridgeval() {  # <key>
  grep -m1 "^$1=" "$MT_FIXTURE/env/xmpp-bridge.env" | cut -d= -f2-
}

# Case 1: no extras and no daemon allowlist → local user
out="$(run_bridge_writer "")"
assert_eq "bridge json defaults to local user" "$(bridgeval XMPP_ALLOW_FROM_JSON)" '["user@xmpp.localhost"]'

# A daemon allowlist with no owner must not gain one in the bridge env.
printf 'XMPP_ALLOW_FROM=user@xmpp.localhost\n' >>"$MT_FIXTURE/env/lunarwing.env"
out="$(run_bridge_writer "")"
assert_eq "bridge json matches daemon allowlist" "$(bridgeval XMPP_ALLOW_FROM_JSON)" '["user@xmpp.localhost"]'
sed -i 's/^XMPP_ALLOW_FROM=.*/XMPP_ALLOW_FROM=admin@xmpp.org,bob@xmpp.org/' \
  "$MT_FIXTURE/env/lunarwing.env"
out="$(run_bridge_writer "")"
assert_eq "bridge json preserves custom allowlist" "$(bridgeval XMPP_ALLOW_FROM_JSON)" \
  '["admin@xmpp.org","bob@xmpp.org"]'

# Pre-existing tenants with the old owner-only default keep that value.
sed -i 's/^XMPP_ALLOW_FROM=.*/XMPP_ALLOW_FROM=fixture-tenant@xmpp.localhost/' \
  "$MT_FIXTURE/env/lunarwing.env"
write_tenant_lunarwing_env fixture-tenant
out="$(run_bridge_writer "")"
assert_eq "rerun preserves old owner-only allowlist" \
  "$(envval XMPP_ALLOW_FROM)" "fixture-tenant@xmpp.localhost"
assert_eq "bridge retains old owner-only allowlist" \
  "$(bridgeval XMPP_ALLOW_FROM_JSON)" '["fixture-tenant@xmpp.localhost"]'

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
  "sel":{"base_port":10030,"workers":{"nanocode":false,"pebble":true,"opencode":false}}
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
assert_eq "retired nanocode remains OFF" "$(worker_state sel nanocode)" "off"
assert_eq "selected pebble ON"           "$(worker_state sel pebble)"   "on"

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
assert_eq "failed backfill hardens env mode" "$(stat -c '%a' "$BAD_ENV")" "600"
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

echo "=== backfill_tenant_worker_base_urls (WI#330 upgrade path) ==="

# upgrade-tenant/patch-env backfill: an enabled worker with no base URL must
# inherit the tenant's endpoint BEFORE the placeholder-refusing images start.
WORKERS_REG_WI330="$MT_FIXTURE/ports-wi330.json"
PORTS_REGISTRY="$WORKERS_REG_WI330"
cat >"$WORKERS_REG_WI330" <<JSON
{"tenants":{"fixture-tenant":{"base_port":10040,"workers":{"opencode":true,"kimi":true,"opencode2":false}}}}
JSON

# A: loopback LLM_BASE_URL (proxy tenant) -> inherit the proxy.env upstream,
# marked inherited; the disabled worker stays untouched.
cat >"$MT_FIXTURE/env/lunarwing.env" <<'EOF'
LLM_BASE_URL=http://127.0.0.1:10004/v1
EOF
chmod 600 "$MT_FIXTURE/env/lunarwing.env"
cat >"$MT_FIXTURE/env/proxy.env" <<'EOF'
TENSORZERO_URL=http://tz-upstream:3030
EOF
backfill_tenant_worker_base_urls fixture-tenant
assert_eq "backfill: opencode inherits proxy upstream" \
  "$(envval OPENCODE_BASE_URL)" "http://tz-upstream:3030/openai/v1"
assert_eq "backfill: opencode marked inherited" \
  "$(envval OPENCODE_BASE_URL_SOURCE)" "inherited"
assert_eq "backfill: kimi inherits proxy upstream" \
  "$(envval KIMI_BASE_URL)" "http://tz-upstream:3030/openai/v1"
assert_eq "backfill: disabled opencode2 untouched" \
  "$(grep -c '^OPENCODE2_BASE_URL=' "$MT_FIXTURE/env/lunarwing.env" || true)" "0"

# B: a non-loopback LLM_BASE_URL (direct-endpoint tenant) is the source, and an
# operator-set value (no marker) is never overwritten.
cat >"$MT_FIXTURE/env/lunarwing.env" <<'EOF'
LLM_BASE_URL=http://direct:9000/v1
OPENCODE_BASE_URL=http://operator-set:4100/v1
EOF
chmod 600 "$MT_FIXTURE/env/lunarwing.env"
backfill_tenant_worker_base_urls fixture-tenant
assert_eq "backfill: operator-set opencode preserved" \
  "$(envval OPENCODE_BASE_URL)" "http://operator-set:4100/v1"
assert_eq "backfill: kimi inherits direct LLM_BASE_URL" \
  "$(envval KIMI_BASE_URL)" "http://direct:9000/v1"

# C: nothing derivable + an enabled worker -> die loudly (before image rebuilds).
cat >"$MT_FIXTURE/env/lunarwing.env" <<'EOF'
LLM_BASE_URL=http://127.0.0.1:10004/v1
EOF
chmod 600 "$MT_FIXTURE/env/lunarwing.env"
rm -f "$MT_FIXTURE/env/proxy.env"
if (backfill_tenant_worker_base_urls fixture-tenant) >/dev/null 2>&1; then
  echo "  FAIL: backfill succeeded with no derivable endpoint"; failures=$((failures + 1))
else
  echo "  PASS: backfill dies when no endpoint is derivable"
fi
assert_eq "backfill: failed run wrote nothing" \
  "$(grep -c '^OPENCODE_BASE_URL=' "$MT_FIXTURE/env/lunarwing.env" || true)" "0"

# D: no enabled workers + nothing derivable -> no-op, no failure.
cat >"$WORKERS_REG_WI330" <<JSON
{"tenants":{"fixture-tenant":{"base_port":10040,"workers":{"opencode":false,"kimi":false,"opencode2":false}}}}
JSON
backfill_tenant_worker_base_urls fixture-tenant
assert_eq "backfill: all-disabled tenant gets no worker keys" \
  "$(grep -cE '^(OPENCODE|KIMI|OPENCODE2)_BASE_URL=' "$MT_FIXTURE/env/lunarwing.env" || true)" "0"

echo "=== CLI dispatch parsing smoke check ==="

# WI#330 CX-1: with no tenant/fleet endpoint at all, add-tenant must fail
# loudly instead of rendering an unreachable default. (The fleet default is
# unset in the subshell to simulate a host without LUNARWING_MT_TENSORZERO_URL.)
# shellcheck disable=SC2034  # read by main() inside the same subshell
no_url_err="$(
  ( require_root() { :; }; DEFAULT_TENSORZERO_URL=""; DEFAULT_LLM_BASE_URL=""; main add-tenant fixture-no-url ) 2>&1 || true
)"
if echo "$no_url_err" | grep -q 'requires an LLM endpoint'; then
  echo "  PASS: add-tenant without any endpoint fails loudly"
else
  echo "  FAIL: add-tenant without any endpoint did not fail loudly"
  echo "        $no_url_err"
  failures=$((failures + 1))
fi
# shellcheck disable=SC2034  # read by main() inside the same subshell
no_url_proxy_err="$(
  ( require_root() { :; }; DEFAULT_TENSORZERO_URL=""; main add-tenant fixture-no-url --enable-proxy --llm-base-url http://custom:9000/v1 ) 2>&1 || true
)"
if echo "$no_url_proxy_err" | grep -q 'proxy needs an upstream'; then
  echo "  PASS: proxy tenant without --tensorzero-url fails loudly"
else
  echo "  FAIL: proxy tenant without --tensorzero-url did not fail loudly"
  echo "        $no_url_proxy_err"
  failures=$((failures + 1))
fi
# Malformed URLs are rejected at the same gate.
bad_url_err="$(
  ( require_root() { :; }; main add-tenant fixture-bad-url --tensorzero-url 'ftp://tz:3030' ) 2>&1 || true
)"
if echo "$bad_url_err" | grep -q 'must be an http(s) URL'; then
  echo "  PASS: non-http(s) --tensorzero-url rejected"
else
  echo "  FAIL: non-http(s) --tensorzero-url accepted"
  echo "        $bad_url_err"
  failures=$((failures + 1))
fi

# Verify the dispatch block recognizes the new flags by invoking the script
# directly with a deliberately-bad tenant name AFTER the flags; if the flags
# parsed, the script reaches require_root (not "unknown flag"). As non-root,
# require_root exits with a permission message; we assert it does NOT mention
# "unknown flag".
err="$(bash "$ADMIN_SCRIPT" add-tenant \
  --llm-model glm-5-air \
  --gateway-host 0.0.0.0 \
  --xmpp-allow-from admin@xmpp.org \
  --with-pebble --with-opencode \
  2>&1 || true)"
if echo "$err" | grep -q 'unknown flag'; then
  echo "  FAIL: a new flag was rejected as unknown"
  echo "        $err"
  failures=$((failures + 1))
else
  echo "  PASS: all supported worker flags parsed"
fi

retired_err="$(
  ( require_root() { :; }; main add-tenant --with-nanocode fixture ) 2>&1 || true
)"
if echo "$retired_err" | grep -q 'Nanocode worker is retired'; then
  echo "  PASS: retired Nanocode flag rejected explicitly"
else
  echo "  FAIL: retired Nanocode flag was not rejected explicitly"
  echo "        $retired_err"
  failures=$((failures + 1))
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

echo "=== WI#423: pg password, worker token, study-notes reads stay tenant-side ==="

# The IG-1 pattern: a tenant-side read must never resolve a symlinked env
# path. The tenant_cat stub emulates the cross-tenant permission boundary (a
# foreign 0600 env is unreadable to the tenant-side cat) by refusing reads
# through a symlink; reverting any fixed function to a root read of the live
# path makes its pin below fail.
eval "$REAL_TENANT_PG_PASSWORD_DEF"
wi423_victim="$MT_FIXTURE/wi423-victim.env"
wi423_deny_tenant_cat() {  # deny reads through a symlink (foreign-target stand-in)
  [[ ! -L "$2" ]] || return 1
  cat -- "$2"
}

# 1) tenant_pg_password: an existing pg.secret read must come from the
#    tenant-side snapshot — a foreign symlink dies there, the victim keeps its
#    password, and nothing is written.
printf 'FOREIGN-PG-PASSWORD\n' >"$wi423_victim"
chmod 600 "$wi423_victim"
wi423_victim_before="$(cat "$wi423_victim")"
tenant_cat() { wi423_deny_tenant_cat "$@"; }
rm -f "$MT_FIXTURE/env/pg.secret" "$MT_FIXTURE/env/lunarwing.env"
ln -s "$wi423_victim" "$MT_FIXTURE/env/pg.secret"
wi423_rc=0
wi423_err="$( ( tenant_pg_password fixture-tenant ) 2>&1 )" || wi423_rc=$?
if [[ "$wi423_rc" -eq 0 ]]; then
  echo "  FAIL: tenant_pg_password read a foreign-linked pg.secret without dying"; failures=$((failures + 1))
elif [[ "$wi423_err" != *"could not read $MT_FIXTURE/env/pg.secret as tenant 'fixture-tenant'"* ]]; then
  echo "  FAIL: tenant_pg_password died without the tenant-read refusal: $wi423_err"; failures=$((failures + 1))
else
  echo "  PASS: tenant_pg_password dies at the tenant-side pg.secret snapshot"
fi
assert_eq "pg.secret refusal leaves the foreign file untouched" "$(cat "$wi423_victim")" "$wi423_victim_before"
assert_eq "pg.secret refusal leaves the symlink in place" \
  "$(if [[ -L "$MT_FIXTURE/env/pg.secret" ]]; then echo symlink; else echo replaced; fi)" "symlink"

# 2) tenant_pg_password: a dangling symlink at pg.secret (fresh-tenant mint
#    shape) dies before any write — root never creates the redirect target.
rm -f "$MT_FIXTURE/env/pg.secret"
ln -s "$MT_FIXTURE/wi423-no-such-target" "$MT_FIXTURE/env/pg.secret"
wi423_rc=0
( tenant_pg_password fixture-tenant ) >/dev/null 2>&1 || wi423_rc=$?
if [[ "$wi423_rc" -eq 0 ]]; then
  echo "  FAIL: tenant_pg_password minted through a dangling pg.secret symlink"; failures=$((failures + 1))
else
  echo "  PASS: tenant_pg_password mint refuses a symlinked pg.secret"
fi
assert_eq "symlinked pg.secret redirect target was never created" \
  "$(if [[ -e "$MT_FIXTURE/wi423-no-such-target" ]]; then echo created; else echo absent; fi)" "absent"

# 2b) the mint write itself commits through the tenant-side atomic writer.
#     The snapshot reads are neutralized (a read that found nothing), so the
#     pre-placed symlink at pg.secret is only seen at COMMIT time — the exact
#     WI#390 original shape this WI closes. Root truncating writes make this
#     pin fail with a zero exit and a truncated victim.
wi423_real_snapshot="$(declare -f _snapshot_tenant_env)"
_snapshot_tenant_env() {  # <tenant> <env_file> <out_var> — empty snapshot stub
  local -n wi423_snap_out="$3"
  # shellcheck disable=SC2034  # nameref write is the whole point of the stub
  wi423_snap_out="$(mktemp)"
}
printf 'DO-NOT-TRUNCATE\n' >"$wi423_victim"
rm -f "$MT_FIXTURE/env/pg.secret" "$MT_FIXTURE/env/lunarwing.env"
ln -s "$wi423_victim" "$MT_FIXTURE/env/pg.secret"
wi423_rc=0
wi423_err="$( ( tenant_pg_password fixture-tenant ) 2>&1 )" || wi423_rc=$?
if [[ "$wi423_rc" -eq 0 ]]; then
  echo "  FAIL: tenant_pg_password minted through a symlinked pg.secret at commit time"; failures=$((failures + 1))
elif [[ "$wi423_err" != *"could not safely write $MT_FIXTURE/env/pg.secret"* ]]; then
  echo "  FAIL: mint died without the atomic-writer refusal: $wi423_err"; failures=$((failures + 1))
else
  echo "  PASS: mint commit refuses a symlinked pg.secret"
fi
assert_eq "mint refusal preserves the redirect target" "$(cat "$wi423_victim")" "DO-NOT-TRUNCATE"
wi423_victim_before="$(cat "$wi423_victim")"
eval "$wi423_real_snapshot"

# 3) tenant_pg_password: the DATABASE_URL migration value comes from a
#    tenant-side snapshot of lunarwing.env, and the mint commits through the
#    tenant-side atomic writer (0600, tenant-owned shape).
rm -f "$MT_FIXTURE/env/pg.secret" "$MT_FIXTURE/env/lunarwing.env"
printf 'DATABASE_URL=postgres://lunarwing:migration-pw@127.0.0.1:10003/lunarwing\n' \
  >"$MT_FIXTURE/env/lunarwing.env"
chmod 600 "$MT_FIXTURE/env/lunarwing.env"
assert_eq "migration password preserved from lunarwing.env" \
  "$(tenant_pg_password fixture-tenant)" "migration-pw"
assert_eq "minted pg.secret carries the preserved password" \
  "$(cat "$MT_FIXTURE/env/pg.secret")" "migration-pw"
assert_eq "minted pg.secret has the private mode" "$(stat -c %a "$MT_FIXTURE/env/pg.secret")" "600"
# and the foreign-linked lunarwing.env shape dies before any pg.secret write
rm -f "$MT_FIXTURE/env/pg.secret" "$MT_FIXTURE/env/lunarwing.env"
ln -s "$wi423_victim" "$MT_FIXTURE/env/lunarwing.env"
wi423_rc=0
wi423_err="$( ( tenant_pg_password fixture-tenant ) 2>&1 )" || wi423_rc=$?
if [[ "$wi423_rc" -eq 0 ]]; then
  echo "  FAIL: tenant_pg_password read a foreign-linked lunarwing.env without dying"; failures=$((failures + 1))
elif [[ "$wi423_err" != *"could not read $MT_FIXTURE/env/lunarwing.env as tenant 'fixture-tenant'"* ]]; then
  echo "  FAIL: tenant_pg_password died without the lunarwing.env tenant-read refusal: $wi423_err"; failures=$((failures + 1))
else
  echo "  PASS: tenant_pg_password dies at the tenant-side lunarwing.env snapshot"
fi
assert_eq "lunarwing.env refusal wrote no pg.secret" \
  "$(if [[ -e "$MT_FIXTURE/env/pg.secret" ]]; then echo written; else echo absent; fi)" "absent"
rm -f "$MT_FIXTURE/env/lunarwing.env"

# 4) resolve_worker_auth_token reads WORKER_AUTH_TOKEN / GATEWAY_AUTH_TOKEN
#    through the tenant-side per-key reader and takes the tenant name.
tenant_cat() { cat -- "$2"; }   # plain reads for the clean-path controls
rm -f "$MT_FIXTURE/env/lunarwing.env"
printf 'WORKER_AUTH_TOKEN=scoped-fixture-token\n' >"$MT_FIXTURE/env/lunarwing.env"
chmod 600 "$MT_FIXTURE/env/lunarwing.env"
assert_eq "resolver returns the scoped worker token" \
  "$(resolve_worker_auth_token fixture-tenant "$MT_FIXTURE/env/lunarwing.env")" "scoped-fixture-token"
# mint shape: missing key on a writable env is generated and persisted
printf 'GATEWAY_AUTH_TOKEN=gateway-fixture-token\n' >"$MT_FIXTURE/env/lunarwing.env"
chmod 600 "$MT_FIXTURE/env/lunarwing.env"
wi423_minted="$(resolve_worker_auth_token fixture-tenant "$MT_FIXTURE/env/lunarwing.env" 2>/dev/null)"
if [[ -n "$wi423_minted" && "$wi423_minted" == "$(grep '^WORKER_AUTH_TOKEN=' "$MT_FIXTURE/env/lunarwing.env" | cut -d= -f2-)" ]]; then
  echo "  PASS: resolver mints and persists the scoped token"
else
  echo "  FAIL: resolver did not persist the minted scoped token"; failures=$((failures + 1))
fi
# refusal shape: a symlinked env path dies instead of leaking/falling back
rm -f "$MT_FIXTURE/env/lunarwing.env"
ln -s "$wi423_victim" "$MT_FIXTURE/env/lunarwing.env"
wi423_rc=0
wi423_err="$( ( resolve_worker_auth_token fixture-tenant "$MT_FIXTURE/env/lunarwing.env" ) 2>&1 )" || wi423_rc=$?
if [[ "$wi423_rc" -eq 0 ]]; then
  echo "  FAIL: resolver read a foreign-linked env without dying"; failures=$((failures + 1))
elif [[ "$wi423_err" != *"unsafe tenant env path: $MT_FIXTURE/env/lunarwing.env"* \
  && "$wi423_err" != *"could not safely read WORKER_AUTH_TOKEN"* ]]; then
  echo "  FAIL: resolver died without the tenant-side refusal: $wi423_err"; failures=$((failures + 1))
else
  echo "  PASS: resolver refuses a foreign-linked env path"
fi

# 5) study-notes mount sources and the preserved JID read tenant-side: the
#    same symlinked env path refuses for the trio and the JID resolver.
wi423_rc=0
wi423_err="$( ( tenant_study_notes_home fixture-tenant ) 2>&1 )" || wi423_rc=$?
if [[ "$wi423_rc" -eq 0 ]]; then
  echo "  FAIL: study-notes home read a foreign-linked env without refusing"; failures=$((failures + 1))
else
  echo "  PASS: study-notes home read refuses a foreign-linked env"
fi
wi423_rc=0
wi423_err="$( ( resolve_tenant_xmpp_jid fixture-tenant "" ) 2>&1 )" || wi423_rc=$?
if [[ "$wi423_rc" -eq 0 ]]; then
  echo "  FAIL: JID resolver read a foreign-linked env without dying"; failures=$((failures + 1))
else
  echo "  PASS: JID resolver refuses a foreign-linked env loudly"
fi
assert_eq "WI#423 refusals leave the foreign env untouched" "$(cat "$wi423_victim")" "$wi423_victim_before"
rm -f "$MT_FIXTURE/env/lunarwing.env" "$wi423_victim"

echo "=== run-166 LF-1: first add_tenant pass (no account yet) reads as absent ==="

# The reader runs `sudo -u <tenant>` before create_tenant_user has made the
# account; real sudo refuses an unknown user, which the head turned into a
# `|| die` abort of add-tenant for EVERY brand-new tenant. Model the real
# refusal (unknown account -> sudo fails before running anything) and require
# the default-JID path: target absent + account absent reads as absent.
wi423_new_tenant="wi423-brand-new-tenant"
rm -f "$MT_FIXTURE/env/lunarwing.env"
lf1_rc=0
lf1_out="$(
  (
    sudo() {
      if [[ "${1:-}" == "-u" ]] && ! id -u -- "$2" >/dev/null 2>&1; then
        echo "sudo: unknown user $2" >&2
        return 1
      fi
      return 1   # any other tenant-side drop is also a refusal here
    }
    read_tenant_env_value "$wi423_new_tenant" "$MT_FIXTURE/env/lunarwing.env" XMPP_JID
  ) 2>/dev/null
)" || lf1_rc=$?
if [[ "$lf1_rc" -ne 0 || -n "$lf1_out" ]]; then
  echo "  FAIL: pre-account read did not read as absent (rc=$lf1_rc out=[$lf1_out])"; failures=$((failures + 1))
else
  echo "  PASS: pre-account read reads as absent"
fi
assert_eq "pre-account JID resolution keeps the default" \
  "$(resolve_tenant_xmpp_jid "$wi423_new_tenant" "")" "wi423-brand-new-tenant@xmpp.localhost"

# Control: an EXISTING target without an account is not the pre-provision
# shape — it still refuses at the tenant-side drop instead of reading as
# absent (the fast path must not become a root read of a present target).
printf 'XMPP_JID=foreign@xmpp.example\n' >"$MT_FIXTURE/env/lunarwing.env"
chmod 600 "$MT_FIXTURE/env/lunarwing.env"
lf1_rc=0
lf1_out="$(
  (
    sudo() { return 1; }   # unknown account: sudo refuses everything
    read_tenant_env_value "$wi423_new_tenant" "$MT_FIXTURE/env/lunarwing.env" XMPP_JID
  ) 2>/dev/null
)" || lf1_rc=$?
if [[ "$lf1_rc" -eq 0 ]]; then
  echo "  FAIL: present target without an account read as absent"; failures=$((failures + 1))
else
  echo "  PASS: present target without an account still refuses"
fi
rm -f "$MT_FIXTURE/env/lunarwing.env"

echo "=== run-166 LF-3: multi-line/blank-line pg.secret refuses fatally ==="

# The head's `grep -c .` count let a leading blank line through (and a dropped
# check — mutant M4 — let anything through). A pg.secret that is not exactly
# one control-character-free line is a shape mt-admin never wrote: die loudly
# instead of rendering the mangled value into DATABASE_URL / the pg quadlet.
tenant_cat() { cat -- "$2"; }
rm -f "$MT_FIXTURE/env/lunarwing.env"
printf 'first-password\nsecond-password\n' >"$MT_FIXTURE/env/pg.secret"
chmod 600 "$MT_FIXTURE/env/pg.secret"
lf3_rc=0
lf3_err="$( ( tenant_pg_password fixture-tenant ) 2>&1 )" || lf3_rc=$?
if [[ "$lf3_rc" -eq 0 ]]; then
  echo "  FAIL: two-line pg.secret was accepted"; failures=$((failures + 1))
elif [[ "$lf3_err" != *"is not a single password line"* ]]; then
  echo "  FAIL: two-line refusal carried no message: $lf3_err"; failures=$((failures + 1))
else
  echo "  PASS: two-line pg.secret dies as not-a-single-password-line"
fi
printf '\nleading-blank-password\n' >"$MT_FIXTURE/env/pg.secret"
chmod 600 "$MT_FIXTURE/env/pg.secret"
lf3_rc=0
( tenant_pg_password fixture-tenant ) >/dev/null 2>&1 || lf3_rc=$?
if [[ "$lf3_rc" -eq 0 ]]; then
  echo "  FAIL: leading-blank-line pg.secret was accepted"; failures=$((failures + 1))
else
  echo "  PASS: leading-blank-line pg.secret refuses"
fi
# The refusal is a die: the render callers cannot continue past it.
lf3_rc=0
lf3_err="$( ( write_tenant_lunarwing_env fixture-tenant ) 2>&1 )" || lf3_rc=$?
if [[ "$lf3_rc" -eq 0 ]]; then
  echo "  FAIL: env render continued past a refused pg.secret"; failures=$((failures + 1))
else
  echo "  PASS: env render aborts on a refused pg.secret"
fi
rm -f "$MT_FIXTURE/env/pg.secret"

echo "=== run-166 LF-7: untraversable ancestor refuses; empty credential dies ==="

# -e cannot tell ENOENT from EACCES: an env dir whose parent chain contains a
# non-searchable directory must not read as "absent" (the head returned an
# empty token with rc 0 and the worker started with AGENT_AUTH_TOKEN absent).
# The tenant is the invoking user (a REAL account), so the LF-1 pre-provision
# fast path does not apply — this exercises the tenant-side ancestor walk.
lf7_tenant="$(id -un)"
lf7_root="$MT_FIXTURE/lf7-root"
mkdir -p "$lf7_root/no-search/sub"
chmod 600 "$lf7_root/no-search"   # exists, visible, NOT searchable
lf7_rc=0
lf7_out="$(
  read_tenant_env_value "$lf7_tenant" "$lf7_root/no-search/sub/lunarwing.env" WORKER_AUTH_TOKEN
)" || lf7_rc=$?
if [[ "$lf7_rc" -eq 0 ]]; then
  echo "  FAIL: untraversable ancestor read as absent (rc 0, out=[$lf7_out])"; failures=$((failures + 1))
else
  echo "  PASS: untraversable ancestor refuses instead of reading as absent"
fi
# Control: the same deep path under a SEARCHABLE, tenant-private tree is
# genuinely absent (the env-dir privacy checks demand 0700 — a 0755 parent is
# itself a refusal shape, not an absence).
mkdir -p "$lf7_root/searchable/sub"
chmod 700 "$lf7_root/searchable/sub"
lf7_rc=0
lf7_out="$(
  read_tenant_env_value "$lf7_tenant" "$lf7_root/searchable/sub/lunarwing.env" WORKER_AUTH_TOKEN
)" || lf7_rc=$?
if [[ "$lf7_rc" -ne 0 || -n "$lf7_out" ]]; then
  echo "  FAIL: absent-under-searchable did not read as absent (rc=$lf7_rc out=[$lf7_out])"; failures=$((failures + 1))
else
  echo "  PASS: absent-under-searchable reads as absent"
fi
chmod 700 "$lf7_root/no-search"
rm -rf "$lf7_root"

# The resolver's last resort must not return success with an empty token: a
# read-only env (no mint) carrying an explicitly empty GATEWAY_AUTH_TOKEN dies
# instead of handing the caller a credential-less "success".
rm -f "$MT_FIXTURE/env/lunarwing.env"
printf 'GATEWAY_AUTH_TOKEN=\n' >"$MT_FIXTURE/env/lunarwing.env"
chmod 400 "$MT_FIXTURE/env/lunarwing.env"
lf7_rc=0
lf7_err="$( ( resolve_worker_auth_token fixture-tenant "$MT_FIXTURE/env/lunarwing.env" ) 2>&1 )" || lf7_rc=$?
if [[ "$lf7_rc" -eq 0 ]]; then
  echo "  FAIL: resolver returned success with an empty credential"; failures=$((failures + 1))
elif [[ "$lf7_err" != *"no worker credential available"* ]]; then
  echo "  FAIL: empty-credential refusal carried no message: $lf7_err"; failures=$((failures + 1))
else
  echo "  PASS: resolver dies on an empty credential"
fi
chmod 600 "$MT_FIXTURE/env/lunarwing.env"
rm -f "$MT_FIXTURE/env/lunarwing.env"

# Restore the env-writer stub shape the earlier sections rely on.
tenant_pg_password()      { echo "pgpass-fixture"; }

echo ""
if [[ "$failures" -eq 0 ]]; then
  echo "ALL TESTS PASSED"
  exit 0
else
  echo "$failures TEST(S) FAILED"
  exit 1
fi
