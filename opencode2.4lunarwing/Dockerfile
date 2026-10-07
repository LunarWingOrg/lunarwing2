# v2.0.24 resolves to this reviewed commit. Both must move together.
# OpenCode 2 is beta in practice (upstream's own migration doc: "server and
# plugin APIs may continue to change"). This lane is deliberately SEPARATE
# from the proven v1 worker (opencode4lunarwing, pinned v1.18.25) so tenants
# opt into v2 explicitly — see docs/plans/OPENCODE2_EXTERNAL_WORKER_PLAN.md.
# Re-bump only after re-verifying the bridge against the v2 server API.
ARG OPENCODE_VERSION=v2.0.24
ARG OPENCODE_REF=e7a34f09bfd9134dfade5a8ddb843f7030bc9a69
ARG DEBIAN_SNAPSHOT=20260906T000000Z
ARG SOURCE_REVISION=unknown

# ──────────────────────
# Stage 1: build opencode from source
# ──────────────────────
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

WORKDIR /build/opencode/packages/client
# The bridge imports @opencode/client (promise API) from THIS build's output.
# Do not hide its errors; the runtime-stage import assertion below turns a
# broken build into a failed image instead of a worker that only dies on boot.
RUN bun run build || echo "WARNING: @opencode/client build reported errors (import-verified in runtime stage)"

WORKDIR /build/opencode/packages/cli
COPY scripts/prepare_reproducible_build.py /build/prepare_reproducible_build.py
# Single-bundle source compilation avoids per-build chunk IDs/bytecode in the
# executable. Fail closed if the pinned upstream build options have drifted.
RUN python3 /build/prepare_reproducible_build.py script/build.ts
# Build only the native platform target (mirrors the v1 worker's --single
# rationale; the image is distributed same-host via podman save|load).
# Inject the pinned release version (sans leading v) into the build env:
# upstream's build script takes OPENCODE_VERSION/CHANNEL from the environment,
# and without it derives a preview version from `git branch --show-current` —
# EMPTY on this detached-HEAD checkout — producing a bogus preview version
# (0.0.0--<timestamp>) baked into the binary, which then fails the
# runtime-stage version check below.
RUN OPENCODE_VERSION="${OPENCODE_VERSION#v}" bun run build -- --single

# Keep cross-stage artifacts small. The compiled binary is standalone; the
# bridge only needs the client package source (its exports map points at
# src/*.ts, so the whole src tree travels).
RUN set -e; \
    set -- dist/*/bin/opencode; \
    [ "$#" -eq 1 ] && [ -x "$1" ]; \
    install -D -m 0755 "$1" /out/bin/opencode; \
    install -d /out/client; \
    cp /build/opencode/packages/client/package.json /out/client/package.json; \
    cp -a /build/opencode/packages/client/src /out/client/src; \
    cp -a /build/opencode/packages/schema/src /out/schema-src

# ──────────────────────
# Stage 2: runtime image
# ──────────────────────
FROM docker.io/oven/bun:debian@sha256:9114c058aeae42162ee16dd5084b95fe9473970bb6bcb5b232ab1630f0546895 AS runtime

ARG DEBIAN_SNAPSHOT
ARG SOURCE_REVISION
# WITH_TOOLCHAINS=false (default): slim runtime — no Rust/Go/C++ toolchains.
# Pass --build-arg WITH_TOOLCHAINS=true for the fat image (CI, dev tenants).
ARG WITH_TOOLCHAINS=false

# Expose both the human release and immutable source revision at runtime.
ARG OPENCODE_VERSION
ARG OPENCODE_REF
ENV OPENCODE_IMAGE_REF="${OPENCODE_VERSION}@${OPENCODE_REF}"

LABEL org.opencontainers.image.title="LunarWing OpenCode 2 Worker" \
      org.opencontainers.image.description="Persistent OpenCode 2 worker with WebSocket agent communication" \
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
    GOPATH=/home/opencode2/go \
    CARGO_HOME=/home/opencode2/.cargo \
    PATH=/home/opencode2/.cargo/bin:/usr/local/cargo/bin:/usr/local/go/bin:/home/opencode2/go/bin:$PATH

# ── non-root user ──────────────────────
# Keep IDs synchronized with ic/scripts/lunarwing-mt-admin.sh (opencode2 =
# 10003, distinct from opencode 10001 and kimi 10002) so two opencode lanes on
# one tenant keep separate userns identities.
RUN groupadd --gid 10003 opencode2 \
    && useradd --uid 10003 --gid 10003 -m -d /home/opencode2 opencode2

# ── app layout ─────────────────────────
WORKDIR /app

COPY --from=builder /out/bin/opencode /usr/local/bin/opencode
COPY --from=builder /out/client /app/node_modules/@opencode/client
# @opencode/client's generated types import from @opencode/schema and
# @opencode/protocol; both resolve as source via their exports maps.
COPY --from=builder /out/schema-src /app/node_modules/@opencode/schema/src
COPY --from=builder /build/opencode/packages/schema/package.json /app/node_modules/@opencode/schema/package.json
COPY --from=builder /build/opencode/packages/protocol/src /app/node_modules/@opencode/protocol/src
COPY --from=builder /build/opencode/packages/protocol/package.json /app/node_modules/@opencode/protocol/package.json

COPY --chown=opencode2:opencode2 entrypoint.sh health_server.py agent_comm_protocol.json ./
COPY --chown=opencode2:opencode2 scripts/ ./scripts/
COPY --chown=opencode2:opencode2 config/ ./config/

RUN chmod +x /app/entrypoint.sh /app/health_server.py

# Fail the BUILD (not container startup) if the bridge's SDK import can't
# resolve — the client build above is best-effort, so verify the exact import
# the worker depends on. Beyond OpenCode.make, construct a client (pure
# construction — no server contact) and assert every method path the bridge
# calls, so upstream SDK drift like WI#320 F7 (session.message.list /
# session.permission.reply do not exist; both are top-level groups) fails the
# image build instead of dying at runtime (WI#303).
#
# Build-time self-tests run under a throwaway HOME rooted in the trap-cleaned
# TMPDIR: OpenCode 2 appends a timestamped, randomly-IDed line to
# XDG_DATA_HOME/opencode/log/opencode.log on EVERY start (including
# `opencode --version`), and Bun keeps caches under $HOME. With the default
# root HOME both land inside the image layer, so the candidate-vs-rebuild
# whole-image digest gate failed on every pipeline (work item #177).
# Redirecting HOME and the XDG dirs keeps every byte of that per-run state
# out of the image.
RUN set -eu; \
    export TMPDIR=/tmp/opencode2-build-tests; \
    export HOME="$TMPDIR/home"; \
    export XDG_DATA_HOME="$HOME/.local/share" \
           XDG_CACHE_HOME="$HOME/.cache" \
           XDG_CONFIG_HOME="$HOME/.config" \
           XDG_STATE_HOME="$HOME/.local/state"; \
    mkdir -p "$HOME"; \
    trap 'rm -rf /tmp/opencode2-build-tests' EXIT; \
    cd /app && bun -e 'import("@opencode/client/promise").then(m => { if (typeof m.OpenCode?.make !== "function") { console.error("FATAL: @opencode/client/promise resolved but is missing OpenCode.make"); process.exit(1) } const client = m.OpenCode.make({ baseUrl: "http://127.0.0.1:4096" }); const required = [["session.create", client.session?.create], ["session.prompt", client.session?.prompt], ["session.interrupt", client.session?.interrupt], ["permission.reply", client.permission?.reply], ["event.subscribe", client.event?.subscribe], ["message.list", client.message?.list]]; for (const [name, fn] of required) { if (typeof fn !== "function") { console.error(`FATAL: pinned @opencode/client is missing client.${name}() — the bridge calls it (WI#303)`); process.exit(1) } } }).catch(e => { console.error("FATAL: cannot import @opencode/client/promise —", e.message); process.exit(1) })'
# `opencode --version` prints "opencode v<semver>" (name + v + version baked
# in at build time above). Compare against exactly that, and LOG the actual
# value on mismatch — a bare `test` fails this step with zero output, which
# made this lane's repeated silent build failures painful to diagnose.
RUN set -eu; \
    export TMPDIR=/tmp/opencode2-build-tests PYTHONDONTWRITEBYTECODE=1; \
    export HOME="$TMPDIR/home"; \
    export XDG_DATA_HOME="$HOME/.local/share" \
           XDG_CACHE_HOME="$HOME/.cache" \
           XDG_CONFIG_HOME="$HOME/.config" \
           XDG_STATE_HOME="$HOME/.local/state"; \
    mkdir -p "$HOME"; \
    trap 'rm -rf /tmp/opencode2-build-tests' EXIT; \
    actual="$(/usr/local/bin/opencode --version)" \
    && expected="opencode v${OPENCODE_VERSION#v}" \
    && { [ "$actual" = "$expected" ] \
         || { echo "FATAL: opencode --version reported '$actual', expected '$expected'" >&2; exit 1; }; } \
    && cd /app \
    && bun scripts/path_expand_test.ts \
    && bun scripts/opencode_result_test.ts \
    && bun scripts/opencode_task_executor_test.ts \
    && python3 scripts/prepare_reproducible_build_test.py \
    && python3 scripts/study_notes_config_test.py \
    && for leaked in /root/.bun /root/.local /root/.cache /root/.config; do \
           [ ! -e "$leaked" ] \
           || { echo "FATAL: build-time self-tests leaked per-build state at $leaked — refusing to bake a non-reproducible layer" >&2; exit 1; }; \
       done

# ── XDG directories for opencode state ────────────────────────
RUN mkdir -p /home/opencode2/.local/share/opencode \
             /home/opencode2/.config/opencode \
             /workspace \
             /var/log/lunarwing \
             /app/results \
    && chown -R opencode2:opencode2 /home/opencode2 /workspace /var/log/lunarwing /app/results

# ── ports ──────────────────────
EXPOSE 8443
EXPOSE 9090

# ── health check ───────────────────────
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD curl -sf http://127.0.0.1:8443/health || exit 1

USER 10003:10003

ENTRYPOINT ["/usr/bin/tini", "--", "/app/entrypoint.sh"]
CMD ["--mode", "websocket"]
