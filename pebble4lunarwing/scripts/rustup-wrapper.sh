#!/bin/sh
# Installed as /usr/local/bin/rustup in the pebble worker image (fat
# toolchain profile only).
#
# WI#334: the rustup shims under /usr/local/cargo/bin locate the toolchain
# via RUSTUP_HOME/CARGO_HOME, which a sanitised task shell may have dropped.
# This wrapper re-exports the image defaults before delegating, so `rustup`
# stays usable no matter how the shell was spawned.
set -eu

export RUSTUP_HOME="${RUSTUP_HOME:-/usr/local/rustup}"
export CARGO_HOME="${CARGO_HOME:-/usr/local/cargo}"
exec /usr/local/cargo/bin/rustup "$@"
