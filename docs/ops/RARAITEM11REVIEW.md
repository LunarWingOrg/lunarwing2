# MCP Part 3 Item 11 Review

**Reviewed branches:** `rarity/kimi-11-20260816-0001`, `rarity/kimi-11-20260816-0601`, `rarity/kimi-11-20260816-1202`

**Status:** Resolved on `rarity/kimi-11-20260816-1202` (2026-08-17)

## Executive Summary

The bundled gateway SSE passthrough work landed in stages. Early iterations
implemented streaming and basic hardening, then closed coverage gaps. A later
review found remaining availability and secret-handling issues. Those issues
are now fixed on `rarity/kimi-11-20260816-1202`.

## Resolution History

### 2026-08-16 — `rarity/kimi-11-20260816-0601`

Addressed the original streaming review:

1. Stream selection keys off upstream `Content-Type`; SSE has no cumulative
   lifetime byte cap.
2. Connect timeout + per-read timeout cut stalled upstreams.
3. Concurrency bounding and mid-stream `tracing::warn!` observability.
4. Method-gate removed so POST/DELETE SSE streams.
5. Non-SSE responses buffer under the body cap (clean `502` before status).
6. Docs corrected: listener is plain HTTP; only upstreams are HTTPS-restricted.

### 2026-08-16 — `rarity/kimi-11-20260816-1202` (coverage)

Added injectable timeouts for tests and three tests:

- stalled upstream cut by read timeout
- downstream disconnect cancels upstream stream
- non-allowlisted upstream response headers stripped

### 2026-08-17 — `rarity/kimi-11-20260816-1202` (merge blockers)

Closed the remaining design issues:

1. **Separate stream and transient permit pools.** Long-lived SSE no longer
   shares a single 64-permit quota with tool-call POSTs. Filling the stream
   pool returns `503 too many concurrent streams` while transient POSTs still
   succeed.
2. **Request-body deadline under the transient permit.** Downstream body reads
   are bounded; slow/incomplete bodies return `408` and release capacity.
3. **Upstream URL sanitization.** Route validation rejects userinfo
   credentials. Logs and client-facing errors use scheme/host/port/path only
   (no userinfo, query, or fragment). Upstream failures return a generic
   `upstream error` body.
4. **Quiet SSE idle policy.** Production stream `read_timeout` defaults to
   300 s (resets per chunk) so healthy quiet notification streams are not
   forced to reconnect every 30 s. Connect/time-to-headers/buffered overall
   timeout remains 30 s.
5. **Bearer-token validation matches header construction.** Tokens must be
   1..=4096 visible ASCII characters and must form a valid `Authorization`
   header value at validation time.

## Current Implementation Contract

| Concern | Behavior |
| --- | --- |
| SSE selection | Upstream `Content-Type: text/event-stream` (case/params tolerant) |
| SSE body cap | None (chunk relay; no cumulative lifetime total) |
| Buffered body cap | 4 MiB; oversized/failed bodies → clean `502` |
| Transient concurrency | 64 permits for setup + buffered responses |
| Stream concurrency | 64 permits for long-lived SSE only |
| Request body | 4 MiB max, overall deadline (default 30 s) → `408` on timeout |
| Stream idle | Per-read timeout default 300 s |
| Upstream URL | No userinfo; sanitized in logs/errors |
| Bearer token | Visible ASCII, max 4096, header-valid at validate |
| Response headers | Forward only `content-type` and `mcp-session-id` |

## Verification

Targeted suite:

```text
taskset -c 0-11 cargo test -j12 --lib gateway_control -- --test-threads=12
```

Expected coverage includes:

- stream quota full while transient POST succeeds
- request-body timeout and capacity reclaim
- userinfo rejection and URL sanitization unit tests
- bearer-token rejection for spaces, non-ASCII, and oversize
- prior SSE/header/stall/disconnect tests

## Deferred (outside this item)

- Marketplace install/upgrade CLI surface
- Automatic npm/pip installation (blocked until full trust chain)
- Cross-daemon gateway HA / cross-tenant credential ownership
- Persistent external-worker MCP delivery
- Per-server configurable timeouts for every transport
