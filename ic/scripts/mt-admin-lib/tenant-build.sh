#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# tenant-build.sh — part of the lunarwing-mt-admin library.
#
# Tenant user lifecycle (create/remove, linger, rootless prereqs, WASM
# toolchain), repo cloning, cargo builds, worker image builds, and WASM
# extension installation.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced, so this file only defines functions.

# ── User management ──────────────────────────────────────────────────────────

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

build_tenant() {
  local name="$1"
  local with_wasm="${2:-false}"
  local with_nanocode="${3:-false}"
  local with_pebble="${4:-false}"
  local with_opencode="${5:-false}"
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
    sudo -u "$name" bash -c "$cargo_env cd '$repo' && cargo build --profile $PROFILE --bin lunarwing" \
      || die "lunarwing build failed for $name"

    say "building xmpp-bridge for $name ..."
    sudo -u "$name" bash -c "$cargo_env cd '$repo/bridges/xmpp-bridge' && cargo build --profile $PROFILE" \
      || die "xmpp-bridge build failed for $name"

    if [[ "$with_wasm" == "true" ]]; then
      if ensure_tenant_wasm_toolchain "$name"; then
        say "building WASM extensions for $name ..."
        local wasm_build_ok="true"
        if ! sudo -u "$name" bash -c "$cargo_env cd '$repo' && bash scripts/build-wasm-extensions.sh"; then
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

  if [[ "$with_nanocode" == "true" ]]; then
    say ""
    say "=== Building nanocode worker image ==="
    build_nanocode_worker "false" "$with_toolchains"
  fi

  if [[ "$with_pebble" == "true" ]]; then
    say ""
    say "=== Building pebble worker image ==="
    build_pebble_worker "false"
  fi

  if [[ "$with_opencode" == "true" ]]; then
    say ""
    say "=== Building opencode worker image ==="
    build_opencode_worker "false" "$with_toolchains"
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

build_darkirc() {
  local tenant_name="${1:-}" darkirc_src="" build_user=""

  if [[ -n "$tenant_name" ]]; then
    build_user="$tenant_name"
    local envf
    envf="$(tenant_env_dir "$tenant_name")/lunarwing.env"
    if [[ -f "$envf" ]]; then
      darkirc_src="$(grep -s '^DARKIRC_SOURCE=' "$envf" | cut -d= -f2-)" || true
    fi
    [[ -n "$darkirc_src" ]] || darkirc_src="$DARKIRC_SOURCE"
    [[ -n "$darkirc_src" ]] || darkirc_src="$(tenant_home "$tenant_name")/darkfi"
  else
    # Fleet-wide build: run as the invoking (sudo) user, never root. Root has no
    # per-user rust toolchain under rustup, and root-owned source/artifacts break
    # the tenant-run daemon.
    build_user="${SUDO_USER:-$(id -un)}"
    [[ -n "$darkirc_src" ]] || darkirc_src="$DARKIRC_SOURCE"
    [[ -n "$darkirc_src" ]] || die "darkirc source path not configured — set DARKIRC_SOURCE/LUNARWING_MT_DARKIRC_SOURCE or pass --tenant <name>"
  fi

  [[ "$build_user" != "root" ]] || die "refusing to build darkirc as root — pass --tenant <name>, or invoke via sudo from a user that has a rust toolchain"

  say "acquiring build lock for darkirc ..."
  (
    flock -x 200

    # Clone darkfi as the build user if absent, so the source tree and build
    # artifacts are owned by that user (no root-owned files) — "clone as the user".
    if [[ ! -d "$darkirc_src/.git" ]]; then
      say "darkirc source not present at $darkirc_src — cloning $DARKIRC_REPO ($DARKIRC_REV) as $build_user ..."
      sudo -u "$build_user" git clone "$DARKIRC_REPO" "$darkirc_src" || die "darkirc clone failed"
      if [[ -n "$DARKIRC_REV" && "$DARKIRC_REV" != "master" ]]; then
        sudo -u "$build_user" git -C "$darkirc_src" checkout "$DARKIRC_REV" || die "darkirc checkout '$DARKIRC_REV' failed"
      fi
    else
      say "darkirc source present at $darkirc_src ($(sudo -u "$build_user" git -C "$darkirc_src" rev-parse --short HEAD 2>/dev/null || echo unknown)) — using as-is"
    fi

    [[ -f "$darkirc_src/Makefile" ]] || die "darkirc Makefile not found (expected darkfi repo root)"
    [[ -f "$darkirc_src/bin/darkirc/Cargo.toml" ]] || die "darkirc Cargo.toml not found"

    local cargo_env="if [ -f \"\$HOME/.cargo/env\" ]; then . \"\$HOME/.cargo/env\"; else export PATH=\"\$HOME/.cargo/bin:\$PATH\"; fi;"

    # Build with darkfi's make as the build user (mirrors build_tenant): uses that
    # user's rust toolchain and leaves no root-owned artifacts.
    say "building darkirc from $darkirc_src via make (as $build_user) ..."
    sudo -u "$build_user" bash -c "$cargo_env cd '$darkirc_src' && make darkirc" \
      || die "darkirc build failed"

    local built_bin="$darkirc_src/darkirc"
    [[ -x "$built_bin" ]] || die "darkirc binary not found at $built_bin"

    # Install the shared daemon binary — the only root-privileged step.
    say "installing darkirc to $DARKIRC_BIN ..."
    install -m 0755 "$built_bin" "$DARKIRC_BIN"

    say "darkirc build complete: $DARKIRC_BIN"
    "$DARKIRC_BIN" --version || say "(darkirc --version not supported, skipping)"
  ) 200>"$BUILD_LOCK"
}

build_all() {
  local with_wasm="${1:-false}"
  local with_nanocode="${2:-false}"
  local with_pebble="${3:-false}"
  local with_opencode="${4:-false}"
  local with_toolchains="${5:-false}"
  local names
  names="$(all_tenant_names)"

  if [[ -z "$names" ]]; then
    say "no tenants registered"
    return 0
  fi

  if [[ "$with_nanocode" == "true" ]]; then
    say ""
    say "=== Building nanocode worker image ==="
    build_nanocode_worker "false" "$with_toolchains"
  fi

  if [[ "$with_pebble" == "true" ]]; then
    say ""
    say "=== Building pebble worker image ==="
    build_pebble_worker "false"
  fi

  if [[ "$with_opencode" == "true" ]]; then
    say ""
    say "=== Building opencode worker image ==="
    build_opencode_worker "false" "$with_toolchains"
  fi

  while IFS= read -r name; do
    say ""
    say "=== Building tenant: $name ==="
    build_tenant "$name" "$with_wasm" "false" "false"
  done <<< "$names"
}

# ── Nanocode worker Docker image build ────────────────────────────────────────

build_nanocode_worker() {
  local no_cache="${1:-false}"
  local with_toolchains="${2:-false}"
  local nanocode_ref="${3:-v1.2.28}"
  local nanocode_dir="${LUNARWING_ROOT}/lunarcode4lunarwing"

  [[ -d "$nanocode_dir" ]] || die "nanocode worker dir not found at $nanocode_dir"

  ensure_container_runtime

  say "building nanocode worker Docker image (nanocode ref: ${nanocode_ref}) ..."
  local cache_flag=""
  [[ "$no_cache" == "true" ]] && cache_flag="--no-cache"

  local toolchain_arg=""
  [[ "$with_toolchains" == "true" ]] && toolchain_arg="--build-arg WITH_TOOLCHAINS=true"

  if [[ "$CONTAINER_RT" == "podman" ]]; then
    # --network=host (F8): rootless/rootful podman's default build network can't
    # reach the internet for RUN steps (apt) on hosts where the bridge/pasta path
    # is broken or IPv6 is preferred-but-unrouted; the host netns has working IPv4.
    # --format docker (O4): podman defaults to OCI, which drops the Dockerfile
    # HEALTHCHECK ("not supported for OCI image format"); build docker-format so the
    # baked healthcheck survives (harmless for the OpenRC init-unit probe, correct
    # if the image is ever run directly / under a healthcheck-honouring runtime).
    podman build $cache_flag $toolchain_arg --network=host --format docker --build-arg NANOCODE_REF="${nanocode_ref}" -t lunarwing-worker-nanocode:latest "$nanocode_dir" \
      || die "nanocode worker image build failed"
  else
    docker build $cache_flag $toolchain_arg --build-arg NANOCODE_REF="${nanocode_ref}" -t lunarwing-worker-nanocode:latest "$nanocode_dir" \
      || die "nanocode worker image build failed"
  fi

  say "nanocode worker image built: lunarwing-worker-nanocode:latest"
}

build_pebble_worker() {
  local no_cache="${1:-false}"
  local pebble_dir="${LUNARWING_ROOT}/pebble4lunarwing"

  [[ -d "$pebble_dir" ]] || die "pebble worker dir not found at $pebble_dir"

  ensure_container_runtime

  say "building pebble worker Docker image ..."
  local cache_flag=""
  [[ "$no_cache" == "true" ]] && cache_flag="--no-cache"

  if [[ "$CONTAINER_RT" == "podman" ]]; then
    # --network=host (F8): see build_nanocode_worker — podman build's default network
    # can't reach the internet for RUN steps on this host; the host netns has IPv4.
    # --format docker (O4): preserve the Dockerfile HEALTHCHECK (podman OCI drops it).
    podman build $cache_flag --network=host --format docker -t lunarwing-worker-pebble:latest -f "$pebble_dir/Dockerfile" "$LUNARWING_ROOT" \
      || die "pebble worker image build failed"
  else
    docker build $cache_flag -t lunarwing-worker-pebble:latest -f "$pebble_dir/Dockerfile" "$LUNARWING_ROOT" \
      || die "pebble worker image build failed"
  fi

  say "pebble worker image built: lunarwing-worker-pebble:latest"
}

build_opencode_worker() {
  local no_cache="${1:-false}"
  local with_toolchains="${2:-false}"
  local opencode_dir="${LUNARWING_ROOT}/opencode4lunarwing"

  [[ -d "$opencode_dir" ]] || die "opencode worker dir not found at $opencode_dir"

  ensure_container_runtime

  local toolchain_desc="slim (no toolchains)"
  [[ "$with_toolchains" == "true" ]] && toolchain_desc="fat (with Rust/Go/C++ toolchains)"
  say "building opencode worker Docker image [$toolchain_desc] ..."

  local cache_flag=""
  [[ "$no_cache" == "true" ]] && cache_flag="--no-cache"

  local toolchain_arg=""
  [[ "$with_toolchains" == "true" ]] && toolchain_arg="--build-arg WITH_TOOLCHAINS=true"

  if [[ "$CONTAINER_RT" == "podman" ]]; then
    podman build $cache_flag $toolchain_arg --network=host --format docker -t lunarwing-worker-opencode:latest "$opencode_dir" \
      || die "opencode worker image build failed"
  else
    docker build $cache_flag $toolchain_arg -t lunarwing-worker-opencode:latest "$opencode_dir" \
      || die "opencode worker image build failed"
  fi

  say "opencode worker image built: lunarwing-worker-opencode:latest [$toolchain_desc]"
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

