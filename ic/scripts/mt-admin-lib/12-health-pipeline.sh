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
      rc-update add "$cron" default >/dev/null 2>&1 || true
      rc-service "$cron" start >/dev/null 2>&1 || true
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

# WI#232 G2 — deployed-pipeline drift detection. The deployed copy under
# $HEALTH_LIB_DIR is a plain `cp -a` of the tree's ic-infrastructure-health-check/,
# so nothing tied the deployed bytes to the tree that produced them: an upgrade
# that skipped the refresh (WI#228's guard was broken; upgrade-tenant never ran
# the installer at all) left the host silently running stale self-heal code —
# the exact class of failure that masqueraded as podman store corruption on
# ajhermes (WI#225). The stamp closes that: a content hash of the pipeline
# scripts, written next to the deployed copy at install/refresh time, compared
# by the health checks against the tree they run from, surfaced as `degraded`
# with a visible issue (never silent).
#
# Hashed set: the executable pipeline surface only (top-level *.sh + tests/ is
# excluded — tests don't run from the deploy). Sorted for stable output across
# filesystems; sha256 via sha256sum with shasum/openssl fallbacks (same ladder
# as self-heal's _service_hash).
health_pipeline_tree_hash() {
  local dir="$1" digest
  [[ -d "$dir" ]] || return 1
  if command -v sha256sum >/dev/null 2>&1; then
    digest="$(find "$dir" -maxdepth 1 -type f -name '*.sh' -printf '%f\n' 2>/dev/null | sort \
      | while IFS= read -r f; do sha256sum "$dir/$f" 2>/dev/null || printf 'MISSING %s\n' "$f"; done \
      | sha256sum)"
  elif command -v shasum >/dev/null 2>&1; then
    digest="$(find "$dir" -maxdepth 1 -type f -name '*.sh' -printf '%f\n' 2>/dev/null | sort \
      | while IFS= read -r f; do shasum -a 256 "$dir/$f" 2>/dev/null || printf 'MISSING %s\n' "$f"; done \
      | shasum -a 256)"
  elif command -v openssl >/dev/null 2>&1; then
    digest="$(find "$dir" -maxdepth 1 -type f -name '*.sh' -printf '%f\n' 2>/dev/null | sort \
      | while IFS= read -r f; do openssl dgst -sha256 "$dir/$f" 2>/dev/null || printf 'MISSING %s\n' "$f"; done \
      | openssl dgst -sha256)"
  else
    return 1
  fi
  digest="${digest%% *}"
  [[ "$digest" =~ ^[[:xdigit:]]{16,}$ ]] || return 1
  printf '%s' "${digest,,}"
}

# Stamp the DEPLOYED copy (post-sync) with the hash of the SOURCE tree it was
# copied from. Written after cp -a so the stamp always matches what was just
# laid down; a stale/absent stamp (pre-WI#232 deploy) is itself reportable drift.
health_pipeline_stamp_tree() {
  local stamp
  stamp="$(health_pipeline_tree_hash "$HEALTH_SRC_DIR")" || {
    say "WARNING: could not hash health pipeline source ($HEALTH_SRC_DIR); skipping version stamp"
    return 0
  }
  printf '%s\n' "$stamp" > "$HEALTH_LIB_DIR/.pipeline-version"
  say "stamped pipeline version $stamp"
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

  # 1) Stable copy of the pipeline scripts (survives repo/worktree moves).
  mkdir -p "$HEALTH_LIB_DIR"
  cp -a "$HEALTH_SRC_DIR/." "$HEALTH_LIB_DIR/"
  chmod 0755 "$HEALTH_LIB_DIR"/*.sh 2>/dev/null || true
  say "synced pipeline scripts -> $HEALTH_LIB_DIR"

  # 1b) WI#232 G2: stamp the deployed copy with a content hash of the pipeline
  # scripts it was installed FROM, so drift (an upgrade that never refreshed the
  # deploy, a hand-edited /usr/local/lib file) becomes a visible health signal
  # instead of a latent defect. The health checks compare the stamp against the
  # tree they run from and degrade on mismatch (never silent).
  health_pipeline_stamp_tree

  # 2) Host-level report/state dir.
  mkdir -p "$HEALTH_BASE_DIR/workspace/reports/health"

  # 3) Config env file (write-if-absent so operator edits survive).
  mkdir -p /etc/lunarwing
  if [[ ! -f "$HEALTH_ENV_FILE" ]]; then
    ( umask 077
      cat >"$HEALTH_ENV_FILE" <<ENVEOF
# /etc/lunarwing/health.env — host-global health/self-heal pipeline config.
# Auto-generated by lunarwing-mt-admin.sh (write-if-absent; safe to edit).
LUNARWING_BASE_DIR=$HEALTH_BASE_DIR
LUNARWING_SERVICE_MANAGER=$INIT_SYSTEM
SELF_HEAL_TENANTS_FILE=$PORTS_REGISTRY

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
  fi

  # 4) Launcher: source env, then run the pipeline (health-check -> self-heal LIVE).
  cat >"$HEALTH_LAUNCHER" <<LAUNCHEOF
#!/bin/sh
# Auto-generated by lunarwing-mt-admin.sh. Host-global health/self-heal pipeline.
set -a
[ -r $HEALTH_ENV_FILE ] && . $HEALTH_ENV_FILE
set +a
exec $HEALTH_LIB_DIR/cron-wrapper.sh "\$@"
LAUNCHEOF
  chmod 0755 "$HEALTH_LAUNCHER"
  say "wrote $HEALTH_LAUNCHER"

  # 5) Schedule it (per init system).
  case "$INIT_SYSTEM" in
    openrc)  _install_health_cron ;;
    systemd) _install_health_systemd_timer ;;
  esac
  say "health pipeline ready (every ${HEALTH_INTERVAL_MIN} min; covers all tenants)"
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
