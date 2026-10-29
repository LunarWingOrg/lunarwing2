#!/usr/bin/env bash
# WI#469: exercise the real start dispatcher, all four starters and lock wrapper.
# Do NOT call the code under test from `if`/`||`: that masks Bash errexit bugs.
# shellcheck disable=SC2034  # fixture globals are consumed by sourced helpers.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
TENANT_LIFECYCLE_LOCK_DIR="$fixture/locks"
events="$fixture/events"
output="$fixture/output"
failures=0
mkdir -p "$fixture/env" "$fixture/run/ssh-agent" "$fixture/workspace"
: >"$fixture/env/lunarwing.env"

pass() { printf 'PASS: %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*"; failures=$((failures + 1)); }
record() { printf '%s\n' "$*" >>"$events"; }
expect() { if "$@"; then pass "$*"; else fail "$*"; command cat "$output" >&2; fi; }

# Only the root-owned directory metadata is mocked, so the harness also runs
# unprivileged. The actual flock descriptors, inode checks and locks are real.
install() { command mkdir -p "$TENANT_LIFECYCLE_LOCK_DIR"; command chmod 700 "$TENANT_LIFECYCLE_LOCK_DIR"; }
stat() {
  if [[ "$*" == "-c %u:%a $TENANT_LIFECYCLE_LOCK_DIR" ]]; then printf '0:700\n';
  else command stat "$@"; fi
}
tenant_exists_in_registry() { return 0; }
tenant_env_dir() { printf '%s/env' "$fixture"; }
tenant_state_dir() { printf '%s/state' "$fixture"; }
tenant_run_dir() { printf '%s/run' "$fixture"; }
tenant_quadlet_dir() { printf '%s/quadlets' "$fixture"; }
tenant_darkirc_enabled() { return 1; }
tenant_uses_managed_sandbox_image() { return 1; }
retire_tenant_nanocode() { :; }
start_tenant_postgres() { :; }
start_tenant_vision() { :; }
start_tenant_voice() { :; }
start_tenant_systemd() { record daemon:systemd; }
start_tenant_openrc() { record daemon:openrc; }
warn_if_sshd_unreachable() { :; }
upload_tenant_darkirc_secret() { :; }
upload_tenant_brave_search_secret() { :; }
patch_weechat_caps() { :; }
_ssh_ready_summary() { record ssh-summary; }
stop_tenant() { record stop; }
stop_tenant_init_services() { record stop-init; }
clean_stale_pause_pid() { :; }
ensure_init_system() { :; }
ensure_container_runtime() { CONTAINER_RT=podman; MT_ROOTLESS=true; }
podman_supports_quadlet() { [[ "$failure_mode" == none ]]; }
_wait_user_manager() { :; }
tenant_worker_enabled() { [[ " $selected " == *" $2 "* ]]; }
tenant_worker_desired_running() { [[ "$2" != "$intentionally_stopped" ]]; }
ports_get() {
  [[ "$2" != "${unallocated}_wss" ]] || return 1
  printf '19091\n'
}
_ensure_tenant_image() { [[ "$2" != "lunarwing-worker-${missing_image}:latest" ]]; }
_worker_launch_contract() { printf fixture-contract; }
prepare_worker_egress() { WORKER_EGRESS_LAUNCH_ARGS=(); }
worker_egress_reconcile_container() { :; }
worker_egress_verify_after_start() { :; }
worker_egress_network_name() { printf fixture-egress; }
_openrc_podman_uses_init_health() { return 1; }
_container_config_changed() { return 1; }
_store_container_hash() { record "hash:$2"; }
_register_worker_unit() { record "unit:$2"; }
render_worker_quadlet() { record "render:$2"; }
_systemctl_user() { record "systemctl:$2:${3:-}"; }
prepare_worker_workspace() { printf '%s/workspace' "$fixture"; }
_snapshot_tenant_env() { local -n target="$3"; target="$(mktemp "$fixture/snapshot.XXXXXX")"; cp "$2" "$target"; }
prepare_worker_task_launch() {
  TASK_WORKSPACE_DIR="$fixture/workspace"
  TASK_USERNS_ARGS=() TASK_ENV_FLAGS=() TASK_MOUNTS=() TASK_RESTART_ARG=()
}
# Keep the real write_kimi_runtime_env: auth failure crosses its staging
# subshell and the starter's command-substitution assignment (QF-1).
read_tenant_env_value() { :; }
resolve_worker_auth_token() { [[ "$failure_mode" != auth ]] || return 1; printf fixture-token; }
write_tenant_file_atomic() { cat >"$2"; }
_ctr() {
  local lane verb="$2"
  # `_ctr_inspect` wraps this as `<runtime> container inspect ...`; keep the
  # fixture's single-verb dispatch working and pin that the typed subcommand is
  # what the real helper sends (a bare `inspect` would regress the volume-name
  # prefix collision the wrapper exists to avoid).
  if [[ "$verb" == container ]]; then
    shift 2
    verb="$1"
    [[ "$verb" == inspect ]] || fail "typed container wrapper used for '$verb'"
  fi
  case "$verb" in
    ps)
      [[ ! -e "$fixture/repaired" ]] || return 0
      printf 'Try running "podman system migrate"\n' >&2
      return 125 ;;
    system) touch "$fixture/repaired" ;;
    image)
      lane="${!#}"; lane="${lane#lunarwing-worker-}"; lane="${lane%:latest}"
      record "probe:$lane"
      if [[ " $refused " == *" $lane "* ]]; then printf '12345:12345\n';
      else local uid; uid="$(worker_task_uid "$lane")"; printf '%s:%s\n' "$uid" "$uid"; fi ;;
    inspect)
      [[ "$failure_mode" == none || "$*" != *lunarwing-kimi-alice* ]] || return 1
      printf 'true\n' ;;
    run)
      record run:kimi
      [[ "$failure_mode" != runtime ]] || return 125 ;;
  esac
  return 0
}

# Run exactly like the CLI: a plain callback through the real lock, with status
# captured by disabling errexit rather than putting a function in a conditional.
run_start() {
  : >"$events"
  set +e
  with_tenant_lifecycle_lock alice "$1" alice >"$output" 2>&1
  rc=$?
  [[ $- != *e* ]] || fail 'caller with errexit off was changed'
  set -e
  expect test -z "${LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD:-}"
  expect flock -n "$TENANT_LIFECYCLE_LOCK_DIR/alice.lock" true
}
selected='pebble opencode kimi opencode2'
intentionally_stopped=''
unallocated=''
missing_image=''
failure_mode=none
for INIT_SYSTEM in systemd openrc; do
  for verb in start_tenant restart_tenant; do
    refused='pebble kimi'
    run_start "$verb"
    expect test "$rc" = 1
    expect grep -q 'failed worker lanes: pebble kimi' "$output"
    expect test "$(grep -c WARNING "$output")" = 3
    expect grep -q 'probe:opencode2' "$events"
    expect grep -q 'probe:kimi' "$events"
    expect grep -q ssh-summary "$events"
    expect test "$(grep -n daemon: "$events" | head -1 | cut -d: -f1)" -lt \
      "$(grep -n probe: "$events" | head -1 | cut -d: -f1)"
  done

  # Deselection and OpenRC desired-running skips must still win over stale images.
  selected='pebble kimi'
  refused=opencode
  intentionally_stopped=kimi
  run_start start_tenant
  expect test "$rc" = 0
  expect test "$(grep -c probe:opencode "$events" || true)" = 0
  if [[ "$INIT_SYSTEM" == openrc ]]; then
    expect test "$(grep -c probe:kimi "$events" || true)" = 0
    expect grep -q 'intentionally stopped' "$output"
  else
    expect grep -q probe:kimi "$events"
  fi
  intentionally_stopped=''
  selected='pebble opencode kimi opencode2'
  refused=''
  for lane in pebble opencode kimi opencode2; do
    unallocated="$lane"
    run_start start_tenant
    expect test "$rc" = 0
    expect grep -q "no ${lane}_wss port allocated" "$output"
    unallocated=''
    missing_image="$lane"
    run_start start_tenant
    expect test "$rc" = 0
    expect grep -q "$lane worker image not available" "$output"
    missing_image=''
  done

  for failure_mode in runtime auth; do
    run_start start_tenant
    if [[ "$failure_mode" == runtime ]]; then expect test "$rc" = 125;
    else expect test "$rc" = 1; fi
    expect test "$(grep -Ec 'hash:lunarwing-kimi-alice$|unit:kimi|probe:opencode2' "$events" || true)" = 0
    expect test "$(grep -c 'kimi worker ready' "$output" || true)" = 0
    if [[ "$failure_mode" == auth ]]; then
      expect test "$(grep -c run:kimi "$events" || true)" = 0
    fi
  done
  failure_mode=none
done

# Recovery callers must not reintroduce an ignored-errexit context around start.
for INIT_SYSTEM in systemd openrc; do
  for failure_mode in runtime auth none; do
    rm -f "$fixture/repaired"
    run_start repair_tenant_podman_userns
    if [[ "$failure_mode" == none ]]; then expect test "$rc" = 0;
    else
      expect test "$rc" = 1  # repair's existing API maps a failed start to 1.
      expect test "$(grep -Ec 'hash:lunarwing-kimi-alice$|unit:kimi|probe:opencode2' "$events" || true)" = 0
      expect test "$(grep -c 'kimi worker ready' "$output" || true)" = 0
    fi
  done
done

# Fresh, externally inherited and nested-active locks must allow a callback's
# inner `set -e` to take effect and restore the caller's option state (QT-3).
callback() {
  ( set -e; ( exit "$callback_rc" ); if [[ "$callback_rc" != 0 ]]; then record forbidden-continuation; fi )
  return $?
}
nested_callback() {
  with_tenant_lifecycle_lock alice callback
  local result=$?
  [[ -n "${LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD:-}" ]] || return 99
  return "$result"
}
darkirc_writer_lock() {
  exec {writer_fd}>"$fixture/writer.lock"
  flock -x "$writer_fd"
}
darkirc_writer_unlock() { flock -u "$writer_fd"; exec {writer_fd}>&-; }
for lock_mode in fresh inherited nested writer; do
  for scenario in on:0 off:0 on:125 off:125; do
    option="${scenario%:*}"
    callback_rc="${scenario#*:}"
    : >"$events"
    set +e
    (
      if [[ "$option" == on ]]; then set -e; else set +e; fi
      trap 'printf "%s\n" "$-" >"$fixture/options"' EXIT
      if [[ "$lock_mode" == inherited ]]; then
        exec {inherited_fd}>"$TENANT_LIFECYCLE_LOCK_DIR/alice.lock"
        flock -x "$inherited_fd"
        LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_FD="$inherited_fd"
        LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_TENANT=alice
      fi
      if [[ "$lock_mode" == writer ]]; then
        with_darkirc_writer_lock alice callback
      elif [[ "$lock_mode" == nested ]]; then
        with_tenant_lifecycle_lock alice nested_callback
      else
        with_tenant_lifecycle_lock alice callback
      fi
      result=$?
      # Success, or a captured failure with errexit off: keep inherited ownership.
      if [[ "$lock_mode" == inherited ]]; then
        inherited_lock_fd_is_held "$inherited_fd" "$TENANT_LIFECYCLE_LOCK_DIR/alice.lock" || exit 98
      fi
      [[ -z "${LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD:-}" ]] || exit 97
      flock -n "$fixture/writer.lock" true || exit 96
      exit "$result"
    )
    rc=$?
    set -e
    expect test "$rc" = "$callback_rc"
    expect test ! -s "$events"
    flags="$(cat "$fixture/options")"
    if [[ "$option" == on ]]; then expect test "${flags//e/}" != "$flags";
    else expect test "${flags//e/}" = "$flags"; fi
    expect flock -n "$TENANT_LIFECYCLE_LOCK_DIR/alice.lock" true
  done
done

if [[ "$failures" -ne 0 ]]; then printf 'RESULT: %s failure(s)\n' "$failures"; exit 1; fi
printf 'RESULT: all worker startup tests passed\n'
