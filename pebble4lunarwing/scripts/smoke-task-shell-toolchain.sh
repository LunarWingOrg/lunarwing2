#!/bin/sh
# WI#334 smoke check for the pebble worker gauntlet: prove that
# `cargo --version` and `go version` (plus rustc/rustup) succeed from the
# worker's task shell even when that shell is a sanitised login shell.
#
# Upstream pebble's shell tool may drop the image-level ENV (login shell /
# scrubbed environment), which is how the WI#320 gauntlet (F6) lost
# cargo/go from PATH. This probe reproduces that shape with
# `env -i bash -lc` inside the worker image or a running worker container.
#
# usage:
#   smoke-task-shell-toolchain.sh [image [ref]]   # default ref: lunarwing-worker-pebble:latest
#   smoke-task-shell-toolchain.sh container <name>
#
# Exit status is the probe's: 0 only when every toolchain command resolves
# and runs from the scrubbed login shell.
set -eu

probe='set -e; command -v cargo; command -v rustc; command -v go; command -v rustup; cargo --version; rustc --version; go version; rustup --version'

runtime=""
for candidate in podman docker; do
  if command -v "${candidate}" >/dev/null 2>&1; then
    runtime="${candidate}"
    break
  fi
done
[ -n "${runtime}" ] || { echo "smoke: podman or docker is required" >&2; exit 2; }

mode="${1:-image}"
case "${mode}" in
  image)
    ref="${2:-lunarwing-worker-pebble:latest}"
    echo "smoke: probing image ${ref} with a sanitised login shell via ${runtime}"
    "${runtime}" run --rm --entrypoint env "${ref}" -i bash -lc "${probe}"
    ;;
  container)
    name="${2:?usage: smoke-task-shell-toolchain.sh container <name>}"
    echo "smoke: probing container ${name} with a sanitised login shell via ${runtime}"
    "${runtime}" exec "${name}" env -i bash -lc "${probe}"
    ;;
  *)
    echo "usage: smoke-task-shell-toolchain.sh [image [ref] | container <name>]" >&2
    exit 2
    ;;
esac

echo "smoke: PASS — cargo/rustc/go/rustup resolve and run from the scrubbed task-shell environment"
