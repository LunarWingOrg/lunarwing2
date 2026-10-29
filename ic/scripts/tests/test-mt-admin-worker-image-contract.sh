#!/usr/bin/env bash
# Worker image rollout contract (mt-admin):
#
# 1. A rebuilt worker image must reach an EXISTING tenant container. The
#    tenant-store image ID is part of every worker container contract
#    (_worker_launch_contract), so start-tenant force-recreates the container
#    on the OpenRC/imperative path and on the Quadlet path when the ID changes,
#    and leaves a container that already runs the current image alone.
# 2. An mt-admin-built worker image must carry its source commit: every
#    build_*_worker passes --build-arg SOURCE_REVISION=<HEAD> (-dirty when the
#    worker directory has uncommitted or untracked changes, unknown outside a
#    git checkout).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# mt-admin redefines SCRIPT_DIR when sourced below, so capture the repo root
# (this file lives at <repo>/ic/scripts/tests/) before that happens.
HARNESS_REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

failures=0
fixture="$(mktemp -d)"
trap 'command rm -rf "$fixture"' EXIT

pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }
assert_eq() {  # <actual> <expected> <label>
  if [[ "$1" == "$2" ]]; then pass "$3"; else fail "$3 (got '$1', want '$2')"; fi
}
CALLS=()
has_call() {
  local wanted="$1" call
  for call in "${CALLS[@]}"; do
    [[ "$call" == "$wanted" ]] && return 0
  done
  return 1
}
has_call_like() {  # glob pattern
  local pattern="$1" call
  for call in "${CALLS[@]}"; do
    # shellcheck disable=SC2053  # the pattern is meant to glob
    [[ "$call" == $pattern ]] && return 0
  done
  return 1
}
assert_call() { if has_call "$1"; then pass "$2"; else fail "$2 (missing call: $1)"; fi; }
assert_no_call() { if has_call "$1"; then fail "$2 (unexpected call: $1)"; else pass "$2"; fi; }

# ── fake tenant container runtime ────────────────────────────────────────────
# IMAGE_ID is what the tenant store currently holds under the worker tag and
# IMAGE_USER answers the lanes' image-user guard. With TRACK_EXISTENCE=true the
# fake forgets the container on `rm -f` and recreates it on `run`, so the real
# create path runs; otherwise the container always "exists", which keeps the
# lanes whose create path needs more fixture off that path (the recreate
# decision under test is made before it).
IMAGE_ID="sha256:aaaa"
IMAGE_USER=""
TRACK_EXISTENCE="false"
# WI#424 control 1: the egress allowlist reads the tenant env through a
# tenant-side read and needs a real netns; it fails closed without either. The
# fixture tenant has no account, so stub the step (its own harness pins the
# behaviour, and the recreate decision under test happens before the worker
# start path runs the filter).
# shellcheck disable=SC2034  # real lane run lines expand this launch-argument array.
prepare_worker_egress() {
  WORKER_EGRESS_LAUNCH_ARGS=(--network "lw-egress-$1" --dns 127.0.0.1)
}
worker_egress_container_attached() { return 0; }
worker_egress_verify_after_start() { :; }
CONTAINER_EXISTS="true"
_ctr() {
  CALLS+=("ctr:$*")
  shift  # tenant name
  case "$*" in
    "image inspect -f {{.Id}} "*)
      [[ -n "$IMAGE_ID" ]] || return 1
      printf '%s\n' "$IMAGE_ID" ;;
    "image inspect --format {{.Config.User}} "*)
      printf '%s\n' "$IMAGE_USER" ;;
    "inspect -f {{.State.Running}} "*|"container inspect -f {{.State.Running}} "*)
      [[ "$CONTAINER_EXISTS" == "true" ]] || return 1
      printf 'true\n' ;;
    "inspect "*|"container inspect "*)
      [[ "$CONTAINER_EXISTS" == "true" ]] ;;
    "rm -f "*)
      [[ "$TRACK_EXISTENCE" != "true" ]] || CONTAINER_EXISTS="false" ;;
    "run "*)
      [[ "$TRACK_EXISTENCE" != "true" ]] || CONTAINER_EXISTS="true" ;;
    *) return 0 ;;
  esac
}

# ── _worker_launch_contract ──────────────────────────────────────────────────
assert_eq "$(_worker_launch_contract alice lunarwing-worker-kimi:latest "base;x")" \
  "base;x;egress=lw-egress-alice;image=sha256:aaaa" 'contract appends egress and the tenant-store image ID'
assert_eq "$(_worker_launch_contract alice lunarwing-worker-kimi:latest)" \
  "egress=lw-egress-alice;image=sha256:aaaa" 'empty base contract becomes egress plus image'
IMAGE_ID=""
assert_eq "$(_worker_launch_contract alice lunarwing-worker-kimi:latest "base;x")" \
  "base;x;egress=lw-egress-alice" 'uninspectable image keeps base plus the required egress migration'
IMAGE_ID="sha256:aaaa"

# ── start-tenant through the real lane functions ─────────────────────────────
ensure_container_runtime() {
  # shellcheck disable=SC2034  # read by the sourced lane functions
  CONTAINER_RT="podman"
  # shellcheck disable=SC2034
  MT_ROOTLESS="true"
}
podman_supports_quadlet() { return 0; }
say() { :; }
ports_get() { case "$2" in *_wss) printf '19090\n' ;; *) return 1 ;; esac; }
_ensure_tenant_image() { return 0; }
_openrc_podman_uses_init_health() { return 1; }
_register_worker_unit() { :; }
_wait_user_manager() { :; }
render_worker_quadlet() { :; }
_systemctl_user() { CALLS+=("systemctl:$*"); return 0; }
tenant_env_dir() { printf '%s/env\n' "$fixture"; }
tenant_home() { printf '%s/home\n' "$fixture"; }
tenant_quadlet_dir() { printf '%s/quadlets\n' "$fixture"; }
write_kimi_runtime_env() { printf '%s/env/kimi-runtime.env\n' "$fixture"; }
prepare_worker_workspace() { printf '%s/workspace\n' "$fixture"; }
mkdir -p "$fixture/env" "$fixture/home" "$fixture/quadlets" "$fixture/workspace"
env_file="$fixture/env/lunarwing.env"
printf 'GATEWAY_AUTH_TOKEN=fixture\n' >"$env_file"
: >"$fixture/env/kimi-runtime.env"
chmod 700 "$fixture/env"
chmod 600 "$env_file" "$fixture/env/kimi-runtime.env"
# WI#446 (run-174 LW-3): the config-hash sidecar lives in root-owned host
# state ($CONTAINER_HASH_DIR/<tenant>/), not the tenant home; the harness
# points it at the fixture.
# shellcheck disable=SC2034  # read by the sourced _hash_dir
CONTAINER_HASH_DIR="$fixture/container-hashes"
# Tenant-side unit/env writers in the exercised lanes drop privileges via
# `sudo -u`. The fixture tenant is not a real account (and CI runs
# no-new-privileges), so the drop maps onto the invoking user; `command`
# bypasses the harness's fake-runtime function stubs for the dropped call.
sudo() {
  if [[ "${1:-}" == "-u" ]]; then shift 2; fi
  command "$@"
}
# The opencode2 image-config policy drops to nobody via setpriv (run-221
# OK-1). The harness cannot change uid, so drop the setpriv options and run
# the wrapped command as the invoking user; test-mt-admin-opencode2.sh pins
# the drop itself.
setpriv() {
  while [[ $# -gt 0 && "$1" != -- ]]; do shift; done
  shift
  command "$@"
}

# WI#446 (run-174 LW-3): the sidecar directory must live OUTSIDE the tenant
# home (root-owned host state), and a stale sidecar left by an older mt-admin
# must stay updatable — the !1807 tenant-side writer could never update the
# old root-owned sidecars, so every start force-recreated the container.
wi446_hash_dir="$(_hash_dir alice)"
case "$wi446_hash_dir" in
  "$fixture/container-hashes/alice") pass "hash sidecar lives in host state, not the tenant home (WI#446)" ;;
  *) fail "hash sidecar lives in host state, not the tenant home (WI#446) (got: $wi446_hash_dir)" ;;
esac
case "$wi446_hash_dir" in
  "$(tenant_home alice)"/*) fail "hash sidecar path must not resolve under the tenant home (WI#446)" ;;
  *) pass "hash sidecar path does not resolve under the tenant home (WI#446)" ;;
esac
# A pre-existing (e.g. root-owned, from the old writer) sidecar is updated in
# place: after the store, the config no longer reports changed.
command mkdir -p "$wi446_hash_dir"
printf 'stale\n' >"$wi446_hash_dir/lunarwing-stale-probe-alice.hash"
_store_container_hash alice lunarwing-stale-probe-alice "$env_file" 2>/dev/null
if [[ "$(<"$wi446_hash_dir/lunarwing-stale-probe-alice.hash")" != "stale" ]] \
  && ! _container_config_changed alice lunarwing-stale-probe-alice "$env_file"; then
  pass "a stale sidecar is updated in place, not stuck (WI#446 LW-3)"
else
  fail "a stale sidecar is updated in place, not stuck (WI#446 LW-3)"
fi
command rm -rf "$wi446_hash_dir"

# The sidecars are private to root: a fresh store leaves the base and the
# per-tenant directory 0700 and the hash file 0600 whatever the caller's umask.
wi446_mode_dir="$(_hash_dir carol)"
(umask 022; _store_container_hash carol lunarwing-mode-probe-carol "$env_file" 2>/dev/null)
if [[ "$(stat -c %a "$CONTAINER_HASH_DIR")" == 700 && "$(stat -c %a "$wi446_mode_dir")" == 700 && "$(stat -c %a "$wi446_mode_dir/lunarwing-mode-probe-carol.hash")" == 600 ]]; then
  pass "hash sidecars are private to root (WI#446)"
else
  fail "hash sidecars are private to root (WI#446)"
fi
command rm -rf "$wi446_mode_dir"

base_contract() {  # <worker>: the rootless-podman base contract the lane uses
  case "$1" in
    kimi) printf '%s' "$KIMI_ROOTLESS_LAUNCH_CONTRACT" ;;
    opencode) printf '%s' "$OPENCODE_ROOTLESS_LAUNCH_CONTRACT" ;;
    opencode2) printf '%s' "$OPENCODE2_ROOTLESS_LAUNCH_CONTRACT" ;;
    pebble) printf '%s' "$PEBBLE_ROOTLESS_LAUNCH_CONTRACT" ;;
  esac
}
image_user_for() {  # <worker>: the lane's real image Config.User
  # WI#454: every image defaults to its task uid; mt-admin selects the
  # distinct non-root bridge uid explicitly for the public container.
  case "$1" in
    kimi) printf '%s:%s' "$KIMI_CONTAINER_UID" "$KIMI_CONTAINER_GID" ;;
    opencode) printf '%s:%s' "$OPENCODE_CONTAINER_UID" "$OPENCODE_CONTAINER_GID" ;;
    opencode2) printf '%s:%s' "$OPENCODE2_CONTAINER_UID" "$OPENCODE2_CONTAINER_GID" ;;
    pebble) printf '%s:%s' "$PEBBLE_CONTAINER_UID" "$PEBBLE_CONTAINER_GID" ;;
    *) printf '' ;;
  esac
}
use_init() {  # <systemd|openrc>
  local init="$1"
  eval "ensure_init_system() { INIT_SYSTEM=\"$init\"; }"
}

missing_worker_port_skip() {
  local worker="$1" init="$2" rc
  local output="$fixture/missing-port-${init}-${worker}.out"
  local calls="$fixture/missing-port-${init}-${worker}.calls"
  : >"$calls"
  set +e
  (
    set -e
    use_init "$init"
    ensure_init_system
    ports_get() { return 1; }
    say() { printf '%s\n' "$*"; }
    _ctr() { printf 'ctr:%s\n' "$*" >>"$calls"; }
    _systemctl_user() { printf 'systemctl:%s\n' "$*" >>"$calls"; }
    _ensure_tenant_image() { printf 'image:%s\n' "$*" >>"$calls"; }
    prepare_worker_egress() { printf 'egress:%s\n' "$*" >>"$calls"; }
    render_worker_quadlet() { printf 'render:%s\n' "$*" >>"$calls"; }
    _register_worker_unit() { printf 'unit:%s\n' "$*" >>"$calls"; }
    "start_tenant_${worker}" alice
  ) >"$output" 2>&1
  rc=$?
  set -e
  assert_eq "$rc" 0 "$init $worker missing port: direct errexit start succeeds (WI#513)"
  assert_eq "$(<"$output")" \
    "no ${worker}_wss port allocated for alice (skipping $worker worker)" \
    "$init $worker missing port: exact skip message (WI#513)"
  if [[ ! -s "$calls" ]]; then
    pass "$init $worker missing port: no runtime, image, egress or unit calls (WI#513)"
  else
    fail "$init $worker missing port: no runtime, image, egress or unit calls (WI#513) (got '$(<"$calls")')"
  fi
}
for skip_init in systemd openrc; do
  for worker in pebble opencode kimi opencode2; do
    missing_worker_port_skip "$worker" "$skip_init"
  done
done

# Seed the stored hash exactly as a container created from image A leaves it,
# then prove the next start-tenant is quiet on A and force-recreates on B.
imperative_scenario() {  # <worker>
  local worker="$1" container="lunarwing-${1}-alice" base
  base="$(base_contract "$worker")"
  IMAGE_USER="$(image_user_for "$worker")"
  use_init openrc
  command rm -rf "$(_hash_dir alice)"

  IMAGE_ID="sha256:aaaa"
  _store_container_hash alice "$container" "$env_file" \
    "$(_worker_launch_contract alice "lunarwing-worker-${worker}:latest" "$base")"
  CALLS=()
  "start_tenant_${worker}" alice
  assert_no_call "ctr:alice rm -f $container" "$worker imperative: same image keeps the running container"

  IMAGE_ID="sha256:bbbb"
  CALLS=()
  "start_tenant_${worker}" alice
  assert_call "ctr:alice stop $container" "$worker imperative: rebuilt image stops the old container"
  assert_call "ctr:alice rm -f $container" "$worker imperative: rebuilt image removes the old container"
}
for worker in kimi opencode opencode2 pebble; do
  imperative_scenario "$worker"
done

# kimi end to end: the recreate is followed by a fresh `run` from the rebuilt
# image, and the hash stored by that create carries the new image ID, so a
# further start-tenant on the same rebuilt image is quiet again.
TRACK_EXISTENCE="true"
CONTAINER_EXISTS="true"
IMAGE_USER="$(image_user_for kimi)"
use_init openrc
command rm -rf "$(_hash_dir alice)"
IMAGE_ID="sha256:aaaa"
_store_container_hash alice lunarwing-kimi-alice "$env_file" \
  "$(_worker_launch_contract alice lunarwing-worker-kimi:latest "$KIMI_ROOTLESS_LAUNCH_CONTRACT")"
IMAGE_ID="sha256:bbbb"
CALLS=()
start_tenant_kimi alice
assert_call "ctr:alice rm -f lunarwing-kimi-alice" 'kimi e2e: rebuilt image removes the old container'
if has_call_like "ctr:alice run -d --name lunarwing-kimi-alice * lunarwing-worker-kimi:latest --mode websocket"; then
  pass 'kimi e2e: a fresh container is created from the rebuilt image'
else
  fail 'kimi e2e: a fresh container is created from the rebuilt image'
fi
CALLS=()
start_tenant_kimi alice
assert_no_call "ctr:alice rm -f lunarwing-kimi-alice" 'kimi e2e: start-tenant on the rebuilt image is quiet again'
TRACK_EXISTENCE="false"
CONTAINER_EXISTS="true"

# Upgrade path: a hash stored by the previous mt-admin (no image segment)
# recreates once on the first start-tenant — the documented one-time recreate.
IMAGE_ID="sha256:aaaa"
command rm -rf "$(_hash_dir alice)"
_store_container_hash alice lunarwing-kimi-alice "$env_file" "$KIMI_ROOTLESS_LAUNCH_CONTRACT"
CALLS=()
start_tenant_kimi alice
assert_call "ctr:alice rm -f lunarwing-kimi-alice" 'legacy stored hash (no image segment) recreates once after upgrade'

# Quadlet path: the unit is bounced (stop + rm) only when the image changed,
# and the hash refreshed by the successful start keeps the next start quiet.
quadlet_scenario() {  # <worker>
  local worker="$1" container="lunarwing-${1}-alice" base quadlet_file
  base="$(base_contract "$worker")"
  quadlet_file="$fixture/quadlets/${container}.container"
  IMAGE_USER="$(image_user_for "$worker")"
  use_init systemd
  printf '[Container]\nImage=lunarwing-worker-%s:latest\n' "$worker" >"$quadlet_file"
  command rm -rf "$(_hash_dir alice)"

  IMAGE_ID="sha256:aaaa"
  _store_container_hash alice "$container" "$quadlet_file" \
    "$(_worker_launch_contract alice "lunarwing-worker-${worker}:latest" "$base")"
  CALLS=()
  "start_tenant_${worker}" alice
  assert_no_call "systemctl:alice stop ${container}.service" "$worker quadlet: same image does not bounce the unit"
  assert_call "systemctl:alice start ${container}.service" "$worker quadlet: unit started"

  IMAGE_ID="sha256:bbbb"
  CALLS=()
  "start_tenant_${worker}" alice
  assert_call "systemctl:alice stop ${container}.service" "$worker quadlet: rebuilt image stops the unit"
  assert_call "ctr:alice rm -f $container" "$worker quadlet: rebuilt image removes the old container"

  CALLS=()
  "start_tenant_${worker}" alice
  assert_no_call "systemctl:alice stop ${container}.service" "$worker quadlet: hash refreshed by the recreate, next start is quiet"
}
for worker in kimi opencode opencode2 pebble; do
  quadlet_scenario "$worker"
done

# /app stays root-owned and read-only to both non-root identities. A
# COPY --chown would hand executable bridge code to a writable uid.
for lane in kimicode4lunarwing opencode4lunarwing pebble4lunarwing; do
  dockerfile="$HARNESS_REPO_ROOT/$lane/Dockerfile"
  case "$lane" in
    kimicode4lunarwing) task_uid=10002; bridge_uid=10102 ;;
    opencode4lunarwing) task_uid=10001; bridge_uid=10101 ;;
    pebble4lunarwing) task_uid=10004; bridge_uid=10104 ;;
  esac
  if [[ ! -f "$dockerfile" ]]; then
    fail "$lane Dockerfile missing (WI#400 LD-2 pin)"
    continue
  fi
  if grep -Eq '^COPY --chown=' "$dockerfile"; then
    fail "$lane: COPY --chown hands /app files to a runtime uid (WI#400 LD-2)"
  else
    pass "$lane: COPYed /app files stay root-owned (WI#400 LD-2)"
  fi
  if grep -Eq "^USER ${task_uid}:${task_uid}[[:space:]]*$" "$dockerfile" \
      && grep -Eq "useradd --uid ${bridge_uid} " "$dockerfile"; then
    pass "$lane: Dockerfile pins task default and distinct bridge uid (WI#454)"
  else
    fail "$lane: Dockerfile must pin task uid ${task_uid} and bridge uid ${bridge_uid} (WI#454)"
  fi
done

# WI#400 LP-1): the opencode bridge is container root and bun loads
# bunfig.toml/.env from its working directory, so the entrypoint must leave
# the task-writable workspace before exec-ing the bridge.
oc_entrypoint="$HARNESS_REPO_ROOT/opencode4lunarwing/entrypoint.sh"
if awk '
  /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
  /^[[:space:]]*exec bun run \/app\/scripts\/lunarwing_bridge\.ts/ { found = 1; if (prev ~ /^[[:space:]]*cd \/app[[:space:]]*$/) ok = 1 }
  { prev = $0 }
  END { exit (found && ok) ? 0 : 1 }
' "$oc_entrypoint"; then
  pass 'opencode entrypoint: the bridge is exec-ed from the root-owned /app, not the workspace (WI#400 LP-1)'
else
  fail 'opencode entrypoint: exec bun run of the bridge must be preceded by cd /app (WI#400 LP-1)'
fi

# WI#400 / WI#469 / WI#454: image-user guards refuse stale images with the
# lane-local status 3 before any start/render/hash operation. Display
# tenant-store Config.User as bounded, escaped DATA, never a printf format.
image_user_refusal() {  # <worker> <stale Config.User>
  local worker="$1" stale="$2" out rc display expected unexpected="$fixture/refusal-unexpected"
  case "$worker" in
    kimi) expected="$KIMI_CONTAINER_UID:$KIMI_CONTAINER_GID" ;;
    opencode) expected="$OPENCODE_CONTAINER_UID:$OPENCODE_CONTAINER_GID" ;;
    opencode2) expected="$OPENCODE2_CONTAINER_UID:$OPENCODE2_CONTAINER_GID" ;;
    pebble) expected="$PEBBLE_CONTAINER_UID:$PEBBLE_CONTAINER_GID" ;;
  esac
  IMAGE_USER="$stale"
  display="$(sanitize_display_value "${stale:-unset}")"
  : >"$unexpected"
  set +e
  out="$(
    _ctr() {
      case "$2 $3" in
        'image inspect')
          if [[ "$*" == *'{{.Config.User}}'* ]]; then printf '%s\n' "$IMAGE_USER";
          else printf '%s\n' "$IMAGE_ID"; fi ;;
        *) printf 'unexpected runtime call\n' >>"$unexpected" ;;
      esac
    }
    render_worker_quadlet() { printf 'render\n' >>"$unexpected"; }
    "start_tenant_${worker}" alice 2>&1
  )"
  rc=$?
  set -e
  if [[ "$rc" == 3 && "$out" == *"image has user '$display', expected $expected (WI#454 task default)"* \
      && "$out" == *"rebuild it" && "$out" != *$'\033'* && "${#out}" -lt 600 \
      && ! -s "$unexpected" ]]; then
    pass "$INIT_SYSTEM $worker safely refuses image user '$display' with status 3"
  else
    fail "$INIT_SYSTEM $worker unsafe/incorrect image refusal (status $rc)"
  fi
  IMAGE_USER="$(image_user_for "$worker")"
}
for refusal_init in openrc systemd; do
  use_init "$refusal_init"
  ensure_init_system
  for worker in kimi pebble opencode opencode2; do
    for stale_user in '12345:12345' '' 'x%sy%dz' '%099999d' '\033[2J' $'\033[2J\nforged\rline' \
        "$(printf '%05000d' 0)"; do
      image_user_refusal "$worker" "$stale_user"
    done
  done
done
use_init openrc
IMAGE_USER="$(image_user_for pebble)"
image_user_refusal kimi "0:0"
image_user_refusal kimi ""
image_user_refusal pebble "0:0"
image_user_refusal pebble ""


# WI#423 (run-171, the merge with !1784): the rootful create path below is the
# one scenario in this harness that reaches the tenant-side credential reader,
# which drops to the tenant with `sudo -u`. The mapping stub is defined once
# in the fixture setup above (WI#446 moved it there so the config-hash
# sidecar's tenant-side writes are covered too); the fixture env takes the
# production shape the tenant-side checks demand: 0700 dir, 0600 file, the
# scoped worker credential present.
chmod 700 "$fixture/env"
chmod 600 "$env_file"
printf 'WORKER_AUTH_TOKEN=fixture-worker\n' >>"$env_file"

# WI#454: rootful Pebble uses the same non-root pair as rootless.
prepare_pebble_workspace() { printf '%s/pebble-workspace\n' "$fixture"; }
ensure_container_runtime() {
  # shellcheck disable=SC2034  # read by the sourced lane functions
  CONTAINER_RT="docker"
  # shellcheck disable=SC2034
  MT_ROOTLESS="false"
}
use_init openrc
TRACK_EXISTENCE="true"
CONTAINER_EXISTS="true"
command rm -rf "$(_hash_dir alice)"
IMAGE_ID="sha256:aaaa"

if [[ "$PEBBLE_ROOTFUL_LAUNCH_CONTRACT" != "$PEBBLE_ROOTLESS_LAUNCH_CONTRACT" ]]; then
  pass 'pebble contract: the rootful base contract differs from the rootless one'
else
  fail 'pebble contract: rootful and rootless base contracts are identical'
fi

_store_container_hash alice lunarwing-pebble-alice "$env_file" \
  "$(_worker_launch_contract alice lunarwing-worker-pebble:latest "$PEBBLE_ROOTLESS_LAUNCH_CONTRACT")"
CALLS=()
start_tenant_pebble alice
assert_call "ctr:alice rm -f lunarwing-pebble-alice" 'rootful pebble: pre-split container is force-recreated'
for pin in \
  'lunarwing-pebble-alice *--user 10104:10004 *-e LUNARWING_CONTAINER_ROLE=split-bridge * lunarwing-worker-pebble:latest' \
  'lunarwing-pebble-task-alice *--user 10004:10004 *-e LUNARWING_CONTAINER_ROLE=split-task * lunarwing-worker-pebble:latest' \
  'lunarwing-pebble-alice *--security-opt no-new-privileges *--cap-drop ALL * lunarwing-worker-pebble:latest' \
  "lunarwing-pebble-alice *-e LUNARWING_ROOTFUL_WORKSPACE=1 -e LUNARWING_WORKSPACE_ID=$(stat -c '%d:%i' -- "$fixture/workspace") *" \
  "lunarwing-pebble-task-alice *-e LUNARWING_ROOTFUL_WORKSPACE=1 -e LUNARWING_WORKSPACE_ID=$(stat -c '%d:%i' -- "$fixture/workspace") *" \
  'lunarwing-pebble-task-alice *lunarwing-pebble-alice-ipc-v2:/run/lunarwing-shared:z*'; do
  if has_call_like "ctr:alice run -d --name ${pin}"; then
    pass "rootful pebble: $pin"
  else
    fail "rootful pebble: missing launch pin: $pin"
  fi
done
if has_call_like "ctr:alice run -d --name lunarwing-pebble-alice *--cap-add *"; then
  fail 'rootful pebble: bridge must not regain capabilities'
else
  pass 'rootful pebble: bridge has no added capabilities'
fi
if has_call_like "ctr:alice run -d --name lunarwing-pebble-alice *--userns *" \
    || has_call_like "ctr:alice run -d --name lunarwing-pebble-alice *PEBBLE_TASK_UID=inherit*"; then
  fail 'rootful pebble: rootless userns and inherit identity are forbidden'
else
  pass 'rootful pebble: no userns mapping or inherited identity'
fi

CALLS=()
start_tenant_pebble alice
assert_no_call "ctr:alice rm -f lunarwing-pebble-alice" 'rootful pebble: refreshed pair is quiet on the next start'

ensure_container_runtime() {
  # shellcheck disable=SC2034  # read by the sourced lane functions
  CONTAINER_RT="podman"
  # shellcheck disable=SC2034
  MT_ROOTLESS="true"
}
command rm -rf "$(_hash_dir alice)"
CALLS=()
start_tenant_pebble alice
for pin in \
  'lunarwing-pebble-alice *--userns keep-id:uid=10004,gid=10004 *--user 10104:10004 * lunarwing-worker-pebble:latest' \
  'lunarwing-pebble-task-alice *--userns keep-id:uid=10004,gid=10004 *--user 10004:10004 * lunarwing-worker-pebble:latest'; do
  if has_call_like "ctr:alice run -d --name ${pin}"; then
    pass "rootless pebble: $pin"
  else
    fail "rootless pebble: missing launch pin: $pin"
  fi
done
if has_call_like "ctr:alice run -d --name lunarwing-pebble-alice *-e HOME=/workspace *" \
    || has_call_like "ctr:alice run -d --name lunarwing-pebble-alice *PEBBLE_TASK_UID=inherit*"; then
  fail 'rootless pebble: legacy tenant toolchain or inherit identity must stay retired'
else
  pass 'rootless pebble: keep-id split has no legacy tenant identity overrides'
fi
unset -f prepare_pebble_workspace
TRACK_EXISTENCE="false"
CONTAINER_EXISTS="true"

# ── SOURCE_REVISION stamping ─────────────────────────────────────────────────
repo="$fixture/src"
for dir in kimicode4lunarwing opencode4lunarwing opencode2.4lunarwing/scripts pebble4lunarwing .gitlab/scripts; do
  mkdir -p "$repo/$dir"
done
for dir in kimicode4lunarwing opencode4lunarwing opencode2.4lunarwing pebble4lunarwing; do
  printf 'FROM scratch\n' >"$repo/$dir/Dockerfile"
done
printf '#!/bin/sh\n' >"$repo/opencode2.4lunarwing/scripts/opencode-exposure-guard.sh"
chmod 755 "$repo/opencode2.4lunarwing/scripts/opencode-exposure-guard.sh"
cp -a "$HARNESS_REPO_ROOT/opencode2.4lunarwing/scripts/." "$repo/opencode2.4lunarwing/scripts/"
cp -a "$HARNESS_REPO_ROOT/opencode2.4lunarwing/config" "$repo/opencode2.4lunarwing/"
cp -a "$HARNESS_REPO_ROOT/opencode2.4lunarwing/entrypoint.sh" \
  "$HARNESS_REPO_ROOT/opencode2.4lunarwing/entrypoint-split.sh" \
  "$HARNESS_REPO_ROOT/opencode2.4lunarwing/health_server.py" \
  "$HARNESS_REPO_ROOT/opencode2.4lunarwing/agent_comm_protocol.json" \
  "$repo/opencode2.4lunarwing/"
cp -a "$HARNESS_REPO_ROOT/.gitlab/scripts/verify-opencode2-exposure-out-of-band.py" \
  "$repo/.gitlab/scripts/"
printf '\x7fELF clean pinned build\n' >"$fixture/real-opencode"
chmod 755 "$fixture/real-opencode"
opencode2_rootfs="$fixture/opencode2-rootfs"
mkdir -p "$opencode2_rootfs/app/node_modules" "$opencode2_rootfs/usr/bin" \
  "$opencode2_rootfs/usr/local/bin" "$opencode2_rootfs/usr/local/lib/opencode2"
cp -a "$repo/opencode2.4lunarwing/scripts" "$opencode2_rootfs/app/scripts"
cp -a "$repo/opencode2.4lunarwing/config" "$opencode2_rootfs/app/config"
cp -a "$repo/opencode2.4lunarwing/entrypoint.sh" \
  "$repo/opencode2.4lunarwing/entrypoint-split.sh" \
  "$repo/opencode2.4lunarwing/health_server.py" \
  "$repo/opencode2.4lunarwing/agent_comm_protocol.json" "$opencode2_rootfs/app/"
cp -a "$repo/opencode2.4lunarwing/scripts/opencode-exposure-guard.sh" \
  "$opencode2_rootfs/usr/local/bin/opencode"
cp -a "$fixture/real-opencode" "$opencode2_rootfs/usr/local/lib/opencode2/opencode"
cp -a "$fixture/real-opencode" "$opencode2_rootfs/usr/bin/tini"
find "$opencode2_rootfs" -type d -exec chmod 755 {} +
find "$opencode2_rootfs" -type f -exec chmod 644 {} +
chmod 755 "$opencode2_rootfs/app/entrypoint.sh" \
  "$opencode2_rootfs/usr/local/bin/opencode" \
  "$opencode2_rootfs/usr/local/lib/opencode2/opencode" \
  "$opencode2_rootfs/usr/bin/tini"
printf 'fixture\n' >"$repo/README"
git_fx=(git -C "$repo" -c user.name=fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false)
"${git_fx[@]}" init -q
"${git_fx[@]}" add -A
"${git_fx[@]}" commit -q -m fixture
head="$("${git_fx[@]}" rev-parse HEAD)"

assert_eq "$(_worker_source_revision "$repo" kimicode4lunarwing)" "$head" 'clean checkout reports HEAD'
printf 'scratch\n' >"$repo/kimicode4lunarwing/untracked"
assert_eq "$(_worker_source_revision "$repo" kimicode4lunarwing)" "${head}-dirty" 'untracked file under the worker dir marks -dirty'
assert_eq "$(_worker_source_revision "$repo" opencode4lunarwing)" "$head" 'a change outside the worker dir leaves it clean'
command rm -f "$repo/kimicode4lunarwing/untracked"
printf 'FROM scratch\nLABEL fixture=1\n' >"$repo/kimicode4lunarwing/Dockerfile"
assert_eq "$(_worker_source_revision "$repo" kimicode4lunarwing)" "${head}-dirty" 'modified tracked file marks -dirty'
"${git_fx[@]}" checkout -q -- kimicode4lunarwing/Dockerfile
mkdir -p "$fixture/nogit/kimicode4lunarwing"
assert_eq "$(GIT_CEILING_DIRECTORIES="$fixture" _worker_source_revision "$fixture/nogit" kimicode4lunarwing)" \
  "unknown" 'non-git root reports unknown'

# The retry-as-owner path needs root and another account; pin its presence.
# shellcheck disable=SC2016  # literal source text, not an expansion
if [[ "$(declare -f _worker_source_git)" == *'sudo -u "$owner" git -C "$root"'* ]]; then
  pass 'source lookup retries as the checkout owner when root is refused'
else
  fail 'source lookup retries as the checkout owner when root is refused'
fi

fake_worker_runtime() {
  local runtime="$1" previous='' argument
  shift
  CALLS+=("$runtime:$*")
  case "${1:-}" in
    build)
      for argument in "$@"; do
        if [[ "$previous" == --iidfile ]]; then
          printf 'sha256:%064d\n' 0 > "$argument"
        fi
        previous="$argument"
      done ;;
    create) printf 'contractcid\n' ;;
    export)
      [[ "${2:-}" == --output && "${4:-}" == contractcid ]] || return 1
      python3 - "$opencode2_rootfs" "$3" <<'PY'
import pathlib
import sys
import tarfile
root, output = pathlib.Path(sys.argv[1]), sys.argv[2]
with tarfile.open(output, "w") as archive:
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        info = archive.gettarinfo(str(path), arcname=relative)
        info.uid = info.gid = 0
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
    image)
      [[ "${2:-}" == inspect && $# -eq 3 ]] || return 1
      python3 - "$runtime" <<'PY'
import json
import sys
runtime = sys.argv[1]
config = {
    "Env": [
        "PATH=/home/opencode2/.cargo/bin:/usr/local/cargo/bin:/usr/local/go/bin:/home/opencode2/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/local/bun-node-fallback-bin",
        "BUN_RUNTIME_TRANSPILER_CACHE_PATH=0",
        "BUN_INSTALL_BIN=/usr/local/bin",
        "OPENCODE_IMAGE_REF=v2.1.2@" + "a" * 40,
        "OPENCODE_DISABLE_AUTOUPDATE=1",
        "RUSTUP_HOME=/usr/local/rustup",
        "GOROOT=/usr/local/go",
        "GOPATH=/home/opencode2/go",
        "CARGO_HOME=/home/opencode2/.cargo",
        "OPENCODE2_TASK_UID=10003",
        "OPENCODE2_TASK_GID=10003",
        "OPENCODE2_BRIDGE_UID=10103",
    ],
    "Entrypoint": ["/usr/bin/tini", "--", "/app/entrypoint.sh"],
    "Cmd": ["--mode", "websocket"],
    "User": "10003:10003",
    "WorkingDir": "/app",
    "ExposedPorts": {"8443/tcp": {}, "9090/tcp": {}},
    "StopSignal": "SIGTERM",
    "Shell": None,
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
  esac
  return 0
}
podman() { fake_worker_runtime podman "$@"; }
docker() { fake_worker_runtime docker "$@"; }
build_arg_seen() {  # <image tag> <SOURCE_REVISION value>
  local call
  for call in "${CALLS[@]}"; do
    [[ "$call" == "podman:build "* && "$call" == *" -t $1 "* \
      && "$call" == *" --build-arg SOURCE_REVISION=$2 "* ]] && return 0
  done
  return 1
}
# The non-root harness cannot chown the verifier's scratch work dir to nobody;
# production keeps that ownership transition mandatory.
chown() { :; }
CALLS=()
build_kimi_worker false false "" "$repo"
build_opencode_worker false false "$repo"
build_opencode2_worker false false "$repo"
LUNARWING_ROOT="$repo" build_pebble_worker false false
for worker in kimi opencode opencode2 pebble; do
  build_tag="lunarwing-worker-${worker}:latest"
  [[ "$worker" != opencode2 ]] || build_tag="lunarwing-worker-opencode2:candidate-$$"
  if build_arg_seen "$build_tag" "$head"; then
    pass "build-${worker}-worker stamps SOURCE_REVISION=HEAD"
  else
    fail "build-${worker}-worker stamps SOURCE_REVISION=HEAD"
  fi
done
expected_image_id="sha256:$(printf '%064d' 0)"
assert_call "podman:tag $expected_image_id lunarwing-worker-opencode2:latest" \
  'opencode2 promotion tags the verified immutable ID'
printf 'FROM scratch\nLABEL fixture=2\n' >"$repo/kimicode4lunarwing/Dockerfile"
CALLS=()
build_kimi_worker false false "" "$repo"
if build_arg_seen lunarwing-worker-kimi:latest "${head}-dirty"; then
  pass 'dirty worker dir stamps SOURCE_REVISION=HEAD-dirty'
else
  fail 'dirty worker dir stamps SOURCE_REVISION=HEAD-dirty'
fi

workspace_identity="$(stat -c '%d:%i' -- "$fixture/workspace")"
mkdir -p "$fixture/other-workspace"
ln -s "$fixture/other-workspace" "$fixture/redirected-workspace"
for launcher in kimicode4lunarwing/entrypoint-split.sh opencode4lunarwing/entrypoint-split.sh \
    opencode2.4lunarwing/entrypoint.sh pebble4lunarwing/entrypoint.sh; do
  for mount_path in workspace redirected-workspace; do
    output="$(env LUNARWING_ROOTFUL_WORKSPACE=1 LUNARWING_WORKSPACE_ID="$workspace_identity" \
      LUNARWING_CONTAINER_ROLE=split-task WORKSPACE_ROOT="$fixture/$mount_path" \
      KIMI_TASK_UID=59990 KIMI_BRIDGE_UID=59991 OPENCODE_TASK_UID=59990 OPENCODE_BRIDGE_UID=59991 \
      OPENCODE2_TASK_UID=59990 PEBBLE_TASK_UID=59990 \
      bash "$HARNESS_REPO_ROOT/$launcher" 2>&1)" && result=0 || result=$?
    expected='split-task must run as uid'
    [[ "$mount_path" == workspace ]] || expected='workspace mount changed identity'
    if [[ "$result" -ne 0 && "$output" == *"$expected"* ]]; then
      pass "$launcher checks $mount_path before task setup"
    else
      fail "$launcher did not enforce $mount_path identity ($result: $output)"
    fi
  done
done

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all worker image contract tests passed\n'
