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

tenant_env_dir() { printf '%s/env\n' "$fixture"; }
tenant_state_dir() { printf '%s/state\n' "$fixture"; }
tenant_lw_root() { printf '%s/home/lunarwing\n' "$fixture"; }
tenant_home() { printf '%s/home\n' "$fixture"; }
tenant_quadlet_dir() { printf '%s/quadlets\n' "$fixture"; }
tenant_run_dir() { printf '%s/run\n' "$fixture"; }
tenant_exists_in_registry() { return 0; }
_ctr() { return 1; }
chown() { CHOWN_CALL="$*"; }

mkdir -p "$fixture/env" "$fixture/state" "$fixture/home/lunarwing"
env_file="$fixture/env/lunarwing.env"
cat >"$env_file" <<'EOF'
GATEWAY_AUTH_TOKEN=gate%token
KIMI_MODEL=old-model
KIMI_BASE_URL=https://old.example/v1
KIMI_API_KEY=old%key
EOF

configure_kimi root new-model '' ''
assert_contains "$env_file" 'KIMI_MODEL=new-model' 'configure-kimi updates selected model'
assert_contains "$env_file" 'KIMI_BASE_URL=https://old.example/v1' 'configure-kimi preserves omitted base URL'
if [[ "$(_read_env_value "$env_file" KIMI_API_KEY)" == 'old%key' ]]; then
  pass 'configure-kimi preserves omitted API key'
else
  fail 'configure-kimi changed omitted API key'
fi

tenant_worker_enabled() { return 1; }
ports_get() { fail 'unselected Kimi queried port'; return 1; }
rm -f "$fixture/state/config.toml"
ensure_external_worker_config root kimi kimi_wss
if [[ ! -e "$fixture/state/config.toml" ]]; then
  pass 'unselected Kimi emits no daemon config'
else
  fail 'unselected Kimi emitted daemon config'
fi

ports_get() {
  case "$2" in
    kimi_wss) printf '12001\n' ;;
    kimi_health) printf '12002\n' ;;
  esac
}
MT_ROOTLESS=true
render_worker_quadlet root kimi 8443
quadlet="$fixture/quadlets/lunarwing-kimi-root.container"
assert_contains "$quadlet" 'UserNS=keep-id:uid=10002,gid=10002' 'systemd Kimi keeps rootless UID mapping'
assert_contains "$quadlet" "EnvironmentFile=$fixture/env/kimi-runtime.env" 'systemd Kimi loads protected runtime env'
assert_contains "$fixture/env/kimi-runtime.env" 'AGENT_AUTH_TOKEN=gate%token' 'runtime env preserves agent token literally'
assert_contains "$fixture/env/kimi-runtime.env" 'KIMI_API_KEY=old%key' 'runtime env preserves Kimi API key literally'
if [[ "$(stat -c '%a' "$fixture/env/kimi-runtime.env")" == "600" ]]; then
  pass 'Kimi runtime env is private'
else
  fail 'Kimi runtime env mode is not 600'
fi
if ! grep -Fq 'old%key' "$quadlet"; then
  pass 'systemd unit does not inline Kimi API key'
else
  fail 'systemd unit exposes Kimi API key'
fi
if [[ "$(stat -c '%a' "$fixture/home/lunarwing/kimi-workspace")" == "2770" ]]; then
  pass 'rootless Kimi workspace is private and setgid'
else
  fail 'rootless Kimi workspace mode is not 2770'
fi

# Override consumed by sourced workspace preparation.
# shellcheck disable=SC2034
MT_ROOTLESS=false
CHOWN_CALL=''
prepare_worker_workspace root kimi >/dev/null
if [[ "$CHOWN_CALL" == "-R 10002:10002 $fixture/home/lunarwing/kimi-workspace" ]]; then
  pass 'rootful Kimi workspace uses image UID and GID'
else
  fail "rootful Kimi workspace ownership wrong: $CHOWN_CALL"
fi

CHOWN_CALL=''
prepare_worker_workspace root opencode >/dev/null
if [[ "$CHOWN_CALL" == "-R 10001:10001 $fixture/home/lunarwing/opencode-workspace" ]]; then
  pass 'rootful OpenCode workspace uses image UID and GID'
else
  fail "rootful OpenCode workspace ownership wrong: $CHOWN_CALL"
fi

cat >"$fixture/env/legacy.env" <<'EOF'
KIMI_API_KEY='key\\with\'quote'
EOF
if [[ "$(_read_env_value "$fixture/env/legacy.env" KIMI_API_KEY)" == "key\with'quote" ]]; then
  pass 'legacy quoted onboarding value is decoded'
else
  fail 'legacy quoted onboarding value was not decoded'
fi
cat >"$fixture/env/current.env" <<'EOF'
KIMI_API_KEY="key'with\\slashes"
EOF
if [[ "$(_read_env_value "$fixture/env/current.env" KIMI_API_KEY)" == "key'with\slashes" ]]; then
  pass 'systemd-safe quoted value is decoded'
else
  fail 'systemd-safe quoted value was not decoded'
fi

mkdir -p "$fixture/target/kimicode4lunarwing"
# Override consumed by sourced worker builder.
# shellcheck disable=SC2034
CONTAINER_RT=docker
ensure_container_runtime() { :; }
docker() { BUILD_CALL="$*"; }
BUILD_CALL=''
# Function comes from sourced mt-admin later in ShellCheck's composed parse.
# shellcheck disable=SC2218
build_kimi_worker false true 9.9.9 "$fixture/target" >/dev/null
if [[ "$BUILD_CALL" == *'--build-arg WITH_TOOLCHAINS=true'* \
  && "$BUILD_CALL" == *'--build-arg KIMI_CODE_VERSION=9.9.9'* \
  && "$BUILD_CALL" == *"$fixture/target/kimicode4lunarwing" ]]; then
  pass 'Kimi builder forwards profile/version against requested source root'
else
  fail "Kimi builder contract wrong: $BUILD_CALL"
fi

require_root() { :; }
BUILD_CALL=''
build_tenant() { BUILD_CALL="tenant:$*"; }
main build-tenant root --with-kimi --with-toolchains
if [[ "$BUILD_CALL" == 'tenant:root false false false true true false' ]]; then
  pass 'build-tenant forwards Kimi toolchain flag'
else
  fail "build-tenant forwarding wrong: $BUILD_CALL"
fi

build_all() { BUILD_CALL="all:$*"; }
main build-all --with-kimi --with-toolchains
if [[ "$BUILD_CALL" == 'all:false false false true true' ]]; then
  pass 'build-all forwards Kimi toolchain flag'
else
  fail "build-all forwarding wrong: $BUILD_CALL"
fi

build_kimi_worker() { BUILD_CALL="kimi:$*"; }
main build-kimi-worker --with-toolchains --kimi-code-version 9.9.9
if [[ "$BUILD_CALL" == 'kimi:false true 9.9.9' ]]; then
  pass 'build-kimi-worker forwards toolchain and version flags'
else
  fail "build-kimi-worker forwarding wrong: $BUILD_CALL"
fi

printf 'file-only-secret\n' >"$fixture/kimi-api-key"
chmod 600 "$fixture/kimi-api-key"
CAPTURED_KIMI_KEY=''
add_tenant() { CAPTURED_KIMI_KEY="${22:-}"; }
with_tenant_lifecycle_lock() { shift; "$@"; }
main add-tenant root --kimi-api-key-file "$fixture/kimi-api-key"
if [[ "$CAPTURED_KIMI_KEY" == 'file-only-secret' ]]; then
  pass 'add-tenant reads Kimi API key from file'
else
  fail 'add-tenant did not read Kimi API key from file'
fi

# These builder contracts live in mt-admin-lib/05-tenant-build.sh after the
# mt-admin library split (the root dispatcher sources the library). Derive the
# path from MT_ADMIN, not SCRIPT_DIR: sourcing "$MT_ADMIN" overwrites
# SCRIPT_DIR with ic/scripts/.
BUILD_LIB="$(dirname "$MT_ADMIN")/mt-admin-lib/05-tenant-build.sh"
assert_contains "$BUILD_LIB" 'local source_root="${4:-$LUNARWING_ROOT}"' 'Kimi builder accepts source-root contract'
assert_contains "$BUILD_LIB" '"$(tenant_lw_root "$name")"' 'tenant build passes tenant checkout to Kimi builder'
assert_contains "$BUILD_LIB" 'local source_root="${3:-$LUNARWING_ROOT}"' 'OpenCode builder accepts source-root contract'
assert_contains "$BUILD_LIB" 'build_opencode_worker "false" "$with_toolchains" "$(tenant_lw_root "$name")"' 'tenant build passes tenant checkout to OpenCode builder'
assert_contains "$BUILD_LIB" 'build-all refuses dirty worker source in tenant checkout' 'build-all rejects mismatched dirty worker trees'
KIMI_DOCKERFILE="$LUNARWING_ROOT/kimicode4lunarwing/Dockerfile"
assert_contains "$KIMI_DOCKERFILE" 'ARG KIMI_CODE_VERSION=0.37.2' 'Kimi image pins build-arg default'
assert_contains "$KIMI_DOCKERFILE" 'npm install -g @moonshot-ai/kimi-code@${KIMI_CODE_VERSION}' 'Kimi image installs selected package version'
assert_contains "$KIMI_DOCKERFILE" 'org.opencontainers.image.toolchain-profile="${WITH_TOOLCHAINS}"' 'Kimi image records toolchain profile'
assert_contains "$KIMI_DOCKERFILE" 'libz-dev' 'Kimi toolchain packages match OpenCode zlib development support'

help_output="$(usage)"
if [[ "$(grep -c -- '--with-toolchains' <<<"$help_output")" -ge 4 ]]; then
  pass 'help documents toolchains for aggregate and worker builds'
else
  fail 'help omits toolchain support'
fi
if [[ "$help_output" == *'--kimi-api-key <key>'* || "$help_output" == *'--api-key <key>'* ]]; then
  fail 'help still recommends passing Kimi API key through argv'
else
  pass 'help documents file-based Kimi API key input only'
fi
# Worker container argv is assembled in mt-admin-lib/09-worker-config.sh
# (BUILD_LIB above points at 05-tenant-build.sh).
WORKER_LIB="$(dirname "$MT_ADMIN")/mt-admin-lib/09-worker-config.sh"
if grep -Fq 'env_flags+=(-e "KIMI_API_KEY=' "$WORKER_LIB"; then
  fail 'Kimi API key still enters container runtime argv'
else
  pass 'Kimi API key stays out of container runtime argv'
fi

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all Kimi mt-admin tests passed\n'
