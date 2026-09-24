# Lean generic sandbox image for the LunarWing podman sandbox.
#
# The generic sandbox (`ic/src/sandbox/`) runs short-lived shell commands from
# the `shell` tool inside hardened containers using this kind of image. It is
# intentionally smaller than the full worker image (`ic/Dockerfile.worker`,
# which contains the lunarwing binary): the sandbox only needs common build
# tooling for commands like `make`, `npm test`, or `python3` (language-specific
# toolchains such as Rust/Cargo can be added in derived images when needed).
#
# Manual build from the repository root (mt-admin builds in the admin store):
#   podman build -t lunarwing-sandbox:latest -f ic/docker/sandbox.Dockerfile ic/docker/
#
# mt-admin builds this image for every new tenant (WI#223) and writes
# SANDBOX_IMAGE=lunarwing-sandbox:latest + SANDBOX_AUTO_PULL=false into the
# tenant env; manual build is available via
# `sudo ic/scripts/lunarwing-mt-admin.sh build-sandbox-image`. The in-code
# default for single-node hosts remains `lunarwing-worker:latest`. Auto-pull
# only works for registry-hosted images — local images like this must be built
# (or shipped via `podman save | podman load`) into each tenant store; mt-admin
# distributes it into each rootless tenant store on add-tenant. The build
# context is ic/docker/ (not ic/): this Dockerfile has no COPY/ADD, and ic/
# carries the vendor/ tree that must stay in-context for Dockerfile.worker.

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    bash \
    ca-certificates \
    curl \
    git \
    openssh-client \
    build-essential \
    pkg-config \
    python3 \
    python3-pip \
    nodejs \
    npm \
    ripgrep \
    jq \
    unzip \
    procps \
    && rm -rf /var/lib/apt/lists/*

# Non-root user; UID/GID 1000 must match the sandbox runner's `--user
# 1000:1000` (with podman >= 5 the runner also pins keep-id:uid=1000,gid=1000
# so the mounted /workspace is owned by this user regardless of host UID).
RUN groupadd -g 1000 sandbox \
    && useradd -m -u 1000 -g sandbox sandbox \
    && mkdir -p /workspace /home/sandbox/.cargo/registry \
    && chown -R sandbox:sandbox /workspace /home/sandbox

USER sandbox
WORKDIR /workspace

CMD ["bash"]
