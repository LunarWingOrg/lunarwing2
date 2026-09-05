# Healthpoint Metrics Exporter

> **Status (2026-08-31): v2.1.0.0 goal #5 sweep DONE — models metrics + configurable thresholds.**
> The models check now emits a numeric `metrics` object (provider counts,
> `latency_max_ms`, per-provider `provider_latency_ms` / `provider_status`
> maps) instead of string-only series the exporter dropped, so every
> healthpoint contributes export series. Degraded/critical thresholds in the
> six legacy checks (gateway, xmpp, ratelimit, clickhouse, tensorzero, models)
> are env-configurable (`HEALTH_GATEWAY_*` / `HEALTH_XMPP` / `HEALTH_RATELIMIT` /
> `HEALTH_CLICKHOUSE` / `HEALTH_TENSORZERO` / `HEALTH_MODELS` knobs,
> health-netbird-style validation fallback), and
> `health-metrics-thresholds.conf.example` now carries a sample
> early-warning rule for every healthpoint.
>
> **Status (2026-08-15): IMPLEMENTED (v2.0.8.0 goal #16) — Phases 1–3 landed.**
> Phase 1: `health-metrics-exporter.sh` (textfile export of the aggregated
> report + self-heal series), `duration_seconds` in the report, cron-wrapper
> wiring, `tests/test-health-metrics-exporter.sh` (61 assertions) in
> `tests/run-all.sh`, config/threshold examples, and `contrib/` alert rules +
> Grafana dashboard. Phase 2: `ic/crates/lunarwing_health_metrics` loopback
> listener (bearer-token, loopback-enforced), `ic/scripts/install-health-metrics-listener.sh`
> (systemd/OpenRC/launchd), and the ports-registry **v16** migration allocating
> the host-global `health_metrics` slot (`ic/scripts/migrate-ports-v16.sh`).
> Phase 3: threshold conf + `early_warning` series + deduplicated Gotify path.
> Historical origin: PROPOSED 2026-08-13 from Rares' notes at
> `~/.hermes/notes/healthpoint-metrics-exporter.md`, capturing a Discord
> message from cmc / IRC discussion with Christopher, who called a dedicated
> metrics exporter for every healthpoint "the ideal path going forward."
> This document closes the long-open **M4 – Prometheus exporter** milestone
> recorded in
> [`../internal/ic-infrastructure-health-check/draft-ic-infrastructure-health-check-analysis-report.md`](../internal/ic-infrastructure-health-check/draft-ic-infrastructure-health-check-analysis-report.md).

## Overview

The infrastructure health pipeline (`ic-infrastructure-health-check/`) now has
both reactive and proactive layers. Eleven parallel healthpoint scripts
aggregate into a JSON report; self-heal restarts unhealthy services and can page
through Gotify; `health-metrics-exporter.sh` then converts the newest report and
self-heal state into Prometheus text exposition. Configurable threshold rules
emit graduated early-warning series and optional deduplicated Gotify warnings
before a hard failure forces a restart-and-page cycle.

This began as the proposal for a **dedicated metrics exporter for every
healthpoint**. Phases 1–3 are now implemented. The default remains a listenerless
atomic textfile export; the optional Rust listener serves that file from loopback
and can require a bearer token.

Why it was added after v2.0.7.0:

- v2.0.7.0 shipped the sidecar rework — LunarVision (goal #7) and LunarVoice
  (goal #9) both gained dedicated health listeners, launch contracts, and
  cross-init supervision. Each new healthpoint can contribute numeric and
  boolean fields without a new exporter probe because export reads the
  aggregated report.
- The health JSON schema was already stable across the 2.0.x line, making the
  report contract the low-risk integration boundary.
- Third-party integration (Grafana, Alertmanager, VictoriaMetrics, uptime
  dashboards) is part of the stated vision for the health services.

## Goals and non-goals

**Goals**

1. Export every healthpoint's metrics as a Prometheus-compatible time series,
   per component and (on multi-tenant hosts) per tenant.
2. Zero new listeners by default: node_exporter-style **textfile** output as
   the primary delivery mechanism.
3. Optional loopback-only HTTP `/metrics` listener for hosts without
   node_exporter.
4. Configurable per-healthpoint early-warning thresholds with graduated
   (warning → critical) signals, feeding both the exposition output and the
   existing Gotify path (deduplicated).
5. Stay init-agnostic (systemd / OpenRC / launchd) and ports-registry-aware,
   per the multi-tenant hard rules in `AGENTS.md`.

**Non-goals**

- Replacing self-heal, Gotify escalation, or the JSON report contract. The
  exporter is a read-only side-channel on top of them.
- Time-series storage, dashboards, or long-term retention inside LunarWing —
  that is the external TSDB's job (though we ship examples).
- Re-instrumenting the sidecars themselves. LunarVision already serves
  `/vision/metrics` and LunarVoice `/metrics`; `health-lunarvision.sh` /
  `health-lunarvoice.sh` already pull the bounded subset worth exporting.
- LTX instrumentation (tracked separately; the exporter picks it up
  automatically once `health-ltx.sh` exists).

## Current state (verified 2026-08-25)

`infrastructure-health-check.sh` runs these twelve healthpoints in parallel,
each emitting one JSON object (`component`, `status`, `metrics{...}`), exit
0/1/2:

| Healthpoint | Script | Exportable metrics already emitted |
|-------------|--------|------------------------------------|
| gateway | `health-gateway.sh` | `active_sessions`, `stale_locks`, `orphan_transcripts`, `oversized_sessions` |
| xmpp | `health-xmpp.sh` | `connected`, `latency_ms`, `error_count`, `active_sessions` |
| omemo | `health-omemo.sh` | `devices`, `bundles`, `sessions`, `recent_sessions` |
| ratelimit | `health-ratelimit.sh` | `throttled_last_hour`, `queue_depth` |
| clickhouse | `health-clickhouse.sh` | `query_time_ms`, `disk_pct`, `memory_pct` |
| tensorzero | `health-tensorzero.sh` | `p50_ms`, `p95_ms`, `error_rate`, `queue_depth`, `gpu_utilization` |
| models | `health-models.sh` | `provider_count`, `providers_{healthy,degraded,critical}`, `latency_max_ms`, per-provider `provider_latency_ms` / `provider_status` numeric maps (status strings/`last_error` remain in `providers[]` and are dropped on export) |
| llm_streams | `health-llm-streams.sh` | per-provider retry reasons, stream aborts, decode failures, corruption signatures, and time-to-first-chunk counters/timings with `provider` + `tenant` labels |
| lunarvision | `health-lunarvision.sh` | `latency_ms`, cache `hit_rate`, `rate_limited`, `avg_latency_ms`, per-tenant `ocr_available`/`vl_available` |
| lunarvoice | `health-lunarvoice.sh` | per-tenant `ready`, `latency_ms`, request counts, cache hits, errors |
| netbird | `health-netbird.sh` | `enabled`, `ready`, `probe_exit` |
| svcmgr | `health-{systemd,openrc,launchd}.sh` | per-unit/service/agent state (incl. per-tenant user units) |

The orchestrator writes `$LUNARWING_BASE_DIR/workspace/reports/health/<ISO>.json`
(plus a `-summary.md`), rotates both after 7 days, and stamps each component's
measured wall time as `duration_seconds`. `cron-wrapper.sh` runs the exporter
against that fresh report before self-heal. `lunarwing-self-heal.sh` keeps
remediation state in the adjacent `self-heal/state.json` (restart history,
backoff `next_attempt_at`, and escalation flags), which the exporter also reads
when `HEALTH_METRICS_INCLUDE_SELFHEAL=true`.

## Design

### Delivery mechanism: textfile first, listener optional

- **Phase 1 (primary): `health-metrics-exporter.sh`** converts the newest
  health report into Prometheus text exposition format and atomically writes
  (`mktemp` + `mv`, matching the mt-admin write pattern) a `.prom` file for a
  node_exporter/VictoriaMetrics **textfile collector**. No listener, no port,
  no new attack surface — appropriate as the default on every host, including
  multi-tenant ones.
- **Phase 2 (optional): loopback HTTP listener** `lunarwing-health-metrics`
  serving `GET /metrics` on `127.0.0.1` only, for hosts that cannot run a
  textfile collector. Implementation options in order of preference: (a) a
  tiny bash/socat or `python3 -m http.server`-style shim is explicitly
  **rejected** (fragile, no auth); (b) a minimal Rust binary in
  `ic-infrastructure-health-check/` or an `ic/crates/` utility reusing the
  daemon's existing HTTP stack conventions; (c) documenting that operators
  point a stock `node_exporter --collector.textfile.directory` at the `.prom`
  output instead. The listener must take an optional bearer token from the
  health env file (never logged, `secrecy`-style handling in the Rust
  option), must bind loopback only, and on multi-tenant hosts gets its port
  from the ports registry — a **v16 registry migration** allocating a
  dedicated `health_metrics` slot (**landed**: `ports_migrate_v16` in
  `ic/scripts/mt-admin-lib/03-ports-registry.sh:287` plus standalone
  `ic/scripts/migrate-ports-v16.sh`; the registry is now v16).
  Tenant-scoped listeners are **not** planned: one host-global exporter
  carries per-tenant labels.

### Metric schema

Naming: `lunarwing_health_*`, snake_case, units suffixed per Prometheus
convention (`_seconds`, `_ms` kept where the source field is already `_ms`).

Core series (every run):

```
# Numeric component status: 0=healthy 1=degraded 2=critical 3=unknown
lunarwing_health_status{component="tensorzero"} 0
lunarwing_health_overall_status 1
# Staleness guard — alert when the report itself stops being produced
lunarwing_health_report_timestamp_seconds 1786615200
lunarwing_health_report_age_seconds 312
```

Per-healthpoint metrics are flattened from each component's `metrics` object:
`lunarwing_health_<component>_<key>` for numeric values; booleans map to 0/1;
strings are dropped (except fixed enumerations, which may become label values
where cardinality is bounded, e.g. `provider` for `models`):

```
lunarwing_health_tensorzero_p95_ms 812
lunarwing_health_tensorzero_error_rate 0.004
lunarwing_health_clickhouse_disk_pct 61
lunarwing_health_lunarvision_cache_hit_rate 0.87
lunarwing_health_xmpp_connected 1
```

Multi-tenant components add a `tenant` label, sourced from the same ports
registry (`SELF_HEAL_TENANTS_FILE` / `/etc/lunarwing/ports.json`) the checks
already read — never from a hardcoded port:

```
lunarwing_health_lunarvision_latency_ms{tenant="lunarium"} 42
lunarwing_health_lunarvoice_ready{tenant="lunarium"} 1
lunarwing_health_unit_up{init="systemd",tenant="lunarium",unit="lunarwing-lunarium.service"} 1
```

Disabled components (`metrics.enabled = false`) export
`lunarwing_health_component_enabled{component="netbird"} 0` and **no** status
series, so external alerting can distinguish "off" from "failing".

Self-heal state (from `state.json`, still Phase 1) gives the proactive layer
its highest-value early signals:

```
lunarwing_health_selfheal_restarts_total{service="tensorzero-gateway"} 4
lunarwing_health_selfheal_backoff_next_attempt_seconds{service="xmpp-bridge"} 1786620000
lunarwing_health_selfheal_escalated{service="clickhouse-server"} 1
lunarwing_health_selfheal_flapping{service="lunarwing"} 0
```

### Early-warning layer (Phase 3)

Thresholds live in a single config file, e.g.
`/etc/lunarwing/health-metrics-thresholds.conf` (sourced shell fragment,
matching `config.sh` conventions), with per-metric warn/crit rules:

```bash
# metric_name  warn_expr  crit_expr   (exprs evaluated against the last value)
THRESHOLD_lunarwing_health_tensorzero_p95_ms="warn:>1500 crit:>4000"
THRESHOLD_lunarwing_health_clickhouse_disk_pct="warn:>75 crit:>90"
THRESHOLD_lunarwing_health_report_age_seconds="crit:>2400"
```

Evaluation happens at export time and produces:

- `lunarwing_health_early_warning{component="tensorzero",metric="p95_ms",severity="warn"} 1`
  series for external alerting, and
- an optional Gotify **warning** notification through the existing
  `send-notification.sh`, rate-limited by the same cooldown discipline as
  self-heal escalations (`SELF_HEAL_ESCALATE_COOLDOWN`) so early warnings
  cannot page-spam. This is deliberately separate from the driver's per-run
  degraded notify (`HEALTHCHECK_NOTIFY`) and does not change its behavior.

The exporter must ship example assets: a Prometheus alerting-rules YAML and a
Grafana dashboard JSON under `ic-infrastructure-health-check/contrib/`
covering the three canonical early warnings: rising latency, rising error
rate, and report staleness.

### Config surface (env, all optional, defaults safe)

| Variable | Default | Description |
|----------|---------|-------------|
| `HEALTH_METRICS_EXPORT_ENABLED` | `true` | Master switch for the textfile export step in `cron-wrapper.sh` |
| `HEALTH_METRICS_OUTPUT` | `<reports>/metrics.prom` | Textfile collector target (atomic tmp+mv) |
| `HEALTH_METRICS_INCLUDE_SELFHEAL` | `true` | Also export self-heal state series |
| `HEALTH_METRICS_LISTEN` | _(empty/off)_ | `127.0.0.1:<port>` for the optional Phase-2 listener |
| `HEALTH_METRICS_TOKEN` | _(empty)_ | Optional bearer token for the listener; empty = no auth on loopback |
| `HEALTH_METRICS_THRESHOLDS_FILE` | `/etc/lunarwing/health-metrics-thresholds.conf` | Phase-3 threshold config |
| `HEALTH_METRICS_EARLY_WARNING_NOTIFY` | `false` | Send deduplicated Gotify warning on threshold crossings |

### Orchestrator/report touch-points (kept minimal)

1. `infrastructure-health-check.sh`: record per-check wall duration into each
   component object (`.duration_seconds`) — it is already measured at
   `infrastructure-health-check.sh:61-75` and currently only logged. This
   powers `lunarwing_health_check_duration_seconds{component=...}`.
2. `cron-wrapper.sh`: invoke `health-metrics-exporter.sh` after the health
   check, gated on `HEALTH_METRICS_EXPORT_ENABLED`. Self-heal still runs
   unconditionally afterward.
3. Report schema stays backward compatible: consumers ignore unknown keys, so
   adding `duration_seconds` is safe.

### Security posture

- Default path adds **no listener and no port**. The optional listener is
  loopback-only with an optional bearer token, and its MT port comes from a
  registry migration — consistent with the `AGENTS.md` rules against
  weakening listeners/auth.
- Metrics must never contain secret material: no env values, no API keys
  (`OPENROUTER_API_KEY` etc. are presence-only today and stay that way — the
  exporter copies only the numeric/boolean fields enumerated by an allowlist,
  never an arbitrary dump of `metrics`).
- The `.prom` file is written `0644` (node_exporter textfile-collector
  convention) — safe because the exposition carries only non-secret
  numeric/boolean series, and required so the optional listener can read it
  as an unprivileged user; nothing tenant-private leaves the host.

### Testing

New suite `ic-infrastructure-health-check/tests/test-health-metrics-exporter.sh`
registered in `tests/run-all.sh`, following the existing mock discipline:

- synthetic report → valid exposition format (parse with a strict
  line-format check; every metric line matches the naming schema);
- status mapping 0/1/2/3 incl. `unknown`;
- disabled components emit `component_enabled 0` and no status series;
- MT labels appear only for tenant-scoped components, sourced from a fixture
  `ports.json`;
- staleness gauge matches the fixture report's timestamp;
- threshold evaluation emits the expected `early_warning` series and respects
  `HEALTH_METRICS_EARLY_WARNING_NOTIFY=false`;
- self-heal series render from a fixture `state.json`, absent state file →
  series omitted, not an error;
- atomic write: no partial `.prom` on simulated failure.

No chaos-harness changes; the exporter never mutates services.

## Rollout plan (post-v2.0.7.0)

1. **Phase 1** — DONE 2026-08-15: `health-metrics-exporter.sh` +
   `duration_seconds` report field + textfile output + self-heal series +
   test suite (61 assertions) + README / `config.sh.example` updates.
2. **Phase 2** — DONE 2026-08-15: optional loopback listener
   (`ic/crates/lunarwing_health_metrics`) + port-registry **v16** migration
   (`health_metrics` slot, host-global, default 50000) + systemd/OpenRC/launchd
   units rendered through `ic/scripts/install-health-metrics-listener.sh`
   following the install-lunarwing-watchdog.sh auto-detect pattern.
3. **Phase 3** — DONE 2026-08-15: threshold config
   (`health-metrics-thresholds.conf.example`) + `early_warning` series +
   deduplicated Gotify warning path (per-metric+severity cooldown stamps,
   `SELF_HEAL_ESCALATE_COOLDOWN` discipline) + `contrib/` alerting rules and
   Grafana dashboard.
4. Roadmap bookkeeping: DONE (2026-08-13) — v2.0.7.0 goals item #28 added the
   health exporter to `docs/ops/ROADMAP_2026.md` (target v2.0.8.0) referencing
   this document; the internal analysis report's M4 milestone flips to Done
   with this implementation.
5. When LTX lands as a healthpoint, verify its metrics appear with correct
   labels — add only a README row, no exporter changes expected.

## Open questions

- Push model for hosts that cannot be scraped (Prometheus Pushgateway /
  remote-write)? Deferred — textfile + optional loopback scrape covers the
  known fleet.
- Should the per-check scripts themselves emit richer metrics (e.g. gateway
  queue latency)? Possible follow-up; exporter schema already tolerates new
  numeric keys without changes.
- Do we want a `lunarwing_health_alert_total` counter derived from the
  report's `alerts[]` array? Cheap to add in Phase 1 if wanted.
- Tenant label cardinality on very large MT fleets: bounded today by tenants
  per host; revisit if a single host ever exceeds ~100 tenants.

## Related

- v2.0.7.0 goals #7 (LunarVision hardening) and #9 (LunarVoice hardening) —
  both shipped with dedicated health listeners this exporter reads
  indirectly.
- [`PODMAN_WAIT_BABYSITTER.md`](PODMAN_WAIT_BABYSITTER.md) and the self-heal
  watchdog docs in `ic-infrastructure-health-check/README.md` for the
  remediation side this proposal complements.
- `docs/internal/ic-infrastructure-health-check/draft-ic-infrastructure-health-check-analysis-report.md`
  — M4 milestone origin.
