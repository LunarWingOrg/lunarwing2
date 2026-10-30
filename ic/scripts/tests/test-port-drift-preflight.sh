#!/usr/bin/env bash
#
# test-port-drift-preflight.sh — TDD harness for the generalized
# env-vs-registry port-drift preflight (WI#509, kumo finding f#191).
#
# Uses LUNARWING_PORTS_REGISTRY / LUNARWING_TENANT_HOME_BASE /
# LUNARWING_MT_OPENRC_INITD_DIR overrides to run against fixture directories.
# No sudo, no services, no root. Fixture env content must NEVER be printed
# by the preflight: the secret probes assert the drift report reduces env
# bytes to ports/loopback URLs only.
#
# Scenarios covered:
#   1. valid setup        — exit 0, env keys OK
#   2. wrong GATEWAY_PORT — exit 1, FAIL (patch-env will not overwrite)
#   3. missing env key    — WARN, exit 0 (backfilled by next render)
#   4. drift in URL port  — exit 1 (XMPP_BRIDGE_URL port mismatch)
#   5. DATABASE_URL port  — OK path, port compared not the password
#   6. DATABASE_URL wrong port + secret never printed
#   7. xmpp-bridge.env bind drift — exit 1
#   8. proxy env + unit drift (systemd + OpenRC)
#   9. darkirc env + TOML drift — exit 1
#  10. quadlet PublishPort drift (pg + worker) — exit 1
#  11. vision external mode skips VISION_SERVICE_URL
#  12. voice disabled skips VOICE_SERVICE_URL
#  13. COORD_ADMIN_PORT mismatch is WARN (soft — preserved by re-render)
#  14. missing lunarwing.env for enabled service — exit 1
#  15. worker quadlet absent (OpenRC host) — INFO, not FAIL
#  16. setup-error exit 2
#  17. non-loopback URL host — FAIL, host named (SI-1)
#  18. non-loopback TOML listen host — FAIL, host named (SI-1)
#  19. DATABASE_URL /db:tag suffix is not the port (SI-2)
#  20. Quadlet PublishPort beyond loopback — FAIL, named (SI-3)
#  21. proxy unit non-loopback or missing --bind — FAIL (SI-5)
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PREFLIGHT="$SCRIPT_DIR/../lunarwing-port-drift-preflight.sh"

# Fixture secrets — must NEVER appear in preflight output.
DB_SECRET='db-pw-aaa111'
FIXTURE_TOKEN='FIXTURE_TOKEN_bbb222'

failures=0
total=0

assert_exit() {  # <label> <expected_exit> <actual_exit>
  total=$((total + 1))
  if [[ "$2" == "$3" ]]; then
    echo "  PASS: $1 (exit $3)"
  else
    echo "  FAIL: $1"
    echo "        expected exit $2, got $3"
    failures=$((failures + 1))
  fi
}

assert_in() {  # <label> <haystack> <needle>
  total=$((total + 1))
  if [[ "$2" == *"$3"* ]]; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1"
    echo "        expected output to contain: $3"
    failures=$((failures + 1))
  fi
}

assert_not_in() {  # <label> <haystack> <needle>
  total=$((total + 1))
  if [[ "$2" != *"$3"* ]]; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1"
    echo "        output must NOT contain: $3"
    failures=$((failures + 1))
  fi
}

# ---- Fixture state ----

FIXTURE_ROOT=""
REGISTRY=""
TENANT_HOME=""
OPENRC_DIR=""
TENANT_NAME="drifty"

# Registry base ports for the fixture tenant.
GW=10000; HTTP=10001; BR=10002; PG=10003; PRX=10004; WC=10005; ORCH=10006
NC=10007; PB=10008; WCA=10009; DI=20000; DIRC=20001; DRPC=20002
VSN=20005; VSNH=20006; OCW=20007; OCH=20008; KW=30000; KH=30001
VOI=40000; VOIH=40001; OC2W=51000; OC2H=51001

# Registry slot values used by the fixture. COORD is the extended_base+9 slot
# (mt-admin allocates coord_admin at extended_base+9).
COORD=$((DI + 9))

registry_json() { # extra tenant fields (JSON member; comma added automatically)
  local extra="${1:-}"
  if [[ -n "$extra" ]]; then extra="$extra,"; fi
  printf '{
  "tenants": {"%s": {"enable_darkirc": true, "enable_proxy": true,
      "workers": {"pebble": true, "opencode": true, "kimi": true, "opencode2": true},
      %s
      "ports": {"gateway": %s, "http": %s, "bridge": %s, "postgres": %s,
                "proxy": %s, "weechat": %s, "orchestrator": %s,
                "nanocode_wss": %s, "pebble_wss": %s, "weechat_adapter": %s},
      "extended_ports": {"darkirc_adapter": %s, "darkirc_irc": %s, "darkirc_rpc": %s,
                         "coord_admin": %s,
                         "vision_service": %s, "vision_health": %s,
                         "opencode_wss": %s, "opencode_health": %s},
      "kimi_ports": {"kimi_wss": %s, "kimi_health": %s},
      "voice_ports": {"voice_service": %s, "voice_health": %s},
      "opencode2_ports": {"opencode2_wss": %s, "opencode2_health": %s}}}
}' "$TENANT_NAME" "$extra" "$GW" "$HTTP" "$BR" "$PG" "$PRX" "$WC" "$ORCH" \
     "$NC" "$PB" "$WCA" "$DI" "$DIRC" "$DRPC" "$COORD" "$VSN" "$VSNH" "$OCW" "$OCH" \
     "$KW" "$KH" "$VOI" "$VOIH" "$OC2W" "$OC2H"
}

# write_main_env <gateway_override> <coord_override> [extra lines...]
write_main_env() {
  local f="$TENANT_HOME/$TENANT_NAME/lunarwing/env/lunarwing.env"
  local gw="$1" coord="$2"
  shift 2
  cat >"$f" <<E
GATEWAY_PORT=$gw
HTTP_PORT=$HTTP
ORCHESTRATOR_PORT=$ORCH
ADAPTER_PORT=$WCA
WEECHAT_ADAPTER_PORT=$WCA
PEBBLE_WSS_PORT=$PB
OPENCODE_WSS_PORT=$OCW
KIMI_WSS_PORT=$KW
OPENCODE2_WSS_PORT=$OC2W
COORD_ADMIN_PORT=$coord
XMPP_BRIDGE_URL=http://127.0.0.1:$BR
RELAY_URL=http://127.0.0.1:$WC
WS_ADAPTER_URL=http://127.0.0.1:$WCA
DARKIRC_ADAPTER_URL=http://127.0.0.1:$DI
VISION_SERVICE_URL=http://127.0.0.1:$VSN
VOICE_SERVICE_URL=http://127.0.0.1:$VOI
DATABASE_URL=postgres://lunarwing:${DB_SECRET}@127.0.0.1:$PG/lunarwing
E
  local line
  for line in "$@"; do printf '%s\n' "$line" >>"$f"; done
}

write_bridge_env() {
  cat >"$TENANT_HOME/$TENANT_NAME/lunarwing/env/xmpp-bridge.env" <<E
XMPP_BRIDGE_BIND=${1:-127.0.0.1:$BR}
XMPP_BRIDGE_TOKEN=$FIXTURE_TOKEN
E
}

write_proxy_env() {
  cat >"$TENANT_HOME/$TENANT_NAME/lunarwing/env/proxy.env" <<E
PROXY_PORT=${1:-$PRX}
PROXY_BIND=127.0.0.1
TENSORZERO_URL=http://127.0.0.1:9999
E
}

write_darkirc_env() {
  cat >"$TENANT_HOME/$TENANT_NAME/lunarwing/env/darkirc-adapter.env" <<E
DARKIRC_HOST=127.0.0.1
DARKIRC_PORT=${1:-$DIRC}
ADAPTER_HOST=127.0.0.1
ADAPTER_PORT=${2:-$DI}
ADAPTER_SECRET=$FIXTURE_TOKEN
E
}

write_darkirc_toml() {
  cat >"$TENANT_HOME/$TENANT_NAME/lunarwing/state/darkirc/darkirc_config.toml" <<E
irc_listen = "tcp://127.0.0.1:${1:-$DIRC}"
[rpc]
rpc_listen = "tcp://127.0.0.1:${2:-$DRPC}"
E
}

write_quadlet() { # <file> <publish-lines...>
  local f="$1"
  shift
  mkdir -p "$(dirname "$f")"
  { printf '[Container]\n'
    local l
    for l in "$@"; do printf '%s\n' "$l"; done
  } >"$f"
}

create_fixture() { # <scenario>
  local scenario="${1:-valid}"
  FIXTURE_ROOT="$(mktemp -d)"
  trap 'cleanup_fixture' EXIT
  REGISTRY="$FIXTURE_ROOT/ports.json"
  TENANT_HOME="$FIXTURE_ROOT/home"
  OPENRC_DIR="$FIXTURE_ROOT/init.d"
  mkdir -p "$TENANT_HOME/$TENANT_NAME/lunarwing/env" \
           "$TENANT_HOME/$TENANT_NAME/lunarwing/state/darkirc" \
           "$TENANT_HOME/$TENANT_NAME/.config/systemd/user" \
           "$OPENRC_DIR"

  case "$scenario" in
    vision-external) registry_json '"vision": {"mode": "external"}' >"$REGISTRY" ;;
    voice-disabled)  registry_json '"enable_voice": false' >"$REGISTRY" ;;
    *)               registry_json '"enable_voice": true' >"$REGISTRY" ;;
  esac

  case "$scenario" in
    missing-env) ;;  # lunarwing.env intentionally absent
    drift-gateway)    write_main_env "$((GW + 500))" "$COORD" ;;
    coord-soft)       write_main_env "$GW" "$COORD" ;;
    url-drift)        write_main_env "$GW" "$COORD" ;;
    *)                write_main_env "$GW" "$COORD" ;;
  esac

  # A missing registry slot must skip the check rather than false-FAIL:
  # scenario no-coord removes the coord_admin slot from the registry.
  if [[ "$scenario" == no-coord ]]; then
    jq --arg t "$TENANT_NAME" 'del(.tenants[$t].extended_ports.coord_admin)' "$REGISTRY" >"$REGISTRY.tmp"
    mv "$REGISTRY.tmp" "$REGISTRY"
  fi

  case "$scenario" in
    missing-env) ;;
    db-wrong)
      write_main_env "$GW" "$COORD"
      sed -i "s|@127.0.0.1:$PG/|@127.0.0.1:$((PG + 500))/|" \
        "$TENANT_HOME/$TENANT_NAME/lunarwing/env/lunarwing.env"
      ;;
  esac

  case "$scenario" in
    missing-env) ;;
    *) write_bridge_env ;;
  esac

  case "$scenario" in
    proxy-drift)
      write_proxy_env $((PRX + 500))
      cat >"$TENANT_HOME/$TENANT_NAME/.config/systemd/user/lunarwing-proxy-${TENANT_NAME}.service" <<E
[Service]
ExecStart=/usr/bin/python3 /bin/lunarwing-proxy.py --port $((PRX + 900)) --bind 127.0.0.1
E
      mkdir -p "$OPENRC_DIR"
      cat >"$OPENRC_DIR/lunarwing-proxy-${TENANT_NAME}" <<E
: "\${proxy_args:=/bin/lunarwing-proxy.py --port $((PRX + 901)) --bind 127.0.0.1 --tensorzero-env}"
E
      ;;
    *)
      write_proxy_env
      # A correctly rendered proxy pair so the OK path of the unit checks
      # is exercised in the valid scenarios.
      cat >"$TENANT_HOME/$TENANT_NAME/.config/systemd/user/lunarwing-proxy-${TENANT_NAME}.service" <<E
[Service]
ExecStart=/usr/bin/python3 /bin/lunarwing-proxy.py --port $PRX --bind 127.0.0.1
E
      cat >"$OPENRC_DIR/lunarwing-proxy-${TENANT_NAME}" <<E
: "\${proxy_args:=/bin/lunarwing-proxy.py --port $PRX --bind 127.0.0.1 --tensorzero-env}"
E
      ;;
  esac

  case "$scenario" in
    darkirc-drift) write_darkirc_env $((DIRC + 500)) $((DI + 500))
                   write_darkirc_toml $((DIRC + 600)) $((DRPC + 600)) ;;
    *)             write_darkirc_env
                   write_darkirc_toml ;;
  esac

  local qd="$TENANT_HOME/$TENANT_NAME/.config/containers/systemd"
  case "$scenario" in
    quadlet-drift)
      write_quadlet "$qd/lunarwing-pg-${TENANT_NAME}.container" \
        "PublishPort=127.0.0.1:$((PG + 700)):5432"
      write_quadlet "$qd/lunarwing-pebble-${TENANT_NAME}.container" \
        "PublishPort=127.0.0.1:$((PB + 700)):$PB" \
        "PublishPort=127.0.0.1:$((20000 + 4)):8443"
      ;;
    no-quadlets) ;;  # OpenRC/docker host shape
    vision-external|voice-disabled)
      write_quadlet "$qd/lunarwing-pg-${TENANT_NAME}.container" \
        "PublishPort=127.0.0.1:$PG:5432"
      ;;
    *)
      write_quadlet "$qd/lunarwing-pg-${TENANT_NAME}.container" \
        "PublishPort=127.0.0.1:$PG:5432"
      write_quadlet "$qd/lunarwing-voice-${TENANT_NAME}.container" \
        "PublishPort=127.0.0.1:$VOI:8085" \
        "PublishPort=127.0.0.1:$VOIH:8086"
      write_quadlet "$qd/lunarwing-vision-${TENANT_NAME}.container" \
        "PublishPort=127.0.0.1:$VSN:8085" \
        "PublishPort=127.0.0.1:$VSNH:8086"
      for w in pebble opencode kimi opencode2; do
        local wss health
        case "$w" in
          pebble)    wss=$PB;    health=$((20000 + 4)) ;;
          opencode)  wss=$OCW;   health=$OCH ;;
          kimi)      wss=$KW;    health=$KH ;;
          opencode2) wss=$OC2W;  health=$OC2H ;;
        esac
        write_quadlet "$qd/lunarwing-${w}-${TENANT_NAME}.container" \
          "PublishPort=127.0.0.1:$wss:$wss" \
          "PublishPort=127.0.0.1:$health:8443"
      done
      ;;
  esac
}

cleanup_fixture() {
  if [[ -n "${FIXTURE_ROOT:-}" && -d "${FIXTURE_ROOT:-}" ]]; then
    rm -rf "$FIXTURE_ROOT"
  fi
}

run_preflight() { # [args...]
  PREFLIGHT_OUT=""
  PREFLIGHT_EXIT=0
  set +e
  PREFLIGHT_OUT="$(LUNARWING_PORTS_REGISTRY="$REGISTRY" \
    LUNARWING_TENANT_HOME_BASE="$TENANT_HOME" \
    LUNARWING_MT_OPENRC_INITD_DIR="$OPENRC_DIR" \
    bash "$PREFLIGHT" "$@" 2>&1)"
  PREFLIGHT_EXIT=$?
  set -e
}

# ============================================================
# 1. Valid setup
# ============================================================
echo "=== 1. valid setup ==="
create_fixture valid
run_preflight "$TENANT_NAME"
assert_exit "valid → exit 0" 0 "$PREFLIGHT_EXIT"
assert_in  "gateway OK" "$PREFLIGHT_OUT" "[OK  ] GATEWAY_PORT"
assert_in  "db port OK" "$PREFLIGHT_OUT" "postgres port $PG"
assert_in  "bind OK" "$PREFLIGHT_OUT" "[OK  ] XMPP_BRIDGE_BIND"
assert_in  "proxy unit OK" "$PREFLIGHT_OUT" "[OK  ] proxy unit (systemd)"
assert_in  "darkirc toml OK" "$PREFLIGHT_OUT" "[OK  ] rpc_listen"
assert_in  "pg quadlet OK" "$PREFLIGHT_OUT" "[OK  ] pg quadlet"
assert_not_in "no db password" "$PREFLIGHT_OUT" "$DB_SECRET"
assert_not_in "no token" "$PREFLIGHT_OUT" "$FIXTURE_TOKEN"
cleanup_fixture
trap - EXIT

# ============================================================
# 2. Wrong GATEWAY_PORT (patch-env will not overwrite)
# ============================================================
echo "=== 2. drift-gateway ==="
create_fixture drift-gateway
run_preflight "$TENANT_NAME"
assert_exit "drift → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "gateway FAIL" "$PREFLIGHT_OUT" "[FAIL] GATEWAY_PORT"
assert_in  "mentions registry value" "$PREFLIGHT_OUT" "registry gateway=$GW"
assert_not_in "no db password" "$PREFLIGHT_OUT" "$DB_SECRET"
cleanup_fixture
trap - EXIT

# ============================================================
# 3. Missing env key is WARN, not FAIL
# ============================================================
echo "=== 3. missing key → WARN ==="
create_fixture missing-key
sed -i '/^PEBBLE_WSS_PORT=/d' "$TENANT_HOME/$TENANT_NAME/lunarwing/env/lunarwing.env"
run_preflight "$TENANT_NAME"
assert_exit "missing key → exit 0" 0 "$PREFLIGHT_EXIT"
assert_in  "pebble warn" "$PREFLIGHT_OUT" "[WARN] PEBBLE_WSS_PORT"
cleanup_fixture
trap - EXIT

# ============================================================
# 4. URL port drift (XMPP_BRIDGE_URL)
# ============================================================
echo "=== 4. url-drift ==="
create_fixture url-drift
sed -i "s|^XMPP_BRIDGE_URL=.*|XMPP_BRIDGE_URL=http://127.0.0.1:$((BR + 5))|" \
  "$TENANT_HOME/$TENANT_NAME/lunarwing/env/lunarwing.env"
run_preflight "$TENANT_NAME"
assert_exit "url drift → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "bridge url FAIL" "$PREFLIGHT_OUT" "[FAIL] XMPP_BRIDGE_URL"
cleanup_fixture
trap - EXIT

# ============================================================
# 5/6. DATABASE_URL: port compared, password never printed
# ============================================================
echo "=== 5. db-wrong ==="
create_fixture db-wrong
run_preflight "$TENANT_NAME"
assert_exit "db drift → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "db FAIL" "$PREFLIGHT_OUT" "[FAIL] DATABASE_URL"
assert_in  "expected port mentioned" "$PREFLIGHT_OUT" "postgres port $((PG + 500)) (registry postgres=$PG)"
assert_not_in "no db password" "$PREFLIGHT_OUT" "$DB_SECRET"
cleanup_fixture
trap - EXIT

# ============================================================
# 7. xmpp-bridge.env bind drift
# ============================================================
echo "=== 7. bind-drift ==="
create_fixture bind-drift
write_bridge_env "127.0.0.1:$((BR + 7))"
run_preflight "$TENANT_NAME"
assert_exit "bind drift → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "bind FAIL" "$PREFLIGHT_OUT" "[FAIL] XMPP_BRIDGE_BIND"
cleanup_fixture
trap - EXIT

# ============================================================
# 8. proxy env + systemd/OpenRC unit drift
# ============================================================
echo "=== 8. proxy-drift ==="
create_fixture proxy-drift
run_preflight "$TENANT_NAME"
assert_exit "proxy drift → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "proxy env FAIL" "$PREFLIGHT_OUT" "[FAIL] PROXY_PORT"
assert_in  "systemd unit FAIL" "$PREFLIGHT_OUT" "[FAIL] proxy unit (systemd)"
assert_in  "openrc unit FAIL" "$PREFLIGHT_OUT" "[FAIL] proxy unit (openrc)"
cleanup_fixture
trap - EXIT

# ============================================================
# 9. darkirc env + TOML drift
# ============================================================
echo "=== 9. darkirc-drift ==="
create_fixture darkirc-drift
run_preflight "$TENANT_NAME"
assert_exit "darkirc drift → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "darkirc env FAIL" "$PREFLIGHT_OUT" "[FAIL] ADAPTER_PORT"
assert_in  "toml irc FAIL" "$PREFLIGHT_OUT" "[FAIL] irc_listen"
assert_in  "toml rpc FAIL" "$PREFLIGHT_OUT" "[FAIL] rpc_listen"
assert_not_in "no secret" "$PREFLIGHT_OUT" "$FIXTURE_TOKEN"
cleanup_fixture
trap - EXIT

# ============================================================
# 10. Quadlet PublishPort drift (pg + worker)
# ============================================================
echo "=== 10. quadlet-drift ==="
create_fixture quadlet-drift
run_preflight "$TENANT_NAME"
assert_exit "quadlet drift → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "pg quadlet FAIL" "$PREFLIGHT_OUT" "[FAIL] pg quadlet"
assert_in  "pebble quadlet FAIL" "$PREFLIGHT_OUT" "[FAIL] pebble quadlet"
cleanup_fixture
trap - EXIT

# ============================================================
# 11. External LunarVision: registry ports not checked for the URL,
#     and the vision quadlet is not required.
# ============================================================
echo "=== 11. vision-external ==="
create_fixture vision-external
sed -i "s|^VISION_SERVICE_URL=.*|VISION_SERVICE_URL=https://vision.example.com/api|" \
  "$TENANT_HOME/$TENANT_NAME/lunarwing/env/lunarwing.env"
run_preflight "$TENANT_NAME"
assert_exit "vision external → exit 0" 0 "$PREFLIGHT_EXIT"
assert_in  "external noted" "$PREFLIGHT_OUT" "external LunarVision mode"
assert_not_in "no vision quadlet demand" "$PREFLIGHT_OUT" "vision quadlet"
cleanup_fixture
trap - EXIT

# ============================================================
# 12. Voice disabled: VOICE_SERVICE_URL not checked, no voice quadlet demand
# ============================================================
echo "=== 12. voice-disabled ==="
create_fixture voice-disabled
sed -i "s|^VOICE_SERVICE_URL=.*|VOICE_SERVICE_URL=http://127.0.0.1:$((VOI + 9))|" \
  "$TENANT_HOME/$TENANT_NAME/lunarwing/env/lunarwing.env"
run_preflight "$TENANT_NAME"
assert_exit "voice disabled → exit 0" 0 "$PREFLIGHT_EXIT"
assert_not_in "no voice url check" "$PREFLIGHT_OUT" "VOICE_SERVICE_URL"
assert_not_in "no voice quadlet demand" "$PREFLIGHT_OUT" "voice quadlet"
cleanup_fixture
trap - EXIT

# ============================================================
# 13. COORD_ADMIN_PORT mismatch is a WARN (soft)
# ============================================================
echo "=== 13. coord-soft ==="
create_fixture coord-soft
sed -i "s|^COORD_ADMIN_PORT=.*|COORD_ADMIN_PORT=$((COORD + 3))|" \
  "$TENANT_HOME/$TENANT_NAME/lunarwing/env/lunarwing.env"
run_preflight "$TENANT_NAME"
assert_exit "coord soft → exit 0" 0 "$PREFLIGHT_EXIT"
assert_in  "coord WARN" "$PREFLIGHT_OUT" "[WARN] COORD_ADMIN_PORT"
assert_not_in "no coord FAIL" "$PREFLIGHT_OUT" "[FAIL] COORD_ADMIN_PORT"
cleanup_fixture
trap - EXIT

# ============================================================
# 14. Missing lunarwing.env — FAIL for an allocated tenant
# ============================================================
echo "=== 14. missing-env ==="
create_fixture missing-env
run_preflight "$TENANT_NAME"
assert_exit "missing env → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "env file FAIL" "$PREFLIGHT_OUT" "[FAIL] env file"
cleanup_fixture
trap - EXIT

# ============================================================
# 15. No quadlets (OpenRC/docker host): INFO, not FAIL
# ============================================================
echo "=== 15. no-quadlets ==="
create_fixture no-quadlets
run_preflight "$TENANT_NAME"
assert_exit "no quadlets → exit 0" 0 "$PREFLIGHT_EXIT"
assert_in  "pg quadlet INFO" "$PREFLIGHT_OUT" "[INFO] pg quadlet"
assert_in  "worker quadlet INFO" "$PREFLIGHT_OUT" "[INFO] pebble quadlet"
cleanup_fixture
trap - EXIT

# ============================================================
# 16. Missing registry slot: check skipped (no false FAIL)
# ============================================================
echo "=== 16. no-coord slot ==="
create_fixture no-coord
sed -i "s|^COORD_ADMIN_PORT=.*|COORD_ADMIN_PORT=|" \
  "$TENANT_HOME/$TENANT_NAME/lunarwing/env/lunarwing.env"
run_preflight "$TENANT_NAME"
assert_exit "no slot → exit 0" 0 "$PREFLIGHT_EXIT"
assert_not_in "no coord false-FAIL" "$PREFLIGHT_OUT" "[FAIL] COORD_ADMIN_PORT"
assert_not_in "no coord false-WARN" "$PREFLIGHT_OUT" "[WARN] COORD_ADMIN_PORT"
cleanup_fixture
trap - EXIT

# ============================================================
# 17. Setup-error exit 2
# ============================================================
echo "=== 17. setup-error ==="
PREFLIGHT_EXIT=0
set +e
PREFLIGHT_OUT="$(LUNARWING_PORTS_REGISTRY="/nonexistent/ports.json" \
  LUNARWING_TENANT_HOME_BASE="/nonexistent/home" \
  bash "$PREFLIGHT" "$TENANT_NAME" 2>&1)"
PREFLIGHT_EXIT=$?
set -e
assert_exit "missing registry → exit 2" 2 "$PREFLIGHT_EXIT"
assert_in "error message" "$PREFLIGHT_OUT" 'error:'

# ============================================================
# 18. Bad tenant argument → exit 2
# ============================================================
create_fixture valid
PREFLIGHT_EXIT=0
set +e
PREFLIGHT_OUT="$(LUNARWING_PORTS_REGISTRY="$REGISTRY" \
  LUNARWING_TENANT_HOME_BASE="$TENANT_HOME" \
  bash "$PREFLIGHT" "unknown-tenant" 2>&1)"
PREFLIGHT_EXIT=$?
set -e
assert_exit "unknown tenant → exit 2" 2 "$PREFLIGHT_EXIT"
cleanup_fixture
trap - EXIT

# ============================================================
# 19. Non-loopback URL host with a matching port is a FAIL (SI-1)
# ============================================================
echo "=== 19. url host drift ==="
create_fixture valid
sed -i "s|^XMPP_BRIDGE_URL=.*|XMPP_BRIDGE_URL=http://0.0.0.0:$BR|" \
  "$TENANT_HOME/$TENANT_NAME/lunarwing/env/lunarwing.env"
run_preflight "$TENANT_NAME"
assert_exit "non-loopback url host → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "url host FAIL" "$PREFLIGHT_OUT" "[FAIL] XMPP_BRIDGE_URL"
assert_in  "url host named" "$PREFLIGHT_OUT" "host 0.0.0.0 is not loopback"
assert_not_in "url host not OK" "$PREFLIGHT_OUT" "[OK  ] XMPP_BRIDGE_URL"
cleanup_fixture
trap - EXIT

# ============================================================
# 20. Non-loopback TOML listen host with a matching port is a FAIL (SI-1)
# ============================================================
echo "=== 20. toml host drift ==="
create_fixture valid
sed -i "s|tcp://127.0.0.1:$DIRC|tcp://0.0.0.0:$DIRC|" \
  "$TENANT_HOME/$TENANT_NAME/lunarwing/state/darkirc/darkirc_config.toml"
run_preflight "$TENANT_NAME"
assert_exit "non-loopback toml host → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "toml host FAIL" "$PREFLIGHT_OUT" "[FAIL] irc_listen"
assert_in  "toml host named" "$PREFLIGHT_OUT" "listen host 0.0.0.0 is not loopback"
assert_not_in "toml host not OK" "$PREFLIGHT_OUT" "[OK  ] irc_listen"
cleanup_fixture
trap - EXIT

# ============================================================
# 21. DATABASE_URL /db:tag suffix is not the port (SI-2)
# ============================================================
echo "=== 21. db-name suffix ==="
create_fixture valid
sed -i "s|^DATABASE_URL=.*|DATABASE_URL=postgres://lunarwing:${DB_SECRET}@127.0.0.1:${PG}/lunarwing:ext|" \
  "$TENANT_HOME/$TENANT_NAME/lunarwing/env/lunarwing.env"
run_preflight "$TENANT_NAME"
assert_exit "db suffix → exit 0" 0 "$PREFLIGHT_EXIT"
assert_in  "db port from host:PORT" "$PREFLIGHT_OUT" "postgres port $PG"
assert_not_in "db suffix not the port" "$PREFLIGHT_OUT" "postgres port ext"
assert_not_in "no db password" "$PREFLIGHT_OUT" "$DB_SECRET"
cleanup_fixture
trap - EXIT

# ============================================================
# 22. Quadlet PublishPort beyond loopback is a FAIL (SI-3)
# ============================================================
for publish in "0.0.0.0:$PG:5432" "$PG:5432"; do
  echo "=== 22. quadlet publish beyond loopback ($publish) ==="
  create_fixture valid
  write_quadlet "$TENANT_HOME/$TENANT_NAME/.config/containers/systemd/lunarwing-pg-${TENANT_NAME}.container" \
    "PublishPort=$publish"
  run_preflight "$TENANT_NAME"
  assert_exit "non-loopback publish $publish → exit 1" 1 "$PREFLIGHT_EXIT"
  assert_in  "pg quadlet FAIL ($publish)" "$PREFLIGHT_OUT" "[FAIL] pg quadlet"
  assert_in  "publish drift named ($publish)" "$PREFLIGHT_OUT" "not strict 127.0.0.1"
  cleanup_fixture
  trap - EXIT
done

# ============================================================
# 23. Proxy unit must bind 127.0.0.1 on its --port line (SI-5)
# ============================================================
echo "=== 23. proxy unit non-loopback bind ==="
create_fixture valid
cat >"$TENANT_HOME/$TENANT_NAME/.config/systemd/user/lunarwing-proxy-${TENANT_NAME}.service" <<E
[Service]
ExecStart=/usr/bin/python3 /bin/lunarwing-proxy.py --port $PRX --bind 0.0.0.0
E
run_preflight "$TENANT_NAME"
assert_exit "non-loopback proxy bind → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "proxy unit FAIL" "$PREFLIGHT_OUT" "[FAIL] proxy unit (systemd)"
assert_in  "proxy bind named" "$PREFLIGHT_OUT" "binds 0.0.0.0 instead of 127.0.0.1"
cleanup_fixture
trap - EXIT

# ============================================================
# 24. Proxy unit without --bind on the --port line is a FAIL
# ============================================================
echo "=== 24. proxy unit missing bind ==="
create_fixture valid
cat >"$TENANT_HOME/$TENANT_NAME/.config/systemd/user/lunarwing-proxy-${TENANT_NAME}.service" <<E
[Service]
ExecStart=/usr/bin/python3 /bin/lunarwing-proxy.py --port $PRX
E
run_preflight "$TENANT_NAME"
assert_exit "proxy unit without bind → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "missing bind FAIL" "$PREFLIGHT_OUT" "no --bind on the --port line"
cleanup_fixture
trap - EXIT

# ============================================================
# Summary
# ============================================================
echo ""
echo "----------------------------------------------------------------"
echo "Tests run: $total  Failures: $failures"
if [[ "$failures" -eq 0 ]]; then
  echo "ALL TESTS PASSED"
  exit 0
else
  echo "$failures TEST(S) FAILED"
  exit 1
fi
