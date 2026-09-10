#!/usr/bin/env bash
# Run the shell harness suite with per-harness JUnit telemetry.
#
# Each harness becomes one JUnit testcase named after its script; a nonzero
# exit fails the job and is recorded as a failure carrying its log tail.
# The combined report is written to junit-shell.xml at the repository root.
set -uo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root" || exit 1

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

harnesses=(
    ic/scripts/tests/*.sh
    ic/scripts/test-mt-admin-darkirc-flag.sh
    ic/scripts/test-mt-onboard.sh
    scripts/tests/test-build-lunarwing.sh
    ic-infrastructure-health-check/tests/run-all.sh
)

status=0
results_tsv="$workdir/results.tsv"
: > "$results_tsv"

for harness in "${harnesses[@]}"; do
    name=$(basename "$harness")
    log="$workdir/$name.log"
    echo "=== shell harness: $harness ==="
    started=$SECONDS
    if bash "$harness" >"$log" 2>&1; then
        result=passed
        message=""
    else
        result=failed
        message=$(tail -n 40 "$log" | tr '\n' ' ' | tr -d '\r')
        status=1
        echo "harness failed: $harness (log tail follows)"
        tail -n 40 "$log"
    fi
    duration=$((SECONDS - started))
    printf '%s\t%s\t%s\t%s\n' "$name" "$result" "$duration" "$message" >> "$results_tsv"
done

python3 - "$results_tsv" <<'PYEOF'
import pathlib
import sys

sys.path.insert(0, ".gitlab/scripts")
import junit_report as junit

records = []
for line in pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines():
    name, result, duration, message = line.split("\t", 3)
    records.append(
        junit.record("shell-harnesses", name, result, duration=float(duration), message=message)
    )
junit.write(pathlib.Path("junit-shell.xml"), records)
PYEOF

if [ "$status" -ne 0 ]; then
    echo "ERROR: one or more shell harnesses failed" >&2
    exit 1
fi

echo "Shell harnesses passed."
