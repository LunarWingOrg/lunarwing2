# Release Notes for LunarWing v2.1.8.0

**Status:** draft, not yet released. Sections are added as changes land on the release branch.

## Upgrade Notes

1. **Re-declare the pebble model backend before the next pebble start (WI#506).** On egress-enforcing multi-tenant hosts the pebble lane now derives its allowlist from the backend that `configure-pebble` pins into `pebble.env` (`PEBBLE_BASE_URL` and `NANOGPT_BASE_URL`), and refuses to start when no backend is declared. Tenants provisioned with `--with-pebble` before this change carry no backend key, so their pebble lane stops at the next start with a named remediation; the other lanes are not affected. Before restarting such a tenant, re-run `configure-pebble <tenant>` (an omitted `--base-url` keeps the binary's default NanoGPT endpoint), or grant the host directly with `configure-worker-egress <tenant> --allow nano-gpt.com:443`, then `restart-tenant <tenant>` and check `worker-egress <tenant>`. See [the pebble worker runbook](../ops/PEBBLE-WORKER.md).
