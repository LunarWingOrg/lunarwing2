# Fork Context

This repository is a hard fork of IronClaw. Future agents should treat this
file as fork-level context, not as an upstream compatibility promise.

## Fork Goal

The fork prioritizes:

- XMPP/OMEMO-first agent operation.
- Stable encrypted group chat behavior.
- Reliable scheduled routines.
- Local/self-hosted deployment.
- Practical systemd operations.
- Clear operational scripts over hidden manual steps.

Compatibility with upstream IronClaw is not required unless a task explicitly
requires it.
Please also see README.md and MANIFESTO for additional information about what the purpose of this hard fork is and what kinds of features are prioritized. Do not edit README.md or MANIFESTO unless explicitly asked.

## Upstream Policy

- Do not assume upstream compatibility.
- Do not preserve code paths only for upstream parity.
- Prefer clear fork-local behavior over generic abstractions when they conflict.
- Upstream merges, if any, should be treated as deliberate porting work, not
  routine synchronization.
- If a change modifies behavior tracked in `ic/FEATURE_PARITY.md`, update that
  file only when the fork still cares about that parity entry.

## Protected Runtime Behavior

Do not break these without explicit approval:

- XMPP bridge operation.
- XMPP room configuration through the gateway.
- OMEMO encrypted one-to-one and group chat support.
- XMPP group chat self-message suppression.
- XMPP live rate-limit control.
- WASM channel loading.
- WASM tool loading.
- Gotify WASM tool usage.
- Scheduled routines.
- Routine manual runs.
- Gateway status/config endpoints.
- Systemd deployment units.
- Lunarwing's watchdog service/timer.

## Runtime Shape

Expected runtime pieces may include:

- Main Lunarwing daemon service.
- Separate XMPP bridge service.
- WASM channels, especially XMPP.
- WASM tools, especially Gotify.
- Gateway API used for status and runtime configuration.
- Systemd units and helper scripts under `ic/systemd/` and `ic/scripts/`.

The XMPP bridge is a separate process that connects to XMPP as a normal user,
not as a Prosody component. OMEMO encryption/decryption happens in the XMPP
bridge/client path, not in the main daemon.

## Deployment Rules

- Codex may build binaries when asked.
- Codex must not deploy binaries unless explicitly asked.
- Codex must not restart services unless explicitly asked.
- For XMPP bridge changes, build and verify first; deployment is a separate
  step unless requested.
- When touching systemd units, call out whether daemon restart, bridge restart,
  or both are required.

## Secrets

Never commit secrets.

By design, Lunarwing uses the encrypted database to store secrets. However, this is simply not the case for some legacy components and certain services and bridges, unfortunately

Therefore, some secrets may exist in:

- `<base_dir>/.env` — default `~/.lunarwing/.env` (current; `LUNARWING_BASE_DIR`
  overrides the base dir, legacy alias `IRONCLAW_BASE_DIR`). Loaded via dotenvy
  early in startup (see `ic/src/bootstrap.rs`, `ic/src/config/mod.rs`).
- `~/.ironclaw/.env` (legacy fallback, still honored when no base-dir override
  is set)
- `<base_dir>/env/lunarwing.env` and sibling env files (e.g.
  `/home/<user>/lunarwing/env/*`) — systemd/service deployments inject these via
  `EnvironmentFile`; they take precedence over `<base_dir>/.env` because dotenvy
  never overwrites an already-set var.
- systemd service environment - (in older versions only)
- local database rows. (only secret names)

Agents may inspect whether required keys exist, but must not print secret
values in logs, diffs, or final answers.

## Build and Test

Preferred baseline commands (run from the `ic/` directory; prefix with
`taskset -c 0-9` and use `-j10` per the root `AGENTS.md` build constraints,
and prefer `cargo check` over full debug builds for compile verification):

```sh
cargo check
cargo test
cargo build --release
```

Use targeted checks when possible. For XMPP bridge work, build the bridge
binary specifically before doing a wider workspace build.

For live database checks, use read-only SQL unless the user explicitly requests
repair or mutation.

## Database and Routine State

Scheduled routines can become blocked if old rows in `routine_runs` remain in
`status='running'`. Most older routines use `max_concurrent=1`, so one stale running
row can block future scheduled runs.

Automatic recovery now exists in `ic/src/agent/routine_engine.rs`:

- `sweep_stuck_lightweight_runs()` finds lightweight runs (`job_id IS NULL`)
  stuck in `running` beyond `lightweight_timeout_secs` and marks them failed
  with "Timed out (stuck run recovery)".
- `sync_dispatched_runs()` reconciles orphaned full-job runs with their linked
  job outcomes; it runs on every tick as a crash-recovery mechanism (orphaned
  runs whose linked job is missing are failed, completed jobs finalize the run).
- Startup recovery processes any backlog before normal scheduling resumes.

Manual cleanup is therefore a last resort, for rows the automatic sweep cannot
classify. If it is ever needed, be explicit and careful:

- Stop the service before mutating routine state.
- Back up the database first.
- Mark stale routine runs failed or cancelled.
- Service state. User groups state. Etc
- Restart and verify that `running` routine counts are clear.

## Gotify Notes

Gotify is currently treated as a WASM TOOL, and NOT a channel.

Do not set a routine's notification channel to `gotify` unless a separate
Gotify channel implementation is registered. A routine that needs Gotify should
call the `gotify` tool inside its prompt and return the same message as backup
output.

See `ic/docs/GOTIFY_ROUTINE_PROMPT.md` for the current working prompt pattern. (Verify this is correct)

## XMPP and OMEMO Notes

Known operational behavior:

- The XMPP bridge rejects conflicting runtime config with HTTP 409 until the
  bridge is restarted.
- Bridge room membership should be checked through the gateway status endpoint.
- OMEMO encrypted group chat can take several messages after restart before
  decrypting reliably.
- Group chat OMEMO requires the room to be configured/enabled as encrypted in
  both the client setup and the bridge/runtime configuration.
- The agent nickname/resource may come from XMPP service environment and bridge
  configuration, so verify both when changing display identity.

## Repository Boundaries

Canonical source areas (the core daemon and its supporting trees live under
`ic/`):

- `ic/src/`
- `ic/bridges/`
- `ic/channels-src/`
- `ic/tools-src/`
- `ic/migrations/`
- `ic/docs/`
- `ic/systemd/`
- `ic/scripts/`
- `docs/` (repo-root documentation)
- `scripts/` (repo-root helper scripts)

Sibling top-level trees also tracked as canonical include the WASM/tool
sources (`lunarwing-gotify-tool/`, `gotify-wasm/`, `darkirc_channel_for_lunarwing/`,
`lunarwing_weechat_wss/`), ops tooling (`ic-infrastructure-health-check/`,
`lunarwing_mt_onboard/`, `lunarwing_mt_onboard_web/`), and worker packaging
(`opencode4lunarwing/`, `kimicode4lunarwing/`, `pebble4lunarwing/`).

Generated, disposable, or review-carefully areas:

- `target*`
- copied working trees.
- temporary experiment folders.
- old backup directories.

Do not assume every directory in this working tree is canonical. When unsure,
ask or verify with git status and recent task context.

## Change Discipline

- Keep changes scoped.
- Respect dirty worktrees.
- Do not revert user changes unless explicitly asked.
- Avoid broad refactors unless the task requires them.
- Preserve security-sensitive behavior around auth, secrets, network listeners,
  approvals, CORS, rate limits, and sandboxing.
- Document behavior changes in the relevant docs/specs when the change is
  user-visible.

