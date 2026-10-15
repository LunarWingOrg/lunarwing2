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
# Run-112 HD-1: BuildKit's continuation whitespace class is spaces and tabs
# only. bash's [[:space:]] is wider even in the C locale (VT, FF, CR) and
# locale-dependent under C.UTF-8 (U+1680, U+2028, U+3000), so the folds and
# tests below use literal-character bracket expressions over these two
# variables instead — quoted characters in a bracket expression are literal
# set members, independent of locale.
_hd1_spacetab=$' \t'
_hd1_ctl=$'\x0b\x0c\x0d'
# Run-110 GX-1: bash's read drops NUL bytes that BuildKit keeps, so a NUL
# after a trailing backslash, or a NUL-prefixed line inside a continuation,
# ends an instruction for BuildKit while this loop joins the next one.
if ! python3 -c 'import sys; sys.exit(b"\0" in open(sys.argv[1], "rb").read())' "$dockerfile"; then
    fail "Dockerfile contains NUL bytes (run-110 GX-1)"
fi
_first_line=1
while IFS= read -r _raw || [[ -n "$_raw" ]]; do
    # Run-103 GD-1: Docker strips a UTF-8 BOM before the first line; without
    # this a BOM-prefixed directive is not even recognized as a comment.
    # Run-110 GX-1: only before the first line. A BOM on a later line is
    # content for BuildKit, so it must reach the GJ-1 refusal below.
    if ((_first_line)); then
        _raw="${_raw#$'\xef\xbb\xbf'}"
        _first_line=0
    fi
    _line="${_raw#"${_raw%%[![:space:]]*}"}"
    if [[ "$_line" == \#* ]]; then
        # Run-97 FQ-3: a parser directive changes how Docker itself parses the
        # file (escape=` makes every backslash a literal, so this script's
        # continuation joining no longer matches Docker's instruction split).
        # Refuse the directives outright instead of parsing Docker's grammar.
        # Run-103 GD-1: the grammar allows whitespace around the '='.
        if [[ "${_line,,}" =~ ^#[[:space:]]*(escape|syntax|check)[[:space:]]*= ]]; then
            fail "Dockerfile parser directives (# escape=/# syntax=/# check=) are not allowed (run-97 FQ-3): ${_line}"
        fi
        continue
    fi
    [[ -n "$_line" ]] || continue
    # Run-106 GJ-1: BuildKit trims leading Unicode whitespace (Go
    # unicode.IsSpace — U+00A0, U+0085, U+202F, …) before directive
    # detection, but the [[:space:]] trim above is ASCII-only, so a parser
    # directive behind such a character is neither comment nor instruction
    # here while the builder honours it. In the C locale every byte of a
    # valid instruction, continuation or comment line is printable ASCII;
    # refuse anything else, fail-closed.
    if ! ( export LC_ALL=C; [[ "${_line:0:1}" == [[:print:]] ]] ); then
        fail "Dockerfile line does not start with printable ASCII (run-106 GJ-1): ${_raw}"
    fi
    # Run-112 HD-1: a VT, FF or CR byte anywhere in a line makes this guard's
    # whitespace folds and BuildKit's parse diverge (BuildKit keeps the byte;
    # the guard used to fold it into the [[:space:]] join), and a lone CR also
    # means a CRLF file this guard never promised to parse. No lane Dockerfile
    # carries these bytes; refuse them fail-closed.
    if [[ "$_raw" == *["${_hd1_ctl}"]* ]]; then
        fail "Dockerfile line contains a vertical tab, form feed or carriage return byte (run-112 HD-1): ${_raw}"
    fi
    # Run-106 GJ-2: BuildKit does not continue a line ending in an escaped
    # escape token (two backslashes — its lineContinuationRegex requires a
    # single trailing token), while this script joins any backslash-final
    # line and buildah/podman joins this one too. A LABEL ending in two
    # backslashes would therefore hide the next instruction (e.g. a
    # post-assert root RUN) from the continuation joining. Refuse the
    # spelling outright. Run-109 GW-2: trailing whitespace after a single
    # backslash is also unsafe: BuildKit joins it, hiding a required assert
    # inside the previous instruction while this guard counts a separate RUN.
    _trimmed="${_line%"${_line##*[![:space:]]}"}"
    if [[ "$_trimmed" == *\\\\ ]]; then
        fail "Dockerfile line ends in an escaped backslash (\\\\) — not a continuation for BuildKit (run-106 GJ-2): ${_raw}"
    fi
    if [[ "$_trimmed" == *\\ && "$_trimmed" != "$_line" ]]; then
        fail "Dockerfile continuation backslash must not have trailing whitespace (run-109 GW-2): ${_raw}"
    fi
    # Heredoc bodies can swallow required instructions too. Refuse both the
    # literal token and a token split across continued physical lines.
    if [[ "$_line" == *'<<'* || "$_trimmed" == *'<\' ]]; then
        fail "Dockerfile heredocs are not allowed (run-109 GW-2): ${_raw}"
    fi
    if [[ "$_line" == *\\ ]]; then
        _part="${_line%\\}"
        # Run-111 GZ-1: BuildKit keeps the character before the backslash and
        # appends the next line with no separator, while this loop joins with
        # a space — so `SHEL\` + `L [...]` is one SHELL instruction for the
        # builder but the word SHEL here, and `ARG LUNARWING_WRAPPER_\` +
        # `SHA256=…` hides a protected name from the checks below. Require a
        # space or tab (run-112 HD-1: BuildKit's own class) before the
        # continuation backslash; a backslash that is the line's whole content
        # is refused too.
        if [[ -z "$_part" || "${_part: -1}" != ["${_hd1_spacetab}"] ]]; then
            fail "Dockerfile continuation backslash must be preceded by whitespace (run-111 GZ-1): ${_raw}"
        fi
        _join+="${_part%"${_part##*[!"${_hd1_spacetab}"]}"} "
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
# Run-97 FQ-1: the wrapper install + binary marker scan must live in the same
# stage as the final --image-filesystem assertion — BuildKit and buildah skip
# unused stages, so the exact line in a dead stage satisfies presence checks
# while the shipped image has neither the wrapper (control 2) nor the marker
# scan (control 5).
stage_index=0
scan_stage=""
assert_stage=""
anchor_stage=""
# Run-97 FQ-2: a USER root tail would void the root-owned read-only controls
# outside mt-admin-managed deployments (mt-admin itself checks the image user).
last_user=""
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
        stage_index=$((stage_index + 1))
        compile_out_copied=0
        compile_out_ran=0
        if [[ "${docker_line^^}" =~ [[:space:]]AS[[:space:]]BUILDER$ ]]; then
            stage=builder
        fi
    fi
    if [[ "$instruction" == USER ]]; then
        last_user="$docker_line"
    fi
    # Run-109 GW-3: an ENV overrides the build ARG, and SHELL can hide or
    # replace it for every RUN. These are cheap drift guards; a root RUN can
    # still rewrite the checker itself. Out-of-band verification is deferred.
    if [[ "$instruction" == SHELL ]]; then
        fail "Dockerfile SHELL overrides are not allowed (run-109 GW-3)"
    fi
    if [[ "$docker_line" == 'ARG LUNARWING_WRAPPER_SHA256=' ]]; then
        anchor_stage=$stage_index
    elif [[ "$docker_line" == *LUNARWING_WRAPPER_SHA256* \
         || "$docker_line" == *LUNARWING_REAL_OPENCODE* \
         || "$docker_line" == *OPENCODE_BIN* ]]; then
        fail "Dockerfile must not override exposure-policy variables (run-109 GW-3): ${docker_line}"
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
            assert_stage=$stage_index
            ;;
        "$binary_scan")
            scan_stage=$stage_index
            ;;
    esac
done <<<"$dockerfile_code"
((guarded_build)) \
    || fail "Dockerfile lost the OpenTunnel compile-out RUN with its source COPY before the builder build (control 8, run-96 FP-1)"
((filesystem_checked)) \
    || fail "Dockerfile must verify installed policy-file ownership and modes (FB-4)"
[[ -n "$scan_stage" && "$scan_stage" == "$assert_stage" ]] \
    || fail "Dockerfile wrapper install + binary marker scan must run in the same stage as the final --image-filesystem assertion (run-97 FQ-1: unused stages are skipped at build time)"
[[ -n "$anchor_stage" && "$anchor_stage" == "$assert_stage" ]] \
    || fail "Dockerfile must declare ARG LUNARWING_WRAPPER_SHA256= in the final assertion stage (run-109 GW-3)"
[[ -n "$last_user" && "$last_user" == "USER 10003:10003" ]] \
    || fail "Dockerfile's last USER instruction must be exactly 'USER 10003:10003' (run-97 FQ-2)"

# ENV keys are expanded by BuildKit, so a split protected name such as
# ${PREFIX}SHA256 escapes the literal checks above. Accept only literal
# identifier=value assignments; shlex keeps quoted/expanded values intact.
python3 -c '
import re
import shlex
import sys

protected = {"LUNARWING_WRAPPER_SHA256", "LUNARWING_REAL_OPENCODE", "OPENCODE_BIN"}
failed = False
for line in sys.stdin:
    if line.split(None, 1)[:1] != ["ENV"]:
        continue
    try:
        assignments = shlex.split(line)[1:]
    except ValueError:
        assignments = []
    if not assignments or any(
        not re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", assignment)
        or assignment.split("=", 1)[0] in protected
        for assignment in assignments
    ):
        print("ERROR: Dockerfile ENV requires literal, unprotected key=value assignments "
              "(no expanded or legacy keys; run-109 GW-3): " + line.strip(), file=sys.stderr)
        failed = True
sys.exit(1 if failed else 0)
' <<<"$dockerfile_code" || failed=1

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
