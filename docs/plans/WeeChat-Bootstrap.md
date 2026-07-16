---
plan name: WeeChat-Bootstrap
plan description: Secure tenant relay provisioning
plan status: done
---

## Idea
Implement the approved Approach A for LunarWing multi-tenant setup: automatically generate a fresh tenant's loopback-only WeeChat API relay configuration through WeeChat's supported one-shot command interface; preserve and fail on any existing configuration; keep the resolved relay password out of argv, logs, unit files, and relay.conf by storing an environment expression; expose only a dedicated minimal credential environment to the persistent WeeChat service on both systemd user managers and OpenRC; provide a fatal explicit retry command while keeping automatic add-tenant failure recoverable; verify with fixture-first shell tests, read-only preflight checks, an opt-in real-WeeChat smoke test, and updated active documentation. The decision-complete source plan is `.omo/plans/weechat-relay-auto-bootstrap.md`.

## Implementation
- Create fixture-first tests and focused mt-admin helpers for safe env parsing, existing-config detection, one-shot WeeChat invocation, generated-config validation, same-filesystem temporary generation, cleanup, and atomic promotion.
- Create and test a mode-0600 tenant-owned `weechat.env` containing only `RELAY_PASSWORD`, then extract testable WeeChat systemd/OpenRC render helpers that load it without exposing the full tenant environment.
- Add and test the root-only `configure-weechat-relay <tenant>` recovery command, then invoke the same helper non-fatally during fresh `add-tenant` after environment generation with explicit degraded-state reporting.
- Extend and test `lunarwing-weechat-preflight.sh` to validate the generated relay API section, registry port, loopback binding, literal password expression, and minimal environment without printing secrets.
- Update active WeeChat operations, architecture, tenant configuration, and verification documentation to describe automatic bootstrap, canonical service names, preserve-and-fail behavior, and recovery.
- Run Bash syntax checks, focused fixture harnesses, shellcheck, and the opt-in installed-WeeChat one-shot smoke test while retaining evidence and ensuring no process, port, temp artifact, or secret leakage remains.
- Run parallel final compliance, code-quality/security, real-QA, and scope-fidelity reviews; require all four to approve and present their evidence before completion.

## Required Specs
<!-- SPECS_START -->
- WeeChat-Relay
<!-- SPECS_END -->