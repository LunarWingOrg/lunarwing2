#!/usr/bin/env bash
# Opt-in isolated real WeeChat auth/save/restart test; no tenant units touched.
# shellcheck disable=SC1091,SC2034  # mt-admin sources and renderer globals
set -euo pipefail
if [[ "${RUN_LIVE_RELAY_ROTATION:-0}" != 1 ]]; then
  echo 'SKIP: set RUN_LIVE_RELAY_ROTATION=1 for real WeeChat relay rotation'
  exit 0
fi
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"
[[ "$EUID" != 0 ]] || die 'run this fixture as an unprivileged user'
require_cmd weechat
require_cmd tmux
require_cmd python3
fixture="$(mktemp -d)"
name="relay-fixture-$$"
socket="weechat-$name"
cleanup() {
  if declare -F stop_relay >/dev/null; then stop_relay >/dev/null 2>&1 || true; fi
  tmux -L "$socket" kill-server 2>/dev/null || true
  rm -rf "$fixture"
}
trap cleanup EXIT
tenant_env_dir() { printf '%s/env' "$fixture"; }
tenant_weechat_home() { printf '%s/weechat' "$fixture"; }
tenant_lw_root() { printf '%s' "$fixture/repo"; }
# WI#446: the systemd unit renderer writes tenant-side (sudo -u <tenant> +
# the atomic writer). This opt-in fixture runs unprivileged with a tenant
# that is not a real account, so map the drop onto the invoking user — the
# fixture tree is owned by that user and satisfies the tenant-side checks.
sudo() {
  [[ "${1:-}" == "-u" ]] || return 1
  shift 2
  "$@"
}
mkdir -p "$fixture/env" "$fixture/weechat" "$fixture/units" "$fixture/repo/ic/scripts"
chmod 700 "$fixture/env"
cp "$SCRIPT_DIR/lunarwing-weechat-"{start,stop}.sh "$fixture/repo/ic/scripts/"
cp "$RELAY_ENV_EXEC_SRC" "$fixture/relay-env-exec"
RELAY_ENV_EXEC="$fixture/relay-env-exec"
chmod 755 "$RELAY_ENV_EXEC"
touch "$fixture/env/.relay-start.lock"
chmod 600 "$fixture/env/.relay-start.lock"
printf 'RELAY_PASSWORD=%s\n' "$(generate_token)" >"$fixture/env/weechat.env"
cp "$fixture/env/weechat.env" "$fixture/env/old.env"
chmod 600 "$fixture/env/"*
# Allocate a disposable loopback port; WeeChat itself binds only 127.0.0.1.
port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
cat >"$fixture/weechat/relay.conf" <<EOF
config_version = 2

[network]
password = "\${env:RELAY_PASSWORD}"
bind_address = "127.0.0.1"
allow_empty_password = off
ipv6 = off
[api]

[port]
api = $port
EOF
_render_weechat_systemd_unit "$name" "$fixture/units"
start_line="$(sed -n 's/^ExecStart=//p' "$fixture/units/lunarwing-weechat-$name.service")"
# Execute the actual rendered tmux/pane command in this isolated fixture.
start_relay() { bash -c "$start_line"; }
stop_relay() {
  "$fixture/repo/ic/scripts/lunarwing-weechat-stop.sh" \
    --weechat-home "$fixture/weechat" --tmux-socket "$socket" --timeout 10
}
probe() {  # <credential-file> <expected-http-status>
  python3 - "$1" "$port" "$2" <<'PY'
import base64
from pathlib import Path
import sys
import time
import urllib.error
import urllib.request
password = Path(sys.argv[1]).read_text().strip().split("=", 1)[1]
authorization = base64.b64encode(("plain:" + password).encode()).decode()
request = urllib.request.Request("http://127.0.0.1:" + sys.argv[2] + "/api/version",
                                 headers={"Authorization": "Basic " + authorization})
for _ in range(50):
    try:
        with urllib.request.urlopen(request, timeout=1) as response:
            status = response.status
    except urllib.error.HTTPError as error:
        status = error.code
    except (OSError, urllib.error.URLError):
        time.sleep(0.1)
        continue
    if status != int(sys.argv[3]):
        print(f"relay returned HTTP {status}, expected {sys.argv[3]}", file=sys.stderr)
        sys.exit(1)
    sys.exit(0)
print("relay listener unavailable", file=sys.stderr)
sys.exit(1)
PY
}

start_relay
probe "$fixture/env/old.env" 200 || die 'initial WeeChat authentication failed'
exec {fd}<"$fixture/env/.relay-start.lock"
flock -x "$fd"
mkdir "$fixture/env/.relay-rotation-blocked"
stop_relay
[[ -f "$fixture/weechat/weechat.upgrade" ]] || die 'session was not saved'
# External start while the writer holds the lock: the tmux parent can fork,
# but the guarded pane must never create a relay listener.
start_relay
if probe "$fixture/env/old.env" 200 2>/dev/null; then die 'relay started during rotation'; fi
tmux -L "$socket" kill-server 2>/dev/null || true
_relay_env_set_password "$fixture/env/weechat.env" "$(generate_token)"
rmdir "$fixture/env/.relay-rotation-blocked"
flock -u "$fd"
exec {fd}<&-
start_relay
probe "$fixture/env/weechat.env" 200 || die 'rotated credential rejected'
probe "$fixture/env/old.env" 401 || die 'old credential still accepted'
stop_relay
_weechat_validate_relay_config "$fixture/weechat" "$port" \
  "$(_read_env_value "$fixture/env/weechat.env" RELAY_PASSWORD)" \
  || die 'saved relay config lost its literal environment expression'
start_relay
probe "$fixture/env/weechat.env" 200 || die 'credential failed after second restart'
stop_relay
echo 'PASS: real WeeChat blocks concurrent start, accepts only the new password, and preserves config/session across restarts'
