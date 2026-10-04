//! Storage trait for engine persistence.
//!
//! Defines CRUD operations for all engine types. The main crate implements
//! this by wrapping its dual-backend `Database` trait (PostgreSQL + libSQL).

use crate::types::capability::{CapabilityLease, LeaseId};
use crate::types::conversation::{ConversationId, ConversationSurface};
use crate::types::error::EngineError;
use crate::types::event::ThreadEvent;
use crate::types::memory::{DocId, MemoryDoc};
use crate::types::mission::{Mission, MissionId, MissionStatus};
use crate::types::project::{Project, ProjectId};
use crate::types::step::Step;
use crate::types::thread::{Thread, ThreadId, ThreadState};
use crate::types::{is_shared_owner, shared_owner_candidates};

/// Persistence abstraction for the engine.
#[async_trait::async_trait]
pub trait Store: Send + Sync {
    // ── Thread operations ───────────────────────────────────

    async fn save_thread(&self, thread: &Thread) -> Result<(), EngineError>;
    async fn load_thread(&self, id: ThreadId) -> Result<Option<Thread>, EngineError>;
    async fn list_threads(
        &self,
        project_id: ProjectId,
        user_id: &str,
    ) -> Result<Vec<Thread>, EngineError>;
    async fn update_thread_state(
        &self,
        id: ThreadId,
        state: ThreadState,
    ) -> Result<(), EngineError>;

    // ── Step operations ─────────────────────────────────────

    async fn save_step(&self, step: &Step) -> Result<(), EngineError>;
    async fn load_steps(&self, thread_id: ThreadId) -> Result<Vec<Step>, EngineError>;

    // ── Event operations ────────────────────────────────────

    async fn append_events(&self, events: &[ThreadEvent]) -> Result<(), EngineError>;
    async fn load_events(&self, thread_id: ThreadId) -> Result<Vec<ThreadEvent>, EngineError>;

    // ── Project operations ──────────────────────────────────

    async fn save_project(&self, project: &Project) -> Result<(), EngineError>;
    async fn load_project(&self, id: ProjectId) -> Result<Option<Project>, EngineError>;
    async fn list_projects(&self, user_id: &str) -> Result<Vec<Project>, EngineError> {
        Err(EngineError::Store {
            reason: format!("Store::list_projects not implemented for user '{user_id}'"),
        })
    }
    async fn list_all_projects(&self) -> Result<Vec<Project>, EngineError> {
        Err(EngineError::Store {
            reason: "Store::list_all_projects not implemented".into(),
        })
    }

    // ── Conversation operations ─────────────────────────────

    async fn save_conversation(
        &self,
        conversation: &ConversationSurface,
    ) -> Result<(), EngineError> {
        Err(EngineError::Store {
            reason: format!(
                "Store::save_conversation not implemented for conversation '{}'",
                conversation.id
            ),
        })
    }
    async fn load_conversation(
        &self,
        id: ConversationId,
    ) -> Result<Option<ConversationSurface>, EngineError> {
        Err(EngineError::Store {
            reason: format!("Store::load_conversation not implemented for '{id}'"),
        })
    }
    async fn list_conversations(
        &self,
        user_id: &str,
    ) -> Result<Vec<ConversationSurface>, EngineError> {
        Err(EngineError::Store {
            reason: format!("Store::list_conversations not implemented for user '{user_id}'"),
        })
    }

    // ── Memory doc operations ───────────────────────────────

    async fn save_memory_doc(&self, doc: &MemoryDoc) -> Result<(), EngineError>;

    /// Host-only persistence for compiled seeds and engine failure bookkeeping.
    /// Never expose this operation to model tools or select it from document
    /// metadata: `source: compiled_in` is data, not write authority.
    async fn save_internal_memory_doc(&self, doc: &MemoryDoc) -> Result<(), EngineError> {
        self.save_memory_doc(doc).await
    }

    async fn load_memory_doc(&self, id: DocId) -> Result<Option<MemoryDoc>, EngineError>;
    /// Atomic evidence validation, run-budget enforcement, and insight upsert.
    ///
    /// Implementations must serialize validation+publication with mission
    /// lifecycle persistence (status/generation changes that pause or revoke
    /// a run) and with insight purge, so a revoked run cannot publish and a
    /// purge cannot race an in-flight save. Insight missions are tombstoned
    /// rather than hard-deleted, so `delete_mission` never interacts with
    /// insight documents; validation additionally fail-closes when the
    /// mission is missing, inactive, or the run was never reserved.
    /// Unsupported adapters deny the call.
    async fn save_conversation_insight(
        &self,
        _thread: &Thread,
        _doc: &MemoryDoc,
    ) -> Result<DocId, EngineError> {
        Err(EngineError::Store {
            reason: "atomic insight persistence is unavailable".into(),
        })
    }

    /// Delete only host-minted insight documents — those carrying a validated
    /// insight record — in this exact project/owner scope. Model- or
    /// user-authored titles/tags must never widen the purge: `purge_insights`
    /// is model-invocable, so a doc that merely looks like an insight must
    /// survive it.
    async fn purge_conversation_insights(
        &self,
        _project_id: ProjectId,
        _user_id: &str,
    ) -> Result<usize, EngineError> {
        Err(EngineError::Store {
            reason: "insight purge is unavailable".into(),
        })
    }
    async fn list_memory_docs(
        &self,
        project_id: ProjectId,
        user_id: &str,
    ) -> Result<Vec<MemoryDoc>, EngineError>;

    /// List memory docs visible to a user: their own docs + shared docs.
    ///
    /// This is the "shared space" pattern: admins can install skills and
    /// knowledge under the shared owner id, and they're visible to all users
    /// alongside their personal docs. Used for skill listing, context
    /// retrieval, and any place where shared knowledge should be accessible.
    async fn list_memory_docs_with_shared(
        &self,
        project_id: ProjectId,
        user_id: &str,
    ) -> Result<Vec<MemoryDoc>, EngineError> {
        if is_shared_owner(user_id) {
            return self.list_shared_memory_docs(project_id).await;
        }
        let mut docs = self.list_memory_docs(project_id, user_id).await?;
        docs.extend(self.list_shared_memory_docs(project_id).await?);
        Ok(docs)
    }

    async fn list_shared_memory_docs(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<MemoryDoc>, EngineError> {
        let mut docs = Vec::new();
        for owner_id in shared_owner_candidates() {
            docs.extend(self.list_memory_docs(project_id, owner_id).await?);
        }
        docs.sort_by_key(|doc| doc.id.0);
        docs.dedup_by_key(|doc| doc.id);
        Ok(docs)
    }

    // ── Capability lease operations ─────────────────────────

    async fn save_lease(&self, lease: &CapabilityLease) -> Result<(), EngineError>;
    async fn load_active_leases(
        &self,
        thread_id: ThreadId,
    ) -> Result<Vec<CapabilityLease>, EngineError>;
    async fn revoke_lease(&self, lease_id: LeaseId, reason: &str) -> Result<(), EngineError>;

    // ── Mission operations ───────────────────────────────────

    async fn save_mission(&self, mission: &Mission) -> Result<(), EngineError>;
    async fn load_mission(&self, id: MissionId) -> Result<Option<Mission>, EngineError>;
    async fn list_missions(
        &self,
        project_id: ProjectId,
        user_id: &str,
    ) -> Result<Vec<Mission>, EngineError>;
    async fn update_mission_status(
        &self,
        id: MissionId,
        status: MissionStatus,
    ) -> Result<(), EngineError>;

    /// Permanently remove a mission from persistence.
    ///
    /// Unlike `update_mission_status(Completed)`, this removes the record
    /// entirely so it no longer appears in list output.
    async fn delete_mission(&self, id: MissionId) -> Result<(), EngineError> {
        Err(EngineError::Store {
            reason: format!("Store::delete_mission not implemented for mission '{id}'"),
        })
    }

    /// List missions visible to a user: their own + shared missions.
    ///
    /// Shared learning missions (self-improvement, skill-extraction, etc.) are
    /// created under the shared owner id and should be visible/manageable by all
    /// users through the API.
    async fn list_missions_with_shared(
        &self,
        project_id: ProjectId,
        user_id: &str,
    ) -> Result<Vec<Mission>, EngineError> {
        if is_shared_owner(user_id) {
            return self.list_shared_missions(project_id).await;
        }
        let mut missions = self.list_missions(project_id, user_id).await?;
        missions.extend(self.list_shared_missions(project_id).await?);
        Ok(missions)
    }

    async fn list_shared_missions(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<Mission>, EngineError> {
        let mut missions = Vec::new();
        for owner_id in shared_owner_candidates() {
            missions.extend(self.list_missions(project_id, owner_id).await?);
        }
        missions.sort_by_key(|mission| mission.id.0);
        missions.dedup_by_key(|mission| mission.id);
        Ok(missions)
    }

    // ── Admin operations (system-level, cross-tenant) ──────────

    /// List all threads in a project regardless of user.
    /// Used by: recovery, background thread resume at startup.
    async fn list_all_threads(&self, project_id: ProjectId) -> Result<Vec<Thread>, EngineError> {
        Err(EngineError::Store {
            reason: format!("Store::list_all_threads not implemented for project '{project_id}'"),
        })
    }

    /// List all missions in a project regardless of user.
    /// Used by: cron ticker, event listener, bootstrap.
    async fn list_all_missions(&self, project_id: ProjectId) -> Result<Vec<Mission>, EngineError> {
        Err(EngineError::Store {
            reason: format!("Store::list_all_missions not implemented for project '{project_id}'"),
        })
    }

    /// Enumerate persisted threads across all projects, including projects
    /// other than the configured owner project (WI#301). The boot-time
    /// recovery sweep must not leave a non-owner project's in-flight threads
    /// as permanent `Running` zombies, and must not silently skip a project
    /// on a listing failure. Adapters whose thread index is not keyed by
    /// project should override this to enumerate threads directly.
    async fn list_all_threads_global(&self) -> Result<Vec<Thread>, EngineError> {
        let mut threads = Vec::new();
        for project in self.list_all_projects().await? {
            threads.extend(self.list_all_threads(project.id).await?);
        }
        Ok(threads)
    }

    /// Enumerate persisted missions across all projects, including ones that
    /// did not enter the in-memory active index during startup. Retry recovery
    /// must not silently drop a project's pending fires on a listing failure.
    async fn list_all_missions_global(&self) -> Result<Vec<Mission>, EngineError> {
        let mut missions = Vec::new();
        for project in self.list_all_projects().await? {
            missions.extend(self.list_all_missions(project.id).await?);
        }
        Ok(missions)
    }

    /// List all active missions across all projects/users.
    ///
    /// Used by startup bootstrap to load missions that don't belong to the
    /// configured owner project. Default implementation iterates all known
    /// projects. Per-project load failures are logged and skipped so a
    /// single broken project doesn't abort the global bootstrap.
    async fn list_all_active_missions_global(&self) -> Result<Vec<Mission>, EngineError> {
        let mut missions = Vec::new();
        let projects = self.list_all_projects().await?;
        for project in projects {
            match self.list_all_missions(project.id).await {
                Ok(p_missions) => missions.extend(p_missions),
                Err(e) => {
                    tracing::warn!(
                        project_id = %project.id,
                        error = %e,
                        "list_all_active_missions_global: skipping project with load error"
                    );
                }
            }
        }
        missions.retain(|m| m.status == MissionStatus::Active);
        Ok(missions)
    }
}
