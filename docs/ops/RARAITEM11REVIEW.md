# Review

## Resolution (2026-08-16, rarity/kimi-11-20260816-0601)

All findings addressed on branch `rarity/kimi-11-20260816-0601` (work
cherry-picked from `rarity/kimi-11-20260816-0001`, then hardened):

1. Fixed — stream selection keys off upstream `Content-Type`;
   `text/event-stream` relays chunks with no cumulative cap; the 4 MiB cap
   governs request bodies and buffered responses only.
2. Fixed — single upstream client with `connect_timeout` + per-read
   `read_timeout` (30 s); stalled upstreams are cut, SSE heartbeats reset it.
3. Fixed — 64-permit semaphore bounds concurrent `/mcp` connections; fast
   `503` on saturation; permits release on stream end or client disconnect.
4. Fixed — method-based client gate removed; POST/DELETE SSE streams.
5. Fixed — non-SSE responses buffer under the cap; oversized bodies and
   mid-body failures return clean `502` before any status line.
6. Fixed — mid-stream and buffered failures `tracing::warn!` route, upstream,
   transferred bytes, and reason; saturating arithmetic.
7. Fixed — docs now state the listener is plain HTTP (loopback by default)
   and only upstreams are HTTPS-restricted; TLS proxy noted for non-loopback.

Coverage gaps closed: stream crossing 4 MiB, POST SSE, declared + chunked
oversized buffered 502, mid-body failure 502, 64-stream saturation and permit
reclamation (6 new tests; 13/13 `gateway_control` tests pass). Still open:
live stalled-upstream read-timeout test (30 s constant too slow for unit
tests), downstream-disconnect cancellation test, response-header allowlist
enforcement test.

## Review Findings
1.
High: the 4 MiB cumulative cap breaks long-lived SSE by design.

ic/src/tools/mcp/gateway_control.rs:558-577
 counts every byte received during the connection and terminates once the lifetime total exceeds 4 MiB. A valid notification stream with events or heartbeat comments will eventually be cut off even though memory is not accumulating. LunarWing then treats the body error as retryable (
http_transport.rs:649-654
), creating repeated disconnect/reconnect cycles and notification-loss windows. The limit should apply to buffered bytes or individual SSE events, not lifetime traffic.
2.
High: a silent upstream can pin the stream forever.

The new stream_client has only connect_timeout (
gateway_control.rs:362-367
). The outer timeout at lines 521-529 ends once response headers arrive. After that, an upstream that stops producing chunks retains the upstream socket, downstream socket, request task, and notification listener indefinitely. This also prevents the client’s reconnect loop from recovering. A reqwest read_timeout, which resets after each successful chunk, fits the intended SSE behavior.
3.
High: 
/mcp
 permits unlimited unauthenticated streaming connections.

Authentication is applied only to the 
/admin/*
 router; 
/mcp
 is registered outside that layer at 
gateway_control.rs:374-385
. There is no global or per-route semaphore. Each GET can therefore hold two sockets and associated tasks indefinitely. This is locally exploitable by any process and remotely exploitable when LUNARWING_MCP_GATEWAY_CONTROL_ALLOW_NON_LOOPBACK=1 is enabled. Long-lived streaming needs bounded ownership, not only a body-byte cap.
4.
Medium: POST SSE is still terminated after 30 seconds.

Client selection is based on request method at 
gateway_control.rs:516-520
. POST uses request_client, whose reqwest total timeout covers the response body through completion. MCP Streamable HTTP permits text/event-stream responses to POST, and LunarWing explicitly advertises and parses that form. Such tool calls still fail after 30 seconds. This existed before the branch, but it means the documentation’s broad “streams responses” claim is incomplete.
5.
Medium: ordinary JSON response error semantics regress.

Every response now streams immediately, not only text/event-stream. For chunked JSON larger than 4 MiB or an upstream read failure, the gateway has already returned the upstream status, commonly 200, and can only abort the body. Previously it buffered and returned a clean 502. Streaming should be selected from the upstream Content-Type; bounded JSON can retain the previous behavior.
6.
Low: mid-stream failures have no gateway-side observability.

Upstream read failures, arithmetic overflow, and cap exhaustion become anonymous io::Errors at 
gateway_control.rs:567-572
. Nothing records the route, upstream, reason, or transferred size with tracing. Operators will see reconnect churn without a gateway explanation.
7.
Low: documentation overstates transport security.

docs/ops/MCP_OVERALL_STATUS.md:57-60
 calls this an “HTTPS-only 
/mcp
 data plane.” The listener is plain axum::serve over TCP; only configured upstream URLs are HTTPS-restricted. This wording is particularly risky because non-loopback binding is supported and the admin bearer token otherwise travels over cleartext unless an external TLS proxy is deployed.
Coverage Gaps
The new test proves only two small GET chunks and header forwarding. Missing cases include:
•
A stream crossing the cumulative 4 MiB boundary
•
An upstream that sends headers and then stalls
•
Concurrent stream saturation and permit reclamation
•
POST returning SSE for longer than 30 seconds
•
Oversized declared and chunked responses
•
Downstream disconnect canceling the upstream body
•
Upstream body failure after headers
•
Response-header allowlist enforcement
Verdict
I would not merge this branch as complete. It implements the basic GET/SSE passthrough and the focused suite passes 7/7, but it does not yet provide bounded, recoverable production streaming. The first three findings are merge blockers.