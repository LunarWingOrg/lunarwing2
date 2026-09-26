# Troubleshooting — Half-Broken Tenants After an Upgrade

Operational guide for the class of failure where a tenant survives
`upgrade-tenant` + `restart-tenant` looking healthy but one subsystem is
silently broken because **a container image is missing from the tenant's
runtime store** (or an older lifecycle path checks the wrong store).

Written from two live incidents:

- **WI#240 / WI#238** — sandboxed shell tool dead on tenants provisioned after
  v2.1.1.0 because `lunarwing-sandbox:latest` existed in no image store at
  all. (Root cause of the "potential podman env prob": not an env problem.)
- **WI#229** — LunarVoice sidecar unrecoverable through the normal lifecycle
  path after upgrade: `start_tenant_voice` aborted on an image check against
  the wrong (admin/root) store.

Upgrades can empty the admin (root) image store, leaving no source for future
distribution. A tenant with an existing image can still run on the current
mt-admin; a tenant without one cannot. Watch for this shape after *any*
upgrade, in subsystems that depend on locally built images (sandbox,
LunarVoice, LunarVision, workers).

---

## 1. Symptom: every sandboxed shell call fails (WI#240 / WI#238)

### What you see

- The tenant daemon is up, `mt-admin status <tenant>` is healthy, chat works.
- Every tool call that needs the execution sandbox fails immediately at
  image resolution — before any command runs. The error mentions the sandbox
  image and/or a TTY prompt for short-name resolution.

### Why it happens

The tenant's `lunarwing.env` sets `SANDBOX_IMAGE=lunarwing-sandbox:latest`.
That image is **local-only**: nothing publishes it to a registry; it exists
only by being *built* from `ic/docker/sandbox.Dockerfile` (via
`mt-admin build-sandbox-image`) and *distributed* into each tenant's rootless
store by `ensure_tenant_sandbox_image` (WI#223). It can be absent because:

1. **The admin store was emptied** — a host upgrade, image prune, or store
   rebuild dropped the admin (root) copy. Per-tenant distribution steps then
   have no source image to copy and only warn (non-fatal).
2. **The provisioning-time build/distribute step silently failed** — the
   warning was journal-only and log retention ate it, so the tenant went live
   with no image anywhere.
3. **`SANDBOX_AUTO_PULL=true` is a trap**, not a fix, on such hosts:
   - Podman short-name resolution enforcement is on by default, so an
     unqualified `lunarwing-sandbox` reference cannot even attempt a pull
     without a TTY prompt (daemon context: no TTY → instant failure).
   - Even with that off, there is nothing to pull — no registry carries the
     image.

Note the drift hazard: WI#223 env generation writes `SANDBOX_AUTO_PULL=false`.
If a tenant env says `true`, something drifted post-provisioning — the same
drift path by which the image check went sideways. Treat unexpected env drift
as a signal to re-verify the image stores, not just the env value.

### Diagnosis

Check the selected sandbox runtime's admin store and, for Podman, the tenant's
rootless store. `SANDBOX_RUNTIME=docker` prefers Docker; `auto`/`podman` prefers
Podman if installed (with CLI fallback as in `sandbox_container_runtime`). The
mt-admin worker runtime may be different. Use mt-admin for tenant state; never
bare system-bus `systemctl` against tenant units. Do not print the whole env
file: it may contain secrets.

```bash
# 1. What does the tenant expect, and is AUTO_PULL drifted?
tenant=YOUR_TENANT  # replace with the actual tenant name
grep -E '^SANDBOX_(IMAGE|RUNTIME|AUTO_PULL)=' \
  "/home/$tenant/lunarwing/env/lunarwing.env"   # mt-admin tenant_env_dir

# 2. If the selected sandbox runtime is Podman, inspect BOTH stores:
sudo podman image inspect lunarwing-sandbox:latest >/dev/null \
  && echo "Podman admin store: present" || echo "Podman admin store: MISSING"

# Match mt-admin's _ctr: start from a tenant-traversable directory and set
# HOME and XDG_RUNTIME_DIR for the tenant's rootless Podman store.
tenant_uid="$(id -u "$tenant")"
tenant_home="$(getent passwd "$tenant" | cut -d: -f6)"
( cd / && sudo -u "$tenant" env HOME="$tenant_home" \
    XDG_RUNTIME_DIR="/run/user/$tenant_uid" \
    podman image inspect lunarwing-sandbox:latest >/dev/null ) \
  && echo "Podman tenant store: present" || echo "Podman tenant store: MISSING"

# If the selected sandbox runtime is Docker instead, the rootful Docker store
# is the daemon's runtime store; there is no separate tenant rootless store:
sudo docker image inspect lunarwing-sandbox:latest >/dev/null \
  && echo "Docker runtime store: present" || echo "Docker runtime store: MISSING"
```

If the env expects the managed image and the **runtime store** lacks it
(tenant Podman or shared Docker), this is an image-availability failure. If
only the Podman admin copy is missing, an already-provisioned tenant image
still runs; distribution of a *new* image cannot proceed until the admin
copy is rebuilt. Confirm actual sandbox failure with a real tool call before
declaring the tenant broken.

### Recovery when the runtime image is missing

```bash
# 1. Build into the admin store of this tenant's selected sandbox runtime
#    (not necessarily mt-admin's worker runtime):
sudo ic/scripts/lunarwing-mt-admin.sh build-sandbox-image <tenant>

# 2. Distribute into the affected Podman tenant's store (Docker uses the
#    shared rootful store). start-tenant reconciles this (WI#223 safety net)
#    and prints a recovery hint if it still fails:
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <tenant>

# 3. Decisive test: dispatch a real shell task through the tenant's daemon —
#    a `sandbox-OK`-style reply means the wall is gone.
```

### After-action (prevent recurrence next upgrade)

- Align the env with WI#223's design: `SANDBOX_AUTO_PULL=false` for tenants on
  the managed image. With `true`, a missing runtime image may trigger a futile
  registry pull (or a non-interactive short-name prompt) instead of recovering.
- After `upgrade-tenant` (or host image-store maintenance), check the
  selected runtime store and exercise a sandboxed shell call. Rebuild the
  admin image when a future distribution/update needs it; an existing tenant
  image alone is sufficient to run until then.

---

## 2. Same defect class: LunarVoice won't start after upgrade (WI#229)

### What you see

```
ERROR: LunarVoice image unavailable; run 'build-voice-sidecar' first
```

…during `restart-tenant` / `start-tenant`, even though the image *is*
present — in the **tenant's** rootless store. The voice unit was left
`failed` (e.g. status=137 from a SIGKILL during the upgrade's stop wave) and
never recovers, because the start path aborts *before* the
`reset-failed` + `start` that would clear it.

### Why it happened

`_ensure_tenant_image()` (`ic/scripts/mt-admin-lib/02-containers.sh`) treated
the **admin (root) store** as the source of truth and returned failure when
the admin copy was gone — even when the tenant store already had the correct
image. An upgrade that pruned/rebuilt the host store removed the admin copy,
and nothing refilled it, so voice could never be started by the lifecycle
path again — only by hand.

**Fixed in mt-admin** (commit `74da5a59e`, v2.1.2.0 late-cycle): the voice
start path no longer strands a tenant on the admin-store check. If you see
this error on a current checkout, **upgrade the mt-admin tree first** —
do not hand-copy images to paper over the old check.

### Recovery on a host still running the old mt-admin

```bash
# Refill the admin store, then let the lifecycle path do the rest:
sudo ic/scripts/lunarwing-mt-admin.sh build-voice-sidecar
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <tenant>
```

Same pattern applies to LunarVision and worker images: if a "start" step
claims a locally built image is unavailable, check **which store** the check
consulted before rebuilding anything.

---

## 3. General post-upgrade checklist for image-dependent subsystems

Run this after `upgrade-tenant` on any tenant that was provisioned or last
upgraded under a different release — especially any tenant "left in a
half-broken state":

1. `mt-admin status <tenant>` — daemon, bridge, sidecars, workers all report
   up. (Never bare system-bus `systemctl` for tenant units.)
2. For every locally built image the tenant uses (`lunarwing-sandbox:latest`,
   `lunarwing/voice-service:latest`, the LunarVision image, worker images):
   verify it exists in the store used by that subsystem's runtime. For a
   rootless Podman tenant, check its store with the section 1 probe; for
   rootful Docker, check the shared Docker store. Check the corresponding
   admin store separately if you need to distribute an updated image. A
   missing admin copy alone does not make an existing tenant image unusable.
3. Grep the tenant env for drift against the current release's generation
   defaults (e.g. `SANDBOX_AUTO_PULL=false`). Drift implies the provisioning
   or upgrade run diverged — re-verify everything that run was supposed to
   do, don't just fix the one value.
4. Exercise one real end-to-end call per subsystem (one sandboxed shell
   command, one voice ping, one OCR request). "Unit is active" is not proof
   the subsystem works — the WI#238 tenant had a healthy daemon and a dead
   shell tool.
5. Check the saved output of the `mt-admin` provisioning/upgrade invocation
   for non-fatal `WARNING`s (they may not be in service logs). For daemon
   warnings during the incident, use the tenant's init-specific logs:

   ```bash
   # systemd user manager (not the system journal's tenant units):
   tenant=YOUR_TENANT  # replace with the actual tenant name
   sudo -u "$tenant" env XDG_RUNTIME_DIR="/run/user/$(id -u "$tenant")" \
     journalctl --user -u "lunarwing-${tenant}.service" --since '-2 days' \
     | grep -i warning

   # OpenRC: rendered daemon stdout/stderr logs under the tenant home:
   grep -i warning "/home/$tenant/lunarwing/logs/lunarwing.log" \
     "/home/$tenant/lunarwing/logs/lunarwing.err"
   ```

## Related

- WI#223 — managed sandbox image build/distribute design
  (`SANDBOX_AUTO_PULL=false` default, `build-sandbox-image` recovery verb).
- WI#229 — LunarVoice image-check-against-wrong-store bug + mt-admin fix.
- WI#238 / WI#240 — sandbox-image-missing incident and this guide.
- `docs/ops/MULTITENANCY-PRODUCTION.md` — general MT troubleshooting
  (services after reboot, OOM builds, docker access, gateway binding).
