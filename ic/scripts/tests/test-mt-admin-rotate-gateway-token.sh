#!/usr/bin/env bash
# Fixture-only test of `rotate-gateway-token` (UZ-2 of the TUI review): the verb
# replaces GATEWAY_AUTH_TOKEN through the tenant-side atomic env rewrite,
# restarts a running tenant only, keeps every other key, and never lets either
# token reach argv, stdout or stderr. No live services or secrets are touched.
# shellcheck disable=SC1091,SC2034  # dynamic source and fixture globals consumed by mt-admin
set -euo pipefail
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
trap 'rm -rf -- "$fixture"' EXIT
tenant_env_dir() { printf '%s/%s/env\n' "$fixture" "$1"; }
tenant_exists_in_registry() { [[ "$1" == test ]]; }
ensure_init_system() { :; }
chown() { :; }
INIT_SYSTEM=systemd # consumed by the sourced mt-admin library
# The fixture tenant is "test"; map its privilege drop to the invoking user,
# or to nobody when CI runs this as root (the writer refuses a foreign owner).
sudo() {
  [[ "$1" == -u && "$2" == test ]] || return 1
  shift 2
  printf '%s\n' "$*" >>"$fixture/sudo.argv"
  if [[ "$EUID" == 0 ]]; then
    runuser -u nobody -- "$@"
  else
    "$@"
  fi
}
# Service state: a flag file says whether lunarwing-test is active.
_relay_service_active() { [[ -f "$fixture/active" ]]; }
restart_tenant() { printf '%s\n' "$1" >>"$fixture/restarts"; }
# Deterministic 256-bit tokens, counted.
generate_token() {
  local n=0
  [[ ! -f "$fixture/counter" ]] || n="$(<"$fixture/counter")"
  n=$((n + 1))
  printf '%s\n' "$n" >"$fixture/counter"
  printf '%064d\n' "$n"
}

env_dir="$(tenant_env_dir test)"
lw="$env_dir/lunarwing.env"
mkdir -p "$env_dir"
chmod 700 "$env_dir"
OLD_TOKEN="old-master-$(printf '%s' 0123456789 abcdef)"
printf 'OTHER_KEY=preserve\nGATEWAY_AUTH_TOKEN=%s\nLAST_KEY=kept\n' "$OLD_TOKEN" >"$lw"
chmod 600 "$lw"
if [[ "$EUID" == 0 ]]; then
  # The drop to nobody must be able to reach the tenant env directory.
  chmod 755 "$fixture"
  command chown -R nobody "$fixture/test"
fi
: >"$fixture/restarts"
: >"$fixture/sudo.argv"

failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() {
  printf 'FAIL: %s\n' "$1"
  failures=$((failures + 1))
}
check() {  # <label> <command...>
  local label="$1"
  shift
  if "$@"; then pass "$label"; else fail "$label"; fi
}
# The secret travels on a pipe into grep (-f), never through argv.
contains_secret() {  # <file> <secret>
  grep -qF -f <(printf '%s\n' "$2") -- "$1"
}
run_rotate() {  # sets RC, writes $OUT and $ERR
  OUT="$fixture/out" ERR="$fixture/err"
  set +e
  (
    set -e
    rotate_tenant_gateway_token test
  ) >"$OUT" 2>"$ERR"
  RC=$?
  set -e
}
env_value() { sed -n "s/^$1=//p" "$lw"; }

# ── 1. a running tenant: value replaced, other keys kept, restart once ───────
touch "$fixture/active"
run_rotate
NEW_1="$(env_value GATEWAY_AUTH_TOKEN)"
assert_rotated() {
  ((RC == 0)) && [[ "$NEW_1" =~ ^[0-9a-f]{64}$ && "$NEW_1" != "$OLD_TOKEN" ]] \
    && [[ "$(env_value OTHER_KEY)" == preserve && "$(env_value LAST_KEY)" == kept ]] \
    && [[ "$(grep -c '^GATEWAY_AUTH_TOKEN=' "$lw")" == 1 ]] \
    && [[ "$(stat -c '%a:%h' -- "$lw")" == 600:1 ]] \
    && ! contains_secret "$lw" "$OLD_TOKEN"
}
check "1. rotation replaces GATEWAY_AUTH_TOKEN with a 64-hex value, keeps the other keys, 0600 and one link" assert_rotated
check "2. a running tenant is restarted exactly once" \
  eval '[[ "$(cat "$fixture/restarts")" == test ]] && grep -q "restarted it" "$OUT"'
assert_quiet() {
  ! contains_secret "$OUT" "$OLD_TOKEN" && ! contains_secret "$ERR" "$OLD_TOKEN" \
    && ! contains_secret "$OUT" "$NEW_1" && ! contains_secret "$ERR" "$NEW_1" \
    && ! contains_secret "$fixture/sudo.argv" "$NEW_1" && ! contains_secret "$fixture/sudo.argv" "$OLD_TOKEN"
}
check "3. neither token appears on stdout, stderr or the argv of the privilege drops" assert_quiet
check "4. the report names every consumer that now holds a stale copy" \
  eval 'grep -q "master-token TUI file" "$OUT" && grep -q "tokens test" "$OUT" && grep -q "worker token fallback" "$OUT" && grep -q "DarkIRC and Brave" "$OUT"'

# ── 2. a stopped tenant: value replaced, no restart ──────────────────────────
rm -f "$fixture/active"
: >"$fixture/restarts"
run_rotate
NEW_2="$(env_value GATEWAY_AUTH_TOKEN)"
check "5. a stopped tenant is rotated without a restart and told the value applies at the next start" \
  eval '((RC == 0)) && [[ "$NEW_2" =~ ^[0-9a-f]{64}$ && "$NEW_2" != "$NEW_1" ]] && [[ ! -s "$fixture/restarts" ]] && grep -q "not running" "$OUT"'

# ── 3. refusals leave the file unchanged ─────────────────────────────────────
before="$(sha256sum <"$lw")"
mv "$lw" "$lw.real"
ln -s "$lw.real" "$lw"
run_rotate
check "6. a symlinked env file is refused and left alone" \
  eval '((RC == 1)) && grep -q "missing or unsafe env file" "$ERR" && [[ "$(sha256sum <"$lw.real")" == "$before" ]]'
rm -f "$lw"
mv "$lw.real" "$lw"

# ── 4. the dispatcher and usage know the verb ────────────────────────────────
check "7. usage lists rotate-gateway-token" \
  eval 'usage | grep -q "rotate-gateway-token <name>"'

if ((failures != 0)); then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all rotate-gateway-token tests passed\n'
