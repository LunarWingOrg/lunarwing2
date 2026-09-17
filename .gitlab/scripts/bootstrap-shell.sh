#!/usr/bin/env bash
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive

apt-get update
# NOTE: shellcheck is NOT installed from apt. Debian bookworm ships 0.9.0,
# below the project floor of 0.11.0 (SC2218 false positives — MR !988). The
# shellcheck CI job installs a checksum-pinned 0.11.0 via
# .gitlab/scripts/install-shellcheck.sh instead.
apt-get install --yes --no-install-recommends \
    bash \
    ca-certificates \
    coreutils \
    curl \
    git \
    gzip \
    jq \
    p7zip-full \
    procps \
    tar \
    tmux \
    util-linux
rm -rf /var/lib/apt/lists/*
