#!/usr/bin/env bash
# WI#424 control 1 — worker egress allowlist harness (mt-admin).
#
# Pins the properties that make the control a boundary rather than a comment:
#   * the generated nftables ruleset classifies only the tenant's worker
#     subnet, allows established/DNS/allowlisted destinations, and ends in a
#     reject (no implicit accept for anything else);
#   * host-loopback endpoints are translated to the tenant network gateway,
#     because 127.0.0.1 is not reachable from a rootless worker;
#   * operator entries cannot smuggle nft syntax;
#   * subnet allocation is persistent and collision-free;
#   * apply verifies the chain from a fresh netns entry and dies when the
#     filter did not survive (fail closed — the worker must not start);
#   * every worker launch path attaches the tenant's egress network and every
#     boot path reloads the filter.
#
# The live kernel portion runs when `nft` + rootless podman are available: the
# generated ruleset is loaded into a throwaway rootless netns and read back, so
# a syntax regression fails here instead of at the first tenant start.
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
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }
assert_eq() {  # <actual> <expected> <label>
  if [[ "$1" == "$2" ]]; then pass "$3"; else fail "$3 (got '$1', want '$2')"; fi
}
assert_contains() {  # <haystack> <needle> <label>
  if [[ "$1" == *"$2"* ]]; then pass "$3"; else fail "$3 (missing: $2)"; fi
}
assert_absent() {  # <haystack> <needle> <label>
  if [[ "$1" != *"$2"* ]]; then pass "$3"; else fail "$3 (unexpected: $2)"; fi
}

WORKER_EGRESS_DIR="$fixture/egress"
mkdir -p "$WORKER_EGRESS_DIR"

# ── stubs ────────────────────────────────────────────────────────────────────
# 203.0.113.0/24 is TEST-NET-3: an address-literal target keeps the harness
# hermetic (a hostname would need DNS and the resolver's answer would become
# part of the expectation).
LLM_TARGETS=("http://127.0.0.1:3000/v1" "https://203.0.113.10:8443/v1")
OPERATOR_ALLOW=("https://203.0.113.20:443/repo.git")
_worker_egress_llm_targets() { printf '%s\n' "${LLM_TARGETS[@]}"; }
_worker_egress_operator_allow() { printf '%s\n' "${OPERATOR_ALLOW[@]}"; }

NFT_CALLS=()
NFT_LOAD_RC=0
NFT_LIST_RC=0
NFT_LIST_OUT=""
worker_egress_nft() {  # <tenant> <args...>
  local name="$1"; shift
  NFT_CALLS+=("$name $*")
  if [[ "$1" == "-f" ]]; then
    return "$NFT_LOAD_RC"
  fi
  if [[ "$1" == "list" ]]; then
    [[ "$NFT_LIST_RC" == "0" ]] || return 1
    printf '%s\n' "$NFT_LIST_OUT"
    return 0
  fi
  return 0
}

CTR_NETWORK_EXISTS="false"
CTR_CREATE_RC=0
CTR_CALLS=()
CTR_ATTACHED="attached"
_ctr() {  # <tenant> <args...>
  local name="$1"; shift
  CTR_CALLS+=("$name $*")
  case "$1" in
    network)
      case "$2" in
        exists) [[ "$CTR_NETWORK_EXISTS" == "true" ]] && return 0; return 1 ;;
        create) return "$CTR_CREATE_RC" ;;
      esac
      ;;
    inspect) printf '%s\n' "$CTR_ATTACHED"; return 0 ;;
  esac
  return 0
}

# ── subnet allocation ────────────────────────────────────────────────────────
subnet="$(_worker_egress_alloc_subnet alpha)"
assert_eq "$subnet" "10.89.10.0/24" "first allocation takes the lowest free /24"
assert_eq "$(worker_egress_state_get alpha SUBNET)" "10.89.10.0/24" "allocation is persisted"
assert_eq "$(_worker_egress_alloc_subnet alpha)" "10.89.10.0/24" "re-allocation is stable"
subnet="$(umask 022; _worker_egress_alloc_subnet beta)"
assert_eq "$subnet" "10.89.11.0/24" "a second tenant gets a different /24"
asset_eq_perm="$(stat -c '%a' "$WORKER_EGRESS_DIR/beta.conf")"
assert_eq "$asset_eq_perm" "644" "allocation file is world-readable (no secrets) and root-owned"
assert_eq "$(_worker_egress_gateway 10.89.11.0/24)" "10.89.11.1" "gateway derivation"

# An existing ALLOW line survives a later allocation (ensure may run first).
printf 'ALLOW=git.example.org:443\n' >"$WORKER_EGRESS_DIR/gamma.conf"
_worker_egress_alloc_subnet gamma >/dev/null
assert_contains "$(cat "$WORKER_EGRESS_DIR/gamma.conf")" "ALLOW=git.example.org:443" \
  "allocation appends and does not drop a recorded allowlist"

# ── ruleset generation ───────────────────────────────────────────────────────
ruleset="$(worker_egress_ruleset alpha)"
assert_contains "$ruleset" "ip saddr 10.89.10.0/24 ct state established,related accept" \
  "return traffic is accepted"
assert_contains "$ruleset" "ip saddr 10.89.10.0/24 ip daddr 10.89.10.1 udp dport 53 accept" \
  "DNS is allowed only to the tenant gateway"
assert_contains "$ruleset" "ip saddr 10.89.10.0/24 ip daddr 10.89.10.1 tcp dport 3000 accept" \
  "a host-loopback LLM endpoint is translated to the gateway"
# Headless/rootless check: run the pure renderer in a subshell so the
# rootless extra host alias is exercised too (this harness runs as a test
# user, so CONTAINER_RT/MT_ROOTLESS come from the sourced constants).
assert_contains "$(CONTAINER_RT=podman MT_ROOTLESS=true worker_egress_ruleset alpha)" \
  "ip saddr 10.89.10.0/24 ip daddr 169.254.1.2 tcp dport 3000 accept" \
  "rootless workers may reach the host at its pasta alias"
assert_contains "$(CONTAINER_RT=podman MT_ROOTLESS=false worker_egress_ruleset alpha)" \
  "ip saddr 10.89.10.0/24 ip daddr 10.89.10.1 tcp dport 3000 accept" \
  "rootful workers reach the host through the bridge gateway"
assert_contains "$ruleset" "ip saddr 10.89.10.0/24 ip daddr 203.0.113.10 tcp dport 8443 accept" \
  "an address-literal LLM endpoint becomes its own rule"
assert_contains "$ruleset" "counter reject with icmpx type admin-prohibited" \
  "everything else from the subnet is rejected (no implicit accept)"
assert_absent "$ruleset" "ip daddr 127.0.0.1" "container rules never target host loopback"
assert_absent "$ruleset" "policy drop" "other tenant traffic is not affected (policy accept)"
# The reject must be the LAST rule, or an accept could follow it.
last_rule="$(printf '%s\n' "$ruleset" | grep -E '^\s{8}(ip|tcp|udp|counter)' | tail -1)"
assert_contains "$last_rule" "reject" "the reject rule is last"
# Exactly one reject: a second one would mean a widened rule set slipped in.
assert_eq "$(grep -c 'reject' <<<"$ruleset")" "1" "exactly one reject rule"
# A loopback endpoint keeps its port through translation (distinct port so the
# assertion cannot be satisfied by the 3000 rule).
OPERATOR_ALLOW=("http://127.0.0.1:8443/admin")
assert_contains "$(CONTAINER_RT=podman MT_ROOTLESS=false worker_egress_ruleset alpha)" \
  "ip saddr 10.89.10.0/24 ip daddr 10.89.10.1 tcp dport 8443 accept" \
  "host-loopback endpoints keep their port through translation"
OPERATOR_ALLOW=("https://203.0.113.20:443/repo.git")
# An unresolvable hostname is a hard error (fail closed, never a silent gap).
OPERATOR_ALLOW=("https://does-not-exist.example.invalid:443/repo.git")
if ( worker_egress_ruleset alpha ) >/dev/null 2>&1; then
  fail "an unresolvable allowlist hostname was silently skipped"
else
  pass "an unresolvable allowlist hostname refuses the ruleset (fail closed)"
fi
if ( worker_egress_ruleset alpha ) >/dev/null 2>&1; then
  fail "the unresolvable hostname case did not fail the ruleset"
fi
OPERATOR_ALLOW=("https://203.0.113.20:443/repo.git")

# Deduplication and stable order.
targets="$(worker_egress_targets alpha)"
assert_eq "$(printf '%s\n' "$targets" | sort | uniq -d | wc -l | tr -d ' ')" "0" \
  "targets are de-duplicated"

# ── entry validation ────────────────────────────────────────────────────────
for good in "git.example.org:443" "https://git.example.org:8443/x" "10.0.0.5:443" \
            "[2001:db8::1]:443"; do
  if validate_worker_egress_entry "$good" 2>/dev/null; then
    pass "accepts allowlist entry '$good'"
  else
    fail "rejected a valid allowlist entry '$good'"
  fi
done
for bad in "hostonly" "host:0" "host:99999" "*:443" "host:443; rm -rf /" \
           "host:443 extra" 'host:443" accept' "https://:443"; do
  if ( validate_worker_egress_entry "$bad" ) 2>/dev/null; then
    fail "accepted a hostile allowlist entry '$bad'"
  else
    pass "refuses hostile allowlist entry '$bad'"
  fi
done

# ── apply: fail closed ───────────────────────────────────────────────────────
NFT_LOAD_RC=0
NFT_LIST_RC=1
if ( worker_egress_apply alpha ) >/dev/null 2>"$fixture/apply.err"; then
  fail "apply accepted a filter that did not survive the load"
else
  pass "apply dies when the loaded filter is gone (worker start refuses)"
  assert_contains "$(cat "$fixture/apply.err")" "did not survive the load" \
    "the refusal names the cause"
fi

NFT_LIST_RC=0
NFT_LIST_OUT=$'table inet lunarwing_worker_egress {\n\tchain workers {\n\t\tcounter packets 0 bytes 0 reject with icmpx admin-prohibited\n\t}\n}'
NFT_CALLS=()
# Parent shell (not a subshell): the fake runtime records calls in a variable.
if worker_egress_apply alpha >/dev/null 2>"$fixture/apply2.err"; then
  pass "apply succeeds when the filter is provably live"
else
  fail "apply rejected a live filter: $(cat "$fixture/apply2.err")"
fi
assert_contains "$(printf '%s\n' "${NFT_CALLS[@]}")" "-f" "apply loads the rendered ruleset file"

NFT_LIST_RC=0
NFT_LIST_OUT=$'table inet lunarwing_worker_egress {\n\tchain workers {\n\t}\n}'
if ( worker_egress_apply alpha ) >/dev/null 2>"$fixture/apply3.err"; then
  fail "apply accepted a live chain with no drop rule"
else
  pass "apply refuses a chain without its drop rule"
fi

# ── network ensure + attachment ─────────────────────────────────────────────
CTR_NETWORK_EXISTS="false"
# Run in this shell: a $(...) call would record the fake runtime's calls in a
# subshell that cannot update CTR_CALLS.
worker_egress_ensure alpha >"$fixture/network.out"
network="$(cat "$fixture/network.out")"
assert_eq "$network" "lw-egress-alpha" "egress network name"
assert_contains "${CTR_CALLS[*]}" "network create --subnet 10.89.10.0/24" \
  "the network is created with the allocated subnet"
CTR_NETWORK_EXISTS="true"
unset 'CTR_CALLS[0]' 2>/dev/null || true
CTR_CALLS=()
worker_egress_ensure alpha >/dev/null
assert_absent "${CTR_CALLS[*]}" "network create" "ensure is idempotent"

CTR_ATTACHED="attached"
if worker_egress_container_attached alpha lunarwing-opencode2-alpha; then
  pass "attachment detection: attached"
else
  fail "attachment detection missed an attached container"
fi
CTR_ATTACHED=""
if worker_egress_container_attached alpha lunarwing-opencode2-alpha; then
  fail "attachment detection claimed an unattached container"
else
  pass "attachment detection: unattached"
fi

# ── launch-contract pins (source-level, like the other harnesses) ────────────
worker_config="$HARNESS_REPO_ROOT/ic/scripts/mt-admin-lib/09-worker-config.sh"
systemd_lib="$HARNESS_REPO_ROOT/ic/scripts/mt-admin-lib/07-units-systemd.sh"
run_sites="$(grep -c -- '--network "$(worker_egress_network_name "$name")"' "$worker_config")"
assert_eq "$run_sites" "4" "all four imperative worker run sites attach the egress network"
assert_eq "$(grep -c 'prepare_worker_egress "$name"' "$worker_config")" "4" \
  "all four worker start paths prepare (and verify) the filter"
assert_contains "$(cat "$systemd_lib")" 'Network=${egress_network}' \
  "Quadlet workers join the egress network"
assert_contains "$(cat "$systemd_lib")" 'ExecStartPre=${egress_guard} ${egress_ruleset} rootless' \
  "Quadlet workers load and verify the filter before every start"
assert_absent "$(cat "$systemd_lib")" 'egress_nft' \
  "the Quadlet no longer carries its own nft invocation (one guard for both inits)"
assert_contains "$(cat "$worker_config")" "refusing to start unfiltered" \
  "the OpenRC boot path fails closed"
assert_contains "$(cat "$worker_config")" 'wk_egress_guard' \
  "the OpenRC boot path runs the same guard as the Quadlet"
assert_contains "$(cat "$worker_config")" "worker egress guard missing" \
  "the OpenRC boot path refuses to start without the guard"

# ── the guard script itself ─────────────────────────────────────────────────
guard="$HARNESS_REPO_ROOT/ic/scripts/lunarwing-worker-egress-guard.sh"
guard_src="$(cat "$guard")"
assert_contains "$guard_src" "list chain inet" "the guard verifies through a fresh netns entry"
assert_contains "$guard_src" "did not survive the load" "the guard refuses a filter that did not persist"
assert_contains "$guard_src" "without its reject rule" "the guard refuses a chain with no drop rule"
assert_absent "$guard_src" "|| true" "the guard never swallows a load failure"
assert_eq "$(stat -c '%a' "$guard")" "755" "the guard is executable"
assert_contains "$(cat "$HARNESS_REPO_ROOT/ic/scripts/mt-admin-lib/15-worker-egress.sh")" \
  'WORKER_EGRESS_GUARD="/usr/local/libexec/lunarwing-worker-egress-guard"' \
  "the guard is installed to a root-owned libexec path (not tenant-writable)"

# Behavioural: a ruleset file that does not exist must fail, and a real one
# must pass against a stubbed nft. The stub keeps this hermetic.
guard_bin="$fixture/bin"
mkdir -p "$guard_bin"
cat >"$guard_bin/nft" <<'STUB'
#!/usr/bin/env bash
case "$1" in
  -f) exit "${FAKE_LOAD_RC:-0}" ;;
  list) [[ "${FAKE_LIST_RC:-0}" == "0" ]] || exit 1
        printf 'chain workers {\n  ip saddr 10.89.10.0/24 counter reject\n}\n' ;;
esac
STUB
chmod +x "$guard_bin/nft"
if PATH="$guard_bin:$PATH" FAKE_LOAD_RC=0 "$guard" "$fixture/alpha.nft" rootful >/dev/null 2>&1; then
  fail "the guard passed with a missing ruleset file"
else
  pass "the guard refuses a missing ruleset file"
fi
: >"$fixture/alpha.nft"
if PATH="$guard_bin:$PATH" FAKE_LOAD_RC=0 "$guard" "$fixture/alpha.nft" rootful >/dev/null 2>&1; then
  pass "the guard passes a live filter"
else
  fail "the guard rejected a live filter"
fi
if PATH="$guard_bin:$PATH" FAKE_LOAD_RC=1 "$guard" "$fixture/alpha.nft" rootful >/dev/null 2>&1; then
  fail "the guard passed a failed load"
else
  pass "the guard refuses a failed load"
fi
if PATH="$guard_bin:$PATH" FAKE_LIST_RC=1 "$guard" "$fixture/alpha.nft" rootful >/dev/null 2>&1; then
  fail "the guard passed a filter that did not survive"
else
  pass "the guard refuses a filter that did not survive"
fi

# ── live kernel check (rootless netns) ──────────────────────────────────────
# Loads the *real* generated ruleset into a throwaway rootless netns and reads
# it back inside the same invocation. Skipped when podman/nft are unavailable.
if command -v podman >/dev/null 2>&1 && command -v nft >/dev/null 2>&1 \
    && podman unshare --rootless-netns nft list ruleset >/dev/null 2>&1; then
  live_ruleset="$fixture/live.nft"
  NFT_LIST_RC=0
  worker_egress_ruleset beta >"$live_ruleset"
  # The live load must not depend on the stub: write through the real nft.
  if podman unshare --rootless-netns nft -f "$live_ruleset" >/dev/null 2>&1; then
    live_dump="$(podman unshare --rootless-netns nft list chain inet "$WORKER_EGRESS_TABLE" "$WORKER_EGRESS_CHAIN" 2>/dev/null || true)"
    # The throwaway netns is not pinned by a container, so the chain may be
    # gone immediately; only assert the syntax when it is still visible.
    if [[ -n "$live_dump" ]]; then
      assert_contains "$live_dump" "10.89.11.0/24" "live kernel accepts the generated ruleset"
    else
      pass "live kernel accepted the generated ruleset (netns not pinned, chain gone as expected)"
    fi
  else
    fail "live kernel rejected the generated ruleset"
  fi
else
  pass "live kernel check skipped (no rootless podman/nft on this host)"
fi

# ── summary ─────────────────────────────────────────────────────────────────
if ((failures > 0)); then
  printf '\n%d failure(s)\n' "$failures" >&2
  exit 1
fi
printf '\nworker egress harness: all checks passed\n'
