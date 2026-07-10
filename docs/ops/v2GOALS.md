# Roadmap Document

Items are grouped to respect the release cadence (`docs/ops/RELEASE_CADENCE.md`): odd-numbered releases focus on bug fixes / security / polish / cleanup, even-numbered releases focus on features, and major versions such as 2.0.0 or 2.1.0 will typically include massive overhauls of existing systems.

| Feature | Target |
|---------|--------|
| Drop legacy `ironclaw-agent-v1` subprotocol offer from the daemon (delete the `SUBPROTOCOL_LEGACY` offer in `ic/src/orchestrator/external_worker.rs`) and `git rm` the 1.1.9-only repo-root compat symlinks `ironclaw_weechat_wss`, `darkirc_channel_for_ironclaw` (deployed tenants must have re-run mt-admin unit regen by then) | v2.0.0 |
| LunarWing Web UI performance overhaul | v2.0.0 |
| LunarWing Web MT admin setup integration (part 2 of earlier plan discussed) | v2.0.0 |
| Several large proposals to ship | v2.0.0 |

---

