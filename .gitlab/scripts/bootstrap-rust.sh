#!/usr/bin/env bash
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive

apt-get update
apt-get install --yes --no-install-recommends \
    build-essential \
    ca-certificates \
    clang \
    cmake \
    curl \
    git \
    libclang-dev \
    libssl-dev \
    perl \
    pkg-config \
    protobuf-compiler \
    python3 \
    util-linux
rm -rf /var/lib/apt/lists/*

rust_version=$(rustc --version)
if [[ "$rust_version" != rustc\ 1.92.0\ * ]]; then
    echo "ERROR: expected rustc 1.92.0, found $rust_version" >&2
    exit 1
fi

if ! command -v rustfmt >/dev/null 2>&1; then
    rustup component add --toolchain 1.92.0 rustfmt
fi
if ! command -v cargo-clippy >/dev/null 2>&1; then
    rustup component add --toolchain 1.92.0 clippy
fi
