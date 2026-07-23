#!/usr/bin/env bash
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive

apt-get update
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
    shellcheck \
    tar \
    tmux \
    util-linux
rm -rf /var/lib/apt/lists/*
