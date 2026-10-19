#!/usr/bin/env bash
# shellcheck disable=SC2178  # DarkIRC argument parsers use intentional array namerefs.
# shellcheck disable=SC2034  # constants here are consumed by the sourced mt-admin-lib modules
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
TENANT_LIFECYCLE_LOCK_DIR="${LUNARWING_MT_LIFECYCLE_LOCK_DIR:-/run/lock/lunarwing-tenants}"
unset LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT
PORTS_LOCK_HELD="false"
PROFILE="${LUNARWING_MT_PROFILE:-release}"
# ── BUG-005 resource bounding ────────────────────────────────────────────────
# Transient heavy workloads (agent shell commands, compiles) must not live in
# the daemon's own service cgroup. On systemd, the daemon spawns them as
# transient scopes in WORKLOAD_SLICE (see ic/src/tools/process.rs) and
# build_tenant runs cargo in BUILD_SLICE; both slices are rendered per-tenant
# with the caps below. The daemon unit itself gets DAEMON_* caps so a runaway
# daemon is bounded independently of its workloads.
WORKLOAD_SLICE="${LUNARWING_MT_WORKLOAD_SLICE:-lunarwing-workloads.slice}"
WORKLOAD_MEMORY_HIGH="${LUNARWING_MT_WORKLOAD_MEMORY_HIGH:-6G}"
WORKLOAD_MEMORY_MAX="${LUNARWING_MT_WORKLOAD_MEMORY_MAX:-8G}"
WORKLOAD_CPU_QUOTA="${LUNARWING_MT_WORKLOAD_CPU_QUOTA:-800%}"
WORKLOAD_TASKS_MAX="${LUNARWING_MT_WORKLOAD_TASKS_MAX:-2048}"
BUILD_SLICE="${LUNARWING_MT_BUILD_SLICE:-lunarwing-build.slice}"
BUILD_JOBS="${LUNARWING_MT_BUILD_JOBS:-$(nproc)}"
BUILD_MEMORY_HIGH="${LUNARWING_MT_BUILD_MEMORY_HIGH:-16G}"
BUILD_MEMORY_MAX="${LUNARWING_MT_BUILD_MEMORY_MAX:-24G}"
DAEMON_MEMORY_HIGH="${LUNARWING_MT_DAEMON_MEMORY_HIGH:-1536M}"
DAEMON_MEMORY_MAX="${LUNARWING_MT_DAEMON_MEMORY_MAX:-2G}"
DAEMON_TASKS_MAX="${LUNARWING_MT_DAEMON_TASKS_MAX:-512}"
SOURCE_REPO="${LUNARWING_MT_SOURCE_REPO:-$LUNARWING_ROOT}"
DARKIRC_SOURCE="${LUNARWING_MT_DARKIRC_SOURCE:-}"
# Managed exchange accepts only this pinned, root-attested DarkFi profile. Build
# recipes execute in a disposable tree as a dedicated unprivileged account.
DARKIRC_REPO="${LUNARWING_MT_DARKIRC_REPO:-https://github.com/darkrenaissance/darkfi}"
DARKIRC_APPROVED_REV="a05956d412a091e8b54c1cd4f4264c33b941203d"
DARKIRC_REV="${LUNARWING_MT_DARKIRC_REV:-$DARKIRC_APPROVED_REV}"
DARKIRC_BUILD_USER="${LUNARWING_MT_DARKIRC_BUILD_USER:-lunarwing-build}"
DARKIRC_BUILD_HOME="${LUNARWING_MT_DARKIRC_BUILD_HOME:-/var/cache/lunarwing/build-home}"
DARKIRC_BUILD_ROOT="${LUNARWING_MT_DARKIRC_BUILD_ROOT:-/var/cache/lunarwing/darkirc-builds}"
DARKIRC_KEY_HELPER_BUILD_ROOT="${LUNARWING_MT_DARKIRC_KEY_HELPER_BUILD_ROOT:-/var/cache/lunarwing/darkirc-key-helper-target}"
DARKIRC_RELEASE_ROOT="${LUNARWING_MT_DARKIRC_RELEASE_ROOT:-/usr/local/lib/lunarwing/darkirc}"
DARKIRC_RELEASES_DIR="$DARKIRC_RELEASE_ROOT/releases"
DARKIRC_RELEASE_SELECTOR="$DARKIRC_RELEASE_ROOT/current"
DARKIRC_RELEASE_DAEMON="$DARKIRC_RELEASE_SELECTOR/darkirc"
DARKIRC_WRITER_LOCK_DIR="${LUNARWING_MT_DARKIRC_WRITER_LOCK_DIR:-/run/lock/lunarwing-darkirc}"
DARKIRC_WRITER_LOCK_HELD="false"
DARKIRC_WRITER_LOCK_FD=""
DARKIRC_WRITER_SCOPE=""
DARKIRC_WRITER_LOCK_INHERITED="false"
OPENRC_ENV_EXEC_SRC="$SCRIPT_DIR/lunarwing-openrc-env-exec.sh"
OPENRC_ENV_EXEC="/usr/local/libexec/lunarwing-openrc-env-exec"
RELAY_ENV_EXEC_SRC="$SCRIPT_DIR/lunarwing-relay-env-exec.py"
RELAY_ENV_EXEC="/usr/local/libexec/lunarwing-relay-env-exec"
OPENRC_INITD_DIR="${LUNARWING_MT_OPENRC_INITD_DIR:-/etc/init.d}"
OPENRC_CONFD_DIR="${LUNARWING_MT_OPENRC_CONFD_DIR:-/etc/conf.d}"
OPENRC_PROXY_BACKUP_DIR="${LUNARWING_MT_OPENRC_PROXY_BACKUP_DIR:-/var/lib/lunarwing/openrc-proxy-backups}"
OPENRC_RUNLEVELS_DIR="${LUNARWING_MT_OPENRC_RUNLEVELS_DIR:-/etc/runlevels}"
TEMPLATES_DIR="${SCRIPT_DIR}/templates"
# WI#330: no built-in default — the historical developer-LAN address was
# unreachable from every other host and silently defeated the worker images'
# placeholder guards. add-tenant fails loudly when neither --tensorzero-url
# nor this fleet default (nor --llm-base-url for non-proxy tenants) is set.
DEFAULT_TENSORZERO_URL="${LUNARWING_MT_TENSORZERO_URL:-}"
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
VOICE_MODELS_DIR="${LUNARWING_MT_VOICE_MODELS_DIR:-/var/lib/lunarwing/voice-models}"
DEFAULT_EXTERNAL_WORKER_TIMEOUT_MS="${LUNARWING_MT_EXTERNAL_WORKER_TIMEOUT_MS:-3600000}"
LEGACY_EXTERNAL_WORKER_TIMEOUT_MS=300000
MIGRATE_LEGACY_EXTERNAL_WORKER_TIMEOUTS="${LUNARWING_MT_MIGRATE_LEGACY_EXTERNAL_WORKER_TIMEOUTS:-true}"

# ── Health-check / self-heal pipeline (host-global) ──────────────────────────
# The infra health-check + self-heal pipeline auto-discovers every tenant from
# /etc/init.d and the port registry, so ONE host-global scheduled run covers all
# current and future tenants. Enabled by default for new tenants on OpenRC; opt
# out per add-tenant with --no-health, or fleet-wide with
# LUNARWING_MT_HEALTH_ENABLED=false.
DEFAULT_HEALTH_ENABLED="${LUNARWING_MT_HEALTH_ENABLED:-true}"
# Resolved after init detection: OpenRC defaults to 5 minutes, systemd to 15.
# An explicit override applies to either init system.
HEALTH_INTERVAL_MIN="${LUNARWING_MT_HEALTH_INTERVAL_MIN:-}"
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

STUDY_NOTES_CONTAINER_HOME="/opt/lw2studymcp"
STUDY_NOTES_CONTAINER_STATE_HOME="/var/lib/lw2studymcp"
# Shared host checkout of the lw2studymcp study-notes MCP. `add-tenant
# --with-study-mcp` and a `configure-study-mcp` without an explicit --home
# sync this checkout to the latest upstream before wiring it into the tenant,
# so every new LunarWing starts with current knowledge of its own codebase.
STUDY_NOTES_REPO="${LUNARWING_MT_STUDY_NOTES_REPO:-https://git.lunarwing.org/cmc/lw2studymcp.git}"
STUDY_NOTES_DEFAULT_HOME="${LUNARWING_MT_STUDY_NOTES_HOME:-/var/cache/lunarwing/lw2studymcp}"
# How many root-owned knowledge-scheduler snapshots to retain under
# releases/ (the live one plus a rollback margin); older ones are pruned
# after each scheduler install.
KNOWLEDGE_SYNC_SNAPSHOT_KEEP="${LUNARWING_MT_KNOWLEDGE_SYNC_SNAPSHOT_KEEP:-5}"
OPENCODE_CONTAINER_UID="10001"
OPENCODE_CONTAINER_GID="10001"
OPENCODE_USERNS="keep-id:uid=${OPENCODE_CONTAINER_UID},gid=${OPENCODE_CONTAINER_GID}"
# ssh-agent-dir-ro: keep the restart-safe directory mount, but make it
# read-only so workers cannot unlink/rebind the host agent socket (WI#226).
# Bumping the contract recreates imperative containers still using RW mounts.
OPENCODE_ROOTLESS_LAUNCH_CONTRACT="userns=${OPENCODE_USERNS};ssh-agent-dir-ro"

KIMI_CONTAINER_UID="10002"
KIMI_CONTAINER_GID="10002"
KIMI_USERNS="keep-id:uid=${KIMI_CONTAINER_UID},gid=${KIMI_CONTAINER_GID}"
# WI#206: the kimi lane dropped the SSH-agent mount and gained
# no-new-privileges; bumping the contract force-recreates existing containers
# that still carry the socket mount.
KIMI_ROOTLESS_LAUNCH_CONTRACT="userns=${KIMI_USERNS};no-ssh-agent;no-new-privileges"

# OpenCode 2 worker lane (docs/plans/OPENCODE2_EXTERNAL_WORKER_PLAN.md).
# Distinct UID so opencode (10001) and opencode2 (10003) on one tenant keep
# separate userns identities; must stay in sync with the image's non-root
# user in opencode2.4lunarwing/Dockerfile.
OPENCODE2_CONTAINER_UID="10003"
OPENCODE2_CONTAINER_GID="10003"
OPENCODE2_USERNS="keep-id:uid=${OPENCODE2_CONTAINER_UID},gid=${OPENCODE2_CONTAINER_GID}"
# Same read-only directory mount and recreation contract as opencode.
OPENCODE2_ROOTLESS_LAUNCH_CONTRACT="userns=${OPENCODE2_USERNS};ssh-agent-dir-ro"

# The pebble lane mounts the SSH agent directory like the other non-kimi
# workers; bump the contract to recreate existing containers with RW mounts.
PEBBLE_ROOTLESS_LAUNCH_CONTRACT="ssh-agent-dir-ro"

# ── Library sourcing ─────────────────────────────────────────────────────────
#
# mt-admin is split into sourced libraries under mt-admin-lib/ (structure kept
# from ref2/sunburst/mtadmin-refactor-v2.0.6.0; design record:
# docs/proposals/MT-ADMIN-DECOMPOSITION.md):
#
#   00-common           helpers, locks, tenant path primitives, feature flags
#   01-detect           init system + container runtime detection
#   02-containers       container helpers, babysitters, vision/voice sidecars
#   03-ports-registry   /etc/lunarwing/ports.json schema, migrations, queries
#   04-tenant-lifecycle add/remove/start/stop/upgrade/status/doctor verbs
#   05-tenant-build     user/repo/checkout, builds, WASM install
#   06-env-generation   env files, DarkIRC runtime/contacts, SSH, Gotify
#   07-units-systemd    quadlets + systemd user lifecycle (_systemctl_user)
#   08-units-openrc     OpenRC conf.d/init.d rendering + lifecycle
#   09-worker-config    external worker config + lifecycle, study-notes MCP
#   10-owner-scope      owner-scope DB migration
#   11-postgres         per-tenant PostgreSQL lifecycle + backups
#   12-health-pipeline  host-global health/self-heal install
#   13-cgroups-openrc   OpenRC cgroup v2 subtrees, local.d + PAM wiring
#
# Libraries define functions (plus a few state defaults) only, so load order
# is not load-bearing. The composition root defines all constants and
# `set -euo pipefail` before any library is sourced.

MT_ADMIN_LIB_DIR="${SCRIPT_DIR}/mt-admin-lib"
[[ -d "$MT_ADMIN_LIB_DIR" ]] || {
  printf 'error: mt-admin library directory not found: %s\n' "$MT_ADMIN_LIB_DIR" >&2
  exit 1
}

# shellcheck source=mt-admin-lib/00-common.sh
source "$MT_ADMIN_LIB_DIR/00-common.sh"
# shellcheck source=mt-admin-lib/01-detect.sh
source "$MT_ADMIN_LIB_DIR/01-detect.sh"
# shellcheck source=mt-admin-lib/02-containers.sh
source "$MT_ADMIN_LIB_DIR/02-containers.sh"
# shellcheck source=mt-admin-lib/03-ports-registry.sh
source "$MT_ADMIN_LIB_DIR/03-ports-registry.sh"
# shellcheck source=mt-admin-lib/04-tenant-lifecycle.sh
source "$MT_ADMIN_LIB_DIR/04-tenant-lifecycle.sh"
# shellcheck source=mt-admin-lib/05-tenant-build.sh
source "$MT_ADMIN_LIB_DIR/05-tenant-build.sh"
# shellcheck source=mt-admin-lib/06-env-generation.sh
source "$MT_ADMIN_LIB_DIR/06-env-generation.sh"
# shellcheck source=mt-admin-lib/07-units-systemd.sh
source "$MT_ADMIN_LIB_DIR/07-units-systemd.sh"
# shellcheck source=mt-admin-lib/08-units-openrc.sh
source "$MT_ADMIN_LIB_DIR/08-units-openrc.sh"
# shellcheck source=mt-admin-lib/09-worker-config.sh
source "$MT_ADMIN_LIB_DIR/09-worker-config.sh"
# shellcheck source=mt-admin-lib/10-owner-scope.sh
source "$MT_ADMIN_LIB_DIR/10-owner-scope.sh"
# shellcheck source=mt-admin-lib/11-postgres.sh
source "$MT_ADMIN_LIB_DIR/11-postgres.sh"
# shellcheck source=mt-admin-lib/12-health-pipeline.sh
source "$MT_ADMIN_LIB_DIR/12-health-pipeline.sh"
# shellcheck source=mt-admin-lib/13-cgroups-openrc.sh
source "$MT_ADMIN_LIB_DIR/13-cgroups-openrc.sh"
# shellcheck source=mt-admin-lib/14-knowledge-sync.sh
source "$MT_ADMIN_LIB_DIR/14-knowledge-sync.sh"

usage() {
  cat <<'EOF'
Usage:
  sudo lunarwing-mt-admin.sh <command> [args...]

Production multi-tenant administration for LunarWing.
Manages OS users, port allocation, per-tenant services, and builds.

Commands:
  add-tenant <name> [options]      Create user, allocate ports, clone repo,
                                   generate env, render and install services
    --docker-group                 Add user to docker/podman group (opt-in:
                                   not needed for rootless podman; only to
                                   reach a shared rootful daemon socket)
    --xmpp-jid <jid>              XMPP JID for this tenant
    --xmpp-password <pass>        XMPP password (generated if omitted)
    --llm-api-key <key>            API key for the LLM backend provider
    --llm-base-url <url>           LLM endpoint the daemon dials (LLM_BASE_URL).
                                   Default: this tenant's local TensorZero proxy.
                                   External workers inherit it when it is
                                   reachable from inside a container (not host
                                   loopback, not an overlay MagicDNS name)
    --tensorzero-url <url>         Upstream TensorZero URL. REQUIRED unless
                                   LUNARWING_MT_TENSORZERO_URL is set — or
                                   --llm-base-url is given and --enable-proxy
                                   is not (WI#330: there is no built-in
                                   default; the old one was a developer-LAN
                                   address unreachable from any other host)
    --gotify-url <url>             Custom Gotify server URL (e.g. https://gotify.example.com)
    --no-health                    Don't enable the host-global health/self-heal pipeline
    --no-ssh                       Don't provision SSH harness (key pair, config, agent)
    --no-weechat-bootstrap         Don't run WeeChat relay auto-bootstrap (still writes
                                   minimal weechat.env; services remain rendered)
    --enable-darkirc               Provision DarkIRC daemon + adapter for this tenant
                                   (disabled by default; darkirc services are NOT created)
    --with-pebble                  Select the pebble worker for this tenant (as above)
    --with-opencode                Select the opencode worker for this tenant (as above)
    --with-kimi                    Select the kimi worker for this tenant (as above)
    --with-opencode2               Select the OpenCode 2 worker for this tenant
                                   (v2 lane; keep off tenants that already run
                                   the proven v1 opencode worker)
    --with-study-mcp               Auto-configure the shared lw2studymcp Study Notes
                                   MCP for this tenant (read-only) so it starts with
                                   knowledge of the LunarWing codebase. The shared
                                   checkout is always synced to the latest upstream
                                   first (cloned when missing, fast-forwarded when
                                   present and clean). Also arms a one-shot import
                                   of the synced checkout into the tenant's own
                                   knowledge store (KNOWLEDGE_SEED_FROM_STUDY_MCP=1,
                                   first boot only) and enables bounded additive
                                   refresh from hourly published checkout generations.
                                   Existing knowledge paths are never overwritten.
    --study-mcp-home <path>        Use this checkout instead of the default shared one
                                   ($STUDY_NOTES_DEFAULT_HOME; still synced to latest)
    --opencode-model <model>       Override the opencode worker's LLM model
                                   (written to lunarwing.env as OPENCODE_MODEL)
    --opencode-base-url <url>      Override the opencode worker's TensorZero baseURL
                                   (written to lunarwing.env as OPENCODE_BASE_URL)
    --opencode2-model <model>      Override the OpenCode 2 worker's LLM model
                                   (written to lunarwing.env as OPENCODE2_MODEL)
    --opencode2-base-url <url>     Override the OpenCode 2 worker's TensorZero baseURL
                                   (written to lunarwing.env as OPENCODE2_BASE_URL)
    --kimi-model <model>           Override the kimi worker's LLM model
                                   (written to lunarwing.env as KIMI_MODEL)
    --kimi-base-url <url>          Override the kimi worker's API base URL
                                   (written to lunarwing.env as KIMI_BASE_URL)
    --kimi-api-key-file <path>     Read kimi API key from a file, never argv
                                   (written to lunarwing.env as KIMI_API_KEY)
    --llm-model <model>            Override LLM_MODEL (default:
                                   tensorzero::function_name::lunarwing)
    --gateway-host <host>          Override GATEWAY_HOST bind address (default:
                                   127.0.0.1; use 0.0.0.0 for LAN access)
    --xmpp-allow-from <jids>       Comma-separated extra XMPP JIDs allowed to DM
                                   the agent (added to the tenant's own JID;
                                   written to both lunarwing.env and xmpp-bridge.env)

   add-tenants <names> [options]    Comma-separated list (e.g. "Ruffles,Miyuki")
     (same options as add-tenant apply to all, including --enable-darkirc,
       --opencode-model/--opencode-base-url, --kimi-model/--kimi-base-url/
       --kimi-api-key-file, --with-study-mcp/--study-mcp-home,
      --llm-model, --gateway-host, --xmpp-allow-from, and --no-weechat-bootstrap)

  remove-tenant <name>             Stop services, deallocate ports
    --purge                        Also delete OS user and home directory

  build-tenant <name>             Build binaries for one tenant (OOM-safe flock)
    --with-wasm                    Also build WASM extensions
    --require-wasm                 Fail instead of preserving stale WASM (requires --with-wasm)
    --with-pebble                  Also build the pebble worker Docker image
    --with-opencode                Also build the opencode worker Docker image
    --with-kimi                    Also build the kimi worker Docker image
    --with-opencode2               Also build the OpenCode 2 worker Docker image
    --with-toolchains              Include toolchains in selected pebble, opencode,
                                   and kimi worker images (default: slim)

  build-all                        Build each tenant sequentially
    --with-wasm                    Also build WASM extensions
    --with-pebble                  Also build the pebble worker Docker image
    --with-opencode                Also build the opencode worker Docker image
    --with-kimi                    Also build the kimi worker Docker image
    --with-opencode2               Also build the OpenCode 2 worker Docker image
    --with-toolchains              Include toolchains in selected pebble, opencode,
                                   and kimi worker images (default: slim)

  build-pebble-worker             Build the pebble worker Docker image
    --no-cache                     Force a full rebuild without Docker cache
    --with-toolchains              Include Rust/Go/C++ toolchains (default: slim)

  build-opencode-worker            Build the opencode worker Docker image
    --no-cache                     Force a full rebuild without Docker cache
                                   (Podman also prunes dangling images and avoids
                                   caching intermediate build layers)
    --with-toolchains              Include Rust/Go/C++ toolchains (default: slim)

  build-kimi-worker                Build the kimi worker Docker image
    --no-cache                     Force a full rebuild without container cache
    --with-toolchains              Include Rust/Go/C++ toolchains (default: slim)
    --kimi-code-version <version>  Override kimi-code package version

  build-opencode2-worker           Build the OpenCode 2 worker Docker image
    --no-cache                     Force a full rebuild without Docker cache
    --with-toolchains              Include Rust/Go/C++ toolchains (default: slim)

  build-sandbox-image [name]       Build the lean generic sandbox image
                                    (lunarwing-sandbox:latest from
                                    ic/docker/sandbox.Dockerfile; optional
                                    tenant selects its configured runtime)
  build-vision-sidecar             Build the LunarVision OCR sidecar Docker image
  configure-vision-external <name> Use an operator-managed remote LunarVision instance
    --url <https-origin>             Base origin routing API and health endpoints
    --token-file <path>             Read its bearer token from a one-line file
  configure-vision-local <name>    Restore the managed local LunarVision sidecar
  build-voice-sidecar              Build the LunarVoice ONNX sidecar container image
  install-voice <name>             Install LunarVoice image/config without activating it
    [--model-manifest <path>]      Install SHA-256-pinned models from a trusted manifest
  configure-voice <name>          Update validated per-tenant voice settings; an active
                                   sidecar and daemon are reconciled automatically
    [--stt-engine whisper|parakeet] [--stt-model <name>] [--stt-language <tag>]
    [--enable-vad|--disable-vad] [--enable-tts|--disable-tts]
    [--max-audio-secs <n>] [--max-audio-bytes <n>] [--max-text-chars <n>]
    [--max-tts-audio-secs <n>] [--request-timeout-secs <n>]
    [--max-concurrent-inference <n>] [--rate-limit-per-sec <n>]
    [--cache-ttl-secs <n>] [--cache-max-entries <n>]
    [--tts-default-voice <name>] [--ort-num-threads <n>]
  activate-voice <name>           Health-gated start; activates daemon transcription
  deactivate-voice <name>         Stop sidecar and restore prior transcription provider
  remove-voice <name>             Deactivate and remove tenant config/container/units
                                   (shared verified models and image are retained)
  enable-voice / disable-voice     Compatibility aliases for activate/deactivate

  build-darkirc                   Build darkirc daemon from external source
  ensure-darkirc-release <name>   Build the atomic release set when required
                                   (shared binary, not per-tenant)

  install-wasm <name>             Install built WASM tools/channels into tenant state dir
  install-wasm-all                Install WASM for all tenants
                                   (darkirc channel installs only for tenants
                                   provisioned with --enable-darkirc; use
                                   install-wasm to drop a stale darkirc channel)

  start-tenant <name>             Start all services for a tenant
  stop-tenant <name>              Stop all services for a tenant
  cleanup-legacy-weechat <name>   Remove obsolete weechat-<name> service
  restart-weechat-adapter <name>  Restart adapter through detected init system
  stop-tenant-for-upgrade <name>  Verify settled DarkIRC state and stop atomically
  migration-export-boundary <name> [export options]
                                  Run export under retained lifecycle/DarkIRC locks
  restart-tenant <name>           Stop then start
  fix-podman-userns <name>        Repair a broken rootless podman userns pairing
                                  (WI#142: "cannot re-exec process to join the
                                  existing user namespace"). Probe -> stop ->
                                  pause.pid cleanup -> system migrate -> start.
                                  No-op when the podman probe is healthy.
  start-pg <name>                 Start only the tenant's PostgreSQL (daemon stays down)
  stop-pg <name>                  Stop only the tenant's PostgreSQL
  start-worker <name> <worker>    Start one enabled external worker
  stop-worker <name> <worker>     Gracefully stop one external worker
  restart-worker <name> <worker>  Restart one external worker
  kill-worker <name> <worker>     Emergency zero-grace stop; leaves worker stopped
                                   Workers: pebble, opencode, kimi, opencode2
  render-units <name>             Re-render a tenant's service units from the current
                                  generator (no restart; applies init-script changes)
  render-units --all-proxies      OpenRC only: replace proxy units fleet-wide without
                                  invoking OpenRC (WI#376 neutralize-first migration)
  upgrade-tenant <name> --target <ref>
                                  In-place upgrade: backup, fetch, clean-tree gate,
                                  stop, checkout <ref> (as tenant user), rebuild
                                   with WASM and selected OpenCode/Kimi workers, re-render
                                  units, patch env, start.
                                  OpenRC neutralizes fleet proxy units before probes/stop.
                                  Long-running — run inside tmux.
    --source-repo <path>           Point the tenant's git origin at this repo first
    --no-backup                    Skip the pre-upgrade Postgres backup
    --skip-render                  Keep other unit files; OpenRC proxy neutralization
                                   is mandatory (run render-units later;
                                   must happen before v2.0.0 for pre-1.1.9 tenants)
  rotate-pg-password <name>       Generate a new random PG password (ALTER ROLE + env update)
  psql <name> [psql args...]      Run psql inside the tenant's own PostgreSQL
                                  container (init/runtime-agnostic, via _ctr).
                                  Read-only diagnostics: SELECTs and fingerprints
                                  only — DB rows can be secret-bearing.
  daemon-pid <name>               Print the tenant daemon's PID (the process that
                                  loaded the tenant env). OpenRC reads OpenRC's
                                  child_pid record — NOT the supervise-daemon
                                  pidfile, which holds the supervisor's PID. The
                                  record is validated as-written (mixed
                                  digit/non-digit records are rejected) and the
                                  process must be owned by the tenant and carry
                                  the tenant home path in its cmdline. Exits
                                  non-zero when unresolvable; treat that as
                                  uninspected, never as evidence.

  configure-gotify <name> <url>    Set custom Gotify URL for a tenant
                                   (updates workspace config + capabilities)

  configure-pebble <name>          Configure pebble worker for a tenant
    --nanogpt-api-key <key>        NanoGPT API key
    --model <model>                Pebble model (default: openai/gpt-5.2)

  configure-opencode <name>        Set opencode worker LLM overrides for a tenant
    --model <model>                TensorZero model (OPENCODE_MODEL; any string)
    --base-url <url>               TensorZero baseURL (OPENCODE_BASE_URL; full URL)
                                   (restart the worker after: stop-tenant && start-tenant)

  configure-opencode2 <name>       Set OpenCode 2 worker LLM overrides for a tenant
    --model <model>                TensorZero model (OPENCODE2_MODEL; any string)
    --base-url <url>               TensorZero baseURL (OPENCODE2_BASE_URL; full URL)
                                   (restart the worker after: stop-tenant && start-tenant)

  configure-kimi <name>            Set kimi worker LLM overrides for a tenant
    --model <model>                Kimi model (KIMI_MODEL; any string)
    --base-url <url>               Kimi API base URL (KIMI_BASE_URL; full URL)
    --api-key-file <path>          Read Kimi API key from a file, never argv
                                   (restart the worker after: stop-tenant && start-tenant)

  configure-study-mcp <name>       Configure lw2studymcp for LunarWing and
    [--home <absolute-path>]       its opencode worker using a shared host checkout
                                   (default: STUDY_NOTES_DEFAULT_HOME; a default-home
                                   run always syncs the checkout to latest upstream
                                   first, cloning it when missing)
    [--sync]                       Also fast-forward an explicit --home checkout to
                                   latest upstream (must be a clean git checkout)
    [--allow-write]                Enable approved core-agent writes using separate state
    [--state-home <absolute-path>]  State path (default: tenant lunarwing/study-notes-state)
    [--enable-knowledge-sync]      Enable additive refresh + host source scheduler
    [--disable-knowledge-sync]     Disable the tenant daemon refresh cadence
                                   (restart the tenant after configuration changes)

  refresh-knowledge <name>         Run one bounded additive tenant import now
    [--sync-source]                Fast-forward and publish its checkout first
  sync-knowledge-sources           Refresh each unique source used by enabled tenants
                                   (normally invoked by the host-global scheduler)

   configure-ssh <name>             Provision SSH harness for an existing tenant
     --host <host>                  SSH host (default: 127.0.0.1)
     --user <user>                  SSH user (default: tenant name)
                                   (restart the tenant after to upload the key: restart-tenant)

  configure-weechat-relay <name>   Generate WeeChat relay config for a tenant.
                                   Preserves existing config: fails on any
                                   non-empty ~/.config/weechat without modifying it.
  rotate-relay-password <name>     Rotate WeeChat relay credentials and restart
                                   running consumers (brief service interruption).

  disable-weechat <name>           Disable WeeChat for a tenant: stops its (optional)
  enable-weechat <name>            weechat units and removes their boot persistence
                                   (systemd disable / OpenRC runlevel removal), and
                                   health checks report them `skipped` when down so
                                   self-heal will not restart-storm them. start-tenant
                                   leaves them off. enable-weechat re-enables + starts
                                   them. Default is enabled (absent flag == enabled).

  patch-env <name>                 Add missing env vars (e.g. ORCHESTRATOR_PORT)
  patch-env-all                    Patch env for all registered tenants

  darkirc-contact list|status|doctor|adopt ...
                                   Inventory/adopt contacts using fingerprints only
  darkirc-contact prepare|respond|complete|cancel|exchanges ...
                                   Public-only Stage A exchange; complete is deferred
  darkirc-contact export-migration|stage-migration|import-migration|migration-ready|recover ...
                                   Structured, settled-state DarkIRC migration
  darkirc-contact validate-migration --in - [--unbound]
                                   Validate a manifest without tenant-state writes
  darkirc-health <tenant> --strict [--json]
                                   Diagnostic daemon/adapter connectivity check

  migrate-owner-scope <name>       Rekey DB data from 'default' to tenant scope
    --from <old_scope>             Old owner_id (default: 'default')
                                   (run after patch-env adds LUNARWING_OWNER_ID)
  owner-scopes <name>              Print restored owner scopes and row counts

  list-tenants                     Show all tenants with ports and status
  status <name>                    Detailed status for one tenant
  worker-listeners <name>          WI#357 exposure audit: list listening
                                   sockets inside the tenant's running worker
                                   containers; fail on any bind or published
                                   mapping outside the loopback contract
  tokens [name]                    Print gateway auth tokens (all or one)
  doctor                           System dependency and health checks
  coord-doctor <name>              Read-only coordination knowledge-share
                                   (KNOW:) validation: room join/receive
                                   allowlists, publisher classification,
                                   bridge presence, and ingest counters.
                                   Prints no secrets; reports, never rewrites.
  setup-cgroups                    (OpenRC) Install pre-delegated tenant cgroup v2
                                   subtrees (docs/ops/PREDELEGATED_SUBTREE_CGROUPS.md):
                                   boot-time /etc/local.d/lunarwing-cgroups.start,
                                   PAM session hook for login shells, and per-tenant
                                   subtrees for every registered tenant. add-tenant
                                   runs this automatically; use it to upgrade hosts
                                   and tenants provisioned before subtree support.

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
  LUNARWING_MT_STUDY_NOTES_HOME    Default shared Study Notes checkout
  LUNARWING_MT_STUDY_NOTES_REPO    Repository used when that checkout is first cloned
  LUNARWING_MT_KNOWLEDGE_SYNC_ADMIN_ROOT
                                   Root-owned snapshot directory used by the host scheduler
  LUNARWING_MT_OPENRC_PROXY_BACKUP_DIR
                                   Private archive for superseded OpenRC proxy units
                                   (default /var/lib/lunarwing/openrc-proxy-backups)
  LUNARWING_MT_BACKUP_DIR          Backup directory (default /var/lib/lunarwing-backups)
  LUNARWING_MT_BACKUP_KEEP         Keep last N dumps per tenant (default 7; 0 = keep all)

Per-tenant LLM tunables (written to each tenant's lunarwing.env with the
fleet defaults below; edit the file + restart-tenant to override — an existing
value is preserved across re-provision):
  LLM_CIRCUIT_BREAKER_THRESHOLD    Consecutive fully-retried LLM failures before
                                   the breaker opens and fast-fails (default 7)
  LLM_CIRCUIT_BREAKER_RECOVERY_SECS  Seconds the breaker stays open before it
                                   probes for recovery (default 45)
  ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS  Cap on tool-calling rounds per lightweight
                                   routine run (new tenants 6; hard limit 10)
EOF
}

# ── Main dispatcher ──────────────────────────────────────────────────────────


main() {
  local command_name="${1:-}"
  if [[ -z "$command_name" ]]; then
    usage
    exit 1
  fi
  shift || true

  if ! openrc_dispatch_preflight "$command_name" "$@"; then
    # writers-active has a 0/1/2 state contract (active/stopped/unknown), and
    # export-tenant/import-tenant read 1 as "stopped". A refusal is unknown —
    # never a service state (EQ-1).
    if [[ "$command_name" == writers-active ]]; then
      say "OpenRC proxy migration required; writer state unknown — run: $0 render-units --all-proxies" >&2
      exit 2
    fi
    die "OpenRC proxy migration required; refusing $command_name"
  fi

  # An explicitly-chosen runtime persists no matter which command runs.
  # Without this, commands that never touch containers (list-tenants, tokens,
  # ...) silently ignore LUNARWING_CONTAINER_RUNTIME and nothing is saved,
  # breaking the "set it once" promise. ensure_container_runtime validates,
  # persists idempotently, and memoizes; unprivileged runs warn-and-continue.
  [[ -z "${LUNARWING_CONTAINER_RUNTIME:-}" ]] || ensure_container_runtime

  case "$command_name" in
    add-tenant)
      require_root
      local name="" docker_group="false" xmpp_jid="" xmpp_password="" tz_url="$DEFAULT_TENSORZERO_URL" gotify_url="$DEFAULT_GOTIFY_URL" gotify_title="$DEFAULT_GOTIFY_TITLE" llm_api_key="" llm_base_url="$DEFAULT_LLM_BASE_URL" enable_darkirc="false" enable_proxy="false" llm_model="" gateway_host="" xmpp_allow_from="" opencode_model="" opencode_base_url="" opencode2_model="" opencode2_base_url="" kimi_model="" kimi_base_url="" kimi_api_key="" with_pebble="false" with_opencode="false" with_kimi="false" with_opencode2="false" darkirc_scope_override="" with_study_mcp="false" study_mcp_home=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --docker-group)    docker_group="true"; shift ;;
          --xmpp-jid)        xmpp_jid="$2"; shift 2 ;;
          --no-health)       HEALTH_OPT_OUT=true; shift ;;
          --no-ssh)          SSH_OPT_OUT=true; shift ;;
          --no-weechat-bootstrap) WEECHAT_BOOTSTRAP_OPT_OUT=true; shift ;;
          --enable-darkirc)  enable_darkirc="true"; shift ;;
          --darkirc-scope-id) [[ $# -ge 2 ]] || die "--darkirc-scope-id requires a value"; darkirc_scope_override="$2"; shift 2 ;;
          --enable-proxy)    enable_proxy="true"; shift ;;
          --with-nanocode|--nanocode-model|--nanocode-base-url) die "Nanocode worker is retired; remove $1" ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --with-kimi)       with_kimi="true"; shift ;;
          --with-opencode2)  with_opencode2="true"; shift ;;
          --with-study-mcp)  with_study_mcp="true"; shift ;;
          --study-mcp-home)  [[ $# -ge 2 ]] || die "--study-mcp-home requires a value"; study_mcp_home="$2"; shift 2 ;;
          --xmpp-password)   xmpp_password="$2"; shift 2 ;;
          --llm-api-key)     llm_api_key="$2"; shift 2 ;;
          --llm-base-url)    llm_base_url="$2"; shift 2 ;;
          --tensorzero-url)  tz_url="$2"; shift 2 ;;
          --gotify-url)      gotify_url="$2"; shift 2 ;;
          --gotify-title)    gotify_title="$2"; shift 2 ;;
          --opencode-model)    opencode_model="$2"; shift 2 ;;
          --opencode-base-url) opencode_base_url="$2"; shift 2 ;;
          --opencode2-model)    [[ $# -ge 2 ]] || die "--opencode2-model requires a value"; opencode2_model="$2"; shift 2 ;;
          --opencode2-base-url) [[ $# -ge 2 ]] || die "--opencode2-base-url requires a value"; opencode2_base_url="$2"; shift 2 ;;
          --kimi-model)        kimi_model="$2"; shift 2 ;;
          --kimi-base-url)     kimi_base_url="$2"; shift 2 ;;
          --kimi-api-key-file) kimi_api_key="$(_read_secret_file "$2" "Kimi API key")"; shift 2 ;;
          --kimi-api-key)      die "--kimi-api-key was removed; use --kimi-api-key-file" ;;
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
      # Leave empty when --xmpp-jid is absent: add_tenant preserves the tenant's
      # existing JID and only defaults to <name>@xmpp.localhost on a first write.
      name="$(sanitize_name "$name")"
      # WI#330: fail loudly here (before any tenant state exists) rather than
      # rendering an unreachable built-in endpoint.
      require_tenant_llm_endpoint "$tz_url" "$llm_base_url" "$enable_proxy"
      with_tenant_lifecycle_lock "$name" add_tenant "$name" "$docker_group" "$xmpp_jid" "$xmpp_password" "$tz_url" "$gotify_url" "$gotify_title" "$llm_api_key" "$llm_base_url" "$enable_darkirc" "$enable_proxy" "$llm_model" "$gateway_host" "$xmpp_allow_from" "$opencode_model" "$opencode_base_url" "$with_pebble" "$with_opencode" "$with_kimi" "$kimi_model" "$kimi_base_url" "$kimi_api_key" "$darkirc_scope_override" "$with_study_mcp" "$study_mcp_home" "$with_opencode2" "$opencode2_model" "$opencode2_base_url"
      ;;

    add-tenants)
      require_root
      local names_csv="" docker_group="false" xmpp_domain="" tz_url="$DEFAULT_TENSORZERO_URL" gotify_url="$DEFAULT_GOTIFY_URL" gotify_title="$DEFAULT_GOTIFY_TITLE" llm_api_key="" llm_base_url="$DEFAULT_LLM_BASE_URL" enable_darkirc="false" enable_proxy="false" llm_model="" gateway_host="" xmpp_allow_from="" opencode_model="" opencode_base_url="" opencode2_model="" opencode2_base_url="" kimi_model="" kimi_base_url="" kimi_api_key="" with_pebble="false" with_opencode="false" with_kimi="false" with_opencode2="false" with_study_mcp="false" study_mcp_home=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --docker-group)    docker_group="true"; shift ;;
          --xmpp-domain)     xmpp_domain="$2"; shift 2 ;;
          --no-health)       HEALTH_OPT_OUT=true; shift ;;
          --no-ssh)          SSH_OPT_OUT=true; shift ;;
          --no-weechat-bootstrap) WEECHAT_BOOTSTRAP_OPT_OUT=true; shift ;;
          --enable-darkirc)  enable_darkirc="true"; shift ;;
          --enable-proxy)    enable_proxy="true"; shift ;;
          --with-nanocode|--nanocode-model|--nanocode-base-url) die "Nanocode worker is retired; remove $1" ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --with-kimi)       with_kimi="true"; shift ;;
          --with-opencode2)  with_opencode2="true"; shift ;;
          --with-study-mcp)  with_study_mcp="true"; shift ;;
          --study-mcp-home)  [[ $# -ge 2 ]] || die "--study-mcp-home requires a value"; study_mcp_home="$2"; shift 2 ;;
          --llm-api-key)     llm_api_key="$2"; shift 2 ;;
          --llm-base-url)    llm_base_url="$2"; shift 2 ;;
          --tensorzero-url)  tz_url="$2"; shift 2 ;;
          --gotify-url)      gotify_url="$2"; shift 2 ;;
          --gotify-title)    gotify_title="$2"; shift 2 ;;
          --opencode-model)    opencode_model="$2"; shift 2 ;;
          --opencode-base-url) opencode_base_url="$2"; shift 2 ;;
          --opencode2-model)    [[ $# -ge 2 ]] || die "--opencode2-model requires a value"; opencode2_model="$2"; shift 2 ;;
          --opencode2-base-url) [[ $# -ge 2 ]] || die "--opencode2-base-url requires a value"; opencode2_base_url="$2"; shift 2 ;;
          --kimi-model)        kimi_model="$2"; shift 2 ;;
          --kimi-base-url)     kimi_base_url="$2"; shift 2 ;;
          --kimi-api-key-file) kimi_api_key="$(_read_secret_file "$2" "Kimi API key")"; shift 2 ;;
          --kimi-api-key)      die "--kimi-api-key was removed; use --kimi-api-key-file" ;;
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
      # WI#330: same endpoint requirement as add-tenant, checked once up front.
      require_tenant_llm_endpoint "$tz_url" "$llm_base_url" "$enable_proxy"

      local IFS=','
      local names_array
      read -ra names_array <<< "$names_csv"
      for raw_name in "${names_array[@]}"; do
        local sname
        sname="$(sanitize_name "$(echo "$raw_name" | xargs)")"
        [[ -n "$sname" ]] || continue
        say ""
        # Empty --xmpp-domain -> empty JID, so add_tenant preserves an existing one.
        local sjid=""
        if [[ -n "$xmpp_domain" ]]; then sjid="${sname}@${xmpp_domain}"; fi
        with_tenant_lifecycle_lock "$sname" add_tenant "$sname" "$docker_group" "$sjid" "" "$tz_url" "$gotify_url" "$gotify_title" "$llm_api_key" "$llm_base_url" "$enable_darkirc" "$enable_proxy" "$llm_model" "$gateway_host" "$xmpp_allow_from" "$opencode_model" "$opencode_base_url" "$with_pebble" "$with_opencode" "$with_kimi" "$kimi_model" "$kimi_base_url" "$kimi_api_key" "" "$with_study_mcp" "$study_mcp_home" "$with_opencode2" "$opencode2_model" "$opencode2_base_url"
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
      name="$(sanitize_name "$name")"
      with_tenant_lifecycle_lock "$name" remove_tenant "$name" "$purge"
      ;;

    build-tenant)
      require_root
      local name="" with_wasm="false" require_wasm="false" with_pebble="false" with_opencode="false" with_kimi="false" with_opencode2="false" with_toolchains="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --with-wasm)       with_wasm="true"; shift ;;
          --require-wasm)    require_wasm="true"; shift ;;
          --with-nanocode)   die "Nanocode worker is retired; remove --with-nanocode" ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --with-kimi)       with_kimi="true"; shift ;;
          --with-opencode2)  with_opencode2="true"; shift ;;
          --with-toolchains) with_toolchains="true"; shift ;;
          -*)                die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: build-tenant <name> [--with-wasm] [--require-wasm] [--with-pebble] [--with-opencode] [--with-kimi] [--with-opencode2] [--with-toolchains]"
      [[ "$require_wasm" != "true" || "$with_wasm" == "true" ]] || die "--require-wasm requires --with-wasm"
      build_tenant "$(sanitize_name "$name")" "$with_wasm" "$with_pebble" "$with_opencode" "$with_kimi" "$with_toolchains" "$require_wasm" "$with_opencode2"
      ;;

    build-all)
      require_root
      local with_wasm="false" with_pebble="false" with_opencode="false" with_kimi="false" with_opencode2="false" with_toolchains="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --with-wasm)       with_wasm="true"; shift ;;
          --with-nanocode)   die "Nanocode worker is retired; remove --with-nanocode" ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --with-kimi)       with_kimi="true"; shift ;;
          --with-opencode2)  with_opencode2="true"; shift ;;
          --with-toolchains) with_toolchains="true"; shift ;;
          -*)                die "unknown flag: $1" ;;
          *)                 die "unexpected argument: $1" ;;
        esac
      done
      build_all "$with_wasm" "$with_pebble" "$with_opencode" "$with_kimi" "$with_toolchains" "$with_opencode2"
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
      build_darkirc "$darkirc_tenant"
      ;;

    ensure-darkirc-release)
      require_root
      [[ $# -eq 1 ]] || die "usage: ensure-darkirc-release <name>"
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" ensure_darkirc_release "$_lifecycle_name"
      ;;

    build-nanocode-worker)
      die "Nanocode worker is retired; build-nanocode-worker is unavailable"
      ;;

    build-pebble-worker)
      require_root
      local no_cache="false"
      local with_toolchains="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --no-cache) no_cache="true"; shift ;;
          --with-toolchains) with_toolchains="true"; shift ;;
          -*)         die "unknown flag: $1" ;;
          *)          die "unexpected argument: $1" ;;
        esac
      done
      build_pebble_worker "$no_cache" "$with_toolchains"
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

    build-opencode2-worker)
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
      build_opencode2_worker "$no_cache" "$with_toolchains"
      ;;

    build-kimi-worker)
      require_root
      local no_cache="false"
      local with_toolchains="false"
      local kimi_code_version=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --no-cache)          no_cache="true"; shift ;;
          --with-toolchains)   with_toolchains="true"; shift ;;
          --kimi-code-version) kimi_code_version="$2"; shift 2 ;;
          -*)                  die "unknown flag: $1" ;;
          *)                   die "unexpected argument: $1" ;;
        esac
      done
      build_kimi_worker "$no_cache" "$with_toolchains" "$kimi_code_version"
      ;;

    build-vision-sidecar)
      require_root
      build_vision_sidecar_image
      ;;

    build-sandbox-image)
      require_root
      [[ $# -le 1 ]] || die "usage: $0 build-sandbox-image [name]"
      if [[ $# -eq 1 ]]; then
        local sandbox_tenant
        sandbox_tenant="$(sanitize_name "$1")"
        tenant_exists_in_registry "$sandbox_tenant" || die "tenant '$sandbox_tenant' not found in registry"
        build_sandbox_image "$sandbox_tenant"
      else
        build_sandbox_image
      fi
      ;;

    configure-vision-external)
      require_root
      local _vision_name="" _vision_url="" _vision_token_file="" _vision_token=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --url)
            [[ $# -ge 2 ]] || die "--url requires an HTTPS origin"
            _vision_url="$2"; shift 2
            ;;
          --token-file)
            [[ $# -ge 2 ]] || die "--token-file requires a path"
            _vision_token_file="$2"; shift 2
            ;;
          -*) die "unknown flag: $1" ;;
          *)
            [[ -z "$_vision_name" ]] || die "unexpected argument: $1"
            _vision_name="$(sanitize_name "$1")"; shift
            ;;
        esac
      done
      [[ -n "$_vision_name" && -n "$_vision_url" && -n "$_vision_token_file" ]] \
        || die "usage: configure-vision-external <name> --url <https-origin> --token-file <path>"
      _vision_token="$(_read_secret_file "$_vision_token_file" "LunarVision token")"
      with_tenant_lifecycle_lock "$_vision_name" \
        configure_tenant_vision_external "$_vision_name" "$_vision_url" "$_vision_token"
      ;;

    configure-vision-local)
      require_root
      local _vision_name=""
      [[ $# -eq 1 ]] || die "usage: configure-vision-local <name>"
      _vision_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_vision_name" \
        configure_tenant_vision_local "$_vision_name"
      ;;

    build-voice-sidecar)
      require_root
      build_voice_sidecar_image
      ;;

    install-voice)
      require_root
      local _voice_name="" _voice_manifest=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --model-manifest)
            [[ $# -ge 2 ]] || die "--model-manifest requires a path"
            _voice_manifest="$2"; shift 2
            ;;
          -*) die "unknown flag: $1" ;;
          *)
            [[ -z "$_voice_name" ]] || die "unexpected argument: $1"
            _voice_name="$(sanitize_name "$1")"; shift
            ;;
        esac
      done
      [[ -n "$_voice_name" ]] \
        || die "usage: install-voice <name> [--model-manifest <path>]"
      with_tenant_lifecycle_lock "$_voice_name" \
        install_tenant_voice "$_voice_name" "$_voice_manifest"
      ;;

    configure-voice)
      require_root
      local _voice_name="" _voice_key="" _voice_value=""
      local -a _voice_settings=()
      [[ $# -gt 0 ]] || die "usage: configure-voice <name> [settings]"
      _voice_name="$(sanitize_name "$1")"; shift
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --enable-vad) _voice_settings+=("VOICE_ENABLE_VAD=true"); shift; continue ;;
          --disable-vad) _voice_settings+=("VOICE_ENABLE_VAD=false"); shift; continue ;;
          --enable-tts) _voice_settings+=("VOICE_ENABLE_TTS=true"); shift; continue ;;
          --disable-tts) _voice_settings+=("VOICE_ENABLE_TTS=false"); shift; continue ;;
          --stt-engine) _voice_key=VOICE_STT_ENGINE ;;
          --stt-model) _voice_key=VOICE_STT_MODEL ;;
          --stt-language) _voice_key=VOICE_STT_LANGUAGE ;;
          --max-audio-secs) _voice_key=VOICE_MAX_AUDIO_SECS ;;
          --max-audio-bytes) _voice_key=VOICE_MAX_AUDIO_BYTES ;;
          --max-text-chars) _voice_key=VOICE_MAX_TEXT_CHARS ;;
          --max-tts-audio-secs) _voice_key=VOICE_MAX_TTS_AUDIO_SECS ;;
          --request-timeout-secs) _voice_key=VOICE_REQUEST_TIMEOUT_SECS ;;
          --max-concurrent-inference) _voice_key=VOICE_MAX_CONCURRENT_INFERENCE ;;
          --rate-limit-per-sec) _voice_key=VOICE_RATE_LIMIT_PER_SEC ;;
          --cache-ttl-secs) _voice_key=VOICE_CACHE_TTL_SECS ;;
          --cache-max-entries) _voice_key=VOICE_CACHE_MAX_ENTRIES ;;
          --tts-default-voice) _voice_key=VOICE_TTS_DEFAULT_VOICE ;;
          --ort-num-threads) _voice_key=ORT_NUM_THREADS ;;
          -*) die "unknown flag: $1" ;;
          *) die "unexpected argument: $1" ;;
        esac
        [[ $# -ge 2 ]] || die "$1 requires a value"
        _voice_value="$2"
        _voice_settings+=("${_voice_key}=${_voice_value}")
        shift 2
      done
      [[ ${#_voice_settings[@]} -gt 0 ]] || die "configure-voice requires a setting"
      with_tenant_lifecycle_lock "$_voice_name" \
        configure_tenant_voice "$_voice_name" "${_voice_settings[@]}"
      ;;

    activate-voice|enable-voice)
      require_root
      [[ $# -eq 1 ]] || die "usage: $command_name <name>"
      _voice_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_voice_name" activate_tenant_voice "$_voice_name"
      ;;

    deactivate-voice|disable-voice)
      require_root
      [[ $# -eq 1 ]] || die "usage: $command_name <name>"
      _voice_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_voice_name" deactivate_tenant_voice "$_voice_name"
      ;;

    remove-voice)
      require_root
      [[ $# -eq 1 ]] || die "usage: remove-voice <name>"
      _voice_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_voice_name" remove_tenant_voice "$_voice_name"
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
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" start_tenant "$_lifecycle_name"
      ;;

    stop-tenant)
      require_root
      [[ -n "${1:-}" ]] || die "usage: stop-tenant <name>"
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" stop_tenant "$_lifecycle_name"
      ;;

    cleanup-legacy-weechat)
      require_root
      [[ $# -eq 1 ]] || die "usage: cleanup-legacy-weechat <name>"
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" cleanup_legacy_weechat "$_lifecycle_name"
      ;;

    restart-weechat-adapter)
      require_root
      [[ $# -eq 1 ]] || die "usage: restart-weechat-adapter <name>"
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" restart_weechat_adapter "$_lifecycle_name"
      ;;

    stop-tenant-for-upgrade)
      require_root
      [[ $# -eq 1 ]] || die "usage: stop-tenant-for-upgrade <name>"
      ports_registry_init
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" stop_tenant_for_upgrade "$_lifecycle_name"
      ;;

    migration-export-boundary)
      require_root
      [[ $# -ge 1 ]] || die "usage: migration-export-boundary <name> [export options]"
      ports_registry_init
      _lifecycle_name="$(sanitize_name "$1")"
      shift
      migration_export_boundary "$_lifecycle_name" "$@"
      ;;

    upgrade-wrapper-boundary)
      require_root
      [[ $# -ge 1 ]] || die "usage: upgrade-wrapper-boundary <name> [upgrade options]"
      ports_registry_init
      _lifecycle_name="$(sanitize_name "$1")"
      shift
      upgrade_wrapper_boundary "$_lifecycle_name" "$@"
      ;;

    validate-upgrade-wrapper-boundary)
      require_root
      [[ $# -eq 1 ]] || die "usage: validate-upgrade-wrapper-boundary <name>"
      ports_registry_require_readonly
      validate_upgrade_wrapper_boundary "$1"
      ;;

    validate-migration-export-boundary)
      require_root
      [[ $# -eq 1 ]] || die "usage: validate-migration-export-boundary <name>"
      ports_registry_require_readonly
      validate_migration_export_boundary "$1"
      ;;

    restart-tenant)
      require_root
      [[ -n "${1:-}" ]] || die "usage: restart-tenant <name>"
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" restart_tenant "$_lifecycle_name"
      ;;

    fix-podman-userns)
      require_root
      [[ -n "${1:-}" ]] || die "usage: fix-podman-userns <name>"
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" repair_tenant_podman_userns "$_lifecycle_name"
      ;;

    stop-writers)
      require_root
      [[ $# -eq 1 ]] || die "usage: stop-writers <name>"
      ports_registry_init
      _writer_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_writer_name" stop_tenant_writers "$_writer_name"
      ;;

    writers-active)
      require_root
      [[ $# -eq 1 ]] || die "usage: writers-active <name>"
      ports_registry_require_readonly
      _writer_name="$(sanitize_name "$1")"
      _writer_rc=0
      with_tenant_lifecycle_lock "$_writer_name" tenant_writers_probe_status "$_writer_name" \
        || _writer_rc=$?
      case "$_writer_rc" in
        0) say "active"; return 0 ;;
        10) say "stopped"; return 1 ;;
        *) say "could not determine tenant writer state" >&2; return 2 ;;
      esac
      ;;

    start-pg)
      require_root
      [[ -n "${1:-}" ]] || die "usage: start-pg <name>"
      # PG-only window for offline population (e.g. hermes_kawarimi loader):
      # daemon stays down while rows are written with nothing racing.
      _pg_only_name="$(sanitize_name "$1")"
      tenant_exists_in_registry "$_pg_only_name" || die "tenant '$1' not found in registry"
      start_tenant_postgres "$_pg_only_name"
      ;;

    stop-pg)
      require_root
      [[ -n "${1:-}" ]] || die "usage: stop-pg <name>"
      _pg_only_name="$(sanitize_name "$1")"
      tenant_exists_in_registry "$_pg_only_name" || die "tenant '$1' not found in registry"
      stop_tenant_postgres "$_pg_only_name"
      ;;

    start-worker|stop-worker|restart-worker|kill-worker)
      require_root
      [[ $# -eq 2 ]] || die "usage: $command_name <name> <pebble|opencode|kimi|opencode2>"
      case "$command_name" in
        start-worker) start_worker "$1" "$2" ;;
        stop-worker) stop_worker "$1" "$2" ;;
        restart-worker) restart_worker "$1" "$2" ;;
        kill-worker) kill_worker "$1" "$2" ;;
      esac
      ;;

    render-units)
      require_root
      [[ $# -eq 1 ]] || die "usage: render-units <name>|--all-proxies"
      if [[ "$1" == --all-proxies ]]; then
        render_all_openrc_proxy_units
      else
        ports_registry_init
        render_tenant_units "$1"
      fi
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

    psql)
      require_root
      [[ -n "${1:-}" ]] || die "usage: psql <name> [psql args...]"
      local psql_tenant="$1"; shift
      tenant_psql "$psql_tenant" "$@"
      ;;

    daemon-pid)
      require_root
      [[ -n "${1:-}" ]] || die "usage: daemon-pid <name>"
      local daemon_pid
      if daemon_pid="$(tenant_daemon_pid "$1")"; then
        say "$daemon_pid"
      else
        die "could not resolve the daemon PID for tenant '$1' (unit not running, PID record missing/malformed, or process not associated with the tenant); treat as uninspected"
      fi
      ;;

    list-tenants|list)
      ports_registry_init
      list_tenants
      ;;

    status)
      [[ -n "${1:-}" ]] || die "usage: status <name>"
      status_tenant "$1"
      ;;

    worker-listeners)
      [[ -n "${1:-}" ]] || die "usage: worker-listeners <name>"
      worker_listeners "$1"
      ;;

    darkirc-contact)
      require_root
      local darkirc_operation="${1:-}"
      if [[ "$darkirc_operation" == validate-migration ]]; then
        shift
        local darkirc_input=false darkirc_unbound=false
        while [[ $# -gt 0 ]]; do
          case "$1" in
            --in) [[ $# -ge 2 && "$2" == - ]] || die "validate-migration accepts stdin only (--in -)"; darkirc_input=true; shift 2 ;;
            --json) shift ;;
            --unbound) darkirc_unbound=true; shift ;;
            *) die "unexpected validate-migration argument: $1" ;;
          esac
        done
        [[ "$darkirc_input" == true ]] || die "validate-migration requires --in -"
        run_darkirc_manifest_validator "$darkirc_unbound"
      else
        case "$darkirc_operation" in
          list|status|doctor|exchanges) ports_registry_require_readonly ;;
          *) ports_registry_init ;;
        esac
        darkirc_contact_command "$darkirc_operation" "${@:2}"
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
      [[ "$health_strict" == true && -n "$health_tenant" ]] \
        || die "darkirc-health requires <tenant> --strict"
      health_tenant="$(sanitize_name "$health_tenant")"
      ports_registry_require_readonly
      if darkirc_health_strict "$health_tenant" 10 "$health_json"; then
        if [[ "$health_json" == true ]]; then
          printf '{"status":"ok","tenant":"%s","irc_connected":true}\n' "$health_tenant"
        fi
      else
        die "strict DarkIRC health failed for tenant '$health_tenant'"
      fi
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
      die "Nanocode worker is retired; configure-nanocode is unavailable"
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

    configure-opencode2)
      require_root
      local name="" oc2_model="" oc2_base_url=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --model)    oc2_model="$2"; shift 2 ;;
          --base-url) oc2_base_url="$2"; shift 2 ;;
          -*)         die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: configure-opencode2 <name> [--model <model>] [--base-url <url>]"
      configure_opencode2 "$name" "$oc2_model" "$oc2_base_url"
      ;;

    configure-kimi)
      require_root
      local name="" kimi_model="" kimi_base_url="" kimi_api_key=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --model)    kimi_model="$2"; shift 2 ;;
          --base-url) kimi_base_url="$2"; shift 2 ;;
          --api-key-file) kimi_api_key="$(_read_secret_file "$2" "Kimi API key")"; shift 2 ;;
          --api-key)  die "--api-key was removed; use --api-key-file" ;;
          -*)         die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: configure-kimi <name> [--model <model>] [--base-url <url>] [--api-key-file <path>]"
      configure_kimi "$name" "$kimi_model" "$kimi_base_url" "$kimi_api_key"
      ;;

    configure-study-mcp)
      require_root
      local name="" study_home="" study_state_home="" study_allow_write="false" study_sync="false" knowledge_sync_mode=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --home) study_home="$2"; shift 2 ;;
          --state-home) study_state_home="$2"; shift 2 ;;
          --allow-write) study_allow_write="true"; shift ;;
          --sync) study_sync="true"; shift ;;
          --enable-knowledge-sync)
            [[ "$knowledge_sync_mode" != "false" ]] || die "knowledge sync enable/disable flags conflict"
            knowledge_sync_mode="true"; shift ;;
          --disable-knowledge-sync)
            [[ "$knowledge_sync_mode" != "true" ]] || die "knowledge sync enable/disable flags conflict"
            knowledge_sync_mode="false"; shift ;;
          -*)     die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] \
        || die "usage: configure-study-mcp <name> [--home <path>] [--sync] [--allow-write] [--state-home <path>] [--enable-knowledge-sync|--disable-knowledge-sync]"
      # No --home: use the shared default checkout and always sync it to the
      # latest upstream first. An explicit --home stays operator-managed unless
      # --sync is also passed (then it must be a clean git checkout).
      if [[ -z "$study_home" ]]; then
        study_home="$STUDY_NOTES_DEFAULT_HOME"
        study_sync="true"
      fi
      if [[ "$study_sync" == "true" ]]; then
        sync_study_notes_checkout "$study_home"
      fi
      configure_study_mcp "$name" "$study_home" "$study_allow_write" "$study_state_home"
      [[ -z "$knowledge_sync_mode" ]] \
        || set_tenant_knowledge_sync "$name" "$knowledge_sync_mode"
      ;;

    refresh-knowledge)
      require_root
      local name="" sync_source="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --sync-source) sync_source="true"; shift ;;
          -*) die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: refresh-knowledge <name> [--sync-source]"
      run_tenant_knowledge_refresh "$name" "$sync_source"
      ;;

    sync-knowledge-sources)
      [[ $# -eq 0 ]] || die "usage: sync-knowledge-sources"
      sync_enabled_study_notes_sources
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
      ensure_ssh_api_token "$name" \
        || die "failed to provision SSH API credential for tenant '$name'"
      provision_tenant_ssh_key "$name" "$ssh_host" "$ssh_user"
      patch_ssh_tool_allowlist "$name"
      say ""
      say "SSH harness configured for tenant '$name' (host=$ssh_host user=$ssh_user)"
      say "Run '$0 restart-tenant $name' to start the daemon and upload the key to the secrets store"
      ;;

    configure-weechat-relay)
      [[ -n "${1:-}" ]] || die "usage: configure-weechat-relay <name>"
      [[ $# -eq 1 ]]   || die "usage: configure-weechat-relay <name> (unexpected extra arguments)"
      require_root
      local name
      name="$(sanitize_name "$1")"
      ports_registry_init
      tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
      configure_weechat_relay "$name"
      ;;

    rotate-relay-password)
      [[ -n "${1:-}" && $# -eq 1 ]] || die "usage: rotate-relay-password <name>"
      require_root
      local name
      name="$(sanitize_name "$1")"
      ports_registry_init
      tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
      with_tenant_lifecycle_lock "$name" rotate_tenant_weechat_relay_password "$name"
      ;;

    disable-weechat|enable-weechat)
      [[ -n "${1:-}" ]] || die "usage: $command_name <name>"
      [[ $# -eq 1 ]]   || die "usage: $command_name <name> (unexpected extra arguments)"
      require_root
      local name
      name="$(sanitize_name "$1")"
      ports_registry_init
      tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
      if [[ "$command_name" == "disable-weechat" ]]; then
        set_tenant_weechat_enabled "$name" false \
          || die "failed to stop/disable WeeChat for tenant '$name'"
        say "WeeChat disabled for tenant '$name': units stopped + boot persistence removed; health reports them skipped (never critical)"
      else
        set_tenant_weechat_enabled "$name" true \
          || die "failed to enable/start WeeChat for tenant '$name'"
        say "WeeChat enabled for tenant '$name': units enabled + started; health reports critical when down"
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
      # The explicit CLI arm dies on failure; migrate_owner_scope itself only
      # returns nonzero so auto-callers (start/build/patch-env) keep their
      # warn-and-continue contract (EQ-7).
      migrate_owner_scope "$name" "$old_scope" \
        || die "owner-scope migration failed for '$name'"
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

    coord-doctor)
      coord_doctor "${1:-}"
      ;;

    setup-cgroups)
      require_root
      ports_registry_init
      setup_cgroups
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
