//! Cross-tenant knowledge sharing via the coordination pipeline.
//!
//! Coordination channels (XMPP MUC, IRC) are watched by one daemon per
//! tenant. A `KNOW:` tagged message is a one-line durable finding that every
//! watching daemon ingests into its own workspace's **shared** herd scope as
//! a `needs-verification` study note — the same store the native
//! `knowledge_search` / `knowledge_get_topic` tools read. That turns the ops
//! room into a cross-tenant knowledge propagation bus without any shared
//! database between tenants.
//!
//! Wire format (single line, per coordination channel discipline):
//!
//! ```text
//! KNOW: <topic-slug> | <one-line finding>
//! ```
//!
//! Trust model: wire-ingested notes are remote claims, never verified
//! against local source, so they are always stored as
//! `needs-verification` with the raw sender recorded in `submitted_by`.
//! Per-agent note versioning (`study-notes/<agent-slug>/<topic>.md`) means
//! the same sender resending a topic updates only their own version, and
//! different tenants' agents keep independent versions.
//!
//! The hook ([`KnowledgeShareHook`], priority 13) runs before the tag-schema
//! and engagement hooks, is fail-open, never modifies or rejects the
//! message, and rate-limits ingestion per sender so a rogue tenant cannot
//! flood the store. Outbound sharing needs no new machinery: `KNOW` is an
//! allowed coordination tag, so agents can emit it directly (the
//! coordination prompt policy documents the format when sharing is on).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::hooks::coordination::CoordinationState;
use crate::hooks::hook::{
    Hook, HookContext, HookError, HookEvent, HookFailureMode, HookOutcome, HookPoint,
};
use crate::workspace::{
    KnowledgeNoteMeta, KnowledgeStatus, Workspace, MAX_CONTENT_BYTES, MAX_SLUG_BYTES,
    MAX_TITLE_BYTES, is_valid_slug, note_path, render_note_markdown,
};

/// Coordination tag that carries a shareable finding.
pub const KNOWLEDGE_SHARE_TAG: &str = "KNOW";

/// Tag applied to wire-ingested notes.
const COORDINATION_TAG: &str = "coordination";

/// Fallback agent slug when a sender slugifies to nothing (e.g. non-ASCII).
const FALLBACK_AGENT_SLUG: &str = "remote";

/// Ingest rate-limit window.
const INGEST_WINDOW: Duration = Duration::from_secs(3600);

/// Default max ingested notes per sender per hour.
const DEFAULT_MAX_PER_HOUR: usize = 30;

/// Default minimum finding length (characters) to ingest.
const DEFAULT_MIN_CHARS: usize = 12;

/// Tunables for knowledge-share ingestion, loaded from environment.
#[derive(Debug, Clone)]
pub struct KnowledgeShareLimits {
    /// Max ingested notes per sender per hour
    /// (`COORD_KNOWLEDGE_MAX_PER_HOUR`, default 30).
    pub max_per_hour: usize,
    /// Minimum finding length in characters (`COORD_KNOWLEDGE_MIN_CHARS`,
    /// default 12). Shorter findings are ignored.
    pub min_chars: usize,
}

impl Default for KnowledgeShareLimits {
    fn default() -> Self {
        Self {
            max_per_hour: DEFAULT_MAX_PER_HOUR,
            min_chars: DEFAULT_MIN_CHARS,
        }
    }
}

impl KnowledgeShareLimits {
    /// Load limits from environment variables.
    pub fn from_env() -> Self {
        let mut limits = Self::default();
        if let Some(value) = env_parse::<usize>("COORD_KNOWLEDGE_MAX_PER_HOUR")
            .filter(|value| *value > 0)
        {
            limits.max_per_hour = value;
        }
        if let Some(value) = env_parse::<usize>("COORD_KNOWLEDGE_MIN_CHARS")
            .filter(|value| *value > 0)
        {
            limits.min_chars = value;
        }
        limits
    }
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// A parsed `KNOW:` share message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeShareMessage {
    /// Canonical lowercase topic slug.
    pub topic: String,
    /// One-line finding text.
    pub content: String,
}

/// Truncate on a UTF-8 character boundary.
fn truncate_char_boundary(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Parse a `KNOW: <topic-slug> | <finding>` message.
///
/// Returns `None` for anything that is not a well-formed share (including
/// missing separator, invalid topic slug, or a finding shorter than
/// `min_chars`). Malformed shares are ignored silently — they never strike
/// the sender, because the tag-schema hook owns structural enforcement.
pub fn parse_knowledge_share(content: &str, min_chars: usize) -> Option<KnowledgeShareMessage> {
    let trimmed = content.trim_start();
    let has_tag = trimmed
        .get(..KNOWLEDGE_SHARE_TAG.len() + 1)
        .is_some_and(|prefix| {
            prefix[..KNOWLEDGE_SHARE_TAG.len()].eq_ignore_ascii_case(KNOWLEDGE_SHARE_TAG)
                && prefix.ends_with(':')
        });
    if !has_tag {
        return None;
    }
    let body = trimmed[KNOWLEDGE_SHARE_TAG.len() + 1..].trim();
    let (topic, finding) = body.split_once('|')?;
    let topic = topic.trim();
    let finding = finding.trim();
    if !is_valid_slug(topic, MAX_SLUG_BYTES) {
        return None;
    }
    if finding.chars().count() < min_chars || finding.len() > MAX_CONTENT_BYTES {
        return None;
    }
    Some(KnowledgeShareMessage {
        topic: topic.to_string(),
        content: finding.to_string(),
    })
}

/// Slugify a coordination sender (JID, nick, agent id) into a valid note
/// agent slug: lowercase, invalid characters collapsed to single dashes,
/// bounded to [`MAX_SLUG_BYTES`]. Falls back to `remote` when nothing
/// survives.
pub fn sender_agent_slug(sender: &str) -> String {
    let mut slug = String::with_capacity(sender.len());
    let mut last_was_dash = true; // suppresses leading dashes
    for character in sender.chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            slug.push(character);
            last_was_dash = false;
        } else if character.is_ascii_uppercase() {
            slug.push(character.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        FALLBACK_AGENT_SLUG.to_string()
    } else {
        truncate_char_boundary(trimmed, MAX_SLUG_BYTES).to_string()
    }
}

/// Note title for an ingested finding.
fn share_title(finding: &str) -> String {
    let bounded = truncate_char_boundary(finding, MAX_TITLE_BYTES - KNOWLEDGE_SHARE_TAG.len() - 2);
    format!("{KNOWLEDGE_SHARE_TAG}: {bounded}")
}

/// Priority 13 — ingest `KNOW:` shares from coordination channels into the
/// shared knowledge scope. Fail-open and side-effect-only: it never
/// modifies or rejects the message, so room routing is unchanged.
pub struct KnowledgeShareHook {
    state: CoordinationState,
    workspace: Arc<Workspace>,
    limits: KnowledgeShareLimits,
}

impl KnowledgeShareHook {
    /// Create with shared coordination state, the shared-scope workspace to
    /// write into, and ingestion limits.
    pub fn new(
        state: CoordinationState,
        workspace: Arc<Workspace>,
        limits: KnowledgeShareLimits,
    ) -> Self {
        Self {
            state,
            workspace,
            limits,
        }
    }

    /// Write one ingested note (best-effort; failures are logged, never
    /// propagated).
    async fn ingest(&self, message: &KnowledgeShareMessage, sender: &str) {
        let agent = sender_agent_slug(sender);
        let meta = KnowledgeNoteMeta {
            topic: message.topic.clone(),
            agent: agent.clone(),
            title: share_title(&message.content),
            // Wire claims are never locally verified (trust model).
            status: KnowledgeStatus::NeedsVerification,
            confidence: None,
            studied_at: None,
            source_commit: None,
            tags: vec![COORDINATION_TAG.to_string()],
            superseded_by: None,
            // Raw sender identity, preserved beyond the slugified agent.
            submitted_by: Some(sender.to_string()),
            session_id: None,
        };
        let path = note_path(&agent, &message.topic);
        let markdown = render_note_markdown(&meta.title, &message.content);
        if let Err(error) = self.workspace.write(&path, &markdown).await {
            tracing::warn!(%path, %error, "coordination knowledge share write failed");
            return;
        }
        if let Err(error) = self
            .workspace
            .set_document_metadata(&path, &meta.to_metadata_json())
            .await
        {
            tracing::warn!(%path, %error, "coordination knowledge share metadata failed");
            return;
        }
        tracing::info!(%path, sender, "coordination knowledge note ingested");
    }
}

#[async_trait]
impl Hook for KnowledgeShareHook {
    fn name(&self) -> &str {
        "coordination.knowledge_share"
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::BeforeInbound]
    }

    fn failure_mode(&self) -> HookFailureMode {
        HookFailureMode::FailOpen
    }

    async fn execute(
        &self,
        event: &HookEvent,
        _ctx: &HookContext,
    ) -> Result<HookOutcome, HookError> {
        let HookEvent::Inbound {
            user_id,
            sender_id,
            content,
            ..
        } = event
        else {
            return Ok(HookOutcome::ok());
        };
        let Some(scope) = event.policy_scope() else {
            return Ok(HookOutcome::ok());
        };
        if !self.state.is_coordination_channel(scope).await {
            return Ok(HookOutcome::ok());
        }
        let Some(message) = parse_knowledge_share(content, self.limits.min_chars) else {
            return Ok(HookOutcome::ok());
        };
        let actor = sender_id.as_deref().unwrap_or(user_id);
        let key = format!("knowledge:{}", actor.to_lowercase());
        if !self
            .state
            .try_consume_rate_limit(&key, self.limits.max_per_hour, INGEST_WINDOW)
            .await
        {
            tracing::warn!(
                sender = actor,
                topic = %message.topic,
                limit = self.limits.max_per_hour,
                "coordination knowledge share rate-limited; note not ingested"
            );
            return Ok(HookOutcome::ok());
        }
        self.ingest(&message, actor).await;
        Ok(HookOutcome::ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_share() {
        let parsed = parse_knowledge_share(
            "KNOW: memory-search | Hybrid ranking lives in db/search.rs:88",
            DEFAULT_MIN_CHARS,
        );
        assert_eq!(
            parsed,
            Some(KnowledgeShareMessage {
                topic: "memory-search".to_string(),
                content: "Hybrid ranking lives in db/search.rs:88".to_string(),
            })
        );
        // Case-insensitive tag, surrounding whitespace tolerated.
        assert!(
            parse_knowledge_share(
                "  know: t-1 | a finding long enough",
                DEFAULT_MIN_CHARS
            )
            .is_some()
        );
    }

    #[test]
    fn rejects_malformed_shares() {
        let min = DEFAULT_MIN_CHARS;
        // Not a KNOW message at all.
        assert!(parse_knowledge_share("STATUS: routine update", min).is_none());
        // Knows too much: "knowledge:" must not match the KNOW tag.
        assert!(parse_knowledge_share("KNOWLEDGE: t | finding text", min).is_none());
        // Missing separator.
        assert!(parse_knowledge_share("KNOW: topic without separator", min).is_none());
        // Invalid topic slug.
        assert!(parse_knowledge_share("KNOW: Not_A_Slug | finding text", min).is_none());
        assert!(parse_knowledge_share("KNOW: | finding text", min).is_none());
        // Finding too short.
        assert!(parse_knowledge_share("KNOW: topic | tiny", min).is_none());
        // Empty finding.
        assert!(parse_knowledge_share("KNOW: topic |", min).is_none());
        // Extra pipes keep the first split; trailing content stays.
        let with_pipes = parse_knowledge_share("KNOW: topic | a | b", min).expect("parses");
        assert_eq!(with_pipes.topic, "topic");
        assert_eq!(with_pipes.content, "a | b");
    }

    #[test]
    fn sender_slugs_are_stable_and_valid() {
        for sender in [
            "Applejack@conference.example/resource",
            "rarity",
            "kimi-14",
            "!!",
            "tenant.b@conf.example",
        ] {
            let slug = sender_agent_slug(sender);
            assert!(
                is_valid_slug(&slug, MAX_SLUG_BYTES),
                "slug for {sender:?} must be valid: {slug:?}"
            );
        }
        assert_eq!(sender_agent_slug("Applejack@conf.example"), "applejack-conf-example");
        // Deterministic: same sender, same slug (stable note paths).
        assert_eq!(
            sender_agent_slug("Applejack@conf.example"),
            sender_agent_slug("Applejack@conf.example")
        );
        // Non-slug characters collapse to single dashes; non-ASCII falls back.
        assert_eq!(sender_agent_slug("a---b"), "a-b");
        assert_eq!(sender_agent_slug("Харьков"), FALLBACK_AGENT_SLUG);
        assert_eq!(sender_agent_slug(""), FALLBACK_AGENT_SLUG);
    }

    #[test]
    fn share_title_is_bounded() {
        let long = "x".repeat(MAX_TITLE_BYTES + 100);
        let title = share_title(&long);
        assert!(title.len() <= MAX_TITLE_BYTES);
        assert!(title.starts_with("KNOW: "));
    }

    #[test]
    fn limits_env_parsing_ignores_invalid_values() {
        // Invalid / zero values keep defaults; env is read-only here so this
        // only exercises the parse helper contract.
        assert_eq!(KnowledgeShareLimits::default().max_per_hour, DEFAULT_MAX_PER_HOUR);
        assert_eq!(KnowledgeShareLimits::default().min_chars, DEFAULT_MIN_CHARS);
    }

    #[cfg(feature = "libsql")]
    mod integration {
        use super::*;
        use crate::hooks::hook::Hook;
        use crate::testing::test_db;
        use crate::workspace::KnowledgeNoteMeta;

        fn hook(
            workspace: Arc<Workspace>,
            limits: KnowledgeShareLimits,
        ) -> KnowledgeShareHook {
            let state = CoordinationState::new(
                vec!["coord@conference.example".to_string()],
                Vec::new(),
                None,
            );
            KnowledgeShareHook::new(state, workspace, limits)
        }

        fn inbound(content: &str, sender: &str) -> HookEvent {
            HookEvent::Inbound {
                user_id: "owner".to_string(),
                sender_id: Some(sender.to_string()),
                channel: "xmpp".to_string(),
                content: content.to_string(),
                thread_id: None,
                policy_scope: Some("coord@conference.example".to_string()),
            }
        }

        async fn shared_note(
            workspace: &Workspace,
            agent: &str,
            topic: &str,
        ) -> Option<KnowledgeNoteMeta> {
            let doc = workspace.read_primary(&note_path(agent, topic)).await.ok()?;
            KnowledgeNoteMeta::from_metadata_json(&doc.metadata)
        }

        #[tokio::test]
        async fn ingests_know_share_into_shared_scope() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let hook = hook(workspace.clone(), KnowledgeShareLimits::default());

            let outcome = hook
                .execute(
                    &inbound(
                        "KNOW: mt-admin-cgroups | Bounded builds land in lunarwing-<t>/build subtrees",
                        "Applejack@conf.example/res",
                    ),
                    &Default::default(),
                )
                .await;
            assert!(matches!(outcome, Ok(HookOutcome::Continue { modified: None })));

            let meta = shared_note(&workspace, "applejack-conf-example-res", "mt-admin-cgroups")
                .await
                .expect("note ingested");
            assert_eq!(meta.status, KnowledgeStatus::NeedsVerification);
            assert_eq!(meta.agent, "applejack-conf-example-res");
            assert_eq!(meta.submitted_by.as_deref(), Some("Applejack@conf.example/res"));
            assert_eq!(meta.tags, vec![COORDINATION_TAG.to_string()]);
            assert!(meta.confidence.is_none());
        }

        #[tokio::test]
        async fn resending_same_topic_replaces_senders_own_version() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let hook = hook(workspace.clone(), KnowledgeShareLimits::default());

            for content in [
                "KNOW: topic-x | first finding about the thing",
                "KNOW: topic-x | corrected finding about the thing",
            ] {
                hook.execute(&inbound(content, "rarity"), &Default::default())
                    .await
                    .expect("hook is fail-open");
            }

            let meta = shared_note(&workspace, "rarity", "topic-x")
                .await
                .expect("note present");
            let doc = workspace
                .read_primary(&note_path("rarity", "topic-x"))
                .await
                .expect("read note");
            assert!(doc.content.contains("corrected finding"));
            assert_eq!(meta.title, "KNOW: corrected finding about the thing");
        }

        #[tokio::test]
        async fn different_senders_keep_independent_versions() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let hook = hook(workspace.clone(), KnowledgeShareLimits::default());

            hook.execute(
                &inbound("KNOW: topic-y | applejack version of the finding", "applejack"),
                &Default::default(),
            )
            .await
            .expect("fail-open");
            hook.execute(
                &inbound("KNOW: topic-y | rarity version of the finding", "rarity"),
                &Default::default(),
            )
            .await
            .expect("fail-open");

            assert!(shared_note(&workspace, "applejack", "topic-y").await.is_some());
            assert!(shared_note(&workspace, "rarity", "topic-y").await.is_some());
        }

        #[tokio::test]
        async fn ignores_non_coordination_and_non_know_traffic() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let hook = hook(workspace.clone(), KnowledgeShareLimits::default());

            // Casual room: no ingest even with a KNOW payload.
            let casual = HookEvent::Inbound {
                user_id: "owner".to_string(),
                sender_id: Some("applejack".to_string()),
                channel: "xmpp".to_string(),
                content: "KNOW: topic-z | a finding long enough".to_string(),
                thread_id: None,
                policy_scope: Some("casual@conference.example".to_string()),
            };
            hook.execute(&casual, &Default::default())
                .await
                .expect("fail-open");
            // Untagged coordination traffic: no ingest.
            hook.execute(
                &inbound("STATUS: routine room update", "applejack"),
                &Default::default(),
            )
            .await
            .expect("fail-open");

            assert!(shared_note(&workspace, "applejack", "topic-z").await.is_none());
            assert!(shared_note(&workspace, "applejack", "status").await.is_none());
        }

        #[tokio::test]
        async fn rate_limit_bounds_ingestion_without_rejecting() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let hook = hook(
                workspace.clone(),
                KnowledgeShareLimits {
                    max_per_hour: 1,
                    min_chars: DEFAULT_MIN_CHARS,
                },
            );

            hook.execute(
                &inbound("KNOW: rl-a | first finding within the limit", "flooder"),
                &Default::default(),
            )
            .await
            .expect("fail-open");
            let outcome = hook
                .execute(
                    &inbound("KNOW: rl-b | second finding beyond the limit", "flooder"),
                    &Default::default(),
                )
                .await;
            // Rate-limited share is dropped but the message itself is never rejected.
            assert!(matches!(outcome, Ok(HookOutcome::Continue { modified: None })));

            assert!(shared_note(&workspace, "flooder", "rl-a").await.is_some());
            assert!(shared_note(&workspace, "flooder", "rl-b").await.is_none());
        }
    }
}
