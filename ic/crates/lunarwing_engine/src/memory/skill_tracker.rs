//! Skill confidence tracking.
//!
//! Tracks usage and success/failure metrics for auto-extracted skills.
//! After each thread completes, the active skills' metrics are updated
//! based on whether the thread succeeded or failed.

use std::sync::Arc;

use lunarwing_skills::SkillTrust;
use lunarwing_skills::v2::{
    DEFAULT_DEMOTE_CONFIDENCE, DEFAULT_DEMOTE_MIN_USAGE, MAX_PATCH_HISTORY, PendingSkillPatch,
    PendingSkillPrune, PendingSkillUpdate, SkillPatch, V2SkillMetadata, V2SkillSource,
    compute_content_hash,
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

        // B-2 inline demotion: after recording this outcome, if the skill has
        // fallen below the demotion floor (with enough usage) and isn't already
        // deprecated, mark it deprecated. This is the automatic, reversible
        // demotion — it excludes the skill from auto-activation going forward.
        // Pruning (archival) stays propose→approve (never auto-destructive).
        // Authored and Installed skills are exempt (is_demote_candidate checks).
        if crate::skill_self_improvement_enabled()
            && meta.deprecated_at.is_none()
            && meta.is_demote_candidate(DEFAULT_DEMOTE_CONFIDENCE, DEFAULT_DEMOTE_MIN_USAGE)
        {
            let conf = meta.metrics.confidence();
            meta.deprecated_at = Some(chrono::Utc::now());
            meta.deprecation_reason = format!(
                "auto-demoted: confidence {:.2} below demotion floor {:.2} over {} uses",
                conf, DEFAULT_DEMOTE_CONFIDENCE, meta.metrics.usage_count
            );
            tracing::info!(
                skill_doc = %doc_id.0,
                confidence = conf,
                usage = meta.metrics.usage_count,
                "skill auto-demoted below confidence floor"
            );
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

        let pending = meta
            .pending_patch
            .take()
            .ok_or_else(|| EngineError::Skill {
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
            // Preserve the pre-patch metrics before the epoch reset below.
            metrics_before: meta.metrics.clone(),
        });
        // Keep history bounded (newest kept).
        if meta.patch_history.len() > MAX_PATCH_HISTORY {
            let overflow = meta.patch_history.len() - MAX_PATCH_HISTORY;
            meta.patch_history.drain(0..overflow);
        }

        // Epoch reset: a patched skill earns a fair fresh evaluation window.
        // Without this, stale pre-patch failures would keep the cumulative
        // ratio below threshold forever, immediately re-tripping the patch
        // trigger and masking whether the patch actually helped. The old
        // counts live on in `metrics_before` above (full audit trail).
        meta.metrics.usage_count = 0;
        meta.metrics.success_count = 0;
        meta.metrics.failure_count = 0;
        meta.metrics.last_used = None;

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

    // ── B-2: demotion & pruning ───────────────────────────────────────────

    /// Demote (deprecate) a skill so it is excluded from auto-activation (B-2).
    ///
    /// Reversible via [`undeprecate_skill`]. Refuses authored and `Installed`
    /// skills (authored intent is respected; external skills are read-only).
    pub async fn demote_skill(&self, doc_id: DocId, reason: String) -> Result<(), EngineError> {
        let doc = self.load_skill_doc(doc_id).await?;
        let mut meta = self.meta_from_doc(doc_id, &doc)?;

        if meta.source == V2SkillSource::Authored || meta.trust == SkillTrust::Installed {
            return Err(EngineError::Skill {
                reason: format!(
                    "refusing to demote authored/Installed (protected) skill {}",
                    doc_id.0
                ),
            });
        }
        meta.deprecated_at = Some(chrono::Utc::now());
        meta.deprecation_reason = reason;
        self.save_skill_meta(doc_id, doc, meta).await
    }

    /// Clear a skill's demotion (B-2 recovery). Used after a B-1 patch epoch-
    /// reset earns the skill enough confidence back, or on manual user action.
    /// Leaves content, version, and metrics untouched.
    pub async fn undeprecate_skill(&self, doc_id: DocId) -> Result<(), EngineError> {
        let doc = self.load_skill_doc(doc_id).await?;
        let mut meta = self.meta_from_doc(doc_id, &doc)?;
        meta.deprecated_at = None;
        meta.deprecation_reason = String::new();
        self.save_skill_meta(doc_id, doc, meta).await
    }

    /// Stage a proposed prune (archival) for later user approval (B-2,
    /// propose-then-approve). Does NOT archive — it only records a
    /// `pending_prune`. Refuses authored/Installed skills and already-archived
    /// skills. Overwrites any prior pending prune.
    pub async fn propose_prune(
        &self,
        doc_id: DocId,
        reason: String,
        source_thread_id: Option<String>,
    ) -> Result<(), EngineError> {
        let doc = self.load_skill_doc(doc_id).await?;
        let mut meta = self.meta_from_doc(doc_id, &doc)?;

        if meta.source == V2SkillSource::Authored || meta.trust == SkillTrust::Installed {
            return Err(EngineError::Skill {
                reason: format!(
                    "refusing to propose prune for authored/Installed (protected) skill {}",
                    doc_id.0
                ),
            });
        }
        if meta.archived_at.is_some() {
            return Err(EngineError::Skill {
                reason: format!("skill {} is already archived", doc_id.0),
            });
        }
        meta.pending_prune = Some(PendingSkillPrune {
            reason,
            confidence_at_staging: meta.metrics.confidence(),
            usage_count_at_staging: meta.metrics.usage_count,
            source_thread_id,
            staged_at: chrono::Utc::now(),
        });
        self.save_skill_meta(doc_id, doc, meta).await
    }

    /// Apply a skill's pending prune on user approval (B-2): archives the skill
    /// (soft delete — sets `archived_at`, never removes the MemoryDoc) and
    /// clears the pending proposal. Returns an error if no prune is pending.
    pub async fn apply_prune(&self, doc_id: DocId, reason: String) -> Result<(), EngineError> {
        let doc = self.load_skill_doc(doc_id).await?;
        let mut meta = self.meta_from_doc(doc_id, &doc)?;
        if meta.pending_prune.take().is_none() {
            return Err(EngineError::Skill {
                reason: format!("skill {} has no pending prune to apply", doc_id.0),
            });
        }
        meta.archived_at = Some(chrono::Utc::now());
        meta.archived_reason = reason;
        self.save_skill_meta(doc_id, doc, meta).await
    }

    /// Discard a skill's pending prune on user rejection (B-2). Leaves the
    /// skill's archived state untouched (it was never archived).
    pub async fn discard_prune(&self, doc_id: DocId) -> Result<(), EngineError> {
        let doc = self.load_skill_doc(doc_id).await?;
        let mut meta = self.meta_from_doc(doc_id, &doc)?;
        if meta.pending_prune.take().is_none() {
            return Err(EngineError::Skill {
                reason: format!("skill {} has no pending prune to discard", doc_id.0),
            });
        }
        self.save_skill_meta(doc_id, doc, meta).await
    }

    // ── B-3: registry update proposals ────────────────────────────────────

    /// Stage a proposed registry update for later user approval (B-3,
    /// propose-then-approve). Does NOT pull or change the skill — it only
    /// records a `pending_update`. Overwrites any prior pending update.
    pub async fn propose_update(
        &self,
        doc_id: DocId,
        registry_version_available: String,
        registry_url: String,
        new_content_hash: String,
    ) -> Result<(), EngineError> {
        let doc = self.load_skill_doc(doc_id).await?;
        let mut meta = self.meta_from_doc(doc_id, &doc)?;
        meta.pending_update = Some(PendingSkillUpdate {
            registry_version_available,
            registry_url,
            new_content_hash,
            staged_at: chrono::Utc::now(),
        });
        self.save_skill_meta(doc_id, doc, meta).await
    }

    /// Apply a registry update on user approval (B-3): record the new registry
    /// version + content hash as provenance, clear the pending update. The
    /// actual pull + content swap is performed by the caller (the bridge
    /// re-installs the skill from the registry before calling this to stamp
    /// provenance). Bumps the skill version (a content change occurred).
    pub async fn apply_update(
        &self,
        doc_id: DocId,
        new_registry_version: String,
        new_content_hash: String,
    ) -> Result<(), EngineError> {
        let doc = self.load_skill_doc(doc_id).await?;
        let mut meta = self.meta_from_doc(doc_id, &doc)?;
        if meta.pending_update.take().is_none() {
            return Err(EngineError::Skill {
                reason: format!("skill {} has no pending update to apply", doc_id.0),
            });
        }
        meta.parent_version = Some(meta.version);
        meta.version += 1;
        meta.registry_version = Some(new_registry_version);
        meta.registry_content_hash = Some(new_content_hash);
        meta.pulled_at = Some(chrono::Utc::now());
        // Epoch-reset metrics: a freshly-updated skill earns a new evaluation
        // window (mirrors the B-1 patch epoch-reset rationale).
        meta.metrics.usage_count = 0;
        meta.metrics.success_count = 0;
        meta.metrics.failure_count = 0;
        meta.metrics.last_used = None;
        self.save_skill_meta(doc_id, doc, meta).await
    }

    /// Discard a pending registry update on user rejection (B-3). Leaves the
    /// skill's content/version untouched.
    pub async fn discard_update(&self, doc_id: DocId) -> Result<(), EngineError> {
        let doc = self.load_skill_doc(doc_id).await?;
        let mut meta = self.meta_from_doc(doc_id, &doc)?;
        if meta.pending_update.take().is_none() {
            return Err(EngineError::Skill {
                reason: format!("skill {} has no pending update to discard", doc_id.0),
            });
        }
        self.save_skill_meta(doc_id, doc, meta).await
    }

    // ── shared load/save helpers (private) ────────────────────────────────

    /// Load a MemoryDoc, asserting it is a skill doc.
    async fn load_skill_doc(&self, doc_id: DocId) -> Result<MemoryDoc, EngineError> {
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
        Ok(doc)
    }

    /// Parse a skill's V2SkillMetadata from its MemoryDoc.
    fn meta_from_doc(
        &self,
        doc_id: DocId,
        doc: &MemoryDoc,
    ) -> Result<V2SkillMetadata, EngineError> {
        serde_json::from_value(doc.metadata.clone()).map_err(|e| EngineError::Skill {
            reason: format!("invalid skill metadata for {}: {e}", doc_id.0),
        })
    }

    /// Serialize a skill's metadata back into a MemoryDoc and save it.
    async fn save_skill_meta(
        &self,
        _doc_id: DocId,
        doc: MemoryDoc,
        meta: V2SkillMetadata,
    ) -> Result<(), EngineError> {
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
            source: V2SkillSource::Extracted,
            trust: SkillTrust::Trusted,
            metrics: SkillMetrics {
                usage_count: 5,
                success_count: 3,
                failure_count: 2,
                last_used: None,
            },
            ..serde_json::from_str::<V2SkillMetadata>("{}").unwrap()
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
        // Epoch reset: live metrics zeroed so the patched skill gets a fresh
        // evaluation window (confidence back to 1.0, benefit of the doubt).
        assert_eq!(meta.metrics.usage_count, 0);
        assert_eq!(meta.metrics.success_count, 0);
        assert_eq!(meta.metrics.failure_count, 0);
        assert!(meta.metrics.last_used.is_none());
        assert!((meta.metrics.confidence() - 1.0).abs() < f64::EPSILON);
        // ...but the pre-patch metrics are preserved in the history snapshot.
        assert_eq!(meta.patch_history[0].metrics_before.usage_count, 5);
        assert_eq!(meta.patch_history[0].metrics_before.success_count, 3);
        assert_eq!(meta.patch_history[0].metrics_before.failure_count, 2);
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

    // ── B-2: demotion & pruning ───────────────────────────────────────────

    fn make_authored_skill_doc(project_id: ProjectId) -> MemoryDoc {
        let mut doc = make_skill_doc(project_id);
        let mut meta: V2SkillMetadata = serde_json::from_value(doc.metadata.clone()).unwrap();
        meta.source = V2SkillSource::Authored;
        doc.metadata = serde_json::to_value(&meta).unwrap();
        doc
    }

    #[tokio::test]
    async fn test_demote_skill_sets_deprecated_at() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc(project_id);
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        tracker
            .demote_skill(doc_id, "low confidence".to_string())
            .await
            .unwrap();

        let updated = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        let meta: V2SkillMetadata = serde_json::from_value(updated.metadata).unwrap();
        assert!(meta.deprecated_at.is_some());
        assert_eq!(meta.deprecation_reason, "low confidence");
    }

    #[tokio::test]
    async fn test_demote_skill_refused_for_authored_and_installed() {
        let project_id = ProjectId::new();

        // Authored → refused.
        let doc = make_authored_skill_doc(project_id);
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store);
        let r = tracker.demote_skill(doc_id, "r".into()).await;
        assert!(r.is_err(), "must refuse authored skill");

        // Installed → refused.
        let doc = make_skill_doc_trust(project_id, SkillTrust::Installed);
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store);
        let r = tracker.demote_skill(doc_id, "r".into()).await;
        assert!(r.is_err(), "must refuse Installed skill");
    }

    #[tokio::test]
    async fn test_undeprecate_skill_clears_demotion() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc(project_id);
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        tracker
            .demote_skill(doc_id, "low confidence".to_string())
            .await
            .unwrap();
        tracker.undeprecate_skill(doc_id).await.unwrap();

        let updated = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        let meta: V2SkillMetadata = serde_json::from_value(updated.metadata).unwrap();
        assert!(meta.deprecated_at.is_none());
        assert!(meta.deprecation_reason.is_empty());
    }

    #[tokio::test]
    async fn test_propose_prune_stages_without_archiving() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc(project_id);
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        tracker
            .propose_prune(
                doc_id,
                "0% over 10 uses".to_string(),
                Some("t-1".to_string()),
            )
            .await
            .unwrap();

        let updated = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        let meta: V2SkillMetadata = serde_json::from_value(updated.metadata).unwrap();
        // NOT archived while pending.
        assert!(meta.archived_at.is_none());
        let pending = meta.pending_prune.expect("pending prune staged");
        assert_eq!(pending.reason, "0% over 10 uses");
        assert_eq!(pending.source_thread_id.as_deref(), Some("t-1"));
        assert_eq!(pending.usage_count_at_staging, 5);
    }

    #[tokio::test]
    async fn test_propose_prune_refused_for_protected_skills() {
        let project_id = ProjectId::new();

        // Authored → refused.
        let doc = make_authored_skill_doc(project_id);
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store);
        assert!(
            tracker
                .propose_prune(doc_id, "r".into(), None)
                .await
                .is_err()
        );

        // Installed → refused.
        let doc = make_skill_doc_trust(project_id, SkillTrust::Installed);
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store);
        assert!(
            tracker
                .propose_prune(doc_id, "r".into(), None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_apply_prune_archives_and_clears_pending() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc(project_id);
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        tracker
            .propose_prune(doc_id, "dead".to_string(), None)
            .await
            .unwrap();
        tracker
            .apply_prune(doc_id, "user-approved archival".to_string())
            .await
            .unwrap();

        let updated = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        let meta: V2SkillMetadata = serde_json::from_value(updated.metadata).unwrap();
        assert!(meta.archived_at.is_some());
        assert_eq!(meta.archived_reason, "user-approved archival");
        assert!(meta.pending_prune.is_none(), "pending cleared on apply");
    }

    #[tokio::test]
    async fn test_discard_prune_clears_pending_without_archiving() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc(project_id);
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        tracker
            .propose_prune(doc_id, "dead".to_string(), None)
            .await
            .unwrap();
        tracker.discard_prune(doc_id).await.unwrap();

        let updated = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        let meta: V2SkillMetadata = serde_json::from_value(updated.metadata).unwrap();
        assert!(meta.archived_at.is_none(), "never archived on discard");
        assert!(meta.pending_prune.is_none());
    }

    #[tokio::test]
    async fn test_apply_prune_without_pending_fails() {
        let project_id = ProjectId::new();
        let doc = make_skill_doc(project_id);
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store);

        let r = tracker.apply_prune(doc_id, "r".into()).await;
        assert!(r.is_err(), "must refuse to apply with no pending prune");
    }

    #[tokio::test]
    async fn test_record_usage_inline_demotes_below_floor() {
        unsafe {
            std::env::set_var("SKILL_SELF_IMPROVEMENT", "true");
        }
        // Start with a Trusted Extracted skill at 1 success / 8 failures over 9
        // uses (confidence ~0.11, below the 0.3 demotion floor). record_usage
        // with a further failure should trip inline demotion.
        let project_id = ProjectId::new();
        let mut doc = make_skill_doc(project_id);
        let mut meta: V2SkillMetadata = serde_json::from_value(doc.metadata.clone()).unwrap();
        meta.metrics = SkillMetrics {
            usage_count: 9,
            success_count: 1,
            failure_count: 8,
            last_used: None,
        };
        doc.metadata = serde_json::to_value(&meta).unwrap();
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        tracker.record_usage(doc_id, false).await.unwrap();

        let updated = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        let meta: V2SkillMetadata = serde_json::from_value(updated.metadata).unwrap();
        assert!(
            meta.deprecated_at.is_some(),
            "should be auto-demoted below floor"
        );
        assert!(!meta.deprecation_reason.is_empty());
    }

    #[tokio::test]
    async fn test_record_usage_no_demote_above_floor() {
        // Healthy skill (3/2 = 0.6 confidence over 5 uses, above the 0.3 floor).
        // record_usage with either outcome must NOT demote.
        let project_id = ProjectId::new();
        let doc = make_skill_doc(project_id); // 3/2 = 0.6
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        tracker.record_usage(doc_id, false).await.unwrap();

        let updated = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        let meta: V2SkillMetadata = serde_json::from_value(updated.metadata).unwrap();
        assert!(
            meta.deprecated_at.is_none(),
            "healthy skill must not be demoted"
        );
    }

    #[tokio::test]
    async fn test_record_usage_no_demote_for_authored() {
        // Authored skill below the floor — must NOT be auto-demoted (exempt).
        let project_id = ProjectId::new();
        let doc = make_authored_skill_doc(project_id);
        let mut meta: V2SkillMetadata = serde_json::from_value(doc.metadata.clone()).unwrap();
        meta.metrics = SkillMetrics {
            usage_count: 9,
            success_count: 0,
            failure_count: 9,
            last_used: None,
        };
        let mut doc = doc;
        doc.metadata = serde_json::to_value(&meta).unwrap();
        let doc_id = doc.id;
        let store = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
        let tracker = SkillTracker::new(store.clone());

        tracker.record_usage(doc_id, false).await.unwrap();

        let updated = store.load_memory_doc(doc_id).await.unwrap().unwrap();
        let meta: V2SkillMetadata = serde_json::from_value(updated.metadata).unwrap();
        assert!(
            meta.deprecated_at.is_none(),
            "authored skills are exempt from demotion"
        );
    }
}
