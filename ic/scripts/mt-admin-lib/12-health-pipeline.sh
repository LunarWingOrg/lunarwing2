#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 12-health-pipeline.sh — part of the lunarwing-mt-admin library.
#
# Host-global infrastructure health-check + self-heal pipeline installation
# (cron or systemd timer, init-agnostic).
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced.
#
# Extracted from the v2.0.8.0 monolith (goals item #9), keeping the library
# structure of ref2/sunburst/mtadmin-refactor-v2.0.6.0 per
# docs/proposals/MT-ADMIN-DECOMPOSITION.md. Pure line-range move: no function
# body was rewritten.


# ── Compound commands ────────────────────────────────────────────────────────

# WeeChat's ws_adapter.py needs the `aiohttp` Python package importable by the
# TENANT user's python3 (system-wide, or in that user's ~/.local — an --user
# install for a different account, e.g. the admin, is NOT visible). Non-fatal:
# the adapter is optional, so we only warn with how to fix it.
warn_if_adapter_deps_missing() {
  local name="$1"
  if ! command -v python3 >/dev/null 2>&1; then
    say "WARNING: python3 not found — the WeeChat adapter cannot run."
    return 0
  fi
  if sudo -u "$name" python3 -c 'import aiohttp' >/dev/null 2>&1; then
    return 0
  fi
  say ""
  say "WARNING: Python package 'aiohttp' is not importable by user '$name'."
  say "         lunarwing-weechat-adapter-${name} will exit on start until it is installed:"
  say "           system-wide (preferred): sudo pacman -S python-aiohttp"
  say "             (Debian: sudo apt install python3-aiohttp  ·  Fedora: sudo dnf install python3-aiohttp)"
  say "           per-tenant fallback:     sudo -u ${name} pip install --user --break-system-packages aiohttp"
  say ""
}

# ── Host-global health-check / self-heal pipeline (OpenRC + systemd) ─────────

_ensure_cron_runlevel() {
  # Ensure a cron daemon is enabled at boot + running so the schedule fires.
  local want="${1:-}" cron
  for cron in ${want:+$want} fcron cronie dcron crond busybox-cron; do
    if [[ -x "/etc/init.d/$cron" ]]; then
      _openrc_update add "$cron" default || return 1
      _openrc_service "$cron" start || return 1
      say "cron daemon '$cron' enabled + running"
      return 0
    fi
  done
  say "WARNING: no cron daemon init script found; install fcron/cronie/dcron so the schedule runs"
}

# Returns 1 when the schedule was not installed, and 2 when it is installed but
# the cron service could not be enabled and started. ensure_health_pipeline
# tells the two apart: after a 2 it must not say that the schedule is missing.
_install_health_cron() {
  local sched="*/${HEALTH_INTERVAL_MIN} * * * * $HEALTH_LAUNCHER"
  local begin="# BEGIN lunarwing-mt-health managed block"
  local end="# END lunarwing-mt-health managed block"
  local cmd=""
  command -v fcrontab >/dev/null 2>&1 && cmd="fcrontab"
  [[ -z "$cmd" ]] && command -v crontab >/dev/null 2>&1 && cmd="crontab"
  if [[ -z "$cmd" ]]; then
    say "WARNING: no fcrontab/crontab found — cannot schedule the pipeline. Install a cron daemon or run $HEALTH_LAUNCHER periodically."
    return 1
  fi
  # Root's whole crontab is replaced, so the new table is built and checked
  # first: an unreadable table, a managed block that cannot be delimited, or a
  # write that fails leaves every other job exactly as it was (WI#740).
  if ! _install_managed_cron_block "$cmd" "$begin" "$end" "$sched"; then
    say "WARNING: the health pipeline schedule was not installed; root's crontab was left as it was (reason above)"
    return 1
  fi
  say "scheduled health pipeline via $cmd: every ${HEALTH_INTERVAL_MIN} min"
  if ! _ensure_cron_runlevel "$([[ "$cmd" == "fcrontab" ]] && echo fcron)"; then
    say "WARNING: the health pipeline schedule is installed, but the cron service could not be enabled and started (reason above)"
    return 2
  fi
}

_install_health_systemd_timer() {
  # systemd hosts: a root, system-level oneshot service + timer. Persistent=true
  # provides the missed-run catch-up fcron gives on OpenRC; system-level (not
  # --user) because one run remediates many tenants' user units via sudo.
  local unit_dir="${LUNARWING_MT_SYSTEMD_UNIT_DIR:-/etc/systemd/system}"
  # Bound a single run: systemd's default TimeoutStartSec (~90s on many
  # distros) can kill a legitimate multi-tenant remediation mid-restart, while
  # a wedged tenant bus must never stall the pipeline forever. Floor of 900s,
  # otherwise one full interval; Persistent=true replays a killed run.
  local run_timeout=$(( HEALTH_INTERVAL_MIN * 60 > 900 ? HEALTH_INTERVAL_MIN * 60 : 900 ))
  local svc_body tmr_body
  if ! command -v systemctl >/dev/null 2>&1; then
    say "WARNING: systemctl not found — cannot schedule the pipeline on systemd."
    return 1
  fi
  # Both units are replaced only together and only once both are complete, so
  # a write that fails half-way (a full disk) leaves the units root's timer
  # runs as they were, and a unit an operator masked (a symbolic link) is not
  # silently replaced by a regular file (WI#740).
  printf -v svc_body '%s\n' \
    '[Unit]' \
    'Description=LunarWing MT health-check + self-heal pipeline' \
    'After=network-online.target' \
    'Wants=network-online.target' \
    '' \
    '[Service]' \
    'Type=oneshot' \
    "TimeoutStartSec=$run_timeout" \
    "ExecStart=$HEALTH_LAUNCHER"
  printf -v tmr_body '%s\n' \
    '[Unit]' \
    "Description=Schedule LunarWing MT health/self-heal pipeline (every ${HEALTH_INTERVAL_MIN} min)" \
    '' \
    '[Timer]' \
    'OnBootSec=5min' \
    "OnCalendar=*:0/${HEALTH_INTERVAL_MIN}" \
    'Persistent=true' \
    'AccuracySec=30s' \
    'Unit=lunarwing-mt-health.service' \
    '' \
    '[Install]' \
    'WantedBy=timers.target'
  if ! _install_unit_pair "$unit_dir" lunarwing-mt-health.service lunarwing-mt-health.timer \
    "$svc_body" "$tmr_body"; then
    say "WARNING: the health pipeline timer was not installed; its unit files were left as they were (reason above)"
    return 1
  fi
  if ! systemctl daemon-reload >/dev/null 2>&1; then
    say "WARNING: systemd was not reloaded after installing the health pipeline timer; the units are in place but the timer is not armed"
    return 1
  fi
  if ! systemctl enable --now lunarwing-mt-health.timer >/dev/null 2>&1; then
    say "WARNING: failed to enable lunarwing-mt-health.timer; the units are in place but the timer is not armed"
    return 1
  fi
  say "scheduled health pipeline via systemd timer: every ${HEALTH_INTERVAL_MIN} min"
}

# Reconcile one KEY=VALUE line in the host-global health env file, creating it
# if absent. The file is otherwise write-once so operator edits survive; a
# stale value here would silently misroute a root-side check, so managed keys
# are rewritten line-by-line (file mode/owner preserved).
#
# The key and value are interpolated into grep/awk patterns verbatim: callers
# must pass fixed key names and value tokens that are regex/ERE-safe (today
# podman/docker and true/false/auto). health.env holds notification
# credentials, so the scratch copy is created mode 0600 in the file's own
# directory and removed on every failure path; the swap into place is a single
# mv(2), so a step failing midway leaves the original intact rather than
# truncated (run-242 PW-1).
health_env_reconcile_line() {  # <KEY> <VALUE>
  local key="$1" value="$2" _tmp
  grep -q "^${key}=${value}$" "$HEALTH_ENV_FILE" && return 0
  # Same directory as the target so the mv stays a rename on one filesystem.
  # mktemp already creates it 0600; --reference restores the original's mode
  # (and owner, when this runs as root) so the credential file never widens.
  _tmp="$(mktemp "$(dirname "$HEALTH_ENV_FILE")/.health-env.XXXXXX")" || return 1
  if grep -q "^${key}=" "$HEALTH_ENV_FILE"; then
    awk -v k="$key" -v v="$value" 'index($0, k "=") == 1 {print k "=" v; next} {print}' \
      "$HEALTH_ENV_FILE" >"$_tmp" || { rm -f "$_tmp"; return 1; }
  else
    cp "$HEALTH_ENV_FILE" "$_tmp" || { rm -f "$_tmp"; return 1; }
    printf '%s=%s\n' "$key" "$value" >>"$_tmp" || { rm -f "$_tmp"; return 1; }
  fi
  chmod --reference="$HEALTH_ENV_FILE" "$_tmp" || { rm -f "$_tmp"; return 1; }
  # Best-effort: a non-root caller cannot chown, and mktemp already left the
  # scratch copy owned by whoever is reconciling the file.
  chown --reference="$HEALTH_ENV_FILE" "$_tmp" 2>/dev/null || true
  mv -f "$_tmp" "$HEALTH_ENV_FILE" || { rm -f "$_tmp"; return 1; }
  say "reconciled ${key}=${value} in $HEALTH_ENV_FILE"
}

# Keep the optional OpenRC listener on the same report directory as the
# exporter. The listener is installed separately, so an absent confd is not an
# error and must not cause us to install one implicitly.
health_metrics_confd_reconcile() {
  [[ "${INIT_SYSTEM:-}" == "openrc" ]] || return 0
  local confd="${OPENRC_CONFD_DIR:-/etc/conf.d}/lunarwing-health-metrics"
  [[ -f "$confd" ]] || return 0
  local desired tmp
  desired="export LUNARWING_BASE_DIR=\"$HEALTH_BASE_DIR\""
  tmp="$(mktemp "$(dirname -- "$confd")/.lunarwing-health-metrics.XXXXXX")" || return 1
  if ! HEALTH_METRICS_CONFD_DESIRED="$desired" awk '
    /^[[:space:]]*(export[[:space:]]+)?LUNARWING_BASE_DIR=/ {
      if (!seen) print ENVIRON["HEALTH_METRICS_CONFD_DESIRED"]
      seen = 1
      next
    }
    { print }
    END { if (!seen) print ENVIRON["HEALTH_METRICS_CONFD_DESIRED"] }
  ' "$confd" >"$tmp"; then
    rm -f "$tmp"
    return 1
  fi
  if cmp -s "$tmp" "$confd"; then
    rm -f "$tmp"
    return 0
  fi
  chmod --reference="$confd" "$tmp" || { rm -f "$tmp"; return 1; }
  chown --reference="$confd" "$tmp" 2>/dev/null || true
  mv -f "$tmp" "$confd" || { rm -f "$tmp"; return 1; }
  say "reconciled LUNARWING_BASE_DIR=$HEALTH_BASE_DIR in $confd"
}

# Entries that other installers put into the library and the checkout does
# not ship. The optional metrics listener binary is installed here by hand
# (ic-infrastructure-health-check/README.md, install-health-metrics-listener.sh)
# and its OpenRC service runs it as the unprivileged lunarwing-metrics account.
# A fresh tree would drop it, so it is
# carried over, but only out of a library that is root-owned and not
# group/other-writable through /, and only as a root-owned regular file
# without group/other write or set-ID bits. In any other state another
# account may have replaced it (a library from before WI#687 is owned by the
# checkout user), so it is moved to a root-only quarantine with a warning and
# the install goes on: stopping would leave that exposed library in place.
health_library_entry_unsafe_reason() {
  local src="$1" owner mode
  if ! health_lib_validate_root_owned_directory "$HEALTH_LIB_DIR"; then
    printf 'the installed library or a parent directory is not root-owned or is group/other-writable (an install from before WI#687)'
  elif [[ -L "$src" ]]; then
    printf 'it is a symbolic link'
  elif [[ ! -f "$src" ]]; then
    printf 'it is not a regular file'
  else
    owner="$(stat -c %u "$src" 2>/dev/null || true)"
    mode="$(stat -c %a "$src" 2>/dev/null || true)"
    if [[ "$owner" != 0 ]]; then
      printf 'it is not owned by root'
    elif [[ ! "$mode" =~ ^[0-7]+$ ]]; then
      printf 'its mode could not be read'
    elif (( (8#$mode & 0022) != 0 )); then
      printf 'it is group/other-writable'
    elif (( (8#$mode & 06000) != 0 )); then
      printf 'it has a set-ID bit'
    fi
  fi
}

# Move an untrusted library entry into a fresh directory under the root-only
# quarantine and print its new path. Fails, leaving the entry where it is,
# when the quarantine is not a root-owned 0700 directory with safe parents.
quarantine_health_library_entry() {
  local src="$1" entry="$2" qroot="$HEALTH_BASE_DIR/quarantine" qdir
  # Check an existing path before touching it: never chmod a link's target
  # or a directory another account owns.
  if [[ ! -e "$qroot" && ! -L "$qroot" ]]; then
    install -d -m 0700 "$qroot" 2>/dev/null || return 1
  fi
  health_lib_validate_root_owned_directory "$qroot" || return 1
  chmod 0700 "$qroot" 2>/dev/null || return 1
  qdir="$(mktemp -d "$qroot/$entry.XXXXXX" 2>/dev/null)" || return 1
  mv -- "$src" "$qdir/$entry" 2>/dev/null || return 1
  printf '%s\n' "$qdir/$entry"
}

carry_health_library_entries() {
  local stage="$1" entry src reason moved
  # A linked library is moved aside as a link; never reach through it.
  [[ -d "$HEALTH_LIB_DIR" && ! -L "$HEALTH_LIB_DIR" ]] || return 0
  for entry in "${HEALTH_LIB_CARRIED_ENTRIES[@]}"; do
    src="$HEALTH_LIB_DIR/$entry"
    [[ -e "$src" || -L "$src" ]] || continue
    if [[ -e "$stage/$entry" || -L "$stage/$entry" ]]; then
      say "health library: the checkout ships $entry; it replaces the installed copy"
      continue
    fi
    reason="$(health_library_entry_unsafe_reason "$src")"
    if [[ -z "$reason" ]]; then
      cp -p -- "$src" "$stage/$entry" || return 1
      continue
    fi
    # Never stop here: the install that follows is what repairs the library.
    if moved="$(quarantine_health_library_entry "$src" "$entry")"; then
      printf 'WARNING: health library: %s was not carried into the new library because %s; it was moved to %s.' \
        "$src" "$reason" "$moved" >&2
    else
      printf 'WARNING: health library: %s was not carried into the new library because %s, and no root-only quarantine could be made under %s; it stays in the old library, which this install replaces and removes.' \
        "$src" "$reason" "$HEALTH_BASE_DIR/quarantine" >&2
    fi
    printf ' Stop the metrics listener now if it is running, and reinstall it from a trusted build (sudo install -m 0755 <binary> %s) before starting it again.\n' \
      "$src" >&2
  done
}

# Leaf checks below print the precise reason and return non-zero. The caller
# removes the staging directory and dies once, so every refusal leaves nothing
# behind in the library's parent.
health_pipeline_tree_require_regular() {  # <path>
  local path="$1" unsafe
  unsafe="$(find "$path" -type l -print -quit 2>/dev/null)" || {
    printf 'error: could not inspect health pipeline source entries\n' >&2
    return 1
  }
  if [[ -n "$unsafe" ]]; then
    printf 'error: health pipeline source contains a symbolic link: %s\n' "$unsafe" >&2
    return 1
  fi
  unsafe="$(find "$path" ! \( -type f -o -type d \) -print -quit 2>/dev/null)" || {
    printf 'error: could not inspect health pipeline source file types\n' >&2
    return 1
  }
  if [[ -n "$unsafe" ]]; then
    printf 'error: health pipeline source contains a non-regular entry: %s\n' "$unsafe" >&2
    return 1
  fi
  return 0
}

# Modes by rule, not from the checkout. chmod 0755 leaves an inherited
# set-group-ID bit on a directory in place (GNU coreutils does not clear it
# unless asked), so the directory rule clears it explicitly and a stage made
# under a set-group-ID parent publishes a library without the bit.
secure_health_pipeline_stage() {  # <stage>
  local tree="$1/tree"
  chown -R -h root:root "$tree" || {
    printf 'error: could not own staged health pipeline scripts as root\n' >&2
    return 1
  }
  find "$tree" -type d -exec chmod 0755 -- {} + || {
    printf 'error: could not set health pipeline directory modes\n' >&2
    return 1
  }
  find "$tree" -type d -exec chmod u-s,g-s -- {} + || {
    printf 'error: could not clear set-ID bits from health pipeline directories\n' >&2
    return 1
  }
  find "$tree" -type f -exec chmod 0644 -- {} + || {
    printf 'error: could not set health pipeline file modes\n' >&2
    return 1
  }
  find "$tree" -maxdepth 1 -type f -name '*.sh' -exec chmod 0755 -- {} + || {
    printf 'error: could not set health pipeline script modes\n' >&2
    return 1
  }
  return 0
}

# Every file the library runs must be executable after staging, including a
# listener binary the checkout ships: the checkouts are made 0644 by the rule
# above and a 0644 listener cannot start. Carried entries arrive after that
# rule, so this runs after the carry and covers both sources.
health_pipeline_stage_mark_executables() {  # <tree>
  local tree="$1" entry
  for entry in "${HEALTH_LIB_CARRIED_ENTRIES[@]}"; do
    [[ -e "$tree/$entry" ]] || continue
    chmod 0755 -- "$tree/$entry" || {
      printf 'error: could not make health library entry executable: %s\n' "$tree/$entry" >&2
      return 1
    }
  done
  return 0
}

publish_health_pipeline_library() {
  local stage="$1" parent="$2" old="" mv_help=""
  # Capture the help text instead of piping it into grep -q: grep exits on the
  # first match, and under `set -o pipefail` the SIGPIPE that gives mv would
  # read as "no --exchange" on a host that has it.
  mv_help="$(mv --help 2>/dev/null || true)"
  if [[ -e "$HEALTH_LIB_DIR" || -L "$HEALTH_LIB_DIR" ]]; then
    if [[ "$mv_help" != *--exchange* ]]; then
      printf 'health pipeline: this mv has no --exchange; using the rollback-protected replacement\n' >&2
    elif mv -T --exchange -- "$stage/tree" "$HEALTH_LIB_DIR"; then
      rm -rf -- "$stage"
      return 0
    else
      printf 'WARNING: atomic health library exchange failed; using the rollback-protected replacement\n' >&2
    fi
  fi
  if [[ -e "$HEALTH_LIB_DIR" || -L "$HEALTH_LIB_DIR" ]]; then
    old="$(mktemp -d "$parent/.lunarwing-health.old.XXXXXX")" \
      || { rm -rf -- "$stage"; die "could not reserve health library backup path"; }
    rmdir "$old" || { rm -rf -- "$stage"; die "could not prepare health library backup path"; }
    if ! mv -T -- "$HEALTH_LIB_DIR" "$old"; then
      rm -rf -- "$stage"
      die "could not move the previous health library aside"
    fi
  fi
  if ! mv -T -- "$stage/tree" "$HEALTH_LIB_DIR"; then
    [[ -z "$old" ]] || mv -T -- "$old" "$HEALTH_LIB_DIR" || true
    rm -rf -- "$stage"
    die "could not publish staged health pipeline scripts"
  fi
  rm -rf -- "$stage"
  [[ -z "$old" ]] || rm -rf -- "$old"
}

reinstall_health_library() {
  local checkout="${1:-}" source_dir
  [[ -n "$checkout" ]] || die "usage: reinstall-health-library <trusted-checkout>"
  source_dir="$checkout/ic-infrastructure-health-check"
  [[ -d "$source_dir" ]] || die "trusted checkout has no health source directory: $source_dir"
  [[ -f "$source_dir/openrc-proxy-unit-contract.sh" ]] \
    || die "trusted checkout has no OpenRC proxy unit contract: $source_dir/openrc-proxy-unit-contract.sh"
  HEALTH_SRC_DIR="$source_dir"
  sync_health_pipeline_library
  say "reinstalled health library from trusted checkout $checkout -> $HEALTH_LIB_DIR"
}

sync_health_pipeline_library() {
  local parent stage
  # A source that is the installed library must pass its check first: the
  # install would otherwise publish the refused bytes as a root-owned library.
  require_safe_installed_health_source installed-only \
    || die "refusing to install from the unsafe installed health library; reinstall from a trusted checkout with: ic/scripts/lunarwing-mt-admin.sh reinstall-health-library <trusted-checkout>"
  [[ "$HEALTH_LIB_DIR" == /* && "$HEALTH_LIB_DIR" != / ]] \
    || die "health library destination must be an absolute non-root path"
  [[ -d "$HEALTH_SRC_DIR" && ! -L "$HEALTH_SRC_DIR" ]] \
    || die "health pipeline source must be a real directory, not a symbolic link: $HEALTH_SRC_DIR"
  health_pipeline_tree_require_regular "$HEALTH_SRC_DIR" \
    || die "health pipeline source is refused: correct the entry named above"
  # A source the load guard refuses (WI#725) must not become root's library
  # either. The load no longer ends mt-admin on a refusal (WI#722), and on a
  # systemd host the command goes on, so the install asks again.
  require_safe_installed_health_source \
    || die "refusing to install from a refused health contract source; the reason is printed above"
  parent="$(dirname -- "$HEALTH_LIB_DIR")"
  mkdir -p "$parent"
  stage="$(mktemp -d "$parent/.lunarwing-health.new.XXXXXX")" \
    || die "could not create health library staging directory"
  if ! (umask 077 && mkdir "$stage/tree" \
    && cp -R --no-preserve=all --preserve=timestamps "$HEALTH_SRC_DIR/." "$stage/tree/"); then
    rm -rf -- "$stage"
    die "could not copy health pipeline scripts into a root-only stage"
  fi
  if ! health_pipeline_tree_require_regular "$stage/tree"; then
    rm -rf -- "$stage"
    die "health pipeline source changed while it was copied: the staged copy is refused"
  fi
  if ! secure_health_pipeline_stage "$stage"; then
    rm -rf -- "$stage"
    die "could not secure the staged health pipeline tree"
  fi
  if ! carry_health_library_entries "$stage/tree"; then
    rm -rf -- "$stage"
    die "could not carry installed entries into the new health library"
  fi
  if ! health_pipeline_stage_mark_executables "$stage/tree"; then
    rm -rf -- "$stage"
    die "could not make carried health library entries executable"
  fi
  if ! health_lib_tree_require_safe "$stage/tree"; then
    rm -rf -- "$stage"
    die "staged health pipeline failed its ownership check"
  fi
  publish_health_pipeline_library "$stage" "$parent"
}

ensure_health_pipeline() {
  # Idempotently install + schedule the host-global health-check -> self-heal
  # pipeline. Auto-discovers all tenants, so one install covers every tenant.
  # Wired for OpenRC (fcron/cron) and systemd (timer); other inits are skipped.
  ensure_init_system
  case "$INIT_SYSTEM" in
    openrc|systemd) : ;;
    *) say "health pipeline: not wired for INIT_SYSTEM=$INIT_SYSTEM; skipping"; return 0 ;;
  esac
  [[ -d "$HEALTH_SRC_DIR" ]] || { say "WARNING: health source dir not found ($HEALTH_SRC_DIR); skipping"; return 0; }

  say "--- Ensuring host-global health/self-heal pipeline ---"
  say "health interval: every ${HEALTH_INTERVAL_MIN} min (source: ${HEALTH_INTERVAL_SOURCE:-unset})"
  # OpenRC disables Podman's timer healthchecks, so the cadence only applies
  # here. ensure_init_system above resolved it.
  if [[ "$INIT_SYSTEM" == "systemd" && -n "${CONTAINER_HEALTH_INTERVAL_SEC:-}" ]]; then
    say "container healthcheck interval: every ${CONTAINER_HEALTH_INTERVAL_SEC}s (source: ${CONTAINER_HEALTH_INTERVAL_SOURCE:-unset})"
  fi

  # 1) Stable copy of the pipeline scripts (survives repo/worktree moves).
  sync_health_pipeline_library
  say "synced pipeline scripts -> $HEALTH_LIB_DIR"

  # 2) Host-level report/state dir.
  mkdir -p "$HEALTH_BASE_DIR/workspace/reports/health"

  # 3) Config env file (write-if-absent so operator edits survive).
  mkdir -p "$(dirname -- "$HEALTH_ENV_FILE")"
  local egress_runtime="auto" egress_rootless="auto" _egress
  # Subshell: ensure_container_runtime dies when no runtime is installed, and
  # that must not abort a health-only install.
  if _egress="$( ensure_container_runtime >/dev/null 2>&1 \
        && printf '%s %s' "$CONTAINER_RT" "$MT_ROOTLESS" )" \
     && [[ -n "$_egress" ]]; then
    egress_runtime="${_egress%% *}"
    egress_rootless="${_egress##* }"
  fi
  # WI#730: record only the manager the host actually runs. Under an explicit
  # force (image build/chroot on a contradicting host) INIT_SYSTEM is the forced
  # value, but the health pipeline must never be pointed at the other manager —
  # that silently drops every existing tenant's unit checks.
  local service_manager
  service_manager="$(health_service_manager_of_record)"
  if [[ ! -f "$HEALTH_ENV_FILE" ]]; then
    ( umask 077
      cat >"$HEALTH_ENV_FILE" <<ENVEOF
# /etc/lunarwing/health.env — host-global health/self-heal pipeline config.
# Auto-generated by lunarwing-mt-admin.sh (write-if-absent; safe to edit).
LUNARWING_BASE_DIR=$HEALTH_BASE_DIR
LUNARWING_SERVICE_MANAGER=$service_manager
# Persistent schedule cadence. Use a plain, unquoted NAME=value line with no
# inline comment. The same-named environment variable overrides this line for
# one-off mt-admin runs and is not written back to this file.
LUNARWING_MT_HEALTH_INTERVAL_MIN=15
# Podman healthcheck cadence for tenant containers, in seconds (10-3600; the
# default is 60). Uncomment to change it, using the same plain NAME=value form;
# containers pick it up when mt-admin next recreates them.
#LUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC=60
SELF_HEAL_TENANTS_FILE=$PORTS_REGISTRY
# WI#424 worker egress audit: the health lib performs its own read-only
# root-owned audit (health-worker-egress.sh). Root cron must never execute or
# source an operator checkout (2026-10-15 ruling), so no LUNARWING_MT_ADMIN is
# recorded here.
#
# The audit must read the SAME launch mode mt-admin provisions with: on a
# rootful host (docker, or LUNARWING_MT_ROOTLESS=false) workers live in the
# host network namespace, so guessing "rootless whenever podman exists" audits
# the wrong netns and reports every running tenant critical (run-232 PB-1).
# Both lines are reconciled on every install/upgrade.
HEALTH_WORKER_EGRESS_RUNTIME=${egress_runtime}
HEALTH_WORKER_EGRESS_ROOTLESS=${egress_rootless}

# Disk capacity monitoring covers blocks and inodes. Percent thresholds apply
# to both; free-space floors are GiB. Values are base-10 integers; invalid or
# reversed degraded/critical pairs fall back to these defaults. Monitoring is
# read-only and self-heal only escalates it.
#HEALTH_DISK_DEGRADED_USED_PCT=80
#HEALTH_DISK_CRITICAL_USED_PCT=90
#HEALTH_DISK_DEGRADED_FREE_GIB=5
#HEALTH_DISK_CRITICAL_FREE_GIB=2
# Bound each probe and the whole fleet sweep below the outer 30-second timeout.
#HEALTH_DISK_TIMEOUT=3
#HEALTH_DISK_SWEEP_DEADLINE_SECS=19
# Add whitespace-separated operator paths (for example, a colocated GitLab
# runner's rootless container store); paths containing spaces are unsupported.
# Explicit paths must exist. Discovered stores may be absent; filesystems
# without inode accounting still have their block capacity checked.
#HEALTH_DISK_EXTRA_PATHS=
#HEALTH_DISK_DOCKER_ROOT=/var/lib/docker

# MT hardening: remediate only auto-discovered per-tenant init units; disable
# checks that are N/A host-globally; page only on self-heal escalation (not on
# every non-healthy run); ignore stale reports.
SELF_HEAL_REMEDY_LOGICAL=false
SELF_HEAL_SYSTEMD_SCOPE=system
HEALTH_XMPP_SERVER=
HEALTH_MODELS_ENABLED=false
HEALTH_OMEMO_ENABLED=false
HEALTH_NETBIRD_ENABLED=false
HEALTHCHECK_NOTIFY=false
SELF_HEAL_MAX_REPORT_AGE=$(( HEALTH_INTERVAL_MIN * 60 * 4 ))

# LunarVision: leave HEALTH_LUNARVISION_URL unset so health-lunarvision.sh
# auto-discovers every tenant's vision_health port from the ports registry
# (single-node still falls back to http://127.0.0.1:8089). Only set the URL
# if you intentionally force a single override target.
# HEALTH_LUNARVISION_URL=

# LunarVoice: leave HEALTH_LUNARVOICE_URL unset. The check discovers only
# tenants explicitly enabled in the registry and reports healthy/disabled when
# none are enabled, so provisioned-but-disabled sidecars never false-page.
# HEALTH_LUNARVOICE_URL=

# NetBird is operator-owned and disabled by default. To monitor readiness, set
# HEALTH_NETBIRD_ENABLED=true. Automatic daemon restart also requires explicit
# SELF_HEAL_NETBIRD_REMEDIATE=true; otherwise failures only escalate.
# SELF_HEAL_NETBIRD_REMEDIATE=false

# Escalation notifications (fill in to enable Gotify pushes).
GOTIFY_URL=$HEALTH_GOTIFY_URL
GOTIFY_TOKEN=$HEALTH_GOTIFY_TOKEN
ENVEOF
    )
    chmod 0600 "$HEALTH_ENV_FILE"
    say "wrote $HEALTH_ENV_FILE (mode 0600)"
  else
    say "$HEALTH_ENV_FILE already exists (preserving)"
    # Reconcile only the service-manager line in case a prior run wrote a
    # different init system (self-heal honors LUNARWING_SERVICE_MANAGER ahead of
    # its own autodetection, so a stale value silently misroutes remediation).
    # WI#730: the value is the host probe's manager, never a forced INIT_SYSTEM.
    # Operator edits (e.g. Gotify tokens) are preserved — we rewrite one line.
    if ! grep -q "^LUNARWING_SERVICE_MANAGER=${service_manager}$" "$HEALTH_ENV_FILE"; then
      local _tmp; _tmp="$(mktemp)"
      if grep -q '^LUNARWING_SERVICE_MANAGER=' "$HEALTH_ENV_FILE"; then
        awk -v v="$service_manager" '/^LUNARWING_SERVICE_MANAGER=/{print "LUNARWING_SERVICE_MANAGER=" v; next} {print}' "$HEALTH_ENV_FILE" > "$_tmp"
      else
        cp "$HEALTH_ENV_FILE" "$_tmp"
        printf 'LUNARWING_SERVICE_MANAGER=%s\n' "$service_manager" >> "$_tmp"
      fi
      cat "$_tmp" > "$HEALTH_ENV_FILE"   # overwrite content, preserve mode/owner
      rm -f "$_tmp"
      say "reconciled LUNARWING_SERVICE_MANAGER=$service_manager in $HEALTH_ENV_FILE"
    fi
    # Reconcile the tenants-file line with the ACTIVE ports registry
    # (ISSUE-012): the file is write-once, so if PORTS_REGISTRY moved after
    # first install, health kept reading the old registry while admin
    # commands updated the new one. Same targeted-line rewrite as above;
    # operator edits and file mode/owner are preserved.
    if ! grep -q "^SELF_HEAL_TENANTS_FILE=${PORTS_REGISTRY}$" "$HEALTH_ENV_FILE"; then
      local _tmp2; _tmp2="$(mktemp)"
      if grep -q '^SELF_HEAL_TENANTS_FILE=' "$HEALTH_ENV_FILE"; then
        awk -v v="$PORTS_REGISTRY" '/^SELF_HEAL_TENANTS_FILE=/{print "SELF_HEAL_TENANTS_FILE=" v; next} {print}' "$HEALTH_ENV_FILE" > "$_tmp2"
      else
        cp "$HEALTH_ENV_FILE" "$_tmp2"
        printf 'SELF_HEAL_TENANTS_FILE=%s\n' "$PORTS_REGISTRY" >> "$_tmp2"
      fi
      cat "$_tmp2" > "$HEALTH_ENV_FILE"   # overwrite content, preserve mode/owner
      rm -f "$_tmp2"
      say "reconciled SELF_HEAL_TENANTS_FILE=$PORTS_REGISTRY in $HEALTH_ENV_FILE"
    fi
    # WI#424 (run-226 OQ-1): root cron must not exec an operator-checkout path.
    # Drop the previously recorded LUNARWING_MT_ADMIN so upgrades stop handing
    # the health lib a user-owned mt-admin to run on every tick.
    if grep -q '^LUNARWING_MT_ADMIN=' "$HEALTH_ENV_FILE"; then
      local _tmp3; _tmp3="$(mktemp)"
      grep -v '^LUNARWING_MT_ADMIN=' "$HEALTH_ENV_FILE" >"$_tmp3" || true
      cat "$_tmp3" >"$HEALTH_ENV_FILE"
      rm -f "$_tmp3"
      say "removed LUNARWING_MT_ADMIN from $HEALTH_ENV_FILE (root cron must not run an operator checkout)"
    fi
  fi

  # WI#424 (run-232 PB-1): record the launch mode the audit must observe, on
  # both the fresh-install and the upgrade path, so a rootful-podman host does
  # not get rootless-netns audits. Reconciled separately from the write-once
  # block because a fresh install already wrote the values above.
  health_env_reconcile_line HEALTH_WORKER_EGRESS_RUNTIME "$egress_runtime"
  health_env_reconcile_line HEALTH_WORKER_EGRESS_ROOTLESS "$egress_rootless"
  if [[ "$INIT_SYSTEM" == "openrc" ]]; then
    health_metrics_confd_reconcile
  fi

  # 4) Launcher: validate the installed tree before sourcing env or running it.
  {
    printf '#!/bin/bash\n'
    declare -f health_lib_validate_root_owned_directory health_lib_tree_require_safe health_lib_refusal_text
    cat <<LAUNCHEOF
# Auto-generated by lunarwing-mt-admin.sh. Host-global health/self-heal pipeline.
PATH=/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin
export PATH
HEALTH_LIB_STAT=/usr/bin/stat
HEALTH_LIB_FIND=/usr/bin/find
HEALTH_LIB_LOGGER=/usr/bin/logger
health_refusal=""
if ! health_refusal="\$(health_lib_tree_require_safe "$HEALTH_LIB_DIR" 2>&1)"; then
  printf '%s\n' "\$health_refusal" >&2
  "\$HEALTH_LIB_LOGGER" -p daemon.err -t lunarwing-mt-health -- "\$health_refusal" >/dev/null 2>&1 || true
  exit 126
fi
set -a
[ -r "$HEALTH_ENV_FILE" ] && . "$HEALTH_ENV_FILE"
set +a
exec "$HEALTH_LIB_DIR/cron-wrapper.sh" "\$@"
LAUNCHEOF
  } >"$HEALTH_LAUNCHER"
  chmod 0755 "$HEALTH_LAUNCHER"
  say "wrote $HEALTH_LAUNCHER"

  # 5) Schedule it (per init system). The schedule replaces files root's
  # scheduler runs, so a refusal or a failed write leaves the previous schedule
  # as it was. That is reported here and is not fatal: the library and the
  # launcher are already in place, and an operator who masked the unit or left
  # a block unrepairable must still be able to add or upgrade tenants (WI#740).
  local sched_rc=0
  case "$INIT_SYSTEM" in
    openrc)  _install_health_cron || sched_rc=$? ;;
    systemd) _install_health_systemd_timer || sched_rc=$? ;;
  esac
  if (( sched_rc == 0 )); then
    say "health pipeline ready (every ${HEALTH_INTERVAL_MIN} min; covers all tenants)"
  elif (( sched_rc == 2 )); then
    # Only _install_health_cron returns 2: root's crontab holds the block, and
    # the cron service is what could not be enabled and started.
    say "WARNING: the health pipeline files and schedule are installed, but the schedule may NOT be running; repair the reason above and rerun add-tenant or upgrade-tenant to enable and start the cron service"
  else
    say "WARNING: the health pipeline files are installed, but the schedule is NOT; repair the reason above and rerun add-tenant or upgrade-tenant to schedule it"
  fi
}

# Upgrade only an already-installed host-global pipeline. Removal leaves the
# script copy behind, so the managed launcher (removed by remove_health_pipeline)
# is the install marker; a leftover copy must not re-arm the schedule.
refresh_deployed_health_pipeline() {
  if [[ ! -f "$HEALTH_LAUNCHER" ]]; then
    say "health pipeline: not installed; skipping upgrade refresh"
    return 0
  fi
  ensure_health_pipeline
}

remove_health_pipeline() {
  ensure_init_system
  local begin="# BEGIN lunarwing-mt-health managed block"
  local end="# END lunarwing-mt-health managed block"
  # A block that could not be removed still runs the launcher, so the launcher
  # is then kept: root's cron entry must never point at a file that is gone.
  local retire_launcher=1
  case "$INIT_SYSTEM" in
    openrc)
      # The block is removed from the table of every cron tool that exists,
      # not only from the one the installer would pick today: a host that was
      # scheduled while only crontab existed and got fcron later still holds
      # the block in crontab's table. Removal rewrites a table too, so each
      # one goes through the same checked path: a table that cannot be read,
      # or a block that cannot be delimited, leaves root's other jobs as they
      # were (WI#740). One table that keeps its block keeps the launcher.
      local cmd
      for cmd in fcrontab crontab; do
        command -v "$cmd" >/dev/null 2>&1 || continue
        if ! _install_managed_cron_block "$cmd" "$begin" "$end"; then
          say "WARNING: could not remove the health pipeline block from root's $cmd table; its other jobs, its schedule, and the launcher were left as they were"
          retire_launcher=0
        fi
      done
      ;;
    systemd)
      if command -v systemctl >/dev/null 2>&1; then
        local unit_dir="${LUNARWING_MT_SYSTEMD_UNIT_DIR:-/etc/systemd/system}"
        systemctl disable --now lunarwing-mt-health.timer >/dev/null 2>&1 || true
        rm -f "$unit_dir/lunarwing-mt-health.timer" "$unit_dir/lunarwing-mt-health.service"
        systemctl daemon-reload >/dev/null 2>&1 || true
      fi
      ;;
    *) return 0 ;;
  esac
  if (( retire_launcher )); then
    rm -f -- "$HEALTH_LAUNCHER"
    say "retired host-global health pipeline schedule (no tenants remain)"
  fi
  # $HEALTH_LIB_DIR + $HEALTH_ENV_FILE left in place (harmless; preserves config/state).
}
