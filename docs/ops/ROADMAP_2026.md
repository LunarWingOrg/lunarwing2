# Roadmap Document

Items are grouped to respect the release cadence (`docs/ops/RELEASE_CADENCE.md`): odd-numbered releases focus on bug fixes / security / polish / cleanup, even-numbered releases focus on features, and major versions such as 2.0.0 or 2.1.0 will typically include massive overhauls of existing systems.

| Feature | Target |
|---------|--------|
| XMPP file transfer — remaining polish (further hardening) | v2.0.3 |
| Drop legacy `ironclaw-agent-v1` acceptance from external workers (pebble/lunarcode/opencode `LEGACY_SUBPROTOCOL` constants + negotiation fallback), one release after the daemon stops offering it | v2.0.3 |
| Org/registry decisions deferred from 1.1.9 item #2: re-host registry WASM artifacts or make them source-build-only (nearai/ironclaw release URLs in `ic/registry/*.json` + `installer.rs` allowlist), fix or delete the `release-plz.yml` `repository_owner == 'nearai'` guard — fits alongside the Forgejo/CI migration. The Docker Hub image namespace was moved to `ggmethos/*` and the obsolete GCP VM bootstrap path was removed in 1.1.9 cleanup. | v2.0.3 |
| Further polishing of Lunarvision AND XMPP file sharing integration - see XMPP_LUNARVISION_INTEGRATION.md in docs/proposals for some information | v2.0.3 |
| Multi-arch CI/CD pipeline for development. Migrate to new dedicated LW v2 repo | v2.0.3 |
| Better githooks for repo | v2.0.3 |
| DarkIRC key exchange automation | v2.0.3 |
| Lorebook support / Agent Profile enhancements / Workspace Seeding improvements / Agent Profile switching / User Profile switching (further planning required) | v2.0.4 |
| Self-Healing Capability Expansion - deferred from 1.1.8 | v2.0.4 |
| Self-Healing Capabilities analysis of any missing pieces from all the new components. Implementation of missing pieces to follow | v2.0.4 |
| More Crate Updates | v2.0.4 |
| MCP Overhaul Part 1 | v2.0.4 |
| Per-tenant WeeChat health-glob gate (fix the flap /`render-units` footgun) | v2.0.5 |
| Deprecate Nanocode external Worker | v2.0.5 |
| Lunartica UI reskin continuation | v2.0.5 |
| Upgrade old testing harness | v2.0.5 |
| Further external worker polishing | v2.0.5 |
| Feature set of subset of agent features adopted from Hermes Agent (Human Delay mode already landed in a prior release); comprehensive documentation to accompany each new feature | v2.0.6 |
| Input-validation security improvements | v2.0.6 |
| ONNX Runtime speech to text model support necessary for LunarVoice | v2.0.6 |
| MCP Overhaul Part 2 | v2.0.6 |
| Additional WASM Channel Polishing | v2.0.7 |
| Additional Opencode/Paseo External Worker Polishing | v2.0.7 |
| Reflex Compiler polishing and improvements | v2.0.7 |
| LunarVoice Two Way Voice Communication | v2.0.8 |
| MCP Overhaul Part 3 | v2.0.8 |
| Additional LunarVoice polishing | v2.0.9 |
| A surprise | v2.1.0 |

---

