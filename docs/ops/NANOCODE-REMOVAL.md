# Nanocode External Worker Removal

Nanocode is no longer a supported LunarWing external worker. Supported persistent
worker containers are Pebble, OpenCode, and Kimi Code.

## Upgrade Behavior

`lunarwing-mt-admin.sh upgrade-tenant` and `patch-env` remove generated Nanocode
routing from tenant `config.toml` and remove `NANOCODE_*` entries from
`lunarwing.env`. `start-tenant` stops and disables legacy Nanocode supervision.

Removal is non-destructive. Existing `nanocode-workspace` directories, container
images, stopped containers, and historical port-registry fields remain available
for manual backup or cleanup. Tenant removal still recognizes legacy Nanocode
units so old installations can be torn down safely.

## Operator Check

After upgrading a tenant:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh patch-env <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh status <tenant>
```

Back up any needed files from
`/home/<tenant>/lunarwing/nanocode-workspace/`, then remove legacy images or
stopped containers manually. LunarWing does not delete workspace data during
upgrade.
