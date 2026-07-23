#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

for test_script in ic/scripts/tests/*.sh; do
    bash "$test_script"
done

bash ic/scripts/test-mt-admin-darkirc-flag.sh
bash ic/scripts/test-mt-onboard.sh
bash scripts/tests/test-build-lunarwing.sh
bash ic-infrastructure-health-check/tests/run-all.sh
bash ic-infrastructure-health-check/tests/test-health-lunarvision.sh
