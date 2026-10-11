#!/bin/sh
# WI#334 smoke probe for the pebble worker image: prove that cargo, rustc,
# go and rustup resolve and run from the worker's task shell.
#
# Upstream pebble's Bash tool spawns every task command as
# `/bin/sh -lc <cmd>` with the process environment inherited
# (pebble/crates/runtime/src/bash.rs, pinned PEBBLE_REF). On the Debian base
# that login shell reads /etc/profile, which resets PATH — dropping the
# Dockerfile ENV toolchain entries (the WI#320 gauntlet F6 failure) — and
# then sources /etc/profile.d/*.sh. Leg 1 below probes exactly that spawn
# mode with the image environment. Leg 2 probes a fully sanitised non-login
# shell (`env -i /bin/sh -c`), where only the /usr/local/bin real-binary
# symlinks and the rustup wrapper can provide the toolchains — this is the
# leg that exercises them (in leg 1 the profile.d snippet's CARGO_HOME
# prepend shadows the symlinks by design).
#
# usage:
#   smoke-task-shell-toolchain.sh [image [ref]]   # default ref: lunarwing-worker-pebble:latest
#   smoke-task-shell-toolchain.sh container <name>
#
# Exit status: 0 only when every probe succeeds in both legs. The `command
# -v` lines in the output are the evidence of which binary resolved. Image
# mode runs with --network=none. Container mode probes a live, untrusted
# worker and is a diagnostic, never a gate.
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

run_leg() {
  desc=$1
  shift
  echo "smoke: ${desc}"
  if "$@"; then
    echo "smoke: ${desc} — ok"
  else
    rc=$?
    echo "smoke: FAIL — ${desc} (exit ${rc})" >&2
    exit 1
  fi
}

mode="${1:-image}"
case "${mode}" in
  image)
    ref="${2:-lunarwing-worker-pebble:latest}"
    run_leg "task-shell spawn mode (/bin/sh -lc, image env) on image ${ref}" \
      "${runtime}" run --rm --network=none --entrypoint /bin/sh "${ref}" -lc "${probe}"
    run_leg "sanitised shell (env -i /bin/sh -c) on image ${ref}" \
      "${runtime}" run --rm --network=none --entrypoint env "${ref}" -i /bin/sh -c "${probe}"
    ;;
  container)
    name="${2:?usage: smoke-task-shell-toolchain.sh container <name>}"
    run_leg "task-shell spawn mode (/bin/sh -lc, image env) in container ${name}" \
      "${runtime}" exec "${name}" /bin/sh -lc "${probe}"
    run_leg "sanitised shell (env -i /bin/sh -c) in container ${name}" \
      "${runtime}" exec "${name}" env -i /bin/sh -c "${probe}"
    ;;
  *)
    echo "usage: smoke-task-shell-toolchain.sh [image [ref] | container <name>]" >&2
    exit 2
    ;;
esac

echo "smoke: PASS — cargo/rustc/go/rustup resolve and run from the task shell and from a fully sanitised shell"
