## Things that concern me

**1. Global `ingest_lock` serializes ALL KNOW writes across the daemon.** Under coordination traffic this is fine (2 msgs/hour/agent default), but if a room gets busy, every share queues behind one mutex for write + metadata. The rate limit is a separate lock, so admission control isn't blocked — but actual writes are.

**2. Global rate limit is approximate, not a hard guarantee.** The `try_consume_global_limit` check happens in the hook before spawning the detached task. Two concurrent hook executions can both pass the check, both spawn, both write. Best-effort flood protection — fine for the use case, but not a contract.

**3. Stranded writes are only self-healing if the sender shares again.** If a sender leaves the room after a partial write, the incomplete path sits forever. It's harmless (queries won't return a note without provenance metadata), but it's operator-visible cruft. A periodic sweep could clean these.

**4. Prompt injection via knowledge tools is mitigated, not eliminated.** The `untrusted` flag and `wrap_external_content` help, but the note content still enters model context. Effectiveness depends on how downstream models respect those metadata flags. Inherent limitation of any shared knowledge store.

**5. The `incomplete_wire_write` heuristic** (`no KnowledgeNoteMeta` + `content.starts_with("# KNOW: ")`) — safe in practice because native notes have metadata and different title format, but the content check is a string prefix match on rendered markdown. Unlikely to false-positive, but it's a heuristic, not a hard marker.

**6. Routine sweeps will encounter these notes.** The `knowledge` tool's `scan` includes coordination notes (capped). If a routine explicitly processes `needs-verification` notes, it'll be processing unverified claims from other tenants. The status filter should protect most workflows, but it's worth knowing.

---