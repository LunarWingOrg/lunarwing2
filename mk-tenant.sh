#!/usr/bin/env bash
set -euo pipefail
MT="sudo bash ic/scripts/lunarwing-mt-admin.sh"
T="${1:?usage: mk-tenant.sh <tenant-name>}"
$MT add-tenant "$T" --with-opencode --with-opencode2 --with-kimi --with-pebble
tmux new-session -d -s "${T}-build" "$MT build-tenant $T --with-wasm --require-wasm --with-opencode --with-opencode2 --with-kimi --with-pebble --with-toolchains 2>&1 | tee /tmp/${T}-build.log"
tmux attach-session -t "${T}-build"
$MT install-wasm "$T"
$MT start-tenant "$T"
$MT status "$T"
