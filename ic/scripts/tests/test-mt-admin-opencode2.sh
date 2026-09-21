#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
failures=0

pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }
assert_contains() {
  local file="$1" expected="$2" label="$3"
  if grep -Fq -- "$expected" "$file"; then pass "$label"; else fail "$label"; fi
}
assert_not_contains() {
  local file="$1" unexpected="$2" label="$3"
  if grep -Fq -- "$unexpected" "$file"; then fail "$label"; else pass "$label"; fi
}

tenant_env_dir() { printf '%s/env\n' "$fixture"; }
tenant_state_dir() { printf '%s/state\n' "$fixture"; }
tenant_lw_root() { printf '%s/home/lunarwing\n' "$fixture"; }
tenant_home() { printf '%s/home\n' "$fixture"; }
tenant_quadlet_dir() { printf '%s/quadlets\n' "$fixture"; }
tenant_run_dir() { printf '%s/run\n' "$fixture"; }
tenant_exists_in_registry() { return 0; }
_ctr() { return 1; }
chown() { CHOWN_CALL="$*"; }
# CI containers run with no-new-privileges, so real sudo dies mid-harness;
# strip the `-u <tenant>` prefix and run the wrapped command directly.
sudo() {
  if [[ "${1:-}" == "-u" ]]; then shift 2; fi
  command "$@"
}
say() { :; }
sanitize_name() { printf '%s' "$1"; }

mkdir -p "$fixture/env" "$fixture/state" "$fixture/home/lunarwing" "$fixture/quadlets"
env_file="$fixture/env/lunarwing.env"
cat >"$env_file" <<'EOF'
GATEWAY_AUTH_TOKEN=gate%token
LLM_API_KEY=llm%key
OPENCODE2_MODEL=old-model
OPENCODE2_BASE_URL=https://old.example/v1
EOF

# ── configure-opencode2 ──────────────────────────────────────────────────────
configure_opencode2 root new-model '' 
assert_contains "$env_file" 'OPENCODE2_MODEL=new-model' 'configure-opencode2 updates selected model'
assert_contains "$env_file" 'OPENCODE2_BASE_URL=https://old.example/v1' 'configure-opencode2 preserves omitted base URL'
assert_not_contains "$env_file" 'OPENCODE2_MODEL=old-model' 'configure-opencode2 replaces prior model'

# ── ensure_external_worker_config gating ─────────────────────────────────────
tenant_worker_enabled() { return 1; }
ports_get() { fail 'unselected opencode2 queried port'; return 1; }
rm -f "$fixture/state/config.toml"
ensure_external_worker_config root opencode2 opencode2_wss
if [[ ! -e "$fixture/state/config.toml" ]]; then
  pass 'unselected opencode2 emits no daemon config'
else
  fail 'unselected opencode2 emitted daemon config'
fi

tenant_worker_enabled() { return 0; }
ports_get() {
  case "$2" in
    opencode2_wss) printf '51000\n' ;;
    opencode2_health) printf '51001\n' ;;
    opencode_wss) printf '20007\n' ;;
    opencode_health) printf '20008\n' ;;
  esac
}
rm -f "$fixture/state/config.toml"
ensure_external_worker_config root opencode2 opencode2_wss
assert_contains "$fixture/state/config.toml" 'name = "opencode2"' 'daemon config names the opencode2 worker'
assert_contains "$fixture/state/config.toml" 'url = "ws://127.0.0.1:51000/ws/agent"' 'daemon config points at the opencode2 WSS port'
assert_contains "$fixture/state/config.toml" 'auth_token = "gate%token"' 'daemon config carries the gateway token'
if [[ "$(stat -c '%a' "$fixture/state/config.toml")" == "600" ]]; then
  pass 'opencode2 daemon config is private'
else
  fail 'opencode2 daemon config mode is not 600'
fi

# Idempotent re-run keeps one entry.
count_before="$(grep -c 'name = "opencode2"' "$fixture/state/config.toml")"
ensure_external_worker_config root opencode2 opencode2_wss
count_after="$(grep -c 'name = "opencode2"' "$fixture/state/config.toml")"
if [[ "$count_before" == "$count_after" ]]; then
  pass 'ensure_external_worker_config re-run is idempotent'
else
  fail "ensure_external_worker_config re-run duplicated entries ($count_before -> $count_after)"
fi

# ── systemd quadlet ──────────────────────────────────────────────────────────
MT_ROOTLESS=true
render_worker_quadlet root opencode2 8443
quadlet="$fixture/quadlets/lunarwing-opencode2-root.container"
assert_contains "$quadlet" 'ContainerName=lunarwing-opencode2-root' 'quadlet names the opencode2 container'
assert_contains "$quadlet" 'Image=lunarwing-worker-opencode2:latest' 'quadlet uses the opencode2 image'
assert_contains "$quadlet" 'UserNS=keep-id:uid=10003,gid=10003' 'systemd opencode2 keeps rootless UID mapping'
assert_contains "$quadlet" 'PublishPort=127.0.0.1:51000:51000' 'quadlet publishes the opencode2 WSS port'
assert_contains "$quadlet" 'PublishPort=127.0.0.1:51001:8443' 'quadlet publishes the opencode2 health port'
assert_contains "$quadlet" 'Environment=OPENCODE2_MODEL=new-model' 'quadlet inlines the model override'
assert_contains "$quadlet" 'Environment=OPENCODE2_BASE_URL=https://old.example/v1' 'quadlet inlines the base URL override'
assert_contains "$quadlet" 'Environment=TENSORZERO_API_KEY=llm%%key' 'quadlet inlines the TensorZero key'
assert_contains "$quadlet" 'Environment=AGENT_AUTH_TOKEN=gate%%token' 'quadlet inlines the agent token'
assert_contains "$quadlet" 'Exec=--mode websocket' 'quadlet selects websocket mode'
assert_contains "$quadlet" 'HealthCmd=curl -sf http://127.0.0.1:8443/health' 'quadlet probes the health endpoint'

# v1 worker's quadlet must be untouched by the opencode2 render.
cat >"$env_file.bak" <<"EOF"
GATEWAY_AUTH_TOKEN=gate%token
EOF
cp "$env_file.bak" "$env_file"
render_worker_quadlet root opencode 8443
v1_quadlet="$fixture/quadlets/lunarwing-opencode-root.container"
assert_contains "$v1_quadlet" 'UserNS=keep-id:uid=10001,gid=10001' 'v1 quadlet keeps UID 10001'
assert_not_contains "$v1_quadlet" 'OPENCODE2_MODEL' 'v1 quadlet does not leak opencode2 overrides'
rm -f "$env_file.bak"

# ── scoped worker credential (WI#205) ────────────────────────────────────────
# A tenant carrying WORKER_AUTH_TOKEN must never hand the gateway bearer to a
# worker container or put it in the daemon's worker config.
cat >"$env_file" <<'EOF'
GATEWAY_AUTH_TOKEN=gate%token
WORKER_AUTH_TOKEN=scoped%token
LLM_API_KEY=llm%key
OPENCODE2_MODEL=new-model
OPENCODE2_BASE_URL=https://old.example/v1
EOF

MT_ROOTLESS=true
render_worker_quadlet root opencode2 8443
scoped_quadlet="$fixture/quadlets/lunarwing-opencode2-root.container"
assert_contains "$scoped_quadlet" 'Environment=AGENT_AUTH_TOKEN=scoped%%token' \
  'quadlet uses the scoped worker credential'
assert_not_contains "$scoped_quadlet" 'GATEWAY' \
  'quadlet carries no gateway bearer'

# The pre-existing config.toml entry still holds the gateway bearer; a re-run
# must migrate it rather than skip past it.
rm -f "$fixture/state/config.toml"
ensure_external_worker_config root opencode2 opencode2_wss
# Recreate the pre-WI#205 shape in place, then re-run the migrate path.
python3 - "$fixture/state/config.toml" <<'PY'
import sys
path = sys.argv[1]
text = open(path).read().replace('auth_token = "scoped%token"', 'auth_token = "gate%token"')
open(path, "w").write(text)
PY
assert_contains "$fixture/state/config.toml" 'auth_token = "gate%token"' \
  'legacy config carries the gateway bearer (pre-migration fixture)'
ensure_external_worker_config root opencode2 opencode2_wss
assert_contains "$fixture/state/config.toml" 'auth_token = "scoped%token"' \
  're-run migrates the config to the scoped credential'
assert_not_contains "$fixture/state/config.toml" 'auth_token = "gate%token"' \
  'no gateway bearer remains in the worker config'
scoped_entries="$(grep -c 'name = "opencode2"' "$fixture/state/config.toml")"
if [[ "$scoped_entries" == "1" ]]; then
  pass 'credential migration does not duplicate the worker entry'
else
  fail "credential migration duplicated entries ($scoped_entries)"
fi

# Missing scoped token: fall back to the gateway bearer so pre-WI#205 tenants
# keep working until their next lifecycle pass rewrites both sides.
cat >"$env_file" <<'EOF'
GATEWAY_AUTH_TOKEN=gate%token
LLM_API_KEY=llm%key
OPENCODE2_MODEL=new-model
OPENCODE2_BASE_URL=https://old.example/v1
EOF
render_worker_quadlet root opencode2 8443
assert_contains "$scoped_quadlet" 'Environment=AGENT_AUTH_TOKEN=gate%%token' \
  'legacy tenant falls back to the gateway bearer'

# ── workspace ownership ──────────────────────────────────────────────────────
# Override consumed by sourced workspace preparation.
# shellcheck disable=SC2034
MT_ROOTLESS=false
CHOWN_CALL=''
prepare_worker_workspace root opencode2 >/dev/null
if [[ "$CHOWN_CALL" == "-R 10003:10003 $fixture/home/lunarwing/opencode2-workspace" ]]; then
  pass 'rootful opencode2 workspace uses image UID and GID'
else
  fail "rootful opencode2 workspace ownership wrong: $CHOWN_CALL"
fi
if [[ "$(stat -c '%a' "$fixture/home/lunarwing/opencode2-workspace")" == "2770" ]]; then
  pass 'rootless opencode2 workspace is private and setgid'
else
  fail 'opencode2 workspace mode is not 2770'
fi

# ── worker name validation ───────────────────────────────────────────────────
if prepare_worker_lifecycle root opencode2 >/dev/null 2>&1; then
  pass 'require_worker_name accepts opencode2'
else
  fail 'require_worker_name rejected opencode2'
fi
tenant_exists_in_registry() { return 0; }
if (prepare_worker_lifecycle root nope) >/dev/null 2>&1; then
  fail 'require_worker_name accepted an unknown worker'
else
  pass 'require_worker_name rejects unknown workers'
fi

# ── builder contract ────────────────────────────────────────────────────────
mkdir -p "$fixture/target/opencode2.4lunarwing"
# Override consumed by the sourced container helpers.
# shellcheck disable=SC2034
CONTAINER_RT=docker
ensure_container_runtime() { :; }
docker() { BUILD_CALL="$*"; }
BUILD_CALL=''
build_opencode2_worker false true "$fixture/target" >/dev/null
if [[ "$BUILD_CALL" == *'--build-arg WITH_TOOLCHAINS=true'* \
  && "$BUILD_CALL" == *"-t lunarwing-worker-opencode2:latest"* \
  && "$BUILD_CALL" == *"$fixture/target/opencode2.4lunarwing"* ]]; then
  pass 'opencode2 builder forwards toolchain flag and source root'
else
  fail "opencode2 builder contract wrong: $BUILD_CALL"
fi

BUILD_CALL=''
build_opencode2_worker true false "$fixture/target" >/dev/null
if [[ "$BUILD_CALL" == *'--no-cache'* ]]; then
  pass 'opencode2 builder honors --no-cache'
else
  fail 'opencode2 builder ignored --no-cache'
fi

# Missing worker dir dies instead of building from the wrong context.
BUILD_CALL=''
if (build_opencode2_worker false false "$fixture/missing") >/dev/null 2>&1; then
  fail 'opencode2 builder accepted a missing worker dir'
else
  pass 'opencode2 builder refuses a missing worker dir'
fi

# ── build flag forwarding through the dispatcher ─────────────────────────────
require_root() { :; }
BUILD_CALL=''
build_tenant() { BUILD_CALL="tenant:$*"; }
main build-tenant root --with-opencode2
if [[ "$BUILD_CALL" == 'tenant:root false false false false false false true' ]]; then
  pass 'build-tenant forwards --with-opencode2'
else
  fail "build-tenant forwarding wrong: $BUILD_CALL"
fi

BUILD_CALL=''
build_all() { BUILD_CALL="all:$*"; }
main build-all --with-opencode2
if [[ "$BUILD_CALL" == 'all:false false false false false true' ]]; then
  pass 'build-all forwards --with-opencode2'
else
  fail "build-all forwarding wrong: $BUILD_CALL"
fi

BUILD_CALL=''
build_opencode2_worker() { BUILD_CALL="worker:$*"; }
main build-opencode2-worker --with-toolchains
if [[ "$BUILD_CALL" == 'worker:false true' ]]; then
  pass 'build-opencode2-worker forwards toolchain flag'
else
  fail "build-opencode2-worker forwarding wrong: $BUILD_CALL"
fi

# ── shared worker source root includes the opencode2 dir ─────────────────────
BUILD_LIB="$(dirname "$MT_ADMIN")/mt-admin-lib/05-tenant-build.sh"
assert_contains "$BUILD_LIB" 'worker_paths+=(opencode2.4lunarwing)' 'shared worker source root tracks opencode2 dir'

# ── reserved tenant-name prefix ─────────────────────────────────────────────
cat >"$fixture/prefix-test.sh" <<'EOF'
set -euo pipefail
die() { exit 1; }
case "$1" in
  pg-*|proxy-*|nanocode-*|pebble-*|opencode-*|kimi-*|opencode2-*|weechat-*) die ;;
  *) exit 0 ;;
esac
EOF
# The guard lives inside add_tenant; assert the pattern list in source.
LIFECYCLE_LIB="$(dirname "$MT_ADMIN")/mt-admin-lib/04-tenant-lifecycle.sh"
assert_contains "$LIFECYCLE_LIB" 'opencode2-*' 'tenant-name guard reserves the opencode2- prefix'

echo ""
if [[ "$failures" -eq 0 ]]; then
  echo "RESULT: all opencode2 mt-admin tests passed"
else
  echo "RESULT: $failures failure(s)"
  exit 1
fi
