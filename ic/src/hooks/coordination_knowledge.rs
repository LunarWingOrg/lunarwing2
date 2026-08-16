//! Cross-tenant knowledge sharing via the coordination pipeline.
//!
//! A `KNOW:` message in a configured coordination room carries one durable
//! finding to every tenant daemon watching that room. Each daemon writes the
//! finding into its own database's shared herd scope, where the native
//! `knowledge_search` and `knowledge_get_topic` tools can find it.
//!
//! Wire format:
//!
//! ```text
//! KNOW: <topic-slug> | <one-line finding>
//! ```
//!
//! Wire claims are untrusted. They are stored as `needs-verification`, without
//! confidence or a source commit, and retain the bounded raw sender identity.
//! Wire-origin agent slugs use a dedicated `coord-` namespace plus a stable
//! sender hash so they cannot overwrite locally authored agent versions or
//! collide merely because two sender IDs normalize to the same readable slug.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::error::WorkspaceError;
use crate::hooks::coordination::{
    CoordinationState, DEFAULT_MAX_MESSAGE_LENGTH, MAX_COORD_AGENT_ID_LEN,
};
use crate::hooks::hook::{
    Hook, HookContext, HookError, HookEvent, HookFailureMode, HookOutcome, HookPoint,
};
use crate::workspace::{
    KnowledgeNoteMeta, KnowledgeStatus, MAX_CONTENT_BYTES, MAX_SLUG_BYTES, MAX_TITLE_BYTES,
    Workspace, is_valid_slug, note_path, render_note_markdown,
};

/// Coordination tag carrying a shareable finding.
pub const KNOWLEDGE_SHARE_TAG: &str = "KNOW";

/// Tag attached to wire-ingested study notes.
const COORDINATION_TAG: &str = "coordination";
/// Namespace preventing wire notes from overwriting native agent versions.
const WIRE_AGENT_PREFIX: &str = "coord";
/// Readable fallback for sender IDs with no slug-compatible characters.
const FALLBACK_AGENT_SLUG: &str = "remote";
/// Hex characters retained from the sender digest (96 bits).
const SENDER_HASH_HEX_LEN: usize = 24;
/// Ingestion rate-limit window.
const INGEST_WINDOW: Duration = Duration::from_secs(3600);
/// Default maximum ingested notes per sender per hour.
const DEFAULT_MAX_PER_HOUR: usize = 30;
/// Hard cap preventing an operator typo from creating an unbounded timestamp vector.
const MAX_CONFIGURED_PER_HOUR: usize = 1_000;
/// Default minimum finding length in characters.
const DEFAULT_MIN_CHARS: usize = 12;

/// Tunables for knowledge-share ingestion, loaded from environment.
#[derive(Debug, Clone)]
pub struct KnowledgeShareLimits {
    /// Max ingested notes per sender per hour
    /// (`COORD_KNOWLEDGE_MAX_PER_HOUR`, default 30, maximum 1000).
    pub max_per_hour: usize,
    /// Minimum finding length in characters (`COORD_KNOWLEDGE_MIN_CHARS`,
    /// default 12, maximum the coordination message cap).
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
    /// Load bounded limits from environment variables.
    pub fn from_env() -> Self {
        let mut limits = Self::default();
        if let Some(value) = env_parse::<usize>("COORD_KNOWLEDGE_MAX_PER_HOUR")
            .filter(|value| (1..=MAX_CONFIGURED_PER_HOUR).contains(value))
        {
            limits.max_per_hour = value;
        }
        if let Some(value) = env_parse::<usize>("COORD_KNOWLEDGE_MIN_CHARS")
            .filter(|value| (1..=DEFAULT_MAX_MESSAGE_LENGTH).contains(value))
        {
            limits.min_chars = value;
        }
        limits
    }
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// Parsed `KNOW:` share payload.
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

fn parse_knowledge_share_with_limit(
    content: &str,
    min_chars: usize,
    max_message_chars: usize,
) -> Option<KnowledgeShareMessage> {
    if content.chars().count() > max_message_chars || content.chars().any(char::is_control) {
        return None;
    }
    let trimmed = content.trim_start();
    let tag_len = KNOWLEDGE_SHARE_TAG.len();
    let tag = trimmed.get(..tag_len)?;
    if !tag.eq_ignore_ascii_case(KNOWLEDGE_SHARE_TAG)
        || trimmed.as_bytes().get(tag_len) != Some(&b':')
    {
        return None;
    }
    let (topic, finding) = trimmed.get(tag_len + 1..)?.trim().split_once('|')?;
    let topic = topic.trim();
    let finding = finding.trim();
    if !is_valid_slug(topic, MAX_SLUG_BYTES)
        || finding.chars().count() < min_chars
        || finding.len() > MAX_CONTENT_BYTES
    {
        return None;
    }
    Some(KnowledgeShareMessage {
        topic: topic.to_string(),
        content: finding.to_string(),
    })
}

/// Parse `KNOW: <topic-slug> | <finding>` using the default room length cap.
///
/// Malformed shares return `None`; the ingestion hook never rejects traffic.
/// Runtime registration additionally applies the configured coordination cap.
pub fn parse_knowledge_share(content: &str, min_chars: usize) -> Option<KnowledgeShareMessage> {
    parse_knowledge_share_with_limit(content, min_chars, DEFAULT_MAX_MESSAGE_LENGTH)
}

fn usable_sender(sender: &str) -> Option<&str> {
    let sender = sender.trim();
    (!sender.is_empty()
        && sender.len() <= MAX_COORD_AGENT_ID_LEN
        && sender.bytes().all(|byte| byte.is_ascii_graphic()))
    .then_some(sender)
}

/// Map a bounded wire sender identity to a collision-resistant knowledge-agent
/// slug. The readable portion aids operators; the hash preserves identity when
/// different sender IDs normalize to the same text.
pub fn sender_agent_slug(sender: &str) -> String {
    let canonical = sender.trim().to_ascii_lowercase();
    let digest = format!("{:x}", Sha256::digest(canonical.as_bytes()));
    let suffix = &digest[..SENDER_HASH_HEX_LEN];
    let readable_limit =
        MAX_SLUG_BYTES.saturating_sub(WIRE_AGENT_PREFIX.len() + SENDER_HASH_HEX_LEN + 2);
    let mut readable = String::with_capacity(readable_limit.min(canonical.len()));
    let mut last_was_dash = true;
    for character in canonical.chars() {
        if readable.len() >= readable_limit {
            break;
        }
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            readable.push(character);
            last_was_dash = false;
        } else if !last_was_dash {
            readable.push('-');
            last_was_dash = true;
        }
    }
    let readable = readable.trim_matches('-');
    let readable = if readable.is_empty() {
        FALLBACK_AGENT_SLUG
    } else {
        readable
    };
    format!("{WIRE_AGENT_PREFIX}-{readable}-{suffix}")
}

fn share_title(finding: &str) -> String {
    let prefix_len = KNOWLEDGE_SHARE_TAG.len() + 2;
    let bounded = truncate_char_boundary(finding, MAX_TITLE_BYTES.saturating_sub(prefix_len));
    format!("{KNOWLEDGE_SHARE_TAG}: {bounded}")
}

fn wire_note_owned_by(
    metadata: &serde_json::Value,
    sender: &str,
    agent: &str,
    topic: &str,
) -> bool {
    KnowledgeNoteMeta::from_metadata_json(metadata).is_some_and(|meta| {
        meta.agent == agent
            && meta.topic == topic
            && meta.tags.iter().any(|tag| tag == COORDINATION_TAG)
            && meta
                .submitted_by
                .as_deref()
                .is_some_and(|owner| owner.eq_ignore_ascii_case(sender))
    })
}

/// Priority 13 — ingest `KNOW:` shares from coordination channels into the
/// daemon-local shared knowledge scope. It is fail-open and side-effect-only:
/// room routing is never modified or rejected.
pub struct KnowledgeShareHook {
    state: CoordinationState,
    workspace: Arc<Workspace>,
    limits: KnowledgeShareLimits,
    max_message_chars: usize,
    ingest_lock: Mutex<()>,
}

impl KnowledgeShareHook {
    /// Create a hook with default coordination message length enforcement.
    pub fn new(
        state: CoordinationState,
        workspace: Arc<Workspace>,
        limits: KnowledgeShareLimits,
    ) -> Self {
        Self {
            state,
            workspace,
            limits,
            max_message_chars: DEFAULT_MAX_MESSAGE_LENGTH,
            ingest_lock: Mutex::new(()),
        }
    }

    /// Apply the room's configured single-message character cap.
    pub fn with_max_message_chars(mut self, max_message_chars: usize) -> Self {
        self.max_message_chars = max_message_chars.max(1);
        self
    }

    async fn restore_previous(
        &self,
        path: &str,
        previous: Option<&crate::workspace::MemoryDocument>,
    ) {
        let result = match previous {
            Some(document) => match self.workspace.write(path, &document.content).await {
                Ok(_) => {
                    self.workspace
                        .set_document_metadata(path, &document.metadata)
                        .await
                }
                Err(error) => Err(error),
            },
            None => self.workspace.delete(path).await,
        };
        if let Err(error) = result {
            tracing::error!(%path, %error, "coordination knowledge rollback failed");
        }
    }

    /// Write one note while preserving content/metadata consistency and
    /// refusing to overwrite a path not already owned by this wire sender.
    async fn ingest(&self, message: &KnowledgeShareMessage, sender: &str) {
        let _guard = self.ingest_lock.lock().await;
        let agent = sender_agent_slug(sender);
        let path = note_path(&agent, &message.topic);
        let previous = match self.workspace.read_primary(&path).await {
            Ok(document) => {
                if !wire_note_owned_by(&document.metadata, sender, &agent, &message.topic) {
                    tracing::warn!(%path, sender, "coordination knowledge path ownership mismatch");
                    return;
                }
                Some(document)
            }
            Err(WorkspaceError::DocumentNotFound { .. }) => None,
            Err(error) => {
                tracing::warn!(%path, %error, "coordination knowledge preflight read failed");
                return;
            }
        };
        let meta = KnowledgeNoteMeta {
            topic: message.topic.clone(),
            agent,
            title: share_title(&message.content),
            status: KnowledgeStatus::NeedsVerification,
            confidence: None,
            studied_at: None,
            source_commit: None,
            tags: vec![COORDINATION_TAG.to_string()],
            superseded_by: None,
            submitted_by: Some(sender.to_string()),
            session_id: None,
        };
        let markdown = render_note_markdown(&meta.title, &message.content);
        if previous.as_ref().is_some_and(|document| {
            document.content == markdown && document.metadata == meta.to_metadata_json()
        }) {
            tracing::debug!(%path, sender, "duplicate coordination knowledge share ignored");
            return;
        }
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
            self.restore_previous(&path, previous.as_ref()).await;
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
            sender_id, content, ..
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
        let Some(message) = parse_knowledge_share_with_limit(
            content,
            self.limits.min_chars,
            self.max_message_chars,
        ) else {
            return Ok(HookOutcome::ok());
        };
        // A missing, overlong, or control-bearing actor cannot provide safe
        // provenance. Never fall back to the tenant owner scope.
        let Some(sender) = sender_id.as_deref().and_then(usable_sender) else {
            tracing::warn!("coordination knowledge share ignored: invalid or missing sender_id");
            return Ok(HookOutcome::ok());
        };
        // A NUL-prefixed internal namespace cannot collide with outbound keys,
        // whose first component is a validated coordination agent ID.
        let key = format!("\0knowledge-share\0{}", sender.to_ascii_lowercase());
        if !self
            .state
            .try_consume_rate_limit(&key, self.limits.max_per_hour, INGEST_WINDOW)
            .await
        {
            tracing::warn!(
                sender,
                topic = %message.topic,
                limit = self.limits.max_per_hour,
                "coordination knowledge share rate-limited; note not ingested"
            );
            return Ok(HookOutcome::ok());
        }
        self.ingest(&message, sender).await;
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
        assert!(
            parse_knowledge_share("  know: t-1 | a finding long enough", DEFAULT_MIN_CHARS)
                .is_some()
        );
    }

    #[test]
    fn rejects_malformed_or_non_single_line_shares() {
        let min = DEFAULT_MIN_CHARS;
        for malformed in [
            "STATUS: routine update",
            "KNOWLEDGE: t | finding text",
            "💡NOW: t | unicode prefix must not panic",
            "KNOW: topic without separator",
            "KNOW: Not_A_Slug | finding text",
            "KNOW: | finding text",
            "KNOW: topic | tiny",
            "KNOW: topic |",
            "KNOW: topic | first line\nsecond line",
            "KNOW: topic | control\tcharacter",
        ] {
            assert!(
                parse_knowledge_share(malformed, min).is_none(),
                "unexpectedly parsed {malformed:?}"
            );
        }
        let too_long = format!("KNOW: topic | {}", "x".repeat(DEFAULT_MAX_MESSAGE_LENGTH));
        assert!(parse_knowledge_share(&too_long, min).is_none());
        let with_pipes =
            parse_knowledge_share("KNOW: topic | a | b findings", min).expect("parses");
        assert_eq!(with_pipes.content, "a | b findings");
    }

    #[test]
    fn sender_slugs_are_namespaced_stable_and_collision_resistant() {
        for sender in [
            "Applejack@conference.example/resource",
            "rarity",
            "kimi-14",
            "!!",
            "tenant.b@conf.example",
        ] {
            let slug = sender_agent_slug(sender);
            assert!(slug.starts_with("coord-"));
            assert!(is_valid_slug(&slug, MAX_SLUG_BYTES));
            assert_eq!(slug, sender_agent_slug(sender));
        }
        assert_eq!(
            sender_agent_slug("Applejack"),
            sender_agent_slug("applejack")
        );
        assert_ne!(sender_agent_slug("a@b"), sender_agent_slug("a-b"));
        assert_ne!(sender_agent_slug("a/b"), sender_agent_slug("a-b"));
    }

    #[test]
    fn sender_and_title_bounds_are_enforced() {
        assert!(usable_sender("agent@example.test").is_some());
        assert!(usable_sender("").is_none());
        assert!(usable_sender("bad sender").is_none());
        assert!(usable_sender("bad\nsender").is_none());
        assert!(usable_sender(&"x".repeat(MAX_COORD_AGENT_ID_LEN + 1)).is_none());

        let title = share_title(&"é".repeat(MAX_TITLE_BYTES));
        assert!(title.len() <= MAX_TITLE_BYTES);
        assert!(title.starts_with("KNOW: "));
    }

    #[cfg(feature = "libsql")]
    mod integration {
        use super::*;
        use crate::hooks::hook::Hook;
        use crate::testing::test_db;

        fn hook(workspace: Arc<Workspace>, limits: KnowledgeShareLimits) -> KnowledgeShareHook {
            let state = CoordinationState::new(
                vec!["coord@conference.example".to_string()],
                Vec::new(),
                None,
            );
            KnowledgeShareHook::new(state, workspace, limits)
        }

        fn inbound(content: &str, sender: Option<&str>) -> HookEvent {
            HookEvent::Inbound {
                user_id: "workspace-owner".to_string(),
                sender_id: sender.map(str::to_string),
                channel: "xmpp".to_string(),
                content: content.to_string(),
                thread_id: None,
                policy_scope: Some("coord@conference.example".to_string()),
            }
        }

        async fn shared_note(
            workspace: &Workspace,
            sender: &str,
            topic: &str,
        ) -> Option<KnowledgeNoteMeta> {
            let path = note_path(&sender_agent_slug(sender), topic);
            let document = workspace.read_primary(&path).await.ok()?;
            KnowledgeNoteMeta::from_metadata_json(&document.metadata)
        }

        #[tokio::test]
        async fn ingests_unverified_share_with_sender_provenance() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let hook = hook(Arc::clone(&workspace), KnowledgeShareLimits::default());
            let sender = "Applejack@conf.example/res";

            let outcome = hook
                .execute(
                    &inbound(
                        "KNOW: mt-admin-cgroups | Bounded builds use per-tenant build subtrees",
                        Some(sender),
                    ),
                    &Default::default(),
                )
                .await;
            assert!(matches!(
                outcome,
                Ok(HookOutcome::Continue { modified: None })
            ));

            let meta = shared_note(&workspace, sender, "mt-admin-cgroups")
                .await
                .expect("note ingested");
            assert_eq!(meta.status, KnowledgeStatus::NeedsVerification);
            assert_eq!(meta.agent, sender_agent_slug(sender));
            assert_eq!(meta.submitted_by.as_deref(), Some(sender));
            assert_eq!(meta.tags, vec![COORDINATION_TAG.to_string()]);
            assert!(meta.confidence.is_none());
        }

        #[tokio::test]
        async fn same_sender_replaces_only_its_wire_version() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let hook = hook(Arc::clone(&workspace), KnowledgeShareLimits::default());

            for content in [
                "KNOW: topic-x | first finding about the component",
                "KNOW: topic-x | corrected finding about the component",
            ] {
                hook.execute(&inbound(content, Some("rarity")), &Default::default())
                    .await
                    .expect("fail-open hook");
            }

            let path = note_path(&sender_agent_slug("rarity"), "topic-x");
            let document = workspace.read_primary(&path).await.expect("read note");
            assert!(document.content.contains("corrected finding"));
            assert!(
                workspace
                    .read_primary(&note_path("rarity", "topic-x"))
                    .await
                    .is_err()
            );
        }

        #[tokio::test]
        async fn normalized_sender_collisions_keep_independent_versions() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let hook = hook(Arc::clone(&workspace), KnowledgeShareLimits::default());

            hook.execute(
                &inbound("KNOW: topic-y | finding from sender at-sign", Some("a@b")),
                &Default::default(),
            )
            .await
            .expect("fail-open hook");
            hook.execute(
                &inbound("KNOW: topic-y | finding from sender dash", Some("a-b")),
                &Default::default(),
            )
            .await
            .expect("fail-open hook");

            assert!(shared_note(&workspace, "a@b", "topic-y").await.is_some());
            assert!(shared_note(&workspace, "a-b", "topic-y").await.is_some());
        }

        #[tokio::test]
        async fn ignores_wrong_scope_malformed_payload_and_missing_sender() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let hook = hook(Arc::clone(&workspace), KnowledgeShareLimits::default());
            let casual = HookEvent::Inbound {
                user_id: "workspace-owner".to_string(),
                sender_id: Some("applejack".to_string()),
                channel: "xmpp".to_string(),
                content: "KNOW: casual | a finding long enough to ingest".to_string(),
                thread_id: None,
                policy_scope: Some("casual@conference.example".to_string()),
            };
            hook.execute(&casual, &Default::default())
                .await
                .expect("fail-open hook");
            hook.execute(
                &inbound("KNOW: malformed | first\nsecond line", Some("applejack")),
                &Default::default(),
            )
            .await
            .expect("fail-open hook");
            hook.execute(
                &inbound("KNOW: no-actor | a finding long enough to ingest", None),
                &Default::default(),
            )
            .await
            .expect("fail-open hook");

            assert!(
                shared_note(&workspace, "applejack", "casual")
                    .await
                    .is_none()
            );
            assert!(
                shared_note(&workspace, "applejack", "malformed")
                    .await
                    .is_none()
            );
            assert!(
                workspace
                    .read_primary(&note_path(
                        &sender_agent_slug("workspace-owner"),
                        "no-actor"
                    ))
                    .await
                    .is_err()
            );
        }

        #[tokio::test]
        async fn rate_limit_drops_ingest_without_rejecting_message() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let hook = hook(
                Arc::clone(&workspace),
                KnowledgeShareLimits {
                    max_per_hour: 1,
                    min_chars: DEFAULT_MIN_CHARS,
                },
            );

            hook.execute(
                &inbound(
                    "KNOW: rl-a | first finding within the limit",
                    Some("flooder"),
                ),
                &Default::default(),
            )
            .await
            .expect("fail-open hook");
            let outcome = hook
                .execute(
                    &inbound(
                        "KNOW: rl-b | second finding beyond the limit",
                        Some("flooder"),
                    ),
                    &Default::default(),
                )
                .await;
            assert!(matches!(
                outcome,
                Ok(HookOutcome::Continue { modified: None })
            ));
            assert!(shared_note(&workspace, "flooder", "rl-a").await.is_some());
            assert!(shared_note(&workspace, "flooder", "rl-b").await.is_none());
        }
    }
}
