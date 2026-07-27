#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

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
chown() { :; }
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
_store_container_hash() { :; }
_ctr() {
  printf '%s\n' "$*" >> "$ctr_calls"
  [[ "$2" != "inspect" ]]
}

study_home="$fixture/study-notes"
study_state="$(tenant_state_dir test)/study-notes"
mkdir -p \
  "$study_home/agent-notes-by-agent" \
  "$study_home/skills/study-notes" \
  "$(tenant_env_dir test)"
touch \
  "$study_home/study_notes_mcp.py" \
  "$study_home/memorydb.py" \
  "$study_home/agent-notes.sqlite3" \
  "$study_home/skills/study-notes/SKILL.md"
cat >"$(tenant_env_dir test)/lunarwing.env" <<EOF
GATEWAY_AUTH_TOKEN=test-token
LLM_API_KEY=test-key
EOF

# Failed seeding must leave no partial final or staging directory.
failed_state="$(tenant_state_dir test)/failed-study-notes"
cp_calls=0
cp() {
  cp_calls=$((cp_calls + 1))
  [[ "$cp_calls" -lt 2 ]] || return 1
  command cp "$@"
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

selinux_active=true
selinux_calls="$fixture/selinux-calls"
configure_study_mcp test "$study_home" true
grep -Fqx "LW2STUDYMCP_HOME=$study_home" "$(tenant_env_dir test)/lunarwing.env"
grep -Fqx "LW2STUDYMCP_STATE_HOME=$study_state" "$(tenant_env_dir test)/lunarwing.env"
grep -Fqx "LW2STUDYMCP_ALLOW_WRITE=1" "$(tenant_env_dir test)/lunarwing.env"
test -f "$study_state/agent-notes.sqlite3"
test -d "$study_state/agent-notes-by-agent"
grep -Fq -- "-R -t container_file_t -- $study_home" "$selinux_calls"
grep -Fq -- "-R -t container_file_t -- $study_state" "$selinux_calls"

ctr_calls="$fixture/container-calls"
CONTAINER_RT="podman"
MT_ROOTLESS="true"
start_tenant_opencode test
grep -Fq -- "-v $study_home:/opt/lw2studymcp:ro,z" "$ctr_calls"
grep -Fq -- "-e LW2STUDYMCP_HOME=/opt/lw2studymcp" "$ctr_calls"
grep -Fq -- "-v $study_state:/var/lib/lw2studymcp:ro,z" "$ctr_calls"
grep -Fq -- "-e LW2STUDYMCP_STATE_HOME=/var/lib/lw2studymcp" "$ctr_calls"
if grep -q -- 'LW2STUDYMCP_ALLOW_WRITE\|--userns=keep-id' "$ctr_calls"; then
  printf 'Core Study Notes write settings leaked into OpenCode Podman command\n' >&2
  exit 1
fi

render_worker_quadlet test opencode 8443
opencode_unit="$(tenant_quadlet_dir test)/lunarwing-opencode-test.container"
grep -Fq "Volume=$study_home:/opt/lw2studymcp:ro,z" "$opencode_unit"
grep -Fq "Environment=LW2STUDYMCP_HOME=/opt/lw2studymcp" "$opencode_unit"
grep -Fq "Volume=$study_state:/var/lib/lw2studymcp:ro,z" "$opencode_unit"
grep -Fq "Environment=LW2STUDYMCP_STATE_HOME=/var/lib/lw2studymcp" "$opencode_unit"
if grep -q 'LW2STUDYMCP_ALLOW_WRITE\|UserNS=keep-id' "$opencode_unit"; then
  printf 'Core Study Notes write settings leaked into OpenCode quadlet\n' >&2
  exit 1
fi

render_worker_quadlet test nanocode 8443
nanocode_unit="$(tenant_quadlet_dir test)/lunarwing-nanocode-test.container"
if grep -q 'LW2STUDYMCP\|/opt/lw2studymcp\|/var/lib/lw2studymcp' "$nanocode_unit"; then
  printf 'Study Notes mount leaked into nanocode quadlet\n' >&2
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
if grep -q -- 'LW2STUDYMCP_ALLOW_WRITE\|--userns=keep-id' "$ctr_calls"; then
  printf 'Study Notes write settings leaked into read-only Podman command\n' >&2
  exit 1
fi

render_worker_quadlet test opencode 8443
grep -Fq "Volume=$study_state:/var/lib/lw2studymcp:ro,z" "$opencode_unit"
if grep -q 'LW2STUDYMCP_ALLOW_WRITE\|UserNS=keep-id' "$opencode_unit"; then
  printf 'Study Notes write settings leaked into read-only quadlet\n' >&2
  exit 1
fi

printf 'Study MCP worker mount tests passed\n'
