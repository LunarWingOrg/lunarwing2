# Goal 12 — real onboarding web acceptance

Date: 2026-09-28. Source baseline: `32655cba4`, plus the fixes
described below. Goal: [v2.1.3.0 item 12](AGENT_GOALS_2.1.3.0.md#phase-4--carried-known-issues-from-release-v2120md-candidates-weigh-each).

## Scope and method

The operator selected **new test tenant only** for lifecycle mutations. The
earlier `g12probe` account was absent from this host; a fresh account with that
name was created. Existing `antelope` and `hermes` tenants were only inspected.

The onboarding web application ran in **REAL** mode on loopback port 1972,
with its normal random session token. Headless Playwright/Chromium drove the
actual forms and consumed the job WebSocket events. Jobs invoked the repository's
mt-admin and migration scripts; no demo scripts or fake container services were
used. Provisioning selected **Pebble and OpenCode**, kept SSH and WeeChat
bootstrap enabled, and disabled installation of the host-global health pipeline.

Release compilation ran in tmux with CPU affinity `0-11` and 12 build jobs.
The runtime was rootless Podman under a systemd **user** manager. All tenant
lifecycle and status operations used mt-admin.

## Findings and repairs

### Podman repair could never reach migration

The previous live run recorded a circular dependency: `fix-podman-userns`
called the full `stop_tenant`, whose writer check correctly rejected Podman's
broken namespace state. That prevented `podman system migrate` from ever running.

Repair now stops native init-managed services/supervisors first, cleans stale
pause metadata, runs tenant-scoped migration, then calls the full tenant stop
and requires its runtime-backed writer proof before restart. Ordinary
stop/export checks still fail closed. Systemd shutdown now reports failed stops,
stops transitional units, and rejects unknown native service state rather than
silently skipping it. OpenRC retains its existing supervisor-first stop helper.

The regression drives the real repair, native-init stop, and quiescence functions
against a runtime that fails until migration. It covers both init paths,
service-stop failure, unresolved runtime failure, surviving writers, transitional
and unclassifiable systemd states, absent optional units, and a stale deselected
OpenRC worker supervisor. This also rechecks the class of omission reported on
rejected MR !1181.

Details: [Podman namespace repair](../../bugs/BUG-workload-scope-podman-userns.md#the-repair-path-kept-for-already-broken-pairings).

### Startup detection destroyed live Podman metadata

The fresh wizard run reproduced the earlier failure after the release builds
completed. Its daemon was active, but runtime queries returned 125 with a
missing `conmon.pid`; PostgreSQL appeared stopped and both workers were absent.

A controlled test identified the trigger on Podman **6.1.2**: inside a user
service with the daemon's `NoNewPrivileges`/`PrivateTmp` settings,
`podman --version` returned success but deleted the live tenant's `pause.pid`.
`podman version --format '{{.Client.Version}}'` returned success and preserved
the file. Both Rust client probes now share the safe subcommand arguments;
the version parser's output format and daemon hardening are preserved.

The repaired lifecycle sequence reached migration on the real fault and then
correctly refused restart when migration itself failed. Recovering this
already-broken instance required restoring the missing tracking file for the
single positively identified tenant-owned `_PODMAN_PAUSE=1` process under the
lifecycle lock. The ordinary verified stop then passed. The automatic repair
is **not** claimed to recover this missing-file/stale-conmon case by itself.

The first deployment release rebuild passed in **7m07s**, but startup still
failed: the background sandbox reaper also directly invoked `podman ps` from
the hardened daemon on its first tick. Its listing and removal commands now
use the existing isolated workload launcher, with guarded timeout cleanup.
Only changed Rust files were copied into the test tenant's checkout for
deployment. No host reboot, container store deletion, or database replacement
was needed.

### Verification accepted a broken runtime

The original UI reported all five provisioning phases successful because it
only checked the gateway port and daemon unit. Verification now additionally
requires runtime-backed PostgreSQL and every selected worker's running state
from mt-admin. Missing, stopped, or unclassifiable containers fail the job;
unselected workers are not required. The CLI forwards the same selection and
returns failure if post-start verification fails. Fixtures cover systemd user
units, OpenRC, the observed false-success state, polling, web wiring, and CLI
exit status.

### Export rejected the web-generated master key

The real export dry-run failed before stopping writers because onboarding's
env writer double-quotes `SECRETS_MASTER_KEY`, while export validated the raw
quoted text as hex. Export now decodes matching env quotes and the writer's
escaped double quotes/backslashes for preflight comparison, without sourcing or
evaluating tenant content. The encrypted manifests still preserve the original
env lines. The production-script regression uses the actual onboarding env
writer, equivalent differently quoted XMPP passwords, and invalid/duplicate
master-key rejection before lifecycle mutation. The real web dry-run then passed.

### Host prerequisites

- The host DNS resolver returned incorrect addresses for `files.pythonhosted.org`
  and `static.rust-lang.org`, causing TLS hostname verification failures.
  Python dependencies were installed using a process-local DNS correction to the
  HTTPS-DNS-verified CDN address. The test account received a copy of the already
  installed Rust **1.96.1** toolchain, including `wasm32-wasip2`. TLS verification
  remained enabled; host DNS configuration was not modified.
- `mt-admin doctor` reported two uidmap setuid failures, but this host supplies
  `newuidmap`/`newgidmap` privileges through file capabilities. Fresh rootless
  PostgreSQL started and passed its health check. The missing optional Kimi image
  was unrelated to the selected workers.
- The configured LLM endpoint and the loopback port-3000 endpoint were
  unreachable. The operator explicitly took responsibility for correcting the
  LLM provider and confirmed it is not a blocker for this lifecycle acceptance.
  Worker connectivity is tested independently of model-backed task execution.

## Verification record

**The scoped real-host lifecycle rehearsal passed.** The initial UI provisioning
exposed the blockers above; the same fresh test tenant was repaired and deployed
with the fixes, then verified through export and restart. This is a real-host
rehearsal with rootless containers, not demo acceptance.

Live checks completed:

- Both selected workers running, PostgreSQL running, LunarVision healthy, and
  daemon/bridge/WeeChat services active through mt-admin.
- Gateway authenticated status passed; unauthenticated status rejected (401).
- Pebble and OpenCode each passed `/health`, `/ready`, authenticated WebSocket
  `ready`/`ping`/`pong`, and unauthenticated WebSocket rejection.
- Both worker-mounted SSH-agent sockets returned the provisioned identity via
  the SSH-agent protocol. The slim Pebble image has no `ssh-add`, so the probe
  used its existing Perl Unix-socket support; no packages were added to workers.
- The deployed background reaper removed an old sandbox-labelled fixture and
  preserved an unlabelled control container. Live pause metadata survived.
- The corrected web verifier passed gateway/daemon/PostgreSQL/Pebble/OpenCode;
  it also rejected the prior broken-runtime state despite the active daemon.
- A test secret stored through the real web form decrypted successfully from
  PostgreSQL, and a workspace marker was seeded for the export roundtrip.
- Real web export dry-run and **APPLY** passed. `writers-active g12probe`
  reported `stopped` (its documented exit 1), with PostgreSQL still available.
- The root-owned, mode-0600 encrypted archive rejected a wrong password. Its
  carried master key matched the source, its state archive contained the marker,
  and regenerated `config.toml`/WASM artifacts were excluded.
- A real `pg_restore` of the exported dump into temporary database
  `goal12_bundle_check` preserved the encrypted secret's ciphertext and salt;
  decryption returned the original value. The temporary database was dropped.
- The real web **encrypted import dry-run** passed with target override
  `g12review`, explicit source owner scope `g12probe`, and both worker flags.
  No target tenant was created.
- `mt-admin restart-tenant g12probe` passed. Gateway/worker health,
  authenticated WebSocket exchange, SSH-agent identities, and live Podman
  metadata all passed again; the original DB secret and workspace marker
  remained unchanged.
- Real web unauthenticated API access was rejected (401), concurrent export
  during provisioning was rejected (409), and all seven audit logs were 0600
  with no test secret, passphrase, current master key, or token values found.

Automated checks completed:

- `test-mt-admin-fix-podman-userns.sh`, including the new real-composition
  `test-mt-admin-podman-repair-quiescence.sh` regression.
- `test-mt-admin-worker-lifecycle.sh` and `test-darkirc-writer-lifecycle.sh`.
- `test-kawarimi-import-flags.sh`, including encrypted production-script
  export/import fixture coverage and passphrase handling.
- `lunarwing_mt_onboard.tests`, `verify_tests`, and
  `lunarwing_mt_onboard_web.web_tests`: **115 passed** together.
- `lunarwing_mt_onboard_web.browser_tests`: **4 passed**; these demo regressions
  are supplementary and are not the real-host acceptance evidence.
- mt-admin shellcheck and focused test-script shellcheck.
- Export/migration shellcheck and the production-script encrypted roundtrip,
  including quoted, invalid, and duplicate source master-key cases.
- Deployment release builds passed (initial daemon **12m10s**, bridge **10m05s**;
  fix deployments **7m07s** and **7m14s**). Rust formatting passed.
- `taskset -c 0-11 cargo test --release -j12 --lib
  orchestrator::reaper::tests -- --test-threads=12`: **23 passed**, including
  isolated output/exit-status handling and timeout descendant cleanup.

## Artifacts and final state

- Encrypted bundle:
  `/var/lib/lunarwing-migrate/goal12/g12probe-migrate-20260928-023152.7z`.
- Private passphrase file: `/tmp/opencode/goal12-bundle-passphrase` (0600).
- Private audit trail: `/tmp/opencode/goal12-audit/`; filtered UI results and
  post-restart checks: `/tmp/opencode/goal12-*.log` / `goal12-ui-*.json`.
  The onboarding server logs contain session URLs and must remain private.
- `g12probe` is running with Pebble and OpenCode. The temporary onboarding
  server was stopped; both reaper fixture containers and the restored test DB
  were removed. Existing `antelope` and `hermes` tenants were not stopped or
  modified. The LLM provider remains operator-owned.

Live OpenRC acceptance, Kimi/OpenCode 2, model-backed worker jobs, and a
cross-host cutover are not covered by this host rehearsal.
