# Per-tenant worker gating (fix: unselected workers start anyway)

## Bug

`start-tenant` starts every external worker (nanocode/pebble/opencode) whose
**shared host-wide image** exists, ignoring what the tenant selected. Verified
on `yector` (built `--with-wasm` only, yet pebble+opencode started because their
images existed from earlier tenants; nanocode skipped only because no image).

Root cause: `start_tenant_<worker>` (mt-admin.sh:3339/3539/3688) gate solely on
(1) WSS port allocated — always true, and (2) image present — shared across
tenants. No per-tenant selection is persisted or checked. Contrast
`enable_darkirc`/`enable_proxy`, which ARE persisted in `ports.json` and read
back via `tenant_darkirc_enabled`.

The GUI is correct: unchecked workers omit `--with-*` (audit log confirms).

## Approach: Option B — persist selection at `add-tenant`, gate `start-tenant`

Mirror the existing darkirc/proxy pattern exactly. Selection recorded at tenant
creation (independent of build), read back at start. Kawarimi preserved by
threading the same flags through import's `add-tenant` call.

### 1. mt-admin.sh — registry writers/readers (mirror darkirc)
- `ports.json` tenant object: add `workers: {nanocode, pebble, opencode}` bools.
- `ports_allocate`: accept a `workers_json` arg; write the `workers` map on
  first allocation; on resume, reconcile any `false -> true` (like darkirc).
- Add `ports_enable_worker <name> <worker>` (idempotent false->true) for resume.
- Add `tenant_worker_enabled <name> <worker>` reader
  (`.tenants[name].workers[worker] // false`).

### 2. mt-admin.sh — `add-tenant` parser + `add_tenant()`
- Parse `--with-nanocode|--with-pebble|--with-opencode` in the `add-tenant`
  case (and `add-tenants` plural for parity).
- Thread three bools into `add_tenant()` (new positional params 19-21, all
  default false — backward compatible).
- Pass a `workers_json` to `ports_allocate`.

### 3. mt-admin.sh — gate the starts
- In `start_tenant` (6135-6137), wrap each call:
  `tenant_worker_enabled "$name" nanocode && start_tenant_nanocode "$name"` etc.
- Absent flag = OFF (per decision). Existing tenants with no `workers` key stop
  auto-starting workers until re-provisioned/flag set. Emit a one-line
  `say` when skipping so it's visible in logs.

### 4. Kawarimi import — thread selection into add-tenant
- import-tenant.sh already parses `--with-*` (lines 58-61) and passes them to
  `build-tenant` (249-251). ALSO append them to `add_args` (the `add-tenant`
  call, ~227) so selection is persisted before `start-tenant` (334) gates on it.
- Without this, imported+`--start`ed tenants would build workers but never
  start them — a regression. This is the key Kawarimi safeguard.
- Export: no change needed (worker selection is an operator choice at import
  time, matching current `--with-*` import flags; not carried in the bundle).

### 5. GUI / Python — thread selection to add-tenant too
- `provisioner.build_add_tenant_args`: append `--with-nanocode/pebble/opencode`
  from `config.workers` (currently only `build_build_tenant_args` does).
- No models.py / wizard.js change: `workers` list already flows in.

### 6. Tests
- mt-admin `tests/`: assert add-tenant/import parse+forward `--with-*`, and that
  start gating reads the registry. Extend `test-kawarimi-import-flags.sh` to
  assert import appends worker flags to the add-tenant call (regression guard).
- Python `web_tests.py`: assert `build_add_tenant_args` emits worker flags.
- `bash -n` + `python3 -m unittest` + run both mt-admin test harnesses.

## Backward compatibility / safety
- New `add_tenant()` params default false → existing callers unaffected.
- `ports.json` gains a `workers` key; readers default missing to false.
- Existing tenants: absent `workers` → workers OFF next start (intended fix).
  Re-enable per tenant with `add-tenant <name> --with-<worker>` (resume path
  flips the registry, mirroring `--enable-darkirc`).
- No full cargo rebuild required to test (parser/gating are shell-level;
  harnesses stub mt-admin).

## Not doing
- Not carrying worker selection in the export bundle (import `--with-*` stays
  the operator's choice, unchanged semantics).
- Not tearing down already-running workers on disable (one-directional, like
  darkirc; teardown remains a manual op).
