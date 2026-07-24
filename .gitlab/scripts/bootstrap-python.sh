#!/usr/bin/env bash
set -euo pipefail

script_dir=${BASH_SOURCE[0]%/*}
repo_root=$(cd -- "$script_dir/../.." && pwd)

bash "$script_dir/bootstrap-shell.sh"
python3 -m pip install \
    --only-binary=:all: \
    --requirement "$repo_root/.gitlab/requirements/python-tests.txt"
