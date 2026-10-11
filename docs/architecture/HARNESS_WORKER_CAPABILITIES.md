# Harness Worker Capabilities (WI#326)

Status: implemented in v2.1.6.0. Owner: `ic/src/orchestrator/harness.rs`.

The conventional LunarWing Docker worker can run in **harness mode**
(`create_job` with `mode: "harness"`), adding three optional capability
layers on top of the plain `worker` mode. Harness jobs require the Docker
sandbox, dispatch asynchronously by default (`wait=false`), and reuse the
standard project-dir mounting under `~/.lunarwing/projects/`.

At least one capability must be requested. `create_job` is on the autonomous
denylist (missions/routines cannot reach it), and requesting `worker_secrets`,
`inject_files` with `allow_identity_injection`, or `write_back` raises the
invocation to `ApprovalRequirement::Always` — these paths sit above the
auto-approve tiers (CW-4).

All harness host state lives under `~/.lunarwing/harness/<job_id>/` (mode
0700) — deliberately **outside** the projects base, which any job can mount
via an explicit `project_dir` (CW-3). Explicit `project_dir` values are
restricted to `projects/<uuid>` dirs owned by the calling user.

## 1. tmpfs secret mounts (`worker_secrets`)

- `worker_secrets` maps secret-store names to file names, e.g.
  `{"github_token": "GITHUB_TOKEN"}`.
- The daemon resolves values host-side (same posture as the existing
  `credentials` map, including usage recording) and holds them in the
  per-job token store. The worker **pulls** them over its authenticated
  per-job channel (`GET /worker/{id}/harness-secrets`) and writes them into
  a **tmpfs** mounted at `/run/secrets` itself, mode `0440` (CV-2). No tar
  upload through the container API, so values never touch overlay layers.
- The tmpfs options are runtime-aware: Docker gets
  `size=4M,mode=0700,uid=1000,gid=1000,noexec,nosuid`; podman (which rejects
  `uid=`/`gid=` through the compat API) gets `size=4M,mode=0700,noexec,nosuid,U`.
  Both leave the mount owned by the container's worker uid, mode 0700.
- **Fail-closed delivery:** the daemon announces the expected file names via
  `LUNARWING_SECRET_FILES` (names only). The worker exits before any job work
  unless the delivered set matches exactly — a job never runs with a partial
  secret set.
- Values are **never exported to the child-process env** (CV-3): the job
  reads the files (`$(cat /run/secrets/NAME)`) when a command needs them.
  File names must match `[A-Z_][A-Z0-9_]*`, must not be on the env-var
  denylist (`LD_PRELOAD`, `PATH`, …), and must not use the reserved
  `LUNARWING_`/`IRONCLAW_` prefixes (CV-4) — enforced daemon-side and
  re-checked worker-side. Bytes are written exactly (no newline trimming).
- **Redaction (CV-7):** exact, base64, and URL-encoded forms of every
  mounted value are scrubbed from job events by the worker before sending
  and again by the daemon at ingest, so `cat /run/secrets/X` output never
  reaches persisted events, SSE, or the parent agent's context. Values
  shorter than 8 bytes are not value-redacted (scrubbing would mangle
  legitimate output); they still never enter env/logs by construction.

## 2. Read-only file/memory injection (`inject_files`)

- `inject_files` lists workspace documents (`"MEMORY.md"`,
  `"projects/x/notes.md"`, or `{"source": ..., "target": ...}`) snapshotted
  at dispatch into `~/.lunarwing/harness/<job_id>/injected/` and mounted
  **read-only** at `/harness/injected`.
- Source paths must be in canonical form: normalized == raw, no `.`/`..`,
  no backslashes, no empty segments, no segment with leading/trailing
  whitespace or control characters (CW-5). The normalized path is what gets
  read and recorded.
- `backups/`, `engine/**`, and quarantined daily logs
  (`daily/*.poisoned-*.md`) are never injectable — they carry quarantined
  injection evidence and engine-private state. Identity documents
  (SOUL/USER/IDENTITY/AGENTS/TOOLS/BOOTSTRAP, by exact path or basename at
  any depth) require `allow_identity_injection: true`.
- Each file is recorded in `PROVENANCE.json` with normalized source path,
  SHA-256, size, and timestamp. Files are written with
  `O_CREAT|O_EXCL|O_NOFOLLOW` and chmodded via the open fd (0444), so a
  planted symlink cannot redirect the write onto a host file (CW-3f).
- The mount is one-way: the job cannot write back through it.

## 3. Staged write-back (`write_back`)

- Opt-in per job (`write_back: true`, or an object that may only **narrow**
  the roots and **lower** the limits). Default off.
- A separate staging dir is mounted RW at `/harness/workspace_out`. Jobs
  never write into tenant memory directly.
- On job completion (`/complete`), after the container teardown is
  confirmed, the daemon runs the promotion pipeline detached (the job sits
  in a `Finalizing` state until it lands, so waiters cannot race it — CW-7):
  - **Allowlist is operator-fixed to `projects/**`** (CW-1). Callers may
    narrow (e.g. `projects/demo/`) but never widen or replace it.
  - Hard-denied regardless of roots: identity documents, system-prompt files
    (MEMORY.md, HEARTBEAT.md, AGENTS.md, …), `daily/`, `lorebooks/`,
    `context/`, `backups/`, quarantined logs, and engine-internal paths
    (`engine/**`, WI#322).
  - Staged paths must be canonical (normalized == raw) — whitespace-aliased
    names like `" SOUL.md"` are rejected before `Workspace::write` can trim
    them onto a denied target (CW-2).
  - Symlinks are never followed (lstat walk + `O_NOFOLLOW|O_NONBLOCK` open +
    fstat regular-file check + parent canonicalization containment); fifos
    fail fast instead of hanging the daemon (CW-3d).
  - Server-side ceilings: ≤ 256 files, ≤ 1 MiB per file, ≤ 8 MiB total;
    lstat size pre-check runs before any read, and reads are bounded
    (`take(max+1)`); the walk is capped at 4096 entries and the rejection
    list at 128 entries (CW-6).
  - UTF-8 only; `LeakDetector` scan plus an exact-value scan against the
    job's own mounted secrets (CW-10).
  - Final write goes through `Workspace::write`, so the standard
    prompt-injection gates still apply.
- Promotion runs **only for jobs that reported success** with a confirmed
  container teardown. Every run — accepted, rejection-only, zero-accept, or
  skipped — writes a `harness_write_back` audit event with the full write
  set, every rejection with its reason, and the skip reason (CW-9). Each
  promoted record carries the owner id and the previous content hash for
  rollback. The synchronous (`wait=true`) result also carries the outcome
  under `write_back`.

## Cleanup (CW-8)

Harness dirs are removed on every terminal path: normal completion (via
`cleanup_job`), stop/cancel, the container-exit watcher (crash/OOM), and
`create_job` failure. At daemon startup a sweep removes harness dirs with no
live labeled container (1-hour grace for in-flight dispatch; skipped
entirely when the runtime cannot be listed).

## Files

- `ic/src/orchestrator/harness.rs` — capability types, path normalization
  and deny sets, provenance manifest, promotion pipeline, secret-value
  redactor, harness-dir sweep.
- `ic/src/orchestrator/job_manager.rs` — `JobMode::Harness`, runtime-aware
  tmpfs/mounts, `Finalizing` state, detached promotion, cleanup paths.
- `ic/src/orchestrator/auth.rs` — per-job harness secret storage in the
  token store (cleared on every terminal path, not on `revoke`).
- `ic/src/orchestrator/api.rs` — `GET /worker/{id}/harness-secrets` worker
  pull endpoint; ingest-side event redaction.
- `ic/src/tools/builtin/job.rs` — `create_job` params, approval elevation,
  harness dispatch, `project_dir` ownership/shape checks.
- `ic/src/worker/container.rs` — worker-side pull, tmpfs staging, event
  redaction. `ic/src/worker/api.rs` — the client call.

## Explicitly out of scope (follow-ups)

- External-worker (opencode/pebble) secrets mounting — this WI covers the
  conventional harness only.
- Conflict policy beyond last-write-wins: promotion records carry the
  previous content hash to enable rollback, but versioned writes /
  reject-and-resubmit are future work (CW-11).
- Restart of a harness job via the web gateway is refused: secrets,
  injection snapshots, and staging are per-dispatch (CV-9). Create a new
  harness job instead.
- Promoted content is written via `Workspace::write`, which also feeds the
  embedding pipeline; deployments that treat promoted artifacts as sensitive
  should note that embeddings leave the host per the workspace's configured
  provider (CW-10 residue).
