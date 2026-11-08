# Harness Worker Capabilities (WI#326)

Status: implemented; source revalidated 2026-11-01 for v2.1.8.0.
Owner: `ic/src/orchestrator/harness.rs`, with lifecycle ownership in
`job_manager.rs` and HTTP completion handling in `api.rs`.

The v2.1.7.0 WI#383 changes close two formerly documented gaps:

- Staged relative paths are scanned for mounted secret representations before
  promotion. `scrub_promotion_summary` also scrubs record/backup paths,
  rejection paths and reasons, and the skip reason before the direct audit and
  synchronous result are stored.
- `complete_job` returns `JobAlreadyCompleted` when another terminal path has
  claimed the job. `report_complete` acknowledges that duplicate and returns
  before status persistence, SSE result broadcast, or parent-context updates.
  A late `/complete` therefore cannot overwrite a claimed stop/cancel outcome.
  `JobFinalizing` separately identifies a stop refused during promotion.

Regression anchors are `promotion_rejects_staged_paths_embedding_mounted_secrets`,
`scrub_promotion_summary_redacts_leaked_paths_and_reasons`, and
`report_complete_after_terminal_claim_skips_side_effects`. The remaining
limitations below describe source behavior, not a promise of exhaustive secret
redaction.

The v2.1.9.0 WI#383 changes close four more residual gaps:

- Bounded direct and folded secret variants are searched through per-job
  Aho-Corasick automatons instead of one search per variant per event; long raw
  values and whole encodings use sequential scans and suppress redundant chunk
  searches in their spans. Encoded values also arm aligned 48-character base64
  and 32-character hex chunks, so large and non-canonically wrapped partial
  output is scrubbed in aligned units.
- Per-line JSON-escaped variants prevent a raw line ending in a backslash from
  splitting a JSON escape during replacement.
- A dispatch drop guard removes partial harness directories on every return
  before successful container creation, including preparation and MCP-validation
  failures.
- Write-back promotion has a five-minute deadline. `wait=true` treats
  `Finalizing` separately from worker execution and waits for that bounded
  outcome instead of reporting a false timeout and abandoning the handle.

The v2.1.9.0 WI#580 follow-up closes three more residuals:

- JSON-mounted secrets arm decoded values of secret-like fields recursively
  (including PEM bodies and JSON documents nested in strings) without arming
  public credential metadata. Traversal is bounded to four parsing levels and
  256 unique secret fields per mounted value.
- Fold matching recognizes and removes `read_file`'s six-column `NNNNNN│ `
  gutters and `cat -n`'s six-column tab-separated prefixes, so non-canonical
  wrap widths and large encodings remain covered in those presentations.
- The worker parses serialized tool results before scrubbing them. For results
  explicitly marked `truncated: true`, partial base64/hex chunks are redacted
  at the output boundaries and at up to 16 shell head/tail cut markers.
  Precomputed, sorted variant indexes keep each boundary check logarithmic in
  the variant count; ordinary untruncated output does not pay this extra work.

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
- **Redaction (CV-7, DG-1/DG-2, DZ-1/DZ-4, EB-3/EB-4, EE-8/EE-9, DZ-5):** the
  conventional worker scrubs selected value representations at the
  **serialized tool-result boundary, before worker event truncation**, and
  reuses that text for `last_output` and the LLM context. Examples include raw
  text, lines of multi-line values, JSON-escaped and URL-encoded text,
  lowercase/uppercase hex, and base64 (standard/URL-safe, padded/unpadded,
  and standard-alphabet offset-1/2 stream cores). Encoded forms and the raw
  value itself are matched with line breaks folded out (real newlines and
  JSON-escaped `\n`/`\r`), so **any** CLI wrap width is covered from one
  contiguous variant — the `base64` 76-column default, `openssl base64` /
  `base64 -w 64`, `xxd -p` at 60 columns, etc. — and so is a token re-wrapped
  by `fold -w N` or `fmt` (EE-8). Folded matches are translated back to
  original offsets per character, so a value ending in a multi-byte character
  (a password ending in `é` or `€`) is redacted instead of panicking the
  replacement sweep. Every direct and folded match is collected before any
  replacement and merged in one sweep (EE-9), so a second value sharing a
  substring cannot leave its non-overlapping tail behind. For secrets whose
  encoded form is at most 4096 chars, individual wrap lines at the canonical
  widths are armed as direct variants as well (a tool returning one line at a
  time). Independently of that cap, encoded forms arm aligned 48-character
  base64 and 32-character hex chunks in the folded list, including a final
  short chunk of at least eight characters. A partial or truncated print is
  therefore scrubbed in those aligned units at any wrap width, including above
  the old cap and at non-canonical widths. Per-line variants skip RFC 7468
  armor delimiters and lines with no alphanumeric byte (DZ-5): a
  punctuation-only line of a multi-line value is therefore not armed on its
  own, though its whole value and its other lines still are. Per-line
  JSON-escaped forms are armed alongside the raw lines, so replacing a line
  that ends in a backslash cannot leave malformed JSON (EE-11). Decoded values
  of JSON-mounted fields receive the same variant treatment only when their key
  is secret-like: case-insensitive `*_key`, a key containing `token` or
  `secret`, exact `password` / `passphrase`, or a key beginning `credential`.
  OAuth `token_uri`, `token_url`, and `token_endpoint` fields are endpoints,
  not token values; other public metadata such as `type`, `project_id`,
  `client_email`, and URLs is not armed separately either. The complete mounted
  value remains armed, and nested traversal retains the depth and field caps
  listed above (DZ-2).
  Fold matching also strips the exact six-column `read_file` and `cat -n`
  prefixes before matching, preserving a map back to original offsets (EE-7).
  Variants are sorted longest-first across all mounted values. JSON object
  keys are also scrubbed. Direct and folded lists each have one per-job
  Aho-Corasick automaton for variants up to 4096 bytes, built once and cached
  next to the held secrets. Longer raw values and whole encodings use direct
  sequential scans; their folded matches suppress redundant chunk searches in
  the covered spans, with a bounded overlap margin around remaining gaps. This
  avoids pathological automaton construction and overlapping-match volume on
  repetitive maximum-size values (EE-1). The daemon re-scrubs event ingest and
  `/complete` before persistence and broadcast; the redactor is cleared on the
  same terminal paths, and cache insertion is skipped when a terminal clear
  raced the build (EE-4).
  - NOT covered (documented gaps): offset/grouped dump formats — plain
    `xxd`, `od`, `hexdump -C` — which interleave offsets and ASCII columns
    and never contain a contiguous encoded span; base32 and other alphabets;
    `rev`-style reorderings. Fold matching can over-redact when unrelated
    base64/hex-looking text spans a folded line break (safe direction).
    Line-prefix formats other than the exact recognized `read_file` / `cat -n`
    gutters can still defeat folding.
  - Tools can truncate **before** this boundary: shell capture is capped at
    64 KiB per stream and combined output can be reduced to a 32 KiB head/tail
    result. When the result says `truncated: true`, the worker's parsed-result
    scrub redacts partial ASCII base64/hex chunks at the final capture cut and
    on both sides of up to 16 `... [truncated N bytes] ...` markers
    (DG-2/DZ-3/EE-2). Arbitrary offset slices, unmarked cuts, non-ASCII
    partials, and markers beyond that cap are not guaranteed coverage.
  - The conventional worker also arms its bearer token as defense in depth;
    the ACP bridge does not. The worker's `PR_SET_DUMPABLE=0` is intended to
    prevent child access to `/proc/1/environ`; token redaction is not proof
    that such access is possible or that every output path is covered.
  - Retained variants use `SecretString` and zeroize on drop. Automaton
    transition tables, temporary encoding strings, pulled strings, and
    serialized copies do not all zeroize; automatons are dropped with the
    redactor on the same terminal cleanup paths (DZ-8).
  - Redaction is **best-effort, not a boundary**: a job that deliberately
    slices a value (e.g. `cut -c1-32` over several calls) or encodes it in a
    form not covered above can smuggle partials out. Structured fields beyond
    the documented depth/count caps and decoded content inside those fields
    (for example, a base64 field decoded to another payload) are not
    recursively transformed beyond the armed variants. Unmatched text can
    reach events, completion reports, parent context and provider logs. The
    real control is the `Always` approval tier on secret mounts.
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
stop/cancel, the container-exit watcher (crash/OOM), and `create_job` failure.
A dispatch drop guard also removes the per-job directory on every return before
successful container creation, including partial preparation and manager-side
MCP-validation failures; filesystem cleanup itself can still fail (EA-2). While
a job is `Finalizing`, stop/cancel are refused and `cleanup_job` defers, so
promotion is never torn down mid-walk (DH-4). Promotion has a five-minute
deadline, after which the finalizer records a skipped outcome, persists the
audit event, flips to `Stopped`, and releases harness state. The deadline can
cancel promotion between a rollback snapshot and its write, leaving an extra
backup; cancellation during `Workspace::write` inherits that backend's
atomicity, and the retained snapshots provide recovery evidence. A `wait=true`
poller does not apply its worker-execution timeout while this bounded promotion
is running. At daemon startup a sweep removes harness dirs with no live labeled
container (1-hour grace for in-flight dispatch; skipped entirely when the
runtime cannot be listed).

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

- Offset/grouped dump formats (`xxd`, `od`, `hexdump -C`), base32 and other
  alphabets, and `rev`-style reorderings remain deliberately uncovered
  (EB-3/DZ-10). Supporting each would require another adversarial normalizer
  and variant family in a best-effort scrubber, not a security boundary; the
  `Always` approval tier remains the control. Arbitrary offset slices are
  likewise not claimed to be detected.
- Remaining !1542 Low/Info findings: low-entropy line over-redaction beyond
  the landed filter (armor delimiters and lines with no alphanumeric byte are
  skipped, but short structural lines such as `apiVersion: v1` and
  `kind: Config` are still armed), temporary plaintext copies, remaining
  synchronous filesystem cleanup on async tasks, and stronger delivery,
  sparse-file and persisted-insert tests. Structured-field extraction (DZ-2),
  staged-path/audit scrubbing (DZ-6), and duplicate `/complete` side effects
  (DZ-7/EA-7/EE-6) are fixed as described above.
- Remaining !1548/run-84 residuals (WI#383): an event body uploaded slowly
  past a token revoke can be persisted daemon-side without the value scrub
  (EE-10), an accepted race in terminal-path cleanup; and EA-1 detection uses
  LunarWing's own effective uid rather than the runtime owner's, with a
  `tcp://` `DOCKER_HOST` resolving bind paths on the remote host — every such
  case fails closed, never a leak (EE-14). Line-prefixed views (EE-7) and the
  explicitly marked shell capture/head-tail cuts (DG-2/DZ-3/EE-2) are fixed
  within the limits documented above.
- Runtime-gated coverage (DG-6): the keep-id + podman tmpfs `U`-flag
  combination and runtime-gated write-back tests still self-skip without a
  container runtime and `lunarwing-worker:latest`. The required Rust lanes do
  not provide both, and the protected external-worker candidate jobs do not
  publish that conventional-worker image. A runtime-equipped required master
  lane is still needed to exercise write-back plus secrets together on every
  pipeline; no such live coverage is claimed here.
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
