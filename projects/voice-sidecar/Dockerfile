# syntax=docker/dockerfile:1.7

FROM rust:1.96.1-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        bzip2 clang cmake libclang-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        bzip2 ca-certificates curl ffmpeg jq libstdc++6 util-linux \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /app --shell /usr/sbin/nologin voice \
    && install -d -o voice -g voice /app /app/lib /models

WORKDIR /app
COPY --from=builder /app/target/release/lunarwing-voice-sidecar /app/lunarwing-voice-sidecar
COPY --from=builder /app/target/release/*.so /app/lib/
COPY scripts/download-models.sh /app/download-models.sh
COPY scripts/entrypoint.sh /app/entrypoint.sh
RUN chmod 0755 /app/lunarwing-voice-sidecar /app/download-models.sh /app/entrypoint.sh \
    && chown -R voice:voice /app /models

USER voice
ENV LD_LIBRARY_PATH=/app/lib \
    VOICE_MODEL_DIR=/models \
    VOICE_BIND_HOST=0.0.0.0 \
    VOICE_PORT=8090 \
    VOICE_HEALTH_PORT=8091

EXPOSE 8090 8091
VOLUME ["/models"]
HEALTHCHECK --interval=30s --timeout=5s --retries=3 --start-period=10s \
    CMD curl -fsS http://127.0.0.1:8091/health >/dev/null || exit 1

ENTRYPOINT ["/app/entrypoint.sh"]
