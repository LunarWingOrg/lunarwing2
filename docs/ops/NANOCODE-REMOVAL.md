# Nanocode External Worker Retirement

Nanocode is no longer a supported LunarWing external worker. Supported
persistent worker containers are Pebble, OpenCode, and Kimi Code.

## Automatic Retirement

Current `lunarwing-mt-admin.sh` lifecycle operations retire legacy Nanocode
resources before continuing. This applies when resuming `add-tenant`, patching
environment files, rendering units, starting, stopping, upgrading, or removing
a tenant.

Retirement runs under the tenant lifecycle lock and performs these steps in
order:

1. Stop legacy systemd user or OpenRC supervision.
2. Stop matching containers in root Docker, root Podman, and tenant rootless
   Podman stores. Failure aborts retirement before configuration or registry
   mutation.
3. Remove legacy systemd Quadlet/user-unit files or OpenRC service files.
4. Remove only the Nanocode `[[sandbox.external_workers]]` entry and its nested
   tables from `config.toml`, validating TOML before and after the edit.
5. Remove `NANOCODE_*` keys from `lunarwing.env` and archive `nanocode.env`.
6. Set `workers.nanocode=false` in `/etc/lunarwing/ports.json`.

Health reports keep discovered legacy Nanocode units visible with status
`skipped` and retirement metadata. Self-heal does not restart them.

## Preserved Data

Retirement is intentionally non-destructive. It preserves:

- `/home/<tenant>/lunarwing/nanocode-workspace/`
- stopped Nanocode containers in every discovered container store
- Nanocode container images
- `ports.nanocode_wss` and `extended_ports.nanocode_health`
- reserved tenant-name prefix `nanocode-`
- first rollback copies named `config.toml.pre-nanocode-retirement` and
  `lunarwing.env.pre-nanocode-retirement`
- archived worker env files named `nanocode.env.retired[.<timestamp>.<pid>]`

New tenants retain the registry fields and port slots, but those slots are not
reallocated or activated.

## Verification

Run:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh patch-env <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh render-units <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh status <tenant>

jq '.tenants["<tenant>"].workers.nanocode' /etc/lunarwing/ports.json
grep -n 'name = "nanocode"' /home/<tenant>/lunarwing/state/config.toml
grep -n '^NANOCODE_' /home/<tenant>/lunarwing/env/lunarwing.env
```

Expected registry value is `false`; both `grep` commands should produce no
matches. A preserved container may still appear in `docker ps -a` or
tenant-scoped `podman ps -a`, but it must not be running.

## Legacy Imports

Machine-migration manifests may contain `NANOCODE_MODEL` and
`NANOCODE_BASE_URL`. Import validation accepts these legacy keys so old bundles
remain readable, then discards them instead of writing retired settings.

Saved onboarding and Hermes plans containing Nanocode are rejected with an
explicit retirement error. Remove Nanocode from the saved plan before retrying.

## Rollback and Cleanup

LunarWing does not automatically restore retired Nanocode execution. To
downgrade, preserve current tenant files first, deploy a pre-retirement release,
then review and restore the `.pre-nanocode-retirement` files and archived
`nanocode.env` under normal change control. Do not mark the registry worker
enabled until old supervision and worker code are installed and reviewed.

Deleting preserved workspaces, containers, images, or rollback files is
irreversible. Back up required tenant data first, then remove those artifacts
manually with the container runtime that owns each store.
