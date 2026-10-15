# Harness Worker Capabilities (WI#326)

Status: implemented in v2.1.6.0. Owner: `ic/src/orchestrator/harness.rs`.
Re-checked 2026-10-14 for the v2.1.6.0 docs pass: the only harness.rs
changes after the EE-doc update (`f084624a1`) were a `cargo fmt`
normalization and the run-84 ET-3 test-only repairs (the `#[cfg(test)]`
find-counter probe pinning the EE-1 guard, the suffixed EE-3 offset-1/2
test blobs, and the new `redactor_contains_secret_folds_compact_json_escapes`
pin) — no
production behavior change; the capability contract, redaction coverage,
write-back pipeline, and WI#383 residual list below are current (the
residual list refreshed from the WI's run-84 state, which added EE-14 and
the widened EE-1 betterment and corrected EE-9's both-passes shape).

The conventional LunarWing Docker worker can run in **harness mode**
(`create_job` with `mode: "harness"`), adding three optional capability
layers on top of the plain `worker` mode. Harness jobs require the Docker
sandbox, dispatch asynchronously by default (`wait=false`), and reuse the
standard project-dir mounting under `~/.lunarwing/projects/`.

At least one capability must be requested. `create_job` is on the autonomous
denylist (missions/routines cannot reach it), and requesting `worker_secrets`,
`inject_files` with `allow_identity_injection`, or `write_back` raises the
invocation to `ApprovalRequirement::Always` — these paths sit above the
auto-approve tiers (CW-4). The elevation is evaluated on the schema-coerced
params execution will use (in both the agent dispatcher and the Engine V2
effect adapter), and is presence-based on raw params, so string-typed values
(`"allow_identity_injection": "true"`) cannot slip past it (DH-1).

All harness host state lives under `~/.lunarwing/harness/<job_id>/`. The
`harness/` base is mode 0700; job subdirectories are ordinarily 0755. This is
deliberately **outside** the projects base, which any job can mount
via an explicit `project_dir` (CW-3). Explicit `project_dir` values are
restricted to `projects/<uuid>` dirs owned by the calling user, and are
rejected outright when the job store needed for the ownership check is
unavailable (DH-7). The job's mode is persisted in the initial `agent_jobs`
insert, and the harness save fails the dispatch on error — a harness job can
never be persisted as `worker` and restart under the wrong mode (CV-9).

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
- **One-shot delivery (DG-7):** the first accepted pull marks the values served
  before its response is written; subsequent pulls get `410 Gone`, including
  retries after an interrupted response.
- Values are **never exported to the child-process env** (CV-3): the job
  reads the files (`$(cat /run/secrets/NAME)`) when a command needs them.
  File names must match `[A-Z_][A-Z0-9_]*`, must not be on the env-var
  denylist (`LD_PRELOAD`, `PATH`, …), and must not use the reserved
  `LUNARWING_`/`IRONCLAW_` prefixes (CV-4) — enforced daemon-side and
  re-checked worker-side. Bytes are written exactly (no newline trimming).
- **Redaction (CV-7, DG-1/DG-2, DZ-1/DZ-4, EB-3/EB-4):** the conventional
  worker scrubs selected value representations at the **serialized
  tool-result boundary, before worker event truncation**, and reuses that
  text for `last_output` and the LLM context. Examples include raw text,
  lines of multi-line values, JSON-escaped and URL-encoded text,
  lowercase/uppercase hex, and base64 (standard/URL-safe, padded/unpadded,
  and standard-alphabet offset-1/2 stream cores). Encoded forms are matched
  with line breaks folded out (real newlines and JSON-escaped `\n`/`\r`), so
  **any** CLI wrap width is covered from one contiguous variant — the
  `base64` 76-column default, `openssl base64` / `base64 -w 64`, `xxd -p` at
  60 columns, etc. For secrets whose encoded form is at most 4096 chars,
  individual wrap lines at the canonical widths are armed as well (a tool
  returning one line at a time); larger secrets rely on whole-blob fold
  matching so the variant count stays bounded.
  - Size limit, stated plainly (EE-2): partial coverage of a truncated
    print exists ONLY at the canonical wrap widths — 76-column base64 and
    60-column hex, whose individual lines are the ones armed — and only up
    to the 4096-char encoding cap (base64 of raw values over ~3 KiB, hex
    over ~2 KiB; a 4096-byte RSA PEM's base64 is already past it). At any
    other width (`base64 -w64`, `openssl base64`, hex at 48) even a
    below-cap `| head -5` leaks everything printed (measured: 320 of 320
    chars for raw 500-3072 B). Above the cap only the whole contiguous
    encoding is matched at all, so a partial or truncated print of such an
    encoding (`base64 secret | head -5`, or the 64 KiB shell-capture cut
    on a ≥48 KiB secret) leaks almost everything printed; measured with
    real CLI output: 285 bytes for a 5-line head of a >3 KiB secret,
    48,513 bytes for base64 of a 48-64 KiB secret, 32,231 bytes for
    `xxd -p` of a ≥32 KiB secret. Restoring partial coverage for large
    encodings needs aligned-chunk variants in a per-job automaton —
    tracked in WI#383.
  Variants are sorted longest-first across all mounted values. JSON object
  keys are also scrubbed. The daemon re-scrubs event ingest and `/complete` before
  persistence and broadcast, using a redactor built **once per job and
  cached** next to the held secrets (cleared on the same terminal paths; the
  cache insert is skipped when a terminal clear raced the build — EE-4).
  - NOT covered (documented gaps): offset/grouped dump formats — plain
    `xxd`, `od`, `hexdump -C` — which interleave offsets and ASCII columns
    and never contain a contiguous encoded span; base32 and other alphabets;
    `rev`-style reorderings. Fold matching can over-redact when unrelated
    base64/hex-looking text spans a folded line break (safe direction).
    Line-prefixed views defeat folding (EE-7): `read_file`'s `NNNNNN│ `
    gutters and `cat -n` break the folded stream at every line, so a
    wrapped encoding in that presentation can only be caught by the armed
    per-line pieces — which exist solely at the canonical widths and only
    up to the 4096-char cap. At the canonical 76-column base64 / 60-column
    hex that covers secrets up to 3072 / 2048 raw bytes and leaks above;
    at any other width (`openssl base64` at 64, `base64 -w 32`) the whole
    encoding leaks for every secret from about 56 bytes up, with no upper
    bound. The raw value itself is matched only directly, never
    fold-matched (EE-8): `fold -w N` of an unencoded token leaks from 16
    bytes up to fully. Two mounted values sharing a substring: on the
    folding slow path the sweep merges overlapping matches, but both
    fold-free passes — the `scrub_folded` fast path (encoded forms) and
    the direct pass (raw values) — still replace sequentially, so in
    fold-free text the non-overlapping tail of the second value can
    survive when only the overlapping form is printed (EE-9 residual,
    WI#383).
  - Tools can truncate **before** this boundary: shell capture is capped at
    64 KiB and then reduced to a 32 KiB head/tail result. Fragments at those
    internal cuts may survive. The partial-prefix check only protects the
    trailing cut made by `scrub_and_truncate`, not arbitrary upstream cuts.
  - The conventional worker also arms its bearer token as defense in depth;
    the ACP bridge does not. The worker's `PR_SET_DUMPABLE=0` is intended to
    prevent child access to `/proc/1/environ`; token redaction is not proof
    that such access is possible or that every output path is covered.
  - Retained variants use `SecretString` and zeroize on drop. Temporary
    encoding strings, pulled strings, and serialized copies do not all
    zeroize (DZ-8).
  - Redaction is **best-effort, not a boundary**: a job that deliberately
    slices a value (e.g. `cut -c1-32` over several calls) or encodes it in a
    form not covered above can smuggle partials out. Extracted/decoded fields
    of structured secrets (e.g. a JSON private-key field) are not guaranteed
    coverage. Unmatched text can reach events, completion reports, parent
    context and provider logs. The real control is the `Always` approval tier
    on secret mounts.
  - Values shorter than 8 bytes are not value-redacted (scrubbing would
    mangle legitimate output). They are not exported to child env, but a
    tool can still print them.

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
  never write into tenant memory directly. The bind is made writable for the
  container's worker uid per runtime (DG-6/EA-1): rootless podman ≥ 5 gets
  `userns_mode: keep-id:uid=1000,gid=1000` (the in-container uid lands on the
  daemon's non-root host uid). Rootful Docker without userns remapping is
  supported only when the daemon is uid 1000, matching the worker. Runtime
  security options are checked through the connected API; rootless/remapped
  Docker, rootful/old podman, unreported security options, and incompatible
  daemon UIDs fail closed. Staging is not chmodded 0777, and a daemon-side
  probe is not treated as evidence of worker access. The runtime test checks
  both worker writes and daemon removal of worker-created subdirectories.
- On job completion (`/complete`), after the container teardown is
  confirmed, the daemon runs the promotion pipeline detached. The job sits
  in a `Finalizing` state until promotion lands, so `wait=true` pollers
  cannot race it (CW-7), and stop/cancel/cleanup are refused or deferred
  while `Finalizing` (DH-4). For the default `wait=false` shape, completion
  is persisted, broadcast, and reflected in the parent's context BEFORE
  promotion lands (EE-5): the parent agent is told the job finished while
  promotion may still be running, and the promotion outcome reaches the
  record only as the `harness_write_back` audit event — never pushed to
  the parent, which can still pull it with the `job_events` tool.
  Stop and the exit watcher claim a terminal result under the same write
  lock used by completion; cleanup checks and removes atomically (EA-3):
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
  - A staged file named exactly like a root (`projects`) is rejected —
    promotion targets are files BELOW an allowed root (DH-9).
  - Final write goes through `Workspace::write`. Note (DH-9): the
    prompt-injection gates inside `Workspace::write` only fire for
    system-prompt files, which the hard-deny set already excludes — for
    `projects/**` the effective gates are the ones above, not
    `Workspace::write`.
- Promotion runs **only for jobs that reported success** with a confirmed
  container teardown. Every run — accepted, rejection-only, zero-accept, or
  skipped — writes a `harness_write_back` audit event with the full write
  set, every rejection with its reason, and the skip reason (CW-9). Each
  promoted record carries the owner id, the previous content hash, and —
  when the write replaced differing, non-empty content — a full snapshot at
  `backups/harness/<job_id>/<path>` (DH-5: a real rollback
  target, not just a hash). The source is read from the primary write scope;
  only `DocumentNotFound` means absent. Other read errors and snapshot-write
  errors reject the file. Snapshots use an empty search-chunk projection,
  like prompt-file shadows: no embeddings or search results (EA-4). If the
  promoted write fails and the original is confirmed intact, its unused
  snapshot is deleted; an uncertain commit/read or cleanup failure retains
  recovery evidence. The synchronous (`wait=true`) result also carries the
  outcome under `write_back`.

## Cleanup (CW-8, DH-2)

Harness dirs (injection snapshot + staging) and the held secret values are
released on the handled terminal paths: normal completion — both the promotion
(`finalize_harness_job`) and non-promotion branches of `complete_job` —
stop/cancel, the container-exit watcher (crash/OOM), `create_job` failure,
and several early-return paths between harness preparation and container
creation. Failures during partial harness preparation or before the manager's
MCP validation completes can still leave directories, and filesystem cleanup
can fail (EA-2). While a job is `Finalizing`, stop/cancel are refused and
`cleanup_job` defers, so promotion is never torn down mid-walk (DH-4). At
daemon startup a sweep removes harness dirs with no live labeled container
(1-hour grace for in-flight dispatch; skipped entirely when the runtime
cannot be listed).

## Files

- `ic/src/orchestrator/harness.rs` — capability types, path normalization
  and deny sets, provenance manifest, promotion pipeline, secret-value
  redactor, harness-dir sweep.
- `ic/src/orchestrator/job_manager.rs` — `JobMode::Harness`, runtime-aware
  tmpfs/mounts, `Finalizing` state, detached promotion, cleanup paths.
- `ic/src/orchestrator/auth.rs` — per-job harness secret storage in the
  token store (explicit terminal cleanup, not cleared by `revoke`) and the
  one-shot delivery marker.
- `ic/src/orchestrator/api.rs` — `GET /worker/{id}/harness-secrets` worker
  pull endpoint (one-shot; replay gets 410); ingest-side event redaction;
  the `/complete` daemon-side message scrub.
- `ic/src/tools/builtin/job.rs` — `create_job` params, approval elevation,
  harness dispatch, `project_dir` ownership/shape checks.
- `ic/src/worker/container.rs` — worker-side pull, tmpfs staging, and the
  tool-result-boundary scrub that precedes worker event truncation.
  `ic/src/worker/api.rs` — the client call.

## Explicitly out of scope (follow-ups)

- !1542 residual Low/Info findings: structured-field extraction redaction
  gaps (`jq -r .field` output is decoded text, not an armed encoding) and
  low-entropy line over-redaction; staged paths bypassing value scans
  and direct audit/result scrubbing; duplicate `/complete` handler side
  effects beyond the manager's idempotence (including a late `/complete`
  turning a claimed `Stopped` into a persisted `completed` — EE-6, under
  DZ-7); temporary plaintext copies;
  remaining preparation/cleanup orphans;
  no promotion deadline; remaining synchronous filesystem cleanup on async
  tasks; and stronger delivery, sparse-file and persisted-insert tests.
- !1548 run-78 residuals (WI#383): per-event scrubbing runs synchronously
  inside the async `/event` handler — beyond the landed needle-length guard
  the proper shape is one per-job `aho-corasick` automaton over the folded
  text that must also cover the DIRECT list (raw values, raw multi-line
  lines, per-line wrap pieces; a per-event leaf cap alone does not bound
  the cost) (the widened EE-1 betterment); partial/truncated prints leak
  near-wholly above the 4096-char arming cap and, at non-canonical widths,
  below it too (EE-2 — needs that automaton with
  aligned chunks); line-prefixed views (`read_file`, `cat -n`) defeat
  folding (EE-7); the raw value is never fold-matched (EE-8); both
  fold-free passes — the `scrub_folded` fast path (encoded forms) and the
  direct pass (raw values) — still replace sequentially, so the
  non-overlapping tail of a second secret sharing a substring can survive
  in fold-free text (EE-9 residual);
  an event body uploaded slowly past a token revoke is persisted
  daemon-side without the value scrub (EE-10); a per-line variant can split
  a JSON escape and leave malformed scrubbed text for the model (EE-11); a
  `wait=true` timeout during `Finalizing` reports `failed` while promotion
  continues and leaks the handle (EE-12, under EA-5); and EA-1 detection
  uses LunarWing's own effective uid rather than the runtime owner's, with
  a `tcp://` `DOCKER_HOST` resolving bind paths on the remote host — every
  such case fails closed, never a leak (EE-14, added by the run-84 ET-5
  reconciliation).
- External-worker (opencode/pebble) secrets mounting — this WI covers the
  conventional harness only.
- Concurrent-edit conflict policy: promotion snapshots the replaced content
  to `backups/harness/<job_id>/` for rollback (DH-5), but a file edited
  between dispatch and promotion is still overwritten last-write-wins —
  versioned writes / reject-and-resubmit are future work (CW-11).
- Restart of a harness job via the web gateway is refused: secrets,
  injection snapshots, and staging are per-dispatch (CV-9). Create a new
  harness job instead.
- Promoted content is written via `Workspace::write`, which also feeds the
  embedding pipeline; deployments that treat promoted artifacts as sensitive
  should note that embeddings leave the host per the workspace's configured
  provider (CW-10 residue).
