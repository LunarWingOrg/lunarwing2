# Spec: podman-wait-babysitter

Scope: feature

# Podman Wait Babysitter Pattern

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
   - One per container type: pg, vision, pebble, opencode, kimi
   - Uses `supervisor="supervise-daemon"`
   - Foreground command: `lunarwing-ctr-babysit <container-name>`
   - Runs as tenant user via `command_user`
   - Respawn policy: `respawn_delay=2 respawn_max=10 respawn_period=120`

3. **Container Units** (existing, unchanged)
   - `lunarwing-pg-<t>`, `lunarwing-vision-<t>`, `lunarwing-pebble-<t>`, `lunarwing-opencode-<t>`, `lunarwing-kimi-<t>`
   - Continue to provide health-aware `status()` for health-openrc.sh
   - Started/stopped by tenant lifecycle, not by babysitter

### Unit Naming

| Container Unit | Babysitter Unit |
|----------------|-----------------|
| `lunarwing-pg-<tenant>` | `lunarwing-pg-<tenant>-sup` |
| `lunarwing-vision-<tenant>` | `lunarwing-vision-<tenant>-sup` |
| `lunarwing-pebble-<tenant>` | `lunarwing-pebble-<tenant>-sup` |
| `lunarwing-opencode-<tenant>` | `lunarwing-opencode-<tenant>-sup` |
| `lunarwing-kimi-<tenant>` | `lunarwing-kimi-<tenant>-sup` |

## Integration Points

### Helper Script Installation

Installed idempotently by `lunarwing-mt-admin.sh`'s `ensure_babysitter_helper()` to `/usr/local/sbin/lunarwing-ctr-babysit` during tenant lifecycle operations.

### Tenant Lifecycle

In `lunarwing-mt-admin.sh`:
- `start_tenant_openrc`: render + enable + start babysitter units
- `stop_tenant_openrc`: stop babysitter units
- `uninstall_tenant_openrc`: disable + remove babysitter units
- `status_tenant`: show babysitter unit status
- `stop-worker` / `kill-worker`: persist `desired_running=false`, stop direct and
  babysitter units, and remove both from all runlevels
- `start-worker`: persist `desired_running=true`, then re-enable and start both
  units through normal worker registration

`start-tenant` reconciles unselected and intentionally stopped workers by
stopping stale units and removing stale runlevel links. Health reports those
down units as `skipped`, so periodic remediation cannot override operator
intent.

### Registration

`_register_worker_unit()` renders/registers babysitters for Pebble, OpenCode, and
Kimi Code. `_register_vision_openrc()` does the same for LunarVision after its
container passes the dedicated `8089/health` probe. Its direct unit retains
endpoint-aware `status()`, while the supervisor supplies fast crash recovery.

## PG Status() Fix

The `lunarwing-pg-<t>` unit's `status()` function changed from:
```sh
status() {
    if [ "$(_pg inspect -f '{{.State.Running}}' "${pg_container}" 2>/dev/null)" = "true" ]; then
        einfo "${pg_container}: started"; return 0
    fi
    einfo "${pg_container}: stopped"; return 3
}
```

To:
```sh
status() {
    # Health-aware: container running AND pg_isready succeeds
    if [ "$(_pg inspect -f '{{.State.Running}}' "${pg_container}" 2>/dev/null)" = "true" ] && \
       _pg exec "${pg_container}" pg_isready -U lunarwing -q 2>/dev/null; then
        einfo "${pg_container}: started"; return 0
    fi
    einfo "${pg_container}: stopped"; return 3
}
```

This closes the "running but wedged" false-healthy hole.

## Respawn Policy

```sh
respawn_delay=2      # 2 seconds between respawns
respawn_max=10       # Max 10 respawns
respawn_period=120   # Within 120 seconds
```

After 10 rapid respawns, OpenRC marks unit `crashed` → self-heal (5-min sweep) becomes backstop.

## Environment Threading

Babysitter runs as tenant user via `command_user="${tenant}:${tenant}"` but needs rootless env:
```sh
start_pre() {
    checkpath -d -m 0700 -o "${tenant}:${tenant}" "/run/user/${uid}"
    export HOME="${home}" XDG_RUNTIME_DIR="/run/user/${uid}"
}
```

## Verification

Fault injection test:
```sh
# Named container types currently cover pg, pebble, opencode, and kimi.
ic/scripts/fault-inject-respawn.sh --tenant <tenant> --container-type opencode

# LunarVision uses the explicit-container form.
ic/scripts/fault-inject-respawn.sh \
  --container lunarwing-vision-<tenant> --user <tenant>

# Watch the matching -sup service; it should restart within seconds.
# The periodic self-heal sweep remains the bounded backstop.
```

## Files Modified

- `ic/scripts/lunarwing-ctr-babysit.sh` (new)
- `ic/scripts/lunarwing-mt-admin.sh` (render_container_babysitter_unit, ensure_babysitter_helper, wiring)
- `ic/scripts/install-lunarwing-watchdog.sh` (removed helper installation — now owned by mt-admin)
