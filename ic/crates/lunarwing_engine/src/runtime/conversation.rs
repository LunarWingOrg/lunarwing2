//! Conversation manager — routes UI messages to threads.
//!
//! The ConversationManager is the bridge between channel I/O (user messages,
//! status updates) and the thread execution model. It maintains conversation
//! surfaces and decides whether to spawn new threads or inject messages into
//! existing ones.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::runtime::manager::ThreadManager;
use crate::runtime::messaging::ThreadOutcome;
use crate::traits::store::Store;
use crate::types::conversation::{ConversationEntry, ConversationId, ConversationSurface};
use crate::types::error::EngineError;
use crate::types::message::{ThreadMessage, TransientContentPart};
use crate::types::project::ProjectId;
use crate::types::thread::{ThreadConfig, ThreadId, ThreadState, ThreadType};

enum ActiveForeground {
    Running(ThreadId),
    Waiting(ThreadId),
    Resumable(ThreadId),
}

/// Manages conversation surfaces and routes messages to threads.
///
/// Each channel message arrives here. The manager decides whether to:
/// 1. Spawn a new foreground thread for the message
/// 2. Inject the message into an existing active thread
/// 3. Create a new conversation if none exists for this channel+user
pub struct ConversationManager {
    thread_manager: Arc<ThreadManager>,
    store: Arc<dyn Store>,
    conversations: RwLock<HashMap<ConversationId, ConversationSurface>>,
    /// Maps (channel, user_id) → conversation ID for lookup.
    channel_user_index: RwLock<HashMap<(String, String), ConversationId>>,
}

impl ConversationManager {
    pub fn new(thread_manager: Arc<ThreadManager>, store: Arc<dyn Store>) -> Self {
        Self {
            thread_manager,
            store,
            conversations: RwLock::new(HashMap::new()),
            channel_user_index: RwLock::new(HashMap::new()),
        }
    }

    /// Restore persisted conversations for a user into the in-memory index.
    pub async fn bootstrap_user(&self, user_id: &str) -> Result<usize, EngineError> {
        let conversations = self.store.list_conversations(user_id).await?;
        let count = conversations.len();
        let mut convs = self.conversations.write().await;
        let mut index = self.channel_user_index.write().await;

        for conversation in conversations {
            if index.contains_key(&(conversation.channel.clone(), conversation.user_id.clone())) {
                continue;
            }
            index.insert(
                (conversation.channel.clone(), conversation.user_id.clone()),
                conversation.id,
            );
            convs.insert(conversation.id, conversation);
        }

        Ok(count)
    }

    /// Get or create a conversation for a channel+user pair.
    pub async fn get_or_create_conversation(
        &self,
        channel: &str,
        user_id: &str,
    ) -> Result<ConversationId, EngineError> {
        let key = (channel.to_string(), user_id.to_string());
        if let Some(conv) = self.find_conversation(channel, user_id).await? {
            return Ok(conv.id);
        }

        // Create new conversation. The scoped `channel:scope` keys handed over
        // by the bridge should already have a surface (bootstrap or a prior
        // turn); creating on a scoped miss means earlier turns on that scope
        // are unreachable, so surface it loudly instead of failing silently.
        let conv = ConversationSurface::new(channel, user_id);
        let conv_id = conv.id;

        let mut convs = self.conversations.write().await;
        let mut index = self.channel_user_index.write().await;
        // Re-check under the write lock: two concurrent first messages on the
        // same key would otherwise each create a surface and orphan one,
        // losing its history while reads keep succeeding.
        if let Some(&existing) = index.get(&key) {
            return Ok(existing);
        }
        self.store.save_conversation(&conv).await?;
        convs.insert(conv_id, conv);
        index.insert(key, conv_id);

        if channel.contains(':') {
            warn!(
                conversation_id = %conv_id,
                channel,
                user_id,
                "created conversation for scoped key on miss; prior turns on \
                 this scope are not linked to the new surface"
            );
        }
        debug!(conversation_id = %conv_id, channel, user_id, "created conversation");
        Ok(conv_id)
    }

    /// Find and hydrate an existing surface without creating one. Read and
    /// create paths share this lookup so approvals survive a cold memory cache.
    pub async fn find_conversation(
        &self,
        channel: &str,
        user_id: &str,
    ) -> Result<Option<ConversationSurface>, EngineError> {
        let key = (channel.to_string(), user_id.to_string());
        {
            // Every path taking both locks uses conversations -> index.
            let convs = self.conversations.read().await;
            let index = self.channel_user_index.read().await;
            if let Some(conv) = index.get(&key).and_then(|id| convs.get(id)) {
                return Ok(Some(conv.clone()));
            }
        }

        let persisted = self.store.list_conversations(user_id).await?;
        let mut convs = self.conversations.write().await;
        let mut index = self.channel_user_index.write().await;
        // A concurrent create/bootstrap/lookup may have hydrated and updated
        // the surface during I/O. Never overwrite it with the older snapshot.
        if let Some(conv) = index.get(&key).and_then(|id| convs.get(id)) {
            return Ok(Some(conv.clone()));
        }
        let Some(conv) = persisted.into_iter().find(|conv| conv.channel == channel) else {
            return Ok(None);
        };
        index.insert(key, conv.id);
        convs.insert(conv.id, conv.clone());
        Ok(Some(conv))
    }

    /// Persist a steer note until the conversation's next foreground turn.
    ///
    /// Returns `false` when the bounded pending-note queue is full.
    pub async fn queue_steer_note(
        &self,
        conversation_id: ConversationId,
        user_id: &str,
        note: String,
    ) -> Result<bool, EngineError> {
        let mut convs = self.conversations.write().await;
        let conv = convs.get_mut(&conversation_id).ok_or(EngineError::Store {
            reason: format!("conversation {conversation_id} not found"),
        })?;
        if conv.user_id != user_id {
            return Err(EngineError::AccessDenied {
                user_id: user_id.to_string(),
                entity: format!("conversation {conversation_id}"),
            });
        }

        let previous = conv.clone();
        if !conv.queue_steer_note(note) {
            return Ok(false);
        }
        if let Err(error) = self.store.save_conversation(conv).await {
            *conv = previous;
            return Err(error);
        }
        Ok(true)
    }

    /// Drain queued steer notes onto `thread_id` before resume or injection.
    pub async fn drain_pending_steer_notes(
        &self,
        conversation_id: ConversationId,
        user_id: &str,
        thread_id: ThreadId,
    ) -> Result<(), EngineError> {
        let mut convs = self.conversations.write().await;
        let conv = convs.get_mut(&conversation_id).ok_or(EngineError::Store {
            reason: format!("conversation {conversation_id} not found"),
        })?;
        if conv.user_id != user_id {
            return Err(EngineError::AccessDenied {
                user_id: user_id.to_string(),
                entity: format!("conversation {conversation_id}"),
            });
        }
        let running = self.thread_manager.is_running(thread_id).await;
        self.deliver_pending_steer_notes(conv, thread_id, user_id, running)
            .await?;
        self.store.save_conversation(conv).await?;
        Ok(())
    }

    async fn deliver_pending_steer_notes(
        &self,
        conv: &mut ConversationSurface,
        thread_id: ThreadId,
        user_id: &str,
        running: bool,
    ) -> Result<(), EngineError> {
        if conv.pending_steer_notes.is_empty() {
            return Ok(());
        }
        if running {
            self.inject_pending_steer_notes(conv, thread_id, user_id)
                .await
        } else {
            self.prepend_pending_steer_notes(conv, thread_id, user_id)
                .await
        }
    }

    async fn inject_pending_steer_notes(
        &self,
        conv: &mut ConversationSurface,
        thread_id: ThreadId,
        user_id: &str,
    ) -> Result<(), EngineError> {
        let notes = conv.take_steer_notes();
        for (index, note) in notes.iter().enumerate() {
            if let Err(error) = self
                .thread_manager
                .inject_message(thread_id, user_id, ThreadMessage::generated_goal(note))
                .await
            {
                restore_steer_notes(conv, notes.into_iter().skip(index));
                return Err(error);
            }
        }
        Ok(())
    }

    async fn prepend_pending_steer_notes(
        &self,
        conv: &mut ConversationSurface,
        thread_id: ThreadId,
        user_id: &str,
    ) -> Result<(), EngineError> {
        let notes = conv.take_steer_notes();
        if notes.is_empty() {
            return Ok(());
        }
        if let Err(error) = self
            .write_steer_notes_to_thread(thread_id, user_id, &notes)
            .await
        {
            restore_steer_notes(conv, notes);
            return Err(error);
        }
        Ok(())
    }

    async fn write_steer_notes_to_thread(
        &self,
        thread_id: ThreadId,
        user_id: &str,
        notes: &[String],
    ) -> Result<(), EngineError> {
        let mut thread = self
            .store
            .load_thread(thread_id)
            .await?
            .ok_or(EngineError::ThreadNotFound(thread_id))?;
        if !thread.is_owned_by(user_id) {
            return Err(EngineError::AccessDenied {
                user_id: user_id.to_string(),
                entity: format!("thread {thread_id}"),
            });
        }
        for note in notes {
            let message = ThreadMessage::generated_goal(note.clone());
            if !thread.internal_messages.is_empty() {
                thread.add_internal_message(message.clone());
            }
            thread.add_message(message);
        }
        self.store.save_thread(&thread).await
    }

    /// Handle an incoming user message.
    ///
    /// If the conversation has an active foreground thread, the message is
    /// injected into it. Otherwise, a new foreground thread is spawned.
    ///
    /// Returns the thread ID that is handling the message.
    pub async fn handle_user_message(
        &self,
        conversation_id: ConversationId,
        content: &str,
        project_id: ProjectId,
        user_id: &str,
        thread_config: ThreadConfig,
        preferred_thread_id: Option<ThreadId>,
    ) -> Result<ThreadId, EngineError> {
        self.handle_user_thread_message(
            conversation_id,
            ThreadMessage::user(content),
            project_id,
            user_id,
            thread_config,
            preferred_thread_id,
        )
        .await
    }

    /// Handle user text with provider-bound multimodal content.
    #[allow(clippy::too_many_arguments)]
    pub async fn handle_user_message_with_parts(
        &self,
        conversation_id: ConversationId,
        content: &str,
        transient_content_parts: Vec<TransientContentPart>,
        project_id: ProjectId,
        user_id: &str,
        thread_config: ThreadConfig,
        preferred_thread_id: Option<ThreadId>,
    ) -> Result<ThreadId, EngineError> {
        self.handle_user_thread_message(
            conversation_id,
            ThreadMessage::user_with_transient_parts(content, transient_content_parts),
            project_id,
            user_id,
            thread_config,
            preferred_thread_id,
        )
        .await
    }

    async fn handle_user_thread_message(
        &self,
        conversation_id: ConversationId,
        user_message: ThreadMessage,
        project_id: ProjectId,
        user_id: &str,
        mut thread_config: ThreadConfig,
        preferred_thread_id: Option<ThreadId>,
    ) -> Result<ThreadId, EngineError> {
        thread_config.conversation_id = Some(conversation_id);
        let mut convs = self.conversations.write().await;
        let conv = convs.get_mut(&conversation_id).ok_or(EngineError::Store {
            reason: format!("conversation {conversation_id} not found"),
        })?;

        // Tenant isolation: verify the requesting user owns this conversation.
        if conv.user_id != user_id {
            return Err(EngineError::AccessDenied {
                user_id: user_id.to_string(),
                entity: format!("conversation {conversation_id}"),
            });
        }

        // Check for an active foreground thread
        let active_foreground = self.find_active_foreground(conv).await;

        match active_foreground {
            Some(ActiveForeground::Running(thread_id)) => {
                conv.add_entry(ConversationEntry::user(&user_message.content));
                debug!(
                    conversation_id = %conversation_id,
                    thread_id = %thread_id,
                    "injecting message into active thread"
                );
                self.deliver_pending_steer_notes(conv, thread_id, user_id, true)
                    .await?;
                self.thread_manager
                    .inject_message(thread_id, user_id, user_message)
                    .await?;
                self.store.save_conversation(conv).await?;
                Ok(thread_id)
            }
            Some(ActiveForeground::Waiting(thread_id)) => Err(EngineError::Thread(
                crate::types::error::ThreadError::Waiting(thread_id),
            )),
            Some(ActiveForeground::Resumable(thread_id)) => {
                conv.add_entry(ConversationEntry::user(&user_message.content));
                debug!(
                    conversation_id = %conversation_id,
                    thread_id = %thread_id,
                    "resuming suspended foreground thread"
                );
                self.deliver_pending_steer_notes(conv, thread_id, user_id, false)
                    .await?;
                self.thread_manager
                    .resume_thread(thread_id, user_id, Some(user_message), None, None)
                    .await?;
                conv.add_entry(ConversationEntry::system_for_thread(
                    thread_id,
                    "Thread resumed",
                ));
                self.store.save_conversation(conv).await?;
                Ok(thread_id)
            }
            None => {
                conv.add_entry(ConversationEntry::user(&user_message.content));
                // Build conversation history from prior entries, followed by any
                // steer notes that arrived while no foreground thread was running.
                let mut history = build_history_from_entries(&conv.entries);
                history.extend(
                    conv.pending_steer_notes
                        .iter()
                        .map(ThreadMessage::generated_goal),
                );

                // Spawn new foreground thread with conversation history
                let thread_id = self
                    .thread_manager
                    .spawn_thread_with_history_and_message(
                        user_message,
                        ThreadType::Foreground,
                        project_id,
                        thread_config,
                        None,
                        user_id,
                        history,
                        preferred_thread_id,
                        None,
                    )
                    .await?;

                // Store the base channel name in thread metadata so the
                // orchestrator can populate `source_channel` in the execution
                // context (used by mission_create to default notify_channels).
                let base_channel = conv
                    .channel
                    .split(':')
                    .next()
                    .unwrap_or(&conv.channel)
                    .to_string();
                self.thread_manager
                    .set_thread_metadata(thread_id, "source_channel", &base_channel)
                    .await;

                conv.take_steer_notes();
                conv.track_thread(thread_id);
                conv.add_entry(ConversationEntry::system_for_thread(
                    thread_id,
                    "Thread started",
                ));
                self.store.save_conversation(conv).await?;

                debug!(
                    conversation_id = %conversation_id,
                    thread_id = %thread_id,
                    "spawned new foreground thread"
                );
                Ok(thread_id)
            }
        }
    }

    /// Untrack a thread from every conversation's active set.
    ///
    /// Reconciliation paths (gate expiry/orphan finalization) finalize a
    /// thread without a conversation id at hand; without this the thread
    /// stays "active" in conversation state forever.
    pub async fn untrack_thread_everywhere(&self, thread_id: ThreadId) {
        let mut convs = self.conversations.write().await;
        for conv in convs.values_mut() {
            conv.untrack_thread(thread_id);
        }
    }

    /// Untrack a thread from its known conversation without scanning others.
    pub async fn untrack_thread(&self, conversation_id: ConversationId, thread_id: ThreadId) {
        if let Some(conv) = self.conversations.write().await.get_mut(&conversation_id) {
            conv.untrack_thread(thread_id);
        }
    }

    /// Record a thread's outcome in its conversation.
    pub async fn record_thread_outcome(
        &self,
        conversation_id: ConversationId,
        thread_id: ThreadId,
        outcome: &ThreadOutcome,
    ) -> Result<(), EngineError> {
        let mut convs = self.conversations.write().await;
        if let Some(conv) = convs.get_mut(&conversation_id) {
            match outcome {
                ThreadOutcome::Completed { response } => {
                    if let Some(text) = response
                        && !text.is_empty()
                    {
                        conv.add_entry(ConversationEntry::agent(thread_id, text));
                    }
                    conv.untrack_thread(thread_id);
                }
                ThreadOutcome::Stopped => {
                    conv.add_entry(ConversationEntry::system_for_thread(
                        thread_id,
                        "Thread stopped",
                    ));
                    conv.untrack_thread(thread_id);
                }
                ThreadOutcome::MaxIterations => {
                    conv.add_entry(ConversationEntry::system_for_thread(
                        thread_id,
                        "Thread reached max iterations after final recovery; retry with narrower scope or a higher iteration limit",
                    ));
                    conv.untrack_thread(thread_id);
                }
                ThreadOutcome::Failed { error, .. } => {
                    conv.add_entry(ConversationEntry::system_for_thread(
                        thread_id,
                        format!("Thread failed: {error}"),
                    ));
                    conv.untrack_thread(thread_id);
                }
                ThreadOutcome::GatePaused {
                    gate_name,
                    action_name,
                    ..
                } => {
                    conv.add_entry(ConversationEntry::system_for_thread(
                        thread_id,
                        format!("Gate '{gate_name}' paused execution of action: {action_name}"),
                    ));
                    // Thread stays active — waiting for gate resolution
                }
            }
            self.store.save_conversation(conv).await?;
        }
        Ok(())
    }

    /// Clear a conversation's entries, active threads, and pending steer notes.
    ///
    /// Stops tracking all threads and removes conversation history and controls
    /// so the next user message spawns a fresh thread with no prior context.
    pub async fn clear_conversation(
        &self,
        conversation_id: ConversationId,
        user_id: &str,
    ) -> Result<(), EngineError> {
        let mut convs = self.conversations.write().await;
        if let Some(conv) = convs.get_mut(&conversation_id) {
            // Tenant isolation: verify ownership.
            if conv.user_id != user_id {
                return Err(EngineError::AccessDenied {
                    user_id: user_id.to_string(),
                    entity: format!("conversation {conversation_id}"),
                });
            }
            conv.active_threads.clear();
            conv.entries.clear();
            conv.pending_steer_notes.clear();
            conv.updated_at = chrono::Utc::now();
            self.store.save_conversation(conv).await?;
            debug!(conversation_id = %conversation_id, "cleared conversation");
        }
        Ok(())
    }

    /// Get a snapshot of a conversation.
    pub async fn get_conversation(
        &self,
        conversation_id: ConversationId,
    ) -> Option<ConversationSurface> {
        let convs = self.conversations.read().await;
        convs.get(&conversation_id).cloned()
    }

    /// List all conversations for a user.
    pub async fn list_conversations(&self, user_id: &str) -> Vec<ConversationSurface> {
        let convs = self.conversations.read().await;
        convs
            .values()
            .filter(|c| c.user_id == user_id)
            .cloned()
            .collect()
    }

    /// Find an active foreground thread in a conversation.
    async fn find_active_foreground(&self, conv: &ConversationSurface) -> Option<ActiveForeground> {
        for &tid in &conv.active_threads {
            if self.thread_manager.is_running(tid).await {
                return Some(ActiveForeground::Running(tid));
            }
            if let Ok(Some(thread)) = self.store.load_thread(tid).await
                && thread.thread_type == ThreadType::Foreground
            {
                match thread.state {
                    ThreadState::Waiting => return Some(ActiveForeground::Waiting(tid)),
                    ThreadState::Suspended => return Some(ActiveForeground::Resumable(tid)),
                    _ => {}
                }
            }
        }
        None
    }
}

fn restore_steer_notes(conv: &mut ConversationSurface, notes: impl IntoIterator<Item = String>) {
    for note in notes {
        conv.queue_steer_note(note);
    }
}

/// Build ThreadMessage history from conversation entries.
///
/// Converts user and agent entries into ThreadMessages so a new thread
/// inherits context from prior turns in the same conversation.
fn build_history_from_entries(
    entries: &[ConversationEntry],
) -> Vec<crate::types::message::ThreadMessage> {
    use crate::types::conversation::EntrySender;

    // Skip the last entry (it's the current user message, added by the caller
    // before this function runs). Also skip system entries (thread lifecycle
    // notifications aren't useful as LLM context).
    let history_entries = if entries.len() > 1 {
        &entries[..entries.len() - 1]
    } else {
        return Vec::new();
    };

    history_entries
        .iter()
        .filter_map(|entry| match &entry.sender {
            EntrySender::User => Some(crate::types::message::ThreadMessage::user(&entry.content)),
            EntrySender::Agent { .. } => Some(crate::types::message::ThreadMessage::assistant(
                &entry.content,
            )),
            EntrySender::System => None, // skip system notifications
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::lease::LeaseManager;
    use crate::capability::policy::PolicyEngine;
    use crate::capability::registry::CapabilityRegistry;
    use crate::traits::effect::EffectExecutor;
    use crate::traits::llm::{LlmBackend, LlmCallConfig, LlmOutput};
    use crate::traits::store::Store;
    use crate::types::capability::{ActionDef, CapabilityLease};
    use crate::types::conversation::{ConversationId, ConversationSurface, EntrySender};
    use crate::types::event::ThreadEvent;
    use crate::types::memory::{DocId, MemoryDoc};
    use crate::types::project::Project;
    use crate::types::step::{ActionResult, LlmResponse, Step, TokenUsage};
    use crate::types::thread::ThreadState;
    use std::sync::Mutex;
    use std::time::Duration;

    // ── Mocks (same as manager tests) ───────────────────────

    struct MockLlm(Mutex<Vec<LlmOutput>>);

    #[async_trait::async_trait]
    impl LlmBackend for MockLlm {
        async fn complete(
            &self,
            _: &[ThreadMessage],
            _: &[ActionDef],
            _: &LlmCallConfig,
        ) -> Result<LlmOutput, EngineError> {
            let mut r = self.0.lock().unwrap();
            if r.is_empty() {
                Ok(LlmOutput {
                    response: LlmResponse::Text("done".into()),
                    usage: TokenUsage::default(),
                    finish_reason: "stop".to_string(),
                })
            } else {
                Ok(r.remove(0))
            }
        }
        fn model_name(&self) -> &str {
            "mock"
        }
    }

    struct MockEffects;

    #[async_trait::async_trait]
    impl EffectExecutor for MockEffects {
        async fn execute_action(
            &self,
            _: &str,
            _: serde_json::Value,
            _: &CapabilityLease,
            _: &crate::traits::effect::ThreadExecutionContext,
        ) -> Result<ActionResult, EngineError> {
            Ok(ActionResult {
                call_id: String::new(),
                action_name: String::new(),
                output: serde_json::json!({}),
                is_error: false,
                duration: Duration::from_millis(1),
            })
        }
        async fn available_actions(
            &self,
            _: &[CapabilityLease],
        ) -> Result<Vec<ActionDef>, EngineError> {
            Ok(vec![])
        }
    }

    struct MockStore {
        conversations: RwLock<HashMap<ConversationId, ConversationSurface>>,
        threads: RwLock<HashMap<ThreadId, crate::types::thread::Thread>>,
    }

    impl MockStore {
        fn new() -> Self {
            Self {
                conversations: RwLock::new(HashMap::new()),
                threads: RwLock::new(HashMap::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl Store for MockStore {
        async fn save_thread(
            &self,
            thread: &crate::types::thread::Thread,
        ) -> Result<(), EngineError> {
            self.threads.write().await.insert(thread.id, thread.clone());
            Ok(())
        }
        async fn load_thread(
            &self,
            id: ThreadId,
        ) -> Result<Option<crate::types::thread::Thread>, EngineError> {
            Ok(self.threads.read().await.get(&id).cloned())
        }
        async fn list_threads(
            &self,
            project_id: ProjectId,
            _user_id: &str,
        ) -> Result<Vec<crate::types::thread::Thread>, EngineError> {
            Ok(self
                .threads
                .read()
                .await
                .values()
                .filter(|thread| thread.project_id == project_id)
                .cloned()
                .collect())
        }
        async fn update_thread_state(
            &self,
            _: ThreadId,
            _: ThreadState,
        ) -> Result<(), EngineError> {
            Ok(())
        }
        async fn save_step(&self, _: &Step) -> Result<(), EngineError> {
            Ok(())
        }
        async fn load_steps(&self, _: ThreadId) -> Result<Vec<Step>, EngineError> {
            Ok(vec![])
        }
        async fn append_events(&self, _: &[ThreadEvent]) -> Result<(), EngineError> {
            Ok(())
        }
        async fn load_events(&self, _: ThreadId) -> Result<Vec<ThreadEvent>, EngineError> {
            Ok(vec![])
        }
        async fn save_project(&self, _: &Project) -> Result<(), EngineError> {
            Ok(())
        }
        async fn load_project(&self, _: ProjectId) -> Result<Option<Project>, EngineError> {
            Ok(None)
        }
        async fn save_conversation(
            &self,
            conversation: &ConversationSurface,
        ) -> Result<(), EngineError> {
            self.conversations
                .write()
                .await
                .insert(conversation.id, conversation.clone());
            Ok(())
        }
        async fn load_conversation(
            &self,
            id: ConversationId,
        ) -> Result<Option<ConversationSurface>, EngineError> {
            Ok(self.conversations.read().await.get(&id).cloned())
        }
        async fn list_conversations(
            &self,
            user_id: &str,
        ) -> Result<Vec<ConversationSurface>, EngineError> {
            Ok(self
                .conversations
                .read()
                .await
                .values()
                .filter(|conversation| conversation.user_id == user_id)
                .cloned()
                .collect())
        }
        async fn save_memory_doc(&self, _: &MemoryDoc) -> Result<(), EngineError> {
            Ok(())
        }
        async fn load_memory_doc(&self, _: DocId) -> Result<Option<MemoryDoc>, EngineError> {
            Ok(None)
        }
        async fn list_memory_docs(
            &self,
            _: ProjectId,
            _: &str,
        ) -> Result<Vec<MemoryDoc>, EngineError> {
            Ok(vec![])
        }
        async fn save_lease(&self, _: &CapabilityLease) -> Result<(), EngineError> {
            Ok(())
        }
        async fn load_active_leases(
            &self,
            _: ThreadId,
        ) -> Result<Vec<CapabilityLease>, EngineError> {
            Ok(vec![])
        }
        async fn revoke_lease(
            &self,
            _: crate::types::capability::LeaseId,
            _: &str,
        ) -> Result<(), EngineError> {
            Ok(())
        }
        async fn save_mission(
            &self,
            _: &crate::types::mission::Mission,
        ) -> Result<(), EngineError> {
            Ok(())
        }
        async fn load_mission(
            &self,
            _: crate::types::mission::MissionId,
        ) -> Result<Option<crate::types::mission::Mission>, EngineError> {
            Ok(None)
        }
        async fn list_missions(
            &self,
            _: ProjectId,
            _: &str,
        ) -> Result<Vec<crate::types::mission::Mission>, EngineError> {
            Ok(vec![])
        }
        async fn update_mission_status(
            &self,
            _: crate::types::mission::MissionId,
            _: crate::types::mission::MissionStatus,
        ) -> Result<(), EngineError> {
            Ok(())
        }
    }

    fn make_conv_manager() -> (Arc<ThreadManager>, ConversationManager) {
        let store = Arc::new(MockStore::new());
        let tm = Arc::new(ThreadManager::new(
            Arc::new(MockLlm(Mutex::new(vec![LlmOutput {
                response: LlmResponse::Text("Hello!".into()),
                usage: TokenUsage::default(),
                finish_reason: "stop".to_string(),
            }]))),
            Arc::new(MockEffects),
            store.clone(),
            Arc::new(CapabilityRegistry::new()),
            Arc::new(LeaseManager::new()),
            Arc::new(PolicyEngine::new()),
        ));
        let cm = ConversationManager::new(Arc::clone(&tm), store);
        (tm, cm)
    }

    // ── Tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn get_or_create_conversation() {
        let (_, cm) = make_conv_manager();
        let c1 = cm
            .get_or_create_conversation("xmpp", "user1")
            .await
            .unwrap();
        let c2 = cm
            .get_or_create_conversation("xmpp", "user1")
            .await
            .unwrap();
        assert_eq!(c1, c2); // same channel+user returns same conversation

        let c3 = cm
            .get_or_create_conversation("slack", "user1")
            .await
            .unwrap();
        assert_ne!(c1, c3); // different channel → different conversation
    }

    #[tokio::test]
    async fn handle_message_spawns_thread() {
        let (tm, cm) = make_conv_manager();
        let conv_id = cm.get_or_create_conversation("web", "user1").await.unwrap();
        let project = ProjectId::new();

        let tid = cm
            .handle_user_message(
                conv_id,
                "Hello",
                project,
                "user1",
                ThreadConfig::default(),
                None,
            )
            .await
            .unwrap();

        // Thread was spawned
        let conv = cm.get_conversation(conv_id).await.unwrap();
        assert!(conv.active_threads.contains(&tid));
        assert_eq!(conv.entries.len(), 2); // user message + "Thread started"

        // Wait for thread to complete
        let outcome = tm.join_thread(tid).await.unwrap();
        assert!(matches!(outcome, ThreadOutcome::Completed { .. }));
    }

    #[tokio::test]
    async fn find_conversation_recovers_persisted_scope_without_creating() {
        let (_, cm) = make_conv_manager();
        let original = cm
            .get_or_create_conversation("gateway:thread-a", "alice")
            .await
            .unwrap();
        cm.conversations.write().await.clear();
        cm.channel_user_index.write().await.clear();
        let found = cm
            .find_conversation("gateway:thread-a", "alice")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.id, original);
        assert_eq!(
            cm.get_or_create_conversation("gateway:thread-a", "alice")
                .await
                .unwrap(),
            original
        );
        assert!(
            cm.find_conversation("gateway:thread-a", "bob")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            cm.find_conversation("gateway:other", "alice")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(cm.store.list_conversations("alice").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn find_conversation_does_not_hold_index_while_waiting_for_conversations() {
        let (_, cm) = make_conv_manager();
        let id = cm.get_or_create_conversation("web", "alice").await.unwrap();
        let conversations = cm.conversations.write().await;
        let mut lookup = Box::pin(cm.find_conversation("web", "alice"));
        // Poll the actual lookup into its lock wait, then require a WRITE lock:
        // another read lock would not detect the old read/write inversion.
        assert!(futures::poll!(&mut lookup).is_pending());
        let index = cm
            .channel_user_index
            .try_write()
            .expect("lookup must not retain index");
        drop(index);
        drop(conversations);
        let found = tokio::time::timeout(std::time::Duration::from_secs(1), lookup)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(found.id, id);
    }

    #[tokio::test]
    async fn concurrent_cold_lookups_and_creates_keep_one_surface() {
        let (_, cm) = make_conv_manager();
        let cm = Arc::new(cm);
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..20 {
            let cm = Arc::clone(&cm);
            tasks.spawn(async move {
                cm.find_conversation("web", "alice").await.unwrap();
                cm.get_or_create_conversation("web", "alice").await.unwrap()
            });
        }
        let mut ids = std::collections::HashSet::new();
        while let Some(id) = tasks.join_next().await {
            ids.insert(id.unwrap());
        }
        assert_eq!(ids.len(), 1);
        assert_eq!(cm.store.list_conversations("alice").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn queued_steer_note_is_persisted_and_injected_into_next_thread() {
        let (tm, cm) = make_conv_manager();
        let conv_id = cm
            .get_or_create_conversation("xmpp:alice@example.org", "user1")
            .await
            .unwrap();
        assert!(
            cm.queue_steer_note(conv_id, "user1", "wrapped steer".to_string())
                .await
                .unwrap()
        );
        let queued = cm.store.load_conversation(conv_id).await.unwrap().unwrap();
        assert_eq!(queued.pending_steer_notes, vec!["wrapped steer"]);

        let thread_id = cm
            .handle_user_message(
                conv_id,
                "next request",
                ProjectId::new(),
                "user1",
                ThreadConfig::default(),
                None,
            )
            .await
            .unwrap();
        let _ = tm.join_thread(thread_id).await.unwrap();

        let thread = cm.store.load_thread(thread_id).await.unwrap().unwrap();
        let user_messages: Vec<&str> = thread
            .messages
            .iter()
            .filter(|message| message.role == crate::types::message::MessageRole::User)
            .map(|message| message.content.as_str())
            .collect();
        assert_eq!(user_messages, vec!["wrapped steer", "next request"]);
        assert!(
            cm.store
                .load_conversation(conv_id)
                .await
                .unwrap()
                .unwrap()
                .pending_steer_notes
                .is_empty()
        );
    }

    fn user_message_contents(thread: &crate::types::thread::Thread) -> Vec<&str> {
        thread
            .messages
            .iter()
            .filter(|message| message.role == crate::types::message::MessageRole::User)
            .map(|message| message.content.as_str())
            .collect()
    }

    #[tokio::test]
    async fn queued_steer_note_is_injected_when_resuming_suspended_thread() {
        let store = Arc::new(MockStore::new());
        let tm = Arc::new(ThreadManager::new(
            Arc::new(MockLlm(Mutex::new(vec![LlmOutput {
                response: LlmResponse::Text("Recovered".into()),
                usage: TokenUsage::default(),
                finish_reason: "stop".to_string(),
            }]))),
            Arc::new(MockEffects),
            store.clone(),
            Arc::new(CapabilityRegistry::new()),
            Arc::new(LeaseManager::new()),
            Arc::new(PolicyEngine::new()),
        ));
        let cm = ConversationManager::new(Arc::clone(&tm), store.clone());
        let conv_id = cm.get_or_create_conversation("web", "user1").await.unwrap();
        let project = ProjectId::new();
        let mut thread = crate::types::thread::Thread::new(
            "resume",
            ThreadType::Foreground,
            project,
            "user1",
            ThreadConfig::default(),
        );
        thread.transition_to(ThreadState::Running, None).unwrap();
        thread.add_message(ThreadMessage::user("earlier"));
        thread
            .transition_to(
                ThreadState::Suspended,
                Some("engine restart; resumable from checkpoint".into()),
            )
            .unwrap();
        store.save_thread(&thread).await.unwrap();
        {
            let mut convs = cm.conversations.write().await;
            convs.get_mut(&conv_id).unwrap().track_thread(thread.id);
        }
        assert!(
            cm.queue_steer_note(conv_id, "user1", "wrapped steer".to_string())
                .await
                .unwrap()
        );

        let resumed = cm
            .handle_user_message(
                conv_id,
                "continue from there",
                project,
                "user1",
                ThreadConfig::default(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(resumed, thread.id);
        let _ = tm.join_thread(thread.id).await.unwrap();
        let saved = store.load_thread(thread.id).await.unwrap().unwrap();
        assert_eq!(
            user_message_contents(&saved),
            vec!["earlier", "wrapped steer", "continue from there"]
        );
        assert!(
            cm.get_conversation(conv_id)
                .await
                .unwrap()
                .pending_steer_notes
                .is_empty()
        );
    }

    #[tokio::test]
    async fn drain_pending_steer_notes_writes_onto_waiting_thread() {
        let (tm, cm) = make_conv_manager();
        let conv_id = cm
            .get_or_create_conversation("xmpp", "user1")
            .await
            .unwrap();
        let project = ProjectId::new();
        let mut thread = crate::types::thread::Thread::new(
            "wait",
            ThreadType::Foreground,
            project,
            "user1",
            ThreadConfig::default(),
        );
        thread.transition_to(ThreadState::Running, None).unwrap();
        thread.add_message(ThreadMessage::user("earlier"));
        thread
            .transition_to(ThreadState::Waiting, Some("approval".into()))
            .unwrap();
        cm.store.save_thread(&thread).await.unwrap();
        {
            let mut convs = cm.conversations.write().await;
            convs.get_mut(&conv_id).unwrap().track_thread(thread.id);
        }
        assert!(
            cm.queue_steer_note(conv_id, "user1", "wrapped steer".to_string())
                .await
                .unwrap()
        );

        cm.drain_pending_steer_notes(conv_id, "user1", thread.id)
            .await
            .unwrap();
        tm.resume_thread(
            thread.id,
            "user1",
            Some(ThreadMessage::user("approved")),
            Some(("call-1".into(), true)),
            None,
        )
        .await
        .unwrap();
        let _ = tm.join_thread(thread.id).await.unwrap();
        let saved = cm.store.load_thread(thread.id).await.unwrap().unwrap();
        assert_eq!(
            user_message_contents(&saved),
            vec!["earlier", "wrapped steer", "approved"]
        );
        assert!(
            cm.get_conversation(conv_id)
                .await
                .unwrap()
                .pending_steer_notes
                .is_empty()
        );
    }

    #[tokio::test]
    async fn handle_message_resumes_suspended_thread() {
        let store = Arc::new(MockStore::new());
        let tm = Arc::new(ThreadManager::new(
            Arc::new(MockLlm(Mutex::new(vec![LlmOutput {
                response: LlmResponse::Text("Recovered".into()),
                usage: TokenUsage::default(),
                finish_reason: "stop".to_string(),
            }]))),
            Arc::new(MockEffects),
            store.clone(),
            Arc::new(CapabilityRegistry::new()),
            Arc::new(LeaseManager::new()),
            Arc::new(PolicyEngine::new()),
        ));
        let cm = ConversationManager::new(Arc::clone(&tm), store.clone());

        let conv_id = cm.get_or_create_conversation("web", "user1").await.unwrap();
        let project = ProjectId::new();
        let mut thread = crate::types::thread::Thread::new(
            "resume",
            ThreadType::Foreground,
            project,
            "user1",
            ThreadConfig::default(),
        );
        thread.transition_to(ThreadState::Running, None).unwrap();
        thread.add_message(ThreadMessage::user("earlier"));
        thread.step_count = 1;
        thread.metadata = serde_json::json!({
            "runtime_checkpoint": {
                "persisted_state": {"last_return": 7},
                "nudge_count": 0,
                "consecutive_errors": 0,
                "compaction_count": 0
            }
        });
        thread
            .transition_to(
                ThreadState::Suspended,
                Some("engine restart; resumable from checkpoint".into()),
            )
            .unwrap();
        store.save_thread(&thread).await.unwrap();

        {
            let mut convs = cm.conversations.write().await;
            let conv = convs.get_mut(&conv_id).unwrap();
            conv.track_thread(thread.id);
        }

        let resumed = cm
            .handle_user_message_with_parts(
                conv_id,
                "continue from there",
                vec![TransientContentPart::Image {
                    mime_type: "image/png".to_string(),
                    data: vec![1, 2, 3],
                }],
                project,
                "user1",
                ThreadConfig::default(),
                None,
            )
            .await
            .unwrap();

        assert_eq!(resumed, thread.id);
        let outcome = tm.join_thread(thread.id).await.unwrap();
        assert!(matches!(outcome, ThreadOutcome::Completed { .. }));
        let saved = store.load_thread(thread.id).await.unwrap().unwrap();
        let resumed_message = saved
            .messages
            .iter()
            .find(|message| message.content == "continue from there")
            .expect("resumed message should be retained");
        assert_eq!(resumed_message.transient_content_parts.len(), 1);
    }

    #[tokio::test]
    async fn handle_message_rejects_waiting_thread_without_overwrite() {
        let (tm, cm) = make_conv_manager();
        let conv_id = cm.get_or_create_conversation("web", "user1").await.unwrap();
        let project = ProjectId::new();
        let mut waiting = crate::types::thread::Thread::new(
            "original request",
            ThreadType::Foreground,
            project,
            "user1",
            ThreadConfig::default(),
        );
        waiting.transition_to(ThreadState::Running, None).unwrap();
        waiting
            .transition_to(ThreadState::Waiting, Some("approval required".into()))
            .unwrap();
        let thread_id = waiting.id;
        cm.store.save_thread(&waiting).await.unwrap();
        cm.conversations
            .write()
            .await
            .get_mut(&conv_id)
            .unwrap()
            .track_thread(thread_id);

        let result = cm
            .handle_user_message(
                conv_id,
                "second message",
                project,
                "user1",
                ThreadConfig::default(),
                Some(thread_id),
            )
            .await;

        assert!(matches!(
            result,
            Err(EngineError::Thread(
                crate::types::error::ThreadError::Waiting(id)
            )) if id == thread_id
        ));
        assert!(!tm.is_running(thread_id).await);
        let saved = cm.store.load_thread(thread_id).await.unwrap().unwrap();
        assert_eq!(saved.state, ThreadState::Waiting);
        assert_eq!(saved.goal, "original request");
        assert!(
            cm.get_conversation(conv_id)
                .await
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[tokio::test]
    async fn record_outcome_adds_entry() {
        let (_, cm) = make_conv_manager();
        let conv_id = cm.get_or_create_conversation("cli", "user1").await.unwrap();
        let tid = ThreadId::new();

        // Manually track a thread
        {
            let mut convs = cm.conversations.write().await;
            let conv = convs.get_mut(&conv_id).unwrap();
            conv.track_thread(tid);
        }

        // Record completion
        cm.record_thread_outcome(
            conv_id,
            tid,
            &ThreadOutcome::Completed {
                response: Some("Done!".into()),
            },
        )
        .await
        .unwrap();

        let conv = cm.get_conversation(conv_id).await.unwrap();
        assert!(conv.active_threads.is_empty());
        assert_eq!(conv.entries.len(), 1);
        assert_eq!(conv.entries[0].content, "Done!");

        // Check sender is agent
        assert!(matches!(
            conv.entries[0].sender,
            EntrySender::Agent { thread_id } if thread_id == tid
        ));
    }

    #[tokio::test]
    async fn record_outcome_skips_empty_response() {
        let (_, cm) = make_conv_manager();
        let conv_id = cm.get_or_create_conversation("cli", "user1").await.unwrap();
        let tid = ThreadId::new();

        cm.conversations
            .write()
            .await
            .get_mut(&conv_id)
            .unwrap()
            .track_thread(tid);
        cm.record_thread_outcome(
            conv_id,
            tid,
            &ThreadOutcome::Completed {
                response: Some(String::new()),
            },
        )
        .await
        .unwrap();

        let conv = cm.get_conversation(conv_id).await.unwrap();
        assert!(conv.active_threads.is_empty());
        assert!(conv.entries.is_empty());
    }

    #[tokio::test]
    async fn untrack_thread_only_updates_named_conversation() {
        let (_, cm) = make_conv_manager();
        let first = cm.get_or_create_conversation("web", "user1").await.unwrap();
        let second = cm
            .get_or_create_conversation("xmpp", "user1")
            .await
            .unwrap();
        let thread_id = ThreadId::new();
        {
            let mut conversations = cm.conversations.write().await;
            conversations
                .get_mut(&first)
                .unwrap()
                .track_thread(thread_id);
            conversations
                .get_mut(&second)
                .unwrap()
                .track_thread(thread_id);
        }

        cm.untrack_thread(first, thread_id).await;

        assert!(
            !cm.get_conversation(first)
                .await
                .unwrap()
                .active_threads
                .contains(&thread_id)
        );
        assert!(
            cm.get_conversation(second)
                .await
                .unwrap()
                .active_threads
                .contains(&thread_id)
        );
    }

    #[tokio::test]
    async fn list_conversations_filters_by_user() {
        let (_, cm) = make_conv_manager();
        cm.get_or_create_conversation("web", "alice").await.unwrap();
        cm.get_or_create_conversation("xmpp", "alice")
            .await
            .unwrap();
        cm.get_or_create_conversation("web", "bob").await.unwrap();

        let alice_convs = cm.list_conversations("alice").await;
        assert_eq!(alice_convs.len(), 2);

        let bob_convs = cm.list_conversations("bob").await;
        assert_eq!(bob_convs.len(), 1);
    }

    #[tokio::test]
    async fn bootstrap_user_loads_persisted_conversations() {
        let store = Arc::new(MockStore::new());
        let mut conv = ConversationSurface::new("web", "user1");
        conv.add_entry(ConversationEntry::user("persisted"));
        store.save_conversation(&conv).await.unwrap();

        let tm = Arc::new(ThreadManager::new(
            Arc::new(MockLlm(Mutex::new(vec![]))),
            Arc::new(MockEffects),
            store.clone(),
            Arc::new(CapabilityRegistry::new()),
            Arc::new(LeaseManager::new()),
            Arc::new(PolicyEngine::new()),
        ));
        let cm = ConversationManager::new(tm, store);

        let loaded = cm.bootstrap_user("user1").await.unwrap();
        assert_eq!(loaded, 1);

        let conv_id = cm.get_or_create_conversation("web", "user1").await.unwrap();
        assert_eq!(conv_id, conv.id);
        let saved = cm.get_conversation(conv.id).await.unwrap();
        assert_eq!(saved.entries.len(), 1);
        assert_eq!(saved.entries[0].content, "persisted");
    }

    #[tokio::test]
    async fn clear_conversation_resets_entries_and_threads() {
        let (tm, cm) = make_conv_manager();
        let conv_id = cm.get_or_create_conversation("web", "user1").await.unwrap();
        let project = ProjectId::new();

        // Spawn a thread so the conversation has entries and active threads
        let tid = cm
            .handle_user_message(
                conv_id,
                "Hello",
                project,
                "user1",
                ThreadConfig::default(),
                None,
            )
            .await
            .unwrap();

        // Wait for thread to finish
        let _ = tm.join_thread(tid).await.unwrap();

        // Record outcome so there's an agent entry
        cm.record_thread_outcome(
            conv_id,
            tid,
            &ThreadOutcome::Completed {
                response: Some("Hi there".into()),
            },
        )
        .await
        .unwrap();

        let conv = cm.get_conversation(conv_id).await.unwrap();
        assert!(!conv.entries.is_empty());
        assert!(
            cm.queue_steer_note(conv_id, "user1", "discard me".to_string())
                .await
                .unwrap()
        );

        // Clear the conversation
        cm.clear_conversation(conv_id, "user1").await.unwrap();

        let conv = cm.get_conversation(conv_id).await.unwrap();
        assert!(conv.entries.is_empty());
        assert!(conv.active_threads.is_empty());
        assert!(conv.pending_steer_notes.is_empty());
    }
}
