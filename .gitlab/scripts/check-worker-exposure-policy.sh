#!/usr/bin/env bash
# WI#357 control 4: source/config exposure-policy checks. Dockerfile wiring is
# owned by .gitlab/go/dockerfile-parse and its required CI job (WI#425).
set -euo pipefail

# No git: the ci-config job runs this in python:3.12-slim-bookworm, which has
# none. The script sits at <repo root>/.gitlab/scripts/.
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
lane="$repo_root/opencode2.4lunarwing"
v1_lane="$repo_root/opencode4lunarwing"
lunardepth_lane="$repo_root/lunardepth4lunarwing"

failed=0
fail() { echo "ERROR: $*" >&2; failed=1; }

# WI#738: name the nested suite that failed. Each lane suite runs with -u -v,
# so a failure (or a job cut off mid-suite) prints the test it was in, and the
# trailing block names the suite itself at the end of stderr, where the outer
# assertion's message is read. Before this the chain was three levels of
# dot-mode output inside one 11,000-character line (job 182901).
nested_failures=()
run_nested() {
    local label=$1
    shift
    if ! "$@"; then
        nested_failures+=("$label")
        failed=1
    fi
}

run_nested opencode2.4lunarwing/scripts/exposure_policy_assert.py \
    python3 "$lane/scripts/exposure_policy_assert.py" --lane "$lane" --profile opencode2
run_nested "opencode4lunarwing (profile opencode-v1)" \
    python3 "$lane/scripts/exposure_policy_assert.py" --lane "$v1_lane" --profile opencode-v1
run_nested opencode2.4lunarwing/scripts/exposure_policy_assert_test.py \
    python3 -u "$lane/scripts/exposure_policy_assert_test.py" -v
run_nested opencode2.4lunarwing/scripts/console_policy_eval_test.py \
    python3 -u "$lane/scripts/console_policy_eval_test.py" -v
run_nested opencode2.4lunarwing/scripts/image_config_policy_test.py \
    python3 -u "$lane/scripts/image_config_policy_test.py" -v
run_nested opencode2.4lunarwing/scripts/compile_out_remote_tunnel_test.py \
    python3 -u "$lane/scripts/compile_out_remote_tunnel_test.py" -v
run_nested opencode2.4lunarwing/scripts/entrypoint_config_test.py \
    python3 -u "$lane/scripts/entrypoint_config_test.py" -v
run_nested lunardepth4lunarwing/scripts/exposure_policy_assert.py \
    python3 "$lunardepth_lane/scripts/exposure_policy_assert.py" --source "$lunardepth_lane"
run_nested lunardepth4lunarwing/scripts/exposure_policy_assert_test.py \
    python3 -u "$lunardepth_lane/scripts/exposure_policy_assert_test.py" -v
run_nested lunardepth4lunarwing/scripts/image_config_policy_test.py \
    python3 -u "$lunardepth_lane/scripts/image_config_policy_test.py" -v
run_nested lunardepth4lunarwing/scripts/render_overlay_test.py \
    python3 -u "$lunardepth_lane/scripts/render_overlay_test.py" -v
run_nested lunardepth4lunarwing/scripts/runtime_smoke_test.py \
    python3 -u "$lunardepth_lane/scripts/runtime_smoke_test.py" -v
run_nested lunardepth4lunarwing/scripts/verify_exposure_out_of_band_test.py \
    python3 -u "$lunardepth_lane/scripts/verify_exposure_out_of_band_test.py" -v

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

if ((${#nested_failures[@]})); then
    echo "ERROR: nested lane test suites failed (WI#738):" >&2
    printf 'ERROR:   %s\n' "${nested_failures[@]}" >&2
fi

if ((failed)); then
    exit 1
fi

echo "worker exposure policy guard: ok"
