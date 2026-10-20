#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
cleanup() { rm -rf "$fixture"; }
trap cleanup EXIT

tenant_home() { printf '%s/home/%s' "$fixture" "$1"; }
tenant_lw_root() { printf '%s/home/%s/lunarwing' "$fixture" "$1"; }
tenant_env_dir() { printf '%s/env' "$(tenant_lw_root "$1")"; }
tenant_state_dir() { printf '%s/state' "$(tenant_lw_root "$1")"; }
tenant_exists_in_registry() { return 0; }
require_root() { :; }
chown() { :; }
say() { :; }
selinuxenabled() { return 1; }
ensure_knowledge_source_scheduler() { :; }
sudo() {
  [[ "$1" == "-u" ]] || return 1
  shift 2
  "$@"
}

# Point the shared checkout and its upstream at fixture paths.
STUDY_NOTES_REPO="$fixture/origin"
STUDY_NOTES_DEFAULT_HOME="$fixture/shared/lw2studymcp"

git_commit() { # <repo> <message>
  git -C "$1" -c user.email=test@example.com -c user.name=test commit -q -m "$2"
}

# Upstream checkout carrying the files validate_study_notes_home() requires.
mkdir -p \
  "$STUDY_NOTES_REPO/agent-notes-by-agent/test-agent" \
  "$STUDY_NOTES_REPO/skills/study-notes"
touch \
  "$STUDY_NOTES_REPO/study_notes_mcp.py" \
  "$STUDY_NOTES_REPO/memorydb.py" \
  "$STUDY_NOTES_REPO/skills/study-notes/SKILL.md"
printf 'initial finding\n' > "$STUDY_NOTES_REPO/agent-notes-by-agent/test-agent/topic.md"
python3 - "$STUDY_NOTES_REPO/agent-notes.sqlite3" <<'PY'
import sqlite3
import sys

connection = sqlite3.connect(sys.argv[1])
connection.execute("CREATE TABLE notes (body TEXT NOT NULL)")
connection.execute("INSERT INTO notes VALUES ('upstream-v1')")
connection.commit()
connection.close()
PY
git -C "$STUDY_NOTES_REPO" init -q -b main
git -C "$STUDY_NOTES_REPO" add -A
git_commit "$STUDY_NOTES_REPO" initial

# A missing checkout is cloned from the configured upstream and validates.
sync_study_notes_checkout "$STUDY_NOTES_DEFAULT_HOME"
validate_study_notes_home "$STUDY_NOTES_DEFAULT_HOME"
test -f "$fixture/shared/.study-notes-sync.lock"
manifest="$STUDY_NOTES_DEFAULT_HOME/.git/lunarwing-study-notes-generation.json"
test -f "$manifest"
[[ "$(jq -r '.schema_version' "$manifest")" == "1" ]]
[[ "$(jq -r '.source_revision' "$manifest")" == "$(git -C "$STUDY_NOTES_DEFAULT_HOME" rev-parse HEAD)" ]]
python3 - "$STUDY_NOTES_DEFAULT_HOME/agent-notes.sqlite3" <<'PY'
import sqlite3
import sys

connection = sqlite3.connect(sys.argv[1])
assert connection.execute("SELECT body FROM notes").fetchall() == [("upstream-v1",)]
connection.close()
PY

# An existing clean checkout is fast-forwarded to the latest upstream.
mkdir -p "$STUDY_NOTES_REPO/agent-notes-by-agent/test-agent"
printf 'latest finding\n' > "$STUDY_NOTES_REPO/agent-notes-by-agent/test-agent/new-topic.md"
git -C "$STUDY_NOTES_REPO" add -A
git_commit "$STUDY_NOTES_REPO" latest
sync_study_notes_checkout "$STUDY_NOTES_DEFAULT_HOME"
grep -Fqx 'latest finding' \
  "$STUDY_NOTES_DEFAULT_HOME/agent-notes-by-agent/test-agent/new-topic.md"
[[ "$(jq -r '.source_revision' "$manifest")" == "$(git -C "$STUDY_NOTES_REPO" rev-parse HEAD)" ]]

# A dirty checkout is never modified by the sync and its old publication is
# invalidated so tenants cannot accept a generation while the source is unsafe.
printf 'local edit\n' >> "$STUDY_NOTES_DEFAULT_HOME/study_notes_mcp.py"
if sync_study_notes_checkout "$STUDY_NOTES_DEFAULT_HOME" 2>/dev/null; then
  printf 'Study Notes sync unexpectedly modified a dirty checkout\n' >&2
  exit 1
fi
[[ ! -e "$manifest" ]]

# A transient pull failure retains the last-good generation when HEAD still
# matches the recorded revision: the published manifest survives (with its
# original revision) and no stale last-good marker is left behind.
# (the dirty-checkout path above leaves no manifest, so restore the checkout
# and seed a last-good state from a fresh successful sync first)
git -C "$STUDY_NOTES_DEFAULT_HOME" checkout -q -- study_notes_mcp.py
sync_study_notes_checkout "$STUDY_NOTES_DEFAULT_HOME"
retained_revision="$(jq -r '.source_revision' "$manifest")"
# The checkout's origin must equal STUDY_NOTES_REPO (pinned below), so a
# transient upstream failure is simulated by making the origin repository
# itself unreachable while its configured URL stays the operator's.
mv "$STUDY_NOTES_REPO" "$fixture/origin-away"
if sync_study_notes_checkout "$STUDY_NOTES_DEFAULT_HOME" 2>/dev/null; then
  printf 'Study Notes sync unexpectedly succeeded with an unreachable upstream\n' >&2
  exit 1
fi
[[ -f "$manifest" ]] || {
  printf 'transient pull failure dropped the last-good generation manifest\n' >&2
  exit 1
}
[[ "$(jq -r '.source_revision' "$manifest")" == "$retained_revision" ]] || {
  printf 'retained manifest does not carry the last-good revision\n' >&2
  exit 1
}
[[ ! -e "$STUDY_NOTES_DEFAULT_HOME/.git/.lunarwing-study-notes-generation.last-good" ]] || {
  printf 'stale last-good marker left behind after restoration\n' >&2
  exit 1
}
mv "$fixture/origin-away" "$STUDY_NOTES_REPO"

# WI#445 LV-9: an existing checkout may only follow the operator's repo — the
# pull must be refused before it runs when the configured origin differs, and
# nothing in the checkout may change. (a rogue repo with real commits makes
# the refusal provably the origin gate, not an upstream reachability artifact)
rogue_origin="$fixture/rogue-origin"
git init -q -b main "$rogue_origin"
printf 'rogue finding\n' >"$rogue_origin/rogue-topic.md"
git -C "$rogue_origin" add -A
git_commit "$rogue_origin" rogue
git -C "$STUDY_NOTES_DEFAULT_HOME" remote set-url origin "$rogue_origin"
if rogue_err="$(sync_study_notes_checkout "$STUDY_NOTES_DEFAULT_HOME" 2>&1)"; then
  printf 'Study Notes sync pulled from a rogue origin\n' >&2
  exit 1
fi
[[ "$rogue_err" == *"origin does not match STUDY_NOTES_REPO"* ]] || {
  printf 'rogue-origin refusal did not name the origin mismatch:\n%s\n' "$rogue_err" >&2
  exit 1
}
[[ ! -e "$STUDY_NOTES_DEFAULT_HOME/agent-notes-by-agent/rogue-topic.md" ]] || {
  printf 'Study Notes sync wrote rogue-origin content into the checkout\n' >&2
  exit 1
}
[[ ! -e "$manifest" ]] || {
  printf 'rogue-origin refusal left a published generation behind\n' >&2
  exit 1
}
# Restore the operator remote and republish for the later scheduler pins.
git -C "$STUDY_NOTES_DEFAULT_HOME" remote set-url origin "$STUDY_NOTES_REPO"
sync_study_notes_checkout "$STUDY_NOTES_DEFAULT_HOME"
test -f "$manifest"

# A non-git directory is never modified by the sync.
mkdir -p "$fixture/not-git"
if sync_study_notes_checkout "$fixture/not-git" 2>/dev/null; then
  printf 'Study Notes sync unexpectedly accepted a non-git directory\n' >&2
  exit 1
fi

# provision_tenant_study_mcp syncs, then wires the tenant read-only.
mkdir -p "$(tenant_env_dir test)"
chmod 700 "$(tenant_env_dir test)"
cat >"$(tenant_env_dir test)/lunarwing.env" <<EOF
GATEWAY_AUTH_TOKEN=test-token
LLM_API_KEY=test-key
EOF
chmod 600 "$(tenant_env_dir test)/lunarwing.env"
provision_tenant_study_mcp test ""
grep -Fqx "LW2STUDYMCP_HOME=$STUDY_NOTES_DEFAULT_HOME" "$(tenant_env_dir test)/lunarwing.env"
if grep -q '^LW2STUDYMCP_ALLOW_WRITE=' "$(tenant_env_dir test)/lunarwing.env"; then
  printf 'Study Notes auto-configuration enabled core writes\n' >&2
  exit 1
fi

# The add-tenant path arms the one-shot seed and opt-in additive cadence.
grep -Fqx 'KNOWLEDGE_SEED_FROM_STUDY_MCP=1' "$(tenant_env_dir test)/lunarwing.env"
[[ "$(grep -c '^KNOWLEDGE_SEED_FROM_STUDY_MCP=' "$(tenant_env_dir test)/lunarwing.env")" -eq 1 ]]
grep -Fqx 'KNOWLEDGE_SYNC_ENABLED=true' "$(tenant_env_dir test)/lunarwing.env"
grep -Fqx 'KNOWLEDGE_SYNC_CADENCE_HOURS=6' "$(tenant_env_dir test)/lunarwing.env"
grep -Fqx 'KNOWLEDGE_SYNC_MAX_RECORDS_PER_RUN=500' "$(tenant_env_dir test)/lunarwing.env"
grep -Fqx 'KNOWLEDGE_SYNC_MAX_BYTES_PER_RUN=10485760' "$(tenant_env_dir test)/lunarwing.env"

# Arming never clobbers an explicit operator opt-out.
mkdir -p "$(tenant_env_dir test3)"
chmod 700 "$(tenant_env_dir test3)"
cat >"$(tenant_env_dir test3)/lunarwing.env" <<EOF
GATEWAY_AUTH_TOKEN=test-token
LLM_API_KEY=test-key
KNOWLEDGE_SEED_FROM_STUDY_MCP=0
EOF
chmod 600 "$(tenant_env_dir test3)/lunarwing.env"
provision_tenant_study_mcp test3 ""
grep -Fqx 'KNOWLEDGE_SEED_FROM_STUDY_MCP=0' "$(tenant_env_dir test3)/lunarwing.env"
[[ "$(grep -c '^KNOWLEDGE_SEED_FROM_STUDY_MCP=' "$(tenant_env_dir test3)/lunarwing.env")" -eq 1 ]]

# The operator-facing configure-study-mcp path (existing tenants) never arms
# the seed: only add-tenant does.
mkdir -p "$(tenant_env_dir test4)"
chmod 700 "$(tenant_env_dir test4)"
cat >"$(tenant_env_dir test4)/lunarwing.env" <<EOF
GATEWAY_AUTH_TOKEN=test-token
LLM_API_KEY=test-key
EOF
chmod 600 "$(tenant_env_dir test4)/lunarwing.env"
configure_study_mcp test4 "$STUDY_NOTES_DEFAULT_HOME" false ""
grep -Fqx "LW2STUDYMCP_HOME=$STUDY_NOTES_DEFAULT_HOME" "$(tenant_env_dir test4)/lunarwing.env"
if grep -q '^KNOWLEDGE_SEED_FROM_STUDY_MCP=' "$(tenant_env_dir test4)/lunarwing.env"; then
  printf 'configure-study-mcp unexpectedly armed the knowledge seed\n' >&2
  exit 1
fi
if grep -q '^KNOWLEDGE_SYNC_ENABLED=' "$(tenant_env_dir test4)/lunarwing.env"; then
  printf 'configure-study-mcp unexpectedly enabled knowledge sync\n' >&2
  exit 1
fi
set_tenant_knowledge_sync test4 true
grep -Fqx 'KNOWLEDGE_SYNC_ENABLED=true' "$(tenant_env_dir test4)/lunarwing.env"
set_tenant_knowledge_sync test4 false
grep -Fqx 'KNOWLEDGE_SYNC_ENABLED=false' "$(tenant_env_dir test4)/lunarwing.env"

# Manual refresh executes the tenant binary through the safe env-loader path,
# without reaching into either init system or restarting the daemon. The
# registry fixture is created here (not later) so every registry-touching call
# below — including run_tenant_knowledge_refresh — reads the fixture instead of
# the host's /etc/lunarwing/ports.json, which does not exist on CI runners.
PORTS_REGISTRY="$fixture/ports.json"
cat >"$PORTS_REGISTRY" <<'EOF'
{"version":16,"tenants":{"test":{},"test3":{},"test4":{}}}
EOF
export REFRESH_LOG="$fixture/refresh.log"
OPENRC_ENV_EXEC="$fixture/env-exec"
cat >"$OPENRC_ENV_EXEC" <<'EOF'
#!/usr/bin/env bash
[[ "$1" == "--env-file" && -n "$2" && "$3" == "--" ]] || exit 2
shift 3
exec "$@"
EOF
chmod +x "$OPENRC_ENV_EXEC"
mkdir -p "$(tenant_repo test4)/target/release"
cat >"$(tenant_repo test4)/target/release/lunarwing" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >"$REFRESH_LOG"
EOF
chmod +x "$(tenant_repo test4)/target/release/lunarwing"
run_tenant_knowledge_refresh test4 false
grep -Fqx -- '--no-onboard refresh-knowledge --json' "$REFRESH_LOG"

# A scheduler installation failure cannot commit a new tenant opt-in.
mkdir -p "$(tenant_env_dir test5)"
chmod 700 "$(tenant_env_dir test5)"
printf 'LW2STUDYMCP_HOME=%s\n' "$STUDY_NOTES_DEFAULT_HOME" \
  >"$(tenant_env_dir test5)/lunarwing.env"
if ( ensure_knowledge_source_scheduler() { die "scheduler unavailable"; }
     set_tenant_knowledge_sync test5 true ) 2>/dev/null; then
  printf 'knowledge sync unexpectedly enabled without a scheduler\n' >&2
  exit 1
fi
if grep -q '^KNOWLEDGE_SYNC_ENABLED=' "$(tenant_env_dir test5)/lunarwing.env"; then
  printf 'failed scheduler install left a knowledge sync opt-in\n' >&2
  exit 1
fi

# A failing sync is contained: the caller survives and the tenant stays unwired.
mkdir -p "$(tenant_env_dir test2)"
chmod 700 "$(tenant_env_dir test2)"
cat >"$(tenant_env_dir test2)/lunarwing.env" <<EOF
GATEWAY_AUTH_TOKEN=test-token
LLM_API_KEY=test-key
EOF
chmod 600 "$(tenant_env_dir test2)/lunarwing.env"
if provision_tenant_study_mcp test2 "$fixture/not-git" 2>/dev/null; then
  printf 'Study Notes auto-configuration unexpectedly accepted a non-git home\n' >&2
  exit 1
fi
if grep -q '^LW2STUDYMCP_HOME=' "$(tenant_env_dir test2)/lunarwing.env"; then
  printf 'Study Notes auto-configuration wired a tenant from a failed sync\n' >&2
  exit 1
fi

# The host scheduler entrypoint discovers enabled tenants and publishes their
# configured source without touching disabled tenant settings.
printf 'scheduled finding\n' > "$STUDY_NOTES_REPO/agent-notes-by-agent/test-agent/scheduled.md"
git -C "$STUDY_NOTES_REPO" add -A
git_commit "$STUDY_NOTES_REPO" scheduled
sync_enabled_study_notes_sources
grep -Fqx 'scheduled finding' \
  "$STUDY_NOTES_DEFAULT_HOME/agent-notes-by-agent/test-agent/scheduled.md"
[[ "$(jq -r '.source_revision' "$manifest")" == "$(git -C "$STUDY_NOTES_REPO" rev-parse HEAD)" ]]

# The host's enabled-value filter mirrors the daemon's parse_bool_env contract:
# only true/1 (case-insensitive) enable. Hand-edited yes/on must NOT enable the
# host refresh (the daemon would refuse to start), and any set-but-unsupported
# value is reported rather than silently skipped. The real
# sync_enabled_study_notes_sources runs against the fixture registry isolated
# above (a test-local override previously shadowed it and tripped shellcheck
# SC2218 by being defined after its first invocation). Each tenant gets its own
# fixture checkout so the enabled tenant's fast-forward and the skipped
# tenant's untouched checkout are both observable.
mkdir -p "$(tenant_env_dir yeson)" "$(tenant_env_dir mixedcase)"
chmod 700 "$(tenant_env_dir yeson)" "$(tenant_env_dir mixedcase)"
yeson_home="$fixture/shared/yeson"
mixedcase_home="$fixture/shared/mixedcase"
sync_study_notes_checkout "$yeson_home"
sync_study_notes_checkout "$mixedcase_home"
printf 'filter finding\n' > "$STUDY_NOTES_REPO/agent-notes-by-agent/test-agent/filter.md"
git -C "$STUDY_NOTES_REPO" add -A
git_commit "$STUDY_NOTES_REPO" filter
yeson_revision="$(jq -r '.source_revision' "$yeson_home/.git/lunarwing-study-notes-generation.json")"
printf 'LW2STUDYMCP_HOME=%s\nKNOWLEDGE_SYNC_ENABLED=yes\n' "$yeson_home" \
  >"$(tenant_env_dir yeson)/lunarwing.env"
printf 'LW2STUDYMCP_HOME=%s\nKNOWLEDGE_SYNC_ENABLED=True\n' "$mixedcase_home" \
  >"$(tenant_env_dir mixedcase)/lunarwing.env"
jq '.tenants.yeson = {} | .tenants.mixedcase = {}' "$PORTS_REGISTRY" >"$PORTS_REGISTRY.tmp"
mv "$PORTS_REGISTRY.tmp" "$PORTS_REGISTRY"
warnings="$(
  say() { printf 'SAY: %s\n' "$*"; }
  # WI#445: the enable-filter pins run with the root-owned anchor validator
  # stubbed so the fixture's user-writable non-default homes stay syncable;
  # the trust gate itself carries dedicated pins further down.
  darkirc_validate_root_owned_directory() { return 0; }
  sync_enabled_study_notes_sources
)"
grep -Fqx 'filter finding' \
  "$mixedcase_home/agent-notes-by-agent/test-agent/filter.md" || {
  printf 'case-insensitive true did not enable knowledge sync\n' >&2
  exit 1
}
[[ ! -e "$yeson_home/agent-notes-by-agent/test-agent/filter.md" ]] || {
  printf 'hand-edited yes/on unexpectedly enabled host knowledge sync\n' >&2
  exit 1
}
[[ "$(jq -r '.source_revision' "$yeson_home/.git/lunarwing-study-notes-generation.json")" == "$yeson_revision" ]] || {
  printf 'skipped tenant checkout unexpectedly fast-forwarded\n' >&2
  exit 1
}
[[ "$warnings" == *yeson* ]] || {
  printf 'unsupported enable value was not reported\n' >&2
  exit 1
}

# WI#445: the root-side sync never follows a tenant-chosen LW2STUDYMCP_HOME
# into root filesystem operations unless it is the operator default or its
# deepest existing ancestor chain is root-owned, symlink-free, and not
# group/other-writable. The gate is pinned at the unit level first (fixture
# trees live under user-writable directories, so an explicitly group-writable
# anchor makes the untrusted shape deterministic for every uid), then the
# hourly scheduler and the explicit refresh path are exercised through it.
mkdir -p "$fixture/untrusted-anchor"
chmod 0777 "$fixture/untrusted-anchor"
# The operator default is always trusted, even when the anchor rules would
# refuse the path (the equality check short-circuits before the walk).
(
  darkirc_validate_root_owned_directory() { return 1; }
  study_notes_sync_home_trusted "$STUDY_NOTES_DEFAULT_HOME"
) || { printf 'the default Study Notes home must always be trusted\n' >&2; exit 1; }
for untrusted_home in relative/path "/has space" "$fixture/untrusted-anchor/chosen"; do
  if study_notes_sync_home_trusted "$untrusted_home"; then
    printf 'Study Notes sync unexpectedly trusted home: %s\n' "$untrusted_home" >&2
    exit 1
  fi
done
# The anchor walk stops at the deepest existing ancestor, so the validator
# judges exactly the chain the sync would create missing directories under.
mkdir -p "$fixture/anchor-unit"
anchor="$(
  darkirc_validate_root_owned_directory() { printf '%s\n' "$1"; return 0; }
  study_notes_sync_home_trusted "$fixture/anchor-unit/a/b"
)"
[[ "$anchor" == "$fixture/anchor-unit" ]] || {
  printf 'anchor walk did not stop at the deepest existing ancestor: %s\n' "$anchor" >&2
  exit 1
}
# A "." or ".." component in the missing tail must be refused before the
# walk (pinned with the validator stubbed to accept, so only the component
# check can refuse; run-174 LV-7).
for dotted_home in "$fixture/anchor-unit/x/../../untrusted-anchor/chosen" "$fixture/anchor-unit/./chosen"; do
  if (
    darkirc_validate_root_owned_directory() { return 0; }
    study_notes_sync_home_trusted "$dotted_home"
  ); then
    printf 'Study Notes sync trusted a home with a dot component: %s\n' "$dotted_home" >&2
    exit 1
  fi
done
# A symlink in the chain stops the walk and the real validator refuses it
# (a dangling link must not be followed to its target's parent).
ln -s "$fixture/anchor-unit/missing-target" "$fixture/anchor-unit/dangling"
if study_notes_sync_home_trusted "$fixture/anchor-unit/dangling/deeper"; then
  printf 'Study Notes sync trusted a path through a symlink\n' >&2
  exit 1
fi

# One tenant's untrusted or malformed home must skip only that tenant: the
# hourly run survives (the shared default home still fast-forwards) and no
# root filesystem operation ever touches the untrusted paths.
mkdir -p "$(tenant_env_dir trustskip)" "$(tenant_env_dir badshape)"
chmod 700 "$(tenant_env_dir trustskip)" "$(tenant_env_dir badshape)"
printf 'LW2STUDYMCP_HOME=%s\nKNOWLEDGE_SYNC_ENABLED=true\n' \
  "$fixture/untrusted-anchor/trustskip-home" >"$(tenant_env_dir trustskip)/lunarwing.env"
printf 'LW2STUDYMCP_HOME=relative/path\nKNOWLEDGE_SYNC_ENABLED=true\n' \
  >"$(tenant_env_dir badshape)/lunarwing.env"
chmod 600 "$(tenant_env_dir trustskip)/lunarwing.env" "$(tenant_env_dir badshape)/lunarwing.env"
jq '.tenants.trustskip = {} | .tenants.badshape = {}' "$PORTS_REGISTRY" >"$PORTS_REGISTRY.tmp"
mv "$PORTS_REGISTRY.tmp" "$PORTS_REGISTRY"
printf 'trust finding\n' > "$STUDY_NOTES_REPO/agent-notes-by-agent/test-agent/trust.md"
git -C "$STUDY_NOTES_REPO" add -A
git_commit "$STUDY_NOTES_REPO" trust
trust_warnings="$(
  say() { printf 'SAY: %s\n' "$*"; }
  sync_enabled_study_notes_sources
)"
grep -Fqx 'trust finding' \
  "$STUDY_NOTES_DEFAULT_HOME/agent-notes-by-agent/test-agent/trust.md" || {
  printf 'an untrusted tenant home aborted the hourly sync for every tenant\n' >&2
  exit 1
}
[[ ! -e "$fixture/untrusted-anchor/trustskip-home" ]] || {
  printf 'the hourly sync ran root filesystem operations under an untrusted home\n' >&2
  exit 1
}
[[ "$trust_warnings" == *trustskip* && "$trust_warnings" == *badshape* ]] || {
  printf 'untrusted Study Notes homes were not reported\n' >&2
  exit 1
}

# The explicit operator path dies instead of syncing an untrusted home.
mkdir -p "$(tenant_env_dir refrefresh)"
chmod 700 "$(tenant_env_dir refrefresh)"
printf 'LW2STUDYMCP_HOME=%s\n' "$fixture/untrusted-anchor/refrefresh-home" \
  >"$(tenant_env_dir refrefresh)/lunarwing.env"
chmod 600 "$(tenant_env_dir refrefresh)/lunarwing.env"
jq '.tenants.refrefresh = {}' "$PORTS_REGISTRY" >"$PORTS_REGISTRY.tmp"
mv "$PORTS_REGISTRY.tmp" "$PORTS_REGISTRY"
if ( run_tenant_knowledge_refresh refrefresh true ) 2>/dev/null; then
  printf 'refresh-knowledge --sync-source accepted an untrusted home\n' >&2
  exit 1
fi
[[ ! -e "$fixture/untrusted-anchor/refrefresh-home" ]] || {
  printf 'refresh-knowledge --sync-source ran root operations under an untrusted home\n' >&2
  exit 1
}

# CLI surface pins: add-tenant(s) auto-configure flags and configure-study-mcp sync.
# (SCRIPT_DIR is redefined by the sourced script to ic/scripts.)
grep -Fq -- '--with-study-mcp' "$SCRIPT_DIR/lunarwing-mt-admin.sh"
grep -Fq -- '--study-mcp-home' "$SCRIPT_DIR/lunarwing-mt-admin.sh"
grep -Fq -- '--sync) study_sync="true"; shift ;;' "$SCRIPT_DIR/lunarwing-mt-admin.sh"
grep -Fq -- '--enable-knowledge-sync)' "$SCRIPT_DIR/lunarwing-mt-admin.sh"
grep -Fq 'refresh-knowledge)' "$SCRIPT_DIR/lunarwing-mt-admin.sh"
grep -Fq 'sync-knowledge-sources)' "$SCRIPT_DIR/lunarwing-mt-admin.sh"
# Both root-side sync entrypoints carry the WI#445 trust gate.
[[ "$(grep -Fc 'study_notes_sync_home_trusted "$home"' \
  "$SCRIPT_DIR/mt-admin-lib/14-knowledge-sync.sh")" -eq 2 ]] || {
  printf 'a root-side sync entrypoint lost the WI#445 trust gate\n' >&2
  exit 1
}
# The add_tenant wiring call moved to the lifecycle library in the mt-admin split.
grep -Fq 'provision_tenant_study_mcp "$name" "$study_mcp_home"' \
  "$SCRIPT_DIR/mt-admin-lib/04-tenant-lifecycle.sh"
# The seed is armed only inside provision_tenant_study_mcp (add-tenant path)
# and carried forward — never introduced — by env regeneration.
grep -Fq 'arm_tenant_knowledge_seed "$name"' \
  "$SCRIPT_DIR/mt-admin-lib/09-worker-config.sh"
grep -Fq 'knowledge_seed_flag="$(_env_existing "$env_snapshot" KNOWLEDGE_SEED_FROM_STUDY_MCP)"' \
  "$SCRIPT_DIR/mt-admin-lib/06-env-generation.sh" || { echo 'FAIL: env render no longer carries KNOWLEDGE_SEED_FROM_STUDY_MCP from the snapshot' >&2; exit 1; }

printf 'Study MCP sync tests passed\n'
