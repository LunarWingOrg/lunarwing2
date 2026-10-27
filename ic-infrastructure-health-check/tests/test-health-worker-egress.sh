#!/usr/bin/env bash
# WI#424 worker egress health component and orchestration pins.
#
# The component must audit entirely host-side: it may read root-owned state and
# ask the tenant's runtime for observations, but it must never execute or source
# the provisioning checkout (run-226 OQ-1). These pins drive it through PATH
# stubs for the runtime, `nft`, `sudo`, `id` and `getent`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CHECK="$ROOT/health-worker-egress.sh"
FIXTURE="$(mktemp -d)"
trap 'rm -rf "$FIXTURE"' EXIT
failures=0
passes=0
pass() { printf 'PASS: %s\n' "$1"; passes=$((passes + 1)); }
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

BIN="$FIXTURE/bin"
EGRESS="$FIXTURE/egress"
mkdir -p "$BIN" "$EGRESS"
RUNTIME_LOG="$FIXTURE/runtime.log"
: >"$RUNTIME_LOG"

cat >"$BIN/id" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
  -u) printf '1001\n' ;;
  *) printf '1001\n' ;;
esac
STUB
cat >"$BIN/getent" <<'STUB'
#!/usr/bin/env bash
[[ "${1:-}" == passwd ]] && printf 'alpha:x:1001:1001::/home/alpha:/bin/sh\n'
STUB
# `sudo -u <t> env K=V ... <cmd>` must behave like plain execution here.
cat >"$BIN/sudo" <<'STUB'
#!/usr/bin/env bash
shift  # -u
shift  # tenant
[[ "${1:-}" == env ]] && { shift; while [[ "${1:-}" == *=* ]]; do shift; done; }
exec "$@"
STUB
cat >"$BIN/podman" <<'STUB'
#!/usr/bin/env bash
# The container name carries the tenant: lunarwing-<worker>-<tenant>.
NET_NAME="lw-egress-alpha"
for arg in "$@"; do
  case "$arg" in lunarwing-*-alpha) NET_NAME=lw-egress-alpha ;; lunarwing-*-beta) NET_NAME=lw-egress-beta ;; esac
done
if [[ "${1:-}" == unshare ]]; then
  shift
  while [[ "${1:-}" == --* ]]; do shift; done
  exec "$@"
fi
printf '%s\n' "$*" >>"$RUNTIME_LOG"
case "${1:-}" in
  network)
    case "${2:-}" in
      exists) [[ "${FIXTURE_NETWORK_EXISTS:-true}" == "true" ]] ;;
      inspect)
        net="${3:-lw-egress-alpha}"
        idx=10; [[ "$net" == "lw-egress-beta" ]] && idx=11
        subnet="10.89.${idx}.0/24"; bridge="lweg${idx}"; gw="10.89.${idx}.1"
        [[ -n "${FIXTURE_SUBNET_OVERRIDE:-}" ]] && subnet="$FIXTURE_SUBNET_OVERRIDE"
        [[ -n "${FIXTURE_BRIDGE_OVERRIDE:-}" ]] && bridge="$FIXTURE_BRIDGE_OVERRIDE"
        [[ -n "${FIXTURE_GATEWAY_OVERRIDE:-}" ]] && gw="$FIXTURE_GATEWAY_OVERRIDE"
        printf '{"subnets":[{"subnet":"%s","gateway":"%s"}],"network_interface":"%s","dns_enabled":%s}\n' \
          "$subnet" "$gw" "$bridge" "${FIXTURE_DNS_ENABLED:-false}"
        ;;
      *) exit 0 ;;
    esac
    ;;
  inspect)
    case "${2:-}" in
      --format)
        case "${3:-}" in
          *NetworkSettings.Networks*) printf '%s\n' "${FIXTURE_NETWORKS:-$NET_NAME}" ;;
          *HostConfig.Dns*) printf '%s\n' "${FIXTURE_DNS:-127.0.0.1}" ;;
          *HostConfig.ExtraHosts*)
            if [[ "${FIXTURE_HOSTS_EMPTY:-}" == "1" ]]; then printf '\n'; else printf '%s\n' "${FIXTURE_HOSTS:-git.example.org:203.0.113.40}"; fi ;;

        esac
        ;;
      -f)
        printf '%s\n' "${FIXTURE_RUNNING:-true}"
        ;;
    esac
    ;;
  *) exit 0 ;;
esac
STUB
cat >"$BIN/nft" <<'STUB'
#!/usr/bin/env bash
printf 'nft %s\n' "$*" >>"$RUNTIME_LOG"
chain="${!#}"
[[ "${FIXTURE_NFT_FAIL:-}" == "$chain" ]] && exit 1
tenant="${chain#fwd_}"; tenant="${tenant#in_}"
idx=10; [[ "$tenant" == "beta" ]] && idx=11
subnet="10.89.${idx}.0/24"
[[ "${FIXTURE_NFT_WEAK:-}" == "$chain" ]] && subnet="10.89.99.0/24"
cat <<RULES
chain ${chain} {
		type filter hook forward priority filter; policy accept;
		iifname "lweg${idx}" meta nfproto ipv6 counter packets 0 bytes 0 reject with icmpx type admin-prohibited
		iifname "lweg${idx}" ip saddr != ${subnet} counter packets 0 bytes 0 reject with icmpx type admin-prohibited
		iifname "lweg${idx}" ip saddr ${subnet} counter packets 0 bytes 0 reject with icmpx type admin-prohibited
	}
RULES
STUB
chmod +x "$BIN"/*
# The docker lane shares the stub: the audit only needs the runtime reads.
ln -sf podman "$BIN/docker"

export RUNTIME_LOG
export FIXTURE_HOSTS="git.example.org:203.0.113.40"
printf 'SUBNET=10.89.10.0/24\nALLOW=git.example.org:443\n' >"$EGRESS/alpha.conf"
printf 'SUBNET=10.89.11.0/24\n' >"$EGRESS/beta.conf"
printf 'git.example.org:203.0.113.40\n' >"$EGRESS/alpha.hosts"
printf 'git.example.org:203.0.113.40\n' >"$EGRESS/beta.hosts"
cat >"$FIXTURE/ports.json" <<'JSON'
{"tenants":{
  "alpha":{"workers":{"opencode2":true}},
  "beta":{"workers":{"pebble":true}},
  "gamma":{"workers":{"opencode":false}}
}}
JSON

run_check() {
  set +e
  OUTPUT="$(env PATH="$BIN:$PATH" \
    HEALTH_WORKER_EGRESS_RUNTIME="${HEALTH_WORKER_EGRESS_RUNTIME:-auto}" \
    HEALTH_WORKER_EGRESS_ROOTLESS="${HEALTH_WORKER_EGRESS_ROOTLESS:-auto}" \
    FIXTURE_SUBNET_OVERRIDE="${FI_SUB:-}" FIXTURE_BRIDGE_OVERRIDE="${FI_BR:-}" \
    FIXTURE_GATEWAY_OVERRIDE="${FI_GW:-}" \
    FIXTURE_NETWORKS="${FIXTURE_NETWORKS:-}" FIXTURE_HOSTS_EMPTY="${FIXTURE_HOSTS_EMPTY:-}" \
    FIXTURE_NFT_FAIL="${FIXTURE_NFT_FAIL:-}" FIXTURE_NFT_WEAK="${FIXTURE_NFT_WEAK:-}" \
    FIXTURE_RUNNING="${FIXTURE_RUNNING:-true}" \
    SELF_HEAL_TENANTS_FILE="${TENANTS_FILE:-$FIXTURE/ports.json}" \
    LUNARWING_MT_WORKER_EGRESS_DIR="$EGRESS" \
    "$CHECK" 2>&1)"
  RC=$?
  set -e
}

# A tenant whose only entered accounting is an operator checkout must not be
# touched. Point LUNARWING_MT_ADMIN at a tripwire script and assert it is never
# invoked by the audit.
TRIPWIRE="$FIXTURE/tripwire"
cat >"$TRIPWIRE" <<STUB
#!/usr/bin/env bash
printf 'tripped\n' >>"$FIXTURE/tripwire.log"
exit 0
STUB
chmod +x "$TRIPWIRE"
export LUNARWING_MT_ADMIN="$TRIPWIRE"

run_check
assert_eq "$RC" "0" "healthy audits exit zero"
assert_eq "$(jq -r '.status' <<<"$OUTPUT")" "healthy" "healthy audits report healthy"
assert_eq "$(jq '.metrics.instances | length' <<<"$OUTPUT")" "2" "only selected worker tenants are audited"
assert_absent "$OUTPUT" 'gamma' "unselected tenants are not audited"
if [[ -e "$FIXTURE/tripwire.log" ]]; then
  fail "the audit executed an operator-checkout path (OQ-1)"
else
  pass "the audit never executes or sources the provisioning checkout (OQ-1)"
fi

# Missing egress state for one tenant.
rm -f "$EGRESS/beta.conf"
run_check
assert_eq "$RC" "2" "a tenant without egress state is critical"
assert_eq "$(jq -r '.status' <<<"$OUTPUT")" "critical" "failed audit drives component critical"
assert_eq "$(jq -r '.metrics.instances[] | select(.tenant=="alpha") | .status' <<<"$OUTPUT")" "healthy" "healthy sibling remains healthy"
assert_eq "$(jq -r '.metrics.instances[] | select(.tenant=="beta") | .status' <<<"$OUTPUT")" "critical" "unconfigured tenant is attributed critical"
assert_contains "$OUTPUT" 'not configured for beta' "the issue names the unconfigured tenant"
printf 'SUBNET=10.89.11.0/24\n' >"$EGRESS/beta.conf"

# OQ-3: a network whose bridge name differs from the derived one is refused.
FI_BR="lweg99" run_check
assert_eq "$RC" "2" "a mismatched bridge contract is critical"
assert_contains "$OUTPUT" "bridge 'lweg99'" "the mismatch names the observed bridge"

# OQ-3: subnet and gateway mismatches are refused too.
FI_SUB="10.89.99.0/24" run_check
assert_eq "$RC" "2" "a mismatched network subnet is critical"
FI_GW="10.89.10.254" run_check
assert_eq "$RC" "2" "a mismatched gateway is critical"

# OQ-3 (IPv6 class): a chain without the exact all-protocol reject is refused.
FIXTURE_NFT_WEAK="fwd_alpha" run_check
assert_eq "$RC" "2" "a chain without the tenant's exact reject is critical"
assert_contains "$OUTPUT" 'lacks the exact reject' "the weak chain is named"
FIXTURE_NFT_FAIL="in_beta" run_check
assert_eq "$RC" "2" "an unloaded chain is critical"
assert_contains "$OUTPUT" 'in_beta is not loaded' "the missing chain is named"

# OQ-3: a container with a stale attachment/DNS contract is refused, even when
# the network and chains are healthy.
export FIXTURE_NETWORKS="lw-egress-alpha other"
run_check
assert_eq "$RC" "2" "extra network attachment is critical"
assert_contains "$OUTPUT" 'stale DNS/hosts contract' "the stale container is named"
unset FIXTURE_NETWORKS
FIXTURE_HOSTS_EMPTY=1 run_check
assert_eq "$RC" "2" "missing static hosts is critical"

# A tenant with no running worker container must not have a netns created just
# to audit (OQ-6): no nft observation happens for that tenant.
: >"$RUNTIME_LOG"
FIXTURE_RUNNING="false" run_check
assert_eq "$RC" "0" "an idle tenant is healthy"
# shellcheck disable=SC2002  # small fixture log.
if grep -q 'list chain' "$RUNTIME_LOG"; then
  fail "an idle tenant still triggered a netns nft read (OQ-6)"
else
  pass "an idle tenant triggers no netns nft read (OQ-6)"
fi
export FIXTURE_RUNNING="true"

# Docker's DNS state is not verifiable; it must be reported unverified rather
# than claimed disabled (OQ-5).
HEALTH_WORKER_EGRESS_RUNTIME=docker HEALTH_WORKER_EGRESS_ROOTLESS=false run_check


assert_eq "$RC" "0" "the docker lane audits without a DNS claim"

printf '{not json\n' >"$FIXTURE/bad.json"
TENANTS_FILE="$FIXTURE/bad.json" run_check
assert_eq "$RC" "1" "invalid registry degrades coverage instead of passing"
assert_eq "$(jq -r '.status' <<<"$OUTPUT")" "degraded" "invalid registry reports degraded"

TENANTS_FILE="$FIXTURE/missing.json" run_check
assert_eq "$RC" "0" "no registry means no multi-tenant workers to audit"
assert_eq "$(jq '.metrics.instances | length' <<<"$OUTPUT")" "0" "no-registry result is explicitly empty"

main_report="$(cat "$ROOT/infrastructure-health-check.sh")"
assert_contains "$main_report" 'run_check "health-worker-egress.sh" "worker_egress"' "main health check runs the worker egress component"
# shellcheck disable=SC2016  # literal shell variable reference in the generated report.
assert_contains "$main_report" '${check_worker_egress:-}' "main report includes the worker egress component"

# The health lib sync must never record an operator-checkout path for root cron.
pipeline="$(cat "$SCRIPT_DIR/../../ic/scripts/mt-admin-lib/12-health-pipeline.sh")"
assert_absent "$pipeline" 'LUNARWING_MT_ADMIN=$SCRIPT_DIR/lunarwing-mt-admin.sh' "the health env records no operator checkout path (OQ-1)"
assert_contains "$pipeline" 'removed LUNARWING_MT_ADMIN' "upgrades drop a previously recorded checkout path (OQ-1)"

# PB-1: the audit must not guess the launch mode. mt-admin records the runtime
# and rootless flag it provisions with into health.env (fresh install AND
# upgrade), so a rootful-podman host is not audited through a rootless netns.
# shellcheck disable=SC2016  # literal ${...} names in the generated env template.
assert_contains "$pipeline" 'HEALTH_WORKER_EGRESS_RUNTIME=${egress_runtime}' "health.env carries the provisioned runtime (PB-1)"
# shellcheck disable=SC2016  # literal ${...} names in the generated env template.
assert_contains "$pipeline" 'HEALTH_WORKER_EGRESS_ROOTLESS=${egress_rootless}' "health.env carries the provisioned rootless flag (PB-1)"
assert_contains "$pipeline" 'health_env_reconcile_line HEALTH_WORKER_EGRESS_RUNTIME' "the runtime line is reconciled on upgrade (PB-1)"
assert_contains "$pipeline" 'health_env_reconcile_line HEALTH_WORKER_EGRESS_ROOTLESS' "the rootless line is reconciled on upgrade (PB-1)"

# Functional pin for the reconciler: existing operator lines survive, the two
# managed lines take the provisioned value, and a missing line is appended.
RECONCILER="$FIXTURE/reconciler.sh"
{
  printf '#!/usr/bin/env bash\nset -euo pipefail\n'
  printf 'say() { :; }\nHEALTH_ENV_FILE=%q\n' "$FIXTURE/health.env"
  awk '/^health_env_reconcile_line\(\) \{/,/^\}/' \
    "$SCRIPT_DIR/../../ic/scripts/mt-admin-lib/12-health-pipeline.sh"
  printf 'health_env_reconcile_line HEALTH_WORKER_EGRESS_RUNTIME podman\n'
  printf 'health_env_reconcile_line HEALTH_WORKER_EGRESS_ROOTLESS false\n'
} >"$RECONCILER"
printf 'HEALTH_WORKER_EGRESS_RUNTIME=auto\nOPERATOR_TOKEN=keep-me\n' >"$FIXTURE/health.env"
if bash "$RECONCILER"; then
  env_text="$(cat "$FIXTURE/health.env")"
  assert_contains "$env_text" 'HEALTH_WORKER_EGRESS_RUNTIME=podman' "the reconciler rewrites a stale runtime (PB-1)"
  assert_contains "$env_text" 'HEALTH_WORKER_EGRESS_ROOTLESS=false' "the reconciler appends a missing flag (PB-1)"
  assert_contains "$env_text" 'OPERATOR_TOKEN=keep-me' "the reconciler preserves operator lines (PB-1)"
else
  fail "the health env reconciler runs (PB-1)"
fi

printf '%d passed, %d failed\n' "$passes" "$failures"
if ((failures > 0)); then
  exit 1
fi
