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

selinux_active=true
selinux_calls="$fixture/selinux-calls"
configure_study_mcp test "$study_home"
grep -Fqx "LW2STUDYMCP_HOME=$study_home" "$(tenant_env_dir test)/lunarwing.env"
grep -Fqx -- "-R -t container_file_t -- $study_home" "$selinux_calls"

ctr_calls="$fixture/container-calls"
start_tenant_opencode test
grep -Fq -- "-v $study_home:/opt/lw2studymcp:ro,z" "$ctr_calls"
grep -Fq -- "-e LW2STUDYMCP_HOME=/opt/lw2studymcp" "$ctr_calls"

render_worker_quadlet test opencode 8443
opencode_unit="$(tenant_quadlet_dir test)/lunarwing-opencode-test.container"
grep -Fq "Volume=$study_home:/opt/lw2studymcp:ro,z" "$opencode_unit"
grep -Fq "Environment=LW2STUDYMCP_HOME=/opt/lw2studymcp" "$opencode_unit"

render_worker_quadlet test nanocode 8443
nanocode_unit="$(tenant_quadlet_dir test)/lunarwing-nanocode-test.container"
if grep -q 'LW2STUDYMCP\|/opt/lw2studymcp' "$nanocode_unit"; then
  printf 'Study Notes mount leaked into nanocode quadlet\n' >&2
  exit 1
fi

printf 'Study MCP worker mount tests passed\n'
