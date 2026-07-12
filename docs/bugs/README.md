# Bug tracker

Reconciled against the v2 source snapshot at base revision `c835294` on
**2026-07-12**.
Every report formerly under `docs/bugs/` was read and checked against current
source, scripts, tests, and reachable Git history. Closely related reports are
now grouped; their original filenames and technical evidence are named in the
canonical documents.

**Status vocabulary:** `FIXED` means the current tree contains the stated fix;
`OPEN` means the issue remains; `PARTIAL` means a fix covers only part of the
original symptom; `UNVERIFIED` means the source does not establish whether the
runtime symptom still reproduces. Resolved reports live under
[`history/`](history/README.md).
Some superseded files remain as compatibility pointers, including pointers in
`history/`; their current disposition is shown in the per-file table below.

## Active canonical reports

| Document | Status | Current finding | Verification |
|---|---|---|---|
| [BUG-agent-worker-lifecycle.md](BUG-agent-worker-lifecycle.md) | PARTIAL | Built-in and external `wait=true` jobs still hold the merged agent stream; the old completion race is fixed and the mpsc report is invalidated | `ic/src/tools/builtin/job.rs`, `agent_loop.rs`, `job_monitor.rs`, `orchestrator/api.rs`; source-only |
| [BUG-e2e-test-bugs.md](BUG-e2e-test-bugs.md) | PARTIAL | Bootstrap/tool tests fixed; Gmail OAuth fixture retired; clipboard test remains unverified | `agent_loop.rs`, E2E scenarios, Pytest collection attempt (64 collected; Playwright import unavailable) |
| [BUG-external-worker-config-persistence.md](BUG-external-worker-config-persistence.md) | PARTIAL | Provisioning blocks are generated; settings rewrites can drop worker bearer tokens | `lunarwing-mt-admin.sh`, `settings.rs`, `commands.rs`, `config/sandbox.rs` |
| [BUG-mt-nanocode-image-size.md](BUG-mt-nanocode-image-size.md) | OPEN | Large Nanocode image is copied per tenant with `save|load` | `lunarwing-mt-admin.sh:585-629,3428-3433`, Dockerfile |
| [BUG-kawarimi-import-flag-parity.md](BUG-kawarimi-import-flag-parity.md) | OPEN | Import rejects an explicit `--with-wasm` although it adds the flag internally | `ic/scripts/import-tenant.sh:51-69,254-260`; related flag harness |
| [BUG-ssh-git-ref-and-remote-head.md](BUG-ssh-git-ref-and-remote-head.md) | PARTIAL | Null-like refs fixed; bare remote HEAD mismatch still reports success for an empty checkout, with only raw Git stderr | `ssh_git.rs`, null-ref tests, local bare-Git reproduction |
| [BUG-weechat-relay-rand-check.md](BUG-weechat-relay-rand-check.md) | PARTIAL | Unused import fixed; `rand_check` still always returns false | `lunarwing_weechat_wss/weechat_relay/src/lib.rs:43-50,1761-1770` |
| [BUG-worker-workspace-path-expansion.md](BUG-worker-workspace-path-expansion.md) | PARTIAL | Structured OpenCode/Nanocode project paths expand `~`; prompt-generated paths and Pebble remain open | worker TypeScript/Rust files and self-check |
| [BUG-xmpp-polling-and-backpressure.md](BUG-xmpp-polling-and-backpressure.md) | PARTIAL | Poll-loop supervision/health is implemented; queue backpressure during a blocked turn remains a residual risk | WASM wrapper, channel manager, watchdog, XMPP bridge source |
| [BUG-xmpp-omemo-warmup-and-processing.md](BUG-xmpp-omemo-warmup-and-processing.md) | UNVERIFIED | Historical MUC fallback-spam has no current live verification; generic stuck-`Processing` recovery is fixed | XMPP config/tests, release v1.0.7, agent timeout path; source-only |

## Open follow-ups

These are the unresolved pieces inside grouped reports:

| Follow-up | Report |
|---|---|
| Async/session job registry for default `wait=true` calls | [agent/worker lifecycle](BUG-agent-worker-lifecycle.md#1-synchronous-worker-jobs-built-in-and-external) |
| Clipboard test runtime behavior | [E2E test bugs](BUG-e2e-test-bugs.md#2-clipboard-copy-test) |
| Preserve external-worker auth tokens across `Settings::save_toml` | [external-worker config](BUG-external-worker-config-persistence.md#2-auth-token-loss-on-settings-rewrites) |
| Slim/share Nanocode image distribution | [MT image size](BUG-mt-nanocode-image-size.md) |
| Detect bare remote HEAD mismatch after `ssh_git clone` | [SSH Git](BUG-ssh-git-ref-and-remote-head.md#2-bare-remote-head-mismatch) |
| Implement or remove WeeChat `rand_check` | [WeeChat relay](BUG-weechat-relay-rand-check.md#open-latent-behavior-bug) |
| Normalize prompt-generated worker paths and Pebble `project_dir` | [worker paths](BUG-worker-workspace-path-expansion.md#remaining-gaps) |
| Detect/mitigate WASM queue backpressure | [XMPP polling](BUG-xmpp-polling-and-backpressure.md#residual-backpressure-path) |
| Accept or clearly reject operator-supplied `--with-wasm` during Kawarimi import | [Kawarimi parity](BUG-kawarimi-import-flag-parity.md) |
| Reproduce or close OMEMO MUC fallback-spam | [OMEMO](BUG-xmpp-omemo-warmup-and-processing.md#1-omemo-fallback-spam) |

## Archived and resolved reports

| Document | Status / note |
|---|---|
| [BUG-FIXED-LAPSE.md](history/BUG-FIXED-LAPSE.md) | FIXED: XML tool-call recovery |
| [BUG-FIXED-engine-and-test-harness.md](history/BUG-FIXED-engine-and-test-harness.md) | FIXED: five engine assertions and surviving `Arc::run` harness sites |
| [BUG-FIXED-kawarimi-import-opencode.md](history/BUG-FIXED-kawarimi-import-opencode.md) | FIXED named `--with-opencode` import flag; separate `--with-wasm` parity bug is active |
| [BUG-FIXED-subagent-worker-hang.md](history/BUG-FIXED-subagent-worker-hang.md) | FIXED: completion-state race |
| [BUG-FIXED-wasm-tools-not-found-on-build.md](history/BUG-FIXED-wasm-tools-not-found-on-build.md) | FIXED: tenant-local `wasm-tools` resolution |
| [BUG-FIXED-workspace-concurrency-fixes-v1.1.0.md](history/BUG-FIXED-workspace-concurrency-fixes-v1.1.0.md) | FIXED: atomic workspace writes; one expected pooling behavior is not a bug |
| [OPENRC-MT-1.1.4-ISSUES.md](history/OPENRC-MT-1.1.4-ISSUES.md) | Historical OpenRC pass; O1/O2/O4/O5 fixed and O3 documented |
| [SYSTEMD-MT-1.1.4-ISSUES.md](history/SYSTEMD-MT-1.1.4-ISSUES.md) | Historical systemd pass; F1-F10/F12 fixed, F11 promoted to active report |
| [WEECHAT-NO-SECRET-ACCESS.md](history/WEECHAT-NO-SECRET-ACCESS.md) | FIXED: owner credential scope for WASM channels |

## Original report dispositions

This table names every original bug write-up (15 root files and 8 history
files). The status is the **primary/headline disposition** of that write-up;
grouped documents can retain a different status for a secondary subclaim. For
example, the Kawarimi `--with-opencode` report is fixed while the separate
`--with-wasm` parity gap is open, and the subagent/harness reports retain
unverified historical symptoms alongside their fixed primary changes.

| Original file | Primary status | Canonical report |
|---|---|---|
| [BUG-agent-loop-blocks-on-worker-jobs.md](BUG-agent-loop-blocks-on-worker-jobs.md) | OPEN | [agent/worker lifecycle](BUG-agent-worker-lifecycle.md) |
| [BUG-daemon-stops-polling-xmpp-bridge.md](BUG-daemon-stops-polling-xmpp-bridge.md) | PARTIAL | [XMPP polling](BUG-xmpp-polling-and-backpressure.md) |
| [BUG-e2e-bootstrap-greeting-tests.md](BUG-e2e-bootstrap-greeting-tests.md) | FIXED | [E2E tests](BUG-e2e-test-bugs.md#1-bootstrap-greeting-tests) |
| [BUG-e2e-clipboard-copy-test.md](BUG-e2e-clipboard-copy-test.md) | UNVERIFIED | [E2E tests](BUG-e2e-test-bugs.md#2-clipboard-copy-test) |
| [BUG-e2e-oauth-url-parameter-tests.md](BUG-e2e-oauth-url-parameter-tests.md) | FIXED/RETIRED | [E2E tests](BUG-e2e-test-bugs.md#3-gmail-oauth-url-parameter-fixture) |
| [BUG-e2e-tool-execution-timeout.md](BUG-e2e-tool-execution-timeout.md) | FIXED | [E2E tests](BUG-e2e-test-bugs.md#4-tool-execution-timeout) |
| [BUG-engine-crate-test-failures.md](BUG-engine-crate-test-failures.md) | FIXED/RETIRED | [engine/harness history](history/BUG-FIXED-engine-and-test-harness.md) |
| [BUG-kawarimi-import-no-opencode.md](BUG-kawarimi-import-no-opencode.md) | FIXED/RETIRED | [Kawarimi history](history/BUG-FIXED-kawarimi-import-opencode.md) |
| [BUG-opencode-worker-tilde-expansion.md](BUG-opencode-worker-tilde-expansion.md) | PARTIAL | [worker paths](BUG-worker-workspace-path-expansion.md) |
| [BUG-rust-integration-test-harness-failures.md](BUG-rust-integration-test-harness-failures.md) | FIXED/RETIRED | [engine/harness history](history/BUG-FIXED-engine-and-test-harness.md) |
| [BUG-ssh-git-bare-repo-head-mismatch.md](BUG-ssh-git-bare-repo-head-mismatch.md) | OPEN | [SSH Git](BUG-ssh-git-ref-and-remote-head.md#2-bare-remote-head-mismatch) |
| [BUG-ssh-git-null-ref-serialization.md](BUG-ssh-git-null-ref-serialization.md) | FIXED | [SSH Git](BUG-ssh-git-ref-and-remote-head.md#1-null-like-ref-values) |
| [BUG-subagent-worker-hang.md](BUG-subagent-worker-hang.md) | FIXED/RETIRED | [worker lifecycle](BUG-agent-worker-lifecycle.md#2-fire-and-forget-worker-completion-race) |
| [BUG-unbounded-mpsc-recv-in-spawned-tasks.md](BUG-unbounded-mpsc-recv-in-spawned-tasks.md) | FIXED/INVALIDATED | [worker lifecycle](BUG-agent-worker-lifecycle.md#3-notification-receiver-report) |
| [MISSING-CONFIG-FOR-NANOCODE.md](MISSING-CONFIG-FOR-NANOCODE.md) | PARTIAL | [external-worker config](BUG-external-worker-config-persistence.md) |
| [history/BUG-FIXED-LAPSE.md](history/BUG-FIXED-LAPSE.md) | FIXED | [LAPSE history](history/BUG-FIXED-LAPSE.md) |
| [history/BUG-FIXED-WEECHAT-WARNINGS.md](history/BUG-FIXED-WEECHAT-WARNINGS.md) | PARTIAL | [WeeChat rand check](BUG-weechat-relay-rand-check.md) |
| [history/BUG-FIXED-wasm-tools-not-found-on-build.md](history/BUG-FIXED-wasm-tools-not-found-on-build.md) | FIXED | [wasm-tools history](history/BUG-FIXED-wasm-tools-not-found-on-build.md) |
| [history/BUG-FIXED-workspace-concurrency-fixes-v1.1.0.md](history/BUG-FIXED-workspace-concurrency-fixes-v1.1.0.md) | FIXED | [workspace history](history/BUG-FIXED-workspace-concurrency-fixes-v1.1.0.md) |
| [history/OPENRC-MT-1.1.4-ISSUES.md](history/OPENRC-MT-1.1.4-ISSUES.md) | FIXED/RETIRED | [OpenRC history](history/OPENRC-MT-1.1.4-ISSUES.md) |
| [history/SYSTEMD-MT-1.1.4-ISSUES.md](history/SYSTEMD-MT-1.1.4-ISSUES.md) | PARTIAL | [systemd history](history/SYSTEMD-MT-1.1.4-ISSUES.md) |
| [history/WEECHAT-NO-SECRET-ACCESS.md](history/WEECHAT-NO-SECRET-ACCESS.md) | FIXED | [WeeChat secret scope](history/WEECHAT-NO-SECRET-ACCESS.md) |
| [history/XMPP-OMEMO-BUG-TO-DO.md](history/XMPP-OMEMO-BUG-TO-DO.md) | UNVERIFIED | [OMEMO fallback](BUG-xmpp-omemo-warmup-and-processing.md) |

## Consolidation map

Superseded filenames remain as short compatibility pointers (so older release
and ops links do not break); their full content was folded into canonical
reports as follows:

- `BUG-agent-loop-blocks-on-worker-jobs.md`, `BUG-subagent-worker-hang.md`, and
  `BUG-unbounded-mpsc-recv-in-spawned-tasks.md` -> agent/worker lifecycle
- `BUG-e2e-bootstrap-greeting-tests.md`, `BUG-e2e-clipboard-copy-test.md`,
  `BUG-e2e-oauth-url-parameter-tests.md`, and
  `BUG-e2e-tool-execution-timeout.md` -> E2E test bugs
- `BUG-ssh-git-bare-repo-head-mismatch.md` and
  `BUG-ssh-git-null-ref-serialization.md` -> SSH Git ref and remote HEAD
- `BUG-opencode-worker-tilde-expansion.md` -> worker workspace paths
- `BUG-daemon-stops-polling-xmpp-bridge.md` -> XMPP polling/backpressure
- `BUG-engine-crate-test-failures.md` and
  `BUG-rust-integration-test-harness-failures.md` -> archived engine/harness
- `BUG-kawarimi-import-no-opencode.md` -> archived Kawarimi import
- `MISSING-CONFIG-FOR-NANOCODE.md` -> external-worker config persistence
- `history/BUG-FIXED-WEECHAT-WARNINGS.md` -> active WeeChat `rand_check` report
- `history/XMPP-OMEMO-BUG-TO-DO.md` -> active OMEMO fallback/processing report

No source, script, test, or file outside `docs/bugs/**` was changed.

## Reconciliation tally

Counting the 23 original bug write-ups (15 root reports plus 8 historical
reports, excluding both README files) by the primary/headline disposition in
the table above: **14 FIXED/retired/invalidated, 2 OPEN, 5 PARTIAL, and 2
UNVERIFIED**. Grouping exposes **10 current open follow-ups**, including the
newly promoted OMEMO reproduction item; some live inside a `PARTIAL` canonical
document rather than having a separate file.
