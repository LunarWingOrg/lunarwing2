# MCP Part 3 Item 11 Review

**Reviewed branch:** `rarity/kimi-11-20260816-0601`

**Reviewed commit:** `84cef0e3a3344a63391ce0919656055c2ca8d979`

**Comparison base:** `origin/aragents/kimil00p/v2.0.8.0` at `c1258e6371f8fd13b07f11368993e9c0d6a1defe`

**Review date:** 2026-08-16

**Status:** Changes requested

## Executive Summary

This branch is a substantial improvement over
`rarity/kimi-11-20260816-0001`. It correctly implements incremental SSE
passthrough for the bundled MCP gateway and resolves the original review's
most visible response-streaming defects. The focused tests, local compile
checks, and core GitLab pipeline jobs pass.

The branch is not ready to merge. The new global semaphore allows permanent
SSE listeners to consume all capacity and starve ordinary MCP tool calls. It
also holds permits while reading unauthenticated request bodies without a
deadline, which permits a straightforward slow-request denial of service. The
new observability paths can expose credentials embedded in upstream URLs.
These are security and availability issues in a listener that may be exposed
off-loopback.

## Scope Reviewed

The branch is ten commits ahead of the comparison base and changes:

- `ic/src/tools/mcp/gateway_control.rs`
- `docs/ops/AGENT_GOALS_2.0.8.0.md`
- `docs/ops/MCP_ADDITIONS.md`
- `docs/ops/MCP_OVERALL_STATUS.md`
- `docs/proposals/V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md`
- This review document

The implementation review covered route validation, authentication boundaries,
header filtering, request and response limits, reqwest timeout semantics, SSE
classification, semaphore ownership, downstream disconnect behavior, logging,
test coverage, and the documented Part 3 completion claim.

## Finding Summary

| Severity | Finding | Merge blocking |
| --- | --- | --- |
| High | Long-lived notification streams can starve every transient MCP request | Yes |
| High | Slow request bodies can hold all proxy permits indefinitely | Yes |
| Medium | Raw upstream URLs can disclose embedded credentials | Yes |
| Medium | A fixed 30-second read timeout churns valid quiet SSE streams | No |
| Low | Bearer-token validation accepts values that header construction rejects | No |
| Low | Important negative and cancellation tests remain absent | No |

## Current Findings

### 1. High: Notification Streams Can Starve All Tool Calls

`ic/src/tools/mcp/gateway_control.rs:514-520` acquires one permit from a
global 64-permit semaphore for every proxied request. For an SSE response, the
permit moves into `StreamState` at lines 626-641 and remains held for the
stream's entire lifetime.

This bounds total connections, but it does not preserve capacity for normal MCP
requests. Once 64 healthy notification GET streams are open, every POST or
DELETE receives `503 Service Unavailable` until one stream disconnects. The
route table supports 256 routes, so normal configured use can trigger this
without malicious traffic. An unauthenticated caller can trigger it more
directly by opening 64 streams for a known route.

The regression test at lines 1479-1558 explicitly saturates all permits with
SSE streams and then asserts that a normal POST receives `503`. It therefore
codifies starvation rather than preventing it.

**Required remediation:** use distinct capacity controls for long-lived streams
and transient requests. Reserve transient request capacity even when the
stream quota is full. Because POST may also return SSE, acquire transient
capacity while establishing the upstream response, then transfer to a separate
stream permit after classifying `Content-Type`.

**Required regression test:** fill the stream quota, verify an additional
stream is rejected, and verify ordinary POST/DELETE requests still complete.

### 2. High: Request-Body Slowloris Defeats the Connection Bound

The semaphore permit is acquired at
`ic/src/tools/mcp/gateway_control.rs:514-520`. The handler then reads the
untrusted body with `axum::body::to_bytes` at lines 526-529 without a deadline.
The size cap limits bytes but does not limit how slowly they arrive.

The `/mcp` data plane is not protected by the admin bearer middleware. Any local
process, or any remote caller when
`LUNARWING_MCP_GATEWAY_CONTROL_ALLOW_NON_LOOPBACK=1`, can open 64 requests with
valid route headers and slowly drip request bytes. Those requests retain every
permit indefinitely, and all subsequent data-plane requests fail with `503`.
No upstream connection is required for the attack.

**Required remediation:** apply a bounded request-body read deadline and return
`408 Request Timeout` or another deliberate timeout response. Do not charge a
long-lived stream permit while collecting a downstream request body.

**Required regression test:** hold incomplete request bodies open, advance a
short test-configured deadline, and verify permits are reclaimed.

### 3. Medium: Upstream URLs Can Leak Credentials

`GatewayRoute::validate` at
`ic/src/tools/mcp/gateway_control.rs:89-112` allows URL userinfo, query
parameters, and fragments. These components commonly carry access tokens.

The complete configured upstream URL is written to logs at lines 650-655 and
684-686. The pre-response error path at lines 542-546 also returns reqwest's
formatted error to the unauthenticated data-plane client. Reqwest 0.13 includes
the request URL in its `Display` output; it strips URL userinfo while building
the request but retains query parameters. The explicit tracing fields retain
the original configured string, including both userinfo and query data.

Examples at risk include:

```text
https://user:password@mcp.example.com/mcp
https://mcp.example.com/mcp?api_key=secret
```

**Required remediation:** reject URL userinfo during route validation and use a
sanitized URL for all logs and client-facing errors. At minimum, remove
userinfo, query, and fragment. Return a stable generic upstream error to the
data-plane caller rather than reqwest's raw error string.

**Required regression tests:** configure userinfo and query secrets, force a
connection/read error, and assert that neither logs nor response bodies contain
the secrets.

### 4. Medium: Quiet SSE Streams Reconnect Every 30 Seconds

The shared reqwest client uses `read_timeout(DEFAULT_UPSTREAM_TIMEOUT)` at
`ic/src/tools/mcp/gateway_control.rs:377-380`, where the default is 30 seconds.
Reqwest resets this timeout after each received chunk, which correctly detects
a stalled stream when the server emits regular heartbeats.

MCP does not require every healthy server to send a heartbeat within 30
seconds. A valid but quiet notification stream is therefore disconnected every
30 seconds. LunarWing treats the body error as retryable, reconnects, advances
the tools generation, and can perform unnecessary rediscovery. This creates
persistent churn for quiet servers.

**Recommended remediation:** make stream idle timeout configurable and use a
more conservative default. Document the heartbeat expectation if a finite idle
timeout remains mandatory.

**Recommended regression test:** use a short injected test timeout to verify
both idle disconnection and timeout reset after a heartbeat.

### 5. Low: Bearer-Token Validation and Header Construction Disagree

`GatewayRoute::validate` at lines 101-105 rejects ASCII controls but accepts
non-ASCII UTF-8. `HeaderValue::from_str` at lines 581-587 rejects those values.
A route containing such a token is accepted and persisted, but every proxied
request then returns `500 invalid route token`.

There is also no explicit token-length cap. Although the route-table and admin
body limits provide indirect bounds, route-level validation should state the
actual contract.

**Recommended remediation:** require visible ASCII and impose a reasonable
maximum token length during route validation.

### 6. Low: Remaining Test Gaps

The branch adds useful coverage for SSE passthrough, aggregate traffic above 4
MiB, POST SSE, declared and chunked oversized responses, upstream body failure,
and semaphore reclamation. The following behavior remains untested:

- A stalled upstream read reaching the configured read timeout
- A downstream disconnect canceling the upstream body promptly
- Negative response-header filtering, such as dropping `Set-Cookie`
- Stream quota saturation while transient POST/DELETE capacity remains usable
- Request-body timeout and permit reclamation
- URL credential redaction in logs and response bodies

## Prior Review Findings Resolved

The branch correctly resolves the original review findings in these areas:

1. SSE responses no longer use a cumulative 4 MiB lifetime cap.
2. Streaming is selected from upstream `Content-Type`, not request method.
3. POST and DELETE responses can stream SSE.
4. Non-SSE responses remain buffered under the body cap, preserving clean
   `502` responses for oversized or failed bodies.
5. Connect and time-to-headers operations are bounded.
6. Per-read timeout support prevents a truly stalled upstream from remaining
   silent forever, subject to Finding 4's timeout-policy concern.
7. Stream permits are owned by the response body and release on body drop.
8. Mid-stream failures now produce structured tracing events.
9. Documentation now correctly distinguishes the plain HTTP listener from its
   HTTPS-only upstream policy and recommends a TLS proxy off-loopback.

These corrections make the branch substantially safer than its predecessor,
but they do not resolve the new starvation and slow-request issues introduced
by the shared semaphore design.

## Verification Evidence

### Local Verification

Commands were run from `ic/` with the repository-required CPU limit:

```text
taskset -c 0-11 cargo test -j12 --lib gateway_control -- --test-threads=12
taskset -c 0-11 cargo fmt --all -- --check
taskset -c 0-11 cargo check -j12 --lib
git diff --check origin/aragents/kimil00p/v2.0.8.0...HEAD
```

Results:

- `gateway_control`: 13 passed, 0 failed
- Rust formatting: passed
- Default-feature library compile check: passed
- Diff whitespace check: passed

### GitLab Pipeline

Pipeline `1864` for commit `84cef0e3` reports successful core jobs, including:

- `ci-config`
- `rustfmt`
- `cargo-metadata`
- `rust-check-default`
- `rust-test-lib`
- `panic-policy`
- `secret-detection`
- `python-unit`
- `shell-harnesses`
- Syntax and registry validation jobs

Passing tests establish that the implemented happy paths work. They do not
invalidate the availability and secret-handling findings above because those
cases are not represented in the current suite.

## Pre-existing Issues Observed

These issues are not introduced by this branch and are not merge blockers for
this Part 3 slice:

- Any `to_bytes` body error is reported as `413 request body too large`, including
  malformed framing or a disconnected client.
- Concurrent route persistence operations share `.gateway-routes.tmp`; a rename
  race can produce a persistence error after in-memory state has changed.
- `/mcp` remains unauthenticated by design, making loopback binding and external
  network controls important deployment boundaries.

## Merge Recommendation

**Do not merge commit `84cef0e3` as currently written.**

Before merge:

1. Separate long-lived stream capacity from transient request capacity.
2. Add a downstream request-body timeout and reclaim capacity on timeout.
3. Sanitize upstream URLs in logs and client-facing errors.
4. Add focused regression tests for those three corrections.
5. Update this review's status and the Item 11 completion note only after the
   remediations are verified.

The fixed 30-second SSE idle timeout and bearer-token validation mismatch should
also be addressed or explicitly accepted as documented follow-up work before the
v2.0.8.0 release boundary.
