ARG LUNARDEPTH_REPOSITORY=https://git.lunarwing.org/lunarwing/lunardepth.git
ARG LUNARDEPTH_REF=da06c33b2091546dc38c61b95452ce27a4ce32d5
ARG LUNARDEPTH_COMPATIBILITY_REF=17aef23e05cc074c8621d38d159d77d25cb348ca
ARG LUNARDEPTH_MODEL_ROUTING_REF=8c610180c84a2f4c27fb2958e6e7dab9e48d476b
ARG LUNARDEPTH_PROFILE_SCHEMA_VERSION=unversioned
ARG DEBIAN_SNAPSHOT=20261105T082236Z
ARG SOURCE_REVISION=unknown

FROM docker.io/library/node:22.23.4-bookworm-slim@sha256:4b150b48b4116e2a22f2c39cc27025fbf4f153e2078fd3b14d159fdb5c7279da AS builder
ARG LUNARDEPTH_REPOSITORY
ARG LUNARDEPTH_REF
ARG LUNARDEPTH_COMPATIBILITY_REF
ARG LUNARDEPTH_MODEL_ROUTING_REF
ARG LUNARDEPTH_PROFILE_SCHEMA_VERSION
ARG DEBIAN_SNAPSHOT
ARG SOURCE_DATE_EPOCH=0
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
COPY --from=docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
RUN set -eu; \
    test -s /etc/ssl/certs/ca-certificates.crt; \
    rm -f /etc/apt/sources.list.d/debian.sources; \
    printf '%s\n' \
      "deb https://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ bookworm main" \
      "deb https://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ bookworm-updates main" \
      "deb https://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT}/ bookworm-security main" \
      > /etc/apt/sources.list; \
    timeout -k 30 240 apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=0 -o Acquire::https::Timeout=60 --error-on=any update; \
    timeout -k 30 600 apt-get -o Acquire::Retries=0 -o Acquire::https::Timeout=60 install -y --no-install-recommends apt; \
    dpkg --compare-versions "$(dpkg-query -W -f='${Version}' apt)" ge 2.6.2; \
    timeout -k 30 600 apt-get -o Acquire::Retries=3 -o Acquire::https::Timeout=60 install -y --no-install-recommends \
      git python3 make g++ musl-tools ca-certificates bubblewrap util-linux; \
    update-ca-certificates; \
    rm -rf /var/lib/apt/lists/* /var/log/apt/* /var/log/dpkg.log* /var/log/alternatives.log* /var/cache/ldconfig/aux-cache

RUN set -eu; \
    test "$LUNARDEPTH_REPOSITORY" = "https://git.lunarwing.org/lunarwing/lunardepth.git"; \
    [[ "$LUNARDEPTH_REF" =~ ^[0-9a-f]{40}$ ]]; \
    [[ "$LUNARDEPTH_COMPATIBILITY_REF" =~ ^[0-9a-f]{40}$ ]]; \
    [[ "$LUNARDEPTH_MODEL_ROUTING_REF" =~ ^[0-9a-f]{40}$ ]]; \
    git clone --filter=blob:none --no-checkout "$LUNARDEPTH_REPOSITORY" /src; \
    git -C /src checkout --detach "$LUNARDEPTH_REF"; \
    test "$(git -C /src rev-parse HEAD)" = "$LUNARDEPTH_REF"; \
    git -C /src merge-base --is-ancestor "$LUNARDEPTH_COMPATIBILITY_REF" "$LUNARDEPTH_REF"; \
    git -C /src merge-base --is-ancestor "$LUNARDEPTH_MODEL_ROUTING_REF" "$LUNARDEPTH_REF"; \
    for path in scripts/check-toolchain.mjs deepseek-harness/package.json deepseek-harness/pnpm-lock.yaml \
      deepseek-harness/scripts/build-exe-for-python-sdk.ts deepseek-harness/packages/api/lunarwing-worker \
      deepseek-harness/packages/bundle/lunarwing-worker deepseek-harness/packages/llm/llm-pi-ai/src/config.ts \
      deepseek-harness/packages/bundle/base/cordis.patch.yml; do \
      test -e "/src/$path"; \
    done; \
    test "$(node -p "require('/src/deepseek-harness/package.json').packageManager")" = "pnpm@11.7.0"; \
    grep -q 'allowInsecureHTTP' /src/deepseek-harness/packages/llm/llm-pi-ai/src/config.ts; \
    grep -q 'tensormoon' /src/deepseek-harness/packages/bundle/base/cordis.patch.yml; \
    corepack enable; \
    corepack prepare pnpm@11.7.0 --activate; \
    test "$(pnpm --version)" = "11.7.0"; \
    node /src/scripts/check-toolchain.mjs

WORKDIR /src/deepseek-harness
RUN set -eu; \
    before="$(sha256sum pnpm-lock.yaml)"; \
    pnpm install --frozen-lockfile; \
    test "$before" = "$(sha256sum pnpm-lock.yaml)"; \
    pnpm exec tsx native/system/scripts/build.ts; \
    pnpm exec vitest run packages/api/lunarwing-worker/tests packages/bundle/lunarwing-worker/tests; \
    pnpm exec tsc -b packages/api/lunarwing-worker/tsconfig.json packages/bundle/lunarwing-worker/tsconfig.json --pretty false; \
    pnpm exec tsx scripts/build-exe-for-python-sdk.ts --deploy-to=/opt/lunardepth; \
    sed -i '/^  "prunedAt":/d' /opt/lunardepth/node_modules/.modules.yaml; \
    landlock="$(node -e "const {createRequire}=require('module'); const {dirname,join}=require('path'); const r=createRequire('/opt/lunardepth/node_modules/@lunardepth/dsh/package.json'); const pkg=r.resolve('@lunardepth/node-addon-system-linux-x64/package.json'); process.stdout.write(join(dirname(pkg),'bin','landlock-run'))")"; \
    test -x "$landlock"; \
    "$landlock" --probe | grep -q 'fully enforced'

WORKDIR /build
COPY scripts/package.json scripts/package-lock.json scripts/tsconfig.json ./scripts/
COPY scripts/*.ts ./scripts/
COPY scripts/*.py ./scripts/
COPY scripts/*.sh ./scripts/
COPY config/ ./config/
COPY entrypoint.sh entrypoint-split.sh entrypoint-task-setup.sh health_server.py agent_comm_protocol.json ./
COPY AGENTS.md README.md Dockerfile docker-compose.yml .env.example ./
WORKDIR /build/scripts
RUN set -eu; \
    npm ci --ignore-scripts; \
    npx tsc -p tsconfig.json; \
    npm test; \
    python3 -m unittest discover -p '*_test.py'; \
    npm prune --omit=dev; \
    rm -rf node_modules/.cache /root/.npm/_logs /tmp/node-compile-cache

RUN set -eu; \
    node -e "const {createRequire}=require('module'); const r=createRequire('/opt/lunardepth/node_modules/@lunardepth/dsh/package.json'); r.resolve('@lunardepth/dsh-lunarwing-worker-app'); r.resolve('@lunardepth/dsh-lunarwing-worker')"; \
    schema="$(node /build/scripts/profile_template.js /opt/lunardepth /opt/lunardepth-templates)"; \
    test "$schema" = "$LUNARDEPTH_PROFILE_SCHEMA_VERSION"; \
    source_version="$(node -p "require('/src/deepseek-harness/package.json').version")"; \
    { \
      printf 'LUNARDEPTH_REPOSITORY=%s\n' "$LUNARDEPTH_REPOSITORY"; \
      printf 'LUNARDEPTH_REF=%s\n' "$LUNARDEPTH_REF"; \
      printf 'LUNARDEPTH_COMPATIBILITY_REF=%s\n' "$LUNARDEPTH_COMPATIBILITY_REF"; \
      printf 'LUNARDEPTH_MODEL_ROUTING_REF=%s\n' "$LUNARDEPTH_MODEL_ROUTING_REF"; \
      printf 'LUNARDEPTH_SOURCE_VERSION=%s\n' "$source_version"; \
      printf 'LUNARDEPTH_PROFILE_SCHEMA_VERSION=%s\n' "$schema"; \
      sha256sum /src/deepseek-harness/pnpm-lock.yaml; \
    } > /opt/lunardepth/worker-inputs.sha256; \
    find /opt/lunardepth -type d \( -name .git -o -name tests -o -name __tests__ -o -name .cache \) -prune -exec rm -rf {} +; \
    find /opt/lunardepth -type f \( -name pnpm-lock.yaml -o -name pnpm-workspace.yaml -o -name .npmrc \) -delete; \
    rm -rf /opt/lunardepth/node_modules/.bin /src /root/.cache /root/.local/share/pnpm /root/.npm

FROM docker.io/library/node:22.23.4-bookworm-slim@sha256:4b150b48b4116e2a22f2c39cc27025fbf4f153e2078fd3b14d159fdb5c7279da AS runtime
ARG LUNARDEPTH_REF
ARG LUNARDEPTH_PROFILE_SCHEMA_VERSION
ARG DEBIAN_SNAPSHOT
ARG SOURCE_REVISION
ARG WITH_TOOLCHAINS=false
ARG SOURCE_DATE_EPOCH=0
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
LABEL org.opencontainers.image.title="LunarWing LunarDepth Worker" \
      org.opencontainers.image.description="Non-root split LunarDepth external worker" \
      org.opencontainers.image.version="1.0.0" \
      org.opencontainers.image.revision="${SOURCE_REVISION}" \
      org.opencontainers.image.toolchain-profile="${WITH_TOOLCHAINS}" \
      org.lunarwing.lunardepth.revision="${LUNARDEPTH_REF}" \
      org.lunarwing.lunardepth.profile-schema="${LUNARDEPTH_PROFILE_SCHEMA_VERSION}"
COPY --from=docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
RUN set -eu; \
    test -s /etc/ssl/certs/ca-certificates.crt; \
    rm -f /etc/apt/sources.list.d/debian.sources; \
    printf '%s\n' \
      "deb https://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ bookworm main" \
      "deb https://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ bookworm-updates main" \
      "deb https://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT}/ bookworm-security main" \
      > /etc/apt/sources.list; \
    timeout -k 30 240 apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=0 -o Acquire::https::Timeout=60 --error-on=any update; \
    timeout -k 30 600 apt-get -o Acquire::Retries=0 -o Acquire::https::Timeout=60 install -y --no-install-recommends apt; \
    dpkg --compare-versions "$(dpkg-query -W -f='${Version}' apt)" ge 2.6.2; \
    timeout -k 30 600 apt-get -o Acquire::Retries=3 -o Acquire::https::Timeout=60 install -y --no-install-recommends \
      tini python3 git curl ca-certificates bubblewrap util-linux jq openssh-client zstd; \
    update-ca-certificates; \
    rm -rf /var/lib/apt/lists/* /var/log/apt/* /var/log/dpkg.log* /var/log/alternatives.log* /var/cache/ldconfig/aux-cache
RUN if [ "${WITH_TOOLCHAINS}" = "true" ]; then \
      set -eu; \
      timeout -k 30 240 apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=3 -o Acquire::https::Timeout=60 --error-on=any update; \
      timeout -k 30 1800 apt-get -o Acquire::Retries=3 -o Acquire::https::Timeout=60 install -y --no-install-recommends \
        python3-dev build-essential; \
      rm -rf /var/lib/apt/lists/* /var/log/apt/* /var/log/dpkg.log* /var/log/alternatives.log* /var/cache/ldconfig/aux-cache; \
    fi

RUN groupadd --gid 10005 lunardepth \
    && useradd --uid 10005 --gid 10005 -m -d /home/lunardepth lunardepth \
    && useradd --uid 10105 --gid 10005 -m -d /home/lunardepth-bridge lunardepth-bridge \
    && shadow_entry="$(getent shadow lunardepth)" \
    && shadow_day="$(printf '%s' "$shadow_entry" | cut -d: -f3)" \
    && test "${shadow_day:-0}" = "$((SOURCE_DATE_EPOCH / 86400))" \
    && shadow_entry="$(getent shadow lunardepth-bridge)" \
    && shadow_day="$(printf '%s' "$shadow_entry" | cut -d: -f3)" \
    && test "${shadow_day:-0}" = "$((SOURCE_DATE_EPOCH / 86400))"

WORKDIR /app
COPY --from=builder /opt/lunardepth /opt/lunardepth
COPY --from=builder /opt/lunardepth-templates /opt/lunardepth-templates
COPY --from=builder /build/scripts/ /app/scripts/
COPY --from=builder /build/entrypoint.sh /build/entrypoint-split.sh /build/entrypoint-task-setup.sh /build/health_server.py /build/agent_comm_protocol.json ./
COPY config/ ./config/
COPY scripts/dsh-exposure-guard.sh /usr/local/bin/dsh
RUN set -eu; \
    find /app/scripts -maxdepth 1 -type f \( -name '*_test.ts' -o -name '*_test.js' -o -name '*_test.py' \) \
      ! -name 'smoke_test.ts' ! -name 'smoke_test.js' ! -name 'smoke_test.py' -delete; \
    rm -f /app/scripts/tsconfig.json /app/scripts/package-lock.json; \
    rm -rf /app/scripts/__pycache__; \
    find /app /opt/lunardepth /opt/lunardepth-templates -type d -exec chmod 0555 {} +; \
    find /app /opt/lunardepth /opt/lunardepth-templates -type f -exec chmod 0444 {} +; \
    find /opt/lunardepth -type f -path '*/bin/*' -exec chmod 0555 {} +; \
    chmod 0555 /app/entrypoint.sh /app/entrypoint-split.sh /app/entrypoint-task-setup.sh /app/health_server.py \
      /app/scripts/render_overlay.py /app/scripts/*.sh /usr/local/bin/dsh; \
    test -x /app/entrypoint.sh; \
    test -x /app/entrypoint-split.sh; \
    test -x /app/entrypoint-task-setup.sh; \
    install -d -o root -g lunardepth -m 3770 /run/lunarwing-shared; \
    install -d -o lunardepth -g lunardepth -m 2711 /run/lunarwing-shared/runner; \
    install -d -o root -g root -m 0555 /run/lunardepth-launch /run/lunardepth-config /home/lunardepth-home; \
    install -d -o lunardepth -g lunardepth -m 0700 /home/lunardepth/.dsh /run/lunardepth-state /workspace; \
    test -z "$(find -L /app /opt/lunardepth /opt/lunardepth-templates \( ! -uid 0 -o -perm /222 \) -print)"

ENV LUNARDEPTH_IMAGE_REVISION="${LUNARDEPTH_REF}" \
    LUNARDEPTH_IMAGE_VERSION="0.2.1-alpha.1" \
    LUNARDEPTH_PROFILE_SCHEMA_VERSION="${LUNARDEPTH_PROFILE_SCHEMA_VERSION}" \
    LUNARDEPTH_TASK_UID=10005 \
    LUNARDEPTH_TASK_GID=10005 \
    LUNARDEPTH_BRIDGE_UID=10105 \
    LUNARDEPTH_CLOSURE_ROOT=/opt/lunardepth \
    LUNARDEPTH_TEMPLATE_ROOT=/opt/lunardepth-templates \
    LUNARWING_APP_DIR=/app \
    DSH_TELEMETRY_DISABLED=1 \
    WS_NO_BUFFER_UTIL=1 \
    WS_NO_UTF_8_VALIDATE=1 \
    PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN --network=none set -eu; \
    LUNARDEPTH_BUILD_SMOKE=1 /app/scripts/runtime_smoke_entrypoint.sh; \
    rm -rf /tmp/lunardepth-* /tmp/dsh-spill-* /tmp/lunardepth_ws_state.json \
      /run/lunardepth-state/* /run/lunarwing-shared/runner/* \
      /home/lunardepth/.dsh /home/lunardepth-bridge/.local/state /run/lunardepth-config/* /root/.cache /root/.npm; \
    install -d -o lunardepth -g lunardepth -m 0700 /home/lunardepth/.dsh; \
    find /app /opt/lunardepth /opt/lunardepth-templates -exec touch -h -d "@${SOURCE_DATE_EPOCH}" {} +; \
    touch -d "@${SOURCE_DATE_EPOCH}" /usr/local /usr/local/bin/dsh

EXPOSE 8443 9090
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD /usr/bin/curl -sf http://127.0.0.1:8443/ready || exit 1
USER 10005:10005
ENTRYPOINT ["/usr/bin/tini", "--", "/app/entrypoint.sh"]
CMD ["--mode", "websocket"]
