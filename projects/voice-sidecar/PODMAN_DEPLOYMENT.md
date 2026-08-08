# LunarVoice Podman Deployment

## Build

```bash
podman build --network=host --format docker \
  -t lunarwing/voice-service:latest .
```

The builder downloads checksum-pinned sherpa-onnx runtime libraries through
the `sherpa-rs` build. Runtime model files remain external.

## Run

```bash
install -d -m 0755 /var/lib/lunarwing/voice-models
podman run -d \
  --name lunarwing-voice-sidecar \
  --env-file /etc/lunarwing/voice.env \
  -e VOICE_BIND_HOST=0.0.0.0 \
  -p 127.0.0.1:8090:8090 \
  -p 127.0.0.1:8091:8091 \
  -v /var/lib/lunarwing/voice-models:/models:ro \
  --security-opt=no-new-privileges \
  --read-only \
  --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  lunarwing/voice-service:latest
```

Inside a container, service binds `0.0.0.0`; published host bindings remain
loopback-only. Bare-metal service should keep `VOICE_BIND_HOST=127.0.0.1`.
Multi-tenant hosts must use the
`voice_service` and `voice_health` ports from `/etc/lunarwing/ports.json`; use
`lunarwing-mt-admin.sh` rather than hand-written container lifecycle commands.

## Verify

```bash
curl -fsS http://127.0.0.1:8091/health
curl -fsS http://127.0.0.1:8091/ready
```

`/health` proves process liveness. `/ready` proves the required STT model was
loaded. A healthy but unready service must not receive transcription traffic.
