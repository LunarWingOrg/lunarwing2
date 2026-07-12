//! Skill confidence tracking.
//!
//! Tracks usage and success/failure metrics for auto-extracted skills.
//! After each thread completes, the active skills' metrics are updated
//! based on whether the thread succeeded or failed.

use std::sync::Arc;

use lunarwing_skills::SkillTrust;
use lunarwing_skills::v2::{
    MAX_PATCH_HISTORY, PendingSkillPatch, SkillPatch, V2SkillMetadata, compute_content_hash,
};

use crate::traits::store::Store;
use crate::types::error::EngineError;
use crate::types::memory::{DocId, DocType, MemoryDoc};

/// Tracks skill usage and updates confidence metrics.
pub struct SkillTracker {
    store: Arc<dyn Store>,
}

impl SkillTracker {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    /// Record that a skill was used in a completed thread.
    ///
    /// Loads the skill's MemoryDoc, updates metrics in the metadata JSON,
    /// and saves it back. If the doc is not found or has invalid metadata,
    /// the error is logged and the operation is skipped.
    pub async fn record_usage(&self, doc_id: DocId, success: bool) -> Result<(), EngineError> {
        let doc = self
            .store
            .load_memory_doc(doc_id)
            .await?
            .ok_or_else(|| EngineError::Skill {
                reason: format!("skill doc not found: {}", doc_id.0),
            })?;

        if doc.doc_type != DocType::Skill {
            return Err(EngineError::Skill {
                reason: format!("doc {} is not a skill (type: {:?})", doc_id.0, doc.doc_type),
            });
        }

        let mut meta: V2SkillMetadata =
            serde_json::from_value(doc.metadata.clone()).map_err(|e| EngineError::Skill {
                reason: format!("invalid skill metadata for {}: {e}", doc_id.0),
            })?;

        meta.metrics.usage_count += 1;
        if success {
            meta.metrics.success_count += 1;
        } else {
            meta.metrics.failure_count += 1;
        }
        meta.metrics.last_used = Some(chrono::Utc::now());

        let updated_doc = MemoryDoc {
            metadata: serde_json::to_value(&meta).map_err(|e| EngineError::Skill {
                reason: format!("failed to serialize skill metadata: {e}"),
            })?,
            updated_at: chrono::Utc::now(),
            ..doc
        };

        self.store.save_memory_doc(&updated_doc).await
    }

    /// Update a skill's content and increment its version.
    ///
    /// Sets `parent_version` to the current version before incrementing,
    /// enabling rollback if the update causes issues.
    pub async fn update_skill(
        &self,
        doc_id: DocId,
        new_content: String,
        updater: impl FnOnce(&mut V2SkillMetadata),
    ) -> Result<(), EngineError> {
        let doc = self
            .store
            .load_memory_doc(doc_id)
            .await?
            .ok_or_else(|| EngineError::Skill {
                reason: format!("skill doc not found: {}", doc_id.0),
            })?;

        let mut meta: V2SkillMetadata =
            serde_json::from_value(doc.metadata.clone()).map_err(|e| EngineError::Skill {
                reason: format!("invalid skill metadata: {e}"),
            })?;

        meta.parent_version = Some(meta.version);
        meta.version += 1;
        updater(&mut meta);

        let updated_doc = MemoryDoc {
            content: new_content,
            metadata: serde_json::to_value(&meta).map_err(|e| EngineError::Skill {
                reason: format!("failed to serialize skill metadata: {e}"),
            })?,
            updated_at: chrono::Utc::now(),
            ..doc
        };

        self.store.save_memory_doc(&updated_doc).await
    }

    /// Stage a proposed patch for later user approval (B-1, propose-then-approve).
    ///
    /// Does NOT change the skill's content or version — it only records a
    /// `pending_patch` on the metadata. The live skill keeps working until the
    /// user approves. Refuses to propose against `Installed` (external,
    /// read-only) skills, and overwrites any prior pending proposal.
    pub async fn propose_patch(
        &self,
        doc_id: DocId,
        proposed_content: String,
        diff: String,
        reason: String,
        source_thread_id: Option<String>,
    ) -> Result<(), EngineError> {
        let doc = self
            .store
            .load_memory_doc(doc_id)
            .await?
            .ok_or_else(|| EngineError::Skill {
                reason: format!("skill doc not found: {}", doc_id.0),
            })?;

        if doc.doc_type != DocType::Skill {
            return Err(EngineError::Skill {
                reason: format!("doc {} is not a skill (type: {:?})", doc_id.0, doc.doc_type),
            });
        }

        let mut meta: V2SkillMetadata =
            serde_json::from_value(doc.metadata.clone()).map_err(|e| EngineError::Skill {
                reason: format!("invalid skill metadata: {e}"),
            })?;

        // Never propose patches for externally-installed (read-only) skills.
        if meta.trust == SkillTrust::Installed {
            return Err(EngineError::Skill {
                reason: format!(
                    "refusing to propose a patch for Installed (read-only) skill {}",
                    doc_id.0
                ),
            });
        }

        meta.pending_patch = Some(PendingSkillPatch {
            proposed_content,
            diff,
            reason,
            source_thread_id,
            confidence_at_proposal: meta.metrics.confidence(),
            base_content_hash: compute_content_hash(&doc.content),
            proposed_at: chrono::Utc::now(),
        });

        let updated_doc = MemoryDoc {
            metadata: serde_json::to_value(&meta).map_err(|e| EngineError::Skill {
                reason: format!("failed to serialize skill metadata: {e}"),
            })?,
            updated_at: chrono::Utc::now(),
            ..doc
        };

        self.store.save_memory_doc(&updated_doc).await
    }

    /// Apply a skill's pending patch on user approval (B-1).
    ///
    /// Bumps the version (with `parent_version` for rollback), swaps in the
    /// proposed content, appends a bounded `patch_history` entry, and clears
    /// the pending proposal. Optimistic concurrency: refuses if the skill's
    /// current content no longer matches the hash captured at propose time.
    pub async fn apply_pending_patch(&self, doc_id: DocId) -> Result<(), EngineError> {
        let doc = self
            .store
            .load_memory_doc(doc_id)
            .await?
            .ok_or_else(|| EngineError::Skill {
                reason: format!("skill doc not found: {}", doc_id.0),
            })?;

        let mut meta: V2SkillMetadata =
            serde_json::from_value(doc.metadata.clone()).map_err(|e| EngineError::Skill {
                reason: format!("invalid skill metadata: {e}"),
            })?;

        let pending = meta.pending_patch.take().ok_or_else(|| EngineError::Skill {
            reason: format!("skill {} has no pending patch to apply", doc_id.0),
        })?;

        // Optimistic concurrency: the skill must not have changed since propose.
        let current_hash = compute_content_hash(&doc.content);
        if pending.base_content_hash != current_hash {
            return Err(EngineError::Skill {
                reason: format!(
                    "skill {} changed since the patch was proposed; discard and re-propose",
                    doc_id.0
                ),
            });
        }

        meta.parent_version = Some(meta.version);
        meta.version += 1;
        meta.content_hash = compute_content_hash(&pending.proposed_content);
        meta.patch_history.push(SkillPatch {
            version: meta.version,
            applied_at: chrono::Utc::now(),
            source_thread_id: pending.source_thread_id.clone(),
            reason: pending.reason.clone(),
        });
        // Keep history bounded (newest kept).
        if meta.patch_history.len() > MAX_PATCH_HISTORY {
            let overflow = meta.patch_history.len() - MAX_PATCH_HISTORY;
            meta.patch_history.drain(0..overflow);
        }

        let updated_doc = MemoryDoc {
            content: pending.proposed_content,
            metadata: serde_json::to_value(&meta).map_err(|e| EngineError::Skill {
                reason: format!("failed to serialize skill metadata: {e}"),
            })?,
            updated_at: chrono::Utc::now(),
            ..doc
        };

        self.store.save_memory_doc(&updated_doc).await
    }

    /// Discard a skill's pending patch on user rejection (B-1). Leaves the
    /// skill's content and version untouched.
    pub async fn discard_pending_patch(&self, doc_id: DocId) -> Result<(), EngineError> {
        let doc = self
            .store
            .load_memory_doc(doc_id)
            .await?
            .ok_or_else(|| EngineError::Skill {
                reason: format!("skill doc not found: {}", doc_id.0),
            })?;

        let mut meta: V2SkillMetadata =
            serde_json::from_value(doc.metadata.clone()).map_err(|e| EngineError::Skill {
                reason: format!("invalid skill metadata: {e}"),
            })?;

        if meta.pending_patch.take().is_none() {
            return Err(EngineError::Skill {
                reason: format!("skill {} has no pending patch to discard", doc_id.0),
            });
        }

        let updated_doc = MemoryDoc {
            metadata: serde_json::to_value(&meta).map_err(|e| EngineError::Skill {
                reason: format!("failed to serialize skill metadata: {e}"),
            })?,
            updated_at: chrono::Utc::now(),
            ..doc
        };

        self.store.save_memory_doc(&updated_doc).await
    }

    /// Rollback a skill to its previous version.
    ///
    /// Decrements the version to `parent_version` if available. This is a
    /// simple version decrement — the actual content rollback requires the
    /// caller to also restore the content from a backup.
    pub async fn rollback_skill(&self, doc_id: DocId) -> Result<(), EngineError> {
        let doc = self
            .store
            .load_memory_doc(doc_id)
            .await?
            .ok_or_else(|| EngineError::Skill {
                reason: format!("skill doc not found: {}", doc_id.0),
            })?;

        let mut meta: V2SkillMetadata =
            serde_json::from_value(doc.metadata.clone()).map_err(|e| EngineError::Skill {
                reason: format!("invalid skill metadata: {e}"),
            })?;

        let parent = meta.parent_version.ok_or_else(|| EngineError::Skill {
            reason: format!("skill {} has no parent version to rollback to", doc_id.0),
        })?;

        meta.version = parent;
        meta.parent_version = None;

        let updated_doc = MemoryDoc {
            metadata: serde_json::to_value(&meta).map_err(|e| EngineError::Skill {
                reason: format!("failed to serialize skill metadata: {e}"),
            })?,
            updated_at: chrono::Utc::now(),
            ..doc
        };

        self.store.save_memory_doc(&updated_doc).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::project::ProjectId;
    use lunarwing_skills::SkillTrust;
    use lunarwing_skills::v2::{SkillMetrics, V2SkillSource};

    fn make_skill_doc(project_id: ProjectId) -> MemoryDoc {
        let meta = V2SkillMetadata {
            name: "test-skill".to_string(),
            version: 1,
            description: "test".to_string(),
            activation: Default::default(),
            source: V2SkillSource::Extracted,
            trust: SkillTrust::Trusted,
            code_snippets: vec![],
            metrics: SkillMetrics {
                usage_count: 5,
                success_count: 3,
                failure_count: 2,
                last_used: None,
            },
            parent_version: None,
            content_hash: String::new(),
            patch_history: vec![],
            pending_patch: None,
        };

        let mut doc = MemoryDoc::new(
            project_id,
            "test-user",
            DocType::Skill,
            "skill:test",
            "Test skill prompt",
        );
        doc.metadata = serde_json::to_value(&meta).unwrap();
        doc
    }

    fn make_skill_doc_trust(project_id: ProjectId, trust: SkillTrust) -> MemoryDoc {
        let mut doc = make_skill_doc(project_id);
        let mut meta: V2SkillMetadata = serde_json::from_value(doc.metadata.clone()).unwrap();
        meta.trust = trust;
        doc.metadata = serde_json::to_value(&meta).unwrap();
        doc
    }

    #[tokio::test]
    async fn test_record_usage_success() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc(project_id);
        let doc_id = doc.id;

        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        tracker.record_usage(doc_id, true).await.unwrap();

        let updated = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        let meta: V2SkillMetadata = serde_json::from_value(updated.metadata).unwrap();
        assert_eq!(meta.metrics.usage_count, 6);
        assert_eq!(meta.metrics.success_count, 4);
        assert_eq!(meta.metrics.failure_count, 2);
        assert!(meta.metrics.last_used.is_some());
    }

    #[tokio::test]
    async fn test_record_usage_failure() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc(project_id);
        let doc_id = doc.id;

        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        tracker.record_usage(doc_id, false).await.unwrap();

        let updated = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        let meta: V2SkillMetadata = serde_json::from_value(updated.metadata).unwrap();
        assert_eq!(meta.metrics.usage_count, 6);
        assert_eq!(meta.metrics.success_count, 3);
        assert_eq!(meta.metrics.failure_count, 3);
    }

    #[tokio::test]
    async fn test_update_skill_increments_version() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc(project_id);
        let doc_id = doc.id;

        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        tracker
            .update_skill(doc_id, "Updated content".to_string(), |meta| {
                meta.description = "Updated description".to_string();
            })
            .await
            .unwrap();

        let updated = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        assert_eq!(updated.content, "Updated content");

        let meta: V2SkillMetadata = serde_json::from_value(updated.metadata).unwrap();
        assert_eq!(meta.version, 2);
        assert_eq!(meta.parent_version, Some(1));
        assert_eq!(meta.description, "Updated description");
    }

    #[tokio::test]
    async fn test_rollback_restores_parent_version() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc(project_id);
        let doc_id = doc.id;

        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        // First update to version 2
        tracker
            .update_skill(doc_id, "v2 content".to_string(), |_| {})
            .await
            .unwrap();

        // Now rollback
        tracker.rollback_skill(doc_id).await.unwrap();

        let rolled = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        let meta: V2SkillMetadata = serde_json::from_value(rolled.metadata).unwrap();
        assert_eq!(meta.version, 1);
        assert_eq!(meta.parent_version, None);
    }

    #[tokio::test]
    async fn test_rollback_without_parent_fails() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc(project_id);
        let doc_id = doc.id;

        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store);

        let result = tracker.rollback_skill(doc_id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_record_usage_missing_doc() {
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![]));
        let tracker = SkillTracker::new(store);

        let result = tracker.record_usage(DocId::new(), true).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_propose_patch_stages_without_version_bump() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc_trust(project_id, SkillTrust::Trusted);
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        tracker
            .propose_patch(
                doc_id,
                "patched body".to_string(),
                "@@ diff @@".to_string(),
                "wrong tool name".to_string(),
                Some("thread-42".to_string()),
            )
            .await
            .unwrap();

        let updated = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        // Content + version UNCHANGED while pending.
        assert_eq!(updated.content, "Test skill prompt");
        let meta: V2SkillMetadata = serde_json::from_value(updated.metadata).unwrap();
        assert_eq!(meta.version, 1);
        let pending = meta.pending_patch.expect("pending staged");
        assert_eq!(pending.proposed_content, "patched body");
        assert_eq!(pending.source_thread_id.as_deref(), Some("thread-42"));
    }

    #[tokio::test]
    async fn test_propose_patch_refused_for_installed_skill() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc_trust(project_id, SkillTrust::Installed);
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store);

        let result = tracker
            .propose_patch(
                doc_id,
                "x".to_string(),
                String::new(),
                "r".to_string(),
                None,
            )
            .await;
        assert!(result.is_err(), "must refuse Installed skill");
    }

    #[tokio::test]
    async fn test_apply_pending_patch_bumps_version_and_records_history() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc_trust(project_id, SkillTrust::Trusted);
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        tracker
            .propose_patch(
                doc_id,
                "patched body".to_string(),
                String::new(),
                "fix".to_string(),
                Some("t-1".to_string()),
            )
            .await
            .unwrap();
        tracker.apply_pending_patch(doc_id).await.unwrap();

        let updated = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        assert_eq!(updated.content, "patched body");
        let meta: V2SkillMetadata = serde_json::from_value(updated.metadata).unwrap();
        assert_eq!(meta.version, 2);
        assert_eq!(meta.parent_version, Some(1));
        assert!(meta.pending_patch.is_none());
        assert_eq!(meta.patch_history.len(), 1);
        assert_eq!(meta.patch_history[0].version, 2);
        assert_eq!(meta.patch_history[0].reason, "fix");
    }

    #[tokio::test]
    async fn test_apply_pending_patch_refuses_on_content_drift() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc_trust(project_id, SkillTrust::Trusted);
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        tracker
            .propose_patch(
                doc_id,
                "patched body".to_string(),
                String::new(),
                "fix".to_string(),
                None,
            )
            .await
            .unwrap();

        // Simulate an out-of-band content change after the proposal.
        let mut drifted = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        drifted.content = "changed underneath".to_string();
        store.save_memory_doc(&drifted).await.unwrap();

        let result = tracker.apply_pending_patch(doc_id).await;
        assert!(result.is_err(), "must refuse to apply over a drifted skill");
    }

    #[tokio::test]
    async fn test_discard_pending_patch_leaves_skill_untouched() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc_trust(project_id, SkillTrust::Trusted);
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        tracker
            .propose_patch(
                doc_id,
                "patched body".to_string(),
                String::new(),
                "fix".to_string(),
                None,
            )
            .await
            .unwrap();
        tracker.discard_pending_patch(doc_id).await.unwrap();

        let updated = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        assert_eq!(updated.content, "Test skill prompt");
        let meta: V2SkillMetadata = serde_json::from_value(updated.metadata).unwrap();
        assert_eq!(meta.version, 1);
        assert!(meta.pending_patch.is_none());
        assert!(meta.patch_history.is_empty());
    }
}
