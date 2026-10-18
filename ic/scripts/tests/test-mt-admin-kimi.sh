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
# This fixture's tenant is the invoking account in CI (root). The CI image
# does not include sudo; execute only root-targeted fixture commands directly.
sudo() {
  [[ "${1:-}" == -u && "${2:-}" == root ]] || return 1
  shift 2
  "$@"
}

mkdir -p "$fixture/env" "$fixture/state" "$fixture/home/lunarwing"
# The tenant-side readers/writers require a private env parent (and a private
# env file) owned by the fixture tenant, which is the invoking root account.
chmod 700 "$fixture/env"
env_file="$fixture/env/lunarwing.env"
cat >"$env_file" <<'EOF'
GATEWAY_AUTH_TOKEN=gate%token
KIMI_MODEL=old-model
KIMI_BASE_URL=https://old.example/v1
KIMI_API_KEY=old%key
EOF
chmod 600 "$env_file"

configure_kimi root new-model '' ''
assert_contains "$env_file" 'KIMI_MODEL=new-model' 'configure-kimi updates selected model'
# WI#330: base URLs are written with the production quoting; decode to compare.
if [[ "$(_read_env_value "$env_file" KIMI_BASE_URL)" == 'https://old.example/v1' ]]; then
  pass 'configure-kimi preserves omitted base URL'
else
  fail 'configure-kimi preserves omitted base URL'
fi
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
    opencode_wss) printf '11001\n' ;;
    opencode_health) printf '11002\n' ;;
    opencode2_wss) printf '13001\n' ;;
    opencode2_health) printf '13002\n' ;;
    pebble_wss) printf '14001\n' ;;
    pebble_health) printf '14002\n' ;;
  esac
}
tenant_worker_enabled() { return 0; }
ensure_external_worker_config root kimi kimi_wss
assert_contains "$fixture/state/config.toml" 'timeout_ms = 14400000' \
  'new Kimi tenant defaults to four-hour timeout'
ensure_external_worker_config root opencode opencode_wss
assert_contains "$fixture/state/config.toml" 'timeout_ms = 3600000' \
  'other worker lanes retain one-hour timeout'
assert_contains "$fixture/state/config.toml" 'timeout_ms = 14400000' \
  'repeated provisioning preserves Kimi timeout'
MT_ROOTLESS=true
render_worker_quadlet root kimi 8443
quadlet="$fixture/quadlets/lunarwing-kimi-root.container"
assert_contains "$quadlet" 'UserNS=keep-id:uid=10002,gid=10002' 'systemd Kimi keeps rootless UID mapping'
assert_contains "$quadlet" "EnvironmentFile=$fixture/env/kimi-runtime.env" 'systemd Kimi loads protected runtime env'
# WI#206: the kimi lane must not mount the tenant SSH-agent socket or expose
# SSH_AUTH_SOCK, and must set no-new-privileges.
assert_not_contains "$quadlet" 'ssh-agent.sock' 'systemd Kimi quadlet has no SSH agent mount (WI#206)'
assert_not_contains "$quadlet" 'SSH_AUTH_SOCK' 'systemd Kimi quadlet has no SSH_AUTH_SOCK env (WI#206)'
assert_contains "$quadlet" 'PodmanArgs=--security-opt=no-new-privileges' 'systemd Kimi quadlet sets no-new-privileges (WI#206)'
assert_not_contains "$quadlet" 'SecurityOpt=' 'systemd Kimi quadlet avoids unsupported SecurityOpt key'
# Non-kimi lanes mount only the socket directory (restart-safe), read-only so
# a compromised worker cannot unlink/rebind the host agent socket (WI#226).
for lane in opencode opencode2 pebble; do
  render_worker_quadlet root "$lane" 8443
  worker_quadlet="$fixture/quadlets/lunarwing-${lane}-root.container"
  assert_contains "$worker_quadlet" 'Volume='"$fixture"'/run/ssh-agent:/tmp/ssh-agent:ro,z' \
    "$lane quadlet mounts the ssh-agent directory read-only"
  assert_contains "$worker_quadlet" 'Environment=SSH_AUTH_SOCK=/tmp/ssh-agent/ssh-agent.sock' \
    "$lane quadlet points SSH_AUTH_SOCK inside the mounted directory"
  assert_not_contains "$worker_quadlet" 'Volume='"$fixture"'/run/ssh-agent:/tmp/ssh-agent:z' \
    "$lane quadlet does not mount the agent directory read-write"
done
# Quadlet's full-file hash must also notice the mount policy change.
legacy_quadlet="$fixture/quadlets/legacy-rw.container"
sed 's#ssh-agent:ro,z#ssh-agent:z#' "$worker_quadlet" > "$legacy_quadlet"
_store_container_hash root old-quadlet "$legacy_quadlet"
if _container_config_changed root old-quadlet "$worker_quadlet"; then
  pass 'quadlet legacy RW mount triggers recreation'
else
  fail 'quadlet legacy RW mount survived config check'
fi
render_worker_quadlet root kimi 8443
minted="$(sed -n 's/^WORKER_AUTH_TOKEN=//p' "$env_file" | tail -1)"
if [[ -n "$minted" && "$minted" != "gate%token" ]]; then
  pass 'missing scoped token is minted and persisted (WI#210)'
else
  fail "worker credential was not minted: [$minted]"
fi
assert_contains "$fixture/env/kimi-runtime.env" "AGENT_AUTH_TOKEN=$minted" 'runtime env carries the scoped worker credential'
assert_not_contains "$fixture/env/kimi-runtime.env" 'AGENT_AUTH_TOKEN=gate%token' 'runtime env never carries the gateway bearer'
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
if [[ "$BUILD_CALL" == 'tenant:root false false false true true false false' ]]; then
  pass 'build-tenant forwards Kimi toolchain flag'
else
  fail "build-tenant forwarding wrong: $BUILD_CALL"
fi

build_all() { BUILD_CALL="all:$*"; }
main build-all --with-kimi --with-toolchains
if [[ "$BUILD_CALL" == 'all:false false false true true false' ]]; then
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
# WI#330: add-tenant refuses to run with no LLM endpoint at all, so the
# api-key-file lane passes an explicit one.
main add-tenant root --kimi-api-key-file "$fixture/kimi-api-key" --tensorzero-url http://tz:3030
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
# Pin consistency, version-agnostic: the Dockerfile build-arg default, the CI
# build script, and the committed npm lock must all agree. Deriving the
# expected value instead of hard-coding it means a future version bump cannot
# silently desync one surface from the others (it did: 0.43.1 -> 2.0.2 left
# this assertion behind and red-lit the shell-harnesses job).
KIMI_PKG_VERSION="$(python3 -c "import json,sys; print(json.load(open('$LUNARWING_ROOT/kimicode4lunarwing/kimi-cli/package.json'))['dependencies']['@moonshot-ai/kimi-code'])" 2>/dev/null)"
if [[ -n "$KIMI_PKG_VERSION" ]]; then
  pass "Kimi CLI version is discoverable from package.json ($KIMI_PKG_VERSION)"
else
  fail 'Kimi CLI version could not be read from package.json'
fi
assert_contains "$KIMI_DOCKERFILE" "ARG KIMI_CODE_VERSION=$KIMI_PKG_VERSION" \
  'Kimi image build-arg default matches the committed CLI lock'
assert_contains "$LUNARWING_ROOT/.gitlab/scripts/build-worker-image.sh" "source_ref=$KIMI_PKG_VERSION" \
  'Kimi worker publication source_ref matches the committed CLI lock'
# The tenant build path passes KIMI_CODE_VERSION as a build-arg override rather
# than carrying a default, so assert the flag plumbing, not a literal version.
assert_contains "$BUILD_LIB" '--build-arg KIMI_CODE_VERSION="${kimi_code_version}"' \
  'Kimi tenant builder forwards the version build-arg'
assert_contains "$KIMI_DOCKERFILE" 'COPY kimi-cli/package.json kimi-cli/package-lock.json /opt/kimi-cli/' 'Kimi image copies the reviewed CLI lock'
assert_contains "$KIMI_DOCKERFILE" 'npm ci --omit=dev --prefix /opt/kimi-cli' 'Kimi image installs selected package from its lock'
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

# WI#206: the kimi imperative launch path must not mount the SSH agent socket
# and must set no-new-privileges. Scope the check to the kimi container block:
# from start_tenant_kimi to the next worker section.
KIMI_BLOCK="$(awk '/^start_tenant_kimi\(\)/,/^stop_tenant_kimi\(\)/' "$WORKER_LIB")"
if grep -Fq 'ssh-agent.sock' <<<"$KIMI_BLOCK"; then
  fail 'kimi imperative launch still mounts the SSH agent socket (WI#206)'
else
  pass 'kimi imperative launch has no SSH agent mount (WI#206)'
fi
if grep -Fq -- '--security-opt no-new-privileges' <<<"$KIMI_BLOCK"; then
  pass 'kimi imperative launch sets no-new-privileges (WI#206)'
else
  fail 'kimi imperative launch missing no-new-privileges (WI#206)'
fi
# The non-kimi imperative lanes mount the directory read-only, preserving
# daemon restart recovery while blocking worker-side socket replacement.
for lane in opencode opencode2 pebble; do
  LANE_BLOCK="$(awk "/^start_tenant_${lane}\\(\\)/,/^stop_tenant_${lane}\\(\\)/" "$WORKER_LIB")"
  if grep -Fq -- '-v "${ssh_agent_dir}:/tmp/ssh-agent:ro,z"' <<<"$LANE_BLOCK" \
    && grep -Fq -- 'SSH_AUTH_SOCK=/tmp/ssh-agent/ssh-agent.sock' <<<"$LANE_BLOCK"; then
    pass "$lane imperative launch mounts the ssh-agent directory read-only"
  else
    fail "$lane imperative launch missing the read-only ssh-agent directory mount"
  fi
done
# The launch contract must carry the policy change so existing containers
# force-recreate instead of surviving with the old mount.
ROOT_SCRIPT="$(dirname "$MT_ADMIN")/lunarwing-mt-admin.sh"
assert_contains "$ROOT_SCRIPT" 'KIMI_ROOTLESS_LAUNCH_CONTRACT="userns=${KIMI_USERNS};no-ssh-agent;no-new-privileges"' \
  'kimi launch contract records the WI#206 policy change'
# Contract changes recreate old RW-mount containers on rootless AND rootful
# imperative lanes. Quadlet recreations hash the rendered Volume= line.
assert_contains "$ROOT_SCRIPT" 'OPENCODE_ROOTLESS_LAUNCH_CONTRACT="userns=${OPENCODE_USERNS};ssh-agent-dir-ro"' \
  'opencode rootless launch contract records the read-only SSH mount'
assert_contains "$ROOT_SCRIPT" 'OPENCODE2_ROOTLESS_LAUNCH_CONTRACT="userns=${OPENCODE2_USERNS};ssh-agent-dir-ro"' \
  'opencode2 rootless launch contract records the read-only SSH mount'
assert_contains "$ROOT_SCRIPT" 'PEBBLE_ROOTLESS_LAUNCH_CONTRACT="userns=${PEBBLE_USERNS};ssh-agent-dir-ro"' \
  'pebble launch contract records the WI#400 task-uid userns + read-only SSH mount'
assert_contains "$WORKER_LIB" 'local opencode_launch_contract="ssh-agent-dir-ro"' \
  'rootful opencode includes the SSH mount in its launch contract'
assert_contains "$WORKER_LIB" 'local launch_contract="ssh-agent-dir-ro"' \
  'rootful opencode2 includes the SSH mount in its launch contract'

_store_container_hash root old-rootless "$env_file" "userns=${OPENCODE_USERNS};ssh-agent-dir"
if _container_config_changed root old-rootless "$env_file" "$OPENCODE_ROOTLESS_LAUNCH_CONTRACT"; then
  pass 'rootless legacy RW mount triggers recreation'
else
  fail 'rootless legacy RW mount survived config check'
fi
_store_container_hash root old-rootless "$env_file" "$OPENCODE_ROOTLESS_LAUNCH_CONTRACT"
if _container_config_changed root old-rootless "$env_file" "$OPENCODE_ROOTLESS_LAUNCH_CONTRACT"; then
  fail 'rootless read-only mount triggers unnecessary recreation'
else
  pass 'rootless read-only mount has stable config hash'
fi
_store_container_hash root old-rootful "$env_file"
if _container_config_changed root old-rootful "$env_file" 'ssh-agent-dir-ro'; then
  pass 'rootful legacy RW mount triggers recreation'
else
  fail 'rootful legacy RW mount survived config check'
fi
# The baked worker policy (WI#206): guard back on + PreToolUse hook shipped.
KIMI_CONFIG="$LUNARWING_ROOT/kimicode4lunarwing/config/config.toml"
assert_contains "$KIMI_CONFIG" 'dangerous_command_guard = true' \
  'baked kimi config re-enables the dangerous-command guard'
assert_contains "$KIMI_CONFIG" 'headless_command_guard.mjs' \
  'baked kimi config wires the PreToolUse guard hook'
assert_contains "$LUNARWING_ROOT/kimicode4lunarwing/entrypoint.sh" 'unset KIMI_CODE_DANGEROUS_COMMAND_GUARD' \
  'entrypoint scrubs the guard-disable env var'
if [[ -f "$LUNARWING_ROOT/kimicode4lunarwing/hooks/headless_command_guard.mjs" ]]; then
  pass 'headless command guard hook ships with the worker'
else
  fail 'headless command guard hook missing'
fi

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all Kimi mt-admin tests passed\n'
