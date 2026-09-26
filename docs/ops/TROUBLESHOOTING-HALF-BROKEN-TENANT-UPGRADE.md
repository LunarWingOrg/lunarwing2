# Troubleshooting — Half-Broken Tenants After an Upgrade

Operational guide for the class of failure where a tenant survives
`upgrade-tenant` + `restart-tenant` looking healthy but one subsystem is
silently broken because **a container image the tenant needs is missing from
the store the lifecycle path consults**.

Written from two live incidents:

- **WI#240 / WI#238** — sandboxed shell tool dead on tenants provisioned after
  v2.1.1.0 because `lunarwing-sandbox:latest` existed in no image store at
  all. (Root cause of the "potential podman env prob": not an env problem.)
- **WI#229** — LunarVoice sidecar unrecoverable through the normal lifecycle
  path after upgrade: `start_tenant_voice` aborted on an image check against
  the wrong (admin/root) store.

Same defect class both times: **upgrades can empty the admin (root) image
store, and per-tenant "get stuff distributed" steps silently keep assuming it
is full.** Watch for this shape after *any* upgrade, in any subsystem that
depends on a locally built image (sandbox, LunarVoice, LunarVision, workers).

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

Check all three stores. Use mt-admin for tenant state; never bare
system-bus `systemctl` against tenant units.

```bash
# 1. What does the tenant expect, and is AUTO_PULL drifted?
grep -E '^SANDBOX_(IMAGE|RUNTIME|AUTO_PULL)=' \
  /home/<tenant>/lunarwing/env/lunarwing.env   # mt-admin tenant_env_dir

# 2. Admin (root) store
sudo podman image inspect lunarwing-sandbox:latest >/dev/null \
  && echo "admin store: present" || echo "admin store: MISSING"

# 3. Tenant rootless store (as the tenant user)
sudo -u <tenant> podman image inspect lunarwing-sandbox:latest >/dev/null \
  && echo "tenant store: present" || echo "tenant store: MISSING"
```

If all of: env expects the managed image + both stores missing (or admin
missing so distribution has no source) → this is your bug.

### Fix (verified live on two tenants)

```bash
# 1. Build into the ADMIN store:
sudo ic/scripts/lunarwing-mt-admin.sh build-sandbox-image

# 2. Distribute into the affected tenant's store and clear the failed state.
#    start-tenant reconciles the rootless store itself (WI#223 safety net)
#    and prints the recovery hint if it still fails:
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <tenant>
#    (explicit equivalent: build-sandbox-image <tenant>, then start-tenant)

# 3. Decisive test: dispatch a real shell task through the tenant's daemon —
#    a `sandbox-OK`-style reply means the wall is gone.
```

### After-action (prevent recurrence next upgrade)

- Align the env with WI#223's design: `SANDBOX_AUTO_PULL=false` for tenants on
  the managed image. With `true`, the *next* upgrade that empties the admin
  store re-creates this exact failure.
- Treat sandbox-image distribution as a post-upgrade lifecycle step:
  after any `upgrade-tenant` (or host image-store maintenance), re-check
  section 1's three probes before declaring the tenant upgraded.

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
   verify it exists in the **admin store** *and* the **tenant rootless
   store** (probes in section 1).
3. Grep the tenant env for drift against the current release's generation
   defaults (e.g. `SANDBOX_AUTO_PULL=false`). Drift implies the provisioning
   or upgrade run diverged — re-verify everything that run was supposed to
   do, don't just fix the one value.
4. Exercise one real end-to-end call per subsystem (one sandboxed shell
   command, one voice ping, one OCR request). "Unit is active" is not proof
   the subsystem works — the WI#238 tenant had a healthy daemon and a dead
   shell tool.
5. Check the journal for non-fatal WARNINGs from the provisioning/upgrade
   run before retention eats them:

   ```bash
   sudo journalctl -u 'lunarwing-*' --since '-2 days' | grep -i warning
   ```

## Related

- WI#223 — managed sandbox image build/distribute design
  (`SANDBOX_AUTO_PULL=false` default, `build-sandbox-image` recovery verb).
- WI#229 — LunarVoice image-check-against-wrong-store bug + mt-admin fix.
- WI#238 / WI#240 — sandbox-image-missing incident and this guide.
- `docs/ops/MULTITENANCY-PRODUCTION.md` — general MT troubleshooting
  (services after reboot, OOM builds, docker access, gateway binding).
