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

detect_init_system() {
  local override="${LUNARWING_SERVICE_MANAGER:-}"
  if [[ -n "$override" ]]; then
    case "${override,,}" in
      systemd|systemd-user) printf 'systemd'; return 0 ;;
      openrc)               printf 'openrc';  return 0 ;;
      *) die "unsupported service manager override '$override'; use systemd or openrc" ;;
    esac
  fi

  if [[ -e /run/openrc/softlevel ]]; then printf 'openrc'; return 0; fi
  if [[ -e /run/systemd/system ]];   then printf 'systemd'; return 0; fi

  if command -v rc-service >/dev/null 2>&1 && ! command -v systemctl >/dev/null 2>&1; then
    printf 'openrc'; return 0
  fi
  if command -v systemctl >/dev/null 2>&1; then printf 'systemd'; return 0; fi
  if command -v rc-service >/dev/null 2>&1; then printf 'openrc'; return 0; fi

  die "could not detect a supported service manager; set LUNARWING_SERVICE_MANAGER=systemd or openrc"
}

INIT_SYSTEM=""
ensure_init_system() {
  [[ -n "$INIT_SYSTEM" ]] || INIT_SYSTEM="$(detect_init_system)"
  if [[ -z "$HEALTH_INTERVAL_MIN" ]]; then
    if [[ "$INIT_SYSTEM" == "openrc" ]]; then
      HEALTH_INTERVAL_MIN=5
    else
      HEALTH_INTERVAL_MIN=15
    fi
  fi
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
