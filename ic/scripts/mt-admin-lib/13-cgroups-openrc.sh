#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 13-cgroups-openrc.sh — part of the lunarwing-mt-admin library.
#
# OpenRC cgroup v2 workload bounding: per-tenant subtrees, boot-time local.d
# rendering, pam_exec login hook, and the setup-cgroups host command.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced.
#
# Extracted from the v2.0.8.0 monolith (goals item #9), keeping the library
# structure of ref2/sunburst/mtadmin-refactor-v2.0.6.0 per
# docs/proposals/MT-ADMIN-DECOMPOSITION.md. Pure line-range move: no function
# body was rewritten.


# ── Build management ─────────────────────────────────────────────────────────

# Convert a systemd-style CPUQuota percentage (e.g. "800%") into the cgroup v2
# cpu.max "quota period" form OpenRC expects in rc_cgroup_settings. Returns 1
# for anything outside the simple "<n>%" form so callers can omit the knob.
_openrc_cpu_max_from_quota() {
  local quota="$1" pct
  pct="${quota%\%}"
  [[ "$pct" == "$quota" || -z "$pct" ]] && return 1
  [[ "$pct" =~ ^[0-9]+$ ]] || return 1
  printf '%d %d' "$((pct * 1000))" 100000
}

# Convert the systemd-style memory sizes accepted by mt-admin into bytes for
# direct cgroup v2 writes. Bash arithmetic is signed 64-bit, so reject inputs
# that cannot be checked without overflow.
_cgroup_memory_bytes() {
  local raw="$1" number suffix multiplier bytes
  [[ "$raw" =~ ^([0-9]+)([KkMmGgTt]?)$ ]] || return 1
  number="${BASH_REMATCH[1]}"
  suffix="${BASH_REMATCH[2]^^}"
  while [[ "$number" == 0* && ${#number} -gt 1 ]]; do
    number="${number#0}"
  done
  [[ ${#number} -le 18 ]] || return 1
  number=$((10#$number))
  (( number > 0 )) || return 1
  case "$suffix" in
    "") multiplier=1 ;;
    K) multiplier=1024 ;;
    M) multiplier=$((1024 * 1024)) ;;
    G) multiplier=$((1024 * 1024 * 1024)) ;;
    T) multiplier=$((1024 * 1024 * 1024 * 1024)) ;;
    *) return 1 ;;
  esac
  (( number <= 9223372036854775807 / multiplier )) || return 1
  bytes=$((number * multiplier))
  printf '%d\n' "$bytes"
}

# Kept behind a function so fixture tests can supply an inert hierarchy without
# making a root-run production command honor an environment-controlled path.
_openrc_cgroup_root() {
  printf '/sys/fs/cgroup\n'
}

# Portable cgroup v2 availability probe for the operator build path (work item
# #54). A writable root is insufficient: memory must be available and either
# delegated already or writable in cgroup.subtree_control. setpriv (util-linux)
# is required for the root-side migration spawn used to place the build.
# Every check reports its own failure so "bounding unavailable" is never a
# mystery: the reason lands on stderr right before the nice-only fallback.
_openrc_build_cgroup_supported() {
  local root
  root="$(_openrc_cgroup_root)"
  command -v setpriv >/dev/null 2>&1 || {
    say "cgroup build bounding unavailable: setpriv not found (install sys-apps/util-linux)" >&2
    return 1
  }
  if ! [[ -d "$root" && -w "$root" ]]; then
    say "cgroup build bounding unavailable: $root is not a writable directory" >&2
    return 1
  fi
  if [[ "$(stat -fc %T "$root" 2>/dev/null)" == cgroup2fs ]]; then
    :
  elif [[ "$(stat -fc %T "$root" 2>/dev/null)" == tmpfs && -d "$root/unified" ]] \
    && [[ "$(stat -fc %T "$root/unified" 2>/dev/null)" == cgroup2fs ]]; then
    say "cgroup build bounding unavailable: hybrid cgroup mode detected" >&2
    say "  (v1 controllers at $root, v2 only at $root/unified — controllers such" >&2
    say "   as memory are bound to v1 there and cannot be delegated for bounding)." >&2
    say "  fix: set rc_cgroup_mode=\"unified\" in /etc/rc.conf and reboot" >&2
    return 1
  else
    say "cgroup build bounding unavailable: $root is not a cgroup v2 hierarchy" >&2
    return 1
  fi
  [[ -r "$root/cgroup.controllers" ]] \
    && grep -qw memory "$root/cgroup.controllers" || {
      say "cgroup build bounding unavailable: memory controller missing at $root (kernel boot arg?)" >&2
      return 1
    }
  [[ -r "$root/cgroup.subtree_control" ]] || {
    say "cgroup build bounding unavailable: $root/cgroup.subtree_control unreadable" >&2
    return 1
  }
  grep -qw memory "$root/cgroup.subtree_control" \
    || [[ -w "$root/cgroup.subtree_control" ]] || {
      say "cgroup build bounding unavailable: memory not enabled in $root/cgroup.subtree_control and not writable" >&2
      return 1
    }
  return 0
}

# Resolve a tenant's home directory for the build spawn environment. The sudo
# spawn this replaced got HOME from sudo's env_reset; setpriv changes no
# environment, so the caller must supply it explicitly.
_tenant_home_dir() {
  local home
  home="$(getent passwd "$1" 2>/dev/null | cut -d: -f6)"
  [[ -n "$home" && -d "$home" ]] || return 1
  printf '%s\n' "$home"
}

_openrc_build_memory_controller_ready() {
  local root="$1"
  [[ -r "$root/cgroup.controllers" ]] || return 1
  grep -qw memory "$root/cgroup.controllers" || return 1
  [[ -r "$root/cgroup.subtree_control" ]] || return 1
  if ! grep -qw memory "$root/cgroup.subtree_control"; then
    [[ -w "$root/cgroup.subtree_control" ]] || return 1
    printf '+memory' >"$root/cgroup.subtree_control" 2>/dev/null || return 1
  fi
  grep -qw memory "$root/cgroup.subtree_control"
}

_openrc_build_cgroup_set_memory_limits() {
  local cg="$1" high max knob expected path actual
  high="$(_cgroup_memory_bytes "$BUILD_MEMORY_HIGH")" || {
    say "invalid build memory.high value: $BUILD_MEMORY_HIGH" >&2
    return 1
  }
  max="$(_cgroup_memory_bytes "$BUILD_MEMORY_MAX")" || {
    say "invalid build memory.max value: $BUILD_MEMORY_MAX" >&2
    return 1
  }
  if (( high > max )); then
    say "build memory.high exceeds memory.max" >&2
    return 1
  fi
  for knob in memory.high memory.max; do
    case "$knob" in
      memory.high) expected="$high" ;;
      memory.max) expected="$max" ;;
    esac
    path="$cg/$knob"
    [[ -f "$path" && ! -L "$path" && -w "$path" ]] || {
      say "build cgroup missing writable $knob" >&2
      return 1
    }
    printf '%s' "$expected" >"$path" 2>/dev/null || {
      say "failed to set build cgroup $knob" >&2
      return 1
    }
    actual="$(<"$path")"
    [[ "$actual" =~ ^[0-9]+$ && "$actual" == "$expected" ]] || {
      say "build cgroup $knob readback mismatch" >&2
      return 1
    }
  done
}

_openrc_cgroup_kill_all() {
  printf '1' >"$1/cgroup.kill" 2>/dev/null
}

# Reap whatever still lives in a build cgroup and remove the directory.
# Best-effort: a still-populated cgroup survives rmdir and is retried by the
# next build's stale-cgroup cleanup.
_openrc_build_cgroup_cleanup() {
  local cg="$1" pid
  [[ -d "$cg" ]] || return 0
  if [[ -w "$cg/cgroup.kill" ]] && _openrc_cgroup_kill_all "$cg"; then
    # kernel 5.14+: SIGKILL every member atomically
    :
  elif [[ -r "$cg/cgroup.procs" ]]; then
    while read -r pid; do
      kill -KILL "$pid" 2>/dev/null || true
    done <"$cg/cgroup.procs"
  fi
  local tries=0
  while [[ -d "$cg" ]] && (( tries < 10 )); do
    rmdir "$cg" 2>/dev/null && break
    sleep 0.2
    tries=$((tries + 1))
  done
  [[ ! -d "$cg" ]]
}

# ── Pre-delegated tenant cgroup subtrees (OpenRC) ───────────────────────────
# Implements docs/ops/PREDELEGATED_SUBTREE_CGROUPS.md: a tenant-owned
# /sys/fs/cgroup/lunarwing-<name>/ hierarchy with a session/ child for login
# shells (placed by a pam_exec hook) and a build/ child created per build by
# _run_tenant_build_cgroup. cgroupfs is not persistent across reboot, so a
# rendered /etc/local.d/lunarwing-cgroups.start recreates the layout at boot.
# Kept behind path functions so fixture tests can redirect every host path.

_openrc_cgroup_login_hook_path() {
  printf '/usr/local/sbin/lunarwing-cgroup-login\n'
}

_openrc_cgroups_locald_path() {
  printf '/etc/local.d/lunarwing-cgroups.start\n'
}

# PAM files whose session stack places user login shells. system-login covers
# sshd/login/su on hosts where pambase provides it; the leaf files cover hosts
# without it. Never wire sudo or a shared include like system-auth: a pam_exec
# hook there would migrate the operator's own `sudo -u <tenant>` processes
# into the tenant cgroup — the wrong direction entirely.
_openrc_cgroup_pam_targets() {
  local f
  for f in system-login sshd su login; do
    [[ -f "/etc/pam.d/$f" ]] && printf '/etc/pam.d/%s\n' "$f"
  done
}

# Install the pam_exec helper that moves a login session's process into the
# tenant's session cgroup. Idempotent: only rewrites when content differs.
_openrc_cgroup_login_hook_install() {
  local path tmp
  path="$(_openrc_cgroup_login_hook_path)"
  tmp="$(mktemp "${TMPDIR:-/tmp}/lunarwing-cgroup-login.XXXXXXXX")" || return 1
  cat >"$tmp" <<'EOF'
#!/bin/bash
# pam_exec helper installed by lunarwing-mt-admin.sh setup-cgroups.
# Moves the PAM application (PPID — sshd's per-connection process, login, or
# su) into the tenant's pre-delegated session cgroup so the whole login
# session inherits membership. Wired as `session optional`, so it must never
# fail authentication: every exit path is 0.
[[ "$PAM_TYPE" == open_session ]] || exit 0
# Tenant names are Linux account names; refuse anything that could escape the
# lunarwing- prefix (defense in depth — useradd already forbids these).
case "$PAM_USER" in
  ""|*[!a-z0-9_-]*) exit 0 ;;
esac
cg="/sys/fs/cgroup/lunarwing-$PAM_USER/session"
[[ -w "$cg/cgroup.procs" ]] || exit 0
echo "$PPID" >"$cg/cgroup.procs" 2>/dev/null || true
exit 0
EOF
  if [[ -f "$path" ]] && cmp -s "$tmp" "$path"; then
    rm -f "$tmp"
    return 0
  fi
  chmod 0755 "$tmp" || { rm -f "$tmp"; return 1; }
  mv -f "$tmp" "$path" || { rm -f "$tmp"; return 1; }
  chown root:root "$path" || return 1
  say "installed PAM cgroup login hook: $path"
}

# Wire the login hook into the session stack of every PAM target that exists.
# Idempotent line append; /etc/pam.d/sudo is deliberately never touched.
_openrc_cgroup_pam_wire() {
  local hook_path target line
  hook_path="$(_openrc_cgroup_login_hook_path)"
  line="session    optional    pam_exec.so $hook_path"
  local wired=0
  while IFS= read -r target; do
    [[ -n "$target" ]] || continue
    if grep -qF "pam_exec.so $hook_path" "$target"; then
      continue
    fi
    printf '%s\n' "$line" >>"$target" || return 1
    say "wired cgroup login hook into $target"
    wired=$((wired + 1))
  done < <(_openrc_cgroup_pam_targets)
  return 0
}

# Render the boot-time local.d script that recreates the controller delegation
# and every registered tenant's subtree (cgroupfs does not survive reboot).
# Regenerated by add-tenant/remove-tenant/setup-cgroups so the baked-in
# tenant list always matches the ports registry; getent guards staleness.
_openrc_cgroups_locald_render() {
  local path tenants tmp
  path="$(_openrc_cgroups_locald_path)"
  tenants="$(all_tenant_names | paste -sd' ' -)"
  tmp="$(mktemp "${TMPDIR:-/tmp}/lunarwing-cgroups.start.XXXXXXXX")" || return 1
  {
    printf '%s\n' '#!/bin/bash'
    cat <<'EOF'
# Rendered by lunarwing-mt-admin.sh (setup-cgroups / add-tenant /
# remove-tenant). Regenerate with `lunarwing-mt-admin.sh setup-cgroups`.
# Recreates the pre-delegated tenant cgroup v2 subtrees at boot — cgroupfs is
# not persistent across reboot. See docs/ops/PREDELEGATED_SUBTREE_CGROUPS.md.
CG=/sys/fs/cgroup
[ -r "$CG/cgroup.controllers" ] || exit 0
grep -qw memory "$CG/cgroup.controllers" || exit 0
# The root cgroup is exempt from the no-internal-process rule, so controllers
# can be enabled here even with processes in the root.
for ctl in memory pids; do
  grep -qw "$ctl" "$CG/cgroup.controllers" || continue
  grep -qw "$ctl" "$CG/cgroup.subtree_control" && continue
  echo "+$ctl" >"$CG/cgroup.subtree_control" 2>/dev/null || true
done
grep -qw memory "$CG/cgroup.subtree_control" || exit 0
EOF
    printf 'TENANTS="%s"\n' "$tenants"
    cat <<'EOF'
for t in $TENANTS; do
  getent passwd "$t" >/dev/null || continue
  uid="$(id -u "$t")"; gid="$(id -g "$t")"
  d="$CG/lunarwing-$t"
  mkdir -p "$d/session"
  # Controllers enabled on lunarwing-<t> mean that dir itself must hold no
  # processes — login shells live in the session/ child.
  for ctl in memory pids; do
    echo "+$ctl" >"$d/cgroup.subtree_control" 2>/dev/null || true
  done
  chown "$uid:$gid" "$d" "$d/session" \
    "$d/cgroup.procs" "$d/cgroup.subtree_control" \
    "$d/session/cgroup.procs" 2>/dev/null || true
done
exit 0
EOF
  } >"$tmp" || { rm -f "$tmp"; return 1; }
  chmod 0755 "$tmp" || { rm -f "$tmp"; return 1; }
  mv -f "$tmp" "$path" || { rm -f "$tmp"; return 1; }
  chown root:root "$path" || return 1
  say "rendered boot-time cgroup setup: $path (tenants: ${tenants:-none})"
}

# Create one tenant's pre-delegated subtree on the running system. Idempotent.
_openrc_tenant_subtree_ensure() {
  local name="$1" root d uid gid ctl
  root="$(_openrc_cgroup_root)"
  d="$root/lunarwing-$name"
  [[ -r "$root/cgroup.controllers" ]] || return 1
  grep -qw memory "$root/cgroup.controllers" || return 1
  uid="$(id -u "$name" 2>/dev/null)" || return 1
  gid="$(id -g "$name" 2>/dev/null)" || return 1
  # Enable the available controllers in one write: cgroupfs applies each
  # "+ctl" additively, but a multi-write sequence leaves partial state behind
  # when a later write fails (and reads back wrong on non-cgroupfs fixtures).
  local enable=""
  for ctl in memory pids; do
    grep -qw "$ctl" "$root/cgroup.controllers" || continue
    grep -qw "$ctl" "$root/cgroup.subtree_control" 2>/dev/null && continue
    enable="$enable +$ctl"
  done
  if [[ -n "$enable" ]]; then
    # pids is nice-to-have for session containment; memory is required.
    printf '%s' "${enable# }" >"$root/cgroup.subtree_control" 2>/dev/null \
      || printf '+memory' >"$root/cgroup.subtree_control" 2>/dev/null \
      || return 1
  fi
  grep -qw memory "$root/cgroup.subtree_control" || return 1
  mkdir -p "$d/session" || return 1
  enable=""
  for ctl in memory pids; do
    grep -qw "$ctl" "$root/cgroup.controllers" || continue
    enable="$enable +$ctl"
  done
  # Same single-write form for the tenant dir; best-effort beyond memory.
  printf '%s' "${enable# }" >"$d/cgroup.subtree_control" 2>/dev/null \
    || printf '+memory' >"$d/cgroup.subtree_control" 2>/dev/null \
    || true
  grep -qw memory "$d/cgroup.subtree_control" || {
    say "cannot delegate memory into $d (processes in the tenant cgroup root?)" >&2
    return 1
  }
  chown "$uid:$gid" "$d" "$d/session" \
    "$d/cgroup.procs" "$d/cgroup.subtree_control" \
    "$d/session/cgroup.procs"
}

# Remove one tenant's subtree, reaping anything still inside (a tenant being
# removed has its services stopped already; leftover login shells of the
# removed account go with it). Best-effort like the build-cgroup cleanup.
_openrc_tenant_subtree_remove() {
  local name="$1" root d child
  root="$(_openrc_cgroup_root)"
  d="$root/lunarwing-$name"
  [[ -d "$d" ]] || return 0
  for child in "$d"/*/; do
    [[ -d "$child" ]] || continue
    _openrc_build_cgroup_cleanup "${child%/}" || true
  done
  printf '%s' '-memory -pids' >"$d/cgroup.subtree_control" 2>/dev/null || true
  rmdir "$d" 2>/dev/null || {
    say "WARNING: could not remove cgroup subtree $d" >&2
    return 1
  }
}

# Full per-tenant setup used by add-tenant: login hook, PAM wiring, live
# subtree, and the boot-time local.d render.
setup_tenant_cgroups() {
  local name="$1"
  _openrc_cgroup_login_hook_install || return 1
  _openrc_cgroup_pam_wire || return 1
  _openrc_tenant_subtree_ensure "$name" || return 1
  _openrc_cgroups_locald_render || return 1
}

# Fleet-wide `setup-cgroups` command: installs the hook/PAM/local.d assets and
# ensures every registered tenant's subtree on the running system. This is
# also the upgrade path for tenants provisioned before subtree support.
setup_cgroups() {
  ensure_init_system
  if [[ "$INIT_SYSTEM" == systemd* ]]; then
    die "setup-cgroups is only for OpenRC hosts (systemd uses per-tenant user-manager slices)"
  fi
  local root
  root="$(_openrc_cgroup_root)"
  [[ "$(stat -fc %T "$root" 2>/dev/null)" == cgroup2fs ]] \
    || die "cgroup v2 hierarchy not mounted at $root (hybrid mode? set rc_cgroup_mode=\"unified\" in /etc/rc.conf and reboot)"
  [[ -r "$root/cgroup.controllers" ]] \
    && grep -qw memory "$root/cgroup.controllers" \
    || die "memory controller not available at $root"
  _openrc_cgroup_login_hook_install || die "failed to install the PAM cgroup login hook"
  _openrc_cgroup_pam_wire || die "failed to wire the PAM cgroup login hook"
  _openrc_cgroups_locald_render || die "failed to render the boot-time cgroup setup"
  if rc-update show default 2>/dev/null | grep -qw local; then
    :
  else
    say "WARNING: the 'local' service is not in the default runlevel; boot-time cgroup setup will not run (fix: rc-update add local default)" >&2
  fi
  local name failures=0 count=0
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    count=$((count + 1))
    if _openrc_tenant_subtree_ensure "$name"; then
      say "subtree ready: $root/lunarwing-$name/{session,build}"
    else
      say "WARNING: cgroup subtree setup failed for tenant '$name'" >&2
      failures=$((failures + 1))
    fi
  done < <(all_tenant_names)
  (( count > 0 )) || say "no tenants registered; host assets installed only"
  (( failures == 0 )) || die "$failures tenant subtree(s) failed to set up"
  say "pre-delegated cgroup setup complete for $count tenant(s)"
}
