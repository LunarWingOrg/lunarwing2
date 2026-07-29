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

# Install ws (WebSocket library) and typescript. The package.json pins ws@8.18.3.
COPY scripts/package.json scripts/tsconfig.json ./scripts/
COPY scripts/*.ts ./scripts/

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

LABEL org.opencontainers.image.title="LunarWing Kimi Code Worker" \
      org.opencontainers.image.description="Persistent Kimi Code CLI worker with WebSocket agent communication" \
      org.opencontainers.image.version="1.0.0"

# WITH_TOOLCHAINS=false (default): slim runtime — no Rust/Go/C++ toolchains.
# Pass --build-arg WITH_TOOLCHAINS=true for the fat image (CI, dev tenants).
ARG WITH_TOOLCHAINS=false

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
        build-essential \
        gcc \
        g++ \
        libc-dev \
        cmake \
        ninja-build \
        pkg-config \
        libssl-dev \
        zlib1g-dev \
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
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN if [ "${WITH_TOOLCHAINS}" = "true" ]; then \
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain 1.96.1 \
      && chmod -R a+r /usr/local/rustup /usr/local/cargo; \
    fi

# ── Go toolchain (only with WITH_TOOLCHAINS=true) ──────────────────────────────
RUN if [ "${WITH_TOOLCHAINS}" = "true" ]; then \
      ARCH=$(dpkg --print-architecture) \
      && curl -fsSL "https://go.dev/dl/go1.24.3.linux-${ARCH}.tar.gz" \
        | tar -C /usr/local -xz; \
    fi
ENV GOROOT=/usr/local/go \
    GOPATH=/home/kimi/go \
    PATH=/usr/local/go/bin:/home/kimi/go/bin:$PATH

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
ARG KIMI_CODE_VERSION=0.30.0
RUN npm install -g @moonshot-ai/kimi-code@${KIMI_CODE_VERSION} && \
    kimi --version && \
    npm cache clean --force

# ── XDG directories for kimi state ────────────────────────────────────────────
RUN mkdir -p /home/kimi/.kimi-code \
             /workspace \
             /var/log/lunarwing \
             /app/results \
    && chown -R kimi:kimi /home/kimi /workspace /var/log/lunarwing /app/results

# ── ports ──────────────────────
EXPOSE 8443
EXPOSE 9090

# ── health check ───────────────────────
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD curl -sf http://127.0.0.1:8443/health || exit 1

USER 10002:10002

ENTRYPOINT ["/usr/bin/tini", "--", "/app/entrypoint.sh"]
CMD ["--mode", "websocket"]
