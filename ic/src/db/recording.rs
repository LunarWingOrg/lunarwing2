//! Test-support `Database` decorators.
//!
//! `RecordingDb` (WI#375 DR-2): `job_events.job_id` has a foreign key to
//! `agent_jobs`, so a test that sabotages the job table cannot prove the
//! supervisor never even TRIED to write an event for an unfired job: the FK
//! makes the write fail indistinguishably. `RecordingDb` wraps a real
//! backend, delegates every call, and records each `save_job_event` attempt
//! before forwarding it, so tests assert on attempts rather than surviving
//! rows.
//!
//! `FaultingSettingsDb` (WI#371): delegates every call except `get_setting`
//! / `set_setting` for keys under one configured prefix, where reads can be
//! failed outright and writes can be failed a programmed number of times
//! before one succeeds. Every prefixed write attempt is recorded so tests
//! assert on retry counts, not just surviving rows.
//!
//! Test-support only; integration tests in `ic/tests/` import the crate as a
//! regular dependency, so this module is always compiled (same shape as
//! `channels::web::test_helpers`). The delegation bodies are a mechanical
//! listing of the `Database` sub-trait surface. The compiler enforces
//! completeness only for REQUIRED methods — a new trait method with a
//! provided (default) body silently runs the default instead of delegating
//! (MR !1669 review IK-9), so provided methods are listed here too; when the
//! trait grows a provided method, add the matching entry deliberately.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

use super::*;
use crate::agent::BrokenTool;
use crate::agent::routine::{Routine, RoutineRun, RunStatus};
use crate::context::{ActionRecord, JobContext, JobState};
use crate::error::WorkspaceError;
use crate::history::{
    AgentJobRecord, AgentJobSummary, ConversationMessage, ConversationSummary, JobEventRecord,
    LlmCallRecord, SandboxJobRecord, SandboxJobSummary, SettingRow,
};
use crate::workspace::{MemoryChunk, MemoryDocument, WorkspaceEntry, WorkspaceFileContent};
use crate::workspace::{SearchConfig, SearchResult};

/// A `Database` wrapper that records `save_job_event` attempts.
pub struct RecordingDb {
    inner: Arc<dyn Database>,
    job_event_attempts: std::sync::Mutex<Vec<(Uuid, String)>>,
}

impl RecordingDb {
    pub fn new(inner: Arc<dyn Database>) -> Self {
        Self {
            inner,
            job_event_attempts: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Every `save_job_event(job_id, event_type, ..)` call seen so far, in
    /// order. Attempts are recorded even when the inner write fails (that is
    /// the point: the FK would mask them).
    pub fn job_event_attempts(&self) -> Vec<(Uuid, String)> {
        self.job_event_attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// SandboxStore is written out by hand (not via the macro) so the
/// `save_job_event` override can record the attempt before delegating.
#[async_trait]
impl SandboxStore for RecordingDb {
    async fn save_sandbox_job(&self, job: &SandboxJobRecord) -> Result<(), DatabaseError> {
        self.inner.save_sandbox_job(job).await
    }
    async fn get_sandbox_job(&self, id: Uuid) -> Result<Option<SandboxJobRecord>, DatabaseError> {
        self.inner.get_sandbox_job(id).await
    }
    async fn list_sandbox_jobs(&self) -> Result<Vec<SandboxJobRecord>, DatabaseError> {
        self.inner.list_sandbox_jobs().await
    }
    async fn update_sandbox_job_status(
        &self,
        id: Uuid,
        status: &str,
        success: Option<bool>,
        message: Option<&str>,
        started_at: Option<DateTime<Utc>>,
        completed_at: Option<DateTime<Utc>>,
    ) -> Result<(), DatabaseError> {
        self.inner
            .update_sandbox_job_status(id, status, success, message, started_at, completed_at)
            .await
    }
    async fn cleanup_stale_sandbox_jobs(&self) -> Result<u64, DatabaseError> {
        self.inner.cleanup_stale_sandbox_jobs().await
    }
    async fn sandbox_job_summary(&self) -> Result<SandboxJobSummary, DatabaseError> {
        self.inner.sandbox_job_summary().await
    }
    async fn list_sandbox_jobs_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<SandboxJobRecord>, DatabaseError> {
        self.inner.list_sandbox_jobs_for_user(user_id).await
    }
    async fn sandbox_job_summary_for_user(
        &self,
        user_id: &str,
    ) -> Result<SandboxJobSummary, DatabaseError> {
        self.inner.sandbox_job_summary_for_user(user_id).await
    }
    async fn sandbox_job_belongs_to_user(
        &self,
        job_id: Uuid,
        user_id: &str,
    ) -> Result<bool, DatabaseError> {
        self.inner
            .sandbox_job_belongs_to_user(job_id, user_id)
            .await
    }
    async fn update_sandbox_job_mode(&self, id: Uuid, mode: &str) -> Result<(), DatabaseError> {
        self.inner.update_sandbox_job_mode(id, mode).await
    }
    async fn get_sandbox_job_mode(&self, id: Uuid) -> Result<Option<String>, DatabaseError> {
        self.inner.get_sandbox_job_mode(id).await
    }
    async fn save_job_event(
        &self,
        job_id: Uuid,
        event_type: &str,
        data: &serde_json::Value,
    ) -> Result<(), DatabaseError> {
        self.job_event_attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((job_id, event_type.to_string()));
        self.inner.save_job_event(job_id, event_type, data).await
    }
    async fn list_job_events(
        &self,
        job_id: Uuid,
        limit: Option<i64>,
    ) -> Result<Vec<JobEventRecord>, DatabaseError> {
        self.inner.list_job_events(job_id, limit).await
    }
}

#[async_trait]
impl Database for RecordingDb {
    async fn run_migrations(&self) -> Result<(), DatabaseError> {
        self.inner.run_migrations().await
    }
}

macro_rules! delegate_database {
    // Fan the same trait list out to each listed decorator type (a flat
    // two-dimensional repetition cannot mix the type and trait variables).
    ([$ty:ident $(, $rest:ident)* $(,)?] $($body:tt)*) => {
        delegate_database!(@impl $ty; $($body)*);
        delegate_database!([$($rest),*] $($body)*);
    };
    ([] $($body:tt)*) => {};
    (@impl $ty:ident; $($trait:ident { $( $name:ident (&self $(, $arg:ident : $argty:ty)* $(,)?) -> $ret:ty ; )* } )*) => {
        $(
            #[async_trait]
            impl $trait for $ty {
                $(
                    async fn $name(&self $(, $arg: $argty)*) -> $ret {
                        self.inner.$name($($arg),*).await
                    }
                )*
            }
        )*
    };
}

// SandboxStore and SettingsStore stay out of the shared list: RecordingDb
// overrides `save_job_event` (hand-written above) and FaultingSettingsDb
// overrides the prefixed `get_setting`/`set_setting` (hand-written below).
delegate_database! {[RecordingDb, FaultingSettingsDb]
    ConversationStore {
        create_conversation( &self, channel: &str, user_id: &str, thread_id: Option<&str>, ) -> Result<Uuid, DatabaseError>;
        touch_conversation(&self, id: Uuid) -> Result<(), DatabaseError>;
        add_conversation_message( &self, conversation_id: Uuid, role: &str, content: &str, ) -> Result<Uuid, DatabaseError>;
        add_conversation_message_if_empty( &self, conversation_id: Uuid, role: &str, content: &str, ) -> Result<bool, DatabaseError>;
        ensure_conversation( &self, id: Uuid, channel: &str, user_id: &str, thread_id: Option<&str>, source_channel: Option<&str>, ) -> Result<bool, DatabaseError>;
        list_conversations_with_preview( &self, user_id: &str, channel: &str, limit: i64, ) -> Result<Vec<ConversationSummary>, DatabaseError>;
        list_conversations_all_channels( &self, user_id: &str, limit: i64, ) -> Result<Vec<ConversationSummary>, DatabaseError>;
        get_or_create_routine_conversation( &self, routine_id: Uuid, routine_name: &str, user_id: &str, ) -> Result<Uuid, DatabaseError>;
        find_routine_conversation( &self, routine_id: Uuid, user_id: &str, ) -> Result<Option<Uuid>, DatabaseError>;
        get_or_create_heartbeat_conversation( &self, user_id: &str, ) -> Result<Uuid, DatabaseError>;
        get_or_create_assistant_conversation( &self, user_id: &str, channel: &str, ) -> Result<Uuid, DatabaseError>;
        create_conversation_with_metadata( &self, channel: &str, user_id: &str, metadata: &serde_json::Value, ) -> Result<Uuid, DatabaseError>;
        list_conversation_messages_paginated( &self, conversation_id: Uuid, before: Option<DateTime<Utc>>, limit: i64, ) -> Result<(Vec<ConversationMessage>, bool), DatabaseError>;
        update_conversation_metadata_field( &self, id: Uuid, key: &str, value: &serde_json::Value, ) -> Result<(), DatabaseError>;
        get_conversation_metadata( &self, id: Uuid, ) -> Result<Option<serde_json::Value>, DatabaseError>;
        list_conversation_messages( &self, conversation_id: Uuid, ) -> Result<Vec<ConversationMessage>, DatabaseError>;
        conversation_belongs_to_user( &self, conversation_id: Uuid, user_id: &str, ) -> Result<bool, DatabaseError>;
        get_conversation_source_channel( &self, conversation_id: Uuid, ) -> Result<Option<String>, DatabaseError>;
        get_or_create_scoped_conversation( &self, channel: &str, user_id: &str, scope: &str, ) -> Result<Uuid, DatabaseError>;
    }

    JobStore {
        save_job(&self, ctx: &JobContext) -> Result<(), DatabaseError>;
        get_job(&self, id: Uuid) -> Result<Option<JobContext>, DatabaseError>;
        update_job_status( &self, id: Uuid, status: JobState, failure_reason: Option<&str>, ) -> Result<(), DatabaseError>;
        mark_job_stuck(&self, id: Uuid) -> Result<(), DatabaseError>;
        get_stuck_jobs(&self) -> Result<Vec<Uuid>, DatabaseError>;
        list_agent_jobs(&self) -> Result<Vec<AgentJobRecord>, DatabaseError>;
        list_agent_jobs_for_user( &self, user_id: &str, ) -> Result<Vec<AgentJobRecord>, DatabaseError>;
        agent_job_summary(&self) -> Result<AgentJobSummary, DatabaseError>;
        agent_job_summary_for_user( &self, user_id: &str, ) -> Result<AgentJobSummary, DatabaseError>;
        get_agent_job_failure_reason(&self, id: Uuid) -> Result<Option<String>, DatabaseError>;
        save_action(&self, job_id: Uuid, action: &ActionRecord) -> Result<(), DatabaseError>;
        get_job_actions(&self, job_id: Uuid) -> Result<Vec<ActionRecord>, DatabaseError>;
        record_llm_call(&self, record: &LlmCallRecord<'_>) -> Result<Uuid, DatabaseError>;
        save_estimation_snapshot( &self, job_id: Uuid, category: &str, tool_names: &[String], estimated_cost: Decimal, estimated_time_secs: i32, estimated_value: Decimal, ) -> Result<Uuid, DatabaseError>;
        update_estimation_actuals( &self, id: Uuid, actual_cost: Decimal, actual_time_secs: i32, actual_value: Option<Decimal>, ) -> Result<(), DatabaseError>;
    }

    RoutineStore {
        create_routine(&self, routine: &Routine) -> Result<(), DatabaseError>;
        get_routine(&self, id: Uuid) -> Result<Option<Routine>, DatabaseError>;
        get_routine_by_name( &self, user_id: &str, name: &str, ) -> Result<Option<Routine>, DatabaseError>;
        list_routines(&self, user_id: &str) -> Result<Vec<Routine>, DatabaseError>;
        list_all_routines(&self) -> Result<Vec<Routine>, DatabaseError>;
        list_event_routines(&self) -> Result<Vec<Routine>, DatabaseError>;
        list_due_cron_routines(&self) -> Result<Vec<Routine>, DatabaseError>;
        update_routine(&self, routine: &Routine) -> Result<(), DatabaseError>;
        update_routine_runtime( &self, id: Uuid, last_run_at: DateTime<Utc>, next_fire_at: Option<DateTime<Utc>>, run_count: u64, consecutive_failures: u32, state: &serde_json::Value, ) -> Result<(), DatabaseError>;
        delete_routine(&self, id: Uuid) -> Result<bool, DatabaseError>;
        create_routine_run(&self, run: &RoutineRun) -> Result<(), DatabaseError>;
        complete_routine_run( &self, id: Uuid, status: RunStatus, result_summary: Option<&str>, tokens_used: Option<i32>, ) -> Result<(), DatabaseError>;
        list_routine_runs( &self, routine_id: Uuid, limit: i64, ) -> Result<Vec<RoutineRun>, DatabaseError>;
        count_running_routine_runs(&self, routine_id: Uuid) -> Result<i64, DatabaseError>;
        count_running_routine_runs_batch( &self, routine_ids: &[Uuid], ) -> Result<HashMap<Uuid, i64>, DatabaseError>;
        batch_get_last_run_status( &self, routine_ids: &[Uuid], ) -> Result<HashMap<Uuid, RunStatus>, DatabaseError>;
        link_routine_run_to_job( &self, run_id: Uuid, job_id: Uuid, ) -> Result<(), DatabaseError>;
        get_webhook_routine_by_path( &self, path: &str, user_id: Option<&str>, ) -> Result<Option<Routine>, DatabaseError>;
        list_dispatched_routine_runs(&self) -> Result<Vec<RoutineRun>, DatabaseError>;
        list_stuck_lightweight_runs( &self, cutoff: chrono::DateTime<chrono::Utc>, ) -> Result<Vec<RoutineRun>, DatabaseError>;
    }

    ToolFailureStore {
        record_tool_failure( &self, tool_name: &str, error_message: &str, ) -> Result<(), DatabaseError>;
        get_broken_tools(&self, threshold: i32) -> Result<Vec<BrokenTool>, DatabaseError>;
        mark_tool_repaired(&self, tool_name: &str) -> Result<(), DatabaseError>;
        increment_repair_attempts(&self, tool_name: &str) -> Result<(), DatabaseError>;
        record_tool_success(&self, tool_name: &str) -> Result<(), DatabaseError>;
        reset_tool_repair_attempts(&self) -> Result<u64, DatabaseError>;
    }

    WorkspaceStore {
        get_document_by_path( &self, user_id: &str, agent_id: Option<Uuid>, path: &str, ) -> Result<MemoryDocument, WorkspaceError>;
        get_document_by_id(&self, id: Uuid) -> Result<MemoryDocument, WorkspaceError>;
        get_or_create_document_by_path( &self, user_id: &str, agent_id: Option<Uuid>, path: &str, ) -> Result<MemoryDocument, WorkspaceError>;
        update_document(&self, id: Uuid, content: &str) -> Result<(), WorkspaceError>;
        append_document( &self, id: Uuid, content: &str, separator: &str, ) -> Result<String, WorkspaceError>;
        update_document_and_replace_chunks( &self, id: Uuid, content: &str, chunks: &[(i32, String, Option<Vec<f32>>)], ) -> Result<(), WorkspaceError>;
        delete_document_by_path( &self, user_id: &str, agent_id: Option<Uuid>, path: &str, ) -> Result<(), WorkspaceError>;
        list_directory( &self, user_id: &str, agent_id: Option<Uuid>, directory: &str, ) -> Result<Vec<WorkspaceEntry>, WorkspaceError>;
        list_files_limited( &self, user_id: &str, agent_id: Option<Uuid>, directory: &str, suffix: &str, limit: usize, ) -> Result<Vec<WorkspaceEntry>, WorkspaceError>;
        get_files_limited( &self, user_id: &str, agent_id: Option<Uuid>, directory: &str, suffix: &str, limit: usize, max_file_bytes: usize, ) -> Result<Vec<WorkspaceFileContent>, WorkspaceError>;
        list_all_paths( &self, user_id: &str, agent_id: Option<Uuid>, ) -> Result<Vec<String>, WorkspaceError>;
        list_documents( &self, user_id: &str, agent_id: Option<Uuid>, ) -> Result<Vec<MemoryDocument>, WorkspaceError>;
        delete_chunks(&self, document_id: Uuid) -> Result<(), WorkspaceError>;
        insert_chunk( &self, document_id: Uuid, chunk_index: i32, content: &str, embedding: Option<&[f32]>, ) -> Result<Uuid, WorkspaceError>;
        replace_chunks( &self, document_id: Uuid, chunks: &[(i32, String, Option<Vec<f32>>)], ) -> Result<Vec<Uuid>, WorkspaceError>;
        update_chunk_embedding( &self, chunk_id: Uuid, embedding: &[f32], ) -> Result<(), WorkspaceError>;
        get_chunks_without_embeddings( &self, user_id: &str, agent_id: Option<Uuid>, limit: usize, ) -> Result<Vec<MemoryChunk>, WorkspaceError>;
        list_stale_index_documents( &self, user_id: &str, agent_id: Option<Uuid>, limit: usize, ) -> Result<Vec<Uuid>, WorkspaceError>;
        purge_chunks_for_path_pattern( &self, user_id: &str, agent_id: Option<Uuid>, path_pattern: &str, ) -> Result<u64, WorkspaceError>;
        hybrid_search( &self, user_id: &str, agent_id: Option<Uuid>, query: &str, embedding: Option<&[f32]>, config: &SearchConfig, ) -> Result<Vec<SearchResult>, WorkspaceError>;
        update_document_metadata( &self, id: Uuid, metadata: &serde_json::Value, ) -> Result<(), crate::error::WorkspaceError>;
        find_config_documents( &self, user_id: &str, agent_id: Option<Uuid>, ) -> Result<Vec<crate::workspace::MemoryDocument>, crate::error::WorkspaceError>;
        knowledge_search( &self, user_ids: &[String], agent_id: Option<Uuid>, query: &str, filter: &crate::workspace::KnowledgeFilter, limit: usize, ) -> Result<Vec<crate::workspace::KnowledgeSearchHit>, crate::error::WorkspaceError>;
        knowledge_topic_versions( &self, user_ids: &[String], agent_id: Option<Uuid>, topic: &str, ) -> Result<Vec<crate::workspace::MemoryDocument>, crate::error::WorkspaceError>;
        knowledge_list_notes( &self, user_ids: &[String], agent_id: Option<Uuid>, limit: usize, before: Option<(&DateTime<Utc>, &str, &str)>, ) -> Result<Vec<crate::workspace::KnowledgeNoteRow>, crate::error::WorkspaceError>;
        save_version( &self, document_id: Uuid, content: &str, content_hash: &str, changed_by: Option<&str>, ) -> Result<i32, crate::error::WorkspaceError>;
        get_version( &self, document_id: Uuid, version: i32, ) -> Result<crate::workspace::DocumentVersion, crate::error::WorkspaceError>;
        list_versions( &self, document_id: Uuid, limit: i64, ) -> Result<Vec<crate::workspace::VersionSummary>, crate::error::WorkspaceError>;
        get_latest_version_number( &self, document_id: Uuid, ) -> Result<Option<i32>, crate::error::WorkspaceError>;
        prune_versions( &self, document_id: Uuid, keep_count: i32, ) -> Result<u64, crate::error::WorkspaceError>;
        hybrid_search_multi( &self, user_ids: &[String], agent_id: Option<Uuid>, query: &str, embedding: Option<&[f32]>, config: &SearchConfig, ) -> Result<Vec<SearchResult>, WorkspaceError>;
        list_all_paths_multi( &self, user_ids: &[String], agent_id: Option<Uuid>, ) -> Result<Vec<String>, WorkspaceError>;
        get_document_by_path_multi( &self, user_ids: &[String], agent_id: Option<Uuid>, path: &str, ) -> Result<MemoryDocument, WorkspaceError>;
        list_directory_multi( &self, user_ids: &[String], agent_id: Option<Uuid>, directory: &str, ) -> Result<Vec<WorkspaceEntry>, WorkspaceError>;
    }

    UserStore {
        create_user(&self, user: &UserRecord) -> Result<(), DatabaseError>;
        get_user(&self, id: &str) -> Result<Option<UserRecord>, DatabaseError>;
        get_user_by_email(&self, email: &str) -> Result<Option<UserRecord>, DatabaseError>;
        list_users(&self, status: Option<&str>) -> Result<Vec<UserRecord>, DatabaseError>;
        update_user_status(&self, id: &str, status: &str) -> Result<(), DatabaseError>;
        update_user_role(&self, id: &str, role: &str) -> Result<(), DatabaseError>;
        update_user_profile( &self, id: &str, display_name: &str, metadata: &serde_json::Value, ) -> Result<(), DatabaseError>;
        record_login(&self, id: &str) -> Result<(), DatabaseError>;
        create_api_token( &self, user_id: &str, name: &str, token_hash: &[u8; 32], token_prefix: &str, expires_at: Option<DateTime<Utc>>, ) -> Result<ApiTokenRecord, DatabaseError>;
        list_api_tokens(&self, user_id: &str) -> Result<Vec<ApiTokenRecord>, DatabaseError>;
        revoke_api_token(&self, token_id: Uuid, user_id: &str) -> Result<bool, DatabaseError>;
        authenticate_token( &self, token_hash: &[u8; 32], ) -> Result<Option<(ApiTokenRecord, UserRecord)>, DatabaseError>;
        record_token_usage(&self, token_id: Uuid) -> Result<(), DatabaseError>;
        has_any_users(&self) -> Result<bool, DatabaseError>;
        delete_user(&self, id: &str) -> Result<bool, DatabaseError>;
        user_usage_stats( &self, user_id: Option<&str>, since: DateTime<Utc>, ) -> Result<Vec<UserUsageStats>, DatabaseError>;
        user_summary_stats( &self, user_id: Option<&str>, ) -> Result<Vec<UserSummaryStats>, DatabaseError>;
        create_user_with_token( &self, user: &UserRecord, token_name: &str, token_hash: &[u8; 32], token_prefix: &str, expires_at: Option<DateTime<Utc>>, ) -> Result<ApiTokenRecord, DatabaseError>;
    }

    IdentityStore {
        get_identity_by_provider( &self, provider: &str, provider_user_id: &str, ) -> Result<Option<UserIdentityRecord>, DatabaseError>;
        list_identities_for_user( &self, user_id: &str, ) -> Result<Vec<UserIdentityRecord>, DatabaseError>;
        create_identity(&self, identity: &UserIdentityRecord) -> Result<(), DatabaseError>;
        update_identity_profile( &self, provider: &str, provider_user_id: &str, display_name: Option<&str>, avatar_url: Option<&str>, ) -> Result<(), DatabaseError>;
        find_identity_by_verified_email( &self, email: &str, ) -> Result<Option<UserIdentityRecord>, DatabaseError>;
        create_user_with_identity( &self, user: &UserRecord, identity: &UserIdentityRecord, ) -> Result<(), DatabaseError>;
    }

    ReflexStore {
        find_recurring_job_patterns( &self, user_id: &str, min_count: i32, limit: i32, ) -> Result<Vec<String>, DatabaseError>;
        upsert_reflex_pattern( &self, user_id: &str, normalized_pattern: &str, original_pattern: &str, tool_name: &str, ) -> Result<(), DatabaseError>;
        get_reflex_pattern( &self, user_id: &str, normalized_pattern: &str, ) -> Result<Option<(String, String)>, DatabaseError>;
        list_reflex_patterns( &self, user_id: &str, ) -> Result<Vec<ReflexPatternRecord>, DatabaseError>;
        disable_reflex_pattern(&self, id: Uuid) -> Result<bool, DatabaseError>;
        bump_reflex_pattern_matches( &self, hits: &[ReflexPatternHit], ) -> Result<(), DatabaseError>;
        record_reflex_compilation_failure( &self, user_id: &str, normalized_pattern: &str, original_pattern: &str, tool_name: &str, max_attempts: i32, ) -> Result<(), DatabaseError>;
        prune_stale_reflex_patterns( &self, stale_after_days: i32, dry_run: bool, ) -> Result<Vec<ReflexPatternRecord>, DatabaseError>;
        update_reflex_pattern_embedding( &self, user_id: &str, normalized_pattern: &str, embedding: &[f32], model: &str, ) -> Result<(), DatabaseError>;
        semantic_search_reflex_patterns( &self, user_id: &str, query_embedding: &[f32], limit: i32, ) -> Result<Vec<(ReflexPatternRecord, f64)>, DatabaseError>;
        bump_reflex_pattern_match( &self, user_id: &str, normalized_pattern: &str, ) -> Result<(), DatabaseError>;
    }
}

// RecordingDb delegates the settings surface outright; FaultingSettingsDb's
// hand-written SettingsStore below injects the prefixed faults instead.
delegate_database! {[RecordingDb]
    SettingsStore {
        get_setting( &self, user_id: &str, key: &str, ) -> Result<Option<serde_json::Value>, DatabaseError>;
        get_setting_full( &self, user_id: &str, key: &str, ) -> Result<Option<SettingRow>, DatabaseError>;
        set_setting( &self, user_id: &str, key: &str, value: &serde_json::Value, ) -> Result<(), DatabaseError>;
        compare_and_set_setting( &self, user_id: &str, key: &str, expected: Option<&serde_json::Value>, value: &serde_json::Value, ) -> Result<bool, DatabaseError>;
        delete_setting(&self, user_id: &str, key: &str) -> Result<bool, DatabaseError>;
        list_settings(&self, user_id: &str) -> Result<Vec<SettingRow>, DatabaseError>;
        get_all_settings( &self, user_id: &str, ) -> Result<HashMap<String, serde_json::Value>, DatabaseError>;
        set_all_settings( &self, user_id: &str, settings: &HashMap<String, serde_json::Value>, ) -> Result<(), DatabaseError>;
        has_settings(&self, user_id: &str) -> Result<bool, DatabaseError>;
    }
}

/// Settings-scoped fault-injecting `Database` decorator for tests (WI#371).
///
/// Delegates every call except `get_setting`/`set_setting` for keys under one
/// configured prefix: reads can be failed outright, and writes can be failed
/// a programmed number of times before one is forwarded. Every prefixed write
/// attempt is recorded, so retry behavior is asserted on attempts rather than
/// on surviving rows. Works over any backend behind the shared trait.
pub struct FaultingSettingsDb {
    inner: Arc<dyn Database>,
    key_prefix: String,
    fail_reads: AtomicBool,
    remaining_write_failures: AtomicU32,
    write_attempts: std::sync::Mutex<Vec<String>>,
}

impl FaultingSettingsDb {
    /// Wrap `inner`; only keys starting with `key_prefix` can be faulted.
    pub fn new(inner: Arc<dyn Database>, key_prefix: impl Into<String>) -> Self {
        Self {
            inner,
            key_prefix: key_prefix.into(),
            fail_reads: AtomicBool::new(false),
            remaining_write_failures: AtomicU32::new(0),
            write_attempts: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Toggle read failures for prefixed keys.
    pub fn set_fail_reads(&self, fail: bool) {
        self.fail_reads.store(fail, Ordering::Relaxed);
    }

    /// Program the next `count` prefixed writes to fail before one succeeds.
    pub fn fail_next_writes(&self, count: u32) {
        self.remaining_write_failures.store(count, Ordering::Relaxed);
    }

    /// Prefixed `set_setting` keys attempted so far, in order.
    pub fn write_attempts(&self) -> Vec<String> {
        self.write_attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn read_fault(&self, key: &str) -> Option<DatabaseError> {
        (self.fail_reads.load(Ordering::Relaxed) && key.starts_with(&self.key_prefix))
            .then(|| DatabaseError::Query(format!("injected settings read failure for {key}")))
    }
}

#[async_trait]
impl Database for FaultingSettingsDb {
    async fn run_migrations(&self) -> Result<(), DatabaseError> {
        self.inner.run_migrations().await
    }
}

#[async_trait]
impl SettingsStore for FaultingSettingsDb {
    async fn get_setting(
        &self,
        user_id: &str,
        key: &str,
    ) -> Result<Option<serde_json::Value>, DatabaseError> {
        if let Some(fault) = self.read_fault(key) {
            return Err(fault);
        }
        self.inner.get_setting(user_id, key).await
    }
    async fn get_setting_full(
        &self,
        user_id: &str,
        key: &str,
    ) -> Result<Option<SettingRow>, DatabaseError> {
        self.inner.get_setting_full(user_id, key).await
    }
    async fn set_setting(
        &self,
        user_id: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<(), DatabaseError> {
        if key.starts_with(&self.key_prefix) {
            self.write_attempts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(key.to_string());
            let remaining = self.remaining_write_failures.load(Ordering::Relaxed);
            if remaining > 0 {
                self.remaining_write_failures
                    .store(remaining - 1, Ordering::Relaxed);
                return Err(DatabaseError::Query(format!(
                    "injected settings write failure for {key}"
                )));
            }
        }
        self.inner.set_setting(user_id, key, value).await
    }
    async fn compare_and_set_setting(
        &self,
        user_id: &str,
        key: &str,
        expected: Option<&serde_json::Value>,
        value: &serde_json::Value,
    ) -> Result<bool, DatabaseError> {
        self.inner
            .compare_and_set_setting(user_id, key, expected, value)
            .await
    }
    async fn delete_setting(&self, user_id: &str, key: &str) -> Result<bool, DatabaseError> {
        self.inner.delete_setting(user_id, key).await
    }
    async fn list_settings(&self, user_id: &str) -> Result<Vec<SettingRow>, DatabaseError> {
        self.inner.list_settings(user_id).await
    }
    async fn get_all_settings(
        &self,
        user_id: &str,
    ) -> Result<HashMap<String, serde_json::Value>, DatabaseError> {
        self.inner.get_all_settings(user_id).await
    }
    async fn set_all_settings(
        &self,
        user_id: &str,
        settings: &HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        self.inner.set_all_settings(user_id, settings).await
    }
    async fn has_settings(&self, user_id: &str) -> Result<bool, DatabaseError> {
        self.inner.has_settings(user_id).await
    }
}

/// Pure delegation — FaultingSettingsDb only faults the settings surface.
#[async_trait]
impl SandboxStore for FaultingSettingsDb {
    async fn save_sandbox_job(&self, job: &SandboxJobRecord) -> Result<(), DatabaseError> {
        self.inner.save_sandbox_job(job).await
    }
    async fn get_sandbox_job(&self, id: Uuid) -> Result<Option<SandboxJobRecord>, DatabaseError> {
        self.inner.get_sandbox_job(id).await
    }
    async fn list_sandbox_jobs(&self) -> Result<Vec<SandboxJobRecord>, DatabaseError> {
        self.inner.list_sandbox_jobs().await
    }
    async fn update_sandbox_job_status(
        &self,
        id: Uuid,
        status: &str,
        success: Option<bool>,
        message: Option<&str>,
        started_at: Option<DateTime<Utc>>,
        completed_at: Option<DateTime<Utc>>,
    ) -> Result<(), DatabaseError> {
        self.inner
            .update_sandbox_job_status(id, status, success, message, started_at, completed_at)
            .await
    }
    async fn cleanup_stale_sandbox_jobs(&self) -> Result<u64, DatabaseError> {
        self.inner.cleanup_stale_sandbox_jobs().await
    }
    async fn sandbox_job_summary(&self) -> Result<SandboxJobSummary, DatabaseError> {
        self.inner.sandbox_job_summary().await
    }
    async fn list_sandbox_jobs_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<SandboxJobRecord>, DatabaseError> {
        self.inner.list_sandbox_jobs_for_user(user_id).await
    }
    async fn sandbox_job_summary_for_user(
        &self,
        user_id: &str,
    ) -> Result<SandboxJobSummary, DatabaseError> {
        self.inner.sandbox_job_summary_for_user(user_id).await
    }
    async fn sandbox_job_belongs_to_user(
        &self,
        job_id: Uuid,
        user_id: &str,
    ) -> Result<bool, DatabaseError> {
        self.inner
            .sandbox_job_belongs_to_user(job_id, user_id)
            .await
    }
    async fn update_sandbox_job_mode(&self, id: Uuid, mode: &str) -> Result<(), DatabaseError> {
        self.inner.update_sandbox_job_mode(id, mode).await
    }
    async fn get_sandbox_job_mode(&self, id: Uuid) -> Result<Option<String>, DatabaseError> {
        self.inner.get_sandbox_job_mode(id).await
    }
    async fn save_job_event(
        &self,
        job_id: Uuid,
        event_type: &str,
        data: &serde_json::Value,
    ) -> Result<(), DatabaseError> {
        self.inner.save_job_event(job_id, event_type, data).await
    }
    async fn list_job_events(
        &self,
        job_id: Uuid,
        limit: Option<i64>,
    ) -> Result<Vec<JobEventRecord>, DatabaseError> {
        self.inner.list_job_events(job_id, limit).await
    }
}

#[cfg(all(test, feature = "libsql"))]
mod tests {
    use super::*;

    /// WI#375 DR-2 / MR !1669 review IK-6: an attempt is recorded even when
    /// the inner write fails (no agent_jobs row, so the job_events FK refuses
    /// it). Without this positive control an empty attempt list could also
    /// mean "the recorder never records", which would make the no-orphan pin
    /// vacuous.
    #[tokio::test]
    async fn records_save_job_event_attempts_even_when_inner_write_fails() {
        let (inner, _dir) = crate::testing::test_db().await;
        let recording = RecordingDb::new(inner);
        let orphan = Uuid::new_v4();
        let _ = recording
            .save_job_event(orphan, "cron_reentry", &serde_json::json!({}))
            .await;
        assert_eq!(
            recording.job_event_attempts(),
            vec![(orphan, "cron_reentry".to_string())]
        );
    }

    /// WI#371: the decorator faults only prefixed keys, counts the programmed
    /// write failures down, and records every prefixed write attempt.
    #[tokio::test]
    async fn faulting_settings_db_faults_only_prefixed_keys() {
        let (inner, _dir) = crate::testing::test_db().await;
        let db = FaultingSettingsDb::new(inner, "cron-mission:");

        // Unprefixed keys are never faulted.
        db.set_setting("owner", "other:key", &serde_json::json!(1))
            .await
            .expect("unprefixed write must delegate"); // safety: test

        db.set_fail_reads(true);
        assert!(
            db.get_setting("owner", "cron-mission:m:control")
                .await
                .is_err(),
            "prefixed read must fail while reads are faulted"
        );
        assert!(
            db.get_setting("owner", "other:key")
                .await
                .expect("unprefixed read must delegate") // safety: test
                .is_some()
        );
        db.set_fail_reads(false);

        db.fail_next_writes(2);
        for _ in 0..2 {
            assert!(
                db.set_setting("owner", "cron-mission:m:control", &serde_json::json!({}))
                    .await
                    .is_err(),
                "programmed write failure must fire"
            );
        }
        db.set_setting("owner", "cron-mission:m:control", &serde_json::json!({"paused": true}))
            .await
            .expect("write succeeds once the programmed failures are spent"); // safety: test
        assert_eq!(
            db.write_attempts(),
            vec![
                "cron-mission:m:control".to_string(),
                "cron-mission:m:control".to_string(),
                "cron-mission:m:control".to_string(),
            ]
        );
        assert_eq!(
            db.get_setting("owner", "cron-mission:m:control")
                .await
                .expect("readback") // safety: test
                .expect("row present"), // safety: test
            serde_json::json!({"paused": true})
        );
    }
}
