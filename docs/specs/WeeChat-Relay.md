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
10. The generated configuration must contain the expected API section/port, loopback bind, and literal environment expression, and must not contain the resolved password.
11. New-tenant provisioning accepts `--no-weechat-bootstrap`. The flag skips
    WeeChat command execution and `relay.conf` generation, still writes the minimal
    `weechat.env` containing only `RELAY_PASSWORD`, still renders WeeChat services,
    and is not forwarded by Kawarimi import. The flag is supported by direct
    `add-tenant`/`add-tenants`, the Python onboarding CLI
    (`TenantConfig.no_weechat_bootstrap`), the browser provision wizard
    (`ProvisionRequest.no_weechat_bootstrap`), and the OpenRC bulk provisioner
    (`ENABLE_WEECHAT_BOOTSTRAP`).

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