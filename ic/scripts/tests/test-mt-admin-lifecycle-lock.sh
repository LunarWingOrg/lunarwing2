#!/usr/bin/env bash
# WI#505/WI#526 harness: tracked mt-admin lock descriptors must not be
# inherited by daemons started inside a locked section, a die inside the
# section must release the lock at process exit, and a stale holder must be
# reported (not blocked on) with its live inheritor pids. Uses
# LUNARWING_MT_LIFECYCLE_LOCK_DIR / LUNARWING_MT_DARKIRC_WRITER_LOCK_DIR and
# BUILD_LOCK to redirect locks into a fixture (no OpenRC required). Runs
# identically as root and non-root: the env-exec probes (which refuse EUID 0
# by design) drop privileges when the harness runs as root.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# mt-admin re-sets SCRIPT_DIR to ic/scripts; capture the scripts dir for the
# env-exec sibling path before sourcing.
ENV_EXEC="$SCRIPT_DIR/../lunarwing-openrc-env-exec.sh"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT
# Must be set before sourcing: mt-admin binds TENANT_LIFECYCLE_LOCK_DIR and
# DARKIRC_WRITER_LOCK_DIR from these variables at load time.
export LUNARWING_MT_LIFECYCLE_LOCK_DIR="$fixture_dir/locks"
export LUNARWING_MT_DARKIRC_WRITER_LOCK_DIR="$fixture_dir/darkirc-locks"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"
# mt-admin fixes BUILD_LOCK to its production path; redirect it after sourcing.
export BUILD_LOCK="$fixture_dir/lunarwing-build.lock"

mkdir -p "$LUNARWING_MT_LIFECYCLE_LOCK_DIR" "$LUNARWING_MT_DARKIRC_WRITER_LOCK_DIR"
chmod 700 "$LUNARWING_MT_LIFECYCLE_LOCK_DIR" "$LUNARWING_MT_DARKIRC_WRITER_LOCK_DIR"

# The wrapper requires the lock dirs to be root-owned; report owner 0 for the
# fixture dirs only and pass every other stat call through.
stat() {
  local fmt="" arg
  for arg in "$@"; do
    case "$arg" in %u:%a|%u) fmt="$arg" ;; esac
  done
  case " $* " in
    *" $LUNARWING_MT_LIFECYCLE_LOCK_DIR "*|*" $LUNARWING_MT_DARKIRC_WRITER_LOCK_DIR "*)
      if [[ "$fmt" == "%u:%a" ]]; then printf '0:700\n'; return 0; fi
      if [[ "$fmt" == "%u" ]]; then printf '0\n'; return 0; fi
      ;;
  esac
  command stat "$@"
}

# Harness convention (WI#390): the fixture lock directories are owned by the
# invoking user, so the root-identity/mode checks in the lock wrappers are
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
skips=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }
skip() { printf 'SKIP: %s\n' "$1"; skips=$((skips + 1)); }
assert() {
  local label="$1"
  shift
  if "$@"; then pass "$label"; else fail "$label"; fi
}

lock_file="$LUNARWING_MT_LIFECYCLE_LOCK_DIR/alice.lock"

# Run a command as an unprivileged user when the harness itself is root, so
# env-exec's EUID-0 refusal does not fire (SH-1b: CI runs harnesses as root).
# Prints nothing; sets AS_USER_RC and captures stdout on stdout. Returns 127
# (unavailable) when no privilege-drop tool or unprivileged user exists.
as_unpriv() {
  if [[ "$EUID" -ne 0 ]]; then
    "$@"
    return
  fi
  local tool=""
  if command -v setpriv >/dev/null 2>&1; then
    tool="setpriv"
  elif command -v runuser >/dev/null 2>&1; then
    tool="runuser"
  else
    return 127
  fi
  id nobody >/dev/null 2>&1 || return 127
  local nogroup
  nogroup="$(id -gn nobody 2>/dev/null)" || return 127
  # nobody needs to traverse/read the fixture; open its search bits.
  chmod 755 "$fixture_dir"
  case "$tool" in
    setpriv) setpriv --reuid=nobody --regid="$nogroup" --clear-groups "$@" ;;
    runuser) runuser -u nobody -- "$@" ;;
  esac
}

# When root, the dropped-to user cannot traverse the repo path to reach the
# env-exec wrapper (the working tree is not world-searchable), so stage a
# world-readable copy inside the fixture for the env-exec probes (SH-1b).
ENV_EXEC_RUN="$ENV_EXEC"
if [[ "$EUID" -eq 0 ]]; then
  ENV_EXEC_RUN="$fixture_dir/lunarwing-openrc-env-exec.sh"
  cp "$ENV_EXEC" "$ENV_EXEC_RUN"
  chmod 755 "$ENV_EXEC_RUN"
fi

# ── 1. die inside the locked section releases the lock at process exit ──────
die_inside_lock() {
  with_tenant_lifecycle_lock alice die "simulated OpenRC abort"
}
rc=0
( set -euo pipefail; die_inside_lock ) 2>"$fixture_dir/die.err" || rc=$?
[[ "$rc" -ne 0 ]] || fail "die inside the locked section fails the command"
assert "die output reports the simulated abort" grep -q "simulated OpenRC abort" "$fixture_dir/die.err"
assert "lock is free after a die inside the locked section" flock -n "$lock_file" true

# ── 2. _openrc_service drops the tracked lock descriptors before exec ───────
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
    *"/alice.lock"|*"/lunarwing-build.lock.alice.upgrade") printf 'inherited\n'; exit 0 ;;
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
assert "rc-service child does not inherit the lifecycle lock descriptor" \
  test "$child_result" = "scrubbed"
assert "lock is free after a guarded rc-service call" flock -n "$lock_file" true

upgrade_lock_file="${BUILD_LOCK}.alice.upgrade"
exec {inherited_upgrade_fd}>"$upgrade_lock_file"
flock -x "$inherited_upgrade_fd"
export LUNARWING_MT_INHERITED_UPGRADE_LOCK_FD="$inherited_upgrade_fd"
wrapper_result="$(_openrc_service lunarwing-alice start)"
assert "rc-service child does not inherit the wrapper upgrade lock descriptor" \
  test "$wrapper_result" = "scrubbed"
flock -u "$inherited_upgrade_fd"
exec {inherited_upgrade_fd}>&-
unset LUNARWING_MT_INHERITED_UPGRADE_LOCK_FD
assert "upgrade lock is free after the wrapper rc-service call" \
  flock -n "$upgrade_lock_file" true

# ── 3. _systemctl_user drops the tracked lock descriptors before exec ───────
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
assert "systemctl --user child does not inherit the lifecycle lock descriptor" \
  test "$systemctl_result" = "scrubbed"
assert "lock is free after a guarded systemctl call" flock -n "$lock_file" true

# ── 4. preflight reports a stale holder (live inheritor) instead of blocking ─
# Hold the lock from a long-lived child, then confirm the preflight fails
# fast and names a live inheritor pid. Wait for the lock to actually be held
# (flock -n fails) rather than grepping host-wide /proc/locks (SH-4).
(
  exec 9>"$lock_file"
  flock -x 9
  sleep 30
) &
stale_pid=$!
for _ in $(seq 1 100); do
  if ! flock -n "$lock_file" true 2>/dev/null; then break; fi
  sleep 0.05
done
lock_probe() {
  with_tenant_lifecycle_lock alice true
}
probe_rc=0
probe_out="$(lock_probe 2>&1)" || probe_rc=$?
# Kill the whole backgrounded process group so the sleep child dies too (SH-4).
kill -- "-$stale_pid" 2>/dev/null || kill "$stale_pid" 2>/dev/null || true
wait "$stale_pid" 2>/dev/null || true
[[ "$probe_rc" -ne 0 ]] || fail "preflight refuses a held lifecycle lock"
assert "preflight failure names the held lock" bash -c \
  '[[ "$1" == *"already held"* ]]' _ "$probe_out"
# The reported pids are the live inheritors (processes holding a descriptor
# on the lock), which may differ from the original acquirer.
assert "preflight failure lists at least one live inheritor pid" bash -c \
  '[[ "$1" =~ live\ inheritor\ pids:\ [0-9]+ ]]' _ "$probe_out"
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
printf 'LUNARWING_TEST_MARKER=present\n' >"$tenant_home/lunarwing.env"
env_exec_probe="$fixture_dir/env-exec-probe.sh"
cat >"$env_exec_probe" <<'PROBE'
#!/usr/bin/env bash
leaked=""
for fd_path in /proc/self/fd/*; do
  fd="${fd_path##*/}"
  [[ "$fd" =~ ^[0-9]+$ ]] || continue
  target="$(readlink "$fd_path" 2>/dev/null || true)"
  case "$target" in
    *"/alice.lock"|*"/lunarwing-build.lock.alice.upgrade") leaked="$fd" ;;
  esac
done
if [[ -n "$leaked" ]]; then
  printf 'leaked fd %s\n' "$leaked"
  exit 1
fi
printf 'clean marker=%s\n' "${LUNARWING_TEST_MARKER:-missing}"
PROBE
chmod 755 "$env_exec_probe"
# env-exec enforces owner==EUID and mode 700/600 on the env file, so when the
# harness runs as root the file must be given to the dropped-to user.
if [[ "$EUID" -eq 0 ]]; then
  chown -R nobody:"$(id -gn nobody 2>/dev/null || echo nobody)" "$tenant_home" 2>/dev/null || true
fi
chmod 700 "$tenant_home"
chmod 600 "$tenant_home/lunarwing.env"
# Simulate the leaked descriptor: open the lock on a high fd, hold the flock,
# then run the wrapper as a descendant — the wrapper must close the fd.
env_exec_rc=0
env_exec_out="$(
  export LUNARWING_MT_LIFECYCLE_LOCK_DIR
  export LUNARWING_MT_DARKIRC_WRITER_LOCK_DIR
  exec 8>"$lock_file"
  flock -x 8
  as_unpriv bash "$ENV_EXEC_RUN" \
    --env-file "$tenant_home/lunarwing.env" -- "$env_exec_probe"
)" || env_exec_rc=$?
exec 8>&-
if [[ "$env_exec_rc" -eq 127 ]]; then
  skip "env-exec closes a leaked lock descriptor before exec (no privilege-drop tool/user)"
elif [[ "$env_exec_rc" -eq 0 && "$env_exec_out" == "clean marker=present" ]]; then
  pass "env-exec closes a leaked lock descriptor before exec"
else
  fail "env-exec closes a leaked lock descriptor before exec (rc=$env_exec_rc out='$env_exec_out')"
fi
assert "lock is free after the env-exec probe" flock -n "$lock_file" true

build_env_exec_rc=0
build_env_exec_out="$(
  export BUILD_LOCK
  exec 8>"$upgrade_lock_file"
  flock -x 8
  as_unpriv bash "$ENV_EXEC_RUN" \
    --env-file "$tenant_home/lunarwing.env" -- "$env_exec_probe"
)" || build_env_exec_rc=$?
exec 8>&-
if [[ "$build_env_exec_rc" -eq 127 ]]; then
  skip "env-exec closes a leaked upgrade lock descriptor before exec (no privilege-drop tool/user)"
elif [[ "$build_env_exec_rc" -eq 0 && "$build_env_exec_out" == "clean marker=present" ]]; then
  pass "env-exec closes a leaked upgrade lock descriptor before exec"
else
  fail "env-exec closes a leaked upgrade lock descriptor before exec (rc=$build_env_exec_rc out='$build_env_exec_out')"
fi
assert "upgrade lock is free after the env-exec probe" flock -n "$upgrade_lock_file" true

# UO-1: /var/lock is a symlink to /run/lock on standard OpenRC hosts. The
# kernel reports the resolved fd target, so an unresolved BUILD_LOCK prefix
# must not prevent the backstop from closing the descriptor.
build_lock_real="$fixture_dir/build-lock-real"
build_lock_link="$fixture_dir/build-lock-link"
mkdir -p "$build_lock_real"
ln -s "$build_lock_real" "$build_lock_link"
symlinked_build_lock="$build_lock_link/lunarwing-build.lock"
symlinked_upgrade_lock="${symlinked_build_lock}.alice.upgrade"
symlinked_env_exec_rc=0
symlinked_env_exec_out="$(
  export BUILD_LOCK="$symlinked_build_lock"
  exec 8>"$symlinked_upgrade_lock"
  flock -x 8
  as_unpriv bash "$ENV_EXEC_RUN" \
    --env-file "$tenant_home/lunarwing.env" -- "$env_exec_probe"
)" || symlinked_env_exec_rc=$?
exec 8>&-
if [[ "$symlinked_env_exec_rc" -eq 127 ]]; then
  skip "env-exec closes an upgrade lock reached through a symlink (no privilege-drop tool/user)"
elif [[ "$symlinked_env_exec_rc" -eq 0 && "$symlinked_env_exec_out" == "clean marker=present" ]]; then
  pass "env-exec closes an upgrade lock reached through a symlink"
else
  fail "env-exec closes an upgrade lock reached through a symlink (rc=$symlinked_env_exec_rc out='$symlinked_env_exec_out')"
fi
assert "symlinked upgrade lock is free after the env-exec probe" \
  flock -n "$symlinked_upgrade_lock" true

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
unrelated_rc=0
unrelated_out="$(
  exec 7<"$unrelated"
  exec 8>"$lock_file"
  flock -x 8
  as_unpriv bash "$ENV_EXEC_RUN" \
    --env-file "$tenant_home/lunarwing.env" -- "$unrelated_probe" 7<&7
)" || unrelated_rc=$?
exec 7<&-
exec 8>&-
if [[ "$unrelated_rc" -eq 127 ]]; then
  skip "env-exec keeps fds that do not point into a lock directory (no privilege-drop tool/user)"
elif [[ "$unrelated_rc" -eq 0 && "$unrelated_out" == "unrelated fd kept" ]]; then
  pass "env-exec keeps fds that do not point into a lock directory"
else
  fail "env-exec keeps fds that do not point into a lock directory (rc=$unrelated_rc out='$unrelated_out')"
fi

printf '\n'
if [[ "$failures" -eq 0 ]]; then
  printf 'ALL TESTS PASSED (%s skipped)\n' "$skips"
  exit 0
fi
printf '%s TEST(S) FAILED (%s skipped)\n' "$failures" "$skips"
exit 1
