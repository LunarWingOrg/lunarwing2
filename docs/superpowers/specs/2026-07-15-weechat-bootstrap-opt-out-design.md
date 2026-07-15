# WeeChat Bootstrap Opt-Out Design

**Status:** Approved design  
**Date:** 2026-07-15

## Goal

Allow operators creating new tenants to skip automatic WeeChat relay configuration without changing the default behavior. The opt-out affects relay bootstrap only; it does not disable WeeChat, suppress service-unit rendering, or remove the later recovery path.

## User Contract

The new flag is:

```text
--no-weechat-bootstrap
```

Without the flag, `add-tenant` and `add-tenants` continue to generate the relay configuration automatically. With the flag:

- `configure_weechat_relay` is not called during tenant creation.
- No `relay.conf` is generated and no WeeChat process is launched for configuration.
- The dedicated `env/weechat.env` is still written so the rendered WeeChat service can start without loading the full tenant environment.
- WeeChat and adapter service units are still rendered.
- Tenant provisioning continues normally.
- The completion summary reports `weechat relay: disabled (--no-weechat-bootstrap)`.
- The operator can enable the relay later with `configure-weechat-relay <tenant>`.

Skipping bootstrap is an intentional state, not a warning or provisioning failure.

## Scope

The flag is available through new-tenant provisioning surfaces:

1. `lunarwing-mt-admin.sh add-tenant`
2. `lunarwing-mt-admin.sh add-tenants`
3. The Python onboarding CLI
4. The browser onboarding console
5. The OpenRC bulk provisioner

Kawarimi import remains unchanged. It continues to provision a fresh target through `add-tenant`, run automatic WeeChat bootstrap, and execute WeeChat preflight before cutover. Fixed-profile `create-tenant-*.sh` scripts also remain unchanged because they do not expose the general `--no-health` and `--no-ssh` opt-out surface.

## Core Implementation

`lunarwing-mt-admin.sh` will follow the established global opt-out pattern used by `--no-health` and `--no-ssh`:

- Add `WEECHAT_BOOTSTRAP_OPT_OUT=false` at script scope.
- Parse `--no-weechat-bootstrap` in both `add-tenant` and `add-tenants` dispatch blocks.
- Do not add another positional argument to `add_tenant`; its current 21-argument signature remains unchanged.
- Gate only the `configure_weechat_relay` call. The opted-out branch reads the generated `RELAY_PASSWORD` and calls `_write_weechat_env` directly without invoking WeeChat.
- Preserve the current nonfatal warning and recovery output when bootstrap is attempted but fails.
- Use the explicit disabled summary only when the operator supplied the opt-out.

The flag is process-local and applies to every tenant in one `add-tenants` invocation, matching the current shared-option behavior of `--no-health` and `--no-ssh`.

## Onboarding Propagation

### Python CLI

`TenantConfig` gains `no_weechat_bootstrap: bool = False`. Serialization and resume behavior preserve the field. `build_add_tenant_args` appends `--no-weechat-bootstrap` when true. The interactive prompt defaults to automatic configuration, and the summary shows whether relay bootstrap is enabled.

### Browser onboarding

`ProvisionRequest` gains `no_weechat_bootstrap: bool = False` and forwards it through `TenantConfig`. The provisioning wizard gains a default-enabled WeeChat relay bootstrap checkbox. The request payload inverts the checkbox into `no_weechat_bootstrap`, matching the existing SSH and health controls.

### OpenRC bulk provisioning

The bulk provisioner gains an enabled-by-default WeeChat bootstrap setting and passes `--no-weechat-bootstrap` to `add-tenants` only when disabled.

## Error and Recovery Behavior

The opt-out performs no relay validation and emits no recovery warning because skipping is deliberate. Failure to write the minimal credential environment remains a nonfatal provisioning warning because the WeeChat service cannot start safely without it. The WeeChat channel remains unavailable until relay configuration is added later. Recovery remains:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-weechat-relay <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh render-units <tenant>
sudo ic/scripts/lunarwing-weechat-preflight.sh <tenant>
```

`configure-weechat-relay` retains preserve-and-fail semantics for existing non-empty WeeChat configuration.

## Testing

Shell coverage will prove:

- Both tenant-creation commands accept `--no-weechat-bootstrap`.
- Default tenant creation still invokes bootstrap after environment generation and before PostgreSQL provisioning.
- Opted-out tenant creation never invokes `configure_weechat_relay`.
- Opted-out tenant creation still writes a mode-`0600` `weechat.env` containing only `RELAY_PASSWORD`.
- Base provisioning continues in the opted-out path.
- The summary distinguishes `configured`, `needs recovery`, and intentional `disabled` states.
- Existing bootstrap, service-rendering, preflight, and Kawarimi tests remain green.

Python and web coverage will prove:

- `TenantConfig` round-trips `no_weechat_bootstrap` through serialized resume data.
- The Python argument builder appends the flag only when requested.
- `ProvisionRequest` forwards the field.
- The web payload defaults to automatic bootstrap and sends the opt-out when unchecked.

## Documentation

Update mt-admin help, Python/web onboarding references, `docs/specs/WeeChat-Relay.md`, and `docs/ops/WEECHAT-SERVICES.md`. Documentation must state that this is a bootstrap opt-out, not a WeeChat service-disable switch, and show the explicit recovery command.

## Non-Goals

- Disabling or removing WeeChat services
- Persisting a tenant-level runtime WeeChat enablement setting
- Changing Kawarimi import behavior
- Changing relay security, ports, credentials, or preserve-and-fail behavior
- Adding a general feature registry or refactoring the positional `add_tenant` interface
