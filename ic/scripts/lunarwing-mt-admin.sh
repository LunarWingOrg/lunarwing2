#!/usr/bin/env bash
set -euo pipefail

# ── Production Multi-Tenant Admin ─────────────────────────────────────────────
#
# Creates and manages OS-level LunarWing tenants. Each tenant is a real system
# user with its own repo clone, build artifacts, services, PostgreSQL container,
# TensorZero proxy, and XMPP bridge.
#
# Must be run as root (or via sudo).

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
LUNARWING_ROOT="$(cd -- "${REPO_ROOT}/.." && pwd)"

PORTS_REGISTRY="/etc/lunarwing/ports.json"
PORT_RANGE_START=10000
PORT_RANGE_END=19999
PORT_BLOCK_SIZE=10
BUILD_LOCK="/var/lock/lunarwing-build.lock"
PORTS_LOCK_HELD="false"
PROFILE="${LUNARWING_MT_PROFILE:-release}"
SOURCE_REPO="${LUNARWING_MT_SOURCE_REPO:-$LUNARWING_ROOT}"
# The shared daemon and its attestation are fixed root-owned trust anchors.
# Caller-controlled destinations would turn a root build/install into an
# arbitrary-path write and would make service units attest one binary but run
# another.
DARKIRC_BIN="/usr/local/bin/darkirc"
DARKIRC_BUILD_ROOT="/var/cache/lunarwing/darkirc-build"
DARKIRC_TRUSTED_PATH="/usr/bin:/bin"
DEFAULT_DARKIRC_KEY_HELPER="${LUNARWING_MT_DARKIRC_KEY_HELPER:-}"
DARKIRC_KEY_HELPER_BIN="/usr/local/libexec/lunarwing-darkirc-key-helper"
DARKIRC_KEY_HELPER_BUILD_ROOT="/var/cache/lunarwing/darkirc-key-helper-target"
OPENRC_ENV_EXEC_SRC="$SCRIPT_DIR/lunarwing-openrc-env-exec.sh"
OPENRC_ENV_EXEC="/usr/local/libexec/lunarwing-openrc-env-exec"
# The Rust helper owns the tenant-state lock.  This root-owned companion lock
# only serializes mt-admin's non-config DarkIRC writers and never follows a
# tenant-controlled path. Keep it fixed: root must not honor a caller-supplied
# lock directory from the environment.
DARKIRC_WRITER_LOCK_ROOT="/run/lunarwing"
DARKIRC_COMPAT_FILE="/etc/lunarwing/darkirc-compat.json"
DARKIRC_COMPAT_ARGS=()
# The shared binary is built only from this fixed upstream revision. Neither a
# tenant environment nor the root caller can redirect the privileged build.
DARKIRC_REPO="https://github.com/darkrenaissance/darkfi"
DARKIRC_REV="a05956d412a091e8b54c1cd4f4264c33b941203d"
TEMPLATES_DIR="${SCRIPT_DIR}/templates"
DEFAULT_TENSORZERO_URL="${LUNARWING_MT_TENSORZERO_URL:-http://192.168.1.157:3000/openai/v1}"
# Fleet-wide default VL (vision-language) backend URL the OCR sidecar proxies to.
# Empty = sidecar comes up with VL disabled (vl_available=false), preserving the
# pre-VL behavior for deployments without a local vision server. Override with
# LUNARWING_MT_VL_URL. The host.containers.internal hostname is the rootless
# podman host bridge (169.254.1.2) — verified reachable from inside tenant
# sidecar containers on this box.
DEFAULT_VL_URL="${LUNARWING_MT_VL_URL:-http://host.containers.internal:8080/v1/chat/completions}"
# Fleet-wide default for the daemon's LLM endpoint (LLM_BASE_URL). Empty = fall
# back to each tenant's local TensorZero proxy. Set this (or --llm-base-url per
# tenant) to point new tenants straight at a gateway as the proxy is phased out.
DEFAULT_LLM_BASE_URL="${LUNARWING_MT_LLM_BASE_URL:-}"
DEFAULT_GOTIFY_URL="${LUNARWING_MT_GOTIFY_URL:-}"
DEFAULT_GOTIFY_TITLE="${LUNARWING_MT_GOTIFY_TITLE:-}"

# ── Health-check / self-heal pipeline (host-global) ──────────────────────────
# The infra health-check + self-heal pipeline auto-discovers every tenant from
# /etc/init.d and the port registry, so ONE host-global scheduled run covers all
# current and future tenants. Enabled by default for new tenants on OpenRC; opt
# out per add-tenant with --no-health, or fleet-wide with
# LUNARWING_MT_HEALTH_ENABLED=false.
DEFAULT_HEALTH_ENABLED="${LUNARWING_MT_HEALTH_ENABLED:-true}"
HEALTH_INTERVAL_MIN="${LUNARWING_MT_HEALTH_INTERVAL_MIN:-15}"
HEALTH_BASE_DIR="${LUNARWING_MT_HEALTH_BASE_DIR:-/var/lib/lunarwing-health}"
HEALTH_SRC_DIR="$LUNARWING_ROOT/ic-infrastructure-health-check"
HEALTH_LIB_DIR="/usr/local/lib/lunarwing-health"
HEALTH_ENV_FILE="/etc/lunarwing/health.env"
HEALTH_LAUNCHER="/usr/local/sbin/lunarwing-mt-health"
# Gotify for self-heal escalations (token must NOT be committed — supply via env;
# it is written only to $HEALTH_ENV_FILE, mode 0600).
HEALTH_GOTIFY_URL="${LUNARWING_MT_GOTIFY_URL:-}"
HEALTH_GOTIFY_TOKEN="${LUNARWING_MT_GOTIFY_TOKEN:-}"
HEALTH_OPT_OUT=false   # set true by --no-health

# ── SSH harness defaults ──────────────────────────────────────────────────────
# SSH is enabled by default for new tenants: the harness provisions an ed25519
# key pair, configures a localhost SSH host in config.toml, and uploads the
# private key to the encrypted secrets store after the daemon starts. Workers
# get the SSH agent socket bind-mounted so they can authenticate over SSH
# without ever holding key material on disk. Opt out per-tenant with --no-ssh,
# or fleet-wide with LUNARWING_MT_SSH_ENABLED=false.
DEFAULT_SSH_ENABLED="${LUNARWING_MT_SSH_ENABLED:-true}"
SSH_OPT_OUT=false   # set true by --no-ssh

WEECHAT_BOOTSTRAP_OPT_OUT=false   # set true by --no-weechat-bootstrap

# ── Per-tenant PostgreSQL image ──────────────────────────────────────────────
# Fully-qualified (registry host included) so rootless podman resolves it WITHOUT
# depending on the host's unqualified-search-registries: docker silently defaults
# short names to docker.io, but rootless podman errors ("short-name ... did not
# resolve to an alias and no unqualified-search registries are defined"). Override
# for a local mirror via LUNARWING_MT_PG_IMAGE.
PG_IMAGE="${LUNARWING_MT_PG_IMAGE:-docker.io/pgvector/pgvector:pg16}"

# ── Per-tenant PostgreSQL backups ────────────────────────────────────────────
# pg_dump each tenant's DB (custom -Fc format) to $BACKUP_DIR/<tenant>/. Keep the
# most recent $BACKUP_KEEP dumps per tenant (0 = keep all).
BACKUP_DIR="${LUNARWING_MT_BACKUP_DIR:-/var/lib/lunarwing-backups}"
BACKUP_KEEP="${LUNARWING_MT_BACKUP_KEEP:-7}"

# ── Helpers ───────────────────────────────────────────────────────────────────

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
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

generate_token() {
  if command -v od >/dev/null 2>&1; then
    dd if=/dev/urandom bs=32 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n'
  else
    printf 'replace-with-random-token-%s' "$(date +%s)"
  fi
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

# Check every existing component without resolving or following symlinks.  The
# final component may be absent when a tenant is being provisioned; callers
# create it only after this guard and re-check before invoking the typed helper.
darkirc_path_components_safe() {
  local path="$1" rest component current="/"
  [[ "$path" == /* ]] || return 1
  rest="${path#/}"
  while [[ -n "$rest" ]]; do
    component="${rest%%/*}"
    if [[ "$rest" == */* ]]; then
      rest="${rest#*/}"
    else
      rest=""
    fi
    [[ -n "$component" ]] || return 1
    [[ "$component" != . && "$component" != .. ]] || return 1
    current="${current%/}/$component"
    [[ -L "$current" ]] && return 1
    [[ -e "$current" && ! -d "$current" ]] && return 1
  done
  return 0
}

darkirc_prepare_tenant_dirs() {
  local name="$1" state_dir config_dir datastore_dir path
  state_dir="$(tenant_state_dir "$name")"
  config_dir="$state_dir/darkirc"
  datastore_dir="$config_dir/datastore"
  for path in "$state_dir" "$config_dir" "$datastore_dir"; do
    darkirc_path_components_safe "$path" \
      || die "unsafe DarkIRC state path for tenant '$name': $path"
  done
  # Creation and mode changes happen under the tenant identity.  A tenant can
  # therefore not use a symlink to make a root process mutate another tenant's
  # files; the Rust helper performs the final no-follow validation as well.
  sudo -u "$name" mkdir -p "$config_dir" "$datastore_dir" \
    || die "could not create DarkIRC state directories for '$name'"
  sudo -u "$name" chmod 0700 "$config_dir" "$datastore_dir" \
    || die "could not secure DarkIRC state directories for '$name'"
  for path in "$state_dir" "$config_dir" "$datastore_dir"; do
    darkirc_path_components_safe "$path" \
      || die "unsafe DarkIRC state path for tenant '$name': $path"
  done
  [[ -d "$config_dir" && ! -L "$config_dir" ]] \
    || die "DarkIRC config directory is not a regular directory for '$name'"
  [[ -d "$datastore_dir" && ! -L "$datastore_dir" ]] \
    || die "DarkIRC datastore directory is not a regular directory for '$name'"
}

darkirc_file_path_safe() {
  local target="$1" parent="${1%/*}"
  [[ "$target" == /* && "$target" != "$parent" ]] || return 1
  darkirc_path_components_safe "$parent" || return 1
  [[ ! -L "$target" && ( ! -e "$target" || -f "$target" ) ]]
}

# Write a fixed tenant-owned file without putting its contents in argv or the
# environment.  The caller pipes the content on stdin; the tenant process
# validates the destination, writes a same-directory 0600 candidate, and renames
# it atomically.  This is used for adapter env files, not the TOML transaction
# (which remains exclusively owned by the Rust helper).
write_tenant_file_atomic() {
  local name="$1" target="$2" mode="${3:-600}"
  darkirc_file_path_safe "$target" \
    || die "unsafe tenant file path: $target"
  local parent="${target%/*}"
  [[ -d "$parent" && ! -L "$parent" ]] \
    || die "unsafe tenant file parent: $parent"
  sudo -u "$name" env TARGET_PATH="$target" TARGET_MODE="$mode" bash -c '
    set -euo pipefail
    target="$TARGET_PATH"
    parent="${target%/*}"
    base="${target##*/}"
    [[ -d "$parent" && ! -L "$parent" ]] || exit 73
    [[ ! -L "$target" && ( ! -e "$target" || -f "$target" ) ]] || exit 73
    if [[ -e "$target" ]]; then
      owner="$(stat -c %u "$target" 2>/dev/null || printf "-1")"
      [[ "$owner" == "$(id -u)" ]] || exit 73
    fi
    umask 077
    tmp="$(mktemp "$parent/.${base}.next.XXXXXX")"
    trap '\''rm -f -- "$tmp"'\'' EXIT
    cat >"$tmp"
    chmod "$TARGET_MODE" "$tmp"
    mv -f -- "$tmp" "$target"
    trap - EXIT
  '
}

# Read one value from a tenant secret-bearing env file without ever opening the
# file under root's credentials. The tenant-side process rejects symlinks,
# shared hard links, unexpected ownership, and permissive modes before reading.
read_tenant_env_value() {
  local name="$1" target="$2" key="$3"
  [[ "$key" =~ ^[A-Z][A-Z0-9_]*$ ]] || die "invalid tenant env key"
  darkirc_file_path_safe "$target" \
    || die "unsafe tenant env path: $target"
  sudo -u "$name" env TARGET_PATH="$target" TARGET_KEY="$key" bash -c '
    set -euo pipefail
    target="$TARGET_PATH"
    parent="${target%/*}"
    [[ -d "$parent" && ! -L "$parent" ]] || exit 73
    uid="$(id -u)"
    parent_owner="$(stat -c %u "$parent" 2>/dev/null || printf "-1")"
    parent_mode="$(stat -c %a "$parent" 2>/dev/null || true)"
    [[ "$parent_owner" == "$uid" && "$parent_mode" =~ ^[0-7]+$ ]] || exit 73
    (( (8#$parent_mode & 077) == 0 )) || exit 73
    [[ ! -e "$target" ]] && exit 0
    [[ -f "$target" && ! -L "$target" ]] || exit 73
    owner="$(stat -c %u "$target" 2>/dev/null || printf "-1")"
    mode="$(stat -c %a "$target" 2>/dev/null || true)"
    links="$(stat -c %h "$target" 2>/dev/null || printf "0")"
    [[ "$owner" == "$uid" && "$links" == 1 && "$mode" =~ ^[0-7]+$ ]] || exit 73
    (( (8#$mode & 077) == 0 && (8#$mode & 0400) != 0 )) || exit 73
    prefix="$TARGET_KEY="
    while IFS= read -r line || [[ -n "$line" ]]; do
      [[ "$line" == "$prefix"* ]] || continue
      printf "%s\n" "${line#"$prefix"}"
      break
    done <"$target"
  '
}

# Append stdin to an existing tenant env file only when KEY is absent. The
# complete read-copy-rename sequence runs as the tenant and the candidate stays
# in the same owner-only directory. Exit 10 means the key already existed.
append_tenant_env_if_missing() {
  local name="$1" target="$2" key="$3"
  [[ "$key" =~ ^[A-Z][A-Z0-9_]*$ ]] || die "invalid tenant env key"
  darkirc_file_path_safe "$target" \
    || die "unsafe tenant env path: $target"
  sudo -u "$name" env TARGET_PATH="$target" TARGET_KEY="$key" bash -c '
    set -euo pipefail
    target="$TARGET_PATH"
    parent="${target%/*}"
    base="${target##*/}"
    [[ -d "$parent" && ! -L "$parent" ]] || exit 73
    uid="$(id -u)"
    parent_owner="$(stat -c %u "$parent" 2>/dev/null || printf "-1")"
    parent_mode="$(stat -c %a "$parent" 2>/dev/null || true)"
    [[ "$parent_owner" == "$uid" && "$parent_mode" =~ ^[0-7]+$ ]] || exit 73
    (( (8#$parent_mode & 077) == 0 )) || exit 73
    [[ -f "$target" && ! -L "$target" ]] || exit 73
    owner="$(stat -c %u "$target" 2>/dev/null || printf "-1")"
    mode="$(stat -c %a "$target" 2>/dev/null || true)"
    links="$(stat -c %h "$target" 2>/dev/null || printf "0")"
    [[ "$owner" == "$uid" && "$links" == 1 && "$mode" =~ ^[0-7]+$ ]] || exit 73
    (( (8#$mode & 077) == 0 && (8#$mode & 0400) != 0 )) || exit 73
    prefix="$TARGET_KEY="
    while IFS= read -r line || [[ -n "$line" ]]; do
      if [[ "$line" == "$prefix"* ]]; then
        cat >/dev/null
        exit 10
      fi
    done <"$target"
    umask 077
    tmp="$(mktemp "$parent/.${base}.next.XXXXXX")"
    trap '\''rm -f -- "$tmp"'\'' EXIT
    cat "$target" >"$tmp"
    cat >>"$tmp"
    chmod 0600 "$tmp"
    mv -f -- "$tmp" "$target"
    trap - EXIT
  '
}

TENANT_ENV_ENTRY_ADDED=false

patch_tenant_env_entry() {
  local name="$1" target="$2" key="$3" existing_message="$4" added_message="$5" rc
  TENANT_ENV_ENTRY_ADDED=false
  if append_tenant_env_if_missing "$name" "$target" "$key"; then
    TENANT_ENV_ENTRY_ADDED=true
    say "$added_message"
    return 0
  else
    rc=$?
  fi
  if [[ "$rc" -eq 10 ]]; then
    say "$existing_message"
    return 0
  fi
  die "could not safely patch $key in $target"
}

darkirc_scope_id() {
  local name="$1"
  jq -r ".tenants[\"$name\"].darkirc_scope_id // empty" "$PORTS_REGISTRY" 2>/dev/null
}

darkirc_load_compatibility() {
  require_cmd jq
  require_cmd sha256sum
  [[ -f "$DARKIRC_COMPAT_FILE" && ! -L "$DARKIRC_COMPAT_FILE" ]] \
    || die "DarkIRC compatibility attestation is missing: $DARKIRC_COMPAT_FILE"
  local mode owner links profile digest key_format source_revision actual
  mode="$(stat -c '%a' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)"
  owner="$(stat -c '%u' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)"
  links="$(stat -c '%h' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)"
  [[ "$mode" == 600 && "$owner" == 0 && "$links" == 1 ]] \
    || die "DarkIRC compatibility attestation must be root-owned, single-link mode 0600"
  profile="$(jq -r '.profile_id // empty' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)"
  digest="$(jq -r '.binary_sha256 // empty' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)"
  key_format="$(jq -r '.key_format // empty' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)"
  source_revision="$(jq -r '.source_revision // empty' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)"
  [[ "$(jq -r '.schema // empty' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)" == "lunarwing.darkirc-compat/v1" ]] \
    || die "invalid DarkIRC compatibility attestation schema"
  [[ "$profile" == "darkfi-a05956d41-chacha-v1" ]] \
    || die "unknown DarkIRC compatibility profile"
  [[ "$key_format" == "darkfi-chacha-base58-32" ]] \
    || die "unknown DarkIRC key format"
  [[ "$source_revision" == "$DARKIRC_REV" ]] \
    || die "unapproved DarkIRC source revision"
  [[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]] \
    || die "invalid DarkIRC binary digest in compatibility attestation"
  darkirc_validate_root_executable "$DARKIRC_BIN" \
    || die "attested DarkIRC binary is missing or unsafe: $DARKIRC_BIN"
  actual="sha256:$(sha256sum "$DARKIRC_BIN" | awk '{print $1}')"
  [[ "$actual" == "$digest" ]] \
    || die "DarkIRC binary digest does not match the root-owned compatibility attestation"
  DARKIRC_COMPAT_ARGS=(
    --generator-profile "$profile"
    --binary-sha256 "$digest"
    --binary-path "$DARKIRC_BIN"
    --key-format "$key_format"
    --source-revision "$source_revision"
  )
}

validate_darkirc_build_candidate() {
  local candidate="$1"
  [[ "$candidate" == "$DARKIRC_BUILD_ROOT/darkirc.candidate."* ]] \
    || die "DarkIRC compatibility validation requires a pinned build artifact"
  [[ -s "$candidate" ]] && darkirc_validate_root_executable "$candidate" \
    || die "pinned DarkIRC build artifact is missing or unsafe"
}

darkirc_validate_root_directory_path() {
  local path="$1" rest component current="/" mode owner
  [[ "$path" == /* ]] || return 1
  rest="${path#/}"
  while [[ -n "$rest" ]]; do
    component="${rest%%/*}"
    if [[ "$rest" == */* ]]; then
      rest="${rest#*/}"
    else
      rest=""
    fi
    [[ -n "$component" && "$component" != . && "$component" != .. ]] || return 1
    current="${current%/}/$component"
    [[ -d "$current" && ! -L "$current" ]] || return 1
    mode="$(stat -c '%a' "$current" 2>/dev/null || true)"
    owner="$(stat -c '%u' "$current" 2>/dev/null || true)"
    [[ "$owner" == 0 && "$mode" =~ ^[0-7]+$ ]] || return 1
    (( (8#$mode & 0022) == 0 )) || return 1
  done
}

prepare_darkirc_build_root() {
  local parent parent_base path
  parent="$(dirname "$DARKIRC_BUILD_ROOT")"
  parent_base="$(dirname "$parent")"
  darkirc_validate_root_directory_path "$parent_base" \
    || die "unsafe DarkIRC build-root base: $parent_base"
  darkirc_path_components_safe "$parent" \
    || die "unsafe DarkIRC build-root parent: $parent"
  if [[ ! -e "$parent" ]]; then
    install -d -o root -g root -m 0755 "$parent"
  fi
  darkirc_validate_root_directory_path "$parent" \
    || die "DarkIRC build-root parent is not root-controlled: $parent"
  for path in "$DARKIRC_BUILD_ROOT" \
    "$DARKIRC_BUILD_ROOT/home" "$DARKIRC_BUILD_ROOT/cargo-home"; do
    darkirc_path_components_safe "$path" \
      || die "unsafe DarkIRC build path: $path"
    [[ ! -e "$path" || -d "$path" && ! -L "$path" ]] \
      || die "DarkIRC build path is not a regular directory: $path"
  done
  install -d -o root -g root -m 0700 \
    "$DARKIRC_BUILD_ROOT" \
    "$DARKIRC_BUILD_ROOT/home" \
    "$DARKIRC_BUILD_ROOT/cargo-home"
  for path in "$parent" "$DARKIRC_BUILD_ROOT" \
    "$DARKIRC_BUILD_ROOT/home" "$DARKIRC_BUILD_ROOT/cargo-home"; do
    darkirc_validate_root_directory_path "$path" \
      || die "DarkIRC build path is not root-controlled: $path"
  done
}

trusted_darkirc_tool_path() {
  local name="$1" candidate
  candidate="$(PATH="$DARKIRC_TRUSTED_PATH" type -P "$name" 2>/dev/null || true)"
  [[ -n "$candidate" ]] \
    || die "trusted system DarkIRC build tool is unavailable: $name"
  darkirc_validate_root_executable "$candidate" \
    || die "DarkIRC build tool is not a root-owned system executable: $candidate"
  printf '%s' "$candidate"
}

validate_darkirc_source_checkout() {
  local source_dir="$1" git_bin="$2" source_revision dirty
  darkirc_validate_root_directory_path "$source_dir" \
    || die "DarkIRC source checkout is outside the root-controlled build boundary"
  source_revision="$(/usr/bin/env -i \
    HOME="$DARKIRC_BUILD_ROOT/home" \
    PATH="$DARKIRC_TRUSTED_PATH" \
    "$git_bin" -C "$source_dir" rev-parse HEAD 2>/dev/null || true)"
  [[ "$source_revision" == "$DARKIRC_REV" ]] \
    || die "DarkIRC source revision is not the approved compatibility revision"
  dirty="$(/usr/bin/env -i \
    HOME="$DARKIRC_BUILD_ROOT/home" \
    PATH="$DARKIRC_TRUSTED_PATH" \
    "$git_bin" -C "$source_dir" status --porcelain 2>/dev/null || true)"
  [[ -z "$dirty" ]] \
    || die "DarkIRC source checkout is not clean; refusing compatibility attestation"
  [[ -f "$source_dir/Makefile" && ! -L "$source_dir/Makefile" ]] \
    || die "DarkIRC Makefile is missing or unsafe"
  [[ -f "$source_dir/bin/darkirc/Cargo.toml" && ! -L "$source_dir/bin/darkirc/Cargo.toml" ]] \
    || die "DarkIRC Cargo.toml is missing or unsafe"
}

pin_darkirc_build_candidate() {
  local candidate="$1" pinned
  [[ "$candidate" == "$DARKIRC_BUILD_ROOT/"* ]] \
    || die "DarkIRC build artifact is outside the root-controlled build boundary"
  [[ -f "$candidate" && -s "$candidate" && ! -L "$candidate" ]] \
    || die "DarkIRC build artifact is missing or unsafe"
  pinned="$(mktemp "$DARKIRC_BUILD_ROOT/darkirc.candidate.XXXXXX")"
  if ! cp --no-dereference -- "$candidate" "$pinned"; then
    rm -f -- "$pinned"
    die "failed to pin DarkIRC build artifact"
  fi
  [[ -f "$pinned" && ! -L "$pinned" ]] || {
    rm -f -- "$pinned"
    die "pinned DarkIRC build artifact is not a regular file"
  }
  chown root:root "$pinned" && chmod 0755 "$pinned" || {
    rm -f -- "$pinned"
    die "failed to secure pinned DarkIRC build artifact"
  }
  darkirc_validate_root_executable "$pinned" || {
    rm -f -- "$pinned"
    die "pinned DarkIRC build artifact failed ownership validation"
  }
  printf '%s' "$pinned"
}

install_darkirc_binary_atomic() {
  local candidate="$1" destination_dir tmp source_digest installed_digest
  [[ "$candidate" == "$DARKIRC_BUILD_ROOT/darkirc.candidate."* ]] \
    || die "DarkIRC installation requires a pinned build artifact"
  darkirc_validate_root_executable "$candidate" \
    || die "DarkIRC installation candidate is not root-controlled"
  source_digest="$(sha256sum "$candidate" | awk '{print $1}')"
  destination_dir="$(dirname "$DARKIRC_BIN")"
  [[ "$destination_dir" == /usr/local/bin ]] \
    || die "DarkIRC binary destination must remain under /usr/local/bin"
  [[ -d "$destination_dir" && ! -L "$destination_dir" ]] \
    || die "unsafe DarkIRC binary destination directory"
  [[ "$(stat -c '%u' "$destination_dir" 2>/dev/null || true)" == 0 ]] \
    || die "DarkIRC binary destination directory must be root-owned"
  local destination_mode
  destination_mode="$(stat -c '%a' "$destination_dir" 2>/dev/null || true)"
  [[ "$destination_mode" =~ ^[0-7]+$ ]] \
    || die "could not validate DarkIRC binary destination mode"
  (( (8#$destination_mode & 07022) == 0 )) \
    || die "DarkIRC binary destination directory must not be group/world writable"
  [[ ! -L "$DARKIRC_BIN" && ( ! -e "$DARKIRC_BIN" || -f "$DARKIRC_BIN" ) ]] \
    || die "unsafe DarkIRC binary destination"
  tmp="$(mktemp "$destination_dir/.darkirc.next.XXXXXX")"
  if ! install -o root -g root -m 0755 "$candidate" "$tmp"; then
    rm -f -- "$tmp"
    die "failed to stage DarkIRC binary"
  fi
  if ! mv -fT -- "$tmp" "$DARKIRC_BIN"; then
    rm -f -- "$tmp"
    die "failed to install DarkIRC binary"
  fi
  darkirc_validate_root_executable "$DARKIRC_BIN" \
    || die "installed DarkIRC binary failed ownership validation"
  installed_digest="$(sha256sum "$DARKIRC_BIN" | awk '{print $1}')"
  [[ "$installed_digest" == "$source_digest" ]] \
    || die "installed DarkIRC binary does not match the pinned build artifact"
}

record_darkirc_compatibility() {
  local source_revision="$DARKIRC_REV" digest compat_dir tmp
  local binary_mode binary_owner binary_links
  binary_mode="$(stat -c '%a' "$DARKIRC_BIN" 2>/dev/null || true)"
  binary_owner="$(stat -c '%u' "$DARKIRC_BIN" 2>/dev/null || true)"
  binary_links="$(stat -c '%h' "$DARKIRC_BIN" 2>/dev/null || true)"
  [[ "$binary_mode" =~ ^[0-7]+$ && "$((8#$binary_mode & 07022))" -eq 0 \
     && "$binary_owner" == 0 && "$binary_links" == 1 ]] \
    || die "installed DarkIRC binary is not a root-owned, non-writable regular file"
  digest="sha256:$(sha256sum "$DARKIRC_BIN" | awk '{print $1}')"
  compat_dir="$(dirname "$DARKIRC_COMPAT_FILE")"
  mkdir -p "$compat_dir"
  chmod 0755 "$compat_dir"
  tmp="$(mktemp "$DARKIRC_COMPAT_FILE.next.XXXXXX")"
  jq -n \
    --arg schema "lunarwing.darkirc-compat/v1" \
    --arg profile "darkfi-a05956d41-chacha-v1" \
    --arg digest "$digest" \
    --arg key_format "darkfi-chacha-base58-32" \
    --arg source_revision "$source_revision" \
    '{schema:$schema,profile_id:$profile,binary_sha256:$digest,key_format:$key_format,source_revision:$source_revision}' \
    >"$tmp" || { rm -f "$tmp"; die "could not render DarkIRC compatibility attestation"; }
  chmod 0600 "$tmp"
  chown root:root "$tmp" 2>/dev/null || true
  mv -f "$tmp" "$DARKIRC_COMPAT_FILE"
  chmod 0600 "$DARKIRC_COMPAT_FILE"
  chown root:root "$DARKIRC_COMPAT_FILE" 2>/dev/null || true
}

generate_darkirc_scope_id() {
  command -v od >/dev/null 2>&1 || die "od is required for cryptographic DarkIRC scope IDs"
  [[ -r /dev/urandom ]] || die "/dev/urandom is required for cryptographic DarkIRC scope IDs"
  local id
  id="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
  [[ "$id" =~ ^[0-9a-f]{32}$ ]] || die "failed to generate a 128-bit DarkIRC scope ID"
  printf '%s' "$id"
}

PORTS_REGISTRY_LOCK_FD=""

ports_registry_lock() {
  [[ -n "$PORTS_REGISTRY_LOCK_FD" ]] && return 0
  require_cmd flock
  local lock_file="${PORTS_REGISTRY}.lock" lock_dir
  lock_dir="$(dirname "$lock_file")"
  mkdir -p "$lock_dir" || die "could not create port registry directory"
  if [[ -L "$lock_file" || -e "$lock_file" && ! -f "$lock_file" ]]; then
    die "unsafe port registry lock: $lock_file"
  fi
  if [[ ! -e "$lock_file" ]]; then
    ( umask 077; : >"$lock_file" ) || die "could not create port registry lock"
  fi
  chmod 0600 "$lock_file" 2>/dev/null || true
  exec {PORTS_REGISTRY_LOCK_FD}>"$lock_file"
  flock -x -w 10 "$PORTS_REGISTRY_LOCK_FD" || die "port registry is busy"
}

ports_registry_unlock() {
  [[ -n "$PORTS_REGISTRY_LOCK_FD" ]] || return 0
  flock -u "$PORTS_REGISTRY_LOCK_FD" || true
  eval "exec ${PORTS_REGISTRY_LOCK_FD}>&-"
  PORTS_REGISTRY_LOCK_FD=""
}

validate_darkirc_scope_id() {
  local id="$1"
  [[ "$id" =~ ^[0-9a-f]{32}$ && ! "$id" =~ ^0+$ ]] \
    || die "invalid DarkIRC scope ID"
}

validate_existing_darkirc_scope_id() {
  local name="$1" id="$2" duplicate
  validate_darkirc_scope_id "$id"
  duplicate="$(jq -r --arg name "$name" --arg id "$id" \
    '.tenants | to_entries[] | select(.key != $name and .value.darkirc_scope_id == $id) | .key' \
    "$PORTS_REGISTRY" 2>/dev/null | head -1)"
  [[ -z "$duplicate" ]] || die "darkirc_scope_id for '$name' conflicts with tenant '$duplicate'"
}

darkirc_scope_is_unique() {
  local name="$1" id="$2"
  ! jq -e --arg name "$name" --arg id "$id" \
    '.tenants | to_entries[] | select(.key != $name and .value.darkirc_scope_id == $id)' \
    "$PORTS_REGISTRY" >/dev/null 2>&1
}

ensure_darkirc_scope_id() {
  local name="$1" current id tmp
  local lock_owned=false
  if [[ -z "$PORTS_REGISTRY_LOCK_FD" ]]; then
    ports_registry_lock
    lock_owned=true
  fi
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  current="$(darkirc_scope_id "$name")"
  if [[ -n "$current" ]]; then
    validate_darkirc_scope_id "$current" || die "tenant '$name' has an invalid darkirc_scope_id"
    local duplicate
    duplicate="$(jq -r --arg name "$name" --arg id "$current" \
      '.tenants | to_entries[] | select(.key != $name and .value.darkirc_scope_id == $id) | .key' \
      "$PORTS_REGISTRY" 2>/dev/null | head -1)"
    [[ -z "$duplicate" ]] || die "darkirc_scope_id for '$name' conflicts with tenant '$duplicate'"
    if [[ "$lock_owned" == true ]]; then
      ports_registry_unlock
    fi
    printf '%s' "$current"
    return 0
  fi

  for _ in {1..8}; do
    id="$(generate_darkirc_scope_id)"
    if ! jq -e --arg id "$id" '.tenants | to_entries[] | select(.value.darkirc_scope_id == $id)' \
      "$PORTS_REGISTRY" >/dev/null 2>&1; then
      break
    fi
    id=""
  done
  [[ -n "$id" ]] || die "could not allocate a unique DarkIRC scope ID"
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq --arg name "$name" --arg id "$id" '.tenants[$name].darkirc_scope_id = $id' \
    "$PORTS_REGISTRY" >"$tmp" || { rm -f "$tmp"; die "failed to persist DarkIRC scope ID"; }
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  if [[ "$lock_owned" == true ]]; then
    ports_registry_unlock
  fi
  printf '%s' "$id"
}

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

# Directory for config-hash sidecars (created on first use).
_hash_dir() { printf '%s/.config/lunarwing/container-hashes' "$(tenant_home "$1")"; }

# Compute and store the hash of a config file (e.g. a .container quadlet).
# Usage: _store_container_hash <tenant> <container-name> <config-file>
_store_container_hash() {
  local name="$1" container="$2" config_file="$3" hdir hash
  hdir="$(_hash_dir "$name")"
  mkdir -p "$hdir" 2>/dev/null || true
  hash="$(sha256sum "$config_file" 2>/dev/null | cut -d' ' -f1 || true)"
  [[ -n "$hash" ]] && printf '%s\n' "$hash" >"$hdir/${container}.hash"
}

# Check whether the config file's hash matches the stored hash.
# Returns 0 (match / first-run) or 1 (mismatch / needs recreate).
# Usage: _container_config_changed <tenant> <container-name> <config-file>
_container_config_changed() {
  local name="$1" container="$2" config_file="$3" hdir stored current
  hdir="$(_hash_dir "$name")"
  [[ -f "$config_file" ]] || return 0  # no config file = no opinion
  current="$(sha256sum "$config_file" 2>/dev/null | cut -d' ' -f1 || true)"
  [[ -n "$current" ]] || return 0      # can't hash = don't force recreate
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

darkirc_state_available() {
  local name="$1" path
  path="$(tenant_state_dir "$name")/darkirc"
  [[ -d "$path" && ! -L "$path" ]]
}

tenant_proxy_enabled() {
  local name="$1"
  local val
  val="$(jq -r ".tenants[\"$name\"].enable_proxy // false" "$PORTS_REGISTRY" 2>/dev/null)"
  [[ "$val" == "true" ]]
}

# Whether a given external worker (nanocode|pebble|opencode) is selected for this
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

# Per-tenant PostgreSQL password. The source of truth is a 0600, tenant-owned
# secret file, generated once (hex → URL-safe inside DATABASE_URL) and reused so
# it stays STABLE across restarts/reconfigures — POSTGRES_PASSWORD only
# initialises an EMPTY datadir, so the value must not drift after first init.
# Migration-safe: if a tenant was already provisioned (its lunarwing.env carries a
# DATABASE_URL password), that value is preserved so an already-initialised DB
# keeps working; only brand-new tenants get a fresh random password. Use
# `rotate-pg-password` to deliberately move an existing tenant onto a random one.
# Never logged (repo rule).
tenant_pg_password() {
  local name="$1" f envf existing pw
  f="$(tenant_env_dir "$name")/pg.secret"
  if [[ -s "$f" ]]; then
    cat "$f"   # callers use $(...), which strips the trailing newline
    return 0
  fi
  envf="$(tenant_env_dir "$name")/lunarwing.env"
  if [[ -f "$envf" ]]; then
    existing="$(sed -n 's#^DATABASE_URL=postgres://lunarwing:\([^@]*\)@.*#\1#p' "$envf" | head -1)"
  fi
  if [[ -n "${existing:-}" ]]; then
    pw="$existing"                       # preserve an already-initialised DB's password
  else
    pw="$(generate_token | cut -c1-32)"  # fresh tenant → random 128-bit hex
  fi
  mkdir -p "$(dirname "$f")"
  ( umask 077; printf '%s\n' "$pw" > "$f" )
  chown "$name:$name" "$f" 2>/dev/null || true
  printf '%s\n' "$pw"
}

usage() {
  cat <<'EOF'
Usage:
  sudo lunarwing-mt-admin.sh <command> [args...]

Production multi-tenant administration for LunarWing.
Manages OS users, port allocation, per-tenant services, and builds.

Commands:
  add-tenant <name> [options]      Create user, allocate ports, clone repo,
                                   generate env, render and install services
    --docker-group                 Add user to docker/podman group
    --xmpp-jid <jid>              XMPP JID for this tenant
    --xmpp-password <pass>        XMPP password (generated if omitted)
    --llm-api-key <key>            API key for the LLM backend provider
    --llm-base-url <url>           LLM endpoint the daemon dials (LLM_BASE_URL).
                                   Default: this tenant's local TensorZero proxy
    --tensorzero-url <url>         Upstream TensorZero URL
    --gotify-url <url>             Custom Gotify server URL (e.g. https://gotify.example.com)
    --no-health                    Don't enable the host-global health/self-heal pipeline
    --no-ssh                       Don't provision SSH harness (key pair, config, agent)
    --no-weechat-bootstrap         Don't run WeeChat relay auto-bootstrap (still writes
                                   minimal weechat.env; services remain rendered)
    --enable-darkirc               Provision DarkIRC daemon + adapter for this tenant
                                   (disabled by default; darkirc services are NOT created)
    --with-nanocode                Select the nanocode worker for this tenant (recorded
                                   in the port registry; start-tenant only starts workers
                                   the tenant selected — see --with-* on build-tenant to
                                   also build the image). Off by default.
    --with-pebble                  Select the pebble worker for this tenant (as above)
    --with-opencode                Select the opencode worker for this tenant (as above)
    --nanocode-model <model>       Override the nanocode worker's LLM model
                                   (written to lunarwing.env as NANOCODE_MODEL)
    --nanocode-base-url <url>      Override the nanocode worker's TensorZero baseURL
                                   (written to lunarwing.env as NANOCODE_BASE_URL)
    --opencode-model <model>       Override the opencode worker's LLM model
                                   (written to lunarwing.env as OPENCODE_MODEL)
    --opencode-base-url <url>      Override the opencode worker's TensorZero baseURL
                                   (written to lunarwing.env as OPENCODE_BASE_URL)
    --llm-model <model>            Override LLM_MODEL (default:
                                   tensorzero::function_name::lunarwing)
    --gateway-host <host>          Override GATEWAY_HOST bind address (default:
                                   127.0.0.1; use 0.0.0.0 for LAN access)
    --xmpp-allow-from <jids>       Comma-separated extra XMPP JIDs allowed to DM
                                   the agent (added to the tenant's own JID;
                                   written to both lunarwing.env and xmpp-bridge.env)

   add-tenants <names> [options]    Comma-separated list (e.g. "Ruffles,Miyuki")
     (same options as add-tenant apply to all, including --enable-darkirc,
      --nanocode-model/--nanocode-base-url, --opencode-model/--opencode-base-url,
      --llm-model, --gateway-host, --xmpp-allow-from, and --no-weechat-bootstrap)

  remove-tenant <name>             Stop services, deallocate ports
    --purge                        Also delete OS user and home directory

  build-tenant <name>             Build binaries for one tenant (OOM-safe flock)
    --with-wasm                    Also build WASM extensions
    --with-nanocode                Also build the nanocode worker Docker image
    --with-pebble                  Also build the pebble worker Docker image
    --with-opencode                Also build the opencode worker Docker image

  build-all                        Build each tenant sequentially
    --with-wasm                    Also build WASM extensions
    --with-nanocode                Also build the nanocode worker Docker image
    --with-pebble                  Also build the pebble worker Docker image
    --with-opencode                Also build the opencode worker Docker image

  build-nanocode-worker            Build the nanocode worker Docker image
    --no-cache                     Force a full rebuild without Docker cache

  build-pebble-worker             Build the pebble worker Docker image
    --no-cache                     Force a full rebuild without Docker cache

  build-opencode-worker            Build the opencode worker Docker image
    --no-cache                     Force a full rebuild without Docker cache
    --with-toolchains              Include Rust/Go/C++ toolchains (default: slim)

  build-vision-sidecar             Build the LunarVision OCR sidecar Docker image

  build-darkirc                   Build darkirc from the pinned upstream revision
    --tenant <name>                Compatibility selector only; tenant files and
                                   toolchains never enter the shared binary build

  install-wasm <name>             Install built WASM tools/channels into tenant state dir
  install-wasm-all                Install WASM for all tenants

  start-tenant <name>             Start all services for a tenant
  stop-tenant <name>              Stop all services for a tenant
  stop-writers <name>             Stop tenant writers but leave PostgreSQL up
  writers-active <name>            Exit 0 when any tenant writer is active
  restart-tenant <name>           Stop then start
  render-units <name>             Re-render a tenant's service units from the current
                                  generator (no restart; applies init-script changes)
  upgrade-tenant <name> --target <ref>
                                  In-place upgrade: backup, stop, git fetch + checkout
                                  <ref> (as the tenant user), rebuild with WASM,
                                  re-render units, patch env, start. Long-running —
                                  run inside tmux.
    --source-repo <path>           Point the tenant's git origin at this repo first
    --no-backup                    Skip the pre-upgrade Postgres backup
    --skip-render                  Keep existing unit files (run render-units later;
                                   must happen before v2.0.0 for pre-1.1.9 tenants)
  rotate-pg-password <name>       Generate a new random PG password (ALTER ROLE + env update)

  configure-gotify <name> <url>    Set custom Gotify URL for a tenant
                                   (updates workspace config + capabilities)

  configure-pebble <name>          Configure pebble worker for a tenant
    --nanogpt-api-key <key>        NanoGPT API key
    --model <model>                Pebble model (default: openai/gpt-5.2)

  configure-nanocode <name>        Set nanocode worker LLM overrides for a tenant
    --model <model>                TensorZero model (NANOCODE_MODEL; any string)
    --base-url <url>               TensorZero baseURL (NANOCODE_BASE_URL; full URL)
                                   (restart the worker after: stop-tenant && start-tenant)

  configure-opencode <name>        Set opencode worker LLM overrides for a tenant
    --model <model>                TensorZero model (OPENCODE_MODEL; any string)
    --base-url <url>               TensorZero baseURL (OPENCODE_BASE_URL; full URL)
                                   (restart the worker after: stop-tenant && start-tenant)

   configure-ssh <name>             Provision SSH harness for an existing tenant
     --host <host>                  SSH host (default: 127.0.0.1)
     --user <user>                  SSH user (default: tenant name)
                                   (restart the tenant after to upload the key: restart-tenant)

  configure-weechat-relay <name>   Generate WeeChat relay config for a tenant.
                                   Preserves existing config: fails on any
                                   non-empty ~/.config/weechat without modifying it.

  patch-env <name>                 Add missing env vars (e.g. ORCHESTRATOR_PORT)
  patch-env-all                    Patch env for all registered tenants
  darkirc-contact list <tenant>    Read-only contact inventory (fingerprints only)
  darkirc-contact status <tenant> <contact>
                                   Inspect one DarkIRC contact (read-only)
  darkirc-contact doctor [--all]   Validate legacy contacts without mutation
  darkirc-contact adopt <tenant> --yes
                                   Adopt settled manual contacts into metadata ledger
  darkirc-contact export-migration <tenant>
                                   Write owner-only darkirc-contacts-v1 manifest
  darkirc-contact stage-migration <tenant> --in -
                                   Stage a manifest from stdin (no secret argv)
  darkirc-contact import-migration <tenant>
                                   Import the staged contact manifest
  darkirc-contact migration-ready <tenant>
                                   Recover/check settled DarkIRC state before upgrade
  darkirc-contact recover <tenant>
                                   Recover an unfinished config/ledger transaction
  darkirc-contact validate-migration --in -
                                   Validate a manifest without writing tenant state
  darkirc-contact prepare <tenant> <contact> --out <file|->
                                   Generate a keypair and emit a public offer artifact
    --expires <secs>               Offer lifetime in seconds (default 1800)
  darkirc-contact respond <tenant> <contact> --in <offer.json> --out <file|->
                                   Respond to an initiator offer with a bound response
    --expect-peer-fingerprint <sha256>  Required: initiator's public fingerprint
  darkirc-contact complete <tenant> <contact> --in <response.json>
                                   Install a contact from a completed exchange (--defer-apply)
    --expect-peer-fingerprint <sha256>  Required: responder's public fingerprint
  darkirc-contact cancel <tenant> --exchange-id <id>
                                   Cancel a pending exchange
  darkirc-contact exchanges <tenant>
                                   List pending DarkIRC key exchanges
  darkirc-health <tenant> --strict Strict DarkIRC activation gate (fail-closed)

  migrate-owner-scope <name>       Rekey DB data from 'default' to tenant scope
    --from <old_scope>             Old owner_id (default: 'default')
                                   (run after patch-env adds LUNARWING_OWNER_ID)
  owner-scopes <name>              Print restored owner scopes and row counts

  list-tenants                     Show all tenants with ports and status
  status <name>                    Detailed status for one tenant
  tokens [name]                    Print gateway auth tokens (all or one)
  doctor                           System dependency and health checks

  backup-tenant <name>             pg_dump a tenant's DB (custom format) to
                                   $LUNARWING_MT_BACKUP_DIR/<name>/
  backup-all                       Back up every registered tenant
  list-backups [name]              List existing backups (all tenants or one)
  restore-tenant <name> <file>     Restore a tenant DB from a dump (DESTRUCTIVE)
    --yes                          Required: confirm the DROP+recreate restore
                                   (stop the tenant daemon first)

Environment:
  LUNARWING_SERVICE_MANAGER        Override: systemd or openrc
  LUNARWING_CONTAINER_RUNTIME      Override: docker or podman (persisted to /etc/lunarwing/container-runtime on first explicit use; later runs need no env var)
  LUNARWING_MT_PROFILE             Build profile: release (default) or debug
  LUNARWING_MT_SOURCE_REPO         Path to source repo to clone from
  LUNARWING_MT_TENSORZERO_URL      Default upstream TensorZero URL
  LUNARWING_MT_LLM_BASE_URL        Fleet-wide default LLM_BASE_URL for new tenants
                                   (empty = each tenant's local TensorZero proxy)
  LUNARWING_MT_GOTIFY_URL          Default Gotify server URL for new tenants
  LUNARWING_MT_GOTIFY_TITLE        Default Gotify notification title for new tenants
  LUNARWING_MT_BACKUP_DIR          Backup directory (default /var/lib/lunarwing-backups)
  LUNARWING_MT_BACKUP_KEEP         Keep last N dumps per tenant (default 7; 0 = keep all)

Per-tenant LLM tunables (written to each tenant's lunarwing.env with the
fleet defaults below; edit the file + restart-tenant to override — an existing
value is preserved across re-provision):
  LLM_CIRCUIT_BREAKER_THRESHOLD    Consecutive fully-retried LLM failures before
                                   the breaker opens and fast-fails (default 7)
  LLM_CIRCUIT_BREAKER_RECOVERY_SECS  Seconds the breaker stays open before it
                                   probes for recovery (default 45)
EOF
}

# ── Template rendering ────────────────────────
#
# render_template_content <template-file> [var=value ...]
#
# Reads a template file and emits the substituted content. Callers that own
# secret-bearing or stateful files must hand the result to their semantic
# updater rather than redirecting it directly to the destination.
render_template_content() {
  local template="$1"
  shift

  [[ "$template" = /* ]] || template="$TEMPLATES_DIR/$template"
  [[ -f "$template" ]] || die "template not found: $template"

  local content
  content="$(<"$template")"

  local pair var val
  for pair; do
    var="${pair%%=*}"
    val="${pair#*=}"
    content="${content//__${var}__/${val}}"
  done

  printf '%s\n' "$content"
}

# Legacy helper for non-state templates (env/unit fragments). DarkIRC's active
# TOML never uses this direct writer; generate_darkirc_config pipes content to
# the locked Rust updater below.
render_template() {
  local template="$1" out="$2"
  shift 2
  render_template_content "$template" "$@" >"$out"
}
# ── Root check ────────────────────────────────────────────────────────────────

require_root() {
  [[ "${EUID}" -eq 0 ]] || die "must run as root (use sudo)"
}

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

# Run the container runtime for a TENANT's containers. When rootless (podman),
# execute as the tenant user against their rootless store + runtime dir; when
# rootful (docker), run as root unchanged. Every per-tenant pg/worker container
# operation MUST go through this so inspect/start/stop/exec/rm hit the SAME store
# that owns the container — root and rootless podman are separate universes.
_ctr() {
  local name="$1"; shift
  ensure_container_runtime
  if [[ "$MT_ROOTLESS" == "true" ]]; then
    local uid home
    uid="$(id -u "$name")" || die "cannot resolve uid for tenant '$name'"
    home="$(getent passwd "$name" | cut -d: -f6)"
    # Run from a tenant-traversable CWD (F10): `sudo -u` keeps the caller's cwd, so
    # when mt-admin runs from an admin dir the tenant can't enter (e.g. ~dame, 0700)
    # `sudo -u` aborts with "cannot chdir ... Permission denied" BEFORE the runtime
    # runs — which silently broke the rootless pg readiness gate (it always timed
    # out). `/` is always traversable; no _ctr call passes a cwd-relative path. exec
    # preserves the exit code and the stdin/stdout redirects used by exec/pg_dump.
    ( cd / && exec sudo -u "$name" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" "$CONTAINER_RT" "$@" )
  else
    "$CONTAINER_RT" "$@"
  fi
}

# Ensure a worker image is available to whoever will run the tenant's container.
# Rootful (docker): the shared root store already has it — just verify presence.
# Rootless (podman): the image lives in the tenant's OWN store; if absent, copy it
# from the admin (root) store via save|load (per-tenant, ~minutes for large images;
# a shared additionalimagestore would avoid the N copies but isn't wired yet).
# Returns non-zero if the image can't be made available (caller should skip).
_ensure_tenant_image() {
  local name="$1" image="$2"

  # The admin (root) store's image ID is the source of truth for "current".
  local admin_id=""
  if "$CONTAINER_RT" image inspect -f '{{.Id}}' "$image" &>/dev/null; then
    admin_id="$("$CONTAINER_RT" image inspect -f '{{.Id}}' "$image")"
  fi

  # rootful: the admin store IS the runtime store, so presence there suffices.
  if [[ "$MT_ROOTLESS" != "true" ]]; then
    [[ -n "$admin_id" ]] && return 0
    return 1   # rootful + not built yet -> caller skips (build first)
  fi

  # rootless: each tenant has its own store. Skip only when the tenant already
  # holds the CURRENT image (same ID as the admin store) — not merely when the
  # name exists — so a rebuilt worker image actually reaches tenants instead of
  # being silently held back by a stale same-named copy.
  local tenant_id=""
  if _ctr "$name" image inspect -f '{{.Id}}' "$image" &>/dev/null; then
    tenant_id="$(_ctr "$name" image inspect -f '{{.Id}}' "$image")"
  fi
  if [[ -n "$tenant_id" && "$tenant_id" == "$admin_id" ]]; then
    return 0   # tenant already has the current image
  fi
  [[ -n "$admin_id" ]] || return 1   # rootless, but the admin store has no source image to copy

  if [[ -n "$tenant_id" ]]; then
    say "refreshing stale image $image in ${name}'s rootless store (save|load — minutes for large images) ..."
  else
    say "distributing image $image into ${name}'s rootless store (save|load — minutes for large images) ..."
  fi
  if "$CONTAINER_RT" save "$image" | _ctr "$name" load >/dev/null 2>&1; then
    # Drop the previous (now-untagged) image if the load re-pointed the tag, so
    # repeated worker-image updates don't accumulate GBs of stale layers in the
    # tenant's rootless store.
    _ctr "$name" image prune -f >/dev/null 2>&1 || true
    say "image $image available in ${name}'s store"
    return 0
  fi
  say "WARNING: failed to load $image into ${name}'s store"
  return 1
}

# Render a dedicated OpenRC unit for a tenant's external worker (nanocode/pebble),
# modeled on the lunarwing-pg-<t> unit. Health-aware status() checks the worker's
# /health endpoint (curl is present in both worker images) so the host self-heal
# pipeline — which auto-discovers /etc/init.d/lunarwing-* units — can detect and
# remediate a crashed OR hung worker, and so it survives reboot. OpenRC only.
render_worker_openrc_unit() {
  local name="$1" worker="$2" health_port="${3:-8443}"
  ensure_container_runtime
  local runtime_bin="" container="lunarwing-${worker}-${name}" uid home
  [[ -n "${CONTAINER_RT:-}" ]] && runtime_bin="$(command -v "$CONTAINER_RT" 2>/dev/null || true)"
  uid="$(id -u "$name" 2>/dev/null || echo "")"
  home="$(tenant_home "$name")"

  cat >"/etc/init.d/${container}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing ${worker} worker ($name)"

: "\${wk_runtime:=$runtime_bin}"
: "\${wk_container:=$container}"
: "\${wk_rootless:=$MT_ROOTLESS}"
: "\${wk_user:=$name}"
: "\${wk_home:=$home}"
: "\${wk_uid:=$uid}"
: "\${wk_health_port:=$health_port}"
: "\${wk_wait:=60}"

depend() {
    need net localmount
    after firewall lunarwing-${name}
}

# Run the container runtime as the owning user (rootless) or root (rootful).
_wk() {
    if [ "\${wk_rootless}" = "true" ]; then
        sudo -u "\${wk_user}" env HOME="\${wk_home}" XDG_RUNTIME_DIR="/run/user/\${wk_uid}" "\${wk_runtime}" "\$@"
    else
        "\${wk_runtime}" "\$@"
    fi
}

# Healthy = container running AND its internal /health endpoint answers.
_wk_healthy() {
    [ "\$(_wk inspect -f '{{.State.Running}}' "\${wk_container}" 2>/dev/null)" = "true" ] || return 1
    _wk exec "\${wk_container}" curl -sf -o /dev/null --max-time 3 "http://127.0.0.1:\${wk_health_port}/health" 2>/dev/null
}

start() {
    [ -n "\${wk_runtime}" ] && [ -x "\${wk_runtime}" ] || { ewarn "no container runtime; skipping ${worker} for $name"; return 0; }
    ebegin "Starting ${worker} worker (\${wk_container})"
    if [ "\${wk_rootless}" = "true" ]; then
        checkpath -d -m 0700 -o "\${wk_user}:\${wk_user}" "/run/user/\${wk_uid}"
    fi
    _wk start "\${wk_container}" >/dev/null 2>&1 || { eend 1 "container start failed"; return 1; }
    _w=0
    while ! _wk_healthy; do
        _w=\$((_w + 1))
        [ "\$_w" -lt "\${wk_wait}" ] || { eend 1 "${worker} worker not healthy after \${wk_wait}s"; return 1; }
        sleep 1
    done
    eend 0
}

stop() {
    [ -n "\${wk_runtime}" ] && [ -x "\${wk_runtime}" ] || return 0
    ebegin "Stopping ${worker} worker (\${wk_container})"
    _wk stop --time 30 "\${wk_container}" >/dev/null 2>&1
    eend 0
}

status() {
    # Standard OpenRC started/stopped wording so health-openrc.sh classifies it.
    if _wk_healthy; then
        einfo "\${wk_container}: started"; return 0
    fi
    einfo "\${wk_container}: stopped"; return 3
}
INITEOF
  chmod 0755 "/etc/init.d/${container}"
}

# Promote a (rootless) worker container to a dedicated OpenRC unit so it is
# health-monitored, self-healed, and boot-persistent. No-op unless OpenRC.
_register_worker_unit() {
  local name="$1" worker="$2"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  render_worker_openrc_unit "$name" "$worker"
  rc-update add "lunarwing-${worker}-${name}" default >/dev/null 2>&1 || true
  rc-service "lunarwing-${worker}-${name}" start >/dev/null 2>&1 || true
  say "registered OpenRC unit lunarwing-${worker}-${name} (health-monitored, boot-persistent)"
  local container="lunarwing-${worker}-${name}"
  local uid home
  uid="$(id -u "$name" 2>/dev/null || echo "")"
  home="$(tenant_home "$name")"
  _register_babysitter "$name" "$worker" "$container" "$uid" "$home"
}

# Tear down a worker's OpenRC unit (boot-disable + remove the init script).
_deregister_worker_unit() {
  local name="$1" worker="$2"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  [[ -f "/etc/init.d/lunarwing-${worker}-${name}" ]] || return 0
  _deregister_babysitter "lunarwing-${worker}-${name}"
  rc-service "lunarwing-${worker}-${name}" stop >/dev/null 2>&1 || true
  rc-update del "lunarwing-${worker}-${name}" default >/dev/null 2>&1 || true
  rm -f "/etc/init.d/lunarwing-${worker}-${name}" "/etc/conf.d/lunarwing-${worker}-${name}"
}

# Install (or refresh) the babysitter helper binary from this repo to /usr/local/sbin.
# Idempotent: skips if the on-disk copy is byte-identical (mtime/perm-check), so
# `start_tenant` is safe to call repeatedly without churning the file. This
# decouples helper provisioning from the watchdog installer — a tenant created
# before the watchdog is installed still gets a working babysitter.
ensure_babysitter_helper() {
  local src="${SCRIPT_DIR}/lunarwing-ctr-babysit.sh"
  local dst="/usr/local/sbin/lunarwing-ctr-babysit"
  [[ -f "$src" ]] || {
    say "WARNING: babysitter helper source missing: $src (skipping install)"
    return 0
  }
  if [[ -f "$dst" ]] && cmp -s "$src" "$dst"; then
    return 0  # up to date; don't touch mtime/perm
  fi
  install -o root -g root -m 0755 "$src" "$dst" \
    && say "Installed babysitter helper: $dst" \
    || say "WARNING: failed to install babysitter helper to $dst"
}

# Render a supervised babysitter OpenRC unit for a rootless container.
# The babysitter blocks on `podman wait <container>` and respawns via supervise-daemon
# when the container exits, providing docker-parity crash recovery (~seconds, not minutes).
# Only applicable to rootless podman on OpenRC (systemd uses Quadlet).
# Usage: render_container_babysitter_unit <tenant> <container-type> <container-name> <uid> <home>
# Example: render_container_babysitter_unit acme pg lunarwing-pg-acme 1001 /home/acme
render_container_babysitter_unit() {
  local name="$1" type="$2" container="$3" uid="$4" home="$5"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  [[ "$MT_ROOTLESS" == "true" ]] || return 0
  # The pg path renders directly (bypassing _register_babysitter), so install the
  # helper here too — otherwise a worker-less tenant renders a -sup unit whose
  # required_files=<helper> never exists and supervise-daemon silently never starts it.
  ensure_babysitter_helper
  local babysitter="/etc/init.d/${container}-sup"
  # Log into the tenant's existing logs dir (created at add-tenant, tenant-owned),
  # exactly like every other unit (lunarwing/proxy/xmpp-bridge). The old
  # /var/log/lunarwing/<t> path had no parent on a fresh host, so the non-recursive
  # `checkpath -d` failed, supervise-daemon could not open output_log/error_log, and
  # the babysitter never stayed up (landed in /run/openrc/failed/).
  local log_dir
  log_dir="$(tenant_lw_root "$name")/logs"

  cat >"$babysitter" <<INITEOF
#!/sbin/openrc-run

description="LunarWing ${type} container babysitter ($name)"

: "\${babysitter_container:=$container}"
: "\${babysitter_user:=$name}"
: "\${babysitter_home:=$home}"
: "\${babysitter_uid:=$uid}"
: "\${babysitter_respawn_delay:=2}"
: "\${babysitter_respawn_max:=10}"
: "\${babysitter_respawn_period:=120}"
: "\${babysitter_log_dir:=$log_dir}"
: "\${babysitter_output_log:=\${babysitter_log_dir}/${type}-babysitter.log}"
: "\${babysitter_error_log:=\${babysitter_log_dir}/${type}-babysitter.err}"

supervisor="supervise-daemon"
command="/usr/local/sbin/lunarwing-ctr-babysit"
command_args="\${babysitter_container}"
command_user="\${babysitter_user}:\${babysitter_user}"
respawn_delay="\${babysitter_respawn_delay}"
respawn_max="\${babysitter_respawn_max}"
respawn_period="\${babysitter_respawn_period}"
output_log="\${babysitter_output_log}"
error_log="\${babysitter_error_log}"
required_files="\${command}"

depend() {
    need net localmount
    after firewall
}

start_pre() {
    checkpath -d -m 0700 -o "\${babysitter_user}:\${babysitter_user}" "/run/user/\${babysitter_uid}"
    checkpath -d -m 0750 -o "\${babysitter_user}:\${babysitter_user}" "\${babysitter_log_dir}"
    checkpath -f -m 0640 -o "\${babysitter_user}:\${babysitter_user}" "\${babysitter_output_log}"
    checkpath -f -m 0640 -o "\${babysitter_user}:\${babysitter_user}" "\${babysitter_error_log}"

    # Export rootless environment. Verified at runtime on OpenRC 0.63.1: across its
    # setuid, supervise-daemon re-sets HOME to the tenant's passwd home and leaves
    # XDG_RUNTIME_DIR untouched, so the supervised podman-wait process already runs
    # with HOME=/home/<t> + XDG_RUNTIME_DIR=/run/user/<uid>. These exports are belt-
    # and-suspenders; no sudo -u wrapper is needed. (No backticks in this heredoc:
    # it is unquoted, so backticks would be executed at render time.)
    if [ -n "\${babysitter_home}" ]; then
        export HOME="\${babysitter_home}"
    fi
    if [ -n "\${babysitter_uid}" ]; then
        export XDG_RUNTIME_DIR="/run/user/\${babysitter_uid}"
    fi
}
INITEOF
  chmod 0755 "$babysitter"
}

# Register a babysitter unit for a container (render + boot-enable + start).
# Idempotent: safe to call multiple times.
_register_babysitter() {
  local name="$1" type="$2" container="$3" uid="$4" home="$5"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  [[ "$MT_ROOTLESS" == "true" ]] || return 0
  # render_container_babysitter_unit installs the helper itself (covers pg + workers).
  render_container_babysitter_unit "$name" "$type" "$container" "$uid" "$home"
  rc-update add "${container}-sup" default >/dev/null 2>&1 || true
  rc-service "${container}-sup" start >/dev/null 2>&1 || true
}

# Deregister a babysitter unit (stop + boot-disable + remove).
_deregister_babysitter() {
  local container="$1"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  [[ -f "/etc/init.d/${container}-sup" ]] || return 0
  rc-service "${container}-sup" stop >/dev/null 2>&1 || true
  rc-update del "${container}-sup" default >/dev/null 2>&1 || true
  rm -f "/etc/init.d/${container}-sup"
}

# ── Port registry ────────────────────────────────────────────────────────────

ports_registry_init() {
  ports_registry_lock
  if [[ ! -d /etc/lunarwing ]]; then
    mkdir -p /etc/lunarwing
    chmod 0755 /etc/lunarwing
  fi

  if [[ ! -f "$PORTS_REGISTRY" ]]; then
    local tmp
    tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    cat >"$tmp" <<'ENDJSON'
{
  "version": 3,
  "range": { "start": 10000, "end": 19999 },
  "block_size": 10,
  "tenants": {}
}
ENDJSON
    chmod 0644 "$tmp"
    mv "$tmp" "$PORTS_REGISTRY"
    say "initialized port registry: $PORTS_REGISTRY"
  fi

  ports_migrate
  ports_registry_unlock
}

ports_registry_require_readonly() {
  require_cmd jq
  [[ -f "$PORTS_REGISTRY" && ! -L "$PORTS_REGISTRY" ]] \
    || die "port registry not found: $PORTS_REGISTRY"
  jq -e '.tenants | type == "object"' "$PORTS_REGISTRY" >/dev/null 2>&1 \
    || die "invalid port registry: $PORTS_REGISTRY"
}

ports_migrate_v2() {
  say "migrating port registry v1 -> v2 (reserved_0 -> orchestrator) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 2 |
    .tenants |= with_entries(
      .value.ports |= (
        if .reserved_0 then
          .orchestrator = .reserved_0 | del(.reserved_0)
        else
          .
        end
      )
    )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v2"
}

ports_migrate_v3() {
  say "migrating port registry v2 -> v3 (reserved_1 -> nanocode_wss) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 3 |
    .tenants |= with_entries(
      .value.ports |= (
        if .reserved_1 then
          .nanocode_wss = .reserved_1 | del(.reserved_1)
        else
          . + { nanocode_wss: (.orchestrator + 1) }
        end
      )
    )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v3"
}

ports_migrate_v4() {
  say "migrating port registry v3 -> v4 (reserved_2 -> pebble_wss) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 4 |
    .tenants |= with_entries(
      .value.ports |= (
        if .reserved_2 then
          .pebble_wss = .reserved_2 | del(.reserved_2)
        else
          . + { pebble_wss: (.orchestrator + 2) }
        end
      )
    )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v4"
}

ports_migrate_v5() {
  say "migrating port registry v4 -> v5 (reserved_3 -> weechat_adapter) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 5 |
    .tenants |= with_entries(
      .value.ports |= (
        if .reserved_3 then
          .weechat_adapter = .reserved_3 | del(.reserved_3)
        else
          . + { weechat_adapter: (.orchestrator + 3) }
        end
      )
    )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v5"
}

ports_migrate_v6() {
  say "migrating port registry v${current_version} -> v6 (add extended port range for overflow services) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 6
    | .extended_range = { "start": 20000, "end": 29999 }
    | .extended_block_size = (.block_size // 10)
    | ( .range.start // 10000 ) as $rstart
    | ( .extended_range.start ) as $estart
    | ( .extended_block_size ) as $bs
    | .tenants |= with_entries(
        .value |= (
          if .base_port then
            ( .base_port - $rstart + $estart ) as $eb
            | .extended_base = $eb
            | .extended_ports = (
                reduce range(0; $bs) as $i ({}; . + { ("reserved_\($i)"): ($eb + $i) })
              )
          else . end
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v6"
}

ports_migrate_v6_1() {
  # v6.1: assign darkirc_adapter to existing tenants from extended_ports[0].
  # Idempotent: detected by the absence of darkirc_adapter in extended_ports.
  if ! jq -e '.tenants | to_entries[] | select(.value.extended_ports | has("darkirc_adapter") | not)' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    return 0
  fi

  say "migrating port registry -> v6.1 (assign darkirc_adapter from extended slot 0) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .tenants |= with_entries(
      .value |= (
        if (.extended_ports | has("darkirc_adapter") | not) and (.extended_ports | type == "object") then
          .extended_ports.darkirc_adapter = .extended_ports.reserved_0
          | if (.extended_ports.reserved_0) then .extended_ports |= del(.reserved_0) else . end
        else . end
      )
    )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v6.1 (darkirc_adapter added)"
}

ports_migrate_v7() {
  say "migrating port registry v${current_version} -> v7 (assign darkirc_irc + darkirc_rpc from extended slots) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 7
    | .tenants |= with_entries(
        .value.extended_ports |= (
          if type == "object" then
            .darkirc_irc = ((.darkirc_irc) // (.reserved_1) // (.darkirc_adapter + 1))
            | .darkirc_rpc = ((.darkirc_rpc) // (.reserved_2) // (.darkirc_adapter + 2))
            | del(.reserved_1, .reserved_2)
          else . end
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  current_version=7
  say "port registry migrated to v7 (darkirc_irc + darkirc_rpc added)"
}

ports_migrate_v8() {
  say "migrating port registry v${current_version} -> v8 (dedicate nanocode_health + pebble_health) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 8
    | .tenants |= with_entries(
        .value |= (
          .extended_base as $eb
          | .extended_ports |= (
              if type == "object" then
                .nanocode_health = ((.nanocode_health) // (.reserved_3) // ($eb + 3))
                | .pebble_health = ((.pebble_health) // (.reserved_4) // ($eb + 4))
                | del(.reserved_3, .reserved_4)
              else . end
            )
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  current_version=8
}

ports_migrate() {
  [[ -f "$PORTS_REGISTRY" ]] || return 0
  require_cmd jq

  local current_version
  current_version="$(jq -r '.version // 0' "$PORTS_REGISTRY")"

  if [[ "$current_version" -lt 2 ]]; then ports_migrate_v2; current_version=2; fi
  if [[ "$current_version" -lt 3 ]]; then ports_migrate_v3; current_version=3; fi
  if [[ "$current_version" -lt 4 ]]; then ports_migrate_v4; current_version=4; fi
  if [[ "$current_version" -lt 5 ]]; then ports_migrate_v5; current_version=5; fi
  if [[ "$current_version" -lt 6 ]]; then ports_migrate_v6; current_version=6; fi
  ports_migrate_v6_1
  if [[ "$current_version" -lt 7 ]]; then ports_migrate_v7; fi
  if [[ "$current_version" -lt 8 ]]; then ports_migrate_v8; fi
  if [[ "$current_version" -lt 9 ]]; then ports_migrate_v9; fi
  if [[ "$current_version" -lt 10 ]]; then ports_migrate_v10; fi
  if [[ "$current_version" -lt 11 ]]; then ports_migrate_v11; fi
  ports_migrate_v12
}

# v11 -> v12: attach an immutable random scope ID to every DarkIRC-enabled
# tenant. This is also called unconditionally so mixed-version mt-admin copies
# repair a missing field without changing an existing ID.
ports_migrate_v12() {
  local name
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    if [[ "$(tenant_darkirc_enabled "$name" && echo true || echo false)" == "true" ]]; then
      ensure_darkirc_scope_id "$name" >/dev/null
    fi
  done < <(jq -r '.tenants // {} | keys[]' "$PORTS_REGISTRY" 2>/dev/null || true)
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '.version = 12' "$PORTS_REGISTRY" >"$tmp" || { rm -f "$tmp"; die "failed to migrate port registry to v12"; }
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
}

# v8 -> v9: rename extended_ports.reserved_5 -> vision_service.
ports_migrate_v9() {
  if jq -e '.tenants | to_entries[] | select(.value.extended_ports | has("vision_service") | not) | select(.value.extended_ports | has("reserved_5"))' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    say "migrating port registry -> v9 (assign vision_service from reserved_5 slot) ..."
    local tmp
    tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    jq '
      .tenants |= with_entries(
          .value |= (
            if (.extended_ports | type == "object") then
              .extended_ports |= (
                .vision_service = ((.vision_service) // (.reserved_5) // ((.extended_base // 0) + 5))
                | del(.reserved_5)
              )
            else . end
          )
        )
    ' "$PORTS_REGISTRY" >"$tmp"
    chmod 0644 "$tmp"
    mv "$tmp" "$PORTS_REGISTRY"
    say "port registry migrated to v9 (vision_service dedicated at extended_base+5)"
  fi

  # Always bump the version when the dispatcher calls v9, even if there were
  # no tenants to migrate (e.g. empty registry) — otherwise .version stays
  # stale and ports_allocate writes v9-shaped tenants into a v8-labeled file.
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '.version = 9' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
}

# v9 -> v10: dedicate extended_ports.reserved_6 -> vision_health.
# The vision sidecar now listens on two internal ports: 8088 (OCR_PORT) for
# API traffic and 8089 (OCR_HEALTH_PORT) for /health only. This dedicates a
# host-side port (extended_base+6) mapped to 8089 so the host self-heal
# pipeline can probe /health independently of OCR traffic — mirroring the
# nanocode_health / pebble_health pattern (v8).
ports_migrate_v10() {
  if jq -e '.tenants | to_entries[] | select(.value.extended_ports | has("vision_health") | not) | select(.value.extended_ports | has("reserved_6"))' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    say "migrating port registry -> v10 (assign vision_health from reserved_6 slot) ..."
    local tmp
    tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    jq '
      .tenants |= with_entries(
          .value |= (
            if (.extended_ports | type == "object") then
              .extended_ports |= (
                .vision_health = ((.vision_health) // (.reserved_6) // ((.extended_base // 0) + 6))
                | del(.reserved_6)
              )
            else . end
          )
        )
    ' "$PORTS_REGISTRY" >"$tmp"
    chmod 0644 "$tmp"
    mv "$tmp" "$PORTS_REGISTRY"
    say "port registry migrated to v10 (vision_health dedicated at extended_base+6)"
  fi

  # Always bump the version when the dispatcher calls v10, even if there were
  # no tenants to migrate (e.g. empty registry) — otherwise .version stays
  # stale and ports_allocate writes v10-shaped tenants into a v9-labeled file.
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '.version = 10' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
}

# v10 -> v11: dedicate extended_ports.reserved_7 -> opencode_wss,
# extended_ports.reserved_8 -> opencode_health.
ports_migrate_v11() {
  if jq -e '.tenants | to_entries[] | select(.value.extended_ports | has("opencode_wss") | not) | select(.value.extended_ports | has("reserved_7"))' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    say "migrating port registry -> v11 (assign opencode_wss from reserved_7 slot) ..."
    local tmp
    tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    jq '
      .tenants |= with_entries(
          .value |= (
            if (.extended_ports | type == "object") then
              .extended_ports |= (
                .opencode_wss = ((.opencode_wss) // (.reserved_7) // ((.extended_base // 0) + 7))
                | .opencode_health = ((.opencode_health) // (.reserved_8) // ((.extended_base // 0) + 8))
                | del(.reserved_7, .reserved_8)
              )
            else . end
          )
        )
    ' "$PORTS_REGISTRY" >"$tmp"
    chmod 0644 "$tmp"
    mv "$tmp" "$PORTS_REGISTRY"
    say "port registry migrated to v11 (opencode_wss + opencode_health dedicated at extended_base+7/+8)"
  fi

  # Always bump the version when the dispatcher calls v11, even if there were
  # no tenants to migrate (e.g. empty registry) — otherwise .version stays
  # stale and ports_allocate writes v11-shaped tenants into a v10-labeled file.
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '.version = 11' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
}

ports_allocate() {
  local name="$1"
  ports_registry_lock
  local enable_darkirc="${2:-false}"
  local enable_proxy="${3:-false}"
  # Selected external workers, as a JSON object literal
  # (e.g. '{"nanocode":true,"pebble":false,"opencode":false}'). Persisted so
  # start_tenant_<worker> can gate on the tenant's choice; see
  # tenant_worker_enabled. NOTE: assign in two steps — `${4:-{}}` mis-parses
  # (bash closes the ${...} at the first `}`, appending a stray `}` and
  # corrupting the JSON), so default explicitly instead.
  local workers_json="${4:-}"
  local requested_scope="${5:-}"
  [[ -n "$workers_json" ]] || workers_json='{}'
  require_cmd jq
  if [[ -n "$requested_scope" ]]; then
    [[ "$enable_darkirc" == true ]] || die "an explicit DarkIRC scope requires --enable-darkirc"
    validate_darkirc_scope_id "$requested_scope"
    darkirc_scope_is_unique "$name" "$requested_scope" \
      || die "darkirc_scope_id '$requested_scope' is already active for another tenant"
  fi

  # Resumable (F4): if this tenant already has a block, reuse it (echo its
  # base_port) instead of dying — so re-running add-tenant after a mid-flow failure
  # resumes cleanly (clone/env/pg/render/pipeline are idempotent, and
  # tenant_pg_password is stable). Use `remove-tenant` to truly start over.
  local existing
  existing="$(jq -r ".tenants[\"$name\"].base_port // empty" "$PORTS_REGISTRY" 2>/dev/null || true)"
  if [[ -n "$existing" ]]; then
    say "tenant '$name' already has ports allocated (base $existing); reusing for resume" >&2
    # Reconcile the darkirc flag on resume (H2): add_tenant()'s env/unit writers
    # (write_tenant_lunarwing_env, render_tenant_*_units, start_tenant_*) gate on
    # tenant_darkirc_enabled (a registry read), but the adapter env/TOML writers
    # at add_tenant() use the in-memory flag. Without this flip, re-running
    # `add-tenant <existing> --enable-darkirc` leaves the tenant half-configured.
    # One-directional (false -> true): disabling darkirc post-provision is a
    # manual teardown (see docs/ops/DARKIRC-MULTITENANT.md).
    [[ "$enable_darkirc" == "true" ]] && ports_enable_darkirc "$name" "$requested_scope"
    [[ "$enable_proxy" == "true" ]] && ports_enable_proxy "$name"
    # Reconcile worker selection on resume, same one-directional false->true rule
    # as darkirc/proxy: re-running `add-tenant <existing> --with-<worker>` turns a
    # worker on; omitting the flag never turns one off (disabling is manual).
    local _w
    for _w in nanocode pebble opencode; do
      if [[ "$(jq -r --arg w "$_w" '.[$w] // false' <<<"$workers_json" 2>/dev/null)" == "true" ]]; then
        ports_enable_worker "$name" "$_w"
      fi
    done
    printf '%s' "$existing"
    release_ports_lock
    return 0
  fi

  local base=-1
  local p
  for ((p = PORT_RANGE_START; p <= PORT_RANGE_END - PORT_BLOCK_SIZE + 1; p += PORT_BLOCK_SIZE)); do
    if ! jq -e ".tenants | to_entries[] | select(.value.base_port == $p)" "$PORTS_REGISTRY" >/dev/null 2>&1; then
      base=$p
      break
    fi
  done

  [[ $base -ge 0 ]] || die "no free port blocks in range $PORT_RANGE_START-$PORT_RANGE_END"

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  local darkirc_json="false"
  [[ "$enable_darkirc" == "true" ]] && darkirc_json="true"
  local darkirc_scope=""
  if [[ "$enable_darkirc" == "true" ]]; then
    darkirc_scope="${requested_scope:-$(generate_darkirc_scope_id)}"
  fi
  local proxy_json="false"
  [[ "$enable_proxy" == "true" ]] && proxy_json="true"
  # Normalize the workers spec to a full {nanocode,pebble,opencode} bool map so
  # every tenant object has a stable, explicit shape (missing/garbage -> all off).
  local workers_norm
  workers_norm="$(jq -c '{
      nanocode: (.nanocode // false),
      pebble:   (.pebble   // false),
      opencode: (.opencode // false)
    }' <<<"$workers_json" 2>/dev/null || echo '{"nanocode":false,"pebble":false,"opencode":false}')"
  jq --arg name "$name" --argjson base "$base" --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson darkirc "$darkirc_json" --argjson proxy "$proxy_json" --arg scope "$darkirc_scope" --argjson workers "$workers_norm" '
    ( .range.start // 10000 ) as $rstart
    | ( .extended_range.start // 20000 ) as $estart
    | ( .extended_block_size // 10 ) as $ebs
    | ( $base - $rstart + $estart ) as $ebase
    | .tenants[$name] = {
        base_port: $base,
        user: $name,
        created_at: $ts,
        enable_darkirc: $darkirc,
        darkirc_scope_id: (if $darkirc then $scope else null end),
        enable_proxy: $proxy,
        workers: $workers,
        ports: {
          gateway:          ($base + 0),
          http:             ($base + 1),
          bridge:           ($base + 2),
          postgres:         ($base + 3),
          proxy:            ($base + 4),
          weechat:          ($base + 5),
          orchestrator:     ($base + 6),
          nanocode_wss:     ($base + 7),
          pebble_wss:       ($base + 8),
          weechat_adapter:  ($base + 9)
        },
        extended_base: $ebase,
        extended_ports: (
          {
            darkirc_adapter: $ebase,
            darkirc_irc: ($ebase + 1),
            darkirc_rpc: ($ebase + 2),
            nanocode_health: ($ebase + 3),
            pebble_health: ($ebase + 4),
            vision_service: ($ebase + 5),
            vision_health: ($ebase + 6),
            opencode_wss: ($ebase + 7),
            opencode_health: ($ebase + 8)
          }
          + (reduce range(9; $ebs) as $i ({}; . + { ("reserved_\($i)"): ($ebase + $i) }))
        )
      }
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"

  say "allocated port block $base-$((base + PORT_BLOCK_SIZE - 1)) for tenant '$name'" >&2
  printf '%s' "$base"
  release_ports_lock
}

# Mark darkirc enabled for a tenant in the registry (one-directional: false -> true).
# Idempotent: no-op if already enabled. Used by ports_allocate()'s resume path so
# `add-tenant <existing> --enable-darkirc` flips the registry (H2). Does NOT tear
# down darkirc (disabling is a manual operation; see DARKIRC-MULTITENANT.md).
ports_enable_darkirc() {
  local name="$1"
  local requested_scope="${2:-}"
  ports_registry_lock
  require_cmd jq
  local current
  current="$(jq -r ".tenants[\"$name\"].enable_darkirc // false" "$PORTS_REGISTRY" 2>/dev/null || true)"
  if [[ "$current" == "true" ]]; then
    if [[ -n "$requested_scope" ]]; then
      validate_darkirc_scope_id "$requested_scope"
      [[ "$(darkirc_scope_id "$name")" == "$requested_scope" ]] \
        || die "tenant '$name' already has a different DarkIRC scope ID"
    else
      ensure_darkirc_scope_id "$name" >/dev/null
    fi
    return 0
  fi

  local tmp existing_scope scope_id
  existing_scope="$(darkirc_scope_id "$name")"
  if [[ -n "$existing_scope" ]]; then
    validate_darkirc_scope_id "$existing_scope" \
      || die "tenant '$name' has an invalid existing DarkIRC scope ID"
    if [[ -n "$requested_scope" && "$requested_scope" != "$existing_scope" ]]; then
      die "tenant '$name' already has a different DarkIRC scope ID"
    fi
  fi
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  scope_id="${requested_scope:-${existing_scope:-$(generate_darkirc_scope_id)}}"
  validate_darkirc_scope_id "$scope_id"
  darkirc_scope_is_unique "$name" "$scope_id" \
    || die "darkirc_scope_id '$scope_id' is already active for another tenant"
  if ! jq --arg name "$name" --arg scope "$scope_id" '.tenants[$name].enable_darkirc = true | .tenants[$name].darkirc_scope_id = $scope' \
      "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    die "failed to enable darkirc flag for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "darkirc for tenant '$name': disabled -> enabled" >&2
}

# Mark proxy enabled for a tenant in the registry (one-directional: false -> true).
# Idempotent. Used by ports_allocate()'s resume path and by the --enable-proxy flag.
ports_enable_proxy() {
  local name="$1"
  ports_registry_lock
  require_cmd jq
  local current
  current="$(jq -r ".tenants[\"$name\"].enable_proxy // false" "$PORTS_REGISTRY" 2>/dev/null || true)"
  [[ "$current" == "true" ]] && return 0

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" '.tenants[$name].enable_proxy = true' \
      "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    die "failed to enable proxy flag for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "proxy for tenant '$name': disabled -> enabled" >&2
}

# Mark a single external worker (nanocode|pebble|opencode) enabled for a tenant
# (one-directional: false -> true). Idempotent. Used by ports_allocate()'s resume
# path so `add-tenant <existing> --with-<worker>` flips the registry, mirroring
# ports_enable_darkirc/proxy. Ensures a .workers object exists on older tenant
# entries that predate the workers map.
ports_enable_worker() {
  local name="$1" worker="$2"
  ports_registry_lock
  require_cmd jq
  case "$worker" in
    nanocode|pebble|opencode) ;;
    *) die "ports_enable_worker: unknown worker '$worker'" ;;
  esac
  local current
  current="$(jq -r ".tenants[\"$name\"].workers[\"$worker\"] // false" "$PORTS_REGISTRY" 2>/dev/null || true)"
  [[ "$current" == "true" ]] && return 0

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" --arg w "$worker" '
      .tenants[$name].workers = ((.tenants[$name].workers // {}) + { ($w): true })
    ' "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    die "failed to enable worker '$worker' for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "worker '$worker' for tenant '$name': disabled -> enabled" >&2
}

ports_deallocate() {
  local name="$1"
  require_cmd jq
  acquire_ports_lock

  if ! jq -e ".tenants[\"$name\"]" "$PORTS_REGISTRY" >/dev/null 2>&1; then
    say "tenant '$name' not in port registry (already removed?)"
    release_ports_lock
    return 0
  fi

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq --arg name "$name" 'del(.tenants[$name])' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"

  say "deallocated ports for tenant '$name'"
  release_ports_lock
}

ports_get() {
  local name="$1" port_name="$2"
  require_cmd jq
  local port
  port="$(jq -r ".tenants[\"$name\"].ports.$port_name // empty" "$PORTS_REGISTRY")"
  if [[ -n "$port" ]]; then
    printf '%s' "$port"
    return 0
  fi
  port="$(jq -r ".tenants[\"$name\"].extended_ports.$port_name // empty" "$PORTS_REGISTRY")"
  if [[ -n "$port" ]]; then
    printf '%s' "$port"
    return 0
  fi
  # Back-compat: pre-v9 registries had `reserved_5` where `vision_service` now
  # lives. Read either name; new tenants only ever carry `vision_service`.
  if [[ "$port_name" == "vision_service" ]]; then
    port="$(jq -r ".tenants[\"$name\"].extended_ports.reserved_5 // empty" "$PORTS_REGISTRY")"
    [[ -n "$port" ]] && { printf '%s' "$port"; return 0; }
  fi
  # Back-compat: pre-v11 registries had `reserved_7`/`reserved_8` where
  # `opencode_wss`/`opencode_health` now live (renamed by ports_migrate_v11).
  # Read either name so an unmigrated tenant's opencode worker still resolves
  # its port instead of being skipped — mirrors the vision_service case above.
  if [[ "$port_name" == "opencode_wss" ]]; then
    port="$(jq -r ".tenants[\"$name\"].extended_ports.reserved_7 // empty" "$PORTS_REGISTRY")"
    [[ -n "$port" ]] && { printf '%s' "$port"; return 0; }
  fi
  if [[ "$port_name" == "opencode_health" ]]; then
    port="$(jq -r ".tenants[\"$name\"].extended_ports.reserved_8 // empty" "$PORTS_REGISTRY")"
    [[ -n "$port" ]] && { printf '%s' "$port"; return 0; }
  fi
  return 1
}

ports_list() {
  require_cmd jq
  if [[ ! -f "$PORTS_REGISTRY" ]]; then
    say "no port registry found; run add-tenant first"
    return 0
  fi
  jq -r '.tenants | to_entries[] | "\(.key)\t\(.value.ports.gateway)\t\(.value.ports.http)\t\(.value.ports.bridge)\t\(.value.ports.postgres)\t\(.value.ports.proxy)\t\(.value.ports.weechat)\t\(.value.ports.weechat_adapter // "-")\t\(.value.extended_ports.darkirc_adapter // "-")\t\(.value.extended_ports.darkirc_irc // "-")\t\(.value.extended_ports.darkirc_rpc // "-")\t\(.value.ports.orchestrator)\t\(.value.ports.nanocode_wss // "-")\t\(.value.ports.pebble_wss // "-")\t\(.value.extended_ports.vision_service // "-")\t\(.value.extended_ports.vision_health // "-")"' "$PORTS_REGISTRY" \
    | column -t -N "TENANT,GATEWAY,HTTP,BRIDGE,PG,PROXY,WEECHAT,WS_ADPT,DARKIRC_ADPT,DARKIRC_IRC,DARKIRC_RPC,ORCH,NANOCODE,PEBBLE,VISION_SVC,VISION_HLTH"
}

tenant_exists_in_registry() {
  local name="$1"
  require_cmd jq
  jq -e ".tenants[\"$name\"]" "$PORTS_REGISTRY" >/dev/null 2>&1
}

all_tenant_names() {
  require_cmd jq
  jq -r '.tenants | keys[]' "$PORTS_REGISTRY" 2>/dev/null
}

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
# stable default, wasm32-wasip1/wasip2 targets, cargo-component, wasm-tools).
# Called from create_tenant_user (add-tenant) and as a build-tenant --with-wasm
# preflight, so a partial install (network hiccup, killed add-tenant) is
# repaired instead of silently disabling WASM builds forever.
# Returns 0 when the toolchain is usable, 1 (after a loud warning) when not.
ensure_tenant_wasm_toolchain() {
  local name="$1"
  local cargo_src='if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; else export PATH="$HOME/.cargo/bin:$PATH"; fi;'

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

  # Ensure WASM targets and cargo-component are installed
  say "ensuring WASM toolchain for $name ..."
  sudo -u "$name" bash -c "${cargo_src} rustup target add wasm32-wasip1 wasm32-wasip2 2>&1" || true
  if ! sudo -u "$name" bash -c "${cargo_src} command -v cargo-component" &>/dev/null; then
    say "installing cargo-component and wasm-tools for $name ..."
    sudo -u "$name" bash -c "${cargo_src} cargo install cargo-component wasm-tools --locked 2>&1" || true
  fi

  # Final verification — loud, actionable, never fatal.
  local missing=()
  sudo -u "$name" bash -c "${cargo_src} command -v cargo-component" &>/dev/null || missing+=("cargo-component")
  sudo -u "$name" bash -c "${cargo_src} rustup target list --installed 2>/dev/null | grep -q wasm32-wasip2" \
    || missing+=("wasm32-wasip2 target")
  if ((${#missing[@]} > 0)); then
    say "" >&2
    say "WARNING: WASM toolchain incomplete for $name — missing: ${missing[*]}" >&2
    say "         WASM extensions will NOT build. To fix, run:" >&2
    say "           sudo -u $name bash -lc 'rustup target add wasm32-wasip1 wasm32-wasip2'" >&2
    say "           sudo -u $name bash -lc 'cargo install cargo-component wasm-tools --locked'" >&2
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
  local repo
  repo="$(tenant_repo "$name")"

  [[ -d "$repo" ]] || die "repo not found at $repo; run add-tenant first"

  local darkirc_enabled=false
  if tenant_darkirc_enabled "$name"; then
    darkirc_enabled=true
    # The helper is a shared root trust anchor, so build it from the admin
    # checkout only when this tenant actually needs DarkIRC configuration.
    install_trusted_darkirc_key_helper
  fi

  say "acquiring build lock (only one tenant builds at a time) ..."
  (
    flock -x 200

    local cargo_env="if [ -f \"\$HOME/.cargo/env\" ]; then . \"\$HOME/.cargo/env\"; else export PATH=\"\$HOME/.cargo/bin:\$PATH\"; fi;"

    say "building lunarwing for $name ..."
    sudo -u "$name" bash -c "$cargo_env cd '$repo' && taskset -c 0-5 cargo build --profile $PROFILE -j6 --bin lunarwing" \
      || die "lunarwing build failed for $name"

    if [[ "$darkirc_enabled" == true ]]; then
      say "updating DarkIRC config through the typed helper ..."
      generate_darkirc_config "$name"
    fi

    say "building xmpp-bridge for $name ..."
    sudo -u "$name" bash -c "$cargo_env cd '$repo/bridges/xmpp-bridge' && cargo build --profile $PROFILE" \
      || die "xmpp-bridge build failed for $name"

    if [[ "$with_wasm" == "true" ]]; then
      if ensure_tenant_wasm_toolchain "$name"; then
        say "building WASM extensions for $name ..."
        sudo -u "$name" bash -c "$cargo_env cd '$repo' && bash scripts/build-wasm-extensions.sh" \
          || say "WARNING: WASM extension build reported errors for $name (some extensions may be missing; re-run '$0 build-tenant $name --with-wasm' after fixing)" >&2

        say "installing WASM extensions for $name ..."
        install_wasm_tenant "$name"
      else
        say "WARNING: skipping WASM build for $name (toolchain incomplete — see above)" >&2
      fi
    fi

    say "build complete for $name"
    say ""
    say "Reminder: set your LLM provider API key (if applicable to your backend) in $(tenant_env_dir "$name")/lunarwing.env"
    say "  e.g.  LLM_API_KEY=sk-..."
  ) 200>"$BUILD_LOCK"

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
  local git_bin make_bin cargo_bin rustc_bin taskset_bin
  prepare_darkirc_build_root
  git_bin="$(trusted_darkirc_tool_path git)"
  make_bin="$(trusted_darkirc_tool_path make)"
  cargo_bin="$(trusted_darkirc_tool_path cargo)"
  rustc_bin="$(trusted_darkirc_tool_path rustc)"
  taskset_bin="$(trusted_darkirc_tool_path taskset)"
  darkirc_validate_root_executable /usr/bin/env \
    || die "DarkIRC build requires a trusted /usr/bin/env"

  say "acquiring build lock for darkirc ..."
  (
    flock -x 200
    local work_dir source_dir built_bin pinned_bin=""
    work_dir="$(mktemp -d "$DARKIRC_BUILD_ROOT/build.XXXXXX")"
    source_dir="$work_dir/source"
    trap 'rm -rf -- "$work_dir"; [[ -z "$pinned_bin" ]] || rm -f -- "$pinned_bin"' EXIT

    say "cloning approved DarkIRC source revision into the root-controlled build workspace ..."
    /usr/bin/env -i \
      HOME="$DARKIRC_BUILD_ROOT/home" \
      PATH="$DARKIRC_TRUSTED_PATH" \
      USER=root LOGNAME=root \
      "$git_bin" clone --no-checkout -- "$DARKIRC_REPO" "$source_dir" \
      || die "darkirc clone failed"
    /usr/bin/env -i \
      HOME="$DARKIRC_BUILD_ROOT/home" \
      PATH="$DARKIRC_TRUSTED_PATH" \
      USER=root LOGNAME=root \
      "$git_bin" -C "$source_dir" checkout --detach "$DARKIRC_REV" \
      || die "darkirc checkout '$DARKIRC_REV' failed"
    validate_darkirc_source_checkout "$source_dir" "$git_bin"

    say "building approved DarkIRC source with the trusted system toolchain ..."
    /usr/bin/env -i \
      HOME="$DARKIRC_BUILD_ROOT/home" \
      PATH="$DARKIRC_TRUSTED_PATH" \
      USER=root LOGNAME=root \
      CARGO="$cargo_bin" RUSTC="$rustc_bin" \
      CARGO_HOME="$DARKIRC_BUILD_ROOT/cargo-home" \
      CARGO_BUILD_JOBS=6 \
      "$taskset_bin" -c 0-5 "$make_bin" -C "$source_dir" darkirc \
      || die "darkirc build failed"

    built_bin="$source_dir/darkirc"
    [[ -x "$built_bin" ]] || die "darkirc binary not found at $built_bin"
    pinned_bin="$(pin_darkirc_build_candidate "$built_bin")"
    validate_darkirc_source_checkout "$source_dir" "$git_bin"
    validate_darkirc_build_candidate "$pinned_bin"

    # Validation and installation consume the same root-owned pinned copy.
    say "installing darkirc to $DARKIRC_BIN ..."
    install_darkirc_binary_atomic "$pinned_bin"

    record_darkirc_compatibility
    say "darkirc build complete: $DARKIRC_BIN"
    rm -f -- "$pinned_bin"
    pinned_bin=""
    rm -rf -- "$work_dir"
    trap - EXIT
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

# ── Environment file generation ──────────────────────────────────────────────

# Echo the existing VALUE of KEY from a tenant env file (empty if file/key absent).
# Used to PRESERVE secrets across re-runs so re-provisioning never rotates them.
_env_existing() {  # <env_file> <KEY>
  [[ -f "$1" ]] || return 0
  sed -n "s/^$2=//p" "$1" | head -1
}

# Build the XMPP_ALLOW_FROM comma-separated value: owner JID first, then any
# extra JIDs from --xmpp-allow-from (comma-separated), deduped (owner JID and
# duplicate extras collapse), surrounding whitespace trimmed. <owner_jid> may
# be empty only in error paths; <extras_csv> is the raw flag value.
build_xmpp_allow_from() {  # <owner_jid> <extras_csv>
  local owner="$1"
  local extras="$2"
  local seen=""
  local result=""
  local jid
  # Owner first (skip if empty, though it normally isn't). `seen` uses leading
  # and trailing commas so substring matching on ",<jid>," is unambiguous.
  if [[ -n "$owner" ]]; then
    result="$owner"
    seen=",$owner,"
  fi
  # Extras: split on comma, trim whitespace, dedupe
  if [[ -n "$extras" ]]; then
    local IFS=','
    read -ra parts <<< "$extras"
    for jid in "${parts[@]}"; do
      jid="$(echo "$jid" | xargs)"   # trim leading/trailing whitespace
      [[ -n "$jid" ]] || continue
      [[ "$seen" == *",$jid,"* ]] && continue
      result="${result:+$result,}$jid"
      seen="${seen}$jid,"
    done
  fi
  echo "$result"
}

# Build the XMPP_ALLOW_FROM_JSON value: same semantics as build_xmpp_allow_from
# but emits a JSON array. Each JID is wrapped in double quotes; no escaping is
# applied (XMPP JIDs do not contain characters that require JSON escaping under
# the XEP-0029 node/domain rules in normal use). Output is a single line.
build_xmpp_allow_from_json() {  # <owner_jid> <extras_csv>
  local owner="$1"
  local extras="$2"
  local csv
  csv="$(build_xmpp_allow_from "$owner" "$extras")"
  local IFS=','
  local parts=()
  [[ -n "$csv" ]] && read -ra parts <<< "$csv"
  local quoted=()
  local jid
  for jid in "${parts[@]}"; do
    quoted+=("\"$jid\"")
  done
  echo "[${quoted[*]}]" | tr ' ' ',' | sed 's/,,*/,/g; s/^\[,/[/; s/,\]$/\]/'
}

# ── WeeChat relay auto-bootstrap ──────────────────────────────────────────────

# WeeChat home directory for a tenant.
tenant_weechat_home() {  # <tenant>
  printf '%s/.config/weechat' "$(tenant_home "$1")"
}

# Read the LAST literal value of KEY from an env file without sourcing it.
# Returns empty if file or key is absent. Never prints the file's other content.
_read_env_value() {  # <file> <KEY>
  [[ -f "$1" ]] || return 0
  sed -n "s/^$2=//p" "$1" | tail -1
}

# Success when <dir> contains any entry (normal or dotfile).
_weechat_config_dir_has_entries() {  # <dir>
  [[ -d "$1" ]] || return 1
  local count
  count="$(ls -A "$1" 2>/dev/null | wc -l)"
  [[ "$count" -gt 0 ]]
}

# Validate a generated WeeChat relay config directory.
# Requires relay.conf with the literal ${env:RELAY_PASSWORD} expression,
# bind_address=127.0.0.1, ipv6=off, allow_empty_password=off, [api] section with
# api=<port>, and rejects any occurrence of the plaintext password.
_weechat_validate_relay_config() {  # <dir> <port> <plaintext_password>
  local dir="$1" port="$2" plaintext="$3"
  local conf="$dir/relay.conf"
  [[ -f "$conf" ]] || return 1
  # Reject plaintext password leak
  if [[ -n "$plaintext" ]] && grep -qF -- "$plaintext" "$conf"; then
    return 1
  fi
  # Require literal env expression
  grep -qF '${env:RELAY_PASSWORD}' "$conf" || return 1
  # Require loopback bind
  grep -q 'bind_address.*127\.0\.0\.1' "$conf" || return 1
  # Require allow_empty_password = off
  grep -q 'allow_empty_password.*off' "$conf" || return 1
  # An IPv4 loopback bind is invalid while WeeChat's relay IPv6 mode is enabled.
  grep -q '^[[:space:]]*ipv6[[:space:]]*=[[:space:]]*off[[:space:]]*$' "$conf" || return 1
  # Require [api] section
  grep -q '\[api\]' "$conf" || return 1
  # Require api = <port>
  grep -q "^[[:space:]]*api[[:space:]]*=[[:space:]]*${port}\$" "$conf" || return 1
  return 0
}

# Invoke WeeChat as the tenant to generate a relay.conf in <temp_dir>.
# The plaintext password is read from <lunarwing_env> and passed via
# environment inheritance only; never in argv.
_weechat_generate_relay_config() {  # <tenant> <temp_dir> <port> <lunarwing_env>
  local tenant="$1" temp_dir="$2" relay_port="$3" lunarwing_env="$4"
  local relay_password weechat_bin
  relay_password="$(_read_env_value "$lunarwing_env" RELAY_PASSWORD)"
  [[ -n "$relay_password" ]] || return 1
  weechat_bin="$(command -v weechat 2>/dev/null || true)"
  if [[ -z "$weechat_bin" ]]; then
    printf 'error: required command not found: weechat\n' >&2
    return 1
  fi
  # The backslash before ${env:RELAY_PASSWORD} is mandatory: WeeChat evaluates
  # --run-command arguments before /set stores the value, so the escape ensures
  # the literal expression is written to relay.conf, not the resolved password.
  RELAY_PASSWORD="$relay_password" sudo --preserve-env=RELAY_PASSWORD -u "$tenant" \
    "$weechat_bin" --dir "$temp_dir" \
    --run-command '/set relay.network.password "\${env:RELAY_PASSWORD}"' \
    --run-command '/set relay.network.allow_empty_password off' \
    --run-command '/set relay.network.ipv6 off' \
    --run-command '/set relay.network.bind_address "127.0.0.1"' \
    --run-command "/relay add api ${relay_port}" \
    --run-command '/set weechat.look.save_layout_on_exit buffers' \
    --run-command '/save' \
    --run-command '/quit'
}

# Full one-shot relay bootstrap for a tenant: generate, validate, atomically
# promote. Rejects non-empty existing config. Cleans all failure paths.
configure_weechat_relay() {  # <tenant>
  local tenant="$1"
  local target_home relay_port lunarwing_env relay_password

  target_home="$(tenant_weechat_home "$tenant")"
  relay_port="$(ports_get "$tenant" weechat)"
  if [[ -z "$relay_port" || "$relay_port" == "0" ]]; then
    printf 'error: no weechat port allocated for tenant %q\n' "$tenant" >&2
    return 1
  fi
  lunarwing_env="$(tenant_env_dir "$tenant")/lunarwing.env"
  if [[ ! -f "$lunarwing_env" ]]; then
    printf 'error: tenant env file not found: %s\n' "$lunarwing_env" >&2
    return 1
  fi
  relay_password="$(_read_env_value "$lunarwing_env" RELAY_PASSWORD)"
  if [[ -z "$relay_password" ]]; then
    printf 'error: RELAY_PASSWORD missing from %s\n' "$lunarwing_env" >&2
    return 1
  fi

  # Invariant: the weechat.env credential file must exist before the
  # preserve-and-fail check so legacy existing configs receive the file
  # required by the rendered systemd/OpenRC service unit.
  #
  # Non-empty legacy config (preserve-and-fail): backfill weechat.env only
  # when it does not exist yet, then return 1 — existing env values must
  # survive alongside the existing config.
  local weechat_env_file
  weechat_env_file="$(tenant_env_dir "$tenant")/weechat.env"
  if [[ -d "$target_home" ]] && _weechat_config_dir_has_entries "$target_home"; then
    if [[ ! -f "$weechat_env_file" ]]; then
      if ! _write_weechat_env "$tenant" "$relay_password"; then
        printf 'error: unable to write the dedicated WeeChat credential environment\n' >&2
        return 1
      fi
    fi
    printf 'error: WeeChat config already exists at %s — configure-weechat-relay never overwrites existing config\n' "$target_home" >&2
    return 1
  fi

  # Fresh generation path: atomically synchronize weechat.env to the current
  # lunarwing.env password before generating config. This overwrites stale
  # values left from a prior tenant or aborted run.
  if ! _write_weechat_env "$tenant" "$relay_password"; then
    printf 'error: unable to write the dedicated WeeChat credential environment\n' >&2
    return 1
  fi

  # Temp directory as a sibling of the target (same filesystem for atomic rename).
  local config_parent temp_home
  config_parent="$(dirname "$target_home")"
  if ! mkdir -p "$config_parent" || ! chown "$tenant:$tenant" "$config_parent"; then
    printf 'error: unable to prepare WeeChat config parent: %s\n' "$config_parent" >&2
    return 1
  fi
  temp_home="$(mktemp -d "${config_parent}/.weechat-bootstrap.XXXXXX")" || {
    printf 'error: unable to create WeeChat bootstrap directory under %s\n' "$config_parent" >&2
    return 1
  }

  (
    trap '[[ -n "${temp_home:-}" && -d "$temp_home" ]] && rm -rf -- "$temp_home"' EXIT
    if ! chown "$tenant:$tenant" "$temp_home"; then
      return 1
    fi

    # Generate via WeeChat one-shot.
    if ! _weechat_generate_relay_config "$tenant" "$temp_home" "$relay_port" "$lunarwing_env"; then
      return 1
    fi

    # Validate the generated config.
    if ! _weechat_validate_relay_config "$temp_home" "$relay_port" "$relay_password"; then
      say "generated WeeChat relay config failed validation"
      return 1
    fi

    # Atomically promote: remove empty target if present, then rename.
    if [[ -d "$target_home" ]]; then
      if ! rmdir "$target_home" 2>/dev/null; then
        printf 'error: target %s exists and is not empty (cannot promote)\n' "$target_home" >&2
        return 1
      fi
    fi
    if ! mv -T -- "$temp_home" "$target_home"; then
      return 1
    fi
    temp_home=""
  )

  local rc=$?
  if [[ $rc -ne 0 ]]; then
    return $rc
  fi

  if ! chown -R "$tenant:$tenant" "$target_home"; then
    printf 'error: unable to set WeeChat config ownership: %s\n' "$target_home" >&2
    return 1
  fi
  say "WeeChat relay configured for tenant '$tenant' (port $relay_port, loopback)"
}

# Write a dedicated mode-0600 weechat.env containing only RELAY_PASSWORD.
# Uses a temp file in the same directory then atomically renames.
_write_weechat_env() {  # <tenant> <relay_password>
  local tenant="$1" relay_password="$2"
  local env_dir env_file tmp_file
  env_dir="$(tenant_env_dir "$tenant")"
  env_file="$env_dir/weechat.env"
  mkdir -p "$env_dir" || return 1
  tmp_file="$(mktemp "$env_dir/.weechat.env.XXXXXX")" || return 1
  (
    trap '[[ -n "${tmp_file:-}" && -f "$tmp_file" ]] && rm -f -- "$tmp_file"' EXIT
    umask 077
    printf 'RELAY_PASSWORD=%s\n' "$relay_password" >"$tmp_file" || return 1
    chmod 0600 "$tmp_file" || return 1
    chown "$tenant:$tenant" "$tmp_file" || return 1
    mv -T -- "$tmp_file" "$env_file" || return 1
    tmp_file=""
  )
}

write_tenant_lunarwing_env() {
  local name="$1"
  local xmpp_jid="${2:-$name@xmpp.localhost}"
  local xmpp_password="${3:-}"   # resolved below (preserve an existing one on re-run)
  local tensorzero_url="${4:-$DEFAULT_TENSORZERO_URL}"
  local llm_api_key="${5:-}"
  local llm_base_url="${6:-}"
  local nanocode_model="${7:-}"
  local nanocode_base_url="${8:-}"
  local llm_model="${9:-}"
  local gateway_host="${10:-}"
  local xmpp_allow_from="${11:-}"
  local opencode_model="${12:-}"
  local opencode_base_url="${13:-}"

  local path gateway_port http_port bridge_port pg_port proxy_port weechat_port weechat_adapter_port orchestrator_port nanocode_wss_port pebble_wss_port opencode_wss_port
  path="$(tenant_env_dir "$name")/lunarwing.env"
  gateway_port="$(ports_get "$name" gateway)"
  http_port="$(ports_get "$name" http)"
  bridge_port="$(ports_get "$name" bridge)"
  pg_port="$(ports_get "$name" postgres)"
  proxy_port="$(ports_get "$name" proxy)"
  weechat_port="$(ports_get "$name" weechat)"
  weechat_adapter_port="$(ports_get "$name" weechat_adapter)"
  orchestrator_port="$(ports_get "$name" orchestrator)"
  nanocode_wss_port="$(ports_get "$name" nanocode_wss)"
  pebble_wss_port="$(ports_get "$name" pebble_wss)"
  opencode_wss_port="$(ports_get "$name" opencode_wss)"

  local state_dir run_dir
  state_dir="$(tenant_state_dir "$name")"
  run_dir="$(tenant_run_dir "$name")"

  # Idempotent on re-run (F4-A/F4-B): PRESERVE existing secrets when lunarwing.env
  # already exists. Regenerating SECRETS_MASTER_KEY would permanently orphan the
  # tenant's encrypted DB secrets (it is the AES-256-GCM vault key); rotating the
  # tokens would break live clients/workers; minting a fresh XMPP_PASSWORD would
  # break the already-registered XMPP account. Generate fresh ONLY on first write.
  local gateway_token bridge_token relay_password secrets_key webhook_secret pg_password
  local darkirc_adapter_secret=""
  gateway_token="$(_env_existing "$path" GATEWAY_AUTH_TOKEN)";   gateway_token="${gateway_token:-$(generate_token)}"
  bridge_token="$(_env_existing "$path" XMPP_BRIDGE_TOKEN)";     bridge_token="${bridge_token:-$(generate_token | cut -c1-32)}"
  relay_password="$(_env_existing "$path" RELAY_PASSWORD)";      relay_password="${relay_password:-$(generate_token | cut -c1-32)}"
  secrets_key="$(_env_existing "$path" SECRETS_MASTER_KEY)";     secrets_key="${secrets_key:-$(generate_token)}"
  webhook_secret="$(_env_existing "$path" HTTP_WEBHOOK_SECRET)"; webhook_secret="${webhook_secret:-$(generate_token)}"
  if tenant_darkirc_enabled "$name"; then
    darkirc_adapter_secret="$(_env_existing "$path" DARKIRC_ADAPTER_SECRET)"
  fi
  # XMPP password: an explicit --xmpp-password wins; else preserve an existing one;
  # else mint a fresh one (first-time provision).
  [[ -n "$xmpp_password" ]] || { xmpp_password="$(_env_existing "$path" XMPP_PASSWORD)"; xmpp_password="${xmpp_password:-$(generate_token | cut -c1-32)}"; }
  # Nanocode model/base_url overrides: an explicit flag wins; else preserve an
  # existing value so re-running add-tenant without the flags keeps prior settings.
  [[ -n "$nanocode_model" ]]    || nanocode_model="$(_env_existing "$path" NANOCODE_MODEL)"
  [[ -n "$nanocode_base_url" ]] || nanocode_base_url="$(_env_existing "$path" NANOCODE_BASE_URL)"
  [[ -n "$opencode_model" ]]    || opencode_model="$(_env_existing "$path" OPENCODE_MODEL)"
  [[ -n "$opencode_base_url" ]] || opencode_base_url="$(_env_existing "$path" OPENCODE_BASE_URL)"
  # LLM circuit breaker: fleet defaults (open after 7 fully-retried failures,
  # probe again after 45s). These are tunables, so preserve an operator's
  # per-tenant override across re-provision rather than clobbering it.
  local cb_threshold cb_recovery
  cb_threshold="$(_env_existing "$path" LLM_CIRCUIT_BREAKER_THRESHOLD)";  cb_threshold="${cb_threshold:-7}"
  cb_recovery="$(_env_existing "$path" LLM_CIRCUIT_BREAKER_RECOVERY_SECS)"; cb_recovery="${cb_recovery:-45}"
  # Stable + migration-safe; resolved before the heredoc so it can read an
  # existing DATABASE_URL (preserving an already-initialised DB's password).
  pg_password="$(tenant_pg_password "$name")"

  # Read this before the env file is replaced.  The adapter credential is an
  # existing tenant secret and must remain stable across patch-env/upgrade.
  if tenant_darkirc_enabled "$name"; then
    darkirc_adapter_secret="$(_env_existing "$path" DARKIRC_ADAPTER_SECRET)"
    darkirc_adapter_secret="${darkirc_adapter_secret:-$(generate_token | cut -c1-32)}"
  fi

  # LLM endpoint the daemon's OpenAI-compatible client dials. When the proxy is
  # enabled, defaults to this tenant's local TensorZero proxy. When disabled,
  # defaults to the upstream TENSORZERO_URL directly. An explicit value (from
  # --llm-base-url or LUNARWING_MT_LLM_BASE_URL) always overrides.
  local llm_base_url_effective
  if tenant_proxy_enabled "$name"; then
    llm_base_url_effective="${llm_base_url:-http://127.0.0.1:${proxy_port}/v1}"
  else
    llm_base_url_effective="${llm_base_url:-$tensorzero_url}"
  fi

  # Idempotent overrides for the configurable flags (--llm-model,
  # --gateway-host, --xmpp-allow-from). An explicit flag value wins; else an
  # existing file value is preserved on re-run; else the hardcoded default
  # (LLM_MODEL, GATEWAY_HOST) or the owner JID alone (XMPP_ALLOW_FROM).
  local llm_model_effective gateway_host_effective xmpp_allow_from_effective
  if [[ -n "$llm_model" ]]; then
    llm_model_effective="$llm_model"
  else
    llm_model_effective="$(_env_existing "$path" LLM_MODEL)"
    llm_model_effective="${llm_model_effective:-tensorzero::function_name::lunarwing}"
  fi
  if [[ -n "$gateway_host" ]]; then
    gateway_host_effective="$gateway_host"
  else
    gateway_host_effective="$(_env_existing "$path" GATEWAY_HOST)"
    gateway_host_effective="${gateway_host_effective:-127.0.0.1}"
  fi
  if [[ -n "$xmpp_allow_from" ]]; then
    xmpp_allow_from_effective="$(build_xmpp_allow_from "$xmpp_jid" "$xmpp_allow_from")"
  else
    # Preserve an existing list; fall back to owner JID only on first write.
    xmpp_allow_from_effective="$(_env_existing "$path" XMPP_ALLOW_FROM)"
    xmpp_allow_from_effective="${xmpp_allow_from_effective:-$xmpp_jid}"
  fi

  (
    umask 077
    cat >"$path" <<ENVEOF
LUNARWING_BASE_DIR=$state_dir
IRONCLAW_BASE_DIR=$state_dir
LUNARWING_SOCKET=$run_dir/lunarwing.sock
IRONCLAW_SOCKET=$run_dir/lunarwing.sock

# Database
DATABASE_BACKEND=postgres
DATABASE_URL=postgres://lunarwing:${pg_password}@127.0.0.1:${pg_port}/lunarwing
DATABASE_SSLMODE=disable
PGSSLMODE=disable

# LLM — OpenAI-compatible endpoint (defaults to the local TensorZero proxy)
LLM_BACKEND=openai_compatible
LLM_BASE_URL=${llm_base_url_effective}
LLM_API_KEY=${llm_api_key:-token-${name}}
LLM_MODEL=$llm_model_effective
ALLOW_PRIVATE_IPS=1
# Circuit breaker: fast-fail once the LLM backend is degraded, then auto-probe
# to recover. Guards against a sick gateway amplifying transient blips into
# tenant-wide slowness. Backend-agnostic (wraps whatever provider is built).
LLM_CIRCUIT_BREAKER_THRESHOLD=$cb_threshold
LLM_CIRCUIT_BREAKER_RECOVERY_SECS=$cb_recovery

# Runtime identity
AGENT_NAME=$name
LUNARWING_OWNER_ID=$name
SECRETS_MASTER_KEY=$secrets_key

# XMPP
XMPP_BRIDGE_URL=http://127.0.0.1:${bridge_port}
XMPP_BRIDGE_TOKEN=$bridge_token
XMPP_JID=$xmpp_jid
XMPP_PASSWORD=$xmpp_password
XMPP_DM_POLICY=allowlist
XMPP_ALLOW_FROM=$xmpp_allow_from_effective
XMPP_ALLOW_ROOMS=
XMPP_ENCRYPTED_ROOMS=
XMPP_OMEMO_DEVICE_ID=0
XMPP_OMEMO_STORE_DIR=$state_dir/xmpp
XMPP_ALLOW_PLAINTEXT_FALLBACK=true
XMPP_RESOURCE=$name

# WASM
WASM_ENABLED=true
WASM_CHANNELS_ENABLED=true
WASM_TOOLS_DIR=$state_dir/tools
WASM_CHANNELS_DIR=$state_dir/channels

# Gateway
GATEWAY_ENABLED=true
GATEWAY_HOST=$gateway_host_effective
GATEWAY_PORT=$gateway_port
GATEWAY_AUTH_TOKEN=$gateway_token

# HTTP webhook (bound to localhost only; secret-protected)
HTTP_HOST=127.0.0.1
HTTP_PORT=$http_port
HTTP_WEBHOOK_SECRET=$webhook_secret

# Orchestrator (sandbox container callback)
ORCHESTRATOR_PORT=$orchestrator_port

# Nanocode worker (WebSocket port for agent communication)
NANOCODE_WSS_PORT=$nanocode_wss_port

# Opencode worker (WebSocket port for agent communication)
OPENCODE_WSS_PORT=$opencode_wss_port

# Pebble worker (WebSocket port for agent communication)
PEBBLE_WSS_PORT=$pebble_wss_port

# WeeChat relay + adapter
# RELAY_URL / WS_ADAPTER_URL are full URLs consumed by the in-process WASM
# channel (via the capabilities 'env' source). ADAPTER_PORT/WEECHAT_ADAPTER_PORT
# are the bare port consumed by the standalone ws_adapter.py process.
RELAY_URL=http://127.0.0.1:${weechat_port}
RELAY_PASSWORD=$relay_password
ADAPTER_PORT=$weechat_adapter_port
WEECHAT_ADAPTER_PORT=$weechat_adapter_port
WS_ADAPTER_URL=http://127.0.0.1:${weechat_adapter_port}

# Engine V2 (parallel execution path with streaming, gates, missions)
ENGINE_V2=true

# Daemon mode
CLI_ENABLED=false
ONBOARD_COMPLETED=true
HEARTBEAT_ENABLED=false
RUST_LOG=lunarwing=info
ENVEOF
  )
  if tenant_darkirc_enabled "$name"; then
    local darkirc_adapter_port
    darkirc_adapter_port="$(ports_get "$name" darkirc_adapter)" || true
    printf '\nDARKIRC_ADAPTER_URL=http://127.0.0.1:%s\nDARKIRC_ADAPTER_SECRET=%s\n' \
      "$darkirc_adapter_port" "$darkirc_adapter_secret" >> "$path"
  fi

  # LunarVision OCR/vision sidecar
  local vision_port vision_token
  vision_port="$(ports_get "$name" vision_service)" || true
  if [[ -n "$vision_port" ]]; then
    vision_token="$(_env_existing "$(tenant_env_dir "$name")/vision.env" LUNARWING_AUTH_TOKEN)"
    vision_token="${vision_token:-$(generate_token)}"
    printf '\nVISION_SERVICE_URL=http://127.0.0.1:%s\nVISION_AUTH_TOKEN=%s\n' \
      "$vision_port" "$vision_token" >> "$path"
  fi
  # Nanocode worker LLM overrides (consumed by the worker container via env;
  # see start_tenant_nanocode / render_worker_quadlet). Written only when set so
  # an unconfigured tenant gets the image's baked-in nanocode.json defaults.
  [[ -n "$nanocode_model" ]]    && printf '\nNANOCODE_MODEL=%s\n'    "$nanocode_model"     >> "$path"
  [[ -n "$nanocode_base_url" ]] && printf 'NANOCODE_BASE_URL=%s\n' "$nanocode_base_url" >> "$path"
  [[ -n "$opencode_model" ]]    && printf '\nOPENCODE_MODEL=%s\n'    "$opencode_model"     >> "$path"
  [[ -n "$opencode_base_url" ]] && printf 'OPENCODE_BASE_URL=%s\n' "$opencode_base_url" >> "$path"
  chown "$name:$name" "$path"
  say "wrote: $path"
}

write_tenant_bridge_env() {
  local name="$1"
  local xmpp_jid="${2:-$name@xmpp.localhost}"
  local xmpp_password="${3:-}"
  local xmpp_allow_from="${4:-}"

  local path bridge_port
  path="$(tenant_env_dir "$name")/xmpp-bridge.env"
  bridge_port="$(ports_get "$name" bridge)"

  local state_dir bridge_token
  state_dir="$(tenant_state_dir "$name")"
  bridge_token="$(grep -s '^XMPP_BRIDGE_TOKEN=' "$(tenant_env_dir "$name")/lunarwing.env" | cut -d= -f2- || true)"
  [[ -n "$bridge_token" ]] || bridge_token="$(generate_token | cut -c1-32)"

  local xmpp_pass_val
  xmpp_pass_val="$(grep -s '^XMPP_PASSWORD=' "$(tenant_env_dir "$name")/lunarwing.env" | cut -d= -f2- || true)"
  [[ -n "$xmpp_pass_val" ]] || xmpp_pass_val="${xmpp_password:-$(generate_token | cut -c1-32)}"

  # Idempotent XMPP allow-from JSON: an explicit flag wins; else re-derive from
  # the daemon's XMPP_ALLOW_FROM (CSV) if present so re-runs without the flag
  # keep the operator-set list in sync; else fall back to owner JID only.
  local xmpp_allow_from_json_effective
  if [[ -n "$xmpp_allow_from" ]]; then
    xmpp_allow_from_json_effective="$(build_xmpp_allow_from_json "$xmpp_jid" "$xmpp_allow_from")"
  else
    local daemon_csv
    daemon_csv="$(grep -s '^XMPP_ALLOW_FROM=' "$(tenant_env_dir "$name")/lunarwing.env" | cut -d= -f2- || true)"
    if [[ -n "$daemon_csv" && "$daemon_csv" != "$xmpp_jid" ]]; then
      # Daemon has extras: rebuild JSON from owner + the extras (everything
      # after the leading owner entry, which build_xmpp_allow_from re-prepends).
      local extras="${daemon_csv#$xmpp_jid}"
      extras="${extras#,}"   # strip a single leading comma if present
      xmpp_allow_from_json_effective="$(build_xmpp_allow_from_json "$xmpp_jid" "$extras")"
    else
      xmpp_allow_from_json_effective="$(build_xmpp_allow_from_json "$xmpp_jid" "")"
    fi
  fi

  (
    umask 077
    cat >"$path" <<ENVEOF
LUNARWING_BASE_DIR=$state_dir
IRONCLAW_BASE_DIR=$state_dir
XMPP_BRIDGE_BIND=127.0.0.1:${bridge_port}
XMPP_BRIDGE_TOKEN=$bridge_token
XMPP_BRIDGE_MAX_MESSAGES=256
RUST_LOG=xmpp_bridge=info,info

XMPP_JID=$xmpp_jid
XMPP_PASSWORD=$xmpp_pass_val
XMPP_DM_POLICY=allowlist
XMPP_ALLOW_FROM_JSON=$xmpp_allow_from_json_effective
XMPP_ALLOW_ROOMS_JSON=[]
XMPP_ENCRYPTED_ROOMS_JSON=[]
XMPP_DEVICE_ID=0
XMPP_OMEMO_STORE_DIR=$state_dir/xmpp
XMPP_ALLOW_PLAINTEXT_FALLBACK=true
XMPP_RESOURCE=$name
XMPP_BRIDGE_WAIT_SECONDS=15
ENVEOF
  )
  chown "$name:$name" "$path"
  say "wrote: $path"
}

write_tenant_proxy_env() {
  local name="$1"
  local tensorzero_url="${2:-$DEFAULT_TENSORZERO_URL}"

  local path proxy_port
  path="$(tenant_env_dir "$name")/proxy.env"
  proxy_port="$(ports_get "$name" proxy)"

  (
    umask 077
    cat >"$path" <<ENVEOF
PROXY_PORT=$proxy_port
PROXY_BIND=127.0.0.1
TENSORZERO_URL=$tensorzero_url
ENVEOF
  )
  chown "$name:$name" "$path"
  say "wrote: $path"
}

write_tenant_darkirc_adapter_env() {
  local name="$1"
  local lock_owned=false
  if [[ "$DARKIRC_WRITER_LOCK_HELD" != true ]]; then
    darkirc_writer_lock "$name"
    lock_owned=true
  fi

  local path adapter_port darkirc_irc_port lunarwing_env
  path="$(tenant_env_dir "$name")/darkirc-adapter.env"
  lunarwing_env="$(tenant_env_dir "$name")/lunarwing.env"
  darkirc_file_path_safe "$path" \
    || die "unsafe DarkIRC adapter env path for tenant '$name'"
  darkirc_file_path_safe "$lunarwing_env" \
    || die "unsafe LunarWing env path for tenant '$name'"
  adapter_port="$(ports_get "$name" darkirc_adapter)" || true
  darkirc_irc_port="$(ports_get "$name" darkirc_irc)" || true

  local adapter_secret
  adapter_secret="$(read_tenant_env_value \
    "$name" "$lunarwing_env" DARKIRC_ADAPTER_SECRET)" \
    || die "could not safely read DarkIRC adapter secret for tenant '$name'"
  [[ -n "$adapter_secret" ]] || adapter_secret="$(generate_token | cut -c1-32)"

  if [[ -f "$TEMPLATES_DIR/darkirc-adapter.env.template" ]]; then
    render_template_content \
      "darkirc-adapter.env.template" \
      "TENANT_NAME=$name" \
      "DARKIRC_IRC_PORT=$darkirc_irc_port" \
      "DARKIRC_ADAPTER_PORT=$adapter_port" \
      "ADAPTER_SECRET=$adapter_secret" \
      | write_tenant_file_atomic "$name" "$path" 600
  else
    cat <<ENVEOF | write_tenant_file_atomic "$name" "$path" 600
DARKIRC_HOST=127.0.0.1
DARKIRC_PORT=$darkirc_irc_port
DARKIRC_NICK=${name}-bridge
DARKIRC_USER=$name
DARKIRC_REALNAME="LunarWing DarkIRC Bridge ($name)"
ADAPTER_HOST=127.0.0.1
ADAPTER_PORT=$adapter_port
ADAPTER_SECRET=$adapter_secret
ADAPTER_LOG_LEVEL=INFO
ENVEOF
  fi
  say "wrote: $path"
  if [[ "$lock_owned" == true ]]; then
    darkirc_writer_unlock
  fi
}

install_trusted_darkirc_key_helper() {
  require_cmd cargo
  require_cmd flock
  require_cmd taskset
  [[ -f "$REPO_ROOT/Cargo.toml" && ! -L "$REPO_ROOT/Cargo.toml" ]] \
    || die "trusted LunarWing admin checkout is unavailable: $REPO_ROOT"

  install -d -o root -g root -m 0700 "$DARKIRC_KEY_HELPER_BUILD_ROOT"
  say "building shared DarkIRC key helper from the admin checkout ..."
  (
    flock -x 200
    cd "$REPO_ROOT"
    CARGO_TARGET_DIR="$DARKIRC_KEY_HELPER_BUILD_ROOT" \
      taskset -c 0-5 cargo build --release -j6 --bin lunarwing-darkirc-key-helper
  ) 200>"$BUILD_LOCK" || die "failed to build trusted DarkIRC key helper"

  local artifact="$DARKIRC_KEY_HELPER_BUILD_ROOT/release/lunarwing-darkirc-key-helper"
  darkirc_validate_root_executable "$artifact" \
    || die "trusted DarkIRC key-helper artifact failed ownership validation"
  install -d -o root -g root -m 0755 "$(dirname "$DARKIRC_KEY_HELPER_BIN")"
  install -o root -g root -m 0755 "$artifact" "$DARKIRC_KEY_HELPER_BIN" \
    || die "failed to install trusted DarkIRC key helper"
  darkirc_validate_root_executable "$DARKIRC_KEY_HELPER_BIN" \
    || die "installed DarkIRC key helper failed ownership validation"
}

darkirc_key_helper_path() {
  local name="$1" candidate
  : "$name"
  if [[ -n "$DEFAULT_DARKIRC_KEY_HELPER" ]]; then
    darkirc_validate_root_executable "$DEFAULT_DARKIRC_KEY_HELPER" \
      || return 1
    printf '%s' "$DEFAULT_DARKIRC_KEY_HELPER"
    return 0
  fi
  for candidate in "$DARKIRC_KEY_HELPER_BIN"; do
    if darkirc_validate_root_executable "$candidate"; then
      printf '%s' "$candidate"
      return 0
    fi
  done
  return 1
}

run_darkirc_manifest_validator() {
  local unbound="${1:-false}" helper
  helper="${DEFAULT_DARKIRC_KEY_HELPER:-}"
  if [[ -z "$helper" ]] || ! darkirc_validate_root_executable "$helper"; then
    install_trusted_darkirc_key_helper
    helper="$DARKIRC_KEY_HELPER_BIN"
  fi
  [[ -x "$helper" ]] || die "DarkIRC key helper is not installed"
  darkirc_validate_root_executable "$helper" \
    || die "DarkIRC manifest validator must be a root-owned, non-writable regular file"
  # This command parses only public/metadata fields and never opens a tenant
  # path; stdin carries the protected manifest directly to the helper.
  if [[ "$unbound" == true ]]; then
    "$helper" validate-migration --json --unbound
  else
    darkirc_load_compatibility
    "$helper" validate-migration --json "${DARKIRC_COMPAT_ARGS[@]}"
  fi
}

darkirc_validate_root_executable() {
  local path="$1" mode owner links parent parent_mode parent_owner
  [[ "$path" == /* ]] || return 1
  parent="$(dirname "$path")"
  darkirc_path_components_safe "$parent" || return 1
  [[ -d "$parent" && ! -L "$parent" ]] || return 1
  parent_mode="$(stat -c '%a' "$parent" 2>/dev/null || true)"
  parent_owner="$(stat -c '%u' "$parent" 2>/dev/null || true)"
  [[ "$parent_owner" == 0 && "$parent_mode" =~ ^[0-7]+$ ]] || return 1
  (( (8#$parent_mode & 07022) == 0 )) || return 1
  [[ -f "$path" && ! -L "$path" && -x "$path" ]] || return 1
  mode="$(stat -c '%a' "$path" 2>/dev/null || true)"
  owner="$(stat -c '%u' "$path" 2>/dev/null || true)"
  links="$(stat -c '%h' "$path" 2>/dev/null || true)"
  [[ "$owner" == 0 && "$links" == 1 && "$mode" =~ ^[0-7]+$ ]] || return 1
  [[ "$((8#$mode & 07022))" -eq 0 ]]
}

run_darkirc_key_helper() {
  local name="$1"; shift
  local helper uid gid home command_name
  command_name="${1:-}"
  case "$command_name" in
    rewrite-baseline|list|status|doctor|adopt|export-migration|stage-migration|import-migration|migration-ready|recover|prepare|respond|complete|cancel|exchanges)
      darkirc_load_compatibility
      set -- "$@" "${DARKIRC_COMPAT_ARGS[@]}"
      ;;
  esac
  helper="$(darkirc_key_helper_path "$name")" \
    || die "DarkIRC key helper is not installed; install lunarwing-darkirc-key-helper before writing tenant config"
  darkirc_validate_root_executable "$helper" \
    || die "DarkIRC key helper is not a root-owned, non-writable regular file"
  uid="$(id -u "$name")" || die "cannot resolve uid for tenant '$name'"
  gid="$(id -g "$name")" || die "cannot resolve gid for tenant '$name'"
  home="$(getent passwd "$name" | cut -d: -f6)"
  [[ -n "$home" ]] || die "cannot resolve home for tenant '$name'"
  if [[ "${EUID}" -eq 0 && "$(id -un)" != "$name" ]]; then
    ( cd / && exec sudo -u "$name" env -i HOME="$home" PATH="/usr/bin:/bin" "$helper" "$@" \
      --expected-uid "$uid" --expected-gid "$gid" )
  else
    "$helper" "$@" --expected-uid "$uid" --expected-gid "$gid"
  fi
}

DARKIRC_WRITER_FD=""
DARKIRC_WRITER_LOCK_HELD=false

darkirc_writer_lock_root_safe() {
  local path="$1" parent owner mode
  [[ "$path" == /* && "$path" != / ]] || return 1
  darkirc_path_components_safe "$path" || return 1
  if [[ -e "$path" ]]; then
    [[ -d "$path" && ! -L "$path" ]] || return 1
    owner="$(stat -c '%u' "$path" 2>/dev/null || true)"
    mode="$(stat -c '%a' "$path" 2>/dev/null || true)"
    [[ "$owner" == "$EUID" && "$mode" =~ ^[0-7]+$ ]] || return 1
    (( (8#$mode & 07022) == 0 ))
    return
  fi
  parent="${path%/*}"
  [[ -d "$parent" && ! -L "$parent" ]] || return 1
  darkirc_path_components_safe "$parent" || return 1
  owner="$(stat -c '%u' "$parent" 2>/dev/null || true)"
  mode="$(stat -c '%a' "$parent" 2>/dev/null || true)"
  [[ "$owner" == "$EUID" && "$mode" =~ ^[0-7]+$ ]] || return 1
  (( (8#$mode & 07022) == 0 ))
}

darkirc_writer_lock() {
  local name="$1" scope lock_file lock_owner lock_mode lock_links
  [[ "$DARKIRC_WRITER_LOCK_HELD" == true ]] && return 0
  require_cmd flock
  scope="$(darkirc_scope_id "$name")"
  validate_darkirc_scope_id "$scope"
  darkirc_writer_lock_root_safe "$DARKIRC_WRITER_LOCK_ROOT" \
    || die "unsafe DarkIRC writer lock root: $DARKIRC_WRITER_LOCK_ROOT"
  if [[ ! -e "$DARKIRC_WRITER_LOCK_ROOT" ]]; then
    mkdir -- "$DARKIRC_WRITER_LOCK_ROOT" \
      || die "could not create DarkIRC writer lock root"
    darkirc_writer_lock_root_safe "$DARKIRC_WRITER_LOCK_ROOT" \
      || die "unsafe DarkIRC writer lock root after creation: $DARKIRC_WRITER_LOCK_ROOT"
  fi
  chmod 0700 "$DARKIRC_WRITER_LOCK_ROOT"
  [[ "$(stat -c '%a' "$DARKIRC_WRITER_LOCK_ROOT" 2>/dev/null || true)" == 700 ]] \
    || die "DarkIRC writer lock root must be mode 0700"
  lock_file="$DARKIRC_WRITER_LOCK_ROOT/darkirc-${scope}.lock"
  if [[ -L "$lock_file" || -e "$lock_file" && ! -f "$lock_file" ]]; then
    die "unsafe DarkIRC writer lock: $lock_file"
  fi
  if [[ ! -e "$lock_file" ]]; then
    ( umask 077; : >"$lock_file" ) || die "could not create DarkIRC writer lock"
    chmod 0600 "$lock_file"
  fi
  lock_owner="$(stat -c '%u' "$lock_file" 2>/dev/null || true)"
  lock_mode="$(stat -c '%a' "$lock_file" 2>/dev/null || true)"
  lock_links="$(stat -c '%h' "$lock_file" 2>/dev/null || true)"
  [[ "$lock_owner" == "$EUID" && "$lock_mode" == 600 && "$lock_links" == 1 ]] \
    || die "DarkIRC writer lock must be owner-controlled, single-link mode 0600 for tenant '$name'"
  exec {DARKIRC_WRITER_FD}>"$lock_file"
  flock -x -w 10 "$DARKIRC_WRITER_FD" \
    || die "DarkIRC config update is busy for tenant '$name'"
  DARKIRC_WRITER_LOCK_HELD=true
}

darkirc_writer_unlock() {
  [[ "$DARKIRC_WRITER_LOCK_HELD" == true ]] || return 0
  flock -u "$DARKIRC_WRITER_FD" || true
  eval "exec ${DARKIRC_WRITER_FD}>&-"
  DARKIRC_WRITER_FD=""
  DARKIRC_WRITER_LOCK_HELD=false
}

generate_darkirc_config() {
  local name="$1"
  local state_dir config_dir datastore_dir irc_port rpc_port log_dir scope_id baseline
  local registry_lock_owned=false
  state_dir="$(tenant_state_dir "$name")"
  config_dir="$state_dir/darkirc"
  datastore_dir="$config_dir/datastore"

  # Never create/chmod/chown beneath the tenant-controlled state path as root.
  # The tenant-side setup is guarded component-by-component, then the typed
  # helper repeats the no-follow and ownership checks before opening anything.
  darkirc_prepare_tenant_dirs "$name"

  if [[ -z "$PORTS_REGISTRY_LOCK_FD" ]]; then
    ports_registry_lock
    registry_lock_owned=true
  fi
  irc_port="$(ports_get "$name" darkirc_irc)" || die "no darkirc_irc port for tenant '$name'"
  rpc_port="$(ports_get "$name" darkirc_rpc)" || die "no darkirc_rpc port for tenant '$name'"
  log_dir="$(tenant_log_dir "$name")"
  scope_id="$(ensure_darkirc_scope_id "$name")"
  [[ -f "$TEMPLATES_DIR/darkirc_config.toml.template" ]] \
    || die "darkirc config template not found at $TEMPLATES_DIR/darkirc_config.toml.template"

  baseline="$(render_template_content \
    "darkirc_config.toml.template" \
    "TENANT_NAME=$name" \
    "IRC_PORT=$irc_port" \
    "RPC_PORT=$rpc_port" \
    "CONFIG_DIR=$config_dir" \
    "LOG_DIR=$log_dir")"
  # The baseline is public/non-secret. Existing contact secrets are read only by
  # the tenant-scoped helper through its private file descriptor and never enter
  # argv, environment, logs, or shell output.
  printf '%s' "$baseline" | run_darkirc_key_helper "$name" rewrite-baseline \
    --tenant "$name" --scope-id "$scope_id" >/dev/null \
    || die "DarkIRC semantic config update failed for tenant '$name'"
  [[ -f "$config_dir/darkirc_config.toml" ]] \
    || die "DarkIRC helper did not produce a config for '$name'"
  if [[ "$registry_lock_owned" == true ]]; then
    ports_registry_unlock
  fi
  say "darkirc config updated for $name (irc=$irc_port rpc=$rpc_port scope=$scope_id)"
}

darkirc_contact_helper() {
  local command_name="$1" name="$2" contact="${3:-}"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  case "$command_name" in
    list|status|doctor|adopt|export-migration|recover)
      if ! tenant_darkirc_enabled "$name" && ! darkirc_state_available "$name"; then
        die "DarkIRC is disabled and no existing state is available for tenant '$name'"
      fi
      ;;
    *)
      tenant_darkirc_enabled "$name" || die "DarkIRC is disabled for tenant '$name'"
      ;;
  esac
  local scope_id
  case "$command_name" in
    list|doctor)
      scope_id="$(darkirc_scope_id "$name")"
      if [[ -n "$scope_id" ]]; then
        validate_existing_darkirc_scope_id "$name" "$scope_id"
        run_darkirc_key_helper "$name" "$command_name" --tenant "$name" --scope-id "$scope_id" --json
      else
        run_darkirc_key_helper "$name" "$command_name" --tenant "$name" --json
      fi
      ;;
    status)
      [[ -n "$contact" ]] || die "usage: darkirc-contact status <tenant> <contact>"
      scope_id="$(darkirc_scope_id "$name")"
      if [[ -n "$scope_id" ]]; then
        validate_existing_darkirc_scope_id "$name" "$scope_id"
        run_darkirc_key_helper "$name" status --tenant "$name" --scope-id "$scope_id" \
          --contact "$contact" --json
      else
        run_darkirc_key_helper "$name" status --tenant "$name" --contact "$contact" --json
      fi
      ;;
    adopt)
      scope_id="$(ensure_darkirc_scope_id "$name")"
      [[ "${DARKIRC_ADOPT_CONFIRMED:-false}" == true ]] \
        || die "adoption requires explicit --yes; no key is rotated"
      run_darkirc_key_helper "$name" adopt --tenant "$name" --scope-id "$scope_id" --yes --json
      ;;
    export-migration)
      scope_id="$(ensure_darkirc_scope_id "$name")"
      run_darkirc_key_helper "$name" export-migration --tenant "$name" --scope-id "$scope_id" --json
      ;;
    stage-migration)
      scope_id="$(ensure_darkirc_scope_id "$name")"
      run_darkirc_key_helper "$name" stage-migration --tenant "$name" --scope-id "$scope_id" --json
      ;;
    import-migration)
      scope_id="$(ensure_darkirc_scope_id "$name")"
      run_darkirc_key_helper "$name" import-migration --tenant "$name" --scope-id "$scope_id" --json
      ;;
    migration-ready)
      scope_id="$(ensure_darkirc_scope_id "$name")"
      run_darkirc_key_helper "$name" migration-ready --tenant "$name" --scope-id "$scope_id" --json
      ;;
    recover)
      scope_id="$(darkirc_scope_id "$name")"
      validate_darkirc_scope_id "$scope_id"
      run_darkirc_key_helper "$name" recover --tenant "$name" --scope-id "$scope_id" --json
      ;;
    prepare)
      [[ -n "$contact" ]] || die "usage: darkirc-contact prepare <tenant> <contact> --out <file|-> [options]"
      darkirc_exchange_prepare "$name" "$contact"
      ;;
    respond)
      [[ -n "$contact" ]] || die "usage: darkirc-contact respond <tenant> <contact> --in <file|-> --out <file|-> --expect-peer-fingerprint <sha256>"
      darkirc_exchange_respond "$name" "$contact"
      ;;
    complete)
      [[ -n "$contact" ]] || die "usage: darkirc-contact complete <tenant> <contact> --in <file|-> --expect-peer-fingerprint <sha256>"
      darkirc_exchange_complete "$name" "$contact"
      ;;
    cancel)
      darkirc_exchange_cancel "$name"
      ;;
    exchanges)
      darkirc_exchange_list "$name"
      ;;
    *) die "unknown DarkIRC contact operation '$command_name'" ;;
  esac
}

darkirc_exchange_prepare() {
  local name="$1" contact="$2" scope_id
  scope_id="$(ensure_darkirc_scope_id "$name")"
  local out_flag=() expires_flag=()
  DARKIRC_EXCHANGE_ARGS=()
  while [[ $# -gt 2 ]]; do
    shift
    case "$1" in
      --out) out_flag=(--out "$2"); shift ;;
      --out=*) out_flag=(--out "${1#--out=}") ;;
      --expires) expires_flag=(--expires "$2"); shift ;;
      --json) DARKIRC_EXCHANGE_ARGS+=(--json) ;;
      --) shift; break ;;
      -*) die "unknown prepare flag: $1" ;;
    esac
  done
  run_darkirc_key_helper "$name" prepare \
    --tenant "$name" --scope-id "$scope_id" --contact "$contact" \
    "${out_flag[@]}" "${expires_flag[@]}" "${DARKIRC_EXCHANGE_ARGS[@]}"
}

darkirc_exchange_respond() {
  local name="$1" contact="$2" scope_id
  scope_id="$(ensure_darkirc_scope_id "$name")"
  local out_flag=() in_flag=() fp_flag=()
  DARKIRC_EXCHANGE_ARGS=()
  while [[ $# -gt 2 ]]; do
    shift
    case "$1" in
      --out) out_flag=(--out "$2"); shift ;;
      --out=*) out_flag=(--out "${1#--out=}") ;;
      --in) in_flag=(--in "$2"); shift ;;
      --in=*) in_flag=(--in "${1#--in=}") ;;
      --expect-peer-fingerprint) fp_flag=(--expect-peer-fingerprint "$2"); shift ;;
      --expect-peer-fingerprint=*) fp_flag=(--expect-peer-fingerprint "${1#--expect-peer-fingerprint=}") ;;
      --json) DARKIRC_EXCHANGE_ARGS+=(--json) ;;
      --) shift; break ;;
      -*) die "unknown respond flag: $1" ;;
    esac
  done
  [[ ${#fp_flag[@]} -gt 0 ]] || die "respond requires --expect-peer-fingerprint"
  run_darkirc_key_helper "$name" respond \
    --tenant "$name" --scope-id "$scope_id" --contact "$contact" \
    "${in_flag[@]}" "${out_flag[@]}" "${fp_flag[@]}" "${DARKIRC_EXCHANGE_ARGS[@]}"
}

darkirc_exchange_complete() {
  local name="$1" contact="$2" scope_id
  scope_id="$(ensure_darkirc_scope_id "$name")"
  local in_flag=() fp_flag=()
  DARKIRC_EXCHANGE_ARGS=()
  while [[ $# -gt 2 ]]; do
    shift
    case "$1" in
      --in) in_flag=(--in "$2"); shift ;;
      --in=*) in_flag=(--in "${1#--in=}") ;;
      --expect-peer-fingerprint) fp_flag=(--expect-peer-fingerprint "$2"); shift ;;
      --expect-peer-fingerprint=*) fp_flag=(--expect-peer-fingerprint "${1#--expect-peer-fingerprint=}") ;;
      --json) DARKIRC_EXCHANGE_ARGS+=(--json) ;;
      --) shift; break ;;
      -*) die "unknown complete flag: $1" ;;
    esac
  done
  [[ ${#fp_flag[@]} -gt 0 ]] || die "complete requires --expect-peer-fingerprint"
  run_darkirc_key_helper "$name" complete \
    --tenant "$name" --scope-id "$scope_id" --contact "$contact" \
    "${in_flag[@]}" "${fp_flag[@]}" "${DARKIRC_EXCHANGE_ARGS[@]}"
}

darkirc_exchange_cancel() {
  local name="$1" scope_id
  scope_id="$(ensure_darkirc_scope_id "$name")"
  local id_flag=()
  DARKIRC_EXCHANGE_ARGS=()
  while [[ $# -gt 1 ]]; do
    shift
    case "$1" in
      --exchange-id) id_flag=(--offer-id "$2"); shift ;;
      --exchange-id=*) id_flag=(--offer-id "${1#--exchange-id=}") ;;
      --json) DARKIRC_EXCHANGE_ARGS+=(--json) ;;
      --) shift; break ;;
      -*) die "unknown cancel flag: $1" ;;
    esac
  done
  [[ ${#id_flag[@]} -gt 0 ]] || die "cancel requires --exchange-id"
  run_darkirc_key_helper "$name" cancel \
    --tenant "$name" --scope-id "$scope_id" \
    "${id_flag[@]}" "${DARKIRC_EXCHANGE_ARGS[@]}"
}

darkirc_exchange_list() {
  local name="$1" scope_id
  scope_id="$(darkirc_scope_id "$name")"
  if [[ -n "$scope_id" ]]; then
    validate_existing_darkirc_scope_id "$name" "$scope_id"
    run_darkirc_key_helper "$name" exchanges --tenant "$name" --scope-id "$scope_id" --json
  else
    run_darkirc_key_helper "$name" exchanges --tenant "$name" --json
  fi
}

darkirc_contact_doctor_all() {
  local name
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    if tenant_darkirc_enabled "$name" || darkirc_state_available "$name"; then
      darkirc_contact_helper doctor "$name"
    fi
  done < <(all_tenant_names)
}

# ── External-worker config.toml generation ────────────────────────────────────
#
# External workers (nanocode, pebble, …) speak the lunarwing-agent-v1 WebSocket
# protocol (legacy alias ironclaw-agent-v1 is still accepted for back-compat)
# and are routed by the agent's `create_job(mode: "<worker>")` tool.
# The daemon discovers them from `[[sandbox.external_workers]]` blocks in
# `config.toml` under the tenant's LUNARWING_BASE_DIR (the state dir). Without
# this block the agent has nothing to route `create_job(mode: "<worker>")` to
# and never logs `External workers configured: <worker>` on startup.
#
# The worker container binds 127.0.0.1:<wss_port> (path /ws/agent) and is
# launched with AGENT_AUTH_TOKEN set to the tenant's GATEWAY_AUTH_TOKEN (see
# start_tenant_nanocode), so the daemon side must present that same token as the
# WebSocket bearer — hence auth_token mirrors GATEWAY_AUTH_TOKEN here.
#
# Idempotent: a tenant entry is written once and skipped on re-runs. The block
# survives the daemon's own config.toml writers (e.g. `/model`), which
# load-modify-save the whole settings struct.
ensure_external_worker_config() {
  local name="$1"      # tenant name
  local worker="$2"    # logical worker name, matches create_job mode (e.g. "nanocode")
  local port_key="$3"  # ports-registry key for its WSS port (e.g. "nanocode_wss")

  local state_dir env_path config_path wss_port auth_token
  state_dir="$(tenant_state_dir "$name")"
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  config_path="$state_dir/config.toml"

  wss_port="$(ports_get "$name" "$port_key")"
  if [[ -z "$wss_port" ]]; then
    say "no $port_key port allocated for $name (skipping $worker external-worker config)"
    return 0
  fi

  # Already configured? Skip so re-runs / patch-env are idempotent. The daemon's
  # TOML writer emits `name = "<worker>"` identically, so this also matches a
  # file that has been load-modify-saved by `/model`.
  if [[ -f "$config_path" ]] && grep -q "name = \"$worker\"" "$config_path" 2>/dev/null; then
    say "external worker '$worker' already configured in $config_path (skipping)"
    return 0
  fi

  auth_token="$(grep -s '^GATEWAY_AUTH_TOKEN=' "$env_path" | cut -d= -f2- || true)"

  # add-tenant runs before the daemon ever starts, so config.toml usually does
  # not exist yet — create it with a header. Appending a fresh
  # `[[sandbox.external_workers]]` array-of-tables to an existing file is valid
  # TOML because we only ever append when no entry for this array exists yet.
  if [[ ! -f "$config_path" ]]; then
    sudo -u "$name" mkdir -p "$state_dir"
    (
      umask 077
      cat >"$config_path" <<'HDR'
# LunarWing tenant configuration (auto-generated by lunarwing-mt-admin.sh).
#
# Priority: env var > this file > database settings > defaults.
# The external-worker blocks below wire create_job(mode: "<name>") to the
# per-tenant worker containers. Hand edits outside these blocks are preserved.
HDR
    )
  fi

  {
    printf '\n# External worker: %s — create_job(mode: "%s")\n' "$worker" "$worker"
    printf '[[sandbox.external_workers]]\n'
    printf 'name = "%s"\n' "$worker"
    printf 'url = "ws://127.0.0.1:%s/ws/agent"\n' "$wss_port"
    if [[ -n "$auth_token" ]]; then
      printf 'auth_token = "%s"\n' "$auth_token"
    fi
    printf 'timeout_ms = 300000\n'
  } >>"$config_path"

  if [[ -z "$auth_token" ]]; then
    say "WARNING: GATEWAY_AUTH_TOKEN not found in $env_path;" \
        "'$worker' worker config written WITHOUT auth_token (connections will fail until set)"
  fi

  chown "$name:$name" "$config_path"
  chmod 600 "$config_path"
  say "wrote $worker external-worker config to $config_path (ws://127.0.0.1:$wss_port/ws/agent)"
}

# ── SSH harness config.toml generation ────────────────────────────────────────
#
# Appends a [[ssh.hosts]] block to the tenant's config.toml so the gateway
# initializes the SSH bridge + agent server at boot. The host defaults to
# localhost (the tenant's own user) for self-referential worker SSH access.
# Keys are stored in the encrypted secrets store (uploaded after the daemon
# starts via upload_tenant_ssh_key), NOT in config.toml.
#
# Idempotent: skips if an SSH host entry already exists in config.toml.
# APPENDS only — never overwrites existing config.toml content.
ensure_ssh_config() {
  local name="$1"
  local ssh_host="${2:-127.0.0.1}"
  local ssh_user="${3:-$name}"
  local state_dir config_path

  state_dir="$(tenant_state_dir "$name")"
  config_path="$state_dir/config.toml"

  # Already configured? Skip so re-runs are idempotent.
  if [[ -f "$config_path" ]] && grep -q '\[\[ssh\.hosts\]\]' "$config_path" 2>/dev/null; then
    say "SSH host config already present in $config_path (skipping)"
    return 0
  fi

  # Create config.toml with a header if it doesn't exist yet (same pattern as
  # ensure_external_worker_config). APPEND only — never truncate.
  if [[ ! -f "$config_path" ]]; then
    sudo -u "$name" mkdir -p "$state_dir"
    (
      umask 077
      printf '# LunarWing tenant configuration (auto-generated by lunarwing-mt-admin.sh).\n'
    ) >"$config_path"
  fi

  # Append the SSH host block. This is valid TOML because [[ssh.hosts]] is an
  # array-of-tables and we only append when no entry exists yet.
  {
    printf '\n# SSH harness — centralized SSH host config for worker integration.\n'
    printf '# Keys are stored in the encrypted secrets store, not on disk.\n'
    printf '[[ssh.hosts]]\n'
    printf 'host = "%s"\n' "$ssh_host"
    printf 'port = 22\n'
    printf 'user = "%s"\n' "$ssh_user"
    printf 'key_type = "ed25519"\n'
    printf 'host_key_mode = "AcceptFirst"\n'
  } >>"$config_path"

  chown "$name:$name" "$config_path"
  chmod 600 "$config_path"
  say "wrote SSH host config to $config_path (host=$ssh_host user=$ssh_user)"
}

# Print the host values of every [[ssh.hosts]] block in the tenant's
# config.toml, one per line. mt-admin writes this file itself (ensure_ssh_config),
# so the shape is known: a `host = "..."` line inside each [[ssh.hosts]] block.
_ssh_hosts_from_config() {
  local name="$1" config_path
  config_path="$(tenant_state_dir "$name")/config.toml"
  [[ -f "$config_path" ]] || return 0
  awk '
    /^\[\[ssh\.hosts\]\]/ { inblk = 1; next }
    /^\[/                 { inblk = 0 }
    inblk && /^host[ ]*=[ ]*"/ {
      line = $0
      sub(/^host[ ]*=[ ]*"/, "", line)
      sub(/".*$/, "", line)
      print line
    }
  ' "$config_path"
}

# Point the installed WASM ssh tool's capability allowlist at the tenant's
# configured [[ssh.hosts]] hosts (the sidecar ships with a "myhost" placeholder).
# Idempotent: always derived from config.toml. Warn-and-continue on any failure.
# Point the installed WASM ssh tool's capability allowlist at the tenant's
# configured [[ssh.hosts]] hosts (the sidecar ships with a "myhost" placeholder).
# Idempotent: always derived from config.toml. Warn-and-continue on any failure:
# every internal step is guarded so a failure can never errexit the script.
patch_ssh_tool_allowlist() {
  local name="$1" caps_path hosts_json tmp
  caps_path="$(tenant_state_dir "$name")/tools/ssh-tool.capabilities.json"

  if [[ ! -f "$caps_path" ]]; then
    say "  ssh-tool allowlist: ssh-tool not installed — nothing to patch"
    return 0
  fi

  command -v jq >/dev/null 2>&1 \
    || { say "WARNING: jq not installed; ssh-tool allowlist left unchanged" >&2; return 0; }

  hosts_json="$(_ssh_hosts_from_config "$name" | jq -R . | jq -s . 2>/dev/null)" \
    || { say "WARNING: could not derive ssh hosts for $name; ssh-tool allowlist left unchanged" >&2; return 0; }
  if [[ -z "$hosts_json" || "$hosts_json" == "[]" ]]; then
    say "  ssh-tool allowlist: no [[ssh.hosts]] in config.toml — leaving sidecar as shipped"
    return 0
  fi

  # Same-directory temp file so mv is atomic (same pattern as
  # configure_gotify_capabilities).
  tmp="$(mktemp "${caps_path}.tmp.XXXXXX")" \
    || { say "WARNING: mktemp failed; ssh-tool allowlist left unchanged" >&2; return 0; }
  if jq --argjson hosts "$hosts_json" '.capabilities.ssh.allowed_hosts = $hosts' \
       "$caps_path" >"$tmp" 2>/dev/null \
    && mv "$tmp" "$caps_path" \
    && chown "$name:$name" "$caps_path"; then
    say "  ssh-tool allowlist set to: $(jq -c . <<<"$hosts_json" 2>/dev/null || printf '%s' "$hosts_json")"
  else
    rm -f "$tmp"
    say "WARNING: failed to patch ssh-tool allowlist at $caps_path (edit .capabilities.ssh.allowed_hosts manually)" >&2
  fi
}

# True if a TCP connect to host:port succeeds within 2s (pure bash /dev/tcp).
_probe_tcp() {
  local host="$1" port="$2"
  timeout 2 bash -c "exec 3<>/dev/tcp/${host}/${port}" 2>/dev/null
}

# Warn (never fail) if the tenant's configured loopback SSH host has no sshd
# listening. Only 127.0.0.1 entries are probed: remote hosts may legitimately
# be unreachable from this box (firewalls, jump hosts).
warn_if_sshd_unreachable() {
  local name="$1" host
  while IFS= read -r host; do
    [[ "$host" == "127.0.0.1" ]] || continue
    if ! _probe_tcp "$host" 22; then
      say "WARNING: no sshd listening on ${host}:22 — the tenant's SSH tools target this host." >&2
      say "         Enable it with: systemctl enable --now sshd   (or 'ssh' on Debian/Ubuntu)" >&2
    fi
  done < <(_ssh_hosts_from_config "$name")
  # Explicit: this probe is warn-only and must never fail its (bare-statement)
  # callers under set -e, regardless of future edits above.
  return 0
}

# ── SSH key provisioning ──────────────────────────────────────────────────────
#
# Generates an ed25519 key pair for the tenant and adds the public key to the
# tenant's authorized_keys. The private key is staged in the tenant's env dir
# (mode 0600) for upload to the secrets store via upload_tenant_ssh_key after
# the daemon starts. The staged copy is deleted after upload so no key material
# is left on disk.
provision_tenant_ssh_key() {
  local name="$1"
  local ssh_host="${2:-127.0.0.1}"
  local ssh_user="${3:-$name}"
  local tenant_home ssh_dir key_path pubkey_path staged_key

  tenant_home="$(tenant_home "$name")"
  ssh_dir="$tenant_home/.ssh"
  key_path="$ssh_dir/id_ed25519_lunarwing"
  pubkey_path="${key_path}.pub"
  staged_key="$(tenant_env_dir "$name")/ssh_key_staged"

  # Generate a key pair if one doesn't already exist (idempotent).
  if [[ ! -f "$key_path" ]]; then
    sudo -u "$name" mkdir -p "$ssh_dir"
    sudo -u "$name" chmod 700 "$ssh_dir"
    sudo -u "$name" ssh-keygen -t ed25519 -f "$key_path" -N "" \
      -C "lunarwing-ssh-harness-$name" 2>/dev/null
    say "generated ed25519 SSH key pair for $name"
  else
    say "SSH key already exists for $name (reusing)"
  fi

  # Add the public key to authorized_keys (idempotent: skip if already present).
  local authorized_keys="$ssh_dir/authorized_keys"
  sudo -u "$name" touch "$authorized_keys"
  sudo -u "$name" chmod 600 "$authorized_keys"
  if ! sudo -u "$name" grep -qF "$(cat "$pubkey_path" 2>/dev/null)" "$authorized_keys" 2>/dev/null; then
    sudo -u "$name" bash -c "cat '$pubkey_path' >> '$authorized_keys'"
    say "added public key to authorized_keys for $name"
  fi

  # Stage the private key for upload (mode 0600, tenant-owned).
  # Deleted by upload_tenant_ssh_key after the daemon ingests it.
  sudo -u "$name" cp "$key_path" "$staged_key"
  sudo -u "$name" chmod 600 "$staged_key"
  chown "$name:$name" "$staged_key"
  say "staged SSH private key for upload to secrets store"
}

# Upload the staged SSH private key to the secrets store via the gateway's SSH
# API. Called AFTER the daemon starts (the API is served by the gateway on the
# HTTP port). Deletes the staged key after upload so no key material persists
# on disk. Init-system-agnostic — uses HTTP, works on both systemd and OpenRC.
upload_tenant_ssh_key() {
  local name="$1"
  local ssh_host="${2:-127.0.0.1}"
  local ssh_user="${3:-$name}"
  local http_port staged_key

  http_port="$(ports_get "$name" http)"
  staged_key="$(tenant_env_dir "$name")/ssh_key_staged"

  [[ -f "$staged_key" ]] || { say "no staged SSH key for $name (skipping upload)"; return 0; }

  # Wait for the gateway to be reachable (it may still be starting up).
  local i=0
  while ! curl -sf --max-time 2 "http://127.0.0.1:${http_port}/agent/status" >/dev/null 2>&1; do
    i=$((i + 1))
    [[ $i -lt 15 ]] || { say "WARNING: gateway not reachable on port $http_port after 30s; SSH key not uploaded (upload manually via the API)" >&2; return 1; }
    sleep 2
  done

  # Upload the key via the SSH API. The key data is sent as a JSON string
  # (base64 not needed — the API accepts raw PEM/OpenSSH format).
  local key_data upload_result
  key_data="$(cat "$staged_key")"
  upload_result="$(jq -n --arg key "$key_data" '{key_data: $key}' | \
    curl -sf -X POST "http://127.0.0.1:${http_port}/hosts/${ssh_host}/key" \
      -H "Content-Type: application/json" -d @- 2>&1)" || true

  if echo "$upload_result" | jq -e '.success == true' >/dev/null 2>&1; then
    say "SSH key uploaded to secrets store for host $ssh_host"
    # Delete the staged key — it's now in the encrypted secrets store only.
    rm -f "$staged_key"
    say "staged SSH key deleted (key material now only in secrets store)"
  else
    say "WARNING: SSH key upload failed: $upload_result" >&2
    say "         staged key remains at $staged_key (upload manually or re-run)" >&2
  fi
}

# Upload the DarkIRC adapter secret to the secrets store via the gateway's
# extension setup API. Called AFTER the daemon starts (the API is served by the
# gateway on the tenant gateway port). The secret is optional — if absent from the tenant
# env, this is a no-op. Idempotent: ExtensionManager::configure overwrites an
# existing secret and refreshes/activates the WASM channel, so re-running
# start_tenant is safe and no daemon restart is required (the credential is read
# live per-request from the secrets store). Init-system-agnostic — uses HTTP,
# works on both systemd and OpenRC. Best-effort: a failed upload warns and
# continues; the daemon keeps running.
upload_tenant_darkirc_secret() {
  local name="$1"
  local env_path gateway_port gateway_token darkirc_adapter_secret upload_result

  # Only tenants provisioned with DarkIRC carry this secret.
  tenant_darkirc_enabled "$name" || return 0

  env_path="$(tenant_env_dir "$name")/lunarwing.env"

  darkirc_adapter_secret="$(grep -s '^DARKIRC_ADAPTER_SECRET=' "$env_path" | cut -d= -f2- || true)"
  # The secret is optional — nothing to upload if it is not set.
  [[ -n "$darkirc_adapter_secret" ]] || return 0

  gateway_token="$(grep -s '^GATEWAY_AUTH_TOKEN=' "$env_path" | cut -d= -f2- || true)"
  if [[ -z "$gateway_token" ]]; then
    say "WARNING: GATEWAY_AUTH_TOKEN not found in $env_path; DarkIRC adapter secret not uploaded (upload manually via the API)" >&2
    return 1
  fi

  gateway_port="$(ports_get "$name" gateway)"

  # Wait for the gateway to be reachable (it may still be starting up).
  if ! _wait_tenant_gateway "$name"; then
    say "WARNING: gateway not reachable on port $gateway_port; DarkIRC adapter secret not uploaded (upload manually via the API)" >&2
    return 1
  fi

  # Upload the secret via the extension setup API. The adapter secret is sent
  # on stdin (never argv), and the gateway auth token is sent via curl -K
  # reading an anonymous process-substitution FD (never argv) so neither
  # credential appears in /proc/<pid>/cmdline or shell history.
  local auth_header="Authorization: Bearer ${gateway_token}"
  upload_result="$(jq -n --arg secret "$darkirc_adapter_secret" \
    '{secrets:{darkirc_adapter_secret:$secret},fields:{}}' | \
    curl -sf -X POST "http://127.0.0.1:${gateway_port}/api/extensions/darkirc/setup" \
      -H "Content-Type: application/json" \
      -K <(printf 'header = \"%s\"\\n' "$auth_header") \
      -d @- 2>&1)" || true

  if echo "$upload_result" | jq -e '.success == true' >/dev/null 2>&1; then
    say "DarkIRC adapter secret uploaded to secrets store"
    return 0
  else
    # Do NOT echo $upload_result: the gateway or adapter may echo the secret
    # back in the error message. Emit a generic warning instead.
    say "WARNING: DarkIRC adapter secret upload failed (upload manually via the API)" >&2
    return 1
  fi
}

patch_tenant_env() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" && ! -L "$env_path" ]] || die "env file not found or unsafe: $env_path"
  darkirc_file_path_safe "$env_path" || die "unsafe tenant env path: $env_path"

  # LUNARWING_OWNER_ID: sets the daemon's DB-scoping owner_id so sessions,
  # memory, and settings are isolated per tenant. Without it the daemon
  # defaults to "default", sharing state across all tenants on the host.
  patch_tenant_env_entry "$name" "$env_path" LUNARWING_OWNER_ID \
    "LUNARWING_OWNER_ID already set in $env_path (skipping)" \
    "added LUNARWING_OWNER_ID=$name to $env_path" <<ENVEOF

# Runtime identity (DB scope — must match tenant name)
LUNARWING_OWNER_ID=$name
ENVEOF
  if [[ "$TENANT_ENV_ENTRY_ADDED" == true ]]; then
    # Auto-migrate existing DB data from 'default' scope to the tenant's scope
    # so the daemon doesn't lose access to conversations, memory, and settings.
    if _owner_scope_needs_migration "$name"; then
      say "  found 'default'-scoped DB data; migrating to '$name' scope"
      migrate_owner_scope "$name" || say "  WARNING: owner-scope migration failed (run 'migrate-owner-scope $name' manually)"
    fi
  fi

  local orchestrator_port
  orchestrator_port="$(ports_get "$name" orchestrator)"
  patch_tenant_env_entry "$name" "$env_path" ORCHESTRATOR_PORT \
    "ORCHESTRATOR_PORT already set in $env_path (skipping)" \
    "added ORCHESTRATOR_PORT=$orchestrator_port to $env_path" <<ENVEOF

# Orchestrator (sandbox container callback)
ORCHESTRATOR_PORT=$orchestrator_port
ENVEOF

  local nanocode_wss_port
  nanocode_wss_port="$(ports_get "$name" nanocode_wss)"
  if [[ -n "$nanocode_wss_port" ]]; then
    patch_tenant_env_entry "$name" "$env_path" NANOCODE_WSS_PORT \
      "NANOCODE_WSS_PORT already set in $env_path (skipping)" \
      "added NANOCODE_WSS_PORT=$nanocode_wss_port to $env_path" <<ENVEOF

# Nanocode worker (WebSocket port for agent communication)
NANOCODE_WSS_PORT=$nanocode_wss_port
ENVEOF
  fi

  local pebble_wss_port
  pebble_wss_port="$(ports_get "$name" pebble_wss)"
  if [[ -n "$pebble_wss_port" ]]; then
    patch_tenant_env_entry "$name" "$env_path" PEBBLE_WSS_PORT \
      "PEBBLE_WSS_PORT already set in $env_path (skipping)" \
      "added PEBBLE_WSS_PORT=$pebble_wss_port to $env_path" <<ENVEOF

# Pebble worker (WebSocket port for agent communication)
PEBBLE_WSS_PORT=$pebble_wss_port
ENVEOF
  fi

  local opencode_wss_port
  opencode_wss_port="$(ports_get "$name" opencode_wss)" || true
  if [[ -n "$opencode_wss_port" ]]; then
    patch_tenant_env_entry "$name" "$env_path" OPENCODE_WSS_PORT \
      "OPENCODE_WSS_PORT already set in $env_path (skipping)" \
      "added OPENCODE_WSS_PORT=$opencode_wss_port to $env_path" <<ENVEOF

# Opencode worker (WebSocket port for agent communication)
OPENCODE_WSS_PORT=$opencode_wss_port
ENVEOF
  fi

  local weechat_adapter_port
  weechat_adapter_port="$(ports_get "$name" weechat_adapter)"
  if [[ -n "$weechat_adapter_port" ]]; then
    patch_tenant_env_entry "$name" "$env_path" WEECHAT_ADAPTER_PORT \
      "WEECHAT_ADAPTER_PORT already set in $env_path (skipping)" \
      "added WEECHAT_ADAPTER_PORT=$weechat_adapter_port to $env_path" <<ENVEOF

# WeeChat adapter (local HTTP adapter bridging WeeChat WS relay to WASM)
WEECHAT_ADAPTER_PORT=$weechat_adapter_port
ENVEOF
    # WS_ADAPTER_URL is the full adapter URL consumed by the in-process WASM
    # channel (via the capabilities `env` source). Without it the channel
    # falls back to the hardcoded :6681 default and silently fails.
    patch_tenant_env_entry "$name" "$env_path" WS_ADAPTER_URL \
      "WS_ADAPTER_URL already set in $env_path (skipping)" \
      "added WS_ADAPTER_URL=http://127.0.0.1:$weechat_adapter_port to $env_path" <<ENVEOF
WS_ADAPTER_URL=http://127.0.0.1:$weechat_adapter_port
ENVEOF
  fi

  local weechat_port
  weechat_port="$(ports_get "$name" weechat)"
  if [[ -n "$weechat_port" ]]; then
    patch_tenant_env_entry "$name" "$env_path" RELAY_URL \
      "RELAY_URL already set in $env_path (skipping)" \
      "added RELAY_URL=http://127.0.0.1:$weechat_port to $env_path" <<ENVEOF

# WeeChat relay URL consumed by the in-process WASM channel
RELAY_URL=http://127.0.0.1:$weechat_port
ENVEOF
  fi

  if tenant_darkirc_enabled "$name"; then
    ensure_darkirc_scope_id "$name" >/dev/null
    darkirc_writer_lock "$name"
    local darkirc_adapter_port
    darkirc_adapter_port="$(ports_get "$name" darkirc_adapter)" || true
    if [[ -n "$darkirc_adapter_port" ]]; then
      patch_tenant_env_entry "$name" "$env_path" DARKIRC_ADAPTER_URL \
        "DARKIRC_ADAPTER_URL already set in $env_path (skipping)" \
        "added DARKIRC_ADAPTER_URL=http://127.0.0.1:$darkirc_adapter_port to $env_path" <<ENVEOF

DARKIRC_ADAPTER_URL=http://127.0.0.1:$darkirc_adapter_port
ENVEOF
      local darkirc_adapter_secret
      darkirc_adapter_secret="$(generate_token | cut -c1-32)"
      patch_tenant_env_entry "$name" "$env_path" DARKIRC_ADAPTER_SECRET \
        "DARKIRC_ADAPTER_SECRET already set in $env_path (skipping)" \
        "added DARKIRC_ADAPTER_SECRET to $env_path" <<ENVEOF
DARKIRC_ADAPTER_SECRET=$darkirc_adapter_secret
ENVEOF
      unset darkirc_adapter_secret
      write_tenant_darkirc_adapter_env "$name"
    fi
    darkirc_writer_unlock
    if [[ -n "${darkirc_adapter_port:-}" ]] && ports_get "$name" darkirc_irc >/dev/null 2>&1; then
      generate_darkirc_config "$name"
    fi
  fi

  # Wire the nanocode/pebble/opencode external workers into config.toml so
  # existing tenants get create_job(mode: ...) routing without a hand-edited
  # config file.
  ensure_external_worker_config "$name" "nanocode" "nanocode_wss"
  ensure_external_worker_config "$name" "pebble" "pebble_wss"
  ensure_external_worker_config "$name" "opencode" "opencode_wss"
}

# ── Owner-scope DB migration ──────────────────────────────────────────────────
#
# Rekeys all user_id='default' (or a specified old scope) rows to user_id=<name>
# in the tenant's PostgreSQL. Used when a tenant that was originally created
# without LUNARWING_OWNER_ID (running as 'default') is upgraded to have its own
# owner_id scope. Handles unique-constraint collisions by preserving the
# existing tenant-scoped row and copying content from the old row if the new
# one is empty.
#
owner_scope_tables() {
  printf '%s\n' \
    settings conversations memory_documents routines agent_jobs api_tokens \
    heartbeat_state reflex_patterns user_identities secrets wasm_tools \
    tool_rate_limit_state secret_usage_log leak_detection_events wasm_channels
}

# Print aggregate owner-scope counts as: <user_id><tab><row_count>
owner_scope_rows() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_container_runtime

  local container_name="lunarwing-pg-$name"
  _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
    || die "PostgreSQL not running for '$name' — start the tenant's pg container first"

  local psql_cmd="psql -U lunarwing -d lunarwing"
  local selects="" sep="" tbl exists
  while IFS= read -r tbl; do
    exists="$(cd / && _ctr "$name" exec "$container_name" $psql_cmd -tAc \
      "SELECT 1 FROM information_schema.columns WHERE table_schema='public' AND table_name='$tbl' AND column_name='user_id'" 2>/dev/null || true)"
    [[ "$exists" == "1" ]] || continue
    selects+="${sep}SELECT user_id, count(*)::bigint AS row_count FROM $tbl GROUP BY user_id"
    sep=" UNION ALL "
  done < <(owner_scope_tables)

  [[ -n "$selects" ]] || return 0
  local sql
  sql="SELECT user_id, sum(row_count)::bigint FROM ($selects) s GROUP BY user_id ORDER BY user_id;"
  cd / && _ctr "$name" exec "$container_name" \
    psql -U lunarwing -d lunarwing -tA -F $'\t' -c "$sql"
}

# Check whether a tenant's DB has orphaned old-scope rows that need migration.
# Returns 0 (needs migration) or 1 (already clean / DB unreachable).
# Usage: _owner_scope_needs_migration <name> [old_scope]
_owner_scope_needs_migration() {
  local name="$1"
  local old_scope="${2:-default}"
  # Confirm a postgres port is allocated (tenant is provisioned); we do NOT connect
  # over it — psql runs INSIDE the pg container via the local socket (see the note in
  # migrate_owner_scope). A host-published port isn't reachable from inside the
  # container under rootless podman.
  ports_get "$name" postgres >/dev/null 2>&1 || return 1

  # If the PG container isn't running, can't check — assume clean.
  _ctr "$name" inspect -f '{{.State.Running}}' "lunarwing-pg-$name" 2>/dev/null | grep -q true || return 1

  local count
  count="$(cd / && _ctr "$name" exec lunarwing-pg-$name \
    psql -U lunarwing -d lunarwing -tAc "
    SELECT count(*) FROM (
      SELECT user_id FROM settings WHERE user_id='$old_scope'
      UNION ALL SELECT user_id FROM conversations WHERE user_id='$old_scope'
      UNION ALL SELECT user_id FROM memory_documents WHERE user_id='$old_scope'
      UNION ALL SELECT user_id FROM secrets WHERE user_id='$old_scope'
      UNION ALL SELECT user_id FROM agent_jobs WHERE user_id='$old_scope'
    ) AS t;" 2>/dev/null || echo 0)"

  [[ "$count" -gt 0 ]] 2>/dev/null
}

# Usage: migrate_owner_scope <name> [--from <old_scope>]
migrate_owner_scope() {
  local name="$1"
  local old_scope="${2:-default}"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  # Self-wipe guard: if the source scope already equals the target tenant name there
  # is nothing to rekey — and proceeding would be CATASTROPHIC. The collision dedup
  # DELETEs self-join the table (DELETE ... d USING ... t WHERE d.user_id=old_scope
  # AND t.user_id=name AND <key match>); when old_scope==name every row matches
  # itself and is deleted before the no-op UPDATE, wiping the table. The default-only
  # auto-callers never reach this (old_scope='default'), but an explicit
  # `--from <name>` (or a future scope-detection caller) could.
  if [[ "$old_scope" == "$name" ]]; then
    say "owner scope '$name' already matches the tenant; nothing to rekey"
    return 0
  fi

  local pg_port
  pg_port="$(ports_get "$name" postgres)" || die "no postgres port for $name"

  # Quick check: are there any old-scope rows at all?
  if ! _owner_scope_needs_migration "$name" "$old_scope"; then
    say "no '$old_scope' rows found for $name; owner scope already clean"
    return 0
  fi

  say "migrating owner scope: '$old_scope' -> '$name' (pg port $pg_port)"

  # Tables with a user_id column (base tables only, not views).
  # Discovered via information_schema — kept as a static list so the migration
  # is deterministic and doesn't break if a view is added/renamed.
  local tables
  tables="$(owner_scope_tables)"

  # Stop the daemon first so it doesn't re-create 'default' rows mid-migration.
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    _systemctl_user "$name" stop "lunarwing-${name}.service" 2>/dev/null || true
  else
    rc-service "lunarwing-${name}" stop >/dev/null 2>&1 || true
  fi

  # Connect over the container's LOCAL SOCKET — psql runs INSIDE the pg container via
  # `_ctr exec`, so NOT `-h 127.0.0.1 -p $pg_port`: that host-published port is not
  # reachable from inside the container under rootless podman, which silently broke
  # the rekey (the needs-check saw 0 rows -> "already clean" no-op, so a real
  # migration never ran). Mirrors restore_tenant_postgres (pg_restore -U lunarwing
  # -d lunarwing, no -h/-p). Do not add -h/-p back.
  local psql_cmd
  psql_cmd="psql -U lunarwing -d lunarwing"

  # Run the migration via the tenant's PG container. Each table's statements run
  # in ONE transaction (-1) with ON_ERROR_STOP, so a unique-constraint collision
  # or FK error rolls that table back atomically instead of half-applying — and
  # the error is SURFACED and counted (not swallowed) rather than aborting the
  # whole migration. Every table with a UNIQUE/PK on (user_id, ...) first deletes
  # the old-scope rows that would collide with an existing tenant-scoped row
  # (keeping the tenant row; memory_documents also salvages content into an empty
  # tenant row first), then updates. Tables with no user_id-bearing unique key get
  # a straight update.
  local total_migrated=0 migrate_errors=0
  for tbl in $tables; do
    # Check if the table exists in this DB (some may not if migrations haven't run).
    local exists
    exists="$(cd / && _ctr "$name" exec lunarwing-pg-$name $psql_cmd -tAc \
      "SELECT 1 FROM information_schema.tables WHERE table_name='$tbl' AND table_schema='public'" 2>/dev/null || true)"
    [[ "$exists" == "1" ]] || continue

    local sql
    case "$tbl" in
      settings)  # PRIMARY KEY (user_id, key)
        sql="DELETE FROM settings d USING settings t
               WHERE d.user_id='$old_scope' AND t.user_id='$name' AND d.key=t.key;
             UPDATE settings SET user_id='$name' WHERE user_id='$old_scope';" ;;
      memory_documents)  # UNIQUE (user_id, path, agent_id) NULLS NOT DISTINCT (V21)
        sql="UPDATE memory_documents t SET content = d.content
               FROM memory_documents d
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.path=t.path AND (d.agent_id IS NOT DISTINCT FROM t.agent_id)
                 AND (t.content IS NULL OR t.content = '');
             DELETE FROM memory_documents d USING memory_documents t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.path=t.path AND (d.agent_id IS NOT DISTINCT FROM t.agent_id);
             UPDATE memory_documents SET user_id='$name' WHERE user_id='$old_scope';" ;;
      secrets)  # UNIQUE (user_id, name)
        sql="DELETE FROM secrets d USING secrets t
               WHERE d.user_id='$old_scope' AND t.user_id='$name' AND d.name=t.name;
             UPDATE secrets SET user_id='$name' WHERE user_id='$old_scope';" ;;
      wasm_tools)  # UNIQUE (user_id, name, version)
        sql="DELETE FROM wasm_tools d USING wasm_tools t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.name=t.name AND d.version=t.version;
             UPDATE wasm_tools SET user_id='$name' WHERE user_id='$old_scope';" ;;
      routines)  # UNIQUE (user_id, name)
        sql="DELETE FROM routines d USING routines t
               WHERE d.user_id='$old_scope' AND t.user_id='$name' AND d.name=t.name;
             UPDATE routines SET user_id='$name' WHERE user_id='$old_scope';" ;;
      reflex_patterns)  # UNIQUE (user_id, normalized_pattern)
        sql="DELETE FROM reflex_patterns d USING reflex_patterns t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.normalized_pattern=t.normalized_pattern;
             UPDATE reflex_patterns SET user_id='$name' WHERE user_id='$old_scope';" ;;
      wasm_channels)  # UNIQUE (user_id, name)
        sql="DELETE FROM wasm_channels d USING wasm_channels t
               WHERE d.user_id='$old_scope' AND t.user_id='$name' AND d.name=t.name;
             UPDATE wasm_channels SET user_id='$name' WHERE user_id='$old_scope';" ;;
      heartbeat_state)  # UNIQUE (user_id, agent_id) — plain (NULLs distinct), so a
                        # NULL agent_id never collides; only dedup non-NULL matches.
        sql="DELETE FROM heartbeat_state d USING heartbeat_state t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.agent_id IS NOT NULL AND d.agent_id = t.agent_id;
             UPDATE heartbeat_state SET user_id='$name' WHERE user_id='$old_scope';" ;;
      tool_rate_limit_state)  # UNIQUE (wasm_tool_id, user_id)
        sql="DELETE FROM tool_rate_limit_state d USING tool_rate_limit_state t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.wasm_tool_id=t.wasm_tool_id;
             UPDATE tool_rate_limit_state SET user_id='$name' WHERE user_id='$old_scope';" ;;
      conversations)  # partial UNIQUE idx (user_id, routine_id) + (user_id) heartbeat singleton (V11)
        sql="DELETE FROM conversations d USING conversations t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.metadata->>'routine_id' IS NOT NULL
                 AND d.metadata->>'routine_id' = t.metadata->>'routine_id';
             DELETE FROM conversations d USING conversations t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.metadata->>'thread_type'='heartbeat'
                 AND t.metadata->>'thread_type'='heartbeat';
             UPDATE conversations SET user_id='$name' WHERE user_id='$old_scope';" ;;
      *)  # agent_jobs, api_tokens, user_identities, secret_usage_log, leak_detection_events: no user_id-bearing unique key
        sql="UPDATE $tbl SET user_id='$name' WHERE user_id='$old_scope';" ;;
    esac

    local out rc=0
    out="$(cd / && _ctr "$name" exec lunarwing-pg-$name \
      $psql_cmd -1 -v ON_ERROR_STOP=1 -c "$sql" 2>&1)" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
      say "  WARNING: $tbl rekey failed (rc=$rc): ${out//$'\n'/ }"
      migrate_errors=$((migrate_errors + 1))
    fi

    local count
    count="$(cd / && _ctr "$name" exec lunarwing-pg-$name $psql_cmd -tAc \
      "SELECT count(*) FROM $tbl WHERE user_id='$name'" 2>/dev/null || echo 0)"
    say "  $tbl: $count rows now scoped to '$name'"
    total_migrated=$((total_migrated + count))
  done

  # Verify no old-scope rows remain — check ALL owner-scoped tables (not just 5)
  # and name the offenders so a swallowed collision can't hide a partial rekey.
  local remaining_total=0 offenders="" unchecked=""
  for tbl in $tables; do
    local exists2 r
    exists2="$(cd / && _ctr "$name" exec lunarwing-pg-$name $psql_cmd -tAc \
      "SELECT 1 FROM information_schema.tables WHERE table_name='$tbl' AND table_schema='public'" 2>/dev/null || true)"
    [[ "$exists2" == "1" ]] || continue
    r="$(cd / && _ctr "$name" exec lunarwing-pg-$name $psql_cmd -tAc \
      "SELECT count(*) FROM $tbl WHERE user_id='$old_scope'" 2>/dev/null || echo "?")"
    if [[ "$r" =~ ^[0-9]+$ ]]; then
      [[ "$r" -gt 0 ]] && { offenders+=" $tbl($r)"; remaining_total=$((remaining_total + r)); }
    else
      unchecked+=" $tbl"
    fi
  done

  say ""
  [[ "$migrate_errors" -gt 0 ]] && \
    say "WARNING: $migrate_errors table(s) errored during rekey — see the WARNINGs above"
  if [[ -n "$offenders" ]]; then
    say "WARNING: $remaining_total '$old_scope' row(s) still remain after migration:$offenders"
    say "  (usually a unique-constraint collision or a users-table FK gap — inspect before starting)"
  elif [[ -n "$unchecked" ]]; then
    say "migration done; could not re-verify:$unchecked (DB read failed)"
  else
    say "migration complete: no '$old_scope' rows remain in any owner-scoped table"
  fi
  [[ "$migrate_errors" -eq 0 && -z "$offenders" && -z "$unchecked" ]] || \
    say "review the WARNINGs above before running 'start-tenant'"
  say "restart the tenant: $0 start-tenant $name"
}

extract_host_from_url() {
  printf '%s' "$1" | sed -E 's|^https?://||; s|[:/].*||'
}

write_tenant_gotify_config() {
  local name="$1"
  local gotify_url="${2:-}"
  local gotify_title="${3:-}"

  [[ -n "$gotify_url" ]] || return 0

  local state_dir config_dir config_path
  state_dir="$(tenant_state_dir "$name")"
  config_dir="$state_dir/workspace/config"
  config_path="$config_dir/gotify.json"

  sudo -u "$name" mkdir -p "$config_dir"
  gotify_url="$(printf '%s' "$gotify_url" | sed 's|/$||')"
  if [[ -n "$gotify_title" ]]; then
    printf '{"url": "%s", "title": "%s"}\n' "$gotify_url" "$gotify_title" >"$config_path"
  else
    printf '{"url": "%s"}\n' "$gotify_url" >"$config_path"
  fi
  chown "$name:$name" "$config_path"
  say "wrote: $config_path"
}

configure_gotify_capabilities() {
  local name="$1"
  local gotify_url="${2:-}"

  [[ -n "$gotify_url" ]] || return 0
  require_cmd jq

  local tools_dir caps_path host
  tools_dir="$(tenant_state_dir "$name")/tools"
  caps_path="$tools_dir/gotify-tool.capabilities.json"

  [[ -f "$caps_path" ]] || return 0

  host="$(extract_host_from_url "$gotify_url")"
  [[ -n "$host" ]] || return 0

  local tmp
  tmp="$(mktemp "$caps_path.tmp.XXXXXX")"
  jq --arg host "$host" '
    .capabilities.http.allowlist[0].host = $host |
    .capabilities.http.credentials.gotify.host_patterns = [$host]
  ' "$caps_path" >"$tmp"
  mv "$tmp" "$caps_path"
  chown "$name:$name" "$caps_path"
  say "  configured gotify capabilities for host: $host"
}

# ── Pebble worker configuration ──────────────────────────────────────────────

configure_pebble() {
  local name="$1"
  local nanogpt_api_key="$2"
  local model="$3"

  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  local env_dir env_path
  env_dir="$(tenant_env_dir "$name")"
  env_path="$env_dir/pebble.env"

  mkdir -p "$env_dir"

  : >"$env_path"

  if [[ -n "$nanogpt_api_key" ]]; then
    printf 'NANOGPT_API_KEY=%s\n' "$nanogpt_api_key" >>"$env_path"
  fi

  if [[ -n "$model" ]]; then
    printf 'PEBBLE_MODEL=%s\n' "$model" >>"$env_path"
  fi

  chown "$name:$name" "$env_path"
  chmod 600 "$env_path"
  say "pebble configured for tenant '$name' at $env_path"

  local container_name="lunarwing-pebble-$name"
  if _ctr "$name" inspect "$container_name" &>/dev/null 2>&1; then
    say "note: restart the pebble worker to pick up new config:"
    say "  sudo $0 stop-tenant $name && sudo $0 start-tenant $name"
  fi
}

# ── Nanocode worker LLM configuration ─────────────────────────────────────────
#
# Sets the nanocode worker's TensorZero model + baseURL overrides for a tenant by
# upserting NANOCODE_MODEL / NANOCODE_BASE_URL into lunarwing.env (the same source
# add-tenant writes, and that both init paths inject into the container). Mirrors
# configure-pebble, but stored in lunarwing.env (not a separate nanocode.env).

configure_nanocode() {
  local name="$1"
  local model="${2:-}"
  local base_url="${3:-}"

  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  [[ -n "$model" || -n "$base_url" ]] \
    || die "configure-nanocode: pass --model <model> and/or --base-url <url>"

  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path (run add-tenant first)"

  # Upsert: drop any existing override lines, then append the provided values.
  local tmp
  tmp="$(mktemp)"
  grep -v -e '^NANOCODE_MODEL=' -e '^NANOCODE_BASE_URL=' "$env_path" >"$tmp" || true
  [[ -n "$model" ]]    && printf 'NANOCODE_MODEL=%s\n'    "$model"    >>"$tmp"
  [[ -n "$base_url" ]] && printf 'NANOCODE_BASE_URL=%s\n' "$base_url" >>"$tmp"
  cat "$tmp" >"$env_path"
  rm -f "$tmp"
  chown "$name:$name" "$env_path"
  chmod 600 "$env_path"
  say "nanocode LLM overrides written to $env_path for tenant '$name'"

  local container_name="lunarwing-nanocode-$name"
  if _ctr "$name" inspect "$container_name" &>/dev/null 2>&1; then
    say "note: restart the nanocode worker to pick up the new config:"
    say "  sudo $0 stop-tenant $name && sudo $0 start-tenant $name"
  fi
}

# ── Nanocode worker container ─────────────────────────────────────────────────

start_tenant_nanocode() {
  local name="$1"
  ensure_container_runtime

  local wss_port container_name nanocode_dir
  wss_port="$(ports_get "$name" nanocode_wss)"
  container_name="lunarwing-nanocode-$name"
  nanocode_dir="${LUNARWING_ROOT}/lunarcode4lunarwing"

  if [[ -z "$wss_port" ]]; then
    say "no nanocode_wss port allocated for $name (skipping nanocode worker)"
    return 0
  fi

  # Ensure the image is available to whoever runs the container (rootless: load it
  # into the tenant's store via save|load; rootful: must already be built in root).
  if ! _ensure_tenant_image "$name" lunarwing-worker-nanocode:latest; then
    say "nanocode worker image not available; run 'build-nanocode-worker' first (skipping)"
    return 0
  fi

  # systemd + rootless podman: Quadlet .container owns the lifecycle (the unit's
  # [Container] spec creates+runs it), so skip the imperative `_ctr run` below.
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-nanocode-${name}.container"
    render_worker_quadlet "$name" nanocode 8443
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    # Force-recreate if the quadlet config changed since the container was
    # last created (Quadlet restarts the existing container without picking
    # up new env vars / volumes).
    if _container_config_changed "$name" "lunarwing-nanocode-${name}" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "lunarwing-nanocode-${name}.service" "lunarwing-nanocode-${name}"
    fi
    if _systemctl_user "$name" start "lunarwing-nanocode-${name}.service" >/dev/null 2>&1; then
      _store_container_hash "$name" "lunarwing-nanocode-${name}" "$quadlet_file"
      say "nanocode worker ready via quadlet (lunarwing-nanocode-${name}.service, WSS port $wss_port)"
    else
      say "WARNING: lunarwing-nanocode-${name}.service failed to start" >&2
      _systemctl_user "$name" status "lunarwing-nanocode-${name}.service" --no-pager >&2 || true
    fi
    return 0
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    # Check if the container's config is stale (env vars / volumes changed
    # since it was created). The imperative path hashes lunarwing.env since
    # there's no quadlet file to compare against.
    local env_file_for_hash
    env_file_for_hash="$(tenant_env_dir "$name")/lunarwing.env"
    if _container_config_changed "$name" "$container_name" "$env_file_for_hash"; then
      say "config changed for $container_name; force-recreating"
      _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
    elif _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "nanocode worker already running ($container_name, WSS port $wss_port)"
      _store_container_hash "$name" "$container_name" "$env_file_for_hash"
      _register_worker_unit "$name" nanocode
      return 0
    else
      say "starting existing nanocode worker container $container_name"
      _ctr "$name" start "$container_name" >/dev/null
      _store_container_hash "$name" "$container_name" "$env_file_for_hash"
      _register_worker_unit "$name" nanocode
      say "nanocode worker ready ($container_name, WSS port $wss_port)"
      return 0
    fi
  fi

  # Container doesn't exist (or was force-removed above) — create it fresh.
  if ! _ctr "$name" inspect "$container_name" &>/dev/null; then
    say "creating nanocode worker container $container_name on WSS port $wss_port"

    # Read tenant env for secrets to pass through
    local tenant_env_path
    tenant_env_path="$(tenant_env_dir "$name")/lunarwing.env"

    # Read nanocode-specific env if it exists
    local nanocode_env_path
    nanocode_env_path="$(tenant_env_dir "$name")/nanocode.env"

    local env_flags=()
    # Core env vars from tenant lunarwing.env
    if [[ -f "$tenant_env_path" ]]; then
      local gateway_token
      gateway_token="$(grep '^GATEWAY_AUTH_TOKEN=' "$tenant_env_path" | cut -d= -f2- || true)"
      [[ -n "$gateway_token" ]] && env_flags+=(-e "AGENT_AUTH_TOKEN=$gateway_token")

      local llm_api_key
      llm_api_key="$(grep '^LLM_API_KEY=' "$tenant_env_path" | cut -d= -f2- || true)"
      [[ -n "$llm_api_key" ]] && env_flags+=(-e "TENSORZERO_API_KEY=$llm_api_key")

      # Nanocode LLM overrides (model / TensorZero baseURL), if set on the tenant.
      local nanocode_model nanocode_base_url
      nanocode_model="$(grep '^NANOCODE_MODEL=' "$tenant_env_path" | cut -d= -f2- || true)"
      [[ -n "$nanocode_model" ]] && env_flags+=(-e "NANOCODE_MODEL=$nanocode_model")
      nanocode_base_url="$(grep '^NANOCODE_BASE_URL=' "$tenant_env_path" | cut -d= -f2- || true)"
      [[ -n "$nanocode_base_url" ]] && env_flags+=(-e "NANOCODE_BASE_URL=$nanocode_base_url")
    fi

    # Override with nanocode-specific env file if present
    if [[ -f "$nanocode_env_path" ]]; then
      env_flags+=(--env-file "$nanocode_env_path")
    fi

    local workspace_dir
    workspace_dir="$(tenant_lw_root "$name")/nanocode-workspace"
    mkdir -p "$workspace_dir"
    chown "$name:$name" "$workspace_dir"
    chmod 777 "$workspace_dir"

    # HEALTH_PORT=8443 matches the image's baked HEALTHCHECK (in-container).
    # v8: also publish the tenant's dedicated nanocode_health port -> container
    # 8443, so the host self-heal pipeline can probe /health directly.
    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    local -a health_publish=()
    local host_health_port
    host_health_port="$(ports_get "$name" nanocode_health)" || true
    [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:8443")
    # SSH agent socket (always included — daemon creates it at startup).
    # :z label so SELinux (Enforcing on Fedora) permits container_t to access the
    # tenant-home-labeled socket; without it SSH_AUTH_SOCK reads fail despite 0666 mode.
    local ssh_agent_socket="$(tenant_run_dir "$name")/ssh-agent.sock"
    local -a ssh_mount=(-v "${ssh_agent_socket}:/tmp/ssh-agent.sock:z" -e SSH_AUTH_SOCK=/tmp/ssh-agent.sock)
    _ctr "$name" run -d \
      --name "$container_name" \
      -e LUNARWING_WORKER_ID="worker-nanocode-${name}" \
      -e WS_PORT="$wss_port" \
      -e HEALTH_PORT="8443" \
      -e NANOCODE_MODE=websocket \
      -e WS_ROLE=server \
      -e WS_BIND_HOST=0.0.0.0 \
      -e WS_PATH=/ws/agent \
      "${env_flags[@]}" \
      "${ssh_mount[@]}" \
      -p "127.0.0.1:${wss_port}:${wss_port}" \
      "${health_publish[@]}" \
      -v "$workspace_dir:/workspace:z" \
      "${restart_arg[@]}" \
      lunarwing-worker-nanocode:latest \
      --mode websocket >/dev/null
    _store_container_hash "$name" "$container_name" "$(tenant_env_dir "$name")/lunarwing.env"
  fi

  _register_worker_unit "$name" nanocode
  say "nanocode worker ready ($container_name, WSS port $wss_port)"
}

stop_tenant_nanocode() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-nanocode-$name"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container_name}" ]]; then
    _deregister_babysitter "$container_name"
    rc-service "$container_name" stop >/dev/null 2>&1 || true
    say "nanocode worker stopped ($container_name)"
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "nanocode worker stopped ($container_name)"
  fi
}

# ── OpenCode worker container ──────────────────────────────────────────────────

configure_opencode() {
  local name="$1"
  local model="${2:-}"
  local base_url="${3:-}"

  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  [[ -n "$model" || -n "$base_url" ]] \
    || die "configure-opencode: pass --model <model> and/or --base-url <url>"

  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path (run add-tenant first)"

  local tmp
  tmp="$(mktemp)"
  grep -v -e '^OPENCODE_MODEL=' -e '^OPENCODE_BASE_URL=' "$env_path" >"$tmp" || true
  [[ -n "$model" ]]    && printf 'OPENCODE_MODEL=%s\n'    "$model"    >>"$tmp"
  [[ -n "$base_url" ]] && printf 'OPENCODE_BASE_URL=%s\n' "$base_url" >>"$tmp"
  cat "$tmp" >"$env_path"
  rm -f "$tmp"
  chown "$name:$name" "$env_path"
  chmod 600 "$env_path"
  say "opencode LLM overrides written to $env_path for tenant '$name'"

  local container_name="lunarwing-opencode-$name"
  if _ctr "$name" inspect "$container_name" &>/dev/null 2>&1; then
    say "note: restart the opencode worker to pick up the new config:"
    say "  sudo $0 stop-tenant $name && sudo $0 start-tenant $name"
  fi
}

start_tenant_opencode() {
  local name="$1"
  ensure_container_runtime

  local wss_port container_name opencode_dir
  # `|| true`: ports_get returns nonzero for a genuinely unallocated key; under
  # `set -euo pipefail` a bare assignment would abort the whole start-tenant run
  # before the `[[ -z "$wss_port" ]]` skip below (mirrors start_tenant_vision).
  wss_port="$(ports_get "$name" opencode_wss)" || true
  container_name="lunarwing-opencode-$name"
  opencode_dir="${LUNARWING_ROOT}/opencode4lunarwing"

  if [[ -z "$wss_port" ]]; then
    say "no opencode_wss port allocated for $name (skipping opencode worker)"
    return 0
  fi

  if ! _ensure_tenant_image "$name" lunarwing-worker-opencode:latest; then
    say "opencode worker image not available; run 'build-opencode-worker' first (skipping)"
    return 0
  fi

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-opencode-${name}.container"
    render_worker_quadlet "$name" opencode 8443
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if _container_config_changed "$name" "lunarwing-opencode-${name}" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "lunarwing-opencode-${name}.service" "lunarwing-opencode-${name}"
    fi
    if _systemctl_user "$name" start "lunarwing-opencode-${name}.service" >/dev/null 2>&1; then
      _store_container_hash "$name" "lunarwing-opencode-${name}" "$quadlet_file"
      say "opencode worker ready via quadlet (lunarwing-opencode-${name}.service, WSS port $wss_port)"
    else
      say "WARNING: lunarwing-opencode-${name}.service failed to start" >&2
      _systemctl_user "$name" status "lunarwing-opencode-${name}.service" --no-pager >&2 || true
    fi
    return 0
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    local env_file_for_hash
    env_file_for_hash="$(tenant_env_dir "$name")/lunarwing.env"
    if _container_config_changed "$name" "$container_name" "$env_file_for_hash"; then
      say "config changed for $container_name; force-recreating"
      _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
    elif _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "opencode worker already running ($container_name, WSS port $wss_port)"
      _store_container_hash "$name" "$container_name" "$env_file_for_hash"
      _register_worker_unit "$name" opencode
      return 0
    else
      say "starting existing opencode worker container $container_name"
      _ctr "$name" start "$container_name" >/dev/null
      _store_container_hash "$name" "$container_name" "$env_file_for_hash"
      _register_worker_unit "$name" opencode
      say "opencode worker ready ($container_name, WSS port $wss_port)"
      return 0
    fi
  fi

  if ! _ctr "$name" inspect "$container_name" &>/dev/null; then
    say "creating opencode worker container $container_name on WSS port $wss_port"

    local tenant_env_path
    tenant_env_path="$(tenant_env_dir "$name")/lunarwing.env"

    local opencode_env_path
    opencode_env_path="$(tenant_env_dir "$name")/opencode.env"

    local env_flags=()
    if [[ -f "$tenant_env_path" ]]; then
      local gateway_token
      gateway_token="$(grep '^GATEWAY_AUTH_TOKEN=' "$tenant_env_path" | cut -d= -f2- || true)"
      [[ -n "$gateway_token" ]] && env_flags+=(-e "AGENT_AUTH_TOKEN=$gateway_token")

      local llm_api_key
      llm_api_key="$(grep '^LLM_API_KEY=' "$tenant_env_path" | cut -d= -f2- || true)"
      [[ -n "$llm_api_key" ]] && env_flags+=(-e "TENSORZERO_API_KEY=$llm_api_key")

      local opencode_model opencode_base_url
      opencode_model="$(grep '^OPENCODE_MODEL=' "$tenant_env_path" | cut -d= -f2- || true)"
      [[ -n "$opencode_model" ]] && env_flags+=(-e "OPENCODE_MODEL=$opencode_model")
      opencode_base_url="$(grep '^OPENCODE_BASE_URL=' "$tenant_env_path" | cut -d= -f2- || true)"
      [[ -n "$opencode_base_url" ]] && env_flags+=(-e "OPENCODE_BASE_URL=$opencode_base_url")
    fi

    if [[ -f "$opencode_env_path" ]]; then
      env_flags+=(--env-file "$opencode_env_path")
    fi

    local workspace_dir
    workspace_dir="$(tenant_lw_root "$name")/opencode-workspace"
    mkdir -p "$workspace_dir"
    chown "$name:$name" "$workspace_dir"
    chmod 777 "$workspace_dir"

    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    local -a health_publish=()
    local host_health_port
    host_health_port="$(ports_get "$name" opencode_health)" || true
    [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:8443")
    local ssh_agent_socket="$(tenant_run_dir "$name")/ssh-agent.sock"
    local -a ssh_mount=(-v "${ssh_agent_socket}:/tmp/ssh-agent.sock:z" -e SSH_AUTH_SOCK=/tmp/ssh-agent.sock)
    _ctr "$name" run -d \
      --name "$container_name" \
      -e LUNARWING_WORKER_ID="worker-opencode-${name}" \
      -e WS_PORT="$wss_port" \
      -e HEALTH_PORT="8443" \
      -e OPENCODE_MODE=websocket \
      -e WS_ROLE=server \
      -e WS_BIND_HOST=0.0.0.0 \
      -e WS_PATH=/ws/agent \
      "${env_flags[@]}" \
      "${ssh_mount[@]}" \
      -p "127.0.0.1:${wss_port}:${wss_port}" \
      "${health_publish[@]}" \
      -v "$workspace_dir:/workspace:z" \
      "${restart_arg[@]}" \
      lunarwing-worker-opencode:latest \
      --mode websocket >/dev/null
    _store_container_hash "$name" "$container_name" "$(tenant_env_dir "$name")/lunarwing.env"
  fi

  _register_worker_unit "$name" opencode
  say "opencode worker ready ($container_name, WSS port $wss_port)"
}

stop_tenant_opencode() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-opencode-$name"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container_name}" ]]; then
    _deregister_babysitter "$container_name"
    rc-service "$container_name" stop >/dev/null 2>&1 || true
    say "opencode worker stopped ($container_name)"
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "opencode worker stopped ($container_name)"
  fi
}

# ── Pebble worker container ──────────────────────────────────────────────────

start_tenant_pebble() {
  local name="$1"
  ensure_container_runtime

  local wss_port container_name
  wss_port="$(ports_get "$name" pebble_wss)"
  container_name="lunarwing-pebble-$name"

  if [[ -z "$wss_port" ]]; then
    say "no pebble_wss port allocated for $name (skipping pebble worker)"
    return 0
  fi

  if ! _ensure_tenant_image "$name" lunarwing-worker-pebble:latest; then
    say "pebble worker image not available; run 'build-pebble-worker' first (skipping)"
    return 0
  fi

  # systemd + rootless podman: Quadlet .container owns the lifecycle (the unit's
  # [Container] spec creates+runs it), so skip the imperative `_ctr run` below.
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-pebble-${name}.container"
    render_worker_quadlet "$name" pebble 8443
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if _container_config_changed "$name" "lunarwing-pebble-${name}" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "lunarwing-pebble-${name}.service" "lunarwing-pebble-${name}"
    fi
    if _systemctl_user "$name" start "lunarwing-pebble-${name}.service" >/dev/null 2>&1; then
      _store_container_hash "$name" "lunarwing-pebble-${name}" "$quadlet_file"
      say "pebble worker ready via quadlet (lunarwing-pebble-${name}.service, WSS port $wss_port)"
    else
      say "WARNING: lunarwing-pebble-${name}.service failed to start" >&2
      _systemctl_user "$name" status "lunarwing-pebble-${name}.service" --no-pager >&2 || true
    fi
    return 0
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    local env_file_for_hash
    env_file_for_hash="$(tenant_env_dir "$name")/lunarwing.env"
    if _container_config_changed "$name" "$container_name" "$env_file_for_hash"; then
      say "config changed for $container_name; force-recreating"
      _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
    elif _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "pebble worker already running ($container_name, WSS port $wss_port)"
      _store_container_hash "$name" "$container_name" "$env_file_for_hash"
      _register_worker_unit "$name" pebble
      return 0
    else
      say "starting existing pebble worker container $container_name"
      _ctr "$name" start "$container_name" >/dev/null
      _store_container_hash "$name" "$container_name" "$env_file_for_hash"
      _register_worker_unit "$name" pebble
      say "pebble worker ready ($container_name, WSS port $wss_port)"
      return 0
    fi
  fi

  if ! _ctr "$name" inspect "$container_name" &>/dev/null; then
    say "creating pebble worker container $container_name on WSS port $wss_port"

    local tenant_env_path
    tenant_env_path="$(tenant_env_dir "$name")/lunarwing.env"

    local pebble_env_path
    pebble_env_path="$(tenant_env_dir "$name")/pebble.env"

    local env_flags=()
    if [[ -f "$tenant_env_path" ]]; then
      local gateway_token
      gateway_token="$(grep '^GATEWAY_AUTH_TOKEN=' "$tenant_env_path" | cut -d= -f2- || true)"
      [[ -n "$gateway_token" ]] && env_flags+=(-e "AGENT_AUTH_TOKEN=$gateway_token")
    fi

    if [[ -f "$pebble_env_path" ]]; then
      env_flags+=(--env-file "$pebble_env_path")
    fi

    local workspace_dir
    workspace_dir="$(tenant_lw_root "$name")/pebble-workspace"
    mkdir -p "$workspace_dir"
    chown "$name:$name" "$workspace_dir"
    chmod 777 "$workspace_dir"

    # HEALTH_PORT=8443 matches the image's baked HEALTHCHECK (in-container).
    # v8: also publish the tenant's dedicated pebble_health port -> container
    # 8443, so the host self-heal pipeline can probe /health directly.
    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    local -a health_publish=()
    local host_health_port
    host_health_port="$(ports_get "$name" pebble_health)" || true
    [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:8443")
    # SSH agent socket (always included — daemon creates it at startup).
    # :z label for SELinux (Enforcing on Fedora) — see start_tenant_nanocode.
    local -a ssh_mount=()
    local ssh_agent_socket="$(tenant_run_dir "$name")/ssh-agent.sock"
    ssh_mount=(-v "${ssh_agent_socket}:/tmp/ssh-agent.sock:z" -e SSH_AUTH_SOCK=/tmp/ssh-agent.sock)
    _ctr "$name" run -d \
      --name "$container_name" \
      -e LUNARWING_WORKER_ID="worker-pebble-${name}" \
      -e WS_PORT="$wss_port" \
      -e HEALTH_PORT="8443" \
      -e PEBBLE_MODE=websocket \
      -e WS_BIND_HOST=0.0.0.0 \
      -e WS_PATH=/ws/agent \
      "${env_flags[@]}" \
      "${ssh_mount[@]}" \
      -p "127.0.0.1:${wss_port}:${wss_port}" \
      "${health_publish[@]}" \
      -v "$workspace_dir:/workspace:z" \
      "${restart_arg[@]}" \
      lunarwing-worker-pebble:latest >/dev/null
    _store_container_hash "$name" "$container_name" "$(tenant_env_dir "$name")/lunarwing.env"
  fi

  _register_worker_unit "$name" pebble
  say "pebble worker ready ($container_name, WSS port $wss_port)"
}

stop_tenant_pebble() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-pebble-$name"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container_name}" ]]; then
    _deregister_babysitter "$container_name"
    rc-service "$container_name" stop >/dev/null 2>&1 || true
    say "pebble worker stopped ($container_name)"
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "pebble worker stopped ($container_name)"
  fi
}

# ── LunarVision OCR/vision sidecar ──────────────────────────────────────────

VISION_SIDECAR_IMAGE=lunarwing/vision-service:latest
VISION_SIDECAR_INTERNAL_PORT=8088
VISION_SIDECAR_HEALTH_PORT=8089

build_vision_sidecar_image() {
  ensure_container_runtime
  local sidecar_dir="$SOURCE_REPO/projects/ocr-sidecar"
  [[ -d "$sidecar_dir" ]] || { say "WARNING: $sidecar_dir not found; skipping vision sidecar build"; return 0; }
  if [[ -f "$sidecar_dir/Dockerfile" ]]; then
    "$CONTAINER_RT" build --network=host --format docker -t "$VISION_SIDECAR_IMAGE" -f "$sidecar_dir/Dockerfile" "$sidecar_dir" >/dev/null \
      || { say "WARNING: vision sidecar image build failed (run 'build-vision-sidecar' to retry)"; return 1; }
    say "vision sidecar image built: $VISION_SIDECAR_IMAGE"
  else
    say "WARNING: $sidecar_dir/Dockerfile missing; cannot build vision sidecar"
    return 1
  fi
}

write_tenant_vision_env() {
  local name="$1"
  local env_dir env_path
  env_dir="$(tenant_env_dir "$name")"
  env_path="$env_dir/vision.env"
  local token
  token="$(_env_existing "$env_path" LUNARWING_AUTH_TOKEN)"
  token="${token:-$(generate_token)}"
  # Resolve VL backend URL: explicit override wins, else fleet default.
  # Empty (LUNARWING_MT_VL_URL= and DEFAULT_VL_URL unset) = no VL line written;
  # sidecar comes up with VL disabled. Idempotent — vision.env is fully rewritten.
  local vl_url
  vl_url="${LUNARWING_MT_VL_URL:-$DEFAULT_VL_URL}"
  mkdir -p "$env_dir"
  (
    umask 077
    if [[ -n "$vl_url" ]]; then
      cat >"$env_path" <<ENVEOF
LUNARWING_AUTH_TOKEN=$token
OCR_PORT=$VISION_SIDECAR_INTERNAL_PORT
OCR_HEALTH_PORT=$VISION_SIDECAR_HEALTH_PORT
VL_URL=$vl_url
VL_MODEL=qwen3-vl
ENVEOF
    else
      cat >"$env_path" <<ENVEOF
LUNARWING_AUTH_TOKEN=$token
OCR_PORT=$VISION_SIDECAR_INTERNAL_PORT
OCR_HEALTH_PORT=$VISION_SIDECAR_HEALTH_PORT
ENVEOF
    fi
  )
  chown "$name:$name" "$env_path"
  printf '%s' "$token"
}

start_tenant_vision() {
  local name="$1"
  ensure_container_runtime

  local vision_port
  vision_port="$(ports_get "$name" vision_service)" || true
  if [[ -z "$vision_port" ]]; then
    say "no vision_service port allocated for $name (skipping vision sidecar)"
    return 0
  fi

  if ! _ensure_tenant_image "$name" "$VISION_SIDECAR_IMAGE"; then
    say "vision sidecar image not available; run 'build-vision-sidecar' first (skipping)"
    return 0
  fi

  local container_name="lunarwing-vision-$name"

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-vision-${name}.container"
    render_vision_quadlet "$name"
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if _container_config_changed "$name" "lunarwing-vision-${name}" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "lunarwing-vision-${name}.service" "lunarwing-vision-${name}"
    fi
    if _systemctl_user "$name" start "lunarwing-vision-${name}.service" >/dev/null 2>&1; then
      _store_container_hash "$name" "lunarwing-vision-${name}" "$quadlet_file"
      say "vision sidecar ready via quadlet (lunarwing-vision-${name}.service, port $vision_port)"
    else
      say "WARNING: lunarwing-vision-${name}.service failed to start" >&2
      _systemctl_user "$name" status "lunarwing-vision-${name}.service" --no-pager >&2 || true
    fi
    return 0
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    if _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "vision sidecar already running ($container_name, port $vision_port)"
    else
      _ctr "$name" start "$container_name" >/dev/null
      say "vision sidecar restarted ($container_name, port $vision_port)"
    fi
  else
    local vision_env_path
    vision_env_path="$(tenant_env_dir "$name")/vision.env"
    [[ -f "$vision_env_path" ]] || write_tenant_vision_env "$name" >/dev/null

    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)

    # v10: also publish the tenant's dedicated vision_health port -> container
    # 8089, so the host self-heal pipeline can probe /health directly (mirrors
    # the nanocode/pebble health_publish pattern).
    local -a health_publish=()
    local host_health_port
    host_health_port="$(ports_get "$name" vision_health)" || true
    [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:${VISION_SIDECAR_HEALTH_PORT}")

    say "creating vision sidecar container $container_name on port $vision_port"
    _ctr "$name" run -d \
      --name "$container_name" \
      --env-file "$vision_env_path" \
      -p "127.0.0.1:${vision_port}:${VISION_SIDECAR_INTERNAL_PORT}" \
      "${health_publish[@]}" \
      "${restart_arg[@]}" \
      "$VISION_SIDECAR_IMAGE" >/dev/null
    say "vision sidecar ready ($container_name, port $vision_port)"
  fi

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" ]]; then
    render_vision_openrc_unit "$name"
    rc-update add "$container_name" default >/dev/null 2>&1 || true
    rc-service "$container_name" start >/dev/null 2>&1 || \
      say "WARNING: rc-service $container_name start returned non-zero (container may already be up)"
  fi
}

stop_tenant_vision() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-vision-$name"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container_name}" ]]; then
    rc-service "$container_name" stop >/dev/null 2>&1 || true
    say "vision sidecar stopped ($container_name)"
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "vision sidecar stopped ($container_name)"
  fi
}

render_vision_openrc_unit() {
  local name="$1"
  ensure_container_runtime
  local vision_port runtime_bin container uid home
  vision_port="$(ports_get "$name" vision_service)"
  [[ -n "$vision_port" ]] || return 0
  [[ -n "${CONTAINER_RT:-}" ]] && runtime_bin="$(command -v "$CONTAINER_RT" 2>/dev/null || true)"
  container="lunarwing-vision-${name}"
  uid="$(id -u "$name" 2>/dev/null || echo "")"
  home="$(tenant_home "$name")"

  cat >"/etc/init.d/${container}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing vision sidecar ($name)"

: "\${vis_runtime:=$runtime_bin}"
: "\${vis_container:=$container}"
: "\${vis_rootless:=$MT_ROOTLESS}"
: "\${vis_user:=$name}"
: "\${vis_home:=$home}"
: "\${vis_uid:=$uid}"
: "\${vis_port:=$vision_port}"
: "\${vis_internal_port:=$VISION_SIDECAR_INTERNAL_PORT}"
: "\${vis_health_internal_port:=$VISION_SIDECAR_HEALTH_PORT}"
: "\${vis_image:=$VISION_SIDECAR_IMAGE}"
: "\${vis_env_file:=$(tenant_env_dir "$name")/vision.env}"
: "\${vis_wait:=30}"

depend() {
    need net localmount
    after firewall lunarwing-${name}
}

_vis() {
    if [ "\${vis_rootless}" = "true" ]; then
        sudo -u "\${vis_user}" env HOME="\${vis_home}" XDG_RUNTIME_DIR="/run/user/\${vis_uid}" "\${vis_runtime}" "\$@"
    else
        "\${vis_runtime}" "\$@"
    fi
}

_vis_healthy() {
    [ "\$(_vis inspect -f '{{.State.Running}}' "\${vis_container}" 2>/dev/null)" = "true" ] || return 1
    _vis exec "\${vis_container}" curl -sf -o /dev/null --max-time 3 "http://127.0.0.1:\${vis_health_internal_port}/health" 2>/dev/null
}

start() {
    [ -n "\${vis_runtime}" ] && [ -x "\${vis_runtime}" ] || { ewarn "no container runtime; skipping vision for $name"; return 0; }
    ebegin "Starting vision sidecar (\${vis_container})"
    if [ "\${vis_rootless}" = "true" ]; then
        checkpath -d -m 0700 -o "\${vis_user}:\${vis_user}" "/run/user/\${vis_uid}"
    fi
    _vis start "\${vis_container}" >/dev/null 2>&1 || { eend 1 "container start failed"; return 1; }
    _w=0
    while ! _vis_healthy; do
        _w=\$((_w + 1))
        [ "\$_w" -lt "\${vis_wait}" ] || { eend 1 "vision sidecar not healthy after \${vis_wait}s"; return 1; }
        sleep 1
    done
    eend 0
}

stop() {
    [ -n "\${vis_runtime}" ] && [ -x "\${vis_runtime}" ] || return 0
    ebegin "Stopping vision sidecar (\${vis_container})"
    _vis stop --time 30 "\${vis_container}" >/dev/null 2>&1
    eend 0
}

status() {
    if _vis_healthy; then
        einfo "\${vis_container}: started"; return 0
    fi
    einfo "\${vis_container}: stopped"; return 3
}
INITEOF
  chmod 0755 "/etc/init.d/${container}"
}

render_vision_quadlet() {
  local name="$1"
  local qdir vision_port vision_health_port env_path token
  qdir="$(tenant_quadlet_dir "$name")"
  vision_port="$(ports_get "$name" vision_service)"
  [[ -n "$vision_port" ]] || return 0
  vision_health_port="$(ports_get "$name" vision_health)" || true
  env_path="$(tenant_env_dir "$name")/vision.env"
  [[ -f "$env_path" ]] || write_tenant_vision_env "$name" >/dev/null
  token="$(grep '^LUNARWING_AUTH_TOKEN=' "$env_path" 2>/dev/null | cut -d= -f2- || true)"
  token="${token//%/%%}"
  mkdir -p "$qdir"

  {
    cat <<EOF
[Unit]
Description=LunarWing vision sidecar ($name)
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Container]
ContainerName=lunarwing-vision-${name}
Image=${VISION_SIDECAR_IMAGE}
PublishPort=127.0.0.1:${vision_port}:${VISION_SIDECAR_INTERNAL_PORT}
Environment=OCR_PORT=${VISION_SIDECAR_INTERNAL_PORT}
Environment=OCR_HEALTH_PORT=${VISION_SIDECAR_HEALTH_PORT}
Environment=LUNARWING_AUTH_TOKEN=${token}
EOF
    # Publish the per-tenant dedicated vision health port (v10) -> container's
    # 8089, so the host self-heal pipeline can probe /health independently of
    # OCR traffic. The sidecar listens on OCR_HEALTH_PORT=8089 internally.
    [[ -n "$vision_health_port" ]] && printf 'PublishPort=127.0.0.1:%s:%s\n' "$vision_health_port" "$VISION_SIDECAR_HEALTH_PORT"
    if [[ -f "$env_path" ]]; then
      printf 'EnvironmentFile=%s\n' "$env_path"
    fi

    cat <<EOF

[Service]
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
EOF
  } > "$qdir/lunarwing-vision-${name}.container"
}

# ── PostgreSQL container ─────────────────────────────────────────────────────

start_tenant_postgres() {
  local name="$1"
  ensure_container_runtime

  local pg_port container_name
  pg_port="$(ports_get "$name" postgres)"
  container_name="lunarwing-pg-$name"

  # ── Rootful→rootless data-orphan guard ──────────────────────────────────────
  # v1.1.0 ran PG ROOTFUL (root's container store, data in the container writable
  # layer, no named volume); v1.1.4 defaults MT_ROOTLESS=true (rootless, tenant
  # store). If a legacy ROOT-store container still exists and the tenant has NO
  # rootless container yet, creating the rootless PG here would silently bring the
  # tenant up on an EMPTY database and orphan the v1.1.0 data. Refuse by default.
  # Fires ONLY in this exact window (rootless target + root container present +
  # rootless container absent), so fresh tenants and already-migrated tenants are
  # never affected. Escape hatches: migrate (upgrade-tenant.sh), keep rootful
  # (LUNARWING_MT_ROOTLESS=false), or acknowledge an intended fresh DB
  # (LUNARWING_MT_ACK_ROOTLESS_FLIP=<tenant> — set automatically by the migration
  # tool once it has a verified backup in hand). The ack is a comma-separated list
  # of tenant NAMES, not a global boolean, so a stray `export …=1` cannot silence
  # the guard for OTHER un-migrated tenants in the same shell/CI session.
  #
  # Coverage note: this fires only when the rootless container is ABSENT. The
  # "rootless exists but is EMPTY while a root orphan persists" case is deliberately
  # NOT caught here — the post-migration steady state (live rootless DB + a root
  # container kept as a rollback net) also has both present, and a start-time probe
  # can't tell them apart without the container running, so guarding it here would
  # break every legitimate restart. That case is surfaced by upgrade-preflight.sh
  # and blocked by `upgrade-tenant.sh --prune-old-root` (refuses to delete the root
  # copy while the rootless DB is empty).
  if [[ "$MT_ROOTLESS" == "true" && "$CONTAINER_RT" == "podman" ]] \
     && ! _ctr "$name" inspect "$container_name" &>/dev/null \
     && "$CONTAINER_RT" inspect "$container_name" &>/dev/null; then
    {
      say "############################################################"
      say "# DATA-ORPHAN GUARD — tenant '$name'"
      say "# A ROOTFUL (root-store) '$container_name' exists, but MT_ROOTLESS=true"
      say "# would create a NEW, EMPTY rootless database and orphan the existing"
      say "# v1.1.0 data (still recoverable from the root container until removed)."
      say "#"
      say "# Choose one:"
      say "#   migrate data:     sudo ic/scripts/upgrade-tenant.sh $name"
      say "#   keep rootful:     LUNARWING_MT_ROOTLESS=false <re-run this command>"
      say "#   intended fresh DB: LUNARWING_MT_ACK_ROOTLESS_FLIP=$name <re-run>"
      say "############################################################"
    } >&2
    if [[ ",${LUNARWING_MT_ACK_ROOTLESS_FLIP:-}," != *",$name,"* ]]; then
      die "refusing to create an empty rootless PG over existing root-store data for '$name' (see guard above)"
    fi
    say "LUNARWING_MT_ACK_ROOTLESS_FLIP lists '$name' — proceeding with a fresh rootless DB; root-store data left intact." >&2
  fi

  # systemd + rootless podman: a Quadlet .container owns the lifecycle (boot-
  # persistent, health-monitored, self-healable). Quadlet creates the container,
  # so skip the imperative `_ctr run` below; keep the pg_isready gate (via exec).
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    render_pg_quadlet "$name"
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if ! _systemctl_user "$name" start "lunarwing-pg-${name}.service" >/dev/null 2>&1; then
      say "WARNING: lunarwing-pg-${name}.service failed to start" >&2
      _systemctl_user "$name" status "lunarwing-pg-${name}.service" --no-pager >&2 || true
    fi
    # Gate on the container's own health (the Quadlet defines a pg_isready
    # HealthCmd) OR a direct pg_isready, rather than a bare `exec` loop: on a fresh
    # volume the initdb cycle (temp server up→down→restart) makes a single exec
    # probe flap, which previously exhausted the budget and `die`d — aborting the
    # WHOLE provision and leaving a half-baked tenant (F2). A slow first boot must
    # NOT strand the tenant: warn and continue (the unit has Restart=on-failure and
    # the host health pipeline/self-heal converge it), so add-tenant still renders
    # units and installs the pipeline.
    local q_attempts=0 q_ready=false q_health=""
    while (( q_attempts < 120 )); do
      q_health="$(_ctr "$name" inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{end}}' "$container_name" 2>/dev/null || true)"
      if [[ "$q_health" == healthy ]] || _ctr "$name" exec "$container_name" pg_isready -U lunarwing -q 2>/dev/null; then
        q_ready=true; break
      fi
      q_attempts=$((q_attempts + 1)); sleep 1
    done
    if [[ "$q_ready" == true ]]; then
      say "PostgreSQL ready via quadlet ($container_name, port $pg_port)"
    else
      say "WARNING: PostgreSQL for $name not confirmed ready after ${q_attempts}s (health=${q_health:-none}); continuing — pg has Restart=on-failure and the health pipeline will converge it." >&2
    fi
    return 0
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    if _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "PostgreSQL already running ($container_name, port $pg_port)"
      return 0
    fi
    say "starting existing PostgreSQL container $container_name"
    if ! _ctr "$name" start "$container_name" >/dev/null; then
      say "WARNING: could not start existing PostgreSQL container $container_name for $name; continuing — re-run start-tenant or let the health pipeline converge it." >&2
      return 0
    fi
  else
    say "creating PostgreSQL container $container_name on port $pg_port"
    # Named volume (not anonymous) so the data has a stable, inspectable,
    # exportable identity for backups; rootless podman auto-chowns it inside the
    # tenant user namespace. --restart is a no-op under rootless podman (no
    # daemon — OpenRC owns lifecycle), so only set it for rootful docker.
    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    # Pass POSTGRES_PASSWORD via a transient 0600 --env-file rather than `-e` so
    # the per-tenant secret never lands on the container-runtime argv (readable in
    # /proc/<pid>/cmdline by other local users). Removed right after creation; the
    # Quadlet path keeps it in its own 0600 unit file for the same reason.
    local pg_init_env
    pg_init_env="$(tenant_env_dir "$name")/.pg-init.env"
    ( umask 077; printf 'POSTGRES_PASSWORD=%s\n' "$(tenant_pg_password "$name")" >"$pg_init_env" )
    chown "$name:$name" "$pg_init_env" 2>/dev/null || true
    # O2: don't let a pg CREATE failure abort the whole provision (set -e). The F2
    # gate already downgrades a slow-readiness race to warn+continue; mirror that
    # for the create call (and the start above) so a transient runtime hiccup
    # doesn't strand the tenant half-baked (no units rendered, no health pipeline
    # installed). add-tenant is resumable (F4): fix the cause and re-run.
    if ! _ctr "$name" run -d \
      --name "$container_name" \
      -e POSTGRES_USER=lunarwing \
      --env-file "$pg_init_env" \
      -e POSTGRES_DB=lunarwing \
      -p "127.0.0.1:${pg_port}:5432" \
      -v "lunarwing-pg-${name}:/var/lib/postgresql/data" \
      "${restart_arg[@]}" \
      "$PG_IMAGE" >/dev/null; then
      rm -f "$pg_init_env"
      # Remove the partial/failed container so a resumed start-tenant re-creates it
      # cleanly, rather than taking the "start existing" branch on a broken shell.
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
      say "WARNING: PostgreSQL container create failed for $name; add-tenant still renders units + installs the health pipeline (so exit 0 does NOT imply pg is up). Fix the cause and re-run start-tenant." >&2
      return 0
    fi
    rm -f "$pg_init_env"
  fi

  local attempts=0
  while ! _ctr "$name" exec "$container_name" pg_isready -U lunarwing -q 2>/dev/null; do
    attempts=$((attempts + 1))
    if (( attempts >= 90 )); then
      say "WARNING: PostgreSQL for $name not confirmed ready after ${attempts}s; continuing — the host health pipeline/self-heal will converge it." >&2
      return 0
    fi
    sleep 1
  done
  say "PostgreSQL ready ($container_name, port $pg_port)"
}

stop_tenant_postgres() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-pg-$name"
  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "PostgreSQL stopped ($container_name)"
  fi
}

reset_tenant_postgres() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-pg-$name"
  stop_tenant_postgres "$name"
  _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
  # Remove the named data volume too so a subsequent create starts fresh (matches
  # the pre-named-volume behaviour where the anonymous volume was orphaned on rm).
  _ctr "$name" volume rm "lunarwing-pg-${name}" >/dev/null 2>&1 || true
  say "PostgreSQL removed ($container_name, data volume cleared)"
}

# Rotate an existing tenant's PG password to a fresh random one. Unlike a new
# tenant (where POSTGRES_PASSWORD seeds an empty datadir), an initialised DB needs
# an in-place ALTER ROLE, then the persisted secret + DATABASE_URL updated, then a
# daemon restart so it reconnects with the new credential. The new password is hex
# (URL/SQL-safe) and is never echoed.
rotate_tenant_pg_password() {
  local name="$1"
  ensure_container_runtime
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  local container_name="lunarwing-pg-$name"
  _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
    || die "PostgreSQL container $container_name is not running; start the tenant first"

  local envf pg_port new_pw secret_file tmp
  envf="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$envf" ]] || die "tenant env not found: $envf"
  pg_port="$(ports_get "$name" postgres)"
  new_pw="$(generate_token | cut -c1-32)"

  # Change the live role password. psql connects over the container's local unix
  # socket (trust auth in the postgres image), and the new value is fed on stdin —
  # never on argv or in logs. A hex value carries no SQL-quoting hazard.
  if ! printf "ALTER ROLE lunarwing PASSWORD '%s';\n" "$new_pw" \
       | _ctr "$name" exec -i "$container_name" psql -v ON_ERROR_STOP=1 -U lunarwing -d lunarwing -q >/dev/null 2>&1; then
    die "failed to ALTER ROLE password inside $container_name (is the DB healthy?)"
  fi

  # Persist the new secret (source of truth) ...
  secret_file="$(tenant_env_dir "$name")/pg.secret"
  ( umask 077; printf '%s\n' "$new_pw" > "$secret_file" )
  chown "$name:$name" "$secret_file" 2>/dev/null || true

  # ... and rewrite DATABASE_URL in place (preserving the env file's owner/perms).
  # Temp lives beside the env file (0600), not in shared /tmp, so the cleartext
  # password isn't briefly exposed there — matching the convention used elsewhere.
  tmp="$(mktemp "$envf.tmp.XXXXXX")"
  sed "s#^DATABASE_URL=.*#DATABASE_URL=postgres://lunarwing:${new_pw}@127.0.0.1:${pg_port}/lunarwing#" "$envf" >"$tmp"
  cat "$tmp" >"$envf"
  rm -f "$tmp"

  say "Rotated PostgreSQL password for tenant '$name'."
  say "IMPORTANT: the running daemon still holds the old credential — restart to apply:"
  say "    lunarwing-mt-admin.sh restart-tenant $name"
}

# ── PostgreSQL backup / restore ──────────────────────────────────────────────

# pg_dump a tenant's database to a timestamped custom-format file under
# $BACKUP_DIR/<tenant>/. Runs pg_dump inside the tenant's pg container via _ctr,
# so it is init- and runtime-agnostic (rootless podman or rootful docker). Safe
# on a live database (MVCC snapshot). Prunes to the most recent $BACKUP_KEEP.
backup_tenant_postgres() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_container_runtime
  local container_name="lunarwing-pg-$name"

  _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
    || die "PostgreSQL not running for '$name' ($container_name) — start the tenant first"

  local dir ts dest tmp
  dir="$BACKUP_DIR/$name"
  mkdir -p "$dir"
  chmod 0700 "$BACKUP_DIR" "$dir" 2>/dev/null || true
  ts="$(date +%Y%m%d%H%M%S)"
  dest="$dir/${name}-${ts}.dump"
  tmp="$dest.partial"

  say "backing up '$name' -> $dest"
  # -Fc: compressed custom format (restore via pg_restore). umask 077 so the
  # .partial is never world-readable, even mid-dump; rename on success so an
  # interrupted dump never looks complete.
  if ( umask 077; _ctr "$name" exec "$container_name" pg_dump -U lunarwing -Fc lunarwing > "$tmp" ); then
    mv "$tmp" "$dest"
    say "backup complete: $dest ($(du -h "$dest" 2>/dev/null | cut -f1))"
  else
    rm -f "$tmp"
    die "pg_dump failed for '$name'"
  fi
  _prune_tenant_backups "$name"
}

# Keep only the most recent $BACKUP_KEEP dumps for a tenant (0/unset = keep all).
_prune_tenant_backups() {
  local name="$1" dir="$BACKUP_DIR/$1"
  [[ "${BACKUP_KEEP:-0}" =~ ^[0-9]+$ && "$BACKUP_KEEP" -gt 0 ]] || return 0
  local -a dumps=("$dir"/*.dump)
  [[ -e "${dumps[0]:-}" ]] || return 0          # glob did not match -> nothing to prune
  local n=${#dumps[@]} i
  (( n > BACKUP_KEEP )) || return 0
  for (( i=0; i < n - BACKUP_KEEP; i++ )); do   # glob is ascending (timestamp) = oldest first
    rm -f "${dumps[$i]}"
    say "pruned old backup: ${dumps[$i]}"
  done
}

# List existing backups for one tenant or all.
list_tenant_backups() {
  local filter="${1:-}" names
  if [[ -n "$filter" ]]; then names="$(sanitize_name "$filter")"; else names="$(all_tenant_names)"; fi
  [[ -n "$names" ]] || { say "no tenants"; return 0; }
  say "=== Backups (under $BACKUP_DIR) ==="
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    say ""
    say "$name:"
    if compgen -G "$BACKUP_DIR/$name/*.dump" >/dev/null 2>&1; then
      ls -1sh "$BACKUP_DIR/$name"/*.dump 2>/dev/null | sed 's/^/  /'
    else
      say "  (no backups)"
    fi
  done <<< "$names"
}

# Restore a tenant's database from a custom-format dump. DESTRUCTIVE: pg_restore
# --clean --if-exists DROPs and recreates objects. Requires the tenant daemon to
# be stopped (no concurrent writes) and an explicit --yes.
restore_tenant_postgres() {
  local name="$1" file="$2" confirmed="${3:-false}"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  [[ -f "$file" ]] || die "backup file not found: $file"
  # Reject anything that is not a custom-format archive before touching the DB
  # (custom-format pg_dump files begin with the magic "PGDMP").
  [[ "$(head -c5 "$file" 2>/dev/null)" == "PGDMP" ]] \
    || die "not a custom-format pg_dump archive (missing PGDMP header): $file"
  [[ "$confirmed" == "true" ]] || die "restore DROPs and recreates the database for '$name'. Re-run with --yes to confirm."
  ensure_container_runtime
  ensure_init_system
  local container_name="lunarwing-pg-$name"

  _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
    || die "PostgreSQL not running for '$name' — start the tenant's pg container first"

  # Refuse unless we can POSITIVELY confirm the daemon is stopped (concurrent
  # writes corrupt a restore). Fail closed: if the service tool is missing or the
  # state is indeterminate, never assume "stopped".
  local daemon_state="unknown"
  if [[ "$INIT_SYSTEM" == "systemd" ]] && command -v systemctl >/dev/null 2>&1; then
    if _systemctl_user "$name" is-active --quiet "lunarwing-${name}.service" 2>/dev/null; then daemon_state="active"; else daemon_state="stopped"; fi
  elif [[ "$INIT_SYSTEM" == "openrc" ]] && command -v rc-service >/dev/null 2>&1; then
    if rc-service "lunarwing-${name}" status >/dev/null 2>&1; then daemon_state="active"; else daemon_state="stopped"; fi
  fi
  case "$daemon_state" in
    stopped) : ;;
    active)  die "stop the daemon first: $0 stop-tenant $name  (restart after restore)" ;;
    *)       die "cannot confirm the '$name' daemon is stopped (no $INIT_SYSTEM service tool?); stop it manually, then re-run" ;;
  esac

  say "restoring '$name' from $file (single transaction, DROP + recreate) ..."
  # --single-transaction: all-or-nothing. A mid-restore failure rolls the whole
  # DROP+recreate back, so a failed restore never leaves the DB half-dropped.
  if _ctr "$name" exec -i "$container_name" pg_restore -U lunarwing -d lunarwing --single-transaction --clean --if-exists < "$file"; then
    say "restore complete for '$name'. Restart the tenant: $0 start-tenant $name"
  else
    die "pg_restore failed for '$name' — rolled back (single transaction); the database is unchanged"
  fi
}

# ── Systemd service units ────────────────────────────────────────────────────

# Render a per-tenant Quadlet .container for the Postgres container. The podman
# user-generator turns this into lunarwing-pg-<t>.service at `systemctl --user
# daemon-reload`; [Install] makes the lingering user manager start it at boot.
# Quadlet owns creation (the imperative `_ctr run` is skipped on this path), so
# the named volume below preserves data across container replacement.
render_pg_quadlet() {
  local name="$1"
  local qdir pg_port pg_password
  qdir="$(tenant_quadlet_dir "$name")"
  pg_port="$(ports_get "$name" postgres)"
  pg_password="$(tenant_pg_password "$name")"   # inlined into the 0600 tenant-owned .container
  mkdir -p "$qdir"
  cat >"$qdir/lunarwing-pg-${name}.container" <<EOF
[Unit]
Description=LunarWing Postgres container ($name)
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Container]
ContainerName=lunarwing-pg-${name}
Image=${PG_IMAGE}
PublishPort=127.0.0.1:${pg_port}:5432
Volume=lunarwing-pg-${name}:/var/lib/postgresql/data
Environment=POSTGRES_USER=lunarwing
Environment=POSTGRES_PASSWORD=${pg_password}
Environment=POSTGRES_DB=lunarwing
HealthCmd=pg_isready -U lunarwing -q
HealthInterval=10s
HealthTimeout=3s
HealthRetries=5
HealthStartPeriod=30s

[Service]
Restart=on-failure
RestartSec=5
TimeoutStartSec=120

[Install]
WantedBy=default.target
EOF
  chmod 0600 "$qdir/lunarwing-pg-${name}.container"
  chown -R "$name:$name" "$(tenant_home "$name")/.config/containers"
}

# Render a per-tenant Quadlet .container for an external worker (nanocode/pebble).
# Mirrors the imperative env from start_tenant_<worker>: the GATEWAY_AUTH_TOKEN ->
# AGENT_AUTH_TOKEN and LLM_API_KEY -> TENSORZERO_API_KEY remap is resolved here and
# inlined as Environment= in a 0600 tenant-owned unit (no secret leaves the file).
# Returns early (no unit) when the worker has no allocated wss port.
render_worker_quadlet() {
  local name="$1" worker="$2" health_port="${3:-8443}"
  local qdir wss_port workspace_dir env_dir tenant_env_path worker_env_path host_health_port
  qdir="$(tenant_quadlet_dir "$name")"
  wss_port="$(ports_get "$name" "${worker}_wss")"
  host_health_port="$(ports_get "$name" "${worker}_health")"
  [[ -n "$wss_port" ]] || return 0
  env_dir="$(tenant_env_dir "$name")"
  tenant_env_path="$env_dir/lunarwing.env"
  worker_env_path="$env_dir/${worker}.env"
  workspace_dir="$(tenant_lw_root "$name")/${worker}-workspace"
  mkdir -p "$workspace_dir"; chown "$name:$name" "$workspace_dir"; chmod 777 "$workspace_dir"
  mkdir -p "$qdir"

  local agent_token tz_key
  agent_token="$(grep '^GATEWAY_AUTH_TOKEN=' "$tenant_env_path" 2>/dev/null | cut -d= -f2- || true)"
  tz_key="$(grep '^LLM_API_KEY=' "$tenant_env_path" 2>/dev/null | cut -d= -f2- || true)"
  local nanocode_model nanocode_base_url
  nanocode_model="$(grep '^NANOCODE_MODEL=' "$tenant_env_path" 2>/dev/null | cut -d= -f2- || true)"
  nanocode_base_url="$(grep '^NANOCODE_BASE_URL=' "$tenant_env_path" 2>/dev/null | cut -d= -f2- || true)"
  # systemd treats % as a unit specifier; escape so a token containing % survives.
  agent_token="${agent_token//%/%%}"
  tz_key="${tz_key//%/%%}"

  # SSH agent socket: the gateway creates an SSH agent server at startup,
  # with its socket at <tenant_home>/lunarwing/run/ssh-agent.sock (NOT /tmp,
  # because the daemon runs with PrivateTmp=true — a /tmp socket would be
  # invisible to podman containers). Always include the volume mount + env var
  # in the quadlet, even if the socket doesn't exist yet at render time — the
  # daemon creates it before the worker needs it (workers only use SSH when a
  # create_job with SSH commands is invoked, well after startup). If SSH is
  # not configured for the tenant, the socket simply won't exist and SSH
  # commands from the worker will fail with a clear "no agent" error.
  local ssh_agent_socket="$(tenant_run_dir "$name")/ssh-agent.sock"

  {
    cat <<EOF
[Unit]
Description=LunarWing ${worker} worker ($name)
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Container]
ContainerName=lunarwing-${worker}-${name}
Image=lunarwing-worker-${worker}:latest
PublishPort=127.0.0.1:${wss_port}:${wss_port}
Volume=${workspace_dir}:/workspace:z
Environment=LUNARWING_WORKER_ID=worker-${worker}-${name}
Environment=WS_PORT=${wss_port}
Environment=HEALTH_PORT=${health_port}
Environment=WS_BIND_HOST=0.0.0.0
Environment=WS_PATH=/ws/agent
EOF
    # SSH agent socket mount + env (always included — the daemon creates the
    # socket at startup; if SSH isn't configured, the socket won't exist and
    # SSH commands from the worker will fail with a clear "no agent" error).
    # :z label so SELinux (Enforcing on Fedora) permits container_t access to
    # the tenant-home-labeled socket.
    printf 'Volume=%s:/tmp/ssh-agent.sock:z\n' "$ssh_agent_socket"
    printf 'Environment=SSH_AUTH_SOCK=/tmp/ssh-agent.sock\n'
    # Publish the per-tenant dedicated health port (v8) -> container's 8443, so
    # the host self-heal pipeline can probe /health directly. The container still
    # listens on HEALTH_PORT=8443 internally (matches the image's baked HEALTHCHECK).
    [[ -n "$host_health_port" ]] && printf 'PublishPort=127.0.0.1:%s:8443\n' "$host_health_port"
    if [[ "$worker" == "nanocode" ]]; then
      printf 'Environment=NANOCODE_MODE=websocket\n'
      printf 'Environment=WS_ROLE=server\n'
    elif [[ "$worker" == "pebble" ]]; then
      printf 'Environment=PEBBLE_MODE=websocket\n'
    elif [[ "$worker" == "opencode" ]]; then
      printf 'Environment=OPENCODE_MODE=websocket\n'
      printf 'Environment=WS_ROLE=server\n'
    fi
    [[ -n "$agent_token" ]] && printf 'Environment=AGENT_AUTH_TOKEN=%s\n' "$agent_token"
    [[ "$worker" == "nanocode" && -n "$tz_key" ]] && printf 'Environment=TENSORZERO_API_KEY=%s\n' "$tz_key"
    [[ "$worker" == "nanocode" && -n "$nanocode_model" ]] && printf 'Environment=NANOCODE_MODEL=%s\n' "$nanocode_model"
    [[ "$worker" == "nanocode" && -n "$nanocode_base_url" ]] && printf 'Environment=NANOCODE_BASE_URL=%s\n' "$nanocode_base_url"
    [[ "$worker" == "opencode" && -n "$tz_key" ]] && printf 'Environment=TENSORZERO_API_KEY=%s\n' "$tz_key"
    local oc_model oc_base_url
    oc_model="$(grep '^OPENCODE_MODEL=' "$tenant_env_path" 2>/dev/null | cut -d= -f2- || true)"
    oc_base_url="$(grep '^OPENCODE_BASE_URL=' "$tenant_env_path" 2>/dev/null | cut -d= -f2- || true)"
    [[ "$worker" == "opencode" && -n "$oc_model" ]] && printf 'Environment=OPENCODE_MODEL=%s\n' "$oc_model"
    [[ "$worker" == "opencode" && -n "$oc_base_url" ]] && printf 'Environment=OPENCODE_BASE_URL=%s\n' "$oc_base_url"
    # Operator override file (optional). EnvironmentFile= has existed since the
    # Quadlet 4.4 debut, so it is safe at our >= 4.6 floor.
    [[ -f "$worker_env_path" ]] && printf 'EnvironmentFile=%s\n' "$worker_env_path"
    [[ "$worker" == "nanocode" || "$worker" == "opencode" ]] && printf 'Exec=--mode websocket\n'
    cat <<EOF
HealthCmd=curl -sf http://127.0.0.1:${health_port}/health || exit 1
HealthInterval=15s
HealthTimeout=5s
HealthRetries=3
HealthStartPeriod=30s

[Service]
Restart=on-failure
RestartSec=5
TimeoutStartSec=120

[Install]
WantedBy=default.target
EOF
  } >"$qdir/lunarwing-${worker}-${name}.container"
  chmod 0600 "$qdir/lunarwing-${worker}-${name}.container"
  chown -R "$name:$name" "$(tenant_home "$name")/.config/containers"
}

# Render the WeeChat systemd user unit into <out_dir>.
# Loads only weechat.env (RELAY_PASSWORD), not the full lunarwing.env.
# ExecStop sends /upgrade -quit via the WeeChat FIFO so WeeChat saves its
# session (buffers, lines, connections) before exiting; next start restores
# it. Falls back to tmux kill-session if the graceful stop fails.
_render_weechat_systemd_unit() {  # <tenant> <out_dir>
  local name="$1" out_dir="$2"
  local weechat_home env_dir stop_helper
  weechat_home="$(tenant_weechat_home "$name")"
  env_dir="$(tenant_env_dir "$name")"
  stop_helper="$(tenant_lw_root "$name")/ic/scripts/lunarwing-weechat-stop.sh"
  cat >"$out_dir/lunarwing-weechat-${name}.service" <<EOF
[Unit]
Description=WeeChat IRC client ($name)
After=network.target

[Service]
Type=forking
ExecStart=$(command -v tmux) -L weechat-${name} new-session -d -s weechat '$(command -v weechat) --dir ${weechat_home}'
ExecStop=$stop_helper --weechat-home ${weechat_home} --tmux-socket weechat-${name} --session weechat
TimeoutStopSec=30
EnvironmentFile=$env_dir/weechat.env
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF
}

render_tenant_systemd_units() {
  local name="$1"
  ensure_container_runtime
  local user_unit_dir
  user_unit_dir="$(tenant_home "$name")/.config/systemd/user"
  mkdir -p "$user_unit_dir"
  chown -R "$name:$name" "$(tenant_home "$name")/.config"

  local repo env_dir state_dir proxy_port bridge_port weechat_port
  repo="$(tenant_repo "$name")"
  env_dir="$(tenant_env_dir "$name")"
  state_dir="$(tenant_state_dir "$name")"
  proxy_port="$(ports_get "$name" proxy)"
  bridge_port="$(ports_get "$name" bridge)"
  weechat_port="$(ports_get "$name" weechat)"

  local proxy_bin
  # Run from the tenant's own clone (in their home), not the admin's source repo,
  # so a tenant's services aren't coupled to another user's home directory.
  proxy_bin="$(tenant_lw_root "$name")/tensorzero-proxy-configurations/lunarwing-proxy.py"

  local ws_adapter_path
  ws_adapter_path="$(tenant_lw_root "$name")/lunarwing_weechat_wss/weechat_relay/ws_adapter.py"

  # Proxy unit (only when enabled)
  if tenant_proxy_enabled "$name"; then
  cat >"$user_unit_dir/lunarwing-proxy-${name}.service" <<EOF
[Unit]
Description=LunarWing TensorZero proxy ($name)
After=network.target

[Service]
Type=simple
ExecStart=$(command -v python3) $proxy_bin --port $proxy_port --bind 127.0.0.1 --tensorzero $DEFAULT_TENSORZERO_URL
EnvironmentFile=$env_dir/proxy.env
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
EOF
  fi

  # WeeChat unit (runs in tmux so you can attach: tmux -L weechat-${name} attach)
  local weechat_home
  weechat_home="$(tenant_home "$name")/.config/weechat"
  mkdir -p "$weechat_home"
  chown "$name:$name" "$weechat_home"

  _render_weechat_systemd_unit "$name" "$user_unit_dir"

  # WeeChat WS adapter unit
  cat >"$user_unit_dir/lunarwing-weechat-adapter-${name}.service" <<EOF
[Unit]
Description=LunarWing WeeChat WS adapter ($name)
After=network.target lunarwing-weechat-${name}.service
Requires=lunarwing-weechat-${name}.service
PartOf=lunarwing-${name}.service

[Service]
Type=simple
WorkingDirectory=$(dirname "$ws_adapter_path")
EnvironmentFile=$env_dir/lunarwing.env
ExecStart=$(command -v python3) $ws_adapter_path
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
EOF

  if tenant_darkirc_enabled "$name"; then
    local darkirc_adapter_path
    darkirc_adapter_path="$(tenant_lw_root "$name")/darkirc_channel_for_lunarwing/darkirc/adapter/darkirc_adapter.py"

    cat >"$user_unit_dir/lunarwing-darkirc-adapter-${name}.service" <<EOF
[Unit]
Description=LunarWing DarkIRC adapter ($name)
After=network.target

[Service]
Type=simple
WorkingDirectory=$(dirname "$darkirc_adapter_path")
EnvironmentFile=$env_dir/darkirc-adapter.env
ExecStart=$(command -v python3) $darkirc_adapter_path
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
EOF

    # DarkIRC daemon unit
    cat >"$user_unit_dir/lunarwing-darkirc-${name}.service" <<EOF
[Unit]
Description=DarkIRC daemon ($name)
After=network.target

[Service]
Type=simple
ExecStart=$DARKIRC_BIN --config $(tenant_state_dir "$name")/darkirc/darkirc_config.toml
Restart=always
RestartSec=10
NoNewPrivileges=true
UMask=0077

[Install]
WantedBy=default.target
EOF
  fi

  # Bridge unit
  cat >"$user_unit_dir/xmpp-bridge-${name}.service" <<EOF
[Unit]
Description=LunarWing XMPP bridge ($name)
After=network.target
PartOf=lunarwing-${name}.service

[Service]
Type=simple
WorkingDirectory=$repo/bridges/xmpp-bridge
EnvironmentFile=$env_dir/xmpp-bridge.env
ExecStart=$repo/bridges/xmpp-bridge/target/${PROFILE}/xmpp-bridge
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
EOF

  # Postgres dependency — only when the pg Quadlet is rendered (systemd + rootless
  # podman with Quadlet support). Mirrors the OpenRC `need lunarwing-pg-<t>`.
  # Rootful docker has no pg unit (the container survives via --restart), so the
  # dependency is omitted there to avoid a Requires on a non-existent unit.
  local pg_dep_after="" pg_dep_requires=""
  if [[ "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    pg_dep_after="lunarwing-pg-${name}.service "
    pg_dep_requires="Requires=lunarwing-pg-${name}.service"
  fi

  local darkirc_sd_after="" darkirc_sd_wants=""
  if tenant_darkirc_enabled "$name"; then
    darkirc_sd_after=" lunarwing-darkirc-adapter-${name}.service"
    darkirc_sd_wants=" lunarwing-darkirc-adapter-${name}.service"
  fi

  local proxy_sd_after="" proxy_sd_wants=""
  if tenant_proxy_enabled "$name"; then
    proxy_sd_after=" lunarwing-proxy-${name}.service"
    proxy_sd_wants=" lunarwing-proxy-${name}.service"
  fi

  # Main daemon unit
  cat >"$user_unit_dir/lunarwing-${name}.service" <<EOF
[Unit]
Description=LunarWing AI assistant ($name)
After=network.target ${pg_dep_after}xmpp-bridge-${name}.service${proxy_sd_after} lunarwing-weechat-${name}.service lunarwing-weechat-adapter-${name}.service${darkirc_sd_after}
  Wants=xmpp-bridge-${name}.service${proxy_sd_wants} lunarwing-weechat-${name}.service lunarwing-weechat-adapter-${name}.service${darkirc_sd_wants}
${pg_dep_requires}

[Service]
Type=simple
WorkingDirectory=$repo
EnvironmentFile=$env_dir/lunarwing.env
Environment=PATH=/usr/local/bin:/usr/bin:/bin:/home/${name}/.cargo/bin
ExecStart=$repo/target/${PROFILE}/lunarwing --no-onboard run
Restart=always
RestartSec=5
TimeoutStartSec=60
TimeoutStopSec=30
KillSignal=SIGTERM
UMask=0077
NoNewPrivileges=true
PrivateTmp=true

[Install]
WantedBy=default.target
EOF

  # The pg + worker Quadlet .container units are rendered by the start functions
  # (start_tenant_postgres / start_tenant_<worker>), NOT here: those guard on
  # image availability, so a not-yet-built worker image never produces a unit
  # that would crash-loop at boot (Restart=always). The daemon's Requires= above
  # still resolves because start_tenant_postgres renders + starts pg first.

  chown -R "$name:$name" "$user_unit_dir"
  say "rendered systemd units for $name in $user_unit_dir"
}

_systemctl_user() {
  local name="$1"
  shift
  local uid
  uid="$(id -u "$name")"
  sudo -u "$name" XDG_RUNTIME_DIR="/run/user/$uid" systemctl --user "$@"
}

# Best-effort wait for the tenant's `systemd --user` manager + bus to be ready,
# so `systemctl --user` and the Quadlet generator work right after enable-linger
# (which can return before the user manager is fully up). Proceeds after ~10s.
_wait_user_manager() {
  local name="$1" uid i
  uid="$(id -u "$name" 2>/dev/null)" || return 0
  for i in $(seq 1 20); do
    [[ -S "/run/user/$uid/bus" ]] && return 0
    sleep 0.5
  done
  return 0
}

start_tenant_systemd() {
  local name="$1"
  _systemctl_user "$name" daemon-reload
  # Quadlet-generated units (pg, nanocode, pebble, vision) are NOT in the
  # enable_list: `systemctl enable` fails on generated/transient units with
  # "Failed to enable unit: ... is transient or generated", and under
  # `set -euo pipefail` that non-zero exit aborts the entire enable batch —
  # leaving every regular service disabled. Each Quadlet unit is already
  # started by its own start_tenant_* function (which renders the quadlet,
  # reloads the daemon, and starts the unit). The imperative `start` below
  # is belt-and-suspenders for the vision unit (harmless if already running).
  local enable_list=("lunarwing-${name}.service" "xmpp-bridge-${name}.service" "lunarwing-weechat-${name}.service" "lunarwing-weechat-adapter-${name}.service")
  if tenant_proxy_enabled "$name"; then
    enable_list+=("lunarwing-proxy-${name}.service")
  fi
  if tenant_darkirc_enabled "$name"; then
    enable_list+=("lunarwing-darkirc-${name}.service" "lunarwing-darkirc-adapter-${name}.service")
  fi
  _systemctl_user "$name" enable "${enable_list[@]}"
  if tenant_darkirc_enabled "$name"; then
    _systemctl_user "$name" start "lunarwing-darkirc-${name}.service"
    sleep 2
  fi
  _systemctl_user "$name" start "lunarwing-vision-${name}.service" 2>/dev/null || true
  _systemctl_user "$name" start "lunarwing-${name}.service"
  sleep 2
  if _systemctl_user "$name" is-active --quiet "lunarwing-${name}.service"; then
    say "lunarwing-${name}.service is active"
  else
    say "WARNING: lunarwing-${name}.service failed to start" >&2
    _systemctl_user "$name" status "lunarwing-${name}.service" --no-pager >&2 || true
    return 1
  fi
}

stop_tenant_systemd() {
  local name="$1"
  local uid
  uid="$(id -u "$name" 2>/dev/null)" || return 0

  for svc in "lunarwing-${name}.service" "xmpp-bridge-${name}.service" "lunarwing-proxy-${name}.service" "lunarwing-weechat-adapter-${name}.service" "lunarwing-weechat-${name}.service" "lunarwing-darkirc-adapter-${name}.service" "lunarwing-darkirc-${name}.service" "lunarwing-nanocode-${name}.service" "lunarwing-pebble-${name}.service" "lunarwing-opencode-${name}.service" "lunarwing-vision-${name}.service" "lunarwing-pg-${name}.service"; do
    if _systemctl_user "$name" is-active --quiet "$svc" 2>/dev/null; then
      _systemctl_user "$name" stop "$svc"
      say "stopped $svc"
    fi
  done
}

uninstall_tenant_systemd() {
  local name="$1"
  local user_unit_dir
  user_unit_dir="$(tenant_home "$name")/.config/systemd/user"

  for svc in "lunarwing-${name}.service" "xmpp-bridge-${name}.service" "lunarwing-proxy-${name}.service" "lunarwing-weechat-adapter-${name}.service" "lunarwing-weechat-${name}.service" "lunarwing-darkirc-adapter-${name}.service" "lunarwing-darkirc-${name}.service"; do
    rm -f "$user_unit_dir/$svc"
  done

  # Quadlet .container units (rootless pg + workers + vision sidecar). Remove the
  # worker containers (their workspace data is bind-mounted in the home); the pg
  # container + named volume are handled by stop_tenant_postgres /
  # reset_tenant_postgres so non-purge removals keep the data for a later re-add.
  local qdir; qdir="$(tenant_quadlet_dir "$name")"
  rm -f "$qdir/lunarwing-pg-${name}.container" \
        "$qdir/lunarwing-nanocode-${name}.container" \
        "$qdir/lunarwing-pebble-${name}.container" \
        "$qdir/lunarwing-opencode-${name}.container" \
        "$qdir/lunarwing-vision-${name}.container"
  if id -u "$name" >/dev/null 2>&1; then
    for w in nanocode pebble opencode; do
      _ctr "$name" rm -f "lunarwing-${w}-${name}" >/dev/null 2>&1 || true
    done
    _ctr "$name" rm -f "lunarwing-vision-${name}" >/dev/null 2>&1 || true
  fi

  _systemctl_user "$name" daemon-reload 2>/dev/null || true
  say "uninstalled systemd units for $name"
}

# Render the WeeChat OpenRC init script into <out_file>.
_render_weechat_openrc_unit() {  # <tenant> <out_file>
  local name="$1" out_file="$2"
  local weechat_home env_dir run_dir log_dir
  weechat_home="$(tenant_weechat_home "$name")"
  env_dir="$(tenant_env_dir "$name")"
  run_dir="$(tenant_run_dir "$name")"
  log_dir="$(tenant_log_dir "$name")"
  local stop_helper
  stop_helper="$(tenant_lw_root "$name")/ic/scripts/lunarwing-weechat-stop.sh"
  cat >"$out_file" <<INITEOF
#!/sbin/openrc-run

description="WeeChat IRC client ($name)"

: "\${weechat_user:=$name}"
: "\${weechat_group:=$name}"
: "\${weechat_home:=$weechat_home}"
: "\${weechat_pidfile:=$run_dir/weechat.pid}"
: "\${weechat_runtime_dir:=$run_dir}"
: "\${weechat_log_dir:=$log_dir}"
: "\${weechat_output_log:=\${weechat_log_dir}/weechat.log}"
: "\${weechat_error_log:=\${weechat_log_dir}/weechat.err}"
: "\${weechat_retry:=SIGTERM/30/KILL/5}"
: "\${weechat_env_file:=$env_dir/weechat.env}"
: "\${weechat_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${weechat_command:=$(command -v tmux)}"
: "\${weechat_binary:=$(command -v weechat)}"
: "\${weechat_stop_helper:=$stop_helper}"

command="\${weechat_openrc_env_exec}"
command_args="--env-file \${weechat_env_file} -- \${weechat_command} -L weechat-${name} new-session -d -s weechat '\${weechat_binary} --dir \${weechat_home}'"
command_user="\${weechat_user}:\${weechat_group}"
required_files="\${weechat_openrc_env_exec} \${weechat_command} \${weechat_binary} \${weechat_env_file}"

depend() {
    need net
    use dns
    after firewall
    before lunarwing-weechat-adapter-${name} lunarwing-${name}
}

start() {
    ebegin "Starting WeeChat ($name)"
    checkpath -d -m 0750 -o "\${weechat_user}:\${weechat_group}" "\${weechat_home}"
    checkpath -d -m 0750 -o "\${weechat_user}:\${weechat_group}" "\${weechat_runtime_dir}"
    start-stop-daemon --start --background --user "\${weechat_user}" \\
        --exec "\${weechat_openrc_env_exec}" -- \\
        --env-file "\${weechat_env_file}" -- "\${weechat_command}" \\
        -L weechat-${name} new-session -d -s weechat "\${weechat_binary} --dir \${weechat_home}"
    eend \$?
}

stop() {
    ebegin "Stopping WeeChat ($name)"
    # Graceful: /upgrade -quit via FIFO saves buffers before exit; falls back
    # to tmux kill-session if the FIFO is unavailable or times out.
    if [ -x "\${weechat_stop_helper}" ]; then
        "\${weechat_stop_helper}" \\
            --weechat-home "\${weechat_home}" \\
            --tmux-socket weechat-${name} \\
            --session weechat \\
            --timeout 20 || true
    else
        su -s /bin/sh "\${weechat_user}" -c "$(command -v tmux) -L weechat-${name} kill-session -t weechat 2>/dev/null" || true
    fi
    eend 0
}
INITEOF
  chmod 0755 "$out_file"
}

# ── OpenRC service units ─────────────────────────────────────────────────────

install_openrc_env_exec() {
  [[ -f "$OPENRC_ENV_EXEC_SRC" && ! -L "$OPENRC_ENV_EXEC_SRC" ]] \
    || die "OpenRC tenant env launcher is missing: $OPENRC_ENV_EXEC_SRC"
  install -d -o root -g root -m 0755 "$(dirname "$OPENRC_ENV_EXEC")"
  install -o root -g root -m 0755 "$OPENRC_ENV_EXEC_SRC" "$OPENRC_ENV_EXEC" \
    || die "failed to install OpenRC tenant env launcher"
  darkirc_validate_root_executable "$OPENRC_ENV_EXEC" \
    || die "OpenRC tenant env launcher failed ownership validation"
}

render_tenant_openrc_units() {
  local name="$1"
  local repo env_dir state_dir log_dir run_dir
  repo="$(tenant_repo "$name")"
  env_dir="$(tenant_env_dir "$name")"
  state_dir="$(tenant_state_dir "$name")"
  log_dir="$(tenant_log_dir "$name")"
  run_dir="$(tenant_run_dir "$name")"
  install_openrc_env_exec

  local proxy_port bridge_port weechat_port
  proxy_port="$(ports_get "$name" proxy)"
  bridge_port="$(ports_get "$name" bridge)"
  weechat_port="$(ports_get "$name" weechat)"

  local proxy_bin
  # Run from the tenant's own clone (in their home), not the admin's source repo,
  # so a tenant's services aren't coupled to another user's home directory.
  proxy_bin="$(tenant_lw_root "$name")/tensorzero-proxy-configurations/lunarwing-proxy.py"

  local ws_adapter_path
  ws_adapter_path="$(tenant_lw_root "$name")/lunarwing_weechat_wss/weechat_relay/ws_adapter.py"
  local ws_adapter_dir
  ws_adapter_dir="$(dirname "$ws_adapter_path")"

  local darkirc_rc_after=""
  if tenant_darkirc_enabled "$name"; then
    darkirc_rc_after=" lunarwing-darkirc-adapter-${name}"
  fi

  local proxy_rc_after=""
  if tenant_proxy_enabled "$name"; then
    proxy_rc_after=" lunarwing-proxy-${name}"
  fi

  local weechat_home
  weechat_home="$(tenant_home "$name")/.config/weechat"

  # Resolve the container runtime path + tenant identity so the dedicated
  # Postgres init service can bring the container up (rootless: as the tenant
  # user; rootful docker: as root). Empty runtime -> the pg service no-ops.
  ensure_container_runtime
  local pg_runtime_bin="" pg_container="lunarwing-pg-$name"
  [[ -n "${CONTAINER_RT:-}" ]] && pg_runtime_bin="$(command -v "$CONTAINER_RT" 2>/dev/null || true)"
  local pg_uid pg_home
  pg_uid="$(id -u "$name" 2>/dev/null || echo "")"
  pg_home="$(tenant_home "$name")"

  # ── Postgres container init script (dedicated service; the daemon needs it) ──
  # A first-class unit (not a daemon start_pre side-effect) so the host self-heal
  # pipeline — which auto-discovers /etc/init.d units — can remediate a crashed
  # Postgres independently.
  cat >"/etc/init.d/lunarwing-pg-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing Postgres container ($name)"

: "\${pg_runtime:=$pg_runtime_bin}"
: "\${pg_container:=$pg_container}"
: "\${pg_rootless:=$MT_ROOTLESS}"
: "\${pg_user:=$name}"
: "\${pg_home:=$pg_home}"
: "\${pg_uid:=$pg_uid}"
: "\${pg_wait:=60}"

depend() {
    need net localmount
    after firewall
    before lunarwing-${name}
}

# Run the container runtime for this tenant's container: rootless -> as the
# tenant user with their runtime dir + HOME; rootful -> as root unchanged.
_pg() {
    if [ "\${pg_rootless}" = "true" ]; then
        sudo -u "\${pg_user}" env HOME="\${pg_home}" XDG_RUNTIME_DIR="/run/user/\${pg_uid}" "\${pg_runtime}" "\$@"
    else
        "\${pg_runtime}" "\$@"
    fi
}

start() {
    [ -n "\${pg_runtime}" ] && [ -x "\${pg_runtime}" ] || { ewarn "no container runtime; skipping Postgres for $name"; return 0; }
    ebegin "Starting Postgres container (\${pg_container})"
    if [ "\${pg_rootless}" = "true" ]; then
        checkpath -d -m 0700 -o "\${pg_user}:\${pg_user}" "/run/user/\${pg_uid}"
    fi
    _pg start "\${pg_container}" >/dev/null 2>&1 || { eend 1 "container start failed"; return 1; }
    _w=0
    while ! _pg exec "\${pg_container}" pg_isready -U lunarwing -q 2>/dev/null; do
        _w=\$((_w + 1))
        [ "\$_w" -lt "\${pg_wait}" ] || { eend 1 "Postgres not ready after \${pg_wait}s"; return 1; }
        sleep 1
    done
    eend 0
}

stop() {
    [ -n "\${pg_runtime}" ] && [ -x "\${pg_runtime}" ] || return 0
    ebegin "Stopping Postgres container (\${pg_container})"
    _pg stop --time 30 "\${pg_container}" >/dev/null 2>&1
    eend 0
}

status() {
    # Emit the standard OpenRC "started"/"stopped" wording (not "running") so the
    # health-check parser (grep started|stopped) and the mt-admin status display
    # classify the container correctly instead of relying on the rc_exit fallback.
    # "started" requires the container be running AND Postgres actually accept
    # connections (pg_isready) — a Running-but-wedged DB (crash recovery, disk
    # full, max_connections) otherwise reports healthy and is never remediated.
    # Mirrors the worker units' _wk_healthy and this unit's own start() gate.
    if [ "\$(_pg inspect -f '{{.State.Running}}' "\${pg_container}" 2>/dev/null)" = "true" ] \\
       && _pg exec "\${pg_container}" pg_isready -U lunarwing -q -t 3 2>/dev/null; then
        einfo "\${pg_container}: started"; return 0
    fi
    einfo "\${pg_container}: stopped"; return 3
}
INITEOF
  chmod 0755 "/etc/init.d/lunarwing-pg-${name}"
  render_container_babysitter_unit "$name" pg "$pg_container" "$pg_uid" "$pg_home"

  # ── Main daemon init script ──
  cat >"/etc/init.d/lunarwing-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing AI assistant ($name)"

: "\${lunarwing_command:=$repo/target/${PROFILE}/lunarwing}"
: "\${lunarwing_args:=--no-onboard run}"
: "\${lunarwing_user:=$name}"
: "\${lunarwing_group:=$name}"
: "\${lunarwing_workdir:=$repo}"
: "\${lunarwing_pidfile:=$run_dir/lunarwing.pid}"
: "\${lunarwing_state_dir:=$state_dir}"
: "\${lunarwing_runtime_dir:=$run_dir}"
: "\${lunarwing_log_dir:=$log_dir}"
: "\${lunarwing_output_log:=\${lunarwing_log_dir}/lunarwing.log}"
: "\${lunarwing_error_log:=\${lunarwing_log_dir}/lunarwing.err}"
: "\${lunarwing_env_file:=$env_dir/lunarwing.env}"
: "\${lunarwing_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${lunarwing_umask:=0077}"
: "\${lunarwing_respawn_delay:=5}"
: "\${lunarwing_respawn_max:=5}"
: "\${lunarwing_respawn_period:=60}"
: "\${lunarwing_retry:=SIGTERM/30/KILL/5}"

command="\${lunarwing_openrc_env_exec}"
command_args="--env-file \${lunarwing_env_file} -- \${lunarwing_command} \${lunarwing_args}"
command_user="\${lunarwing_user}:\${lunarwing_group}"
directory="\${lunarwing_workdir}"
pidfile="\${lunarwing_pidfile}"
supervisor="supervise-daemon"
retry="\${lunarwing_retry}"
respawn_delay="\${lunarwing_respawn_delay}"
respawn_max="\${lunarwing_respawn_max}"
respawn_period="\${lunarwing_respawn_period}"
output_log="\${lunarwing_output_log}"
error_log="\${lunarwing_error_log}"
required_files="\${lunarwing_openrc_env_exec} \${lunarwing_command} \${lunarwing_env_file}"

depend() {
    need net localmount lunarwing-pg-${name}
    use dns logger
    after firewall lunarwing-pg-${name} xmpp-bridge-${name}${proxy_rc_after} weechat-${name} lunarwing-weechat-adapter-${name}${darkirc_rc_after}
}

start_pre() {
    checkpath -d -m 0750 -o "\${lunarwing_user}:\${lunarwing_group}" "\${lunarwing_state_dir}"
    checkpath -d -m 0750 -o "\${lunarwing_user}:\${lunarwing_group}" "\${lunarwing_log_dir}"
    checkpath -d -m 0750 -o "\${lunarwing_user}:\${lunarwing_group}" "\${lunarwing_runtime_dir}"
    checkpath -f -m 0640 -o "\${lunarwing_user}:\${lunarwing_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${lunarwing_user}:\${lunarwing_group}" "\${error_log}"
    # Postgres is brought up by the dedicated lunarwing-pg-${name} service, which
    # this unit declares as a hard dependency (need), so the DB is already up.
    umask "\${lunarwing_umask}"
}
INITEOF
  chmod 0755 "/etc/init.d/lunarwing-${name}"

  # ── XMPP bridge init script ──
  cat >"/etc/init.d/xmpp-bridge-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing XMPP bridge ($name)"

: "\${xmpp_bridge_command:=$repo/bridges/xmpp-bridge/target/${PROFILE}/xmpp-bridge}"
: "\${xmpp_bridge_user:=$name}"
: "\${xmpp_bridge_group:=$name}"
: "\${xmpp_bridge_workdir:=$repo/bridges/xmpp-bridge}"
: "\${xmpp_bridge_pidfile:=$run_dir/xmpp-bridge.pid}"
: "\${xmpp_bridge_state_dir:=$state_dir}"
: "\${xmpp_bridge_runtime_dir:=$run_dir}"
: "\${xmpp_bridge_log_dir:=$log_dir}"
: "\${xmpp_bridge_output_log:=\${xmpp_bridge_log_dir}/xmpp-bridge.log}"
: "\${xmpp_bridge_error_log:=\${xmpp_bridge_log_dir}/xmpp-bridge.err}"
: "\${xmpp_bridge_env_file:=$env_dir/xmpp-bridge.env}"
: "\${xmpp_bridge_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${xmpp_bridge_umask:=0077}"
: "\${xmpp_bridge_respawn_delay:=5}"
: "\${xmpp_bridge_respawn_max:=5}"
: "\${xmpp_bridge_respawn_period:=60}"
: "\${xmpp_bridge_retry:=SIGTERM/30/KILL/5}"

command="\${xmpp_bridge_openrc_env_exec}"
command_args="--env-file \${xmpp_bridge_env_file} -- \${xmpp_bridge_command}"
command_user="\${xmpp_bridge_user}:\${xmpp_bridge_group}"
directory="\${xmpp_bridge_workdir}"
pidfile="\${xmpp_bridge_pidfile}"
supervisor="supervise-daemon"
retry="\${xmpp_bridge_retry}"
respawn_delay="\${xmpp_bridge_respawn_delay}"
respawn_max="\${xmpp_bridge_respawn_max}"
respawn_period="\${xmpp_bridge_respawn_period}"
output_log="\${xmpp_bridge_output_log}"
error_log="\${xmpp_bridge_error_log}"
required_files="\${xmpp_bridge_openrc_env_exec} \${xmpp_bridge_command} \${xmpp_bridge_env_file}"

depend() {
    need net localmount
    use dns logger
    after firewall
    before lunarwing-${name}
}

start_pre() {
    checkpath -d -m 0750 -o "\${xmpp_bridge_user}:\${xmpp_bridge_group}" "\${xmpp_bridge_state_dir}"
    checkpath -d -m 0750 -o "\${xmpp_bridge_user}:\${xmpp_bridge_group}" "\${xmpp_bridge_log_dir}"
    checkpath -d -m 0750 -o "\${xmpp_bridge_user}:\${xmpp_bridge_group}" "\${xmpp_bridge_runtime_dir}"
    checkpath -f -m 0640 -o "\${xmpp_bridge_user}:\${xmpp_bridge_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${xmpp_bridge_user}:\${xmpp_bridge_group}" "\${error_log}"
    umask "\${xmpp_bridge_umask}"
}
INITEOF
  chmod 0755 "/etc/init.d/xmpp-bridge-${name}"

  # ── DarkIRC daemon init script (only when enabled) ──
  if tenant_darkirc_enabled "$name"; then
  cat >"/etc/init.d/lunarwing-darkirc-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing DarkIRC daemon ($name)"

: "\${darkirc_command:=$DARKIRC_BIN}"
: "\${darkirc_args:=--config $state_dir/darkirc/darkirc_config.toml}"
: "\${darkirc_user:=$name}"
: "\${darkirc_group:=$name}"
: "\${darkirc_workdir:=$state_dir/darkirc}"
: "\${darkirc_pidfile:=$run_dir/darkirc.pid}"
: "\${darkirc_state_dir:=$state_dir}"
: "\${darkirc_runtime_dir:=$run_dir}"
: "\${darkirc_log_dir:=$log_dir}"
: "\${darkirc_output_log:=\${darkirc_log_dir}/darkirc.log}"
: "\${darkirc_error_log:=\${darkirc_log_dir}/darkirc.err}"
: "\${darkirc_umask:=0077}"
: "\${darkirc_respawn_delay:=5}"
: "\${darkirc_respawn_max:=5}"
: "\${darkirc_respawn_period:=60}"
: "\${darkirc_retry:=SIGTERM/30/KILL/5}"

command="\${darkirc_command}"
command_args="\${darkirc_args}"
command_user="\${darkirc_user}:\${darkirc_group}"
directory="\${darkirc_workdir}"
pidfile="\${darkirc_pidfile}"
supervisor="supervise-daemon"
retry="\${darkirc_retry}"
respawn_delay="\${darkirc_respawn_delay}"
respawn_max="\${darkirc_respawn_max}"
respawn_period="\${darkirc_respawn_period}"
output_log="\${darkirc_output_log}"
error_log="\${darkirc_error_log}"
required_files="\${command}"

depend() {
    need net localmount
    use dns logger
    after firewall
    before lunarwing-darkirc-adapter-${name}
}

start_pre() {
    # DarkIRC state is prepared and ownership-checked by mt-admin/the typed
    # helper.  Do not let OpenRC's root-side checkpath follow a tenant symlink.
    [ -d "\${darkirc_state_dir}/darkirc" ] && [ ! -L "\${darkirc_state_dir}/darkirc" ] || return 1
    checkpath -d -m 0750 -o "\${darkirc_user}:\${darkirc_group}" "\${darkirc_log_dir}"
    checkpath -d -m 0750 -o "\${darkirc_user}:\${darkirc_group}" "\${darkirc_runtime_dir}"
    checkpath -f -m 0640 -o "\${darkirc_user}:\${darkirc_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${darkirc_user}:\${darkirc_group}" "\${error_log}"
    umask "\${darkirc_umask}"
}
INITEOF
  chmod 0755 "/etc/init.d/lunarwing-darkirc-${name}"
  fi

  # ── TensorZero proxy init script (only when enabled) ──
  if tenant_proxy_enabled "$name"; then
  cat >"/etc/init.d/lunarwing-proxy-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing TensorZero proxy ($name)"

: "\${proxy_command:=$(command -v python3)}"
: "\${proxy_args:=$proxy_bin --port $proxy_port --bind 127.0.0.1 --tensorzero $DEFAULT_TENSORZERO_URL}"
: "\${proxy_user:=$name}"
: "\${proxy_group:=$name}"
: "\${proxy_pidfile:=$run_dir/proxy.pid}"
: "\${proxy_runtime_dir:=$run_dir}"
: "\${proxy_log_dir:=$log_dir}"
: "\${proxy_output_log:=\${proxy_log_dir}/proxy.log}"
: "\${proxy_error_log:=\${proxy_log_dir}/proxy.err}"
: "\${proxy_env_file:=$env_dir/proxy.env}"
: "\${proxy_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${proxy_umask:=0077}"
: "\${proxy_respawn_delay:=5}"
: "\${proxy_respawn_max:=5}"
: "\${proxy_respawn_period:=60}"
: "\${proxy_retry:=SIGTERM/30/KILL/5}"

command="\${proxy_openrc_env_exec}"
command_args="--env-file \${proxy_env_file} -- \${proxy_command} \${proxy_args}"
command_user="\${proxy_user}:\${proxy_group}"
pidfile="\${proxy_pidfile}"
supervisor="supervise-daemon"
retry="\${proxy_retry}"
respawn_delay="\${proxy_respawn_delay}"
respawn_max="\${proxy_respawn_max}"
respawn_period="\${proxy_respawn_period}"
output_log="\${proxy_output_log}"
error_log="\${proxy_error_log}"
required_files="\${proxy_openrc_env_exec} \${proxy_command} \${proxy_env_file}"

depend() {
    need net
    use dns
    after firewall
    before lunarwing-${name}
}

start_pre() {
    checkpath -d -m 0750 -o "\${proxy_user}:\${proxy_group}" "\${proxy_runtime_dir}"
    checkpath -d -m 0750 -o "\${proxy_user}:\${proxy_group}" "\${proxy_log_dir}"
    checkpath -f -m 0640 -o "\${proxy_user}:\${proxy_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${proxy_user}:\${proxy_group}" "\${error_log}"
    umask "\${proxy_umask}"
}
INITEOF
  chmod 0755 "/etc/init.d/lunarwing-proxy-${name}"
  fi

  # ── WeeChat init script (tmux-based) ──
  _render_weechat_openrc_unit "$name" "/etc/init.d/lunarwing-weechat-${name}"

  # WeeChat WS adapter init script
  cat >"/etc/init.d/lunarwing-weechat-adapter-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing WeeChat WS adapter ($name)"

: "\${adapter_command:=$(command -v python3)}"
: "\${adapter_args:=$ws_adapter_path}"
: "\${adapter_user:=$name}"
: "\${adapter_group:=$name}"
: "\${adapter_pidfile:=$run_dir/weechat-adapter.pid}"
: "\${adapter_runtime_dir:=$run_dir}"
: "\${adapter_log_dir:=$log_dir}"
: "\${adapter_output_log:=\${adapter_log_dir}/weechat-adapter.log}"
: "\${adapter_error_log:=\${adapter_log_dir}/weechat-adapter.err}"
: "\${adapter_env_file:=$env_dir/lunarwing.env}"
: "\${adapter_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${adapter_umask:=0077}"
: "\${adapter_respawn_delay:=5}"
: "\${adapter_respawn_max:=5}"
: "\${adapter_respawn_period:=60}"
: "\${adapter_retry:=SIGTERM/30/KILL/5}"

command="\${adapter_openrc_env_exec}"
command_args="--env-file \${adapter_env_file} -- \${adapter_command} \${adapter_args}"
command_user="\${adapter_user}:\${adapter_group}"
directory="$ws_adapter_dir"
pidfile="\${adapter_pidfile}"
supervisor="supervise-daemon"
retry="\${adapter_retry}"
respawn_delay="\${adapter_respawn_delay}"
respawn_max="\${adapter_respawn_max}"
respawn_period="\${adapter_respawn_period}"
output_log="\${adapter_output_log}"
error_log="\${adapter_error_log}"
required_files="\${adapter_openrc_env_exec} \${adapter_command} \${adapter_env_file}"

depend() {
    need net lunarwing-weechat-${name}
    use dns
    after firewall lunarwing-weechat-${name}
    before lunarwing-${name}
}

start_pre() {
    checkpath -d -m 0750 -o "\${adapter_user}:\${adapter_group}" "\${adapter_runtime_dir}"
    checkpath -d -m 0750 -o "\${adapter_user}:\${adapter_group}" "\${adapter_log_dir}"
    checkpath -f -m 0640 -o "\${adapter_user}:\${adapter_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${adapter_user}:\${adapter_group}" "\${error_log}"
    umask "\${adapter_umask}"
}
INITEOF
  chmod 0755 "/etc/init.d/lunarwing-weechat-adapter-${name}"

  # DarkIRC adapter init script (only when enabled)
  if tenant_darkirc_enabled "$name"; then
  local darkirc_adapter_path darkirc_adapter_dir
  darkirc_adapter_path="$(tenant_lw_root "$name")/darkirc_channel_for_lunarwing/darkirc/adapter/darkirc_adapter.py"
  darkirc_adapter_dir="$(dirname "$darkirc_adapter_path")"

  cat >"/etc/init.d/lunarwing-darkirc-adapter-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing DarkIRC adapter ($name)"

: "\${darkirc_adapter_command:=$(command -v python3)}"
: "\${darkirc_adapter_args:=$darkirc_adapter_path}"
: "\${darkirc_adapter_user:=$name}"
: "\${darkirc_adapter_group:=$name}"
: "\${darkirc_adapter_pidfile:=$run_dir/darkirc-adapter.pid}"
: "\${darkirc_adapter_runtime_dir:=$run_dir}"
: "\${darkirc_adapter_log_dir:=$log_dir}"
: "\${darkirc_adapter_output_log:=\${darkirc_adapter_log_dir}/darkirc-adapter.log}"
: "\${darkirc_adapter_error_log:=\${darkirc_adapter_log_dir}/darkirc-adapter.err}"
: "\${darkirc_adapter_env_file:=$env_dir/darkirc-adapter.env}"
: "\${darkirc_adapter_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${darkirc_adapter_umask:=0077}"
: "\${darkirc_adapter_respawn_delay:=5}"
: "\${darkirc_adapter_respawn_max:=5}"
: "\${darkirc_adapter_respawn_period:=60}"
: "\${darkirc_adapter_retry:=SIGTERM/30/KILL/5}"

command="\${darkirc_adapter_openrc_env_exec}"
command_args="--env-file \${darkirc_adapter_env_file} -- \${darkirc_adapter_command} \${darkirc_adapter_args}"
command_user="\${darkirc_adapter_user}:\${darkirc_adapter_group}"
directory="$darkirc_adapter_dir"
pidfile="\${darkirc_adapter_pidfile}"
supervisor="supervise-daemon"
retry="\${darkirc_adapter_retry}"
respawn_delay="\${darkirc_adapter_respawn_delay}"
respawn_max="\${darkirc_adapter_respawn_max}"
respawn_period="\${darkirc_adapter_respawn_period}"
output_log="\${darkirc_adapter_output_log}"
error_log="\${darkirc_adapter_error_log}"
required_files="\${darkirc_adapter_openrc_env_exec} \${darkirc_adapter_command} \${darkirc_adapter_env_file}"

depend() {
    need net
    use dns
    after firewall
    before lunarwing-${name}
}

start_pre() {
    checkpath -d -m 0750 -o "\${darkirc_adapter_user}:\${darkirc_adapter_group}" "\${darkirc_adapter_runtime_dir}"
    checkpath -d -m 0750 -o "\${darkirc_adapter_user}:\${darkirc_adapter_group}" "\${darkirc_adapter_log_dir}"
    checkpath -f -m 0640 -o "\${darkirc_adapter_user}:\${darkirc_adapter_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${darkirc_adapter_user}:\${darkirc_adapter_group}" "\${error_log}"
    umask "\${darkirc_adapter_umask}"
}
INITEOF
  chmod 0755 "/etc/init.d/lunarwing-darkirc-adapter-${name}"
  fi

  # ── Conf.d files ──
  local darkirc_rc_need=""
  if tenant_darkirc_enabled "$name"; then
    darkirc_rc_need=" lunarwing-darkirc-adapter-${name}"
  fi
  local proxy_rc_need=""
  if tenant_proxy_enabled "$name"; then
    proxy_rc_need=" lunarwing-proxy-${name}"
  fi
  cat >"/etc/conf.d/lunarwing-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
lunarwing_rc_need="xmpp-bridge-${name}${proxy_rc_need} lunarwing-weechat-${name} lunarwing-weechat-adapter-${name}${darkirc_rc_need}"
CONFD

  cat >"/etc/conf.d/xmpp-bridge-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
xmpp_bridge_rc_before="lunarwing-${name}"
CONFD

  if tenant_proxy_enabled "$name"; then
    cat >"/etc/conf.d/lunarwing-proxy-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD
  fi

  cat >"/etc/conf.d/lunarwing-weechat-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD

  cat >"/etc/conf.d/lunarwing-weechat-adapter-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD

  if tenant_darkirc_enabled "$name"; then
    cat >"/etc/conf.d/lunarwing-darkirc-adapter-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD
  fi

  say "rendered OpenRC init scripts and conf.d for $name"
}

start_tenant_openrc() {
  local name="$1"
  # Record operator intent UP FRONT: enabling the primary daemon in the default
  # runlevel marks this tenant as "started" (boot-persistent) regardless of whether
  # any unit's FIRST start succeeds. health-openrc.sh's started-gate
  # (tenant_started) keys on exactly this, so a daemon that crashes on its first
  # start is still reported `critical` (a real outage) and remediated — not masked
  # as `skipped`. The selective rc-update-add loop below additionally boot-enables
  # the optional units that actually came up. Idempotent.
  rc-update add "lunarwing-${name}" default >/dev/null 2>&1 || true

  # Postgres first: the daemon `need`s it (and it's idempotent if already up).
  rc-service "lunarwing-pg-${name}" start
  # Start PG babysitter (supervises the container via podman wait)
  rc-service "lunarwing-pg-${name}-sup" start 2>/dev/null || true
  # Optional channels next, non-fatal: a missing weechat/aiohttp must not abort
  # the core stack (the main daemon does not depend on them).
  rc-service "lunarwing-weechat-${name}" start 2>/dev/null || say "  (lunarwing-weechat-${name} skipped — optional)"
  rc-service "lunarwing-weechat-adapter-${name}" start 2>/dev/null || say "  (lunarwing-weechat-adapter-${name} skipped — optional)"
  if tenant_darkirc_enabled "$name"; then
    rc-service "lunarwing-darkirc-${name}" start 2>/dev/null || say "  (lunarwing-darkirc-${name} skipped — optional)"
    rc-service "lunarwing-darkirc-adapter-${name}" start 2>/dev/null || say "  (lunarwing-darkirc-adapter-${name} skipped — optional)"
  fi
  if tenant_proxy_enabled "$name"; then
    rc-service "lunarwing-proxy-${name}" start
  fi
  rc-service "xmpp-bridge-${name}" start
  rc-service "lunarwing-${name}" start
  # Start worker babysitters (if workers are configured)
  for worker in nanocode pebble opencode; do
    rc-service "lunarwing-${worker}-${name}-sup" start 2>/dev/null || true
  done
  say "OpenRC services started for $name"

  # Auto-enable on boot whatever is actually running (idempotent, OpenRC only).
  local svc
  local boot_svcs=("lunarwing-pg-${name}" "lunarwing-pg-${name}-sup" "xmpp-bridge-${name}" "lunarwing-${name}" "lunarwing-weechat-${name}" "lunarwing-weechat-adapter-${name}" "lunarwing-vision-${name}")
  if tenant_proxy_enabled "$name"; then
    boot_svcs+=("lunarwing-proxy-${name}")
  fi
  if tenant_darkirc_enabled "$name"; then
    boot_svcs+=("lunarwing-darkirc-${name}" "lunarwing-darkirc-adapter-${name}")
  fi
  for svc in "${boot_svcs[@]}"; do
    if rc-service "$svc" status >/dev/null 2>&1; then
      rc-update add "$svc" default >/dev/null 2>&1 || true
    fi
  done
  # Auto-enable worker babysitters
  for worker in nanocode pebble opencode; do
    if rc-service "lunarwing-${worker}-${name}-sup" status >/dev/null 2>&1; then
      rc-update add "lunarwing-${worker}-${name}-sup" default >/dev/null 2>&1 || true
    fi
  done
  say "enabled boot persistence (default runlevel) for $name's running services"
}

stop_tenant_openrc() {
  local name="$1"
  rc-service "lunarwing-${name}" stop 2>/dev/null || true
  rc-service "xmpp-bridge-${name}" stop 2>/dev/null || true
  rc-service "lunarwing-proxy-${name}" stop 2>/dev/null || true
  rc-service "lunarwing-weechat-adapter-${name}" stop 2>/dev/null || true
  rc-service "lunarwing-weechat-${name}" stop 2>/dev/null || true
  rc-service "lunarwing-darkirc-adapter-${name}" stop 2>/dev/null || true
  rc-service "lunarwing-darkirc-${name}" stop 2>/dev/null || true
  # Worker babysitters: stop the supervisors so they don't respawn the stopped containers.
  for worker in nanocode pebble opencode; do
    rc-service "lunarwing-${worker}-${name}-sup" stop 2>/dev/null || true
  done
  rc-service "lunarwing-vision-${name}" stop 2>/dev/null || true
  # Postgres last: the daemon depends on it, so it stops after its consumers.
  rc-service "lunarwing-pg-${name}-sup" stop 2>/dev/null || true
  rc-service "lunarwing-pg-${name}" stop 2>/dev/null || true
  say "OpenRC services stopped for $name"
}

# Stop only tenant services that can write the migration snapshot. PostgreSQL is
# deliberately excluded so export-tenant.sh can still run pg_dump after the
# writer quiesce. All init-specific calls stay behind mt-admin; migration
# wrappers must never probe or control tenant units directly.
tenant_writer_services() {
  local name="$1"
  printf '%s\n' "lunarwing-${name}" "xmpp-bridge-${name}" \
    "lunarwing-weechat-adapter-${name}" "lunarwing-weechat-${name}"
  tenant_proxy_enabled "$name" \
    && printf '%s\n' "lunarwing-proxy-${name}"
  if tenant_darkirc_enabled "$name"; then
    printf '%s\n' "lunarwing-darkirc-adapter-${name}" "lunarwing-darkirc-${name}"
  fi
  for worker in nanocode pebble opencode; do
    if tenant_worker_enabled "$name" "$worker"; then
      printf '%s\n' "lunarwing-${worker}-${name}" "lunarwing-${worker}-${name}-sup"
    fi
  done
  if [[ "$INIT_SYSTEM" != openrc || -e "/etc/init.d/lunarwing-vision-${name}" ]]; then
    printf '%s\n' "lunarwing-vision-${name}"
  fi
}

stop_tenant_writers() {
  local name="$1" service
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    while IFS= read -r service; do
      _systemctl_user "$name" stop "${service}.service" >/dev/null 2>&1 || true
    done < <(tenant_writer_services "$name")
  else
    while IFS= read -r service; do
      rc-service "$service" stop >/dev/null 2>&1 || true
    done < <(tenant_writer_services "$name")
  fi
  say "tenant writers stopped for $name (PostgreSQL left running)"
}

tenant_writers_active() {
  local name="$1" service state rc
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || return 2
  ensure_init_system || return 2
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    while IFS= read -r service; do
      rc=0
      state="$(_systemctl_user "$name" is-active "${service}.service" 2>/dev/null)" || rc=$?
      case "$state" in
        active|activating|reloading|deactivating) return 0 ;;
        inactive|dead|failed|maintenance) ;;
        unknown) [[ "$rc" -eq 4 ]] || return 2 ;;
        *) return 2 ;;
      esac
    done < <(tenant_writer_services "$name")
  else
    while IFS= read -r service; do
      rc=0
      state="$(rc-service "$service" status 2>/dev/null)" || rc=$?
      case "$state" in
        *stopped*|*inactive*|*dead*) ;;
        *started*|*running*|*active*) return 0 ;;
        *) return 2 ;;
      esac
    done < <(tenant_writer_services "$name")
  fi
  return 1
}

# Strict DarkIRC activation gate used by managed contact operations. The daemon
# and adapter must both be active through the selected init abstraction, then
# the adapter's authenticated health endpoint must report an IRC connection.
# The bearer is supplied to curl over stdin (`--header @-`) so it never appears
# in argv or process listings.
darkirc_health_strict() {
  local name="$1" attempts="${2:-10}" quiet="${3:-false}" service adapter_port env_path secret response
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || return 1
  tenant_darkirc_enabled "$name" || return 1
  ensure_init_system

  if [[ "$INIT_SYSTEM" == systemd ]]; then
    _systemctl_user "$name" is-active --quiet "lunarwing-darkirc-${name}.service" \
      || return 1
    _systemctl_user "$name" is-active --quiet "lunarwing-darkirc-adapter-${name}.service" \
      || return 1
  else
    rc-service "lunarwing-darkirc-${name}" status >/dev/null 2>&1 || return 1
    rc-service "lunarwing-darkirc-adapter-${name}" status >/dev/null 2>&1 || return 1
  fi

  adapter_port="$(ports_get "$name" darkirc_adapter 2>/dev/null || true)"
  [[ "$adapter_port" =~ ^[0-9]+$ ]] || return 1
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  secret="$(read_tenant_env_value \
    "$name" "$env_path" DARKIRC_ADAPTER_SECRET 2>/dev/null)" || return 1
  [[ -n "$secret" ]] || return 1
  require_cmd curl
  require_cmd jq

  for ((attempt = 1; attempt <= attempts; attempt++)); do
    response="$(printf 'Authorization: Bearer %s\n' "$secret" | \
      curl -fsS --max-time 2 --header @- \
        "http://127.0.0.1:${adapter_port}/health" 2>/dev/null || true)"
    if jq -e '(.status == "ok") and (.irc_connected == true)' <<<"$response" \
      >/dev/null 2>&1; then
      [[ "$quiet" == true ]] || say "strict DarkIRC health passed for $name"
      return 0
    fi
    [[ "$attempt" -lt "$attempts" ]] && sleep 1
  done
  say "strict DarkIRC health failed for $name" >&2
  return 1
}

uninstall_tenant_openrc() {
  local name="$1"
  for svc in "lunarwing-${name}" "xmpp-bridge-${name}" "lunarwing-proxy-${name}" "lunarwing-weechat-adapter-${name}" "lunarwing-weechat-${name}" "lunarwing-darkirc-adapter-${name}" "lunarwing-darkirc-${name}" "lunarwing-pg-${name}" "lunarwing-nanocode-${name}" "lunarwing-pebble-${name}" "lunarwing-opencode-${name}" "lunarwing-vision-${name}" "lunarwing-pg-${name}-sup" "lunarwing-nanocode-${name}-sup" "lunarwing-pebble-${name}-sup" "lunarwing-opencode-${name}-sup"; do
    rc-update del "$svc" default 2>/dev/null || true
    rm -f "/etc/init.d/$svc" "/etc/conf.d/$svc"
  done
  say "uninstalled OpenRC services for $name"
}

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
  local svc="/etc/systemd/system/lunarwing-mt-health.service"
  local tmr="/etc/systemd/system/lunarwing-mt-health.timer"
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
HEALTH_XMPP_SERVER=
HEALTH_MODELS_ENABLED=false
HEALTH_OMEMO_ENABLED=false
HEALTHCHECK_NOTIFY=false
SELF_HEAL_MAX_REPORT_AGE=$(( HEALTH_INTERVAL_MIN * 60 * 4 ))

# LunarVision: leave HEALTH_LUNARVISION_URL unset so health-lunarvision.sh
# auto-discovers every tenant's vision_health port from the ports registry
# (single-node still falls back to http://127.0.0.1:8088). Only set the URL
# if you intentionally force a single override target.
# HEALTH_LUNARVISION_URL=

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
        systemctl disable --now lunarwing-mt-health.timer >/dev/null 2>&1 || true
        rm -f /etc/systemd/system/lunarwing-mt-health.timer /etc/systemd/system/lunarwing-mt-health.service
        systemctl daemon-reload >/dev/null 2>&1 || true
      fi
      ;;
    *) return 0 ;;
  esac
  rm -f "$HEALTH_LAUNCHER"
  say "retired host-global health pipeline schedule (no tenants remain)"
  # $HEALTH_LIB_DIR + $HEALTH_ENV_FILE left in place (harmless; preserves config/state).
}

add_tenant() {
  local name="$1"
  local docker_group="${2:-false}"
  local xmpp_jid="${3:-$name@xmpp.localhost}"
  local xmpp_password="${4:-}"
  local tensorzero_url="${5:-$DEFAULT_TENSORZERO_URL}"
  local gotify_url="${6:-$DEFAULT_GOTIFY_URL}"
  local gotify_title="${7:-$DEFAULT_GOTIFY_TITLE}"
  local llm_api_key="${8:-}"
  local llm_base_url="${9:-$DEFAULT_LLM_BASE_URL}"
  local enable_darkirc="${10:-false}"
  local enable_proxy="${11:-false}"
  local nanocode_model="${12:-}"
  local nanocode_base_url="${13:-}"
  local llm_model="${14:-}"
  local gateway_host="${15:-}"
  local xmpp_allow_from="${16:-}"
  local opencode_model="${17:-}"
  local opencode_base_url="${18:-}"
  # External worker selection (default off). Persisted into the port registry so
  # start_tenant_<worker> starts only what this tenant chose, not every worker
  # whose shared host image happens to exist. See PER_TENANT_WORKER_GATING.md.
  local with_nanocode="${19:-false}"
  local with_pebble="${20:-false}"
  local with_opencode="${21:-false}"
  local darkirc_scope_override="${22:-}"

  name="$(sanitize_name "$name")"
  [[ -n "$name" ]] || die "invalid tenant name"
  # A tenant whose name begins with a reserved per-service prefix would make its
  # primary daemon unit (lunarwing-<name>) collide with another tenant's
  # per-service unit — e.g. tenant 'pebble-1' -> lunarwing-pebble-1, byte-identical
  # to tenant '1's pebble worker unit (lunarwing-pebble-1). That collision is
  # undisambiguatable downstream (health-openrc.sh's started-gate would mis-key the
  # unit and could mask a real outage as `skipped`), so forbid such names at the
  # source. (weechat-* also covers weechat-adapter-*.)
  case "$name" in
    pg-*|proxy-*|nanocode-*|pebble-*|opencode-*|weechat-*)
      die "tenant name '$name' collides with a reserved per-service unit prefix (pg-/proxy-/nanocode-/pebble-/opencode-/weechat-/weechat-adapter-); choose another name" ;;
  esac

  say "=== Adding tenant: $name ==="
  say ""

  ports_registry_init
  local base_port workers_json
  workers_json="$(printf '{"nanocode":%s,"pebble":%s,"opencode":%s}' \
    "$with_nanocode" "$with_pebble" "$with_opencode")"
  base_port="$(ports_allocate "$name" "$enable_darkirc" "$enable_proxy" "$workers_json" "$darkirc_scope_override")"
  say ""

  create_tenant_user "$name" "$docker_group"
  say ""

  warn_if_adapter_deps_missing "$name"

  clone_tenant_repo "$name"
  say ""

  say "--- Generating environment files ---"
  write_tenant_vision_env "$name" >/dev/null
  write_tenant_lunarwing_env "$name" "$xmpp_jid" "$xmpp_password" "$tensorzero_url" "$llm_api_key" "$llm_base_url" "$nanocode_model" "$nanocode_base_url" "$llm_model" "$gateway_host" "$xmpp_allow_from" "$opencode_model" "$opencode_base_url"
  write_tenant_bridge_env "$name" "$xmpp_jid" "$xmpp_password" "$xmpp_allow_from"
  if [[ "$enable_proxy" == "true" ]]; then
    write_tenant_proxy_env "$name" "$tensorzero_url"
  fi
  if [[ "$enable_darkirc" == "true" ]]; then
    ensure_darkirc_scope_id "$name" >/dev/null
    darkirc_writer_lock "$name"
    write_tenant_darkirc_adapter_env "$name"
    darkirc_writer_unlock
    if darkirc_key_helper_path "$name" >/dev/null 2>&1; then
      generate_darkirc_config "$name"
    else
      say "DarkIRC config deferred until build-tenant installs the typed key helper"
    fi
  fi
  write_tenant_gotify_config "$name" "$gotify_url" "$gotify_title"
  ensure_external_worker_config "$name" "nanocode" "nanocode_wss"
  ensure_external_worker_config "$name" "pebble" "pebble_wss"
  ensure_external_worker_config "$name" "opencode" "opencode_wss"

  # WeeChat relay auto-bootstrap: write a dedicated minimal weechat.env, then
  # attempt a one-shot relay configuration. Non-fatal: a degraded WeeChat setup
  # must not strand the base tenant. Recovery: configure-weechat-relay <name>.
  local weechat_bootstrap_ok="configured"
  if [[ "$WEECHAT_BOOTSTRAP_OPT_OUT" == "true" ]]; then
    local relay_password
    relay_password="$(_read_env_value "$(tenant_env_dir "$name")/lunarwing.env" RELAY_PASSWORD)"
    if [[ -n "$relay_password" ]] && _write_weechat_env "$name" "$relay_password"; then
      weechat_bootstrap_ok="disabled (--no-weechat-bootstrap)"
    else
      weechat_bootstrap_ok="disabled (credential env setup failed)"
      say "WARNING: WeeChat bootstrap was disabled, but the minimal credential env could not be written." >&2
    fi
  elif ! configure_weechat_relay "$name" 2>&1; then
    weechat_bootstrap_ok="needs recovery"
    say ""
    say "WARNING: WeeChat relay auto-bootstrap failed for tenant '$name'."
    say "         WeeChat will start but the relay is not configured."
    say "         Recovery: sudo $0 configure-weechat-relay $name"
    say ""
  fi

  # SSH harness: config.toml [[ssh.hosts]] block + ed25519 key pair.
  # Enabled by default; opt out with --no-ssh or LUNARWING_MT_SSH_ENABLED=false.
  if [[ "$DEFAULT_SSH_ENABLED" == "true" && "$SSH_OPT_OUT" != "true" ]]; then
    say "--- Provisioning SSH harness ---"
    ensure_ssh_config "$name"
    provision_tenant_ssh_key "$name"
    warn_if_sshd_unreachable "$name"
  else
    say "SSH harness: disabled (enabled=$DEFAULT_SSH_ENABLED, opt-out=$SSH_OPT_OUT)"
  fi

  if ! "$CONTAINER_RT" image inspect "$VISION_SIDECAR_IMAGE" &>/dev/null; then
    build_vision_sidecar_image || true
  fi
  say ""

  say "--- Starting PostgreSQL ---"
  start_tenant_postgres "$name"
  say ""

  ensure_init_system
  say "--- Rendering $INIT_SYSTEM services ---"
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    render_tenant_systemd_units "$name"
  else
    render_tenant_openrc_units "$name"
  fi
  say ""

  # Host-global health-check + self-heal pipeline (auto-covers every tenant).
  if [[ "$DEFAULT_HEALTH_ENABLED" == "true" && "$HEALTH_OPT_OUT" != "true" ]]; then
    ensure_health_pipeline
  else
    say "health pipeline: disabled (enabled=$DEFAULT_HEALTH_ENABLED, opt-out=$HEALTH_OPT_OUT)"
  fi
  say ""

  say "=== Tenant '$name' added ==="
  say ""
  say "Port block: $base_port-$((base_port + PORT_BLOCK_SIZE - 1))"
  say "  gateway:          $(ports_get "$name" gateway)"
  say "  http:             $(ports_get "$name" http)"
  say "  bridge:           $(ports_get "$name" bridge)"
  say "  postgres:         $(ports_get "$name" postgres)"
  say "  proxy:            $(ports_get "$name" proxy)"
  say "  weechat:          $(ports_get "$name" weechat)"
  say "  orchestrator:     $(ports_get "$name" orchestrator)"
  say "  nanocode_wss:     $(ports_get "$name" nanocode_wss)"
  say "  pebble_wss:       $(ports_get "$name" pebble_wss)"
  say "  opencode_wss:     $(ports_get "$name" opencode_wss)"
  say "  weechat_adapter:  $(ports_get "$name" weechat_adapter)"
  say "  darkirc:          $( [[ "$enable_darkirc" == "true" ]] && echo "enabled" || echo "disabled (pass --enable-darkirc to enable)" )"
  say "  proxy:            $( [[ "$enable_proxy" == "true" ]] && echo "enabled" || echo "disabled (pass --enable-proxy to enable)" )"
  say "  workers:          $( _selected="$( [[ "$with_nanocode" == "true" ]] && printf 'nanocode '; [[ "$with_pebble" == "true" ]] && printf 'pebble '; [[ "$with_opencode" == "true" ]] && printf 'opencode ' )"; [[ -n "$_selected" ]] && echo "${_selected% }" || echo "none (pass --with-nanocode/--with-pebble/--with-opencode to select)" )"
  say "  ssh:              $( [[ "$DEFAULT_SSH_ENABLED" == "true" && "$SSH_OPT_OUT" != "true" ]] && echo "enabled (key upload + activation handled by start-tenant)" || echo "disabled (pass --no-ssh)" )"
  say "  weechat relay:    $weechat_bootstrap_ok"
  say ""
  say "Next steps:"
  say "  sudo $0 build-tenant $name --with-wasm --with-nanocode"
  say "  sudo $0 start-tenant $name"
}

add_tenants() {
  local names_csv="$1"
  shift

  local IFS=','
  local names_array
  read -ra names_array <<< "$names_csv"

  for raw_name in "${names_array[@]}"; do
    local name
    name="$(sanitize_name "$(echo "$raw_name" | xargs)")"
    [[ -n "$name" ]] || continue
    say ""
    add_tenant "$name" "$@"
  done
}

remove_tenant() {
  local name="$1"
  local purge="${2:-false}"

  name="$(sanitize_name "$name")"
  say "=== Removing tenant: $name ==="
  say ""

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    stop_tenant_systemd "$name"
    uninstall_tenant_systemd "$name"
  else
    stop_tenant_openrc "$name"
    uninstall_tenant_openrc "$name"
  fi
  say ""

  stop_tenant_postgres "$name"
  if [[ "$purge" == "true" ]]; then
    reset_tenant_postgres "$name"
  fi
  say ""

  ports_deallocate "$name"
  say ""

  remove_tenant_user "$name" "$purge"
  say ""

  # If that was the last tenant, retire the host-global health pipeline schedule.
  if [[ -z "$(all_tenant_names)" ]]; then
    remove_health_pipeline
    say ""
  fi

  say "=== Tenant '$name' removed ==="
}

# Re-render a tenant's service units from the current generator WITHOUT touching
# secrets/env or restarting anything — for applying a generator change (e.g. an
# updated init-script status()) to an already-provisioned tenant. The systemd
# renderer daemon-reloads internally; OpenRC reads the script per invocation. A
# status()/health change takes effect immediately; a change to the run command
# needs a restart.
render_tenant_units() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_init_system
  say "--- Re-rendering $INIT_SYSTEM units for $name ---"
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    render_tenant_systemd_units "$name"
  else
    render_tenant_openrc_units "$name"
  fi
  say "units re-rendered for '$name' (services NOT restarted)."
  say "run-command changes need a restart to apply: $0 restart-tenant $name"
}

# Print a post-start SSH readiness block sourced from the live API. Warn-only:
# a missing/failed API must never fail start-tenant.
_ssh_ready_summary() {
  local name="$1" http_port status keys hosts
  hosts="$(_ssh_hosts_from_config "$name" | paste -sd, -)"
  [[ -n "$hosts" ]] || return 0  # SSH not configured for this tenant

  http_port="$(ports_get "$name" http)"
  status="$(curl -sf --max-time 3 "http://127.0.0.1:${http_port}/agent/status" 2>/dev/null)" || status=""
  keys="$(jq -r '.data.keys_loaded // "?"' <<<"$status" 2>/dev/null)" || keys="?"

  say ""
  say "--- SSH readiness ---"
  say "  hosts:        $hosts"
  if [[ "$keys" =~ ^[0-9]+$ ]] && ((keys >= 1)); then
    say "  agent:        running, $keys key(s) loaded — ssh/ssh_git tools ready"
  else
    say "  agent:        keys_loaded=$keys — if a key upload just failed, re-run '$0 start-tenant $name'"
  fi
  if [[ -f "$(tenant_state_dir "$name")/tools/ssh-tool.wasm" ]]; then
    say "  wasm ssh:     installed (activate it in the web panel: Settings → Extensions → ssh)"
  fi
  say "  verify:       curl -s http://127.0.0.1:${http_port}/agent/status | jq"
}

# Poll the authenticated tenant gateway until reachable (up to ~30s).
_wait_tenant_gateway() {
  local name="$1" env_path gateway_port gateway_token auth_header i=0
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  gateway_port="$(ports_get "$name" gateway)"
  gateway_token="$(grep -s '^GATEWAY_AUTH_TOKEN=' "$env_path" | cut -d= -f2- || true)"
  [[ -n "$gateway_token" ]] || return 1
  auth_header="Authorization: Bearer ${gateway_token}"
  while ! curl -sf --max-time 2 \
    -K <(printf 'header = "%s"\n' "$auth_header") \
    "http://127.0.0.1:${gateway_port}/api/gateway/status" >/dev/null 2>&1; do
    i=$((i + 1))
    [[ $i -lt 15 ]] || return 1
    sleep 2
  done
  return 0
}

# Restart ONLY the lunarwing daemon unit for a tenant (not the full stack).
# Used by start_tenant to make a freshly-uploaded SSH key signable: the agent
# loads keys from the secrets store at startup only ("runtime key add is
# status-only" — see docs/architecture/SSH_AGENT_HARNESS.md §7).
_restart_tenant_daemon() {
  local name="$1"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    _systemctl_user "$name" restart "lunarwing-${name}.service"
  else
    rc-service "lunarwing-${name}" restart
  fi
}

start_tenant() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  say "=== Starting tenant: $name ==="

  # Safety net: if the tenant has LUNARWING_OWNER_ID set but the DB still has
  # orphaned 'default'-scoped rows (e.g. an upgrade that didn't run patch-env),
  # migrate them before starting the daemon so no data is orphaned.
  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  if grep -q '^LUNARWING_OWNER_ID=' "$env_path" 2>/dev/null && _owner_scope_needs_migration "$name"; then
    say "found orphaned 'default'-scoped DB data; auto-migrating to '$name' scope"
    migrate_owner_scope "$name" || say "WARNING: owner-scope migration failed (run 'migrate-owner-scope $name' manually)"
  fi

  # Pre-create the SSH agent socket path so podman can bind-mount it into
  # worker containers. The daemon creates the actual Unix socket here at
  # startup; without a pre-existing path, podman would create it as a
  # directory (breaking the daemon's socket bind). A touch-file is safe —
  # the daemon removes it and binds the real socket.
  local ssh_socket_path
  ssh_socket_path="$(tenant_run_dir "$name")/ssh-agent.sock"
  if [[ ! -S "$ssh_socket_path" ]]; then
    sudo -u "$name" mkdir -p "$(tenant_run_dir "$name")" 2>/dev/null || true
    sudo -u "$name" touch "$ssh_socket_path" 2>/dev/null || true
  fi

  # Start the daemon BEFORE the workers so the SSH agent socket exists when
  # the worker containers are created (podman bind-mounts the file at creation
  # time; if the socket doesn't exist yet, the mount is a stale touch-file).
  # The daemon's SSH agent creates the real Unix socket at
  # <run_dir>/ssh-agent.sock, which the workers bind-mount.
  start_tenant_postgres "$name"
  start_tenant_vision "$name"

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    start_tenant_systemd "$name"
  else
    start_tenant_openrc "$name"
  fi

  # Upload the staged SSH key to the secrets store (if one was provisioned by
  # add-tenant but not yet uploaded), BEFORE the workers start. If a key was
  # actually ingested, bounce the daemon once so the agent loads it (keys are
  # only read from the secrets store at startup); the workers then bind-mount
  # the post-bounce socket inode, so they are never left on a stale socket.
  local staged_key
  staged_key="$(tenant_env_dir "$name")/ssh_key_staged"
  warn_if_sshd_unreachable "$name"
  if [[ -f "$staged_key" ]]; then
    upload_tenant_ssh_key "$name" || true
    if [[ ! -f "$staged_key" ]]; then
      # Upload succeeded (upload_tenant_ssh_key deletes the staged file).
      say "restarting lunarwing-${name} so the SSH agent loads the new key ..."
      if _restart_tenant_daemon "$name"; then
        if _wait_tenant_gateway "$name"; then
          say "lunarwing-${name} restarted; SSH key active"
        else
          say "WARNING: gateway not reachable after SSH-key restart (check 'status $name')" >&2
        fi
      else
        say "WARNING: daemon restart failed after SSH key upload; run '$0 restart-tenant $name' manually" >&2
      fi
    fi
  fi

  # Seed the optional DarkIRC adapter secret into the encrypted secrets store
  # so the WASM channel authenticates outbound adapter requests. Best-effort:
  # a failed upload warns and continues; the WASM channel refreshes the credential
  # live from the store, so no daemon restart is required.
  upload_tenant_darkirc_secret "$name" || true

  # Workers start AFTER the daemon (and after any SSH-key bounce) so the SSH
  # agent socket is already a real, current Unix socket when podman bind-mounts it.
  #
  # Gate each worker on the tenant's persisted selection (add-tenant --with-*).
  # Without this, start_tenant_<worker> would start every worker whose SHARED
  # host image happens to exist — regardless of what this tenant chose — because
  # its only guards are "port allocated" (always true) and "image present"
  # (host-wide). See tenant_worker_enabled / PER_TENANT_WORKER_GATING.md.
  local _w
  for _w in nanocode pebble opencode; do
    if tenant_worker_enabled "$name" "$_w"; then
      "start_tenant_${_w}" "$name"
    else
      say "$_w worker not selected for $name (skipping; enable with 'add-tenant $name --with-$_w')"
    fi
  done
  _ssh_ready_summary "$name" || true
}

stop_tenant() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  say "=== Stopping tenant: $name ==="

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    stop_tenant_systemd "$name"
  else
    stop_tenant_openrc "$name"
  fi

  stop_tenant_vision "$name"
  stop_tenant_pebble "$name"
  stop_tenant_nanocode "$name"
  stop_tenant_opencode "$name"
  stop_tenant_postgres "$name"
}

restart_tenant() {
  local name="$1"
  stop_tenant "$name"
  start_tenant "$name"
}

# In-place tenant upgrade: composes the verified sequence (backup -> stop ->
# fetch/checkout as the tenant user -> rebuild with WASM -> re-render units ->
# patch env -> start). The verbs it calls dispatch OpenRC/systemd themselves,
# so this works on any supported init system. See
# docs/ops/TENANT-RENAME-MIGRATION-1.1.9.md for the 1.1.9 rename specifics.
upgrade_tenant() {
  local name="$1" target="$2" source_repo="$3" do_backup="$4" do_render="$5"
  local lw_root repo
  lw_root="$(tenant_lw_root "$name")"
  repo="$(tenant_repo "$name")"

  [[ -d "$lw_root/.git" ]] || die "no git checkout at $lw_root; run add-tenant first"

  if tenant_darkirc_enabled "$name"; then
    local darkirc_scope
    darkirc_scope="$(darkirc_scope_id "$name")"
    validate_darkirc_scope_id "$darkirc_scope"
    run_darkirc_key_helper "$name" migration-ready --tenant "$name" \
      --scope-id "$darkirc_scope" --json >/dev/null \
      || die "DarkIRC migration state is not settled; refusing upgrade"
  fi

  local tgit=(sudo -u "$name" git -c safe.directory="$lw_root" -C "$lw_root")

  if [[ -n "$source_repo" ]]; then
    [[ -d "$source_repo/.git" || -f "$source_repo/HEAD" ]] || die "--source-repo $source_repo is not a git repository"
    say "pointing origin at $source_repo ..."
    "${tgit[@]}" remote set-url origin "$source_repo" || die "failed to retarget origin"
  fi

  local before
  before="$("${tgit[@]}" describe --tags --always 2>/dev/null || echo unknown)"
  say "upgrading tenant '$name': $before -> $target"

  if [[ "$do_backup" == "true" ]]; then
    backup_tenant_postgres "$name"
  else
    say "skipping pre-upgrade backup (--no-backup)"
  fi

  say "fetching tags + refs from origin (as $name) ..."
  "${tgit[@]}" fetch --tags --prune origin || die "git fetch failed"
  "${tgit[@]}" rev-parse --verify --quiet "${target}^{commit}" >/dev/null 2>&1 \
    || "${tgit[@]}" rev-parse --verify --quiet "origin/${target}^{commit}" >/dev/null 2>&1 \
    || die "target ref '$target' not found after fetch (need a branch, tag, or commit reachable from origin)"

  stop_tenant "$name"

  say "checking out $target (as $name) ..."
  if "${tgit[@]}" rev-parse --verify --quiet "refs/remotes/origin/$target" >/dev/null 2>&1; then
    # Branch on origin: (re)create the local branch on it so repeat upgrades
    # of the same branch move forward instead of reusing a stale local tip.
    "${tgit[@]}" checkout -B "$target" "origin/$target" || die "git checkout $target failed"
  else
    "${tgit[@]}" checkout "$target" || die "git checkout $target failed"
  fi
  [[ -d "$repo/migrations" ]] || die "ic/migrations missing after checkout — wrong ref? Aborting before build."
  say "  now at: $("${tgit[@]}" describe --tags --always 2>/dev/null)"

  build_tenant "$name" "true"
  install_wasm_tenant "$name"

  if [[ "$do_render" == "true" ]]; then
    render_tenant_units "$name"
  else
    say "skipping render-units (--skip-render); unit files keep their embedded paths"
  fi

  patch_tenant_env "$name"
  start_tenant "$name"

  # Pre-1.1.9 units embed renamed adapter paths that only keep working through
  # the 1.1.9-only compat symlinks — warn (don't fail) if any remain.
  local stale
  stale="$(grep -rlE 'ironclaw_weechat_wss|darkirc_channel_for_ironclaw' \
    /etc/init.d /etc/conf.d "$(tenant_home "$name")/.config/systemd/user" 2>/dev/null \
    | grep -F -- "$name" || true)"
  if [[ -n "$stale" ]]; then
    say "WARNING: these units still embed pre-rename paths (run render-units before v2.0.0):"
    say "$stale"
  fi

  say ""
  say "tenant '$name' upgraded: $before -> $("${tgit[@]}" describe --tags --always 2>/dev/null)"
  say "verify with: $0 status $name"
}

status_tenant() {
  local name="$1"
  name="$(sanitize_name "$name")"

  if ! tenant_exists_in_registry "$name"; then
    say "tenant '$name' not found in registry"
    return 1
  fi

  say "=== Tenant: $name ==="
  say ""
  say "Ports:"
  say "  gateway:          $(ports_get "$name" gateway)"
  say "  http:             $(ports_get "$name" http)"
  say "  bridge:           $(ports_get "$name" bridge)"
  say "  postgres:         $(ports_get "$name" postgres)"
  say "  proxy:            $(ports_get "$name" proxy)"
  say "  weechat:          $(ports_get "$name" weechat)"
  say "  orchestrator:     $(ports_get "$name" orchestrator)"
  say "  nanocode_wss:     $(ports_get "$name" nanocode_wss)"
  say "  pebble_wss:       $(ports_get "$name" pebble_wss)"
  say "  opencode_wss:     $(ports_get "$name" opencode_wss)"
  say "  weechat_adapter:  $(ports_get "$name" weechat_adapter)"
  say ""

  ensure_container_runtime
  local container_name="lunarwing-pg-$name"
  if _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
    say "PostgreSQL: running ($container_name)"
  else
    say "PostgreSQL: stopped ($container_name)"
  fi

  local nanocode_container="lunarwing-nanocode-$name"
  if _ctr "$name" inspect -f '{{.State.Running}}' "$nanocode_container" 2>/dev/null | grep -q true; then
    say "Nanocode worker: running ($nanocode_container, WSS port $(ports_get "$name" nanocode_wss))"
  elif _ctr "$name" inspect "$nanocode_container" &>/dev/null; then
    say "Nanocode worker: stopped ($nanocode_container)"
  else
    say "Nanocode worker: not created"
  fi

  local pebble_container="lunarwing-pebble-$name"
  if _ctr "$name" inspect -f '{{.State.Running}}' "$pebble_container" 2>/dev/null | grep -q true; then
    say "Pebble worker: running ($pebble_container, WSS port $(ports_get "$name" pebble_wss))"
  elif _ctr "$name" inspect "$pebble_container" &>/dev/null; then
    say "Pebble worker: stopped ($pebble_container)"
  else
    say "Pebble worker: not created"
  fi

  local opencode_container="lunarwing-opencode-$name"
  if _ctr "$name" inspect -f '{{.State.Running}}' "$opencode_container" 2>/dev/null | grep -q true; then
    say "OpenCode worker: running ($opencode_container, WSS port $(ports_get "$name" opencode_wss))"
  elif _ctr "$name" inspect "$opencode_container" &>/dev/null; then
    say "OpenCode worker: stopped ($opencode_container)"
  else
    say "OpenCode worker: not created"
  fi

  ensure_init_system
  say ""
  say "Services ($INIT_SYSTEM):"
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    local svcs=("lunarwing-${name}" "xmpp-bridge-${name}" "lunarwing-proxy-${name}" \
                "lunarwing-weechat-${name}" "lunarwing-weechat-adapter-${name}")
    if tenant_darkirc_enabled "$name"; then
      svcs+=("lunarwing-darkirc-${name}" "lunarwing-darkirc-adapter-${name}")
    fi
    # pg + workers are Quadlet units only on rootless podman; on rootful docker
    # they run as plain containers (shown above), not systemd units.
    if [[ "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
      svcs+=("lunarwing-pg-${name}" "lunarwing-nanocode-${name}" "lunarwing-pebble-${name}" "lunarwing-opencode-${name}")
    fi
    local svc state
    for svc in "${svcs[@]}"; do
      state="$(_systemctl_user "$name" is-active "${svc}.service" 2>/dev/null || echo "inactive")"
      say "  ${svc}.service: $state"
    done
  else
    local rc_svcs=("lunarwing-pg-${name}" "lunarwing-pg-${name}-sup" "lunarwing-${name}" "xmpp-bridge-${name}" "lunarwing-proxy-${name}" \
                   "lunarwing-weechat-${name}" "lunarwing-weechat-adapter-${name}")
    if tenant_darkirc_enabled "$name"; then
      rc_svcs+=("lunarwing-darkirc-${name}" "lunarwing-darkirc-adapter-${name}")
    fi
    rc_svcs+=("lunarwing-nanocode-${name}" "lunarwing-nanocode-${name}-sup" \
              "lunarwing-pebble-${name}" "lunarwing-pebble-${name}-sup" \
              "lunarwing-opencode-${name}" "lunarwing-opencode-${name}-sup")
    local svc state
    for svc in "${rc_svcs[@]}"; do
      state="$(rc-service "$svc" status 2>/dev/null | grep -oE 'started|stopped|crashed' || echo "unknown")"
      say "  $svc: $state"
    done
  fi
}

list_tenants() {
  ports_registry_init
  say "=== Registered tenants ==="
  say ""

  local names
  names="$(all_tenant_names)"
  if [[ -z "$names" ]]; then
    say "no tenants registered"
    return 0
  fi

  printf '%-15s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s\n' \
    "TENANT" "GATEWAY" "HTTP" "BRIDGE" "PG" "PROXY" "WEECHAT" "WS_ADPT" "ORCH" "NANOCODE" "PEBBLE" "OPENCODE"
  printf '%-15s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s\n' \
    "------" "-------" "----" "------" "--" "-----" "-------" "-------" "----" "--------" "------" "--------"

  while IFS= read -r name; do
    printf '%-15s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s\n' \
      "$name" \
      "$(ports_get "$name" gateway)" \
      "$(ports_get "$name" http)" \
      "$(ports_get "$name" bridge)" \
      "$(ports_get "$name" postgres)" \
      "$(ports_get "$name" proxy)" \
      "$(ports_get "$name" weechat)" \
      "$(ports_get "$name" weechat_adapter)" \
      "$(ports_get "$name" orchestrator)" \
      "$(ports_get "$name" nanocode_wss)" \
      "$(ports_get "$name" pebble_wss)" \
      "$(ports_get "$name" opencode_wss)"
  done <<< "$names"
}

show_tokens() {
  local filter="${1:-}"

  say "=== Gateway auth tokens ==="
  say ""

  local names
  if [[ -n "$filter" ]]; then
    names="$(sanitize_name "$filter")"
  else
    names="$(all_tenant_names)"
  fi

  if [[ -z "$names" ]]; then
    say "no tenants found"
    return 0
  fi

  while IFS= read -r name; do
    local env_path gateway_port token
    env_path="$(tenant_env_dir "$name")/lunarwing.env"
    gateway_port="$(ports_get "$name" gateway)"
    token="$(grep -s '^GATEWAY_AUTH_TOKEN=' "$env_path" | cut -d= -f2- || true)"
    say "$name (port $gateway_port): ${token:-<not set>}"
  done <<< "$names"
}

doctor() {
  say "=== Multi-tenant doctor ==="
  say ""

  local pass=0 fail=0
  _check() {
    local label="$1"; shift
    if "$@" >/dev/null 2>&1; then
      printf '[PASS] %s\n' "$label"
      pass=$((pass + 1))
    else
      printf '[FAIL] %s\n' "$label"
      fail=$((fail + 1))
    fi
  }

  _check "running as root" test "${EUID}" -eq 0
  _check "jq installed" command -v jq
  _check "git installed" command -v git
  _check "python3 installed" command -v python3
  _check "cargo installed" command -v cargo
  _check "rustup installed" command -v rustup

  if command -v docker >/dev/null 2>&1; then
    _check "docker available" docker info
  fi
  if command -v podman >/dev/null 2>&1; then
    _check "podman available" podman info
  fi

  # Informational: which runtime commands will use, and why. Single call,
  # protected by the if: detect_container_runtime dies (exits) on a box with
  # neither runtime, and only a $( ) subshell can contain that exit. The
  # source label is re-derived here with the same precedence the function
  # uses (env > saved file > auto-detect).
  local _rt_resolved _rt_source
  if _rt_resolved="$(detect_container_runtime 2>/dev/null)"; then
    if [[ -n "${LUNARWING_CONTAINER_RUNTIME:-}" ]]; then
      _rt_source="env"
    elif [[ "$(_load_saved_container_runtime 2>/dev/null)" == "$_rt_resolved" ]]; then
      _rt_source="saved — $RUNTIME_STATE_FILE"
    else
      _rt_source="auto-detected"
    fi
    printf '[info] container runtime: %s (%s)\n' "$_rt_resolved" "$_rt_source"
  else
    printf '[info] container runtime: unresolved (install docker or podman, or set LUNARWING_CONTAINER_RUNTIME)\n'
  fi

  _check "sshd listening on 127.0.0.1:22 (needed for loopback SSH tenants)" \
    bash -c 'timeout 2 bash -c "exec 3<>/dev/tcp/127.0.0.1/22"'

  ensure_container_runtime
  if [[ "$MT_ROOTLESS" == "true" ]]; then
    _check "rootless: newuidmap setuid" bash -c '[ -u "$(command -v newuidmap 2>/dev/null)" ]'
    _check "rootless: newgidmap setuid" bash -c '[ -u "$(command -v newgidmap 2>/dev/null)" ]'
    _check "rootless: /etc/subuid populated" test -s /etc/subuid
    _check "rootless: /etc/subgid populated" test -s /etc/subgid
    # Every per-tenant container publishes 127.0.0.1:<port>:…, which under rootless
    # needs a userspace port-forwarder (pasta or slirp4netns).
    _check "rootless: pasta or slirp4netns (port-forward)" \
      bash -c 'command -v pasta >/dev/null 2>&1 || command -v slirp4netns >/dev/null 2>&1'
  fi

  ensure_init_system
  _check "init system detected ($INIT_SYSTEM)" true

  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    _check "loginctl available" command -v loginctl
    if [[ "$MT_ROOTLESS" == "true" ]]; then
      _check "podman >= 4.6 (Quadlet supervision)" podman_supports_quadlet
    fi
    # Per-tenant linger keeps /run/user/<uid> + the systemd --user manager alive
    # across reboot — boot-persistent Quadlet/user units depend on it. (Highest-
    # value rootless-on-systemd check.)
    local _dt _du _duid
    while IFS=$'\t' read -r _dt _du; do
      [[ -n "$_du" ]] || continue
      _duid="$(id -u "$_du" 2>/dev/null || echo "")"
      [[ -n "$_duid" ]] || continue
      _check "tenant $_dt: linger enabled" \
        bash -c "loginctl show-user '$_du' -p Linger --value 2>/dev/null | grep -qx yes"
      _check "tenant $_dt: /run/user/$_duid present" test -d "/run/user/$_duid"
    done < <(jq -r '.tenants // {} | to_entries[] | "\(.key)\t\(.value.user)"' "$PORTS_REGISTRY" 2>/dev/null || true)
  else
    _check "rc-service available" command -v rc-service
    _check "rc-update available" command -v rc-update
  fi

  _check "port registry exists" test -f "$PORTS_REGISTRY"
  _check "source repo exists" test -d "$SOURCE_REPO/ic"
  _check "proxy script exists" test -f "$SOURCE_REPO/tensorzero-proxy-configurations/lunarwing-proxy.py"
  _check "health-check orchestrator present" test -x "$HEALTH_SRC_DIR/infrastructure-health-check.sh"
  _check "self-heal script present" test -x "$HEALTH_SRC_DIR/lunarwing-self-heal.sh"
  _check "curl installed (self-heal/gotify)" command -v curl
  _check "flock installed (self-heal lock)" command -v flock
  if [[ "$DEFAULT_HEALTH_ENABLED" == "true" ]]; then
    _check "health pipeline scheduled" bash -c 'systemctl is-enabled lunarwing-mt-health.timer >/dev/null 2>&1 || crontab -l 2>/dev/null | grep -q lunarwing-mt-health || { command -v fcrontab >/dev/null 2>&1 && fcrontab -l 2>/dev/null | grep -q lunarwing-mt-health; }'
  fi
  _check "nanocode worker dir exists" test -d "$LUNARWING_ROOT/lunarcode4lunarwing"
  _check "nanocode worker Dockerfile exists" test -f "$LUNARWING_ROOT/lunarcode4lunarwing/Dockerfile"
  _check "pebble worker dir exists" test -d "$LUNARWING_ROOT/pebble4lunarwing"
  _check "pebble worker Dockerfile exists" test -f "$LUNARWING_ROOT/pebble4lunarwing/Dockerfile"
  _check "opencode worker dir exists" test -d "$LUNARWING_ROOT/opencode4lunarwing"
  _check "opencode worker Dockerfile exists" test -f "$LUNARWING_ROOT/opencode4lunarwing/Dockerfile"

  # Check if worker images are built
  if command -v docker >/dev/null 2>&1; then
    _check "nanocode worker image exists" docker image inspect lunarwing-worker-nanocode:latest
    _check "pebble worker image exists" docker image inspect lunarwing-worker-pebble:latest
    _check "opencode worker image exists" docker image inspect lunarwing-worker-opencode:latest
    _check "vision sidecar image exists" docker image inspect "$VISION_SIDECAR_IMAGE"
  elif command -v podman >/dev/null 2>&1; then
    _check "nanocode worker image exists" podman image inspect lunarwing-worker-nanocode:latest
    _check "pebble worker image exists" podman image inspect lunarwing-worker-pebble:latest
    _check "opencode worker image exists" podman image inspect lunarwing-worker-opencode:latest
    _check "vision sidecar image exists" podman image inspect "$VISION_SIDECAR_IMAGE"
  fi

  say ""
  say "passed: $pass, failed: $fail"
  [[ $fail -eq 0 ]]
}

# ── Main dispatcher ──────────────────────────────────────────────────────────

main() {
  local command_name="${1:-}"
  if [[ -z "$command_name" ]]; then
    usage
    exit 1
  fi
  shift || true

  # An explicitly-chosen runtime persists no matter which command runs.
  # Without this, commands that never touch containers (list-tenants, tokens,
  # ...) silently ignore LUNARWING_CONTAINER_RUNTIME and nothing is saved,
  # breaking the "set it once" promise. ensure_container_runtime validates,
  # persists idempotently, and memoizes; unprivileged runs warn-and-continue.
  [[ -z "${LUNARWING_CONTAINER_RUNTIME:-}" ]] || ensure_container_runtime

  case "$command_name" in
    add-tenant)
      require_root
      local name="" docker_group="false" xmpp_jid="" xmpp_password="" tz_url="$DEFAULT_TENSORZERO_URL" gotify_url="$DEFAULT_GOTIFY_URL" gotify_title="$DEFAULT_GOTIFY_TITLE" llm_api_key="" llm_base_url="$DEFAULT_LLM_BASE_URL" enable_darkirc="false" enable_proxy="false" nanocode_model="" nanocode_base_url="" llm_model="" gateway_host="" xmpp_allow_from="" opencode_model="" opencode_base_url="" with_nanocode="false" with_pebble="false" with_opencode="false" darkirc_scope_override=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --docker-group)    docker_group="true"; shift ;;
          --xmpp-jid)        xmpp_jid="$2"; shift 2 ;;
          --no-health)       HEALTH_OPT_OUT=true; shift ;;
          --no-ssh)          SSH_OPT_OUT=true; shift ;;
          --no-weechat-bootstrap) WEECHAT_BOOTSTRAP_OPT_OUT=true; shift ;;
          --enable-darkirc)  enable_darkirc="true"; shift ;;
          --darkirc-scope-id) darkirc_scope_override="$2"; shift 2 ;;
          --enable-proxy)    enable_proxy="true"; shift ;;
          --with-nanocode)   with_nanocode="true"; shift ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --xmpp-password)   xmpp_password="$2"; shift 2 ;;
          --llm-api-key)     llm_api_key="$2"; shift 2 ;;
          --llm-base-url)    llm_base_url="$2"; shift 2 ;;
          --tensorzero-url)  tz_url="$2"; shift 2 ;;
          --gotify-url)      gotify_url="$2"; shift 2 ;;
          --gotify-title)    gotify_title="$2"; shift 2 ;;
          --nanocode-model)    nanocode_model="$2"; shift 2 ;;
          --nanocode-base-url) nanocode_base_url="$2"; shift 2 ;;
          --opencode-model)    opencode_model="$2"; shift 2 ;;
          --opencode-base-url) opencode_base_url="$2"; shift 2 ;;
          --llm-model)         llm_model="$2"; shift 2 ;;
          --gateway-host)      gateway_host="$2"; shift 2 ;;
          --xmpp-allow-from)   xmpp_allow_from="$2"; shift 2 ;;
          -*)                die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: add-tenant <name> [--docker-group] [--xmpp-jid <jid>]"
      [[ -n "$xmpp_jid" ]] || xmpp_jid="$(sanitize_name "$name")@xmpp.localhost"
      add_tenant "$name" "$docker_group" "$xmpp_jid" "$xmpp_password" "$tz_url" "$gotify_url" "$gotify_title" "$llm_api_key" "$llm_base_url" "$enable_darkirc" "$enable_proxy" "$nanocode_model" "$nanocode_base_url" "$llm_model" "$gateway_host" "$xmpp_allow_from" "$opencode_model" "$opencode_base_url" "$with_nanocode" "$with_pebble" "$with_opencode" "$darkirc_scope_override"
      ;;

    add-tenants)
      require_root
      local names_csv="" docker_group="false" xmpp_domain="xmpp.localhost" tz_url="$DEFAULT_TENSORZERO_URL" gotify_url="$DEFAULT_GOTIFY_URL" gotify_title="$DEFAULT_GOTIFY_TITLE" llm_api_key="" llm_base_url="$DEFAULT_LLM_BASE_URL" enable_darkirc="false" enable_proxy="false" nanocode_model="" nanocode_base_url="" llm_model="" gateway_host="" xmpp_allow_from="" opencode_model="" opencode_base_url="" with_nanocode="false" with_pebble="false" with_opencode="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --docker-group)    docker_group="true"; shift ;;
          --xmpp-domain)     xmpp_domain="$2"; shift 2 ;;
          --no-health)       HEALTH_OPT_OUT=true; shift ;;
          --no-ssh)          SSH_OPT_OUT=true; shift ;;
          --no-weechat-bootstrap) WEECHAT_BOOTSTRAP_OPT_OUT=true; shift ;;
          --enable-darkirc)  enable_darkirc="true"; shift ;;
          --enable-proxy)    enable_proxy="true"; shift ;;
          --with-nanocode)   with_nanocode="true"; shift ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --llm-api-key)     llm_api_key="$2"; shift 2 ;;
          --llm-base-url)    llm_base_url="$2"; shift 2 ;;
          --tensorzero-url)  tz_url="$2"; shift 2 ;;
          --gotify-url)      gotify_url="$2"; shift 2 ;;
          --gotify-title)    gotify_title="$2"; shift 2 ;;
          --nanocode-model)    nanocode_model="$2"; shift 2 ;;
          --nanocode-base-url) nanocode_base_url="$2"; shift 2 ;;
          --opencode-model)    opencode_model="$2"; shift 2 ;;
          --opencode-base-url) opencode_base_url="$2"; shift 2 ;;
          --llm-model)         llm_model="$2"; shift 2 ;;
          --gateway-host)      gateway_host="$2"; shift 2 ;;
          --xmpp-allow-from)   xmpp_allow_from="$2"; shift 2 ;;
          -*)                die "unknown flag: $1" ;;
          *)
            if [[ -z "$names_csv" ]]; then names_csv="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$names_csv" ]] || die "usage: add-tenants <name1,name2,...> [--docker-group]"

      local IFS=','
      local names_array
      read -ra names_array <<< "$names_csv"
      for raw_name in "${names_array[@]}"; do
        local sname
        sname="$(sanitize_name "$(echo "$raw_name" | xargs)")"
        [[ -n "$sname" ]] || continue
        say ""
        add_tenant "$sname" "$docker_group" "${sname}@${xmpp_domain}" "" "$tz_url" "$gotify_url" "$gotify_title" "$llm_api_key" "$llm_base_url" "$enable_darkirc" "$enable_proxy" "$nanocode_model" "$nanocode_base_url" "$llm_model" "$gateway_host" "$xmpp_allow_from" "$opencode_model" "$opencode_base_url" "$with_nanocode" "$with_pebble" "$with_opencode"
      done
      ;;

    remove-tenant)
      require_root
      local name="" purge="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --purge) purge="true"; shift ;;
          -*)      die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: remove-tenant <name> [--purge]"
      remove_tenant "$name" "$purge"
      ;;

    build-tenant)
      require_root
      local name="" with_wasm="false" with_nanocode="false" with_pebble="false" with_opencode="false" with_toolchains="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --with-wasm)       with_wasm="true"; shift ;;
          --with-nanocode)   with_nanocode="true"; shift ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --with-toolchains) with_toolchains="true"; shift ;;
          -*)                die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: build-tenant <name> [--with-wasm] [--with-nanocode] [--with-pebble] [--with-opencode] [--with-toolchains]"
      build_tenant "$(sanitize_name "$name")" "$with_wasm" "$with_nanocode" "$with_pebble" "$with_opencode" "$with_toolchains"
      ;;

    build-all)
      require_root
      local with_wasm="false" with_nanocode="false" with_pebble="false" with_opencode="false" with_toolchains="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --with-wasm)       with_wasm="true"; shift ;;
          --with-nanocode)   with_nanocode="true"; shift ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --with-toolchains) with_toolchains="true"; shift ;;
          -*)                die "unknown flag: $1" ;;
          *)                 die "unexpected argument: $1" ;;
        esac
      done
      build_all "$with_wasm" "$with_nanocode" "$with_pebble" "$with_opencode" "$with_toolchains"
      ;;

    build-darkirc)
      require_root
      local darkirc_tenant=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --tenant) darkirc_tenant="$2"; shift 2 ;;
          -*)       die "unknown flag: $1" ;;
          *)        die "unexpected argument: $1" ;;
        esac
      done
      if [[ -n "$darkirc_tenant" ]]; then
        say "note: --tenant identifies the requesting tenant only; the shared DarkIRC binary uses the fixed root-controlled build boundary"
      fi
      build_darkirc
      ;;

    build-nanocode-worker)
      require_root
      local no_cache="false"
      local with_toolchains="false"
      local nanocode_ref="v1.2.28"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --no-cache)        no_cache="true"; shift ;;
          --with-toolchains) with_toolchains="true"; shift ;;
          --nanocode-ref)    nanocode_ref="$2"; shift 2 ;;
          -*)                die "unknown flag: $1" ;;
          *)                 die "unexpected argument: $1" ;;
        esac
      done
      build_nanocode_worker "$no_cache" "$with_toolchains" "$nanocode_ref"
      ;;

    build-pebble-worker)
      require_root
      local no_cache="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --no-cache) no_cache="true"; shift ;;
          -*)         die "unknown flag: $1" ;;
          *)          die "unexpected argument: $1" ;;
        esac
      done
      build_pebble_worker "$no_cache"
      ;;

    build-opencode-worker)
      require_root
      local no_cache="false"
      local with_toolchains="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --no-cache)         no_cache="true"; shift ;;
          --with-toolchains)  with_toolchains="true"; shift ;;
          -*)                 die "unknown flag: $1" ;;
          *)                  die "unexpected argument: $1" ;;
        esac
      done
      build_opencode_worker "$no_cache" "$with_toolchains"
      ;;

    build-vision-sidecar)
      require_root
      build_vision_sidecar_image
      ;;

    install-wasm)
      require_root
      [[ -n "${1:-}" ]] || die "usage: install-wasm <name>"
      install_wasm_tenant "$1"
      ;;

    install-wasm-all)
      require_root
      install_wasm_all
      ;;

    start-tenant)
      require_root
      [[ -n "${1:-}" ]] || die "usage: start-tenant <name>"
      start_tenant "$1"
      ;;

    stop-tenant)
      require_root
      [[ -n "${1:-}" ]] || die "usage: stop-tenant <name>"
      stop_tenant "$1"
      ;;

    stop-writers)
      require_root
      [[ -n "${1:-}" ]] || die "usage: stop-writers <name>"
      ports_registry_init
      stop_tenant_writers "$1"
      ;;

    writers-active)
      require_root
      [[ -n "${1:-}" ]] || die "usage: writers-active <name>"
      ports_registry_init
      local writers_rc=0
      tenant_writers_active "$1" || writers_rc=$?
      if [[ "$writers_rc" -eq 0 ]]; then
        say "active"
        exit 0
      fi
      if [[ "$writers_rc" -eq 1 ]]; then
        say "stopped"
        exit 1
      fi
      die "could not determine tenant writer state"
      ;;

    restart-tenant)
      require_root
      [[ -n "${1:-}" ]] || die "usage: restart-tenant <name>"
      restart_tenant "$1"
      ;;

    render-units)
      require_root
      [[ -n "${1:-}" ]] || die "usage: render-units <name>"
      ports_registry_init
      render_tenant_units "$1"
      ;;

    upgrade-tenant)
      require_root
      local name="" target="" source_repo="" do_backup="true" do_render="true"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --target)      target="$2"; shift 2 ;;
          --source-repo) source_repo="$2"; shift 2 ;;
          --no-backup)   do_backup="false"; shift ;;
          --skip-render) do_render="false"; shift ;;
          -*)            die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: upgrade-tenant <name> --target <ref> [--source-repo <path>] [--no-backup] [--skip-render]"
      [[ -n "$target" ]] || die "upgrade-tenant requires an explicit --target <ref> (no implicit default)"
      name="$(sanitize_name "$name")"
      ports_registry_init
      tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
      upgrade_tenant "$name" "$target" "$source_repo" "$do_backup" "$do_render"
      ;;

    rotate-pg-password)
      require_root
      [[ -n "${1:-}" ]] || die "usage: rotate-pg-password <name>"
      rotate_tenant_pg_password "$1"
      ;;

    list-tenants|list)
      ports_registry_init
      list_tenants
      ;;

    status)
      [[ -n "${1:-}" ]] || die "usage: status <name>"
      status_tenant "$1"
      ;;

    tokens)
      show_tokens "${1:-}"
      ;;

    configure-gotify)
      require_root
      local name="${1:-}" gotify_url="${2:-}" gotify_title="${3:-}"
      [[ -n "$name" && -n "$gotify_url" ]] || die "usage: configure-gotify <name> <url> [title]"
      name="$(sanitize_name "$name")"
      tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
      write_tenant_gotify_config "$name" "$gotify_url" "$gotify_title"
      configure_gotify_capabilities "$name" "$gotify_url"
      say "Gotify configured for tenant '$name': $gotify_url"
      ;;

    configure-pebble)
      require_root
      local name="" nanogpt_key="" pebble_model=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --nanogpt-api-key) nanogpt_key="$2"; shift 2 ;;
          --model)           pebble_model="$2"; shift 2 ;;
          -*)                die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: configure-pebble <name> --nanogpt-api-key <key> [--model <model>]"
      [[ -n "$nanogpt_key" ]] || die "configure-pebble requires --nanogpt-api-key"
      ports_registry_init
      configure_pebble "$(sanitize_name "$name")" "$nanogpt_key" "$pebble_model"
      ;;

    configure-nanocode)
      require_root
      local name="" nc_model="" nc_base_url=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --model)    nc_model="$2"; shift 2 ;;
          --base-url) nc_base_url="$2"; shift 2 ;;
          -*)         die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: configure-nanocode <name> [--model <model>] [--base-url <url>]"
      configure_nanocode "$name" "$nc_model" "$nc_base_url"
      ;;

    configure-opencode)
      require_root
      local name="" oc_model="" oc_base_url=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --model)    oc_model="$2"; shift 2 ;;
          --base-url) oc_base_url="$2"; shift 2 ;;
          -*)         die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: configure-opencode <name> [--model <model>] [--base-url <url>]"
      configure_opencode "$name" "$oc_model" "$oc_base_url"
      ;;

    configure-ssh)
      require_root
      local name="" ssh_host="" ssh_user=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --host) ssh_host="$2"; shift 2 ;;
          --user) ssh_user="$2"; shift 2 ;;
          -*)     die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: configure-ssh <name> [--host <host>] [--user <user>]"
      name="$(sanitize_name "$name")"
      ports_registry_init
      tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
      [[ -n "$ssh_host" ]] || ssh_host="127.0.0.1"
      [[ -n "$ssh_user" ]] || ssh_user="$name"
      ensure_ssh_config "$name" "$ssh_host" "$ssh_user"
      provision_tenant_ssh_key "$name" "$ssh_host" "$ssh_user"
      patch_ssh_tool_allowlist "$name"
      say ""
      say "SSH harness configured for tenant '$name' (host=$ssh_host user=$ssh_user)"
      say "Run '$0 restart-tenant $name' to start the daemon and upload the key to the secrets store"
      ;;

    darkirc-contact)
      require_root
      local contact_action="" contact_tenant="" contact_name="" adopt_yes=false doctor_all=false migration_in=false migration_json=false
      contact_action="${1:-}"; shift || true
      case "$contact_action" in
        list|doctor)
          if [[ "$contact_action" == doctor && "${1:-}" == "--all" ]]; then
            doctor_all=true; shift
          else
            contact_tenant="${1:-}"; shift || true
          fi
          ;;
        status)
          contact_tenant="${1:-}"; contact_name="${2:-}"; shift 2 || true
          ;;
        adopt)
          contact_tenant="${1:-}"; shift || true
          [[ "${1:-}" == "--yes" ]] && adopt_yes=true && shift
          ;;
        export-migration|import-migration|migration-ready|recover)
          contact_tenant="${1:-}"; shift || true
          ;;
        stage-migration)
          contact_tenant="${1:-}"; shift || true
          [[ "${1:-}" == "--in" ]] || die "stage-migration requires --in -"
          [[ "${2:-}" == "-" ]] || die "stage-migration accepts stdin only (--in -)"
          migration_in=true; shift 2
          ;;
        validate-migration)
          [[ "${1:-}" == "--in" ]] || die "validate-migration requires --in -"
          [[ "${2:-}" == "-" ]] || die "validate-migration accepts stdin only (--in -)"
          migration_in=true; shift 2
          [[ "${1:-}" == "--json" ]] && migration_json=true && shift
          local migration_unbound=false
          [[ "${1:-}" == "--unbound" ]] && migration_unbound=true && shift
          [[ $# -eq 0 ]] || die "unexpected validate-migration argument: $1"
          run_darkirc_manifest_validator "$migration_unbound"
          exit 0
          ;;
        prepare|respond|complete)
          contact_tenant="${1:-}"; contact_name="${2:-}"; shift 2 || true
          ;;
        cancel)
          contact_tenant="${1:-}"; shift || true
          ;;
        exchanges)
          contact_tenant="${1:-}"; shift || true
          ;;
        *) die "usage: darkirc-contact {list|status|doctor|adopt|export-migration|stage-migration|import-migration|migration-ready|recover|validate-migration|prepare|respond|complete|cancel|exchanges} ..." ;;
      esac
      if [[ "$contact_action" == list || "$contact_action" == status || "$contact_action" == doctor || "$contact_action" == exchanges ]]; then
        ports_registry_require_readonly
      else
        ports_registry_init
      fi
      if $doctor_all; then
        darkirc_contact_doctor_all
      else
        [[ -n "$contact_tenant" ]] || die "missing DarkIRC tenant"
        contact_tenant="$(sanitize_name "$contact_tenant")"
        if [[ "$contact_action" == adopt ]]; then
          $adopt_yes || die "adoption requires --yes (this does not rotate any key)"
          DARKIRC_ADOPT_CONFIRMED=true darkirc_contact_helper adopt "$contact_tenant"
        elif [[ "$contact_action" == status ]]; then
          darkirc_contact_helper status "$contact_tenant" "$contact_name"
        elif [[ "$contact_action" == prepare || "$contact_action" == respond || "$contact_action" == complete ]]; then
          darkirc_contact_helper "$contact_action" "$contact_tenant" "$contact_name" "$@"
        elif [[ "$contact_action" == cancel ]]; then
          darkirc_contact_helper cancel "$contact_tenant" "$@"
        elif [[ "$contact_action" == exchanges ]]; then
          darkirc_contact_helper exchanges "$contact_tenant"
        else
          darkirc_contact_helper "$contact_action" "$contact_tenant"
        fi
      fi
      ;;

    darkirc-health)
      require_root
      local health_tenant="" health_strict=false health_json=false
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --strict) health_strict=true; shift ;;
          --json) health_json=true; shift ;;
          -*) die "unknown flag: $1" ;;
          *) [[ -z "$health_tenant" ]] || die "unexpected argument: $1"; health_tenant="$1"; shift ;;
        esac
      done
      $health_strict || die "darkirc-health requires --strict"
      ports_registry_init
      tenant_exists_in_registry "$health_tenant" || die "tenant '$health_tenant' not found in registry"
      if darkirc_health_strict "$health_tenant" 10 "$health_json"; then
        if $health_json; then
          printf '{"status":"ok","tenant":"%s","irc_connected":true}\n' "$health_tenant"
        fi
      else
        die "strict DarkIRC health failed for tenant '$health_tenant'"
      fi
      ;;

    patch-env)
      require_root
      local name="${1:-}"
      [[ -n "$name" ]] || die "usage: patch-env <name>"
      ports_registry_init
      patch_tenant_env "$name"
      ;;

    patch-env-all)
      require_root
      ports_registry_init
      local names
      names="$(all_tenant_names)"
      [[ -n "$names" ]] || { say "no tenants registered"; exit 0; }
      while IFS= read -r name; do
        patch_tenant_env "$name"
      done <<< "$names"
      ;;

    migrate-owner-scope)
      require_root
      local name="" old_scope="default"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --from) old_scope="$2"; shift 2 ;;
          -*)     die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: migrate-owner-scope <name> [--from <old_scope>]"
      ports_registry_init
      migrate_owner_scope "$name" "$old_scope"
      ;;

    owner-scopes)
      require_root
      local name="${1:-}"
      [[ -n "$name" ]] || die "usage: owner-scopes <name>"
      ports_registry_init
      owner_scope_rows "$name"
      ;;

    backup-tenant)
      require_root
      local name="${1:-}"
      [[ -n "$name" ]] || die "usage: backup-tenant <name>"
      ports_registry_init
      backup_tenant_postgres "$name"
      ;;

    backup-all)
      require_root
      ports_registry_init
      local names
      names="$(all_tenant_names)"
      [[ -n "$names" ]] || { say "no tenants registered"; exit 0; }
      while IFS= read -r name; do
        [[ -n "$name" ]] || continue
        # Subshell so a per-tenant die() doesn't abort the whole fleet backup.
        ( backup_tenant_postgres "$name" ) || say "WARNING: backup failed for $name (continuing)"
      done <<< "$names"
      ;;

    list-backups)
      ports_registry_init
      list_tenant_backups "${1:-}"
      ;;

    restore-tenant)
      require_root
      local name="" file="" confirmed=false
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --yes) confirmed=true; shift ;;
          -*)    die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            elif [[ -z "$file" ]]; then file="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" && -n "$file" ]] || die "usage: restore-tenant <name> <file> --yes"
      ports_registry_init
      restore_tenant_postgres "$name" "$file" "$confirmed"
      ;;

    doctor)
      doctor
      ;;

    help|--help|-h)
      usage
      ;;

    *)
      die "unknown command: $command_name (try: $0 help)"
      ;;
  esac
}

# Only dispatch when executed directly; allows sourcing for tests/inspection.
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  main "$@"
fi
