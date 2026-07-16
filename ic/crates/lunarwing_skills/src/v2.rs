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

// ── B-2: demotion & pruning thresholds ───────────────────────────────────
/// Default confidence below which an extracted skill is demoted (excluded from
/// auto-activation). Stricter than the patch threshold — a skill is only demoted
/// once patching has failed to recover it. Reversible via `undeprecate_skill`.
pub const DEFAULT_DEMOTE_CONFIDENCE: f64 = 0.3;
/// Minimum usage before the demotion floor applies (mirrors the patch min-usage
/// gate so a single early failure can't demote a fresh skill).
pub const DEFAULT_DEMOTE_MIN_USAGE: u64 = 5;
/// Prune (archive) candidate floor: confidence at or below this (0.0 = all
/// failures, no successes) over at least this many uses is considered dead and
/// staged for user-approved archival. Strictly worse than the demotion floor.
pub const DEFAULT_PRUNE_CONFIDENCE: f64 = 0.0;
/// Minimum usage before a skill can be staged for pruning. Higher than the
/// demotion min-usage so we accumulate strong evidence before proposing removal.
pub const DEFAULT_PRUNE_MIN_USAGE: u64 = 10;
/// Default quarantine after automatic demotion before a skill may be staged
/// for pruning without accumulating more usage.
pub const DEFAULT_PRUNE_QUARANTINE_DAYS: u64 = 30;
/// Prefix used by legacy automatic demotions written before typed provenance
/// was added. Retained so those documents can enter the quarantine lifecycle.
pub const AUTOMATIC_DEMOTION_REASON_PREFIX: &str = "auto-demoted:";

// ── B-3: cross-agent sharing publish gate ────────────────────────────────
/// Minimum confidence for a skill to be publishable to the registry. Higher
/// than the patch threshold: we only share proven skills, not untested ones.
pub const DEFAULT_PUBLISH_CONFIDENCE: f64 = 0.7;
/// Minimum usage before a skill is publishable. Ensures the skill has been
/// exercised enough that its confidence ratio is statistically meaningful.
pub const DEFAULT_PUBLISH_MIN_USAGE: u64 = 10;

/// The kind of a staged skill proposal (B-1 patch, B-2 prune, B-3 update).
///
/// Discriminates the unified proposals surface so one API/GUI panel can render
/// and act on all three propose-then-approve flows. Defaults to `Patch` so old
/// pending-proposal records (B-1) deserialize cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    #[default]
    Patch,
    Prune,
    Update,
}

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

impl V2SkillMetadata {
    /// Whether this skill may be demoted (B-2): extracted, not Installed, not
    /// authored (authored intent is respected — mirrors the confidence-factor
    /// exemption), with enough usage and confidence below the demotion floor.
    /// Never true for already-archived or already-deprecated skills.
    pub fn is_demote_candidate(&self, threshold: f64, min_usage: u64) -> bool {
        self.trust != SkillTrust::Installed
            && self.source == V2SkillSource::Extracted
            && self.archived_at.is_none()
            && self.deprecated_at.is_none()
            && self.metrics.is_patch_candidate(threshold, min_usage)
    }

    /// Whether this skill may be staged for pruning (B-2): extracted, not
    /// Installed, not authored, not already archived/deprecated, with usage at
    /// the prune floor and confidence at or below the prune floor (0.0 = dead).
    /// Stricter than `is_demote_candidate` — prune is the last resort.
    pub fn is_prune_candidate(&self, threshold: f64, min_usage: u64) -> bool {
        self.trust != SkillTrust::Installed
            && self.source == V2SkillSource::Extracted
            && self.archived_at.is_none()
            && self.deprecated_at.is_none()
            && self.pending_patch.is_none()
            && self.metrics.usage_count >= min_usage
            && self.metrics.confidence() <= threshold
    }

    /// Whether this skill is currently demoted by the automatic confidence
    /// floor. The reason-prefix fallback preserves the lifecycle for metadata
    /// written before `automatic_demotion` existed.
    pub fn is_automatically_demoted(&self) -> bool {
        self.deprecated_at.is_some()
            && (self.automatic_demotion
                || self
                    .deprecation_reason
                    .starts_with(AUTOMATIC_DEMOTION_REASON_PREFIX))
    }

    /// Whether an automatically demoted skill has completed its quarantine and
    /// may be staged for user-approved pruning. Demoted skills cannot accumulate
    /// more usage because activation excludes them, so elapsed quarantine time
    /// is the evidence gate for this branch of the lifecycle.
    pub fn is_quarantine_prune_candidate(
        &self,
        now: &DateTime<Utc>,
        quarantine: std::time::Duration,
    ) -> bool {
        if self.trust == SkillTrust::Installed
            || self.source != V2SkillSource::Extracted
            || self.archived_at.is_some()
            || self.pending_patch.is_some()
            || self.pending_prune.is_some()
            || !self.is_automatically_demoted()
        {
            return false;
        }

        let Some(deprecated_at) = self.deprecated_at else {
            return false;
        };
        now.signed_duration_since(deprecated_at)
            .to_std()
            .is_ok_and(|elapsed| elapsed >= quarantine)
    }

    /// Clear demotion state only when it was produced automatically. Returns
    /// whether the skill was reactivated.
    pub fn clear_automatic_demotion(&mut self) -> bool {
        if !self.is_automatically_demoted() {
            return false;
        }
        self.deprecated_at = None;
        self.deprecation_reason.clear();
        self.automatic_demotion = false;
        true
    }

    /// Whether this skill is eligible to be published to a registry (B-3):
    /// `Trusted` (never publish `Installed`/read-only external skills), with a
    /// proven confidence and usage track record, and no pending patch in flight
    /// (don't publish a skill mid-revision).
    pub fn is_publish_eligible(&self, confidence_threshold: f64, min_usage: u64) -> bool {
        self.trust == SkillTrust::Trusted
            && self.archived_at.is_none()
            && self.pending_patch.is_none()
            && self.metrics.usage_count >= min_usage
            && self.metrics.confidence() >= confidence_threshold
    }

    /// Whether this skill is archived (B-2 soft-delete). Archived skills are
    /// fully excluded from activation and considered retired.
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }

    /// Whether this skill is deprecated/demoted (B-2). Deprecated skills are
    /// excluded from activation but still explicitly invocable by name in a
    /// future revision; for now the demotion is a hard exclusion.
    pub fn is_deprecated(&self) -> bool {
        self.deprecated_at.is_some()
    }
}

/// A single applied skill patch, recorded for auditability (B-1).
///
/// Captures the metrics as they stood *before* this patch reset them, so the
/// full history (e.g. "v1 was 2/8 = 0.25, then patched") is never lost even
/// though live metrics are epoch-reset on each patch.
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
    /// Metrics snapshot immediately before this patch reset them (audit trail).
    #[serde(default)]
    pub metrics_before: SkillMetrics,
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

/// A proposed-but-not-yet-applied skill prune (B-2, propose-then-approve).
///
/// Staged by the skill-maintenance sweep when a skill is confidently dead
/// (`is_prune_candidate`). Applied only on explicit user approval, which
/// archives (soft-deletes) the skill — never a hard delete. Authored and
/// `Installed` skills are never staged for pruning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingSkillPrune {
    /// Why the prune was proposed (diagnosis of deadness).
    #[serde(default)]
    pub reason: String,
    /// The skill confidence at staging time (what tripped the prune floor).
    #[serde(default)]
    pub confidence_at_staging: f64,
    /// The skill usage count at staging time.
    #[serde(default)]
    pub usage_count_at_staging: u64,
    /// The thread whose outcome most recently contributed, if applicable.
    #[serde(default)]
    pub source_thread_id: Option<String>,
    /// When the prune was staged.
    pub staged_at: DateTime<Utc>,
}

/// A proposed-but-not-yet-applied registry update (B-3, propose-then-approve).
///
/// Staged when a newer registry version is detected for an installed
/// registry-sourced skill. Applied only on explicit user approval (the pull +
/// re-validation never happen silently).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingSkillUpdate {
    /// The newer version available on the registry.
    pub registry_version_available: String,
    /// Exact registry slug to download on approval.
    #[serde(default)]
    pub registry_slug: String,
    /// Registry URL the update would be pulled from.
    #[serde(default)]
    pub registry_url: String,
    /// Content hash of the newer version, if known (change detection).
    #[serde(default)]
    pub new_content_hash: String,
    /// When the update was staged.
    pub staged_at: DateTime<Utc>,
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
    /// B-2 demotion flag: when set, the skill is excluded from auto-activation
    /// (and explicit `/skill` activation) until cleared. Reversible via
    /// `undeprecate_skill`. NOT a trust tier — `SkillTrust` is unchanged.
    #[serde(default)]
    pub deprecated_at: Option<DateTime<Utc>>,
    /// Human-readable reason for the current demotion (empty when not demoted).
    #[serde(default)]
    pub deprecation_reason: String,
    /// True only when the current demotion was applied automatically by the
    /// confidence floor. Manual/operator demotions remain false.
    #[serde(default)]
    pub automatic_demotion: bool,
    /// B-2 archive (soft-delete) flag: when set, the skill is fully excluded
    /// from activation and is considered retired. Set only by user-approved
    /// prune. The MemoryDoc is retained for audit/recovery (never hard-deleted).
    #[serde(default)]
    pub archived_at: Option<DateTime<Utc>>,
    /// Human-readable reason for archival (empty when not archived).
    #[serde(default)]
    pub archived_reason: String,
    /// A staged prune awaiting user approval, if any. B-2 propose-then-approve.
    #[serde(default)]
    pub pending_prune: Option<PendingSkillPrune>,
    // ── B-3: cross-agent sharing provenance ───────────────────────────────
    /// Registry URL the skill was pulled from (None for locally-authored).
    #[serde(default)]
    pub registry_url: Option<String>,
    /// Publisher handle at the registry, if known.
    #[serde(default)]
    pub registry_publisher: Option<String>,
    /// Exact registry slug used for publish, pull, and update checks.
    #[serde(default)]
    pub registry_slug: Option<String>,
    /// Registry version string installed, if pulled from a registry.
    #[serde(default)]
    pub registry_version: Option<String>,
    /// When the skill was pulled from the registry, if applicable.
    #[serde(default)]
    pub pulled_at: Option<DateTime<Utc>>,
    /// When this local skill was most recently published to the registry.
    #[serde(default)]
    pub published_at: Option<DateTime<Utc>>,
    /// Content hash recorded at pull time (for update change detection).
    #[serde(default)]
    pub registry_content_hash: Option<String>,
    /// A staged registry update awaiting user approval, if any.
    /// B-3 propose-then-approve.
    #[serde(default)]
    pub pending_update: Option<PendingSkillUpdate>,
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
                metrics_before: SkillMetrics {
                    usage_count: 8,
                    success_count: 2,
                    failure_count: 6,
                    last_used: None,
                },
            }],
            pending_patch: None,
            // B-2 demotion/prune fields
            deprecated_at: None,
            deprecation_reason: String::new(),
            automatic_demotion: false,
            archived_at: None,
            archived_reason: String::new(),
            pending_prune: None,
            // B-3 provenance fields
            registry_url: Some("https://example.registry".to_string()),
            registry_publisher: Some("alice".to_string()),
            registry_slug: Some("alice/test-skill".to_string()),
            registry_version: Some("1.2.3".to_string()),
            pulled_at: Some(Utc::now()),
            published_at: None,
            registry_content_hash: Some("sha256:dead".to_string()),
            pending_update: None,
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
        // Pre-patch metrics snapshot survives serialization (audit trail).
        assert_eq!(parsed.patch_history[0].metrics_before.usage_count, 8);
        assert_eq!(parsed.patch_history[0].metrics_before.failure_count, 6);
        // B-3 provenance survives serialization.
        assert_eq!(
            parsed.registry_url.as_deref(),
            Some("https://example.registry")
        );
        assert_eq!(parsed.registry_version.as_deref(), Some("1.2.3"));
        assert_eq!(parsed.registry_publisher.as_deref(), Some("alice"));
        assert_eq!(parsed.registry_slug.as_deref(), Some("alice/test-skill"));
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
        // B-2/B-3 fields default empty/None on old skills (forward compat).
        assert!(parsed.deprecated_at.is_none());
        assert!(parsed.deprecation_reason.is_empty());
        assert!(!parsed.automatic_demotion);
        assert!(parsed.archived_at.is_none());
        assert!(parsed.archived_reason.is_empty());
        assert!(parsed.pending_prune.is_none());
        assert!(parsed.registry_url.is_none());
        assert!(parsed.registry_slug.is_none());
        assert!(parsed.registry_version.is_none());
        assert!(parsed.published_at.is_none());
        assert!(parsed.pending_update.is_none());
    }

    /// Helper: an extracted Trusted skill with the given metrics.
    fn extracted_trusted(skill_success: u64, skill_failure: u64, usage: u64) -> V2SkillMetadata {
        V2SkillMetadata {
            name: "s".to_string(),
            source: V2SkillSource::Extracted,
            trust: SkillTrust::Trusted,
            metrics: SkillMetrics {
                usage_count: usage,
                success_count: skill_success,
                failure_count: skill_failure,
                last_used: None,
            },
            ..serde_json::from_str::<V2SkillMetadata>("{}").unwrap()
        }
    }

    #[test]
    fn test_is_demote_candidate_below_floor_enough_usage() {
        // 1/9 = 0.1 confidence, 9 uses, extracted Trusted → demote candidate.
        let m = extracted_trusted(1, 8, 9);
        assert!(m.is_demote_candidate(DEFAULT_DEMOTE_CONFIDENCE, DEFAULT_DEMOTE_MIN_USAGE));
    }

    #[test]
    fn test_is_demote_candidate_healthy_excluded() {
        // 8/10 = 0.8 confidence → healthy, not a demote candidate.
        let m = extracted_trusted(8, 2, 10);
        assert!(!m.is_demote_candidate(DEFAULT_DEMOTE_CONFIDENCE, DEFAULT_DEMOTE_MIN_USAGE));
    }

    #[test]
    fn test_is_demote_candidate_too_few_uses_excluded() {
        // 0/2 = 0.0 confidence but only 2 uses → below min-usage, not a candidate.
        let m = extracted_trusted(0, 2, 2);
        assert!(!m.is_demote_candidate(DEFAULT_DEMOTE_CONFIDENCE, DEFAULT_DEMOTE_MIN_USAGE));
    }

    #[test]
    fn test_is_demote_candidate_installed_excluded() {
        // Installed (read-only) skills are never demote candidates.
        let mut m = extracted_trusted(0, 8, 8);
        m.trust = SkillTrust::Installed;
        assert!(!m.is_demote_candidate(DEFAULT_DEMOTE_CONFIDENCE, DEFAULT_DEMOTE_MIN_USAGE));
    }

    #[test]
    fn test_is_demote_candidate_authored_excluded() {
        // Authored skills are exempt from demotion (respect user intent).
        let mut m = extracted_trusted(0, 8, 8);
        m.source = V2SkillSource::Authored;
        assert!(!m.is_demote_candidate(DEFAULT_DEMOTE_CONFIDENCE, DEFAULT_DEMOTE_MIN_USAGE));
    }

    #[test]
    fn test_is_demote_candidate_already_deprecated_or_archived_excluded() {
        // Once deprecated or archived, not a (re-)demote candidate.
        let mut m = extracted_trusted(0, 8, 8);
        m.deprecated_at = Some(Utc::now());
        assert!(!m.is_demote_candidate(DEFAULT_DEMOTE_CONFIDENCE, DEFAULT_DEMOTE_MIN_USAGE));
        let mut m2 = extracted_trusted(0, 8, 8);
        m2.archived_at = Some(Utc::now());
        assert!(!m2.is_demote_candidate(DEFAULT_DEMOTE_CONFIDENCE, DEFAULT_DEMOTE_MIN_USAGE));
    }

    #[test]
    fn test_is_prune_candidate_dead_skill() {
        // 0/10 = 0.0 confidence, 10 uses, extracted Trusted → prune candidate.
        let m = extracted_trusted(0, 10, 10);
        assert!(m.is_prune_candidate(DEFAULT_PRUNE_CONFIDENCE, DEFAULT_PRUNE_MIN_USAGE));
    }

    #[test]
    fn test_is_prune_candidate_strictly_worse_than_demote() {
        // 1/9 = 0.1 confidence: a demote candidate but NOT a prune candidate
        // (prune floor is 0.0 — only fully-dead skills stage for pruning).
        let m = extracted_trusted(1, 8, 9);
        assert!(m.is_demote_candidate(DEFAULT_DEMOTE_CONFIDENCE, DEFAULT_DEMOTE_MIN_USAGE));
        assert!(!m.is_prune_candidate(DEFAULT_PRUNE_CONFIDENCE, DEFAULT_PRUNE_MIN_USAGE));
    }

    #[test]
    fn test_is_prune_candidate_too_few_uses_excluded() {
        // 0/5 = 0.0 confidence but only 5 uses → below prune min-usage (10).
        let m = extracted_trusted(0, 5, 5);
        assert!(!m.is_prune_candidate(DEFAULT_PRUNE_CONFIDENCE, DEFAULT_PRUNE_MIN_USAGE));
    }

    #[test]
    fn test_auto_demotion_becomes_prune_candidate_after_quarantine() {
        let now = Utc::now();
        let quarantine = std::time::Duration::from_secs(30 * 24 * 60 * 60);
        let mut m = extracted_trusted(0, 5, 5);
        m.deprecated_at = Some(now - chrono::Duration::days(31));
        m.deprecation_reason = format!("{AUTOMATIC_DEMOTION_REASON_PREFIX} low confidence");
        m.automatic_demotion = true;

        assert!(m.is_quarantine_prune_candidate(&now, quarantine));
    }

    #[test]
    fn test_auto_demotion_is_not_prune_candidate_before_quarantine() {
        let now = Utc::now();
        let quarantine = std::time::Duration::from_secs(30 * 24 * 60 * 60);
        let mut m = extracted_trusted(0, 5, 5);
        m.deprecated_at = Some(now - chrono::Duration::days(29));
        m.deprecation_reason = format!("{AUTOMATIC_DEMOTION_REASON_PREFIX} low confidence");
        m.automatic_demotion = true;

        assert!(!m.is_quarantine_prune_candidate(&now, quarantine));
    }

    #[test]
    fn test_manual_demotion_never_enters_automatic_quarantine_pruning() {
        let now = Utc::now();
        let quarantine = std::time::Duration::from_secs(30 * 24 * 60 * 60);
        let mut m = extracted_trusted(0, 5, 5);
        m.deprecated_at = Some(now - chrono::Duration::days(90));
        m.deprecation_reason = "operator disabled this skill".to_string();

        assert!(!m.is_quarantine_prune_candidate(&now, quarantine));
    }

    #[test]
    fn test_pending_patch_blocks_quarantine_pruning() {
        let now = Utc::now();
        let quarantine = std::time::Duration::from_secs(30 * 24 * 60 * 60);
        let mut m = extracted_trusted(0, 5, 5);
        m.deprecated_at = Some(now - chrono::Duration::days(31));
        m.deprecation_reason = format!("{AUTOMATIC_DEMOTION_REASON_PREFIX} low confidence");
        m.automatic_demotion = true;
        m.pending_patch = Some(PendingSkillPatch {
            proposed_content: "patched".to_string(),
            diff: String::new(),
            reason: "recover".to_string(),
            source_thread_id: None,
            confidence_at_proposal: 0.0,
            base_content_hash: "sha256:old".to_string(),
            proposed_at: now,
        });

        assert!(!m.is_quarantine_prune_candidate(&now, quarantine));
    }

    #[test]
    fn test_legacy_auto_demotion_deserializes_into_quarantine_lifecycle() {
        let deprecated_at = Utc::now() - chrono::Duration::days(31);
        let json = serde_json::json!({
            "source": "extracted",
            "trust": "trusted",
            "deprecated_at": deprecated_at,
            "deprecation_reason": "auto-demoted: legacy metadata"
        });
        let parsed: V2SkillMetadata = serde_json::from_value(json).expect("deserialize legacy");

        assert!(!parsed.automatic_demotion);
        assert!(parsed.is_automatically_demoted());
        assert!(parsed.is_quarantine_prune_candidate(
            &Utc::now(),
            std::time::Duration::from_secs(30 * 24 * 60 * 60)
        ));
    }

    #[test]
    fn test_is_publish_eligible_proven_trusted() {
        // 9/1 = 0.9 confidence, 10 uses, Trusted → publishable.
        let m = extracted_trusted(9, 1, 10);
        assert!(m.is_publish_eligible(DEFAULT_PUBLISH_CONFIDENCE, DEFAULT_PUBLISH_MIN_USAGE));
    }

    #[test]
    fn test_is_publish_eligible_low_confidence_excluded() {
        // 5/5 = 0.5 confidence → below publish threshold (0.7).
        let m = extracted_trusted(5, 5, 10);
        assert!(!m.is_publish_eligible(DEFAULT_PUBLISH_CONFIDENCE, DEFAULT_PUBLISH_MIN_USAGE));
    }

    #[test]
    fn test_is_publish_eligible_too_few_uses_excluded() {
        // 9/1 = 0.9 confidence but only 5 uses → below publish min-usage (10).
        let m = extracted_trusted(9, 1, 5);
        assert!(!m.is_publish_eligible(DEFAULT_PUBLISH_CONFIDENCE, DEFAULT_PUBLISH_MIN_USAGE));
    }

    #[test]
    fn test_is_publish_eligible_installed_excluded() {
        // Installed skills are read-only external — never publishable.
        let mut m = extracted_trusted(9, 1, 10);
        m.trust = SkillTrust::Installed;
        assert!(!m.is_publish_eligible(DEFAULT_PUBLISH_CONFIDENCE, DEFAULT_PUBLISH_MIN_USAGE));
    }

    #[test]
    fn test_is_publish_eligible_pending_patch_excluded() {
        // A skill mid-revision (pending patch) is not publishable.
        let mut m = extracted_trusted(9, 1, 10);
        m.pending_patch = Some(PendingSkillPatch {
            proposed_content: "x".to_string(),
            diff: String::new(),
            reason: "r".to_string(),
            source_thread_id: None,
            confidence_at_proposal: 0.9,
            base_content_hash: "sha256:y".to_string(),
            proposed_at: Utc::now(),
        });
        assert!(!m.is_publish_eligible(DEFAULT_PUBLISH_CONFIDENCE, DEFAULT_PUBLISH_MIN_USAGE));
    }

    #[test]
    fn test_pending_prune_serde_roundtrip() {
        let meta = V2SkillMetadata {
            name: "s".to_string(),
            pending_prune: Some(PendingSkillPrune {
                reason: "0% confidence over 12 uses".to_string(),
                confidence_at_staging: 0.0,
                usage_count_at_staging: 12,
                source_thread_id: Some("t-3".to_string()),
                staged_at: Utc::now(),
            }),
            ..serde_json::from_str::<V2SkillMetadata>("{}").unwrap()
        };
        let json = serde_json::to_string(&meta).expect("serialize");
        let parsed: V2SkillMetadata = serde_json::from_str(&json).expect("deserialize");
        let p = parsed.pending_prune.expect("pending prune present");
        assert_eq!(p.reason, "0% confidence over 12 uses");
        assert_eq!(p.usage_count_at_staging, 12);
        assert!((p.confidence_at_staging).abs() < f64::EPSILON);
    }

    #[test]
    fn test_pending_update_serde_roundtrip() {
        let meta = V2SkillMetadata {
            name: "s".to_string(),
            pending_update: Some(PendingSkillUpdate {
                registry_version_available: "2.0.0".to_string(),
                registry_slug: "alice/s".to_string(),
                registry_url: "https://reg".to_string(),
                new_content_hash: "sha256:new".to_string(),
                staged_at: Utc::now(),
            }),
            ..serde_json::from_str::<V2SkillMetadata>("{}").unwrap()
        };
        let json = serde_json::to_string(&meta).expect("serialize");
        let parsed: V2SkillMetadata = serde_json::from_str(&json).expect("deserialize");
        let u = parsed.pending_update.expect("pending update present");
        assert_eq!(u.registry_version_available, "2.0.0");
        assert_eq!(u.registry_slug, "alice/s");
        assert_eq!(u.new_content_hash, "sha256:new");
    }

    #[test]
    fn test_proposal_kind_default_is_patch() {
        // Old pending-proposal records deserialize as Patch (back-compat).
        assert_eq!(ProposalKind::default(), ProposalKind::Patch);
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
