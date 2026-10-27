#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # constants/state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
#
# 15-worker-egress.sh — WI#424 control 1, external-worker egress enforcement.
#
# Each tenant has one IPv4-only, DNS-disabled network. Per-tenant forward and
# input chains classify only that network's bridge; no apply flushes another
# tenant's rules. Host-side resolution produces both the nft allowlist and the
# static /etc/hosts mappings from the SAME answers, so workers need no recursive
# resolver. Every start verifies the tenant's own all-protocol reject rules.

WORKER_EGRESS_DIR="${LUNARWING_MT_WORKER_EGRESS_DIR:-/etc/lunarwing/worker-egress}"
WORKER_EGRESS_TABLE="lunarwing_worker_egress"
WORKER_EGRESS_SUBNET_PREFIX="10.89"
WORKER_EGRESS_SUBNET_MIN=10
WORKER_EGRESS_SUBNET_MAX=250
WORKER_EGRESS_LOCK=""
WORKER_EGRESS_HOST_ADDRS="${LUNARWING_MT_WORKER_HOST_ADDRS:-169.254.1.2}"
WORKER_EGRESS_GUARD_SRC="$SCRIPT_DIR/lunarwing-worker-egress-guard.sh"
WORKER_EGRESS_GUARD="${LUNARWING_MT_WORKER_EGRESS_GUARD:-/usr/local/libexec/lunarwing-worker-egress-guard}"
WORKER_EGRESS_LAUNCH_ARGS=()

_worker_egress_validate_name() {
  [[ "$1" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] \
    || die "invalid tenant name for worker egress: '$1'"
}

worker_egress_network_name() {
  _worker_egress_validate_name "$1"
  printf 'lw-egress-%s\n' "$1"
}

# OQ-4: distinct fixed prefixes keep the two chain families from ever colliding
# (the previous `worker_`/`worker_input_` pair made tenant `x`'s input chain and
# tenant `input-x`'s forward chain the same name).
worker_egress_chain() {
  _worker_egress_validate_name "$1"
  local normalized="${1//-/_}"
  printf 'fwd_%s\n' "$normalized"
}

worker_egress_input_chain() {
  _worker_egress_validate_name "$1"
  local normalized="${1//-/_}"
  printf 'in_%s\n' "$normalized"
}

worker_egress_conf_path() { printf '%s/%s.conf\n' "$WORKER_EGRESS_DIR" "$1"; }
worker_egress_ruleset_path() { printf '%s/%s.nft\n' "$WORKER_EGRESS_DIR" "$1"; }
worker_egress_hosts_path() { printf '%s/%s.hosts\n' "$WORKER_EGRESS_DIR" "$1"; }

_worker_egress_install_dir() {
  [[ ! -L "$WORKER_EGRESS_DIR" ]] || die "unsafe worker egress state directory"
  if [[ "$(id -u)" == "0" ]]; then
    install -d -o root -g root -m 0755 "$WORKER_EGRESS_DIR"
  else
    install -d -m 0755 "$WORKER_EGRESS_DIR"
  fi
}

_worker_egress_own_root() {
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
}

worker_egress_state_get() {
  local name="$1" key="$2" conf line
  [[ "$key" =~ ^[A-Z][A-Z0-9_]*$ ]] || die "invalid worker egress state key"
  conf="$(worker_egress_conf_path "$name")"
  [[ -f "$conf" && ! -L "$conf" ]] || return 0
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ "$line" == "$key="* ]] && printf '%s\n' "${line#*=}"
  done <"$conf"
  return 0
}

_worker_egress_subnet_in_use() {
  local subnet="$1" exclude="$2" conf
  for conf in "$WORKER_EGRESS_DIR"/*.conf; do
    [[ -e "$conf" && ! -L "$conf" ]] || continue
    [[ "${conf##*/}" == "$exclude.conf" ]] && continue
    grep -qx "SUBNET=$subnet" "$conf" 2>/dev/null && return 0
  done
  return 1
}

_worker_egress_alloc_subnet() {
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
    printf 'SUBNET=%s\n' "$subnet" >>"$conf"
    chmod 0644 "$conf"
    _worker_egress_own_root "$conf"
    printf '%s\n' "$subnet"
    return 0
  done
  die "no free worker egress subnet in ${WORKER_EGRESS_SUBNET_PREFIX}.${WORKER_EGRESS_SUBNET_MIN}-${WORKER_EGRESS_SUBNET_MAX}"
}

_worker_egress_gateway() {
  local subnet="${1%%/*}" prefix
  prefix="${subnet%.*}"
  printf '%s.1\n' "$prefix"
}

_worker_egress_bridge_for_subnet() {
  [[ "$1" =~ ^[0-9]+\.[0-9]+\.([0-9]+)\.0/24$ ]] \
    || die "invalid worker egress subnet '$1'"
  ((BASH_REMATCH[1] >= WORKER_EGRESS_SUBNET_MIN && BASH_REMATCH[1] <= WORKER_EGRESS_SUBNET_MAX)) \
    || die "worker egress subnet index is out of range: '$1'"
  printf 'lweg%s\n' "${BASH_REMATCH[1]}"
}

_worker_egress_valid_ipv4() {
  local -a octets=()
  IFS='.' read -r -a octets <<<"$1"
  ((${#octets[@]} == 4)) || return 1
  local octet
  for octet in "${octets[@]}"; do
    [[ "$octet" =~ ^[0-9]{1,3}$ ]] || return 1
    ((10#$octet <= 255)) || return 1
  done
}

_worker_egress_split_target() {
  local target="$1" require_port="${2:-false}" scheme="" authority host port
  if [[ "$target" == *"://"* ]]; then
    scheme="${target%%://*}"
    authority="${target#*://}"
    [[ "$scheme" == "http" || "$scheme" == "https" ]] || return 1
  else
    authority="$target"
  fi
  authority="${authority%%/*}"
  [[ "$authority" == *"@"* ]] && authority="${authority##*@}"
  case "$authority" in
    \[*\]:*) host="${authority%]:*}"; host="${host#[}"; port="${authority##*]:}" ;;
    \[*\]) host="${authority#[}"; host="${host%]}"; port="" ;;
    *:*) host="${authority%:*}"; port="${authority##*:}" ;;
    *) host="$authority"; port="" ;;
  esac
  [[ -n "$host" ]] || return 1
  if [[ "$host" == *:* ]]; then
    [[ "$host" =~ ^[0-9A-Fa-f:]+$ ]] || return 1
  else
    [[ "$host" =~ ^[A-Za-z0-9._-]+$ ]] || return 1
  fi
  if [[ -z "$port" && -n "$scheme" ]]; then
    [[ "$scheme" == "http" ]] && port=80 || port=443
  fi
  if [[ -n "$port" ]]; then
    [[ "$port" =~ ^[0-9]+$ ]] && ((port > 0 && port < 65536)) || return 1
  elif [[ "$require_port" == "true" ]]; then
    return 1
  fi
  printf '%s %s\n' "$host" "$port"
}

_worker_egress_host_addrs() {
  local gateway="$1" address
  if [[ "${CONTAINER_RT:-}" == "podman" && "${MT_ROOTLESS:-}" == "true" ]]; then
    for address in $WORKER_EGRESS_HOST_ADDRS; do
      _worker_egress_valid_ipv4 "$address" \
        || die "invalid LUNARWING_MT_WORKER_HOST_ADDRS entry '$address'"
      printf '%s\n' "$address"
    done
    return 0
  fi
  printf '%s\n' "$gateway"
}

_worker_egress_resolution_failed() {
  local target="$1" strict="$2"
  if [[ "$strict" == "true" ]]; then
    die "cannot resolve worker egress target '$target' to IPv4"
  fi
  printf 'worker-egress: WARNING: cannot resolve %s to IPv4; omitting it (the worker will not reach it)\n' \
    "$target" >&2
}

_worker_egress_resolve_target() {
  local target="$1" gateway="$2" strict="${3:-false}" host port answer address split
  split="$(_worker_egress_split_target "$target" true)" \
    || die "invalid worker egress target '$target' (host:port or http(s) URL required)"
  read -r host port <<<"$split"
  case "$host" in
    127.0.0.1|localhost|0.0.0.0|::1)
      local host_addrs
      host_addrs="$(_worker_egress_host_addrs "$gateway")" \
        || die "cannot derive the host addresses for worker egress"
      while IFS= read -r address; do
        printf '%s %s %s\n' "$host" "$address" "$port"
      done <<<"$host_addrs"
      return 0
      ;;
  esac
  if [[ "$host" == *:* ]]; then
    _worker_egress_resolution_failed "$target" "$strict"
    return 0
  fi
  if _worker_egress_valid_ipv4 "$host"; then
    printf '%s %s %s\n' "$host" "$host" "$port"
    return 0
  fi
  answer="$(getent ahostsv4 "$host" 2>/dev/null | awk '$2 == "STREAM" { print $1 }' | sort -u || true)"
  if [[ -z "$answer" ]]; then
    _worker_egress_resolution_failed "$target" "$strict"
    return 0
  fi
  while IFS= read -r address; do
    _worker_egress_valid_ipv4 "$address" || continue
    printf '%s %s %s\n' "$host" "$address" "$port"
  done <<<"$answer"
}

_worker_egress_llm_targets() {
  local name="$1" env_path key value
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  for key in LLM_BASE_URL OPENCODE_BASE_URL OPENCODE2_BASE_URL KIMI_BASE_URL PEBBLE_BASE_URL; do
    value="$(read_tenant_env_value "$name" "$env_path" "$key")" \
      || die "unsafe tenant env path for worker egress ($key)"
    [[ -n "$value" ]] && printf '%s\n' "$value"
  done
  return 0
}

_worker_egress_operator_allow() {
  local name="$1" entry
  for entry in $(worker_egress_state_get "$name" ALLOW); do
    printf '%s\n' "$entry"
  done
}

worker_egress_target_records() {
  local name="$1" subnet gateway target raw line resolved
  local -a llm_targets=() operator_targets=() lines=()
  local -A seen=()
  subnet="$(worker_egress_state_get "$name" SUBNET)"
  [[ -n "$subnet" ]] || return 0
  gateway="$(_worker_egress_gateway "$subnet")"
  raw="$(_worker_egress_llm_targets "$name")" || return 1
  while IFS= read -r target; do
    [[ -n "$target" ]] && llm_targets+=("$target")
  done <<<"$raw"
  raw="$(_worker_egress_operator_allow "$name")" || return 1
  while IFS= read -r target; do
    [[ -n "$target" ]] && operator_targets+=("$target")
  done <<<"$raw"
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

worker_egress_targets() {
  local name="$1" records host address port
  local -A seen=()
  records="$(worker_egress_target_records "$name")" || return 1
  while read -r host address port; do
    [[ -n "$address" && -n "$port" ]] || continue
    [[ -n "${seen["$address $port"]:-}" ]] && continue
    seen["$address $port"]=1
    printf '%s %s\n' "$address" "$port"
  done <<<"$records"
}

_worker_egress_static_hosts() {
  local records="$1" host address port
  local -A seen=()
  while read -r host address port; do
    [[ -n "$host" && -n "$address" && -n "$port" ]] || continue
    case "$host" in 127.0.0.1|0.0.0.0|::1) continue ;; esac
    [[ "$host" == *:* ]] && continue
    _worker_egress_valid_ipv4 "$host" && continue
    [[ -n "${seen["$host:$address"]:-}" ]] && continue
    seen["$host:$address"]=1
    printf '%s:%s\n' "$host" "$address"
  done <<<"$records"
}

_worker_egress_is_host_address() {
  local address="$1" gateway="$2" candidate
  [[ "$address" == "$gateway" ]] && return 0
  [[ "${CONTAINER_RT:-}" == "podman" && "${MT_ROOTLESS:-}" == "true" ]] || return 1
  for candidate in $WORKER_EGRESS_HOST_ADDRS; do
    [[ "$address" == "$candidate" ]] && return 0
  done
  return 1
}

_worker_egress_forward_rules() {
  local subnet="$1" bridge="$2" records="$3" host address port
  cat <<EOF
        iifname "$bridge" meta nfproto ipv6 counter reject with icmpx type admin-prohibited
        iifname "$bridge" ip saddr != $subnet counter reject with icmpx type admin-prohibited
        iifname "$bridge" ip saddr $subnet ct state established,related accept
EOF
  while read -r host address port; do
    [[ -n "$address" && -n "$port" ]] || continue
    printf '        iifname "%s" ip saddr %s ip daddr %s tcp dport %s accept\n' \
      "$bridge" "$subnet" "$address" "$port"
  done <<<"$records"
  printf '        iifname "%s" ip saddr %s counter reject with icmpx type admin-prohibited\n' \
    "$bridge" "$subnet"
}

_worker_egress_input_rules() {
  local subnet="$1" bridge="$2" records="$3" gateway="$4" host address port
  cat <<EOF
        iifname "$bridge" meta nfproto ipv6 counter reject with icmpx type admin-prohibited
        iifname "$bridge" ip saddr != $subnet counter reject with icmpx type admin-prohibited
        iifname "$bridge" ip saddr $subnet ct state established,related accept
EOF
  while read -r host address port; do
    [[ -n "$address" && -n "$port" ]] || continue
    _worker_egress_is_host_address "$address" "$gateway" || continue
    printf '        iifname "%s" ip saddr %s ip daddr %s tcp dport %s accept\n' \
      "$bridge" "$subnet" "$address" "$port"
  done <<<"$records"
  printf '        iifname "%s" ip saddr %s counter reject with icmpx type admin-prohibited\n' \
    "$bridge" "$subnet"
}

_worker_egress_ruleset_render() {
  local name="$1" records="$2" subnet gateway bridge chain input_chain
  subnet="$(worker_egress_state_get "$name" SUBNET)"
  [[ -n "$subnet" ]] || die "worker egress subnet not allocated for '$name'"
  gateway="$(_worker_egress_gateway "$subnet")"
  bridge="$(_worker_egress_bridge_for_subnet "$subnet")"
  chain="$(worker_egress_chain "$name")"
  input_chain="$(worker_egress_input_chain "$name")"
  cat <<EOF
table inet $WORKER_EGRESS_TABLE {
    chain $chain {
        type filter hook forward priority filter; policy accept;
    }
    chain $input_chain {
        type filter hook input priority filter; policy accept;
    }
}
flush chain inet $WORKER_EGRESS_TABLE $chain
flush chain inet $WORKER_EGRESS_TABLE $input_chain
table inet $WORKER_EGRESS_TABLE {
    chain $chain {
$(_worker_egress_forward_rules "$subnet" "$bridge" "$records")
    }
    chain $input_chain {
$(_worker_egress_input_rules "$subnet" "$bridge" "$records" "$gateway")
    }
}
EOF
}

worker_egress_ruleset() {
  local name="$1" records
  records="$(worker_egress_target_records "$name")" \
    || die "failed to resolve the worker egress targets for '$name' (refusing a partial allowlist)"
  _worker_egress_ruleset_render "$name" "$records"
}

worker_egress_nft() {
  local name="$1"; shift
  ensure_container_runtime
  if [[ "$CONTAINER_RT" == "podman" && "$MT_ROOTLESS" == "true" ]]; then
    local uid home
    uid="$(id -u "$name")" || die "cannot resolve uid for tenant '$name'"
    home="$(getent passwd "$name" | cut -d: -f6)"
    (cd / && exec sudo -u "$name" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
      podman unshare --rootless-netns nft "$@")
  else
    nft "$@"
  fi
}

worker_egress_verify_dump() {
  local name="$1" dump="$2" subnet
  subnet="$(worker_egress_state_get "$name" SUBNET)"
  [[ -n "$subnet" ]] || return 1
  awk -v subnet="$subnet" '
    $1 == "iifname" && $3 == "ip" && $4 == "saddr" && $5 == subnet {
      if ($6 == "counter" && $7 == "reject" && $8 == "with" && $9 == "icmpx" && (($10 == "type" && $11 == "admin-prohibited") || ($10 == "admin-prohibited"))) found = 1
      if ($6 == "counter" && $7 == "packets" && $11 == "reject" && $12 == "with" && $13 == "icmpx" && (($14 == "type" && $15 == "admin-prohibited") || ($14 == "admin-prohibited"))) found = 1
    }
    END { exit(found ? 0 : 1) }
  ' <<<"$dump"
}

worker_egress_verify() {
  local name="$1" chain dump output=""
  for chain in "$(worker_egress_chain "$name")" "$(worker_egress_input_chain "$name")"; do
    dump="$(worker_egress_nft "$name" list chain inet "$WORKER_EGRESS_TABLE" "$chain" 2>/dev/null)" \
      || return 1
    worker_egress_verify_dump "$name" "$dump" || return 1
    output+="$dump"$'\n'
  done
  printf '%s' "$output"
}

worker_egress_network_exists() {
  local name="$1" network="$2"
  ensure_container_runtime
  if [[ "$CONTAINER_RT" == "podman" ]]; then
    _ctr "$name" network exists "$network" >/dev/null 2>&1
  else
    _ctr "$name" network inspect "$network" >/dev/null 2>&1
  fi
}

worker_egress_network_details() {
  local name="$1" network="$2" json
  json="$(_ctr "$name" network inspect "$network" 2>/dev/null)" || return 1
  python3 -I -c '
import json, sys
runtime = sys.argv[1]
doc = json.load(sys.stdin)
net = doc[0] if isinstance(doc, list) else doc
def ci(obj, key, default=None):
    for candidate, value in obj.items():
        if candidate.lower() == key.lower():
            return value
    return default
subnet = gateway = ""
for item in ci(net, "subnets", []) or []:
    if isinstance(item, dict):
        subnet = subnet or ci(item, "subnet", "")
        gateway = gateway or ci(item, "gateway", "")
if not subnet:
    ipam = ci(net, "ipam", {}) or {}
    for item in ci(ipam, "config", []) or []:
        subnet = subnet or ci(item, "subnet", "")
        gateway = gateway or ci(item, "gateway", "")
options = ci(net, "options", {}) or {}
interface = ci(net, "network_interface", "") or ci(options, "com.docker.network.bridge.name", "")
dns_disabled = ci(net, "dns_enabled", None) is False if runtime == "podman" else True
print(subnet)
print(interface)
print("disabled" if dns_disabled else "enabled")
print(gateway)
' "$CONTAINER_RT" <<<"$json"
}

worker_egress_ensure() {
  local name="$1" network subnet bridge details
  local -a fields=()
  network="$(worker_egress_network_name "$name")"
  _worker_egress_install_dir
  _worker_egress_lock
  subnet="$(_worker_egress_alloc_subnet "$name")"
  _worker_egress_unlock
  bridge="$(_worker_egress_bridge_for_subnet "$subnet")"
  ensure_container_runtime
  if worker_egress_network_exists "$name" "$network"; then
    details="$(worker_egress_network_details "$name" "$network")" \
      || die "cannot inspect worker egress network '$network'"
    mapfile -t fields <<<"$details"
    ((${#fields[@]} == 4)) || die "invalid worker egress network metadata for '$network'"
    [[ "${fields[0]}" == "$subnet" ]] \
      || die "worker egress network '$network' has subnet '${fields[0]}', expected '$subnet'; recreate it"
    [[ "${fields[1]}" == "$bridge" ]] \
      || die "worker egress network '$network' has bridge '${fields[1]}', expected '$bridge'; recreate it"
    [[ "${fields[2]}" == "disabled" ]] \
      || die "worker egress network '$network' still has recursive DNS enabled; recreate it"
    [[ "${fields[3]}" == "$(_worker_egress_gateway "$subnet")" ]] \
      || die "worker egress network '$network' has gateway '${fields[3]}', expected '$(_worker_egress_gateway "$subnet")'; recreate it"
  else
    if [[ "$CONTAINER_RT" == "podman" ]]; then
      _ctr "$name" network create --subnet "$subnet" --interface-name "$bridge" \
        --disable-dns --label "lunarwing.tenant=$name" \
        --label "lunarwing.purpose=worker-egress" "$network" >/dev/null \
        || die "failed to create worker egress network '$network'"
    else
      _ctr "$name" network create --subnet "$subnet" \
        --opt "com.docker.network.bridge.name=$bridge" \
        --label "lunarwing.tenant=$name" \
        --label "lunarwing.purpose=worker-egress" "$network" >/dev/null \
        || die "failed to create worker egress network '$network'"
    fi
  fi
  printf '%s\n' "$network"
}

_worker_egress_write_ruleset() {
  local name="$1" ruleset_path hosts_path staged_nft staged_hosts records rendered hosts
  ruleset_path="$(worker_egress_ruleset_path "$name")"
  hosts_path="$(worker_egress_hosts_path "$name")"
  staged_nft="$(mktemp "${WORKER_EGRESS_DIR}/.ruleset.XXXXXX")"
  staged_hosts="$(mktemp "${WORKER_EGRESS_DIR}/.hosts.XXXXXX")"
  if ! records="$(worker_egress_target_records "$name")" \
      || ! rendered="$(_worker_egress_ruleset_render "$name" "$records")" \
      || ! hosts="$(_worker_egress_static_hosts "$records")"; then
    rm -f -- "$staged_nft" "$staged_hosts" "$ruleset_path" "$hosts_path"
    return 1
  fi
  printf '%s\n' "$rendered" >"$staged_nft"
  printf '%s\n' "$hosts" >"$staged_hosts"
  chmod 0644 "$staged_nft" "$staged_hosts"
  _worker_egress_own_root "$staged_nft"
  _worker_egress_own_root "$staged_hosts"
  mv -f -- "$staged_nft" "$ruleset_path"
  mv -f -- "$staged_hosts" "$hosts_path"
}

install_worker_egress_guard() {
  [[ "$(id -u)" == "0" ]] || return 0
  [[ -f "$WORKER_EGRESS_GUARD_SRC" && ! -L "$WORKER_EGRESS_GUARD_SRC" ]] \
    || die "worker egress guard script missing: $WORKER_EGRESS_GUARD_SRC"
  install -d -o root -g root -m 0755 /usr/local/libexec
  install -m 0755 -o root -g root "$WORKER_EGRESS_GUARD_SRC" "$WORKER_EGRESS_GUARD"
}

worker_egress_apply() {
  local name="$1" ruleset_path verified
  require_cmd nft
  ruleset_path="$(worker_egress_ruleset_path "$name")"
  _worker_egress_write_ruleset "$name" \
    || die "failed to render worker egress ruleset for '$name'; previous boot state removed"
  install_worker_egress_guard
  worker_egress_nft "$name" -f "$ruleset_path" \
    || die "failed to load worker egress ruleset for '$name'"
  verified="$(worker_egress_verify "$name")" \
    || die "worker egress filter for '$name' did not survive the load with both exact reject rules (fail closed)"
  printf '%s\n' "$verified"
}

worker_egress_flush() {
  local name="$1" chain
  for chain in "$(worker_egress_chain "$name")" "$(worker_egress_input_chain "$name")"; do
    worker_egress_nft "$name" flush chain inet "$WORKER_EGRESS_TABLE" "$chain" >/dev/null 2>&1 || true
    worker_egress_nft "$name" delete chain inet "$WORKER_EGRESS_TABLE" "$chain" >/dev/null 2>&1 || true
  done
}

# Tenant removal must not leave the egress network, the rendered state or the
# live chains behind for a later tenant of the same name to inherit (OQ-6).
# Every step is best-effort: the tenant account may already be gone.
worker_egress_teardown() {
  local name="$1" network conf ruleset hosts
  _worker_egress_validate_name "$name"
  worker_egress_flush "$name" || true
  network="$(worker_egress_network_name "$name")"
  _ctr "$name" network rm "$network" >/dev/null 2>&1 || true
  _worker_egress_lock
  conf="$(worker_egress_conf_path "$name")"
  ruleset="$(worker_egress_ruleset_path "$name")"
  hosts="$(worker_egress_hosts_path "$name")"
  rm -f -- "$conf" "$ruleset" "$hosts"
  _worker_egress_unlock
}

worker_egress_host_mappings() {
  local name="$1" hosts_path mapping address
  hosts_path="$(worker_egress_hosts_path "$name")"
  [[ -f "$hosts_path" && ! -L "$hosts_path" ]] \
    || die "worker egress static hosts missing for '$name'; run worker-egress-apply"
  while IFS= read -r mapping || [[ -n "$mapping" ]]; do
    [[ -n "$mapping" ]] || continue
    address="${mapping##*:}"
    if [[ ! "$mapping" =~ ^[A-Za-z0-9._-]+:[0-9.]+$ ]] \
        || ! _worker_egress_valid_ipv4 "$address"; then
      die "invalid worker egress static host mapping '$mapping'"
    fi
    printf '%s\n' "$mapping"
  done <"$hosts_path"
}

worker_egress_container_attached() {
  local name="$1" container="$2" network networks dns hosts mappings mapping
  network="$(worker_egress_network_name "$name")"
  # shellcheck disable=SC2016  # $name is a Go-template key, not a shell expansion.
  networks="$(_ctr "$name" inspect --format '{{range $name, $_ := .NetworkSettings.Networks}}{{printf "%s\n" $name}}{{end}}' \
    "$container" 2>/dev/null)" || return 1
  [[ "$networks" == "$network" ]] || return 1
  dns="$(_ctr "$name" inspect --format '{{range .HostConfig.Dns}}{{printf "%s\n" .}}{{end}}' \
    "$container" 2>/dev/null)" || return 1
  [[ "$dns" == "127.0.0.1" ]] || return 1
  hosts="$(_ctr "$name" inspect --format '{{range .HostConfig.ExtraHosts}}{{printf "%s\n" .}}{{end}}' \
    "$container" 2>/dev/null)" || return 1
  mappings="$(worker_egress_host_mappings "$name")" || return 1
  while IFS= read -r mapping; do
    [[ -n "$mapping" ]] || continue
    grep -Fqx -- "$mapping" <<<"$hosts" || return 1
  done <<<"$mappings"
}

worker_egress_reconcile_container() {
  local name="$1" container="$2"
  _ctr "$name" inspect "$container" >/dev/null 2>&1 || return 0
  worker_egress_container_attached "$name" "$container" && return 0
  say "worker container $container has a stale egress network/DNS/hosts contract; force-recreating"
  _ctr "$name" rm -f "$container" >/dev/null 2>&1 \
    || die "could not remove stale worker container $container (refusing unfiltered start)"
}

worker_egress_verify_after_start() {
  local name="$1" container="$2"
  if ! worker_egress_container_attached "$name" "$container"; then
    _ctr "$name" stop "$container" >/dev/null 2>&1 || true
    die "worker container $container is not exclusively on its egress network with pinned DNS/hosts; stopped it"
  fi
  worker_egress_verify "$name" >/dev/null \
    || { _ctr "$name" stop "$container" >/dev/null 2>&1 || true; \
         die "worker egress filter vanished after $container started; stopped it"; }
}

worker_egress_audit() {
  local name="$1" network subnet container attached=0 missing=0 dump details
  name="$(sanitize_name "$1")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  network="$(worker_egress_network_name "$name")"
  subnet="$(worker_egress_state_get "$name" SUBNET)"
  say "worker egress: tenant=$name network=$network subnet=${subnet:-unallocated}"
  if [[ -z "$subnet" ]]; then
    say "  status: not configured (no egress network allocated)"
    return 1
  fi
  if worker_egress_network_exists "$name" "$network"; then
    say "  network: present"
    details="$(worker_egress_network_details "$name" "$network")" || missing=1
    if [[ -n "$details" ]]; then
      say "  network contract: $details"
      [[ "$details" == "$subnet"$'\n'"$(_worker_egress_bridge_for_subnet "$subnet")"$'\n'disabled$'\n'"$(_worker_egress_gateway "$subnet")" ]] \
        || missing=1
    fi
  else
    say "  network: MISSING"
    missing=1
  fi
  for container in "lunarwing-pebble-$name" "lunarwing-opencode-$name" \
                   "lunarwing-opencode2-$name" "lunarwing-kimi-$name"; do
    _ctr "$name" inspect "$container" >/dev/null 2>&1 || continue
    attached=1
    if worker_egress_container_attached "$name" "$container"; then
      say "  container $container: attached (exclusive, DNS pinned, static hosts current)"
    else
      say "  container $container: STALE EGRESS CONTRACT (unfiltered or DNS-capable)"
      missing=1
    fi
  done
  [[ "$attached" == "1" ]] || say "  containers: none present"
  if dump="$(worker_egress_verify "$name")"; then
    say "  filter: live (tenant-specific forward + input reject rules verified)"
    awk '{print "    " $0}' <<<"$dump"
  else
    say "  filter: NOT LOADED or tenant reject missing"
    missing=1
  fi
  return "$missing"
}

configure_worker_egress() {
  local name="$1"; shift
  local allow_csv="" entry conf staged
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
  say "worker egress allowlist updated for $name; run restart-tenant $name to refresh static hosts in existing workers"
}

validate_worker_egress_entry() {
  local entry="$1" split host port
  [[ "$entry" =~ ^[]A-Za-z0-9._:/[-]+$ ]] \
    || die "invalid worker egress entry '$entry'"
  split="$(_worker_egress_split_target "$entry" true)" \
    || die "worker egress entry '$entry' needs host:port"
  read -r host port <<<"$split"
  [[ "$host" != *:* || "$host" == "::1" ]] \
    || die "worker egress entry '$entry' uses IPv6; this control is IPv4-only"
}

worker_egress_launch_args() {
  local name="$1" mappings mapping
  mappings="$(worker_egress_host_mappings "$name")" || return 1
  printf '%s\n' "--network" "$(worker_egress_network_name "$name")" "--dns" "127.0.0.1"
  while IFS= read -r mapping; do
    [[ -n "$mapping" ]] && printf '%s\n' "--add-host" "$mapping"
  done <<<"$mappings"
}

worker_egress_enabled() {
  [[ -n "$(worker_egress_state_get "$1" SUBNET)" ]]
}

prepare_worker_egress() {
  local name="$1" worker="$2" launch_text
  require_cmd nft
  worker_egress_ensure "$name" >/dev/null
  worker_egress_apply "$name" >/dev/null
  launch_text="$(worker_egress_launch_args "$name")" \
    || die "cannot render worker egress launch arguments for '$name'"
  mapfile -t WORKER_EGRESS_LAUNCH_ARGS <<<"$launch_text"
  say "worker egress filter enforced for $name ($worker)"
}
