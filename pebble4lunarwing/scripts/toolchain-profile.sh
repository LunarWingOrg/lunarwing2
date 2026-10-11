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
# alongside this file (see pebble4lunarwing/Dockerfile). The PATH ordering
# below keeps the rustup proxies ahead of those symlinks so
# rust-toolchain.toml and `cargo +toolchain` keep working in login shells.
#
# Every PATH edit is guarded on the directory existing, so this file is
# inert in the slim (WITH_TOOLCHAINS=false) image where the toolchain
# directories do not exist.

export RUSTUP_HOME="${RUSTUP_HOME:-/usr/local/rustup}"
export CARGO_HOME="${CARGO_HOME:-/usr/local/cargo}"
export GOROOT="${GOROOT:-/usr/local/go}"
export GOPATH="${GOPATH:-/root/go}"

# Rust first: the rustup proxies in CARGO_HOME must sort ahead of the
# /usr/local/bin real-binary symlinks (see the header).
if [ -d "${CARGO_HOME}/bin" ]; then
  case ":${PATH}:" in
    *":${CARGO_HOME}/bin:"*) ;;
    *) PATH="${CARGO_HOME}/bin:${PATH}" ;;
  esac
fi

if [ -d "${GOROOT}/bin" ]; then
  case ":${PATH}:" in
    *":${GOROOT}/bin:"*) ;;
    *) PATH="${PATH}:${GOROOT}/bin" ;;
  esac
fi

# GOPATH/bin holds `go install`ed tools (goimports, ...); the login reset
# drops it just like the toolchain dirs.
if [ -d "${GOPATH}/bin" ]; then
  case ":${PATH}:" in
    *":${GOPATH}/bin:"*) ;;
    *) PATH="${PATH}:${GOPATH}/bin" ;;
  esac
fi

export PATH
