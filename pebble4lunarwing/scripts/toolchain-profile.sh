#!/bin/sh
# Installed as /etc/profile.d/lunarwing-toolchain.sh in the pebble worker image.
#
# WI#334: upstream pebble's Bash tool spawns every task command as
# `/bin/sh -lc <cmd>` with the process environment inherited — there is no
# env_clear (pebble/crates/runtime/src/bash.rs, pinned PEBBLE_REF; the
# in-repo pebble/ copy is byte-identical for the spawn code). On the Debian
# base /bin/sh is dash, and a login shell reads /etc/profile, which resets
# PATH to the system default and then sources /etc/profile.d/*.sh. The rest
# of the image-level ENV survives; only the Dockerfile ENV PATH entries for
# the toolchains are lost (the WI#320 gauntlet F6 failure). This snippet
# re-adds them after that reset.
#
# This file MUST stay POSIX/dash-clean: dash aborts the whole task command
# when a sourced profile.d file has a syntax error, so a bashism here breaks
# every pebble task in both image variants (the COPY is unconditional).
#
# Non-login sanitised shells (no profile sourcing at all) are covered by the
# /usr/local/bin real-binary symlinks and the rustup wrapper installed
# alongside this file (see pebble4lunarwing/Dockerfile). The PATH restore
# below reproduces the image ENV order — rustup proxies first (so
# rust-toolchain.toml and `cargo +toolchain` keep working), then GOROOT/bin,
# then GOPATH/bin — so login shells resolve go through GOROOT just like the
# image ENV, and only sanitised shells resolve through the symlinks.

export RUSTUP_HOME="${RUSTUP_HOME:-/usr/local/rustup}"
export CARGO_HOME="${CARGO_HOME:-/usr/local/cargo}"
export GOROOT="${GOROOT:-/usr/local/go}"
export GOPATH="${GOPATH:-/root/go}"

# _lw_tc_prepend DIR: prepend DIR to PATH once, but only when DIR is an
# absolute path to an existing directory. A relative or empty home must
# never become a PATH entry (it would resolve against the caller's current
# directory), and an empty/unset PATH must gain no empty entry (same
# meaning). The existence guard keeps this file inert in the slim
# (WITH_TOOLCHAINS=false) image, where the toolchain directories do not
# exist.
_lw_tc_prepend() {
  case $1 in
    /*) ;;
    *) return 0 ;;
  esac
  [ -d "$1" ] || return 0
  if [ -z "${PATH:-}" ]; then
    PATH="$1"
    return 0
  fi
  case ":${PATH}:" in
    *":$1:"*) ;;
    *) PATH="$1:${PATH}" ;;
  esac
}

# Prepend in reverse priority order; the last prepend sorts first.
_lw_tc_prepend "${GOPATH}/bin"
_lw_tc_prepend "${GOROOT}/bin"
_lw_tc_prepend "${CARGO_HOME}/bin"
unset -f _lw_tc_prepend

export PATH
