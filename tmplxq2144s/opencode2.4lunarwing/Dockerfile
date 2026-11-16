FROM example.invalid/bun AS builder
# COPY scripts/compile_out_remote_tunnel.py /build/compile_out_remote_tunnel.py
RUN python3 /build/compile_out_remote_tunnel.py /build/opencode
RUN OPENCODE_VERSION="${OPENCODE_VERSION#v}" bun run build -- --single
FROM example.invalid/bun AS runtime
COPY --from=builder /out/bin/opencode /usr/local/lib/opencode2/opencode
RUN install -D -m 0755 /app/scripts/opencode-exposure-guard.sh /usr/local/bin/opencode \
    && python3 /app/scripts/exposure_policy_assert.py --lane /app --profile opencode2 --binary /usr/local/lib/opencode2/opencode
COPY scripts/ ./scripts/
COPY config/ ./config/
COPY entrypoint.sh entrypoint-split.sh health_server.py agent_comm_protocol.json ./
RUN chmod -R a-w /app/scripts /app/config /app/entrypoint.sh /app/entrypoint-split.sh /app/health_server.py /app/agent_comm_protocol.json
RUN chmod -R a-s,a-w /app/node_modules
RUN --network=none bash /app/scripts/runtime_smoke_entrypoint.sh
ARG LUNARWING_WRAPPER_SHA256=
RUN python3 /app/scripts/exposure_policy_assert.py --lane /app --profile opencode2 --image-filesystem
USER 10003:10003
