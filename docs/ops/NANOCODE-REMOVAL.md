# Nanocode External Worker Removal

Nanocode is no longer a supported LunarWing external worker. Supported persistent
worker containers are Pebble, OpenCode, and Kimi Code.

## Upgrade Behavior

`lunarwing-mt-admin.sh upgrade-tenant` and `patch-env` remove generated Nanocode
routing, including nested endpoint tables, from tenant `config.toml`; remove
`NANOCODE_*` entries from `lunarwing.env`; and delete legacy `nanocode.env` files.
`start-tenant` stops and disables legacy Nanocode supervision. Podman cleanup
checks both root and tenant rootless stores, and aborts if a container cannot stop.

Removal is non-destructive. Existing `nanocode-workspace` directories, container
images, stopped containers, and historical port-registry fields remain available
for manual backup or cleanup. Tenant removal still recognizes legacy Nanocode
units so old installations can be torn down safely.

Nanocode worker source, image-build entrypoints, Compose runtime definitions, and
active TensorZero function routing are removed from the repository.

Machine-migration imports accept legacy `NANOCODE_MODEL` and
`NANOCODE_BASE_URL` manifest entries for backward compatibility, but discard
them instead of writing retired settings to the target tenant.

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
