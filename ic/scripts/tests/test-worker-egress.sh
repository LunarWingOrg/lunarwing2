#!/usr/bin/env bash
# WI#424 control 1 — hermetic worker egress boundary harness.
# shellcheck disable=SC1091,SC2016,SC2034,SC2329  # generated sources, literal unit pins, indirect test doubles
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HARNESS_REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

failures=0
fixture="$(mktemp -d)"
trap 'command rm -rf "$fixture"' EXIT
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1" >&2; failures=$((failures + 1)); }
assert_eq() {
  if [[ "$1" == "$2" ]]; then pass "$3"; else fail "$3 (got '$1', want '$2')"; fi
}
assert_contains() {
  if [[ "$1" == *"$2"* ]]; then pass "$3"; else fail "$3 (missing: $2)"; fi
}
assert_absent() {
  if [[ "$1" != *"$2"* ]]; then pass "$3"; else fail "$3 (unexpected: $2)"; fi
}
assert_rc_fail() {
  if [[ "${1:-}" == "env" ]]; then
    shift
    local -a assignments=()
    while [[ $# -gt 0 && "$1" == *=* ]]; do
      assignments+=("$1")
      shift
    done
    if (export "${assignments[@]}"; "$@") >/dev/null 2>&1; then
      fail "expected failure: env ${assignments[*]} $*"
    else
      pass "expected failure: env ${assignments[*]} $*"
    fi
  elif ("$@") >/dev/null 2>&1; then
    fail "expected failure: $*"
  else
    pass "expected failure: $*"
  fi
}

WORKER_EGRESS_DIR="$fixture/egress"
WORKER_EGRESS_GUARD="$fixture/installed-egress-guard"
mkdir -p "$WORKER_EGRESS_DIR"
CTR_LOG="$fixture/ctr.log"
GUARD_LOG="$fixture/guard.log"
: >"$CTR_LOG"
: >"$GUARD_LOG"

require_cmd() { :; }
ensure_container_runtime() { :; }
ensure_babysitter_helper() { :; }
tenant_exists_in_registry() { return 0; }
CONTAINER_RT=podman
MT_ROOTLESS=true

LLM_TARGETS=("http://127.0.0.1:3000/v1" "https://203.0.113.10:8443/v1" "https://git.example.org/v1")
OPERATOR_ALLOW=("https://203.0.113.20:443/repo.git")
_worker_egress_llm_targets() { printf '%s\n' "${LLM_TARGETS[@]}"; }
_worker_egress_operator_allow() { printf '%s\n' "${OPERATOR_ALLOW[@]}"; }
getent() {
  if [[ "${1:-}" == "ahostsv4" ]]; then
    case "${2:-}" in
      git.example.org) printf '203.0.113.40 STREAM git.example.org\n' ;;
      *) return 1 ;;
    esac
    return 0
  fi
  command getent "$@"
}

NFT_LOAD_RC=0
NFT_LIST_RC=0
NFT_LIST_OUT=""
NFT_RULESET_FILE=""
NFT_CALLS=()
NFT_LOG="$fixture/nft.log"
: >"$NFT_LOG"
worker_egress_nft() {
  local name="$1"; shift
  NFT_CALLS+=("$name $*")
  printf '%s\n' "$name $*" >>"$NFT_LOG"
  if [[ "${1:-}" == "-f" ]]; then
    NFT_RULESET_FILE="$2"
    return "$NFT_LOAD_RC"
  fi
  if [[ "${1:-}" == "list" ]]; then
    [[ "$NFT_LIST_RC" == "0" ]] || return 1
    if [[ -n "$NFT_LIST_OUT" ]]; then
      printf '%s\n' "$NFT_LIST_OUT"
      return 0
    fi
    local chain="${!#}"
    awk -v chain="$chain" '
      index($0, "chain " chain " {") { inside = 1; print; next }
      inside && /^    }/ { inside = 0; print; next }
      inside && /counter reject with icmpx type admin-prohibited/ { print }
    ' "$NFT_RULESET_FILE"
    return 0
  fi
  return 0
}

CTR_NETWORK_EXISTS=false
CTR_CREATE_RC=0
CTR_CONTAINER_EXISTS=true
CTR_SUBNET=""
CTR_DNS_ENABLED=false
CTR_NETWORKS="lw-egress-alpha"
CTR_DNS="127.0.0.1"
CTR_EXTRA_HOSTS="git.example.org:203.0.113.40"
CTR_CALLS=()
network_json() {
  local subnet="${CTR_SUBNET:-$(worker_egress_state_get alpha SUBNET)}"
  local bridge
  # CTR_BRIDGE lets a fixture present a network whose bridge differs from the
  # derived one (OQ-3: the bridge check is what keeps every rule meaningful —
  # all rules key on iifname, so a wrong bridge means no filtering at all).
  bridge="${CTR_BRIDGE:-$(_worker_egress_bridge_for_subnet "$subnet")}"
  printf '{"subnets":[{"subnet":"%s","gateway":"%s"}],"network_interface":"%s","dns_enabled":%s}\n' \
    "$subnet" "${CTR_GATEWAY:-$(_worker_egress_gateway "$subnet")}" "$bridge" "$CTR_DNS_ENABLED"
}
_ctr() {
  local name="$1"; shift
  CTR_CALLS+=("$name $*")
  printf '%s\n' "$name $*" >>"$CTR_LOG"
  case "${1:-}" in
    network)
      case "${2:-}" in
        exists) [[ "$CTR_NETWORK_EXISTS" == "true" ]] ;;
        inspect) [[ "$CTR_NETWORK_EXISTS" == "true" ]] && network_json ;;
        create) CTR_NETWORK_EXISTS=true; return "$CTR_CREATE_RC" ;;
      esac
      ;;
    inspect)
      [[ "$CTR_CONTAINER_EXISTS" == "true" ]] || return 1
      if [[ "${2:-}" == "--format" ]]; then
        case "$3" in
          *NetworkSettings.Networks*) printf '%s\n' "$CTR_NETWORKS" ;;
          *HostConfig.Dns*) printf '%s\n' "$CTR_DNS" ;;
          *HostConfig.ExtraHosts*) printf '%s\n' "$CTR_EXTRA_HOSTS" ;;
        esac
      fi
      ;;
    image)
      return 1
      ;;
    *) return 0 ;;
  esac
}

# ── allocation, locking, and per-tenant rendering ─────────────────────────────
subnet="$(_worker_egress_alloc_subnet alpha)"
assert_eq "$subnet" "10.89.10.0/24" "first allocation takes the lowest free /24"
assert_eq "$(_worker_egress_alloc_subnet alpha)" "10.89.10.0/24" "allocation is stable"
assert_eq "$(_worker_egress_alloc_subnet beta)" "10.89.11.0/24" "second tenant gets another /24"
assert_eq "$(_worker_egress_bridge_for_subnet "$subnet")" "lweg10" "bridge name derives from the allocation"
printf 'ALLOW=git.example.org:443\n' >"$WORKER_EGRESS_DIR/gamma.conf"
_worker_egress_alloc_subnet gamma >/dev/null
assert_contains "$(cat "$WORKER_EGRESS_DIR/gamma.conf")" "ALLOW=git.example.org:443" "allocation preserves operator entries"

_worker_egress_lock
(
  exec 197>&-
  _worker_egress_lock
  printf 'acquired\n' >"$fixture/second-lock"
  _worker_egress_unlock
) &
lock_pid=$!
sleep 0.1
[[ ! -e "$fixture/second-lock" ]] || fail "subnet lock allows a concurrent holder"
_worker_egress_unlock
wait "$lock_pid"
assert_eq "$(cat "$fixture/second-lock")" "acquired" "second allocator acquires the lock after release"
pass "subnet allocation lock excludes concurrent holders"

ruleset="$(worker_egress_ruleset alpha)"
assert_contains "$ruleset" 'chain fwd_alpha' "tenant has its own forward chain"
assert_contains "$ruleset" 'chain in_alpha' "tenant has its own input chain"
assert_contains "$ruleset" 'flush chain inet lunarwing_worker_egress fwd_alpha' "apply flushes only the tenant forward chain"
assert_absent "$ruleset" 'flush chain inet lunarwing_worker_egress fwd_beta' "tenant apply never flushes another tenant"
assert_absent "$ruleset" 'chain workers' "the shared workers chain is gone"
assert_absent "$ruleset" 'dport 53' "recursive DNS has no allow rule"
assert_contains "$ruleset" 'iifname "lweg10" meta nfproto ipv6 counter reject with icmpx type admin-prohibited' "IPv6 is rejected on the worker bridge"
forward_block="$(awk '/chain fwd_alpha/{blocks++; inside=1} inside && blocks == 2 {print} inside && blocks == 2 && /^    }/{exit}' <<<"$ruleset")"
assert_contains "$forward_block" 'meta nfproto ipv6 counter reject with icmpx type admin-prohibited' "the forward chain carries the IPv6 reject (OQ-3)"
assert_contains "$forward_block" 'ip saddr != 10.89.10.0/24 counter reject with icmpx type admin-prohibited' "the forward chain rejects secondary sources (OQ-3)"
assert_contains "$ruleset" 'iifname "lweg10" ip saddr != 10.89.10.0/24 counter reject with icmpx type admin-prohibited' "unclassified source addresses are rejected"
assert_contains "$ruleset" 'iifname "lweg10" ip saddr 10.89.10.0/24 counter reject with icmpx type admin-prohibited' "exact all-protocol tenant reject is rendered"
assert_absent "$ruleset" 'meta l4proto tcp' "the final reject is not narrowed to TCP"
assert_contains "$ruleset" 'ip daddr 203.0.113.40 tcp dport 443 accept' "hostname resolution pins the default HTTPS port"
assert_contains "$ruleset" 'ip daddr 169.254.1.2 tcp dport 3000 accept' "rootless host alias is allowlisted"
input_block="$(awk '/chain in_alpha/{blocks++; inside=1} inside && blocks == 2 {print} inside && blocks == 2 && /^    }/{exit}' <<<"$ruleset")"
assert_contains "$input_block" 'ip daddr 169.254.1.2 tcp dport 3000 accept' "input chain permits the configured host LLM port"
assert_absent "$input_block" '203.0.113.40' "input chain does not expose arbitrary host ports for external destinations"
assert_eq "$(CONTAINER_RT=podman MT_ROOTLESS=false _worker_egress_host_addrs 10.89.10.1)" '10.89.10.1' "rootful loopback maps to the bridge gateway"
assert_absent "$ruleset" 'policy drop' "unrelated host traffic remains outside the tenant chains"
beta_ruleset="$(worker_egress_ruleset beta)"
assert_absent "$beta_ruleset" 'flush chain inet lunarwing_worker_egress "fwd_alpha"' "beta render never touches alpha"

LLM_TARGETS=("http://203.0.113.30/v1")
assert_contains "$(worker_egress_ruleset alpha)" 'ip daddr 203.0.113.30 tcp dport 80 accept' "portless HTTP uses port 80"
LLM_TARGETS=("https://203.0.113.31/v1")
assert_contains "$(worker_egress_ruleset alpha)" 'ip daddr 203.0.113.31 tcp dport 443 accept' "portless HTTPS uses port 443"
LLM_TARGETS=("http://does-not-exist.example.invalid:8080/v1")
if worker_egress_ruleset alpha >"$fixture/llm.out" 2>"$fixture/llm.err"; then
  pass "unresolvable tenant LLM narrows instead of widening"
else
  fail "unresolvable tenant LLM unexpectedly aborted rendering"
fi
assert_contains "$(cat "$fixture/llm.err")" 'WARNING: cannot resolve' "omitted tenant endpoint warns"
OPERATOR_ALLOW=("https://does-not-exist.example.invalid:443/repo.git")
assert_rc_fail worker_egress_ruleset alpha
OPERATOR_ALLOW=("[2001:db8::1]:443")
assert_rc_fail validate_worker_egress_entry "[2001:db8::1]:443"
OPERATOR_ALLOW=("https://203.0.113.20:443/repo.git")
LLM_TARGETS=("http://127.0.0.1:3000/v1" "https://203.0.113.10:8443/v1" "https://git.example.org/v1")
assert_rc_fail env CONTAINER_RT=podman MT_ROOTLESS=true WORKER_EGRESS_HOST_ADDRS=not-an-ip _worker_egress_host_addrs 10.89.10.1

# ── apply and static host launch contract ─────────────────────────────────────
NFT_LIST_RC=1
if (worker_egress_apply alpha) >/dev/null 2>"$fixture/apply.err"; then
  fail "apply accepted a filter that did not survive"
else
  pass "apply fails when the loaded chains disappear"
  assert_contains "$(cat "$fixture/apply.err")" 'did not survive the load' "apply refusal names the real cause"
fi
NFT_LIST_RC=0
NFT_LIST_OUT=$'chain "fwd_alpha" {\n}'
if (worker_egress_apply alpha) >/dev/null 2>"$fixture/no-reject.err"; then
  fail "apply accepted a chain without its reject"
else
  pass "apply rejects a chain without its reject"
  assert_contains "$(cat "$fixture/no-reject.err")" 'exact reject' "no-reject refusal names the exact-reject contract"
fi
NFT_LIST_OUT=""
NFT_LOAD_RC=1
assert_rc_fail worker_egress_apply alpha
NFT_LOAD_RC=0
NFT_CALLS=()
: >"$NFT_LOG"
if worker_egress_apply alpha >"$fixture/applied.dump" 2>"$fixture/apply-ok.err"; then
  pass "apply succeeds with both live tenant chains"
else
  fail "apply rejected a valid ruleset: $(cat "$fixture/apply-ok.err")"
fi
assert_contains "$(printf '%s\n' "${NFT_CALLS[@]}")" '-f' "apply loads the rendered file"
assert_eq "$(grep -c 'list chain' "$NFT_LOG")" "2" "apply verifies forward and input chains"
assert_eq "$(cat "$WORKER_EGRESS_DIR/alpha.hosts")" 'git.example.org:203.0.113.40' "static hosts come from the same resolved answers"
launch_args="$(worker_egress_launch_args alpha)"
assert_contains "$launch_args" '--dns
127.0.0.1' "launch pins DNS to an unserved loopback resolver"
assert_contains "$launch_args" '--add-host
git.example.org:203.0.113.40' "launch installs the static host mapping"
assert_contains "$launch_args" '--network
lw-egress-alpha' "launch joins only the tenant network"
if [[ "$(id -u)" == "0" ]]; then
  assert_eq "$(stat -c '%U:%G:%a' "$WORKER_EGRESS_GUARD")" 'root:root:755' "installed guard is root-owned 0755"
else
  assert_eq "$(stat -c '%a' "$HARNESS_REPO_ROOT/ic/scripts/lunarwing-worker-egress-guard.sh")" '755' "guard source is executable"
  assert_contains "$(declare -f install_worker_egress_guard)" 'install -m 0755 -o root -g root' "root installation pins owner and mode"
fi

printf 'stale\n' >"$WORKER_EGRESS_DIR/alpha.nft"
printf 'stale\n' >"$WORKER_EGRESS_DIR/alpha.hosts"
OPERATOR_ALLOW=("https://does-not-exist.example.invalid:443/repo.git")
if (worker_egress_apply alpha) >/dev/null 2>&1; then
  fail "apply accepted an unresolvable operator entry"
elif [[ -e "$WORKER_EGRESS_DIR/alpha.nft" || -e "$WORKER_EGRESS_DIR/alpha.hosts" ]]; then
  fail "render failure left stale boot state behind"
else
  pass "render failure removes stale nft and host state"
fi
OPERATOR_ALLOW=("https://203.0.113.20:443/repo.git")
worker_egress_apply alpha >/dev/null

if (
  worker_egress_ensure() { printf 'ensure\n' >>"$fixture/prepare.log"; }
  worker_egress_apply() { printf 'apply\n' >>"$fixture/prepare.log"; }
  prepare_worker_egress alpha opencode2 >/dev/null
  printf '%s\n' "${WORKER_EGRESS_LAUNCH_ARGS[@]}" >"$fixture/prepare-args"
  grep -qx ensure "$fixture/prepare.log" && grep -qx apply "$fixture/prepare.log"
); then
  pass "prepare_worker_egress performs ensure and apply"
else
  fail "prepare_worker_egress can be bypassed without ensure/apply"
fi
assert_contains "$(cat "$fixture/prepare-args")" 'lw-egress-alpha' "prepare populates launch arguments"

# ── network inspection and container reconciliation ───────────────────────────
CTR_NETWORK_EXISTS=false
CTR_CALLS=()
: >"$CTR_LOG"
worker_egress_ensure alpha >/dev/null
assert_contains "$(cat "$CTR_LOG")" 'network create --subnet 10.89.10.0/24 --interface-name lweg10 --disable-dns' "network creation disables DNS and pins the bridge"
worker_egress_ensure alpha >/dev/null
assert_eq "$(grep -c 'network create' "$CTR_LOG")" "1" "network ensure is idempotent"
CTR_SUBNET=10.89.99.0/24
if (worker_egress_ensure alpha) >/dev/null 2>"$fixture/subnet-mismatch.err"; then
  fail "network ensure accepted a subnet mismatch"
else
  pass "network ensure rejects a subnet mismatch"
  assert_contains "$(cat "$fixture/subnet-mismatch.err")" 'expected' "subnet mismatch names the expected contract"
fi
CTR_SUBNET=""
CTR_BRIDGE=lweg99
if (worker_egress_ensure alpha) >/dev/null 2>"$fixture/bridge-mismatch.err"; then
  fail "network ensure accepted a bridge-name mismatch (OQ-3)"
else
  pass "network ensure rejects a bridge-name mismatch (OQ-3)"
  assert_contains "$(cat "$fixture/bridge-mismatch.err")" "bridge 'lweg99'" "bridge mismatch names the observed bridge"
fi
CTR_BRIDGE=""
CTR_DNS_ENABLED=true
if (worker_egress_ensure alpha) >/dev/null 2>"$fixture/dns-enabled.err"; then
  fail "network ensure accepted recursive DNS"
else
  pass "network ensure rejects recursive DNS"
  assert_contains "$(cat "$fixture/dns-enabled.err")" 'recursive DNS enabled' "DNS-enabled network refusal names the cause"
fi
CTR_DNS_ENABLED=false
CTR_GATEWAY=""
if (CTR_GATEWAY=10.89.10.254 worker_egress_ensure alpha) >/dev/null 2>"$fixture/gateway-mismatch.err"; then
  fail "network ensure accepted a gateway mismatch (OQ-3)"
else
  pass "network ensure rejects a gateway mismatch (OQ-3)"
  assert_contains "$(cat "$fixture/gateway-mismatch.err")" "gateway '10.89.10.254'" "gateway mismatch names the observed gateway"
fi
CTR_NETWORK_EXISTS=false
CTR_CALLS=()
: >"$CTR_LOG"
if (CONTAINER_RT=docker MT_ROOTLESS=false worker_egress_ensure alpha) >/dev/null 2>&1; then
  pass "docker network creation is supported"
  assert_contains "$(cat "$CTR_LOG")" 'com.docker.network.bridge.name=lweg10' "docker network pins its bridge name"
else
  fail "docker network creation failed"
fi
CTR_NETWORK_EXISTS=true

# OQ-5: docker's embedded resolver is unobservable. An existing docker network
# must audit as `unverified` (never a bogus `disabled` claim), and podman must
# still refuse anything that is not verifiably `disabled`.
if (CONTAINER_RT=docker MT_ROOTLESS=false worker_egress_ensure alpha) >/dev/null 2>&1; then
  pass "an existing docker network audits as unverified DNS (OQ-5)"
else
  fail "docker network ensure rejected an unverified-DNS contract"
fi

worker_egress_apply alpha >/dev/null
CTR_NETWORKS=lw-egress-alpha
CTR_DNS=127.0.0.1
CTR_EXTRA_HOSTS=git.example.org:203.0.113.40
assert_rc_fail env CTR_NETWORKS=lw-egress-alpha,default worker_egress_container_attached alpha lunarwing-opencode2-alpha
assert_rc_fail env CTR_DNS=8.8.8.8 worker_egress_container_attached alpha lunarwing-opencode2-alpha
assert_rc_fail env CTR_EXTRA_HOSTS= worker_egress_container_attached alpha lunarwing-opencode2-alpha
if worker_egress_container_attached alpha lunarwing-opencode2-alpha; then
  pass "container contract accepts exclusive network, pinned DNS, and current hosts"
else
  fail "container contract rejected a healthy worker"
fi
: >"$CTR_LOG"
CTR_NETWORKS=default
worker_egress_reconcile_container alpha lunarwing-opencode2-alpha
assert_contains "$(cat "$CTR_LOG")" 'rm -f lunarwing-opencode2-alpha' "stale pre-WI#424 container is force-recreated"
CTR_NETWORKS=lw-egress-alpha
if (worker_egress_verify() { return 1; }; worker_egress_verify_after_start alpha lunarwing-opencode2-alpha) >/dev/null 2>&1; then
  fail "post-start check accepted a vanished filter"
else
  pass "post-start check fails when the filter vanishes"
  assert_contains "$(cat "$CTR_LOG")" 'stop lunarwing-opencode2-alpha' "post-start failure stops the worker"
fi
: >"$CTR_LOG"
if (CTR_NETWORKS=lw-egress-alpha,default worker_egress_verify_after_start alpha lunarwing-opencode2-alpha) >/dev/null 2>&1; then
  fail "post-start check accepted an extra network attachment (OQ-3)"
else
  pass "post-start check refuses an extra network attachment (OQ-3)"
  assert_contains "$(cat "$CTR_LOG")" 'stop lunarwing-opencode2-alpha' "extra-network post-start failure stops the worker"
fi
contract="$(_worker_launch_contract alpha lunarwing-worker-opencode2:latest ssh-agent-dir-ro)"
assert_contains "$contract" 'egress=lw-egress-alpha' "launch contract forces pre-egress containers to recreate"

# ── host-side audit, including another tenant's rules ─────────────────────────
CTR_NETWORK_EXISTS=true
CTR_NETWORKS=lw-egress-alpha
CTR_DNS=127.0.0.1
CTR_EXTRA_HOSTS=git.example.org:203.0.113.40
NFT_LIST_OUT=""
NFT_LIST_RC=0
if (worker_egress_audit alpha) >"$fixture/audit.out" 2>&1; then
  pass "audit passes a healthy tenant"
else
  fail "audit rejected a healthy tenant: $(cat "$fixture/audit.out")"
fi
assert_contains "$(cat "$fixture/audit.out")" 'tenant-specific forward + input reject rules verified' "audit verifies both tenant chains"
NFT_LIST_OUT=$'iifname "lweg11" ip saddr 10.89.11.0/24 counter packets 0 bytes 0 reject with icmpx type admin-prohibited'
assert_rc_fail worker_egress_audit alpha
NFT_LIST_OUT=""
NFT_LIST_RC=1
assert_rc_fail worker_egress_audit alpha
NFT_LIST_RC=0
assert_rc_fail env CTR_NETWORKS=default worker_egress_audit alpha
assert_rc_fail worker_egress_audit unallocated

# ── the real guard with a hermetic nft executable ─────────────────────────────
guard="$HARNESS_REPO_ROOT/ic/scripts/lunarwing-worker-egress-guard.sh"
worker_egress_ruleset alpha >"$fixture/alpha.nft"
guard_bin="$fixture/bin"
mkdir -p "$guard_bin"
cat >"$guard_bin/nft" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
  -f) exit "${FAKE_LOAD_RC:-0}" ;;
  list)
    [[ "${FAKE_LIST_RC:-0}" == "0" ]] || exit 1
    chain="${!#}"
    printf 'chain "%s" {\n' "$chain"
    case "${FAKE_MODE:-good}" in
      empty) ;;
      wrong) printf '  iifname "lweg11" ip saddr 10.89.11.0/24 counter packets 0 bytes 0 reject with icmpx type admin-prohibited\n' ;;
      narrow) printf '  iifname "lweg10" meta l4proto tcp ip saddr 10.89.10.0/24 counter packets 0 bytes 0 reject with icmpx type admin-prohibited\n' ;;
      *) printf '  iifname "lweg10" ip saddr 10.89.10.0/24 counter packets 0 bytes 0 reject with icmpx admin-prohibited\n' ;;
    esac
    printf '}\n'
    ;;
esac
STUB
chmod +x "$guard_bin/nft"
assert_rc_fail env PATH="$guard_bin:$PATH" "$guard" "$fixture/missing.nft" rootful fwd_alpha
if PATH="$guard_bin:$PATH" "$guard" "$fixture/alpha.nft" rootful fwd_alpha >/dev/null; then
  pass "guard accepts both exact live chains"
else
  fail "guard rejected both exact live chains"
fi
assert_rc_fail env PATH="$guard_bin:$PATH" FAKE_LOAD_RC=1 "$guard" "$fixture/alpha.nft" rootful fwd_alpha
assert_rc_fail env PATH="$guard_bin:$PATH" FAKE_LIST_RC=1 "$guard" "$fixture/alpha.nft" rootful fwd_alpha
assert_rc_fail env PATH="$guard_bin:$PATH" FAKE_MODE=empty "$guard" "$fixture/alpha.nft" rootful fwd_alpha
assert_rc_fail env PATH="$guard_bin:$PATH" FAKE_MODE=wrong "$guard" "$fixture/alpha.nft" rootful fwd_alpha
assert_rc_fail env PATH="$guard_bin:$PATH" FAKE_MODE=narrow "$guard" "$fixture/alpha.nft" rootful fwd_alpha
if PATH="$guard_bin:$PATH" "$guard" --verify "$fixture/alpha.nft" rootful fwd_alpha >/dev/null; then
  pass "guard --verify rechecks a started worker without reloading"
else
  fail "guard --verify rejected a live worker"
fi
mkdir -p "$fixture/no-nft"
assert_rc_fail env PATH="$fixture/no-nft" /bin/bash "$guard" "$fixture/alpha.nft" rootful fwd_alpha
assert_rc_fail env PATH="$guard_bin:$PATH" /bin/bash "$guard" "$fixture/alpha.nft" rootless fwd_alpha 'bad;tenant'
sed 's/ip saddr 10.89.10.0\/24 counter reject/meta l4proto tcp ip saddr 10.89.10.0\/24 counter reject/' \
  "$fixture/alpha.nft" >"$fixture/narrow.nft"
assert_rc_fail env PATH="$guard_bin:$PATH" "$guard" "$fixture/narrow.nft" rootful fwd_alpha

# ── rendered OpenRC start behavior ────────────────────────────────────────────
mkdir -p "$fixture/initd"
OPENRC_INITD_DIR="$fixture/initd"
CONTAINER_RT=podman
MT_ROOTLESS=true
WORKER_EGRESS_GUARD="$fixture/openrc-guard"
render_worker_openrc_unit alpha opencode2
cat >"$fixture/openrc-guard" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$GUARD_LOG"
if [[ "${1:-}" == "--verify" ]]; then exit "${VERIFY_RC:-0}"; fi
exit "${LOAD_RC:-0}"
STUB
chmod +x "$fixture/openrc-guard"
cat >"$fixture/runtime" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$CTR_LOG"
case "${1:-}" in
  inspect) printf 'true\n' ;;
  start|stop) ;;
  exec) ;;
esac
STUB
chmod +x "$fixture/runtime"
openrc_start() {
  (
    ebegin() { :; }; eend() { return 0; }; ewarn() { :; }; einfo() { :; }; checkpath() { :; }
    source "$OPENRC_INITD_DIR/lunarwing-opencode2-alpha"
    wk_rootless=false
    wk_runtime="$fixture/runtime"
    wk_wait=1
    start
  )
}
: >"$GUARD_LOG"; : >"$CTR_LOG"
if LOAD_RC=0 VERIFY_RC=0 GUARD_LOG="$GUARD_LOG" CTR_LOG="$CTR_LOG" openrc_start >/dev/null 2>&1; then
  pass "OpenRC start passes pre/post guards and starts the worker"
  assert_contains "$(cat "$GUARD_LOG")" '--verify' "OpenRC verifies after container start"
  assert_contains "$(cat "$CTR_LOG")" 'start lunarwing-opencode2-alpha' "OpenRC starts the container after the pre-guard"
else
  fail "OpenRC start failed with healthy guards"
fi
: >"$GUARD_LOG"; : >"$CTR_LOG"
assert_rc_fail env LOAD_RC=1 VERIFY_RC=0 GUARD_LOG="$GUARD_LOG" CTR_LOG="$CTR_LOG" openrc_start
assert_absent "$(cat "$CTR_LOG")" 'start lunarwing-opencode2-alpha' "OpenRC pre-guard failure prevents container start"
: >"$GUARD_LOG"; : >"$CTR_LOG"
assert_rc_fail env LOAD_RC=0 VERIFY_RC=1 GUARD_LOG="$GUARD_LOG" CTR_LOG="$CTR_LOG" openrc_start
assert_contains "$(cat "$CTR_LOG")" 'stop lunarwing-opencode2-alpha' "OpenRC post-guard failure stops the container"
: >"$CTR_LOG"
if (
  ebegin() { :; }; eend() { return 0; }; ewarn() { :; }; einfo() { :; }; checkpath() { :; }
  source "$OPENRC_INITD_DIR/lunarwing-opencode2-alpha"
  wk_rootless=false; wk_runtime="$fixture/runtime"; wk_egress_guard="$fixture/missing-guard"
  start
) >/dev/null 2>&1; then
  fail "OpenRC accepted a missing guard"
else
  pass "OpenRC refuses a missing guard"
  assert_absent "$(cat "$CTR_LOG")" 'start lunarwing-opencode2-alpha' "missing guard cannot start the worker"
fi

# ── OpenRC babysitter: guard on every respawn, and worker-unit dependency ─────
INIT_SYSTEM=openrc
MT_ROOTLESS=true
render_container_babysitter_unit alpha opencode2 lunarwing-opencode2-alpha 1000 /home/alpha
babysitter_unit="$OPENRC_INITD_DIR/lunarwing-opencode2-alpha-sup"
assert_contains "$(cat "$babysitter_unit")" 'babysitter_egress_guard' "babysitter receives the root-owned guard"
assert_contains "$(cat "$babysitter_unit")" 'need net localmount lunarwing-opencode2-alpha' "babysitter depends on the worker unit"
cat >"$fixture/podman" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$CTR_LOG"
exit 0
STUB
chmod +x "$fixture/podman"
babysitter="$HARNESS_REPO_ROOT/ic/scripts/lunarwing-ctr-babysit.sh"
: >"$GUARD_LOG"; : >"$CTR_LOG"
if PATH="$fixture:$PATH" LOAD_RC=0 VERIFY_RC=0 GUARD_LOG="$GUARD_LOG" CTR_LOG="$CTR_LOG" \
    "$babysitter" lunarwing-opencode2-alpha "$fixture/openrc-guard" "$fixture/alpha.nft" fwd_alpha >/dev/null 2>&1; then
  pass "babysitter runs pre/post guards around podman start"
  assert_contains "$(cat "$CTR_LOG")" 'start lunarwing-opencode2-alpha' "babysitter starts the guarded container"
else
  fail "babysitter rejected healthy guards"
fi
: >"$GUARD_LOG"; : >"$CTR_LOG"
assert_rc_fail env PATH="$fixture:$PATH" LOAD_RC=1 VERIFY_RC=0 GUARD_LOG="$GUARD_LOG" CTR_LOG="$CTR_LOG" \
  "$babysitter" lunarwing-opencode2-alpha "$fixture/openrc-guard" "$fixture/alpha.nft" fwd_alpha
assert_absent "$(cat "$CTR_LOG")" 'start lunarwing-opencode2-alpha' "babysitter pre-guard failure prevents respawn"
: >"$GUARD_LOG"; : >"$CTR_LOG"
assert_rc_fail env PATH="$fixture:$PATH" LOAD_RC=0 VERIFY_RC=1 GUARD_LOG="$GUARD_LOG" CTR_LOG="$CTR_LOG" \
  "$babysitter" lunarwing-opencode2-alpha "$fixture/openrc-guard" "$fixture/alpha.nft" fwd_alpha
assert_contains "$(cat "$CTR_LOG")" 'stop lunarwing-opencode2-alpha' "babysitter post-guard failure stops the respawned worker"
assert_rc_fail env PATH="$fixture:$PATH" "$babysitter" lunarwing-opencode2-alpha "$fixture/missing-guard" "$fixture/alpha.nft" fwd_alpha

# ── source wiring pins ────────────────────────────────────────────────────────
worker_config="$HARNESS_REPO_ROOT/ic/scripts/mt-admin-lib/09-worker-config.sh"
systemd_lib="$HARNESS_REPO_ROOT/ic/scripts/mt-admin-lib/07-units-systemd.sh"
assert_eq "$(grep -c 'WORKER_EGRESS_LAUNCH_ARGS' "$worker_config")" "4" "all four imperative workers use the complete egress launch args"
assert_eq "$(grep -c 'worker_egress_reconcile_container' "$worker_config")" "4" "all four imperative workers recreate stale containers"
assert_eq "$(grep -c 'worker_egress_verify_after_start' "$worker_config")" "5" "all Quadlet workers and the shared imperative registration verify after start"
assert_contains "$(cat "$systemd_lib")" 'ExecStartPre=${egress_guard} ${egress_ruleset} rootless ${egress_chain}' "Quadlet pre-start guard is mandatory"
assert_contains "$(cat "$systemd_lib")" 'ExecStartPost=${egress_guard} --verify ${egress_ruleset} rootless ${egress_chain}' "Quadlet post-start guard is mandatory"
assert_contains "$(cat "$systemd_lib")" 'DNS=127.0.0.1' "Quadlet disables upstream DNS"
assert_contains "$(cat "$systemd_lib")" 'AddHost=' "Quadlet installs static host mappings"

# ── optional live acceptance, never a default host-side nft mutation ─────────
say_skip() { printf 'SKIP: %s\n' "$1"; }
LIVE_NET=lw-egress-liveprobe
LIVE_CONTAINER=egress-liveprobe
LIVE_HTTPD_PID=""
run_live_arm() {
  local image=docker.io/library/busybox:stable subnet=10.89.249.0/24 port=18099
  local addr_ok="${WORKER_EGRESS_HOST_ADDRS%% *}" addr_deny=203.0.113.10
  command -v podman >/dev/null 2>&1 || { say_skip "live arm: podman missing"; return 0; }
  command -v nft >/dev/null 2>&1 || { say_skip "live arm: nft missing"; return 0; }
  command -v curl >/dev/null 2>&1 || { say_skip "live arm: curl missing"; return 0; }
  podman image exists "$image" 2>/dev/null || podman pull -q "$image" >/dev/null 2>&1 \
    || { say_skip "live arm: image unavailable"; return 0; }
  [[ "$(podman info --format '{{.Host.Security.Rootless}}' 2>/dev/null)" == "true" ]] \
    || { say_skip "live arm: rootless podman only"; return 0; }
  cleanup_live() {
    podman rm -f "$LIVE_CONTAINER" >/dev/null 2>&1 || true
    podman network rm -f "$LIVE_NET" >/dev/null 2>&1 || true
    [[ -z "$LIVE_HTTPD_PID" ]] || kill -9 "$LIVE_HTTPD_PID" >/dev/null 2>&1 || true
    podman unshare --rootless-netns nft flush chain inet "$WORKER_EGRESS_TABLE" fwd_liveprobe >/dev/null 2>&1 || true
    podman unshare --rootless-netns nft delete chain inet "$WORKER_EGRESS_TABLE" fwd_liveprobe >/dev/null 2>&1 || true
    podman unshare --rootless-netns nft flush chain inet "$WORKER_EGRESS_TABLE" in_liveprobe >/dev/null 2>&1 || true
    podman unshare --rootless-netns nft delete chain inet "$WORKER_EGRESS_TABLE" in_liveprobe >/dev/null 2>&1 || true
  }
  trap cleanup_live EXIT
  (cd "$fixture" && exec python3 -m http.server "$port" --bind 0.0.0.0) >"$fixture/httpd.log" 2>&1 </dev/null &
  LIVE_HTTPD_PID=$!
  sleep 1
  curl -sf -o /dev/null -m 2 "http://127.0.0.1:$port/" || { say_skip "live arm: fixture server failed"; return 0; }
  podman network create --subnet "$subnet" --interface-name lweg249 --disable-dns "$LIVE_NET" >/dev/null \
    || { fail "live arm: network create failed"; return 1; }
  printf 'SUBNET=%s\nALLOW=localhost:%s\n' "$subnet" "$port" >"$WORKER_EGRESS_DIR/liveprobe.conf"
  _worker_egress_llm_targets() { return 0; }
  _worker_egress_operator_allow() { worker_egress_state_get liveprobe ALLOW; }
  CONTAINER_RT=podman MT_ROOTLESS=true worker_egress_ruleset liveprobe >"$fixture/liveprobe.nft" \
    || { fail "live arm: render failed"; return 1; }
  _worker_egress_static_hosts "$(worker_egress_target_records liveprobe)" >"$WORKER_EGRESS_DIR/liveprobe.hosts"
  if ! "$guard" "$fixture/liveprobe.nft" rootless fwd_liveprobe >"$fixture/live-guard.out" 2>"$fixture/live-guard.err"; then
    cat "$fixture/live-guard.err" >&2
    sed 's/^/  /' "$fixture/liveprobe.nft" >&2
    fail "live arm: pre-start guard failed"; return 1
  fi
  local -a live_args=()
  mapfile -t live_args < <(CONTAINER_RT=podman MT_ROOTLESS=true worker_egress_launch_args liveprobe)
  podman run -d --name "$LIVE_CONTAINER" "${live_args[@]}" "$image" sleep 120 >/dev/null \
    || { fail "live arm: container start failed"; return 1; }
  sleep 2
  "$guard" --verify "$fixture/liveprobe.nft" rootless fwd_liveprobe >/dev/null 2>&1 \
    || { fail "live arm: post-start guard failed"; return 1; }
  if podman exec "$LIVE_CONTAINER" wget -q -O /dev/null -T 5 "http://localhost:$port/"; then
    pass "live arm: static-host allowlisted fetch succeeds"
  else
    fail "live arm: static-host allowlisted fetch failed"
  fi
  if podman exec "$LIVE_CONTAINER" wget -q -O /dev/null -T 5 "https://$addr_deny/" >/dev/null 2>&1; then
    fail "live arm: non-allowlisted fetch succeeded"
  else
    pass "live arm: non-allowlisted fetch fails"
  fi
  if podman exec "$LIVE_CONTAINER" wget -q -O /dev/null -T 5 "https://1.1.1.1/" >/dev/null 2>&1; then
    fail "live arm: raw-IP fetch succeeded"
  else
    pass "live arm: raw-IP fetch fails"
  fi
  if podman exec "$LIVE_CONTAINER" nslookup example.com >/dev/null 2>&1; then
    fail "live arm: recursive DNS still works as an exfiltration path"
  else
    pass "live arm: recursive DNS is disabled"
  fi
  [[ -z "$addr_ok" ]] || pass "live arm: host alias under test was $addr_ok"
}
if [[ "${LUNARWING_MT_EGRESS_LIVE:-0}" == "1" ]]; then
  run_live_arm || true
else
  say_skip "live acceptance arm (set LUNARWING_MT_EGRESS_LIVE=1 to run it)"
fi

if ((failures > 0)); then
  printf '\n%d failure(s)\n' "$failures" >&2
  exit 1
fi
printf '\nworker egress harness: all checks passed\n'
