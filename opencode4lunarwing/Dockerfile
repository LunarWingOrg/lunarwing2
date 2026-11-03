# v1.19.5 resolves to this reviewed commit. Both must move together.
# v1.18.29 was reverted on 2026-09-07 because it shipped two regressions: the
# provider/model metadata-shape crash ("a.name" TypeError) and a silent death
# in the ai-sdk stream path. WI#531 moves to v1.19.5 on the WI's authorization;
# neither v1.18.29 regression has been verified fixed upstream at v1.19.5 and
# no live dispatch has run, so both remain open risks on this pin. The lane is
# expected to be retired (goals item 34); re-bump only after re-verifying the
# bridge against the v1 server API, and run one live v1 dispatch on a test
# tenant after the first master candidate build.
# The pin also carries the 61845b0 whitespace repair (content-equivalent
# backport, not the commit itself), so the in-tree photon-node hunk repair is
# gone.
ARG OPENCODE_VERSION=v1.19.5
ARG OPENCODE_REF=810c0eba1a7348036f8bdf61e6632d0c36c6876c
ARG DEBIAN_SNAPSHOT=20261024T011300Z
# WI#467: sha256 of the committed models.dev snapshot (models-dev/api.json)
# the upstream build compiles into the binary instead of fetching live.
# Refresh the file and this pin together in one deliberate commit (see the
# opencode bump checklist in docs/ops/WORKER-CONTAINERS.md); CI's
# validate_worker_versions fails when the two drift apart. The name says
# SNAPSHOT on purpose: secret scanning (gitleaks generic-api-key) reads an
# ARG whose name contains API and whose value is 64 hex as a leaked key.
ARG MODELS_DEV_SNAPSHOT_SHA256=7552284ad975a895e287180ab1e2157328ff19bc7832516ab00ddbaceba51b9a
ARG SOURCE_REVISION=unknown

# ──────────────────────
# Stage 1: build opencode from source
# ──────────────────────
# The bun digest below must stay equal to CI_BUN_IMAGE in
# .gitlab/ci/defaults.yml (enforced by validate_config.py).
FROM docker.io/oven/bun:1.4.4-debian@sha256:93d5032fe06d163427822dc1598f420363759bb18155c9cc90eb1be05a64a19e AS builder

ARG OPENCODE_VERSION
ARG OPENCODE_REF
ARG DEBIAN_SNAPSHOT
ARG MODELS_DEV_SNAPSHOT_SHA256
WORKDIR /build

RUN set -eu; \
    rm -f /etc/apt/sources.list.d/debian.sources; \
    printf '%s\n' \
      "deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ trixie main" \
      "deb http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ trixie-updates main" \
      "deb http://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT}/ trixie-security main" \
      > /etc/apt/sources.list; \
    apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=5 -o Acquire::http::Timeout=60 update; \
    apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=60 install -y --no-install-recommends \
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
# The Bun 1.4.4 photon-node hunk-10 repair (upstream 61845b0) that this lane
# used to apply at the v1.18.25 pin is retired: v1.19.5 carries that whitespace
# repair as a content-equivalent backport, so the fixer is redundant.
RUN bun install --frozen-lockfile && bun pm cache rm

WORKDIR /build/opencode/packages/sdk/js
# The bridge imports @opencode-ai/sdk/v2/client from THIS build's output. Do not hide
# its errors (no `2>/dev/null`); a build-time import assertion in the runtime
# stage below turns a broken build into a failed image instead of a worker that
# only dies on first boot.
RUN bun run build || echo "WARNING: @opencode-ai/sdk build reported errors (import-verified in runtime stage)"

WORKDIR /build/opencode/packages/opencode
# WI#467: pin the models.dev snapshot the upstream build compiles into the
# binary. At the pinned ref, script/generate.ts reads the snapshot from the
# file named by MODELS_DEV_API_JSON when set, and fetches
# ${OPENCODE_MODELS_URL:-https://models.dev}/api.json live otherwise — the
# live response then became part of the binary, so candidate and
# clean-rebuild digests differed whenever models.dev published between the
# two builds (the clean-rebuild mismatches tracked as WI#467). COPY the
# committed snapshot in, verify its hash against the ARG pin, check that it is
# strict JSON and that generate.ts still reads both env vars, then build with
# OPENCODE_MODELS_URL pointed at a closed loopback port: an upstream that
# ignores the snapshot and fetches anyway fails the build instead of
# compiling a live catalog. validate_worker_versions pins this builder-stage
# tail (this WORKDIR up to the next FROM) instruction by instruction, so
# dropping an env assignment or the guard call, or adding a later rebuild,
# fails CI (validate_config.OPENCODE_MODELS_DEV_BUILD_STEPS).
COPY models-dev/api.json /build/models-dev-api.json
COPY scripts/check_models_dev_snapshot.py /build/check_models_dev_snapshot.py
# Build only the native platform target. opencode's build.ts (script/build.ts)
# otherwise cross-compiles 12 targets (linux/darwin/win32 × arm64/x64 ×
# musl/baseline variants), each a separate Rust compile pulling fresh toolchains.
# `--single` filters allTargets to the current host's os/arch (and skips the
# baseline + musl variants), which is the only binary the worker ever loads —
# the image is distributed same-host via podman save|load, never cross-arch.
# See docs/proposals/OPENCODE-WORKER-SINGLE-TARGET-BUILD.md for full rationale.
RUN set -eu; \
    actual="$(sha256sum /build/models-dev-api.json | cut -d' ' -f1)"; \
    [ "$actual" = "${MODELS_DEV_SNAPSHOT_SHA256}" ] || { \
      echo "models.dev snapshot sha256 mismatch: got $actual, expected ${MODELS_DEV_SNAPSHOT_SHA256}" >&2; \
      exit 1; \
    }; \
    python3 /build/check_models_dev_snapshot.py script/generate.ts /build/models-dev-api.json; \
    MODELS_DEV_API_JSON=/build/models-dev-api.json OPENCODE_MODELS_URL=http://127.0.0.1:9 \
      bun run build -- --single

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
FROM docker.io/oven/bun:1.4.4-debian@sha256:93d5032fe06d163427822dc1598f420363759bb18155c9cc90eb1be05a64a19e AS runtime

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
    apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=5 -o Acquire::http::Timeout=60 update; \
    apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=60 install -y --no-install-recommends \
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
# WI#454/ME-1: the image-wide PATH carries ONLY root-owned directories.
# Since WI#400 the container runs as root, and every root-side command
# resolution (the image HEALTHCHECK, the root entrypoint on an in-place
# restart, the bridge's setpriv lookup) walks this PATH — task-owned
# entries first would let the task uid plant a file that root executes.
# The task gets its toolchain directories at the privilege drop instead
# (as_task in entrypoint.sh; the serve process's env).
# MI-2 (run-186 review): /usr/local/bun-node-fallback-bin is root-owned
# (0755) on the oven/bun:debian base and holds `node -> /usr/local/bin/bun`;
# it must stay on the PATH so task code calling `node`/`npx` still resolves.
ENV GOROOT=/usr/local/go \
    GOPATH=/home/opencode/go \
    CARGO_HOME=/home/opencode/.cargo \
    PATH=/usr/local/cargo/bin:/usr/local/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/local/bun-node-fallback-bin

# ── non-root user ──────────────────────
# Keep IDs synchronized with ic/scripts/lunarwing-mt-admin.sh. Rootless Podman
# maps the tenant's host identity onto these IDs so private read-only mounts
# remain accessible without making Study Notes state world-readable.
# WI#454: the image defaults to the task identity. mt-admin selects the
# distinct bridge uid for the credential-holding container; both share one group.
ARG SOURCE_DATE_EPOCH=0
RUN groupadd --gid 10001 opencode \
    && useradd --uid 10001 --gid 10001 -m -d /home/opencode opencode \
    && useradd --uid 10101 --gid 10001 -m -d /home/opencode-bridge opencode-bridge \
    && shadow_entry="$(getent shadow opencode)" \
    && shadow_day="$(printf '%s' "$shadow_entry" | cut -d: -f3)" \
    && test "${shadow_day:-0}" = "$((SOURCE_DATE_EPOCH / 86400))" \
    && shadow_entry="$(getent shadow opencode-bridge)" \
    && shadow_day="$(printf '%s' "$shadow_entry" | cut -d: -f3)" \
    && test "${shadow_day:-0}" = "$((SOURCE_DATE_EPOCH / 86400))"

# ── app layout ─────────────────
WORKDIR /app

COPY --from=builder /out/bin/opencode /usr/local/bin/opencode
COPY --from=builder /out/sdk /app/node_modules/@opencode-ai/sdk

# /app stays root-owned and read-only to both runtime identities.
COPY entrypoint.sh entrypoint-split.sh entrypoint-task-setup.sh health_server.py agent_comm_protocol.json ./
COPY scripts/ ./scripts/
COPY config/ ./config/

# WI#206/RG-1: task-side inputs must be readable by the task uid (10001) yet
# stay root-owned and read-only. COPY preserves build-context modes, so a
# checkout made under a restrictive umask (0700 dirs / 0600 files, the shape
# mt-admin's private tenant clone has) would leave /app/config and /app/scripts
# unreadable by 10001 while root-side boot still worked. Normalize the modes
# here so the image is identical regardless of the builder's umask.
#
# WI#529 REWRITE CONTRACT (binding, do not relax): the baked inputs under
# /app/config are immutable — root-owned, no write bit, asserted below.
# In-container consumers that need to change their config (the model/baseURL
# override, the Paseo MCP entry, the Study Notes MCP entry in
# entrypoint-task-setup.sh) write a TASK-OWNED WORKSPACE COPY, never the baked
# original: they call materialize_config_copy/ensure_config_rewritable, which
# copy and then `chmod u+w` the copy because `cp` inherits this 0444 mode.
# Adding an in-place writer? Bind it to those helpers; the static + live
# contract test is .gitlab/tests/test_worker_config_rewrite_contract.py.
RUN set -eu; \
    chmod -R a+rX /app/config /app/scripts; \
    chmod a+r /app/health_server.py /app/agent_comm_protocol.json; \
    chmod 0555 /app/entrypoint.sh /app/entrypoint-split.sh /app/entrypoint-task-setup.sh /app/health_server.py; \
    chmod -R a-w /app/config /app/scripts /app/agent_comm_protocol.json; \
    command -v setpriv >/dev/null; \
    test -z "$(find -L /app/config /app/scripts /app/entrypoint.sh /app/entrypoint-split.sh \
      /app/entrypoint-task-setup.sh /app/health_server.py /app/agent_comm_protocol.json \
      \( ! -uid 0 -o -perm /222 \) -print)"; \
    setpriv --reuid=10001 --regid=10001 --clear-groups \
      find -L /app/config /app/scripts /app/entrypoint.sh /app/entrypoint-split.sh \
      /app/entrypoint-task-setup.sh /app/health_server.py /app/agent_comm_protocol.json \
      -type f -exec cat -- {} + >/dev/null; \
    probe_dir="$(mktemp -d)"; \
    trap 'rm -rf "$probe_dir"' EXIT; \
    chown 10001:10001 "$probe_dir"; \
    install -d -o 10001 -g 10001 -m 0755 "$probe_dir/home" "$probe_dir/workspace"; \
    setpriv --reuid=10001 --regid=10001 --clear-groups \
      env -i PATH="$PATH" HOME="$probe_dir/home" WORKSPACE_ROOT="$probe_dir/workspace" \
      OPENCODE_BASE_URL=https://probe.example.test/openai/v1/ \
      /app/entrypoint-task-setup.sh; \
    test -s "$probe_dir/workspace/.opencode/opencode.json"; \
    test -n "$(find "$probe_dir/workspace/.opencode/opencode.json" -perm -0200 -print)"; \
    setpriv --reuid=10001 --regid=10001 --clear-groups \
      env -i PATH="$PATH" HOME="$probe_dir/home" WORKSPACE_ROOT="$probe_dir/workspace" \
      OPENCODE_BASE_URL=https://probe.example.test/openai/v1/ \
      PASEO_URL=https://paseo.example.test/ PASEO_TOKEN=probe \
      /app/entrypoint-task-setup.sh; \
    grep -q '"paseo"' "$probe_dir/workspace/.opencode/opencode.json"; \
    test "$(stat -c %a /app/config/opencode.json)" = 444

# Fail the BUILD (not container startup) if the bridge's SDK import can't
# resolve — the SDK build above is best-effort, so verify the exact
# import the worker depends on. Without this a broken/renamed SDK passes
# `docker build` and only surfaces as a module-resolution error when the bridge
# starts inside the running container.
RUN cd /app && bun -e 'import("@opencode-ai/sdk/v2/client").then(m => { if (typeof m.createOpencodeClient !== "function") { console.error("FATAL: @opencode-ai/sdk/v2/client resolved but is missing createOpencodeClient"); process.exit(1) } }).catch(e => { console.error("FATAL: cannot import @opencode-ai/sdk/v2/client —", e.message); process.exit(1) })'
RUN test "$(/usr/local/bin/opencode --version)" = "${OPENCODE_VERSION}" \
    && cd /app \
    && bun scripts/path_expand_test.ts \
    && bun scripts/task_identity_test.ts \
    && bun scripts/opencode_result_test.ts \
    && bun scripts/opencode_task_executor_test.ts \
    && bun scripts/lunarwing_bridge_test.ts \
    && bun scripts/lunarwing_tool_proxy_test.ts \
    && bun scripts/lunarwing_tool_plugin_test.ts \
    && bun test scripts/task_runner_test.ts \
    && python3 scripts/study_notes_config_test.py

# ── XDG directories for opencode state ────────────────────────
RUN mkdir -p /home/opencode/.local/share/opencode \
             /home/opencode/.config/opencode \
             /workspace \
             /var/log/lunarwing \
             /app/results \
    && chown -R opencode:opencode /home/opencode /workspace /var/log/lunarwing /app/results

RUN install -d -o root -g opencode -m 3770 /run/lunarwing-shared \
    && install -d -o opencode -g opencode -m 2711 /run/lunarwing-shared/runner \
    && install -d -o opencode-bridge -g opencode -m 2711 /run/lunarwing-shared/tool-proxy

# ── ports ──────────────────────
EXPOSE 8443
EXPOSE 9090

# ── health check ───────────────────────
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD /usr/bin/curl -sf http://127.0.0.1:8443/health || exit 1

# WI#454: task containers use this default; mt-admin selects the bridge uid
# and split-bridge role for the credential-holding container.
ENV OPENCODE_TASK_UID=10001 \
    OPENCODE_TASK_GID=10001 \
    OPENCODE_BRIDGE_UID=10101
USER 10001:10001

ENTRYPOINT ["/usr/bin/tini", "--", "/app/entrypoint.sh"]
CMD ["--mode", "websocket"]
