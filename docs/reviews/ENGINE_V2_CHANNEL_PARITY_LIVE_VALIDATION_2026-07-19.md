# Engine V2 Channel Parity Live Validation - 2026-07-19

## Result

The WeeChat slice of CHPAR-012 passed on disposable tenant `chparlive` under
OpenRC. DarkIRC and XMPP live protocol validation remain pending, so CHPAR-012
is not complete.

## Environment

- Tenant lifecycle was managed only through
  `ic/scripts/lunarwing-mt-admin.sh`.
- The tenant ran with `ENGINE_V2=true` and
  `ENGINE_V2_CHANNELS=xmpp,darkirc,weechat`.
- Nanocode, Pebble, and OpenCode workers were not created or started.
- The loopback IRC fixture listened only on `127.0.0.1:16667` and exposed no
  external network service.
- The workspace release binary was installed as `chparlive:chparlive`, mode
  `0755`. Its SHA-256 was
  `4dece5eaeef80a89a27334f111bf783cb74ee5f09bdb5981ab0cabb30ed0cc2c`.
- `restart-tenant chparlive` completed successfully and restarted the daemon,
  PostgreSQL, WeeChat relay and adapter, XMPP bridge, DarkIRC services, and
  vision sidecar through OpenRC.

## Live WeeChat Evidence

| Scenario | Result | Observable evidence |
| --- | --- | --- |
| Reactive DM | Pass | `CHPAR-WEECHAT-DM-POSTDEPLOY-OK-20260719` arrived once at `probe`. |
| Group fallback auth privacy | Pass | Fresh fallback gates were observed in `pending-gates.json`; neither synthetic credential marker was emitted to the group. |
| Completed-gate interrupt | Pass | `/interrupt` returned `Interrupted.`, removed the exact scoped gate, and a subsequent group sentinel arrived once. |
| Completed-gate clear | Pass | `/clear` returned `Conversation cleared.`, removed the exact scoped gate, and a subsequent group sentinel arrived once. |
| Proactive DM | Pass | The message tool reported success for `irc.chpar.probe`, and the exact sentinel arrived once at the IRC client. |
| Proactive group | Pass | The message tool reported success for `irc.chpar.#chpar`, and the exact sentinel arrived once in `#chpar`. |
| Scope separation | Pass | PostgreSQL recorded distinct scoped conversations for `weechat:dm:v2:nick:chpar:probe` and `weechat:group:chpar:#chpar`. |

The exact post-deploy response sentinels each appeared once in the fixture wire
log. The two group auth credential markers appeared only in inbound fixture
prompts and never in an outbound `chparlive` group message.

## Security And Cleanup Checks

- No credential token was submitted during either auth fallback scenario.
- PostgreSQL contained zero secret rows matching the three synthetic CHPAR
  credential names used during this validation.
- The expired pre-fix fixture gate was removed after confirming there were no
  active gates; `pending-gates.json` now contains zero gates.
- Service and fixture evidence contains synthetic identifiers only, not secret
  values.
- `/etc/lunarwing/ports.json` still records all external workers as disabled
  for `chparlive`.
- Unrelated tenant `starforce` was not modified.

## Verification

- Router regressions: 3 passed.
- Scoped router/control tests: 17 passed.
- Core WeeChat-filtered tests: 7 passed.
- Auth-clear tests: 2 passed.
- Engine V2 channel delivery matrix: passed.
- PostgreSQL compile check: passed.
- Workspace release build: passed.
- Standalone WeeChat relay suite after deployment: 56 passed.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed before the live run.
- Clippy was unavailable because the selected Rust toolchain did not include
  the `cargo-clippy` component; the host toolchain was not modified.

## Remaining CHPAR-012 Work

- Run the DarkIRC live scenarios if that protocol is brought back into scope.
- Run the XMPP normal-chat and privacy scenarios.
- Purge `chparlive` and verify registry cleanup only after explicit operator
  authorization.
