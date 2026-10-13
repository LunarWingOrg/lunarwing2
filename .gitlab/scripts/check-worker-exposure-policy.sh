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
# Exercise the real wrapper and non-root boot path on MRs, not only during
# the protected image build. All three suites use only Python's stdlib + sh.
python3 "$lane/scripts/exposure_policy_assert_test.py" || failed=1
python3 "$lane/scripts/compile_out_remote_tunnel_test.py" || failed=1
python3 "$lane/scripts/entrypoint_config_test.py" || failed=1

# The wrapper must actually be installed over the pinned binary path, and
# each build-time policy step must be an anchored RUN: a deleted RUN used to
# pass because the script stayed named in COPY lines and comments (run-90
# FC-4 G1/G5). Greps run over comment-stripped, continuation-joined logical
# lines (pure bash: the ci-config image has no awk/sed, run-79 EI-1).
dockerfile="$lane/Dockerfile"
dockerfile_code=""
_join=""
while IFS= read -r _raw || [[ -n "$_raw" ]]; do
    _line="${_raw#"${_raw%%[![:space:]]*}"}"
    if [[ -z "$_line" || "$_line" == \#* ]]; then
        continue
    fi
    if [[ "$_line" == *\\ ]]; then
        _part="${_line%\\}"
        _join+="${_part%"${_part##*[![:space:]]}"} "
        continue
    fi
    dockerfile_code+="${_join}${_line}"$'\n'
    _join=""
done < "$dockerfile"
grep -q 'COPY --from=builder /out/bin/opencode /usr/local/lib/opencode2/opencode' <<<"$dockerfile_code" \
    || fail "Dockerfile no longer moves the pinned binary to /usr/local/lib/opencode2/opencode"
grep -q 'install -D -m 0755 /app/scripts/opencode-exposure-guard.sh /usr/local/bin/opencode' <<<"$dockerfile_code" \
    || fail "Dockerfile no longer installs the exposure-guard wrapper as /usr/local/bin/opencode"
grep -q -- 'exposure_policy_assert.py --lane /app --profile opencode2 --binary /usr/local/lib/opencode2/opencode' <<<"$dockerfile_code" \
    || fail "Dockerfile lost the build-time binary marker scan (exposure_policy_assert --binary, run-90 FC-4 G5)"
grep -q '^RUN python3 /build/compile_out_remote_tunnel.py /build/opencode$' <<<"$dockerfile_code" \
    || fail "Dockerfile lost the OpenTunnel compile-out RUN (control 8, run-90 FC-4 G1)"
grep -q '^RUN python3 /app/scripts/exposure_policy_assert.py --lane /app --profile opencode2 --image-filesystem$' <<<"$dockerfile_code" \
    || fail "Dockerfile must verify installed policy-file ownership and modes (FB-4)"

# WI#357 (run-79 EI-5): the exposure controls must be root-owned and
# read-only in the image; agent-writable scripts/config would let the agent
# rewrite the very guards that constrain it.
if grep -Eq '^\s*COPY\s+[^#]*--chown=[^#]*\bscripts/' <<<"$dockerfile_code"; then
    fail "Dockerfile hands /app/scripts to the agent uid — scripts must be root-owned"
fi
if grep -Eq '^\s*COPY\s+[^#]*--chown=[^#]*\bconfig/' <<<"$dockerfile_code"; then
    fail "Dockerfile hands /app/config to the agent uid — config must be root-owned"
fi
grep -q '^COPY scripts/ ./scripts/$' <<<"$dockerfile_code" \
    || fail "Dockerfile lost the root-owned COPY of scripts/"
grep -q '^COPY config/ ./config/$' <<<"$dockerfile_code" \
    || fail "Dockerfile lost the root-owned COPY of config/"
if grep -Eq '^\s*COPY\s+[^#]*--chown=[^#]*\b(entrypoint\.sh|health_server\.py|agent_comm_protocol\.json)\b' <<<"$dockerfile_code"; then
    fail "Dockerfile hands boot files to the agent uid — boot files must be root-owned (FB-4)"
fi
grep -q '^COPY entrypoint.sh health_server.py agent_comm_protocol.json ./$' <<<"$dockerfile_code" \
    || fail "Dockerfile lost the root-owned COPY of the boot files (FB-4)"
grep -Eq 'chmod -R a-w /app/scripts /app/config /app/entrypoint\.sh /app/health_server\.py /app/agent_comm_protocol\.json' <<<"$dockerfile_code" \
    || fail "Dockerfile must mark scripts, config and boot files read-only (FB-4)"

# Run-90 FC-4 G3b: nothing may re-permission the guarded policy paths — any
# chown/chmod RUN touching them, other than the two shipped lines (the exec
# bit before the a-w step, and the a-w itself), fails the guard. This also
# catches a late `chown -R` / `chmod u+w` placed after the final assertion,
# which the image-filesystem check cannot see.
while IFS= read -r perm_line; do
    case "$perm_line" in
        *"chmod +x /app/entrypoint.sh /app/health_server.py"*) ;;
        *"chmod -R a-w /app/scripts /app/config /app/entrypoint.sh /app/health_server.py /app/agent_comm_protocol.json"*) ;;
        *)
            fail "Dockerfile must not re-permission the guarded policy paths (run-90 FC-4): ${perm_line}"
            ;;
    esac
done < <(grep -E '(^|[;[:space:]])ch(own|mod)[[:space:]][^#]*(/app/(scripts|config|entrypoint\.sh|health_server\.py|agent_comm_protocol\.json))' <<<"$dockerfile_code")

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
