# Pin the Kimi Code CLI to a reviewed npm release for reproducible builds.
# Override with: docker build --build-arg KIMI_CODE_VERSION=0.39.1 ...
# Bump this default when upgrading the worker to a newer kimi-code release.
# Declared in global scope so both stages share one default; the runtime
# stage re-declares `ARG KIMI_CODE_VERSION` (no default) to consume it.
ARG KIMI_CODE_VERSION=0.39.1

# ──────────────────────────────
# Stage 1: build the bridge + tests
# ──────────────────────────────
FROM docker.io/library/node:22-bookworm-slim AS builder

WORKDIR /build

# Snapshot fallback helper — tries live mirror first, falls back to
# snapshot.debian.org if live mirror has an invalid signature.
RUN set -e; \
    (apt-get update 2>/dev/null || { \
      echo "apt-get update failed — switching to snapshot.debian.org fallback mirror" >&2; \
      sed -i 's|debian\.org|snapshot.debian.org/archive|g' /etc/apt/sources.list; \
      apt-get update; \
    }) && \
    apt-get install -y --no-install-recommends \
      python3 make g++ ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Install ws (WebSocket library) and typescript. package.json pins a reviewed release.
COPY scripts/package.json scripts/tsconfig.json ./scripts/
COPY scripts/*.ts ./scripts/
# entrypoint_config_test.js reads ../entrypoint.sh — ship it in the build
# stage so the in-build test gate validates the actual shipped entrypoint.
COPY entrypoint.sh /build/entrypoint.sh

WORKDIR /build/scripts
RUN npm install --omit=dev --ignore-scripts && \
    npm install --no-save typescript@5.9.3 @types/node@22.20.1 @types/ws@8.18.1 && \
    npx tsc -p tsconfig.json && \
    npm test && \
    rm -rf node_modules/.cache

# ──────────────────────────────
# Stage 2: runtime image
# ──────────────────────────────
FROM docker.io/library/node:22-bookworm-slim AS runtime

# WITH_TOOLCHAINS=false (default): slim runtime — no Rust/Go/C++ toolchains.
# Pass --build-arg WITH_TOOLCHAINS=true for the fat image (CI, dev tenants).
ARG WITH_TOOLCHAINS=false

LABEL org.opencontainers.image.title="LunarWing Kimi Code Worker" \
      org.opencontainers.image.description="Persistent Kimi Code CLI worker with WebSocket agent communication" \
      org.opencontainers.image.version="1.0.0" \
      org.opencontainers.image.toolchain-profile="${WITH_TOOLCHAINS}"

# Runtime packages: always installed (needed by the worker itself).
RUN set -e; \
    (apt-get update 2>/dev/null || { \
      echo "apt-get update failed — switching to snapshot.debian.org fallback mirror" >&2; \
      sed -i 's|debian\.org|snapshot.debian.org/archive|g' /etc/apt/sources.list; \
      apt-get update; \
    }) && \
    apt-get install -y --no-install-recommends \
      tini \
      python3 \
      python3-pip \
      python3-venv \
      git \
      ripgrep \
      curl \
      wget \
      ca-certificates \
      openssh-client \
      jq \
      neovim \
    && rm -rf /var/lib/apt/lists/*

# Toolchain packages: only when WITH_TOOLCHAINS=true.
RUN if [ "${WITH_TOOLCHAINS}" = "true" ]; then \
      set -e; \
      (apt-get update 2>/dev/null || { \
        echo "apt-get update failed — switching to snapshot.debian.org fallback mirror" >&2; \
        sed -i 's|debian\.org|snapshot.debian.org/archive|g' /etc/apt/sources.list; \
        apt-get update; \
      }) && \
      apt-get install -y --no-install-recommends \
        python3-dev \
        build-essential \
        gcc \
        g++ \
        libc-dev \
        binutils \
        cmake \
        ninja-build \
        clang \
        lld \
        llvm \
        libclang-dev \
        pkg-config \
        libssl-dev \
        zlib1g-dev \
        libsqlite3-dev \
        libpq-dev \
        libffi-dev \
        libz-dev \
        liblzma-dev \
        libzstd-dev \
        libcurl4-openssl-dev \
        libxml2-dev \
        protobuf-compiler \
        libprotobuf-dev \
        musl-tools \
        autoconf \
        automake \
        libtool \
        meson \
        nodejs \
        npm \
      && rm -rf /var/lib/apt/lists/*; \
    fi

# ── GitHub CLI ─────────────────
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg \
    && chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
      > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update && apt-get install -y --no-install-recommends gh \
    && rm -rf /var/lib/apt/lists/*

# ── Rust toolchain (only with WITH_TOOLCHAINS=true) ───────────────────────────
ENV RUSTUP_HOME=/usr/local/rustup

RUN if [ "${WITH_TOOLCHAINS}" = "true" ]; then \
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | CARGO_HOME=/usr/local/cargo sh -s -- -y --default-toolchain 1.96.1 \
      && chmod -R a+rX /usr/local/rustup /usr/local/cargo; \
    fi

# ── Go toolchain (only with WITH_TOOLCHAINS=true) ──────────────────────────────
RUN if [ "${WITH_TOOLCHAINS}" = "true" ]; then \
      ARCH=$(dpkg --print-architecture) \
      && curl -fsSL "https://go.dev/dl/go1.24.3.linux-${ARCH}.tar.gz" \
        | tar -C /usr/local -xz; \
    fi
ENV GOROOT=/usr/local/go \
    GOPATH=/home/kimi/go \
    CARGO_HOME=/home/kimi/.cargo \
    PATH=/home/kimi/.cargo/bin:/usr/local/cargo/bin:/usr/local/go/bin:/home/kimi/go/bin:$PATH

# ── non-root user ──────────────────────
# Keep IDs synchronized with ic/scripts/lunarwing-mt-admin.sh. Rootless Podman
# maps the tenant's host identity onto these IDs so private read-only mounts
# remain accessible without making state world-readable.
RUN groupadd --gid 10002 kimi \
    && useradd --uid 10002 --gid 10002 -m -d /home/kimi kimi

# ── app layout ─────────────────
WORKDIR /app

COPY --from=builder /build/scripts/ /app/scripts/
COPY --chown=kimi:kimi entrypoint.sh health_server.py agent_comm_protocol.json ./
COPY --chown=kimi:kimi config/ ./config/

RUN chmod +x /app/entrypoint.sh /app/health_server.py

# ── Kimi Code CLI ─────────────────────────────────────────────────────────────
# Install the official Kimi Code CLI from npm. Pinned to a known-good release.
# Consume the global pin and record it in the runtime image so operators can
# tell which upstream kimi-code release a running worker was built from
# (surfaced on the /health endpoint as `kimi_code_version`).
ARG KIMI_CODE_VERSION
ENV KIMI_CODE_IMAGE_VERSION="${KIMI_CODE_VERSION}"
RUN npm install -g @moonshot-ai/kimi-code@${KIMI_CODE_VERSION} && \
    kimi --version && \
    npm cache clean --force

# ── XDG directories for kimi state ────────────────────────────────────────────
RUN mkdir -p /home/kimi/.kimi-code \
             /home/kimi/.cargo \
             /home/kimi/go \
             /workspace \
             /var/log/lunarwing \
             /app/results \
    && chown -R kimi:kimi /home/kimi /workspace /var/log/lunarwing /app/results

# ── ports ──────────────────────
EXPOSE 8443
EXPOSE 9090

# ── health check ───────────────────────
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD curl -sf http://127.0.0.1:8443/ready || exit 1

USER 10002:10002

ENTRYPOINT ["/usr/bin/tini", "--", "/app/entrypoint.sh"]
CMD ["--mode", "websocket"]
