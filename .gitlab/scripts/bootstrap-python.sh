#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)

bash "$repo_root/.gitlab/scripts/bootstrap-shell.sh"
python3 -m pip install \
    --only-binary=:all: \
    --requirement "$repo_root/.gitlab/requirements/python-tests.txt"
