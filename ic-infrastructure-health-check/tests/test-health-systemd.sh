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
  --user)
    [ -n "${HS_CALL_LOG:-}" ] && printf 'user %s\n' "$*" >> "$HS_CALL_LOG"
    exec "$0" "$@" ;;
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
    EL=""; NE=""; [ -f "$SD/$unit.elapse" ] && read -r EL NE < "$SD/$unit.elapse"
    for p in "${props[@]}"; do case "$p" in
      LoadState) printf '%s\n' "$LS";; ActiveState) printf '%s\n' "$AS";; SubState) printf '%s\n' "$SS";;
      NRestarts) printf '%s\n' "${NR:-0}";; UnitFileState) printf '%s\n' "$UF";;
      LastTriggerUSec) printf '%s\n' "$EL";; NextElapseUSecRealtime) printf '%s\n' "$NE";;
      *) printf '\n';;
    esac; done ;;
  is-active)
    # WI#766: the tenant user manager lives on the SYSTEM bus. Keyed by uid so
    # it is independent of HS_TENANT_USER (which only the sudo mock sets).
    # Default is `active`; a `<ROOT>/user-manager/<uid>` fixture overrides it.
    unit="${1:-}"
    if [ "${unit#user@}" != "$unit" ]; then
      uid="${unit#user@}"; uid="${uid%.service}"
      if [ -f "$ROOT/user-manager/$uid" ]; then cat "$ROOT/user-manager/$uid"; else echo active; fi
      exit 0
    fi
    exit 1 ;;
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
  # Per-user uid fixture (WI#766: two tenants must not share user@<uid>).
  if [ -f "${HS_UNIT_DIR:?}/uid/${2:-}" ]; then cat "$HS_UNIT_DIR/uid/${2:-}"; exit 0; fi
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
    # Mock loginctl: linger state per user from <ROOT>/linger/<user>; absent user
    # (exit 1) leaves health-systemd to its LUNARWING_MT_LINGER_DIR seam.
    cat > "$bin/loginctl" <<'EOF'
#!/usr/bin/env bash
ROOT="${HS_UNIT_DIR:?}"
user=""
for a in "$@"; do case "$a" in -*) ;; *) user="$a";; esac; done
[ -n "$user" ] && [ -f "$ROOT/linger/$user" ] && { cat "$ROOT/linger/$user"; exit 0; }
exit 1
EOF
    chmod +x "$bin"/*
}

unit_set() { printf '%s\n' "$3" > "$1/units/$2"; }   # <sb> <unit> "<ls> <as> <ss> <nr> <uf>"
unit_set_user() { mkdir -p "$1/units/$2"; printf '%s\n' "$4" > "$1/units/$2/$3"; }
container_health_user() { mkdir -p "$1/units/$2"; printf '%s\n' "$4" > "$1/units/$2/$3.container-health"; }
# WI#766 fixtures: linger state per user, system-bus user-manager state per uid,
# and a per-user uid override (all tenants share the mock's default uid).
linger_set() { mkdir -p "$1/units/linger"; printf '%s\n' "$3" > "$1/units/linger/$2"; }
mgr_set()    { mkdir -p "$1/units/user-manager"; printf '%s\n' "$3" > "$1/units/user-manager/$2"; }
uid_set()    { mkdir -p "$1/units/uid"; printf '%s\n' "$3" > "$1/units/uid/$2"; }

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
run_hs_single() {  # <sb> [ENV=VALUE ...] -> single-node base-unit report
    local sb="$1"; shift
    env PATH="$sb/bin:$PATH" HS_UNIT_DIR="$sb/units" \
        LUNARWING_SERVICE_MANAGER=systemd LUNARWING_TENANTS_FILE="$sb/no-registry.json" \
        UNITS=lunarwing.service "$@" bash "$HS" 2>/dev/null
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

# ── S: Sidecar container health drives the generic init verdict ───────────────
echo "=== Section S: sidecar container health ==="
s="$(sb)"; mk_hs_mock "$s"
jq -n '{tenants:{acme:{user:"acme"}}}' > "$s/ports.json"
unit_set_user "$s" acme lunarwing-acme.service "loaded active running 0 enabled"
unit_set_user "$s" acme lunarwing-vision-acme.service "loaded active running 0 generated"
unit_set_user "$s" acme lunarwing-voice-acme.service "loaded active running 0 generated"
container_health_user "$s" acme lunarwing-vision-acme healthy
container_health_user "$s" acme lunarwing-voice-acme unhealthy
oS="$(run_hs_registry "$s")"
assert_eq "$(ustat "$oS" lunarwing-vision-acme.service)" "healthy" "S1: healthy LunarVision container stays healthy"
assert_eq "$(ustat "$oS" lunarwing-voice-acme.service)" "critical" "S2: active but unhealthy LunarVoice container is critical"
assert_eq "$(jq -r '.metrics.units[] | select(.name=="lunarwing-voice-acme.service") | .extra.container_health' <<<"$oS")" "unhealthy" "S3: LunarVoice container health exposed"
assert_eq "$(oall "$oS")" "critical" "S4: LunarVoice container failure drives overall status"

# ── S5: opencode2 container health is probed like the other workers (WI#180 F1) ──
echo "=== Section S5: opencode2 container health ==="
s5="$(sb)"; mk_hs_mock "$s5"
jq -n '{tenants:{acme:{user:"acme"}}}' > "$s5/ports.json"
unit_set_user "$s5" acme lunarwing-acme.service "loaded active running 0 enabled"
unit_set_user "$s5" acme lunarwing-opencode2-acme.service "loaded active running 0 generated"
container_health_user "$s5" acme lunarwing-opencode2-acme unhealthy
oS5="$(run_hs_registry "$s5")"
assert_eq "$(ustat "$oS5" lunarwing-opencode2-acme.service)" "critical" "S5: active but unhealthy opencode2 container is critical"
assert_eq "$(jq -r '.metrics.units[] | select(.name=="lunarwing-opencode2-acme.service") | .extra.container_health' <<<"$oS5")" "unhealthy" "S5: opencode2 container health exposed"
assert_eq "$(oall "$oS5")" "critical" "S5: opencode2 container failure drives overall status"

# ── T: single-node user scope probes the same bus remediation targets ────────
echo "=== Section T: single-node user-manager scope ==="
t="$(sb)"; mk_hs_mock "$t"
unit_set "$t" lunarwing.service "loaded active running 0 enabled"
: > "$t/systemctl-calls"
oT="$(run_hs_single "$t" SELF_HEAL_SYSTEMD_SCOPE=user HS_CALL_LOG="$t/systemctl-calls")"
assert_eq "$(ustat "$oT" lunarwing.service)" "healthy" "T1: user-scoped base unit is healthy"
if grep -q '^user ' "$t/systemctl-calls"; then
    ok "T2: base-unit probe uses systemctl --user"
else
    bad "T2: base-unit probe uses systemctl --user" "no user-bus call recorded"
fi

# ── U: active/waiting timer with a scheduled wake is healthy ─────────────────
echo "=== Section U: scheduled timer -> healthy ==="
u="$(sb)"; mk_hs_mock "$u"
unit_set "$u" lunarwing-mt-health.timer "loaded active waiting 0 enabled"
printf '1760000000000000 1760000900000000\n' > "$u/units/lunarwing-mt-health.timer.elapse"
oU="$(run_hs_single "$u" UNITS=lunarwing-mt-health.timer)"
assert_eq "$(ustat "$oU" lunarwing-mt-health.timer)" "healthy" "U1: timer with next elapse is healthy"
assert_eq "$(oall "$oU")" "healthy" "U2: overall healthy"
assert_eq "$(jq -r '.metrics.units[] | select(.name=="lunarwing-mt-health.timer") | .extra.next_elapse' <<<"$oU")" \
    "1760000900000000" "U3: next elapse surfaced in extra"

# ── V: active/waiting timer with NO next elapse -> degraded (never fires) ─────
echo "=== Section V: unscheduled timer -> degraded ==="
v="$(sb)"; mk_hs_mock "$v"
unit_set "$v" lunarwing-mt-health.timer "loaded active waiting 0 enabled"
printf '1760000000000000 0\n' > "$v/units/lunarwing-mt-health.timer.elapse"
oV="$(run_hs_single "$v" UNITS=lunarwing-mt-health.timer)"; rcV=$?
assert_eq "$(ustat "$oV" lunarwing-mt-health.timer)" "degraded" "V1: no next elapse -> degraded"
assert_eq "$rcV" "1" "V2: exit code 1 (degraded)"
assert_contains "$oV" "no next elapse scheduled" "V3: blind-spot issue surfaced"

v2="$(sb)"; mk_hs_mock "$v2"
unit_set "$v2" lunarwing-mt-health.timer "loaded active waiting 0 enabled"
# no .elapse file at all -> both timestamps empty (older systemd / missing props)
oV2="$(run_hs_single "$v2" UNITS=lunarwing-mt-health.timer)"
assert_eq "$(ustat "$oV2" lunarwing-mt-health.timer)" "degraded" "V4: empty next elapse -> degraded"

# ── W: timer mid-run (sub=running) is not flagged by the blind-spot check ─────
echo "=== Section W: running timer not flagged ==="
w="$(sb)"; mk_hs_mock "$w"
unit_set "$w" lunarwing-mt-health.timer "loaded active running 0 enabled"
printf '1760000000000000 0\n' > "$w/units/lunarwing-mt-health.timer.elapse"
oW="$(run_hs_single "$w" UNITS=lunarwing-mt-health.timer)"
assert_eq "$(ustat "$oW" lunarwing-mt-health.timer)" "healthy" "W1: running timer with next=0 not flagged (gate on waiting)"
assert_eq "$(oall "$oW")" "healthy" "W2: overall healthy"

# ── X: inactive timer remains critical (existing behavior unchanged) ──────────
echo "=== Section X: inactive timer -> critical ==="
x="$(sb)"; mk_hs_mock "$x"
unit_set "$x" lunarwing-mt-health.timer "loaded inactive dead 0 enabled"
oX="$(run_hs_single "$x" UNITS=lunarwing-mt-health.timer)"; rcX=$?
assert_eq "$(ustat "$oX" lunarwing-mt-health.timer)" "critical" "X1: inactive timer -> critical"
assert_eq "$rcX" "2" "X2: exit code 2 (critical)"

# ── Y: dead tenant user manager (WI#766) ─────────────────────────────────────
# A lingering tenant's user@<uid>.service down means every tenant unit is
# unobservable. That must surface as a per-tenant instance (so self-heal can
# build a systemd@<tenant> monitor-only target) and a named issue, not just as
# "zero units".
echo "=== Section Y: dead tenant user manager -> named instance (WI#766) ==="
y="$(sb)"; mk_hs_mock "$y"; write_two_tenant_registry "$y"
linger_set "$y" acme yes; linger_set "$y" beta yes
uid_set "$y" acme 1001; uid_set "$y" beta 1002
mgr_set "$y" 1001 failed; mgr_set "$y" 1002 active
unit_set_user "$y" beta lunarwing-beta.service "loaded active running 0 enabled"
oY="$(run_hs_registry "$y" HS_FAIL_USER=acme)"
assert_eq "$(jq -r '.metrics.instances[] | select(.tenant=="acme") | .status' <<<"$oY")" \
    "degraded" "Y1: dead manager -> degraded acme instance"
assert_eq "$(jq -r '.metrics.instances[] | select(.tenant=="acme") | .manager_state' <<<"$oY")" \
    "failed" "Y2: manager unit state surfaced"
assert_eq "$(jq -r '.metrics.instances[] | select(.tenant=="acme") | .manager_unit' <<<"$oY")" \
    "user@1001.service" "Y3: manager unit named"
assert_eq "$(jq -r '.metrics.instances[] | select(.tenant=="beta") | .status' <<<"$oY")" \
    "healthy" "Y4: healthy peer manager still reported"
assert_contains "$oY" "user@1001.service is failed" "Y5: dead manager named with its unit"
assert_contains "$oY" "systemctl start user@1001.service" "Y6: recovery command in the issue"
assert_eq "$(oall "$oY")" "degraded" "Y7: dead manager degrades the component"
assert_eq "$(ustat "$oY" lunarwing-beta.service)" "healthy" "Y8: peer tenant units still probed"

# ── Z: same dead manager on a NON-lingering tenant is not a tenant fault ─────
# A non-lingering account's user manager being down is normal and says nothing
# about the tenant (the same rule mt-admin's _user_manager_probe applies).
echo "=== Section Z: non-lingering tenant's manager is not judged (WI#766) ==="
z="$(sb)"; mk_hs_mock "$z"; write_two_tenant_registry "$z"
linger_set "$z" acme no
uid_set "$z" acme 1001; uid_set "$z" beta 1002
mgr_set "$z" 1001 failed; mgr_set "$z" 1002 active
oZ="$(run_hs_registry "$z" HS_FAIL_USER=acme)"
assert_absent "$oZ" "user manager" "Z1: non-lingering manager never reported"
assert_absent "$oZ" "user@1001.service" "Z2: no synthetic manager instance for non-lingering tenant"
assert_eq "$(jq -r '[.metrics.instances[]? | select(.tenant=="acme")] | length' <<<"$oZ")" \
    "0" "Z3: no instance record for a non-lingering tenant"

# ── AA: manager state inconclusive (no answer) -> unknown, never "dead" ──────
# Only a known down state proves the manager dead; an empty answer must not
# invent one, and the instance stays non-actionable.
echo "=== Section AA: inconclusive manager probe -> unknown instance ==="
aa="$(sb)"; mk_hs_mock "$aa"
linger_set "$aa" acme yes
uid_set "$aa" acme 1001
mgr_set "$aa" 1001 ""
unit_set_user "$aa" acme lunarwing-acme.service "loaded active running 0 enabled"
oAA="$(run_hs "$aa" acme acme)"
assert_eq "$(jq -r '.metrics.instances[] | select(.tenant=="acme") | .status' <<<"$oAA")" \
    "unknown" "AA1: no manager answer -> unknown instance"
assert_absent "$oAA" "user manager user@1000.service is" "AA2: unknown state never called dead"
assert_eq "$(oall "$oAA")" "healthy" "AA3: inconclusive manager alone does not degrade"

finish
