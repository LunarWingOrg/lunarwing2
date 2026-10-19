# Keep release inputs immutable: the CLI version is cross-checked against the
# committed npm lock, and package indexes come from one Debian snapshot.
ARG KIMI_CODE_VERSION=2.2.0
ARG DEBIAN_SNAPSHOT=20261017T082437Z
ARG SOURCE_REVISION=unknown

# ──────────────────────────────
# Stage 1: build the bridge + tests
# ──────────────────────────────
FROM docker.io/library/node:22-bookworm-slim@sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5 AS builder

ARG DEBIAN_SNAPSHOT
WORKDIR /build

RUN set -eu; \
    rm -f /etc/apt/sources.list.d/debian.sources; \
    printf '%s\n' \
      "deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ bookworm main" \
      "deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ bookworm-updates main" \
      "deb http://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT}/ bookworm-security main" \
      > /etc/apt/sources.list; \
    apt-get -o Acquire::Check-Valid-Until=false update; \
    apt-get install -y --no-install-recommends \
      python3 make g++ ca-certificates; \
    rm -rf /var/lib/apt/lists/* \
      /var/log/apt/* /var/log/dpkg.log* /var/log/alternatives.log* /var/cache/ldconfig/aux-cache

COPY scripts/package.json scripts/package-lock.json scripts/tsconfig.json ./scripts/
COPY scripts/*.ts ./scripts/
# entrypoint_config_test.js reads ../entrypoint.sh — ship it in the build
# stage so the in-build test gate validates the actual shipped entrypoint.
COPY entrypoint.sh /build/entrypoint.sh
# WI#206 fixtures: the headless-guard tests read /build/hooks/ and
# /build/config/ during the in-build npm test gate (same builder-stage
# pattern as entrypoint.sh above). Without these COPYs the 7 new
# headless_command_guard / entrypoint policy tests fail with ENOENT and
# the image build gate fails (inttest 2026-09-22 15:16, barracuda).
COPY hooks/ /build/hooks/
COPY config/ /build/config/

WORKDIR /build/scripts
RUN npm ci --ignore-scripts && \
    npx tsc -p tsconfig.json && \
    npm test && \
    rm -rf node_modules/.cache

# ── CLI bitness + flag contract gate ─────────────────────────────────────────
# WI#195 finding 2: every worker test points KIMI_BIN at a mock, so the real
# CLI's flag surface was pinned only in a comment. Compile the CLI (locked
# version — kimi-cli/package-lock.json is the reproducible pin) for the
# image's node, then re-run just the contract tests with the real binary on
# PATH. The lane self-skips on host builds (no binary), so this is the only
# place the contract is asserted for real.
ARG KIMI_CODE_VERSION
COPY kimi-cli/package.json kimi-cli/package-lock.json /opt/kimi-cli/
RUN npm ci --omit=dev --prefix /opt/kimi-cli && \
    PATH="/opt/kimi-cli/node_modules/.bin:$PATH" npm run --prefix /build/scripts test -- \
      --test-name-pattern "real CLI"

# ──────────────────────────────
# Stage 2: runtime image
# ──────────────────────────────
FROM docker.io/library/node:22-bookworm-slim@sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5 AS runtime

ARG DEBIAN_SNAPSHOT
ARG SOURCE_REVISION
# WITH_TOOLCHAINS=false (default): slim runtime — no Rust/Go/C++ toolchains.
# Pass --build-arg WITH_TOOLCHAINS=true for the fat image (CI, dev tenants).
ARG WITH_TOOLCHAINS=false

LABEL org.opencontainers.image.title="LunarWing Kimi Code Worker" \
      org.opencontainers.image.description="Persistent Kimi Code CLI worker with WebSocket agent communication" \
      org.opencontainers.image.version="1.0.0" \
      org.opencontainers.image.revision="${SOURCE_REVISION}" \
      org.opencontainers.image.toolchain-profile="${WITH_TOOLCHAINS}"

RUN set -eu; \
    rm -f /etc/apt/sources.list.d/debian.sources; \
    printf '%s\n' \
      "deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ bookworm main" \
      "deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ bookworm-updates main" \
      "deb http://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT}/ bookworm-security main" \
      > /etc/apt/sources.list; \
    apt-get -o Acquire::Check-Valid-Until=false update; \
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
      neovim; \
    rm -rf /var/lib/apt/lists/* \
      /var/log/apt/* /var/log/dpkg.log* /var/log/alternatives.log* /var/cache/ldconfig/aux-cache

# Toolchain packages: only when WITH_TOOLCHAINS=true.
RUN if [ "${WITH_TOOLCHAINS}" = "true" ]; then \
      set -eu; \
      apt-get -o Acquire::Check-Valid-Until=false update; \
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
        npm; \
      rm -rf /var/lib/apt/lists/* \
        /var/log/apt/* /var/log/dpkg.log* /var/log/alternatives.log* /var/cache/ldconfig/aux-cache; \
    fi

# ── Rust toolchain (only with WITH_TOOLCHAINS=true) ───────────────────────────
ENV RUSTUP_HOME=/usr/local/rustup

RUN if [ "${WITH_TOOLCHAINS}" = "true" ]; then \
      set -eu; \
      case "$(dpkg --print-architecture)" in \
        amd64) RUST_TARGET=x86_64-unknown-linux-gnu; RUSTUP_SHA=20a06e644b0d9bd2fbdbfd52d42540bdde820ea7df86e92e533c073da0cdd43c ;; \
        arm64) RUST_TARGET=aarch64-unknown-linux-gnu; RUSTUP_SHA=e3853c5a252fca15252d07cb23a1bdd9377a8c6f3efa01531109281ae47f841c ;; \
        *) echo "unsupported Rust architecture" >&2; exit 1 ;; \
      esac; \
      curl -fsSLo /tmp/rustup-init \
        "https://static.rust-lang.org/rustup/archive/1.28.2/${RUST_TARGET}/rustup-init"; \
      echo "${RUSTUP_SHA}  /tmp/rustup-init" | sha256sum -c -; \
      chmod +x /tmp/rustup-init; \
      CARGO_HOME=/usr/local/cargo /tmp/rustup-init -y --no-modify-path \
        --profile minimal --default-toolchain 1.96.1 \
      ; rm /tmp/rustup-init; \
      chmod -R a+rX /usr/local/rustup /usr/local/cargo; \
    fi

# ── Go toolchain (only with WITH_TOOLCHAINS=true) ──────────────────────────────
RUN if [ "${WITH_TOOLCHAINS}" = "true" ]; then \
      set -eu; \
      case "$(dpkg --print-architecture)" in \
        amd64) GO_SHA=3333f6ea53afa971e9078895eaa4ac7204a8c6b5c68c10e6bc9a33e8e391bdd8 ;; \
        arm64) GO_SHA=a463cb59382bd7ae7d8f4c68846e73c4d589f223c589ac76871b66811ded7836 ;; \
        *) echo "unsupported Go architecture" >&2; exit 1 ;; \
      esac; \
      ARCH=$(dpkg --print-architecture); \
      curl -fsSLo /tmp/go.tgz "https://go.dev/dl/go1.24.3.linux-${ARCH}.tar.gz"; \
      echo "${GO_SHA}  /tmp/go.tgz" | sha256sum -c -; \
      tar -C /usr/local -xzf /tmp/go.tgz; \
      rm /tmp/go.tgz; \
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
COPY --chown=kimi:kimi hooks/ ./hooks/

RUN chmod +x /app/entrypoint.sh /app/health_server.py

# ── Kimi Code CLI ─────────────────────────────────────────────────────────────
# npm ci verifies every bridge and CLI dependency against committed integrity
# hashes. An override that disagrees with the lock fails instead of re-resolving.
ARG KIMI_CODE_VERSION
ARG SOURCE_DATE_EPOCH=0
ENV KIMI_CODE_IMAGE_VERSION="${KIMI_CODE_VERSION}"
COPY kimi-cli/package.json kimi-cli/package-lock.json /opt/kimi-cli/
# The stock npm bundled in the node base ships node-tar < 7.5.19
# (CVE-2026-59873); upgrade npm to a pinned release that bundles the fix.
# The trailing touch pins the /usr/local directory mtime: this step does not
# always modify the directory itself, so depending on the run the layer tar
# records either the base image mtime or SOURCE_DATE_EPOCH (clamped by
# rewrite-timestamp) — flaking the candidate vs clean-rebuild digest gate
# (worker-kimi-candidate in pipelines 5158, 5263, 5390, 5481, 5734).
RUN test "$(node -p "require('/opt/kimi-cli/package.json').dependencies['@moonshot-ai/kimi-code']")" = "${KIMI_CODE_VERSION}" && \
    npm install -g npm@12.0.2 && \
    npm ci --omit=dev --prefix /opt/kimi-cli && \
    ln -s /opt/kimi-cli/node_modules/.bin/kimi /usr/local/bin/kimi && \
    kimi --version && \
    npm cache clean --force && \
    rm -rf /root/.npm/_logs /tmp/node-compile-cache && \
    touch -d "@${SOURCE_DATE_EPOCH}" /usr/local

# ── XDG directories for kimi state ────────────────────────────────────────────
RUN mkdir -p /home/kimi/.kimi-code \
             /home/kimi/.cargo \
             /home/kimi/go \
             /workspace \
             /var/log/lunarwing \
             /app/results \
    && chown -R kimi:kimi /home/kimi /workspace /var/log/lunarwing /app/results

# WI#206: the baked policy (guard config + PreToolUse hook) must not be
# editable by the runtime user — the worker runs as kimi, so dropping write
# access means a task inside the container cannot strip its own guard.
RUN chmod -R a-w /app/config /app/hooks && chmod a-w /app/entrypoint.sh

# ── ports ──────────────────────
EXPOSE 8443
EXPOSE 9090

# ── health check ───────────────────────
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD curl -sf http://127.0.0.1:8443/ready || exit 1

USER 10002:10002

ENTRYPOINT ["/usr/bin/tini", "--", "/app/entrypoint.sh"]
CMD ["--mode", "websocket"]
