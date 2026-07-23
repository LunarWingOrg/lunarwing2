-- Engine runtime state must remain persisted for restart recovery, but it is
-- operational data rather than semantic memory. Remove legacy search chunks
-- without deleting the source documents.
DELETE FROM memory_chunks
WHERE document_id IN (
    SELECT id
    FROM memory_documents
    WHERE path = 'engine/.runtime'
       OR path LIKE 'engine/.runtime/%'
);
