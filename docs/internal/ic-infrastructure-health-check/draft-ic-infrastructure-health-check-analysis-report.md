# ic-infrastructure-health-check — Deep Analysis Report (Draft)

> **Updated 2026-08-13** — re-verified against the current subproject code
> (driver, self-heal watchdog v2.1.0, checks, tests) for checklist item #24.
> The original draft described a much older tree (`ironclaw-watchdog.timer`,
> `icservices/`, `icscripts/`, no self-heal, no config, no tests); all of that
> has been corrected below.

## 1. Executive summary
The **ic-infrastructure-health-check** subproject provides a Bash-based orchestration layer that runs health diagnostics across key LunarWing services (gateway, XMPP bridge, ClickHouse, TensorZero, model APIs, LunarVision/LunarVoice sidecars, NetBird tunnel, and the host service manager). Each diagnostic is a self-contained script that emits structured JSON, which the main driver (`infrastructure-health-check.sh`) aggregates, writes a timestamped report, produces a markdown summary, and optionally sends a Gotify notification when the overall status is degraded or critical. A separate watchdog (`lunarwing-self-heal.sh`, v2.1.0) reads the report and auto-remediates unhealthy services with a grace period, exponential backoff with jitter, a flapping guard, post-restart verification, escalation cooldown, and multi-tenant awareness.

The pipeline runs through `cron-wrapper.sh`, which always proceeds to self-heal even if the health check fails. Scheduling is handled by the LunarWing watchdog assets that live in the main repo (`ic/systemd/lunarwing-watchdog.service`/`.timer`/`.confd`/`.cron.hourly`, `ic/systemd/com.lunarwing.watchdog.plist` for launchd), installed via `ic/scripts/install-lunarwing-watchdog.sh`; on multi-tenant hosts the pipeline is installed to `/usr/local/lib/lunarwing-health/` by `ic/scripts/lunarwing-mt-admin.sh`.

Key strengths:
- Simple, dependency-light implementation (pure Bash, `jq`, `curl`; `nc` only for the XMPP check).
- JSON-first output makes downstream processing easy.
- Extensible – adding a new component is a matter of creating a `health-<name>.sh` script and wiring it into the driver.
- Init-agnostic: exactly one of `health-systemd.sh` / `health-openrc.sh` / `health-launchd.sh` runs, auto-detected (or forced via `LUNARWING_SERVICE_MANAGER`).
- Real self-healing with restart verification and escalation instead of alert-only monitoring.

Remaining improvement areas: the check list is still hard-coded in the driver (no auto-discovery), there is no aggregate schema validation beyond "is valid JSON", and no Prometheus exporter.

## 2. Project structure
```
ic-infrastructure-health-check/
├─ README.md                      # High-level design & usage (kept current; last updated 2026-08-08)
├─ infrastructure-health-check.sh # Main driver (11 parallel checks → JSON report)
├─ lunarwing-self-heal.sh         # Watchdog: reads report → restarts → verifies → escalates (v2.1.0)
├─ send-notification.sh           # Gotify wrapper (report + escalation shapes)
├─ cron-wrapper.sh                # Runs health check then self-heal; forwards args (e.g. --dry-run)
├─ config.sh.example              # Optional central env configuration (copy to config.sh / source from env)
├─ init-templates/                # Standalone single-node LunarVision units
│   ├─ ocr-sidecar                # OpenRC script (rootless Podman, supervise-daemon)
│   └─ ocr-sidecar.service        # systemd unit
├─ health-gateway.sh              # Agent sessions: stale locks, orphan transcripts, oversized sessions
├─ health-xmpp.sh                 # XMPP server TCP reachability + latency
├─ health-omemo.sh                # OMEMO key/bundle/session store
├─ health-ratelimit.sh            # Throttle counters, queue depth
├─ health-clickhouse.sh           # ClickHouse query latency & resource usage
├─ health-tensorzero.sh           # TensorZero latency (from ClickHouse), GPU utilisation
├─ health-models.sh               # Model API availability
├─ health-lunarvision.sh          # OCR + VL sidecar /health, /vision/metrics; MT port-registry aware
├─ health-lunarvoice.sh           # Enabled voice sidecars: /health, /ready, /metrics
├─ health-netbird.sh              # Opt-in NetBird tunnel readiness
├─ health-systemd.sh              # systemd unit health (incl. per-tenant user units)
├─ health-openrc.sh               # OpenRC service health
├─ health-launchd.sh              # launchd agent health (macOS)
├─ tests/                         # Self-contained suites, no live services touched
│   ├─ run-all.sh                 # Aggregate runner (10 registered suites)
│   ├─ test-self-heal.sh          # Regression suite
│   ├─ test-self-heal-matrix.sh   # Dry-run remediation matrix
│   ├─ chaos-harness.sh           # Chaos tests against a mock init system
│   ├─ test-health-{openrc,systemd,lunarvision,lunarvoice,netbird}.sh
│   └─ test-{lunarvision,lunarvoice}-init-templates.sh
└─ reports/ (runtime)             # Generated JSON & markdown per run, rotated after 7 days
```
No compiled languages are used – the whole stack is Bash scripts. (The old
`icservices/`, `icscripts/`, and `ironclaw-watchdog.timer` entries no longer
exist; service/timer assets live under `ic/systemd/` in the main repo.)

## 3. Build system & CI/CD
- No compiled artifacts; the project is shipped as-is.
- `tests/run-all.sh` runs every suite (regression, matrix, chaos, per-init and per-sidecar checks, init-template rendering); each suite runs in a throwaway sandbox and never touches live services.
- Multi-tenant hosts get the pipeline installed by `lunarwing-mt-admin.sh`; single-node scheduling uses the watchdog installer in `ic/scripts/`.
- No Dockerfile or package – deployment is a source checkout plus the timer/cron assets above.

## 4. Dependencies
| Category | Tool | Reason |
|----------|------|--------|
| Shell | `bash >=4.0` | Core scripting language (associative arrays, `mapfile`) |
| JSON | `jq` | Validation & extraction of JSON output |
| HTTP | `curl` | Gotify notifications and external API checks |
| Timeout | coreutils `timeout` | Per-check 30 s cap and bounded self-heal probes (degrades gracefully when absent, e.g. macOS) |
| Locking | `flock` | Self-heal single-instance lock |
| TCP | `nc` (netcat) | Only `health-xmpp.sh` port reachability |
| DB | `clickhouse-client` (optional) | ClickHouse diagnostics and TensorZero latency queries |
| GPU | `nvidia-smi` / `rocm-smi` (optional) | TensorZero GPU utilisation |
| Containers | `sudo` + `podman` (MT hosts) | Per-tenant user units and sidecar container health |
| Init | `systemctl` / `rc-service` / `launchctl` | Exactly one, matching the detected service manager |
| Logging | Stdout/stderr + log file at `$REPORT_DIR/health.log` | Reports under `<base>/workspace/reports/health/` |

No external language runtimes (Python, Rust, Go) are required. (`bc` was
previously needed; float comparisons now use `awk`.)

## 5. Core components & runtime flow
1. **`infrastructure-health-check.sh`** (driver)
   - Resolves paths from `LUNARWING_BASE_DIR` (legacy alias `IRONCLAW_BASE_DIR`, default `$HOME/.lunarwing`).
   - `run_check` executes a health script with a 30 s timeout, validates JSON, logs timing, and returns the exit code.
   - Detects the service manager (`LUNARWING_SERVICE_MANAGER` override → Darwin → `/run/openrc/softlevel` → `/run/systemd/system` → binary fallback) and runs exactly one init-specific check.
   - Launches all eleven checks in parallel into a private `mktemp -d` workdir (no fixed `/tmp/check-*.tmp` paths, so concurrent tenants cannot collide; the earlier duplicated-launch bug is gone).
   - Aggregates into `$REPORT_DIR/<timestamp>.json` plus a `-summary.md`, rotates reports older than 7 days, and exits 0/1/2 (healthy/degraded/critical).
   - Sends a Gotify notification when not healthy, unless `HEALTHCHECK_NOTIFY=false` (e.g. MT hosts where only self-heal escalations should page).
2. **`lunarwing-self-heal.sh`** (watchdog)
   - Reads the latest report; per unhealthy service: grace period (`SELF_HEAL_GRACE_CHECKS`, default 2 consecutive failures) → bounded restarts (`SELF_HEAL_MAX_RETRIES`, default 3) → post-restart verification via the component checks → escalation.
   - Exponential backoff with jitter across cron ticks via persisted `next_attempt_at` state (linear kill-switch available), flapping guard (default 5 restarts/hour), restart-history cap, state auto-prune, per-service escalation cooldown, and a 30 s notifier timeout.
   - Init-agnostic like the driver (also honors the legacy `IRONCLAW_SERVICE_MANAGER` alias); multi-tenant remediation goes through `lunarwing-mt-admin.sh`, never bare system-bus `systemctl`.
   - `--dry-run` exercises the whole pipeline without restarting anything.
3. **Individual `health-*.sh` scripts** – same contract: JSON object with `component`, `status`, `timestamp`, `metrics`, `issues`; exit 0/1/2.
4. **`send-notification.sh`** – posts to Gotify when `GOTIFY_TOKEN` is set; handles both the full report shape and the self-heal escalation shape (null-defaulted so malformed input can never abort before `curl`).

## 6. Monitoring & observability capabilities
- **JSON reports** under `<base>/workspace/reports/health/` – easy to ingest into Grafana Loki, Prometheus exporters, or custom dashboards; 7-day rotation built in.
- **Markdown summaries** for quick console inspection.
- **Gotify integration** for both health degradation and self-heal escalations.
- **Exit codes** enable downstream orchestration.
- **Watchdog timer/cron** guarantees regular execution without manual cron management on all three supported inits.

Remaining observability gaps:
- No Prometheus metrics endpoint – health data is still file-based only.
- No structured log aggregation (per-run log appended to `health.log`, not exported).
- Alert spam is mitigated (grace period, flap guard, escalation cooldown, `HEALTHCHECK_NOTIFY=false`) but there is no dedupe on the driver-side degraded/critical notification itself.

## 7. Extensibility assessment
| Aspect | Current state | Suggested improvement |
|--------|----------------|-----------------------|
| **Adding a new component** | Create `health-<name>.sh` that obeys the JSON contract and add a `run_check` line plus aggregation entry in the driver. | Driver could iterate over `health-*.sh` instead of a hard-coded list (still not auto-discovered). |
| **Configuration** | Optional central env file: `config.sh.example` documents every `HEALTH_*`/`SELF_HEAL_*` variable; copy to `config.sh` and source from the environment/systemd unit. Sane defaults throughout. | None outstanding — done (M2). |
| **Parallel execution** | All checks run in parallel in a per-run `mktemp -d` workdir; the old duplicated `run_check` lines are fixed. | None outstanding — done (M1). |
| **Testing** | `tests/` with 10 self-contained suites (`run-all.sh`), including a chaos harness with a mock init system and init-template rendering tests. | Wire into CI explicitly — partially done (suites exist; CI invocation is via the repo-wide shell harness). |
| **Language** | Pure Bash – easy to read but limited in robustness. | Consider moving core orchestration to Rust or Go for stronger typing (long-term, optional). |
| **Packaging** | Deployed via source checkout + watchdog installer / mt-admin. | Provide a Docker image or installable package for reproducible deployments (open). |

## 8. Integration points with LunarWing services
- **Workspace paths** – All scripts read/write under `LUNARWING_BASE_DIR` (default `$HOME/.lunarwing`; legacy `IRONCLAW_BASE_DIR` alias honored), e.g. `<base>/agents`, `<base>/ratelimit`, `<base>/workspace/reports/health`.
- **Environment variables** – `GOTIFY_URL`, `GOTIFY_TOKEN`, `HEALTH_*` per-check overrides (e.g. `HEALTH_LUNARVISION_URL` — leave unset on MT hosts so ports are discovered from `/etc/lunarwing/ports.json`), `SELF_HEAL_*` watchdog tunables, and provider API keys, mirroring other services.
- **Scheduling assets** – `ic/systemd/lunarwing-watchdog.{service,timer,confd,cron.hourly}` and `ic/systemd/com.lunarwing.watchdog.plist`; installer auto-detects systemd vs OpenRC vs launchd (`ic/scripts/install-lunarwing-watchdog.sh`). MT installs go through `lunarwing-mt-admin.sh`.
- **Reporting directory** – Other services can consume the JSON files for cross-service health dashboards.
- **Notification channel** – Gotify is also used by other LunarWing components, providing a unified alerting layer.

## 9. Risks, gaps, and potential improvements
1. **Hard-coded check list** – The driver enumerates checks explicitly; a new check needs edits in three places (launch, log-append loop, aggregation). Auto-discovery would remove this.
2. **No aggregate schema validation** – The final report is only validated as well-formed JSON, not against a schema.
3. **Hard-coded paths under base dir** – Scripts assume the `<base>/agents`, `<base>/workspace/...` layout; a relocated workspace must be expressed through `LUNARWING_BASE_DIR`.
4. **Security** – API keys arrive via environment/env files; a secret-manager integration (Vault etc.) remains future work.
5. **Observability** – No Prometheus exporter yet; file-based reports only.
6. **Driver-side notify dedupe** – Repeated degraded runs still re-notify (self-heal escalations are cooldown-guarded, driver notifications are not).

(Items from the original draft that are **fixed** and therefore removed: the duplicated `run_check` launches, the missing `health-irc.sh` README reference — the README no longer mentions IRC — the absent central configuration, the absent test suite, and the `$HOME/.ironclaw` path assumptions.)

## 10. Roadmap status
| Milestone | Description | Status |
|-----------|-------------|--------|
| **M1 – Refactor driver** | Fix duplicated launch lines; private per-run workdir; bounded parallel checks. | **Done** (auto-discovery of `health-*.sh` still open, low priority). |
| **M2 – Central configuration** | `config.sh.example` documents all thresholds/tunables; env/EnvironmentFile sourced. | **Done**. |
| **M3 – Add unit tests** | BATS-style sandboxed suites for self-heal, chaos, per-init and per-sidecar checks, init templates. | **Done** (`tests/run-all.sh`, 10 suites). |
| **M4 – Prometheus exporter** | Thin exporter reading the latest JSON report and exposing `/metrics`. | Open. |
| **M5 – Docker packaging** | Lightweight image bundling the scripts on a schedule. | Open. |
| **M6 – Secret management** | Replace plain-env API keys with Vault lookups; env fallback for dev. | Open. |
| **M7 – Documentation refresh** | README covers configuration flow, data flow, and supported services. | **Done** (README kept current; this report re-verified 2026-08-13). |

---
*Report generated by Admiral Starforce Nebula; updated 2026-08-13 against current code.*
