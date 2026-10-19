#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
wal_pid=""
wal_release=""
cleanup() {
  if [[ -n "$wal_pid" ]]; then
    [[ -z "$wal_release" ]] || touch "$wal_release"
    wait "$wal_pid" 2>/dev/null || true
  fi
  rm -rf "$fixture"
}
trap cleanup EXIT

tenant_home() { printf '%s/home/%s' "$fixture" "$1"; }
tenant_lw_root() { printf '%s/home/%s/lunarwing' "$fixture" "$1"; }
tenant_env_dir() { printf '%s/env' "$(tenant_lw_root "$1")"; }
tenant_quadlet_dir() { printf '%s/.config/containers/systemd' "$(tenant_home "$1")"; }
tenant_run_dir() { printf '%s/run' "$(tenant_lw_root "$1")"; }
ports_get() {
  case "$2" in
    opencode_wss) echo 19090 ;;
    opencode_health) echo 18443 ;;
    nanocode_wss) echo 29090 ;;
    nanocode_health) echo 28443 ;;
    *) return 1 ;;
  esac
}
chown_calls="$fixture/chown-calls"
chown() { printf '%s\n' "$*" >>"$chown_calls"; }
say() { :; }
selinuxenabled() { [[ "$selinux_active" == "true" ]]; }
chcon() { printf '%s\n' "$*" >> "$selinux_calls"; }
tenant_exists_in_registry() { return 0; }
sudo() {
  [[ "$1" == "-u" ]] || return 1
  shift 2
  "$@"
}
ensure_container_runtime() { :; }
_ensure_tenant_image() { return 0; }
ensure_init_system() { INIT_SYSTEM="openrc"; }
_register_worker_unit() { :; }
# WI#400: the opencode lane runs a root entrypoint (image USER 0:0) whose
# serve/tasks drop to the task uid, so the stubbed image user is 0:0.
container_image_user="0:0"
_ctr() {
  printf '%s\n' "$*" >> "$ctr_calls"
  if [[ "$2" == "image" && "$3" == "inspect" ]]; then
    printf '%s\n' "$container_image_user"
    return 0
  fi
  [[ "$2" != "inspect" ]]
}

study_home="$fixture/study-notes"
study_state="$(tenant_state_dir test)/study-notes"
mkdir -p \
  "$study_home/agent-notes-by-agent" \
  "$study_home/skills/study-notes" \
  "$(tenant_env_dir test)"
# The tenant-side atomic writer (WI#390) requires a tenant-private (0700) env
# dir for its 'private' parent policy, matching create_tenant_user on a real
# provision; the bare mkdir above lands at the umask default (0755).
chmod 0700 "$(tenant_env_dir test)"
touch \
  "$study_home/study_notes_mcp.py" \
  "$study_home/memorydb.py" \
  "$study_home/skills/study-notes/SKILL.md"
python3 - "$study_home/agent-notes.sqlite3" <<'PY'
import sqlite3
import sys

connection = sqlite3.connect(sys.argv[1])
connection.execute("CREATE TABLE notes (body TEXT NOT NULL)")
connection.commit()
connection.close()
PY
cat >"$(tenant_env_dir test)/lunarwing.env" <<EOF
GATEWAY_AUTH_TOKEN=test-token
LLM_API_KEY=test-key
OPENCODE_MODEL=stale-model
OPENCODE_MODEL=final-model
OPENCODE_BASE_URL=http://stale.example
OPENCODE_BASE_URL=http://final.example
EOF

# Failed seeding must leave no partial final or staging directory.
failed_state="$(tenant_state_dir test)/failed-study-notes"
cp() {
  return 1
}
if (initialize_study_notes_state test "$study_home" "$failed_state" >/dev/null 2>&1); then
  printf 'Study Notes partial-seed fixture unexpectedly succeeded\n' >&2
  exit 1
fi
unset -f cp
test ! -e "$failed_state"
if find "$(dirname "$failed_state")" -maxdepth 1 -name '.study-notes-state.*' -print -quit \
    | grep -q .; then
  printf 'Study Notes failed seed left staging directory\n' >&2
  exit 1
fi

# Keep a committed row only in the live WAL while state is seeded. A raw copy
# of agent-notes.sqlite3 would lose this row; SQLite's backup API must retain it.
wal_ready="$fixture/wal-ready"
wal_release="$fixture/wal-release"
python3 - "$study_home/agent-notes.sqlite3" "$wal_ready" "$wal_release" <<'PY' &
import sqlite3
import sys
import time
from pathlib import Path

database, ready, release = map(Path, sys.argv[1:])
connection = sqlite3.connect(database)
connection.execute("PRAGMA journal_mode=WAL")
connection.execute("PRAGMA wal_autocheckpoint=0")
connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
connection.execute("INSERT INTO notes VALUES ('committed-in-wal')")
connection.commit()
ready.touch()
while not release.exists():
    time.sleep(0.05)
connection.close()
PY
wal_pid="$!"
for _ in {1..100}; do
  [[ -f "$wal_ready" ]] && break
  sleep 0.05
done
test -f "$wal_ready"
test -s "$study_home/agent-notes.sqlite3-wal"

selinux_active=true
selinux_calls="$fixture/selinux-calls"
configure_study_mcp test "$study_home" true
grep -Fqx "LW2STUDYMCP_HOME=$study_home" "$(tenant_env_dir test)/lunarwing.env"
grep -Fqx "LW2STUDYMCP_STATE_HOME=$study_state" "$(tenant_env_dir test)/lunarwing.env"
grep -Fqx "LW2STUDYMCP_ALLOW_WRITE=1" "$(tenant_env_dir test)/lunarwing.env"
test -f "$study_state/agent-notes.sqlite3"
test -d "$study_state/agent-notes-by-agent"
python3 - "$study_state/agent-notes.sqlite3" <<'PY'
import sqlite3
import sys

connection = sqlite3.connect(sys.argv[1])
assert connection.execute("SELECT body FROM notes").fetchall() == [("committed-in-wal",)]
assert connection.execute("PRAGMA quick_check").fetchall() == [("ok",)]
connection.close()
PY
touch "$wal_release"
wait "$wal_pid"
wal_pid=""
grep -Fq -- "-R -t container_file_t -- $study_home" "$selinux_calls"
grep -Fq -- "-R -t container_file_t -- $study_state" "$selinux_calls"

ctr_calls="$fixture/container-calls"
CONTAINER_RT="podman"
MT_ROOTLESS="true"
start_tenant_opencode test
grep -Fq -- "-v $(tenant_run_dir test)/ssh-agent:/tmp/ssh-agent:ro,z" "$ctr_calls"
grep -Fqx -- "-R test:test $(tenant_lw_root test)/opencode-workspace" "$chown_calls"
grep -Fq -- "-v $study_home:/opt/lw2studymcp:ro,z" "$ctr_calls"
grep -Fq -- "-e LW2STUDYMCP_HOME=/opt/lw2studymcp" "$ctr_calls"
grep -Fq -- "-v $study_state:/var/lib/lw2studymcp:ro,z" "$ctr_calls"
grep -Fq -- "-e LW2STUDYMCP_STATE_HOME=/var/lib/lw2studymcp" "$ctr_calls"
grep -Fq -- "--userns $OPENCODE_USERNS" "$ctr_calls"
grep -Fq -- "--no-healthcheck" "$ctr_calls"
grep -Fq -- "-e OPENCODE_MODEL=final-model" "$ctr_calls"
grep -Fq -- "-e OPENCODE_BASE_URL=http://final.example" "$ctr_calls"
if grep -Eq -- 'stale-model|stale\.example' "$ctr_calls"; then
  printf 'OpenCode imperative launch retained stale duplicate env values\n' >&2
  exit 1
fi
if grep -q -- 'LW2STUDYMCP_ALLOW_WRITE' "$ctr_calls"; then
  printf 'Core Study Notes write settings leaked into OpenCode Podman command\n' >&2
  exit 1
fi

render_worker_quadlet test opencode 8443
opencode_unit="$(tenant_quadlet_dir test)/lunarwing-opencode-test.container"
grep -Fq "Volume=$(tenant_run_dir test)/ssh-agent:/tmp/ssh-agent:ro,z" "$opencode_unit"
grep -Fqx -- "-R test:test $(tenant_lw_root test)/opencode-workspace" "$chown_calls"
grep -Fq "Volume=$study_home:/opt/lw2studymcp:ro,z" "$opencode_unit"
grep -Fq "Environment=LW2STUDYMCP_HOME=/opt/lw2studymcp" "$opencode_unit"
grep -Fq "Volume=$study_state:/var/lib/lw2studymcp:ro,z" "$opencode_unit"
grep -Fq "Environment=LW2STUDYMCP_STATE_HOME=/var/lib/lw2studymcp" "$opencode_unit"
grep -Fq "UserNS=$OPENCODE_USERNS" "$opencode_unit"
grep -Fq "Environment=OPENCODE_MODEL=final-model" "$opencode_unit"
grep -Fq "Environment=OPENCODE_BASE_URL=http://final.example" "$opencode_unit"
if grep -Eq 'stale-model|stale\.example' "$opencode_unit"; then
  printf 'OpenCode quadlet retained stale duplicate env values\n' >&2
  exit 1
fi
if grep -q 'LW2STUDYMCP_ALLOW_WRITE' "$opencode_unit"; then
  printf 'Core Study Notes write settings leaked into OpenCode quadlet\n' >&2
  exit 1
fi

# Disabling core writes retains latest state for worker reads.
configure_study_mcp test "$study_home" false
grep -Fqx "LW2STUDYMCP_STATE_HOME=$study_state" "$(tenant_env_dir test)/lunarwing.env"
if grep -q '^LW2STUDYMCP_ALLOW_WRITE=' "$(tenant_env_dir test)/lunarwing.env"; then
  printf 'Study Notes write flag survived read-only reconfiguration\n' >&2
  exit 1
fi

: > "$ctr_calls"
start_tenant_opencode test
grep -Fq -- "-v $study_state:/var/lib/lw2studymcp:ro,z" "$ctr_calls"
grep -Fq -- "--userns $OPENCODE_USERNS" "$ctr_calls"
if grep -q -- 'LW2STUDYMCP_ALLOW_WRITE' "$ctr_calls"; then
  printf 'Study Notes write settings leaked into read-only Podman command\n' >&2
  exit 1
fi

render_worker_quadlet test opencode 8443
grep -Fq "Volume=$study_state:/var/lib/lw2studymcp:ro,z" "$opencode_unit"
grep -Fq "UserNS=$OPENCODE_USERNS" "$opencode_unit"
if grep -q 'LW2STUDYMCP_ALLOW_WRITE' "$opencode_unit"; then
  printf 'Study Notes write settings leaked into read-only quadlet\n' >&2
  exit 1
fi

# Rootful runtimes do not use Podman's rootless keep-id mapping.
: > "$ctr_calls"
MT_ROOTLESS="false"
start_tenant_opencode test
if grep -q -- '--userns' "$ctr_calls"; then
  printf 'Rootless OpenCode user namespace leaked into rootful command\n' >&2
  exit 1
fi

# Static run arguments participate in imperative container recreation checks.
hash_source="$(tenant_env_dir test)/lunarwing.env"
_store_container_hash test contract-fixture "$hash_source" "$OPENCODE_ROOTLESS_LAUNCH_CONTRACT"
if _container_config_changed \
    test contract-fixture "$hash_source" "$OPENCODE_ROOTLESS_LAUNCH_CONTRACT"; then
  printf 'Matching OpenCode launch contract reported changed\n' >&2
  exit 1
fi
if ! _container_config_changed test contract-fixture "$hash_source" "userns=changed"; then
  printf 'Changed OpenCode launch contract did not trigger recreation\n' >&2
  exit 1
fi

dockerfile="$SCRIPT_DIR/../../opencode4lunarwing/Dockerfile"
grep -Fq "groupadd --gid $OPENCODE_CONTAINER_GID opencode" "$dockerfile"
grep -Fq "useradd --uid $OPENCODE_CONTAINER_UID --gid $OPENCODE_CONTAINER_GID" "$dockerfile"
# WI#400: the bridge runs as container root and drops tasks to the task uid.
grep -Eq '^ENV OPENCODE_TASK_UID=' "$dockerfile"
grep -Eq '^    OPENCODE_TASK_GID=' "$dockerfile"
grep -Eq '^USER 0:0[[:space:]]*$' "$dockerfile"

# The pre-WI#400 image user (the task uid directly) must now be refused.
container_image_user="10001:10001"
: > "$ctr_calls"
if stale_output="$(start_tenant_opencode test 2>&1)"; then
  printf 'OpenCode worker accepted stale image user contract\n' >&2
  exit 1
fi
if [[ "$stale_output" != *"expected 0:0 (WI#400 root entrypoint; serve/tasks drop to $OPENCODE_CONTAINER_UID); rebuild it"* ]]; then
  printf 'OpenCode stale image error omitted rebuild instruction\n' >&2
  exit 1
fi
if grep -Fq -- ' run -d ' "$ctr_calls"; then
  printf 'OpenCode worker started with stale image user contract\n' >&2
  exit 1
fi

# A rebuilt image must escape any StartLimitBurst lockout left by old failures.
container_image_user="0:0"
CONTAINER_RT="podman"
# Override consumed by sourced worker startup.
# shellcheck disable=SC2034
MT_ROOTLESS="true"
ensure_init_system() {
  # Global consumed by sourced worker startup.
  # shellcheck disable=SC2034
  INIT_SYSTEM="systemd"
}
podman_supports_quadlet() { return 0; }
_wait_user_manager() { :; }
systemctl_calls=()
_systemctl_user() {
  systemctl_calls+=("$*")
  [[ "${2:-}" != "start" ]]
}
start_tenant_opencode test >/dev/null 2>&1
reset_seen=false
status_full_seen=false
for call in "${systemctl_calls[@]}"; do
  [[ "$call" == "test reset-failed lunarwing-opencode-test.service" ]] && reset_seen=true
  if [[ "$call" == "test start lunarwing-opencode-test.service" && "$reset_seen" != "true" ]]; then
    printf 'OpenCode Quadlet start ran before reset-failed\n' >&2
    exit 1
  fi
  [[ "$call" == "test status lunarwing-opencode-test.service --no-pager --full" ]] \
    && status_full_seen=true
done
if [[ "$reset_seen" != "true" || "$status_full_seen" != "true" ]]; then
  printf 'OpenCode Quadlet failure recovery omitted reset or full status\n' >&2
  exit 1
fi

# Uncached Podman builds minimize peak storage without pruning tenant containers.
build_calls="$fixture/build-calls"
podman() { printf '%s\n' "$*" >>"$build_calls"; }
LUNARWING_ROOT="$fixture"
mkdir -p "$LUNARWING_ROOT/opencode4lunarwing"
build_opencode_worker true true
grep -Fqx 'image prune -f' "$build_calls"
grep -Fq 'build --no-cache --layers=false --build-arg WITH_TOOLCHAINS=true' "$build_calls"

: >"$build_calls"
build_opencode_worker false false
if grep -Eq -- 'image prune|--no-cache|--layers=false' "$build_calls"; then
  printf 'Cached OpenCode build used uncached-build storage cleanup\n' >&2
  exit 1
fi

docker() { printf '%s\n' "$*" >>"$build_calls"; }
# Override consumed by sourced worker builder.
# shellcheck disable=SC2034
CONTAINER_RT="docker"
: >"$build_calls"
build_opencode_worker true true
grep -Fq 'build --no-cache --build-arg WITH_TOOLCHAINS=true' "$build_calls"
if grep -Eq -- 'image prune|--layers=false' "$build_calls"; then
  printf 'Podman-only OpenCode storage flags leaked into Docker build\n' >&2
  exit 1
fi

grep -Fq 'bun pm cache rm' "$dockerfile"
grep -Fq 'COPY --from=builder /out/bin/opencode /usr/local/bin/opencode' "$dockerfile"
grep -Fq 'COPY --from=builder /out/sdk /app/node_modules/@opencode-ai/sdk' "$dockerfile"
if grep -Fq 'COPY --from=builder /build/opencode /app/opencode' "$dockerfile"; then
  printf 'OpenCode runtime image copies full upstream build tree\n' >&2
  exit 1
fi

entrypoint="$SCRIPT_DIR/../../opencode4lunarwing/entrypoint.sh"
executor="$SCRIPT_DIR/../../opencode4lunarwing/scripts/opencode_task_executor.ts"
grep -Fq 'OPENCODE_BIN="${OPENCODE_BIN:-/usr/local/bin/opencode}"' "$entrypoint"
grep -Fq '"$OPENCODE_BIN" serve' "$entrypoint"
grep -Fq 'from "@opencode-ai/sdk/v2/client"' "$executor"

printf 'Study MCP worker mount tests passed\n'
