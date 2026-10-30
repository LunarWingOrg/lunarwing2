#!/usr/bin/env bash
# WI#505 harness: the tenant lifecycle lock descriptor must not be inherited by
# daemons started inside the locked section, and a die inside the section must
# release the lock at process exit. Uses LUNARWING_MT_LIFECYCLE_LOCK_DIR to
# redirect the lock directory into a fixture (no root, no OpenRC required).
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# mt-admin re-sets SCRIPT_DIR to ic/scripts; capture the scripts dir for the
# env-exec sibling path before sourcing.
ENV_EXEC="$SCRIPT_DIR/../lunarwing-openrc-env-exec.sh"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT
# Must be set before sourcing: mt-admin binds TENANT_LIFECYCLE_LOCK_DIR from
# this variable at load time.
export LUNARWING_MT_LIFECYCLE_LOCK_DIR="$fixture_dir/locks"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

mkdir -p "$LUNARWING_MT_LIFECYCLE_LOCK_DIR"
chmod 700 "$LUNARWING_MT_LIFECYCLE_LOCK_DIR"

# The wrapper requires the lock dir to be root-owned; report owner 0 for the
# fixture dir only and pass every other stat call through.
stat() {
  local fmt="" arg
  for arg in "$@"; do
    case "$arg" in %u:%a) fmt="$arg" ;; esac
  done
  if [[ -n "$fmt" && " $* " == *" $LUNARWING_MT_LIFECYCLE_LOCK_DIR "* ]]; then
    printf '0:700\n'
    return 0
  fi
  command stat "$@"
}

# Harness convention (WI#390): the fixture lock directory is owned by the
# invoking user, so the root-identity/mode checks in the lock wrapper are
# redirected at the fixture (mkdir instead of install -o root; stat accepted).
install() {
  local args=()
  while [[ $# -gt 0 ]]; do
    case "$1" in
      -o|-g|-m) shift 2 ;;
      -d) shift ;;
      *) args+=("$1"); shift ;;
    esac
  done
  mkdir -p -- "${args[@]}"
}

failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }
assert() {
  local label="$1"
  shift
  if "$@"; then pass "$label"; else fail "$label"; fi
}

lock_file="$LUNARWING_MT_LIFECYCLE_LOCK_DIR/alice.lock"

# ── 1. die inside the locked section releases the lock at process exit ──────
die_inside_lock() {
  with_tenant_lifecycle_lock alice die "simulated OpenRC abort"
}
# Subshell exits via die (exit 1); capture the status without errexit so the
# harness keeps running.
rc=0
( set -euo pipefail; die_inside_lock ) 2>"$fixture_dir/die.err" || rc=$?
[[ "$rc" -ne 0 ]] || fail "die inside the locked section fails the command"
assert "die output reports the simulated abort" grep -q "simulated OpenRC abort" "$fixture_dir/die.err"
assert "lock is free after a die inside the locked section" flock -n "$lock_file" true

# ── 2. _openrc_service drops the active lock descriptor before exec ─────────
# The stub must be an external binary so the scrub happens in a child process
# (a shell-function stub would run in the mt-admin shell and consume the
# scrub itself, leaving the parent's lock fd closed before the unlock).
require_safe_openrc_proxy_units() { return 0; }
mkdir -p "$fixture_dir/bin"
cat >"$fixture_dir/bin/rc-service" <<STUB
#!/usr/bin/env bash
for fd_path in /proc/self/fd/*; do
  fd="\${fd_path##*/}"
  [[ "\$fd" =~ ^[0-9]+\$ ]] || continue
  target="\$(readlink "\$fd_path" 2>/dev/null || true)"
  case "\$target" in
    *"/alice.lock") printf 'inherited\n'; exit 0 ;;
  esac
done
printf 'scrubbed\n'
STUB
chmod 755 "$fixture_dir/bin/rc-service"
PATH="$fixture_dir/bin:$PATH"
child_sees_lock_fd() {
  with_tenant_lifecycle_lock alice _openrc_service lunarwing-alice start
}
child_result="$(child_sees_lock_fd)"
assert "rc-service child does not inherit the lock descriptor" \
  test "$child_result" = "scrubbed"
assert "lock is free after a guarded rc-service call" flock -n "$lock_file" true

# ── 3. _systemctl_user drops the active lock descriptor before exec ─────────
id() {
  if [[ "${1:-}" == "-u" ]]; then printf '1000\n'; else command id "$@"; fi
}
cat >"$fixture_dir/bin/sudo" <<'STUB'
#!/usr/bin/env bash
[[ "${1:-}" == "-u" ]] || exit 1
shift 2
while [[ "${1:-}" == *=* ]]; do shift; done
exec "$@"
STUB
cat >"$fixture_dir/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
for fd_path in /proc/self/fd/*; do
  fd="${fd_path##*/}"
  [[ "$fd" =~ ^[0-9]+$ ]] || continue
  target="$(readlink "$fd_path" 2>/dev/null || true)"
  case "$target" in
    *"/alice.lock") printf 'inherited\n'; exit 0 ;;
  esac
done
printf 'scrubbed\n'
STUB
chmod 755 "$fixture_dir/bin/sudo" "$fixture_dir/bin/systemctl"
systemctl_child_sees_lock_fd() {
  with_tenant_lifecycle_lock alice _systemctl_user alice start lunarwing-alice.service
}
systemctl_result="$(systemctl_child_sees_lock_fd)"
assert "systemctl --user child does not inherit the lock descriptor" \
  test "$systemctl_result" = "scrubbed"
assert "lock is free after a guarded systemctl call" flock -n "$lock_file" true

# ── 4. preflight reports a stale holder instead of blocking ─────────────────
(
  exec 9>"$lock_file"
  flock -x 9
  sleep 30
) &
stale_pid=$!
for _ in $(seq 1 50); do
  grep -q "FLOCK" /proc/locks && break
  sleep 0.05
done
lock_probe() {
  with_tenant_lifecycle_lock alice true
}
probe_rc=0
probe_out="$(lock_probe 2>&1)" || probe_rc=$?
kill "$stale_pid" 2>/dev/null || true
wait "$stale_pid" 2>/dev/null || true
[[ "$probe_rc" -ne 0 ]] || fail "preflight refuses a held lifecycle lock"
assert "preflight failure names the held lock" bash -c \
  '[[ "$1" == *"already held"* ]]' _ "$probe_out"
# The holder pid in /proc/locks is the flock-owning descendant of the
# backgrounded subshell, not necessarily $!, so assert that some pid list is
# reported rather than the exact background pid.
assert "preflight failure lists at least one holder pid" bash -c \
  '[[ "$1" =~ pids:\ [0-9]+ ]]' _ "$probe_out"
assert "lock is free once the stale holder exits" flock -n "$lock_file" true

# ── 5. nested same-tenant lock reuse still validates the inherited fd ───────
nested_ok=true
nested_body() {
  [[ "$LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD" =~ ^[0-9]+$ ]] || return 1
  with_tenant_lifecycle_lock alice true
}
with_tenant_lifecycle_lock alice nested_body || nested_ok=false
assert "nested same-tenant lifecycle lock reuse succeeds" \
  test "$nested_ok" = true

forged_probe() {
  (
    exec {probe_fd}>"$lock_file" || exit 1
    LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD="$probe_fd" \
    LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT=alice \
      with_tenant_lifecycle_lock alice true
  )
}
if forged_probe 2>/dev/null; then
  fail "an unlocked same-inode fd is refused as a forged active lock"
else
  pass "an unlocked same-inode fd is refused as a forged active lock"
fi
assert "lock is free after the forged-fd refusal" flock -n "$lock_file" true

# ── 6. env-exec closes a leaked lock descriptor before exec ─────────────────
tenant_home="$fixture_dir/tenant-home"
mkdir -p "$tenant_home"
chmod 700 "$tenant_home"
printf 'LUNARWING_TEST_MARKER=present\n' >"$tenant_home/lunarwing.env"
chmod 600 "$tenant_home/lunarwing.env"
env_exec_probe="$fixture_dir/env-exec-probe.sh"
cat >"$env_exec_probe" <<'PROBE'
#!/usr/bin/env bash
leaked=""
for fd_path in /proc/self/fd/*; do
  fd="${fd_path##*/}"
  [[ "$fd" =~ ^[0-9]+$ ]] || continue
  target="$(readlink "$fd_path" 2>/dev/null || true)"
  case "$target" in *"/alice.lock") leaked="$fd" ;; esac
done
if [[ -n "$leaked" ]]; then
  printf 'leaked fd %s\n' "$leaked"
  exit 1
fi
printf 'clean marker=%s\n' "${LUNARWING_TEST_MARKER:-missing}"
PROBE
chmod 755 "$env_exec_probe"
# Simulate the leaked descriptor: open the lock on a high fd, hold the flock,
# then exec the wrapper as a descendant — the wrapper must close the fd.
env_exec_rc=0
env_exec_out="$(
  export LUNARWING_MT_LIFECYCLE_LOCK_DIR
  exec 8>"$lock_file"
  flock -x 8
  bash "$ENV_EXEC" \
    --env-file "$tenant_home/lunarwing.env" -- "$env_exec_probe"
)" || env_exec_rc=$?
exec 8>&-
if [[ "$env_exec_rc" -eq 0 && "$env_exec_out" == "clean marker=present" ]]; then
  pass "env-exec closes a leaked lock descriptor before exec"
else
  fail "env-exec closes a leaked lock descriptor before exec (rc=$env_exec_rc out='$env_exec_out')"
fi
assert "lock is free after the env-exec probe" flock -n "$lock_file" true

# ── 7. env-exec leaves unrelated inherited fds alone ────────────────────────
unrelated="$fixture_dir/unrelated.txt"
printf 'keep-me\n' >"$unrelated"
unrelated_probe="$fixture_dir/unrelated-probe.sh"
cat >"$unrelated_probe" <<'PROBE'
#!/usr/bin/env bash
content="$(cat <&7 2>/dev/null || true)"
[[ "$content" == "keep-me" ]] || { printf 'unrelated fd lost\n'; exit 1; }
printf 'unrelated fd kept\n'
PROBE
chmod 755 "$unrelated_probe"
unrelated_out="$(
  exec 7<"$unrelated"
  exec 8>"$lock_file"
  flock -x 8
  bash "$ENV_EXEC" \
    --env-file "$tenant_home/lunarwing.env" -- "$unrelated_probe" 7<&7
)"
unrelated_rc=$?
exec 7<&-
exec 8>&-
if [[ "$unrelated_rc" -eq 0 && "$unrelated_out" == "unrelated fd kept" ]]; then
  pass "env-exec keeps fds that do not point into the lock directory"
else
  fail "env-exec keeps fds that do not point into the lock directory (rc=$unrelated_rc out='$unrelated_out')"
fi

printf '\n'
if [[ "$failures" -eq 0 ]]; then
  printf 'ALL TESTS PASSED\n'
  exit 0
fi
printf '%s TEST(S) FAILED\n' "$failures"
exit 1
