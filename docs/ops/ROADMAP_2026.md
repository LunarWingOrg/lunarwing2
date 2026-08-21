# The Roadmap Document

## Actual Official Roadmap:
| Feature | Target |
|---------|--------|
| Lunartica improvements | v2.0.9.0 |
| DarkIRC key manager | v2.0.9.0 |
| Purificiation | v2.0.9.0 |
| Durable cross-channel attachments — a file sent on one channel stays retrievable from all other channels and the WebUI via a shared attachment store (`docs/ops/IRONCLAW_IDEAS_TAKEAWAYS.md`) | v2.0.9.0 |
| LunarHub extension marketplace — phased rollout starting with foundation repairs (unified verified installer, atomic WASM update/rollback, bounded MCP discovery), then binary/artifact distribution, signed catalog + CAS lifecycle, and finally hosted catalog/deep links | v2.1.0.0 |
| LunarHub polish | v2.1.1.0 |
| LunarVoice Two Way Voice Communication | v2.1.2.0 |
| Memory as a capability-declaring extension — memory/knowledge providers advertise declared capabilities (search, get, submit, list-stale, stats) so providers are swappable (`docs/ops/IRONCLAW_IDEAS_TAKEAWAYS.md`) | v2.1.2.0 |
| TPM 2.0 Decryption Integration via Clevis | v2.1.4.0 |
| LunarVoice Two Way Voice Communication | v2.1.4.0 |
| LTX integration | v2.1.6.0 |

### Unscoped discussed ideas (no release target yet)

- BENCHMARKING_MODE — opt-in system-prompt addendum for unattended evaluation runs; adopt once automated eval infrastructure becomes a priority (`docs/ops/IRONCLAW_IDEAS_TAKEAWAYS.md`)
- Interrupt/steer — mid-turn steering (inject guidance into an in-flight turn at the next model-call boundary instead of only queuing for the next turn) plus an interrupt mode that preserves queued follow-ups; cancellation plumbing already exists on both paths (v1 `/interrupt`, Engine V2 interrupt-aware dispatch) (`docs/ops/IRONCLAW_IDEAS_TAKEAWAYS.md`)
- LTX sidecar healthpoint — the next sidecar target; the metrics exporter picks it up automatically once `health-ltx.sh` exists (`docs/proposals/HEALTHPOINT_METRICS_EXPORTER.md`)

---
