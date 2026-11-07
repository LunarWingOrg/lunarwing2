#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 05-tenant-build.sh — part of the lunarwing-mt-admin library.
#
# Tenant user/repo/checkout management, cgroup-bounded build runner,
# tenant/worker/DarkIRC builds, and WASM tool + channel installation.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced.
#
# Extracted from the v2.0.8.0 monolith (goals item #9), keeping the library
# structure of ref2/sunburst/mtadmin-refactor-v2.0.6.0 per
# docs/proposals/MT-ADMIN-DECOMPOSITION.md. Pure line-range move: no function
# body was rewritten.


# ── User management ──────────────────────────────────────────────────────────

# Remove the tenant's rootless pause.pid when it is stale or foreign-owned:
# an empty/corrupt file, a dead pid, or a pid since REUSED by another user's
# process (host-ns /proc owner != tenant uid). A LIVE pause process owned by
# the tenant is kept (the "user already exists" resume path re-runs
# ensure_rootless_prereqs against a live tenant). Everything else is stale: a
# stale pause.pid makes EVERY podman call — including `podman system migrate`
# and the first pg container start — fail with "cannot re-exec process to
# join the existing user namespace" (the v1.1.4 O1 fault; see
# docs/bugs/history/OPENRC-MT-1.1.4-ISSUES.md and
# docs/bugs/BUG-workload-scope-podman-userns.md).
#
# The tenant's pause process (catatonit) runs as the tenant uid in the host
# ns, so /proc/<pid> ownership distinguishes it reliably. A bare `kill -0`
# (we run as root) would read a reused pid as "alive" and wrongly keep the
# stale file, leaving the fault unfixed.
#
# Shared by provisioning (ensure_rootless_prereqs) and the WI#142 runtime
# repair verb (`fix-podman-userns`).
#
# <runtime_dir> (optional) overrides the /run/user/<uid> root; tests use it to
# exercise the stale/foreign/keep branches against a fixture dir.
clean_stale_pause_pid() {
  local name="$1" uid="${2:-}" runtime_dir="${3:-}"
  if [[ -z "$uid" ]]; then
    uid="$(id -u "$name" 2>/dev/null)" || return 0
  fi
  if [[ -z "$runtime_dir" ]]; then
    runtime_dir="/run/user/$uid"
  fi
  local pause_pid pause_owner
  pause_pid="$runtime_dir/libpod/tmp/pause.pid"
  if [[ -f "$pause_pid" ]]; then
    pause_owner="$(cat "$pause_pid" 2>/dev/null || true)"
    if [[ -z "$pause_owner" ]] \
       || ! kill -0 "$pause_owner" 2>/dev/null \
       || [[ "$(stat -c %u "/proc/$pause_owner" 2>/dev/null || echo -1)" != "$uid" ]]; then
      rm -f "$pause_pid" 2>/dev/null || true
      say "removed stale or foreign-owned pause.pid for $name (uid $uid)"
    fi
  fi
}

# Provision rootless-podman prerequisites for a tenant user. Idempotent: skips
# anything already present, never overlaps existing subordinate-id ranges.
ensure_rootless_prereqs() {
  local name="$1" uid home start
  ensure_container_runtime
  uid="$(id -u "$name")" || die "cannot resolve uid for tenant '$name'"
  home="$(getent passwd "$name" | cut -d: -f6)"

  # Subordinate uid/gid ranges for the user namespace. useradd may pre-allocate
  # these (via /etc/login.defs); only add when absent, and append AFTER the
  # current max so a new tenant never overlaps an existing range (or eris).
  if ! grep -q "^${name}:" /etc/subuid 2>/dev/null; then
    start="$(awk -F: 'BEGIN{m=100000}{e=$2+$3; if(e>m)m=e}END{print m}' /etc/subuid 2>/dev/null)"
    usermod --add-subuids "${start}-$((start + 65535))" "$name" \
      || die "failed to allocate subuid range for $name (shadow with subid support required)"
    say "allocated subuid range ${start}-$((start + 65535)) for $name"
  fi
  if ! grep -q "^${name}:" /etc/subgid 2>/dev/null; then
    start="$(awk -F: 'BEGIN{m=100000}{e=$2+$3; if(e>m)m=e}END{print m}' /etc/subgid 2>/dev/null)"
    usermod --add-subgids "${start}-$((start + 65535))" "$name" \
      || die "failed to allocate subgid range for $name"
    say "allocated subgid range ${start}-$((start + 65535)) for $name"
  fi

  # Runtime dir (XDG_RUNTIME_DIR). linger (enabled in create_tenant_user)
  # recreates it at boot; create it now for immediate use. tmpfs, 0700, owned.
  install -d -m 0700 -o "$name" -g "$name" "/run/user/$uid"

  # A freshly (re)created tenant may REUSE a uid whose previous holder left
  # rootless-podman runtime state behind in /run/user/$uid: that dir is created by
  # `install -d` above (not by a login session), so logind/elogind never reaps it
  # when the prior tenant is removed (see remove_tenant_user). Clear stale state
  # so podman spawns a fresh pause process instead of failing on every call.
  clean_stale_pause_pid "$name" "$uid"

  # One-time rootless storage init (safe to re-run after subid changes).
  sudo -u "$name" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
    "$CONTAINER_RT" system migrate >/dev/null 2>&1 || true
  # Pre-warm the rootless userns pairing from the operator context (WI#142).
  # `system migrate` leaves NO pause process, so the tenant's user namespace
  # would otherwise be born by whichever podman invocation runs first — on
  # agent-driven tenants that is a tool shell inside a daemon-forked workload
  # scope, whose mount-namespace context later podman invocations (login
  # shells, mt-admin, other scopes) can fail to rejoin. Creating the pause
  # process here, from a plain host context, gives every later invocation —
  # in any context — a joinable, tenant-owned pairing from the start.
  # Best-effort: a failure here warns and the tenant still provisions.
  if ! sudo -u "$name" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
        "$CONTAINER_RT" ps >/dev/null 2>&1; then
    say "WARNING: could not pre-warm the rootless podman userns for $name; the first podman call will create it in its own context (see docs/bugs/BUG-workload-scope-podman-userns.md)" >&2
  fi
  say "rootless prerequisites ready for $name (subuid/subgid, /run/user/$uid, storage)"
}

# Idempotent: install/verify the tenant user's WASM build toolchain (rustup,
# stable default, and the wasm32-wasip2 target).
# Called from create_tenant_user (add-tenant) and as a build-tenant --with-wasm
# preflight, so a partial install (network hiccup, killed add-tenant) is
# repaired instead of silently disabling WASM builds forever.
# Returns 0 when the toolchain is usable, 1 (after a loud warning) when not.
ensure_tenant_wasm_toolchain() {
  local name="$1"
  local cargo_src='if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; else export PATH="$HOME/.cargo/bin:$PATH"; fi;'

  # Run rustup inside the tenant repo when it exists so rust-toolchain.toml
  # selects the pinned build toolchain: the wasm target must be added to (and
  # verified on) THAT toolchain, not the tenant's default, or the pinned build in
  # build-wasm-extensions.sh still can't find it.
  local repo repo_cd=""
  repo="$(tenant_repo "$name" 2>/dev/null || true)"
  [[ -n "$repo" && -d "$repo" ]] && repo_cd="cd '$repo' && "

  # Install rustup for tenant user if not already present. rustup-init is
  # staged under TMPDIR; on hosts mounting /tmp noexec it dies with "Cannot
  # execute ... rustup-init", so stage it in an exec-guaranteed dir under the
  # tenant home instead.
  if ! sudo -u "$name" bash -c "${cargo_src} command -v rustup" &>/dev/null; then
    say "installing rustup for $name ..."
    sudo -u "$name" bash -c 'mkdir -p "$HOME/tmp" && curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | TMPDIR="$HOME/tmp" sh -s -- -y' \
      || { say "WARNING: rustup installation failed for $name" >&2; }
  fi

  # Ensure a default toolchain is set (rustup install may leave none configured)
  if ! sudo -u "$name" bash -c "${cargo_src} rustup show active-toolchain" &>/dev/null; then
    say "setting default toolchain to stable for $name ..."
    sudo -u "$name" bash -c "${cargo_src} rustup default stable" \
      || say "WARNING: failed to set default toolchain for $name" >&2
  fi

  # First-party extensions use wit-bindgen and compile directly to Preview 2.
  say "ensuring WASM toolchain for $name ..."
  sudo -u "$name" bash -c "${cargo_src} ${repo_cd}rustup target add wasm32-wasip2 2>&1" || true

  # Final verification — loud, actionable, never fatal.
  local missing=()
  sudo -u "$name" bash -c "${cargo_src} ${repo_cd}rustup target list --installed 2>/dev/null | grep -q wasm32-wasip2" \
    || missing+=("wasm32-wasip2 target")
  if ((${#missing[@]} > 0)); then
    say "" >&2
    say "WARNING: WASM toolchain incomplete for $name — missing: ${missing[*]}" >&2
    say "         WASM extensions will NOT build. To fix, run:" >&2
    say "           sudo -u $name bash -lc 'rustup target add wasm32-wasip2'" >&2
    say "         then re-run: $0 build-tenant $name --with-wasm" >&2
    say "" >&2
    return 1
  fi
  return 0
}

create_tenant_user() {
  local name="$1"
  local add_docker_group="${2:-false}"

  if id "$name" &>/dev/null; then
    say "user '$name' already exists"
  else
    useradd --create-home --shell /bin/bash --comment "LunarWing tenant $name" "$name"
    say "created user: $name"
  fi

  ensure_init_system

  # Rootless-podman prerequisites for the tenant (subuid/subgid, runtime dir,
  # storage). No-op when rootful (docker).
  ensure_container_runtime
  if [[ "$MT_ROOTLESS" == "true" ]]; then
    ensure_rootless_prereqs "$name"
  fi

  # Persist a per-user runtime manager so /run/user/<uid> survives reboot. Works
  # on both systemd-logind and elogind (OpenRC) — capability-gated, not
  # systemd-only, so rootless podman keeps a runtime dir across reboots.
  if command -v loginctl >/dev/null 2>&1; then
    if loginctl enable-linger "$name" 2>/dev/null; then
      say "enabled linger for $name"
    fi
  fi

  if [[ "$add_docker_group" == "true" ]]; then
    ensure_container_runtime
    local group_name="$CONTAINER_RT"
    if getent group "$group_name" >/dev/null 2>&1; then
      usermod -aG "$group_name" "$name"
      # WI#389: rootless podman needs no group membership — this opt-in only
      # matters when the tenant must reach a shared rootful daemon socket
      # (access to an unrestricted root-run daemon is host-equivalent).
      say "added $name to $group_name group (not needed for rootless podman; only for shared rootful daemon access)"
    else
      say "WARNING: group '$group_name' does not exist; skipping"
    fi
  fi

  local lw_root
  lw_root="$(tenant_lw_root "$name")"
  sudo -u "$name" mkdir -p \
    "$lw_root/env" \
    "$lw_root/state/channels" \
    "$lw_root/state/tools" \
    "$lw_root/state/xmpp" \
    "$lw_root/logs" \
    "$lw_root/run"
  # WI#446 (run-166 LF-8): root's chmod followed a tenant-planted symlink at
  # the env dir name — run it as the tenant, the directory's owner.
  sudo -u "$name" chmod 0700 -- "$lw_root/env"
  say "created directories under $lw_root"

  ensure_tenant_wasm_toolchain "$name" || true
}

remove_tenant_user() {
  local name="$1"
  local purge="${2:-false}"

  ensure_init_system

  # Capture the uid BEFORE userdel removes the passwd entry — needed below to
  # reap the rootless runtime dir (/run/user/<uid>) once the account is gone.
  local _uid; _uid="$(id -u "$name" 2>/dev/null || true)"

  # Stop the per-user runtime manager so it doesn't keep/recreate /run/user/<uid>.
  # loginctl is provided by systemd-logind AND elogind (OpenRC), so gate on the
  # binary, not the init system — otherwise linger is never disabled on OpenRC.
  if command -v loginctl >/dev/null 2>&1; then
    loginctl disable-linger "$name" 2>/dev/null || true
  fi

  if [[ "$purge" == "true" ]]; then
    # Tear the user session down BEFORE userdel. Running userdel immediately after
    # disable-linger races the still-stopping user@<uid>.service and fails with
    # "user busy" (exit 8); previously that error was swallowed (2>/dev/null||true)
    # and "removed user" printed anyway, leaving orphaned accounts/home dirs.
    if [[ "$INIT_SYSTEM" == "systemd" ]]; then
      loginctl terminate-user "$name" 2>/dev/null || true
      if [[ -n "$_uid" ]]; then
        local _w=0
        while [[ -d "/run/user/$_uid" ]] && (( _w < 20 )); do sleep 0.5; _w=$((_w + 1)); done
      fi
    fi
    pkill -KILL -u "$name" 2>/dev/null || true
    # `userdel -r` prints a benign "mail spool not found" warning but still exits 0;
    # a real failure (user busy) exits nonzero — surface it instead of hiding it.
    if userdel -r "$name" 2>/dev/null; then
      say "removed user and home directory: $name"
    elif ! getent passwd "$name" >/dev/null 2>&1; then
      # Account gone but userdel exited nonzero (e.g. exit 12: home removal failed
      # on a busy mount / immutable file). Don't overclaim the home dir was removed.
      say "user '$name' account removed (userdel -r exited nonzero — home dir may persist; verify $(tenant_home "$name"))"
    else
      say "WARNING: failed to remove user '$name' (still present); remove manually: userdel -r $name"
    fi

    # O1 teardown half: /run/user/<uid> is created by `install -d` in
    # ensure_rootless_prereqs (not by a login session), so logind/elogind never
    # reaps it on disable-linger/terminate-user. Left behind, its stale libpod
    # pause.pid breaks the NEXT tenant that REUSES this uid. Remove it explicitly
    # once the account is gone. Guard on a real tenant uid (>=1000) so we never
    # touch root's or a system user's runtime dir.
    if [[ -n "$_uid" ]] && (( _uid >= 1000 )) \
       && ! getent passwd "$name" >/dev/null 2>&1 \
       && ! getent passwd "$_uid" >/dev/null 2>&1 \
       && [[ -d "/run/user/$_uid" ]]; then
      rm -rf "/run/user/$_uid" 2>/dev/null || true
      if [[ -d "/run/user/$_uid" ]]; then
        say "WARNING: could not fully remove rootless runtime dir /run/user/$_uid (busy mounts?); remove manually once released" >&2
      else
        say "reaped stale rootless runtime dir /run/user/$_uid"
      fi
    fi
  else
    say "user $name preserved (use --purge to remove)"
  fi
}

# ── Repo cloning ─────────────────────────────────────────────────────────────

clone_tenant_repo() {
  local name="$1"
  local dest
  dest="$(tenant_lw_root "$name")"

  if [[ -d "$dest/.git" ]] && [[ -d "$dest/ic" ]]; then
    say "repo already cloned at $dest"
    return 0
  fi
  # Clean up incomplete clone (has .git but missing content)
  if [[ -d "$dest/.git" ]] && [[ ! -d "$dest/ic" ]]; then
    say "incomplete clone detected, removing .git and re-cloning ..."
    rm -rf "$dest/.git"
  fi

  say "cloning repo from $SOURCE_REPO to $dest ..."
  local -a safedir=(-c "safe.directory=$dest" -c "safe.directory=$SOURCE_REPO")
  # A shallow SOURCE_REPO (scratch/harness checkouts prepared with
  # `git fetch --depth`/`--shallow-since`) cannot serve plain fetches:
  # upload-pack aborts with "did not send all necessary objects", and the
  # checkout below then fails pathspec, leaving an unusable half-clone.
  # A depth-bounded fetch negotiates the source's shallow boundary instead.
  # Full-depth sources keep the historical full-clone behavior.
  local -a depth_args=()
  if [[ "$(git "${safedir[@]}" -C "$SOURCE_REPO" rev-parse --is-shallow-repository 2>/dev/null)" == "true" ]]; then
    say "source repo is shallow — tenant clone bounded with --depth=10000"
    depth_args=(--depth=10000)
  fi
  if [[ -d "$dest" ]] && [[ -n "$(ls -A "$dest")" ]]; then
    # Directory already has content (env/, state/, etc.) — init in place
    git "${safedir[@]}" init "$dest" >/dev/null
    git "${safedir[@]}" -C "$dest" remote add origin "$SOURCE_REPO"
    git "${safedir[@]}" -C "$dest" fetch "${depth_args[@]}" origin --quiet
    local branch
    branch="$(git "${safedir[@]}" -C "$SOURCE_REPO" symbolic-ref --short HEAD 2>/dev/null || echo staging)"
    git "${safedir[@]}" -C "$dest" checkout -b "$branch" "origin/$branch" 2>&1 | tail -1
  else
    git "${safedir[@]}" clone --single-branch "${depth_args[@]}" "$SOURCE_REPO" "$dest" 2>&1 | tail -1
  fi
  chown -R "$name:$name" "$dest"
  say "repo cloned for $name"
}

# Run one tenant build inside a temporary cgroup v2 directory with the BUILD_*
# memory ceilings — the OpenRC-portable equivalent of the systemd BUILD_SLICE
# scope (work item #54). When the pre-delegated tenant subtree exists
# (setup-cgroups; docs/ops/PREDELEGATED_SUBTREE_CGROUPS.md), the build cgroup is
# created inside it as lunarwing-<name>/build; otherwise a root-level
# lunarwing-build-<name> cgroup is used so builds stay bounded on hosts where
# the subtree has not been deployed yet. The setup-complete flag lets the
# caller distinguish setup failure from every possible build exit status.
_run_tenant_build_cgroup() {
  local name="$1" build_cmd="$2"
  local root cg uid gid home
  _OPENRC_BUILD_CGROUP_SETUP_COMPLETE=false
  root="$(_openrc_cgroup_root)"
  if [[ -d "$root/lunarwing-$name" ]]; then
    cg="$root/lunarwing-$name/build"
  else
    cg="$root/lunarwing-build-$name"
  fi
  uid="$(id -u "$name" 2>/dev/null)" || return 1
  gid="$(id -g "$name" 2>/dev/null)" || return 1
  home="$(_tenant_home_dir "$name")" || return 1
  if [[ -e "$cg" ]]; then
    # Stale cgroup from an interrupted run: reap it before reusing the name.
    _openrc_build_cgroup_cleanup "$cg" || return 1
  fi
  if ! _openrc_build_memory_controller_ready "$root"; then
    say "memory controller is unavailable for $name build cgroup" >&2
    return 1
  fi
  if [[ "$cg" == "$root/lunarwing-$name/build" ]] \
    && ! _openrc_build_memory_controller_ready "$root/lunarwing-$name"; then
    say "memory controller is unavailable in the $name cgroup subtree" >&2
    return 1
  fi
  mkdir "$cg" || return 1
  if ! _openrc_build_cgroup_set_memory_limits "$cg"; then
    _openrc_build_cgroup_cleanup "$cg" || true
    return 1
  fi
  local rc=0
  _run_tenant_build_cgroup_spawn "$name" "$uid" "$gid" "$home" "$cg" "$build_cmd" || rc=$?
  if ! _openrc_build_cgroup_cleanup "$cg"; then
    say "WARNING: could not clean up build cgroup $cg" >&2
    (( rc != 0 )) || rc=1
  fi
  return "$rc"
}

# Spawn the tenant build behind a rendezvous gate, migrate it into the build
# cgroup as root, then release it — Option A (root-side migrate + drop privs)
# from docs/ops/PREDELEGATED_SUBTREE_CGROUPS.md. Unlike the old tenant
# self-migration this needs no tenant write access on any cgroup.procs, no
# chown delegation, and no PAM hook, so bounding works on every OpenRC host;
# every child the build spawns inherits membership at fork.
#
# The gate is a "go" file the wrapper polls for: it cannot start the build
# before the migration write lands, and a failed migration kills the wrapper
# before the build ever runs (setup stays incomplete, so the caller falls
# back to the nice'd invocation). A poll loop is used instead of a FIFO/signal
# rendezvous because both of those have unfixable races here: FIFO open/read
# semantics deadlock once inherited writer fds enter the picture, and
# SIGCONT sent before a self-SIGSTOP is lost. The loop is bounded so an
# aborted mt-admin can never leave a wrapper parked forever.
_run_tenant_build_cgroup_spawn() {
  local name="$1" uid="$2" gid="$3" home="$4" cg="$5" build_cmd="$6"
  local sync_dir build_pid rc=0
  sync_dir="$(mktemp -d "${TMPDIR:-/tmp}/lunarwing-build-sync.XXXXXXXX")" || return 1
  # The tenant wrapper must be able to stat the go file inside.
  chmod 0755 "$sync_dir" || { rm -rf "$sync_dir"; return 1; }
  setpriv --reuid="$uid" --regid="$gid" --clear-groups \
    env HOME="$home" USER="$name" LOGNAME="$name" \
      PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/opt/bin" \
    bash -c 'for ((i=0; i<600; i++)); do
               [[ -e "$2" ]] && break
               sleep 0.05
             done
             [[ -e "$2" ]] || exit 125
             exec nice -n 10 bash -c "$1"' _ "$build_cmd" "$sync_dir/go" &
  build_pid=$!
  if { printf '%s' "$build_pid" >"$cg/cgroup.procs"; } 2>/dev/null; then
    _OPENRC_BUILD_CGROUP_SETUP_COMPLETE=true
    : >"$sync_dir/go"
    wait "$build_pid" || rc=$?
  else
    kill -KILL "$build_pid" 2>/dev/null || true
    wait "$build_pid" 2>/dev/null || true
    say "cgroup v2 root-side migration failed for $name build; bounding ineffective, falling back" >&2
    rc=1
  fi
  rm -rf "$sync_dir"
  return "$rc"
}

# Run one tenant build step with resource bounding (BUG-005). On systemd the
# step runs as a transient scope in BUILD_SLICE via the tenant's own user
# manager, so cargo/rustc memory is capped and accounted outside both the
# operator's session and the daemon service. On other init systems a
# temporary cgroup v2 directory provides the same ceilings (work item #54);
# the build is placed there by root-side migration, so no tenant-side cgroup
# write access is required. Falls back to a plain nice'd invocation only when
# neither mechanism can be set up, so a missing cgroup setup never breaks the
# build.
_run_tenant_build() {
  local name="$1" build_cmd="$2"
  local uid
  uid="$(id -u "$name")"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == systemd* ]] && command -v systemd-run >/dev/null 2>&1 \
    && sudo -u "$name" XDG_RUNTIME_DIR="/run/user/$uid" systemctl --user show --property=Version >/dev/null 2>&1; then
    sudo -u "$name" XDG_RUNTIME_DIR="/run/user/$uid" \
      systemd-run --user --scope --quiet --collect \
      --slice="$BUILD_SLICE" \
      --property="MemoryHigh=$BUILD_MEMORY_HIGH" \
      --property="MemoryMax=$BUILD_MEMORY_MAX" \
      bash -c "$build_cmd"
    return
  fi
  if [[ "$INIT_SYSTEM" != systemd* ]]; then
    if _openrc_build_cgroup_supported; then
      local rc=0
      _OPENRC_BUILD_CGROUP_SETUP_COMPLETE=false
      _run_tenant_build_cgroup "$name" "$build_cmd" || rc=$?
      if [[ "$_OPENRC_BUILD_CGROUP_SETUP_COMPLETE" == true ]]; then
        return "$rc"
      fi
    fi
    say "cgroup v2 bounding unavailable for $name build; running nice-only" >&2
  fi
  sudo -u "$name" nice -n 10 bash -c "$build_cmd"
}

# Shell command (run as the tenant) that builds one cargo root of the tenant
# checkout, wrapped in scripts/vendor-artifact-guard.sh. cargo never re-checks
# vendored crates, so a crate patched in place at the same version (the WI#292
# rig-core patch) would otherwise be linked from the tenant's pre-patch
# artifacts: upgrade-tenant failed with E0599 while CI, building from scratch,
# passed. The guard is the target-dir counterpart of the container config
# hashes in 00-common.sh: before the build it cleans crates whose recorded
# checksum digest changed, and after a successful build it records the
# digests (a failed record only makes the next build re-check, so it does not
# fail the build). Checkouts that predate the guard build unguarded.
_vendor_guarded_cargo_build() {  # <repo> <cargo-root> [extra cargo build args...]
  local repo="$1" root="$2"
  shift 2
  local guard="$repo/scripts/vendor-artifact-guard.sh"
  local guard_args="--manifest-dir '$root' --profile '$PROFILE'"
  local build="cargo build --profile '$PROFILE' -j$BUILD_JOBS${*:+ $*}"
  printf "cd '%s' && if [ -f '%s' ]; then bash '%s' %s && %s && { bash '%s' %s --record || true; }; else %s; fi" \
    "$root" "$guard" "$guard" "$guard_args" "$build" "$guard" "$guard_args" "$build"
}

# WI#527: which build stages a `build_tenant` run has completed. Recorded to a
# per-tenant file rather than a shell array because the upgrade path runs
# build_tenant inside a captured subshell (to keep errexit and return a status)
# and the external wrapper (ic/scripts/upgrade-tenant.sh) is a separate process
# that must classify the same failure. build_tenant starts the file with a
# "start" sentinel and appends one line per completed stage. Stage names:
# daemon, bridge, wasm. Callers use this to decide whether an in-place restart
# would start the NEW daemon binary, instead of parsing error prose (run-287
# SX-1/SX-4). Worker lanes are never built after the stop: they are built to
# candidate tags before it and recorded separately (see
# TENANT_CANDIDATE_LANES_FILE), so a post-stop stage record never names a lane
# (run-294 TJ-14).
#
# run-291 TD-2: marker files live in the root-owned 0700 lifecycle lock
# directory, NOT in the tenant-writable state directory. Root truncating or
# appending a file inside tenant-writable space follows tenant symlinks (WI#446
# class), so the markers are kept root-owned alongside the build lock.
_tenant_marker_dir() {
  printf '%s' "${TENANT_LIFECYCLE_LOCK_DIR:-/run/lock/lunarwing-tenants}"
}

tenant_build_stage_file() {
  printf '%s' "${LUNARWING_MT_BUILD_STAGE_FILE:-$(_tenant_marker_dir)/$1-build-stages}"
}

tenant_candidate_lanes_file() {
  printf '%s' "${LUNARWING_MT_CANDIDATE_LANES_FILE:-$(_tenant_marker_dir)/$1-candidate-lanes}"
}

# Ensure the root-owned marker directory exists and is safe (root:root 0700).
# Callers must run as root; the dir is shared with the lifecycle lock path.
# run-294 TJ-4: same hardened sequence as the lifecycle lock directory in
# 00-common.sh. `[[ -d ]] || install -d` follows a symlink planted in a
# world-writable /run/lock and then truncates/appends through it (WI#446 class);
# this helper is reached without the lifecycle lock by build-tenant, build-all,
# build-worker-candidates, promote-worker-images and reset-build-stage.
_ensure_marker_dir() {
  local dir
  dir="$(_tenant_marker_dir)"
  [[ ! -L "$dir" ]] || die "unsafe marker directory $dir (symlink)"
  install -d -o root -g root -m 0700 "$dir"
  [[ "$(stat -c '%u:%a' "$dir" 2>/dev/null)" == "0:700" ]] \
    || die "unsafe marker directory $dir (must be root-owned mode 0700)"
}

_build_stage_log_path() {  # <name>
  local path
  path="$(tenant_build_stage_file "$1")"
  _ensure_marker_dir
  printf '%s' "$path"
}

# Start a fresh per-run stage file. The literal "start" sentinel keeps "nothing
# built yet" distinguishable from "marker unreadable" (run-294 TJ-9).
_reset_build_stage_file() {  # <name>
  local path
  path="$(_build_stage_log_path "$1")"
  : >"$path"
  printf 'start\n' >>"$path"
}

_record_build_stage() {  # <name> <stage>
  printf '%s\n' "$2" >>"$(_build_stage_log_path "$1")"
}

# WI#527: the last stage a build_tenant run completed for <name>:
# "daemon", "bridge", "wasm", or "none" when nothing completed (worker lanes are
# pre-stop candidates and never appear here).
# A missing or existing-but-unreadable marker reports "unknown" so the external
# wrapper fails closed instead of restarting a possibly-new binary (run-294 TJ-9,
# run-300 TO-7: a missing marker used to read as "none", the one fail-open shape
# left on the wrapper path).
upgrade_last_completed_stage() {  # <name>
  local file last=""
  file="$(tenant_build_stage_file "$1")"
  [[ -e "$file" ]] || { printf 'unknown'; return 0; }
  last="$(grep -v '^[[:space:]]*$' "$file" 2>/dev/null | tail -n 1 || true)"
  case "$last" in
    start) printf 'none' ;;
    "") printf 'unknown' ;;
    *) printf '%s' "$last" ;;
  esac
}

# WI#527: tri-state view of the daemon stage for convergence (run-294 TJ-9).
# "none"    — the marker exists and this run has not reached the daemon stage.
# "built"   — the run built (and therefore left on disk) the new binary.
# "unknown" — the marker is missing or unreadable; treated as past the daemon so
#             a recording failure after cargo replaced the binary can never be
#             reported as "restored to the previous build".
upgrade_daemon_stage_state() {  # <name>
  local file
  file="$(tenant_build_stage_file "$1")"
  [[ -e "$file" ]] || { printf 'unknown'; return 0; }
  if grep -qx 'daemon' "$file" 2>/dev/null; then
    printf 'built'
  elif grep -q '[^[:space:]]' "$file" 2>/dev/null; then
    printf 'none'
  else
    printf 'unknown'
  fi
}

# WI#527: worker lanes built to candidate tags before the stop, oldest first.
upgrade_candidate_lanes() {  # <name>
  local file
  file="$(tenant_candidate_lanes_file "$1")"
  [[ -r "$file" ]] || return 0
  grep -v '^[[:space:]]*$' "$file" 2>/dev/null || true
}

build_tenant() {
  local name="$1"
  local with_wasm="${2:-false}"
  local with_pebble="${3:-false}"
  local with_opencode="${4:-false}"
  local with_kimi="${5:-false}"
  local with_toolchains="${6:-false}"
  local require_wasm="${7:-false}"
  local with_opencode2="${8:-false}"
  local repo
  repo="$(tenant_repo "$name")"

  # WI#527: stage markers are per-run; an empty file means "nothing built yet".
  # TD-11: truncate inside the build lock so a concurrent build-tenant or
  # build-all cannot blank another run's markers mid-build.
  _ensure_marker_dir
  if [[ "$require_wasm" == "true" && "$with_wasm" != "true" ]]; then
    die "--require-wasm requires --with-wasm"
  fi

  [[ -d "$repo" ]] || die "repo not found at $repo; run add-tenant first"

  say "acquiring build lock (only one tenant builds at a time) ..."
  (
    flock -x 200
    # Fresh per-run marker ("start" = nothing built yet, distinguishable from an
    # unreadable marker; run-294 TJ-9).
    _reset_build_stage_file "$name"

    local cargo_env="if [ -f \"\$HOME/.cargo/env\" ]; then . \"\$HOME/.cargo/env\"; else export PATH=\"\$HOME/.cargo/bin:\$PATH\"; fi;"

    say "building lunarwing for $name ..."
    _run_tenant_build "$name" "$cargo_env $(_vendor_guarded_cargo_build "$repo" "$repo" --bin lunarwing)" \
      || die "lunarwing build failed for $name"
    _record_build_stage "$name" daemon

    say "building xmpp-bridge for $name ..."
    _run_tenant_build "$name" "$cargo_env $(_vendor_guarded_cargo_build "$repo" "$repo/bridges/xmpp-bridge")" \
      || die "xmpp-bridge build failed for $name"
    _record_build_stage "$name" bridge

    if [[ "$with_wasm" == "true" ]]; then
      if ensure_tenant_wasm_toolchain "$name"; then
        say "building WASM extensions for $name ..."
        local wasm_build_ok="true"
        if ! _run_tenant_build "$name" "$cargo_env cd '$repo' && CARGO_BUILD_JOBS=$BUILD_JOBS bash scripts/build-wasm-extensions.sh"; then
          if [[ "$require_wasm" == "true" ]]; then
            die "WASM extension build failed for $name; refusing to install stale artifacts"
          fi
          say "WARNING: WASM extension build reported errors for $name (some extensions may be missing; re-run '$0 build-tenant $name --with-wasm' after fixing)" >&2
          wasm_build_ok="false"
        fi

        if [[ "$wasm_build_ok" == "true" ]]; then
          say "installing WASM extensions for $name ..."
          install_wasm_tenant "$name"
          _record_build_stage "$name" wasm
        else
          say "WARNING: skipping WASM install for $name; existing installed artifacts were preserved" >&2
        fi
      else
        if [[ "$require_wasm" == "true" ]]; then
          die "WASM toolchain incomplete for $name; refusing to continue upgrade"
        fi
        say "WARNING: skipping WASM build for $name (toolchain incomplete — see above)" >&2
      fi
    fi

    say "build complete for $name"
    say ""
    say "Reminder: set your LLM provider API key (if applicable to your backend) in $(tenant_env_dir "$name")/lunarwing.env"
    say "  e.g.  LLM_API_KEY=sk-..."
  ) 200>"$BUILD_LOCK" || return $?

  if [[ "$with_pebble" == "true" ]]; then
    say ""
    say "=== Building pebble worker image ==="
    build_pebble_worker "false" "$with_toolchains" "$(tenant_lw_root "$name")"
  fi

  if [[ "$with_opencode" == "true" ]]; then
    say ""
    say "=== Building opencode worker image ==="
    build_opencode_worker "false" "$with_toolchains" "$(tenant_lw_root "$name")"
  fi

  if [[ "$with_kimi" == "true" ]]; then
    say ""
    say "=== Building kimi worker image ==="
    build_kimi_worker "false" "$with_toolchains" "" "$(tenant_lw_root "$name")"
  fi

  if [[ "$with_opencode2" == "true" ]]; then
    say ""
    say "=== Building opencode2 worker image ==="
    build_opencode2_worker "false" "$with_toolchains" "$(tenant_lw_root "$name")"
  fi

  # After a rebuild, the tenant will be restarted with the new binary. If the
  # tenant has LUNARWING_OWNER_ID but still has orphaned 'default'-scoped DB
  # data (e.g. upgrading from a pre-owner-id version), auto-migrate so the
  # new binary doesn't lose access to existing conversations and memory.
  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  if grep -q '^LUNARWING_OWNER_ID=' "$env_path" 2>/dev/null && _owner_scope_needs_migration "$name"; then
    say ""
    say "--- Auto-migrating owner scope ---"
    migrate_owner_scope "$name" || say "WARNING: owner-scope migration failed (run 'migrate-owner-scope $name' manually)"
  fi
}

# ── WI#527: pre-stop candidate worker images ─────────────────────────────────
# The upgrade path builds every selected worker image to a per-run candidate tag
# WHILE the tenant still runs, then stops, rebuilds the daemon, and promotes the
# candidates only after every selected lane succeeded (run-287 SX-2). Image
# builds never needed the tenant down; building them before the stop means a
# lane build failure leaves the tenant running on its untouched :latest images.
# Promotion is a retag of the already-built image, so a promote failure is still
# recoverable by re-running it.

# Candidate image tag suffix for this upgrade run. Uses a per-tenant tag
# (wi527-<tenant>) rather than a filesystem path so the result is a valid image
# reference — a tag cannot contain '/' (run-291 TD-1). One per upgrade attempt;
# re-runs replace it via the promote/cleanup cycle.
_upgrade_candidate_tag_suffix() {  # <tenant>
  printf 'wi527-%s' "$1"
}

# Full candidate image reference for <lane> on <tenant>. Built by
# build_candidate_worker, promoted by promote_candidate_workers, removed by
# cleanup_candidate_workers — all share this one helper (run-291 TD-1).
upgrade_candidate_image_ref() {  # <tenant> <lane>
  printf 'lunarwing-worker-%s:%s' "$2" "$(_upgrade_candidate_tag_suffix "$1")"
}

# Validate a lane name against the fixed set (run-291 TD-2: lane names flow into
# podman tag/rmi and must not be attacker-controlled). Returns non-zero instead
# of dying so callers iterating the lane file can skip a bad line and keep
# converging (run-294 TJ-10).
_upgrade_validate_lane() {  # <lane>
  case "$1" in
    pebble|opencode|kimi|opencode2) return 0 ;;
    *)
      say "WARNING: ignoring unknown worker lane '$1'" >&2
      return 1
      ;;
  esac
}

# Build <lane>'s image to this run's candidate tag and record it.
# Usage: build_candidate_worker <tenant> <lane> <with_toolchains> [release_ref]
build_candidate_worker() {
  local name="$1" lane="$2" with_toolchains="${3:-false}" release_ref="${4:-}"
  local lw_root tag rc=0 profile
  _upgrade_validate_lane "$lane" || die "unknown worker lane '$lane'"
  ensure_container_runtime
  lw_root="$(tenant_lw_root "$name")"
  # LUNARWING_WORKER_IMAGE_TAG is the tag alone: every builder names its output
  # worker_image_ref <lane> = lunarwing-worker-<lane>:<tag>, which is exactly
  # upgrade_candidate_image_ref, the ref promote and cleanup use. Passing that
  # full ref here built "lunarwing-worker-<lane>:lunarwing-worker-<lane>:wi527-
  # <tenant>", an invalid reference, so every upgrade with a worker lane failed.
  tag="$(_upgrade_candidate_tag_suffix "$name")"
  # run-294 TJ-6: the upgrade used to rebuild pebble slim unconditionally. The
  # image carries its build profile as a label (pebble4lunarwing/Dockerfile), so
  # a candidate keeps the profile of the :latest it will replace instead of
  # silently downgrading a fat (--with-toolchains) image.
  if [[ "$lane" == pebble && "$with_toolchains" != true ]]; then
    profile="$(_worker_toolchain_profile pebble)"
    with_toolchains="$profile"
  fi
  case "$lane" in
    pebble)    LUNARWING_WORKER_IMAGE_TAG="$tag" build_pebble_worker "false" "$with_toolchains" "$lw_root" "$release_ref" || rc=$? ;;
    opencode)  LUNARWING_WORKER_IMAGE_TAG="$tag" build_opencode_worker "false" "$with_toolchains" "$lw_root" "$release_ref" || rc=$? ;;
    kimi)      LUNARWING_WORKER_IMAGE_TAG="$tag" build_kimi_worker "false" "$with_toolchains" "" "$lw_root" "$release_ref" || rc=$? ;;
    opencode2) LUNARWING_WORKER_IMAGE_TAG="$tag" build_opencode2_worker "false" "$with_toolchains" "$lw_root" "$release_ref" || rc=$? ;;
  esac
  [[ "$rc" -eq 0 ]] || return "$rc"
  _ensure_marker_dir
  printf '%s\n' "$lane" >>"$(tenant_candidate_lanes_file "$name")"
}

# The toolchain profile of a lane's current :latest image ("true" for a fat
# image). Anything unreadable or unlabelled is slim, matching the Dockerfile
# default and pre-label images.
_worker_toolchain_profile() {  # <lane>
  local latest
  latest="$(worker_image_ref "$1" latest)"
  [[ -n "${CONTAINER_RT:-}" ]] || return 0
  local profile
  profile="$("$CONTAINER_RT" image inspect \
    --format '{{ index .Config.Labels "org.opencontainers.image.toolchain-profile" }}' \
    "$latest" 2>/dev/null || true)"
  [[ "$profile" == true ]] && printf 'true'
  # run-300 TO-8: every current caller uses this in a `||` context, but a
  # profile helper must not report failure just because the image is slim.
  return 0
}

# Build every selected lane's candidate image, recording each success. Fails at
# the first failing lane with that lane named, leaving the tenant untouched.
# Usage: build_candidate_workers <tenant> <with_opencode> <with_kimi> <with_opencode2> [with_pebble] [with_toolchains] [release_ref]
build_candidate_workers() {
  local name="$1" with_opencode="$2" with_kimi="$3" with_opencode2="$4"
  local with_pebble="${5:-false}" with_toolchains="${6:-false}" release_ref="${7:-}"
  _ensure_marker_dir
  : >"$(tenant_candidate_lanes_file "$name")"
  # run-300 TO-3: a new run must not inherit a prior run's pre-promotion ids, or
  # a later failed run could "restore" a lane to an image this run never had.
  clear_promote_rollback "$name"
  local lane
  for lane in pebble opencode kimi opencode2; do
    case "$lane" in
      pebble)    [[ "$with_pebble" == true ]] || continue ;;
      opencode)  [[ "$with_opencode" == true ]] || continue ;;
      kimi)      [[ "$with_kimi" == true ]] || continue ;;
      opencode2) [[ "$with_opencode2" == true ]] || continue ;;
    esac
    say ""
    say "=== Building $lane worker candidate image (tenant still running) ==="
    build_candidate_worker "$name" "$lane" "$with_toolchains" "$release_ref" \
      || die "$lane worker image candidate build failed; tenant '$name' has not been stopped"
  done
}

# Promote every recorded candidate lane to :latest. Idempotent and re-runnable.
# TD-4: record old :latest image IDs before promoting and re-tag them on
# failure; skip a lane whose :latest already equals its candidate; update the
# lane file per lane so a re-run only promotes what remains. Only a successful
# candidate build is ever promoted, so a failed run can never leave :latest
# pointing at a partial image.
#
# run-294 TJ-5: the rollback record appends one line per lane and keeps the
# FIRST pre-promotion id, so re-running after a mid-promote die never rewrites
# the record with an already-promoted candidate. run-300 TO-2: it is NOT removed
# by a successful promote — the new :latest tags are only safe to keep once the
# whole upgrade finished, so the removal happens there (or in
# restore_promoted_workers, which consumes it on a rollback).
#
# Old :latest IDs are recorded to $(_tenant_marker_dir)/$name-promote-rollback
# so upgrade_converge_after_failure (a separate invocation) can restore them
# after a mid-promote die.
_promote_rollback_file() {  # <name>
  printf '%s/%s-promote-rollback' "$(_tenant_marker_dir)" "$1"
}

# run-300 TO-2: drop the pre-promotion record once the upgrade as a whole has
# succeeded (the wrapper, upgrade_tenant_locked). Never called by a promote.
clear_promote_rollback() {  # <name>
  rm -f "$(_promote_rollback_file "$1")"
}

# Append <lane>'s pre-promotion image id unless this run already recorded one.
_promote_rollback_record() {  # <name> <lane> <old_id>
  local name="$1" lane="$2" old_id="$3" file
  file="$(_promote_rollback_file "$name")"
  _ensure_marker_dir
  [[ -f "$file" ]] || : >"$file"
  awk -F'\t' -v lane="$lane" '$1 == lane { found = 1 } END { exit !found }' "$file" \
    || printf '%s\t%s\n' "$lane" "$old_id" >>"$file"
}

restore_promoted_workers() {  # <name> — re-tag the pre-promotion :latest ids
  local name="$1" file lane old_id latest_ref rc=0
  file="$(_promote_rollback_file "$name")"
  [[ -r "$file" ]] || return 0
  while IFS=$'\t' read -r lane old_id; do
    [[ -n "$lane" && -n "$old_id" ]] || continue
    if ! _upgrade_validate_lane "$lane"; then rc=1; continue; fi
    latest_ref="$(worker_image_ref "$lane" latest)"
    # TD-10: only claim a rollback when the re-tag actually succeeded (a pruned
    # old image must not be reported as restored).
    if "$CONTAINER_RT" tag "$old_id" "$latest_ref" >/dev/null 2>&1; then
      say "rolled back $latest_ref to $old_id"
    else
      say "WARNING: could not roll back $latest_ref to $old_id (old image may be pruned)" >&2
      rc=1
    fi
  done <"$file"
  [[ "$rc" -eq 0 ]] && rm -f "$file"
  return "$rc"
}

promote_candidate_workers() {  # <tenant>
  local name="$1" lane candidate_ref latest_ref old_id candidate_id
  local lanes_file rollback_file
  local -a lanes=()
  lanes_file="$(tenant_candidate_lanes_file "$name")"
  mapfile -t lanes < <(upgrade_candidate_lanes "$name")
  if [[ "${#lanes[@]}" -eq 0 ]]; then
    say "no candidate worker images to promote for '$name'"
    return 0
  fi
  ensure_container_runtime
  _ensure_marker_dir
  rollback_file="$(_promote_rollback_file "$name")"
  [[ -f "$rollback_file" ]] || : >"$rollback_file"
  for lane in "${lanes[@]}"; do
    # run-294 TJ-1: resolve the path through the helper, never the bare env
    # override (production does not export it), and never concatenate an unset
    # variable under `set -u`.
    _upgrade_validate_lane "$lane" || die "candidate lane file for '$name' names an unknown lane"
    candidate_ref="$(upgrade_candidate_image_ref "$name" "$lane")"
    latest_ref="$(worker_image_ref "$lane" latest)"
    candidate_id="$("$CONTAINER_RT" image inspect --format '{{.Id}}' "$candidate_ref" 2>/dev/null || true)"
    old_id="$("$CONTAINER_RT" image inspect --format '{{.Id}}' "$latest_ref" 2>/dev/null || true)"
    # Skip if :latest already equals the candidate (already promoted).
    if [[ -n "$candidate_id" && "$old_id" == "$candidate_id" ]]; then
      say "$latest_ref already at candidate, skipping"
      continue
    fi
    if [[ -z "$candidate_id" ]]; then
      die "candidate image $candidate_ref is missing; cannot promote lane '$lane'"
    fi
    # TD-4: record the old :latest id before overwriting the tag (first wins).
    [[ -n "$old_id" ]] && _promote_rollback_record "$name" "$lane" "$old_id"
    "$CONTAINER_RT" tag "$candidate_ref" "$latest_ref" \
      || die "cannot promote $candidate_ref to $latest_ref"
    say "promoted $latest_ref"
    # TD-4: update the lane file per lane so a mid-loop failure is recoverable.
    grep -vx "$lane" "$lanes_file" >"$lanes_file.tmp" 2>/dev/null || true
    mv "$lanes_file.tmp" "$lanes_file"
    "$CONTAINER_RT" rmi "$candidate_ref" >/dev/null 2>&1 || true
  done
  : >"$lanes_file"
  # run-300 TO-2: the rollback record stays until the whole upgrade succeeds;
  # deleting it here made a later render/env/start failure report "restored"
  # while the new worker images stayed on :latest.
}

# ── Darkirc daemon build (shared, not per-tenant) ────────────────────────────

darkirc_validate_root_owned_directory() {
  local path="$1" rest component current="" owner mode
  [[ "$path" == /* && -d "$path" ]] || return 1
  rest="${path#/}"
  while [[ -n "$rest" ]]; do
    component="${rest%%/*}"
    [[ -n "$component" && "$component" != . && "$component" != .. ]] || return 1
    current="$current/$component"
    [[ -d "$current" && ! -L "$current" ]] || return 1
    owner="$(stat -c '%u' "$current" 2>/dev/null || true)"
    mode="$(stat -c '%a' "$current" 2>/dev/null || true)"
    [[ "$owner" == 0 && "$mode" =~ ^[0-7]+$ ]] || return 1
    (( (8#$mode & 0022) == 0 )) || return 1
    if [[ "$rest" == */* ]]; then rest="${rest#*/}"; else rest=""; fi
  done
}

darkirc_validate_clean_root_checkout() {
  local path="$1" expected_revision="${2:-}" revision dirty git_dir common_dir unsafe
  darkirc_validate_root_owned_directory "$path" || return 1
  revision="$(git -C "$path" rev-parse HEAD 2>/dev/null)" || return 1
  [[ -z "$expected_revision" || "$revision" == "$expected_revision" ]] || return 1
  dirty="$(git -C "$path" status --porcelain --untracked-files=all 2>/dev/null)" || return 1
  [[ -z "$dirty" ]] || return 1
  git_dir="$(git -C "$path" rev-parse --absolute-git-dir 2>/dev/null)" || return 1
  common_dir="$(git -C "$path" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || return 1
  for git_path in "$git_dir" "$common_dir"; do
    darkirc_validate_root_owned_directory "$git_path" || return 1
    unsafe="$(find "$git_path" -xdev \( -type l -o ! -user root -o -perm /022 \) -print -quit 2>/dev/null)" \
      || return 1
    [[ -z "$unsafe" ]] || return 1
  done
  ! git -C "$path" ls-tree -r "$revision" | grep -Eq '^(120000|160000) '
}

ensure_darkirc_build_user() {
  require_cmd useradd
  require_cmd passwd
  require_cmd setpriv
  if ! id "$DARKIRC_BUILD_USER" >/dev/null 2>&1; then
    useradd --system --no-create-home --home-dir "$DARKIRC_BUILD_HOME" \
      --shell "$(command -v nologin || printf '/sbin/nologin')" \
      --comment "LunarWing unprivileged build worker" "$DARKIRC_BUILD_USER"
  fi
  local build_uid build_gid build_groups account_home account_shell account_state
  build_uid="$(id -u "$DARKIRC_BUILD_USER")"
  build_gid="$(id -g "$DARKIRC_BUILD_USER")"
  build_groups="$(id -G "$DARKIRC_BUILD_USER")"
  account_home="$(getent passwd "$DARKIRC_BUILD_USER" | cut -d: -f6)"
  account_shell="$(getent passwd "$DARKIRC_BUILD_USER" | cut -d: -f7)"
  account_state="$(passwd -S "$DARKIRC_BUILD_USER" 2>/dev/null | awk '{print $2}')"
  [[ "$build_uid" -ne 0 && "$build_groups" == "$build_gid" \
    && "$account_home" == "$DARKIRC_BUILD_HOME" \
    && "$account_shell" == */nologin \
    && ( "$account_state" == L || "$account_state" == LK ) ]] \
    || die "DarkIRC build account must be locked, dedicated, and have no supplementary groups"
  DARKIRC_BUILD_GROUP="$(id -gn "$DARKIRC_BUILD_USER")"
  [[ ! -L "$DARKIRC_BUILD_HOME" && ( ! -e "$DARKIRC_BUILD_HOME" || -d "$DARKIRC_BUILD_HOME" ) ]] \
    || die "unsafe DarkIRC build home"
  install -d -o root -g root -m 0711 "$DARKIRC_BUILD_HOME"
  [[ "$(stat -c '%u:%a' "$DARKIRC_BUILD_HOME" 2>/dev/null)" == "0:711" ]] \
    || die "DarkIRC build home must be root-owned mode 0711"
}

darkirc_validate_build_artifact() {
  local artifact="$1" owner links
  [[ -f "$artifact" && ! -L "$artifact" && -x "$artifact" ]] || return 1
  owner="$(stat -c '%U' "$artifact" 2>/dev/null || true)"
  links="$(stat -c '%h' "$artifact" 2>/dev/null || true)"
  [[ "$owner" == "$DARKIRC_BUILD_USER" && "$links" == 1 ]]
}

darkirc_path_components_safe() {
  local path="$1" rest component current=""
  [[ "$path" == /* ]] || return 1
  rest="${path#/}"
  while [[ -n "$rest" ]]; do
    component="${rest%%/*}"
    [[ -n "$component" && "$component" != . && "$component" != .. ]] || return 1
    current="$current/$component"
    [[ ! -L "$current" ]] || return 1
    if [[ "$rest" == */* ]]; then rest="${rest#*/}"; else rest=""; fi
  done
}

darkirc_validate_root_executable() {
  local path="$1" mode owner links
  darkirc_path_components_safe "$path" || return 1
  darkirc_validate_root_owned_directory "$(dirname "$path")" || return 1
  [[ -f "$path" && ! -L "$path" && -x "$path" ]] || return 1
  mode="$(stat -c '%a' "$path" 2>/dev/null || true)"
  owner="$(stat -c '%u' "$path" 2>/dev/null || true)"
  links="$(stat -c '%h' "$path" 2>/dev/null || true)"
  [[ "$owner" == 0 && "$links" == 1 && "$mode" =~ ^[0-7]+$ ]] || return 1
  (( (8#$mode & 07022) == 0 ))
}

darkirc_current_helper_source_revision() {
  local revision
  revision="$(git -C "$LUNARWING_ROOT" rev-parse HEAD 2>/dev/null)" || return 1
  [[ "$revision" =~ ^[0-9a-f]{40}$ ]] || return 1
  printf '%s' "$revision"
}

install_darkirc_key_helper() {
  local archive_root cargo_home artifact source_revision
  require_cmd cargo
  require_cmd taskset
  require_cmd tar
  darkirc_validate_clean_root_checkout "$LUNARWING_ROOT" \
    || die "trusted LunarWing admin checkout must be root-owned, non-writable, and clean"
  source_revision="$(darkirc_current_helper_source_revision)" \
    || die "could not attest LunarWing helper source revision"
  ensure_darkirc_build_user
  [[ "$DARKIRC_KEY_HELPER_BUILD_ROOT" == /var/cache/lunarwing/* \
    && "$DARKIRC_KEY_HELPER_BUILD_ROOT" != *'/../'* \
    && ! -L "$DARKIRC_KEY_HELPER_BUILD_ROOT" ]] \
    || die "DarkIRC helper build root must be a safe /var/cache/lunarwing path"
  rm -rf -- "$DARKIRC_KEY_HELPER_BUILD_ROOT"
  install -d -o "$DARKIRC_BUILD_USER" -g "$DARKIRC_BUILD_GROUP" -m 0700 \
    "$DARKIRC_KEY_HELPER_BUILD_ROOT"
  archive_root="$DARKIRC_KEY_HELPER_BUILD_ROOT/source"
  install -d -o root -g root -m 0700 "$archive_root"
  git -C "$LUNARWING_ROOT" archive --format=tar HEAD | tar -x -C "$archive_root"
  chown -R "$DARKIRC_BUILD_USER:$DARKIRC_BUILD_GROUP" "$archive_root"
  cargo_home="$(mktemp -d "$DARKIRC_BUILD_HOME/cargo.XXXXXX")"
  install -d -o root -g root -m 0700 "$cargo_home/.cargo"
  chown -R "$DARKIRC_BUILD_USER:$DARKIRC_BUILD_GROUP" "$cargo_home"
  say "building trusted DarkIRC key helper as $DARKIRC_BUILD_USER ..."
  setpriv --reuid="$(id -u "$DARKIRC_BUILD_USER")" \
    --regid="$(id -g "$DARKIRC_BUILD_USER")" --clear-groups --no-new-privs -- \
    env -i HOME="$cargo_home" PATH="/usr/bin:/bin" CARGO_HOME="$cargo_home/.cargo" \
    CARGO_TARGET_DIR="$DARKIRC_KEY_HELPER_BUILD_ROOT" \
    taskset -c 0-7 cargo build --manifest-path "$archive_root/ic/Cargo.toml" \
      --release -j8 --bin lunarwing-darkirc-key-helper \
    || die "DarkIRC key helper build failed"
  darkirc_validate_clean_root_checkout "$LUNARWING_ROOT" \
    || die "DarkIRC helper build changed trusted LunarWing checkout"
  rm -rf -- "$cargo_home"
  artifact="$DARKIRC_KEY_HELPER_BUILD_ROOT/release/lunarwing-darkirc-key-helper"
  # Cargo hard-links release/<bin> to deps/<bin>-<hash> on capable filesystems,
  # so the artifact would have st_nlink=2 and fail validation. Copy over itself
  # to break the link (same bytes, fresh inode) before checking ownership.
  local unlinked="$DARKIRC_KEY_HELPER_BUILD_ROOT/release/.lunarwing-darkirc-key-helper.unlinked"
  cp -f "$artifact" "$unlinked" && mv -f "$unlinked" "$artifact"
  chown "$DARKIRC_BUILD_USER:$DARKIRC_BUILD_GROUP" "$artifact"
  darkirc_validate_build_artifact "$artifact" \
    || die "DarkIRC key helper artifact failed ownership validation"
  DARKIRC_HELPER_ARTIFACT="$artifact"
  DARKIRC_HELPER_SOURCE_REVISION="$source_revision"
}

write_darkirc_compatibility() {
  local release_dir="$1" release_id="$2" source_revision="$3" test_user="$4"
  local daemon_digest="$5" helper_digest="$6" helper_source_revision="$7"
  local tmp="$release_dir/attestation.json"
  require_cmd jq
  [[ "$source_revision" == "$DARKIRC_APPROVED_REV" ]] \
    || die "DarkIRC source revision is not approved for managed config"
  [[ "$helper_source_revision" =~ ^[0-9a-f]{40}$ ]] \
    || die "LunarWing helper source revision is invalid"
  darkirc_validate_root_executable "$release_dir/darkirc" \
    || die "staged DarkIRC daemon is unsafe"
  darkirc_validate_root_executable "$release_dir/lunarwing-darkirc-key-helper" \
    || die "staged DarkIRC key helper is unsafe"
  id "$test_user" >/dev/null 2>&1 || die "DarkIRC self-test user '$test_user' does not exist"
  jq -n --arg release "$release_id" --arg daemon "$daemon_digest" \
    --arg helper "$helper_digest" --arg revision "$source_revision" \
    --arg helper_revision "$helper_source_revision" \
    '{schema:"lunarwing.darkirc-compat/v2",release_id:$release,profile_id:"darkfi-a05956d41-chacha-v1",daemon_sha256:$daemon,helper_sha256:$helper,helper_source_revision:$helper_revision,key_format:"darkfi-chacha-base58-32",source_revision:$revision}' \
    >"$tmp" || die "failed to create DarkIRC compatibility attestation"
  chmod 0444 "$tmp"
  chown root:root "$tmp"
  sudo -u "$test_user" env -i HOME="$(getent passwd "$test_user" | cut -d: -f6)" PATH="/usr/bin:/bin" \
    "$release_dir/lunarwing-darkirc-key-helper" self-test-generator \
    --generator-profile darkfi-a05956d41-chacha-v1 \
    --binary-sha256 "$daemon_digest" --binary-path "$release_dir/darkirc" \
    --helper-sha256 "$helper_digest" --release-id "$release_id" \
    --key-format darkfi-chacha-base58-32 --source-revision "$source_revision" >/dev/null \
    || die "staged DarkIRC generator failed compatibility self-test"
}

darkirc_fsync_path() {
  sync -f -- "$1" || die "could not durably sync DarkIRC release path: $1"
}

darkirc_sync_release_durability() {
  local release_dir="$1"
  darkirc_fsync_path "$release_dir/darkirc" || return 1
  darkirc_fsync_path "$release_dir/lunarwing-darkirc-key-helper" || return 1
  darkirc_fsync_path "$release_dir/attestation.json" || return 1
  darkirc_fsync_path "$release_dir" || return 1
  darkirc_fsync_path "$DARKIRC_RELEASES_DIR" || return 1
  darkirc_fsync_path "$DARKIRC_RELEASE_ROOT" || return 1
  darkirc_fsync_path "$(dirname "$DARKIRC_RELEASE_ROOT")" || return 1
}

reject_legacy_darkirc_path_overrides() {
  local variable
  for variable in LUNARWING_MT_DARKIRC_BIN LUNARWING_MT_DARKIRC_COMPAT_FILE \
                  LUNARWING_MT_DARKIRC_KEY_HELPER; do
    [[ -z "${!variable:-}" ]] \
      || die "$variable is unsupported with atomic DarkIRC releases; use LUNARWING_MT_DARKIRC_RELEASE_ROOT"
  done
}

darkirc_validate_release_id() {
  [[ "$1" =~ ^[a-f0-9]{40}-[a-f0-9]{16}-[a-f0-9]{16}-[a-f0-9]{16}$ ]]
}

darkirc_validate_release_directory() {
  local release_dir="$1" expected_id="${2:-}" mode owner links
  [[ "$release_dir" == "$DARKIRC_RELEASES_DIR/"* ]] || return 1
  local release_id="${release_dir##*/}"
  darkirc_validate_release_id "$release_id" || return 1
  [[ -z "$expected_id" || "$release_id" == "$expected_id" ]] || return 1
  darkirc_validate_root_owned_directory "$release_dir" || return 1
  mode="$(stat -c '%a' "$release_dir" 2>/dev/null || true)"
  owner="$(stat -c '%u' "$release_dir" 2>/dev/null || true)"
  links="$(stat -c '%h' "$release_dir" 2>/dev/null || true)"
  [[ "$mode" == 555 && "$owner" == 0 ]] || return 1
  # Directory st_nlink is filesystem-dependent: 2 on ext4/tmpfs (leaf dirs),
  # 1 on btrfs and overlayfs. The exact-contents check below carries integrity.
  [[ "$links" == 1 || "$links" == 2 ]] || return 1
  darkirc_validate_root_executable "$release_dir/darkirc" || return 1
  darkirc_validate_root_executable "$release_dir/lunarwing-darkirc-key-helper" || return 1
  [[ "$(stat -c '%u:%a:%h' "$release_dir/darkirc" 2>/dev/null)" == "0:555:1" ]] || return 1
  [[ "$(stat -c '%u:%a:%h' "$release_dir/lunarwing-darkirc-key-helper" 2>/dev/null)" == "0:555:1" ]] || return 1
  [[ -f "$release_dir/attestation.json" && ! -L "$release_dir/attestation.json" ]] || return 1
  [[ "$(stat -c '%u:%a:%h' "$release_dir/attestation.json" 2>/dev/null)" == "0:444:1" ]] || return 1
  [[ "$(find "$release_dir" -mindepth 1 -maxdepth 1 -printf '%f\n' 2>/dev/null | LC_ALL=C sort | tr '\n' ' ')" \
    == "attestation.json darkirc lunarwing-darkirc-key-helper " ]]
}

darkirc_validate_release_contents() {
  local release_dir="$1" release_id="$2" daemon_digest="$3" helper_digest="$4"
  local helper_source_revision="$5"
  local attestation="$release_dir/attestation.json"
  darkirc_validate_release_directory "$release_dir" "$release_id" || return 1
  [[ "sha256:$(sha256sum "$release_dir/darkirc" | cut -d' ' -f1)" == "$daemon_digest" ]] \
    || return 1
  [[ "sha256:$(sha256sum "$release_dir/lunarwing-darkirc-key-helper" | cut -d' ' -f1)" == "$helper_digest" ]] \
    || return 1
  jq -e --arg release "$release_id" --arg daemon "$daemon_digest" \
    --arg helper "$helper_digest" --arg revision "$DARKIRC_APPROVED_REV" \
    --arg helper_revision "$helper_source_revision" \
    'type == "object" and
     (keys == ["daemon_sha256","helper_sha256","helper_source_revision","key_format","profile_id","release_id","schema","source_revision"]) and
     .schema == "lunarwing.darkirc-compat/v2" and .release_id == $release and
     .profile_id == "darkfi-a05956d41-chacha-v1" and .daemon_sha256 == $daemon and
     .helper_sha256 == $helper and .helper_source_revision == $helper_revision and
     .key_format == "darkfi-chacha-base58-32" and
     .source_revision == $revision' "$attestation" >/dev/null 2>&1
}

darkirc_resolve_release() {
  local target release_id selector_owner selector_links
  reject_legacy_darkirc_path_overrides
  darkirc_path_components_safe "$DARKIRC_RELEASE_ROOT" || return 1
  darkirc_validate_root_owned_directory "$DARKIRC_RELEASE_ROOT" || return 1
  [[ -L "$DARKIRC_RELEASE_SELECTOR" ]] || return 1
  selector_owner="$(stat -c '%u' "$DARKIRC_RELEASE_SELECTOR" 2>/dev/null || true)"
  selector_links="$(stat -c '%h' "$DARKIRC_RELEASE_SELECTOR" 2>/dev/null || true)"
  [[ "$selector_owner" == 0 && "$selector_links" == 1 ]] || return 1
  target="$(readlink -- "$DARKIRC_RELEASE_SELECTOR")" || return 1
  [[ "$target" == releases/* && "$target" != */*/* ]] || return 1
  release_id="${target#releases/}"
  darkirc_validate_release_id "$release_id" || return 1
  DARKIRC_PINNED_RELEASE="$DARKIRC_RELEASES_DIR/$release_id"
  darkirc_validate_release_directory "$DARKIRC_PINNED_RELEASE" "$release_id"
}

darkirc_publish_selector() {
  local release_id="$1" selector_tmp
  selector_tmp="$DARKIRC_RELEASE_ROOT/.current.$$.tmp"
  rm -f -- "$selector_tmp"
  ln -s -- "releases/$release_id" "$selector_tmp" || return 1
  chown -h root:root "$selector_tmp"
  if ! mv -Tf -- "$selector_tmp" "$DARKIRC_RELEASE_SELECTOR"; then
    rm -f -- "$selector_tmp"
    return 1
  fi
  darkirc_fsync_path "$DARKIRC_RELEASE_ROOT"
}

build_darkirc() {
  local tenant_name="${1:-}" darkirc_src test_user build_tree built_bin source_revision cargo_home
  local staging_root="" staging_dir="" release_dir release_id daemon_digest helper_digest
  darkirc_src="${DARKIRC_SOURCE:-/var/cache/lunarwing/darkfi-source}"
  test_user="${tenant_name:-${SUDO_USER:-nobody}}"
  [[ "$test_user" != root ]] || test_user=nobody
  reject_legacy_darkirc_path_overrides

  say "acquiring build lock for darkirc ..."
  (
    flock -x 200
    [[ "$DARKIRC_REV" == "$DARKIRC_APPROVED_REV" ]] \
      || die "DarkIRC revision is not approved: $DARKIRC_REV"
    if [[ ! -d "$darkirc_src/.git" ]]; then
      local source_parent
      source_parent="$(dirname "$darkirc_src")"
      [[ -d "$source_parent" ]] || install -d -o root -g root -m 0700 "$source_parent"
      darkirc_validate_root_owned_directory "$source_parent" \
        || die "DarkIRC source parent must be root-owned and non-writable"
      say "darkirc source not present at $darkirc_src — cloning pinned source as root ..."
      git clone "$DARKIRC_REPO" "$darkirc_src" || die "darkirc clone failed"
      git -C "$darkirc_src" checkout --detach "$DARKIRC_REV" \
        || die "darkirc checkout '$DARKIRC_REV' failed"
    fi
    darkirc_validate_clean_root_checkout "$darkirc_src" "$DARKIRC_REV" \
      || die "DarkIRC source must be root-owned, clean, and at approved revision"
    [[ -f "$darkirc_src/Makefile" && -f "$darkirc_src/bin/darkirc/Cargo.toml" ]] \
      || die "approved DarkIRC source layout is incomplete"

    ensure_darkirc_build_user
    # The build account must be able to traverse into DARKIRC_BUILD_ROOT and
    # DARKIRC_BUILD_HOME; a restrictive ambient umask can otherwise leave the
    # shared parent non-traversable (e.g. 0700) and fail the unprivileged build.
    install -d -o root -g root -m 0711 "$(dirname "$DARKIRC_BUILD_ROOT")"
    install -d -o root -g root -m 0711 "$DARKIRC_BUILD_ROOT"
    build_tree="$(mktemp -d "$DARKIRC_BUILD_ROOT/build.XXXXXX")"
    cleanup_darkirc_build() {
      [[ -z "$staging_root" ]] || rm -rf -- "$staging_root"
      [[ -z "${build_tree:-}" ]] || rm -rf -- "$build_tree"
      [[ -z "${cargo_home:-}" ]] || rm -rf -- "$cargo_home"
    }
    trap cleanup_darkirc_build EXIT
    git -C "$darkirc_src" archive --format=tar "$DARKIRC_APPROVED_REV" | tar -x -C "$build_tree"
    chown -R "$DARKIRC_BUILD_USER:$DARKIRC_BUILD_GROUP" "$build_tree"
    cargo_home="$(mktemp -d "$DARKIRC_BUILD_HOME/cargo.XXXXXX")"
    install -d -o root -g root -m 0700 "$cargo_home/.cargo"
    chown -R "$DARKIRC_BUILD_USER:$DARKIRC_BUILD_GROUP" "$cargo_home"
    say "building darkirc as $DARKIRC_BUILD_USER from disposable source copy ..."
    # The build tree is a git-archive extraction (no .git), so darkirc's
    # build.rs cannot derive COMMITISH. Provide it from the attested checkout
    # so the embedded version string still carries the approved revision.
    setpriv --reuid="$(id -u "$DARKIRC_BUILD_USER")" \
      --regid="$(id -g "$DARKIRC_BUILD_USER")" --clear-groups --no-new-privs -- \
      env -i HOME="$cargo_home" PATH="/usr/bin:/bin" CARGO_HOME="$cargo_home/.cargo" \
      COMMITISH="$(git -C "$darkirc_src" rev-parse --short HEAD)" \
      MAKEFLAGS=-j8 taskset -c 0-7 make -C "$build_tree" darkirc \
      || die "darkirc build failed"
    darkirc_validate_clean_root_checkout "$darkirc_src" "$DARKIRC_REV" \
      || die "DarkIRC build changed trusted source checkout"
    built_bin="$build_tree/darkirc"
    darkirc_validate_build_artifact "$built_bin" \
      || die "darkirc build artifact failed ownership validation"
    source_revision="$(git -C "$darkirc_src" rev-parse HEAD 2>/dev/null)" \
      || die "could not attest DarkIRC source revision"
    [[ "$source_revision" == "$DARKIRC_APPROVED_REV" ]] \
      || die "DarkIRC checkout is at unapproved revision $source_revision"

    install_darkirc_key_helper
    daemon_digest="sha256:$(sha256sum "$built_bin" | cut -d' ' -f1)"
    helper_digest="sha256:$(sha256sum "$DARKIRC_HELPER_ARTIFACT" | cut -d' ' -f1)"
    release_id="${source_revision}-${DARKIRC_HELPER_SOURCE_REVISION:0:16}-${daemon_digest:7:16}-${helper_digest:7:16}"
    local release_parent
    release_parent="$(dirname "$DARKIRC_RELEASE_ROOT")"
    install -d -o root -g root -m 0755 "$release_parent"
    darkirc_fsync_path "$release_parent"
    darkirc_fsync_path "$(dirname "$release_parent")"
    install -d -o root -g root -m 0755 "$DARKIRC_RELEASE_ROOT" "$DARKIRC_RELEASES_DIR"
    staging_root="$(mktemp -d "$DARKIRC_RELEASES_DIR/.staging.XXXXXX")"
    chown root:root "$staging_root"
    chmod 0755 "$staging_root"
    staging_dir="$staging_root/$release_id"
    install -d -o root -g root -m 0755 "$staging_dir"
    install -o root -g root -m 0555 "$built_bin" "$staging_dir/darkirc"
    install -o root -g root -m 0555 "$DARKIRC_HELPER_ARTIFACT" "$staging_dir/lunarwing-darkirc-key-helper"
    write_darkirc_compatibility "$staging_dir" "$release_id" "$source_revision" "$test_user" \
      "$daemon_digest" "$helper_digest" "$DARKIRC_HELPER_SOURCE_REVISION"
    darkirc_fsync_path "$staging_dir/darkirc"
    darkirc_fsync_path "$staging_dir/lunarwing-darkirc-key-helper"
    darkirc_fsync_path "$staging_dir/attestation.json"
    chmod 0555 "$staging_dir"
    darkirc_fsync_path "$staging_dir"
    release_dir="$DARKIRC_RELEASES_DIR/$release_id"
    if [[ -e "$release_dir" || -L "$release_dir" ]]; then
      darkirc_validate_release_contents "$release_dir" "$release_id" \
        "$daemon_digest" "$helper_digest" "$DARKIRC_HELPER_SOURCE_REVISION" \
        || die "existing immutable DarkIRC release conflicts with build: $release_id"
      rm -rf -- "$staging_root"
      staging_root=""; staging_dir=""
    else
      mv -- "$staging_dir" "$release_dir"
      staging_dir=""
      rmdir -- "$staging_root"
      staging_root=""
      darkirc_fsync_path "$DARKIRC_RELEASES_DIR"
    fi
    darkirc_validate_release_contents "$release_dir" "$release_id" \
      "$daemon_digest" "$helper_digest" "$DARKIRC_HELPER_SOURCE_REVISION" \
      || die "published DarkIRC release failed validation"
    darkirc_sync_release_durability "$release_dir" \
      || die "published DarkIRC release could not be made durable"
    darkirc_publish_selector "$release_id"
    darkirc_resolve_release || die "published DarkIRC selector failed validation"
    rm -rf -- "$build_tree" "$cargo_home"
    build_tree=""; cargo_home=""
    say "darkirc build complete: $release_dir"
    sudo -u "$test_user" "$DARKIRC_RELEASE_DAEMON" --version \
      || say "(darkirc --version not supported, skipping)"
  ) 200>"$BUILD_LOCK"
}

ensure_darkirc_release() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  if ! tenant_darkirc_enabled "$name"; then
    say "DarkIRC is disabled for $name; no shared release required"
    return 0
  fi
  if (
    darkirc_load_compatibility >/dev/null 2>&1 \
      && darkirc_release_matches_current_helper_source
  ); then
    say "atomic DarkIRC release already ready for $name"
    return 0
  fi
  say "atomic DarkIRC release missing or invalid; rebuilding before unit activation"
  build_darkirc "$name"
  darkirc_load_compatibility >/dev/null \
    || die "DarkIRC release remained invalid after rebuild"
}

shared_worker_source_root() {  # <names> <with-opencode> <with-kimi> <with-opencode2> <with-pebble>
  local names="$1" with_opencode="$2" with_kimi="$3" with_opencode2="${4:-false}" with_pebble="${5:-false}"
  local name root commit dirty expected_commit="" selected_root=""
  local -a worker_paths=()
  [[ "$with_opencode" != "true" ]] || worker_paths+=(opencode4lunarwing)
  [[ "$with_opencode2" != "true" ]] || worker_paths+=(opencode2.4lunarwing)
  [[ "$with_kimi" != "true" ]] || worker_paths+=(kimicode4lunarwing)
  [[ "$with_pebble" != "true" ]] || worker_paths+=(pebble4lunarwing)
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    root="$(tenant_lw_root "$name")"
    commit="$(sudo -u "$name" git -C "$root" rev-parse HEAD 2>/dev/null)" \
      || die "cannot resolve tenant checkout for worker build: $name"
    dirty="$(sudo -u "$name" git -C "$root" status --porcelain --untracked-files=all -- "${worker_paths[@]}")" \
      || die "cannot inspect tenant worker source: $name"
    [[ -z "$dirty" ]] \
      || die "build-all refuses dirty worker source in tenant checkout: $name"
    if [[ -z "$expected_commit" ]]; then
      expected_commit="$commit"
      selected_root="$root"
    elif [[ "$commit" != "$expected_commit" ]]; then
      die "build-all cannot publish one shared worker image from mixed tenant revisions"
    fi
  done <<<"$names"
  [[ -n "$selected_root" ]] || die "no tenant checkout available for worker build"
  printf '%s\n' "$selected_root"
}

build_all() {
  local with_wasm="${1:-false}"
  local with_pebble="${2:-false}"
  local with_opencode="${3:-false}"
  local with_kimi="${4:-false}"
  local with_toolchains="${5:-false}"
  local with_opencode2="${6:-false}"
  local names
  names="$(all_tenant_names)"

  if [[ -z "$names" ]]; then
    say "no tenants registered"
    return 0
  fi

  local worker_source_root=""
  if [[ "$with_opencode" == "true" || "$with_kimi" == "true" || "$with_opencode2" == "true" || "$with_pebble" == "true" ]]; then
    worker_source_root="$(shared_worker_source_root "$names" "$with_opencode" "$with_kimi" "$with_opencode2" "$with_pebble")"
  fi

  if [[ "$with_pebble" == "true" ]]; then
    say ""
    say "=== Building pebble worker image ==="
    build_pebble_worker "false" "$with_toolchains" "$worker_source_root"
  fi

  if [[ "$with_opencode" == "true" ]]; then
    say ""
    say "=== Building opencode worker image ==="
    build_opencode_worker "false" "$with_toolchains" "$worker_source_root"
  fi

  if [[ "$with_kimi" == "true" ]]; then
    say ""
    say "=== Building kimi worker image ==="
    build_kimi_worker "false" "$with_toolchains" "" "$worker_source_root"
  fi

  if [[ "$with_opencode2" == "true" ]]; then
    say ""
    say "=== Building opencode2 worker image ==="
    build_opencode2_worker "false" "$with_toolchains" "$worker_source_root"
  fi

  while IFS= read -r name; do
    say ""
    say "=== Building tenant: $name ==="
    build_tenant "$name" "$with_wasm" "false" "false" "false" "$with_toolchains" "false"
  done <<< "$names"
}

# Run git against a worker build's source root, retrying as the directory's
# owner when the invoking account (root) is refused by git's dubious-ownership
# check — the shape shared_worker_source_root already uses for tenant checkouts.
# Usage: _worker_source_git <source-root> <git args...>
_worker_source_git() {
  local root="$1"; shift
  local out owner
  if out="$(git -C "$root" "$@" 2>/dev/null)"; then
    printf '%s' "$out"
    return 0
  fi
  owner="$(stat -c %U "$root" 2>/dev/null)" || return 1
  [[ -n "$owner" && "$owner" != "$(id -un)" ]] || return 1
  # `cd /`: sudo keeps the caller's cwd, which the owner may not be able to
  # enter (see _ctr).
  ( cd / && exec sudo -u "$owner" git -C "$root" "$@" 2>/dev/null )
}

# Source revision for a worker image's org.opencontainers.image.revision label.
# Every worker Dockerfile declares ARG SOURCE_REVISION=unknown and CI's
# build-worker-image.sh passes $CI_COMMIT_SHA, but mt-admin builds passed
# nothing, so an image built here could not be traced to a commit afterwards
# (nobody could say which target the promoted kimi 2.2.0 image came from).
# Uncommitted or untracked changes under the worker directory append -dirty so
# the label never claims a commit the image does not match; "unknown" when the
# root is not a readable git checkout.
# Usage: _worker_source_revision <source-root> <worker-subdir>
_worker_source_revision() {
  local root="$1" subdir="$2" revision dirty
  revision="$(_worker_source_git "$root" rev-parse --verify HEAD)" || revision=""
  if ! [[ "$revision" =~ ^[0-9a-f]{40}$ ]]; then
    printf 'unknown'
    return 0
  fi
  # An unreadable status also marks the build dirty: unverified is not clean.
  dirty="$(_worker_source_git "$root" status --porcelain --untracked-files=all -- "$subdir")" || dirty="unverified"
  [[ -z "$dirty" ]] || revision="${revision}-dirty"
  printf '%s' "$revision"
}

WORKER_BUILD_SOURCE_ROOT=""
WORKER_BUILD_SOURCE_REVISION=""
WORKER_BUILD_ARCHIVE_ROOT=""
WORKER_BUILD_PRIOR_EXIT_TRAP=""
WORKER_BUILD_PRIOR_EXIT_ACTION=""

_operator_worker_release_ref() {  # <branch, tag, remote ref, or commit>
  local requested="$1" candidate
  if [[ "$requested" =~ ^[0-9a-f]{40}$ ]]; then
    printf '%s\n' "$requested"
    return 0
  fi
  local -a candidates=()
  case "$requested" in
    refs/*)   candidates=("$requested") ;;
    origin/*) candidates=("refs/remotes/$requested" "refs/heads/${requested#origin/}" "refs/tags/${requested#origin/}") ;;
    *)        candidates=("refs/heads/$requested" "refs/remotes/origin/$requested" "refs/tags/$requested") ;;
  esac
  for candidate in "${candidates[@]}"; do
    git check-ref-format "$candidate" >/dev/null 2>&1 || continue
    if git -C "$LUNARWING_ROOT" rev-parse --verify --quiet "${candidate}^{commit}" >/dev/null 2>&1; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

_worker_source_provenance() {  # <tenant source root> <lane> [release ref]
  local source_root="$1" lane="$2" requested="${3:-}"
  local revision dirty owner branch release_ref
  revision="$(_worker_source_git "$source_root" rev-parse --verify HEAD)" || revision=""
  [[ "$revision" =~ ^[0-9a-f]{40}$ ]] \
    || die "cannot verify tenant worker checkout HEAD: $source_root"
  dirty="$(_worker_source_git "$source_root" status --porcelain --untracked-files=all -- "$lane")" \
    || die "cannot inspect tenant worker source: $source_root"
  [[ -z "$dirty" ]] \
    || die "refusing to build a shared worker image from a dirty tenant checkout: $source_root ($lane)"
  owner="$(stat -c %U "$source_root" 2>/dev/null)" || owner="unknown"
  git -C "$LUNARWING_ROOT" cat-file -e "${revision}^{commit}" 2>/dev/null \
    || die "tenant '$owner' HEAD $revision is unknown to operator checkout $LUNARWING_ROOT"
  if [[ -z "$requested" ]]; then
    branch="$(_worker_source_git "$source_root" symbolic-ref --quiet --short HEAD)" || branch=""
    [[ -n "$branch" ]] \
      || die "tenant '$owner' HEAD $revision is detached; an explicit release ref is required"
    requested="$branch"
  fi
  release_ref="$(_operator_worker_release_ref "$requested")" || release_ref=""
  [[ -n "$release_ref" ]] \
    || die "tenant '$owner' HEAD $revision has no operator release ref matching '$requested'"
  git -C "$LUNARWING_ROOT" merge-base --is-ancestor "$revision" "$release_ref" \
    || die "tenant '$owner' HEAD $revision is not on operator release ref $release_ref"
  WORKER_BUILD_SOURCE_REVISION="$revision"
}

_archive_worker_source() {  # <tenant source root> <lane>
  local source_root="$1" lane="$2" tmp_parent tmp archive links
  local -a paths=("$lane")
  [[ "$lane" != opencode2.4lunarwing ]] \
    || paths+=(.gitlab/scripts/verify-opencode2-exposure-out-of-band.py)
  require_cmd tar
  tmp_parent="${LUNARWING_WORKER_BUILD_TMPDIR:-/var/tmp}"
  [[ -d "$tmp_parent" && ! -L "$tmp_parent" ]] \
    || die "worker archive parent is not a real directory: $tmp_parent"
  tmp="$(mktemp -d "$tmp_parent/lw-worker-source.XXXXXX")" \
    || die "cannot create private worker source archive under $tmp_parent"
  chmod 0700 "$tmp" || { rm -rf -- "$tmp"; die "cannot protect worker source archive $tmp"; }
  archive="$tmp/source.tar"
  if ! git -C "$LUNARWING_ROOT" archive --format=tar --output="$archive" \
      "$WORKER_BUILD_SOURCE_REVISION" -- "${paths[@]}" \
    || ! tar -x -f "$archive" -C "$tmp"; then
    rm -rf -- "$tmp"
    die "cannot archive tenant revision $WORKER_BUILD_SOURCE_REVISION from operator checkout"
  fi
  rm -f -- "$archive" || { rm -rf -- "$tmp"; die "cannot remove worker source archive stream"; }
  links="$(find "$tmp" ! -type f ! -type d -print -quit)" \
    || { rm -rf -- "$tmp"; die "cannot validate worker source archive $tmp"; }
  [[ -z "$links" ]] || { rm -rf -- "$tmp"; die "worker source archive contains a link or special file"; }
  links="$(find "$tmp" -type f -links +1 -print -quit)" \
    || { rm -rf -- "$tmp"; die "cannot validate worker source archive links"; }
  [[ -z "$links" ]] || { rm -rf -- "$tmp"; die "worker source archive contains a hard link"; }
  [[ -f "$tmp/$lane/Dockerfile" && ! -L "$tmp/$lane/Dockerfile" ]] \
    || { rm -rf -- "$tmp"; die "archived worker source has no regular $lane/Dockerfile"; }
  WORKER_BUILD_SOURCE_ROOT="$tmp"
  WORKER_BUILD_ARCHIVE_ROOT="$tmp"
}

_prepare_worker_build_source() {  # <source root> <lane> [release ref]
  local source_root="$1" lane="$2" release_ref="${3:-}"
  WORKER_BUILD_SOURCE_ROOT="$source_root"
  WORKER_BUILD_ARCHIVE_ROOT=""
  if [[ "$source_root" == "$LUNARWING_ROOT" ]]; then
    WORKER_BUILD_SOURCE_REVISION="$(_worker_source_revision "$source_root" "$lane")"
    return 0
  fi
  _worker_source_provenance "$source_root" "$lane" "$release_ref"
  _archive_worker_source "$source_root" "$lane"
}

_cleanup_worker_build_context() {  # <archive root>
  local root="$1"
  [[ -n "$root" && -d "$root" && ! -L "$root" ]] || return 0
  rm -rf -- "$root"
}

_worker_build_context_begin() {
  WORKER_BUILD_PRIOR_EXIT_TRAP="$(trap -p EXIT)"
  WORKER_BUILD_PRIOR_EXIT_ACTION=""
  if [[ -n "$WORKER_BUILD_PRIOR_EXIT_TRAP" ]]; then
    local -a trap_parts=()
    eval "trap_parts=($WORKER_BUILD_PRIOR_EXIT_TRAP)"
    WORKER_BUILD_PRIOR_EXIT_ACTION="${trap_parts[2]:-}"
  fi
  trap '_worker_build_context_exit $?' EXIT
}

_worker_build_context_exit() {  # <status>
  local status="$1"
  trap - EXIT
  _cleanup_worker_build_context "$WORKER_BUILD_ARCHIVE_ROOT" || true
  [[ -z "$WORKER_BUILD_PRIOR_EXIT_ACTION" ]] || eval "$WORKER_BUILD_PRIOR_EXIT_ACTION"
  exit "$status"
}

_worker_build_context_finish() {
  [[ -n "$WORKER_BUILD_ARCHIVE_ROOT" ]] || return 0
  _cleanup_worker_build_context "$WORKER_BUILD_ARCHIVE_ROOT" \
    || die "cannot remove worker source archive $WORKER_BUILD_ARCHIVE_ROOT"
  WORKER_BUILD_ARCHIVE_ROOT=""
  if [[ -n "$WORKER_BUILD_PRIOR_EXIT_TRAP" ]]; then
    eval "$WORKER_BUILD_PRIOR_EXIT_TRAP"
  else
    trap - EXIT
  fi
  WORKER_BUILD_PRIOR_EXIT_TRAP=""
  WORKER_BUILD_PRIOR_EXIT_ACTION=""
}

# WI#527: the image tag a worker build writes. Defaults to :latest so every
# existing caller (build-tenant, build-all, per-lane verbs) is unchanged; the
# upgrade path sets LUNARWING_WORKER_IMAGE_TAG to a candidate tag so the images
# build while the tenant still runs and are promoted only after every selected
# lane has succeeded (run-287 SX-2).
worker_image_tag() {
  printf '%s' "${LUNARWING_WORKER_IMAGE_TAG:-latest}"
}

worker_image_ref() {  # <lane> [tag]
  printf 'lunarwing-worker-%s:%s' "$1" "${2:-$(worker_image_tag)}"
}

build_pebble_worker() {
  local no_cache="${1:-false}"
  local with_toolchains="${2:-false}"
  local source_root="${3:-$LUNARWING_ROOT}"
  local release_ref="${4:-}"
  _prepare_worker_build_source "$source_root" pebble4lunarwing "$release_ref"
  source_root="$WORKER_BUILD_SOURCE_ROOT"
  local pebble_dir="${source_root}/pebble4lunarwing"
  local source_revision="$WORKER_BUILD_SOURCE_REVISION"
  [[ -z "$WORKER_BUILD_ARCHIVE_ROOT" ]] || _worker_build_context_begin

  [[ -d "$pebble_dir" ]] || die "pebble worker dir not found at $pebble_dir"

  ensure_container_runtime

  local toolchain_desc="slim (no toolchains)"
  [[ "$with_toolchains" == "true" ]] && toolchain_desc="fat (with Rust/Go/C++ toolchains)"
  local -a revision_args=(--build-arg "SOURCE_REVISION=$source_revision")
  say "building pebble worker Docker image [$toolchain_desc] (source revision: $source_revision) ..."
  local cache_flag=""
  [[ "$no_cache" == "true" ]] && cache_flag="--no-cache"

  local -a toolchain_args=()
  [[ "$with_toolchains" == "true" ]] && toolchain_args=(--build-arg WITH_TOOLCHAINS=true)

  # Fat builds target a candidate tag first: the toolchain smoke gate below
  # must pass before the image is re-tagged to the build's output tag, so a
  # failed probe can never replace the previous good image (WI#334).
  # WI#527: the output tag honours LUNARWING_WORKER_IMAGE_TAG (upgrade builds
  # write a candidate tag and promote after every lane succeeded).
  local output_tag fat_candidate build_tag
  output_tag="$(worker_image_ref pebble)"
  # run-300 TO-6: the fat staging tag is derived from this build's output tag
  # (candidate -> wi527-<tenant>-candidate), so two concurrent builds — another
  # tenant's upgrade or a build-pebble-worker --with-toolchains — can never
  # retag each other's image between the smoke gate and the final retag.
  fat_candidate="${output_tag}-candidate"
  build_tag="$output_tag"
  [[ "$with_toolchains" == "true" ]] && build_tag="$fat_candidate"

  if [[ "$CONTAINER_RT" == "podman" ]]; then
    # --network=host: podman's default build network
    # can't reach the internet for RUN steps on this host; the host netns has IPv4.
    # --format docker (O4): preserve the Dockerfile HEALTHCHECK (podman OCI drops it).
    podman build $cache_flag "${toolchain_args[@]}" "${revision_args[@]}" --network=host --format docker -t "$build_tag" -f "$pebble_dir/Dockerfile" "$source_root" \
      || die "pebble worker image build failed"
  else
    docker build $cache_flag "${toolchain_args[@]}" "${revision_args[@]}" -t "$build_tag" -f "$pebble_dir/Dockerfile" "$source_root" \
      || die "pebble worker image build failed"
  fi

  if [[ "$with_toolchains" == "true" ]]; then
    # WI#334: prove the task-shell toolchain anchors on the freshly built fat
    # image (pebble's real /bin/sh -lc spawn mode plus a sanitised shell).
    # The slim image has no toolchains, so the probe is fat-only.
    # CONTAINER_RT is passed explicitly: the smoke must probe the same store
    # this build just wrote to (its own autodetect prefers podman, which may
    # not be the build runtime).
    # LUNARWING_PEBBLE_TOOLCHAIN_SMOKE is a test hook for harnesses to stub
    # the probe; it is undocumented on purpose and only settable from the
    # build operator's own environment. Production runs use the committed
    # script from the operator checkout, never the tenant source root.
    local smoke_script="${LUNARWING_PEBBLE_TOOLCHAIN_SMOKE:-$LUNARWING_ROOT/pebble4lunarwing/scripts/smoke-task-shell-toolchain.sh}"
    say "smoke-checking task-shell toolchain anchors (WI#334) ..."
    CONTAINER_RT="$CONTAINER_RT" "$smoke_script" image "$fat_candidate" \
      || die "pebble worker task-shell toolchain smoke check failed (WI#334)"
    "$CONTAINER_RT" tag "$fat_candidate" "$output_tag" \
      || die "pebble worker image tag failed"
    "$CONTAINER_RT" rmi "$fat_candidate" >/dev/null 2>&1 || true
  fi

  say "pebble worker image built: $output_tag [$toolchain_desc] (revision $source_revision)"
  _worker_build_context_finish
}

build_opencode_worker() {
  local no_cache="${1:-false}"
  local with_toolchains="${2:-false}"
  local source_root="${3:-$LUNARWING_ROOT}"
  local release_ref="${4:-}"
  _prepare_worker_build_source "$source_root" opencode4lunarwing "$release_ref"
  source_root="$WORKER_BUILD_SOURCE_ROOT"
  local opencode_dir="${source_root}/opencode4lunarwing"
  local source_revision="$WORKER_BUILD_SOURCE_REVISION"
  [[ -z "$WORKER_BUILD_ARCHIVE_ROOT" ]] || _worker_build_context_begin

  [[ -d "$opencode_dir" ]] || die "opencode worker dir not found at $opencode_dir"

  ensure_container_runtime

  local toolchain_desc="slim (no toolchains)"
  [[ "$with_toolchains" == "true" ]] && toolchain_desc="fat (with Rust/Go/C++ toolchains)"
  local -a revision_args=(--build-arg "SOURCE_REVISION=$source_revision")
  say "building opencode worker Docker image [$toolchain_desc] (source revision: $source_revision) ..."

  local -a cache_args=()
  [[ "$no_cache" == "true" ]] && cache_args=(--no-cache)

  local -a toolchain_args=()
  [[ "$with_toolchains" == "true" ]] && toolchain_args=(--build-arg WITH_TOOLCHAINS=true)

  local output_tag
  output_tag="$(worker_image_ref opencode)"

  if [[ "$CONTAINER_RT" == "podman" ]]; then
    local -a layer_args=()
    if [[ "$no_cache" == "true" ]]; then
      # --no-cache avoids reading old layers but still creates new intermediate
      # layers by default. Drop safe dangling images first, then keep this large
      # Bun build in one layer per stage to reduce peak storage and inode use.
      say "pruning dangling Podman images before uncached OpenCode build ..."
      if ! podman image prune -f >/dev/null; then
        say "WARNING: failed to prune dangling Podman images; continuing build" >&2
      fi
      layer_args=(--layers=false)
    fi
    podman build "${cache_args[@]}" "${layer_args[@]}" "${toolchain_args[@]}" "${revision_args[@]}" --network=host --format docker -t "$output_tag" "$opencode_dir" \
      || die "opencode worker image build failed"
  else
    docker build "${cache_args[@]}" "${toolchain_args[@]}" "${revision_args[@]}" -t "$output_tag" "$opencode_dir" \
      || die "opencode worker image build failed"
  fi

  say "opencode worker image built: $output_tag [$toolchain_desc] (revision $source_revision)"
  _worker_build_context_finish
}

_opencode2_snapshot_source() {
  local source_root="$1" source="$2"
  local parent base resolved_parent prefix
  parent="$(dirname -- "$source")"
  base="$(basename -- "$source")"
  resolved_parent="$(realpath -e -- "$parent")" || {
    echo "ERROR: cannot resolve checkout source parent for $source (WI#425 out-of-band)" >&2
    return 1
  }
  prefix="${source_root%/}/"
  if [[ "$resolved_parent" != "$source_root" && "$resolved_parent" != "$prefix"* ]]; then
    echo "ERROR: checkout source $source escapes the checkout root (WI#425 out-of-band)" >&2
    return 1
  fi
  printf '%s/%s\n' "$resolved_parent" "$base"
}

# Resolve every source parent below the canonical checkout, copy without
# following final links, then refuse every link or special file before any
# snapshot Python is executed. The snapshot is root-owned and read-only.
_opencode2_snapshot_checkout() {
  local source_root="$1" opencode2_dir="$2" tmp_dir="$3"
  local snapshot="$tmp_dir/checkout" name source resolved_root
  local lane="$snapshot/opencode2.4lunarwing"
  resolved_root="$(realpath -e -- "$source_root")" || return 1
  mkdir -p "$snapshot/.gitlab/scripts" "$lane" || return 1
  for name in scripts config; do
    source="$(_opencode2_snapshot_source "$resolved_root" "$opencode2_dir/$name")" || return 1
    cp -R --no-dereference --no-preserve=ownership -- \
      "$source" "$lane/$name" || return 1
  done
  for name in entrypoint.sh entrypoint-split.sh health_server.py agent_comm_protocol.json; do
    source="$(_opencode2_snapshot_source "$resolved_root" "$opencode2_dir/$name")" || return 1
    cp -R --no-dereference --no-preserve=ownership -- \
      "$source" "$lane/$name" || return 1
  done
  source="$(_opencode2_snapshot_source "$resolved_root" \
    "$source_root/.gitlab/scripts/verify-opencode2-exposure-out-of-band.py")" || return 1
  cp -R --no-dereference --no-preserve=ownership -- \
    "$source" "$snapshot/.gitlab/scripts/verify-opencode2-exposure-out-of-band.py" || return 1
  if [[ -n "$(find "$snapshot" ! -type f ! -type d -print -quit)" ]]; then
    echo "ERROR: checkout snapshot contains a link or special file (WI#425 out-of-band)" >&2
    return 1
  fi
  chmod -R go-w,a+rX "$snapshot" || return 1
  printf '%s\n' "$snapshot"
}

# Verify by immutable image ID using the same tar policy as protected CI.
# Tenant-controlled Python runs only as nobody, with an empty environment.
# Usage: _verify_opencode2_image_out_of_band <image-id> <source-root> <wrapper-sha256>
_verify_opencode2_image_out_of_band() (
  local image_ref="$1" source_root="$2" wrapper_anchor="$3"
  local opencode2_dir="$source_root/opencode2.4lunarwing"
  local cid='' tmp_dir inputs archive config_json config_file snapshot snapshot_anchor
  local python nobody_uid nobody_gid work
  [[ -n "$image_ref" && -d "$opencode2_dir" && "$wrapper_anchor" =~ ^[0-9a-f]{64}$ ]] || return 2

  tmp_dir="$(mktemp -d "/var/tmp/lw-oob.XXXXXX")" || return 1
  trap '_status=$?; [[ -z "$cid" ]] || "$CONTAINER_RT" rm -f "$cid" >/dev/null 2>&1 || true; rm -rf "$tmp_dir"; exit "$_status"' EXIT
  inputs="$tmp_dir/inputs"
  mkdir -m 0700 "$inputs" || return 1
  cid="$("$CONTAINER_RT" create --pull=never "$image_ref" 2>/dev/null)" || cid=''
  if [[ -z "$cid" || ! "$cid" =~ ^[[:alnum:]]+$ ]]; then
    echo "ERROR: out-of-band verification could not create a container from $image_ref" >&2
    return 1
  fi
  archive="$inputs/rootfs.tar"
  if ! "$CONTAINER_RT" export --output "$archive" "$cid" >/dev/null 2>&1 \
     || [[ ! -f "$archive" || -L "$archive" ]]; then
    echo "ERROR: cannot export the opencode2 image filesystem (WI#425 out-of-band)" >&2
    return 1
  fi
  config_json="$("$CONTAINER_RT" image inspect "$image_ref" 2>/dev/null)" || {
    echo "ERROR: cannot inspect full image config for $image_ref (WI#425 out-of-band)" >&2
    return 1
  }
  config_file="$inputs/image-config.json"
  printf '%s' "$config_json" > "$config_file" || return 1
  snapshot="$(_opencode2_snapshot_checkout "$source_root" "$opencode2_dir" "$tmp_dir")" || return 1
  snapshot_anchor="$(sha256sum < "$snapshot/opencode2.4lunarwing/scripts/opencode-exposure-guard.sh")" || return 1
  snapshot_anchor="${snapshot_anchor%% *}"
  if [[ "$snapshot_anchor" != "$wrapper_anchor" ]]; then
    echo "ERROR: checkout wrapper changed between build and verification (WI#425 out-of-band)" >&2
    return 1
  fi

  python="$(command -v python3)" || return 1
  nobody_uid="$(id -u nobody 2>/dev/null)" || nobody_uid=''
  nobody_gid="$(id -g nobody 2>/dev/null)" || nobody_gid=''
  if [[ ! "$nobody_uid" =~ ^[1-9][0-9]*$ || ! "$nobody_gid" =~ ^[1-9][0-9]*$ ]]; then
    echo "ERROR: an unprivileged nobody account is required for out-of-band verification" >&2
    return 1
  fi
  work="$tmp_dir/work"
  mkdir -m 0700 "$work" \
    && chown "$nobody_uid:$nobody_gid" "$work" "$inputs" "$archive" "$config_file" \
    && chmod 0711 "$tmp_dir" && chmod 0700 "$work" "$inputs" \
    && chmod 0600 "$archive" "$config_file" || return 1
  if ! setpriv --reuid="$nobody_uid" --regid="$nobody_gid" --clear-groups --no-new-privs -- \
      env -i PATH=/usr/bin:/bin PYTHONDONTWRITEBYTECODE=1 \
      "$python" -I "$snapshot/.gitlab/scripts/verify-opencode2-exposure-out-of-band.py" \
      --archive "$archive" --config "$config_file" --workspace-dir "$work"; then
    echo "ERROR: shared opencode2 export verification failed (WI#425 out-of-band)" >&2
    return 1
  fi
  say "out-of-band verify: $image_ref passes the shared tar/config exposure policy"
)

# Builder capability floor for the opencode2 image's inline `RUN --network=none`
# smoke step. Kept separate from build_opencode2_worker so upgrade-tenant can
# preflight it BEFORE stopping the tenant (WI#456): build_tenant rebuilds
# pebble/opencode/kimi first and only then reaches this gate, so on a host below
# the floor the build's failure lands after the tenant is already stopped.
#
# Never dies: a caller decides how to fail. Returns 0 when the active builder
# meets the floor, otherwise 1 with an operator-actionable reason in
# OPENCODE2_BUILDER_FLOOR_PROBLEM. It runs outside command substitution so
# ensure_container_runtime's CONTAINER_RT/MT_ROOTLESS detection reaches the
# caller's shell.
OPENCODE2_BUILDER_FLOOR_PROBLEM=""
opencode2_builder_supported() {
  OPENCODE2_BUILDER_FLOOR_PROBLEM=""
  ensure_container_runtime
  if [[ "$CONTAINER_RT" == "podman" ]]; then
    if ! podman_supports_run_network; then
      OPENCODE2_BUILDER_FLOOR_PROBLEM="opencode2 worker builds require Podman >= 4.5 (Buildah >= 1.30) for RUN --network=none; upgrade Podman before rebuilding"
      return 1
    fi
    return 0
  fi
  case "${DOCKER_BUILDKIT:-1}" in
    1|true|True|TRUE|t|T) ;;
    *)
      OPENCODE2_BUILDER_FLOOR_PROBLEM="opencode2 worker builds require Docker BuildKit for RUN --network=none; enable DOCKER_BUILDKIT=1 (legacy builder unsupported)"
      return 1
      ;;
  esac
  if ! docker buildx version >/dev/null 2>&1; then
    OPENCODE2_BUILDER_FLOOR_PROBLEM="opencode2 worker builds require Docker BuildKit with the buildx plugin; install buildx before rebuilding"
    return 1
  fi
  return 0
}

build_opencode2_worker() {
  local no_cache="${1:-false}"
  local with_toolchains="${2:-false}"
  local source_root="${3:-$LUNARWING_ROOT}"
  local release_ref="${4:-}"
  _prepare_worker_build_source "$source_root" opencode2.4lunarwing "$release_ref"
  source_root="$WORKER_BUILD_SOURCE_ROOT"
  local opencode2_dir="${source_root}/opencode2.4lunarwing"
  local source_revision="$WORKER_BUILD_SOURCE_REVISION"
  [[ -z "$WORKER_BUILD_ARCHIVE_ROOT" ]] || _worker_build_context_begin

  [[ -d "$opencode2_dir" ]] || die "opencode2 worker dir not found at $opencode2_dir"

  opencode2_builder_supported \
    || die "$OPENCODE2_BUILDER_FLOOR_PROBLEM"

  local toolchain_desc="slim (no toolchains)"
  [[ "$with_toolchains" == "true" ]] && toolchain_desc="fat (with Rust/Go/C++ toolchains)"
  local -a revision_args=(--build-arg "SOURCE_REVISION=$source_revision")
  say "building opencode2 worker Docker image [$toolchain_desc] (source revision: $source_revision) ..."

  local -a cache_args=()
  [[ "$no_cache" == "true" ]] && cache_args=(--no-cache)

  local -a toolchain_args=()
  [[ "$with_toolchains" == "true" ]] && toolchain_args=(--build-arg WITH_TOOLCHAINS=true)

  # WI#357 run-106 GJ-3(a): pass the checkout's exposure-guard wrapper digest
  # for the in-image --image-filesystem comparison. This detects accidental
  # drift; an adversarial Dockerfile can still replace the environment or
  # checker — the WI#425 out-of-band verification below re-checks the built
  # image host-side with the checkout's own bytes.
  local wrapper_file="$opencode2_dir/scripts/opencode-exposure-guard.sh" wrapper_anchor
  wrapper_anchor=$(sha256sum < "$wrapper_file") || die "cannot hash $wrapper_file"
  wrapper_anchor=${wrapper_anchor%% *}
  [[ "$wrapper_anchor" =~ ^[0-9a-f]{64}$ ]] || die "invalid wrapper sha256 for $wrapper_file"

  # A per-build tag and --iidfile avoid cross-build verification races. Verify
  # and promote only the immutable ID; the friendly tag is never trusted.
  # WI#527: the verified ID is tagged at the build's output tag (candidate on the
  # upgrade path, :latest otherwise) so a pre-stop build never disturbs the
  # running lane's image until every selected lane succeeded.
  local candidate_tag="lunarwing-worker-opencode2:candidate-$$" iid_file image_id output_tag
  output_tag="$(worker_image_ref opencode2)"
  iid_file="$(mktemp "${TMPDIR:-/tmp}/lw-opencode2-iid.XXXXXX")" \
    || die "cannot create opencode2 image-id file"
  local -a identity_args=(--iidfile "$iid_file")
  if [[ "$CONTAINER_RT" == "podman" ]]; then
    local -a layer_args=()
    if [[ "$no_cache" == "true" ]]; then
      say "pruning dangling Podman images before uncached OpenCode 2 build ..."
      if ! podman image prune -f >/dev/null; then
        say "WARNING: failed to prune dangling Podman images; continuing build" >&2
      fi
      layer_args=(--layers=false)
    fi
    # --network=host: on affected tenant hosts, the default Podman build
    # network cannot reach upstreams while the host netns can. Consequence (WI#511):
    # every RUN step in the worker images shares the HOST socket table, so a
    # build-time listener check must scope itself — the opencode2 runtime smoke
    # does that with `RUN --network=none` (its own private netns, loopback up)
    # rather than trying to attribute host listeners to a uid.
    podman build "${cache_args[@]}" "${layer_args[@]}" "${toolchain_args[@]}" "${revision_args[@]}" --build-arg "LUNARWING_WRAPPER_SHA256=$wrapper_anchor" --network=host --format docker "${identity_args[@]}" -t "$candidate_tag" "$opencode2_dir" \
      || { rm -f "$iid_file"; die "opencode2 worker image build failed"; }
  else
    # Explicit opt-in prevents Docker silently falling back to the legacy builder.
    DOCKER_BUILDKIT=1 docker build "${cache_args[@]}" "${toolchain_args[@]}" "${revision_args[@]}" --build-arg "LUNARWING_WRAPPER_SHA256=$wrapper_anchor" "${identity_args[@]}" -t "$candidate_tag" "$opencode2_dir" \
      || { rm -f "$iid_file"; die "opencode2 worker image build failed"; }
  fi
  image_id="$(<"$iid_file")"
  rm -f "$iid_file"
  [[ "$image_id" =~ ^(sha256:)?[0-9a-f]{64}$ ]] \
    || die "opencode2 build did not produce a valid immutable image ID"

  if ! _verify_opencode2_image_out_of_band "$image_id" "$source_root" "$wrapper_anchor"; then
    "$CONTAINER_RT" rmi "$candidate_tag" >/dev/null 2>&1 || true
    die "out-of-band opencode2 image verification FAILED for $image_id (WI#425)"
  fi
  "$CONTAINER_RT" tag "$image_id" "$output_tag" \
    || die "cannot re-tag verified $image_id as $output_tag"
  "$CONTAINER_RT" rmi "$candidate_tag" >/dev/null 2>&1 || true

  say "opencode2 worker image built: $output_tag [$toolchain_desc] (revision $source_revision)"
  _worker_build_context_finish
}

build_kimi_worker() {
  local no_cache="${1:-false}"
  local with_toolchains="${2:-false}"
  local kimi_code_version="${3:-}"
  local source_root="${4:-$LUNARWING_ROOT}"
  local release_ref="${5:-}"
  _prepare_worker_build_source "$source_root" kimicode4lunarwing "$release_ref"
  source_root="$WORKER_BUILD_SOURCE_ROOT"
  local kimi_dir="${source_root}/kimicode4lunarwing"
  local source_revision="$WORKER_BUILD_SOURCE_REVISION"
  [[ -z "$WORKER_BUILD_ARCHIVE_ROOT" ]] || _worker_build_context_begin

  [[ -d "$kimi_dir" ]] || die "kimi worker dir not found at $kimi_dir"

  ensure_container_runtime

  local toolchain_desc="slim (no toolchains)"
  [[ "$with_toolchains" == "true" ]] && toolchain_desc="fat (with Rust/Go/C++ toolchains)"
  local version_desc="Dockerfile default"
  [[ -z "$kimi_code_version" ]] || version_desc="$kimi_code_version"
  local -a revision_args=(--build-arg "SOURCE_REVISION=$source_revision")
  say "building kimi worker Docker image [$toolchain_desc] (kimi-code version: ${version_desc}, source revision: $source_revision) ..."

  local -a cache_args=()
  [[ "$no_cache" == "true" ]] && cache_args=(--no-cache)

  local -a toolchain_args=()
  [[ "$with_toolchains" == "true" ]] && toolchain_args=(--build-arg WITH_TOOLCHAINS=true)

  local -a version_args=()
  [[ -z "$kimi_code_version" ]] || version_args=(--build-arg KIMI_CODE_VERSION="${kimi_code_version}")
  local output_tag
  output_tag="$(worker_image_ref kimi)"

  if [[ "$CONTAINER_RT" == "podman" ]]; then
    local -a layer_args=()
    if [[ "$no_cache" == "true" ]]; then
      say "pruning dangling Podman images before uncached Kimi build ..."
      if ! podman image prune -f >/dev/null; then
        say "WARNING: failed to prune dangling Podman images; continuing build" >&2
      fi
      layer_args=(--layers=false)
    fi
    podman build "${cache_args[@]}" "${layer_args[@]}" "${toolchain_args[@]}" "${version_args[@]}" "${revision_args[@]}" --network=host --format docker -t "$output_tag" "$kimi_dir" \
      || die "kimi worker image build failed"
  else
    docker build "${cache_args[@]}" "${toolchain_args[@]}" "${version_args[@]}" "${revision_args[@]}" -t "$output_tag" "$kimi_dir" \
      || die "kimi worker image build failed"
  fi

  say "kimi worker image built: $output_tag [$toolchain_desc] (revision $source_revision)"
  _worker_build_context_finish
}

# ── WASM install ─────────────────────────────────────────────────────────────

channel_crate_name() {
  case "$1" in
    weechat) printf 'weechat_relay_channel' ;;
    *)       printf '%s_channel' "$1" ;;
  esac
}

tool_binary_name() {
  printf '%s_tool' "$(printf '%s' "$1" | tr '-' '_')"
}

install_wasm_tenant() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  local repo state_dir
  repo="$(tenant_repo "$name")"
  state_dir="$(tenant_state_dir "$name")"

  local channels_dir="$state_dir/channels"
  local tools_dir="$state_dir/tools"

  mkdir -p "$channels_dir" "$tools_dir"

  # Resolve wasm-tools. add-tenant installs it for the *tenant* user under
  # ~/.cargo/bin; this function runs as root/admin, so prefer the tenant's copy
  # (otherwise we'd miss it and warn spuriously) before falling back to the
  # admin PATH. Output is chowned to the tenant at the end either way.
  local wasm_tools="" tenant_wasm_tools
  tenant_wasm_tools="$(tenant_home "$name")/.cargo/bin/wasm-tools"
  if [[ -x "$tenant_wasm_tools" ]]; then
    wasm_tools="$tenant_wasm_tools"
  elif command -v wasm-tools >/dev/null 2>&1; then
    wasm_tools="wasm-tools"
  fi

  local has_wasm_tools=true
  if [[ -z "$wasm_tools" ]]; then
    say "note: wasm-tools not installed — installing raw WASM components (works fine; skipping optional debug-info strip)"
    has_wasm_tools=false
  fi

  local installed=0 skipped=0

  say "installing WASM channels to $channels_dir..."
  for dir in "$repo/channels-src"/*/; do
    [[ -d "$dir" ]] || continue
    local ch_name crate_name src_wasm dest_wasm caps_src caps_dest
    ch_name="$(basename "$dir")"
    crate_name="$(channel_crate_name "$ch_name")"
    src_wasm="$dir/target/wasm32-wasip2/release/${crate_name}.wasm"
    dest_wasm="$channels_dir/${ch_name}.wasm"
    caps_src="$dir/${ch_name}.capabilities.json"
    caps_dest="$channels_dir/${ch_name}.capabilities.json"

    if [[ ! -f "$src_wasm" ]]; then
      skipped=$((skipped + 1))
      continue
    fi

    # Honor the enable_darkirc tenant flag: without --enable-darkirc the
    # tenant has no darkirc adapter, and installing the channel anyway starts
    # a 3s poll loop against a nonexistent default adapter (log-flooding
    # connect errors). Skip installing it and drop stale artifacts.
    if [[ "$ch_name" == "darkirc" ]] && ! tenant_darkirc_enabled "$name"; then
      rm -f "$dest_wasm" "$caps_dest"
      skipped=$((skipped + 1))
      say "  skipping channel: darkirc (tenant darkirc disabled)"
      continue
    fi

    if [[ "$has_wasm_tools" == "true" ]]; then
      "$wasm_tools" component new "$src_wasm" -o "$dest_wasm" 2>/dev/null \
        || cp "$src_wasm" "$dest_wasm"
      "$wasm_tools" strip "$dest_wasm" -o "$dest_wasm" 2>/dev/null || true
    else
      cp "$src_wasm" "$dest_wasm"
    fi

    if [[ -f "$caps_src" ]]; then
      cp "$caps_src" "$caps_dest"
    fi
    say "  installed channel: $ch_name"
    installed=$((installed + 1))
  done

  say "installing WASM tools to $tools_dir..."
  for dir in "$repo/tools-src"/*/; do
    [[ -d "$dir" ]] || continue
    local t_name bin_name install_name src_wasm dest_wasm caps_src caps_dest
    t_name="$(basename "$dir")"
    bin_name="$(tool_binary_name "$t_name")"
    install_name="${t_name}-tool"
    src_wasm="$dir/target/wasm32-wasip2/release/${bin_name}.wasm"
    dest_wasm="$tools_dir/${install_name}.wasm"
    caps_dest="$tools_dir/${install_name}.capabilities.json"

    if [[ ! -f "$src_wasm" ]]; then
      skipped=$((skipped + 1))
      continue
    fi

    if [[ "$has_wasm_tools" == "true" ]]; then
      "$wasm_tools" component new "$src_wasm" -o "$dest_wasm" 2>/dev/null \
        || cp "$src_wasm" "$dest_wasm"
      "$wasm_tools" strip "$dest_wasm" -o "$dest_wasm" 2>/dev/null || true
    else
      cp "$src_wasm" "$dest_wasm"
    fi

    caps_src="$dir/${install_name}.capabilities.json"
    if [[ ! -f "$caps_src" ]]; then
      caps_src="$dir/${t_name}.capabilities.json"
    fi
    if [[ -f "$caps_src" ]]; then
      cp "$caps_src" "$caps_dest"
    fi
    say "  installed tool: $install_name"
    installed=$((installed + 1))
  done

  chown -R "$name:$name" "$channels_dir" "$tools_dir"
  # The tenant-UID writer requires both the channel directory and the copied
  # capabilities file to be tenant-owned before it can replace the file.
  # Source caps contain generic 9001/6681 ports, not this tenant's ports.
  patch_weechat_caps "$name"
  patch_ssh_tool_allowlist "$name"

  local gotify_config="$state_dir/workspace/config/gotify.json"
  if [[ -f "$gotify_config" ]] && [[ -f "$tools_dir/gotify-tool.capabilities.json" ]]; then
    local gotify_url
    gotify_url="$(jq -r '.url // empty' "$gotify_config" 2>/dev/null)"
    if [[ -n "$gotify_url" ]]; then
      configure_gotify_capabilities "$name" "$gotify_url"
    fi
  fi

  say "WASM install for $name: $installed installed, $skipped skipped (not built)"
}

install_wasm_all() {
  local names
  names="$(all_tenant_names)"

  if [[ -z "$names" ]]; then
    say "no tenants registered"
    return 0
  fi

  while IFS= read -r name; do
    say ""
    say "=== Installing WASM for tenant: $name ==="
    install_wasm_tenant "$name"
  done <<< "$names"
}

# Cargo builds can refresh tracked lockfiles in nested standalone crates. Restore
# them only when they are the sole tracked changes; otherwise preserve everything
# and fail. Untracked tenant runtime directories do not block checkout.
prepare_tenant_checkout() {
  local name="$1" lw_root="$2" target_ref="$3"
  local tgit=(sudo -u "$name" git -c safe.directory="$lw_root" -C "$lw_root")
  local dirty dirty_locks staged_changes other_unstaged checkout_error

  if ! dirty="$("${tgit[@]}" status --short --untracked-files=no)"; then
    die "failed to inspect tracked changes in $lw_root"
  fi
  if [[ -n "$dirty" ]]; then
    if ! dirty_locks="$("${tgit[@]}" diff --name-only -- ':(glob)**/Cargo.lock')" \
      || ! staged_changes="$("${tgit[@]}" diff --cached --name-only)" \
      || ! other_unstaged="$("${tgit[@]}" diff --name-only -- . ':(exclude,glob)**/Cargo.lock')"; then
      die "failed to classify tracked changes in $lw_root"
    fi

    if [[ -n "$dirty_locks" && -z "$staged_changes" && -z "$other_unstaged" ]]; then
      say "restoring build-regenerated Cargo.lock files before checkout:"
      while IFS= read -r lockfile; do
        say "  $lockfile"
      done <<<"$dirty_locks"
      "${tgit[@]}" restore --worktree -- ':(glob)**/Cargo.lock' \
        || die "failed to restore build-regenerated Cargo.lock files"
    fi

    if ! dirty="$("${tgit[@]}" status --short --untracked-files=no)"; then
      die "failed to inspect tracked changes in $lw_root"
    fi
  fi
  if [[ -n "$dirty" ]]; then
    say "tracked changes still present in tenant checkout:"
    while IFS= read -r changed; do
      say "  $changed"
    done <<<"$dirty"
    die "checkout $lw_root has tracked changes; commit or stash them, then re-run (tenant was not stopped)"
  fi

  if ! checkout_error="$("${tgit[@]}" read-tree --dry-run -m -u HEAD "$target_ref" 2>&1)"; then
    say "target checkout preflight failed:"
    while IFS= read -r detail; do
      say "  $detail"
    done <<<"$checkout_error"
    die "checkout $target_ref would not be safe (tenant was not stopped)"
  fi
}

recover_failed_tenant_checkout() {
  local name="$1" lw_root="$2" target="$3" previous_commit="$4"
  local tgit=(sudo -u "$name" git -c safe.directory="$lw_root" -C "$lw_root")
  local current_commit dirty

  if current_commit="$("${tgit[@]}" rev-parse --verify HEAD 2>/dev/null)" \
    && dirty="$("${tgit[@]}" status --short --untracked-files=no 2>/dev/null)" \
    && [[ "$current_commit" == "$previous_commit" && -z "$dirty" ]]; then
    # TD-9: this runs before the stop (WI#527 candidate build), so the tenant
    # is still running — don't try to restart it.
    die "git checkout $target failed; tracked tree is unchanged, tenant '$name' is still running its previous build"
  fi

  die "git checkout $target failed after repository state changed; tenant '$name' is still running — stop it and check the checkout manually: $0 stop-tenant $name"
}
