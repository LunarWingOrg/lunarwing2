#!/usr/bin/env bash
# WI#474 / !1943 ON-1: provisioning and upgrade must refuse useless grants at
# the hub, without changing other worker policy or bypassing the tenant writer.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"
fixture="$(mktemp -d)"
trap 'rm -rf -- "$fixture"' EXIT

tenant_state_dir() { printf '%s/state\n' "$fixture"; }
tenant_env_dir() { printf '%s/env\n' "$fixture"; }
tenant_worker_enabled() { return 0; }
ports_get() { printf '20001\n'; }
resolve_worker_auth_token() { printf 'synthetic-worker-token'; }
tenant_cat() { cat -- "$2"; }
write_tenant_file_atomic() {
  [[ "$3" == 600 && "$4" == owned-parent ]]
  cat >"$2"
  chmod "$3" "$2"
}
sudo() { [[ "$1" != -u ]] || shift 2; command "$@"; }
say() { :; }
mkdir -p "$fixture/state" "$fixture/env"
config="$fixture/state/config.toml"

for worker in opencode opencode2 kimi pebble; do
  ensure_external_worker_config fixture "$worker" "${worker}_wss"
done
python3 - "$config" <<'PY'
import sys, tomllib
with open(sys.argv[1], 'rb') as handle:
    workers = tomllib.load(handle)['sandbox']['external_workers']
assert len(workers) == 4
for worker in workers:
    if worker['name'] in ('opencode', 'opencode2'):
        assert worker['supports_credentials'] is False
    else:
        assert 'supports_credentials' not in worker
print('PASS: fresh OpenCode entries disable grants; kimi/pebble keep defaults')
PY

# Legacy entries include an explicit true, no key, a subtable, and unrelated
# per-worker policy. The new key must stay in the worker table, not its subtable.
cat >"$config" <<'TOML'
[[sandbox.external_workers]]
supports_credentials = true
name = "opencode"
url = "ws://127.0.0.1:20001/ws/agent"
auth_token = "synthetic-worker-token"
[sandbox.external_workers.prompt_context]
max_messages = 8
[[sandbox.external_workers]]
name = "kimi"
url = "ws://127.0.0.1:20002/ws/agent"
supports_credentials = false
[[sandbox.external_workers]]
name = "opencode2"
url = "ws://127.0.0.1:20003/ws/agent"
auth_token = "synthetic-worker-token"
TOML
cp "$config" "$fixture/before.toml"
for worker in opencode opencode2; do
  ensure_external_worker_config fixture "$worker" "${worker}_wss"
done
python3 - "$fixture/before.toml" "$config" <<'PY'
import sys, tomllib
with open(sys.argv[1], 'rb') as handle:
    expected = tomllib.load(handle)
for worker in expected['sandbox']['external_workers']:
    if worker['name'] in ('opencode', 'opencode2'):
        worker['supports_credentials'] = False
with open(sys.argv[2], 'rb') as handle:
    assert tomllib.load(handle) == expected
print('PASS: existing OpenCode entries converge without unrelated TOML changes')
PY
cp "$config" "$fixture/after.toml"
for worker in opencode opencode2; do
  ensure_external_worker_config fixture "$worker" "${worker}_wss"
done
cmp "$config" "$fixture/after.toml"
printf 'PASS: repeated migration is byte-idempotent\n'

# All unsafe/ambiguous inputs fail closed, with no write to the tenant file.
for kind in malformed duplicate misleading-string; do
  case "$kind" in
    malformed) printf '[[sandbox.external_workers]\n' >"$config" ;;
    duplicate) cat "$fixture/before.toml" "$fixture/before.toml" >"$config" ;;
    misleading-string)
      { printf 'note = """\n[[sandbox.external_workers]]\n"""\n'; cat "$fixture/before.toml"; } >"$config"
      ;;
  esac
  cp "$config" "$fixture/unsafe.toml"
  if upgrade_external_worker_credentials fixture opencode "$config"; then
    printf 'FAIL: accepted %s config\n' "$kind" >&2
    exit 1
  fi
  cmp "$config" "$fixture/unsafe.toml"
done
printf 'PASS: malformed, duplicate and misleading headers cannot change config\n'

cp "$fixture/before.toml" "$config"
if (write_tenant_file_atomic() { return 1; }; upgrade_external_worker_credentials fixture opencode "$config"); then
  printf 'FAIL: ignored tenant writer refusal\n' >&2
  exit 1
fi
cmp "$config" "$fixture/before.toml"
printf 'PASS: tenant writer refusal propagates without root-side fallback\n'
