# Lean generic sandbox image for the LunarWing podman sandbox.
#
# The generic sandbox (`ic/src/sandbox/`) runs short-lived shell commands from
# the `shell` tool inside hardened containers using this kind of image. It is
# intentionally smaller than the full worker image (`ic/Dockerfile.worker`,
# which contains the lunarwing binary): the sandbox only needs common build
# tooling so commands like `cargo check`, `npm test`, or `python -m pytest`
# work out of the box.
#
# Build (rootless podman):
#   podman build -t lunarwing-sandbox:latest -f docker/sandbox.Dockerfile docker/
#
# The default configured image remains `lunarwing-worker:latest`; point
# SANDBOX_IMAGE (or [sandbox].image) at lunarwing-sandbox:latest to use this
# one. Auto-pull only works for registry-hosted images — local images like
# this must be built (or shipped via `podman save | podman load`) into each
# tenant store.

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
RUN useradd -m -u 1000 -g 1000 sandbox \
    && mkdir -p /workspace /home/sandbox/.cargo/registry \
    && chown -R sandbox:sandbox /workspace /home/sandbox

USER sandbox
WORKDIR /workspace

CMD ["bash"]
