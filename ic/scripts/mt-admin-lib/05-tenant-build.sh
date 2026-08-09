#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 05-tenant-build.sh — part of the lunarwing-mt-admin library.
#
# Tenant provisioning: rootless podman prerequisites, user creation/removal,
# repo cloning, build pipeline (tenant + DarkIRC daemon + WASM extensions),
# worker builds (pebble, opencode, kimi), and WASM installation.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced, so this file only defines functions.
#
# Extracted from master (v2.0.6.0 era) as Phase 2 of the mt-admin refactor.
# Architecture follows docs/proposals/MT-ADMIN-DECOMPOSITION.md and the prior
# attempt on origin/mtadminrefactor/meta4/v2.0.3.0, updated for new growth:
# DarkIRC release management/validation, kimi worker build, helper source
# revision attestation, and compatibility version tracking.
#
# Pure line-range move from the monolith: no function body was rewritten.

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
  # when the prior tenant is removed (see remove_tenant_user). A stale libpod
  # pause.pid then makes EVERY podman call — including the `system migrate` just
  # below, and the first pg container start in add-tenant — fail with
  # "cannot re-exec process to join the existing user namespace". Clear it so podman
  # spawns a fresh pause process — but KEEP it when it points to a live process
  # actually OWNED BY THIS tenant (its own running pause process on the "user
  # already exists" resume path, where ensure_rootless_prereqs re-runs from
  # create_tenant_user against a live tenant). Everything else is stale: an
  # empty/corrupt file, a dead pid, OR a pid since REUSED by another user's process
  # (host-ns owner != tenant uid) — a bare `kill -0` (we run as root) would read
  # that reused pid as "alive" and wrongly keep the stale file, leaving the O1 fault
  # unfixed. The tenant's pause process (catatonit) runs as the tenant uid in the
  # host ns, so /proc/<pid> ownership distinguishes it reliably.
  local pause_pid pause_owner
  pause_pid="/run/user/$uid/libpod/tmp/pause.pid"
  if [[ -f "$pause_pid" ]]; then
    pause_owner="$(cat "$pause_pid" 2>/dev/null || true)"
    if [[ -z "$pause_owner" ]] \
       || ! kill -0 "$pause_owner" 2>/dev/null \
       || [[ "$(stat -c %u "/proc/$pause_owner" 2>/dev/null || echo -1)" != "$uid" ]]; then
      rm -f "$pause_pid" 2>/dev/null || true
    fi
  fi

  # One-time rootless storage init (safe to re-run after subid changes).
  sudo -u "$name" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
    "$CONTAINER_RT" system migrate >/dev/null 2>&1 || true
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

  # Install rustup for tenant user if not already present
  if ! sudo -u "$name" bash -c "${cargo_src} command -v rustup" &>/dev/null; then
    say "installing rustup for $name ..."
    sudo -u "$name" bash -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y' \
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
      say "added $name to $group_name group"
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
  chmod 0700 "$lw_root/env"
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
  if [[ -d "$dest" ]] && [[ -n "$(ls -A "$dest")" ]]; then
    # Directory already has content (env/, state/, etc.) — init in place
    git "${safedir[@]}" init "$dest" >/dev/null
    git "${safedir[@]}" -C "$dest" remote add origin "$SOURCE_REPO"
    git "${safedir[@]}" -C "$dest" fetch origin --quiet
    local branch
    branch="$(git "${safedir[@]}" -C "$SOURCE_REPO" symbolic-ref --short HEAD 2>/dev/null || echo staging)"
    git "${safedir[@]}" -C "$dest" checkout -b "$branch" "origin/$branch" 2>&1 | tail -1
  else
    git "${safedir[@]}" clone --single-branch "$SOURCE_REPO" "$dest" 2>&1 | tail -1
  fi
  chown -R "$name:$name" "$dest"
  say "repo cloned for $name"
}

# ── Build management ─────────────────────────────────────────────────────────

# Run one tenant build step with resource bounding (BUG-005). On systemd the
# step runs as a transient scope in BUILD_SLICE via the tenant's own user
# manager, so cargo/rustc memory is capped and accounted outside both the
# operator's session and the daemon service. Falls back to a plain nice'd
# invocation when no user manager is reachable (OpenRC, or not yet started).
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
  else
    sudo -u "$name" nice -n 10 bash -c "$build_cmd"
  fi
}

build_tenant() {
  local name="$1"
  local with_wasm="${2:-false}"
  local with_pebble="${3:-false}"
  local with_opencode="${4:-false}"
  local with_kimi="${5:-false}"
  local with_toolchains="${6:-false}"
  local require_wasm="${7:-false}"
  local repo
  repo="$(tenant_repo "$name")"

  if [[ "$require_wasm" == "true" && "$with_wasm" != "true" ]]; then
    die "--require-wasm requires --with-wasm"
  fi

  [[ -d "$repo" ]] || die "repo not found at $repo; run add-tenant first"

  say "acquiring build lock (only one tenant builds at a time) ..."
  (
    flock -x 200

    local cargo_env="if [ -f \"\$HOME/.cargo/env\" ]; then . \"\$HOME/.cargo/env\"; else export PATH=\"\$HOME/.cargo/bin:\$PATH\"; fi;"

    say "building lunarwing for $name ..."
    _run_tenant_build "$name" "$cargo_env cd '$repo' && cargo build --profile $PROFILE --bin lunarwing -j$BUILD_JOBS" \
      || die "lunarwing build failed for $name"

    say "building xmpp-bridge for $name ..."
    _run_tenant_build "$name" "$cargo_env cd '$repo/bridges/xmpp-bridge' && cargo build --profile $PROFILE -j$BUILD_JOBS" \
      || die "xmpp-bridge build failed for $name"

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
    build_pebble_worker "false" "$with_toolchains"
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
  [[ "$mode" == 555 && "$owner" == 0 && "$links" == 2 ]] || return 1
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

shared_worker_source_root() {  # <names> <with-opencode> <with-kimi>
  local names="$1" with_opencode="$2" with_kimi="$3"
  local name root commit dirty expected_commit="" selected_root=""
  local -a worker_paths=()
  [[ "$with_opencode" != "true" ]] || worker_paths+=(opencode4lunarwing)
  [[ "$with_kimi" != "true" ]] || worker_paths+=(kimicode4lunarwing)
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
  local names
  names="$(all_tenant_names)"

  if [[ -z "$names" ]]; then
    say "no tenants registered"
    return 0
  fi

  local worker_source_root=""
  if [[ "$with_opencode" == "true" || "$with_kimi" == "true" ]]; then
    worker_source_root="$(shared_worker_source_root "$names" "$with_opencode" "$with_kimi")"
  fi

  if [[ "$with_pebble" == "true" ]]; then
    say ""
    say "=== Building pebble worker image ==="
    build_pebble_worker "false" "$with_toolchains"
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

  while IFS= read -r name; do
    say ""
    say "=== Building tenant: $name ==="
    build_tenant "$name" "$with_wasm" "false" "false" "false" "$with_toolchains" "false"
  done <<< "$names"
}

build_pebble_worker() {
  local no_cache="${1:-false}"
  local with_toolchains="${2:-false}"
  local pebble_dir="${LUNARWING_ROOT}/pebble4lunarwing"

  [[ -d "$pebble_dir" ]] || die "pebble worker dir not found at $pebble_dir"

  ensure_container_runtime

  local toolchain_desc="slim (no toolchains)"
  [[ "$with_toolchains" == "true" ]] && toolchain_desc="fat (with Rust/Go/C++ toolchains)"
  say "building pebble worker Docker image [$toolchain_desc] ..."
  local cache_flag=""
  [[ "$no_cache" == "true" ]] && cache_flag="--no-cache"

  local -a toolchain_args=()
  [[ "$with_toolchains" == "true" ]] && toolchain_args=(--build-arg WITH_TOOLCHAINS=true)

  if [[ "$CONTAINER_RT" == "podman" ]]; then
    # --network=host: podman's default build network
    # can't reach the internet for RUN steps on this host; the host netns has IPv4.
    # --format docker (O4): preserve the Dockerfile HEALTHCHECK (podman OCI drops it).
    podman build $cache_flag "${toolchain_args[@]}" --network=host --format docker -t lunarwing-worker-pebble:latest -f "$pebble_dir/Dockerfile" "$LUNARWING_ROOT" \
      || die "pebble worker image build failed"
  else
    docker build $cache_flag "${toolchain_args[@]}" -t lunarwing-worker-pebble:latest -f "$pebble_dir/Dockerfile" "$LUNARWING_ROOT" \
      || die "pebble worker image build failed"
  fi

  say "pebble worker image built: lunarwing-worker-pebble:latest [$toolchain_desc]"
}

build_opencode_worker() {
  local no_cache="${1:-false}"
  local with_toolchains="${2:-false}"
  local source_root="${3:-$LUNARWING_ROOT}"
  local opencode_dir="${source_root}/opencode4lunarwing"

  [[ -d "$opencode_dir" ]] || die "opencode worker dir not found at $opencode_dir"

  ensure_container_runtime

  local toolchain_desc="slim (no toolchains)"
  [[ "$with_toolchains" == "true" ]] && toolchain_desc="fat (with Rust/Go/C++ toolchains)"
  say "building opencode worker Docker image [$toolchain_desc] ..."

  local -a cache_args=()
  [[ "$no_cache" == "true" ]] && cache_args=(--no-cache)

  local -a toolchain_args=()
  [[ "$with_toolchains" == "true" ]] && toolchain_args=(--build-arg WITH_TOOLCHAINS=true)

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
    podman build "${cache_args[@]}" "${layer_args[@]}" "${toolchain_args[@]}" --network=host --format docker -t lunarwing-worker-opencode:latest "$opencode_dir" \
      || die "opencode worker image build failed"
  else
    docker build "${cache_args[@]}" "${toolchain_args[@]}" -t lunarwing-worker-opencode:latest "$opencode_dir" \
      || die "opencode worker image build failed"
  fi

  say "opencode worker image built: lunarwing-worker-opencode:latest [$toolchain_desc]"
}

build_kimi_worker() {
  local no_cache="${1:-false}"
  local with_toolchains="${2:-false}"
  local kimi_code_version="${3:-}"
  local source_root="${4:-$LUNARWING_ROOT}"
  local kimi_dir="${source_root}/kimicode4lunarwing"

  [[ -d "$kimi_dir" ]] || die "kimi worker dir not found at $kimi_dir"

  ensure_container_runtime

  local toolchain_desc="slim (no toolchains)"
  [[ "$with_toolchains" == "true" ]] && toolchain_desc="fat (with Rust/Go/C++ toolchains)"
  local version_desc="Dockerfile default"
  [[ -z "$kimi_code_version" ]] || version_desc="$kimi_code_version"
  say "building kimi worker Docker image [$toolchain_desc] (kimi-code version: ${version_desc}) ..."

  local -a cache_args=()
  [[ "$no_cache" == "true" ]] && cache_args=(--no-cache)

  local -a toolchain_args=()
  [[ "$with_toolchains" == "true" ]] && toolchain_args=(--build-arg WITH_TOOLCHAINS=true)

  local -a version_args=()
  [[ -z "$kimi_code_version" ]] || version_args=(--build-arg KIMI_CODE_VERSION="${kimi_code_version}")

  if [[ "$CONTAINER_RT" == "podman" ]]; then
    local -a layer_args=()
    if [[ "$no_cache" == "true" ]]; then
      say "pruning dangling Podman images before uncached Kimi build ..."
      if ! podman image prune -f >/dev/null; then
        say "WARNING: failed to prune dangling Podman images; continuing build" >&2
      fi
      layer_args=(--layers=false)
    fi
    podman build "${cache_args[@]}" "${layer_args[@]}" "${toolchain_args[@]}" "${version_args[@]}" --network=host --format docker -t lunarwing-worker-kimi:latest "$kimi_dir" \
      || die "kimi worker image build failed"
  else
    docker build "${cache_args[@]}" "${toolchain_args[@]}" "${version_args[@]}" -t lunarwing-worker-kimi:latest "$kimi_dir" \
      || die "kimi worker image build failed"
  fi

  say "kimi worker image built: lunarwing-worker-kimi:latest [$toolchain_desc]"
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

  # Patch WeeChat channel caps with per-tenant ports from the registry.
  # The source caps file has generic default ports (9001/6681) that are wrong
  # for every multi-tenant host. This runs after every install-wasm so the
  # tenant's allocated ports are always correct, even after a fresh copy.
  patch_weechat_caps "$name"

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
