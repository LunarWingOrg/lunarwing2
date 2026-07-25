# Spec: WeeChat-Relay

Scope: feature

# WeeChat Relay Auto-Bootstrap

## Purpose

Automatically provision a working WeeChat HTTP API relay for newly created LunarWing multi-tenant tenants without manual tmux interaction, while preserving existing WeeChat configuration and minimizing credential exposure.

## Functional requirements

1. Fresh `add-tenant` flows attempt a one-shot WeeChat relay bootstrap after tenant environment generation and before expensive service startup work.
2. Bootstrap uses WeeChat's supported repeated `--run-command` interface and configures:
   - `relay.network.password` as the literal expression `${env:RELAY_PASSWORD}`.
   - `relay.network.allow_empty_password=off`.
   - `relay.network.ipv6=off` before applying the IPv4 loopback bind.
   - `relay.network.bind_address=127.0.0.1`.
   - `/relay add api <registry weechat port>`.
   - `/save`, followed by `/quit`.
3. The startup argument must escape the expression as `\${env:RELAY_PASSWORD}` so WeeChat stores the expression rather than resolving and persisting the secret during command evaluation.
4. `configure-weechat-relay <tenant>` provides an explicit root-only retry/recovery operation using the same implementation as automatic setup.
5. The explicit command returns nonzero on failure. Automatic `add-tenant` setup failure is non-fatal to base provisioning but must emit a prominent degraded-state warning and exact retry command.
6. Existing non-empty `~/.config/weechat` content is never overwritten or reconciled automatically. The operation fails before mutating the target or its managed credential environment.
7. Generation occurs in a tenant-owned temporary sibling directory on the same filesystem. The complete generated directory is validated before atomic promotion. Every failure path removes temporary state.
8. A dedicated tenant-owned `env/weechat.env`, mode `0600`, contains only `RELAY_PASSWORD`. The persistent systemd user unit and OpenRC service load this file before starting tmux/WeeChat.
9. The persistent WeeChat process must not receive the complete `lunarwing.env` because it contains unrelated DB, LLM, XMPP, and gateway secrets.
10. The generated configuration must contain the expected API section/port, loopback bind, `ipv6=off`, and literal environment expression, and must not contain the resolved password.
11. New-tenant provisioning accepts `--no-weechat-bootstrap`. The flag skips
    WeeChat command execution and `relay.conf` generation, still writes the minimal
    `weechat.env` containing only `RELAY_PASSWORD`, still renders WeeChat services,
    and is not forwarded by Kawarimi import. The flag is supported by direct
    `add-tenant`/`add-tenants`, the Python onboarding CLI
    (`TenantConfig.no_weechat_bootstrap`), the browser provision wizard
    (`ProvisionRequest.no_weechat_bootstrap`), and the OpenRC bulk provisioner
    (`ENABLE_WEECHAT_BOOTSTRAP`).

## Managed session preservation

12. Managed shutdown sends the exact FIFO record `*/upgrade -quit`. The leading
    `*` selects the current buffer for FIFO input; `/upgrade -quit` writes
    `*.upgrade` session files and exits. `*:/upgrade -quit` is invalid and must
    never be used because the colon changes the command payload.
13. The stop helper selects only a FIFO whose PID suffix names a live process
    in the target tmux pane's process tree. FIFO writes are bounded so a stale
    pipe with no reader cannot hang OpenRC shutdown or `restart-tenant`.
14. After a successful FIFO write, the helper waits for WeeChat to exit. Missing
    FIFO, write failure, or graceful-exit timeout emits a warning and falls back
    to `tmux kill-session`. A surviving tmux session is an error, not success.
15. Persistent service units start through `lunarwing-weechat-start.sh`. The
    helper adds `--upgrade` when `<weechat-home>/weechat.upgrade` exists and
    otherwise performs a normal `--dir <weechat-home>` start.
16. The systemd user unit and OpenRC service use the same start and stop helpers.
    OpenRC invokes both paths under the tenant UID because tmux sockets and FIFO
    files are tenant-owned.
17. `weechat.look.save_layout_on_exit buffers` preserves layout ordering as a
    fallback. It does not replace the `/upgrade -quit` and `--upgrade` handoff
    required to reopen buffers with their session content.
18. Existing tenants must re-render service units after upgrading LunarWing so
    their units reference the managed start and stop helpers.

## Security invariants

- Never place the resolved relay password in argv, a shell command string, stdout/stderr, logs, rendered unit text, test evidence, or `relay.conf`.
- Never use `/relay addreplace`.
- Never bind the relay to a non-loopback address.
- Missing runtime `RELAY_PASSWORD` must remain fail-closed through WeeChat's default `allow_empty_password=off` behavior.
- Fixture and smoke tests use dummy credentials only.

## Platform requirements

- systemd support targets per-tenant user-manager units and must not use the system bus.
- OpenRC is first-class and must receive equivalent environment loading and lifecycle behavior.
- Tenant lifecycle logic remains inside `ic/scripts/lunarwing-mt-admin.sh`; wrappers must not duplicate init-specific behavior.

## Verification requirements

- Use TDD with standalone Bash harnesses that source the admin script and stub root/service operations.
- Cover happy path, empty destination, existing-config conflict, missing password, missing WeeChat binary, invocation failure, malformed generated config, promotion failure, cleanup, and secret non-disclosure.
- Render both systemd and OpenRC outputs into fixture directories and assert canonical service names and minimal environment loading.
- Assert the exact `*/upgrade -quit` FIFO payload, bounded blocked-FIFO fallback,
  conditional `--upgrade` startup, and failure when destructive fallback cannot
  terminate the tmux session.
- Exercise a real WeeChat cycle when available: create a buffer, stop, verify
  `*.upgrade` files, start, verify the files are consumed and the buffer returns.
- Extend the read-only WeeChat preflight with relay config, loopback, registry-port, password-expression, and minimal-env checks without printing secrets.
- Provide an opt-in installed-WeeChat smoke test that uses `/tmp/opencode`, a dummy credential, and an ephemeral port; it must leave no process, listener, or temporary artifact.
- Final verification requires plan-compliance, code-quality/security, real-QA, and scope-fidelity approval.

## Out of scope

- Existing-tenant migration or automatic repair.
- Password rotation.
- Kawarimi import propagation of the opt-out flag (Kawarimi import deliberately does not expose `--no-weechat-bootstrap`).
- Engine V2 channel behavior.
- Pairing workflow changes.
- Health/self-heal expansion.
- Service `PartOf` or unrelated hardening changes.
- Port allocation changes or unrelated mt-admin refactoring.

## Source plan

Decision-complete implementation details are maintained in `.omo/plans/weechat-relay-auto-bootstrap.md` and registered as plan `WeeChat-Bootstrap`.
