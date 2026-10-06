//! Conversation surface — the UI layer, separate from execution.
//!
//! A conversation is a stream of entries visible to the user. Threads
//! (the execution units) run independently and produce entries that
//! appear in conversations. One conversation can have multiple active
//! threads; one thread can outlive its originating conversation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::thread::ThreadId;

/// Strongly-typed conversation identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConversationId(pub Uuid);

impl ConversationId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ConversationId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ConversationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Strongly-typed entry identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntryId(pub Uuid);

impl EntryId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EntryId {
    fn default() -> Self {
        Self::new()
    }
}

/// Who sent a conversation entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntrySender {
    /// The human user.
    User,
    /// The agent (from a specific thread).
    Agent { thread_id: ThreadId },
    /// System notification (thread started, completed, etc.).
    System,
}

/// A single entry in a conversation — a message visible to the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationEntry {
    pub id: EntryId,
    pub sender: EntrySender,
    pub content: String,
    /// Which thread produced this entry (if any).
    pub origin_thread_id: Option<ThreadId>,
    pub timestamp: DateTime<Utc>,
    /// Optional metadata (channel-specific formatting, attachments, etc.).
    pub metadata: serde_json::Value,
}

impl ConversationEntry {
    /// Create a user entry.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            id: EntryId::new(),
            sender: EntrySender::User,
            content: content.into(),
            origin_thread_id: None,
            timestamp: Utc::now(),
            metadata: serde_json::Value::Null,
        }
    }

    /// Create an agent entry from a thread.
    pub fn agent(thread_id: ThreadId, content: impl Into<String>) -> Self {
        Self {
            id: EntryId::new(),
            sender: EntrySender::Agent { thread_id },
            content: content.into(),
            origin_thread_id: Some(thread_id),
            timestamp: Utc::now(),
            metadata: serde_json::Value::Null,
        }
    }

    /// Create a system notification entry.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            id: EntryId::new(),
            sender: EntrySender::System,
            content: content.into(),
            origin_thread_id: None,
            timestamp: Utc::now(),
            metadata: serde_json::Value::Null,
        }
    }

    /// Create a system notification linked to a thread.
    pub fn system_for_thread(thread_id: ThreadId, content: impl Into<String>) -> Self {
        Self {
            id: EntryId::new(),
            sender: EntrySender::System,
            content: content.into(),
            origin_thread_id: Some(thread_id),
            timestamp: Utc::now(),
            metadata: serde_json::Value::Null,
        }
    }
}

/// Maximum steer notes retained for the next foreground turn.
pub const MAX_PENDING_STEER_NOTES: usize = 4;

/// Maximum entries retained per conversation. Every new foreground thread is
/// seeded with the conversation's prior user/agent entries, so an unbounded
/// surface grows the prompt of every later turn until the model degrades.
/// Older entries are evicted FIFO on `add_entry`.
pub const MAX_CONVERSATION_ENTRIES: usize = 400;

/// Maximum total entry content retained per conversation, in bytes (about
/// 24k tokens at the orchestrator's four-chars-per-token estimate). Evicted
/// FIFO together with [`MAX_CONVERSATION_ENTRIES`]; the newest entry is
/// always kept.
pub const MAX_CONVERSATION_HISTORY_BYTES: usize = 96_000;

/// A conversation surface — the UI-facing view of a chat.
///
/// Conversations are NOT execution boundaries. They are streams of entries
/// that may come from multiple concurrent threads. A user can start a new
/// thread while another is still running, and both produce entries that
/// appear in the same conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSurface {
    pub id: ConversationId,
    /// Which channel this conversation is on (e.g. "xmpp", "web", "cli").
    pub channel: String,
    /// The user who owns this conversation.
    pub user_id: String,
    /// All entries in chronological order.
    pub entries: Vec<ConversationEntry>,
    /// Currently active (non-terminal) thread IDs.
    pub active_threads: Vec<ThreadId>,
    /// Steer notes waiting to be injected when the next foreground turn starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_steer_notes: Vec<String>,
    /// Metadata (channel-specific state, external thread IDs, etc.).
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ConversationSurface {
    pub fn new(channel: impl Into<String>, user_id: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: ConversationId::new(),
            channel: channel.into(),
            user_id: user_id.into(),
            entries: Vec::new(),
            active_threads: Vec::new(),
            pending_steer_notes: Vec::new(),
            metadata: serde_json::Value::Null,
            created_at: now,
            updated_at: now,
        }
    }

    /// Add an entry, evict the oldest entries past the retention bounds, and
    /// update the timestamp.
    pub fn add_entry(&mut self, entry: ConversationEntry) {
        self.entries.push(entry);
        self.evict_overflow();
        self.updated_at = Utc::now();
    }

    /// Drop the oldest entries until the surface fits
    /// [`MAX_CONVERSATION_ENTRIES`] and [`MAX_CONVERSATION_HISTORY_BYTES`].
    /// The newest entry is always kept, even when it alone exceeds the byte
    /// bound.
    fn evict_overflow(&mut self) {
        let mut bytes: usize = self.entries.iter().map(|e| e.content.len()).sum();
        let mut evict = 0;
        while self.entries.len() - evict > 1
            && (self.entries.len() - evict > MAX_CONVERSATION_ENTRIES
                || bytes > MAX_CONVERSATION_HISTORY_BYTES)
        {
            bytes -= self.entries[evict].content.len();
            evict += 1;
        }
        if evict > 0 {
            self.entries.drain(..evict);
        }
    }

    /// Register a thread as active in this conversation.
    pub fn track_thread(&mut self, thread_id: ThreadId) {
        if !self.active_threads.contains(&thread_id) {
            self.active_threads.push(thread_id);
        }
    }

    /// Remove a thread from the active list (it completed or failed).
    pub fn untrack_thread(&mut self, thread_id: ThreadId) {
        self.active_threads.retain(|id| *id != thread_id);
    }

    /// Queue a steer note for the next foreground turn.
    pub fn queue_steer_note(&mut self, note: String) -> bool {
        if self.pending_steer_notes.len() >= MAX_PENDING_STEER_NOTES {
            return false;
        }
        self.pending_steer_notes.push(note);
        self.updated_at = Utc::now();
        true
    }

    /// Take all steer notes queued for the next foreground turn.
    pub fn take_steer_notes(&mut self) -> Vec<String> {
        let notes = std::mem::take(&mut self.pending_steer_notes);
        if !notes.is_empty() {
            self.updated_at = Utc::now();
        }
        notes
    }

    /// Get the most recent entry, if any.
    pub fn last_entry(&self) -> Option<&ConversationEntry> {
        self.entries.last()
    }

    /// Get all entries from a specific thread.
    pub fn entries_for_thread(&self, thread_id: ThreadId) -> Vec<&ConversationEntry> {
        self.entries
            .iter()
            .filter(|e| e.origin_thread_id == Some(thread_id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_lifecycle() {
        let mut conv = ConversationSurface::new("xmpp", "user_123");
        assert!(conv.entries.is_empty());
        assert!(conv.active_threads.is_empty());

        // User sends a message
        conv.add_entry(ConversationEntry::user("Hello!"));
        assert_eq!(conv.entries.len(), 1);

        // Thread starts
        let tid = ThreadId::new();
        conv.track_thread(tid);
        conv.add_entry(ConversationEntry::system_for_thread(tid, "Thread started"));
        assert_eq!(conv.active_threads.len(), 1);

        // Agent responds
        conv.add_entry(ConversationEntry::agent(tid, "Hi there!"));
        assert_eq!(conv.entries.len(), 3);

        // Thread completes
        conv.untrack_thread(tid);
        conv.add_entry(ConversationEntry::system_for_thread(
            tid,
            "Thread completed",
        ));
        assert!(conv.active_threads.is_empty());
        assert_eq!(conv.entries.len(), 4);
    }

    #[test]
    fn multiple_concurrent_threads() {
        let mut conv = ConversationSurface::new("web", "user_456");

        let t1 = ThreadId::new();
        let t2 = ThreadId::new();

        conv.track_thread(t1);
        conv.track_thread(t2);
        assert_eq!(conv.active_threads.len(), 2);

        conv.add_entry(ConversationEntry::agent(t1, "Research result A"));
        conv.add_entry(ConversationEntry::agent(t2, "Research result B"));
        conv.add_entry(ConversationEntry::agent(t1, "More from A"));

        let t1_entries = conv.entries_for_thread(t1);
        assert_eq!(t1_entries.len(), 2);

        let t2_entries = conv.entries_for_thread(t2);
        assert_eq!(t2_entries.len(), 1);

        conv.untrack_thread(t1);
        assert_eq!(conv.active_threads.len(), 1);
    }

    #[test]
    fn track_thread_is_idempotent() {
        let mut conv = ConversationSurface::new("cli", "user");
        let tid = ThreadId::new();
        conv.track_thread(tid);
        conv.track_thread(tid);
        assert_eq!(conv.active_threads.len(), 1);
    }

    #[test]
    fn add_entry_evicts_the_oldest_entries_beyond_the_entry_cap() {
        let mut conv = ConversationSurface::new("xmpp", "user");
        for index in 0..MAX_CONVERSATION_ENTRIES + 5 {
            conv.add_entry(ConversationEntry::user(format!("message {index}")));
        }
        assert_eq!(conv.entries.len(), MAX_CONVERSATION_ENTRIES);
        assert_eq!(conv.entries[0].content, "message 5");
        let newest = format!("message {}", MAX_CONVERSATION_ENTRIES + 4);
        assert_eq!(
            conv.last_entry().map(|e| e.content.as_str()),
            Some(newest.as_str())
        );
    }

    #[test]
    fn add_entry_evicts_the_oldest_entries_beyond_the_byte_budget() {
        let mut conv = ConversationSurface::new("xmpp", "user");
        let chunk = "x".repeat(10_000);
        let count = MAX_CONVERSATION_HISTORY_BYTES / chunk.len() + 3;
        for index in 0..count {
            conv.add_entry(ConversationEntry::agent(
                ThreadId::new(),
                format!("{index}:{chunk}"),
            ));
        }
        let bytes: usize = conv.entries.iter().map(|e| e.content.len()).sum();
        assert!(
            bytes <= MAX_CONVERSATION_HISTORY_BYTES,
            "retained {bytes} bytes"
        );
        assert!(conv.entries.len() < count);
        let oldest_retained = format!("{}:", count - conv.entries.len());
        assert!(conv.entries[0].content.starts_with(&oldest_retained));
        let newest = format!("{}:", count - 1);
        assert!(
            conv.last_entry()
                .is_some_and(|e| e.content.starts_with(&newest))
        );
    }

    #[test]
    fn add_entry_keeps_the_newest_entry_even_when_it_exceeds_the_byte_budget() {
        let mut conv = ConversationSurface::new("xmpp", "user");
        conv.add_entry(ConversationEntry::user("earlier"));
        conv.add_entry(ConversationEntry::user(
            "y".repeat(MAX_CONVERSATION_HISTORY_BYTES + 1),
        ));
        assert_eq!(conv.entries.len(), 1);
        assert_eq!(
            conv.entries[0].content.len(),
            MAX_CONVERSATION_HISTORY_BYTES + 1
        );
    }

    #[test]
    fn pending_steer_notes_are_bounded_and_drained_in_order() {
        let mut conv = ConversationSurface::new("xmpp", "user");
        for index in 0..MAX_PENDING_STEER_NOTES {
            assert!(conv.queue_steer_note(format!("note {index}")));
        }
        assert!(!conv.queue_steer_note("overflow".to_string()));

        assert_eq!(
            conv.take_steer_notes(),
            vec!["note 0", "note 1", "note 2", "note 3"]
        );
        assert!(conv.pending_steer_notes.is_empty());
    }

    #[test]
    fn empty_pending_steer_queue_is_backward_compatible() {
        let conv = ConversationSurface::new("xmpp", "user");
        let serialized = serde_json::to_value(&conv).expect("conversation should serialize");
        assert!(serialized.get("pending_steer_notes").is_none());

        let restored: ConversationSurface =
            serde_json::from_value(serialized).expect("legacy conversation should deserialize");
        assert!(restored.pending_steer_notes.is_empty());
    }
}
