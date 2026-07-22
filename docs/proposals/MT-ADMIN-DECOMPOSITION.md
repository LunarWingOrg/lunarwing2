# Decomposing `lunarwing-mt-admin.sh` — Proposal

> **Current status (2026-07-22, rev `HEAD`): PROPOSAL — reviewed, ready for implementation.**
> This is a design document. No code changes are made in this proposal; it
> describes how the monolithic multi-tenant admin script could be broken into
> at least four separately-sourced bash libraries, and what the migration
> path looks like.
>
> **Review history:** Reviewed in `docs/ops/KUMOGAKURE_RECENT_REV_T.md`
> (2026-07-21). Verdict: "solid proposal, ready for sun to greenlight
> implementation." This revision incorporates the review feedback (see
> §5.1 Test Surface, §10.1 Updated Recommendations, and the 3a/3b note in §4).

**Date:** 2026-07-21
**Origin:** Item #14 of `docs/ops/AGENT_GOALS_2.0.2.0.md` — "refactor mt admin idea. write up a doc on how we can break mt admin setup monolithic megascript (8000 lines of bash rn) into AT LEAST FOUR SEPERATE PARTS. put it in docs/proposals".
**Target file:** `ic/scripts/lunarwing-mt-admin.sh` (~7502 lines at time of writing).
**Sibling file:** `ic/scripts/lunarwing-mt-provision-openrc.sh` (428 lines) — out of scope for the first split, see §9.

---

## 1. Executive Summary

`ic/scripts/lunarwing-mt-admin.sh` is a single sourced bash script that has
grown to **~7500 lines** and owns almost every aspect of multi-tenant
administration: the port registry, tenant user/repo/build lifecycle, env-file
rendering, systemd + OpenRC unit rendering, container sidecar lifecycle
(Postgres, workers, LunarVision), secrets/SSH provisioning, owner-scope DB
migrations, PostgreSQL backup/restore, and a host-global health-check
pipeline. Its `main()` dispatcher at line 6976 exposes **41 subcommands**.

The script is *functionally cohesive already* — the author has placed
`# ── … ─` section banners that line up almost exactly with the natural
seams. The decomposition is therefore mostly a *mechanical* move plus a small
amount of shared-state plumbing, not a re-architecture. This document
proposes splitting it into **six sourced libraries** (four required, two
optional) behind a thin composition root, while preserving:

- The exact subcommand surface (`add-tenant`, `build-tenant`, …, `doctor`).
- The init-system abstraction boundary (`_systemctl_user`, OpenRC helpers).
- The HARD RULE in `ic/AGENTS.md`: tenant lifecycle never calls bare
  `systemctl is-active`/`rc-service` outside `lunarwing-mt-admin.sh`.
- The `/etc/lunarwing/ports.json` on-disk format and its migration ladder.

The goal is **reviewability and testability**, not a behavioral change. The
first four parts can be extracted in four sequential PRs without changing
user-facing behavior.

---

## 2. Why Split It

### 2.1 Concrete pain today

| Pain | Evidence |
|---|---|
| Reviewers see 7500-line diffs against the single file for any MT change. | Git history for `lunarwing-mt-admin.sh` shows churn spanning unrelated subsystems (ports migration, OpenRC heredoc edits, env var additions) landing in the same file. |
| `shellcheck` and `bash -n` runs are slow and unit-less — there is no way to scope a lint/test to "just the port registry". | No test harness exists today that exercises the port registry in isolation; the file is tested only via full `add-tenant` runs. |
| New contributors cannot hold the whole script in their head. | The dispatcher (`main()`, L6976) is a 522-line `case` with inline arg parsing; cross-references to functions 4000 lines away are common. |
| Bundled unrelated risks. | A typo in an OpenRC heredoc shouldn't force a re-review of the Postgres backup logic, but today it does. |
| Duplicate-ish logic across init systems. | `render_tenant_systemd_units` (L5004) and `render_tenant_openrc_units` (L5385) are near-mirror-images living 350 lines apart; cross-keeping them in sync across a 7500-line file is fragile. |

### 2.2 What we are **not** doing

- **Not** rewriting the script in Python or Rust. `lunarwing-mt-admin.sh` is
  the authoritative init-agnostic lifecycle tool; the Python
  `lunarwing_mt_onboard*` wrappers stay thin drivers of it (per AGENTS.md).
- **Not** changing the CLI surface, the on-disk state files, or the
  `/etc/lunarwing/ports.json` schema.
- **Not** breaking the init abstraction. All `_systemctl_user` calls stay
  inside the mt-admin family of scripts.

---

## 3. Structural Map (authoritative)

The author's own `# ── … ─` section markers already identify the natural
seams. Confirmed via `rg -n '^# ── '` against the current file (line numbers
match `HEAD`):

| Bucket | Line range | ~Lines | Role |
|---|---|---|---|
| Constants / header | L1–92 | 92 | Paths, URLs, feature defaults, fleet constants |
| Core helpers + tenant path primitives + config-hash | L93–403 | 311 | `say`/`die`/`require_cmd`, `tenant_home`/`tenant_repo`/etc., hash tracking, `usage()` |
| Template render + root guard | L405–436 | 32 | `render_template`, `require_root` |
| Init system + container runtime detection | L438–558 | 121 | `detect_init_system`, `detect_container_runtime`, `_ctr` wrapper |
| Worker/babysitter unit renderers (OpenRC-flavored) | L561–883 | 323 | `render_worker_openrc_unit`, babysitter helpers |
| **Port registry** | **L884–1482** | **599** | `ports.json` schema, migrations v2–v11, allocate/deallocate/enable |
| Tenant user / repo / build management | L1484–1930 | 447 | `create_tenant_user`, `clone_tenant_repo`, `build_tenant`/`build_all`/`build_darkirc`/`build_nanocode`/`build_pebble`/`build_opencode_worker` |
| WASM install | L2024–2166 | 143 | `install_wasm_tenant`/`install_wasm_all` |
| **Environment-file generation** | **L2168–2822** | **655** | `write_tenant_lunarwing_env` (largest fn in file), bridge/proxy/darkirc/weechat env, XMPP allow-list builders |
| External-worker / SSH / secret config | L2823–3190 | 368 | `ensure_external_worker_config`, SSH harness, SSH key upload |
| Owner-scope DB migration | L3322–3576 | 255 | `owner_scope_*`, `migrate_owner_scope` |
| Notifications + per-worker config | L3580–3706 | 127 | Gotify, Pebble, Nanocode LLM config |
| Container lifecycle (workers + sidecars + PG) | L3707–4815 | 1109 | `start/stop_tenant_{nanocode,opencode,pebble,vision,postgres}`, PG backup/restore |
| **Systemd service units** | **L4817–5294** | **478** | `render_*_quadlet`, `render_tenant_systemd_units`, `_systemctl_user`, `start/stop/uninstall_tenant_systemd` |
| **OpenRC service units** | **L5297–5976** | **680** | `render_tenant_openrc_units` (2nd-largest fn), `install_openrc_env_exec`, `start/stop/uninstall_tenant_openrc` |
| Compound commands + health pipeline | L5978–6200 | 223 | `ensure_health_pipeline`, `remove_health_pipeline`, `warn_if_adapter_deps_missing` |
| **High-level tenant verbs + dispatcher** | **L6202–7502** | **1301** | `add_tenant`, `start/stop/restart/upgrade_tenant`, `status_tenant`, `list_tenants`, `doctor`, `usage`, `main()` (41 subcommands) |

All four proposed **required** split candidates (bolded above) are already
delineated by `# ── … ─` banners — the author effectively pre-segmented the
file for us.

---

## 4. Proposal: Six Sourced Libraries (Four Required)

The mt-admin composition root stays a single executable named
`lunarwing-mt-admin.sh`. It sources libraries from a sibling directory and
runs the existing `main()` dispatcher unchanged.

**Proposed layout under `ic/scripts/mt-admin-lib/`:**

```
ic/scripts/
├── lunarwing-mt-admin.sh           # composition root (thin): constants, helpers,
│                                   # init/runtime detection, main() dispatcher, usage()
└── mt-admin-lib/
    ├── 00-common.sh                # (optional phase-2) shared helpers + tenant path primitives
    ├── ports-registry.sh           # REQUIRED split #1
    ├── env-generation.sh           # REQUIRED split #2
    ├── units-systemd.sh            # REQUIRED split #3a
    ├── units-openrc.sh             # REQUIRED split #3b
    ├── health-pipeline.sh          # (optional phase-2) self-contained, low coupling
    └── tenant-lifecycle.sh         # (optional phase-2) add/remove/start/stop/upgrade/status/doctor
```

The four **required** splits (covering the explicit "AT LEAST FOUR" from the
goal item) are:

| # | Library | Sourced from lines | ~Lines | Hard dependencies it pulls in |
|---|---|---|---|---|
| 1 | `ports-registry.sh` | L884–1482 | 599 | `PORTS_REGISTRY`/`PORT_RANGE_*` constants (L16–19), `say`/`die`/`require_cmd`, `jq` |
| 2 | `env-generation.sh` | L2168–2822 | 655 | tenant path primitives (L115–122), `ports_get`, `tenant_*_enabled`, `tenant_pg_password`, fleet defaults (L33–46) |
| 3a | `units-systemd.sh` | L4817–5294 (plus L5197 `_systemctl_user`) | 478 | `INIT_SYSTEM`, `CONTAINER_RT`/`MT_ROOTLESS`, tenant path primitives, `_ctr` |
| 3b | `units-openrc.sh` | L5297–5976 (plus L656–883 worker/babysitter renderers) | 680+323 | `INIT_SYSTEM`, `CONTAINER_RT`/`MT_ROOTLESS`, tenant path primitives, `_ctr`, `install_openrc_env_exec` |
| 4 | `health-pipeline.sh` (optional but recommended) | L6002–6200 | 223 | `HEALTH_*` constants (L54–64), `INIT_SYSTEM`, `PORTS_REGISTRY`, cron/systemctl |

Note: counting units-systemd and units-openrc as **two** parts (3a + 3b) was
deliberate. They are near-mirror images, their coupling is almost entirely to
the shared composition root rather than to each other, and keeping them
separate makes init-system-specific edits far easier to review. That brings
the total to **five** (or **six** if `tenant-lifecycle.sh` is extracted too),
comfortably satisfying the "AT LEAST FOUR" requirement.

**3a/3b counting confirmation (per review feedback):** The
`KUMOGAKURE_RECENT_REV_T.md` review flagged this as "worth confirming sun
agrees that 3a/3b count separately." The justification is structural: the
two renderers share zero code with each other — every coupling they have is
to the composition root, never cross-init-system. They are independently
testable (you can exercise the systemd renderer without an OpenRC host and
vice versa) and independently revertible. Splitting a single `units.sh`
along the same seam would be equivalent in line count but worse in
reviewability (the PR diff would mix both init systems' heredocs). Therefore
3a/3b counting as two parts is the correct framing, not a padding trick.

After the required splits, the composition root
(`lunarwing-mt-admin.sh`) shrinks from ~7500 lines to **~4700 lines**:

| Stays in root | Lines |
|---|---|
| Constants, helpers, tenant path primitives, config-hash, `usage` | L1–436 (~436) |
| Init + container-runtime detection, `_ctr`, `main()` dispatcher | L438–883 (~446), L6202–7502 (~1301) |
| User/repo/build mgmt, WASM install | L1484–2166 (~683) |
| External-worker/SSH/secret config | L2823–3190 (~368) |
| Owner-scope migration | L3322–3576 (~255) |
| Notifications + per-worker config | L3580–3706 (~127) |
| Container lifecycle (PG + workers + vision + backup/restore) | L3707–4815 (~1109) |
| Compound commands + `warn_if_adapter_deps_missing` | L5978–6001 (~24) |

The optional phase-2 `tenant-lifecycle.sh` extraction would further shrink
the root to **~3400 lines**, leaving only constants, detection, user/build,
SSH, container lifecycle, and the dispatcher in the main file.

---

## 5. The Four Required Parts — Detail

### Part 1 — `ports-registry.sh`

**Source range:** L884–1482 (~599 lines)
**What moves:**

- Schema init: `ports_registry_init`
- The migration ladder `ports_migrate_v2` … `ports_migrate_v11` (eleven functions + the dispatcher `ports_migrate`)
- Allocation: `ports_allocate`, `ports_enable_darkirc`, `ports_enable_proxy`, `ports_enable_worker`, `ports_deallocate`
- Queries: `ports_get`, `ports_list`, `tenant_exists_in_registry`, `all_tenant_names`
- The per-tenant feature-flag readers `tenant_darkirc_enabled`, `tenant_proxy_enabled`, `tenant_worker_enabled` (currently L175–211 but tightly coupled to the registry readers and worth moving with them)

**Why it's the cleanest split:**

- Pure data layer. The only external deps are the `PORTS_REGISTRY` /
  `PORT_RANGE_*` constants and the `jq` binary (gated by `require_cmd`).
- No init-system awareness, no `$CONTAINER_RT`, no user/group code, no
  heredocs.
- Self-contained on-disk format (`/etc/lunarwing/ports.json`) — the
  migration ladder is the riskiest part of the script and deserves its own
  test surface.

**Shared-state contract (what stays in root and must be sourced before this lib):**

| Symbol | Provided by | Notes |
|---|---|---|
| `PORTS_REGISTRY`, `PORT_RANGE_START`, `PORT_RANGE_END` | root constants | Must be defined before `mt-admin-lib/ports-registry.sh` is sourced |
| `say`, `die`, `require_cmd`, `sanitize_name`, `generate_token` | root helpers | Used for diagnostics and `jq` gating |
| `jq` | external binary | Checked once at source-time via `require_cmd jq` |

**Sourcing contract:** root script sources `mt-admin-lib/ports-registry.sh`
**after** constants and helpers are defined. No circular references —
`ports_*` functions only ever call `say`/`die`/`jq`.

**Migration-ladder note:** `ports_migrate` (L1094) dispatches to `ports_migrate_v2` … `ports_migrate_v11`. Keep all eleven in the library together. The dispatcher must run before any `ports_allocate` call; the library should expose a single `ports_registry_init` entrypoint that calls `ports_migrate` internally (as it already does today).

### Part 2 — `env-generation.sh`

**Source range:** L2168–2822 (~655 lines)
**What moves:**

- The giant `write_tenant_lunarwing_env` (L2429, ~224 lines, **single largest non-renderer function in the file**)
- `write_tenant_bridge_env` (L2653), `write_tenant_proxy_env` (L2719), `write_tenant_darkirc_adapter_env` (L2739)
- `generate_darkirc_config` (L2780)
- WeeChat relay auto-bootstrap: `_read_env_value`, `_weechat_config_dir_has_entries`, `_weechat_validate_relay_config`, `_weechat_generate_relay_config`, `configure_weechat_relay`, `_write_weechat_env`, `tenant_weechat_home`
- XMPP allow-list builders: `build_xmpp_allow_from`, `build_xmpp_allow_from_json`, `_env_existing`

**Why it's a clean split:**

- All heredoc-heavy writers cluster here — this is the densest "data-shaped"
  section after the OpenRC renderer.
- Depends only on tenant path primitives (`tenant_home`, `tenant_env_dir`),
  the feature-flag readers from Part 1 (`tenant_darkirc_enabled`, etc.),
  `ports_get`, `tenant_pg_password`, and a handful of fleet defaults
  (`DEFAULT_TENSORZERO_URL`, `DEFAULT_VL_URL`, `DEFAULT_LLM_BASE_URL`,
  `DEFAULT_GOTIFY_*`).
- Zero init-system awareness. Zero `$CONTAINER_RT` references. No container
  lifecycle.

**Shared-state contract:**

| Symbol | Provided by | Notes |
|---|---|---|
| `tenant_home`, `tenant_env_dir`, `tenant_lw_root`, `tenant_state_dir` | root path primitives (L115–122) | |
| `ports_get` | Part 1 (`ports-registry.sh`) | Must be sourced before Part 2 |
| `tenant_darkirc_enabled`, `tenant_proxy_enabled`, `tenant_worker_enabled`, `tenant_pg_password` | Part 1 (move with the feature-flag readers) | |
| Fleet defaults (`DEFAULT_TENSORZERO_URL`, `DEFAULT_VL_URL`, `DEFAULT_LLM_BASE_URL`, `DEFAULT_GOTIFY_*`, `WEECHAT_BOOTSTRAP_OPT_OUT`) | root constants | |
| `say`, `die` | root helpers | |

**Caveat:** moving the feature-flag readers into Part 1 tightens the
coupling between Parts 1 and 2 — the alternative is to leave
`tenant_*_enabled` in the root. Recommendation: **move them with Part 1**,
because they are pure `ports.json` reads and logically belong there.

### Part 3a — `units-systemd.sh`

**Source range:** L4817–5294 plus `_systemctl_user` (L5197) and `_wait_user_manager` (L5208)
**What moves:**

- `render_pg_quadlet` (L4824), `render_worker_quadlet` (L4870)
- `_render_weechat_systemd_unit` (L4981)
- `render_tenant_systemd_units` (L5004, ~193 lines, six heredocs)
- `_systemctl_user` (L5197) — **the sanctioned path for all tenant systemctl calls** per AGENTS.md HARD RULE
- `_wait_user_manager` (L5208)
- `start_tenant_systemd` (L5218), `stop_tenant_systemd` (L5253), `uninstall_tenant_systemd` (L5266)

**Why it's a clean split:**

- All systemd-specific rendering and lifecycle is contiguous (L4817–5294).
- The `_systemctl_user` helper is the **HARD-RULE boundary** and benefits
  from living in its own file: any future audit for "is anything calling
  `systemctl` outside this file" becomes a one-line `rg` against
  `lunarwing-mt-admin.sh` and `units-systemd.sh`.
- Natural seam with the OpenRC equivalents.

**Shared-state contract:**

| Symbol | Provided by | Notes |
|---|---|---|
| `INIT_SYSTEM`, `ensure_init_system` | root init detection (L438–465) | |
| `CONTAINER_RT`, `MT_ROOTLESS`, `ensure_container_runtime`, `podman_supports_quadlet`, `_ctr`, `_ensure_tenant_image` | root container-runtime detection (L502–606) | |
| `_store_container_hash`, `_container_config_changed`, `_recreate_quadlet_container` | root config-hash tracking (L124–171) | Quadlet recreation uses these |
| Tenant path primitives (esp. `tenant_quadlet_dir`) | root (L115–122) | |
| `ports_get`, `tenant_*_enabled`, `tenant_pg_password` | Parts 1 (after feature-flag move) | For rendering ExecStart lines |
| `say`, `die` | root helpers | |

**No call may leave this file for raw `systemctl`/`loginctl`.** The library
is the sole tenant-systemctl surface; `_systemctl_user` must remain the only
exit point.

### Part 3b — `units-openrc.sh`

**Source range:** L5297–5976 plus the worker/babysitter helpers at L656–883
**What moves:**

- `_render_weechat_openrc_unit` (L5297)
- `install_openrc_env_exec` (L5357) — installs `/usr/local/libexec/lunarwing-openrc-env-exec`
- `render_tenant_openrc_units` (L5385, ~507 lines, **eight init-script heredocs + six conf.d heredocs** — second-largest function in the file)
- `start_tenant_openrc` (L5892), `stop_tenant_openrc` (L5949), `uninstall_tenant_openrc` (L5969)
- The OpenRC-flavored worker/babysitter helpers: `render_worker_openrc_unit`, `_register_worker_unit`, `_deregister_worker_unit`, `ensure_babysitter_helper`, `render_container_babysitter_unit`, `_register_babysitter`, `_deregister_babysitter`

**Why it's a clean split:**

- Mirror image of Part 3a. Its coupling to the root is essentially
  identical.
- The worker/babysitter render helpers at L656–883 currently live near the
  top of the file but emit OpenRC init scripts only; moving them here puts
  all OpenRC heredocs in one place.
- `_install_health_cron` is OpenRC-flavored too but lives in
  `health-pipeline.sh` (Part 4 / optional) for cohesion with the rest of
  the health-pipeline dispatch.

**Shared-state contract:** same shape as Part 3a.

### Part 4 — `health-pipeline.sh` (optional but recommended)

**Source range:** L5978–6200 (~223 lines)
**What moves:**

- `warn_if_adapter_deps_missing` (L5984) — pure read-only check
- `_ensure_cron_runlevel` (L6004)
- `_install_health_cron` (L6018)
- `_install_health_systemd_timer` (L6041)
- `ensure_health_pipeline` (L6083) — installs `ic-infrastructure-health-check/*` to `/usr/local/lib/lunarwing-health/`
- `remove_health_pipeline` (L6179) — currently **not wired into the dispatcher** (see §8)

**Why it's a clean split:**

- Highly self-contained: depends only on `HEALTH_*` constants, `INIT_SYSTEM`,
  `PORTS_REGISTRY`, and cron/systemctl/rc-update.
- Has its own install surface (`/usr/local/lib/lunarwing-health/`,
  `/usr/local/sbin/lunarwing-mt-health`, `/etc/lunarwing/health.env`,
  `/etc/systemd/system/lunarwing-mt-health.{service,timer}`).
- Tests for the health pipeline (init-agnostic, ports-registry-aware per
  AGENTS.md HARD RULE) belong with this library, not with the root.

**This part is optional under the "at least four" requirement** (3a + 3b +
ports + env already satisfy it), but it's the highest-value *fifth* part
because it has the clearest independent test surface.

---

## 5.1 Test Surface and Shellcheck Scoping (added per review feedback)

> **Review note:** "No mention of how `shellcheck` scoping or a test harness
> would attach to the new libraries — that's the stated motivation (§2.1) but
> the proposal doesn't sketch the test surface. Worth a follow-up section
> before implementation begins." — `KUMOGAKURE_RECENT_REV_T.md`

The decomposition is the *enabler* for per-library testing, not the test
harness itself. This section sketches what that test surface looks like so
implementers know what to build in the follow-up PRs.

### Shellcheck scoping

Today, `shellcheck` runs against the entire 7500-line monolith. Any warning
in an OpenRC heredoc blocks linting of unrelated code. After the split:

```bash
# Scoped lint per library:
shellcheck ic/scripts/mt-admin-lib/ports-registry.sh
shellcheck ic/scripts/mt-admin-lib/env-generation.sh
shellcheck ic/scripts/mt-admin-lib/units-systemd.sh
shellcheck ic/scripts/mt-admin-lib/units-openrc.sh
shellcheck ic/scripts/mt-admin-lib/health-pipeline.sh   # optional

# Full-file lint (catches cross-library issues):
shellcheck ic/scripts/lunarwing-mt-admin.sh
```

Each library should carry a top-level `# shellcheck shell=bash` directive and
a `# shellcheck disable=SC2317` banner (functions are only called from the
dispatcher in a different file). A CI rule can lint each library independently
and report failures scoped to the library, not the monolith.

### bats test harness (follow-up, not part of the split PRs)

The natural test surface per library:

| Library | Testable in isolation? | Mock requirements |
|---|---|---|
| `ports-registry.sh` | **Yes — highest priority.** Pure data layer. | Fake `PORTS_REGISTRY` temp file, `jq` (real binary). Test every migration step v2→v11, allocation, deallocation, feature-flag reads. |
| `env-generation.sh` | **Yes.** Heredoc output is deterministic for fixed inputs. | Mock `ports_get`, `tenant_*_enabled`, `tenant_pg_password` by sourcing fakes before the lib. Byte-diff generated env files against golden fixtures. |
| `units-systemd.sh` | **Partial.** Rendering is testable; start/stop/uninstall require a real systemd user manager. | Render to temp files; assert heredoc output. Skip lifecycle tests unless running under `systemd-run --user`. |
| `units-openrc.sh` | **Partial.** Same as systemd — rendering testable, lifecycle not. | Render to temp files; assert. Cannot run on the dev VM (no OpenRC). |
| `health-pipeline.sh` | **Yes.** Install surface is file-ops + systemctl/rc-update stubs. | Mock `INIT_SYSTEM`, stub `systemctl`/`rc-update` with no-op wrappers. |

**Suggested test layout:**

```
ic/scripts/tests/
├── mt-admin-lib/
│   ├── ports-registry.bats         # migration ladder, alloc/dealloc, feature flags
│   ├── env-generation.bats         # byte-diff against golden env files
│   ├── units-systemd.bats          # heredoc output assertions
│   ├── units-openrc.bats           # heredoc output assertions (render-only)
│   └── health-pipeline.bats        # install surface (stubbed init)
└── fixtures/
    ├── ports-v1.json               # pre-migration registry snapshots
    ├── ports-v11.json              # post-migration expected state
    ├── golden-lunarwing.env        # expected env output for a fixed tenant
    └── golden-systemd-unit.service # expected rendered unit
```

**The split PRs themselves should not include the test harness** — they are
pure code moves. The test harness lands as a separate follow-up PR after the
four splits are merged and the sourcing pattern is proven stable.

### Symbol-availability self-check

Per review suggestion (§11 Q3), a `mt-admin selfcheck` subcommand can verify
all expected library symbols are defined after sourcing:

```bash
# Inside lunarwing-mt-admin.sh, after all libraries are sourced:
mt_selfcheck() {
  local missing=0
  for fn in ports_registry_init ports_migrate ports_allocate ports_get \
            ports_deallocate ports_list write_tenant_lunarwing_env \
            render_tenant_systemd_units _systemctl_user start_tenant_systemd \
            render_tenant_openrc_units start_tenant_openrc \
            ensure_health_pipeline; do
    if ! declare -f "$fn" >/dev/null 2>&1; then
      say "MISSING: $fn"
      missing=$((missing + 1))
    fi
  done
  [[ "$missing" -eq 0 ]] || die "selfcheck: $missing function(s) not defined after sourcing"
  say "selfcheck: all expected symbols present"
}
```

This catches load-order regressions cheaply and can be wired into `doctor`.

---

## 6. Sourcing Strategy and Migration Path

### 6.1 Source order

```bash
# lunarwing-mt-admin.sh — composition root (preserved entrypoint)
#!/usr/bin/env bash
set -euo pipefail

# 1. Constants and shared helpers (L1–436 of today's file)
#    - PORTS_REGISTRY, PORT_RANGE_*, HEALTH_*, fleet defaults, etc.
#    - say, die, require_cmd, sanitize_name, generate_token
#    - tenant_home / tenant_repo / tenant_env_dir / tenant_quadlet_dir / ...
#    - usage(), render_template(), require_root()

# 2. Init + container runtime detection (L438–606)
#    - detect_init_system, ensure_init_system, INIT_SYSTEM
#    - detect_container_runtime, ensure_container_runtime, _ctr

# 3. Required libraries (sourced, not executed)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/mt-admin-lib/ports-registry.sh"      # Part 1
source "$SCRIPT_DIR/mt-admin-lib/env-generation.sh"      # Part 2
source "$SCRIPT_DIR/mt-admin-lib/units-systemd.sh"       # Part 3a
source "$SCRIPT_DIR/mt-admin-lib/units-openrc.sh"        # Part 3b
# source "$SCRIPT_DIR/mt-admin-lib/health-pipeline.sh"   # Part 4 (optional)

# 4. User/repo/build management, WASM, owner-scope, SSH, notifications,
#    container lifecycle (L1484–2166, L2823–4815, L5978–6001) — stay in root

# 5. High-level tenant verbs (L6202–6972) — stay in root
#    add_tenant, start_tenant, stop_tenant, status_tenant, doctor, etc.

# 6. main() dispatcher (L6974–7502) — stays in root

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then main "$@"; fi
```

The source order is **load-bearing** because each library references symbols
from the earlier ones. Each library file should `set -euo pipefail`
defensively and should **not** call `main` or execute any code at source
time other than function definitions and a `require_cmd` guard.

### 6.2 Phased rollout — four PRs

**The constraint:** no PR may change the user-visible CLI surface, the
on-disk state files, or init behavior. Each PR must be independently
revertible.

| PR | Split | Post-PR `wc -l lunarwing-mt-admin.sh` | Validation |
|---|---|---|---|
| 1 | Extract `ports-registry.sh` (Part 1) + move feature-flag readers | ~6900 | `mt-admin add-tenant --dry-run`, `list-tenants`, `status` on an existing tenant; ports.json diff-empty |
| 2 | Extract `env-generation.sh` (Part 2) | ~6250 | Re-render env files for an existing tenant; byte-identical output for unchanged inputs |
| 3 | Extract `units-systemd.sh` (Part 3a) | ~5800 | `render-units`, `restart-tenant` on a systemd host; byte-diff empty for rendered units |
| 4 | Extract `units-openrc.sh` (Part 3b) + move worker/babysitter renderers | ~4800 | Same as PR 3 but on an OpenRC host (Gentoo dev VM can't test — OpenRC maintainers must validate) |

After PR 4 the file is **~4800 lines** — a 36% reduction — with the four
required parts in separately-reviewable libraries. The optional Part 5
(`health-pipeline.sh`) and Part 6 (`tenant-lifecycle.sh`) can land later
without churn.

### 6.3 Backwards compatibility for callers

`lunarwing-mt-admin.sh` remains the single entrypoint. Existing callers
(`lunarwing_mt_onboard`, `lunarwing_mt_onboard_web`, verify harnesses,
operator runbooks) need **zero changes** — they still invoke the same
subcommands against the same path. The script's `if [[ "${BASH_SOURCE[0]}"
== "${0}" ]]; then main "$@"; fi` guard stays put so it can still be sourced
by tests.

The `LUNARWING_MT_ADMIN` override (per AGENTS.md) continues to resolve to
the composition root; libraries are sourced relative to the root's
`${BASH_SOURCE[0]}` directory and move with it.

---

## 7. Risks and Mitigations

| Risk | Likelihood | Mitigation |
|---|---|---|
| Sourcing a library twice (double-define) causes subtle bugs. | Low | Each library uses plain function definitions (idempotent redefine in bash). Add a one-line `# sourced by lunarwing-mt-admin.sh; do not execute directly` banner. |
| Library tries to use a symbol that hasn't been defined yet (load-order bug). | Medium | Document source order in the composition root. Add an `:_assert_defined` helper that the test harness calls after sourcing each lib, checking `${!name}` for required symbols. |
| Path drift: `SCRIPT_DIR` resolves wrong when invoked via symlink. | Medium | Use `BASH_SOURCE[0]` (already correct for the existing entrypoint), not `$0`. The `LUNARWING_MT_ADMIN` override already handles this for the root. |
| OpenRC regression introduced silently (no Gentoo dev box to catch it). | Medium | PR 4 must be validated by an OpenRC maintainer before merge; include a rendered-unit byte-diff against `HEAD` in the PR description. |
| `shellcheck` SC2317 (unreachable command) false-positives on functions only called via dispatcher from a different file. | High | Add a per-library `# shellcheck disable=SC2317` banner at the top of each sourced file, with a comment explaining why. |
| Test coverage regression: today's only test is "run `add-tenant` and see what breaks". | High (pre-existing) | This split is the *enabler* for per-library tests, not the solution. Land per-library `bats`/`shellcheck` harness as a follow-up; the split is still worth it on reviewability grounds alone. |
| Operators have custom-patched copies of `lunarwing-mt-admin.sh` in `/usr/local/sbin/`. | Medium | The install path (`/usr/local/sbin/lunarwing-mt-admin`) is unchanged; the library directory lives under the repo and is copied alongside the root on install. Document the new `mt-admin-lib/` directory in the release notes. |
| `set -euo pipefail` semantics differ when a function is sourced vs executed. | Low | All current functions already assume `set -euo pipefail` from the root. No change needed. |

---

## 8. Drive-by Observations (not part of the "four parts" requirement)

These are findings from the structural survey that the split makes easier to
act on later. **Do not bundle them into the decomposition PRs.**

1. **`remove_health_pipeline` is dead code.** Defined at L6179 but never
   wired into the dispatcher (no `remove-health` subcommand exists). After
   the split, this is obvious because `health-pipeline.sh` will have a
   clearly-unreferenced public function. Add a `remove-health` subcommand as
   a separate follow-up.

2. **`owner_scope_*` and `migrate_owner_scope` (L3322–3576)** could be a
   seventh library (`owner-scope-migration.sh`). It's a 255-line
   one-shot-migration surface with its own `psql` dependency. Not required
   for the "four parts" minimum; left in root for now to avoid over-splitting.

3. **Container lifecycle (L3707–4815, ~1109 lines)** is the single largest
   remaining block after the required splits. Each sidecar
   (`nanocode`/`opencode`/`pebble`/`vision`/`postgres`) follows the same
   pattern: render unit (systemd or OpenRC), start, wait-for-ready, stop.
   A future refactor could introduce a `start_tenant_sidecar` dispatch
   helper that takes the sidecar name — but that is a behavior change, not
   a move, and is out of scope here.

4. **The `usage()` heredoc (L234–403, ~170 lines)** is a maintenance pain
   (every new subcommand needs a matching edit). Consider generating it from
   a table in a follow-up. Not part of this proposal.

5. **Worker/babysitter helpers (L656–883)** currently sit at the top of the
   file but emit OpenRC-only artifacts. Part 3b pulls them down where they
   belong.

6. **`main()` dispatcher (L6976–7497, 522 lines)** could be replaced by a
   per-subcommand `mt-admin-cmd/<name>.sh` lookup (à la `git-<subcommand>`).
   Out of scope; the dispatcher stays in the composition root for now.

---

## 9. Explicitly Out of Scope

- The sibling `lunarwing-mt-provision-openrc.sh` (428 lines) — already a
  separate script. Leave it alone unless the OpenRC maintainers ask for it
  to be folded into `units-openrc.sh`.
- `lunarwing_mt_onboard*` Python wrappers — they stay thin drivers of the
  composition root.
- Any change to `/etc/lunarwing/ports.json` schema or migration ladder.
- Any change to the CLI surface or the `LUNARWING_MT_ADMIN` override.
- Any change to the init abstraction boundary (`_systemctl_user` and
  friends) — the split *strengthens* the boundary by centralizing it; it
  does not move or weaken it.
- Any change to `ic-infrastructure-health-check/` tooling.

---

## 10. Recommendation Summary

| Decision | Choice | Rationale |
|---|---|---|
| Number of parts | **Five** (3a + 3b + ports + env + optional health) | Comfortably exceeds the "AT LEAST FOUR" floor; honors the author's own section banners. Counting 3a/3b separately reflects their mirror-image cohesion (see §4 confirmation note). |
| Extraction order | ports → env → systemd → openrc | Each step has the smallest possible diff; ports is the cleanest seam to prove the sourcing pattern. |
| Mechanism | Bash `source` of sibling files under `mt-admin-lib/` | Zero behavioral change; preserves the existing entrypoint and `LUNARWING_MT_ADMIN` override; no new runtime dependency. |
| Composition root | `lunarwing-mt-admin.sh` keeps constants, helpers, detection, user/build, SSH, container lifecycle, tenant verbs, dispatcher | These have high inter-coupling and would create circular dependencies if split further. |
| Rollout | Four sequential PRs, each independently revertible | Lets reviewers sign off per-part; lets OpenRC maintainers block PR 4 without blocking the first three. |
| Post-split root size | ~4800 lines (after PR 4) | 36% reduction; remaining size is driven by legitimate container-lifecycle verbosity, not by lack of modularity. |
| Testing | Per-library `bats`/`shellcheck` harness as a follow-up (see §5.1) | The split is the enabler, not the test surface itself. Sketch in §5.1 defines the per-library test matrix and fixture layout. |

### 10.1 Updated Recommendations (per review feedback)

> **Review note:** "Phase-2 `tenant-lifecycle.sh` extraction is flagged
> optional; given it's the 1301-line dispatcher + verbs, it's where the real
> reviewability win is. Recommend not deferring it indefinitely." —
> `KUMOGAKURE_RECENT_REV_T.md`

**`tenant-lifecycle.sh` should be prioritized as PR 5, not indefinitely
deferred.** The 1301-line high-level tenant verbs + dispatcher block
(L6202–7502) is the single largest remaining chunk after the four required
splits. It is where the most cross-referencing happens (every `add_tenant`
call touches ports, env, units, health) and therefore where the
reviewability win is greatest. Recommendation:

- **PR 5 (post-required-splits):** Extract `tenant-lifecycle.sh` containing
  `add_tenant`, `start_tenant`, `stop_tenant`, `restart_tenant`,
  `upgrade_tenant`, `status_tenant`, `list_tenants`, `doctor`, and the
  `usage()` heredoc. This shrinks the root to ~3400 lines.
- **`main()` dispatcher** stays in the root (it is 522 lines of arg-parsing
  + case-dispatch and is tightly coupled to the usage text). A future
  per-subcommand lookup (§8 item 6) can address it separately.
- This PR should land **after** the four required splits are proven stable,
  so the sourcing pattern is battle-tested before the largest extraction.

---

## 11. Open Questions (non-blocking)

These do not block the proposal but should be resolved before PR 1 lands:

1. Should the libraries be installed under `/usr/local/lib/lunarwing-mt/`
   (mirroring `HEALTH_LIB_DIR`) rather than copied next to the composition
   root? Currently the install path is just `/usr/local/sbin/lunarwing-mt-admin`
   as a single file — we'd need to decide whether `mt-admin-lib/` ships as
   a sibling directory or under a lib path.
2. Should the libraries be made executable (with a `die "sourced only"`
   guard when run directly) or kept as `0644`? Lean toward `0644` with a
   banner.
3. ~~Should we add a top-level `mt-admin selfcheck` subcommand that verifies
   all expected library symbols are defined after sourcing? Would catch
   load-order regressions cheaply.~~ **Resolved:** Yes — see the `mt_selfcheck`
   sketch in §5.1. Wire it into `doctor` as a post-sourcing assertion.

---

**End of proposal.** No code is changed by this document. Implementation is
tracked separately as four sequential PRs per §6.2 (plus optional PR 5 for
`tenant-lifecycle.sh` per §10.1). The test harness (§5.1) lands as a
follow-up after the splits are proven stable.
