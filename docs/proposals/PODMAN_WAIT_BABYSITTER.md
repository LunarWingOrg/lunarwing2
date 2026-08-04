# Podman Wait Babysitter Pattern

> **Current status (updated 2026-08-04): IMPLEMENTED.** The helper, OpenRC
> unit renderer, register/deregister lifecycle, worker and PostgreSQL wiring, and
> `pg_isready` status check are in current source. Supported worker babysitters
> are Pebble, OpenCode, and Kimi Code; Nanocode references below are historical.
> Helper ownership belongs to mt-admin rather than the watchdog installer. See
> [`../specs/podman-wait-babysitter.md`](../specs/podman-wait-babysitter.md) for
> the current as-built contract.

## Overview

The podman wait babysitter provides docker-parity crash recovery for rootless Podman containers on OpenRC by using OpenRC's `supervise-daemon` to supervise a foreground process that blocks on `podman wait <container>`. When the container exits, `podman wait` returns, the supervised process exits, and `supervise-daemon` respawns it (which starts the container again).

## Architecture

### Components

1. **Helper Script** (`/usr/local/sbin/lunarwing-ctr-babysit`)
   - Takes container name as argument
   - Ensures container is started: `podman start <ctr> || true`
   - Blocks on container exit: `exec podman wait <ctr>`
   - Runs as tenant user with proper rootless environment (HOME, XDG_RUNTIME_DIR)

2. **Babysitter OpenRC Units** (`/etc/init.d/lunarwing-<type>-<tenant>-sup`)
   - One per container type: pg, pebble, opencode, kimi
   - Uses `supervisor="supervise-daemon"`
   - Foreground command: `lunarwing-ctr-babysit <container-name>`
   - Runs as tenant user via `command_user`
   - Respawn policy: `respawn_delay=2 respawn_max=10 respawn_period=120`

3. **Container Units** (existing, unchanged)
   - `lunarwing-pg-<t>`, `lunarwing-pebble-<t>`, `lunarwing-opencode-<t>`, `lunarwing-kimi-<t>`
   - Continue to provide health-aware `status()` for health-openrc.sh
   - Started/stopped by tenant lifecycle, not by babysitter

### Unit Naming

| Container Unit | Babysitter Unit |
|----------------|-----------------|
| `lunarwing-pg-<tenant>` | `lunarwing-pg-<tenant>-sup` |
| `lunarwing-pebble-<tenant>` | `lunarwing-pebble-<tenant>-sup` |
| `lunarwing-opencode-<tenant>` | `lunarwing-opencode-<tenant>-sup` |
| `lunarwing-kimi-<tenant>` | `lunarwing-kimi-<tenant>-sup` |

## Integration Points

### Helper Script Installation

Installed idempotently by `lunarwing-mt-admin.sh` via `ensure_babysitter_helper()` during tenant lifecycle operations. The watchdog installer historically copied this helper too, but helper provisioning is now owned by mt-admin so tenants work even if the watchdog is not installed yet.

### Tenant Lifecycle

In `lunarwing-mt-admin.sh`:
- `render_tenant_openrc_units`: render babysitter unit for PG container
- `_register_worker_unit`: register babysitters for Pebble, OpenCode, and Kimi Code
- `start_tenant_openrc`: start + boot-enable all babysitter units
- `stop_tenant_openrc`: stop babysitter units before container units
- `uninstall_tenant_openrc`: disable + remove babysitter units
- `status_tenant`: display babysitter unit status

### Worker Registration

`_register_worker_unit()` renders/registers babysitters for Pebble, OpenCode,
and Kimi Code.

## PG Status() Fix

The `lunarwing-pg-<t>` unit's `status()` function checks both container liveness AND `pg_isready`:

```sh
status() {
    if [ "$(_pg inspect -f '{{.State.Running}}' "${pg_container}" 2>/dev/null)" = "true" ] \
       && _pg exec "${pg_container}" pg_isready -U lunarwing -q -t 3 2>/dev/null; then
        einfo "${pg_container}: started"; return 0
    fi
    einfo "${pg_container}: stopped"; return 3
}
```

This closes the "running but wedged" false-healthy hole where a container running with a crashed/non-responsive Postgres would incorrectly report healthy.

## Respawn Policy

```sh
respawn_delay=2      # 2 seconds between respawns
respawn_max=10       # Max 10 respawns
respawn_period=120   # Within 120 seconds
```

After 10 rapid respawns, OpenRC marks the unit `crashed`; mt-admin's OpenRC
health/self-heal schedule (five minutes by default) becomes the backstop.

## Environment Threading

Babysitter runs as tenant user via `command_user="${tenant}:${tenant}"` with rootless env set in `start_pre()`:

```sh
start_pre() {
    checkpath -d -m 0700 -o "${babysitter_user}:${babysitter_user}" "/run/user/${babysitter_uid}"
    export HOME="${babysitter_home}" XDG_RUNTIME_DIR="/run/user/${babysitter_uid}"
}
```

## Health Pipeline Compatibility

Babysitter units are auto-discovered by `health-openrc.sh` via the existing glob `/etc/init.d/lunarwing-*`. The `-sup` suffix ensures they are covered by the same self-heal sweep that handles the main daemon units, providing a backstop after the `respawn_max` cap is exceeded.

## Verification

Fault injection test:
```sh
# As tenant
podman kill lunarwing-pg-<tenant>
# Watch: rc-service lunarwing-pg-<tenant>-sup status → restarts within seconds
# Self-heal sweep no longer needed for container crashes
```

## Files Created

- `ic/scripts/lunarwing-ctr-babysit.sh`

## Files Modified

- `ic/scripts/lunarwing-mt-admin.sh` — helper installation/cleanup,
  `render_container_babysitter_unit`, register/deregister helpers, and lifecycle
  wiring
