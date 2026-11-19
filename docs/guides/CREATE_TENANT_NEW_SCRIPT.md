# Create a LunarWing Tenant

`ic/scripts/create-tenant.sh` is the convenience wrapper for provisioning one multi-tenant LunarWing instance. It delegates lifecycle work to `ic/scripts/lunarwing-mt-admin.sh`; use the MT admin directly for ongoing start, stop, status, build, and removal operations.

**Revalidated against source 2026-11-19 (v2.2.1.0).** The wrapper's `--minimal`,
`--no-pebble`, required LLM endpoint forwarding, and retired-Nanocode rejection
match `ic/scripts/create-tenant.sh`. For `--with-study-mcp`, provision the new
tenant through MT admin directly.

## Before You Run It

- Run the script from the checkout and branch that the tenant should use. The wrapper's plan prints its checkout's branch and commit. MT admin clones from that checkout by default, or from `LUNARWING_MT_SOURCE_REPO` when supplied.
- Review [MT-ADMIN-QUICKSTART.md](MT-ADMIN-QUICKSTART.md), especially the host prerequisites and port registry behavior.
- Run as root through `sudo`. Provisioning creates an OS user, PostgreSQL resources, service definitions, environment files, and port-registry entries.
- Avoid putting passwords or API keys in shell history. Omit them when generated defaults are appropriate, or pass protected environment values with an approved operator workflow.

## Usage

```bash
sudo ic/scripts/create-tenant.sh <name> [options]
```

The name is lowercased and sanitized to `[a-z0-9-]`. All paths, service names, and default tenant identifiers are derived from it.

Common examples (replace `https://llm.example.com/v1` with your endpoint):

```bash
# Full tenant: core daemon, Pebble worker, and WASM extensions
sudo ic/scripts/create-tenant.sh acme --llm-base-url https://llm.example.com/v1

# Skip the Pebble worker
sudo ic/scripts/create-tenant.sh acme --no-pebble --llm-base-url https://llm.example.com/v1

# Skip Pebble and Docker-group membership (WASM still builds by default)
sudo ic/scripts/create-tenant.sh scratch --minimal --llm-base-url https://llm.example.com/v1

# Show usage and options
bash ic/scripts/create-tenant.sh --help
```

The default full flow calls MT admin to add the tenant, sets up the WASM toolchain when requested, builds the selected components, starts the tenant, and prints status, reachability checks, and WeeChat setup instructions. Not all status and reachability checks are fatal; review their output. Use `--yes` only after reviewing the plan.

Since v2.1.6.0 (WI#330) a tenant LLM endpoint is required and there is no built-in default: pass `--tensorzero-url <url>` (the wrapper never passes `--enable-proxy`, so the daemon dials this TensorZero gateway directly at its `/openai/v1` surface) or `--llm-base-url <url>` (direct endpoint), or export the wrapper's own `TENSORZERO_URL`/`LLM_BASE_URL` env overrides, or export the mt-admin fleet `LUNARWING_MT_TENSORZERO_URL`/`LUNARWING_MT_LLM_BASE_URL`; without one, `add-tenant` refuses before any tenant state exists.

Do not use retired Nanocode flags. The script rejects `--nanocode`, `--with-nanocode`, and `--no-nanocode`; Pebble is the supported external worker.

This wrapper does not accept `--with-study-mcp`. To seed a new tenant's
knowledge store from the shared lw2studymcp study notes, use MT admin in place
of the wrapper. The startup import skips existing notes and does not repeat
once its completion marker is saved:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant acme --with-study-mcp \
  --llm-base-url https://llm.example.com/v1
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant acme
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant acme
```

See [`TENANT_KNOWLEDGE_SEED.md`](TENANT_KNOWLEDGE_SEED.md) for what that does and its one-shot safety invariants.

## After Provisioning

Use the init-agnostic MT admin interface rather than calling tenant services directly:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh status acme
sudo ic/scripts/lunarwing-mt-admin.sh doctor
```

On systemd hosts, tenant units run in the tenant user's user manager. On OpenRC hosts, MT admin uses the corresponding OpenRC services. Do not substitute bare system-bus `systemctl` checks.
