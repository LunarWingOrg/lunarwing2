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
chown() {
  CHOWN_CALL="$*"
  # Preserve real --reference ownership updates in the env-file fixture;
  # workspace chowns are still intercepted below.
  [[ "${1:-}" != --reference=* ]] || command chown "$@"
}
# CI containers run with no-new-privileges, so real sudo dies mid-harness;
# strip the `-u <tenant>` prefix and run the wrapped command directly.
sudo() {
  if [[ "${1:-}" == "-u" ]]; then shift 2; fi
  command "$@"
}
say() { :; }
sanitize_name() { printf '%s' "$1"; }

mkdir -p "$fixture/env" "$fixture/state" "$fixture/home/lunarwing" "$fixture/quadlets"
chmod 700 "$fixture/env"
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
# WI#330: base URLs are written with the production quoting; decode to compare.
if [[ "$(_read_env_value "$env_file" OPENCODE2_BASE_URL)" == 'https://old.example/v1' ]]; then
  pass 'configure-opencode2 preserves omitted base URL'
else
  fail 'configure-opencode2 preserves omitted base URL'
fi
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
minted="$(sed -n 's/^WORKER_AUTH_TOKEN=//p' "$env_file" | tail -1)"
if [[ -n "$minted" && "$minted" != "gate%token" ]]; then
  pass 'missing scoped token is minted and persisted (WI#210)'
else
  fail "worker credential was not minted: [$minted]"
fi
assert_contains "$fixture/state/config.toml" "auth_token = \"$minted\"" 'daemon config carries the scoped worker credential'
assert_not_contains "$fixture/state/config.toml" 'auth_token = "gate%token"' 'daemon config never carries the gateway bearer'
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
assert_contains "$quadlet" "Environment=AGENT_AUTH_TOKEN=${minted//%/%%}" 'quadlet inlines the scoped worker credential'
assert_not_contains "$quadlet" 'Environment=AGENT_AUTH_TOKEN=gate%%token' 'quadlet never inlines the gateway bearer'
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

# Missing scoped token (WI#210): resolve_worker_auth_token must mint and
# persist a scoped WORKER_AUTH_TOKEN rather than silently substituting the
# gateway bearer, so the upgrade path never re-exposes the gateway credential.
cat >"$env_file" <<'EOF'
GATEWAY_AUTH_TOKEN=gate%token
LLM_API_KEY=llm%key
OPENCODE2_MODEL=new-model
OPENCODE2_BASE_URL=https://old.example/v1
EOF
# The fixture user owns the private env directory, as the deployed tenant does.
# The tenant-side atomic writer must preserve ownership and 0600 mode.
command chmod 600 "$env_file"
owner_before="$(stat -c '%u:%g' "$env_file")"
render_worker_quadlet root opencode2 8443
assert_contains "$env_file" 'WORKER_AUTH_TOKEN=' \
  'upgrade path persists a scoped WORKER_AUTH_TOKEN'
if [[ "$(stat -c '%u:%g:%a' "$env_file")" == "$owner_before:600" ]]; then
  pass 'scoped-token backfill preserves tenant env ownership and 0600 mode'
else
  fail 'scoped-token backfill changed tenant env ownership or mode'
fi
minted="$(sed -n 's/^WORKER_AUTH_TOKEN=//p' "$env_file" | tail -1)"
if [[ -n "$minted" && "$minted" != "gate%token" ]]; then
  pass 'minted worker credential differs from the gateway bearer'
else
  fail "minted worker credential fell back to the gateway bearer ($minted)"
fi
assert_contains "$scoped_quadlet" "Environment=AGENT_AUTH_TOKEN=${minted//%/%%}" \
  'quadlet uses the freshly minted scoped credential'
assert_not_contains "$scoped_quadlet" 'gate%%token' \
  'quadlet carries no gateway bearer on the upgrade path'

# ── workspace ownership ──────────────────────────────────────────────────────
# Override consumed by sourced workspace preparation.
# shellcheck disable=SC2034
MT_ROOTLESS=false
CHOWN_CALL=''
prepare_worker_workspace root opencode2 >/dev/null
if [[ "$CHOWN_CALL" == "--no-dereference 10003:10003 ." ]]; then
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
mkdir -p "$fixture/target/opencode2.4lunarwing/scripts"
wrapper_file="$fixture/target/opencode2.4lunarwing/scripts/opencode-exposure-guard.sh"
printf '#!/bin/sh\n# fixture wrapper\n' > "$wrapper_file"
# Override consumed by the sourced container helpers.
# shellcheck disable=SC2034
CONTAINER_RT=docker
ensure_container_runtime() { :; }
# WI#425 out-of-band verify mock: the fake image serves the wrapper and the
# real binary from a fixture root, so the verify path can pass and — when
# the fixture bytes are tampered — fail exactly like the real runtime would.
oob_image_root="$fixture/oob-image"
oob_env_extra=()
oob_shell_override=''
oob_real_binary_missing=false
oob_serve_wrapper_file="$wrapper_file"
mkdir -p "$oob_image_root/usr/local/bin" "$oob_image_root/usr/local/lib/opencode2"
cp "$wrapper_file" "$oob_image_root/usr/local/bin/opencode"
printf '\x7fELF clean pinned build\n' > "$oob_image_root/usr/local/lib/opencode2/opencode"
docker() {
  case "${1:-}" in
    build) BUILD_CALL="$*"; printf '%s\n' "$*" >> "$fixture/build-calls" ;;
    create) printf 'fake-container-id\n' ;;
    cp)
      local src="$2" dst="$3" rel
      rel="${src#fake-container-id:}"
      if [[ "$rel" == "$src" ]]; then
        return 1
      fi
      # The fake image serves the CURRENT served-wrapper file (normally the
      # checkout wrapper, so the GW-1 rewrite loop and untampered builds
      # pass); the WI#425 refusal tests point it at tampered bytes.
      if [[ "$rel" == /usr/local/bin/opencode ]]; then
        cp "$oob_serve_wrapper_file" "$dst"
      elif [[ "$rel" == /usr/local/lib/opencode2/opencode ]]; then
        # server-side absence is simulated by the flag, not by the fixture
        # file: the fixture keeps a real file so untampered passes serve it.
        if [[ "$oob_real_binary_missing" == true ]]; then
          return 1
        fi
        cp "$oob_image_root/$rel" "$dst"
      elif [[ -f "$oob_image_root/$rel" ]]; then
        cp "$oob_image_root/$rel" "$dst"
      else
        return 1
      fi ;;
    rm | rmi) return 0 ;;
    tag) OOB_TAG_CALLS+=("$2 -> $3") ;;
    image)
      case "${2:-}" in
        inspect)
          local fmt="${*: -1}" out
          case "$fmt" in
            '{{json .Config.Env}}')
              out='["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin","OPENCODE_DISABLE_AUTOUPDATE=1"'
              local kv
              for kv in "${oob_env_extra[@]}"; do out+=",\"$kv\""; done
              out+=']'
              printf '%s\n' "$out" ;;
            '{{.Config.Shell}}')
              if [[ -n "$oob_shell_override" ]]; then
                printf '%s\n' "$oob_shell_override"
              else
                printf '<nil>\n'
              fi ;;
            *) return 1 ;;
          esac ;;
        *) return 1 ;;
      esac ;;
    *) BUILD_CALL="$*"; printf '%s\n' "$*" >> "$fixture/build-calls" ;;
  esac
}
podman() { docker "$@"; }
OOB_TAG_CALLS=()
BUILD_CALL=''
build_opencode2_worker false true "$fixture/target" >/dev/null
if [[ "$BUILD_CALL" == *'--build-arg WITH_TOOLCHAINS=true'* \
  && "$BUILD_CALL" == *'-t lunarwing-worker-opencode2:candidate'* \
  && "$BUILD_CALL" == *"$fixture/target/opencode2.4lunarwing"* ]]; then
  pass 'opencode2 builder forwards toolchain flag and source root (candidate tag)'
else
  fail "opencode2 builder contract wrong: $BUILD_CALL"
fi
# WI#425: the out-of-band verification gates the :latest re-tag.
if [[ "${OOB_TAG_CALLS[0]}" == 'lunarwing-worker-opencode2:candidate -> lunarwing-worker-opencode2:latest' ]]; then
  pass 'opencode2 builder re-tags :latest only after the out-of-band verify'
else
  fail "opencode2 builder did not gate the :latest re-tag on the out-of-band verify: ${OOB_TAG_CALLS[*]:-}"
fi

BUILD_CALL=''
build_opencode2_worker true false "$fixture/target" >/dev/null
if [[ "$BUILD_CALL" == *'--no-cache'* ]]; then
  pass 'opencode2 builder honors --no-cache'
else
  fail 'opencode2 builder ignored --no-cache'
fi

# GW-1: bind both runtime lanes to the selected checkout's wrapper bytes.
for CONTAINER_RT in docker podman; do
  for revision in first second; do
    printf '#!/bin/sh\n# %s wrapper\n' "$revision" > "$wrapper_file"
    expected_anchor=$(sha256sum < "$wrapper_file")
    expected_anchor=${expected_anchor%% *}
    BUILD_CALL=''
    build_opencode2_worker false false "$fixture/target" >/dev/null
    if [[ "$BUILD_CALL" == *"--build-arg LUNARWING_WRAPPER_SHA256=$expected_anchor"* ]]; then
      pass "$CONTAINER_RT builder forwards the $revision checkout wrapper digest"
    else
      fail "$CONTAINER_RT builder lost the $revision wrapper digest: $BUILD_CALL"
    fi
  done

  mv "$wrapper_file" "$wrapper_file.saved"
  : > "$fixture/build-calls"
  if (build_opencode2_worker false false "$fixture/target") >"$fixture/missing-wrapper.log" 2>&1; then
    fail "$CONTAINER_RT builder accepted a missing wrapper"
  else
    assert_contains "$fixture/missing-wrapper.log" 'cannot hash' \
      "$CONTAINER_RT builder explains a missing wrapper"
  fi
  if [[ ! -s "$fixture/build-calls" ]]; then
    pass "$CONTAINER_RT builder refuses a missing wrapper before invoking the runtime"
  else
    fail "$CONTAINER_RT builder invoked the runtime without a wrapper digest"
  fi
  mv "$wrapper_file.saved" "$wrapper_file"
done

for bad_digest in '' short "$(printf '%064d' 0)g" "$(printf '%064d' 0 | tr 0 g)"; do
  : > "$fixture/build-calls"
  if (
    sha256sum() { printf '%s  -\n' "$bad_digest"; }
    build_opencode2_worker false false "$fixture/target"
  ) >"$fixture/invalid-digest.log" 2>&1; then
    fail 'opencode2 builder accepted an invalid wrapper digest'
  else
    assert_contains "$fixture/invalid-digest.log" 'invalid wrapper sha256' \
      'opencode2 builder rejects an invalid wrapper digest'
  fi
  if [[ -s "$fixture/build-calls" ]]; then
    fail 'opencode2 builder invoked the runtime with an invalid wrapper digest'
  fi
done

: > "$fixture/build-calls"
if (
  sha256sum() { printf '%064d  -\n' 0; return 1; }
  build_opencode2_worker false false "$fixture/target"
) >"$fixture/hash-failure.log" 2>&1; then
  fail 'opencode2 builder accepted a failed hash command'
else
  assert_contains "$fixture/hash-failure.log" 'cannot hash' \
    'opencode2 builder explains a failed hash command even with valid-looking output'
fi
if [[ -s "$fixture/build-calls" ]]; then
  fail 'opencode2 builder invoked the runtime after a failed hash command'
fi

# Missing worker dir dies instead of building from the wrong context.
BUILD_CALL=''
if (build_opencode2_worker false false "$fixture/missing") >/dev/null 2>&1; then
  fail 'opencode2 builder accepted a missing worker dir'
else
  pass 'opencode2 builder refuses a missing worker dir'
fi

# ── WI#425 out-of-band verification refusals ─────────────────────────────────
# Each tampered shape must FAIL the build verify (die) and never re-tag
# :latest — the good previous image stays in place.
oob_refusal_run() {
  local log_file="$1" label="$2"
  OOB_TAG_CALLS=()
  if (build_opencode2_worker false false "$fixture/target") >"$log_file" 2>&1; then
    fail "out-of-band verify accepted $label"
    return 1
  fi
  if [[ "${#OOB_TAG_CALLS[@]}" -ne 0 ]]; then
    fail "out-of-band verify re-tagged :latest on $label refusal"
    return 1
  fi
  pass "out-of-band verify refuses $label and keeps :latest untouched"
  return 0
}

# Tampered installed wrapper: the image serves different bytes than the
# checkout (the run-103 GD-2 shape, now caught host-side too). The mock's
# served wrapper comes from oob_serve_wrapper_file, which the untampered
# runs keep pointed at the live checkout wrapper.
oob_serve_wrapper_file="$wrapper_file"
printf '#!/bin/sh\n# tampered wrapper\nexec /bin/sh\n' > "$fixture/tampered-wrapper"
oob_serve_wrapper_file="$fixture/tampered-wrapper"
oob_refusal_run "$fixture/oob-tampered.log" 'a tampered installed wrapper'
assert_contains "$fixture/oob-tampered.log" 'WI#425 out-of-band' \
  'tampered-wrapper refusal names the out-of-band check'
oob_serve_wrapper_file="$wrapper_file"

# Drifted image Env: the config retargets OPENCODE_BIN at the real binary —
# the GX-2 shape caught host-side too.
oob_env_extra=("OPENCODE_BIN=/usr/local/lib/opencode2/opencode")
oob_refusal_run "$fixture/oob-env.log" 'a drifted image Env (OPENCODE_BIN retarget)'
assert_contains "$fixture/oob-env.log" 'retargets OPENCODE_BIN' \
  'drifted-Env refusal names the retarget'
oob_env_extra=()

# Missing real binary in the image.
oob_real_binary_missing=true
oob_refusal_run "$fixture/oob-binary.log" 'a missing real binary'
assert_contains "$fixture/oob-binary.log" 'missing from' \
  'missing-binary refusal names the path'
oob_real_binary_missing=false

# Baked Shell override (the Dockerfile refuses SHELL; a config carrying one
# is drift).
oob_shell_override='[/bin/false,-c]'
oob_refusal_run "$fixture/oob-shell.log" 'a baked Shell override'
assert_contains "$fixture/oob-shell.log" 'Shell override' \
  'Shell-override refusal names the drift'
oob_shell_override=''

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

# ── run-166 LF-2: a resolver refusal must abort, not render tokenless ────────
# A lunarwing.env shape the tenant-side reader rejects (0640 — a restore or a
# hand edit) made the !1771 head render the unit/config with AGENT_AUTH_TOKEN
# absent: the resolver's die only exits the $(...) subshell, and production
# runs the render under the lifecycle lock's suppressed errexit. Model that
# wrapper (set +e) and require the refusal to be fatal at every call site.
chmod 0640 "$env_file"
rm -f "$fixture/quadlets/lunarwing-opencode2-root.container"
lf2_rc=0
( set +e; render_worker_quadlet root opencode2 8443 ) >/dev/null 2>&1 || lf2_rc=$?
if [[ "$lf2_rc" -eq 0 ]]; then
  fail 'quadlet render continued past a refused worker credential'
else
  pass 'quadlet render aborts on a refused worker credential'
fi
if [[ -e "$fixture/quadlets/lunarwing-opencode2-root.container" ]]; then
  fail 'quadlet was written without the scoped worker credential'
else
  pass 'no tokenless quadlet is left behind'
fi

rm -f "$fixture/state/config.toml"
lf2_rc=0
( set +e; ensure_external_worker_config root opencode2 opencode2_wss ) >/dev/null 2>&1 || lf2_rc=$?
if [[ "$lf2_rc" -eq 0 ]]; then
  fail 'fresh worker config continued past a refused worker credential'
else
  pass 'fresh worker config aborts on a refused worker credential'
fi
if [[ -e "$fixture/state/config.toml" ]]; then
  fail 'daemon config was written without the scoped worker credential'
else
  pass 'no tokenless daemon config is left behind'
fi
chmod 0600 "$env_file"

echo ""
if [[ "$failures" -eq 0 ]]; then
  echo "RESULT: all opencode2 mt-admin tests passed"
else
  echo "RESULT: $failures failure(s)"
  exit 1
fi
