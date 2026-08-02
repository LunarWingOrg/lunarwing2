#P1 FINDINGS

1. Coalescer can OOM and hang shutdown. Commits f57b4a0, c995575, f25c42a. Detached flush tasks have no timeout/backpressure and capture cloned IncomingMessages (
ic/src/channels/coalesce.rs:225-231
, 258-276, 305-313; 
ic/src/agent/agent_loop.rs:1274-1292
). XMPP messages may contain ten 20 MiB attachments. Stalled outbound queue permits unlimited tasks retaining roughly 200 MiB each; flush_all() then waits forever. Use bounded delivery queue/semaphore, timeout sends, and capture only routing metadata.
2. Violation flood causes unbounded O(N²) synchronous persistence. Commits 1384b95, fec6865. Every violation holds global std::sync::Mutex, clones and pretty-serializes entire map, then synchronously writes/renames it (
ic/src/gate/violation_store.rs:168-183
, 250-263, 309-333). Sender records lack global bounds/pruning. Flood grows RAM/disk and blocks Tokio workers. Cap actors/records and use debounced async persistence outside lock.
3. WASM response cap applies after full buffering. Newly exposed by GitLab commit 2870e35. response.bytes().await buffers entire chunked/compressed response before checking 8 MiB limit (
ic/src/tools/wasm/wrapper.rs:622-645
). Concurrent oversized responses can OOM daemon. Stream chunks and abort once cumulative size exceeds cap.
4. Default lorebook path reloads full corpus every turn. Commits caa13d4, 7636d46. Each direct turn performs listing plus up to 16 sequential reads, reparses up to 2 MiB JSON, and rescans 1,024 entries (
ic/src/agent/dispatcher.rs:102-120 , 
ic/src/lorebook/mod.rs:33-113 ). Lorebooks default enabled (
ic/src/config/agent.rs:101-109
). Cache by path/version and fetch bounded content in one indexed query.
5. Lorebook scan-depth cache can allocate 25.6 MiB per turn. Commit 88ed8a3. Cache keys use requested depth rather than effective min(depth, messages.len()) (
ic/src/lorebook/engine.rs:269-294
). One 128 KiB message plus entries using depths 1 through 100 creates 100 identical raw and lowercase windows. Canonicalize depth and cap aggregate cache bytes.
6. Concise valid answers trigger up to three LLM calls. Commit 3bba05c. Any non-simple task answer below 80 characters fails looks_like_final_answer() and receives two retries (
ic/crates/lunarwing_engine/orchestrator/default.py:207-222
, 662-676). This triples latency, token cost, and provider load for common concise answers. Remove length heuristic; retry only explicit tool-intent or leaked-roleplay patterns.
7. Unbounded DNS lookup blocks whole XMPP event loop. Commit 8b01a53. Attachment lookup_host().await runs before reqwest timeout and inline within stanza processing (
ic/src/channels/xmpp/mod.rs:922-934
, 1552, 3043-3059). Slow resolver can stop messages, presence, IQ handling, and outbound draining indefinitely. Add DNS/whole-extraction deadlines and move downloads to bounded workers.
8. Gate expiry holds global gate mutex across DB I/O. Commit f25c42a. run_if_absent() keeps store-wide mutex while finalizing thread, revoking leases, persisting events, and scanning every conversation (
ic/src/gate/store.rs:313-324
, 
ic/src/bridge/router.rs:927-969
, 
ic/crates/lunarwing_engine/src/runtime/conversation.rs:275-284
). Expiration burst becomes O(gates × conversations) and blocks all gate operations. Use keyed reservation/CAS and direct conversation lookup.