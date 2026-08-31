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

## Bare metal (systemd / OpenRC)

`systemd/voice-sidecar.service` and `init.d/voice-sidecar` run the released
binary directly instead of a container. Build or obtain a release binary using
the repository's trusted release process, then install the service account,
binary, native libraries, models, and secret environment:

```bash
sudo useradd --system --home-dir /var/lib/lunarvoice \
  --shell /usr/sbin/nologin lunarvoice 2>/dev/null || true
sudo install -d -o lunarvoice -g lunarvoice -m 0750 \
  /opt/lunarvoice /opt/lunarvoice/lib /var/lib/lunarvoice/models
sudo install -o root -g root -m 0755 \
  target/release/lunarwing-voice-sidecar /opt/lunarvoice/
# Install the checksum-pinned native libraries produced by the release build
# beneath /opt/lunarvoice/lib, and verified model artifacts beneath
# /var/lib/lunarvoice/models.
sudo install -d -o root -g root -m 0755 /etc/lunarwing
sudo install -o root -g root -m 0600 /dev/null \
  /etc/lunarwing/voice-sidecar.env
# Write VOICE_AUTH_TOKEN=... and any VOICE_* overrides. The committed systemd
# unit already sets VOICE_MODEL_DIR=/var/lib/lunarvoice/models.
```

The environment file carries a bearer token. Keep it a regular, non-symlinked,
mode-`0600` file; the OpenRC unit refuses any other shape before filesystem
mutation. Then install exactly one init template:

```bash
# systemd
sudo install -o root -g root -m 0644 \
  systemd/voice-sidecar.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now voice-sidecar.service

# OpenRC (Gentoo)
sudo install -o root -g root -m 0755 \
  init.d/voice-sidecar /etc/init.d/voice-sidecar
sudo rc-update add voice-sidecar default
sudo rc-service voice-sidecar start
```

Both init paths wait for `/health` and `/ready`; startup fails if the process is
alive but its required STT model is unavailable. OpenRC uses
`supervise-daemon` with bounded respawn. The systemd unit applies the standard
sandbox set (`ProtectSystem=strict`, read-only model store,
`NoNewPrivileges=true`, `UMask=0077`, bounded restart storms). Validate either
installation with:

```bash
curl -fsS http://127.0.0.1:8091/health
curl -fsS http://127.0.0.1:8091/ready
```

For multi-tenant hosts, do not install these global templates. Use the
`install-voice` → `configure-voice` → `activate-voice` mt-admin lifecycle in
`README.md`; it owns per-tenant ports, credentials, containers, rollback, and
systemd-user/OpenRC supervision.
