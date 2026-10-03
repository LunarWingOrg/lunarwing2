# LunarWing Infrastructure Health Check System

_Last updated: 2026-09-19 — MR !1041 review findings addressed (gateway legacy-telemetry verdict, runtime/knowledge monitor-only escalation, bounded sweep budget, scrape-time staleness alert, runtime classification, knowledge enablement)._

Automated health monitoring and self-healing for LunarWing infrastructure.
Twelve parallel component checks aggregate into a JSON report; a separate
watchdog reads that report and auto-remediates unhealthy services with a grace
period, exponential backoff, a flapping guard, post-restart verification, and
multi-tenant awareness.

## Architecture

```
cron-wrapper.sh
  ├── infrastructure-health-check.sh   (parallel checks → JSON report)
  │     ├── health-gateway.sh          (agent sessions: locks, orphans, oversized)
  │     ├── health-xmpp.sh             (XMPP bridge reachability + latency)
  │     ├── health-omemo.sh            (OMEMO key/bundle/session store)
  │     ├── health-ratelimit.sh        (throttle counters, queue depth)
  │     ├── health-clickhouse.sh       (query time, disk, memory)
  │     ├── health-tensormoon.sh       (proxy p50/p95, error rate, queue, GPU)
  │     ├── health-models.sh           (LLM provider APIs)
  │     ├── health-llm-streams.sh      (provider stream retries, aborts, corruption, TTFC)
  │     ├── health-lunarvision.sh      (OCR + VL sidecar: health, metrics, VL status)
  │     ├── health-lunarvoice.sh       (enabled local voice sidecars: health + STT readiness)
  │     ├── health-netbird.sh          (opt-in host tunnel readiness)
  │     ├── health-runtime.sh          (tenant gateway/bridge runtime diagnostics)
  │     ├── health-knowledge.sh         (knowledge sync/sweep progress monitoring)
  │     └── health-{systemd,openrc,launchd}.sh  (service manager — one, auto-detected)
  │
  └── lunarwing-self-heal.sh           (reads report → restarts → verifies → escalates)
        └── send-notification.sh       (Gotify push notifications)
```

`cron-wrapper.sh` runs the two halves in sequence and always proceeds to
self-heal even if the health check fails. It auto-detects its own install
location, so it works from wherever this directory lives. Any arguments are
forwarded to `lunarwing-self-heal.sh` (e.g. `cron-wrapper.sh --dry-run`).

### Init-system auto-detection

`infrastructure-health-check.sh` and `lunarwing-self-heal.sh` both detect the
service manager and run exactly one of the three service checks — never both.
Detection order:

1. `LUNARWING_SERVICE_MANAGER` override — `systemd` | `openrc` | `launchd`.
   (Self-heal additionally honors the legacy `IRONCLAW_SERVICE_MANAGER` alias;
   the health-check orchestrator reads only `LUNARWING_SERVICE_MANAGER`.)
2. macOS (`uname -s` = `Darwin`) → `launchd`.
3. `/run/openrc/softlevel` present → `openrc`.
4. `/run/systemd/system` present → `systemd`.
5. Binary fallback: `rc-service`/`systemctl` presence.

If none match, the service check is skipped (logged as a warning) and the rest
of the report is still produced.

## Dependencies

### Required

| Dependency | Purpose | Install |
|------------|---------|---------|
| bash ≥ 4.0 | Associative arrays, `mapfile`, extended globbing | Pre-installed on most systems |
| jq | JSON parsing and generation | `apt install jq` / `brew install jq` / `apk add jq` |
| curl | HTTP health checks, Gotify notifications | Pre-installed on most systems |
| coreutils `timeout` | Per-check 30 s cap; bounded restart/notify/tenant probes | Pre-installed on Linux; `brew install coreutils` on macOS |

### Optional (per-check)

| Dependency | Used by | Install |
|------------|---------|---------|
| systemctl | health-systemd.sh, self-heal (systemd hosts) | Pre-installed on systemd systems |
| rc-service | health-openrc.sh, self-heal (OpenRC hosts) | Pre-installed on OpenRC systems |
| launchctl | health-launchd.sh, self-heal (macOS) | Pre-installed on macOS |
| sudo + podman | health-systemd.sh & self-heal (per-tenant **user** units + container health) | systemd multi-tenant hosts |
| clickhouse-client | health-clickhouse.sh, health-tensormoon.sh | ClickHouse docs |
| nc (netcat) | health-xmpp.sh (port reachability) | `apt install netcat-openbsd` / `brew install netcat` |
| nvidia-smi / rocm-smi | health-tensormoon.sh (GPU utilization) | NVIDIA driver / ROCm package |
| flock | lunarwing-self-heal.sh (single-instance lock) | util-linux (pre-installed on most Linux) |

> `timeout` is only *required* in spirit — every call site degrades gracefully
> when it is absent (notably macOS/launchd hosts without GNU coreutils), running
> the underlying command unbounded instead of failing.

### Not required (replaced)

| Previously needed | Replaced by |
|-------------------|-------------|
| bc | `awk` for float comparisons in health-tensormoon.sh |

## Quick Start

```bash
# 1. Make scripts executable
chmod +x ic-infrastructure-health-check/*.sh

# 2. Run manually (dry-run self-heal — restarts nothing)
cd ic-infrastructure-health-check
./infrastructure-health-check.sh
./lunarwing-self-heal.sh --dry-run

# 3. Schedule recurring runs — see "Cron / Timer Setup" below
```

## Component Checks

Every check writes a single JSON object to stdout (logs go to stderr) and exits
`0` healthy / `1` degraded / `2` critical. The orchestrator aggregates them.

| Component | Script | Inspects | Key metrics |
|-----------|--------|----------|-------------|
| gateway | health-gateway.sh | `$BASE/agents` legacy session files — **only when the directory exists**; an absent legacy directory reports healthy with a telemetry note (live gateway health belongs to the runtime component) | `active_sessions`, `stale_locks` (>10 min), `orphan_transcripts` (`*.jsonl` >10 MB), `oversized_sessions` (>500 KB), `legacy_sessions_dir` |
| xmpp | health-xmpp.sh | TCP reachability of the XMPP server + latency | `connected`, `latency_ms`, `error_count`, `active_sessions` |
| omemo | health-omemo.sh | OMEMO key/bundle/session store | `devices`, `bundles`, `sessions`, `recent_sessions` |
| ratelimit | health-ratelimit.sh | `$BASE/ratelimit` throttle logs + queue | `throttled_last_hour`, `queue_depth` |
| clickhouse | health-clickhouse.sh | ClickHouse responsiveness + resources | `query_time_ms`, `disk_pct`, `memory_pct` |
| tensormoon | health-tensormoon.sh | Gateway latency (ClickHouse `ModelInference`) + GPU | `p50_ms`, `p95_ms`, `error_rate`, `queue_depth`, `gpu_utilization` |
| models | health-models.sh | OpenRouter / OpenAI / Anthropic / local LLM APIs | `provider_count`, `providers_{healthy,degraded,critical}`, `latency_max_ms`, per-provider `provider_latency_ms` / `provider_status` (numeric map; `last_error` stays string-only in `providers[]`) |
| llm_streams | health-llm-streams.sh | Daemon snapshots at `$BASE/runtime/llm-stream-metrics.json`; all registry tenants on MT hosts | Per-provider stream starts/completions, retries by reason, aborts, decode failures, corruption signatures, and time-to-first-chunk samples/max/last; provider and tenant are bounded Prometheus labels |
| lunarvision | health-lunarvision.sh | OCR + VL sidecar `/health` and `/vision/metrics` | `service_status`, `ocr_available`, `vl_available`, `capabilities`, `latency_ms`, cache `hit_rate`, `rate_limited`, `avg_latency_ms` |
| lunarvoice | health-lunarvoice.sh | Enabled LunarVoice `/health`, `/ready`, and `/metrics` endpoints | per-tenant `ready`, `latency_ms`, request counts, cache hits, errors |
| netbird | health-netbird.sh | `netbird status --check startup` (opt-in) | `enabled`, `ready`, `probe_exit` |
| runtime | health-runtime.sh | Per-tenant gateway `/api/gateway/status` + `/api/chat/pending-gates` and bridge `/v1/status` (ports-registry discovery, bearer tokens from tenant env files) | `gateway_reachable`, `gateway_uptime_secs`, `gateway_connections`, `channels_unhealthy` (unhealthy channels degrade the tenant), `pending_gates` (metric only — approval/auth waits are not stalled execution; an unobservable pending-gates endpoint degrades as an observability gap), `bridge_running`, `bridge_queued_messages`, `bridge_outbound_last_hour`, `bridge_omemo_enabled`, `bridge_prekeys_available` |
| knowledge | health-knowledge.sh | Per-tenant `knowledge_sync_state.json` + `knowledge_sweep_state.json` **and the tenant's actual feature enablement** (`KNOWLEDGE_SYNC_ENABLED` default false, `KNOWLEDGE_SWEEP_ENABLED` default true, plus cadences, from the tenant env file) | `sync_enabled`, `sweep_enabled`, `sync_cadence_hours`, `sweep_cadence_hours`, `sync_consecutive_failures`, `sync_last_error_class`, `sync_deferred_notes`, `sync_last_success_age_seconds`, `sweep_last_run_age_seconds`; monitor-only — disabled features ignore their (possibly stale) state; enabled-but-never-observed degrades only past the startup grace period; overdue derives from the configured cadence |
| systemd / openrc / launchd | health-{systemd,openrc,launchd}.sh | Service-manager unit state (incl. per-tenant) | `metrics.units` / `.services` / `.agents` |

A check that cannot reach its target generally reports `critical`; several
checks can instead be **disabled** so they report `healthy` with
`metrics.enabled = false` (see [Disabling individual checks](#disabling-individual-checks)).

## Environment Variables

### Core

| Variable | Default | Description |
|----------|---------|-------------|
| `LUNARWING_BASE_DIR` | `$HOME/.lunarwing` | Base directory for LunarWing data (reports, agents, stores) |
| `IRONCLAW_BASE_DIR` | (unset) | Legacy alias, consulted only if `LUNARWING_BASE_DIR` is unset; the ultimate fallback is `$HOME/.lunarwing` (not `~/.ironclaw`) |
| `LUNARWING_SERVICE_MANAGER` | auto-detect | Force init system: `systemd` \| `openrc` \| `launchd`. Self-heal also accepts the legacy `IRONCLAW_SERVICE_MANAGER` alias; the orchestrator does not |
| `HEALTHCHECK_NOTIFY` | `true` | `false` suppresses the orchestrator's per-run Gotify notify (e.g. MT hosts where only self-heal escalations should page) |
| `GOTIFY_URL` | `http://localhost:3000` | Gotify server URL (`send-notification.sh`) |
| `GOTIFY_TOKEN` | (empty) | Gotify app token — **empty disables all notifications** |
| `TMPDIR` | `/tmp` | Parent for the per-run scratch dir (`mktemp -d`) |

### Per-check tuning

| Variable | Used by | Default | Description |
|----------|---------|---------|-------------|
| `HEALTH_XMPP_SERVER` | health-xmpp.sh | `xmpp.sobe.world` | XMPP reachability target; **empty disables the probe** |
| `HEALTH_XMPP_PORT` | health-xmpp.sh | `5222` | XMPP port to probe |
| `HEALTH_OMEMO_ENABLED` | health-omemo.sh | `true` | Non-`true` disables (reports healthy/disabled) |
| `AGENT_DIR` | health-omemo.sh | `$BASE/agents` | Agent session directory |
| `OMEMO_STORE` | health-omemo.sh | `$BASE/omemo` | OMEMO key/bundle/session store |
| `RATELIMIT_DIR` | health-ratelimit.sh | `$BASE/ratelimit` | Rate-limit state directory |
| `HEALTH_MODELS_ENABLED` | health-models.sh | `true` | Non-`true` disables (reports healthy/disabled) |
| `HEALTH_LLM_STREAMS_ISSUE_WINDOW_SECS` | health-llm-streams.sh | `900` | A retry, abort, decode failure, or corruption observed this many seconds ago makes the monitor-only component degraded |
| `HEALTH_LLM_STREAMS_FILE` | health-llm-streams.sh | _(unset)_ | Test/single-target snapshot override; normal MT operation discovers tenant state paths from `SELF_HEAL_TENANTS_FILE` |
| `HEALTH_LUNARVISION_ENABLED` | health-lunarvision.sh | `true` | Non-`true` disables (reports healthy/disabled) |
| `HEALTH_LUNARVISION_URL` | health-lunarvision.sh | _(unset)_ → each registry tenant's external HTTPS origin or local `vision_health` port, else `http://127.0.0.1:8089` | Optional single-URL override targeting a listener that serves `/health` and `/vision/metrics`. On multi-tenant hosts leave unset so mixed local/external targets are discovered from the ports registry. |
| `HEALTH_LUNARVISION_TIMEOUT` | health-lunarvision.sh | `5` | curl timeout (seconds) for `/health` and `/vision/metrics` |
| `HEALTH_LUNARVISION_FETCH_METRICS` | health-lunarvision.sh | `true` | Also probe `/vision/metrics` for request counts, cache stats, rate limiting |
| `HEALTH_LUNARVISION_REQUIRE_VL` | health-lunarvision.sh | `false` | Degrade if the VL backend is not confirmed available |
| `HEALTH_LUNARVISION_LATENCY_DEGRADED_MS` | health-lunarvision.sh | `2000` | Health-endpoint latency threshold for `degraded` |
| `HEALTH_LUNARVISION_LATENCY_CRITICAL_MS` | health-lunarvision.sh | `5000` | Health-endpoint latency threshold for `critical` |
| `HEALTH_LUNARVOICE_ENABLED` | health-lunarvoice.sh | `auto` | Auto-probes every enabled registry tenant or an explicit URL; `true` forces single-node 8091 fallback when no registry exists; other values disable |
| `HEALTH_LUNARVOICE_URL` | health-lunarvoice.sh | _(unset)_ → enabled registry `voice_health` ports, else `http://127.0.0.1:8091` | Optional single-node override; leave unset on multi-tenant hosts so no tenant is skipped |
| `HEALTH_LUNARVOICE_TIMEOUT` | health-lunarvoice.sh | `5` | Per-request curl timeout for `/health`, `/ready`, and `/metrics` |
| `HEALTH_LUNARVOICE_FETCH_METRICS` | health-lunarvoice.sh | `true` | Fetch bounded operational counters from `/metrics`; disabling this does not bypass liveness/readiness |

LunarVoice health is strict: an enabled instance must return `status=ok` from
`/health` and HTTP 200 with `ready=true` from `/ready`. Malformed tenant JSON,
non-boolean `enable_voice`, missing or duplicate enabled `voice_health` ports,
an unreachable instance, or an unready STT model makes the component critical.
All enabled instances remain visible in `metrics.instances`, so one healthy
tenant cannot mask a failing peer. LLM stream integrity is also monitor-only:
recent retry/abort/corruption counters should trigger provider/TensorMoon
investigation, not an automatic tenant restart. LunarVoice is monitor-only in self-heal; the
pipeline alerts and escalates but does not automatically restart optional model
workloads.
| `HEALTH_NETBIRD_ENABLED` | health-netbird.sh | `false` | Opt in to host NetBird readiness monitoring |
| `HEALTH_NETBIRD_TIMEOUT` | health-netbird.sh | `5` | Readiness probe timeout (seconds) |
| `NETBIRD_BINARY` | health-netbird.sh | `netbird` | NetBird CLI name or path |
| `OPENROUTER_API_KEY` | health-models.sh | (empty) | OpenRouter key; unset → provider `unknown` (degraded) |
| `OPENAI_API_KEY` | health-models.sh | (empty) | OpenAI key; unset → provider `unknown` (degraded) |
| `ANTHROPIC_API_KEY` | health-models.sh | (empty) | Anthropic key; unset → provider `unknown` (degraded) |
| `HEALTH_MODELS_LATENCY_DEGRADED_MS` / `..._CRITICAL_MS` | health-models.sh | `3000` / `10000` | Per-provider latency thresholds |
| `HEALTH_GATEWAY_STALE_LOCK_MINUTES` | health-gateway.sh | `10` | Lock age before a session lock counts as stale |
| `HEALTH_GATEWAY_STALE_LOCKS_CRITICAL` | health-gateway.sh | `3` | More than this many stale locks → critical |
| `HEALTH_GATEWAY_ORPHAN_DEGRADED` / `..._ORPHAN_CRITICAL` | health-gateway.sh | `10` / `50` | Orphan-transcript count thresholds |
| `HEALTH_GATEWAY_OVERSIZED_DEGRADED` | health-gateway.sh | `0` | Oversized (>500 KB) session count before degraded |
| `HEALTH_RUNTIME_ENABLED` | health-runtime.sh | `auto` | `false` disables; `auto` probes registry tenants or the single-node base env |
| `HEALTH_RUNTIME_TIMEOUT` | health-runtime.sh | `5` | Per-request curl timeout for gateway/bridge endpoints; each request is additionally capped by the remaining sweep budget so a slow tenant cannot push the sweep past the driver's outer timeout |
| `HEALTH_RUNTIME_QUEUE_DEGRADED` / `..._QUEUE_CRITICAL` | health-runtime.sh | `10` / `50` | Bridge delivery backlog thresholds |
| `HEALTH_RUNTIME_SWEEP_DEADLINE_SECS` | health-runtime.sh | `22` | Fleet sweep deadline; each probe request is budget-capped against it, past it remaining (and budget-exhausted) targets are marked unprobed and coverage loss degrades (0 disables) |
| `HEALTH_KNOWLEDGE_ENABLED` | health-knowledge.sh | `auto` | `false` disables; `auto` probes registry tenants or the single-node base dir |
| `HEALTH_KNOWLEDGE_SYNC_FAIL_DEGRADED` / `..._CRITICAL` | health-knowledge.sh | `1` / `5` | Consecutive knowledge-sync failure thresholds (enabled sync only) |
| `HEALTH_KNOWLEDGE_SWEEP_OVERDUE_HOURS` | health-knowledge.sh | cadence × 3 (72h at the default 24h cadence) | Absolute sweep-overdue override; by default the budget derives from the tenant's `KNOWLEDGE_SWEEP_CADENCE_HOURS` × `HEALTH_KNOWLEDGE_SWEEP_OVERDUE_MULTIPLIER` |
| `HEALTH_KNOWLEDGE_SWEEP_OVERDUE_MULTIPLIER` | health-knowledge.sh | `3` | Sweep-overdue budget as a multiple of the configured cadence |
| `HEALTH_KNOWLEDGE_SYNC_OVERDUE_MULTIPLIER` | health-knowledge.sh | `3` | Enabled-sync "not attempting" budget as a multiple of `KNOWLEDGE_SYNC_CADENCE_HOURS` |
| `HEALTH_KNOWLEDGE_STARTUP_GRACE_HOURS` | health-knowledge.sh | `24` | Grace window during which an enabled-but-never-observed feature stays healthy (env-file mtime proxies "enabled since"; `0` degrades immediately) |
| `HEALTH_KNOWLEDGE_ENV_FILE` | health-knowledge.sh | tenant env file | Override the enablement/cadence source (defaults to `/home/<user>/lunarwing/env/lunarwing.env`, single-node `$BASE/.env`) |
| `HEALTH_KNOWLEDGE_STATE_ROOT` | health-knowledge.sh | _(unset)_ | Test/override state directory instead of `/home/<user>/lunarwing/state` |
| `HEALTH_XMPP_LATENCY_DEGRADED_MS` / `..._CRITICAL_MS` | health-xmpp.sh | `500` / `2000` | XMPP latency thresholds |
| `HEALTH_XMPP_ERROR_DEGRADED` / `..._ERROR_CRITICAL` | health-xmpp.sh | `1` / `5` | XMPP error-count thresholds |
| `HEALTH_RATELIMIT_THROTTLED_DEGRADED` / `..._CRITICAL` | health-ratelimit.sh | `10` / `50` | Throttled events in the last hour |
| `HEALTH_RATELIMIT_QUEUE_DEGRADED` | health-ratelimit.sh | `100` | Queued messages before degraded |
| `HEALTH_CLICKHOUSE_{DISK,MEMORY}_{DEGRADED,CRITICAL}` | health-clickhouse.sh | `70`/`85`, `80`/`95` | Disk and memory utilization thresholds (percent) |
| `HEALTH_CLICKHOUSE_QUERY_{DEGRADED,CRITICAL}_MS` | health-clickhouse.sh | `1000` / `5000` | Query latency thresholds |
| `HEALTH_TENSORMOON_P95_{DEGRADED,CRITICAL}_MS` | health-tensormoon.sh | `5000` / `15000` | Inference p95 latency thresholds |
| `HEALTH_TENSORMOON_ERROR_{DEGRADED,CRITICAL}_PCT` | health-tensormoon.sh | `1` / `5` | Error-rate thresholds (percent; floats allowed) |
| `HEALTH_TENSORMOON_QUEUE_{DEGRADED,CRITICAL}` | health-tensormoon.sh | `5` / `20` | Queue-depth thresholds |
| `HEALTH_TENSORMOON_GPU_UTIL_WARN_PCT` | health-tensormoon.sh | `90` | Advisory high-GPU-utilization issue (never critical) |
| `HEALTH_TENSORMOON_CLICKHOUSE_DB` | health-tensormoon.sh | `tensormoon` | ClickHouse database holding the gateway's `ModelInference` table |
| `TENSORMOON_SERVICE` | lunarwing-self-heal.sh | `tensormoon-gateway` | Unit the `tensormoon` component restarts (`tensorzero-gateway` on hosts still running the pre-fork unit) |
| `UNITS` | health-systemd.sh | `lunarwing.service xmpp-bridge.service tensormoon-gateway.service` | Override the base systemd units to probe |
| `SERVICES` | health-openrc.sh | auto-discovered | Override the OpenRC services to probe (space-separated) |
| `INITD_DIR` | health-openrc.sh | `/etc/init.d` | Init-script directory scanned for tenant services |
| `HEALTH_OPENRC_STATUS_TIMEOUT` | health-openrc.sh | `8` | Per-unit `rc-service status` timeout (s); `0` disables |
| `HEALTH_OPENRC_SWEEP_DEADLINE_SECS` | health-openrc.sh | `22` | Fleet sweep deadline; past it, remaining services are emitted as unprobed/skipped and the component degrades for lost coverage (0 disables) |
| `HEALTH_LUNARVISION_SWEEP_DEADLINE_SECS` | health-lunarvision.sh | `22` | Fleet sweep deadline for tenant targets (same partial-results contract) |
| `HEALTH_LUNARVOICE_SWEEP_DEADLINE_SECS` | health-lunarvoice.sh | `22` | Fleet sweep deadline for tenant targets (same partial-results contract) |
| `HEALTH_CHECK_TIMEOUT_SECS` | infrastructure-health-check.sh | `30` | Per-component outer timeout; a component killed after emitting valid JSON keeps its partial report |
| `LUNARWING_HEALTH_PATH` | cron-wrapper.sh | _(unset)_ | Appended to the safe PATH (which includes `/usr/local/sbin:/usr/sbin` for admin tools) |
| `HEALTH_OPENRC_CRON_DIRS` | health-openrc.sh | `/etc/cron.hourly /etc/periodic/hourly` | Cron hook dirs scanned for the watchdog hook (scheduler blind-spot check) |
| `AGENTS` | health-launchd.sh | auto-discovered | Override the launchd labels to probe |

> `$BASE` above is `${LUNARWING_BASE_DIR:-${IRONCLAW_BASE_DIR:-$HOME/.lunarwing}}`.

The full self-heal `SELF_HEAL_*` knobs are tabulated under
[Self-Healing → Tuning](#tuning-flags--env-vars).

### Disabling individual checks

On multi-tenant or specialised hosts a base-level probe can be a false positive
(no shared XMPP server, no host-level OMEMO store, no external provider keys).
Each of these reports `healthy` with `metrics.enabled = false` instead of a
spurious `critical`:

```bash
HEALTH_XMPP_SERVER=''        # skip the XMPP reachability probe
HEALTH_OMEMO_ENABLED=false   # skip the OMEMO store probe
HEALTH_MODELS_ENABLED=false  # skip the LLM-provider probe
HEALTH_LUNARVISION_ENABLED=false  # skip the OCR/VL sidecar probe
HEALTH_NETBIRD_ENABLED=false      # default; opt in only on NetBird hosts
HEALTHCHECK_NOTIFY=false     # don't page on every per-run degrade (let self-heal escalations page instead)
```

**Multi-tenant LunarVision:** do **not** hardcode a single target on MT hosts.
Local sidecars expose their dedicated operational listener through registry
`vision_health` ports (e.g. lunarium → `20006`); tenants in external mode carry
their HTTPS health/API origin under `vision.external_url`. Leave the URL unset
and set `SELF_HEAL_TENANTS_FILE` (mt-admin writes `/etc/lunarwing/ports.json`) so
`health-lunarvision.sh` discovers every local and external tenant. External
plain-HTTP targets are rejected before curl. Without registry targets, the
single-node fallback is `http://127.0.0.1:8089`; port `8088` is the API listener
and retains `/health` only as a pre-v10 compatibility alias. Only set
`HEALTH_LUNARVISION_URL` when you intentionally force one override.

Put these in a shared env file (e.g. `/etc/lunarwing/health.env`) loaded by the
timer/cron unit so every run picks them up.

## Report Output

Reports are saved to `$LUNARWING_BASE_DIR/workspace/reports/health/`:

- `YYYY-MM-DDTHH:MM:SSZ.json` — full machine-readable report
- `YYYY-MM-DDTHH:MM:SSZ-summary.md` — human-readable summary
- `health.log` — appended run log (also echoed to stderr)

Both `.json` and `-summary.md` files are rotated after 7 days
(`find -mtime +7 -delete`). Report filenames are ISO-8601, so lexical sort is
chronological — self-heal picks the newest by `sort | tail -1`.

Report shape:

```json
{
  "timestamp": "2026-06-16T12:00:00Z",
  "overall_status": "degraded",
  "components": [ { "component": "gateway", "status": "healthy", "duration_seconds": 2, "metrics": {…} }, … ],
  "alerts":     [ { "component": "xmpp", "severity": "warning", "message": "xmpp is degraded" } ]
}
```

`overall_status` is the worst component status; a component reporting `unknown`
(timeout, crash, invalid JSON) degrades the overall result. Each component
object carries the orchestrator-measured wall `duration_seconds` (additive;
older consumers ignore it) — this powers
`lunarwing_health_check_duration_seconds` in the metrics exporter.

## Metrics Exporter (Prometheus)

`health-metrics-exporter.sh` turns the newest report into Prometheus text
exposition format and atomically writes `metrics.prom` (mode 0644 — the
node_exporter textfile-collector convention; series are non-secret numeric
leaves only, and the optional listener reads the file as an unprivileged
user) next to the reports. `cron-wrapper.sh` runs it after every health check, gated on
`HEALTH_METRICS_EXPORT_ENABLED` (default `true`) — no listener and no port by
default. Point node_exporter/VictoriaMetrics at it:

```bash
node_exporter --collector.textfile.directory=$LUNARWING_BASE_DIR/workspace/reports/health
```

Exported series (`lunarwing_health_*`):

| Family | Meaning |
|--------|---------|
| `overall_status`, `status{component}` | 0 healthy · 1 degraded · 2 critical · 3 unknown · 4 skipped |
| `report_timestamp_seconds`, `report_age_seconds`, `alerts_total` | staleness guard — alert when the report itself stops. `report_age_seconds` is a **frozen export-time snapshot** (the optional listener re-derives it at serve time); alert rules must derive age at evaluation time (`time() - report_timestamp_seconds`, as the shipped `LunarWingHealthReportStale` rule does) so a stopped exporter serving an unchanged textfile still trips the alert |
| `component_enabled{component}` | disabled components export `0` and **no** status series (off ≠ failing) |
| `check_duration_seconds{component}` | per-check wall time |
| `<component>_<key>` | every numeric/boolean leaf of `metrics` flattened (booleans 0/1; strings dropped, so no secret material can leak); nested objects keep their path (`cache_hit_rate`) |
| `lunarvision_*{tenant=…}`, `lunarvoice_*{tenant=…}` | per-tenant instance series from the report's own tenant labels |
| `unit_status/unit_up/unit_restarts_total{init,unit[,tenant]}` | init-agnostic (systemd/openrc/launchd) unit series; `unit_status` uses the same 0–4 code (4 = intentionally skipped/unprobed, never a failure) |
| `selfheal_*{service}` | restarts, escalated, consecutive_unhealthy, flapping window, backoff next-attempt (absent state file → series omitted) |
| `early_warning{metric,severity}` | threshold crossings (below) |

Early-warning layer: copy `health-metrics-thresholds.conf.example` to
`/etc/lunarwing/health-metrics-thresholds.conf` and set
`THRESHOLD_<metric>="warn:>N crit:>M"` rules. Crossings emit the
`early_warning` series and — with `HEALTH_METRICS_EARLY_WARNING_NOTIFY=true` —
a Gotify warning via `send-notification.sh`, deduplicated per metric+severity
by `HEALTH_METRICS_NOTIFY_COOLDOWN` (defaults to `SELF_HEAL_ESCALATE_COOLDOWN`).
This is deliberately separate from the per-run degraded notify.

Optional loopback listener (Phase 2): for hosts without a textfile collector,
`ic/crates/lunarwing_health_metrics` serves the same exposition on
`GET /metrics`, loopback-only (non-loopback binds refused at startup,
non-loopback peers dropped), optional bearer token via `HEALTH_METRICS_TOKEN`
(constant-time compare). Build + install:

```bash
cd ic && cargo build --release --bin lunarwing-health-metrics
install -Dm0755 target/release/lunarwing-health-metrics \
    /usr/local/lib/lunarwing-health/lunarwing-health-metrics
sudo ic/scripts/install-health-metrics-listener.sh   # auto-detects systemd/OpenRC/launchd
```

On multi-tenant hosts the listener port comes from the ports registry v16
`health_metrics` slot (`ic/scripts/migrate-ports-v16.sh`; default 50000) —
never hardcode a tenant port. Tenant-scoped listeners are not planned; one
host-global exporter carries per-tenant labels.

`contrib/` ships a Prometheus alerting-rules YAML and a Grafana dashboard JSON
covering the three canonical early warnings (rising latency, rising error
rate, report staleness) plus unit/self-heal alerting.

## Self-Healing

`lunarwing-self-heal.sh` (v2.1.0) reads the latest health report and:

1. **Maps** unhealthy logical components to base service names
   (`gateway → lunarwing`, …) and discovers per-tenant init sub-units from the
   service-manager report; resolves per-tenant systemd **user** units to the
   owning OS user via the tenant registry.
2. **Grace period** — waits for N consecutive unhealthy observations before the
   first restart (absorbs transient blips).
3. **Restarts**, then **verifies** recovery by re-running the component's own
   `health-*.sh` and parsing `.status` (falls back to `is-active`).
4. **Backs off** between failed attempts with exponential delay + full jitter
   (`linear` kill-switch available), scheduling the next attempt *across* ticks
   rather than sleeping in-run.
5. **Flapping guard** — a service restarted too many times inside the window is
   escalated, not looped.
6. **Escalates** via Gotify (bounded by a timeout) after max retries or on
   flapping. Confirmed pages use a collision-resistant per-service cooldown;
   failed, disabled, or cooldown-deferred delivery remains pending and retries
   on later ticks.
7. **Tracks state** in `$LUNARWING_BASE_DIR/workspace/reports/self-heal/state.json`,
   clears any service the latest report calls healthy ("report as truth"), and
   auto-prunes stale, non-escalated entries.

A single-instance `flock` guard (fd 200) prevents overlapping runs; the lock fd
is closed across restarts so a long-lived `supervise-daemon` can't inherit and
wedge it.

### Component → service mapping

| Component | Base service (single-instance) | Verify script |
|-----------|--------------------------------|---------------|
| gateway | `lunarwing` | health-gateway.sh |
| xmpp | `xmpp-bridge` | health-xmpp.sh |
| tensormoon | `tensormoon-gateway` | health-tensormoon.sh |
| clickhouse | `clickhouse-server` | health-clickhouse.sh |
| lunarvision | `ocr-sidecar` | health-lunarvision.sh |
| lunarvoice | `voice-sidecar` (monitor-only) | health-lunarvoice.sh |
| netbird | `netbird` (monitor-only unless explicitly delegated) | health-netbird.sh |
| runtime | _(none — monitor-only diagnostic)_ per-tenant synthetic identity `runtime@<tenant>`; escalates with tenant attribution, never restarts | health-runtime.sh |
| knowledge | _(none — monitor-only diagnostic)_ per-tenant synthetic identity `knowledge@<tenant>`; escalates with tenant attribution, never restarts | health-knowledge.sh |
| omemo, ratelimit, models | _(none — feature/external, never remediated)_ | — |

`runtime`/`knowledge` failures become synthetic monitor-only targets: a failing
tenant instance escalates as `runtime@<tenant>` / `knowledge@<tenant>` (bare
`runtime` / `knowledge` when a component-level failure has no failing
instance, e.g. an invalid registry or an incomplete sweep). These identities
grace-track and page like any monitor-only component — including under the
standard MT configuration (`SELF_HEAL_REMEDY_LOGICAL=false`) — and a healthy
instance report clears their state through the report-as-truth path. Unprobed
instances (coverage notes) never page per tenant.

Logical mappings use configured base names. Init sub-units retain names reported
by service-manager checks (including `.service`); per-tenant systemd units are
remediated as tenant user.

```bash
# Test without restarting anything or changing state.json
./lunarwing-self-heal.sh --dry-run

# Tuning via flags
./lunarwing-self-heal.sh --max-retries 5 --grace-checks 2 \
    --backoff-strategy exponential --backoff-base 60 --backoff-max 3600 \
    --prune-ttl 86400 --verify-health true

# Restore the old fixed-delay behavior
./lunarwing-self-heal.sh --backoff-strategy linear --backoff-base 30

# Point at a specific report
./lunarwing-self-heal.sh --report /path/to/report.json
```

Self-heal exits `0` in normal operation (including lock contention and
"nothing to do"); it exits `1` only on a fatal error (no report found, report
is not valid JSON).

### Tuning (flags / env vars)

| Flag | Env var | Default | Description |
|------|---------|---------|-------------|
| `--max-retries N` | `SELF_HEAL_MAX_RETRIES` | 3 | Restart attempts before escalation |
| `--backoff N` | `SELF_HEAL_BACKOFF_SECONDS` | 5 | Fixed in-run settle wait after a restart, before verifying |
| `--backoff-base N` | `SELF_HEAL_BACKOFF_BASE` | 60 | Backoff base delay (1st retry) |
| `--backoff-max N` | `SELF_HEAL_BACKOFF_MAX` | 3600 | Backoff ceiling |
| `--backoff-strategy S` | `SELF_HEAL_BACKOFF_STRATEGY` | exponential | `exponential` (full jitter) or `linear` (returns base, no jitter) |
| `--grace-checks N` | `SELF_HEAL_GRACE_CHECKS` | 2 | Consecutive unhealthy observations before the first restart |
| `--prune-ttl S` | `SELF_HEAL_STATE_PRUNE_TTL` | 86400 | Prune non-escalated entries older than this; `0` (or `false`/`no`/`off`) disables |
| `--verify-health B` | `SELF_HEAL_VERIFY_HEALTH` | true | Re-run the component health check after a restart (`false` → `is-active` only) |
| `--report PATH` | — | latest | Act on a specific report instead of the newest in the report dir |
| `--dry-run` / `-n` | — | off | Log intended actions; never restart, notify, quarantine corrupt state, or persist state changes |
| — | `SELF_HEAL_FLAP_MAX_RESTARTS` | 5 | Restarts within the window that mark a service flapping |
| — | `SELF_HEAL_FLAP_WINDOW_SECS` | 3600 | Flapping detection window (seconds) |
| — | `SELF_HEAL_HISTORY_MAX` | 20 | Cap on `restart_history[]` length (must be ≥ flap-max) |
| — | `SELF_HEAL_ESCALATE_TIMEOUT` | 30 | Timeout (s) around the notifier so a hung Gotify can't stall a tick |
| — | `SELF_HEAL_ESCALATE_COOLDOWN` | 3600 | Per-service cooldown after confirmed page delivery; `0`/`false` disables |
| — | `SELF_HEAL_NOTIFY_SCRIPT` | bundled Gotify notifier | Executable custom notifier; exit 0 confirms delivery |
| — | `SELF_HEAL_MAX_REPORT_AGE` | 0 | Refuse to act on a report older than this many seconds; `0` disables |
| — | `SELF_HEAL_REMEDY_LOGICAL` | true | `false` → remediate **only** discovered init sub-units, not logical base services (MT hosts) |
| — | `SELF_HEAL_SYSTEMD_SCOPE` | system | Bus for non-tenant LunarWing base services: `system` or `user`; host NetBird daemon stays on system bus |
| — | `SELF_HEAL_PROBE_TIMEOUT` | 15 | Per-call timeout (s) for init probes/remediations (tenant user-bus restarts/probes AND OpenRC `rc-service` status/restart); a wedged tenant manager or hung supervise-daemon can't stall the tick (needs `timeout(1)`). `SELF_HEAL_SYSTEMD_PROBE_TIMEOUT` is a legacy alias |
| — | `SELF_HEAL_NETBIRD_REMEDIATE` | false | Allow NetBird daemon restart; otherwise NetBird failure is escalation-only |
| — | `SELF_HEAL_STATE_DIR` | `<reports>/../self-heal` | State + lock + escalation directory |
| — | `SELF_HEAL_LOG` | `<state-dir>/actions.log` | Action log path |
| — | `SELF_HEAL_HEALTH_CHECK_DIR` | script dir | Where the `health-*.sh` verify scripts live |
| — | `SELF_HEAL_TENANTS_FILE` | see below | Multi-tenant registry for per-tenant remediation |

`SELF_HEAL_TENANTS_FILE` defaults to `$LUNARWING_BASE_DIR/tenants/ports.json`
when that file exists, otherwise `/etc/lunarwing/ports.json`. (The data-volume
path is preferred because on some platforms — e.g. Umbrel — `/etc/` is not
persistent across app updates.)

The self-heal state directory holds `state.json`, `actions.log`,
`self-heal.lock`, per-service `.last_escalation_page.*` cooldown sidecars,
transient `escalation-*.json.*`, and (on corruption) a single
`state.json.corrupt` forensic copy.

> **Backoff is a cross-tick gate**, not an in-run sleep: a failed service's
> `next_attempt_at` is pushed out by `compute_backoff`, so the run never blocks
> and a flapping service is retried less often. The grace period is counted in
> *observations* (self-heal runs once per health-check tick).

> **Multi-tenant:** run as root with the registry present and self-heal
> remediates per-tenant units — `rc-service lunarwing-<tenant>` on OpenRC, and
> `sudo -n -u <user> … systemctl --user restart lunarwing-<tenant>.service` for
> systemd user units. `health-systemd.sh` discovers those per-tenant user units
> (and probes `podman inspect` health for container units), and
> `health-openrc.sh` auto-discovers tenant init scripts under `INITD_DIR`. On MT
> hosts set `SELF_HEAL_REMEDY_LOGICAL=false` so the watchdog doesn't try to
> restart non-existent base services.

## Cron / Timer Setup

### systemd (recommended for Linux)

Run `cron-wrapper.sh` on a user-level timer (adjust path). Explicitly select
user bus so base-service remediation targets units owned by timer user:

```bash
DIR="$(pwd)"   # run from inside ic-infrastructure-health-check/
mkdir -p ~/.config/systemd/user

cat > ~/.config/systemd/user/lunarwing-health-check.service <<EOF
[Unit]
Description=LunarWing infrastructure health check
[Service]
Type=oneshot
Environment=SELF_HEAL_SYSTEMD_SCOPE=user
ExecStart=$DIR/cron-wrapper.sh
EOF

cat > ~/.config/systemd/user/lunarwing-health-check.timer <<EOF
[Unit]
Description=Run LunarWing health check every 30 min
[Timer]
OnBootSec=5min
OnUnitActiveSec=30min
Persistent=true
[Install]
WantedBy=timers.target
EOF

systemctl --user daemon-reload
systemctl --user enable --now lunarwing-health-check.timer
systemctl --user list-timers lunarwing-health-check.timer
```

> The separate **service-level** watchdog that restarts `lunarwing.service`
> itself lives in the main repo: `ic/scripts/install-lunarwing-watchdog.sh`
> (auto-detects systemd/OpenRC/launchd). This directory's self-heal watchdog
> operates at the component/tenant level on top of that.

### crontab (fallback)

```bash
# Run every 30 minutes
*/30 * * * * SELF_HEAL_SYSTEMD_SCOPE=user /path/to/ic-infrastructure-health-check/cron-wrapper.sh
```

### launchd (macOS)

Create a plist in `~/Library/LaunchAgents/` whose `ProgramArguments` invoke
`cron-wrapper.sh`, with a `StartInterval` of 1800 (30 min).

## Exit Codes

`infrastructure-health-check.sh` (and each `health-*.sh`):

| Code | Meaning |
|------|---------|
| 0 | All components healthy |
| 1 | One or more components degraded (or `unknown`) |
| 2 | One or more components critical |

## Testing & Chaos Suite

The self-heal watchdog and the OpenRC probe have a test suite under `tests/`
(see `docs/proposals/CHAOS_ENGINEERING_TEST_PLAN.md` for the full matrix):

```bash
cd ic-infrastructure-health-check
bash tests/run-all.sh                       # all registered suites
bash tests/run-all.sh matrix chaos          # pick a subset
bash tests/run-all.sh --list                # print suite names for CI introspection
bash tests/test-self-heal-matrix.sh         # synthetic report matrix (sections A–O)
bash tests/chaos-harness.sh                 # end-to-end fault-injection (13 scenarios: CH1–CH6, CH9–CH15)
bash tests/test-health-openrc.sh            # health-openrc.sh probe scenarios
bash tests/test-lunarvision-init-templates.sh # systemd/OpenRC template contracts
bash tests/test-health-netbird.sh           # opt-in readiness probe scenarios
bash tests/test-health-lunarvoice.sh        # registry-aware voice readiness scenarios
```

| Suite | Script | What it does |
|-------|--------|--------------|
| `regression` | `test-self-heal.sh` | Original regression checks (jq precedence, backoff, grace, flapping, …). |
| `matrix` | `test-self-heal-matrix.sh` | Full A–O synthetic-report matrix: dry-run routing plus live state transitions against mock init (152 assertions). |
| `chaos` | `chaos-harness.sh` | Drives the **real** self-heal loop (kill → restart → verify → recover/escalate) against a mock init system. |
| `openrc` | `test-health-openrc.sh` | Exercises `health-openrc.sh` (discovery dedup, real exit-code capture, per-unit timeout, missing/stopped units, cron-scheduler blind-spot check). |
| `systemd` | `test-health-systemd.sh` | Exercises tenant user-bus discovery, failure bias, and container health including LunarVision. |
| `lunarvision` | `test-health-lunarvision.sh` | Exercises `health-lunarvision.sh` (mock curl, 83 assertions: reachability, JSON, dedicated/legacy registry discovery, VL and metrics behavior). |
| `init-templates` | `test-lunarvision-init-templates.sh` | Parses both systemd templates and executes the standalone OpenRC lifecycle against mocks (26 assertions). |
| `lunarvoice` | `test-health-lunarvoice.sh` | Exercises disabled mode, STT readiness, metrics, unreachable service, and enabled-only ports-registry discovery. |
| `voice-init-templates` | `test-lunarvoice-init-templates.sh` | Validates the standalone LunarVoice systemd unit, Compose manifest, and executes the OpenRC lifecycle against mocks. |
| `netbird` | `test-health-netbird.sh` | Exercises disabled, ready, unready, missing-binary, and timeout behavior against mock CLI. |
| `llm-streams` | `test-health-llm-streams.sh` | Exercises the daemon-snapshot and MT-fleet llm_streams monitor. |
| `metrics-exporter` | `test-health-metrics-exporter.sh` | Exercises exposition generation, thresholds, notify dedup, atomic writes, and the scrape-time staleness contract. |
| `cron-wrapper` | `test-cron-wrapper.sh` | Exercises the scheduled launcher (PATH, sequencing). |
| `runtime` | `test-health-runtime.sh` | Exercises `health-runtime.sh` (mock curl: discovery, reachability, backlog thresholds, channel health, pending-gates observability, budget-bounded sweeps). |
| `knowledge` | `test-health-knowledge.sh` | Exercises `health-knowledge.sh` (enablement-aware sync/sweep classification, grace window, cadence-derived overdue budgets). |
| `gateway` | `test-health-gateway.sh` | Exercises `health-gateway.sh` (absent legacy dir stays healthy; stale locks and oversized sessions still degrade). |
| `watchdog-openrc` | `test-watchdog-openrc.sh` | Exercises the OpenRC service-level watchdog (start/verify, deep-check guards, bounded rc-service probes, lock contention) against a mock rc-service. |
| — | `lib.sh` | Shared harness + the mock init system (sourced, not run directly). |

`run-all.sh` runs each suite in its own process, tallies per-suite
`N passed, M failed`, and exits non-zero if any suite fails.

**Safety.** Matrix routing cases use dry-run; state-transition cases and chaos
run live only against a *mock* `systemctl`/`rc-service`/`sudo` shadowed onto
`PATH`, so a
"restart" flips a sandbox file — no live unit is touched — and escalation runs
a sandbox notifier, so it never hits the network. OpenRC and service-manager
probe suites use their own mocks. Prefer a dedicated test box over a live
multi-tenant host regardless.

The pure-function harness (`src_fn` in `lib.sh`) sources everything up to the
`# HARNESS_ENTRY_POINT` sentinel in `lunarwing-self-heal.sh` to unit-test
functions like `compute_backoff` in isolation; if you move that sentinel, the
harness fails loudly with a pointer to fix it.

Requirements: `bash` + `jq` (everywhere), and `flock` for the locking test
(skipped gracefully if absent). The OpenRC and per-unit-timeout tests skip
gracefully when `timeout` is unavailable.

## TensorMoon rename (was TensorZero)

LunarWing's model gateway moved from the abandoned TensorZero to the
organisation's fork, TensorMoon. In this directory that renamed:

| Before | After |
|--------|-------|
| component `tensorzero`, `health-tensorzero.sh` | component `tensormoon`, `health-tensormoon.sh` |
| metrics `lunarwing_health_tensorzero_*` | `lunarwing_health_tensormoon_*` (update external dashboards; `contrib/` is already updated) |
| `HEALTH_TENSORZERO_*` thresholds | `HEALTH_TENSORMOON_*` (old names still read as fallbacks) |
| self-heal target `tensorzero-gateway` | `tensormoon-gateway` (override with `TENSORMOON_SERVICE`) |
| `health-systemd.sh` default unit `tensorzero-gateway.service` | `tensormoon-gateway.service` (override with `UNITS`) |

`health-tensormoon.sh` now reads p50/p95 latency from TensorMoon's
`ModelInference` ClickHouse table (database `HEALTH_TENSORMOON_CLICKHOUSE_DB`)
when `clickhouse-client` can reach it, and otherwise falls back to the GPU-only
probe as before. Full cutover runbook:
`docs/guides/MIGRATE_TENSORZERO_TO_TENSORMOON.md`.
