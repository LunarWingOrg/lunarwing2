# Rootless Podman Deployment Guide

This guide covers standalone LunarVision deployment. Multi-tenant operators
should use `ic/scripts/lunarwing-mt-admin.sh`; it owns tenant ports, rootless
stores, systemd user units, OpenRC services, container recreation, and health
pipeline integration.

## Runtime contract

LunarVision has two listeners:

- `8088`: authenticated OCR/vision API plus `/metrics` and `/vision/metrics`
- `8089`: unauthenticated `/health` and `/vision/metrics` operational routes

Publish both on loopback. The dedicated listener lets health checks keep working
when inference traffic saturates the API listener. The process exits non-zero if
either listener unexpectedly stops, allowing init supervision to recover it.

## Prerequisites

- Podman with a working Compose provider (`podman compose version`)
- The repository checkout at the path configured by the service template
- A rootless service account with a persistent runtime directory
- A mode-`0600`, service-account-owned environment file containing at least
  `LUNARWING_AUTH_TOKEN`

## Quick start

```bash
cd projects/ocr-sidecar
install -m 0600 /dev/null "$HOME/.config/lunarwing/ocr-sidecar.env"
printf 'LUNARWING_AUTH_TOKEN=%s\n' 'replace-with-a-random-token' \
  >"$HOME/.config/lunarwing/ocr-sidecar.env"
set -a
. "$HOME/.config/lunarwing/ocr-sidecar.env"
set +a
podman compose up --build -d

curl --fail http://127.0.0.1:8089/health
curl --fail http://127.0.0.1:8089/vision/metrics
podman inspect lunarwing-ocr-sidecar --format '{{.State.Health.Status}}'
```

Use `podman compose logs -f` for logs and `podman compose down` to stop the
standalone deployment.

## Direct `podman run`

```bash
podman build -t lunarwing/vision-service .
podman run -d \
  --name lunarwing-ocr-sidecar \
  --env-file "$HOME/.config/lunarwing/ocr-sidecar.env" \
  --security-opt=no-new-privileges \
  -p 127.0.0.1:8088:8088 \
  -p 127.0.0.1:8089:8089 \
  -v ocr_cache:/app/cache \
  lunarwing/vision-service
```

Do not rely on an image health check alone for crash recovery. Use systemd,
OpenRC, or another supervisor that restarts the container lifecycle.

## systemd user service

The standalone template is
`../../ic-infrastructure-health-check/init-templates/ocr-sidecar.service`.
Review its `WorkingDirectory`, then install it as the same user that owns the
rootless Podman store:

```bash
mkdir -p "$HOME/.config/systemd/user" "$HOME/.config/lunarwing"
cp ../../ic-infrastructure-health-check/init-templates/ocr-sidecar.service \
  "$HOME/.config/systemd/user/ocr-sidecar.service"
systemctl --user daemon-reload
systemctl --user enable --now ocr-sidecar.service
systemctl --user status ocr-sidecar.service
```

Enable lingering when the service must survive logout:

```bash
sudo loginctl enable-linger "$USER"
```

The user unit runs Compose in the foreground, validates startup through the
container health check, restarts failed runs, and executes `compose down` during
an intentional stop.

## OpenRC service (Gentoo)

The standalone template is
`../../ic-infrastructure-health-check/init-templates/ocr-sidecar`. Set
`ocr_user`, `ocr_home`, and `directory` in `/etc/conf.d/ocr-sidecar`; the default
service account is `lunarwing`.

```bash
sudo cp ../../ic-infrastructure-health-check/init-templates/ocr-sidecar \
  /etc/init.d/ocr-sidecar
sudo chmod 0755 /etc/init.d/ocr-sidecar
sudo install -m 0644 /dev/null /etc/conf.d/ocr-sidecar
sudo rc-update add ocr-sidecar default
sudo rc-service ocr-sidecar start
sudo rc-service ocr-sidecar status
```

OpenRC uses `supervise-daemon` around foreground `podman compose up`, prepares
`HOME` and `XDG_RUNTIME_DIR` for the rootless account, and runs `compose down`
after an intentional stop. A missing runtime, environment file, service user, or
Compose directory is a start failure rather than a silent skip.

For multi-tenant Gentoo hosts, do not install this global service. Run
`lunarwing-mt-admin.sh start-tenant <tenant>` instead; it renders per-tenant
`lunarwing-vision-<tenant>` health units plus `-sup` crash supervisors.

## Health and metrics

```bash
# Dedicated operational listener
curl --fail http://127.0.0.1:8089/health
curl --fail http://127.0.0.1:8089/vision/metrics

# API-listener Prometheus endpoint
curl --fail http://127.0.0.1:8088/metrics

# Container-level health history
podman inspect lunarwing-ocr-sidecar --format '{{json .State.Health}}' | jq
```

On multi-tenant hosts, leave `HEALTH_LUNARVISION_URL` unset. The infrastructure
health pipeline discovers every tenant's `vision_health` port from
`/etc/lunarwing/ports.json`. The single-node fallback is `127.0.0.1:8089`; port
`8088` is not the default health target.

## Configuration

Common environment variables:

- `LUNARWING_AUTH_TOKEN`: bearer token for OCR/vision POST APIs
- `OCR_PORT`: API listener, default `8088`
- `OCR_HEALTH_PORT`: operational listener, default `8089`
- `VL_URL`, `VL_API_KEY`, `VL_MODEL`, `VL_TIMEOUT_SECS`: VL backend
- `ENABLE_PADDLEOCR`, `ENABLE_CACHE`, `CACHE_PERSIST`, `CACHE_DIR`
- `ENABLE_PROMETHEUS`, `RATE_LIMIT_PER_SECOND`

Keep secret-bearing environment files out of the checkout, mode `0600`, and owned
by the service account. Bind both host ports to loopback unless an authenticated
reverse proxy and explicit network policy protect them; operational endpoints
are intentionally unauthenticated.

## Troubleshooting

```bash
# Confirm both listeners from inside the container
podman exec lunarwing-ocr-sidecar curl --fail http://127.0.0.1:8089/health
podman exec lunarwing-ocr-sidecar curl --fail http://127.0.0.1:8089/vision/metrics

# Confirm loopback publications
podman port lunarwing-ocr-sidecar

# systemd user logs
journalctl --user -u ocr-sidecar.service

# OpenRC logs use output_log/error_log from /etc/conf.d/ocr-sidecar
sudo rc-service ocr-sidecar status
```

If the container exists but uses stale ports, environment, or image data, remove
and recreate it; `podman start` cannot change creation-time configuration.
`lunarwing-mt-admin.sh start-tenant` performs this reconciliation automatically
for managed tenants.
