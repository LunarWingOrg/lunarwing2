#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # constants/state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
#
# 15-worker-egress.sh — part of the lunarwing-mt-admin library.
#
# WI#424 control 1: the external-worker egress allowlist.
#
# Workers may reach only:
#   * the tenant's configured LLM endpoint(s) (the container-reachable form of
#     `{opencode,opencode2,kimi,pebble}_BASE_URL` and `LLM_BASE_URL`),
#   * DNS on the tenant network's gateway (aardvark-dns resolves external names
#     for the container; the container never talks to an upstream resolver),
#   * operator-configured extra `host:port` entries (`configure-worker-egress`),
#   * already-established flows (return traffic).
# Everything else from the worker subnet is rejected, including
# `opentunnel.xyz`, opencode's share service, and GitHub release downloads.
#
# Mechanism (recorded in docs/ops/WORKER-CONTAINERS.md): each tenant gets its
# own podman network `lw-egress-<tenant>` with a registered /24, and an
# nftables forward chain keyed on that source subnet lives in the network
# namespace that owns the bridge — the tenant's rootless netns for rootless
# podman, the host netns for rootful. The chain's policy stays `accept`; only
# packets sourced from the worker subnet are classified, so tenant sidecars on
# other networks are untouched.
#
# Two properties matter more than the rule text:
#   1. Fail closed. `worker_egress_apply` re-reads the chain from a *fresh*
#      netns entry after loading it. A rootless netns that did not survive the
#      load (no container pinning it yet) would otherwise leave the worker
#      running with open egress; the verification dies instead, so the caller
#      refuses to start the worker.
#   2. Host-side truth. The audit renders the live chain through the same
#      path, never from a file the container can write.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly.

WORKER_EGRESS_DIR="${LUNARWING_MT_WORKER_EGRESS_DIR:-/etc/lunarwing/worker-egress}"
WORKER_EGRESS_TABLE="lunarwing_worker_egress"
WORKER_EGRESS_CHAIN="workers"
WORKER_EGRESS_SUBNET_PREFIX="10.89"
WORKER_EGRESS_SUBNET_MIN=10
WORKER_EGRESS_SUBNET_MAX=250
WORKER_EGRESS_LOCK=""
# Addresses a worker can use to reach services on THIS host. The network
# gateway covers rootful podman/netavark (the bridge gateway is a host
# address); rootless podman with pasta publishes the host at 169.254.1.2 and
# puts it in the container's /etc/hosts as `host.containers.internal`
# (verified on this fleet). Both are the same machine, so a loopback endpoint
# is allowed on both. LUNARWING_MT_WORKER_HOST_ADDRS overrides the extra list
# for a different rootless backend (slirp4netns uses 10.0.2.2).
WORKER_EGRESS_HOST_ADDRS="${LUNARWING_MT_WORKER_HOST_ADDRS:-169.254.1.2}"
# Root-owned copy of the load+verify helper the boot paths call. It lives in
# /usr/local/libexec so a tenant-writable path can never shadow it (the guard
# itself runs as the tenant in a Quadlet ExecStartPre).
WORKER_EGRESS_GUARD_SRC="$SCRIPT_DIR/lunarwing-worker-egress-guard.sh"
WORKER_EGRESS_GUARD="/usr/local/libexec/lunarwing-worker-egress-guard"

worker_egress_network_name() {  # <tenant>
  printf 'lw-egress-%s\n' "$1"
}

worker_egress_conf_path() {  # <tenant>
  printf '%s/%s.conf\n' "$WORKER_EGRESS_DIR" "$1"
}

worker_egress_ruleset_path() {  # <tenant>
  printf '%s/%s.nft\n' "$WORKER_EGRESS_DIR" "$1"
}

# The lock file lives next to the allocations so concurrent add-tenant calls
# cannot hand two tenants the same /24.
# The state directory is root-owned in production (mt-admin runs as root) and
# carries no secrets — the ruleset must be readable by the tenant's own units
# because they load it inside the tenant netns. A non-root invocation (the
# harness, a dry run) keeps the invoking user's ownership rather than failing.
_worker_egress_install_dir() {
  [[ ! -L "$WORKER_EGRESS_DIR" ]] || die "unsafe worker egress state directory"
  if [[ "$(id -u)" == "0" ]]; then
    install -d -o root -g root -m 0755 "$WORKER_EGRESS_DIR"
  else
    install -d -m 0755 "$WORKER_EGRESS_DIR"
  fi
}

_worker_egress_own_root() {  # <path>
  [[ "$(id -u)" != "0" ]] || chown root:root -- "$1"
}

_worker_egress_lock() {
  require_cmd flock
  _worker_egress_install_dir
  WORKER_EGRESS_LOCK="$WORKER_EGRESS_DIR/.lock"
  exec 197>>"$WORKER_EGRESS_LOCK"
  flock -x 197
}

_worker_egress_unlock() {
  [[ -n "$WORKER_EGRESS_LOCK" ]] || return 0
  exec 197>&-
  WORKER_EGRESS_LOCK=""
# Addresses a worker can use to reach services on THIS host. The network
# gateway covers rootful podman/netavark (the bridge gateway is a host
# address); rootless podman with pasta publishes the host at 169.254.1.2 and
# puts it in the container's /etc/hosts as `host.containers.internal`
# (verified on this fleet). Both are the same machine, so a loopback endpoint
# is allowed on both. LUNARWING_MT_WORKER_HOST_ADDRS overrides the extra list
# for a different rootless backend (slirp4netns uses 10.0.2.2).
WORKER_EGRESS_HOST_ADDRS="${LUNARWING_MT_WORKER_HOST_ADDRS:-169.254.1.2}"
# Root-owned copy of the load+verify helper the boot paths call. It lives in
# /usr/local/libexec so a tenant-writable path can never shadow it (the guard
# itself runs as the tenant in a Quadlet ExecStartPre).
WORKER_EGRESS_GUARD_SRC="$SCRIPT_DIR/lunarwing-worker-egress-guard.sh"
WORKER_EGRESS_GUARD="/usr/local/libexec/lunarwing-worker-egress-guard"
}

worker_egress_state_get() {  # <tenant> <KEY>
  local name="$1" key="$2" conf line
  [[ "$key" =~ ^[A-Z][A-Z0-9_]*$ ]] || die "invalid worker egress state key"
  conf="$(worker_egress_conf_path "$name")"
  [[ -f "$conf" && ! -L "$conf" ]] || return 0
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ "$line" == "$key="* ]] && printf '%s\n' "${line#*=}"
  done <"$conf"
  # The read loop ends on EOF with status 1; callers use this in $(...)
  # assignments under `set -e`, so an empty/absent key must succeed.
  return 0
}

_worker_egress_subnet_in_use() {  # <subnet> <excluding tenant>
  local subnet="$1" exclude="$2" conf
  for conf in "$WORKER_EGRESS_DIR"/*.conf; do
    [[ -e "$conf" && ! -L "$conf" ]] || continue
    [[ "${conf##*/}" == "$exclude.conf" ]] && continue
    grep -qx "SUBNET=$subnet" "$conf" 2>/dev/null && return 0
  done
  return 1
}

_worker_egress_alloc_subnet() {  # <tenant>
  local name="$1" conf index subnet
  conf="$(worker_egress_conf_path "$name")"
  subnet="$(worker_egress_state_get "$name" SUBNET)"
  if [[ -n "$subnet" ]]; then
    printf '%s\n' "$subnet"
    return 0
  fi
  for ((index = WORKER_EGRESS_SUBNET_MIN; index <= WORKER_EGRESS_SUBNET_MAX; index++)); do
    subnet="${WORKER_EGRESS_SUBNET_PREFIX}.${index}.0/24"
    _worker_egress_subnet_in_use "$subnet" "$name" && continue
    # Append: the operator allowlist may already be recorded for this tenant.
    printf 'SUBNET=%s\n' "$subnet" >>"$conf"
    chmod 0644 "$conf"
    printf '%s\n' "$subnet"
    return 0
  done
  die "no free worker egress subnet in ${WORKER_EGRESS_SUBNET_PREFIX}.${WORKER_EGRESS_SUBNET_MIN}-${WORKER_EGRESS_SUBNET_MAX}"
}

_worker_egress_gateway() {  # <subnet e.g. 10.89.12.0/24>
  local subnet="${1%%/*}" prefix
  prefix="${subnet%.*}"
  printf '%s.1\n' "$prefix"
}

# Print one `address port` pair per allowlisted destination.
#
# Hostnames are resolved on the host (the container's DNS path is allowlisted
# to the gateway, but the rule set is static): every IPv4 answer becomes its
# own entry. `127.0.0.1`/`localhost`/`0.0.0.0` mean "a service on this host"
# and are translated to the tenant network's gateway — the only host address
# reachable from a rootless worker.
_worker_egress_split_target() {  # <target> <require-port: true|false>
  local target="$1" require_port="${2:-false}" scheme="" authority host port
  if [[ "$target" == *"://"* ]]; then
    scheme="${target%%://*}"
    authority="${target#*://}"
  else
    authority="$target"
  fi
  authority="${authority%%/*}"
  [[ "$authority" == *"@"* ]] && authority="${authority##*@}"
  case "$authority" in
    \[*\]:*) host="${authority%]:*}"; host="${host#[}"
              port="${authority##*]:}" ;;
    \[*\])   host="${authority#[}"; host="${host%]}"; port="" ;;
    *:*)      host="${authority%:*}"; port="${authority##*:}" ;;
    *)        host="$authority"; port="" ;;
  esac
  [[ -n "$host" ]] || return 1
  if [[ -z "$port" ]] && [[ -n "$scheme" ]]; then
    # A URL without an explicit port uses the scheme default (HTTP semantics);
    # refusing it would drop a tenant's whole LLM endpoint from the allowlist.
    case "$scheme" in
      http) port=80 ;;
      https) port=443 ;;
      *) return 1 ;;
    esac
  fi
  if [[ -n "$port" ]]; then
    [[ "$port" =~ ^[0-9]+$ ]] && ((port > 0 && port < 65536)) || return 1
  elif [[ "$require_port" == "true" ]]; then
    return 1
  fi
  printf '%s %s\n' "$host" "$port"
}

# Print one `address port` pair for a target.
#
# Hostnames are resolved on the host (the container's DNS path is allowlisted
# to the gateway, but the rule set is static): every IPv4 answer becomes its
# own entry. `127.0.0.1`/`localhost`/`0.0.0.0` mean "a service on this host"
# and are translated to the tenant network's gateway — the only host address
# reachable from a rootless worker. Split and resolution are separate so
# *validation* stays offline (no DNS at configure time); resolution failure at
# render time is fatal, never a silently dropped allowlist entry.
# Every address a worker can use to reach this host: the tenant network
# gateway, plus the rootless host alias(es). Rootful runs add only the gateway.
_worker_egress_host_addrs() {  # <gateway>
  local gateway="$1" address
  printf '%s\n' "$gateway"
  [[ "${CONTAINER_RT:-}" == "podman" && "${MT_ROOTLESS:-}" == "true" ]] || return 0
  for address in $WORKER_EGRESS_HOST_ADDRS; do
    [[ "$address" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "invalid LUNARWING_MT_WORKER_HOST_ADDRS entry '$address'"
    [[ "$address" == "$gateway" ]] || printf '%s\n' "$address"
  done
}

# <url-or-host:port> <gateway> [strict]
#
# `strict` decides what an unresolvable hostname means:
#   * operator entries (strict): die — the operator asked for this destination
#     explicitly, so a typo or a dead DNS server must be loud, not quietly
#     dropped from their allowlist;
#   * tenant-derived LLM endpoints (non-strict): warn and omit. Omitting a
#     target can only NARROW the allowlist (the worker then cannot reach that
#     endpoint and says so), so a transient resolver failure at boot must not
#     take the worker down; the warning keeps it visible.
_worker_egress_resolve_target() {  # <target> <gateway> [strict]
  local target="$1" gateway="$2" strict="${3:-false}" host port answer address split
  split="$(_worker_egress_split_target "$target" true)" \
    || die "invalid worker egress target '$target' (host:port or http(s) URL required)"
  read -r host port <<<"$split"
  case "$host" in
    127.0.0.1|localhost|0.0.0.0|::1)
      local host_addrs
      host_addrs="$(_worker_egress_host_addrs "$gateway")" \
        || die "cannot derive the host addresses for worker egress"
      printf '%s\n' "$host_addrs" | awk -v p="$port" 'NF == 1 { print $1, p }'
      return 0
      ;;
    *[!0-9.]*)
      answer="$(getent ahostsv4 "$host" 2>/dev/null | awk '$2 == "STREAM" { print $1 }' | sort -u || true)"
      if [[ -z "$answer" ]]; then
        if [[ "$strict" == "true" ]]; then
          die "cannot resolve worker egress target '$host'"
        fi
        printf 'worker-egress: WARNING: cannot resolve %s; omitting it from the allowlist (the worker will not reach it)\n' \
          "$host" >&2
        return 0
      fi
      while IFS= read -r address; do
        printf '%s %s\n' "$address" "$port"
      done <<<"$answer"
      ;;
    *)
      printf '%s %s\n' "$host" "$port"
      ;;
  esac
}

# The LLM endpoints the workers themselves were configured with. Read through
# read_tenant_env_value so a tenant-planted symlink cannot steer root's read;
# an unsafe path refuses (and the caller dies) rather than falling back.
_worker_egress_llm_targets() {  # <tenant>
  local name="$1" env_path key value
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  for key in LLM_BASE_URL OPENCODE_BASE_URL OPENCODE2_BASE_URL KIMI_BASE_URL PEBBLE_BASE_URL; do
    value="$(read_tenant_env_value "$name" "$env_path" "$key")" \
      || die "unsafe tenant env path for worker egress ($key)"
    [[ -n "$value" ]] && printf '%s\n' "$value"
  done
  return 0
}

_worker_egress_operator_allow() {  # <tenant>
  local name="$1" entry
  for entry in $(worker_egress_state_get "$name" ALLOW); do
    printf '%s\n' "$entry"
  done
}

# Emit `address port` lines in a stable order, de-duplicated.
#
# Both the source collection and the resolution stay in the CURRENT shell (no
# pipeline, no process substitution around the resolver): a target that cannot
# be resolved must fail the whole ruleset, and a `die` inside a pipeline only
# exits that pipeline element — the ruleset would then be emitted with the
# target silently missing. That is the fail-open shape this control exists to
# avoid, caught by the harness's unresolvable-hostname case.
worker_egress_targets() {  # <tenant>
  local name="$1" subnet gateway target raw
  local -a llm_targets=() operator_targets=()
  local line
  subnet="$(worker_egress_state_get "$name" SUBNET)"
  [[ -n "$subnet" ]] || return 0
  gateway="$(_worker_egress_gateway "$subnet")"
  # Tenant-derived endpoints first (non-strict), then operator entries
  # (strict). Both sources stay in this shell: a `die` inside a pipeline would
  # only exit that element and emit a ruleset with the entry silently missing.
  raw="$(_worker_egress_llm_targets "$name")" || return 1
  while IFS= read -r target; do
    [[ -n "$target" ]] && llm_targets+=("$target")
  done <<<"$raw"
  raw="$(_worker_egress_operator_allow "$name")" || return 1
  while IFS= read -r target; do
    [[ -n "$target" ]] && operator_targets+=("$target")
  done <<<"$raw"
  # Resolve through COMMAND substitution, not process substitution, so a
  # `die` propagates: `< <(...)` would let a strict-target failure exit only
  # that subshell and leave the ruleset with the entry silently missing. The
  # dedupe stays in this shell for the same reason.
  local -A seen=()
  local -a lines=()
  local resolved
  for target in "${llm_targets[@]}"; do
    resolved="$(_worker_egress_resolve_target "$target" "$gateway" false)" || return 1
    while IFS= read -r line; do
      [[ -n "$line" ]] && lines+=("$line")
    done <<<"$resolved"
  done
  for target in "${operator_targets[@]}"; do
    resolved="$(_worker_egress_resolve_target "$target" "$gateway" true)" || return 1
    while IFS= read -r line; do
      [[ -n "$line" ]] && lines+=("$line")
    done <<<"$resolved"
  done
  for line in "${lines[@]}"; do
    [[ -n "${seen[$line]:-}" ]] && continue
    seen[$line]=1
    printf '%s\n' "$line"
  done
}

# Render the complete nftables script. Statements:
#   1. declare the chain (with its hook) so a first load creates it;
#   2. flush it, so a reload replaces the previous rule set;
#   3. re-declare the chain purely to append the current rules.
# The three statements are one `nft -f` file, so the replace is atomic.
worker_egress_ruleset() {  # <tenant>
  local name="$1" subnet gateway dns_extra address port
  subnet="$(worker_egress_state_get "$name" SUBNET)"
  [[ -n "$subnet" ]] || die "worker egress subnet not allocated for '$name'"
  gateway="$(_worker_egress_gateway "$subnet")"
  cat <<EOF
table inet $WORKER_EGRESS_TABLE {
    chain $WORKER_EGRESS_CHAIN {
        type filter hook forward priority filter; policy accept;
    }
}
flush chain inet $WORKER_EGRESS_TABLE $WORKER_EGRESS_CHAIN
table inet $WORKER_EGRESS_TABLE {
    chain $WORKER_EGRESS_CHAIN {
        ip saddr $subnet ct state established,related accept
        ip saddr $subnet ip daddr $gateway udp dport 53 accept
        ip saddr $subnet ip daddr $gateway tcp dport 53 accept
EOF
  local target_lines
  target_lines="$(worker_egress_targets "$name")" \
    || die "failed to resolve the worker egress targets for '$name' (refusing a partial allowlist)"
  while read -r address port; do
    [[ -n "$address" && -n "$port" ]] || continue
    printf '        ip saddr %s ip daddr %s tcp dport %s accept\n' \
      "$subnet" "$address" "$port"
  done <<<"$target_lines"
  cat <<EOF
        ip saddr $subnet counter reject with icmpx type admin-prohibited
    }
}
EOF
}

# Run nft in the netns that owns the tenant's bridge. Rootless podman keeps
# the bridge (and the NAT) in the tenant's rootless netns; rootful uses the
# host netns. `unshare --rootless-netns` is the documented way to enter it.
worker_egress_nft() {  # <tenant> <nft args...>
  local name="$1"; shift
  ensure_container_runtime
  if [[ "$CONTAINER_RT" == "podman" && "$MT_ROOTLESS" == "true" ]]; then
    local uid home
    uid="$(id -u "$name")" || die "cannot resolve uid for tenant '$name'"
    home="$(getent passwd "$name" | cut -d: -f6)"
    ( cd / && exec sudo -u "$name" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
        podman unshare --rootless-netns nft "$@" )
  else
    nft "$@"
  fi
}

# Create the tenant's egress network if it does not exist. Idempotent.
worker_egress_ensure() {  # <tenant>
  local name="$1" network subnet
  network="$(worker_egress_network_name "$name")"
  _worker_egress_install_dir
  _worker_egress_lock
  subnet="$(_worker_egress_alloc_subnet "$name")"
  _worker_egress_unlock
  if ! _ctr "$name" network exists "$network" >/dev/null 2>&1; then
    _ctr "$name" network create \
      --subnet "$subnet" \
      --label "lunarwing.tenant=$name" \
      --label "lunarwing.purpose=worker-egress" \
      "$network" >/dev/null \
      || die "failed to create worker egress network '$network'"
  fi
  printf '%s\n' "$network"
}

# Write the ruleset root-owned and readable by the tenant (the user unit loads
# it inside the tenant's own netns). Atomic replace: a partially written file
# must never be loadable.
_worker_egress_write_ruleset() {  # <tenant>
  local name="$1" ruleset_path staged
  ruleset_path="$(worker_egress_ruleset_path "$name")"
  staged="$(mktemp "${WORKER_EGRESS_DIR}/.ruleset.XXXXXX")"
  worker_egress_ruleset "$name" >"$staged" || { rm -f "$staged"; return 1; }
  chmod 0644 "$staged"
  _worker_egress_own_root "$staged"
  mv -f -- "$staged" "$ruleset_path"
}

# Load the ruleset and prove it is live afterwards. The re-read goes through a
# fresh netns entry on purpose: a rootless netns that did not survive the load
# would report success here while leaving egress open.
worker_egress_apply() {  # <tenant>
  local name="$1" ruleset_path dump
  require_cmd nft
  ruleset_path="$(worker_egress_ruleset_path "$name")"
  _worker_egress_write_ruleset "$name" || die "failed to render worker egress ruleset for '$name'"
  install_worker_egress_guard
  worker_egress_nft "$name" -f "$ruleset_path" \
    || die "failed to load worker egress ruleset for '$name'"
  if ! dump="$(worker_egress_nft "$name" list chain inet "$WORKER_EGRESS_TABLE" "$WORKER_EGRESS_CHAIN" 2>/dev/null)"; then
    die "worker egress filter for '$name' did not survive the load (fail closed: refusing to run the worker unfiltered)"
  fi
  grep -q "reject" <<<"$dump" \
    || die "worker egress filter for '$name' loaded without its drop rule (fail closed)"
  printf '%s\n' "$dump"
}

# Install the root-owned load+verify helper the boot paths invoke. Same shape as
# the other mt-admin libexec helpers: byte-copied, root-owned, 0755.
install_worker_egress_guard() {
  # Only root installs into the system path; a non-root dry run (or the
  # harness) keeps the load+verify behaviour without touching /usr/local.
  [[ "$(id -u)" == "0" ]] || return 0
  [[ -f "$WORKER_EGRESS_GUARD_SRC" ]] \
    || die "worker egress guard script missing: $WORKER_EGRESS_GUARD_SRC"
  install -d -m 0755 /usr/local/libexec
  install -m 0755 -o root -g root "$WORKER_EGRESS_GUARD_SRC" "$WORKER_EGRESS_GUARD"
}

worker_egress_flush() {  # <tenant>
  local name="$1"
  worker_egress_nft "$name" delete table inet "$WORKER_EGRESS_TABLE" 2>/dev/null || true
}

# True when the named container is attached to the tenant's egress network.
worker_egress_container_attached() {  # <tenant> <container>
  local name="$1" container="$2" network
  network="$(worker_egress_network_name "$name")"
  _ctr "$name" inspect \
    --format "{{if index .NetworkSettings.Networks \"$network\"}}attached{{end}}" \
    "$container" 2>/dev/null | grep -qx attached
}

# Read-only audit. Host-side: network config, container attachment, and the
# live chain from the netns itself. Exit 0 only when the filter is provably
# live (usable as a health/CI check); 1 otherwise.
worker_egress_audit() {  # <tenant>
  local name="$1" network subnet container attached=0 missing=0 dump
  name="$(sanitize_name "$1")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  network="$(worker_egress_network_name "$name")"
  subnet="$(worker_egress_state_get "$name" SUBNET)"

  say "worker egress: tenant=$name network=$network subnet=${subnet:-unallocated}"
  if [[ -z "$subnet" ]]; then
    say "  status: not configured (no egress network allocated)"
    return 1
  fi
  if _ctr "$name" network exists "$network" >/dev/null 2>&1; then
    say "  network: present"
  else
    say "  network: MISSING"
    missing=1
  fi

  for container in "lunarwing-pebble-$name" "lunarwing-opencode-$name" \
                   "lunarwing-opencode2-$name" "lunarwing-kimi-$name"; do
    _ctr "$name" inspect "$container" >/dev/null 2>&1 || continue
    attached=1
    if worker_egress_container_attached "$name" "$container"; then
      say "  container $container: attached"
    else
      say "  container $container: NOT ATTACHED (egress is unfiltered)"
      missing=1
    fi
  done
  [[ "$attached" == "1" ]] || say "  containers: none running"

  if dump="$(worker_egress_nft "$name" list chain inet "$WORKER_EGRESS_TABLE" "$WORKER_EGRESS_CHAIN" 2>/dev/null)"; then
    say "  filter: live"
    sed 's/^/    /' <<<"$dump"
  else
    say "  filter: NOT LOADED"
    missing=1
  fi
  return "$missing"
}

# Persist the operator allowlist, then re-render and re-apply.
# `--allow none` clears it. Entries are `host:port` or full URLs.
configure_worker_egress() {  # <tenant> [--allow csv|none]
  local name="$1"; shift
  local allow_csv="" entry
  local -a entries=()
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --allow)
        [[ $# -ge 2 ]] || die "usage: configure-worker-egress <name> [--allow host:port,...|none]"
        allow_csv="$2"; shift 2 ;;
      *) die "usage: configure-worker-egress <name> [--allow host:port,...|none]" ;;
    esac
  done
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  _worker_egress_install_dir
  _worker_egress_lock
  local conf staged
  conf="$(worker_egress_conf_path "$name")"
  staged="$(mktemp "${WORKER_EGRESS_DIR}/.conf.XXXXXX")"
  {
    [[ -n "$(worker_egress_state_get "$name" SUBNET)" ]] \
      && printf 'SUBNET=%s\n' "$(worker_egress_state_get "$name" SUBNET)"
  } >"$staged"
  if [[ "$allow_csv" != "none" && -n "$allow_csv" ]]; then
    IFS=',' read -r -a entries <<<"$allow_csv"
    for entry in "${entries[@]}"; do
      entry="${entry//[[:space:]]/}"
      [[ -n "$entry" ]] || continue
      validate_worker_egress_entry "$entry"
      printf 'ALLOW=%s\n' "$entry" >>"$staged"
    done
  fi
  chmod 0644 "$staged"
  _worker_egress_own_root "$staged"
  mv -f -- "$staged" "$conf"
  _worker_egress_unlock
  worker_egress_ensure "$name" >/dev/null
  worker_egress_apply "$name" >/dev/null
  say "worker egress allowlist updated for $name"
}

# Reject entries that could smuggle nft syntax or a wildcard into the ruleset.
# Syntax only: configuration must not depend on DNS being reachable, and the
# resolution that matters happens when the ruleset is rendered.
validate_worker_egress_entry() {  # <entry>
  local entry="$1"
  # `]` first, `-` last: the only spelling of a bracket class that treats
  # both as literals in a bash ERE (escaped brackets close the class early).
  [[ "$entry" =~ ^[]A-Za-z0-9._:/[-]+$ ]] \
    || die "invalid worker egress entry '$entry'"
  _worker_egress_split_target "$entry" true >/dev/null \
    || die "worker egress entry '$entry' needs host:port"
}

# `_ctr run` argument for the worker container.
worker_egress_launch_args() {  # <tenant>
  local name="$1"
  printf '%s\n' "--network" "$(worker_egress_network_name "$name")"
}

worker_egress_enabled() {  # <tenant>
  [[ -n "$(worker_egress_state_get "$1" SUBNET)" ]]
}

# Called by every worker start path before the container is created/started.
# Fail closed: a worker without a verified egress filter is exactly the state
# WI#357's gate record says must not run (the in-image guards are not a
# boundary), so a missing/unsurvivable filter refuses the start.
prepare_worker_egress() {  # <tenant> <worker>
  local name="$1" worker="$2"
  require_cmd nft
  worker_egress_ensure "$name" >/dev/null
  worker_egress_apply "$name" >/dev/null
  say "worker egress filter enforced for $name ($worker)"
}
