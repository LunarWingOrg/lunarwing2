#!/usr/bin/env bash
# WI#357 control 4: worker exposure policy guard.
#
# Pins the "workers must never self-expose" policy for the opencode worker
# lanes in plain CI (no image build): the lane's own
# exposure_policy_assert.py checks the committed config/entrypoint/wrapper,
# and this script additionally asserts the Dockerfile wiring and the bridge
# session ruleset cannot silently lose the guards (a removed Dockerfile RUN
# or a reverted permission list would otherwise pass a source-only audit).
#
# The runtime-stage binary scan and the compiled binary itself are covered by
# the image build gate (control 5) and the compile-out patch (control 8).
set -euo pipefail

# No git: the ci-config job runs this in python:3.12-slim-bookworm, which has
# none. The script sits at <repo root>/.gitlab/scripts/.
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
lane="$repo_root/opencode2.4lunarwing"
v1_lane="$repo_root/opencode4lunarwing"

failed=0
fail() { echo "ERROR: $*" >&2; failed=1; }

python3 "$lane/scripts/exposure_policy_assert.py" --lane "$lane" --profile opencode2 || failed=1
python3 "$lane/scripts/exposure_policy_assert.py" --lane "$v1_lane" --profile opencode-v1 || failed=1

# The wrapper must actually be installed over the pinned binary path.
dockerfile="$lane/Dockerfile"
grep -q 'COPY --from=builder /out/bin/opencode /usr/local/lib/opencode2/opencode' "$dockerfile" \
    || fail "Dockerfile no longer moves the pinned binary to /usr/local/lib/opencode2/opencode"
grep -q 'install -D -m 0755 /app/scripts/opencode-exposure-guard.sh /usr/local/bin/opencode' "$dockerfile" \
    || fail "Dockerfile no longer installs the exposure-guard wrapper as /usr/local/bin/opencode"
grep -q 'exposure_policy_assert.py' "$dockerfile" \
    || fail "Dockerfile lost its build-time exposure policy assertion"
grep -q 'compile_out_remote_tunnel.py' "$dockerfile" \
    || fail "Dockerfile lost the OpenTunnel compile-out build patch (control 8)"

# The bridge session ruleset must deny the self-exposure subcommands; the
# denies must come after the allow-all shell rule to win under findLast.
executor="$lane/scripts/opencode_task_executor.ts"
for sub in service pair serve; do
    grep -q "resource: \"\*opencode $sub \*\", effect: \"deny\"" "$executor" \
        || fail "bridge session ruleset lost the shell deny for 'opencode $sub'"
done
python3 - "$executor" <<'PY' || failed=1
import pathlib
import sys

text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
allow = text.find('{ action: "shell", resource: "*", effect: "allow" }')
denies = [text.find(f'"*opencode {sub} *"') for sub in ("service", "pair", "serve")]
if allow < 0 or any(deny < 0 for deny in denies) or any(deny < allow for deny in denies):
    print("ERROR: session ruleset ordering regressed: exposure denies must follow the shell allow",
          file=sys.stderr)
    sys.exit(1)
PY

if ((failed)); then
    exit 1
fi

echo "worker exposure policy guard: ok"
