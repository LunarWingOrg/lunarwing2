#!/usr/bin/env bash
# Render and install the LunarWing healthpoint metrics listener units for the
# detected init system (systemd / OpenRC / launchd), following the
# auto-detect pattern of install-lunarwing-watchdog.sh. The listener is
# OPTIONAL — the textfile exporter is the primary delivery mechanism and needs
# none of this.
#
# Usage:
#   sudo ./install-health-metrics-listener.sh          # install for detected init
#   ./install-health-metrics-listener.sh launchd       # explicit init
#   sudo ./install-health-metrics-listener.sh --uninstall
#
# The binary itself is NOT built here; install it first:
#   cargo build --release --bin lunarwing-health-metrics
#   install -m 0755 target/release/lunarwing-health-metrics \
#       /usr/local/lib/lunarwing-health/lunarwing-health-metrics

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IC_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

SYSTEMD_UNIT_DIR="/etc/systemd/system"
OPENRC_INIT_DIR="/etc/init.d"
OPENRC_CONFD="/etc/conf.d/lunarwing-health-metrics"
HEALTH_LIB_DIR="/usr/local/lib/lunarwing-health"
SYSTEMD_UNIT="lunarwing-health-metrics.service"
OPENRC_UNIT="lunarwing-health-metrics"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

UNINSTALL=false
INIT_OVERRIDE=""
for arg in "$@"; do
  case "$arg" in
    --uninstall) UNINSTALL=true ;;
    systemd|openrc|launchd) INIT_OVERRIDE="$arg" ;;
    *) die "unknown argument: $arg (use systemd|openrc|launchd|--uninstall)" ;;
  esac
done

detect_init() {
  if [[ -n "$INIT_OVERRIDE" ]]; then
    printf '%s' "$INIT_OVERRIDE"
    return 0
  fi
  [[ "$(uname -s)" == "Darwin" ]] && { printf 'launchd'; return 0; }
  [[ -e /run/openrc/softlevel ]] && { printf 'openrc'; return 0; }
  [[ -e /run/systemd/system ]] && { printf 'systemd'; return 0; }
  command -v rc-service >/dev/null 2>&1 && ! command -v systemctl >/dev/null 2>&1 && { printf 'openrc'; return 0; }
  command -v systemctl >/dev/null 2>&1 && { printf 'systemd'; return 0; }
  command -v rc-service >/dev/null 2>&1 && { printf 'openrc'; return 0; }
  printf 'unknown'
}

uninstall_units() {
  local init="$1"
  case "$init" in
    systemd)
      systemctl disable --now "$SYSTEMD_UNIT" 2>/dev/null || true
      rm -f "${SYSTEMD_UNIT_DIR}/${SYSTEMD_UNIT}"
      systemctl daemon-reload || true
      ;;
    openrc)
      rc-update del "$OPENRC_UNIT" default >/dev/null 2>&1 || true
      rc-service "$OPENRC_UNIT" stop >/dev/null 2>&1 || true
      rm -f "${OPENRC_INIT_DIR}/${OPENRC_UNIT}" "$OPENRC_CONFD"
      ;;
    launchd)
      launchctl unload "${HOME}/Library/LaunchAgents/com.lunarwing.health-metrics.plist" >/dev/null 2>&1 || true
      rm -f "${HOME}/Library/LaunchAgents/com.lunarwing.health-metrics.plist"
      ;;
    *) die "cannot uninstall: unknown init system" ;;
  esac
  say "uninstalled health metrics listener units ($init)"
}

install_systemd() {
  [[ -x "${HEALTH_LIB_DIR}/lunarwing-health-metrics" ]] \
    || die "binary missing at ${HEALTH_LIB_DIR}/lunarwing-health-metrics (see header for build+install)"
  install -o root -g root -m 0644 \
    "${IC_DIR}/systemd/${SYSTEMD_UNIT}" "${SYSTEMD_UNIT_DIR}/${SYSTEMD_UNIT}"
  systemctl daemon-reload
  systemctl enable --now "$SYSTEMD_UNIT"
  say "installed + started ${SYSTEMD_UNIT} (systemctl status for details)"
}

install_openrc() {
  [[ -x "${HEALTH_LIB_DIR}/lunarwing-health-metrics" ]] \
    || die "binary missing at ${HEALTH_LIB_DIR}/lunarwing-health-metrics (see header for build+install)"
  install -o root -g root -m 0755 \
    "${IC_DIR}/systemd/lunarwing-health-metrics.openrc" "${OPENRC_INIT_DIR}/${OPENRC_UNIT}"
  install -o root -g root -m 0644 \
    "${IC_DIR}/systemd/lunarwing-health-metrics.confd" "$OPENRC_CONFD"
  # conf.d values must be exported for openrc-run's supervise-daemon child.
  set -a
  # shellcheck disable=SC1090
  [[ -r "$OPENRC_CONFD" ]] && source "$OPENRC_CONFD"
  set +a
  rc-update add "$OPENRC_UNIT" default >/dev/null 2>&1 || true
  rc-service "$OPENRC_UNIT" restart >/dev/null 2>&1 || true
  say "installed + started OpenRC service ${OPENRC_UNIT} (config: ${OPENRC_CONFD})"
}

install_launchd() {
  local plist_dir="${HOME}/Library/LaunchAgents"
  local plist="${plist_dir}/com.lunarwing.health-metrics.plist"
  local listen="${HEALTH_METRICS_LISTEN:-127.0.0.1:50000}"
  local token="${HEALTH_METRICS_TOKEN:-}"
  local refresh="${HEALTH_METRICS_REFRESH_SECS:-15}"
  mkdir -p "$plist_dir"
  cat >"$plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.lunarwing.health-metrics</string>
  <key>ProgramArguments</key>
  <array>
    <string>${HEALTH_LIB_DIR}/lunarwing-health-metrics</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HEALTH_METRICS_LISTEN</key><string>${listen}</string>
    <key>HEALTH_METRICS_TOKEN</key><string>${token}</string>
    <key>HEALTH_METRICS_REFRESH_SECS</key><string>${refresh}</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/tmp/lunarwing-health-metrics.log</string>
  <key>StandardErrorPath</key><string>/tmp/lunarwing-health-metrics.log</string>
</dict>
</plist>
PLIST
  launchctl unload "$plist" >/dev/null 2>&1 || true
  launchctl load "$plist"
  say "installed + loaded ${plist}"
}

[[ "${1:-}" == "--uninstall" || "$UNINSTALL" == true ]] && { uninstall_units "$(detect_init)"; exit 0; }

# launchd installs are user-level; the others need root.
init="$(detect_init)"
[[ "$init" == "launchd" ]] || [[ $EUID -eq 0 ]] || die "must run as root (launchd excepted)"

case "$init" in
  systemd) install_systemd ;;
  openrc)  install_openrc ;;
  launchd) install_launchd ;;
  *) die "no supported init system detected; override with systemd|openrc|launchd" ;;
esac
