ARG PEBBLE_REF=2d0f3786cbebb705cbd4f6212b0aaf47d8d8590b
ARG DEBIAN_SNAPSHOT=20261017T082437Z
ARG SOURCE_REVISION=unknown

FROM docker.io/library/rust:1.96.1-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663 AS builder

ARG PEBBLE_REF
ARG DEBIAN_SNAPSHOT
RUN set -eu; \
    rm -f /etc/apt/sources.list.d/debian.sources; \
    printf '%s\n' \
      "deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ bookworm main" \
      "deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ bookworm-updates main" \
      "deb http://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT}/ bookworm-security main" \
      > /etc/apt/sources.list; \
    apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=5 -o Acquire::http::Timeout=60 update; \
    apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=60 install -y --no-install-recommends git python3; \
    rm -rf /var/lib/apt/lists/* \
      /var/log/apt/* /var/log/dpkg.log* /var/log/alternatives.log* /var/cache/ldconfig/aux-cache

WORKDIR /build

COPY pebble4lunarwing/Cargo.toml pebble4lunarwing/Cargo.lock ./bridge/
COPY pebble4lunarwing/src ./bridge/src
# Test before the release build: the workspace-path confinement and cron
# bootstrap suites (including the FIFO/O_DIRECTORY and transient-classification
# pins) must gate the image, not just host-side runs (MR !1531 DR-3).
RUN cd bridge && cargo test --locked
RUN cd bridge && cargo build --release --locked

RUN printf '%s' "${PEBBLE_REF}" | grep -Eq '^[0-9a-f]{40}$' && \
    git init /build/pebble && \
    git -C /build/pebble remote add origin https://github.com/nanogpt-community/pebble.git && \
    git -C /build/pebble fetch --depth 1 origin "${PEBBLE_REF}" && \
    git -C /build/pebble checkout --detach FETCH_HEAD && \
    test "$(git -C /build/pebble rev-parse HEAD)" = "${PEBBLE_REF}"
# The pinned upstream checkout ships rustls 0.23.41, which is affected by
# RUSTSEC-2026-0285 (fixed >= 0.23.45). WI#409 raises the override to 0.23.46
# for RUSTSEC-2026-0346/0347/0348/0349. Upstream also pins chacha20 0.10.1,
# which is yanked on crates.io. The reviewed patched lock is supplied as a
# hashed build input instead of an unpinned
# build-time `cargo update`: it is upstream at PEBBLE_REF with only
# rustls 0.23.41 -> 0.23.46, rustls-webpki 0.103.13 -> 0.103.15, and
# chacha20 0.10.1 -> 0.10.2 changed.
# The build fails if the checked-out lock or the pinned digest drift from the
# reviewed inputs, keeping the locked/reproducible-build guarantee.
COPY pebble4lunarwing/upstream/Cargo.lock /tmp/pebble-upstream-Cargo.lock
COPY pebble4lunarwing/scripts/verify_upstream_lock.py /tmp/verify-pebble-lock.py
# Canonical reviewed pin also covers mt-admin, Compose and plain docker builds.
ARG PEBBLE_LOCK_SHA256=a87837f52b769730b33126250be12ed3b979988bfbffb490cf564f9c85f870ad
RUN python3 /tmp/verify-pebble-lock.py pebble/Cargo.lock \
    /tmp/pebble-upstream-Cargo.lock "${PEBBLE_LOCK_SHA256}"
RUN cp /tmp/pebble-upstream-Cargo.lock pebble/Cargo.lock && \
    cd pebble && cargo build --release --locked --bin pebble

# --- Runtime ---
FROM docker.io/library/debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

ARG DEBIAN_SNAPSHOT
ARG SOURCE_REVISION
# WITH_TOOLCHAINS=false (default): slim runtime — no Rust/Go/C++ toolchains.
# Pass --build-arg WITH_TOOLCHAINS=true for the fat image (dev containers that
# must compile native code), matching the opencode/kimi toolchain profile.
ARG WITH_TOOLCHAINS=false

LABEL org.opencontainers.image.revision="${SOURCE_REVISION}" \
      org.opencontainers.image.toolchain-profile="${WITH_TOOLCHAINS}"

RUN set -eu; \
    rm -f /etc/apt/sources.list.d/debian.sources; \
    printf '%s\n' \
      "deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ bookworm main" \
      "deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ bookworm-updates main" \
      "deb http://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT}/ bookworm-security main" \
      > /etc/apt/sources.list; \
    apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=5 -o Acquire::http::Timeout=60 update; \
    apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=60 install -y --no-install-recommends ca-certificates git curl; \
    rm -rf /var/lib/apt/lists/* \
      /var/log/apt/* /var/log/dpkg.log* /var/log/alternatives.log* /var/cache/ldconfig/aux-cache

# Toolchain packages: only when WITH_TOOLCHAINS=true. Mirrors the opencode
# worker's list so all external workers expose the same dev-tool surface.
RUN if [ "${WITH_TOOLCHAINS}" = "true" ]; then \
      set -eu; \
      apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=5 -o Acquire::http::Timeout=60 update; \
      apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=60 install -y --no-install-recommends \
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
        npm; \
      rm -rf /var/lib/apt/lists/* \
        /var/log/apt/* /var/log/dpkg.log* /var/log/alternatives.log* /var/cache/ldconfig/aux-cache; \
    fi

# ── Rust toolchain (only with WITH_TOOLCHAINS=true) ───────────────────
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo

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
      /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain 1.96.1; \
      rm /tmp/rustup-init; \
      chmod -R a+rX /usr/local/rustup /usr/local/cargo; \
    fi

# ── Go toolchain (only with WITH_TOOLCHAINS=true) ──────────────────────
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
    GOPATH=/root/go \
    PATH=/usr/local/cargo/bin:/usr/local/go/bin:/root/go/bin:$PATH

# ── Task-shell toolchain anchors (WI#334) ────────────────────────────
# Upstream pebble's Bash tool spawns every task command as
# `/bin/sh -lc <cmd>` with the process environment inherited (no env_clear;
# pebble/crates/runtime/src/bash.rs at the pinned PEBBLE_REF). On this
# Debian base /bin/sh is dash: as a login shell it reads /etc/profile, which
# resets PATH to the system default — dropping the ENV PATH above (the
# WI#320 gauntlet F6 failure) — and then sources /etc/profile.d/*.sh.
# An entrypoint.sh export cannot survive that reset, so the toolchains are
# anchored where a reset shell still finds them:
#   * /etc/profile.d/lunarwing-toolchain.sh restores the toolchain env for
#     login shells, sourcing AFTER the /etc/profile PATH reset. It must stay
#     dash-clean: dash aborts the task command on a syntax error in a
#     sourced profile.d file. Existence-guarded, so inert in the slim image.
#   * Real-binary symlinks in /usr/local/bin (cargo, rustc, go, gofmt) cover
#     sanitised shells whose fallback PATH still includes /usr/local/bin.
#   * /usr/local/bin/rustup is a wrapper that re-exports RUSTUP_HOME/
#     CARGO_HOME before delegating, keeping rustup resolvable from any shell.
#     The snippet restores the image ENV PATH order (rustup proxies, then
#     GOROOT/bin, then GOPATH/bin); the fat image pre-creates /root/go/bin so
#     `go install`ed tools resolve from the very first task command.
COPY pebble4lunarwing/scripts/toolchain-profile.sh /etc/profile.d/lunarwing-toolchain.sh
COPY pebble4lunarwing/scripts/rustup-wrapper.sh /usr/local/share/lunarwing/rustup-wrapper.sh

# Build-time proof, two legs mirroring smoke-task-shell-toolchain.sh:
#   1. the real spawn mode — /bin/sh -lc with the image env (login PATH
#      reset + profile.d restore; the rustup proxies answer);
#   2. a fully sanitised shell — env -i /bin/sh -c, where only the
#      /usr/local/bin symlinks and the rustup wrapper can resolve the
#      toolchains. The command -v lines are the resolution evidence.
RUN if [ "${WITH_TOOLCHAINS}" = "true" ]; then \
      set -eu; \
      toolchain_bins="$(ls -d /usr/local/rustup/toolchains/*/bin)"; \
      [ "$(printf '%s\n' "${toolchain_bins}" | wc -l)" = "1" ] || { \
        echo "expected exactly one rustup toolchain dir" >&2; exit 1; }; \
      ln -s "${toolchain_bins}/cargo" /usr/local/bin/cargo; \
      ln -s "${toolchain_bins}/rustc" /usr/local/bin/rustc; \
      ln -s /usr/local/go/bin/go /usr/local/bin/go; \
      ln -s /usr/local/go/bin/gofmt /usr/local/bin/gofmt; \
      install -m 0755 /usr/local/share/lunarwing/rustup-wrapper.sh /usr/local/bin/rustup; \
      mkdir -p /root/go/bin; \
      /bin/sh -lc 'set -e; command -v cargo; command -v rustc; command -v go; command -v rustup; cargo --version; rustc --version; go version; rustup --version'; \
      env -i /bin/sh -c 'set -e; command -v cargo; command -v rustc; command -v go; command -v rustup; cargo --version; rustc --version; go version; rustup --version'; \
    fi

COPY --from=builder /build/bridge/target/release/pebble4lunarwing /usr/local/bin/pebble4lunarwing
COPY --from=builder /build/pebble/target/release/pebble /usr/local/bin/pebble
COPY pebble4lunarwing/entrypoint.sh /usr/local/bin/entrypoint.sh

RUN /usr/local/bin/pebble --version && \
    test -x /usr/local/bin/pebble4lunarwing && \
    test -x /usr/local/bin/entrypoint.sh
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
