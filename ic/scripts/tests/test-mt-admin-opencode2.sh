#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HARNESS_REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
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
# The image-config policy drops to nobody via setpriv (run-221 OK-1). The
# harness cannot change uid: record the call to a file (a subshell-safe
# record) and run the wrapped command as the invoking user.
setpriv() {
  printf '%s\n' "$*" >> "$fixture/setpriv-calls"
  while [[ $# -gt 0 && "$1" != -- ]]; do shift; done
  shift
  local previous='' argument
  for argument in "$@"; do
    if [[ "$previous" == --archive ]]; then
      printf '%s %s\n' "$(dirname "$argument")" "$(stat -c '%a' "$(dirname "$argument")")" \
        >> "$fixture/oob-input-locations"
    fi
    previous="$argument"
  done
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
mkdir -p "$fixture/target/opencode2.4lunarwing" "$fixture/target/.gitlab/scripts"
cp -a "$HARNESS_REPO_ROOT/opencode2.4lunarwing/scripts" "$fixture/target/opencode2.4lunarwing/"
cp -a "$HARNESS_REPO_ROOT/opencode2.4lunarwing/config" "$fixture/target/opencode2.4lunarwing/"
cp -a "$HARNESS_REPO_ROOT/opencode2.4lunarwing/Dockerfile" \
  "$HARNESS_REPO_ROOT/opencode2.4lunarwing/entrypoint.sh" \
  "$HARNESS_REPO_ROOT/opencode2.4lunarwing/health_server.py" \
  "$HARNESS_REPO_ROOT/opencode2.4lunarwing/agent_comm_protocol.json" \
  "$fixture/target/opencode2.4lunarwing/"
cp -a "$HARNESS_REPO_ROOT/.gitlab/scripts/verify-opencode2-exposure-out-of-band.py" \
  "$fixture/target/.gitlab/scripts/"
wrapper_file="$fixture/target/opencode2.4lunarwing/scripts/opencode-exposure-guard.sh"
chmod 755 "$wrapper_file"
# Override consumed by the sourced container helpers.
# shellcheck disable=SC2034
CONTAINER_RT=docker
ensure_container_runtime() { :; }
oob_image_root="$fixture/oob-image"
oob_env_extra=()
oob_shell_override=''
oob_drop_env=''
oob_user='10003:10003'
oob_config_inspect_failure=false
oob_export_failure=false
oob_empty_cid=false
oob_wrapper_symlink=false
oob_wrapper_mode=''
oob_wrapper_uid=0
oob_tamper_app=false
oob_real_binary_missing=false
oob_path_shadow=''
oob_serve_wrapper_file="$wrapper_file"
oob_prepare_image() {
  rm -rf "$oob_image_root"
  mkdir -p "$oob_image_root/app/node_modules" "$oob_image_root/usr/bin" \
    "$oob_image_root/usr/local/bin" "$oob_image_root/usr/local/lib/opencode2"
  cp -a "$fixture/target/opencode2.4lunarwing/scripts" "$oob_image_root/app/scripts"
  cp -a "$fixture/target/opencode2.4lunarwing/config" "$oob_image_root/app/config"
  cp -a "$fixture/target/opencode2.4lunarwing/entrypoint.sh" \
    "$fixture/target/opencode2.4lunarwing/health_server.py" \
    "$fixture/target/opencode2.4lunarwing/agent_comm_protocol.json" \
    "$oob_image_root/app/"
  if [[ "$oob_tamper_app" == true ]]; then
    printf '# tampered in image\n' >> "$oob_image_root/app/scripts/opencode-exposure-guard.sh"
  fi
  if [[ "$oob_wrapper_symlink" == true ]]; then
    ln -s "$oob_serve_wrapper_file" "$oob_image_root/usr/local/bin/opencode"
  else
    cp "$oob_serve_wrapper_file" "$oob_image_root/usr/local/bin/opencode"
    chmod "${oob_wrapper_mode:-755}" "$oob_image_root/usr/local/bin/opencode"
  fi
  if [[ "$oob_real_binary_missing" != true ]]; then
    printf '\x7fELF clean pinned build\n' > "$oob_image_root/usr/local/lib/opencode2/opencode"
    chmod 755 "$oob_image_root/usr/local/lib/opencode2/opencode"
  fi
  printf '\x7fELF pinned tini\n' > "$oob_image_root/usr/bin/tini"
  chmod 755 "$oob_image_root/usr/bin/tini"
  if [[ -n "$oob_path_shadow" ]]; then
    mkdir -p "$oob_image_root/$(dirname "$oob_path_shadow")"
    cp "$oob_image_root/usr/local/lib/opencode2/opencode" "$oob_image_root/$oob_path_shadow"
    chmod 755 "$oob_image_root/$oob_path_shadow"
  fi
}
fake_container_runtime() {
  local runtime="$1"
  shift
  case "${1:-}" in
    build)
      BUILD_CALL="$*"
      printf '%s\n' "$*" >> "$fixture/build-calls"
      local previous='' argument
      for argument in "$@"; do
        if [[ "$previous" == --iidfile ]]; then
          printf 'sha256:%064d\n' 0 > "$argument"
        fi
        previous="$argument"
      done ;;
    create)
      printf '%s\n' "$*" >> "$fixture/create-calls"
      [[ "$oob_empty_cid" == true ]] || printf 'fakecontainerid\n' ;;
    export)
      [[ "$oob_export_failure" != true ]] || return 1
      [[ "${2:-}" == --output && "${4:-}" == fakecontainerid ]] || return 1
      oob_prepare_image
      python3 - "$oob_image_root" "$3" "$oob_wrapper_uid" <<'PY'
import pathlib
import sys
import tarfile
root, output, wrapper_uid = pathlib.Path(sys.argv[1]), sys.argv[2], int(sys.argv[3])
with tarfile.open(output, "w") as archive:
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        info = archive.gettarinfo(str(path), arcname=relative)
        info.uid = wrapper_uid if relative == "usr/local/bin/opencode" else 0
        info.gid = 0
        if relative.startswith("app/") and not path.is_symlink() and path.is_file():
            info.mode = 0o444
        if path.is_dir() and (
            relative in {"app/scripts", "app/config"}
            or relative.startswith(("app/scripts/", "app/config/"))
        ):
            info.mode = 0o555
        if path.is_symlink() or path.is_dir():
            archive.addfile(info)
        elif path.is_file():
            with path.open("rb") as source:
                archive.addfile(info, source)
        else:
            raise SystemExit(f"unsupported fixture entry: {path}")
PY
      ;;
    rm | rmi) return 0 ;;
    tag) printf '%s\n' "$2 -> $3" >> "$fixture/tag-calls" ;;
    image)
      [[ "$oob_config_inspect_failure" != true ]] || return 1
      [[ "${2:-}" == inspect && $# -eq 3 ]] || return 1
      python3 - "$runtime" "$oob_shell_override" "$oob_drop_env" "$oob_user" "${oob_env_extra[@]}" <<'PY'
import json
import sys
runtime, shell, drop, user, *extra = sys.argv[1:]
env = [
    "PATH=/home/opencode2/.cargo/bin:/usr/local/cargo/bin:/usr/local/go/bin:/home/opencode2/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/local/bun-node-fallback-bin",
    "BUN_RUNTIME_TRANSPILER_CACHE_PATH=0",
    "BUN_INSTALL_BIN=/usr/local/bin",
    "OPENCODE_IMAGE_REF=v2.1.2@" + "a" * 40,
    "OPENCODE_DISABLE_AUTOUPDATE=1",
    "RUSTUP_HOME=/usr/local/rustup",
    "GOROOT=/usr/local/go",
    "GOPATH=/home/opencode2/go",
    "CARGO_HOME=/home/opencode2/.cargo",
] + extra
if drop:
    env = [entry for entry in env if not entry.startswith(drop + "=")]
config = {
    "Env": env,
    "Entrypoint": ["/usr/bin/tini", "--", "/app/entrypoint.sh"],
    "Cmd": ["--mode", "websocket"],
    "User": user,
    "WorkingDir": "/app",
    "ExposedPorts": {"8443/tcp": {}, "9090/tcp": {}},
    "StopSignal": "SIGTERM",
    "Shell": json.loads(shell) if shell else None,
}
healthcheck = {
    "Test": ["CMD-SHELL", "curl -sf http://127.0.0.1:8443/health || exit 1"],
    "Interval": 30000000000,
    "Timeout": 5000000000,
    "StartPeriod": 30000000000,
    "Retries": 3,
}
if runtime == "podman":
    print(json.dumps([{"Config": config, "Healthcheck": healthcheck}]))
else:
    config["Healthcheck"] = healthcheck
    print(json.dumps([{"Config": config}]))
PY
      ;;
    *) BUILD_CALL="$*"; printf '%s\n' "$*" >> "$fixture/build-calls" ;;
  esac
}
docker() { fake_container_runtime docker "$@"; }
podman() { fake_container_runtime podman "$@"; }
: > "$fixture/tag-calls"
BUILD_CALL=''
build_opencode2_worker false true "$fixture/target" >/dev/null
if [[ "$BUILD_CALL" == *'--build-arg WITH_TOOLCHAINS=true'* \
  && "$BUILD_CALL" == *'-t lunarwing-worker-opencode2:candidate'* \
  && "$BUILD_CALL" == *'--iidfile '* \
  && "$BUILD_CALL" == *"$fixture/target/opencode2.4lunarwing"* ]]; then
  pass 'opencode2 builder forwards toolchain flag, source root, and immutable-ID output'
else
  fail "opencode2 builder contract wrong: $BUILD_CALL"
fi
# WI#425: the out-of-band verification gates the :latest re-tag by image ID.
expected_image_id="sha256:$(printf '%064d' 0)"
if [[ "$(head -n1 "$fixture/tag-calls")" == "$expected_image_id -> lunarwing-worker-opencode2:latest" ]]; then
  pass 'opencode2 builder re-tags :latest only after verifying the immutable image ID'
else
  fail "opencode2 builder did not gate the :latest re-tag on the verified ID: $(cat "$fixture/tag-calls")"
fi

read -r input_parent input_mode < <(head -n1 "$fixture/oob-input-locations")
if [[ "$input_parent" == /var/tmp/lw-oob.*/inputs && "$input_mode" == 700 ]]; then
  pass 'archive and config handoff uses a private /var/tmp input directory'
else
  fail "out-of-band input directory is wrong: $input_parent mode $input_mode"
fi

# run-221 OK-1: root never executes checkout Python. A planted scripts/json.py
# must not be imported by the shared verifier's isolated Python mode.
planted_marker="$fixture/planted-json-ran"
cat > "$fixture/target/opencode2.4lunarwing/scripts/json.py" <<EOF
import pathlib
pathlib.Path("$planted_marker").write_text("ran")
EOF
: > "$fixture/setpriv-calls"
if (build_opencode2_worker false false "$fixture/target") >"$fixture/oob-planted.log" 2>&1 \
   && [[ ! -e "$planted_marker" ]]; then
  pass 'shared verifier ignores a json.py planted next to the checkout policy'
else
  fail "planted checkout json.py ran or broke the verify: $(cat "$fixture/oob-planted.log")"
fi
rm -f "$fixture/target/opencode2.4lunarwing/scripts/json.py" "$planted_marker"
nobody_uid="$(id -u nobody)"
setpriv_call="$(head -n1 "$fixture/setpriv-calls")"
if [[ "$setpriv_call" == "--reuid=$nobody_uid --regid=$(id -g nobody) --clear-groups --no-new-privs -- env -i PATH=/usr/bin:/bin PYTHONDONTWRITEBYTECODE=1 "* \
   && "$setpriv_call" == *" -I "*"/checkout/.gitlab/scripts/verify-opencode2-exposure-out-of-band.py --archive "* \
   && "$setpriv_call" == *" --config "*" --workspace-dir "* \
   && "$setpriv_call" != *"$fixture/target"* ]]; then
  pass 'shared tar/config verifier runs as nobody from an isolated root-owned snapshot'
else
  fail "shared verifier privilege drop wrong: $setpriv_call"
fi

BUILD_CALL=''
build_opencode2_worker true false "$fixture/target" >/dev/null
if [[ "$BUILD_CALL" == *'--no-cache'* ]]; then
  pass 'opencode2 builder honors --no-cache'
else
  fail 'opencode2 builder ignored --no-cache'
fi

# GW-1: bind both runtime lanes to the selected checkout's wrapper bytes.
cp "$wrapper_file" "$wrapper_file.base"
for CONTAINER_RT in docker podman; do
  for revision in first second; do
    cp "$wrapper_file.base" "$wrapper_file"
    printf '# %s wrapper digest variant\n' "$revision" >> "$wrapper_file"
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
mv "$wrapper_file.base" "$wrapper_file"

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
  : > "$fixture/tag-calls"
  if (build_opencode2_worker false false "$fixture/target") >"$log_file" 2>&1; then
    fail "out-of-band verify accepted $label"
    return 1
  fi
  if [[ -s "$fixture/tag-calls" ]]; then
    fail "out-of-band verify re-tagged :latest on $label refusal"
    return 1
  fi
  pass "out-of-band verify refuses $label and keeps :latest untouched"
  return 0
}

assert_contains "$fixture/create-calls" '--pull=never' \
  'out-of-band create never pulls or resolves a moving tag'

printf '#!/bin/sh\n# tampered wrapper\nexec /bin/sh\n' > "$fixture/tampered-wrapper"
chmod 755 "$fixture/tampered-wrapper"
oob_serve_wrapper_file="$fixture/tampered-wrapper"
oob_refusal_run "$fixture/oob-tampered.log" 'a tampered installed wrapper'
assert_contains "$fixture/oob-tampered.log" 'not byte-identical' \
  'tampered-wrapper refusal names the checkout comparison'
oob_serve_wrapper_file="$wrapper_file"

for retarget in OPENCODE_BIN LUNARWING_REAL_OPENCODE; do
  oob_env_extra=("$retarget=/usr/local/lib/opencode2/opencode")
  oob_refusal_run "$fixture/oob-env.log" "a drifted image Env ($retarget retarget)"
  assert_contains "$fixture/oob-env.log" "retargets $retarget" \
    "drifted-Env refusal names $retarget"
done
oob_env_extra=()

oob_drop_env=OPENCODE_DISABLE_AUTOUPDATE
oob_refusal_run "$fixture/oob-kill-switch.log" 'a missing autoupdate kill switch'
assert_contains "$fixture/oob-kill-switch.log" 'OPENCODE_DISABLE_AUTOUPDATE' \
  'missing kill switch is refused by the shared full-config policy'
oob_drop_env=''

oob_config_inspect_failure=true
oob_refusal_run "$fixture/oob-inspect-failure.log" 'an image-config inspect failure'
assert_contains "$fixture/oob-inspect-failure.log" 'cannot inspect full image config' \
  'image-config inspect failure is never tolerated'
oob_config_inspect_failure=false

oob_export_failure=true
oob_refusal_run "$fixture/oob-export-failure.log" 'an image filesystem export failure'
assert_contains "$fixture/oob-export-failure.log" 'cannot export the opencode2 image filesystem' \
  'filesystem export failure is never tolerated'
oob_export_failure=false

oob_wrapper_symlink=true
oob_refusal_run "$fixture/oob-wrapper-link.log" 'a symlinked installed wrapper'
assert_contains "$fixture/oob-wrapper-link.log" 'must be a regular file, not a link' \
  'symlinked wrapper is refused from its tar type'
oob_wrapper_symlink=false

oob_wrapper_mode=744
oob_refusal_run "$fixture/oob-wrapper-mode.log" 'an owner-only-executable wrapper'
assert_contains "$fixture/oob-wrapper-mode.log" 'GX-3' \
  'wrapper with the wrong mode is refused'
oob_wrapper_mode=''

oob_wrapper_uid=10003
oob_refusal_run "$fixture/oob-wrapper-owner.log" 'an agent-owned installed wrapper'
assert_contains "$fixture/oob-wrapper-owner.log" 'owned by uid 0' \
  'mt-admin preserves CI tar-header ownership enforcement'
oob_wrapper_uid=0

oob_path_shadow='usr/local/sbin/opencode'
oob_refusal_run "$fixture/oob-path-shadow.log" 'an opencode earlier on PATH'
assert_contains "$fixture/oob-path-shadow.log" 'PATH shadow' \
  'mt-admin preserves CI PATH-shadow enforcement'
oob_path_shadow=''

oob_tamper_app=true
oob_refusal_run "$fixture/oob-app-drift.log" 'a tampered checkout-owned /app tree'
assert_contains "$fixture/oob-app-drift.log" 'app/scripts/opencode-exposure-guard.sh differs from the checkout' \
  'checkout-owned app content is compared out of band'
oob_tamper_app=false

oob_empty_cid=true
oob_refusal_run "$fixture/oob-empty-cid.log" 'an empty container ID'
assert_contains "$fixture/oob-empty-cid.log" 'could not create a container' \
  'empty container ID is refused'
oob_empty_cid=false

oob_real_binary_missing=true
oob_refusal_run "$fixture/oob-binary.log" 'a missing real binary'
assert_contains "$fixture/oob-binary.log" 'installed file usr/local/lib/opencode2/opencode' \
  'missing-binary refusal names the path'
oob_real_binary_missing=false

oob_user='0:0'
oob_refusal_run "$fixture/oob-root-user.log" 'a root image user'
assert_contains "$fixture/oob-root-user.log" 'image config User' \
  'root image user is refused by the shared config policy'
oob_user='10003:10003'

oob_shell_override='["/bin/false","-c"]'
oob_refusal_run "$fixture/oob-shell.log" 'a baked Shell override'
assert_contains "$fixture/oob-shell.log" 'Shell override' \
  'Shell-override refusal names the drift'
oob_shell_override=''

# A linked checkout policy is refused in the root-owned snapshot before any
# Python is invoked, preserving the OK-1 no-follow guarantee.
policy_file="$fixture/target/opencode2.4lunarwing/scripts/image_config_policy.py"
mv "$policy_file" "$fixture/policy-real.py"
ln -s "$fixture/policy-real.py" "$policy_file"
oob_refusal_run "$fixture/oob-policy-link.log" 'a symlinked checkout image-config policy'
assert_contains "$fixture/oob-policy-link.log" 'checkout snapshot contains a link or special file' \
  'symlinked checkout policy is refused before snapshot Python runs'
rm -f "$policy_file"
mv "$fixture/policy-real.py" "$policy_file"

mv "$fixture/target/.gitlab" "$fixture/gitlab-outside"
ln -s "$fixture/gitlab-outside" "$fixture/target/.gitlab"
oob_refusal_run "$fixture/oob-parent-link.log" 'a symlinked checkout parent'
assert_contains "$fixture/oob-parent-link.log" 'escapes the checkout root' \
  'checkout parent symlinks cannot redirect the root snapshot'
rm -f "$fixture/target/.gitlab"
mv "$fixture/gitlab-outside" "$fixture/target/.gitlab"

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
