# Create a LunarWing Tenant

`ic/scripts/create-tenant.sh` is the convenience wrapper for provisioning one multi-tenant LunarWing instance. It delegates lifecycle work to `ic/scripts/lunarwing-mt-admin.sh`; use the MT admin directly for ongoing start, stop, status, build, and removal operations.

**Revalidated against source 2026-09-17 (v2.1.2.0); re-checked 2026-09-27 (v2.1.3.0), 2026-10-01 (v2.1.4.0), 2026-10-04 (v2.1.5.0), and 2026-10-14 (v2.1.6.0).** The wrapper's `--minimal`,
`--no-pebble`, and retired-Nanocode rejection were re-checked against
`ic/scripts/create-tenant.sh`; the `--with-study-mcp` flow documented here
routes through MT admin and is unchanged. The 2026-10-14 repass added the
v2.1.6.0 (WI#330) required LLM endpoint flags below.

## Before You Run It

- Run the script from the checkout and branch that the tenant should use. The MT tooling checks out the source repository's current branch and prints the selected branch and commit before provisioning.
- Review [MT-ADMIN-QUICKSTART.md](MT-ADMIN-QUICKSTART.md), especially the host prerequisites and port registry behavior.
- Run as root through `sudo`. Provisioning creates an OS user, PostgreSQL resources, service definitions, environment files, and port-registry entries.
- Avoid putting passwords or API keys in shell history. Omit them when generated defaults are appropriate, or pass protected environment values with an approved operator workflow.

## Usage

```bash
sudo ic/scripts/create-tenant.sh <name> [options]
```

The name is lowercased and sanitized to `[a-z0-9-]`. All paths, service names, and default tenant identifiers are derived from it.

Common examples:

```bash
# Full tenant: core daemon, Pebble worker, and WASM extensions
sudo ic/scripts/create-tenant.sh acme

# Skip the Pebble worker
sudo ic/scripts/create-tenant.sh acme --no-pebble

# Core daemon without Pebble or Docker-group membership
sudo ic/scripts/create-tenant.sh scratch --minimal

# Preview every current option and environment override
bash ic/scripts/create-tenant.sh --help
```

The default full flow calls MT admin to add the tenant, ensures the WASM toolchain when requested, builds the selected components, starts the tenant, verifies health, and prints WeeChat setup instructions. Use `--yes` only after reviewing the printed plan.

Since v2.1.6.0 (WI#330) a tenant LLM endpoint is required and there is no built-in default: pass `--tensorzero-url <url>` (the wrapper never passes `--enable-proxy`, so the daemon dials this TensorZero gateway directly at its `/openai/v1` surface) or `--llm-base-url <url>` (direct endpoint), or export the wrapper's own `TENSORZERO_URL`/`LLM_BASE_URL` env overrides, or export the mt-admin fleet `LUNARWING_MT_TENSORZERO_URL`/`LUNARWING_MT_LLM_BASE_URL`; without one, `add-tenant` refuses before any tenant state exists.

Do not use retired Nanocode flags. The script rejects `--nanocode`, `--with-nanocode`, and `--no-nanocode`; Pebble is the supported external worker.

This wrapper does not accept `--with-study-mcp` (unknown flags are rejected). To seed the new tenant's knowledge store with the full shared lw2studymcp study-notes base, arm it through MT admin afterwards (the seed never overwrites existing notes and runs at most once, so re-arming before or after first boot is safe):

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant acme --with-study-mcp
```

See [`TENANT_KNOWLEDGE_SEED.md`](TENANT_KNOWLEDGE_SEED.md) for what that does and its one-shot safety invariants.

## After Provisioning

Use the init-agnostic MT admin interface rather than calling tenant services directly:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh status acme
sudo ic/scripts/lunarwing-mt-admin.sh doctor
```

On systemd hosts, tenant units run in the tenant user's user manager. On OpenRC hosts, MT admin uses the corresponding OpenRC services. Do not substitute bare system-bus `systemctl` checks.
