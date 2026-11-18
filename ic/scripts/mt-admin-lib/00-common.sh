#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 00-common.sh — part of the lunarwing-mt-admin library.
#
# Shared helpers: diagnostics, registry/lifecycle locks, token + DarkIRC
# scope-id generation, tenant path primitives, container config-hash tracking,
# per-tenant feature-flag readers, PG password reader, template rendering, and
# the root guard.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced.
#
# Extracted from the v2.0.8.0 monolith (goals item #9), keeping the library
# structure of ref2/sunburst/mtadmin-refactor-v2.0.6.0 per
# docs/proposals/MT-ADMIN-DECOMPOSITION.md. Pure line-range move: no function
# body was rewritten.

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
warn_err() { printf 'error: %s\n' "$*" >&2; }

HEALTH_LIB_UNSAFE_COMPONENT=""

# The text of a refusal, without the "error: " prefix. <what> says which tree
# failed: the installed library (the default), or a contract source that is not
# the library. The repair differs, so the text must not offer the wrong one:
# an entry or the tree itself is replaced from a trusted checkout when it is the
# library and corrected where it lies when it is a checkout; a parent directory
# of the library is repaired in place, and no reinstall can do that; a parent
# of a contract source belongs to that checkout's owner, so no command is
# printed for it. `link` is the library path that is itself a symbolic link:
# the install replaces the link, so the reinstall is the repair whatever the
# link's target looks like. An entry name is chosen by whoever can write the
# tree, so it is printed quoted: a line break in it must not start a line.
health_lib_refusal_text() {  # <path> [tree|entry|link] [library|source]
  local path="$1" kind="${2:-tree}" what="${3:-library}"
  local component="${HEALTH_LIB_UNSAFE_COMPONENT:-$1}" name="health library"
  local reinstall="reinstall from a trusted checkout with: ic/scripts/lunarwing-mt-admin.sh reinstall-health-library <trusted-checkout>"
  local must="must be root-owned, symlink-free, and not group/other-writable"
  local elsewhere="run mt-admin from a clean root-owned checkout (reinstalling the health library does not change it)"
  [[ "$what" != source ]] || name="health contract source"
  if [[ "$kind" == link ]]; then
    printf 'refusing %s %s: it is a symbolic link to %s, which is not a safe root-owned tree; %s' \
      "$name" "$path" "$component" "$reinstall"
  elif [[ "$kind" == entry ]]; then
    printf 'refusing %s entry %q: symbolic link, non-root owner, group/other write, set-ID bits on a regular file, or a non-regular file type; ' \
      "$name" "$component"
    if [[ "$what" == source ]]; then
      printf 'correct or remove it in that checkout, or %s' "$elsewhere"
    else
      printf '%s' "$reinstall"
    fi
  elif [[ -e "$component" ]] && ! "${HEALTH_LIB_STAT:-stat}" -c '%u' "$component" >/dev/null 2>&1; then
    printf 'refusing %s %s: owner and mode of %s could not be read with %s' \
      "$name" "$path" "$component" "${HEALTH_LIB_STAT:-stat}"
  elif [[ "$component" != "$path" && -L "$component" ]]; then
    printf 'refusing %s %s: parent directory %s %s; it is a symbolic link: replace it with a real root-owned directory (chown and chmod would change only its target)' \
      "$name" "$path" "$component" "$must"
  elif [[ "$component" != "$path" && "$what" == source ]]; then
    printf 'refusing %s %s: parent directory %s %s; correct that directory if it is meant to be root-owned, or %s' \
      "$name" "$path" "$component" "$must" "$elsewhere"
  elif [[ "$component" != "$path" ]]; then
    printf 'refusing %s %s: parent directory %s %s; repair that parent (not the library tree) with: chown root:root -- %q && chmod go-w -- %q' \
      "$name" "$path" "$component" "$must" "$component" "$component"
  elif [[ "$what" == source ]]; then
    printf 'refusing %s %s: path component %s %s; correct its owner and mode, or run mt-admin from a clean root-owned checkout' \
      "$name" "$path" "$component" "$must"
  else
    printf 'refusing %s %s: path component %s %s; %s' \
      "$name" "$path" "$component" "$must" "$reinstall"
  fi
}

health_lib_validate_root_owned_directory() {
  local path="$1" rest component current="/" owner mode
  local stat_bin="${HEALTH_LIB_STAT:-stat}"
  HEALTH_LIB_UNSAFE_COMPONENT="$path"
  [[ "$path" == /* && -d "$path" ]] || return 1
  rest="${path#/}"
  while :; do
    HEALTH_LIB_UNSAFE_COMPONENT="$current"
    [[ -d "$current" && ! -L "$current" ]] || return 1
    owner="$("$stat_bin" -c '%u' "$current" 2>/dev/null || true)"
    mode="$("$stat_bin" -c '%a' "$current" 2>/dev/null || true)"
    [[ "$owner" == 0 && "$mode" =~ ^[0-7]+$ ]] || return 1
    (( (8#$mode & 0022) == 0 )) || return 1
    [[ -n "$rest" ]] || break
    component="${rest%%/*}"
    if [[ -z "$component" || "$component" == . || "$component" == .. ]]; then
      HEALTH_LIB_UNSAFE_COMPONENT="$path"
      return 1
    fi
    current="${current%/}/$component"
    if [[ "$rest" == */* ]]; then rest="${rest#*/}"; else rest=""; fi
  done
  HEALTH_LIB_UNSAFE_COMPONENT=""
  return 0
}

# A root-owned directory with no group/other write grants a set-group-ID bit
# nothing: the bit only changes the group of new entries, and nobody else can
# create any. It is inherited from a set-group-ID parent (chmod 0755 does not
# clear it), so refusing it on directories would stop an install, a snapshot
# load and every command of a checkout that live under such a parent. Set-ID
# bits are therefore refused on regular files only, where they do change what
# the file does when it runs.
health_lib_tree_require_safe() {  # <path> [library|source]
  local path="${1:-}" what="${2:-library}" unsafe name="health library"
  local find_bin="${HEALTH_LIB_FIND:-find}"
  [[ "$what" != source ]] || name="health contract source"
  HEALTH_LIB_UNSAFE_COMPONENT="$path"
  if [[ "$path" == / ]] || ! health_lib_validate_root_owned_directory "$path"; then
    printf 'error: %s\n' "$(health_lib_refusal_text "$path" tree "$what")" >&2
    return 1
  fi
  unsafe="$("$find_bin" "$path" \( -type l -o ! -uid 0 -o -perm /022 -o \( -type f -a -perm /6000 \) -o ! \( -type f -o -type d \) \) -print -quit 2>/dev/null)" \
    || { printf 'error: refusing %s %s: ownership, file types, and permissions could not be inspected\n' "$name" "$path" >&2; return 1; }
  if [[ -n "$unsafe" ]]; then
    HEALTH_LIB_UNSAFE_COMPONENT="$unsafe"
    printf 'error: %s\n' "$(health_lib_refusal_text "$path" entry "$what")" >&2
    return 1
  fi
  return 0
}

# WI#384 display hygiene: tenant-controlled values echoed into root-side
# terminal output and logs must never carry control bytes or terminal escapes.
# %-encode every non-printable byte (LC_ALL=C) and cap the echo length so a
# hostile value cannot forge terminal output or flood the log. Printable shell
# metacharacters ($ & ; etc.) are DATA here, not an injection: die prints them
# with %s and nothing downstream evals die output. A literal '%' is encoded as
# '%25' so the rendering is unambiguous (a raw '%1B' never reads as an encoded
# ESC), and the '...' truncation marker is appended only when a byte was
# actually dropped (MM-1).
sanitize_display_value() {  # <value> [max_len=160]
  local value="$1" max_len="${2:-160}" out="" ch i n LC_ALL=C
  n=${#value}
  if ((max_len < 4)); then
    printf '%s' "${value:0:max_len}"
    return 0
  fi
  for ((i = 0; i < n; i++)); do
    ch="${value:i:1}"
    if [[ "$ch" == [[:print:]] && "$ch" != '%' ]]; then
      out+="$ch"
    else
      printf -v ch '%%%02X' "'$ch"
      out+="$ch"
    fi
    if ((${#out} + 3 >= max_len)); then
      if ((i + 1 < n)); then
        out+='...'
      fi
      break
    fi
  done
  printf '%s' "$out"
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

# ── WI#762: privileged lock paths ────────────────────────────────────────────
# Root lock state lives under LUNARWING_MT_LOCK_ROOT (/run/lunarwing/mt-admin),
# a root-created tree beneath /run (0755 root), not under /run/lock, which some
# distributions create world-writable: an account that made the lock directory
# before root's first command chose what root's later lock-file opens reached.
# Every privileged lock goes through the helpers below:
#   - the directory is asserted BEFORE it is created, because install -d on an
#     existing directory re-owns it and hides who made it;
#   - a link in place of a lock file is refused by name;
#   - the file is opened with `<>` (O_RDWR|O_CREAT, bash never adds O_TRUNC),
#     never `>`, followed by a descriptor/path identity check, so a link swapped
#     in behind the pre-check fails instead of being followed and truncated.
# A failure names the path and the way out, and leaves everything else alone.

_lw_lstat_id() {
  local id
  id="$(stat -c '%d:%i' "$1" 2>/dev/null)" || return 1
  printf '%s' "$id"
}

# Describe why an existing lock directory is unsafe; print nothing when it is.
_lw_lock_dir_refusal() {  # <dir> [expected-mode]
  local dir="$1" want="${2:-700}" owner mode
  [[ -e "$dir" || -L "$dir" ]] || return 0
  if [[ -L "$dir" ]]; then printf 'it is a symlink'; return 0; fi
  if [[ ! -d "$dir" ]]; then printf 'it is not a directory'; return 0; fi
  mode="$(stat -c '%a' "$dir" 2>/dev/null || true)"
  owner="$(stat -c '%u' "$dir" 2>/dev/null || true)"
  if [[ "$mode" != "$want" ]]; then
    printf 'it has mode %s, not %s' "${mode:-unknown}" "$want"
    return 0
  fi
  if ((EUID == 0)) && [[ "$owner" != 0 ]]; then
    printf 'it is owned by uid %s, not root' "${owner:-unknown}"
  fi
}

# Refuse a lock directory whose existing ancestors are links or are writable by
# another account without the sticky bit: such a parent lets that account
# replace the directory between the check and the mkdir. A sticky parent
# (/tmp, and /run/lock on Debian) cannot be used to replace a root-owned
# directory, so it is allowed; the final directory check above still applies.
_lw_lock_parent_chain_safe() {  # <dir>
  local dir="$1" component current="" mode
  local IFS=/
  for component in ${dir#/}; do
    current="$current/$component"
    [[ "$current" == "$dir" ]] && break
    if [[ -L "$current" ]]; then return 1; fi
    [[ -e "$current" ]] || continue
    [[ -d "$current" ]] || return 1
    mode="$(stat -c '%a' "$current" 2>/dev/null || true)"
    [[ "$mode" =~ ^[0-7]+$ ]] || return 1
    if (( (8#$mode & 0022) != 0 && (8#$mode & 01000) == 0 )); then
      return 1
    fi
  done
}

_lw_lock_dir_remediation() {  # <path>
  printf "remove it as root and rerun the command: rm -rf '%s'" "$1"
}

# Assert, create, and re-assert a root lock directory. An existing directory is
# never re-owned or re-moded: a foreign one fails closed before that can hide
# the evidence.
_lw_lock_dir_prepare() {  # <dir> [expected-mode]
  local dir="$1" want="${2:-700}" refusal prev_umask
  [[ "$dir" == /* ]] || die "unsafe lock directory $dir: not an absolute path"
  refusal="$(_lw_lock_dir_refusal "$dir" "$want")"
  [[ -z "$refusal" ]] || die "unsafe lock directory $dir: $refusal; $(_lw_lock_dir_remediation "$dir")"
  if [[ ! -e "$dir" && ! -L "$dir" ]]; then
    _lw_lock_parent_chain_safe "$dir" \
      || die "unsafe lock directory $dir: a parent is a symlink or writable by another account; move the lock path under ${LUNARWING_MT_LOCK_ROOT:-/run/lunarwing/mt-admin} or fix the parent"
    # After a reboot /run is empty; recreate the missing parts of the root tree
    # (0755 root) so the leaf can be made. Each level is checked the same way.
    if [[ "$dir" != / && ! -d "$(dirname "$dir")" ]]; then
      _lw_lock_dir_prepare "$(dirname "$dir")" 755
    fi
    prev_umask="$(umask)"
    umask 077
    if ! mkdir -- "$dir" 2>/dev/null; then
      umask "$prev_umask"
      refusal="$(_lw_lock_dir_refusal "$dir" "$want")"
      [[ -z "$refusal" ]] || die "unsafe lock directory $dir: $refusal; $(_lw_lock_dir_remediation "$dir")"
      return 0
    fi
    umask "$prev_umask"
    # mkdir made it root-owned (mt-admin runs as root); never touch a directory
    # that already existed.
    chown root:root -- "$dir" 2>/dev/null || true
    chmod "$want" -- "$dir" 2>/dev/null || true
  fi
  refusal="$(_lw_lock_dir_refusal "$dir" "$want")"
  [[ -z "$refusal" ]] || die "unsafe lock directory $dir: $refusal; $(_lw_lock_dir_remediation "$dir")"
  _lw_lstat_id "$dir" >/dev/null || die "cannot read lock directory $dir"
}

# Describe why an open lock descriptor is unsafe; print nothing when it is.
_lw_lock_file_refusal() {  # <fd> <path>
  local fd="$1" path="$2" fd_id path_id links mode owner
  fd_id="$(stat -Lc '%d:%i' "/proc/self/fd/$fd" 2>/dev/null || true)"
  path_id="$(_lw_lstat_id "$path" || true)"
  links="$(stat -Lc '%h' "/proc/self/fd/$fd" 2>/dev/null || true)"
  mode="$(stat -Lc '%a' "/proc/self/fd/$fd" 2>/dev/null || true)"
  owner="$(stat -Lc '%u' "/proc/self/fd/$fd" 2>/dev/null || true)"
  if [[ -z "$fd_id" || "$fd_id" != "$path_id" ]]; then
    printf 'it is a symlink or was replaced during open'
    return 0
  fi
  if [[ "$links" != 1 ]]; then
    printf 'it has %s hard links, not 1' "${links:-unknown}"
    return 0
  fi
  if [[ ! "$mode" =~ ^[0-7]+$ ]] || (( (8#$mode & 0022) != 0 )); then
    printf 'it has mode %s, which is group- or other-writable' "${mode:-unknown}"
    return 0
  fi
  if ((EUID == 0)) && [[ "$owner" != 0 ]]; then
    printf 'it is owned by uid %s, not root' "${owner:-unknown}"
  fi
}

# Open a privileged lock file without following a link and without truncating.
_lw_lock_open_fd() {  # <path> <fd-var> [lock-dir-mode]
  local path="$1" fd_var="$2" dir_mode="${3:-700}" dir="${1%/*}" fd refusal prev_umask
  [[ "$path" == /* && -n "$dir" && "$dir" != "$path" ]] \
    || die "unsafe lock file $path: not an absolute path"
  _lw_lock_dir_prepare "$dir" "$dir_mode"
  [[ ! -L "$path" ]] \
    || die "refusing lock file $path: it is a symlink; $(_lw_lock_dir_remediation "$path")"
  [[ ! -e "$path" || -f "$path" ]] \
    || die "refusing lock file $path: it is not a regular file; $(_lw_lock_dir_remediation "$path")"
  prev_umask="$(umask)"
  umask 077
  if ! exec {fd}<>"$path"; then
    umask "$prev_umask"
    die "cannot open lock file $path"
  fi
  umask "$prev_umask"
  refusal="$(_lw_lock_file_refusal "$fd" "$path")"
  if [[ -n "$refusal" ]]; then
    eval "exec $fd>&-"
    die "refusing lock file $path: $refusal; $(_lw_lock_dir_remediation "$path")"
  fi
  printf -v "$fd_var" '%s' "$fd"
}

# WI#762: create the whole root lock tree (suggestion: a root helper, run at
# install/boot via init-lock-tree, owns the directories before any tenant
# account exists). Parents are 0755 so tenant-side fd backstops can stat the
# leaf directories; the leaves are 0700 and hold all lock files.
lw_lock_tree_init() {
  local root="${LUNARWING_MT_LOCK_ROOT:-/run/lunarwing/mt-admin}"
  _lw_lock_dir_prepare "$(dirname "$root")" 755
  _lw_lock_dir_prepare "$root" 755
  _lw_lock_dir_prepare "$root/locks"
  _lw_lock_dir_prepare "$root/build"
  _lw_lock_dir_prepare "$root/darkirc"
}

# WI#762: `init-lock-tree` — the root helper that creates the lock tree before
# any tenant account exists. On systemd it also renders the tmpfiles.d drop-in
# that recreates the tree at boot ahead of tenant logins; OpenRC tenant units
# create it in start_pre, and every lock acquisition re-asserts it.
init_lock_tree_command() {
  local root="${LUNARWING_MT_LOCK_ROOT:-/run/lunarwing/mt-admin}"
  local tmpfiles=/etc/tmpfiles.d/lunarwing-mt-admin.conf
  lw_lock_tree_init
  if [[ "${INIT_SYSTEM:-}" == systemd ]]; then
    cat >"$tmpfiles" <<EOF
# WI#762: root lock state, created before tenant accounts can run anything.
# /run/lock is 1777 on Debian; a lock directory another account made first
# would choose what root's later lock-file opens reach.
d $(dirname "$root") 0755 root root -
d $root 0755 root root -
d $root/locks 0700 root root -
d $root/build 0700 root root -
d $root/darkirc 0700 root root -
EOF
    chmod 0644 "$tmpfiles"
    say "rendered $tmpfiles"
  fi
  say "root lock tree ready under $root"
}

acquire_ports_lock() {
  local registry_dir
  [[ "$PORTS_LOCK_HELD" == "true" ]] && return 0
  require_cmd flock
  registry_dir="$(dirname "$PORTS_REGISTRY")"
  mkdir -p "$registry_dir"
  exec 199<"$registry_dir"
  flock -x 199
  PORTS_LOCK_HELD="true"
}

release_ports_lock() {
  [[ "$PORTS_LOCK_HELD" == "true" ]] || return 0
  flock -u 199
  exec 199>&-
  PORTS_LOCK_HELD="false"
}

# Check identity and held state without converting an unlocked forged descriptor
# into a valid boundary. A separate open-file description must remain blocked.
inherited_lock_fd_is_held() {
  local lock_fd="$1" lock_file="$2" probe_fd
  [[ "$lock_fd" =~ ^[0-9]+$ && -e "/proc/self/fd/$lock_fd" \
    && -f "$lock_file" && ! -L "$lock_file" \
    && "$(stat -Lc '%d:%i' "/proc/self/fd/$lock_fd" 2>/dev/null)" \
      == "$(stat -c '%d:%i' "$lock_file" 2>/dev/null)" ]] || return 1
  # WI#762: `<>` never truncates and the identity check refuses a link that
  # replaced the file after the pre-check above.
  exec {probe_fd}<>"$lock_file" || return 1
  if [[ -n "$(_lw_lock_file_refusal "$probe_fd" "$lock_file")" ]]; then
    eval "exec $probe_fd>&-"
    return 1
  fi
  if flock -x -n "$probe_fd"; then
    flock -u "$probe_fd" || true
    exec {probe_fd}>&-
    return 1
  fi
  exec {probe_fd}>&-
  # Probe proved some open-file description holds this inode. This succeeds
  # without changing lock ownership only when inherited FD is that description;
  # an unlocked same-inode FD remains blocked by holder and must fail.
  flock -x -n "$lock_fd"
}

# WI#505: run a command with every tracked mt-admin lock descriptor closed.
# bash's `exec {fd}>` has no close-on-exec, and an flock belongs to the
# open-file description, so a descriptor inherited by a daemon started inside
# a locked section keeps the tenant lock held after mt-admin dies (a `die`
# inside the section skips the unlock/close). On OpenRC, rc-service and
# supervise-daemon are children of mt-admin, so they carry every tracked fd —
# the lifecycle fd, the direct and wrapper-inherited upgrade fds
# (04-tenant-lifecycle.sh) and the DarkIRC writer fd (06-env-generation.sh) —
# not just the lifecycle one. Per-command redirections close the fds only for
# that command's child, so mt-admin keeps its own fds and still owns the
# unlocks, and shell-function stubs in harnesses keep working.
with_tracked_lock_fds_closed() {
  local -a lw_fds=()
  local lw_fd lw_close_args=""
  for lw_fd in "${LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD:-}" \
    "${LUNARWING_MT_ACTIVE_UPGRADE_LOCK_FD:-}" \
    "${LUNARWING_MT_INHERITED_UPGRADE_LOCK_FD:-}" \
    "${LUNARWING_MT_UPGRADE_LOCK_FD:-}" \
    "${DARKIRC_WRITER_LOCK_FD:-}"; do
    [[ "$lw_fd" =~ ^[0-9]+$ ]] && lw_fds+=("$lw_fd")
  done
  if ((${#lw_fds[@]} == 0)); then
    "$@"
    return
  fi
  for lw_fd in "${lw_fds[@]}"; do
    lw_close_args+=" ${lw_fd}>&-"
  done
  # eval is required: per-command fd redirections must be literal in the
  # parsed command, and the tracked values are digit-validated above, so no
  # tenant-controlled value can reach the eval string.
  eval '"$@"'"$lw_close_args"
}

# WI#505: decode a file's device as the "major:minor" pair /proc/locks uses.
# The dev_t layout is NOT a hex string split in half: the minor is bits 0-7
# plus 20-31 and the major is bits 8-19 (linux/kdev_t.h), so splitting
# `stat -c %D` at the midpoint misreads tmpfs/overlay/nvme devices (e.g. a
# tmpfs `2e` is 0:46, not 02:0e, and a one-character hex device breaks the
# arithmetic entirely). `stat -c %d` is the decimal dev_t; coreutils
# `%Hd:%Ld` already decodes the layout directly, with an awk fallback.
_lock_file_dev_id() {
  local lock_file="$1" dev inode
  dev="$(stat -Lc '%Hd:%Ld' "$lock_file" 2>/dev/null || true)"
  if [[ ! "$dev" =~ ^[0-9]+:[0-9]+$ ]]; then
    dev="$(stat -Lc '%d' "$lock_file" 2>/dev/null)" || return 1
    dev="$(awk -v d="$dev" 'BEGIN { printf "%d:%d", int(d / 256) % 4096, (d % 256) + int(d / 1048576) * 256 }')"
  fi
  inode="$(stat -Lc '%i' "$lock_file" 2>/dev/null)" || return 1
  printf '%s:%s' "$dev" "$inode"
}

# WI#505: best-effort list of LIVE processes holding a descriptor open on the
# given lock file, for stale-holder diagnosis. /proc/locks names the original
# flock(2) acquirer (for mt-admin the transient `flock -x` child), which is
# usually dead and can be recycled — never kill by that value. The processes
# that actually keep the lock held are the live inheritors, so scan
# /proc/[0-9]/fd for open descriptors on the lock path instead (fd targets
# keep the path even after the file is unlinked, suffixed " (deleted)").
_lock_file_live_inheritor_pids() {
  local lock_file="$1" pids="" pid_path pid fd_path fd_target
  for pid_path in /proc/[0-9]*; do
    pid="${pid_path##*/}"
    for fd_path in "$pid_path"/fd/*; do
      fd_target="$(readlink "$fd_path" 2>/dev/null || true)"
      case "$fd_target" in
        "$lock_file"|"$lock_file (deleted)") ;;
        *) continue ;;
      esac
      case " $pids " in
        *" $pid "*) ;;
        *) pids="${pids:+$pids }$pid" ;;
      esac
      break
    done
  done
  printf '%s' "$pids"
}

# Fail fast instead of blocking on a lock held only by leaked descriptors
# (WI#505). A live competing mt-admin command fails loudly here too; wait for
# it to finish before retrying. The holder list names live inheritor pids
# (processes keeping the flock held via an inherited descriptor), never the
# dead original acquirer from /proc/locks.
_lock_held_preflight() {
  local kind="$1" lock_file="$2" probe_fd holder_pids
  _lw_lock_open_fd "$lock_file" probe_fd
  if flock -x -n "$probe_fd"; then
    flock -u "$probe_fd" || true
    eval "exec $probe_fd>&-"
    return 0
  fi
  eval "exec $probe_fd>&-"
  holder_pids="$(_lock_file_live_inheritor_pids "$lock_file")"
  die "tenant $kind lock is already held${holder_pids:+ by live inheritor pids: $holder_pids}; wait for the competing mt-admin command to finish, or stop leftover tenant daemons if they inherited the descriptor (WI#505)"
}

_lifecycle_lock_preflight() {
  _lock_held_preflight "lifecycle for '$1'" "$TENANT_LIFECYCLE_LOCK_DIR/${1}.lock"
}

# WI#527 (run-300 TO-1): true when this process inherited the tenant's upgrade
# lock — i.e. it is a child of the upgrade wrapper's own boundary, which holds
# that inode for the whole run. A verb running there must never preflight the
# lock (it would meet its own parent) and must never re-acquire it from a fresh
# open-file description (a second description blocks even for the same process).
upgrade_lock_inherited_for() {  # <name>
  local name="$1" upgrade_fd="${LUNARWING_MT_INHERITED_UPGRADE_LOCK_FD:-}"
  [[ "$upgrade_fd" =~ ^[0-9]+$ ]] || return 1
  inherited_lock_fd_is_held "$upgrade_fd" "${BUILD_LOCK}.${name}.upgrade"
}

with_tenant_lifecycle_lock() {
  local name="$1"
  shift
  [[ "$name" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] \
    || die "invalid tenant name for lifecycle lock"
  require_cmd flock
  # WI#762: assert the directory before creating it, then open the lock file
  # without following a link and without truncation.
  _lw_lock_dir_prepare "$TENANT_LIFECYCLE_LOCK_DIR"
  local lock_fd result caller_errexit=false owns_lock=false active_lock_set=false
  [[ $- != *e* ]] || caller_errexit=true
  if [[ -n "${LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD:-}" ]]; then
    lock_fd="$LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD"
    [[ "${LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT:-}" == "$name" ]] \
      && inherited_lock_fd_is_held "$lock_fd" "$TENANT_LIFECYCLE_LOCK_DIR/${name}.lock" \
      || die "invalid active tenant lifecycle lock"
  elif [[ -n "${LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_FD:-}" ]]; then
    lock_fd="$LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_FD"
    [[ "${LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_TENANT:-}" == "$name" ]] \
      && inherited_lock_fd_is_held "$lock_fd" "$TENANT_LIFECYCLE_LOCK_DIR/${name}.lock" \
      || die "invalid inherited tenant lifecycle lock"
    LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD="$lock_fd"
    LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT="$name"
    active_lock_set=true
  else
    # WI#505: fail fast (naming any live inheritor pids) instead of blocking
    # forever on a lock orphaned by a die inside an earlier locked section.
    _lifecycle_lock_preflight "$name"
    _lw_lock_open_fd "$TENANT_LIFECYCLE_LOCK_DIR/${name}.lock" lock_fd
    if ! flock -x "$lock_fd"; then
      eval "exec $lock_fd>&-"
      return 1
    fi
    LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD="$lock_fd"
    LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT="$name"
    active_lock_set=true
    owns_lock=true
  fi

  # Do not use `"$@" || result=$?`: Bash would ignore even an explicit
  # `set -e` in the callback's worker subshells (WI#469). Capture outside a
  # conditional, then clean up before restoring the caller's errexit state.
  set +e
  "$@"
  result=$?
  if [[ "$active_lock_set" == true ]]; then
    unset LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT
  fi
  if [[ "$owns_lock" == true ]]; then
    flock -u "$lock_fd" || result=1
    exec {lock_fd}>&-
  fi
  if [[ "$caller_errexit" == true ]]; then set -e; else set +e; fi
  return "$result"
}

generate_token() {
  require_cmd od
  local token
  token="$(dd if=/dev/urandom bs=32 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n')"
  [[ "$token" =~ ^[0-9a-f]{64}$ && "$token" != "0000000000000000000000000000000000000000000000000000000000000000" ]] \
    || die "failed to generate secure token"
  printf '%s' "$token"
}

# Generate a non-secret registry identity. Callers hold the ports lock while
# checking uniqueness and persisting the value.
generate_darkirc_scope_id() {
  require_cmd od
  local scope_id
  scope_id="$(dd if=/dev/urandom bs=16 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n')"
  [[ "$scope_id" =~ ^[0-9a-f]{32}$ && "$scope_id" != "00000000000000000000000000000000" ]] \
    || die "failed to generate DarkIRC scope ID"
  printf '%s' "$scope_id"
}

generate_unique_darkirc_scope_id() {
  local scope_id attempts=0 jq_status
  while (( attempts < 16 )); do
    scope_id="$(generate_darkirc_scope_id)"
    if jq -e --arg scope "$scope_id" \
        '[.tenants // {} | to_entries[] | select(.value.darkirc_scope_id == $scope)] | length == 0' \
        "$PORTS_REGISTRY" >/dev/null 2>&1; then
      printf '%s' "$scope_id"
      return 0
    fi
    jq_status=$?
    [[ "$jq_status" == "1" ]] || die "failed to validate DarkIRC scope ID uniqueness"
    attempts=$((attempts + 1))
  done
  die "failed to allocate unique DarkIRC scope ID"
}

ensure_darkirc_scope_id() {
  local name="$1" current matches scope_id tmp
  local had_lock="$PORTS_LOCK_HELD"
  acquire_ports_lock
  jq -e --arg name "$name" \
    '(.tenants // {}) | has($name) and (.[$name].enable_darkirc == true)' \
    "$PORTS_REGISTRY" >/dev/null 2>&1 || die "tenant '$name' is missing or DarkIRC is disabled"
  current="$(jq -r --arg name "$name" '.tenants[$name].darkirc_scope_id // empty' \
    "$PORTS_REGISTRY" 2>/dev/null)" || die "failed to read DarkIRC scope ID for tenant '$name'"
  if [[ -n "$current" ]]; then
    validate_darkirc_scope_id "$current"
    matches="$(jq -r --arg scope "$current" \
      '[.tenants // {} | to_entries[] | select(.value.darkirc_scope_id == $scope)] | length' \
      "$PORTS_REGISTRY" 2>/dev/null)" || die "failed to validate DarkIRC scope ID for tenant '$name'"
    [[ "$matches" == "1" ]] || die "duplicate DarkIRC scope ID for tenant '$name'"
    [[ "$had_lock" == "true" ]] || release_ports_lock
    printf '%s' "$current"
    return 0
  fi

  scope_id="$(generate_unique_darkirc_scope_id)"
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" --arg scope "$scope_id" \
      '.tenants[$name].darkirc_scope_id = $scope' "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    die "failed to store DarkIRC scope ID for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  [[ "$had_lock" == "true" ]] || release_ports_lock
  printf '%s' "$scope_id"
}

sanitize_name() {
  local raw="$1"
  printf '%s' "$raw" | tr '[:upper:]' '[:lower:]' | tr -cs 'a-z0-9-' '-' | sed 's/^-//;s/-$//'
}

tenant_home() { printf '/home/%s' "$1"; }
tenant_lw_root() { printf '%s/lunarwing' "$(tenant_home "$1")"; }
tenant_repo() { printf '%s/ic' "$(tenant_lw_root "$1")"; }
tenant_env_dir() { printf '%s/env' "$(tenant_lw_root "$1")"; }
tenant_quadlet_dir() { printf '%s/.config/containers/systemd' "$(tenant_home "$1")"; }
tenant_state_dir() { printf '%s/state' "$(tenant_lw_root "$1")"; }
tenant_log_dir() { printf '%s/logs' "$(tenant_lw_root "$1")"; }
tenant_run_dir() { printf '%s/run' "$(tenant_lw_root "$1")"; }
# ── Container config-hash tracking ────────────────────────────────────────────
#
# Quadlet and imperative `podman run` both create a container with config
# (env vars, volumes, ports) baked in at creation time. `systemctl restart`
# and `podman start` only restart the EXISTING container — they do NOT pick
# up changes to the .container file or a re-rendered `podman run` command.
# This causes stale-env bugs on upgrade (e.g. AGENT_AUTH_TOKEN missing,
# SSH_AUTH_SOCK not mounted) because the old container survives the re-render.
#
# Fix: after rendering a quadlet or before an imperative `podman run`, compute
# a hash of the config source and compare it to the hash stored when the
# container was last created. If they differ, force-recreate the container.
# The hash is stored in a sidecar file next to the container's quadlet/state.

# Directory for config-hash sidecars (created on first use). The sidecar is
# mt-admin's own state, so it lives in root-owned host state, not the
# tenant-controlled ~/.config chain (WI#446, run-174 LW-3): root's historical
# mkdir/printf under ~/.config followed pre-placed symlinks, and the root-owned
# sidecars the old writer left on upgraded tenants could never be updated by a
# tenant-side writer (every start then force-recreated the container).
_hash_dir() { printf '%s/%s' "$CONTAINER_HASH_DIR" "$1"; }

# Hash a config file with an optional launch contract for arguments that are not
# represented in that file. Empty contracts retain the historical file hash.
_container_config_hash() {  # <config-file> [launch-contract]
  local config_file="$1" contract="${2:-}" file_hash
  file_hash="$(sha256sum "$config_file" 2>/dev/null | cut -d' ' -f1 || true)"
  [[ -n "$file_hash" ]] || return 1
  if [[ -z "$contract" ]]; then
    printf '%s\n' "$file_hash"
  else
    printf '%s\n%s\n' "$file_hash" "$contract" | sha256sum | cut -d' ' -f1
  fi
}

# Compute and store the hash of a config file (e.g. a .container quadlet).
# Usage: _store_container_hash <tenant> <container-name> <config-file> [launch-contract]
# WI#446 (run-174 LW-3): written as root into root-owned host state (see
# _hash_dir), so no tenant-controlled path component is ever followed and the
# sidecar stays updatable across upgrades. WI#450 (run-181 MC-1): each hash
# covers a secret-bearing env file or Quadlet, and the tenant home that used
# to hold these sidecars was private — root's ambient umask would leave the
# base and per-tenant directories 0755 and the file 0644, readable by every
# local user. The directories are forced 0700 and the file is written under
# umask 077 (0600). The mkdir itself stays outside umask 077: on a host where
# /var/lib/lunarwing does not exist yet, a 0700 base would block tenants from
# reaching /var/lib/lunarwing/voice-models, which is installed 0755.
# A store failure is non-fatal by design: without a stored hash every start
# recreates the container (the fail-safe direction for a sidecar) until the
# write succeeds.
_store_container_hash() {
  local name="$1" container="$2" config_file="$3" contract="${4:-}" hdir hash
  hdir="$(_hash_dir "$name")"
  hash="$(_container_config_hash "$config_file" "$contract" || true)"
  [[ -n "$hash" ]] || return 0
  if ! mkdir -p -- "$hdir" 2>/dev/null \
    || ! chmod 0700 -- "$CONTAINER_HASH_DIR" "$hdir" 2>/dev/null \
    || ! (umask 077 && printf '%s\n' "$hash" >"$hdir/${container}.hash") 2>/dev/null; then
    say "WARNING: could not store the config hash sidecar for $container (tenant '$name'); starts will recreate the container until the sidecar can be written" >&2
  fi
}

# Check whether the config file's hash matches the stored hash.
# Returns 0 when missing/mismatched and recreation is needed, 1 when unchanged.
# Usage: _container_config_changed <tenant> <container-name> <config-file> [launch-contract]
_container_config_changed() {
  local name="$1" container="$2" config_file="$3" contract="${4:-}" hdir stored current
  hdir="$(_hash_dir "$name")"
  [[ -f "$config_file" ]] || return 1  # no config file = no opinion
  current="$(_container_config_hash "$config_file" "$contract" || true)"
  [[ -n "$current" ]] || return 1      # can't hash = don't force recreate
  stored=""
  [[ -f "$hdir/${container}.hash" ]] && stored="$(cat "$hdir/${container}.hash" 2>/dev/null || true)"
  [[ -z "$stored" || "$stored" != "$current" ]]
}

# Force-recreate a Quadlet-managed container: stop the service, remove the
# stale container, then start the service (Quadlet re-runs `podman run`).
# Usage: _recreate_quadlet_container <tenant> <service-name> <container-name>
_recreate_quadlet_container() {
  local name="$1" svc="$2" container="$3"
  say "config changed for $container; force-recreating"
  _systemctl_user "$name" stop "$svc" >/dev/null 2>&1 || true
  _ctr "$name" rm -f "$container" >/dev/null 2>&1 || true
}

tenant_darkirc_enabled() {
  local name="$1"
  local val
  val="$(jq -r ".tenants[\"$name\"].enable_darkirc // false" "$PORTS_REGISTRY" 2>/dev/null)"
  [[ "$val" == "true" ]]
}

tenant_proxy_enabled() {
  local name="$1"
  local val
  val="$(jq -r ".tenants[\"$name\"].enable_proxy // false" "$PORTS_REGISTRY" 2>/dev/null)"
  [[ "$val" == "true" ]]
}

# Whether WeeChat is enabled for this tenant. Default TRUE: the flag is
# absent on tenants created before the health-gate existed, and a missing or
# unreadable registry must fail safe to the old behavior (weechat managed).
# Used to make disable-weechat authoritative for lifecycle, not just health
# classification (ISSUE-006). Explicit ==false test: jq `//` misreads false.
tenant_weechat_enabled() {
  local name="$1"
  local val
  val="$(jq -r --arg name "$name" 'if .tenants[$name].enable_weechat == false then "false" else "true" end' "$PORTS_REGISTRY" 2>/dev/null)"
  [[ "$val" != "false" ]]
}

# Whether a supported external worker (pebble|opencode|kimi|opencode2) is selected for this
# tenant. The selection is persisted at add-tenant into
# .tenants[<name>].workers.<worker> and gates start_tenant_<worker> — otherwise a
# tenant would start every worker whose SHARED host image happens to exist,
# regardless of what was chosen (see docs/proposals/PER_TENANT_WORKER_GATING.md).
# Absent key = false (OFF): a tenant provisioned before this flag existed does not
# auto-start workers until re-selected via `add-tenant <name> --with-<worker>`.
tenant_worker_enabled() {
  local name="$1" worker="$2"
  local val
  val="$(jq -r ".tenants[\"$name\"].workers[\"$worker\"] // false" "$PORTS_REGISTRY" 2>/dev/null)"
  [[ "$val" == "true" ]]
}

# OpenRC worker pause state. Existing enabled workers default to running when
# this optional field is absent; stop-worker/kill-worker persist false and
# start-worker restores true. Kept separate from .workers because selection and
# operator-requested runtime state are different concerns.
tenant_worker_desired_running() {
  local name="$1" worker="$2" val
  tenant_worker_enabled "$name" "$worker" || return 1
  val="$(jq -r --arg name "$name" --arg worker "$worker" '
    if .tenants[$name].worker_runtime[$worker].desired_running == false
    then "false" else "true" end
  ' "$PORTS_REGISTRY" 2>/dev/null || true)"
  [[ "$val" == "true" ]]
}

# Cleanup may stop live services, so unknown registry state must fail safe. Only
# an explicit unselected worker or desired_running=false authorizes OpenRC stop.
tenant_worker_openrc_stop_requested() {
  local name="$1" worker="$2" val
  val="$(jq -r --arg name "$name" --arg worker "$worker" '
    if .tenants[$name] == null then "unknown"
    elif .tenants[$name].workers[$worker] != true then "stopped"
    elif .tenants[$name].worker_runtime[$worker].desired_running == false then "stopped"
    else "running" end
  ' "$PORTS_REGISTRY" 2>/dev/null)" || return 1
  [[ "$val" == "stopped" ]]
}

# Per-tenant PostgreSQL password. The source of truth is a 0600, tenant-owned
# secret file, generated once (hex → URL-safe inside DATABASE_URL) and reused so
# it stays STABLE across restarts/reconfigures — POSTGRES_PASSWORD only
# initialises an EMPTY datadir, so the value must not drift after first init.
# Migration-safe: if a tenant was already provisioned (its lunarwing.env carries a
# DATABASE_URL password), that value is preserved so an already-initialised DB
# keeps working; only brand-new tenants get a fresh random password. Use
# `rotate-pg-password` to deliberately move an existing tenant onto a random one.
# Never logged (repo rule).
#
# WI#423: root never resolves the tenant-writable env paths here. Both value
# reads (pg.secret, the DATABASE_URL line of lunarwing.env) come from ONE
# root-private snapshot per file taken as the tenant — a cross-tenant symlink
# present for the read window is unreadable to the tenant-side cat and dies at
# the snapshot instead of handing this render the other tenant's PG password.
# The mint write commits through the tenant-side atomic writer, so a pre-placed
# symlink at pg.secret can no longer redirect a root truncating write (the
# original WI#390 shape). An existing-but-unreadable-as-tenant pg.secret
# refuses loudly: such a file is unreadable to the tenant daemon too.
tenant_pg_password() {
  local name="$1" f envf existing pw secret_snapshot env_snapshot
  f="$(tenant_env_dir "$name")/pg.secret"
  envf="$(tenant_env_dir "$name")/lunarwing.env"

  _snapshot_tenant_env "$name" "$f" secret_snapshot
  if ! pw="$(_pg_secret_from_snapshot "$secret_snapshot")"; then
    rm -f -- "$secret_snapshot"
    die "pg.secret for tenant '$name' is not a single password line: $f"
  fi
  rm -f -- "$secret_snapshot"
  if [[ -n "$pw" ]]; then
    printf '%s\n' "$pw"
    return 0
  fi

  _snapshot_tenant_env "$name" "$envf" env_snapshot
  existing="$(sed -n 's#^DATABASE_URL=postgres://lunarwing:\([^@]*\)@.*#\1#p' "$env_snapshot" | head -1)"
  rm -f -- "$env_snapshot"
  if [[ -n "${existing:-}" ]]; then
    pw="$existing"                       # preserve an already-initialised DB's password
  else
    pw="$(generate_token | cut -c1-32)"  # fresh tenant → random 128-bit hex
  fi
  printf '%s\n' "$pw" | write_tenant_file_atomic "$name" "$f" 600 \
    || die "could not safely write $f as tenant '$name' (unsafe path shape; WI#423)"
  printf '%s\n' "$pw"
}

# Read the single password line out of a root-private pg.secret snapshot.
# Callers use $(...), which strips the trailing newline. A file carrying more
# than one line — or any control character at all, which also catches a
# leading blank line the old `grep -c .` count let through — is not a shape
# mt-admin ever wrote: refuse it (run-166 LF-3).
_pg_secret_from_snapshot() {  # <snapshot file>
  local value
  value="$(<"$1")"
  [[ "$value" != *[[:cntrl:]]* ]] || return 1
  printf '%s' "$value"
}

# ── Template rendering ────────────────────────
#
# render_template <template-file> <output-file> [var=value ...]
#
# Reads a template file, replaces __VARNAME__ placeholders with their values
# (supplied as var=value pairs on the command line), and writes the result.
# Template files live under $TEMPLATES_DIR. If the template path is relative
# (no leading /), it is resolved against $TEMPLATES_DIR.
render_template() {
  local template="$1" out="$2"
  shift 2
  render_template_content "$template" "$@" >"$out"
}

render_template_content() {
  local template="$1"
  shift
  [[ "$template" = /* ]] || template="$TEMPLATES_DIR/$template"
  [[ -f "$template" ]] || die "template not found: $template"
  local content pair var val
  content="$(<"$template")"
  for pair; do
    var="${pair%%=*}"
    val="${pair#*=}"
    content="${content//__${var}__/${val}}"
  done
  printf '%s\n' "$content"
}
# ── Root check ────────────────────────────────────────────────────────────────

require_root() {
  [[ "${EUID}" -eq 0 ]] || die "must run as root (use sudo)"
}

tenant_voice_enabled() {
  [[ "$(jq -r --arg name "$1" '.tenants[$name].enable_voice // false' "$PORTS_REGISTRY" 2>/dev/null)" == "true" ]]
}

# ── Managed scheduler writers (root crontab block, root unit pair) ────────────
#
# The host health pipeline (12-health-pipeline.sh) owns a block in root's
# crontab and a pair of root unit files, and replaces something root's scheduler
# runs on a timer. A half-written replacement is worse than no update: root's
# other cron jobs disappear, or the unit root's timer runs is left empty. Each
# helper therefore builds the replacement beside its place, checks it, and only
# then installs it. Both are reached from callers that may run with errexit
# ignored, so they return nonzero and print the reason instead of exiting; the
# caller decides whether that is fatal (WI#740). The Study Notes sync scheduler
# (14-knowledge-sync.sh) uses the same writers (WI#743).

# How each supported cron tool reports "root has no crontab", measured with
# LC_ALL=C on a real tool (2026-11-17). Only one of these lines means the empty
# table a block may be added to; any other failing listing is refused:
#
#   fcrontab 3.3.1        exit 0, no stdout, stderr "user root has no fcrontab."
#   crontab (cronie)      exit 1, no stdout, stderr "no crontab for root"
#   crontab (dcron 4.5+)  exit 0, no stdout, stderr "no crontab for root"
#   busybox crontab       exit 1, no stdout, stderr
#                         "crontab: can't open 'root': No such file or directory"
#                         (a missing spool directory says instead
#                          "crontab: can't change directory to '...'")
#
# An existing but empty table never prints one of them (fcron prints "listing
# root's fcrontab" and exits 0). `/dev/null` is not used for stderr, so a tool
# that fails for another reason is reported with its own message instead of
# being taken for an empty crontab (WI#743).
_cron_listing_detail() {  # <stderr text>
  local detail="$1"
  detail="${detail//$'\n'/ }"
  detail="${detail%% }"
  [[ -n "$detail" ]] && printf ' (%s)' "$detail"
}

_cron_listing_messages() {  # <stderr text>: omit only known benign fcron INFO
  local err="$1" line kept=""
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    # fcron stamps each of its log lines; strip the stamp so the message can
    # be compared. Its ordinary "listing ..." line is not a message.
    if [[ "$line" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}\ [0-9]{2}:[0-9]{2}:[0-9]{2}\ +INFO\ +(.*)$ ]]; then
      line="${BASH_REMATCH[1]}"
    fi
    [[ "$line" == "listing root's fcrontab" ]] && continue
    kept+="$line"$'\n'
  done <<<"$err" || return 1
  printf '%s' "${kept%$'\n'}"
}

_cron_listing_is_clean() {  # <stderr text>
  local messages
  messages="$(_cron_listing_messages "$1")" || return 1
  [[ -z "$messages" ]]
}

_cron_listing_says_no_table() {  # <stderr text>
  local body
  body="$(_cron_listing_messages "$1")" || return 1
  # Exactly one line, and one of the measured messages: anything else — no
  # output at all, a second error line, a wrapper's own complaint — is a
  # failed listing, not proof that root has no crontab.
  [[ "$body" != *$'\n'* ]] || return 1
  # These writers run as root and only replace root's table. A response for
  # another account cannot prove that root's old table may safely be replaced.
  [[ "$body" == 'no crontab for root' ]] && return 0
  [[ "$body" == 'user root has no fcrontab.' ]] && return 0
  [[ "$body" == "crontab: can't open 'root': No such file or directory" ]]
}

# Print, on stdout, the table root's crontab must become: every line of the
# current table outside the managed block, byte for byte, then the block —
# <begin>, the <block> lines, <end> — or nothing at all when no block lines are
# given, which is how a block is removed. Returns nonzero, having printed no
# table, when the current table
# cannot be delimited — a BEGIN line with no later END line, or a second BEGIN
# line before the first one's END line. Dropping "everything from BEGIN to the
# end of the file" would take root's later jobs with it, so the caller must
# leave the crontab as it was. The same holds when the listing cannot be read
# back in completely, or when it fails without positively reporting that root
# has no crontab. An END line with no BEGIN line above it belongs
# to root's own jobs and is kept. A root without a crontab is not an error:
# only the block is printed.
_managed_cron_table() {  # <crontab command> <begin> <end> [<block line>...]
  local command="$1" begin="$2" end="$3" listing line inside=0 kept="" read_rc=0
  local err_file read_err=""
  shift 3
  err_file="$(mktemp 2>/dev/null)" \
    || { warn_err "could not create a temporary file to read root's crontab with $command; the crontab was left as it was"; return 1; }
  listing="$(LC_ALL=C "$command" -l 2>"$err_file")" || read_rc=$?
  read_err="$(<"$err_file")" || read_err=""
  rm -f -- "$err_file"
  if (( read_rc != 0 )); then
    if [[ -n "$listing" ]]; then
      warn_err "could not read root's crontab with $command: it failed (exit $read_rc) after printing part of the table; the crontab was left as it was$(_cron_listing_detail "$read_err")"
      return 1
    fi
    if ! _cron_listing_says_no_table "$read_err"; then
      warn_err "could not read root's crontab with $command: it failed (exit $read_rc) without reporting that root has no crontab; the crontab was left as it was$(_cron_listing_detail "$read_err")"
      return 1
    fi
    # The tool positively reported a missing table: that is the empty table the
    # block is added to.
    listing=""
  elif [[ -z "$listing" ]] && _cron_listing_says_no_table "$read_err"; then
    # fcron and dcron exit 0 and report a missing table on stderr.
    listing=""
  elif ! _cron_listing_is_clean "$read_err"; then
    # fcrontab can exit 0 after an ERROR, with empty or partial stdout. Only
    # its measured listing INFO is benign; unknown diagnostics fail closed.
    warn_err "could not read root's crontab with $command: it reported unexpected diagnostics despite exit 0; the crontab was left as it was$(_cron_listing_detail "$read_err")"
    return 1
  fi
  if [[ -n "$listing" ]]; then
    while IFS= read -r line || [[ -n "$line" ]]; do
      if [[ "$line" == "$begin" ]]; then
        if (( inside )); then
          warn_err "root's crontab has a second line '$begin' before the line '$end'; repair the block with '$command -e', then schedule again; the crontab was left as it was"
          return 1
        fi
        inside=1
        continue
      fi
      if [[ "$line" == "$end" ]]; then
        if (( inside )); then
          inside=0
          continue
        fi
        kept+="$line"$'\n'
        continue
      fi
      (( inside )) || kept+="$line"$'\n'
    done <<<"$listing" || {
      # bash keeps a here-string larger than its pipe size (64 KiB) in a
      # temporary file. When that file cannot be written, the redirection
      # fails and the loop above does not run at all: the table would read as
      # empty, and the block alone would replace every job root has.
      warn_err "could not read the whole listing of root's crontab ($command -l) back in: the shell could not buffer it (is ${TMPDIR:-/tmp} full?); the crontab was left as it was"
      return 1
    }
  fi
  if (( inside )); then
    warn_err "root's crontab has the line '$begin' without a later line that reads exactly '$end'; repair the block with '$command -e', then schedule again; the crontab was left as it was"
    return 1
  fi
  # Each write is checked: a table that lost a line to a full disk must report
  # the failure rather than install the rest of itself.
  printf '%s' "$kept" || return 1
  if (( $# )); then
    {
      printf '%s\n' "$begin" \
        && printf '%s\n' "$@" \
        && printf '%s\n' "$end"
    } || return 1
  fi
}

# Replace root's crontab with the table _managed_cron_table builds. Nothing is
# handed to the scheduler until the new table is complete, so a failed read, a
# block that cannot be delimited, or a failed write leaves the crontab as it was
# and the reason is printed. Returns nonzero on any of those, after removing the
# temporary file.
_install_managed_cron_block() {  # <crontab command> <begin> <end> [<block line>...]
  local command="$1" begin="$2" end="$3" tmp
  shift 3
  tmp="$(mktemp 2>/dev/null)" \
    || { warn_err "could not create a temporary file to rebuild root's crontab; the crontab was left as it was"; return 1; }
  if ! _managed_cron_table "$command" "$begin" "$end" "$@" >"$tmp"; then
    rm -f -- "$tmp"
    warn_err "could not rebuild root's crontab; the crontab was left as it was"
    return 1
  fi
  if [[ $# -gt 0 && "$(tail -n 1 -- "$tmp")" != "$end" ]]; then
    rm -f -- "$tmp"
    warn_err "could not write the new root crontab completely; the crontab was left as it was"
    return 1
  fi
  if ! "$command" "$tmp"; then
    rm -f -- "$tmp"
    warn_err "could not hand the new root crontab to $command; check root's crontab with '$command -l' before scheduling again"
    return 1
  fi
  rm -f -- "$tmp"
}

# Install a root .service + .timer pair. Both files are written beside their
# places and moved into place only once both are complete, so a write that
# fails half-way (a full disk) leaves the units root's timer runs as they were
# rather than empty. The temporary names end in no unit suffix, so systemd
# never reads them. A symbolic link at either path — a masked unit, or one
# linked in from another tree — is refused instead: renaming over it would
# replace the link with a regular file and undo the operator's mask.
_install_unit_pair() {  # <unit dir> <service name> <timer name> <service body> <timer body>
  local unit_dir="$1" svc tmr svc_tmp tmr_tmp
  svc="$unit_dir/$2"
  tmr="$unit_dir/$3"
  mkdir -p -- "$unit_dir" 2>/dev/null \
    || { warn_err "cannot create $unit_dir; both units were left as they were"; return 1; }
  if [[ -L "$svc" || -L "$tmr" ]]; then
    warn_err "a unit under $unit_dir is a symbolic link (a masked or linked unit); unmask or remove it, then schedule again; both units were left as they were"
    return 1
  fi
  svc_tmp="$(mktemp "$svc.XXXXXX" 2>/dev/null)" \
    || { warn_err "could not create a temporary file beside $svc; both units were left as they were"; return 1; }
  if ! printf '%s' "$4" >"$svc_tmp"; then
    rm -f -- "$svc_tmp"
    warn_err "could not write $svc; both units were left as they were"
    return 1
  fi
  tmr_tmp="$(mktemp "$tmr.XXXXXX" 2>/dev/null)" \
    || { rm -f -- "$svc_tmp"; warn_err "could not create a temporary file beside $tmr; both units were left as they were"; return 1; }
  if ! printf '%s' "$5" >"$tmr_tmp"; then
    rm -f -- "$svc_tmp" "$tmr_tmp"
    warn_err "could not write $tmr; both units were left as they were"
    return 1
  fi
  if ! { chmod 0644 -- "$svc_tmp" "$tmr_tmp" \
      && mv -f -- "$svc_tmp" "$svc" && mv -f -- "$tmr_tmp" "$tmr"; }; then
    rm -f -- "$svc_tmp" "$tmr_tmp"
    warn_err "could not put both units in place under $unit_dir; compare $svc and $tmr before the next start"
    return 1
  fi
}
