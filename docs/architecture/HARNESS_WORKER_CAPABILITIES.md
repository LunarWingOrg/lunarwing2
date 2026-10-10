# Harness Worker Capabilities (WI#326)

Status: implemented in v2.1.6.0. Owner: `ic/src/orchestrator/harness.rs`.

The conventional LunarWing Docker worker can run in **harness mode**
(`create_job` with `mode: "harness"`), adding three optional capability
layers on top of the plain `worker` mode. Harness jobs require the Docker
sandbox, dispatch asynchronously by default (`wait=false`), and reuse the
standard project-dir mounting under `~/.lunarwing/projects/`.

At least one capability must be requested; `create_job` itself stays behind
the standard tool-approval gate (`UnlessAutoApproved`), so dispatch —
including any write-back — is never silently auto-approved beyond the
configured tiers.

## 1. tmpfs secret mounts (`worker_secrets`)

- `worker_secrets` maps secret-store names to file names, e.g.
  `{"github_token": "GITHUB_TOKEN"}`.
- The daemon resolves values host-side (same posture as the existing
  `credentials` map) and uploads them as a tar into a **tmpfs** mounted at
  `/run/secrets` (`mode=0700,uid=1000,gid=1000,noexec,nosuid`). Files land
  as `0440`, owned by the worker uid.
- File names must match `[A-Z_][A-Z0-9_]*`; the worker exports each file as
  an env var for child processes (values never enter the container env, job
  context, logs, or job events). Reading is gated on
  `LUNARWING_HARNESS_MODE=1` so host `/run/secrets` dirs are never picked up.
- Upload happens after container start and the job fails to create if the
  upload fails — a job never runs with a partial secret set.

## 2. Read-only file/memory injection (`inject_files`)

- `inject_files` lists workspace documents (`"MEMORY.md"`,
  `"projects/x/notes.md"`, or `{"source": ..., "target": ...}`) snapshotted
  at dispatch into `~/.lunarwing/projects/.harness/<job_id>/injected/` and
  mounted **read-only** at `/harness/injected`.
- Each file is recorded in `PROVENANCE.json` with source path, SHA-256,
  size, and timestamp.
- Identity documents (`SOUL.md`, `USER.md`, `IDENTITY.md`) are rejected
  unless `allow_identity_injection: true`.
- The mount is one-way: the job cannot write back through it.

## 3. Staged write-back (`write_back`)

- Opt-in per job (`write_back: true` or an object overriding
  `allowed_roots`, `max_files`, `max_file_bytes`, `max_total_bytes`).
- A separate staging dir is mounted RW at `/harness/workspace_out`. Jobs
  never write into tenant memory directly.
- On job completion (`/complete`), the daemon promotes staged files into the
  tenant workspace only after all checks pass:
  - path allowlist (default `projects/`), relative-path traversal rejected;
  - identity documents, engine runtime/internal paths (`engine/**`, WI#322),
    and backup paths are never writable;
  - per-file/per-total size limits and file-count limits;
  - UTF-8 only; `LeakDetector` secret scan must be clean;
  - final write goes through `Workspace::write`, so the standard
    prompt-injection gates still apply.
- Every promotion run is recorded as a `harness_write_back` job event with
  the full write set (path, SHA-256, size, job id, timestamp) and every
  rejection with its reason. The synchronous (`wait=true`) result also
  carries the outcome under `write_back`.

## Files

- `ic/src/orchestrator/harness.rs` — capability types, validation,
  provenance manifest, promotion pipeline, secrets tar builder.
- `ic/src/orchestrator/job_manager.rs` — `JobMode::Harness`, container
  mounts/tmpfs/env, post-start secrets upload, promotion on completion.
- `ic/src/tools/builtin/job.rs` — `create_job` params and harness dispatch.
- `ic/src/worker/container.rs` — worker-side `/run/secrets` consumption.

## Explicitly out of scope (follow-ups)

- External-worker (opencode/pebble) secrets mounting — this WI covers the
  conventional harness only.
- Conflict policy beyond last-write-wins: `Workspace::write` semantics apply
  to promoted files; versioned writes / reject-and-resubmit are future work.
- Restart of a harness job via the web gateway re-runs as a plain worker
  (harness options are per-dispatch and not persisted).
