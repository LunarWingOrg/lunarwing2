#!/bin/sh
# Installed as /etc/profile.d/lunarwing-toolchain.sh in the pebble worker image.
#
# WI#334: upstream pebble's task shell may spawn a login or otherwise
# sanitised shell that drops the image-level ENV (RUSTUP_HOME/CARGO_HOME/
# GOROOT/GOPATH/PATH), hiding cargo and go from task commands. Login shells
# source this snippet after Debian's /etc/profile resets PATH, restoring the
# toolchain environment. Non-login sanitised shells are covered by the
# /usr/local/bin symlinks and the rustup wrapper installed alongside this
# file (see pebble4lunarwing/Dockerfile).
#
# Every PATH edit is guarded so this file is inert in the slim
# (WITH_TOOLCHAINS=false) image where the toolchain directories do not exist.

export RUSTUP_HOME="${RUSTUP_HOME:-/usr/local/rustup}"
export CARGO_HOME="${CARGO_HOME:-/usr/local/cargo}"
export GOROOT="${GOROOT:-/usr/local/go}"
export GOPATH="${GOPATH:-/root/go}"

if [ -d /usr/local/cargo/bin ]; then
  case ":${PATH}:" in
    *":/usr/local/cargo/bin:"*) ;;
    *) PATH="/usr/local/cargo/bin:${PATH}" ;;
  esac
fi

if [ -d /usr/local/go/bin ]; then
  case ":${PATH}:" in
    *":/usr/local/go/bin:"*) ;;
    *) PATH="${PATH}:/usr/local/go/bin" ;;
  esac
fi

export PATH
