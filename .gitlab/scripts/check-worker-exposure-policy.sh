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
# FC-4 G1/G5). Checks run over comment-stripped, continuation-joined logical
# lines with case-normalized instructions (pure bash: the ci-config image
# has no awk/sed, run-79 EI-1).
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
    _line="${_join}${_line%"${_line##*[![:space:]]}"}"
    _instruction="${_line%%[[:space:]]*}"
    dockerfile_code+="${_instruction^^}${_line#"$_instruction"}"$'\n'
    _join=""
done < "$dockerfile"
[[ -z "$_join" ]] || fail "Dockerfile ends with an unfinished continuation"
grep -Fxq 'COPY --from=builder /out/bin/opencode /usr/local/lib/opencode2/opencode' <<<"$dockerfile_code" \
    || fail "Dockerfile no longer moves the pinned binary to /usr/local/lib/opencode2/opencode"
binary_scan='RUN install -D -m 0755 /app/scripts/opencode-exposure-guard.sh /usr/local/bin/opencode'
binary_scan+=' && python3 /app/scripts/exposure_policy_assert.py --lane /app --profile opencode2 --binary /usr/local/lib/opencode2/opencode'
grep -Fxq "$binary_scan" <<<"$dockerfile_code" \
    || fail "Dockerfile no longer installs the exposure-guard wrapper with the build-time binary marker scan (run-90 FC-4 G5)"

# Run-96 FP-1: presence alone does not prove that the patch reaches the
# shipped binary. Require its exact COPY and RUN in AS builder, in that
# order, before the pinned native build RUN (not after it or in a dead stage).
# The filesystem assertion must be the final filesystem-changing step:
# allow only metadata afterwards, rather than enumerating chmod/chown forms.
stage=""
compile_out_copied=0
compile_out_ran=0
guarded_build=0
filesystem_checked=0
while IFS= read -r docker_line; do
    [[ -n "$docker_line" ]] || continue
    instruction="${docker_line%%[[:space:]]*}"
    if ((filesystem_checked)); then
        case "$instruction" in
            EXPOSE|HEALTHCHECK|USER|ENTRYPOINT|CMD|LABEL|STOPSIGNAL) ;;
            *) fail "Dockerfile must not change the filesystem or re-permission paths after --image-filesystem: ${docker_line}" ;;
        esac
    fi
    if [[ "$instruction" == FROM ]]; then
        stage=""
        compile_out_copied=0
        compile_out_ran=0
        if [[ "${docker_line^^}" =~ [[:space:]]AS[[:space:]]BUILDER$ ]]; then
            stage=builder
        fi
    fi
    # Match the Dockerfile's variable reference literally, without expansion.
    # shellcheck disable=SC2016
    case "$docker_line" in
        'COPY scripts/compile_out_remote_tunnel.py /build/compile_out_remote_tunnel.py')
            if [[ "$stage" == builder ]]; then compile_out_copied=1; fi
            ;;
        'RUN python3 /build/compile_out_remote_tunnel.py /build/opencode')
            if [[ "$stage" == builder ]] && ((compile_out_copied)); then compile_out_ran=1; fi
            ;;
        'RUN OPENCODE_VERSION="${OPENCODE_VERSION#v}" bun run build -- --single')
            if [[ "$stage" == builder ]] && ((compile_out_ran)); then
                guarded_build=1
            else
                fail "Dockerfile must run the OpenTunnel compile-out in AS builder before bun run build -- --single"
            fi
            ;;
        'RUN python3 /app/scripts/exposure_policy_assert.py --lane /app --profile opencode2 --image-filesystem')
            filesystem_checked=1
            ;;
    esac
done <<<"$dockerfile_code"
((guarded_build)) \
    || fail "Dockerfile lost the OpenTunnel compile-out RUN with its source COPY before the builder build (control 8, run-96 FP-1)"
((filesystem_checked)) \
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
grep -Fxq 'COPY scripts/ ./scripts/' <<<"$dockerfile_code" \
    || fail "Dockerfile lost the root-owned COPY of scripts/"
grep -Fxq 'COPY config/ ./config/' <<<"$dockerfile_code" \
    || fail "Dockerfile lost the root-owned COPY of config/"
if grep -Eq '^\s*COPY\s+[^#]*--chown=[^#]*\b(entrypoint\.sh|health_server\.py|agent_comm_protocol\.json)\b' <<<"$dockerfile_code"; then
    fail "Dockerfile hands boot files to the agent uid — boot files must be root-owned (FB-4)"
fi
grep -Fxq 'COPY entrypoint.sh health_server.py agent_comm_protocol.json ./' <<<"$dockerfile_code" \
    || fail "Dockerfile lost the root-owned COPY of the boot files (FB-4)"
# One whole instruction, not a substring allowlist: neither the optional
# exec-bit prefix nor the a-w command may hide a suffix restoring write access.
grep -Eq '^RUN (chmod \+x /app/entrypoint\.sh /app/health_server\.py && )?chmod -R a-w /app/scripts /app/config /app/entrypoint\.sh /app/health_server\.py /app/agent_comm_protocol\.json$' <<<"$dockerfile_code" \
    || fail "Dockerfile must mark scripts, config and boot files read-only (FB-4)"

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
