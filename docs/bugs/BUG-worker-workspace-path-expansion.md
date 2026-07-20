# Worker workspace path expansion

> **Status: PARTIALLY-FIXED (verified against `51ae5a8` on 2026-07-20).**
> Structured worker project directories now normalize `~`; prompt text and the
> Pebble executor still pass user-supplied paths without that normalization.

This consolidates `BUG-opencode-worker-tilde-expansion.md` and the analogous
Nanocode coverage discovered while checking the current worker tree.

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
- `opencode_task_executor.ts:52-54,252-253` applies the helper to structured
  `request.context.project_dir`.
- The pure self-check covers `~`, `~/a/b`, absolute, relative, and `~user`
  inputs (`opencode4lunarwing/scripts/path_expand_test.ts:3-13`).
- Nanocode has the same helper and structured-directory wiring
  (`lunarcode4lunarwing/scripts/workspace_path.ts:1-6`,
  `scripts/nanocode_task_executor.ts:52-54,219-220`).

The `feat/opencode-tilde-expand` fix is contained in this branch: `bef99a3` is
an ancestor of `HEAD` (`merge-base --is-ancestor` succeeded).

## Remaining gaps

The original user prompt puts `~/.lunarwing/...` in `request.prompt`. The
opencode and Nanocode executors forward that prompt unchanged
(`opencode4lunarwing/scripts/opencode_task_executor.ts:99-102`,
`lunarcode4lunarwing/scripts/nanocode_task_executor.ts:100-103`), so the helper
cannot rewrite paths generated later by either worker. Pebble likewise uses
`request.context.project_dir` directly as its process working directory
(`pebble4lunarwing/src/executor.rs:86-107`). No `HOME=/workspace` override is
present in the opencode image (`opencode4lunarwing/Dockerfile:159-163,198`).

Therefore the structured-directory bug is fixed, but the broader prompt-driven
symptom remains possible. The old claim that Nanocode and Pebble were entirely
unchanged is stale: Nanocode now has the helper; Pebble still needs an explicit
normalization decision.

## Verification record

Verification used source inspection and Git ancestry. The checked-in OpenCode
and Nanocode TypeScript self-checks cover the helper, but fresh execution on
2026-07-20 was blocked because Bun is not installed in this environment. No
worker container or Cargo command was run. Runtime prompt behavior remains
unverified.
