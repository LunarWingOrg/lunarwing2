-- Study-Notes Knowledge Layer (Phase 1: Core Knowledge Store).
--
-- Study notes are ordinary workspace documents under `study-notes/<agent>/<topic>.md`
-- carrying a `study-note` provenance tag in `metadata`. Full-text search runs on
-- the existing `memory_chunks.content_tsv` GIN index; this partial index covers
-- the metadata filters used by knowledge topic/status scans.

CREATE INDEX IF NOT EXISTS idx_memory_documents_study_notes
    ON memory_documents (user_id, ((metadata ->> 'topic')))
    WHERE metadata ->> 'provenance' = 'study-note';
