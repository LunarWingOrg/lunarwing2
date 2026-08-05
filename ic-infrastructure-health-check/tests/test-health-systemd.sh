#!/usr/bin/env bash
# F3 per-tenant classification tests for health-systemd.sh.
#
# Self-contained mock (systemctl/sudo/id) driven by a unit-state directory — no
# real systemd bus and no real services are touched. Validates the F3 rework:
#   - a not-started/disabled unit on a not-started tenant -> `skipped`
#   - a down unit on a STARTED tenant (primary daemon active) -> `critical`
#     (F3-A; covers `generated` Quadlet pg/worker units that are never "enabled")
#   - crashed (failed / auto-restart) -> `critical` regardless of enable-state
#   - enabled-but-down OR inconclusive enable-state -> `critical` (fail loud, F3-B)
#
# Each unit's state file holds five space-separated fields:
#   <LoadState> <ActiveState> <SubState> <NRestarts> <UnitFileState>
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/lib.sh
source "$DIR/lib.sh"
HS="$DIR/../health-systemd.sh"

mk_hs_mock() {
    local sb="$1" bin="$1/bin" sd="$1/units"
    mkdir -p "$bin" "$sd"
    # Mock systemctl: serves unit state from $HS_UNIT_DIR. `*) exit 0` covers any
    # subcommand we don't model. show returns props in the ORDER requested (the
    # caller asks for LoadState/ActiveState/SubState/NRestarts together, and asks
    # for UnitFileState/ActiveState separately), so no canonical-order coupling.
    cat > "$bin/systemctl" <<'EOF'
#!/usr/bin/env bash
ROOT="${HS_UNIT_DIR:?}"
SD="$ROOT"
[ -n "${HS_TENANT_USER:-}" ] && [ -d "$ROOT/$HS_TENANT_USER" ] && SD="$ROOT/$HS_TENANT_USER"
sub="${1:-}"; shift || true
[ -n "${HS_FAIL_USER:-}" ] && [ "${HS_TENANT_USER:-}" = "$HS_FAIL_USER" ] && exit 1
case "$sub" in
  --user) exec "$0" "$@" ;;
  list-unit-files)
    [ -n "${HS_FAIL_ENUM_USER:-}" ] && [ "${HS_TENANT_USER:-}" = "$HS_FAIL_ENUM_USER" ] && exit 1
    pats=(); for a in "$@"; do case "$a" in --no-legend|--user|--type=service) ;; *) pats+=("$a");; esac; done
    globby=false; for p in "${pats[@]}"; do [[ "$p" == *'*'* ]] && globby=true; done
    if $globby || [ ${#pats[@]} -eq 0 ]; then
      for f in "$SD"/*.service; do [ -e "$f" ] || continue; printf '%s generated\n' "$(basename "$f")"; done
    else
      for p in "${pats[@]}"; do [ -e "$SD/$p" ] || exit 1; printf '%s generated\n' "$p"; done
    fi ;;
  status) [ -e "$SD/${1:-}" ] ;;
  show)
    props=(); unit=""
    while [ $# -gt 0 ]; do case "$1" in --value|--user) ;; -p) props+=("$2"); shift;; -*) ;; *) unit="$1";; esac; shift; done
    if [ -n "${HS_FAIL_DAEMON_USER:-}" ] && [ "${HS_TENANT_USER:-}" = "$HS_FAIL_DAEMON_USER" ] \
      && [ "$unit" = "lunarwing-${HS_TENANT_USER}.service" ] && [ "${props[*]}" = "ActiveState" ]; then exit 1; fi
    if [ -n "${HS_FAIL_UFS_USER:-}" ] && [ "${HS_TENANT_USER:-}" = "$HS_FAIL_UFS_USER" ] \
      && [ "${props[*]}" = "UnitFileState" ]; then exit 1; fi
    if [ -e "$SD/$unit" ]; then read -r LS AS SS NR UF < "$SD/$unit"; else LS=""; AS=""; SS=""; NR=""; UF=""; fi
    for p in "${props[@]}"; do case "$p" in
      LoadState) printf '%s\n' "$LS";; ActiveState) printf '%s\n' "$AS";; SubState) printf '%s\n' "$SS";;
      NRestarts) printf '%s\n' "${NR:-0}";; UnitFileState) printf '%s\n' "$UF";; *) printf '\n';;
    esac; done ;;
  *) exit 0 ;;
esac
EOF
    # Mock sudo: drop -n, -u <user>, env, VAR=VAL, exec the rest (same shape as
    # health-systemd's `sudo -n -u <u> env XDG_RUNTIME_DIR=.. systemctl --user ..`).
    cat > "$bin/sudo" <<'EOF'
#!/usr/bin/env bash
a=("$@"); i=0
user=""
while [ $i -lt ${#a[@]} ]; do case "${a[$i]}" in -n) ;; -u) i=$((i+1)); user="${a[$i]}" ;; env) ;; *=*) ;; -*) break ;; *) break ;; esac; i=$((i+1)); done
export HS_TENANT_USER="$user"
exec "${a[@]:$i}"
EOF
    # Mock id: tenant uid resolves to a fixed value (only used to build the XDG path
    # for the mocked --user bus, which the mock ignores).
    cat > "$bin/id" <<'EOF'
#!/usr/bin/env bash
[ "${1:-}" = "-u" ] && {
  [ -n "${HS_MISSING_USER:-}" ] && [ "${2:-}" = "$HS_MISSING_USER" ] && exit 1
  echo 1000
  exit 0
}
exec /usr/bin/id "$@"
EOF
    cat > "$bin/podman" <<'EOF'
#!/usr/bin/env bash
ROOT="${HS_UNIT_DIR:?}"
user="${HS_TENANT_USER:-}"
container="${@: -1}"
cat "$ROOT/$user/$container.container-health" 2>/dev/null
EOF
    chmod +x "$bin"/*
}

unit_set() { printf '%s\n' "$3" > "$1/units/$2"; }   # <sb> <unit> "<ls> <as> <ss> <nr> <uf>"
unit_set_user() { mkdir -p "$1/units/$2"; printf '%s\n' "$4" > "$1/units/$2/$3"; }
container_health_user() { mkdir -p "$1/units/$2"; printf '%s\n' "$4" > "$1/units/$2/$3.container-health"; }

run_hs() {  # <sb> <tenant> <user> [weechat:true|false|absent] -> health-systemd.sh JSON on stdout
    local sb="$1" t="$2" u="$3" w="${4:-absent}"
    if [[ "$w" == absent ]]; then
        jq -n --arg t "$t" --arg u "$u" '{tenants:{($t):{user:$u}}}' > "$sb/ports.json"
    else
        jq -n --arg t "$t" --arg u "$u" --argjson w "$w" '{tenants:{($t):{user:$u, enable_weechat:$w}}}' > "$sb/ports.json"
    fi
    env PATH="$sb/bin:$PATH" HS_UNIT_DIR="$sb/units" \
        LUNARWING_SERVICE_MANAGER=systemd LUNARWING_TENANTS_FILE="$sb/ports.json" \
        bash "$HS" 2>/dev/null
}
run_hs_registry() {  # <sb> [ENV=VALUE ...] -> health-systemd.sh JSON on stdout
    local sb="$1"; shift
    env PATH="$sb/bin:$PATH" HS_UNIT_DIR="$sb/units" \
        LUNARWING_SERVICE_MANAGER=systemd LUNARWING_TENANTS_FILE="$sb/ports.json" \
        "$@" bash "$HS" 2>/dev/null
}
write_two_tenant_registry() {
    jq -n '{tenants:{acme:{user:"acme"},beta:{user:"beta"}}}' > "$1/ports.json"
}
ustat() { jq -r --arg n "$2" '(.metrics.units[]?|select(.name==$n)|.status) // "ABSENT"' <<<"$1"; }
oall()  { jq -r '.status' <<<"$1"; }

# ── A: not-started tenant (primary daemon inactive+disabled) ─────────────────
echo "=== Section A: not-started tenant -> skipped, overall healthy ==="
a="$(sb)"; mk_hs_mock "$a"
unit_set "$a" lunarwing-acme.service        "loaded inactive dead 0 disabled"
unit_set "$a" lunarwing-pg-acme.service     "loaded active running 0 generated"
unit_set "$a" lunarwing-proxy-acme.service  "loaded inactive dead 0 disabled"
oA="$(run_hs "$a" acme acme)"
assert_eq "$(ustat "$oA" lunarwing-acme.service)"       "skipped" "A1: not-started daemon -> skipped"
assert_eq "$(ustat "$oA" lunarwing-pg-acme.service)"    "healthy" "A2: active pg -> healthy"
assert_eq "$(ustat "$oA" lunarwing-proxy-acme.service)" "skipped" "A3: not-started proxy -> skipped"
assert_eq "$(oall "$oA")"                               "healthy" "A4: overall healthy (no false critical)"

# ── B: started tenant (daemon active) with a down generated pg (F3-A) ────────
echo "=== Section B: down unit on a started tenant -> critical (F3-A) ==="
b="$(sb)"; mk_hs_mock "$b"
unit_set "$b" lunarwing-acme.service     "loaded active running 0 enabled"
unit_set "$b" lunarwing-pg-acme.service  "loaded inactive dead 0 generated"
oB="$(run_hs "$b" acme acme)"
assert_eq "$(ustat "$oB" lunarwing-acme.service)"    "healthy"  "B1: started daemon -> healthy"
assert_eq "$(ustat "$oB" lunarwing-pg-acme.service)" "critical" "B2: down generated pg on started tenant -> critical (F3-A)"
assert_eq "$(oall "$oB")"                            "critical" "B3: overall critical"

# ── C: crashed + enabled-down + unknown-enable -> critical (fail loud, F3-B) ─
echo "=== Section C: crashed / enabled-down / unknown -> critical (fail loud) ==="
c="$(sb)"; mk_hs_mock "$c"
unit_set "$c" lunarwing-acme.service       "loaded inactive dead 0 disabled"  # not-started daemon -> skipped
unit_set "$c" lunarwing-pg-acme.service    "loaded failed failed 0 generated" # crashed -> critical
unit_set "$c" xmpp-bridge-acme.service     "loaded inactive dead 0 enabled"   # enabled+down -> critical
unit_set "$c" lunarwing-proxy-acme.service "loaded inactive dead 0 "          # unknown enable -> critical
oC="$(run_hs "$c" acme acme)"
assert_eq "$(ustat "$oC" lunarwing-acme.service)"       "skipped"  "C1: not-started daemon -> skipped"
assert_eq "$(ustat "$oC" lunarwing-pg-acme.service)"    "critical" "C2: crashed (failed) -> critical"
assert_eq "$(ustat "$oC" xmpp-bridge-acme.service)"     "critical" "C3: enabled+down -> critical (fail loud)"
assert_eq "$(ustat "$oC" lunarwing-proxy-acme.service)" "critical" "C4: unknown enable-state -> critical (fail loud)"

# ── D: auto-restart (crash loop) and elevated restart count ──────────────────
echo "=== Section D: auto-restart -> critical; high restart count -> degraded ==="
d="$(sb)"; mk_hs_mock "$d"
unit_set "$d" lunarwing-acme.service       "loaded active running 0 enabled"        # started
unit_set "$d" xmpp-bridge-acme.service     "loaded activating auto-restart 0 enabled" # crash loop
unit_set "$d" lunarwing-proxy-acme.service "loaded active running 7 generated"       # flapping-ish
oD="$(run_hs "$d" acme acme)"
assert_eq "$(ustat "$oD" xmpp-bridge-acme.service)"     "critical" "D1: auto-restart -> critical"
assert_eq "$(ustat "$oD" lunarwing-proxy-acme.service)" "degraded" "D2: active w/ restart_count>=3 -> degraded"
assert_eq "$(oall "$oD")"                               "critical" "D3: overall critical (auto-restart dominates)"

# ── E: weechat-DISABLED tenant (enable_weechat=false) — down weechat -> skipped ──
# The render-units footgun scenario on systemd: optional weechat user units that
# cannot start must not read critical (and self-heal must not restart-storm them)
# once the operator disabled weechat for the tenant.
echo "=== Section E: weechat-disabled tenant -> down weechat units skipped ==="
e="$(sb)"; mk_hs_mock "$e"
unit_set "$e" lunarwing-acme.service                  "loaded active running 0 enabled"
unit_set "$e" lunarwing-weechat-acme.service          "loaded inactive dead 0 enabled"
unit_set "$e" lunarwing-weechat-adapter-acme.service  "loaded failed failed 0 enabled"
unit_set "$e" lunarwing-proxy-acme.service            "loaded inactive dead 0 enabled"
oE="$(run_hs "$e" acme acme false)"
assert_eq "$(ustat "$oE" lunarwing-weechat-acme.service)"         "skipped"  "E1: disabled tenant's down weechat -> skipped"
assert_eq "$(ustat "$oE" lunarwing-weechat-adapter-acme.service)" "skipped"  "E2: disabled tenant's failed weechat-adapter -> skipped (no restart-storm)"
assert_eq "$(ustat "$oE" lunarwing-proxy-acme.service)"           "critical" "E3: non-weechat down unit still critical (gate is weechat-only)"
assert_eq "$(oall "$oE")"                                         "critical" "E4: overall critical (driven by proxy, not weechat)"

# ── F: disabled weechat the ONLY thing down -> host healthy; running weechat unaffected ──
echo "=== Section F: disabled weechat only-down -> healthy; gate inert when enabled ==="
f="$(sb)"; mk_hs_mock "$f"
unit_set "$f" lunarwing-acme.service        "loaded active running 0 enabled"
unit_set "$f" lunarwing-weechat-acme.service "loaded inactive dead 0 enabled"
oF="$(run_hs "$f" acme acme false)"
assert_eq "$(ustat "$oF" lunarwing-weechat-acme.service)" "skipped" "F1: down weechat on disabled tenant -> skipped"
assert_eq "$(oall "$oF")"                               "healthy" "F2: overall healthy — the render-units footgun is closed"

f2="$(sb)"; mk_hs_mock "$f2"
unit_set "$f2" lunarwing-acme.service        "loaded active running 0 enabled"
unit_set "$f2" lunarwing-weechat-acme.service "loaded active running 0 enabled"
oF2="$(run_hs "$f2" acme acme false)"
assert_eq "$(ustat "$oF2" lunarwing-weechat-acme.service)" "healthy" "F3: running weechat on disabled tenant still classified normally"

f3="$(sb)"; mk_hs_mock "$f3"
unit_set "$f3" lunarwing-acme.service        "loaded active running 0 enabled"
unit_set "$f3" lunarwing-weechat-acme.service "loaded inactive dead 0 enabled"
oF3="$(run_hs "$f3" acme acme true)"
assert_eq "$(ustat "$oF3" lunarwing-weechat-acme.service)" "critical" "F4: flag true -> down weechat still critical (gate inert)"

f4="$(sb)"; mk_hs_mock "$f4"
unit_set "$f4" lunarwing-acme.service        "loaded active running 0 enabled"
unit_set "$f4" lunarwing-weechat-acme.service "loaded inactive dead 0 enabled"
oF4="$(run_hs "$f4" acme acme)"   # absent flag = pre-gate tenant entry
assert_eq "$(ustat "$oF4" lunarwing-weechat-acme.service)" "critical" "F5: flag absent -> down weechat still critical (backward compat)"

# ── F6: retired Nanocode unit remains visible but cannot degrade health ──────
f5="$(sb)"; mk_hs_mock "$f5"
unit_set "$f5" lunarwing-acme.service          "loaded active running 0 enabled"
unit_set "$f5" lunarwing-nanocode-acme.service "loaded failed failed 9 generated"
oF5="$(run_hs "$f5" acme acme)"
assert_eq "$(ustat "$oF5" lunarwing-nanocode-acme.service)" "skipped" "F6: retired Nanocode unit -> skipped"
assert_eq "$(jq -r '.metrics.units[] | select(.name=="lunarwing-nanocode-acme.service") | .extra.retired' <<<"$oF5")" "true" "F7: retired Nanocode unit remains explicit"
assert_eq "$(oall "$oF5")" "healthy" "F8: retired Nanocode unit cannot degrade host"

# ── G: malformed tenant registry -> degraded, never healthy-blank (ISSUE-007) ─
echo "=== Section G: malformed registry -> degraded (ISSUE-007) ==="
g="$(sb)"; mk_hs_mock "$g"
unit_set "$g" lunarwing-acme.service "loaded active running 0 enabled"
echo '{not valid json' > "$g/ports.json"
oG="$(env PATH="$g/bin:$PATH" HS_UNIT_DIR="$g/units" \
    LUNARWING_SERVICE_MANAGER=systemd LUNARWING_TENANTS_FILE="$g/ports.json" \
    bash "$HS" 2>/dev/null)"
assert_eq "$(oall "$oG")" "degraded" "G1: malformed registry -> degraded"
assert_contains "$oG" "not valid JSON" "G2: malformed registry issue surfaced"

# ── H: valid registry with zero tenants -> degraded, documented (ISSUE-007) ──
echo "=== Section H: empty-but-valid registry -> degraded (ISSUE-007) ==="
h="$(sb)"; mk_hs_mock "$h"
echo '{"tenants":{}}' > "$h/ports.json"
oH="$(env PATH="$h/bin:$PATH" HS_UNIT_DIR="$h/units" \
    LUNARWING_SERVICE_MANAGER=systemd LUNARWING_TENANTS_FILE="$h/ports.json" \
    bash "$HS" 2>/dev/null)"
assert_eq "$(oall "$oH")" "degraded" "H1: zero-tenant registry -> degraded (explicit documented status)"
assert_contains "$oH" "lists no tenants" "H2: zero-tenant issue surfaced"

# ── I: jq missing on an MT host -> degraded JSON, never healthy (ISSUE-007) ──
echo "=== Section I: missing jq -> degraded (ISSUE-007) ==="
i="$(sb)"; mk_hs_mock "$i"
echo '{"tenants":{"acme":{"user":"acme"}}}' > "$i/ports.json"
REAL_JQ="$(command -v jq)"
shim="$i/shim"; mkdir -p "$shim"
for tool in bash date cat awk grep sed sort head tail env chmod timeout; do
    src="$(command -v "$tool" 2>/dev/null || true)"
    [ -n "$src" ] && ln -sf "$src" "$shim/$tool"
done
for tool in systemctl sudo id; do ln -sf "$i/bin/$tool" "$shim/$tool"; done
oI="$(env PATH="$shim" HS_UNIT_DIR="$i/units" \
    LUNARWING_SERVICE_MANAGER=systemd LUNARWING_TENANTS_FILE="$i/ports.json" \
    bash "$HS" 2>/dev/null)"; rcI=$?
assert_eq "$rcI" "1" "I1: missing jq -> degraded exit code"
assert_eq "$("$REAL_JQ" -r '.status' <<<"$oI")" "degraded" "I2: missing jq -> degraded JSON (not healthy)"
assert_contains "$oI" "discovery unavailable" "I3: missing jq issue surfaced"

# ── J: tenant on bus but zero observable units -> degraded (ISSUE-007) ────────
echo "=== Section J: zero observable tenant units -> degraded (ISSUE-007) ==="
j="$(sb)"; mk_hs_mock "$j"   # no unit files at all: tenant bus exposes nothing
oJ="$(run_hs "$j" acme acme)"
assert_eq "$(oall "$oJ")" "degraded" "J1: zero observable tenant units -> degraded"
assert_contains "$oJ" "no tenant units observable" "J2: observability gap issue surfaced"

# ── K: tenant bus probes fail (sudo denied) -> degraded (ISSUE-007) ───────────
echo "=== Section K: failed tenant probes -> degraded (ISSUE-007) ==="
k="$(sb)"; mk_hs_mock "$k"
unit_set "$k" lunarwing-acme.service "loaded active running 0 enabled"
cat > "$k/bin/sudo" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
chmod +x "$k/bin/sudo"
oK="$(run_hs "$k" acme acme)"
assert_eq "$(oall "$oK")" "degraded" "K1: failed tenant probes -> degraded"
assert_contains "$oK" "probe(s) failed" "K2: probe failure issue surfaced"

# ── L: one healthy tenant cannot hide a missing tenant OS user ───────────────
echo "=== Section L: mixed fleet missing OS user -> degraded ==="
l="$(sb)"; mk_hs_mock "$l"; write_two_tenant_registry "$l"
unit_set_user "$l" acme lunarwing-acme.service "loaded active running 0 enabled"
oL="$(run_hs_registry "$l" HS_MISSING_USER=beta)"
assert_eq "$(oall "$oL")" "degraded" "L1: healthy tenant cannot hide missing peer user"
assert_contains "$oL" "OS user beta not found" "L2: missing tenant user named"

# ── M: one healthy tenant cannot hide another tenant's dead user bus ─────────
echo "=== Section M: mixed fleet selective user-bus failure -> degraded ==="
m="$(sb)"; mk_hs_mock "$m"; write_two_tenant_registry "$m"
unit_set_user "$m" acme lunarwing-acme.service "loaded active running 0 enabled"
unit_set_user "$m" beta lunarwing-beta.service "loaded active running 0 enabled"
oM="$(run_hs_registry "$m" HS_FAIL_USER=beta)"
assert_eq "$(oall "$oM")" "degraded" "M1: healthy tenant cannot hide failed peer bus"
assert_contains "$oM" "tenant beta" "M2: failed peer tenant named"

# ── N: malformed per-tenant user field is a discovery failure ────────────────
echo "=== Section N: malformed tenant user -> degraded ==="
n="$(sb)"; mk_hs_mock "$n"
jq -n '{tenants:{acme:{user:"acme"},beta:{}}}' > "$n/ports.json"
unit_set_user "$n" acme lunarwing-acme.service "loaded active running 0 enabled"
oN="$(run_hs_registry "$n")"
assert_eq "$(oall "$oN")" "degraded" "N1: missing registry user cannot be skipped"
assert_contains "$oN" "not valid JSON" "N2: invalid tenant shape surfaced"

# ── O: initial daemon probe failure stays degraded despite later visibility ──
echo "=== Section O: initial daemon probe failure -> degraded ==="
o="$(sb)"; mk_hs_mock "$o"; write_two_tenant_registry "$o"
unit_set_user "$o" acme lunarwing-acme.service "loaded active running 0 enabled"
unit_set_user "$o" beta lunarwing-beta.service "loaded inactive dead 0 enabled"
oO="$(run_hs_registry "$o" HS_FAIL_DAEMON_USER=beta)"
assert_eq "$(oall "$oO")" "degraded" "O1: initial peer daemon probe failure degrades"
assert_contains "$oO" "daemon probe failed" "O2: initial probe failure surfaced"
assert_eq "$(ustat "$oO" lunarwing-beta.service)" "degraded" "O3: inactive unit with unknown daemon state degrades"

# ── P: failed enumeration remains degraded even when fallback units load ─────
echo "=== Section P: tenant unit enumeration failure -> degraded ==="
p="$(sb)"; mk_hs_mock "$p"; write_two_tenant_registry "$p"
unit_set_user "$p" acme lunarwing-acme.service "loaded active running 0 enabled"
unit_set_user "$p" beta lunarwing-beta.service "loaded active running 0 enabled"
unit_set_user "$p" beta xmpp-bridge-beta.service "loaded active running 0 enabled"
oP="$(run_hs_registry "$p" HS_FAIL_ENUM_USER=beta)"
assert_eq "$(oall "$oP")" "degraded" "P1: fallback units do not hide failed enumeration"
assert_contains "$oP" "unit enumeration failed" "P2: enumeration failure surfaced"

# ── Q: per-tenant zero visibility is checked, not only fleet total ───────────
echo "=== Section Q: mixed fleet zero-unit tenant -> degraded ==="
q="$(sb)"; mk_hs_mock "$q"; write_two_tenant_registry "$q"
unit_set_user "$q" acme lunarwing-acme.service "loaded active running 0 enabled"
mkdir -p "$q/units/beta"
oQ="$(run_hs_registry "$q")"
assert_eq "$(oall "$oQ")" "degraded" "Q1: healthy tenant cannot hide zero-unit peer"
assert_contains "$oQ" "tenant beta (beta): no user units observable" "Q2: zero-unit tenant named"

# ── R: unknown enable-state for a down unit remains fail-loud critical ────────
echo "=== Section R: enable-state probe failure -> critical ==="
r="$(sb)"; mk_hs_mock "$r"
jq -n '{tenants:{acme:{user:"acme"}}}' > "$r/ports.json"
unit_set "$r" lunarwing-acme.service "loaded inactive dead 0 enabled"
oR="$(run_hs_registry "$r" HS_FAIL_UFS_USER=acme)"
assert_eq "$(oall "$oR")" "critical" "R1: inconclusive enable-state for down unit fails loud"
assert_contains "$oR" "probe(s) failed" "R2: enable-state probe failure surfaced"

# ── S: LunarVision is a container unit; active+unhealthy must be critical ─────
echo "=== Section S: LunarVision container health ==="
s="$(sb)"; mk_hs_mock "$s"
jq -n '{tenants:{acme:{user:"acme"}}}' > "$s/ports.json"
unit_set_user "$s" acme lunarwing-acme.service "loaded active running 0 enabled"
unit_set_user "$s" acme lunarwing-vision-acme.service "loaded active running 0 generated"
container_health_user "$s" acme lunarwing-vision-acme unhealthy
oS="$(run_hs_registry "$s")"
assert_eq "$(ustat "$oS" lunarwing-vision-acme.service)" "critical" "S1: active but unhealthy LunarVision container is critical"
assert_eq "$(jq -r '.metrics.units[] | select(.name=="lunarwing-vision-acme.service") | .extra.container_health' <<<"$oS")" "unhealthy" "S2: LunarVision container health exposed"
assert_eq "$(oall "$oS")" "critical" "S3: LunarVision container failure drives overall status"

finish
