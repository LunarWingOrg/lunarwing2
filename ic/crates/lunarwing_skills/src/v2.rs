//! V2 engine skill types.
//!
//! These types extend the v1 skill model with capabilities needed by the v2
//! engine: executable code snippets, usage/confidence metrics, and versioning.
//! They are serialized into `MemoryDoc.metadata` JSON in the engine crate.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{ActivationCriteria, SkillTrust};

/// How a v2 skill was created.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V2SkillSource {
    /// User-authored SKILL.md (migrated from v1 or hand-written).
    #[default]
    Authored,
    /// Auto-extracted by the skill-extraction learning mission.
    Extracted,
    /// One-time v1 → v2 migration.
    Migrated,
}

/// A Python code snippet carried by a v2 skill.
///
/// Registered as a callable function in the CodeAct/Monty runtime so the LLM
/// can call it directly without reconstructing the logic from scratch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeSnippet {
    /// Function name (e.g., "fetch_issues"). Must be a valid Python identifier.
    pub name: String,
    /// Python function body (e.g., `def fetch_issues(owner, repo): ...`).
    pub code: String,
    /// Short description for the LLM context / docstring.
    #[serde(default)]
    pub description: String,
}

/// Usage and confidence metrics for auto-extracted skills.
///
/// Tracks how often a skill is used and whether it contributes to successful
/// thread outcomes. Skills with low confidence get demoted in scoring.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillMetrics {
    /// Total number of times this skill was activated in a thread.
    #[serde(default)]
    pub usage_count: u64,
    /// Number of times the skill was active in a successfully completed thread.
    #[serde(default)]
    pub success_count: u64,
    /// Number of times the skill was active in a failed thread.
    #[serde(default)]
    pub failure_count: u64,
    /// When this skill was last activated.
    #[serde(default)]
    pub last_used: Option<DateTime<Utc>>,
}

/// Default confidence below which a skill becomes a patch candidate (B-1).
pub const DEFAULT_PATCH_CONFIDENCE_THRESHOLD: f64 = 0.5;
/// Default minimum usage before confidence is trusted enough to trigger a patch
/// proposal — avoids reacting to a single early failure.
pub const DEFAULT_PATCH_MIN_USAGE: u64 = 5;
/// Cap on retained patch-history entries (bounded, newest kept).
pub const MAX_PATCH_HISTORY: usize = 20;

/// Compute the canonical content hash of a skill's body (B-1 optimistic
/// concurrency). Stored as `content_hash` and re-checked before applying a
/// pending patch so a patch cannot silently clobber an out-of-band edit.
pub fn compute_content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

impl SkillMetrics {
    /// Compute confidence as success ratio.
    ///
    /// Returns 1.0 if there are no recorded outcomes (benefit of the doubt).
    pub fn confidence(&self) -> f64 {
        let total = self.success_count + self.failure_count;
        if total == 0 {
            return 1.0;
        }
        self.success_count as f64 / total as f64
    }

    /// Whether this skill is a patch candidate (B-1): enough recorded usage AND
    /// confidence below the threshold. Requiring a minimum sample size prevents
    /// one early failure from tripping the trigger.
    pub fn is_patch_candidate(&self, threshold: f64, min_usage: u64) -> bool {
        self.usage_count >= min_usage && self.confidence() < threshold
    }
}

/// A single applied skill patch, recorded for auditability (B-1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillPatch {
    /// The version this patch produced.
    pub version: u32,
    /// When the patch was applied.
    pub applied_at: DateTime<Utc>,
    /// The thread whose failure motivated the patch (if any).
    #[serde(default)]
    pub source_thread_id: Option<String>,
    /// Human-readable reason / diagnosis.
    #[serde(default)]
    pub reason: String,
}

/// A proposed-but-not-yet-applied skill patch (B-1, propose-then-approve model).
///
/// Staged by the self-improvement mission; applied only on explicit user
/// approval. The live skill is untouched while a proposal is pending.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingSkillPatch {
    /// Proposed replacement content for the skill's prompt/body.
    pub proposed_content: String,
    /// Unified diff (current → proposed) for user review.
    #[serde(default)]
    pub diff: String,
    /// Why the patch is proposed (diagnosis of the failure).
    #[serde(default)]
    pub reason: String,
    /// The thread whose failure triggered the proposal.
    #[serde(default)]
    pub source_thread_id: Option<String>,
    /// The skill confidence at proposal time (what tripped the threshold).
    #[serde(default)]
    pub confidence_at_proposal: f64,
    /// Content hash of the skill when the proposal was made — used for
    /// optimistic concurrency (refuse to apply if the skill changed since).
    #[serde(default)]
    pub base_content_hash: String,
    /// When the proposal was staged.
    pub proposed_at: DateTime<Utc>,
}

/// Full metadata for a v2 skill.
///
/// Serialized to/from the `metadata` JSON field of a `MemoryDoc` with
/// `DocType::Skill`. All fields use `#[serde(default)]` for forward
/// compatibility — old skills missing new fields deserialize gracefully.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V2SkillMetadata {
    /// Skill name (matches the MemoryDoc title minus the "skill:" prefix).
    #[serde(default)]
    pub name: String,
    /// Skill version (incremented by extraction/update missions).
    #[serde(default = "default_version")]
    pub version: u32,
    /// Short description.
    #[serde(default)]
    pub description: String,
    /// Activation criteria for deterministic selection.
    #[serde(default)]
    pub activation: ActivationCriteria,
    /// How this skill was created.
    #[serde(default)]
    pub source: V2SkillSource,
    /// Trust level.
    #[serde(default = "default_trust")]
    pub trust: SkillTrust,
    /// Executable Python code snippets for CodeAct injection.
    #[serde(default)]
    pub code_snippets: Vec<CodeSnippet>,
    /// Usage and confidence metrics.
    #[serde(default)]
    pub metrics: SkillMetrics,
    /// Previous version number (for rollback).
    #[serde(default)]
    pub parent_version: Option<u32>,
    /// SHA-256 hash of the prompt content.
    #[serde(default)]
    pub content_hash: String,
    /// Bounded history of applied patches (newest last, capped at
    /// `MAX_PATCH_HISTORY`). B-1 auditability.
    #[serde(default)]
    pub patch_history: Vec<SkillPatch>,
    /// A staged patch awaiting user approval, if any. Only one at a time; the
    /// live skill is untouched while this is `Some`. B-1 propose-then-approve.
    #[serde(default)]
    pub pending_patch: Option<PendingSkillPatch>,
}

fn default_version() -> u32 {
    1
}

fn default_trust() -> SkillTrust {
    SkillTrust::Installed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_confidence_no_data() {
        let m = SkillMetrics::default();
        assert!((m.confidence() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_confidence_all_success() {
        let m = SkillMetrics {
            success_count: 10,
            failure_count: 0,
            ..Default::default()
        };
        assert!((m.confidence() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_confidence_mixed() {
        let m = SkillMetrics {
            success_count: 3,
            failure_count: 7,
            ..Default::default()
        };
        assert!((m.confidence() - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn test_confidence_all_failure() {
        let m = SkillMetrics {
            success_count: 0,
            failure_count: 5,
            ..Default::default()
        };
        assert!((m.confidence() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_v2_metadata_serde_roundtrip() {
        let meta = V2SkillMetadata {
            name: "test-skill".to_string(),
            version: 3,
            description: "A test".to_string(),
            activation: ActivationCriteria {
                keywords: vec!["test".to_string()],
                ..Default::default()
            },
            source: V2SkillSource::Extracted,
            trust: SkillTrust::Trusted,
            code_snippets: vec![CodeSnippet {
                name: "do_thing".to_string(),
                code: "def do_thing(): pass".to_string(),
                description: "Does a thing".to_string(),
            }],
            metrics: SkillMetrics {
                usage_count: 5,
                success_count: 4,
                failure_count: 1,
                last_used: None,
            },
            parent_version: Some(2),
            content_hash: "sha256:abc".to_string(),
            patch_history: vec![SkillPatch {
                version: 3,
                applied_at: Utc::now(),
                source_thread_id: Some("thread-1".to_string()),
                reason: "fixed bad command".to_string(),
            }],
            pending_patch: None,
        };

        let json = serde_json::to_string(&meta).expect("serialize");
        let parsed: V2SkillMetadata = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(parsed.name, "test-skill");
        assert_eq!(parsed.version, 3);
        assert_eq!(parsed.source, V2SkillSource::Extracted);
        assert_eq!(parsed.code_snippets.len(), 1);
        assert_eq!(parsed.metrics.success_count, 4);
        assert_eq!(parsed.parent_version, Some(2));
        assert_eq!(parsed.patch_history.len(), 1);
        assert_eq!(parsed.patch_history[0].version, 3);
    }

    #[test]
    fn test_v2_metadata_default_fields() {
        // Deserializing an empty JSON object should produce valid defaults
        let parsed: V2SkillMetadata = serde_json::from_str("{}").expect("deserialize empty");
        assert_eq!(parsed.name, "");
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.source, V2SkillSource::Authored);
        assert_eq!(parsed.trust, SkillTrust::Installed);
        assert!(parsed.code_snippets.is_empty());
        assert!((parsed.metrics.confidence() - 1.0).abs() < f64::EPSILON);
        // B-1 fields default empty on old skills (forward compat).
        assert!(parsed.patch_history.is_empty());
        assert!(parsed.pending_patch.is_none());
    }

    #[test]
    fn test_is_patch_candidate_requires_min_usage() {
        // Below the confidence threshold but too few samples → NOT a candidate.
        let m = SkillMetrics {
            usage_count: 2,
            success_count: 0,
            failure_count: 2,
            last_used: None,
        };
        assert!(!m.is_patch_candidate(DEFAULT_PATCH_CONFIDENCE_THRESHOLD, DEFAULT_PATCH_MIN_USAGE));
    }

    #[test]
    fn test_is_patch_candidate_low_confidence_enough_usage() {
        // 2/8 = 0.25 confidence, 8 uses → candidate.
        let m = SkillMetrics {
            usage_count: 8,
            success_count: 2,
            failure_count: 6,
            last_used: None,
        };
        assert!(m.is_patch_candidate(DEFAULT_PATCH_CONFIDENCE_THRESHOLD, DEFAULT_PATCH_MIN_USAGE));
    }

    #[test]
    fn test_is_patch_candidate_healthy_skill_excluded() {
        // 7/8 = 0.875 confidence → healthy, not a candidate.
        let m = SkillMetrics {
            usage_count: 8,
            success_count: 7,
            failure_count: 1,
            last_used: None,
        };
        assert!(!m.is_patch_candidate(DEFAULT_PATCH_CONFIDENCE_THRESHOLD, DEFAULT_PATCH_MIN_USAGE));
    }

    #[test]
    fn test_pending_patch_serde_roundtrip() {
        let meta = V2SkillMetadata {
            name: "s".to_string(),
            pending_patch: Some(PendingSkillPatch {
                proposed_content: "new body".to_string(),
                diff: "@@ -1 +1 @@".to_string(),
                reason: "wrong tool name".to_string(),
                source_thread_id: Some("t-9".to_string()),
                confidence_at_proposal: 0.25,
                base_content_hash: "sha256:old".to_string(),
                proposed_at: Utc::now(),
            }),
            ..serde_json::from_str::<V2SkillMetadata>("{}").unwrap()
        };
        let json = serde_json::to_string(&meta).expect("serialize");
        let parsed: V2SkillMetadata = serde_json::from_str(&json).expect("deserialize");
        let p = parsed.pending_patch.expect("pending present");
        assert_eq!(p.proposed_content, "new body");
        assert_eq!(p.base_content_hash, "sha256:old");
        assert!((p.confidence_at_proposal - 0.25).abs() < f64::EPSILON);
    }
}
