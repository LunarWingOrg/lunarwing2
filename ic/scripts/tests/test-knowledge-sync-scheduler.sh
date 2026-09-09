#!/usr/bin/env bash
set -euo pipefail

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$TEST_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
cleanup() { rm -rf "$fixture"; }
trap cleanup EXIT
mkdir -p "$fixture/bin" "$fixture/systemd"

export SYSTEMCTL_LOG="$fixture/systemctl.log"
cat >"$fixture/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$SYSTEMCTL_LOG"
EOF
chmod +x "$fixture/bin/systemctl"
PATH="$fixture/bin:$PATH"

LUNARWING_MT_SYSTEMD_UNIT_DIR="$fixture/systemd"
INIT_SYSTEM=systemd
ensure_knowledge_source_scheduler
grep -Fq "ExecStart=$SCRIPT_DIR/lunarwing-mt-admin.sh sync-knowledge-sources" \
  "$fixture/systemd/lunarwing-study-notes-sync.service"
grep -Fq 'OnCalendar=hourly' "$fixture/systemd/lunarwing-study-notes-sync.timer"
grep -Fqx 'daemon-reload' "$SYSTEMCTL_LOG"
grep -Fqx 'enable --now lunarwing-study-notes-sync.timer' "$SYSTEMCTL_LOG"

export FCRONTAB_STATE="$fixture/fcrontab"
cat >"$fixture/bin/fcrontab" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "-l" ]]; then
  [[ ! -f "$FCRONTAB_STATE" ]] || cat "$FCRONTAB_STATE"
  exit 0
fi
cp "$1" "$FCRONTAB_STATE"
EOF
chmod +x "$fixture/bin/fcrontab"
_ensure_cron_runlevel() { :; }
INIT_SYSTEM=openrc
ensure_knowledge_source_scheduler
ensure_knowledge_source_scheduler
grep -Fq "17 * * * * $SCRIPT_DIR/lunarwing-mt-admin.sh sync-knowledge-sources" \
  "$FCRONTAB_STATE"
[[ "$(grep -c '^# BEGIN lunarwing-study-notes-sync managed block$' "$FCRONTAB_STATE")" -eq 1 ]]
[[ "$(grep -c '^# END lunarwing-study-notes-sync managed block$' "$FCRONTAB_STATE")" -eq 1 ]]

printf 'Knowledge sync scheduler tests passed\n'
