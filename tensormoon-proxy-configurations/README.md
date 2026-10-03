# Deprecated
## all files in this directory are deprecated and should be removed or archived
### ensure nothing uses these for tests or anything first before rm

## TensorMoon cutover note

This directory was `tensorzero-proxy-configurations/` while LunarWing ran against
the (now abandoned) TensorZero gateway. It was renamed for the TensorMoon fork;
`tensorzero-proxy-configurations` remains as a symlink so unit files rendered
before the rename keep working until `render-units` is rerun.

Still live despite the deprecation notice above:

- `tensormoon.toml` (was `tensorzero.toml`) — the gateway configuration deployed
  on the TensorMoon host. Same schema; the `tensorzero::` → `tensormoon::` rename
  only affects client-side model names, not this file.
- `lunarwing-proxy.py` — started by mt-admin for tenants provisioned with
  `--enable-proxy` and by `ic/scripts/lunarwing-xmpp-test-env.sh`. `--tensormoon <url>`
  is the upstream flag; `--tensorzero` is accepted as a deprecated alias.

See `docs/guides/MIGRATE_TENSORZERO_TO_TENSORMOON.md` for the full cutover.
