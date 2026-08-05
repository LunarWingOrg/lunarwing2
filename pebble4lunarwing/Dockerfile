FROM docker.io/library/rust:1.92-bookworm AS builder

# Snapshot fallback helper — tries live mirror first, falls back to
# snapshot.debian.org if live mirror has an invalid signature (e.g. pre-staged
# security signatures not yet valid for the current UTC window).
RUN set -e; \
    (apt-get update 2>/dev/null || { \
      echo "apt-get update failed — switching to snapshot.debian.org fallback mirror" >&2; \
      sed -i 's|debian\.org|snapshot.debian.org/archive|g' /etc/apt/sources.list; \
      apt-get update 2>/dev/null || apt-get update -o Acquire::AllowInsecureRepositories=true; \
    }) && \
    apt-get install -y --no-install-recommends git && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Build the bridge binary
COPY pebble4lunarwing/Cargo.toml pebble4lunarwing/Cargo.lock ./bridge/
COPY pebble4lunarwing/src ./bridge/src
RUN cd bridge && cargo build --release

# Build pebble
RUN git clone --depth 1 https://github.com/nanogpt-community/pebble.git ./pebble
RUN cd pebble && cargo build --release --bin pebble

# --- Runtime ---
FROM docker.io/library/debian:bookworm-slim

# WITH_TOOLCHAINS=false (default): slim runtime — no Rust/Go/C++ toolchains.
# Pass --build-arg WITH_TOOLCHAINS=true for the fat image (dev containers that
# must compile native code), matching the opencode/kimi toolchain profile.
ARG WITH_TOOLCHAINS=false

LABEL org.opencontainers.image.toolchain-profile="${WITH_TOOLCHAINS}"

RUN set -e; \
    (apt-get update 2>/dev/null || { \
      echo "apt-get update failed — switching to snapshot.debian.org fallback mirror" >&2; \
      sed -i 's|debian\.org|snapshot.debian.org/archive|g' /etc/apt/sources.list; \
      apt-get update 2>/dev/null || apt-get update -o Acquire::AllowInsecureRepositories=true; \
    }) && \
    apt-get install -y --no-install-recommends \
      ca-certificates git curl && \
    rm -rf /var/lib/apt/lists/*

# Toolchain packages: only when WITH_TOOLCHAINS=true. Mirrors the opencode
# worker's list so all external workers expose the same dev-tool surface.
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
        protobuf-compiler \
        libprotobuf-dev \
        musl-tools \
        libcurl4-openssl-dev \
        libxml2-dev \
        autoconf \
        automake \
        libtool \
        meson \
        nodejs \
        npm \
      && rm -rf /var/lib/apt/lists/*; \
    fi

# ── Rust toolchain (only with WITH_TOOLCHAINS=true) ───────────────────
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo

RUN if [ "${WITH_TOOLCHAINS}" = "true" ]; then \
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain 1.96.1 \
      && chmod -R a+rX /usr/local/rustup /usr/local/cargo; \
    fi

# ── Go toolchain (only with WITH_TOOLCHAINS=true) ──────────────────────
RUN if [ "${WITH_TOOLCHAINS}" = "true" ]; then \
      ARCH=$(dpkg --print-architecture) \
      && curl -fsSL "https://go.dev/dl/go1.24.3.linux-${ARCH}.tar.gz" \
        | tar -C /usr/local -xz; \
    fi
ENV GOROOT=/usr/local/go \
    GOPATH=/root/go \
    PATH=/usr/local/cargo/bin:/usr/local/go/bin:/root/go/bin:$PATH

COPY --from=builder /build/bridge/target/release/pebble4lunarwing /usr/local/bin/pebble4lunarwing
COPY --from=builder /build/pebble/target/release/pebble /usr/local/bin/pebble
COPY pebble4lunarwing/entrypoint.sh /usr/local/bin/entrypoint.sh

RUN mkdir -p /workspace && chmod 777 /workspace

ENV PEBBLE_BIN=/usr/local/bin/pebble \
    PEBBLE_MODE=websocket \
    WS_PORT=9090 \
    HEALTH_PORT=8443 \
    WORKSPACE_ROOT=/workspace

EXPOSE 9090 8443

HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD curl -sf http://127.0.0.1:8443/health || exit 1

ENTRYPOINT ["entrypoint.sh"]
