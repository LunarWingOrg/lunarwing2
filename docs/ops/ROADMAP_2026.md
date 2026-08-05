# The Roadmap Document


## Maintenance notes

- GitLab container-registry setup and manual worker-image CI are implemented;
  see [`GITLAB_CONTAINER_REGISTRY.md`](GITLAB_CONTAINER_REGISTRY.md).
- OpenCode updates remain normal dependency maintenance. The current worker
  image pins upstream `v1.17.13`; change that pin only with worker-image and
  bridge validation.

## Actual Official Roadmap:
| Feature | Target |
|---------|--------|
| Per-tenant WeeChat health-glob gate (fix the flap /`render-units` footgun) | v2.0.5 |
| Security hardening | v2.0.5 |
| Lunartica UI reskin continuation | v2.0.5 |
| Upgrade old testing harness | v2.0.5 |
| Further external worker polishing | v2.0.5 |
| Feature set of subset of agent features adopted from Hermes Agent (Human Delay mode already landed in a prior release); comprehensive documentation to accompany each new feature | v2.0.6 |
| Input-validation security improvements | v2.0.6 |
| ONNX Runtime speech to text model support necessary for LunarVoice | v2.0.6 |
| MCP Overhaul Part 2 - See [MCP Overhaul Part 1](MCP_OVERALL_STATUS.md) for details — runtime-neutral package/policy vocabulary, runtime adapter boundary, and worker-local prototype using preinstalled or verified packages | v2.0.6 |
| Additional Opencode External Worker Polishing | v2.0.7 |
| Reflex Compiler polishing and improvements | v2.0.7 |
| LunarVoice Two Way Voice Communication | v2.0.8 |
| MCP Overhaul Part 3 — marketplace provenance/signatures/revocation, safe package-install policy, dedicated gateway evaluation, and MCP-over-WASM feasibility | v2.0.8 |
| Lunartica improvements | v2.0.9 |
| Additional LunarVoice polishing | v2.0.9 |
| A surprise | v2.1.0 |

---
