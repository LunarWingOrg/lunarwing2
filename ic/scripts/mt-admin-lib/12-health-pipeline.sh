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

_install_health_cron() {
  local sched="*/${HEALTH_INTERVAL_MIN} * * * * $HEALTH_LAUNCHER"
  local begin="# BEGIN lunarwing-mt-health managed block"
  local end="# END lunarwing-mt-health managed block"
  local cmd=""
  command -v fcrontab >/dev/null 2>&1 && cmd="fcrontab"
  [[ -z "$cmd" ]] && command -v crontab >/dev/null 2>&1 && cmd="crontab"
  if [[ -z "$cmd" ]]; then
    say "WARNING: no fcrontab/crontab found — cannot schedule the pipeline. Install a cron daemon or run $HEALTH_LAUNCHER periodically."
    return 0
  fi
  local tmp; tmp="$(mktemp)"
  "$cmd" -l 2>/dev/null | sed "/^${begin}$/,/^${end}$/d" > "$tmp" || true
  { printf '%s\n' "$begin" "$sched" "$end"; } >> "$tmp"
  if "$cmd" "$tmp" 2>/dev/null; then
    say "scheduled health pipeline via $cmd: every ${HEALTH_INTERVAL_MIN} min"
  else
    say "WARNING: failed to install $cmd schedule"
  fi
  rm -f "$tmp"
  _ensure_cron_runlevel "$([[ "$cmd" == "fcrontab" ]] && echo fcron)"
}

_install_health_systemd_timer() {
  # systemd hosts: a root, system-level oneshot service + timer. Persistent=true
  # provides the missed-run catch-up fcron gives on OpenRC; system-level (not
  # --user) because one run remediates many tenants' user units via sudo.
  local unit_dir="${LUNARWING_MT_SYSTEMD_UNIT_DIR:-/etc/systemd/system}"
  local svc="$unit_dir/lunarwing-mt-health.service"
  local tmr="$unit_dir/lunarwing-mt-health.timer"
  # Bound a single run: systemd's default TimeoutStartSec (~90s on many
  # distros) can kill a legitimate multi-tenant remediation mid-restart, while
  # a wedged tenant bus must never stall the pipeline forever. Floor of 900s,
  # otherwise one full interval; Persistent=true replays a killed run.
  local run_timeout=$(( HEALTH_INTERVAL_MIN * 60 > 900 ? HEALTH_INTERVAL_MIN * 60 : 900 ))
  if ! command -v systemctl >/dev/null 2>&1; then
    say "WARNING: systemctl not found — cannot schedule the pipeline on systemd."
    return 0
  fi
  cat >"$svc" <<UNITEOF
[Unit]
Description=LunarWing MT health-check + self-heal pipeline
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
TimeoutStartSec=$run_timeout
ExecStart=$HEALTH_LAUNCHER
UNITEOF
  cat >"$tmr" <<UNITEOF
[Unit]
Description=Schedule LunarWing MT health/self-heal pipeline (every ${HEALTH_INTERVAL_MIN} min)

[Timer]
OnBootSec=5min
OnCalendar=*:0/${HEALTH_INTERVAL_MIN}
Persistent=true
AccuracySec=30s
Unit=lunarwing-mt-health.service

[Install]
WantedBy=timers.target
UNITEOF
  systemctl daemon-reload >/dev/null 2>&1 || true
  if systemctl enable --now lunarwing-mt-health.timer >/dev/null 2>&1; then
    say "scheduled health pipeline via systemd timer: every ${HEALTH_INTERVAL_MIN} min"
  else
    say "WARNING: failed to enable lunarwing-mt-health.timer"
  fi
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

sync_health_pipeline_library() {
  local parent stage old="" unsafe
  [[ "$HEALTH_LIB_DIR" == /* && "$HEALTH_LIB_DIR" != / ]] \
    || die "health library destination must be an absolute non-root path"
  parent="$(dirname -- "$HEALTH_LIB_DIR")"
  mkdir -p "$parent"
  stage="$(mktemp -d "$parent/.lunarwing-health.new.XXXXXX")" \
    || die "could not create health library staging directory"
  if ! cp -R --no-preserve=ownership --preserve=mode,timestamps "$HEALTH_SRC_DIR/." "$stage/"; then
    rm -rf -- "$stage"
    die "could not copy health pipeline scripts"
  fi
  unsafe="$(find "$stage" -type l -print -quit 2>/dev/null)" \
    || { rm -rf -- "$stage"; die "could not inspect staged health pipeline scripts"; }
  if [[ -n "$unsafe" ]]; then
    rm -rf -- "$stage"
    die "health pipeline source contains a symbolic link: $unsafe"
  fi
  if ! chown -R -h root:root "$stage" || ! chmod -R go-w "$stage"; then
    rm -rf -- "$stage"
    die "could not secure staged health pipeline scripts"
  fi
  find "$stage" -maxdepth 1 -type f -name '*.sh' -exec chmod 0755 -- {} + \
    || { rm -rf -- "$stage"; die "could not set health pipeline script modes"; }
  if ! health_lib_tree_require_safe "$stage"; then
    rm -rf -- "$stage"
    die "staged health pipeline failed its ownership check"
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
  if ! mv -T -- "$stage" "$HEALTH_LIB_DIR"; then
    [[ -z "$old" ]] || mv -T -- "$old" "$HEALTH_LIB_DIR" || true
    rm -rf -- "$stage"
    die "could not publish staged health pipeline scripts"
  fi
  [[ -z "$old" ]] || rm -rf -- "$old"
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
  if [[ ! -f "$HEALTH_ENV_FILE" ]]; then
    ( umask 077
      cat >"$HEALTH_ENV_FILE" <<ENVEOF
# /etc/lunarwing/health.env — host-global health/self-heal pipeline config.
# Auto-generated by lunarwing-mt-admin.sh (write-if-absent; safe to edit).
LUNARWING_BASE_DIR=$HEALTH_BASE_DIR
LUNARWING_SERVICE_MANAGER=$INIT_SYSTEM
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
    # Operator edits (e.g. Gotify tokens) are preserved — we rewrite one line.
    if ! grep -q "^LUNARWING_SERVICE_MANAGER=${INIT_SYSTEM}$" "$HEALTH_ENV_FILE"; then
      local _tmp; _tmp="$(mktemp)"
      if grep -q '^LUNARWING_SERVICE_MANAGER=' "$HEALTH_ENV_FILE"; then
        awk -v v="$INIT_SYSTEM" '/^LUNARWING_SERVICE_MANAGER=/{print "LUNARWING_SERVICE_MANAGER=" v; next} {print}' "$HEALTH_ENV_FILE" > "$_tmp"
      else
        cp "$HEALTH_ENV_FILE" "$_tmp"
        printf 'LUNARWING_SERVICE_MANAGER=%s\n' "$INIT_SYSTEM" >> "$_tmp"
      fi
      cat "$_tmp" > "$HEALTH_ENV_FILE"   # overwrite content, preserve mode/owner
      rm -f "$_tmp"
      say "reconciled LUNARWING_SERVICE_MANAGER=$INIT_SYSTEM in $HEALTH_ENV_FILE"
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

  # 4) Launcher: validate the installed tree before sourcing env or running it.
  {
    printf '#!/bin/bash\n'
    declare -f health_lib_validate_root_owned_directory health_lib_tree_require_safe
    cat <<LAUNCHEOF
# Auto-generated by lunarwing-mt-admin.sh. Host-global health/self-heal pipeline.
PATH=/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin
export PATH
if ! health_lib_tree_require_safe "$HEALTH_LIB_DIR"; then
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

  # 5) Schedule it (per init system).
  case "$INIT_SYSTEM" in
    openrc)  _install_health_cron ;;
    systemd) _install_health_systemd_timer ;;
  esac
  say "health pipeline ready (every ${HEALTH_INTERVAL_MIN} min; covers all tenants)"
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
  case "$INIT_SYSTEM" in
    openrc)
      command -v fcrontab >/dev/null 2>&1 && fcrontab -l 2>/dev/null | sed "/^${begin}$/,/^${end}$/d" | fcrontab - 2>/dev/null || true
      command -v crontab  >/dev/null 2>&1 && crontab  -l 2>/dev/null | sed "/^${begin}$/,/^${end}$/d" | crontab  - 2>/dev/null || true
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
  rm -f "$HEALTH_LAUNCHER"
  say "retired host-global health pipeline schedule (no tenants remain)"
  # $HEALTH_LIB_DIR + $HEALTH_ENV_FILE left in place (harmless; preserves config/state).
}
