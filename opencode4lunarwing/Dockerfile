# v1.18.25 resolves to this reviewed commit. Both must move together.
# PINNED BACK from v1.18.29: that release ships two regressions hit live on
# brightdawn 2026-09-07 — (1) provider/model resolution crashes on config
# model entries lacking the full metadata shape ("a.name" TypeError), and
# (2) the ai-sdk stream path dies silently between "llm runtime selected"
# and the first chunk. v1.18.25 is the last version proven end-to-end in
# this worker. Re-bump only after both are fixed upstream.
ARG OPENCODE_VERSION=v1.18.25
ARG OPENCODE_REF=cb7d8b2f5e44876ef98b661dc10590c915af3a9f
ARG DEBIAN_SNAPSHOT=20261017T082437Z
ARG SOURCE_REVISION=unknown

# ──────────────────────
# Stage 1: build opencode from source
# ──────────────────────
# The bun digest below must stay equal to CI_BUN_IMAGE in
# .gitlab/ci/defaults.yml (enforced by validate_config.py).
FROM docker.io/oven/bun:debian@sha256:9114c058aeae42162ee16dd5084b95fe9473970bb6bcb5b232ab1630f0546895 AS builder

ARG OPENCODE_VERSION
ARG OPENCODE_REF
ARG DEBIAN_SNAPSHOT
WORKDIR /build

RUN set -eu; \
    rm -f /etc/apt/sources.list.d/debian.sources; \
    printf '%s\n' \
      "deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ trixie main" \
      "deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ trixie-updates main" \
      "deb http://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT}/ trixie-security main" \
      > /etc/apt/sources.list; \
    apt-get -o Acquire::Check-Valid-Until=false update; \
    apt-get install -y --no-install-recommends \
      git python3 make g++ ca-certificates; \
    rm -rf /var/lib/apt/lists/* \
      /var/log/apt/* /var/log/dpkg.log* /var/log/alternatives.log* /var/cache/ldconfig/aux-cache

RUN printf '%s' "${OPENCODE_REF}" | grep -Eq '^[0-9a-f]{40}$' && \
    git init /build/opencode && \
    git -C /build/opencode remote add origin https://github.com/anomalyco/opencode.git && \
    git -C /build/opencode fetch --depth 1 origin "refs/tags/${OPENCODE_VERSION}" && \
    test "$(git -C /build/opencode rev-parse FETCH_HEAD)" = "${OPENCODE_REF}" && \
    git -C /build/opencode checkout --detach "${OPENCODE_REF}" && \
    git -C /build/opencode tag --force "${OPENCODE_VERSION}" "${OPENCODE_REF}"

WORKDIR /build/opencode
RUN bun install --frozen-lockfile && bun pm cache rm

WORKDIR /build/opencode/packages/sdk/js
# The bridge imports @opencode-ai/sdk/v2/client from THIS build's output. Do not hide
# its errors (no `2>/dev/null`); a build-time import assertion in the runtime
# stage below turns a broken build into a failed image instead of a worker that
# only dies on first boot.
RUN bun run build || echo "WARNING: @opencode-ai/sdk build reported errors (import-verified in runtime stage)"

WORKDIR /build/opencode/packages/opencode
# Build only the native platform target. opencode's build.ts (script/build.ts)
# otherwise cross-compiles 12 targets (linux/darwin/win32 × arm64/x64 ×
# musl/baseline variants), each a separate Rust compile pulling fresh toolchains.
# `--single` filters allTargets to the current host's os/arch (and skips the
# baseline + musl variants), which is the only binary the worker ever loads —
# the image is distributed same-host via podman save|load, never cross-arch.
# See docs/proposals/OPENCODE-WORKER-SINGLE-TARGET-BUILD.md for full rationale.
RUN bun run build -- --single

# Keep cross-stage artifacts small. Copying the full monorepo retains thousands
# of source and node_modules files in the runtime stage. The compiled binary is
# standalone; the bridge only needs the generated SDK client source.
RUN set -e; \
    set -- dist/opencode-*/bin/opencode; \
    [ "$#" -eq 1 ] && [ -x "$1" ]; \
    install -D -m 0755 "$1" /out/bin/opencode; \
    install -d /out/sdk; \
    cp /build/opencode/packages/sdk/js/package.json /out/sdk/package.json; \
    cp -a /build/opencode/packages/sdk/js/src /out/sdk/src

# ──────────────────────
# Stage 2: runtime image
# ──────────────────────
FROM docker.io/oven/bun:debian@sha256:9114c058aeae42162ee16dd5084b95fe9473970bb6bcb5b232ab1630f0546895 AS runtime

ARG DEBIAN_SNAPSHOT
ARG SOURCE_REVISION
# WITH_TOOLCHAINS=false (default): slim runtime — no Rust/Go/C++ toolchains.
# The opencode binary is already built in the builder stage; these are only
# needed if coding tasks inside the container must compile native code.
# Pass --build-arg WITH_TOOLCHAINS=true for the fat image (CI, dev tenants).
# Also gates the heavy -dev packages and build tools (cmake, clang, llvm, etc.)
# which are only needed to compile native dependencies at runtime.
ARG WITH_TOOLCHAINS=false

# Expose both the human release and immutable source revision at runtime.
ARG OPENCODE_VERSION
ARG OPENCODE_REF
ENV OPENCODE_IMAGE_REF="${OPENCODE_VERSION}@${OPENCODE_REF}"

LABEL org.opencontainers.image.title="LunarWing OpenCode Worker" \
      org.opencontainers.image.description="Persistent opencode worker with WebSocket agent communication" \
      org.opencontainers.image.version="1.0.0" \
      org.opencontainers.image.revision="${SOURCE_REVISION}" \
      org.opencontainers.image.toolchain-profile="${WITH_TOOLCHAINS}"

RUN set -eu; \
    rm -f /etc/apt/sources.list.d/debian.sources; \
    printf '%s\n' \
      "deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ trixie main" \
      "deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ trixie-updates main" \
      "deb http://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT}/ trixie-security main" \
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
    GOPATH=/home/opencode/go \
    CARGO_HOME=/home/opencode/.cargo \
    PATH=/home/opencode/.cargo/bin:/usr/local/cargo/bin:/usr/local/go/bin:/home/opencode/go/bin:$PATH

# ── non-root user ──────────────────────
# Keep IDs synchronized with ic/scripts/lunarwing-mt-admin.sh. Rootless Podman
# maps the tenant's host identity onto these IDs so private read-only mounts
# remain accessible without making Study Notes state world-readable.
RUN groupadd --gid 10001 opencode \
    && useradd --uid 10001 --gid 10001 -m -d /home/opencode opencode

# ── app layout ─────────────────
WORKDIR /app

COPY --from=builder /out/bin/opencode /usr/local/bin/opencode
COPY --from=builder /out/sdk /app/node_modules/@opencode-ai/sdk

COPY --chown=opencode:opencode entrypoint.sh health_server.py agent_comm_protocol.json ./
COPY --chown=opencode:opencode scripts/ ./scripts/
COPY --chown=opencode:opencode config/ ./config/

RUN chmod +x /app/entrypoint.sh /app/health_server.py

# Fail the BUILD (not container startup) if the bridge's SDK import can't
# resolve — the SDK build above is best-effort, so verify the exact
# import the worker depends on. Without this a broken/renamed SDK passes
# `docker build` and only surfaces as a module-resolution error when the bridge
# starts inside the running container.
RUN cd /app && bun -e 'import("@opencode-ai/sdk/v2/client").then(m => { if (typeof m.createOpencodeClient !== "function") { console.error("FATAL: @opencode-ai/sdk/v2/client resolved but is missing createOpencodeClient"); process.exit(1) } }).catch(e => { console.error("FATAL: cannot import @opencode-ai/sdk/v2/client —", e.message); process.exit(1) })'
RUN test "$(/usr/local/bin/opencode --version)" = "${OPENCODE_VERSION}" \
    && cd /app \
    && bun scripts/path_expand_test.ts \
    && bun scripts/opencode_result_test.ts \
    && bun scripts/opencode_task_executor_test.ts \
    && bun scripts/lunarwing_bridge_test.ts \
    && python3 scripts/study_notes_config_test.py

# ── XDG directories for opencode state ────────────────────────
RUN mkdir -p /home/opencode/.local/share/opencode \
             /home/opencode/.config/opencode \
             /workspace \
             /var/log/lunarwing \
             /app/results \
    && chown -R opencode:opencode /home/opencode /workspace /var/log/lunarwing /app/results

# ── ports ──────────────────────
EXPOSE 8443
EXPOSE 9090

# ── health check ───────────────────────
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD curl -sf http://127.0.0.1:8443/health || exit 1

USER 10001:10001

ENTRYPOINT ["/usr/bin/tini", "--", "/app/entrypoint.sh"]
CMD ["--mode", "websocket"]
