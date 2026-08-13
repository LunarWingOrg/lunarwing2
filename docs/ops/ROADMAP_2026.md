# The Roadmap Document

## Actual Official Roadmap:
| Feature | Target |
|---------|--------|
| Cross-tenant knowledge sharing via coordination pipeline | v2.0.8.0 |
| Vector embeddings hybrid ranking | v2.0.8.0 |
| MCP Overhaul Part 3 — marketplace provenance/signatures/revocation, safe package-install policy, and optional bundled gateway control-plane/HA evaluation | v2.0.8.0 |
| Health service improvements — healthpoint metrics exporter: Prometheus textfile export of the aggregated health report, self-heal state series, optional loopback-only listener behind a port-registry v16 slot, and a graduated early-warning threshold layer with a deduplicated Gotify path (`docs/proposals/HEALTHPOINT_METRICS_EXPORTER.md`) | v2.0.8.0 |
| mt-admin refactor/decomposition — reuse the `ref2/sunburst/mtadmin-refactor-v2.0.6.0` branch where applicable (`docs/proposals/MT-ADMIN-DECOMPOSITION.md`) | v2.0.8.0 |
| Agent failure recovery with next-steps — every termination path (iteration limit, disabled capability, denied call, provider error) tells the model what would unlock the call instead of stopping opaquely (`docs/ops/35d44998cadf3e6d.md`) | v2.0.8.0 |
| Lunartica improvements | v2.0.9.0 |
| Unfinished features from previous releases, including the DarkIRC key manager | v2.0.9.0 |
| Additional LunarVoice polishing | v2.0.9.0 |
| Cleaning up old code, stale documentation Purificiation process similar to v1.1.9.0 | v2.0.9.0 |
| Durable cross-channel attachments — a file sent on one channel stays retrievable from all other channels and the WebUI via a shared attachment store (`docs/ops/35d44998cadf3e6d.md`) | v2.0.9.0 |
| LunarHub extension marketplace — phased rollout starting with foundation repairs (unified verified installer, atomic WASM update/rollback, bounded MCP discovery), then binary/artifact distribution, signed catalog + CAS lifecycle, and finally hosted catalog/deep links (`docs/ops/LUNARHUB_IRONHUB_MARKETPLACE_RESEARCH.md`) | v2.0.9.0 |
| LunarVoice Two Way Voice Communication | v2.1.0.0 |
| Memory as a capability-declaring extension — memory/knowledge providers advertise declared capabilities (search, get, submit, list-stale, stats) so providers are swappable (`docs/ops/35d44998cadf3e6d.md`) | v2.1.0.0 |

### Unscoped discussed ideas (no release target yet)

- BENCHMARKING_MODE — opt-in system-prompt addendum for unattended evaluation runs; adopt once automated eval infrastructure becomes a priority (`docs/ops/35d44998cadf3e6d.md`)
- LTX sidecar healthpoint — the next sidecar target; the metrics exporter picks it up automatically once `health-ltx.sh` exists (`docs/proposals/HEALTHPOINT_METRICS_EXPORTER.md`)

---
