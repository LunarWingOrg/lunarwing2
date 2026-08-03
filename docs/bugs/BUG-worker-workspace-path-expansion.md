# Worker workspace path expansion

> **Status: PARTIALLY-FIXED (verified against current source on 2026-08-03).**
> Structured worker project directories now normalize `~`; prompt text and the
> Pebble executor still pass user-supplied paths without that normalization.

This consolidates `BUG-opencode-worker-tilde-expansion.md`. Historical Nanocode
coverage became obsolete when that worker was retired.

## Original opencode symptom and reproduction

An opencode task that writes to `~/.lunarwing/projects/hello-test/hello.txt`
could create a literal `~` directory under `/workspace`, placing the file at
`<tenant>/opencode-workspace/~/.lunarwing/...` rather than at the intended
workspace path. The original report reproduced this on a rootless Podman
tenant.

The root cause was an expectation mismatch: the worker runs as a non-root user,
uses `/workspace` as its mounted working directory, and SDK project-directory
arguments are not shell-expanded.

Misplaced files were recoverable under the literal `~` directory, but follow-up
tasks could not find them at the expected path. The original proposed options
were setting `HOME=/workspace`, rewriting paths in the bridge, or relying on a
prompt-only instruction; the current implementation chose the structured-path
helper and did not implement those broader options.

## Fixes present

- `opencode4lunarwing/scripts/workspace_path.ts:1-6` maps `~` and `~/...` to
  `WORKSPACE_ROOT` and maps relative paths under that root.
- `opencode_task_executor.ts:102-104,446-448` applies the helper to structured
  `request.context.project_dir`, and `lunarwing_bridge.ts:54-56` normalizes the
  same field for per-workspace task serialization.
- The pure self-check covers `~`, `~/a/b`, absolute, relative, and `~user`
  inputs (`opencode4lunarwing/scripts/path_expand_test.ts:3-13`).

The `feat/opencode-tilde-expand` fix is contained in this branch: `bef99a3` is
an ancestor of `HEAD` (`merge-base --is-ancestor` succeeded).

## Remaining gaps

The original user prompt puts `~/.lunarwing/...` in `request.prompt`. The
OpenCode executor forwards that prompt unchanged
(`opencode4lunarwing/scripts/opencode_task_executor.ts:228,231-238`;
`lunarwing_runtime.ts:63-109`), so the helper cannot rewrite paths generated
later by the worker. Pebble likewise uses `request.context.project_dir`
directly as its process working directory
(`pebble4lunarwing/src/executor.rs:95-126`, key lines `:99-103,116`). The
daemon also forwards `project_dir` to workers verbatim
(`ic/src/orchestrator/external_worker.rs:95-102`). No `HOME=/workspace`
override is present in the opencode image (`opencode4lunarwing/Dockerfile`:
non-root user with home `/home/opencode` at `:174-179`, `USER 10001:10001` at
`:220`, no `ENV HOME`).

Therefore the structured-directory bug is fixed, but the broader prompt-driven
symptom remains possible. Pebble still needs an explicit normalization decision.

## Verification record

Verification used source inspection and Git ancestry. The checked-in OpenCode
TypeScript self-check covers the helper; Bun is now available in this
environment and `bun scripts/path_expand_test.ts` passed on 2026-08-03. No
worker container or Cargo command was run. Runtime prompt behavior remains
unverified.
