# WI#734 — LunarDepth external-worker test (`lunardepthtester`, AJ edition, second attempt)

> Test started 2026-11-16 by Applejack on branch
> `aj/kimi-wi734-20261116-0731`, whose head contains the target
> `aragents/kimil00p/v2.2.0.0` at `f0d47a882`.
> This is the second attempt at the WI#720 test; !2487 was closed without
> merging and its review (MR !2487 notes 17864 and 17866) is the input for this
> report. The goals file is deliberately unchanged; this is a work-item run.

## Scope

Work item #734 asks for a tenant named `lunardepthtester` with the LunarDepth
external worker and `--with-toolchains` where available, jobs of medium to high
complexity, and a record of the results and every hiccup. This attempt differs
from !2487 in four ways:

1. The test runs on the host's **own** service manager — no
   `LUNARWING_SERVICE_MANAGER` override of any kind.
2. The state in which the tenant name is found is recorded before anything is
   done, and nothing is removed or reused without the maintainer's word.
3. The run is finished — build, runtime and jobs — before the MR is opened.
4. Each hiccup is recorded as what it was: a finding about the call, the
   product, or the host.

## Host and preflight

Recorded before the first `mt-admin` command, on the host this run used:

| Fact | Value |
| --- | --- |
| Distribution | Arch Linux (rolling), per `/etc/os-release` |
| PID 1 | `systemd` (`/proc/1/comm`) |
| `rc-service` | absent (`command -v rc-service` → not found) |
| `systemctl` | `/usr/sbin/systemctl` |
| Kernel | `6.18.54-2-lts` (`uname -a`, `6.18.54-2-lts #1 SMP PREEMPT_DYNAMIC`) |
| Podman | `6.2.0`, rootless |
| CPUs | 12 (`nproc`); every build was confined with `taskset -c 0-11` |
| RAM / disk at start | 17 GiB available of 23 GiB; 173 GiB free on `/` |
| LSM | `capability,landlock,lockdown,yama,bpf` — Landlock present |
| `setpriv`, `tmux`, `taskset`, `jq` | present |
| Health pipeline | `/etc/lunarwing/health.env` reads `LUNARWING_SERVICE_MANAGER=systemd` |
| Existing tenants | `hermes` (base 10040) and `antelope` (base 10000) |

No command of this run sets `LUNARWING_SERVICE_MANAGER`. The service manager is
whatever `ic/scripts/mt-admin-lib/01-detect.sh` finds on this host, which is
systemd.

### State in which the tenant name was found

Checked before the first `mt-admin` command:

- `getent passwd lunardepthtester` → no such account.
- `/etc/lunarwing/ports.json` → `tenants` holds `hermes` and `antelope` only; no
  `lunardepthtester` entry, so ports 10010–10019 and 52010 were unallocated.
- No `/home/lunardepthtester`.
- `/etc/lunarwing/health.env` is `systemd` and a `health.env.pre-fix-20261115`
  backup is present from the host's own recovery.

So the account and registry entry that the previous attempt left on this host
(`/tmp/wi720-*` logs at 22:02–22:09Z on 2026-11-15 are from it) are gone. I
removed nothing and reused nothing; the tenant was created fresh under its own
name. No maintainer decision was needed on this item.

### Model route

- TensorZero `http://100.88.232.146:3000/status` returned HTTP 200, version
  `2026.6.0`.
- An unauthenticated OpenAI-compatible request with
  `tensorzero::function_name::lunarwing` returned HTTP 200 with `finish_reason=stop`
  and the exact marker `WI734_ROUTE_OK` (43 completion tokens; the first probe
  at `max_tokens=40` stopped on `length` mid-reasoning, which is a budget
  artifact, not a route failure).
- The lane therefore uses `--lunardepth-allow-unauthenticated-gateway` and the
  private-network cleartext exception, as WI#719 and WI#685 did.

## Provisioning

Provisioning began at 07:32Z with no service-manager override:

```bash
sudo taskset -c 0-11 bash ic/scripts/lunarwing-mt-admin.sh add-tenant lunardepthtester \
  --no-ssh --no-weechat-bootstrap \
  --llm-base-url http://100.88.232.146:3000/openai/v1 \
  --with-lunardepth \
  --lunardepth-base-url http://100.88.232.146:3000/openai/v1 \
  --lunardepth-model tensorzero::function_name::lunarwing \
  --lunardepth-allow-insecure-http \
  --lunardepth-allow-unauthenticated-gateway
```

`ADD_EXIT=0`. What it did, from `/tmp/wi734-add.log`:

- Allocated block 10010–10019 and LunarDepth 52010/52011; registry entry
  `workers.lunardepth=true`, all other lanes false.
- Created the account, enabled linger, and rendered **systemd user units** into
  `/home/lunardepthtester/.config/systemd/user`
  (`lunarwing-lunardepthtester.service`, `xmpp-bridge-lunardepthtester.service`,
  the WeeChat pair, `lunarwing-build.slice`, `lunarwing-workloads.slice`). This
  is the host's own manager, chosen by detection, not by an override.
- Started PostgreSQL as the user quadlet `lunarwing-pg-lunardepthtester` on
  10013.
- Distributed the generic sandbox image into the tenant's rootless store.
- Left `/etc/lunarwing/health.env` alone (`already exists (preserving)`) and
  scheduled the host pipeline through its **systemd timer**.
- Wrote the LunarDepth external-worker block with the explicit `tensorzero::`
  route and the unauthenticated-gateway exception; no key stored.
- Tenant clone at `d872e9632` on branch `aj/kimi-wi734-20261116-0731`, whose
  history contains `f0d47a882` (the target `aragents/kimil00p/v2.2.0.0`).

One hiccup at provisioning, non-blocking and already a tracked bug:

- **H1 (host, cosmetic):** `WARNING: could not pre-warm the rootless podman
  userns for lunardepthtester; the first podman call will create it in its own
  context (see docs/bugs/BUG-workload-scope-podman-userns.md)`. Image
  distribution and the PostgreSQL quadlet both succeeded afterwards, so the
  deferred userns creation cost one slow first call and nothing else.

### First mt-admin command output (abridged)

```text
=== Adding tenant: lunardepthtester ===
allocated port block 10010-10019 for tenant 'lunardepthtester'
created user: lunardepthtester
```

## Build

## Runtime audits

## Job exercise

## Findings and hand-off
