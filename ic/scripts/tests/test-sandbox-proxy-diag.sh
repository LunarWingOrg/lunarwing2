#!/usr/bin/env bash
# WI#641: exercise the diagnostic CLI without touching tenant state or host audit logs.
set -euo pipefail
export DIAG_SCRIPTS
DIAG_SCRIPTS="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
export DIAG_FIXTURE
DIAG_FIXTURE="$(mktemp -d)"
trap 'rm -rf -- "$DIAG_FIXTURE"' EXIT
umask 077
env_dir="$DIAG_FIXTURE/home/review-tenant/lunarwing/env"
mkdir -p "$env_dir" "$DIAG_FIXTURE/bin"
diag="$DIAG_FIXTURE/lunarwing-sandbox-proxy-diag.sh"
cp "$DIAG_SCRIPTS/lunarwing-sandbox-proxy-diag.sh" "$diag"

# Use the real mt-admin path builder and safe reader. Only redirect the home
# base into the fixture, and simulate reading as the tenant (our own UID).
cat >"$DIAG_FIXTURE/lunarwing-mt-admin.sh" <<'EOF'
source "$DIAG_SCRIPTS/lunarwing-mt-admin.sh"
tenant_home() { printf '%s/home/%s' "$DIAG_FIXTURE" "$1"; }
EOF
cat >"$DIAG_FIXTURE/bin/id" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$DIAG_FIXTURE/id-calls"
case "$*" in
  "-u -- review-tenant") printf '24681\n' ;;
  "-u -- root") printf '0\n' ;;
  -un) printf 'review-tenant\n' ;;
  *) exit 1 ;;
esac
EOF
cat >"$DIAG_FIXTURE/bin/ps" <<'EOF'
#!/usr/bin/env bash
[[ "$*" == '-u 24681 -o pid=,label=,comm=' ]] || exit 1
printf '%s\n' '1234 test_u:test_r:daemon_t:s0 lunarwing' '1235 unrelated_t worker-fixture'
EOF
cat >"$DIAG_FIXTURE/bin/getenforce" <<'EOF'
#!/usr/bin/env bash
printf 'Enforcing\n'
EOF
cat >"$DIAG_FIXTURE/bin/ausearch" <<'EOF'
#!/usr/bin/env bash
cat <<'LOG'
type=AVC msg=audit(100:1): avc: denied { write } for name="proxy.sock" tclass=sock_file
node=fixture type=USER_AVC msg=audit(100:2): avc: denied { connectto } scontext=test_u:test_r:container_t:s0 tcontext=test_u:test_r:daemon_t:s0 tclass=unix_stream_socket
type=PROCTITLE msg=audit(100:2): proctitle=unrelated-audit-argv-connectto
type=AVC msg=audit(100:3): avc: denied { write } for name="not-a-relay-file" tclass=file
LOG
EOF
chmod +x "$DIAG_FIXTURE/bin/"*
export PATH="$DIAG_FIXTURE/bin:$PATH"
unset SANDBOX_NETWORK

cat >"$env_dir/lunarwing.env" <<'EOF'
SANDBOX_NETWORK=none
SANDBOX_NETWORK='proxy_enforced'
OTHER_VAR=unrelated-fixture-value
DO_NOT_SOURCE=$(touch -- "$DIAG_FIXTURE/env-executed")
EOF
output="$DIAG_FIXTURE/output"
bash "$diag" review-tenant >"$output"
grep -Fq 'SANDBOX_NETWORK=proxy-enforced' "$output"
grep -Fq 'getenforce: Enforcing' "$output"
grep -Fq '1234 test_u:test_r:daemon_t:s0 lunarwing' "$output"
grep -Fq 'Relay sockets under /run/user/24681' "$output"
grep -Fq 'name="proxy.sock" tclass=sock_file' "$output"
grep -Fq 'tcontext=test_u:test_r:daemon_t:s0 tclass=unix_stream_socket' "$output"
if grep -Eq 'unrelated-fixture-value|worker-fixture|unrelated-audit-argv|not-a-relay-file' "$output" \
  || [[ -e "$DIAG_FIXTURE/env-executed" ]]; then
  printf 'FAIL: diagnostic leaked unrelated data or sourced the tenant env\n' >&2
  exit 1
fi
printf 'PASS: tenant env, process context and candidate AVCs are scoped\n'

SANDBOX_NETWORK=tunnel bash "$diag" review-tenant >"$output"
grep -Fq 'SANDBOX_NETWORK=tunnel' "$output"
printf 'SANDBOX_NETWORK=%s\n' 'unrecognised-private-fixture-value' >"$env_dir/lunarwing.env"
bash "$diag" review-tenant >"$output"
grep -Fq 'SANDBOX_NETWORK=unrecognized (value omitted)' "$output"
if grep -Fq 'unrecognised-private-fixture-value' "$output"; then
  printf 'FAIL: unknown environment value was printed\n' >&2
  exit 1
fi
mv "$env_dir/lunarwing.env" "$env_dir/other.env"
bash "$diag" review-tenant >"$output"
grep -Fq 'SANDBOX_NETWORK=unset' "$output"
grep -Fq 'tclass=unix_stream_socket' "$output"
ln -s other.env "$env_dir/lunarwing.env"
bash "$diag" review-tenant >"$output" 2>"$DIAG_FIXTURE/unsafe-env.err"
grep -Fq 'SANDBOX_NETWORK=unavailable' "$output"
grep -Fq 'getenforce: Enforcing' "$output"
grep -Fq 'tclass=unix_stream_socket' "$output"
printf 'PASS: override, unknown value, missing env, and unsafe env handling\n'

: >"$DIAG_FIXTURE/id-calls"
for name in '../review-tenant' 'other/review-tenant' '..' 'UPPER' 'two names' 'bad.'; do
  if bash "$diag" "$name" >"$output" 2>&1; then
    printf 'FAIL: invalid tenant name accepted\n' >&2
    exit 1
  fi
  grep -Fq 'expected a lowercase mt-admin tenant name' "$output"
done
[[ ! -s "$DIAG_FIXTURE/id-calls" ]]
for name in root no-such-tenant; do
  if bash "$diag" "$name" >"$output" 2>&1; then
    printf 'FAIL: root or missing user accepted as a tenant\n' >&2
    exit 1
  fi
  grep -Fq 'no non-root account' "$output"
done
printf 'PASS: invalid tenant names and accounts are refused before diagnosis\n'

# A missing ausearch is named. It must not read as "searched, nothing matched".
# The fixture's ausearch is set aside; a real one on this host is never run.
mv "$DIAG_FIXTURE/bin/ausearch" "$DIAG_FIXTURE/ausearch.off"
if command -v ausearch >/dev/null 2>&1; then
  printf 'SKIP: a real ausearch is installed here; the missing-tool case is not run\n'
else
  bash "$diag" review-tenant >"$output" 2>"$DIAG_FIXTURE/no-ausearch.err"
  grep -Fq 'ausearch not installed; skipped.' "$output"
  if grep -Fq 'no matching AVC returned' "$output"; then
    printf 'FAIL: a missing ausearch was reported as an empty search\n' >&2
    exit 1
  fi
  printf 'PASS: a missing ausearch is named, not reported as an empty search\n'
fi
mv "$DIAG_FIXTURE/ausearch.off" "$DIAG_FIXTURE/bin/ausearch"

python3 -I -B - "$DIAG_SCRIPTS/../src/sandbox/proxy/relay.py" <<'PY'
import contextlib
import errno
import importlib.util
import io
import sys
from unittest import mock

spec = importlib.util.spec_from_file_location("proxy_relay", sys.argv[1])
relay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(relay)
stderr = io.StringIO()
with (mock.patch.object(relay.socket, "socket") as socket,
      mock.patch.object(relay, "Relay") as server,
      mock.patch.object(relay.subprocess, "run") as command,
      contextlib.redirect_stderr(stderr)):
    probe = socket.return_value.__enter__.return_value
    probe.connect.side_effect = PermissionError(errno.EACCES, "Permission denied")
    assert relay.main() == 125
socket.assert_called_once_with(relay.socket.AF_UNIX, relay.socket.SOCK_STREAM)
probe.settimeout.assert_called_once_with(5)
probe.connect.assert_called_once_with(relay.SOCKET)
server.assert_not_called()
command.assert_not_called()
assert stderr.getvalue().startswith("lunarwing-proxy-relay: [Errno 13] Permission denied;")
assert "euid=" in stderr.getvalue() and "proxy.sock" in stderr.getvalue()
# WI#641 note 19288: an EACCES on this path can be the SELinux connectto check
# against the listener's own context. The errno has two other causes, so the
# hint must name all three and must not claim one of them.
assert "connectto" in stderr.getvalue()
assert "sock_file" in stderr.getvalue()
assert "cannot traverse" in stderr.getvalue()
assert "three possible causes" in stderr.getvalue()
assert "self-label=" in stderr.getvalue()
print("PASS: a failed relay probe reports diagnostics and never starts the command")

# WI#641 note 19288: container runtimes do not mount selinuxfs, so the
# container's own label (the denial's source context) must be read from
# /proc and reported without an enforce node.
def fake_open(path, *args, **kwargs):
    if path.endswith("/attr/current"):
        return io.StringIO("system_u:system_r:container_t:s0:c533,c991\n")
    raise FileNotFoundError(2, "No such file or directory", path)

with mock.patch("builtins.open", fake_open):
    assert relay._self_label() == "system_u:system_r:container_t:s0:c533,c991"
    with mock.patch.object(relay, "_node", return_value="node"):
        state = relay._security_state()
assert "self-label=system_u:system_r:container_t:s0:c533,c991" in state, state

# A non-EACCES startup failure must not carry the EACCES hint.
other = relay._startup_error(FileNotFoundError(2, "No such file or directory"))
assert "connectto" not in other and "possible causes" not in other
print("PASS: the container's own label is reported without selinuxfs")
PY
