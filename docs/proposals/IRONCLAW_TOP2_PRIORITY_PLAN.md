# IronClaw Port: Top 2 Highest-Priority Additions — Implementation Plan

> **Current status (2026-07-22): OPEN / PLANNED.** Neither item has been
> implemented. Both are confirmed gaps against the current LunarWing tree.
> This document synthesizes findings from `IRONCLAW_ADDITION_CANDIDATES.md`
> and the three `OLDPROJECT_PORT_ANALYSES/` documents and plans the two
> highest-priority additions.

## Selection Rationale

Four source documents were reviewed:

- `docs/proposals/IRONCLAW_ADDITION_CANDIDATES.md`
- `docs/proposals/OLDPROJECT_PORT_ANALYSES/README.md`
- `docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-1.0.0-rc.1-port-analysis.md`
- `docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-reborn-port-analysis.md`

The two items below are the highest-priority additions because they are:

- **Consistently P1** across every document that rates them.
- **Confirmed current gaps** — verified against LunarWing source, not just
  upstream changelogs.
- **Small in scope** (S to S-M), reducing risk and review burden.
- **Dependency-free** — neither blocks on schema migrations, new types, or
  other port work.
- **Security-critical** — both close SSRF / egress-boundary bypasses on
  network surfaces that handle untrusted or extension-supplied input.
- **Ranked first** in every recommended implementation order across the
  four documents.

The RC range audit groups them together as the single highest-priority
work item: *"Existing WASM channel HTTP/leak-scan fixes and sandbox
redirect SSRF."*

Other strong candidates (row-bound secret AAD, browser-send idempotency,
reserved SSE event rename) are P1/P2 but carry migration-design or
medium-size dependencies that place them behind these two confined
security fixes.

---

## Item 1: Harden WASM Channel HTTP Egress

### Source and Priority

| Field | Value |
|-------|-------|
| Source doc | `IRONCLAW_ADDITION_CANDIDATES.md` #1 |
| IronClaw reference | `792357b7b` (`fix: WASM channel HTTP SSRF protections`) |
| Priority | P1 |
| Size | Small to medium |
| Dependencies | None |

### Problem

LunarWing's WASM **tool** host already has the full SSRF hardening pattern:

- pre-request private/internal IP rejection unless `ALLOW_PRIVATE_IPS=1`,
- `reqwest::redirect::Policy::none()`,
- dedicated runtime inside `spawn_blocking`.

Reference: `ic/src/tools/wasm/wrapper.rs` around the HTTP request path.

The WASM **channel** host does **not** match that posture:

- `ic/src/channels/wasm/wrapper.rs` builds a plain
  `reqwest::Client::builder()` with no redirect policy.
- Channel HTTP does not perform the same private/internal IP rejection
  before sending.

WASM channels are extension-supplied network surfaces. A channel callback
that can follow redirects or resolve private/internal targets is an SSRF
path distinct from the built-in `http` tool and from the sandbox proxy
(Item 2 below).

### Goals

- Close the WASM channel SSRF gap so channel HTTP egress matches the
  tool-side security posture.
- Preserve LunarWing's `ALLOW_PRIVATE_IPS=1` local/multi-tenant escape
  hatch.
- Reuse existing tool-side HTTP security helpers rather than duplicating
  logic.

### Design

1. **Factor a shared HTTP security helper** from the existing tool-side
   code in `ic/src/tools/wasm/wrapper.rs`. The helper should encapsulate:
   - redirect policy (`Policy::none()` or per-hop revalidation),
   - private/internal IP rejection gated on `ALLOW_PRIVATE_IPS`,
   - the `spawn_blocking` execution context.

2. **Apply the helper to the channel path** in
   `ic/src/channels/wasm/wrapper.rs`, replacing the plain
   `reqwest::Client::builder()`.

3. **Add channel-side regression tests:**
   - redirect blocking: a `302` from the channel callback is not followed,
   - private/internal target rejection: requests to `127.0.0.1`,
     `10.x`, `192.168.x`, `169.254.169.254` are rejected by default,
   - `ALLOW_PRIVATE_IPS=1` positive case: local/private targets succeed
     when the escape hatch is set.

### Adaptation Constraints

- Do **not** import IronClaw's `deny_private_ip_ranges` verbatim.
  LunarWing intentionally supports local/private endpoints via
  `ALLOW_PRIVATE_IPS=1`; the helper must gate on that env var.
- Do not change the built-in `http` tool path — it is already hardened
  and documented in `docs/proposals/HTTP_TOOL_SSRF_PROTECTIONS.md`.
- Keep the helper inside the WASM module family
  (`ic/src/tools/wasm/` and `ic/src/channels/wasm/`), not in a new crate.

### Files to Change

- `ic/src/channels/wasm/wrapper.rs` — apply hardened client builder.
- `ic/src/tools/wasm/wrapper.rs` — factor shared helper (if extracting).
- New or existing test module for channel HTTP regression tests.

### Verification

From `ic/`:

```bash
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo check -j6
taskset -c 0-5 cargo test -j6 wasm_channel -- --test-threads=6
```

Network hardening requires both negative tests (redirect/private blocked)
and positive tests (allowed local/private under `ALLOW_PRIVATE_IPS=1`).

---

## Item 2: Sandbox Proxy Redirect SSRF

### Source and Priority

| Field | Value |
|-------|-------|
| Source doc | `ironclaw-reborn-port-analysis.md` P1-B; also in `IRONCLAW_ADDITION_CANDIDATES.md` |
| IronClaw reference | `ironclaw_network/transport.rs` (`redirect::Policy::none()` + revalidate) |
| Priority | P1 |
| Size | Small |
| Dependencies | None |

### Problem

`ic/src/sandbox/proxy/http.rs` builds `ProxyState.http_client` with
`reqwest::Client::new()` and **no redirect policy**. Reqwest 0.12 then
follows up to 10 redirects that are **never re-checked against the
allowlist**. `decider.decide()` runs once *before* `forward_request`; the
`Location` host is never re-validated.

This proxy is the **sole egress boundary for untrusted Docker worker
containers** (`ic/src/sandbox/manager.rs` `build_and_start`). Net effect:
an allowlisted host can issue a `302` redirecting a container's request to
**any** host — including `169.254.169.254` (cloud metadata) or the
operator's private network — and the proxy fetches it and returns the
body, carrying any header/query-injected credentials across the redirect.

This is a confirmed trust-boundary bypass and SSRF. It is distinct from
Item 1: Item 1 covers WASM channel egress; this covers the container
sandbox proxy egress.

### Goals

- Stop the sandbox proxy from following redirects to off-allowlist hosts.
- Preserve all existing allowlist and IP-policy behavior, including
  `ALLOW_PRIVATE_IPS=1` deployments.

### Design

Two viable approaches (pick one):

**Option A — Disable redirects (simpler, matches Reborn):**

Set `redirect::Policy::none()` on `ProxyState.http_client`. In
`forward_request`, return the upstream `3xx` to the container unmodified.
A container re-request re-enters `decider.decide()`, so the redirect
destination is naturally re-validated.

**Option B — Revalidate on redirect (more transparent to containers):**

Keep redirect following but re-run `state.decider.decide()` on the
`Location` host before following each redirect. Reject if the destination
is off-allowlist.

**Recommendation:** Option A is simpler, matches the IronClaw/Reborn
pattern, and is the lower-risk change. Containers that need to follow
redirects can handle `3xx` responses themselves.

### Adaptation Constraints

- Do **not** import `deny_private_ip_ranges` — this is orthogonal to IP
  policy. `ALLOW_PRIVATE_IPS=1`, TensorZero `192.168.1.157`, and local
  Ollama deployments must keep working.
- The change is confined to the proxy client build and the
  `forward_request` redirect handling. Do not change `policy.rs` or
  `manager.rs` unless the redirect policy plumbing requires it.

### Files to Change

- `ic/src/sandbox/proxy/http.rs` — client build (~L89) and
  `forward_request` (~L327-441).
- New or existing test module for proxy redirect regression.

### Verification

From `ic/`:

```bash
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo check -j6
taskset -c 0-5 cargo test -j6 sandbox_proxy -- --test-threads=6
```

Key regression test: a `302` from an allowlisted host to an
off-allowlist host is **not** fetched by the proxy.

---

## Implementation Order

1. **Item 1 (WASM channel HTTP hardening)** first — it is the larger of
   the two and establishes the shared helper that could be referenced by
   other WASM paths.
2. **Item 2 (sandbox proxy redirect SSRF)** second — it is the smaller,
   more confined fix.

Both should be separate PRs with targeted regression tests.

## Items Explicitly Deferred

These are strong candidates from the same documents but are not the top 2
because they carry migration design, medium-to-large scope, or
dependencies on other work:

- Row-bound secret AAD (P1-A / N1) — requires migration design first.
- Browser chat send idempotency (N16) — medium scope, ledger design needed.
- Reserved SSE event rename (N2) — P2, quick but lower priority.
- WASM channel leak-scan ordering (addition candidates #2) — P1 but
  depends on the HTTP hardening helper from Item 1.
- Bounded concurrent WASM work (addition candidates #6) — P1/P2, medium
  scope, recommended after Items 1 and 2.
- Typed `JobResultStatus` (addition candidates #4) — P2, medium refactor.
- `ExternalThreadId` newtype (addition candidates #5) — P2, medium scope.
- Durable SSE event log (P1-D) — large, needs its own design.
- WASM credential authority ceiling (P1-C) — medium, needs current-surface
  re-verification first.

## Verification Summary

Both items are documentation-only in this plan. Implementation PRs should
run from `ic/`:

```bash
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo check -j6 --no-default-features --features postgres
taskset -c 0-5 cargo check -j6 --no-default-features --features libsql
taskset -c 0-5 cargo test -j6 -- --test-threads=6
```

Security changes require both negative tests (redirect/private blocked)
and positive tests (intentional private-network configurations preserved).
