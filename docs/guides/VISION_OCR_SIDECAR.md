# Vision and OCR Sidecar

The service in `projects/ocr-sidecar/` provides Tesseract OCR and optional vision-language analysis for LunarWing. The daemon uses it automatically for inbound image attachments when `VISION_SERVICE_URL` is configured; the `vision-analyze` WASM tool uses the same host-injected service URL and credential.

**Revalidated against source 2026-09-17 (v2.1.2.0); re-checked 2026-09-27 (v2.1.3.0), 2026-10-01 (v2.1.4.0), 2026-10-04 (v2.1.5.0), and 2026-10-14 (v2.1.6.0).** Canonical doc links,
endpoint table, `configure-vision-external`/`configure-vision-local` mt-admin
commands, and the ports-registry guidance were re-checked against the current
tree; no changes were needed (the vision-analyze tool registry moved
0.1.5 → 0.1.6 in v2.1.6.0; the host contract here is unchanged).

## Canonical Documentation

- [`projects/ocr-sidecar/README.md`](../../projects/ocr-sidecar/README.md) — API, configuration, and quick start
- [`projects/ocr-sidecar/DOCUMENTATION.md`](../../projects/ocr-sidecar/DOCUMENTATION.md) — implementation and operational details
- [`projects/ocr-sidecar/PODMAN_DEPLOYMENT.md`](../../projects/ocr-sidecar/PODMAN_DEPLOYMENT.md) — Podman deployment
- [`projects/ocr-sidecar/SERVICE_MAP.md`](../../projects/ocr-sidecar/SERVICE_MAP.md) — service and port relationships
- [`ic/tools-src/vision-analyze/`](../../ic/tools-src/vision-analyze/) — LunarWing WASM tool source

## Quick Start

```bash
cd projects/ocr-sidecar
podman compose up --build
```

The compose file is `compose.yaml`. Docker Compose can use the same file where Docker is the approved runtime.

A single-node default deployment exposes the main API on `OCR_PORT=8088` and the dedicated health listener on `OCR_HEALTH_PORT=8089`. Multi-tenant deployments must use each tenant's `vision_service` and `vision_health` values from `/etc/lunarwing/ports.json`; do not hardcode `8088` or `8089` for tenant health checks.

## Current Endpoints

| Endpoint | Listener | Description |
| --- | --- | --- |
| `POST /v1/ocr` | main API | Versioned OCR endpoint |
| `POST /v1/vision/analyze` | main API | Versioned OCR/VL smart-routing endpoint |
| `GET /v1/vision/metrics` | main API | JSON usage metrics |
| `GET /metrics` | main API | Prometheus metrics |
| `GET /openapi.json` | main API | OpenAPI document |
| `GET /health` | dedicated operational port | Health check; no bearer token required |
| `GET /vision/metrics` | dedicated operational port | JSON metrics for the host health pipeline |

Unversioned `/ocr`, `/vision/analyze`, and `/vision/metrics` routes remain compatibility aliases, but new API integrations should use `/v1/...`. The dedicated operational listener intentionally exposes only `/health` and unversioned `/vision/metrics`.

Set `LUNARWING_AUTH_TOKEN` on the sidecar to require bearer authentication for OCR and vision-analysis POST endpoints, and set the matching `VISION_AUTH_TOKEN` at the LunarWing host boundary. Never embed the token in WASM capabilities or client-visible configuration. Operational and metadata endpoints are unauthenticated, so publish them only on trusted interfaces.

## Managed Lifecycle

Use `ic/scripts/lunarwing-mt-admin.sh` for multi-tenant deployments. Local mode
publishes distinct registry `vision_service` and `vision_health` ports, recreates
a container when its image, environment, port, runtime, or init contract changes,
and reports startup success only after the internal `8089/health` probe passes.
systemd hosts use tenant user Quadlets, with a managed `podman wait` user-unit
fallback on older Podman. OpenRC hosts use a health-aware direct unit plus a
`lunarwing-vision-<tenant>-sup` crash supervisor.

A tenant on a machine without suitable inference hardware can use a full
external LunarVision instance instead:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-vision-external <tenant> \
  --url https://vision.example.com \
  --token-file /root/lunarvision.token
```

The URL must be an HTTPS origin without embedded credentials, path, query, or
fragment. The command probes `<origin>/health` first, retires local supervision,
updates the daemon's mode-`0600` URL/token configuration, records non-secret mode
metadata for health discovery, and restarts a running daemon. `start-tenant` then
health-gates the external service without creating a local container. Restore
local service management with:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-vision-local <tenant>
```

The remote host must terminate TLS in front of the sidecar and route `/v1/*` to
the API listener. Route `/health` and `/vision/metrics` to the operational
listener when possible; both are unauthenticated and should be network-restricted.
See the external-instance section in the canonical sidecar README for a reverse
proxy example and the full security contract.
