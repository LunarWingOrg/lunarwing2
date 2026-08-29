# Operations documentation

Maintained runbooks, release procedures, and current operational follow-ups live
in this directory. Start with the production guide and the host-specific tenant
guide; older point-in-time reports belong under [`history/`](history/README.md)
and must not be treated as current procedures.

## Start here

- [`MULTITENANCY-PRODUCTION.md`](MULTITENANCY-PRODUCTION.md) — production
  multi-tenant deployment and day-to-day lifecycle.
- [`TENANT-CONFIGURATION.md`](TENANT-CONFIGURATION.md) — per-tenant settings,
  environment, services, and ports.
- Host guides:
  [Gentoo/OpenRC](guide-for-spin-up-gentoo-tenants.md),
  [Arch/systemd](guide-for-spin-up-arch-tenants.md),
  [Fedora/systemd](guide-for-spin-up-fedora-tenants.md), and
  [Debian/Ubuntu/systemd](guide-for-spin-up-debian-ubuntu-tenants.md).

Tenant lifecycle and status operations must go through
`ic/scripts/lunarwing-mt-admin.sh`; do not probe tenant units with bare
system-bus `systemctl` or duplicate OpenRC lifecycle logic in wrappers.

## Deployment, migration, and recovery

- [`MT-MACHINE-MIGRATION.md`](MT-MACHINE-MIGRATION.md) — move a multi-tenant
  installation to another host.
- [`MT-LEGACY-UPGRADE-NOTES.md`](MT-LEGACY-UPGRADE-NOTES.md) and
  [`TENANT-RENAME-MIGRATION-1.1.9.md`](TENANT-RENAME-MIGRATION-1.1.9.md) —
  legacy upgrade paths and rename compatibility.
- [`GITLAB_CONTAINER_REGISTRY.md`](GITLAB_CONTAINER_REGISTRY.md) — registry and
  worker-image operations; [`REGISTRY_PUSH_499.md`](REGISTRY_PUSH_499.md)
  covers the host-side timeout failure mode.
- [`GITHOOKS_HOW_TO.md`](GITHOOKS_HOW_TO.md) and
  [`RELEASE_COMMANDS.md`](RELEASE_COMMANDS.md) — repository and release
  operations.

## Harnesses and verification

- [`HARNESS-SINGLE-TENANT.md`](HARNESS-SINGLE-TENANT.md)
- [`MULTITENANCY-HARNESS.md`](MULTITENANCY-HARNESS.md)
- [`SSH-HARNESS-SETUP.md`](SSH-HARNESS-SETUP.md)
- [`PRE-RELEASE-TESTING.md`](PRE-RELEASE-TESTING.md)
- [`TEST-SCRIPTS-STATUS.md`](TEST-SCRIPTS-STATUS.md)
- [`TEST-PLAN-UPGRADED-TENANT-1.1.9.md`](TEST-PLAN-UPGRADED-TENANT-1.1.9.md)

## Component operations

- Workers: [`WORKER-CONTAINERS.md`](WORKER-CONTAINERS.md),
  [`PEBBLE-WORKER.md`](PEBBLE-WORKER.md), and
  [`PEBBLE-SUBMODULE.md`](PEBBLE-SUBMODULE.md).
- DarkIRC: [`DARKIRC-MULTITENANT.md`](DARKIRC-MULTITENANT.md) and
  [`DARKIRC-KEYEXCHANGE-STATUS.md`](DARKIRC-KEYEXCHANGE-STATUS.md).
- WeeChat: [`WEECHAT-SERVICES.md`](WEECHAT-SERVICES.md).
- XMPP: [`XMPP_TRANSFERS.md`](XMPP_TRANSFERS.md),
  [`XMPP_KNOWN_ISSUES.md`](XMPP_KNOWN_ISSUES.md), and
  [`XMPP_MESSAGE_BODY_LIMIT_SECURITY_REVIEW.md`](XMPP_MESSAGE_BODY_LIMIT_SECURITY_REVIEW.md).
- MCP: [`MCP_OVERALL_STATUS.md`](MCP_OVERALL_STATUS.md) and
  [`MCP_ADDITIONS.md`](MCP_ADDITIONS.md).
- SSH: [`SSH_KEY_LOG_EXPOSURE.md`](SSH_KEY_LOG_EXPOSURE.md)
  (key-upload log-leak finding, fix, and rotation guidance).

## Planning and active ledgers

- The current `AGENT_GOALS_*.md` file is the active pre-release checklist.
- [`ROADMAP_2026.md`](ROADMAP_2026.md) and
  [`RELEASE_CADENCE.md`](RELEASE_CADENCE.md) define forward planning.
- [`SUNBURST_BUG_LIST.md`](SUNBURST_BUG_LIST.md),
  [`COORDINATION_FIXES.md`](COORDINATION_FIXES.md), and
  [`UI_FIX_SUGGESTIONS.md`](UI_FIX_SUGGESTIONS.md) retain unresolved or
  deployment-verification follow-ups alongside their completed findings.

## Archive boundary

Completed release evidence, superseded roadmaps, review snapshots, and old
checklists are in [`history/`](history/README.md). Versioned release notes live
in [`../releases/`](../releases/). Raw CI transcripts, assistant session output,
and one-line placeholders are not documentation and should not be committed;
use the CI job or Git history as the source of record.
