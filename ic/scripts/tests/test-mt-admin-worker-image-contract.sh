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
    "inspect -f {{.State.Running}} "*)
      [[ "$CONTAINER_EXISTS" == "true" ]] || return 1
      printf 'true\n' ;;
    "inspect "*)
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
  "base;x;image=sha256:aaaa" 'contract appends the tenant-store image ID'
assert_eq "$(_worker_launch_contract alice lunarwing-worker-kimi:latest)" \
  "image=sha256:aaaa" 'empty base contract becomes the image segment alone'
IMAGE_ID=""
assert_eq "$(_worker_launch_contract alice lunarwing-worker-kimi:latest "base;x")" \
  "base;x" 'uninspectable image keeps the base contract (no forced recreate)'
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

base_contract() {  # <worker>: the rootless-podman base contract the lane uses
  case "$1" in
    kimi) printf '%s' "$KIMI_ROOTLESS_LAUNCH_CONTRACT" ;;
    opencode) printf '%s' "$OPENCODE_ROOTLESS_LAUNCH_CONTRACT" ;;
    opencode2) printf '%s' "$OPENCODE2_ROOTLESS_LAUNCH_CONTRACT" ;;
    pebble) printf '%s' "$PEBBLE_ROOTLESS_LAUNCH_CONTRACT" ;;
  esac
}
image_user_for() {  # <worker>: the lane's real image Config.User
  # Feed each lane the user its current image actually carries so each
  # image-user guard is checked against its own expectation: WI#400 migrated
  # the kimi, opencode and pebble lanes to a root bridge (USER 0:0) whose
  # serve/tasks drop to the task uid, while the opencode2 lane is NOT
  # migrated (its image still carries the task uid directly) — its guard
  # must keep accepting exactly $OPENCODE2_CONTAINER_UID[:GID].
  case "$1" in
    kimi|opencode|pebble) printf '0:0' ;;
    opencode2) printf '%s:%s' "$OPENCODE2_CONTAINER_UID" "$OPENCODE2_CONTAINER_GID" ;;
    *) printf '' ;;
  esac
}
use_init() {  # <systemd|openrc>
  local init="$1"
  eval "ensure_init_system() { INIT_SYSTEM=\"$init\"; }"
}

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

# ── WI#400 LD-2: /app files must stay root-owned in the USER 0:0 lanes ───────
# Container root executes the /app entrypoint, scripts and config, so files
# the task uid can rewrite would run task-authored code as root on the next
# container start (restart policy, daemon restart, health restart). A
# COPY --chown=<task user> hands the tree over at build time — refuse it.
for lane in kimicode4lunarwing opencode4lunarwing pebble4lunarwing; do
  dockerfile="$HARNESS_REPO_ROOT/$lane/Dockerfile"
  if [[ ! -f "$dockerfile" ]]; then
    fail "$lane Dockerfile missing (WI#400 LD-2 pin)"
    continue
  fi
  if grep -Eq '^COPY --chown=' "$dockerfile"; then
    fail "$lane: COPY --chown hands /app files to the task uid (WI#400 LD-2)"
  else
    pass "$lane: COPYed /app files stay root-owned (WI#400 LD-2)"
  fi
  if grep -Eq '^USER 0:0[[:space:]]*$' "$dockerfile"; then
    pass "$lane: Dockerfile pins the root bridge user (WI#400)"
  else
    fail "$lane: Dockerfile must set USER 0:0 for the root bridge (WI#400)"
  fi
done

# ── SOURCE_REVISION stamping ─────────────────────────────────────────────────
repo="$fixture/src"
for dir in kimicode4lunarwing opencode4lunarwing opencode2.4lunarwing/scripts pebble4lunarwing; do
  mkdir -p "$repo/$dir"
done
for dir in kimicode4lunarwing opencode4lunarwing opencode2.4lunarwing pebble4lunarwing; do
  printf 'FROM scratch\n' >"$repo/$dir/Dockerfile"
done
printf '#!/bin/sh\n' >"$repo/opencode2.4lunarwing/scripts/opencode-exposure-guard.sh"
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

podman() { CALLS+=("podman:$*"); return 0; }
docker() { CALLS+=("docker:$*"); return 0; }
build_arg_seen() {  # <image tag> <SOURCE_REVISION value>
  local call
  for call in "${CALLS[@]}"; do
    [[ "$call" == "podman:build "* && "$call" == *" -t $1 "* \
      && "$call" == *" --build-arg SOURCE_REVISION=$2 "* ]] && return 0
  done
  return 1
}
CALLS=()
build_kimi_worker false false "" "$repo"
build_opencode_worker false false "$repo"
build_opencode2_worker false false "$repo"
LUNARWING_ROOT="$repo" build_pebble_worker false false
for worker in kimi opencode opencode2 pebble; do
  if build_arg_seen "lunarwing-worker-${worker}:latest" "$head"; then
    pass "build-${worker}-worker stamps SOURCE_REVISION=HEAD"
  else
    fail "build-${worker}-worker stamps SOURCE_REVISION=HEAD"
  fi
done
printf 'FROM scratch\nLABEL fixture=2\n' >"$repo/kimicode4lunarwing/Dockerfile"
CALLS=()
build_kimi_worker false false "" "$repo"
if build_arg_seen lunarwing-worker-kimi:latest "${head}-dirty"; then
  pass 'dirty worker dir stamps SOURCE_REVISION=HEAD-dirty'
else
  fail 'dirty worker dir stamps SOURCE_REVISION=HEAD-dirty'
fi

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all worker image contract tests passed\n'
