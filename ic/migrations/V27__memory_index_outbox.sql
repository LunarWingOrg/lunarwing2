-- WI#132 hardening item 2: memory index desync (outbox marker).
--
-- `memory_chunks` is the search index projection of `memory_documents`.
-- The append path commits content first and re-chunks in a separate
-- transaction (Workspace::append -> append_document -> reindex_document).
-- A crash between the two strands the document zero-chunked forever:
-- chunk-driven search/listings then under-report or hide content that
-- `ws.read` still serves in full (the Sep-2026 Kageho "0 vs 6671" symptom).
--
-- `indexed_at` is the outbox marker: NULL = content changed, chunks not
-- (yet) rebuilt; non-NULL = chunk projection current. It is set to NULL in
-- the SAME statement that commits content (append/content-only update) and
-- stamped NOW() inside the same transaction that replaces chunks. A repair
-- sweep (Workspace::repair_stale_index) re-chunks stranded documents.
--
-- Backfill: existing documents are considered indexed (indexed_at =
-- updated_at) so an upgrade does not trigger a mass re-chunk / re-embed;
-- only writes after this migration participate in the outbox protocol.
ALTER TABLE memory_documents ADD COLUMN indexed_at TIMESTAMPTZ;

UPDATE memory_documents SET indexed_at = updated_at WHERE indexed_at IS NULL;

CREATE INDEX idx_memory_documents_stale_index
    ON memory_documents (updated_at)
    WHERE indexed_at IS NULL;
