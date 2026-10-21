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
# WI#446 (run-174 LW-3): the config-hash sidecar lives in root-owned host
# state; the harness points it at the fixture.
# shellcheck disable=SC2034  # read by the sourced _hash_dir
CONTAINER_HASH_DIR="$fixture/container-hashes"
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

# WI#324 KL-15(a): configure-worker-tools renders the proxied-tool grant into
# the tenant's daemon config — the surface a rebuilt worker image was missing
# (deny-by-default left every proxied call refused with no way to opt in).
CFG="$fixture/state/config.toml"
WORKER_BLOCK() {  # <worker> — prints that worker's block body from its name line
  local worker="$1"
  awk -v w="$worker" '
    /^\[\[sandbox\.external_workers\]\][[:space:]]*$/ { if (emit) emit = 0; inblk = 1; next }
    /^\[/ { inblk = 0; emit = 0; next }
    inblk && $0 == "name = \"" w "\"" { emit = 1; print; next }
    emit { print }
  ' "$CFG"
}
KIMI_BLOCK() { WORKER_BLOCK kimi; }
OPENCODE_BLOCK() { WORKER_BLOCK opencode; }

set_external_worker_tool_allowlist root kimi "memory_read,memory_write"
if grep -Fq 'tool_allowlist = ["memory_read", "memory_write"]' <<<"$(KIMI_BLOCK)"; then
  pass 'worker tool grant lands inside the kimi block'
else
  fail 'worker tool grant missing from the kimi block'
fi
if grep -Fq 'tool_allowlist' <<<"$(OPENCODE_BLOCK)"; then
  fail 'worker tool grant leaked into another worker block'
else
  pass 'worker tool grant stays scoped to the named worker'
fi
if [[ "$(grep -c '^tool_allowlist = ' "$CFG")" -eq 1 ]]; then
  pass 'grant insert produces exactly one tool_allowlist key'
else
  fail 'grant insert produced the wrong key count'
fi

set_external_worker_tool_allowlist root kimi "memory_read,memory_write"
if [[ "$(grep -c '^tool_allowlist = ' "$CFG")" -eq 1 ]] \
  && grep -Fq 'tool_allowlist = ["memory_read", "memory_write"]' <<<"$(KIMI_BLOCK)"; then
  pass 're-running the same grant is idempotent'
else
  fail 're-running the same grant duplicated or changed the key'
fi

set_external_worker_tool_allowlist root kimi "gitlab_tool,knowledge_search"
grep -Fq 'tool_allowlist = ["gitlab_tool", "knowledge_search"]' <<<"$(KIMI_BLOCK)" \
  && pass 'changing the grant rewrites the existing key' \
  || fail 'changing the grant did not rewrite the key'

if ( set_external_worker_tool_allowlist root kimi none ) >/dev/null 2>&1 \
  && ! grep -Fq 'tool_allowlist' "$CFG"; then
  pass "clearing with 'none' removes the grant (deny-by-default restored)"
else
  fail "clearing with 'none' left a tool_allowlist behind"
fi
if ( set_external_worker_tool_allowlist root kimi none ) >/dev/null 2>&1 \
  && ! grep -Fq 'tool_allowlist' "$CFG"; then
  pass "clearing an absent grant is a no-op"
else
  fail "clearing an absent grant misbehaved"
fi

if ( set_external_worker_tool_allowlist root kimi "shell" ) >/dev/null 2>&1; then
  fail 'non-phase-1 tool name was accepted'
else
  pass 'non-phase-1 tool name is refused'
fi
grep -Fq 'tool_allowlist' "$CFG" \
  && fail 'refused grant still wrote a key' \
  || pass 'refused grant leaves config untouched'
if ( set_external_worker_tool_allowlist root kimi "memory_read,memory_read" ) >/dev/null 2>&1; then
  fail 'duplicate tool entry was accepted'
else
  pass 'duplicate tool entry is refused'
fi
if ( set_external_worker_tool_allowlist root nanocode "memory_read" ) >/dev/null 2>&1; then
  fail 'unknown worker name was accepted'
else
  pass 'unknown worker name is refused'
fi

# A cron-mode worker never keeps a worker-level list (stripped at load,
# MR !1549 EF-1) — the command must refuse the grant outright.
cron_dir="$fixture/cron-state"
real_tenant_state_dir() { printf '%s/state\n' "$fixture"; }
tenant_state_dir() { printf '%s\n' "$cron_dir"; }
mkdir -p "$cron_dir"
cat >"$cron_dir/config.toml" <<'EOF'
[[sandbox.external_workers]]
name = "kimi"
url = "ws://127.0.0.1:12001/ws/agent"
mode = "cron"
EOF
if ( set_external_worker_tool_allowlist root kimi "memory_read" ) >/dev/null 2>&1; then
  fail 'cron-mode worker accepted a worker-level grant'
else
  pass 'cron-mode worker refuses a worker-level grant'
fi
grep -Fq 'tool_allowlist' "$cron_dir/config.toml" \
  && fail 'refused cron grant still wrote a key' \
  || pass 'refused cron grant leaves config untouched'
# Minimal block without timeout_ms: the grant inserts after the url line.
cat >"$cron_dir/config.toml" <<'EOF'
[[sandbox.external_workers]]
name = "kimi"
url = "ws://127.0.0.1:12001/ws/agent"
EOF
set_external_worker_tool_allowlist root kimi "knowledge_get_topic"
if awk 'found_url && /^tool_allowlist = \["knowledge_get_topic"\]$/ {ok=1} /^url = /{found_url=1} END{exit ok?0:1}' \
  "$cron_dir/config.toml"; then
  pass 'grant inserts after url when timeout_ms is absent'
else
  fail 'grant insert without timeout_ms misplaced'
fi

# Run-171 (note #10468) LM-1..LM-4: the grant editor runs on parsed TOML.
kimi_list_json() {  # prints the kimi entry's tool_allowlist as JSON, or "absent"
  python3 - "$1" <<'PY'
import json, sys, tomllib
with open(sys.argv[1], "rb") as handle:
    data = tomllib.load(handle)
for entry in data.get("sandbox", {}).get("external_workers", []):
    if entry.get("name") == "kimi":
        print(json.dumps(entry.get("tool_allowlist", "absent")))
        break
PY
}
toml_parses() { python3 -c 'import sys, tomllib; tomllib.load(open(sys.argv[1], "rb"))' "$1" 2>/dev/null; }
write_save_toml_shape() {  # the daemon's settings write-back puts each element on its own line
  cat >"$cron_dir/config.toml" <<'EOF'
[[sandbox.external_workers]]
name = "kimi"
url = "ws://127.0.0.1:12001/ws/agent"
timeout_ms = 3600000
tool_allowlist = [
    "memory_read",
    "memory_write",
]

[[sandbox.external_workers]]
name = "opencode"
url = "ws://127.0.0.1:11001/ws/agent"
timeout_ms = 3600000
tool_allowlist = [
    "knowledge_search",
]
EOF
}
# LM-1: replace on the save_toml shape.
write_save_toml_shape
( set_external_worker_tool_allowlist root kimi "knowledge_search" ) && lm1_rc=0 || lm1_rc=$?
if [[ "$lm1_rc" -eq 0 && "$(grep -c '^tool_allowlist' "$cron_dir/config.toml")" -eq 2 ]] \
  && grep -Fqx 'tool_allowlist = ["knowledge_search"]' "$cron_dir/config.toml" \
  && ! grep -Fq '    "memory_read",' "$cron_dir/config.toml"; then
  pass 'multi-line grant (save_toml shape) is replaced as a whole'
else
  fail 'multi-line grant (save_toml shape) left orphan lines or lost the key'
fi
toml_parses "$cron_dir/config.toml" && pass 'replaced config is valid TOML' || fail 'replaced config is not valid TOML'
[[ "$(kimi_list_json "$cron_dir/config.toml")" == '["knowledge_search"]' ]] \
  && pass 'parsed kimi grant equals the request' || fail 'parsed kimi grant differs from the request'
grep -Fq '    "knowledge_search",' "$cron_dir/config.toml" \
  && pass 'other worker entries are untouched by the replace' || fail 'the replace touched another worker entry'
# LM-1: clear on the save_toml shape.
write_save_toml_shape
( set_external_worker_tool_allowlist root kimi none ) && lm1_rc=0 || lm1_rc=$?
if [[ "$lm1_rc" -eq 0 && "$(grep -c '^tool_allowlist' "$cron_dir/config.toml")" -eq 1 ]] \
  && ! grep -Fq '    "memory_read",' "$cron_dir/config.toml" \
  && [[ "$(kimi_list_json "$cron_dir/config.toml")" == '"absent"' ]]; then
  pass 'multi-line grant (save_toml shape) is removed as a whole'
else
  fail 'clearing a multi-line grant left orphan lines'
fi
toml_parses "$cron_dir/config.toml" && pass 'cleared config is valid TOML' || fail 'cleared config is not valid TOML'
# LM-1: the design doc's two-line shape.
cat >"$cron_dir/config.toml" <<'EOF'
[[sandbox.external_workers]]
name = "kimi"
url = "ws://127.0.0.1:12001/ws/agent"
tool_allowlist = ["memory_read", "memory_write", "knowledge_search",
                  "knowledge_submit_note", "knowledge_get_topic", "gitlab_tool"]
EOF
( set_external_worker_tool_allowlist root kimi "memory_read" ) && lm1_rc=0 || lm1_rc=$?
[[ "$lm1_rc" -eq 0 ]] && toml_parses "$cron_dir/config.toml" && [[ "$(kimi_list_json "$cron_dir/config.toml")" == '["memory_read"]' ]] \
  && pass 'two-line grant (design-doc shape) is replaced as a whole' || fail 'two-line grant (design-doc shape) was torn'
# LM-3: mode = "cron" ahead of the name line is still refused.
cat >"$cron_dir/config.toml" <<'EOF'
[[sandbox.external_workers]]
mode = "cron"
name = "kimi"
url = "ws://127.0.0.1:12001/ws/agent"
EOF
cp "$cron_dir/config.toml" "$fixture/lm3-before.toml"
lm3_out="$( ( set_external_worker_tool_allowlist root kimi "memory_read" ) 2>&1 )" && lm3_rc=0 || lm3_rc=$?
[[ "$lm3_rc" -ne 0 && "$lm3_out" == *'mode = "cron"'* ]] \
  && pass 'cron-mode worker is refused whatever the key order' || fail "cron-mode worker with mode before name was accepted: [$lm3_out]"
cmp -s "$cron_dir/config.toml" "$fixture/lm3-before.toml" && pass 'refused cron (key order) leaves config untouched' || fail 'refused cron (key order) modified the config'
# LM-3: a non-cron entry sharing a cron worker's endpoint is refused (EV-1).
cat >"$cron_dir/config.toml" <<'EOF'
[[sandbox.external_workers]]
name = "kimi"
url = "ws://127.0.0.1:12001/ws/agent"

[[sandbox.external_workers]]
name = "opencode2"
url = "ws://127.0.0.1:12001/ws/agent"
mode = "cron"
EOF
cp "$cron_dir/config.toml" "$fixture/lm3b-before.toml"
lm3_out="$( ( set_external_worker_tool_allowlist root kimi "memory_read" ) 2>&1 )" && lm3_rc=0 || lm3_rc=$?
[[ "$lm3_rc" -ne 0 && "$lm3_out" == *'shares its endpoint'* ]] \
  && pass 'shared cron endpoint is refused' || fail "shared cron endpoint was accepted: [$lm3_out]"
cmp -s "$cron_dir/config.toml" "$fixture/lm3b-before.toml" && pass 'refused shared endpoint leaves config untouched' || fail 'refused shared endpoint modified the config'
# LM-4: a worker named only outside [[sandbox.external_workers]] dies with a message.
cat >"$cron_dir/config.toml" <<'EOF'
[[mcp_servers]]
name = "kimi"
url = "http://127.0.0.1:1/"
EOF
lm4_out="$( ( set_external_worker_tool_allowlist root kimi "memory_read" ) 2>&1 )" && lm4_rc=0 || lm4_rc=$?
[[ "$lm4_rc" -ne 0 && "$lm4_out" == *'is not configured'* ]] \
  && pass 'worker named only in another table is refused with a message' || fail "worker in another table: rc=$lm4_rc out=[$lm4_out]"
printf 'this = is = not toml\n' >"$cron_dir/config.toml"
lm4_out="$( ( set_external_worker_tool_allowlist root kimi "memory_read" ) 2>&1 )" && lm4_rc=0 || lm4_rc=$?
[[ "$lm4_rc" -ne 0 && "$lm4_out" == *'not valid TOML'* ]] \
  && pass 'an invalid config is refused with a message' || fail "invalid config: rc=$lm4_rc out=[$lm4_out]"
# LM-2: the config is staged as the tenant before any check reads it. A
# symlinked config the tenant cannot read (modelled by a tenant_cat that
# denies symlinks — the invoking user may be root) is refused up front; no
# root-side grep/awk ever saw the target.
lm2_orig_tenant_cat="$(declare -f tenant_cat)"
tenant_cat() { if [[ -L "$2" ]]; then return 1; fi; cat -- "$2"; }
# The link target names a DIFFERENT worker: a root-side grep of the live path
# would wrongly report "not configured", the tenant-side staging refuses first.
printf '[[sandbox.external_workers]]\nname = "opencode"\nurl = "ws://127.0.0.1:1/ws/agent"\n' >"$fixture/lm2-foreign.toml"
rm -f "$cron_dir/config.toml"
ln -s "$fixture/lm2-foreign.toml" "$cron_dir/config.toml"
lm2_out="$( ( set_external_worker_tool_allowlist root kimi "memory_read" ) 2>&1 )" && lm2_rc=0 || lm2_rc=$?
if [[ "$lm2_rc" -ne 0 && "$lm2_out" == *'could not read the tenant config'* && "$lm2_out" != *'is not configured'* ]]; then
  pass 'symlinked config is refused at the tenant-side staging, before any check'
else
  fail "symlinked config: rc=$lm2_rc out=[$lm2_out]"
fi
grep -Fq 'tool_allowlist' "$fixture/lm2-foreign.toml" && fail 'symlinked config refusal wrote through the link' || pass 'symlinked config refusal leaves the link target untouched'
rm -f "$cron_dir/config.toml"
eval "$lm2_orig_tenant_cat"
tenant_state_dir() { real_tenant_state_dir; }

# CLI surface: exact-arity dispatch and forwarding. The harness may run as a
# non-root account here; the dispatcher's require_root is stubbed for these
# forwarding checks only (the library calls above exercise the function body
# directly, as every other configure-* test in this file does).
require_root() { :; }
set_external_worker_tool_allowlist() { CAPTURED_TOOLS="$*"; }
CAPTURED_TOOLS=''
main configure-worker-tools root kimi none
if [[ "$CAPTURED_TOOLS" == 'root kimi none' ]]; then
  pass 'configure-worker-tools forwards tenant, worker, and list'
else
  fail "configure-worker-tools forwarded wrong args: [$CAPTURED_TOOLS]"
fi
if ( main configure-worker-tools root kimi ) >/dev/null 2>&1; then
  fail 'configure-worker-tools accepted a missing list argument'
else
  pass 'configure-worker-tools enforces its arity'
fi
help_output_tools="$(usage)"
if [[ "$help_output_tools" == *'configure-worker-tools'* ]]; then
  pass 'help documents configure-worker-tools'
else
  fail 'help omits configure-worker-tools'
fi

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
assert_contains "$quadlet" '--cap-drop=ALL' 'systemd Kimi quadlet drops all capabilities (WI#400 KW-3)'
assert_contains "$quadlet" '--cap-add=SETUID' 'systemd Kimi quadlet keeps SETUID for the setpriv task drop (WI#400 KW-3)'
assert_contains "$quadlet" '--cap-add=SETGID' 'systemd Kimi quadlet keeps SETGID for the setpriv task drop (WI#400 KW-3)'
assert_contains "$quadlet" '--cap-add=CHOWN' 'systemd Kimi quadlet keeps CHOWN for the task-uid handoff (WI#400 KW-3)'
assert_contains "$quadlet" '--cap-add=KILL' 'systemd Kimi quadlet keeps KILL for task cancel/timeout (WI#400 KW-3)'
assert_contains "$quadlet" '--cap-add=DAC_OVERRIDE' 'systemd Kimi quadlet keeps DAC_OVERRIDE for cron workspace creation (WI#400 KW-3/KW-4)'
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
# WI#400 run-164 KW-2: the root-bridge lanes (opencode, pebble) get the same
# container-level no-new-privileges kimi carries; the unmigrated opencode2
# lane is deliberately excluded (KW-1). LD-7: pin the FULL trimmed set on
# the Quadlet PodmanArgs — a single dropped cap must fail the pin.
KW3_QUADLET_ARGS='PodmanArgs=--security-opt=no-new-privileges --cap-drop=ALL --cap-add=CHOWN --cap-add=DAC_OVERRIDE --cap-add=FOWNER --cap-add=KILL --cap-add=SETUID --cap-add=SETGID'
assert_contains "$quadlet" "$KW3_QUADLET_ARGS" 'systemd Kimi quadlet carries the exact WI#400 capability trim (KW-3/LD-7)'
for lane in opencode pebble; do
  assert_contains "$fixture/quadlets/lunarwing-${lane}-root.container" \
    "$KW3_QUADLET_ARGS" \
    "$lane quadlet carries the exact WI#400 capability trim (KW-3/LD-7)"
done
assert_not_contains "$fixture/quadlets/lunarwing-opencode2-root.container" \
  'no-new-privileges' \
  'opencode2 quadlet stays off no-new-privileges until its own WI#400 migration'
assert_not_contains "$fixture/quadlets/lunarwing-opencode2-root.container" \
  '--cap-drop' \
  'opencode2 quadlet stays off the WI#400 capability trim until its migration'
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
if [[ "$CHOWN_CALL" == "-R --no-dereference 10002:10002 $fixture/home/lunarwing/kimi-workspace" ]]; then
  pass 'rootful Kimi workspace uses image UID and GID'
else
  fail "rootful Kimi workspace ownership wrong: $CHOWN_CALL"
fi

CHOWN_CALL=''
prepare_worker_workspace root opencode >/dev/null
if [[ "$CHOWN_CALL" == "-R --no-dereference 10001:10001 $fixture/home/lunarwing/opencode-workspace" ]]; then
  pass 'rootful OpenCode workspace uses image UID and GID'
else
  fail "rootful OpenCode workspace ownership wrong: $CHOWN_CALL"
fi

# ── WI#453: the workspace sits in the tenant-controlled ~/lunarwing chain, so
# root must never create or chown through it. A tenant-planted symlink at the
# workspace (or at a parent component) must be refused, with no chown run.
wi453_target="$fixture/home/outside-wi453"
command mkdir -p "$wi453_target"
command rm -rf -- "$fixture/home/lunarwing/kimi-workspace"
command ln -s "$wi453_target" "$fixture/home/lunarwing/kimi-workspace"
CHOWN_CALL=''
if prepare_worker_workspace root kimi >/dev/null 2>&1; then
  fail 'prepare_worker_workspace accepted a symlinked workspace (WI#453)'
else
  pass 'prepare_worker_workspace refuses a symlinked workspace (WI#453)'
fi
if [[ -z "$CHOWN_CALL" ]]; then
  pass 'symlinked workspace refusal runs no chown (WI#453)'
else
  fail "symlinked workspace refusal ran chown: $CHOWN_CALL"
fi
if [[ "$(stat -c %u "$wi453_target" 2>/dev/null || printf -- -1)" \
    == "$(stat -c %u "$fixture/home" 2>/dev/null || printf -- -2)" ]]; then
  pass 'symlink redirect target was not chowned (WI#453)'
else
  fail 'symlink redirect leaked an ownership change to the target (WI#453)'
fi
command rm -f -- "$fixture/home/lunarwing/kimi-workspace"
command rm -rf -- "$wi453_target"
unset wi453_target

# A symlinked parent component must also be refused (the tenant_mkdir_p
# ancestor walk), and the helper must not have created anything behind it.
wi453_real="$fixture/home/wi453-real"
command mkdir -p "$wi453_real"
command rm -rf -- "$fixture/home/lunarwing"
command ln -s "$wi453_real" "$fixture/home/lunarwing"
CHOWN_CALL=''
if prepare_worker_workspace root kimi >/dev/null 2>&1; then
  fail 'prepare_worker_workspace accepted a symlinked parent (WI#453)'
else
  pass 'prepare_worker_workspace refuses a symlinked parent (WI#453)'
fi
if [[ -n "$CHOWN_CALL" ]]; then
  fail "symlinked parent refusal ran chown: $CHOWN_CALL"
else
  pass 'symlinked parent refusal runs no chown (WI#453)'
fi
if [[ -e "$wi453_real/kimi-workspace" || -L "$wi453_real/kimi-workspace" ]]; then
  fail 'symlinked parent refusal left a workspace behind the link (WI#453)'
else
  pass 'symlinked parent refusal leaves nothing behind the link (WI#453)'
fi
command rm -f -- "$fixture/home/lunarwing"
command mkdir -p "$fixture/home/lunarwing"
command rm -rf -- "$wi453_real"
unset wi453_real

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
# WI#400 KW-3/LD-7: the root-bridge lanes run the bridge with a trimmed
# capability set instead of the default root set. The FULL string is pinned:
# a dropped KILL or DAC_OVERRIDE would otherwise only show in production.
KW3_CAP_TRIM='--cap-drop ALL --cap-add CHOWN --cap-add DAC_OVERRIDE --cap-add FOWNER --cap-add KILL --cap-add SETUID --cap-add SETGID'
for lane in kimi opencode pebble; do
  LANE_BLOCK="$(awk "/^start_tenant_${lane}\\(\\)/,/^stop_tenant_${lane}\\(\\)/" "$WORKER_LIB")"
  if grep -Fq -- "$KW3_CAP_TRIM" <<<"$LANE_BLOCK"; then
    pass "$lane imperative launch runs the exact WI#400 capability trim (KW-3/LD-7)"
  else
    fail "$lane imperative launch missing the exact WI#400 capability trim (KW-3/LD-7)"
  fi
done
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
# WI#400 run-164 KW-2: opencode and pebble imperative launches set
# no-new-privileges like kimi's; opencode2 stays off it until its lane
# migrates (KW-1).
for lane in opencode pebble; do
  LANE_BLOCK="$(awk "/^start_tenant_${lane}\\(\\)/,/^stop_tenant_${lane}\\(\\)/" "$WORKER_LIB")"
  if grep -Fq -- '--security-opt no-new-privileges' <<<"$LANE_BLOCK"; then
    pass "$lane imperative launch sets no-new-privileges (WI#400 KW-2)"
  else
    fail "$lane imperative launch missing no-new-privileges (WI#400 KW-2)"
  fi
done
OPENCODE2_BLOCK="$(awk '/^start_tenant_opencode2\(\)/,/^stop_tenant_opencode2\(\)/' "$WORKER_LIB")"
if grep -Fq -- '--security-opt no-new-privileges' <<<"$OPENCODE2_BLOCK"; then
  fail 'opencode2 imperative launch picked up no-new-privileges before its WI#400 migration'
else
  pass 'opencode2 imperative launch unchanged by WI#400 (KW-1)'
fi
# The launch contract must carry the policy change so existing containers
# force-recreate instead of surviving with the old mount.
ROOT_SCRIPT="$(dirname "$MT_ADMIN")/lunarwing-mt-admin.sh"
assert_contains "$ROOT_SCRIPT" 'KIMI_ROOTLESS_LAUNCH_CONTRACT="userns=${KIMI_USERNS};no-ssh-agent;no-new-privileges;cap-trim"' \
  'kimi launch contract records the WI#206 policy change and the WI#400 cap-trim'
# Contract changes recreate old RW-mount containers on rootless AND rootful
# imperative lanes. Quadlet recreations hash the rendered Volume= line.
assert_contains "$ROOT_SCRIPT" 'OPENCODE_ROOTLESS_LAUNCH_CONTRACT="userns=${OPENCODE_USERNS};ssh-agent-dir-ro;no-new-privileges;cap-trim"' \
  'opencode rootless launch contract records the read-only SSH mount, WI#400 no-new-privileges, and cap-trim'
assert_contains "$ROOT_SCRIPT" 'OPENCODE2_ROOTLESS_LAUNCH_CONTRACT="userns=${OPENCODE2_USERNS};ssh-agent-dir-ro"' \
  'opencode2 rootless launch contract records the read-only SSH mount'
assert_contains "$ROOT_SCRIPT" 'PEBBLE_ROOTLESS_LAUNCH_CONTRACT="userns=${PEBBLE_USERNS};ssh-agent-dir-ro;no-new-privileges;cap-trim"' \
  'pebble launch contract records the WI#400 task-uid userns, read-only SSH mount, no-new-privileges, and cap-trim'
assert_contains "$WORKER_LIB" 'local opencode_launch_contract="ssh-agent-dir-ro;cap-trim"' \
  'rootful opencode includes the SSH mount and the WI#400 cap-trim in its launch contract'
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
