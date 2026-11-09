# Generic sandbox image for the LunarWing podman sandbox.
#
# The generic sandbox (`ic/src/sandbox/`) runs short-lived shell commands from
# the `shell` tool inside hardened containers using this kind of image. It is
# smaller than the full worker image (`ic/Dockerfile.worker`, which contains
# the lunarwing binary) but includes common build tooling and a complete Python
# development suite for builds, tests, packaging, linting, and debugging.
#
# Manual build from the repository root (mt-admin builds in the admin store):
#   podman build -t lunarwing-sandbox:latest -f ic/docker/sandbox.Dockerfile ic/docker/
#
# mt-admin builds this image for every new tenant (WI#223) and writes
# SANDBOX_IMAGE=lunarwing-sandbox:latest + SANDBOX_AUTO_PULL=false into the
# tenant env; manual build is available via
# `sudo ic/scripts/lunarwing-mt-admin.sh build-sandbox-image`. The in-code
# default for single-node hosts remains `lunarwing-worker:latest`. Auto-pull
# only works for registry-hosted images — local images like this must be built
# (or shipped via `podman save | podman load`) into each tenant store; mt-admin
# distributes it into each rootless tenant store on add-tenant. The build
# context is ic/docker/ (not ic/): this Dockerfile has no build-context
# COPY/ADD, and ic/ carries the vendor/ tree that must stay in-context for
# Dockerfile.worker.

ARG DEBIAN_SNAPSHOT=20261105T082236Z

FROM docker.io/library/debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

ARG DEBIAN_SNAPSHOT

# HTTPS snapshot sources, a fixed APT client before any retries, bounded
# downloads, and explicit Python package versions keep this image reproducible
# and preserve the WI#559/WI#561 worker-image apt hardening. The CA bundle comes
# from a digest-pinned image because the slim base cannot verify HTTPS yet.
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
    timeout -k 30 1800 apt-get -o Acquire::Retries=3 -o Acquire::https::Timeout=60 install -y --no-install-recommends \
      bash \
      ca-certificates \
      curl \
      git \
      openssh-client \
      build-essential \
      pkg-config \
      nodejs \
      npm \
      ripgrep \
      jq \
      unzip \
      procps \
      python3=3.11.2-1+b1 \
      python3-dev=3.11.2-1+b1 \
      python3-pip=23.0.1+dfsg-1 \
      python3-venv=3.11.2-1+b1 \
      python3-setuptools=66.1.1-1+deb12u2 \
      python3-wheel=0.38.4-2 \
      python3-build=0.9.0-1 \
      python3-pytest=7.2.1-2 \
      python3-pytest-cov=4.0.0-1 \
      python3-coverage=6.5.0+dfsg1-2+b1 \
      python3-mypy=1.0.1-1 \
      mypy=1.0.1-1 \
      black=23.1.0-1 \
      python3-isort=5.6.4-1 \
      isort=5.6.4-1 \
      python3-flake8=5.0.4-4 \
      flake8=5.0.4-4 \
      pylint=2.16.2-2 \
      python3-ipython=8.5.0-4 \
      ipython3=8.5.0-4 \
      python3-debugpy=1.6.6+ds-1 \
      tox=3.28.0-2 \
      twine=4.0.2-1 \
      pipx=1.1.0-1 \
      python3-poetry=1.3.2+dfsg-3; \
    update-ca-certificates; \
    rm -rf /var/lib/apt/lists/* \
      /var/log/apt/* /var/log/dpkg.log* /var/log/alternatives.log* /var/cache/ldconfig/aux-cache

# Non-root user; UID/GID 1000 must match the sandbox runner's `--user
# 1000:1000` (with podman >= 5 the runner also pins keep-id:uid=1000,gid=1000
# so the mounted /workspace is owned by this user regardless of host UID).
RUN groupadd -g 1000 sandbox \
    && useradd -m -u 1000 -g sandbox sandbox \
    && mkdir -p /workspace /home/sandbox/.cargo/registry \
    && chown -R sandbox:sandbox /workspace /home/sandbox

USER sandbox
WORKDIR /workspace

CMD ["bash"]
