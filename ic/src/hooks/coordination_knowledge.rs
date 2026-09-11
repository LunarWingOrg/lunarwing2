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
//!
//! Trust boundary: room occupancy is not authentication. Only sender IDs on
//! the operator-configured publisher allowlist are ingested; everything else
//! is ignored without rejecting the room traffic it arrived on.

use std::sync::Arc;
use std::time::{Duration, Instant};

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
    WIRE_AGENT_PREFIX, Workspace, is_incomplete_wire_write, is_valid_slug, note_path,
    render_wire_note_markdown,
};

/// Coordination tag carrying a shareable finding.
pub const KNOWLEDGE_SHARE_TAG: &str = "KNOW";

/// Tag attached to wire-ingested study notes.
const COORDINATION_TAG: &str = "coordination";
/// Readable fallback for sender IDs with no slug-compatible characters.
const FALLBACK_AGENT_SLUG: &str = "remote";
/// Hex characters retained from the sender digest (96 bits).
const SENDER_HASH_HEX_LEN: usize = 24;
/// Number of per-path ingest lock stripes. Writes to the same note path
/// always serialize; writes to different paths land on independent stripes
/// and proceed concurrently, so one busy room no longer queues every KNOW
/// write behind a single daemon-wide mutex.
const INGEST_LOCK_STRIPES: usize = 64;
/// Ingestion rate-limit window.
const INGEST_WINDOW: Duration = Duration::from_secs(3600);
/// Default maximum ingested notes per sender per hour.
const DEFAULT_MAX_PER_HOUR: usize = 30;
/// Hard cap preventing an operator typo from creating an unbounded sender window.
const MAX_CONFIGURED_PER_HOUR: usize = 1_000;
/// Default daemon-wide maximum ingested notes per hour.
const DEFAULT_GLOBAL_MAX_PER_HOUR: usize = 300;
/// Hard cap preventing an unbounded daemon-wide timestamp window.
const MAX_CONFIGURED_GLOBAL_PER_HOUR: usize = 10_000;
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
    /// Sender IDs allowed to publish `KNOW:` shares into this tenant
    /// (`COORD_KNOWLEDGE_PUBLISHERS`, default empty = deny all).
    ///
    /// Room occupancy is not authentication: any occupant can claim any
    /// sender identity the room forwards. Ingestion therefore only accepts
    /// senders an operator explicitly listed. Entries match case-insensitively
    /// against the full inbound sender ID (for XMPP MUC that is
    /// `room@server/nick`, so the resource/nick is significant) or against the
    /// room bare JID (`room@server`) as an explicit room-wide opt-in. A user
    /// bare JID (`user@server`) never matches a MUC occupant sender, and a
    /// full-occupant entry silently stops matching when the publisher's nick
    /// changes — nick changes are config changes.
    pub publishers: Vec<String>,
}

impl Default for KnowledgeShareLimits {
    fn default() -> Self {
        Self {
            max_per_hour: DEFAULT_MAX_PER_HOUR,
            min_chars: DEFAULT_MIN_CHARS,
            publishers: Vec::new(),
        }
    }
}

impl KnowledgeShareLimits {
    /// Load bounded limits from environment variables.
    pub fn from_env() -> Self {
        let mut limits = Self::default();
        apply_bounded_env(
            "COORD_KNOWLEDGE_MAX_PER_HOUR",
            1..=MAX_CONFIGURED_PER_HOUR,
            |value| limits.max_per_hour = value,
            DEFAULT_MAX_PER_HOUR,
        );
        apply_bounded_env(
            "COORD_KNOWLEDGE_MIN_CHARS",
            1..=DEFAULT_MAX_MESSAGE_LENGTH,
            |value| limits.min_chars = value,
            DEFAULT_MIN_CHARS,
        );
        if let Ok(raw) = std::env::var("COORD_KNOWLEDGE_PUBLISHERS") {
            for entry in raw.split(',') {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                if let Some(validated) = usable_sender(entry) {
                    if !limits
                        .publishers
                        .iter()
                        .any(|existing| existing.eq_ignore_ascii_case(validated))
                    {
                        limits.publishers.push(validated.to_string());
                    }
                } else {
                    tracing::warn!(
                        entry,
                        "COORD_KNOWLEDGE_PUBLISHERS entry is not a usable sender ID; skipping it"
                    );
                }
            }
        }
        limits
    }

    /// Whether a validated inbound sender may publish knowledge shares.
    ///
    /// An entry matches either the full inbound sender ID or, when the sender
    /// carries an XMPP MUC occupant nick (`room@server/nick`), the room bare
    /// JID with the nick stripped. The room-level form admits every occupant
    /// the room forwards, so listing a room bare JID is an explicit room-wide
    /// trust decision; listing the full occupant JID pins the publisher to
    /// that exact nick.
    pub fn is_allowed_publisher(&self, sender: &str) -> bool {
        self.publishers
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(sender))
            || sender.split_once('/').is_some_and(|(room, _nick)| {
                self.publishers
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(room))
            })
    }
}

fn apply_bounded_env(
    name: &str,
    range: std::ops::RangeInclusive<usize>,
    apply: impl FnOnce(usize),
    default: usize,
) {
    let Ok(raw) = std::env::var(name) else {
        return;
    };
    match raw.trim().parse::<usize>() {
        Ok(value) if range.contains(&value) => apply(value),
        _ => tracing::warn!(
            name,
            raw = raw.trim(),
            default,
            "invalid coordination knowledge limit; using default"
        ),
    }
}

fn global_max_per_hour_from_env() -> usize {
    let mut limit = DEFAULT_GLOBAL_MAX_PER_HOUR;
    apply_bounded_env(
        "COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR",
        1..=MAX_CONFIGURED_GLOBAL_PER_HOUR,
        |value| limit = value,
        DEFAULT_GLOBAL_MAX_PER_HOUR,
    );
    limit
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

/// Map a note path to its ingest lock stripe. FNV-1a: cheap and fully
/// deterministic; striping only needs consistency within this process.
fn ingest_stripe(path: &str) -> usize {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash as usize) % INGEST_LOCK_STRIPES
}

/// Why an inbound `KNOW:` share did not become a note. Kept as a bounded,
/// body-free reason set so ingest diagnostics can be counted without ever
/// exposing room message content or sender credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IngestDropReason {
    /// `sender_id` was missing, overlong, or carried control bytes.
    InvalidSender,
    /// Sender is not on the operator-configured publisher allowlist.
    NotAllowedPublisher,
    /// Per-sender hourly budget exhausted.
    SenderRateLimited,
    /// Daemon-wide hourly budget exhausted.
    GlobalRateLimited,
}

impl IngestDropReason {
    fn label(self) -> &'static str {
        match self {
            Self::InvalidSender => "invalid_sender",
            Self::NotAllowedPublisher => "not_allowed_publisher",
            Self::SenderRateLimited => "sender_rate_limited",
            Self::GlobalRateLimited => "global_rate_limited",
        }
    }
}

/// Saturating per-reason drop counters shared between the hook and the
/// coordination admin status surface. Counts are monotonic diagnostics only;
/// they never gate ingestion.
#[derive(Debug, Default)]
pub struct IngestDiagnostics {
    invalid_sender: std::sync::atomic::AtomicU64,
    not_allowed_publisher: std::sync::atomic::AtomicU64,
    sender_rate_limited: std::sync::atomic::AtomicU64,
    global_rate_limited: std::sync::atomic::AtomicU64,
}

impl IngestDiagnostics {
    /// Record one drop of the given reason. Saturates at `u64::MAX` rather
    /// than wrapping, so a long-lived counter can never under-report.
    pub fn record_drop(&self, reason: IngestDropReason) {
        let counter = match reason {
            IngestDropReason::InvalidSender => &self.invalid_sender,
            IngestDropReason::NotAllowedPublisher => &self.not_allowed_publisher,
            IngestDropReason::SenderRateLimited => &self.sender_rate_limited,
            IngestDropReason::GlobalRateLimited => &self.global_rate_limited,
        };
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Current per-reason drop counts as `(label, count)` pairs, ordered by
    /// the enum declaration.
    pub fn drop_counts(&self) -> Vec<(&'static str, u64)> {
        use std::sync::atomic::Ordering::Relaxed;
        [
            (IngestDropReason::InvalidSender, &self.invalid_sender),
            (
                IngestDropReason::NotAllowedPublisher,
                &self.not_allowed_publisher,
            ),
            (
                IngestDropReason::SenderRateLimited,
                &self.sender_rate_limited,
            ),
            (
                IngestDropReason::GlobalRateLimited,
                &self.global_rate_limited,
            ),
        ]
        .into_iter()
        .map(|(reason, counter)| (reason.label(), counter.load(Relaxed)))
        .collect()
    }
}

/// Priority 13 — ingest `KNOW:` shares from coordination channels into the
/// daemon-local shared knowledge scope. It is fail-open and side-effect-only:
/// room routing is never modified or rejected.
pub struct KnowledgeShareHook {
    state: CoordinationState,
    workspace: Arc<Workspace>,
    limits: KnowledgeShareLimits,
    max_message_chars: usize,
    /// Bounded, body-free ingest diagnostics surfaced by the coordination
    /// admin status endpoint.
    pub diagnostics: Arc<IngestDiagnostics>,
    /// Per-path striped locks serializing write-then-metadata pairs so two
    /// concurrent shares for the same note cannot interleave into mismatched
    /// content/metadata states, while shares for different paths proceed
    /// concurrently. Shared with the detached ingest tasks, which outlive
    /// individual hook executions.
    ingest_locks: Arc<Vec<Mutex<()>>>,
    /// Daemon-wide ingress window, independent of per-sender buckets.
    global_window: Arc<Mutex<Vec<Instant>>>,
    global_max_per_hour: usize,
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
            diagnostics: Arc::new(IngestDiagnostics::default()),
            ingest_locks: Arc::new((0..INGEST_LOCK_STRIPES).map(|_| Mutex::new(())).collect()),
            global_window: Arc::new(Mutex::new(Vec::new())),
            global_max_per_hour: global_max_per_hour_from_env(),
        }
    }

    /// Apply the room's configured single-message character cap.
    pub fn with_max_message_chars(mut self, max_message_chars: usize) -> Self {
        self.max_message_chars = max_message_chars.max(1);
        self
    }

    /// Replace the internal diagnostics with an externally owned handle so
    /// hook drops and a status surface observe the same counters.
    pub fn with_shared_diagnostics(mut self, shared: Arc<IngestDiagnostics>) -> Self {
        self.diagnostics = shared;
        self
    }

    #[cfg(all(test, feature = "libsql"))]
    fn with_global_max_per_hour(mut self, limit: usize) -> Self {
        self.global_max_per_hour = limit.max(1);
        self
    }

    async fn try_consume_global_limit(&self) -> bool {
        let now = Instant::now();
        let mut window = self.global_window.lock().await;
        window.retain(|timestamp| now.duration_since(*timestamp) < INGEST_WINDOW);
        if window.len() >= self.global_max_per_hour {
            return false;
        }
        window.push(now);
        true
    }

    async fn restore_previous(
        workspace: &Workspace,
        path: &str,
        previous: Option<&crate::workspace::MemoryDocument>,
    ) {
        let result = match previous {
            Some(document) => match workspace.write(path, &document.content).await {
                Ok(_) => {
                    workspace
                        .set_document_metadata(path, &document.metadata)
                        .await
                }
                Err(error) => Err(error),
            },
            None => workspace.delete(path).await,
        };
        if let Err(error) = result {
            tracing::error!(%path, %error, "coordination knowledge rollback failed");
        }
    }

    /// Write one note while preserving content/metadata consistency and
    /// refusing to overwrite a path not already owned by this wire sender.
    ///
    /// Runs as a detached task (see [`Hook::execute`]) so the registry's
    /// hook deadline can never cancel it between the content and metadata
    /// commits. Should a crash or shutdown still strand a note between the
    /// two commits, the preflight below recognizes it via the wire sentinel
    /// (see [`is_incomplete_wire_write`]) and the next share for that
    /// sender/topic repairs the path instead of wedging it behind an
    /// ownership refusal. Stranded notes whose sender never shares again are
    /// reaped by the periodic knowledge sweep.
    async fn ingest(
        workspace: Arc<Workspace>,
        ingest_locks: Arc<Vec<Mutex<()>>>,
        message: &KnowledgeShareMessage,
        sender: &str,
    ) {
        let agent = sender_agent_slug(sender);
        let path = note_path(&agent, &message.topic);
        // Only same-path writes must serialize; the stripe lock keeps
        // unrelated paths (other senders/topics) fully concurrent.
        let _guard = ingest_locks[ingest_stripe(&path)].lock().await;
        let previous = match workspace.read_primary(&path).await {
            Ok(document) => {
                let owned = wire_note_owned_by(&document.metadata, sender, &agent, &message.topic);
                if !owned && !is_incomplete_wire_write(&document.metadata, &document.content) {
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
            stale_after_days: None,
            status_reason: None,
            submitted_by: Some(sender.to_string()),
            session_id: None,
        };
        let markdown = render_wire_note_markdown(&meta.title, &message.content);
        if previous.as_ref().is_some_and(|document| {
            document.content == markdown && document.metadata == meta.to_metadata_json()
        }) {
            tracing::debug!(%path, sender, "duplicate coordination knowledge share ignored");
            return;
        }
        if let Err(error) = workspace.write(&path, &markdown).await {
            tracing::warn!(%path, %error, "coordination knowledge share write failed");
            return;
        }
        if let Err(error) = workspace
            .set_document_metadata(&path, &meta.to_metadata_json())
            .await
        {
            tracing::warn!(%path, %error, "coordination knowledge share metadata failed");
            Self::restore_previous(&workspace, &path, previous.as_ref()).await;
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
            self.diagnostics
                .record_drop(IngestDropReason::InvalidSender);
            tracing::warn!("coordination knowledge share ignored: invalid or missing sender_id");
            return Ok(HookOutcome::ok());
        };
        // Room occupancy is not authentication: an occupant can claim any
        // nick the room forwards. Only operator-listed publishers enter the
        // knowledge scope, so unlisted senders are ignored without affecting
        // room traffic.
        if !self.limits.is_allowed_publisher(sender) {
            self.diagnostics
                .record_drop(IngestDropReason::NotAllowedPublisher);
            tracing::debug!(
                sender,
                "coordination knowledge share from non-allowlisted sender ignored"
            );
            return Ok(HookOutcome::ok());
        }
        // A NUL-prefixed internal namespace cannot collide with outbound keys,
        // whose first component is a validated coordination agent ID.
        let key = format!("\0knowledge-share\0{}", sender.to_ascii_lowercase());
        if !self
            .state
            .try_consume_rate_limit(&key, self.limits.max_per_hour, INGEST_WINDOW)
            .await
        {
            self.diagnostics
                .record_drop(IngestDropReason::SenderRateLimited);
            tracing::warn!(
                sender,
                topic = %message.topic,
                limit = self.limits.max_per_hour,
                "coordination knowledge share sender rate-limited; note not ingested"
            );
            return Ok(HookOutcome::ok());
        }
        if !self.try_consume_global_limit().await {
            self.diagnostics
                .record_drop(IngestDropReason::GlobalRateLimited);
            tracing::warn!(
                sender,
                topic = %message.topic,
                limit = self.global_max_per_hour,
                "coordination knowledge share global rate limit reached; note not ingested"
            );
            return Ok(HookOutcome::ok());
        }
        // Detached: the registry drops hook futures at their deadline, and a
        // drop between the content and metadata commits would strand the
        // note. The ingest task runs to completion on its own budget.
        let workspace = Arc::clone(&self.workspace);
        let ingest_locks = Arc::clone(&self.ingest_locks);
        let message = message.clone();
        let sender = sender.to_string();
        tokio::spawn(async move {
            Self::ingest(workspace, ingest_locks, &message, &sender).await;
        });
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

    /// Process-env limit parsing must reject non-numeric and out-of-range
    /// values with a warning and fall back to the documented defaults, never
    /// producing an unbounded or zero window.
    #[test]
    fn invalid_env_limits_fall_back_to_defaults() {
        let _env_lock = crate::config::helpers::lock_env();
        // SAFETY: the env lock is held for the whole test, so no other
        // env-mutating test runs concurrently; the guard restores on drop.
        unsafe {
            std::env::set_var("COORD_KNOWLEDGE_MAX_PER_HOUR", "not-a-number");
            std::env::set_var("COORD_KNOWLEDGE_MIN_CHARS", "0");
            std::env::set_var("COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR", "999999");
        }
        let limits = KnowledgeShareLimits::from_env();
        assert_eq!(limits.max_per_hour, DEFAULT_MAX_PER_HOUR);
        assert_eq!(limits.min_chars, DEFAULT_MIN_CHARS);
        assert_eq!(global_max_per_hour_from_env(), DEFAULT_GLOBAL_MAX_PER_HOUR);
    }

    #[test]
    fn env_limits_accept_range_boundaries() {
        let _env_lock = crate::config::helpers::lock_env();
        // SAFETY: see invalid_env_limits_fall_back_to_defaults.
        unsafe {
            std::env::set_var("COORD_KNOWLEDGE_MAX_PER_HOUR", "1000");
            std::env::set_var("COORD_KNOWLEDGE_MIN_CHARS", "1");
            std::env::set_var("COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR", "10000");
        }
        let limits = KnowledgeShareLimits::from_env();
        assert_eq!(limits.max_per_hour, MAX_CONFIGURED_PER_HOUR);
        assert_eq!(limits.min_chars, 1);
        assert_eq!(
            global_max_per_hour_from_env(),
            MAX_CONFIGURED_GLOBAL_PER_HOUR
        );
        // SAFETY: same lock still held.
        unsafe {
            std::env::set_var("COORD_KNOWLEDGE_MAX_PER_HOUR", "1001");
            std::env::set_var(
                "COORD_KNOWLEDGE_MIN_CHARS",
                (DEFAULT_MAX_MESSAGE_LENGTH + 1).to_string(),
            );
        }
        let limits = KnowledgeShareLimits::from_env();
        assert_eq!(limits.max_per_hour, DEFAULT_MAX_PER_HOUR);
        assert_eq!(limits.min_chars, DEFAULT_MIN_CHARS);
    }

    #[test]
    fn publisher_env_entries_are_validated_and_deduplicated() {
        let _env_lock = crate::config::helpers::lock_env();
        // SAFETY: see invalid_env_limits_fall_back_to_defaults.
        unsafe {
            std::env::set_var(
                "COORD_KNOWLEDGE_PUBLISHERS",
                "Agent@Conf.example/Res, ,bad sender,agent@conf.example/res,peer-b",
            );
        }
        let limits = KnowledgeShareLimits::from_env();
        assert_eq!(
            limits.publishers,
            vec!["Agent@Conf.example/Res".to_string(), "peer-b".to_string()]
        );
        assert!(limits.is_allowed_publisher("agent@conf.example/RES"));
        assert!(!limits.is_allowed_publisher("bad sender"));
    }

    /// WI#97: an allowlist entry must match either the full MUC occupant JID
    /// (`room@server/nick`) or the room bare JID (`room@server`) as an
    /// explicit room-wide opt-in. A user bare JID (`user@server`) never
    /// matches a MUC occupant sender — that was the misconfiguration behind
    /// the silent cross-tenant ingestion failure.
    #[test]
    fn publisher_allowlist_matches_full_occupant_jid_or_room_bare_jid() {
        let limits = KnowledgeShareLimits {
            publishers: vec![
                "mepuhiy@conference.xmpp.sobe.world".to_string(),
                "other@conference.example/sunburst".to_string(),
            ],
            ..KnowledgeShareLimits::default()
        };
        // Full occupant JID entries keep exact occupant-only semantics.
        assert!(limits.is_allowed_publisher("other@conference.example/Sunburst"));
        assert!(!limits.is_allowed_publisher("other@conference.example/impostor"));
        // A room bare JID entry admits any occupant of that room, and no
        // occupant of a different room.
        assert!(limits.is_allowed_publisher("mepuhiy@conference.xmpp.sobe.world/ruffles"));
        assert!(limits.is_allowed_publisher("mepuhiy@conference.xmpp.sobe.world/SUNBURST"));
        assert!(!limits.is_allowed_publisher("elsewhere@conference.xmpp.sobe.world/ruffles"));
        // Different room, same server: no match.
        assert!(!limits.is_allowed_publisher("ruffles@conference.xmpp.sobe.world/x"));
        // Senders without an occupant nick only match an exact entry; the
        // fallback never strips anything when there is no nick to strip.
        assert!(!limits.is_allowed_publisher("conference.xmpp.sobe.world"));
        assert!(!limits.is_allowed_publisher("other@conference.example"));

        // A user bare JID entry matches a DM-style sender but never a MUC
        // occupant sender carrying a room JID.
        let user_bare = KnowledgeShareLimits {
            publishers: vec!["ruffles@xmpp.sobe.world".to_string()],
            ..KnowledgeShareLimits::default()
        };
        assert!(user_bare.is_allowed_publisher("ruffles@xmpp.sobe.world"));
        assert!(!user_bare.is_allowed_publisher("mepuhiy@conference.xmpp.sobe.world/ruffles"));
    }

    #[test]
    fn ingest_diagnostics_count_is_monotonic_and_labeled() {
        let diagnostics = IngestDiagnostics::default();
        diagnostics.record_drop(IngestDropReason::NotAllowedPublisher);
        diagnostics.record_drop(IngestDropReason::NotAllowedPublisher);
        diagnostics.record_drop(IngestDropReason::InvalidSender);
        let counts: std::collections::HashMap<_, _> =
            diagnostics.drop_counts().into_iter().collect();
        assert_eq!(counts["not_allowed_publisher"], 2);
        assert_eq!(counts["invalid_sender"], 1);
        assert_eq!(counts["sender_rate_limited"], 0);
        assert_eq!(counts["global_rate_limited"], 0);
        assert_eq!(counts.len(), 4);
    }

    #[test]
    fn ingest_diagnostics_labels_are_stable_and_full() {
        let diagnostics = IngestDiagnostics::default();
        let labels: Vec<&str> = diagnostics
            .drop_counts()
            .into_iter()
            .map(|(label, _)| label)
            .collect();
        assert_eq!(
            labels,
            vec![
                "invalid_sender",
                "not_allowed_publisher",
                "sender_rate_limited",
                "global_rate_limited",
            ]
        );
    }

    #[cfg(feature = "libsql")]
    mod integration {
        use super::*;
        use crate::hooks::hook::Hook;
        use crate::testing::test_db;

        fn limits_for(publishers: &[&str]) -> KnowledgeShareLimits {
            KnowledgeShareLimits {
                publishers: publishers.iter().map(|p| p.to_string()).collect(),
                ..KnowledgeShareLimits::default()
            }
        }

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

        /// Ingestion runs on a detached task; poll briefly for its result so
        /// assertions stay deterministic without production-side handles.
        async fn await_ingest(
            workspace: &Workspace,
            sender: &str,
            topic: &str,
        ) -> Option<KnowledgeNoteMeta> {
            let path = note_path(&sender_agent_slug(sender), topic);
            for _ in 0..500 {
                if let Ok(document) = workspace.read_primary(&path).await
                    && let Some(meta) = KnowledgeNoteMeta::from_metadata_json(&document.metadata)
                {
                    return Some(meta);
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            shared_note(workspace, sender, topic).await
        }

        #[tokio::test]
        async fn ingests_unverified_share_with_sender_provenance() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let sender = "Applejack@conf.example/res";
            let hook = hook(Arc::clone(&workspace), limits_for(&[sender]));

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

            let meta = await_ingest(&workspace, sender, "mt-admin-cgroups")
                .await
                .expect("note ingested");
            assert_eq!(meta.status, KnowledgeStatus::NeedsVerification);
            assert_eq!(meta.agent, sender_agent_slug(sender));
            assert_eq!(meta.submitted_by.as_deref(), Some(sender));
            assert_eq!(meta.tags, vec![COORDINATION_TAG.to_string()]);
            assert!(meta.confidence.is_none());

            // The stored body carries the hard wire sentinel from the first
            // content commit (stranded-write detection, sweep eligibility),
            // and the completed note no longer reads as stranded.
            let document = workspace
                .read_primary(&note_path(&meta.agent, "mt-admin-cgroups"))
                .await
                .expect("read ingested note");
            assert!(
                document
                    .content
                    .starts_with(crate::workspace::WIRE_NOTE_SENTINEL)
            );
            assert!(!is_incomplete_wire_write(
                &document.metadata,
                &document.content
            ));

            let hits = workspace
                .knowledge_search(
                    &["shared".to_string()],
                    "bounded builds tenant subtrees",
                    &crate::workspace::KnowledgeFilter::default(),
                    10,
                )
                .await
                .expect("search ingested share");
            assert!(hits.iter().any(|hit| hit.meta == meta));
        }

        #[tokio::test]
        async fn configured_message_cap_is_enforced_before_ingest() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let sender = "bounded-sender";
            let hook =
                hook(Arc::clone(&workspace), limits_for(&[sender])).with_max_message_chars(40);

            hook.execute(
                &inbound(
                    "KNOW: capped-topic | this finding exceeds the configured room cap",
                    Some(sender),
                ),
                &Default::default(),
            )
            .await
            .expect("fail-open hook");

            assert!(
                shared_note(&workspace, sender, "capped-topic")
                    .await
                    .is_none()
            );
            assert!(hook.global_window.lock().await.is_empty());
        }

        #[tokio::test]
        async fn secret_hook_rejects_before_knowledge_ingest() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let sender = "secret-sender";
            let state = CoordinationState::new(
                vec!["coord@conference.example".to_string()],
                Vec::new(),
                None,
            );
            let registry = crate::hooks::HookRegistry::new();
            registry
                .register_with_priority(
                    Arc::new(crate::hooks::coordination::SecretEntropyHook::new(
                        state.clone(),
                        "local-agent".to_string(),
                        4.8,
                        32,
                    )),
                    5,
                )
                .await;
            let knowledge = Arc::new(KnowledgeShareHook::new(
                state,
                Arc::clone(&workspace),
                limits_for(&[sender]),
            ));
            registry.register_with_priority(knowledge.clone(), 13).await;

            let result = registry
                .run(&inbound(
                    "KNOW: secret-order | api_key=abcdefghijklmnop",
                    Some(sender),
                ))
                .await;
            assert!(matches!(result, Err(HookError::Rejected { .. })));
            assert!(
                shared_note(&workspace, sender, "secret-order")
                    .await
                    .is_none()
            );
            assert!(knowledge.global_window.lock().await.is_empty());
        }

        #[tokio::test]
        async fn non_allowlisted_sender_is_ignored_without_rejecting_the_room() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            // Empty allowlist: nobody may publish.
            let deny_all = hook(Arc::clone(&workspace), limits_for(&[]));

            let outcome = deny_all
                .execute(
                    &inbound(
                        "KNOW: topic-a | a finding long enough to ingest",
                        Some("stranger"),
                    ),
                    &Default::default(),
                )
                .await;
            assert!(matches!(
                outcome,
                Ok(HookOutcome::Continue { modified: None })
            ));
            assert!(
                shared_note(&workspace, "stranger", "topic-a")
                    .await
                    .is_none()
            );

            // An allowlisted nick does not admit a different sender identity,
            // even one differing only by MUC resource. The refusal is
            // synchronous, so no settle wait is needed.
            let allowlisted = hook(Arc::clone(&workspace), limits_for(&["rarity"]));
            allowlisted
                .execute(
                    &inbound(
                        "KNOW: topic-b | another finding long enough to ingest",
                        Some("rarity@conf.example/impostor"),
                    ),
                    &Default::default(),
                )
                .await
                .expect("fail-open hook");
            assert!(
                shared_note(&workspace, "rarity@conf.example/impostor", "topic-b")
                    .await
                    .is_none()
            );
        }

        #[test]
        fn publisher_allowlist_matches_full_sender_case_insensitively() {
            let limits = limits_for(&["Applejack@conf.example/res", "rarity"]);
            assert!(limits.is_allowed_publisher("applejack@conf.example/RES"));
            assert!(limits.is_allowed_publisher("Rarity"));
            assert!(!limits.is_allowed_publisher("applejack@conf.example/other"));
            assert!(!limits.is_allowed_publisher("stranger"));
        }

        /// WI#97 regression: a room bare JID entry is a room-wide opt-in, so
        /// a `KNOW:` share from an occupant of the coordination room ingests
        /// even though the allowlist names no full occupant JID. Raw sender
        /// provenance keeps the full occupant JID either way.
        #[tokio::test]
        async fn room_bare_entry_ingests_room_occupant_share() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let sender = "coord@conference.example/sunburst";
            let room_hook = hook(
                Arc::clone(&workspace),
                limits_for(&["coord@conference.example"]),
            );

            room_hook
                .execute(
                    &inbound(
                        "KNOW: topic-w | a cross-tenant finding long enough to ingest",
                        Some(sender),
                    ),
                    &Default::default(),
                )
                .await
                .expect("fail-open hook");

            let meta = await_ingest(&workspace, sender, "topic-w")
                .await
                .expect("room occupant share ingested via room-bare entry");
            assert_eq!(meta.submitted_by.as_deref(), Some(sender));

            // An occupant of a different, unlisted room stays ignored.
            hook(
                Arc::clone(&workspace),
                limits_for(&["coord@conference.example"]),
            )
            .execute(
                &inbound(
                    "KNOW: topic-v | another finding long enough to ingest",
                    Some("elsewhere@conference.example/sunburst"),
                ),
                &Default::default(),
            )
            .await
            .expect("fail-open hook");
            assert!(
                shared_note(
                    &workspace,
                    "elsewhere@conference.example/sunburst",
                    "topic-v"
                )
                .await
                .is_none()
            );
        }

        #[tokio::test]
        async fn ownership_mismatch_refuses_existing_wire_path() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let sender = "owner-a";
            let topic = "owned-topic";
            let path = note_path(&sender_agent_slug(sender), topic);
            let foreign_meta = KnowledgeNoteMeta {
                topic: topic.to_string(),
                agent: sender_agent_slug(sender),
                title: "Foreign note".to_string(),
                status: KnowledgeStatus::Active,
                confidence: Some(1.0),
                studied_at: None,
                source_commit: Some("verified".to_string()),
                tags: vec!["local".to_string()],
                superseded_by: None,
                stale_after_days: None,
                status_reason: None,
                submitted_by: Some("different-owner".to_string()),
                session_id: None,
            };
            let original =
                crate::workspace::render_note_markdown("Foreign note", "verified local content");
            workspace
                .write(&path, &original)
                .await
                .expect("seed foreign note");
            workspace
                .set_document_metadata(&path, &foreign_meta.to_metadata_json())
                .await
                .expect("seed foreign metadata");
            let hook = hook(Arc::clone(&workspace), limits_for(&[sender]));

            hook.execute(
                &inbound(
                    "KNOW: owned-topic | replacement must be refused",
                    Some(sender),
                ),
                &Default::default(),
            )
            .await
            .expect("fail-open hook");
            tokio::time::sleep(Duration::from_millis(50)).await;

            let document = workspace
                .read_primary(&path)
                .await
                .expect("read foreign note");
            assert_eq!(document.content, original);
            assert_eq!(document.metadata, foreign_meta.to_metadata_json());
        }

        #[tokio::test]
        async fn same_sender_replaces_only_its_wire_version() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let hook = hook(Arc::clone(&workspace), limits_for(&["rarity"]));

            for content in [
                "KNOW: topic-x | first finding about the component",
                "KNOW: topic-x | corrected finding about the component",
            ] {
                hook.execute(&inbound(content, Some("rarity")), &Default::default())
                    .await
                    .expect("fail-open hook");
                await_ingest(&workspace, "rarity", "topic-x")
                    .await
                    .expect("share ingested");
            }
            // The second share replaces the first; the poll above may have
            // observed the first version's metadata, so await the new body
            // with a bounded retry.
            let path = note_path(&sender_agent_slug("rarity"), "topic-x");
            let mut document = None;
            for _ in 0..500 {
                let current = workspace.read_primary(&path).await.expect("read note");
                if current.content.contains("corrected finding") {
                    document = Some(current);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            assert!(
                document
                    .expect("corrected finding eventually ingested")
                    .content
                    .contains("corrected finding")
            );
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
            let hook = hook(Arc::clone(&workspace), limits_for(&["a@b", "a-b"]));

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
            await_ingest(&workspace, "a@b", "topic-y")
                .await
                .expect("at-sign note ingested");
            await_ingest(&workspace, "a-b", "topic-y")
                .await
                .expect("dash note ingested");

            assert!(shared_note(&workspace, "a@b", "topic-y").await.is_some());
            assert!(shared_note(&workspace, "a-b", "topic-y").await.is_some());
        }

        #[tokio::test]
        async fn repairs_a_stranded_write_from_a_cancelled_ingest() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let sender = "repairer";
            let hook = hook(Arc::clone(&workspace), limits_for(&[sender]));

            // Simulate a hook timeout after the content commit: the note body
            // exists but never received study-note metadata. Without the
            // repair path this wedges the sender/topic behind an ownership
            // refusal forever.
            let path = note_path(&sender_agent_slug(sender), "topic-r");
            let stranded_title = share_title("stranded finding about the component");
            workspace
                .write(
                    &path,
                    &render_wire_note_markdown(&stranded_title, "stranded finding"),
                )
                .await
                .expect("strand note");
            let stranded_doc = workspace.read_primary(&path).await.expect("read stranded");
            assert!(is_incomplete_wire_write(
                &stranded_doc.metadata,
                &stranded_doc.content
            ));

            hook.execute(
                &inbound(
                    "KNOW: topic-r | repaired finding about the component",
                    Some(sender),
                ),
                &Default::default(),
            )
            .await
            .expect("fail-open hook");
            let meta = await_ingest(&workspace, sender, "topic-r")
                .await
                .expect("stranded note repaired");
            assert_eq!(meta.topic, "topic-r");
            assert_eq!(meta.submitted_by.as_deref(), Some(sender));
        }

        #[tokio::test]
        async fn ignores_wrong_scope_malformed_payload_and_missing_sender() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let hook = hook(
                Arc::clone(&workspace),
                limits_for(&["applejack", "workspace-owner"]),
            );
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
            // All three refusals are synchronous; a short grace period keeps
            // the negative assertions from racing a late spawn.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

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
                    publishers: vec!["flooder".to_string()],
                    ..KnowledgeShareLimits::default()
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
            await_ingest(&workspace, "flooder", "rl-a")
                .await
                .expect("first share ingested");
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

        #[tokio::test]
        async fn global_rate_limit_caps_cross_sender_ingest_work() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let hook = hook(
                Arc::clone(&workspace),
                KnowledgeShareLimits {
                    max_per_hour: 10,
                    publishers: vec!["sender-a".to_string(), "sender-b".to_string()],
                    ..KnowledgeShareLimits::default()
                },
            )
            .with_global_max_per_hour(1);

            hook.execute(
                &inbound(
                    "KNOW: global-a | first finding within the daemon budget",
                    Some("sender-a"),
                ),
                &Default::default(),
            )
            .await
            .expect("fail-open hook");
            await_ingest(&workspace, "sender-a", "global-a")
                .await
                .expect("first share ingested");

            let outcome = hook
                .execute(
                    &inbound(
                        "KNOW: global-b | second sender exceeds daemon budget",
                        Some("sender-b"),
                    ),
                    &Default::default(),
                )
                .await;
            assert!(matches!(
                outcome,
                Ok(HookOutcome::Continue { modified: None })
            ));
            assert!(
                shared_note(&workspace, "sender-a", "global-a")
                    .await
                    .is_some()
            );
            assert!(
                shared_note(&workspace, "sender-b", "global-b")
                    .await
                    .is_none()
            );
            assert_eq!(hook.global_window.lock().await.len(), 1);
        }

        /// The daemon-wide budget is a hard admission guarantee: the
        /// check-and-insert runs under one lock, so concurrent hook
        /// executions cannot both slip past it. 50 racing admissions for a
        /// budget of 5 must admit exactly 5.
        #[tokio::test]
        async fn global_limit_admission_is_atomic_under_concurrency() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let hook = Arc::new(
                hook(
                    Arc::clone(&workspace),
                    KnowledgeShareLimits {
                        max_per_hour: 100,
                        publishers: vec!["burst".to_string()],
                        ..KnowledgeShareLimits::default()
                    },
                )
                .with_global_max_per_hour(5),
            );

            let mut handles = Vec::new();
            for index in 0..50 {
                let hook = Arc::clone(&hook);
                handles.push(tokio::spawn(async move {
                    hook.execute(
                        &inbound(
                            &format!("KNOW: burst-{index} | concurrent admission probe finding"),
                            Some("burst"),
                        ),
                        &Default::default(),
                    )
                    .await
                }));
            }
            for handle in handles {
                handle
                    .await
                    .expect("spawn panicked")
                    .expect("fail-open hook");
            }
            assert_eq!(hook.global_window.lock().await.len(), 5);
        }

        #[tokio::test]
        async fn duplicate_share_is_an_ingest_noop() {
            let (db, _dir) = test_db().await;
            let workspace = Arc::new(Workspace::new_with_db("shared", db));
            let sender = "repeater";
            let hook = hook(Arc::clone(&workspace), limits_for(&[sender]));
            let share = "KNOW: topic-d | an idempotent finding about the component";

            for _ in 0..2 {
                hook.execute(&inbound(share, Some(sender)), &Default::default())
                    .await
                    .expect("fail-open hook");
                await_ingest(&workspace, sender, "topic-d")
                    .await
                    .expect("note present");
            }

            let meta = shared_note(&workspace, sender, "topic-d")
                .await
                .expect("note present");
            assert_eq!(meta.submitted_by.as_deref(), Some(sender));
        }
    }
}
