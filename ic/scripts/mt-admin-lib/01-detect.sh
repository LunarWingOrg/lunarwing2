#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 01-detect.sh — part of the lunarwing-mt-admin library.
#
# Init system detection (systemd user manager vs OpenRC) and container runtime
# detection/persistence (podman/docker, quadlet support).
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced.
#
# Extracted from the v2.0.8.0 monolith (goals item #9), keeping the library
# structure of ref2/sunburst/mtadmin-refactor-v2.0.6.0 per
# docs/proposals/MT-ADMIN-DECOMPOSITION.md. Pure line-range move: no function
# body was rewritten.


# ── Init system detection ────────────────────────────────────────────────────

# PID-1 artifacts that name the host's real service manager. Harness tests may
# point these at fixtures; production always reads the /run paths.
OPENRC_HOST_MARKER="${LUNARWING_MT_OPENRC_MARKER:-/run/openrc/softlevel}"
SYSTEMD_HOST_MARKER="${LUNARWING_MT_SYSTEMD_MARKER:-/run/systemd/system}"

# Print the manager the host's init really runs, from the PID-1 markers only, or
# nothing when neither exists (chroot, image build, container). This is
# deliberately narrower than the command-existence ladder below: a chroot can
# carry systemctl/rc-service without any init running.
probe_host_service_manager() {
  if [[ -e "$OPENRC_HOST_MARKER" ]]; then printf 'openrc'; return 0; fi
  if [[ -e "$SYSTEMD_HOST_MARKER" ]]; then printf 'systemd'; return 0; fi
  return 0
}

_manager_probe_path() {
  case "$1" in
    openrc)  printf '%s' "$OPENRC_HOST_MARKER" ;;
    systemd) printf '%s' "$SYSTEMD_HOST_MARKER" ;;
  esac
}

# True when the port registry holds a tenant whose units are installed under
# <manager>. Only the forcing switch consults this: forcing a manager must not
# be available on a host whose registered tenants are managed by the other one
# (WI#730 — the health pipeline would silently stop checking them).
_registered_tenants_under_manager() {
  local manager="$1" registry="${PORTS_REGISTRY:-}" name tenants
  [[ -n "$registry" && -f "$registry" ]] || return 1
  # Fail closed: an unreadable registry is treated as tenants present, so a
  # broken jq can never unlock the forcing switch.
  tenants="$(jq -r '.tenants | keys[]?' "$registry" 2>/dev/null)" || return 0
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    case "$manager" in
      systemd)
        [[ -e "$(tenant_home "$name")/.config/systemd/user/lunarwing-${name}.service" ]] && return 0
        ;;
      openrc)
        [[ -e "$OPENRC_INITD_DIR/lunarwing-${name}" ]] && return 0
        ;;
    esac
  done <<< "$tenants"
  return 1
}

# The manager the host's own health pipeline may record. A positive host probe
# always wins, even under LUNARWING_SERVICE_MANAGER_FORCE, so no tenant command
# can point the pipeline at a manager the host does not run (WI#730).
health_service_manager_of_record() {
  local probed
  probed="$(probe_host_service_manager)"
  printf '%s' "${probed:-$INIT_SYSTEM}"
}

detect_init_system() {
  local override="${LUNARWING_SERVICE_MANAGER:-}" wanted="" detected=""
  if [[ -n "$override" ]]; then
    case "${override,,}" in
      systemd|systemd-user) wanted="systemd" ;;
      openrc)               wanted="openrc" ;;
      *) die "unsupported service manager override '$override'; use systemd or openrc" ;;
    esac

    # An override must not contradict the host's PID-1 markers. The check is
    # the markers only (not the command ladder), so a chroot or image build
    # without either marker is not treated as a contradiction.
    detected="$(probe_host_service_manager)"
    if [[ -n "$detected" && "$detected" != "$wanted" ]]; then
      if [[ "${LUNARWING_SERVICE_MANAGER_FORCE:-}" != "1" ]]; then
        die "LUNARWING_SERVICE_MANAGER=$override asks for $wanted, but this host runs $detected ($(_manager_probe_path "$detected") exists); refusing to write $wanted units on a $detected host. Unset LUNARWING_SERVICE_MANAGER, or set LUNARWING_SERVICE_MANAGER_FORCE=1 only for an image build or chroot"
      fi
      if _registered_tenants_under_manager "$detected"; then
        die "LUNARWING_SERVICE_MANAGER_FORCE=1 cannot force $wanted on this $detected host: it already has tenants registered under $detected, and forcing $wanted would drop them from the health pipeline's unit checks"
      fi
      say "WARNING: LUNARWING_SERVICE_MANAGER_FORCE=1 forcing $wanted on a $detected host (image build/chroot); the health pipeline will keep recording $detected" >&2
    elif [[ -z "$detected" ]]; then
      say "WARNING: LUNARWING_SERVICE_MANAGER=$override with no host init marker ($OPENRC_HOST_MARKER, $SYSTEMD_HOST_MARKER); assuming an image build/chroot" >&2
    fi
    printf '%s' "$wanted"; return 0
  fi

  if [[ -e "$OPENRC_HOST_MARKER" ]]; then printf 'openrc'; return 0; fi
  if [[ -e "$SYSTEMD_HOST_MARKER" ]];   then printf 'systemd'; return 0; fi

  if command -v rc-service >/dev/null 2>&1 && ! command -v systemctl >/dev/null 2>&1; then
    printf 'openrc'; return 0
  fi
  if command -v systemctl >/dev/null 2>&1; then printf 'systemd'; return 0; fi
  if command -v rc-service >/dev/null 2>&1; then printf 'openrc'; return 0; fi

  die "could not detect a supported service manager; set LUNARWING_SERVICE_MANAGER=systemd or openrc"
}

_persisted_health_setting() {  # <key> -> last KEY=value in health.env, or nothing
  local key="$1"
  [[ -r "$HEALTH_ENV_FILE" ]] || return 0
  awk -F= -v key="$key" '
    $1 == key {
      sub(/^[^=]*=/, "")
      value = $0
    }
    END { if (value != "") print value }
  ' "$HEALTH_ENV_FILE" 2>/dev/null || true
}

_persisted_health_interval() {
  _persisted_health_setting LUNARWING_MT_HEALTH_INTERVAL_MIN
}

# Podman runs every container healthcheck from its own systemd timer, and each
# run is a full `podman healthcheck run` process. Dozens of tenant containers on
# a 10-15 s cadence caused host-wide CPU bursts, so the default is 60 s. Same
# precedence as the pipeline cadence: environment, then health.env, then default.
CONTAINER_HEALTH_INTERVAL_SEC=""
CONTAINER_HEALTH_INTERVAL_SOURCE=""
_resolve_container_health_interval() {
  local value origin
  if [[ -n "${LUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC:-}" ]]; then
    value="$LUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC"
    origin="environment (LUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC)"
  else
    value="$(_persisted_health_setting LUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC)"
    if [[ -n "$value" ]]; then
      origin="$HEALTH_ENV_FILE"
    else
      value=60
      origin="default"
    fi
  fi
  if [[ ! "$value" =~ ^[1-9][0-9]{0,3}$ ]] || (( value < 10 || value > 3600 )); then
    die "invalid LUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC from $origin: '$value' (expected an integer from 10 to 3600)"
  fi
  CONTAINER_HEALTH_INTERVAL_SEC="$value"
  CONTAINER_HEALTH_INTERVAL_SOURCE="$origin"
}

# Renderers call this directly (never inside a command substitution) so an
# invalid value stops mt-admin before any unit is written.
_ensure_container_health_interval() {
  [[ -n "$CONTAINER_HEALTH_INTERVAL_SEC" ]] || _resolve_container_health_interval
}

HEALTH_INTERVAL_SOURCE=""
_resolve_health_interval() {
  local persisted=""
  if [[ -n "${LUNARWING_MT_HEALTH_INTERVAL_MIN:-}" ]]; then
    HEALTH_INTERVAL_MIN="$LUNARWING_MT_HEALTH_INTERVAL_MIN"
    HEALTH_INTERVAL_SOURCE="environment (LUNARWING_MT_HEALTH_INTERVAL_MIN)"
  else
    persisted="$(_persisted_health_interval)"
    if [[ -n "$persisted" ]]; then
      HEALTH_INTERVAL_MIN="$persisted"
      HEALTH_INTERVAL_SOURCE="$HEALTH_ENV_FILE"
    else
      HEALTH_INTERVAL_MIN=15
      HEALTH_INTERVAL_SOURCE="default"
    fi
  fi
  if [[ ! "$HEALTH_INTERVAL_MIN" =~ ^([1-9]|[1-5][0-9])$ ]]; then
    die "invalid LUNARWING_MT_HEALTH_INTERVAL_MIN from $HEALTH_INTERVAL_SOURCE: '$HEALTH_INTERVAL_MIN' (expected an integer from 1 to 59)"
  fi
}

INIT_SYSTEM=""
ensure_init_system() {
  # Propagate detection failure: detect_init_system die()s inside the command
  # substitution, which only fails the subshell — the assignment still leaves
  # INIT_SYSTEM empty and, without this check, the function returned 0, so
  # guards like 'ensure_init_system || return 1' never fired (EQ-6).
  if [[ -z "$INIT_SYSTEM" ]]; then
    INIT_SYSTEM="$(detect_init_system)" || return 1
    [[ -n "$INIT_SYSTEM" ]] || return 1
  fi
  _resolve_health_interval
  _resolve_container_health_interval
}

# Machine-wide persisted runtime choice (see _save_container_runtime). One
# line: "podman" or "docker". World-readable so unprivileged doctor runs can
# still resolve the saved choice.
RUNTIME_STATE_FILE="/etc/lunarwing/container-runtime"

# Print the persisted runtime choice, or nothing. Invalid/unreadable content
# warns (stderr) and prints nothing so callers fall through to auto-detect.
_load_saved_container_runtime() {
  [[ -f "$RUNTIME_STATE_FILE" ]] || return 0
  local saved=""
  saved="$(tr -d '[:space:]' <"$RUNTIME_STATE_FILE" 2>/dev/null)" || return 0
  saved="${saved,,}"
  case "$saved" in
    docker|podman) printf '%s' "$saved" ;;
    *) say "WARNING: ignoring invalid $RUNTIME_STATE_FILE: '$saved' (expected docker or podman)" >&2 ;;
  esac
  return 0
}

# Persist an explicitly-chosen runtime machine-wide. Warn-and-continue: a
# read-only /etc or non-root caller must never break the invoking command.
_save_container_runtime() {
  local rt="$1" tmp
  mkdir -p /etc/lunarwing 2>/dev/null || { say "WARNING: cannot create /etc/lunarwing; runtime choice not persisted" >&2; return 0; }
  tmp="$(mktemp "${RUNTIME_STATE_FILE}.tmp.XXXXXX" 2>/dev/null)" \
    || { say "WARNING: cannot write $RUNTIME_STATE_FILE; runtime choice not persisted" >&2; return 0; }
  if printf '%s\n' "$rt" >"$tmp" && chmod 0644 "$tmp" && mv "$tmp" "$RUNTIME_STATE_FILE"; then
    say "container runtime '$rt' saved to $RUNTIME_STATE_FILE (env var no longer needed)" >&2
  else
    rm -f "$tmp"
    say "WARNING: cannot write $RUNTIME_STATE_FILE; runtime choice not persisted" >&2
  fi
  return 0
}

# ── Container runtime detection ──────────────────────────────────────────────

detect_container_runtime() {
  local override="${LUNARWING_CONTAINER_RUNTIME:-}" saved=""
  if [[ -n "$override" ]]; then
    case "${override,,}" in
      docker|podman)
        override="${override,,}"
        # Persist the explicit choice (idempotent: skip when unchanged).
        saved="$(_load_saved_container_runtime)"
        [[ "$saved" == "$override" ]] || _save_container_runtime "$override"
        printf '%s' "$override"; return 0 ;;
      *) die "unsupported container runtime '$override'; use docker or podman" ;;
    esac
  fi

  saved="$(_load_saved_container_runtime)"
  if [[ -n "$saved" ]]; then
    printf '%s' "$saved"; return 0
  fi

  if command -v podman >/dev/null 2>&1 && ! command -v docker >/dev/null 2>&1; then
    printf 'podman'; return 0
  fi
  if command -v docker >/dev/null 2>&1; then printf 'docker'; return 0; fi
  if command -v podman >/dev/null 2>&1; then printf 'podman'; return 0; fi

  die "neither docker nor podman found; install one or set LUNARWING_CONTAINER_RUNTIME"
}

CONTAINER_RT=""
MT_ROOTLESS=""
ensure_container_runtime() {
  [[ -n "$CONTAINER_RT" ]] || CONTAINER_RT="$(detect_container_runtime)"
  if [[ -z "$MT_ROOTLESS" ]]; then
    # Rootless-per-tenant is the default for podman (no daemon; each tenant owns
    # its containers under ~/.local/share/containers). Docker keeps the legacy
    # rootful-as-root model (it has a daemon). Override via LUNARWING_MT_ROOTLESS.
    if [[ -n "${LUNARWING_MT_ROOTLESS:-}" ]]; then
      MT_ROOTLESS="${LUNARWING_MT_ROOTLESS}"
    elif [[ "$CONTAINER_RT" == "podman" ]]; then
      MT_ROOTLESS="true"
    else
      MT_ROOTLESS="false"
    fi
  fi
}

# Inline RUN --network first shipped in Buildah 1.30, vendored by Podman 4.5.
# Older hosts may still run existing workers; only building opencode2 needs
# this floor so its smoke can use a private netns despite --network=host.
podman_supports_run_network() {
  ensure_container_runtime
  [[ "$CONTAINER_RT" == "podman" ]] || return 1
  local ver major minor
  ver="$(podman version --format '{{.Client.Version}}' 2>/dev/null || true)"
  [[ -n "$ver" ]] || ver="$(podman --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -n1 || true)"
  major="${ver%%.*}"
  minor="${ver#*.}"; minor="${minor%%.*}"
  [[ "$major" =~ ^[0-9]+$ && "$minor" =~ ^[0-9]+$ ]] \
    && { [[ "$major" -gt 4 ]] || { [[ "$major" -eq 4 && "$minor" -ge 5 ]]; }; }
}

# True if the active runtime is podman new enough for the Quadlet .container
# features we emit. Floor is >= 4.6: Quadlet itself shipped in 4.4, but the
# Health* keys render_pg_quadlet uses first exist in 4.5 (Quadlet hard-errors
# and skips the whole unit on an unknown key), and 4.6 is the conservative
# stable baseline. Gates the systemd rootless container-supervision path against
# the imperative `podman run` fallback. Result is memoised in QUADLET_OK.
QUADLET_OK=""
podman_supports_quadlet() {
  ensure_container_runtime
  [[ "$CONTAINER_RT" == "podman" ]] || return 1
  if [[ -z "$QUADLET_OK" ]]; then
    local ver major minor
    ver="$(podman version --format '{{.Client.Version}}' 2>/dev/null || true)"
    [[ -n "$ver" ]] || ver="$(podman --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -n1 || true)"
    major="${ver%%.*}"
    minor="${ver#*.}"; minor="${minor%%.*}"
    if [[ "$major" =~ ^[0-9]+$ && "$minor" =~ ^[0-9]+$ ]] \
       && { [[ "$major" -gt 4 ]] || { [[ "$major" -eq 4 ]] && [[ "$minor" -ge 6 ]]; }; }; then
      QUADLET_OK="yes"
    else
      QUADLET_OK="no"
    fi
  fi
  [[ "$QUADLET_OK" == "yes" ]]
}
