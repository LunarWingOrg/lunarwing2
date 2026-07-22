# IronClaw Port: Top 3 Easiest Additions — Implementation Plan

> **Current status (2026-07-22): OPEN / PLANNED.** None of the three items
> below have been implemented. All three are confirmed gaps against the
> current LunarWing tree. This document complements
> `IRONCLAW_TOP2_PRIORITY_PLAN.md` (which covers the two highest-priority
> security items) by selecting the three **easiest** candidates — smallest
> scope, fewest dependencies, most confined changes.

## Selection Rationale

Four source documents were reviewed:

- `docs/proposals/IRONCLAW_ADDITION_CANDIDATES.md`
- `docs/proposals/OLDPROJECT_PORT_ANALYSES/README.md`
- `docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-1.0.0-rc.1-port-analysis.md`
- `docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-reborn-port-analysis.md`

The three items below are the easiest additions because they are:

- **Small in scope** (all S), reducing risk and review burden.
- **Dependency-free** — none blocks on schema migrations, new types, or
  other port work.
- **Confirmed current gaps** — verified against LunarWing source, not just
  upstream changelogs.
- **Mechanically straightforward** — each is a localized fix in one module
  family with a clear before/after behavior.
- **Independently shippable** — each can be a standalone PR with targeted
  regression tests.

These are distinct from the two highest-priority items (WASM channel HTTP
hardening and sandbox proxy redirect SSRF) already planned in
`IRONCLAW_TOP2_PRIORITY_PLAN.md`. Those items are higher priority because
they close SSRF boundary bypasses; these three are easier because they are
correctness/robustness fixes with no security-boundary implications.

---

## Item 1: UTF-8-Safe CLI Truncation Sweep

### Source and Priority

| Field | Value |
|-------|-------|
| Source doc | `IRONCLAW_ADDITION_CANDIDATES.md` #3 |
| IronClaw reference | `90a6dadb4` (`fix(cli): prevent UTF-8 panic in MCP tool description truncation`) |
| Priority | P1/P2 |
| Size | Small |
| Dependencies | None |

### Problem

Several CLI modules truncate strings by byte index, which panics if the
byte offset lands inside a multi-byte UTF-8 character. Confirmed sites:

- `ic/src/cli/mcp.rs:535` — `&tool.description[..57]`
- `ic/src/cli/config.rs:130` — `&value[..57]`
- `ic/src/cli/reflex.rs:136` — `&p.normalized_pattern[..25]`
- `ic/src/cli/reflex.rs:141` — `&p.tool_name[..15]`
- `ic/src/cli/reflex.rs:286` — `&p.normalized_pattern[..25]`

(`ic/src/cli/tool.rs:262` uses `&hash_hex[..16]` but hex strings are
ASCII-safe, so it is not a bug.)

LunarWing already has `floor_char_boundary()` in `ic/src/util.rs` and some
CLI modules already use char-based truncation helpers correctly.

### Goals

- Eliminate all runtime panics from byte-index truncation of non-ASCII
  display strings.
- Reuse the existing `floor_char_boundary()` helper.

### Design

1. **Add a `truncate_for_display()` helper** to `ic/src/util.rs` that
   takes a `&str` and a max byte length, uses `floor_char_boundary()` to
   find the nearest valid char boundary, and appends `"..."` if truncated.

2. **Replace each byte-slice truncation** with a call to the helper:

   | File | Line | Current | Replacement |
   |------|------|---------|-------------|
   | `cli/mcp.rs` | 535 | `&tool.description[..57]` | `truncate_for_display(&tool.description, 57)` |
   | `cli/config.rs` | 130 | `&value[..57]` | `truncate_for_display(&value, 57)` |
   | `cli/reflex.rs` | 136 | `&p.normalized_pattern[..25]` | `truncate_for_display(&p.normalized_pattern, 25)` |
   | `cli/reflex.rs` | 141 | `&p.tool_name[..15]` | `truncate_for_display(&p.tool_name, 15)` |
   | `cli/reflex.rs` | 286 | `&p.normalized_pattern[..25]` | `truncate_for_display(&p.normalized_pattern, 25)` |

3. **Add regression tests** with CJK (e.g. `"日本語のツール説明"`) and
   emoji (e.g. `"🦀 tool description"`) inputs at the truncation boundary
   to prove no panic occurs.

### Adaptation Constraints

- Do not change `cli/tool.rs:262` — hex strings are ASCII-safe.
- The helper signature should be `pub fn truncate_for_display(s: &str, max_bytes: usize) -> String` — returns an owned `String` because the `"..."` suffix may require allocation.
- Keep the helper in `ic/src/util.rs` alongside `floor_char_boundary()`.

### Files to Change

- `ic/src/util.rs` — add `truncate_for_display()`.
- `ic/src/cli/mcp.rs` — replace byte-slice at line 535.
- `ic/src/cli/config.rs` — replace byte-slice at line 130.
- `ic/src/cli/reflex.rs` — replace byte-slices at lines 136, 141, 286.
- Test module for the helper and at least one CLI module regression test.

### Verification

From `ic/`:

```bash
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo check -j6
taskset -c 0-5 cargo test -j6 truncate_for_display -- --test-threads=6
taskset -c 0-5 cargo test -j6 cli -- --test-threads=6
```

Key regression test: a multi-byte string whose byte length exceeds the
truncation point does not panic and produces a valid UTF-8 string with
`"..."` appended.

---

## Item 2: Rename Reserved SSE Error Event

### Source and Priority

| Field | Value |
|-------|-------|
| Source doc | `ironclaw-1.0.0-rc.1-port-analysis.md` N2 |
| IronClaw reference | `bf67f0289` (SSE handlers + frontend reconnect) |
| Priority | P2 |
| Size | Small |
| Dependencies | None |

### Problem

`ic/src/channels/web/sse.rs:200` maps `SseEvent::Error` to the SSE event
name `"error"`. The browser's native `EventSource` API reserves the
`error` event name for transport-level connection failures. The frontend
(`ic/src/channels/web/static/app.js`) installs both `eventSource.onerror`
for transport failure and `addEventListener('error', ...)` for application
failures.

When the server emits an application error frame named `error`, it can
enter both the message path and the connection-failure handling path. This
displays a false disconnect/reconnecting state after a valid server-sent
error and can reset stream UI while the browser is still connected.

The serde rename is at `ic/src/channels/web/types.rs:207`
(`#[serde(rename = "error")]`) and the event-type string is at
`types.rs:1162`.

### Goals

- Stop using the reserved `error` SSE event name for application errors.
- Preserve transport-level `onerror` exclusively for connection state.
- Keep the wire payload shape unchanged (only the event name changes).

### Design

1. **Rename the SSE event name** from `"error"` to `"stream_error"` in:
   - `ic/src/channels/web/sse.rs:200` — the `event_type` match arm.
   - `ic/src/channels/web/types.rs:207` — the `#[serde(rename = "error")]`
     attribute should become `#[serde(rename = "stream_error")]`.
   - `ic/src/channels/web/types.rs:1162` — the fallback event-type string.

2. **Update the frontend listener** in
   `ic/src/channels/web/static/app.js`: change the `addEventListener`
   target from `'error'` to `'stream_error'`. Leave `eventSource.onerror`
   untouched (it handles genuine transport failures).

3. **Update existing tests** that assert the `"error"` event name (e.g.
   `types.rs:1466` which asserts `parsed["type"] == "error"`).

4. **Add a browser-level regression** proving an application failure does
   not increment reconnect attempts or mark the connection disconnected.

### Adaptation Constraints

- Do not change the `SseEvent::Error` variant name in the Rust enum — only
  the wire-format event name and serde rename.
- Do not remove or weaken the `onerror` transport handler.
- This is independent of, but should precede, the larger durable SSE replay
  work (P1-D / N16).
- The frontend change is a single string rename in the event listener.

### Files to Change

- `ic/src/channels/web/sse.rs` — event-type string (line 200).
- `ic/src/channels/web/types.rs` — serde rename (line 207), event-type
  string (line 1162), and test assertion (line 1466).
- `ic/src/channels/web/static/app.js` — event listener target.
- Any other test that asserts the `"error"` SSE event name.

### Verification

From `ic/`:

```bash
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo check -j6
taskset -c 0-5 cargo test -j6 sse -- --test-threads=6
```

Key regression test: an `SseEvent::Error` frame serializes with event name
`stream_error`, not `error`. A frontend listener for `stream_error`
receives the payload; `onerror` is not triggered.

---

## Item 3: Fix Built-in HTTP Tool Leak-Scan Ordering

### Source and Priority

| Field | Value |
|-------|-------|
| Source doc | `ironclaw-reborn-port-analysis.md` P2-A |
| IronClaw reference | `ironclaw_host_runtime/egress` scan-then-inject ordering |
| Priority | P2 |
| Size | Small |
| Dependencies | None |

### Problem

`ic/src/tools/builtin/http.rs` performs two leak scans:

1. **Line 517-520:** Pre-injection scan on caller-provided
   URL/headers/body — correct.
2. **Line 611-614:** Post-injection scan that runs AFTER credentials have
   been injected into `headers_vec` (lines 574-607) — incorrect.

The second scan can self-block on the tool's own legitimately-injected
credentials. Because `LeakDetector` has Block patterns for
`github_token`, `google_api_key`, `aws_access_key`, etc., injecting a
managed `Authorization: Bearer ghp_...` header makes the tool block its
own credential, producing a false-positive `SecretLeakBlocked →
NotAuthorized` error.

This is distinct from the WASM channel leak-scan ordering issue
(Addition Candidates #2) — that concerns the WASM channel host path,
while this concerns the built-in `http` tool path.

### Goals

- Eliminate false-positive self-blocks from host-injected credentials.
- Preserve exfiltration detection for caller-supplied secrets.
- Maintain the pre-injection scan as the primary defense.

### Design

1. **Remove the post-injection leak scan** (lines 611-614). The
   pre-injection scan at line 517 already covers caller-supplied URL,
   headers, and body. Host-injected credentials are trusted by definition
   (they come from the encrypted secrets store, not from guest input).

2. **Alternatively** (if defense-in-depth is preferred): split the
   post-injection scan to only scan the original caller-supplied portions
   of `headers_vec`, not the injected credential headers. This is more
   complex and less clearly beneficial since the pre-injection scan
   already covers those.

   **Recommendation:** Remove the post-injection scan. It is redundant
   with the pre-injection scan for caller-supplied values, and it is the
   sole cause of the self-block false positive for injected credentials.

3. **Add regression tests:**
   - A `ghp_`-shaped secret injected as an http credential via the
     secrets store is **not** blocked.
   - A `ghp_`-shaped value in a caller-supplied header or body **is**
     blocked by the pre-injection scan.

### Adaptation Constraints

- Do not remove or weaken the pre-injection scan (line 517).
- Do not change the `LeakDetector` block patterns.
- Do not change credential injection logic — only the scan ordering.
- While here, consider wiring `record_usage()` on the injection path if it
  currently skips it, but keep that as a separate concern if it adds scope.

### Files to Change

- `ic/src/tools/builtin/http.rs` — remove or fix the post-injection scan
  (lines 611-614).
- Test module for http tool credential injection + leak scan interaction.

### Verification

From `ic/`:

```bash
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo check -j6
taskset -c 0-5 cargo test -j6 http_tool -- --test-threads=6
```

Key regression test: a `ghp_`-shaped secret injected as an http credential
is **not** blocked, but a `ghp_` value in a caller-supplied body **is**.

---

## Implementation Order

1. **Item 1 (UTF-8-safe CLI truncation)** first — pure mechanical fix,
   lowest risk, establishes the shared helper.
2. **Item 2 (SSE error event rename)** second — small, self-contained,
   but touches frontend JS so needs slightly more verification.
3. **Item 3 (HTTP tool leak-scan ordering)** third — touches security-
   adjacent code so needs the most careful regression tests of the three.

All three should be separate PRs with targeted regression tests.

## Items Explicitly Deferred

These are other small candidates from the same documents but are either
not as easy, carry dependencies, or overlap with the top-2 priority plan:

- WASM channel leak-scan ordering (Addition Candidates #2) — P1 but
  depends on the HTTP hardening helper from the top-2 priority plan.
- Engine `AccessDenied` → non-existence (P2-C) — S but touches 15 call
  sites across the engine crate.
- Effect-level floors (P2-G) — S-M, needs policy engine design decisions.
- Gate tamper-tripwire (P2-F) — trivial but does not solve the real
  crash-safety problem (durable consume/execute).
- Typed `JobResultStatus` (Addition Candidates #4) — P2, medium refactor.
- `ExternalThreadId` newtype (Addition Candidates #5) — P2, medium scope.
- Row-bound secret AAD (P1-A / N1) — requires migration design first.
- Browser send idempotency (N16) — medium scope, ledger design needed.

## Verification Summary

All three items are documentation-only in this plan. Implementation PRs
should run from `ic/`:

```bash
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo check -j6 --no-default-features --features postgres
taskset -c 0-5 cargo check -j6 --no-default-features --features libsql
taskset -c 0-5 cargo test -j6 -- --test-threads=6
```

Each fix needs a regression test proving the specific bug is fixed.
