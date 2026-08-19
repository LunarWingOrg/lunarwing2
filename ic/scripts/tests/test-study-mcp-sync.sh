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
chown() { :; }
say() { :; }
selinuxenabled() { return 1; }
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

# A dirty checkout is never modified by the sync.
printf 'local edit\n' >> "$STUDY_NOTES_DEFAULT_HOME/study_notes_mcp.py"
if sync_study_notes_checkout "$STUDY_NOTES_DEFAULT_HOME" 2>/dev/null; then
  printf 'Study Notes sync unexpectedly modified a dirty checkout\n' >&2
  exit 1
fi
git -C "$STUDY_NOTES_DEFAULT_HOME" checkout -q -- study_notes_mcp.py

# A non-git directory is never modified by the sync.
mkdir -p "$fixture/not-git"
if sync_study_notes_checkout "$fixture/not-git" 2>/dev/null; then
  printf 'Study Notes sync unexpectedly accepted a non-git directory\n' >&2
  exit 1
fi

# provision_tenant_study_mcp syncs, then wires the tenant read-only.
mkdir -p "$(tenant_env_dir test)"
cat >"$(tenant_env_dir test)/lunarwing.env" <<EOF
GATEWAY_AUTH_TOKEN=test-token
LLM_API_KEY=test-key
EOF
provision_tenant_study_mcp test ""
grep -Fqx "LW2STUDYMCP_HOME=$STUDY_NOTES_DEFAULT_HOME" "$(tenant_env_dir test)/lunarwing.env"
if grep -q '^LW2STUDYMCP_ALLOW_WRITE=' "$(tenant_env_dir test)/lunarwing.env"; then
  printf 'Study Notes auto-configuration enabled core writes\n' >&2
  exit 1
fi

# A failing sync is contained: the caller survives and the tenant stays unwired.
mkdir -p "$(tenant_env_dir test2)"
cat >"$(tenant_env_dir test2)/lunarwing.env" <<EOF
GATEWAY_AUTH_TOKEN=test-token
LLM_API_KEY=test-key
EOF
if provision_tenant_study_mcp test2 "$fixture/not-git" 2>/dev/null; then
  printf 'Study Notes auto-configuration unexpectedly accepted a non-git home\n' >&2
  exit 1
fi
if grep -q '^LW2STUDYMCP_HOME=' "$(tenant_env_dir test2)/lunarwing.env"; then
  printf 'Study Notes auto-configuration wired a tenant from a failed sync\n' >&2
  exit 1
fi

# CLI surface pins: add-tenant(s) auto-configure flags and configure-study-mcp sync.
# (SCRIPT_DIR is redefined by the sourced script to ic/scripts.)
grep -Fq -- '--with-study-mcp' "$SCRIPT_DIR/lunarwing-mt-admin.sh"
grep -Fq -- '--study-mcp-home' "$SCRIPT_DIR/lunarwing-mt-admin.sh"
grep -Fq -- '--sync) study_sync="true"; shift ;;' "$SCRIPT_DIR/lunarwing-mt-admin.sh"
# The add_tenant wiring call moved to the lifecycle library in the mt-admin split.
grep -Fq 'provision_tenant_study_mcp "$name" "$study_mcp_home"' \
  "$SCRIPT_DIR/mt-admin-lib/04-tenant-lifecycle.sh"

printf 'Study MCP sync tests passed\n'
