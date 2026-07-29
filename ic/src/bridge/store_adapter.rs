//! Hybrid store adapter — workspace-backed persistence for engine state.
//!
//! Knowledge docs use frontmatter+markdown for human readability.
//! Runtime state uses JSON under `.runtime/` to stay out of the way.
//!
//! ## Workspace layout
//!
//! ```text
//! engine/
//! ├── README.md                            (auto-generated index)
//! ├── knowledge/{type}/{slug}--{id8}.md    (frontmatter + content)
//! ├── orchestrator/v{N}.py                 (Python orchestrator versions)
//! ├── orchestrator/failures.json
//! ├── orchestrator/codeact-preamble-overlay.md  (runtime prompt patches)
//! ├── missions/{slug}--{id8}.json
//! ├── projects/{slug}--{id8}.json
//! └── .runtime/                            (internal, not for browsing)
//!     ├── threads/active/{id}.json
//!     ├── threads/archive/{slug}.json      (compacted summaries)
//!     ├── conversations/{id}.json
//!     ├── leases/{id}.json
//!     ├── events/{thread_id}.json
//!     └── steps/{thread_id}.json
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;
use tracing::{debug, warn};
use uuid::Uuid;

use lunarwing_engine::{
    CapabilityLease, ConversationId, ConversationSurface, DocId, DocType, EngineError, LeaseId,
    MemoryDoc, Project, ProjectId, Step, Store, Thread, ThreadEvent, ThreadId, ThreadState,
    types::mission::{Mission, MissionId, MissionStatus},
};

use crate::secrets::{CreateSecretParams, SecretsStore};
use crate::workspace::{Workspace, WorkspaceEntry};

// ── Path constants ──────────────────────────────────────────

const KNOWLEDGE_PREFIX: &str = "engine/knowledge";
const ORCHESTRATOR_PREFIX: &str = "engine/orchestrator";
const PROJECTS_PREFIX: &str = "engine/projects";

const THREADS_PREFIX: &str = "engine/.runtime/threads/active";
const THREAD_ARCHIVE_PREFIX: &str = "engine/.runtime/threads/archive";
const STEPS_PREFIX: &str = "engine/.runtime/steps";
const EVENTS_PREFIX: &str = "engine/.runtime/events";
const LEASES_PREFIX: &str = "engine/.runtime/leases";
const CONVERSATIONS_PREFIX: &str = "engine/.runtime/conversations";

// Well-known titles for special-case routing (must match engine crate constants)
const ORCHESTRATOR_MAIN_TITLE: &str = "orchestrator:main";
const ORCHESTRATOR_FAILURES_TITLE: &str = "orchestrator:failures";
const PREAMBLE_OVERLAY_TITLE: &str = "prompt:codeact_preamble";
const ORCHESTRATOR_CODE_TAG: &str = "orchestrator_code";
const FIX_PATTERN_TITLE: &str = "fix_pattern_database";
const MISSION_WEBHOOK_SECRET_PROVIDER: &str = "lunarwing_engine_webhook";

#[derive(Clone, Debug, PartialEq, Eq)]
struct MissionSecretRef {
    user_id: String,
    name: String,
}

/// Workspace-backed engine store.
pub struct HybridStore {
    threads: RwLock<HashMap<ThreadId, Thread>>,
    steps: RwLock<HashMap<ThreadId, Vec<Step>>>,
    events: RwLock<HashMap<ThreadId, Vec<ThreadEvent>>>,
    projects: RwLock<HashMap<ProjectId, Project>>,
    conversations: RwLock<HashMap<ConversationId, ConversationSurface>>,
    leases: RwLock<HashMap<LeaseId, CapabilityLease>>,
    missions: RwLock<HashMap<MissionId, Mission>>,
    /// Tracks every workspace path found for each mission, including legacy duplicates.
    mission_paths: RwLock<HashMap<MissionId, HashSet<String>>>,
    mission_secret_refs: RwLock<HashMap<MissionId, MissionSecretRef>>,
    docs: RwLock<HashMap<DocId, MemoryDoc>>,
    /// Tracks current workspace path for each doc so renames can delete the old file.
    doc_paths: RwLock<HashMap<DocId, String>>,
    workspace: Option<Arc<Workspace>>,
    secrets_store: Option<Arc<dyn SecretsStore + Send + Sync>>,
}

impl HybridStore {
    #[cfg(test)]
    pub fn new(workspace: Option<Arc<Workspace>>) -> Self {
        Self::new_with_secrets_store(workspace, None)
    }

    pub fn new_with_secrets_store(
        workspace: Option<Arc<Workspace>>,
        secrets_store: Option<Arc<dyn SecretsStore + Send + Sync>>,
    ) -> Self {
        Self {
            threads: RwLock::new(HashMap::new()),
            steps: RwLock::new(HashMap::new()),
            events: RwLock::new(HashMap::new()),
            projects: RwLock::new(HashMap::new()),
            conversations: RwLock::new(HashMap::new()),
            leases: RwLock::new(HashMap::new()),
            missions: RwLock::new(HashMap::new()),
            mission_paths: RwLock::new(HashMap::new()),
            mission_secret_refs: RwLock::new(HashMap::new()),
            docs: RwLock::new(HashMap::new()),
            doc_paths: RwLock::new(HashMap::new()),
            workspace,
            secrets_store,
        }
    }

    /// Load persisted engine state from the workspace on startup.
    pub async fn load_state_from_workspace(&self) -> Result<(), EngineError> {
        let Some(ws) = self.workspace.as_ref() else {
            return Ok(());
        };

        self.load_knowledge_docs(ws).await;
        self.load_map(ws, PROJECTS_PREFIX, |project: Project| async {
            self.projects.write().await.insert(project.id, project);
        })
        .await;
        self.load_map(
            ws,
            CONVERSATIONS_PREFIX,
            |conversation: ConversationSurface| async {
                self.conversations
                    .write()
                    .await
                    .insert(conversation.id, conversation);
            },
        )
        .await;
        self.load_map(ws, THREADS_PREFIX, |thread: Thread| async {
            self.threads.write().await.insert(thread.id, thread);
        })
        .await;
        self.load_map(ws, STEPS_PREFIX, |steps: Vec<Step>| async {
            if let Some(thread_id) = steps.first().map(|step| step.thread_id) {
                self.steps.write().await.insert(thread_id, steps);
            }
        })
        .await;
        self.load_map(ws, EVENTS_PREFIX, |events: Vec<ThreadEvent>| async {
            if let Some(thread_id) = events.first().map(|event| event.thread_id) {
                self.events.write().await.insert(thread_id, events);
            }
        })
        .await;
        self.load_map(ws, LEASES_PREFIX, |lease: CapabilityLease| async {
            self.leases.write().await.insert(lease.id, lease);
        })
        .await;
        // Missions live under each project: engine/projects/{slug}/missions/{slug}/mission.json
        self.load_missions_from_projects(ws).await?;

        // Backfill archived threads referenced by missions but missing from the
        // active threads map (threads archived before the fix that preserves
        // stripped Thread objects in the active path).
        self.backfill_archived_threads(ws).await;

        let projects = self.projects.read().await.len();
        let conversations = self.conversations.read().await.len();
        let threads = self.threads.read().await.len();
        let steps = self.steps.read().await.len();
        let events = self.events.read().await.len();
        let leases = self.leases.read().await.len();
        let missions = self.missions.read().await.len();
        let docs = self.docs.read().await.len();

        debug!(
            projects,
            conversations,
            threads,
            steps,
            events,
            leases,
            missions,
            docs,
            "loaded engine state from workspace"
        );
        Ok(())
    }

    /// Evict terminal (Done/Failed) threads from in-memory caches.
    ///
    /// Full thread data (messages, events, steps) is **always preserved on
    /// disk** — LLM output is never deleted.  This method only removes old
    /// terminal threads from the in-memory maps to keep RAM bounded.
    /// `load_thread()` will lazy-reload from disk on the next access.
    ///
    /// Also writes a compact archive summary for human-browsable indexing
    /// and reconciles leases owned by terminal or missing threads before
    /// cleaning dead leases from memory. Lease files stay on disk for audit
    /// history — reconciliation persists `revoked`, so they reload as revoked
    /// and are filtered out rather than counted active again.
    ///
    /// The orphan predicate treats a lease whose thread is absent from the
    /// in-memory map as dead, which is only sound because the sole caller runs
    /// this immediately after `load_state_from_workspace()` has eagerly loaded
    /// every thread. Do not move this call after lazy eviction.
    pub async fn cleanup_terminal_state(&self, min_age: chrono::Duration) -> usize {
        let mut cleaned = 0;
        let now = chrono::Utc::now();

        let (known_thread_ids, terminal_thread_ids, referenced_lease_ids) = {
            let threads = self.threads.read().await;
            let known = threads.keys().copied().collect::<HashSet<_>>();
            let terminal = threads
                .values()
                .filter(|thread| {
                    matches!(
                        thread.state,
                        ThreadState::Done | ThreadState::Failed | ThreadState::Completed
                    )
                })
                .map(|thread| thread.id)
                .collect::<HashSet<_>>();
            let referenced = threads
                .values()
                .flat_map(|thread| thread.capability_leases.iter().copied())
                .collect::<HashSet<_>>();
            (known, terminal, referenced)
        };

        // Older versions left non-expiring leases valid after their owning
        // thread finished. Revoke those durably before evicting thread state.
        let (terminal_lease_ids, orphaned_lease_ids) = {
            let leases = self.leases.read().await;
            let mut terminal = Vec::new();
            let mut orphaned = Vec::new();
            for lease in leases.values().filter(|lease| lease.is_valid()) {
                if !known_thread_ids.contains(&lease.thread_id)
                    || !referenced_lease_ids.contains(&lease.id)
                {
                    orphaned.push(lease.id);
                } else if terminal_thread_ids.contains(&lease.thread_id) {
                    terminal.push(lease.id);
                }
            }
            (terminal, orphaned)
        };
        for lease_id in &terminal_lease_ids {
            if let Err(error) = self
                .revoke_lease(*lease_id, "owning thread reached terminal state")
                .await
            {
                warn!(?lease_id, %error, "failed to reconcile terminal thread lease");
            }
        }
        for lease_id in &orphaned_lease_ids {
            if let Err(error) = self
                .revoke_lease(
                    *lease_id,
                    "owning thread is missing or does not reference lease",
                )
                .await
            {
                warn!(?lease_id, %error, "failed to reconcile orphaned lease");
            }
        }

        // 1. Evict terminal threads from in-memory maps (disk files stay)
        let terminal: Vec<Thread> = self
            .threads
            .read()
            .await
            .values()
            .filter(|t| {
                matches!(
                    t.state,
                    ThreadState::Done | ThreadState::Failed | ThreadState::Completed
                ) && t
                    .completed_at
                    .or(Some(t.updated_at))
                    .is_some_and(|at| (now - at) > min_age)
            })
            .cloned()
            .collect();

        for thread in &terminal {
            // Write compact archive summary (for human-readable browsing)
            let slug = slugify(&thread.goal, &thread.id.0.to_string());
            let archive_path = format!("{THREAD_ARCHIVE_PREFIX}/{slug}.json");
            let summary = compact_thread_summary(thread);
            self.persist_json(archive_path, &summary).await;

            // Evict from in-memory maps only — disk files are never deleted.
            self.threads.write().await.remove(&thread.id);
            self.events.write().await.remove(&thread.id);
            self.steps.write().await.remove(&thread.id);
            cleaned += 1;
        }

        // 2. Evict dead leases from memory. Leases are granted without TTL and
        //    historically were never revoked, so without this they accumulate
        //    forever (the "active leases" leak). The reconciliation above has
        //    already persisted `revoked` for terminal/orphaned leases, so this
        //    is a memory-only eviction and the disk copy stays as audit history.
        let live_threads: std::collections::HashSet<ThreadId> = self
            .threads
            .read()
            .await
            .values()
            .filter(|t| {
                !matches!(
                    t.state,
                    ThreadState::Done | ThreadState::Failed | ThreadState::Completed
                )
            })
            .map(|t| t.id)
            .collect();
        let dead_leases: Vec<LeaseId> = self
            .leases
            .read()
            .await
            .iter()
            .filter(|(_, l)| l.revoked || !l.is_valid() || !live_threads.contains(&l.thread_id))
            .map(|(id, _)| *id)
            .collect();
        for lid in &dead_leases {
            self.leases.write().await.remove(lid);
            cleaned += 1;
        }

        if cleaned > 0 {
            debug!(
                threads_evicted = terminal.len(),
                leases_cleaned = dead_leases.len(),
                "evicted terminal state from memory (disk preserved)"
            );
        }

        cleaned
    }

    /// Generate `engine/README.md` with a summary of current engine state.
    pub async fn generate_engine_readme(&self) {
        let docs = self.docs.read().await;
        let threads = self.threads.read().await;
        let missions = self.missions.read().await;
        let leases = self.leases.read().await;

        let count_by_type = |dt: DocType| docs.values().filter(|d| d.doc_type == dt).count();
        let active_threads = threads
            .values()
            .filter(|t| !matches!(t.state, ThreadState::Done | ThreadState::Failed))
            .count();
        let active_leases = leases.values().filter(|l| l.is_valid()).count();

        // Count orchestrator versions
        let orch_versions = docs
            .values()
            .filter(|d| {
                d.title == ORCHESTRATOR_MAIN_TITLE
                    && d.tags.contains(&ORCHESTRATOR_CODE_TAG.to_string())
            })
            .count();

        let mut readme = format!(
            "# Engine State\n\n\
             Last updated: {}\n\n",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        );

        readme.push_str("## Knowledge (`engine/knowledge/`)\n\n");
        readme.push_str(&format!(
            "- **{} lessons** — learned rules\n",
            count_by_type(DocType::Lesson)
        ));
        readme.push_str(&format!(
            "- **{} skills** — extracted procedures\n",
            count_by_type(DocType::Skill)
        ));
        readme.push_str(&format!(
            "- **{} summaries** — thread completion records\n",
            count_by_type(DocType::Summary)
        ));
        readme.push_str(&format!(
            "- **{} specs** — specifications\n",
            count_by_type(DocType::Spec)
        ));
        readme.push_str(&format!(
            "- **{} issues** — known problems\n",
            count_by_type(DocType::Issue)
        ));

        readme.push_str(&format!(
            "\n## Orchestrator (`engine/orchestrator/`)\n\n\
             - {} version(s) stored\n",
            orch_versions
        ));

        readme.push_str("\n## Missions (`engine/missions/`)\n\n");
        for m in missions.values() {
            readme.push_str(&format!(
                "- **{}** ({:?}) — {}\n",
                m.name,
                m.status,
                truncate_for_readme(&m.goal, 80)
            ));
        }

        readme.push_str(&format!(
            "\n## Runtime (`engine/.runtime/`)\n\n\
             - {} active thread(s)\n\
             - {} active lease(s)\n",
            active_threads, active_leases,
        ));

        self.persist_text("engine/README.md".to_string(), &readme)
            .await;
    }

    // ── Internal helpers ────────────────────────────────────

    async fn load_knowledge_docs(&self, ws: &Workspace) {
        // Knowledge docs can be .md (frontmatter) or .json (legacy).
        // Also load special docs from orchestrator/ and prompts/ paths.
        let search_prefixes = [KNOWLEDGE_PREFIX, ORCHESTRATOR_PREFIX];

        for prefix in search_prefixes {
            for entry in self
                .file_entries(ws, prefix, &[".md", ".json", ".py"])
                .await
            {
                match ws.read(&entry.path).await {
                    Ok(doc) => {
                        // Try frontmatter format first, then JSON
                        let parsed = deserialize_knowledge_doc(&doc.content)
                            .or_else(|| serde_json::from_str::<MemoryDoc>(&doc.content).ok());
                        if let Some(memory_doc) = parsed {
                            self.doc_paths
                                .write()
                                .await
                                .insert(memory_doc.id, entry.path.clone());
                            self.docs.write().await.insert(memory_doc.id, memory_doc);
                        } else {
                            debug!(path = %entry.path, "skipped non-doc file in engine");
                        }
                    }
                    Err(e) => debug!(path = %entry.path, "failed to read engine doc: {e}"),
                }
            }
        }
    }

    async fn load_map<T, F, Fut>(&self, ws: &Workspace, directory: &str, on_value: F)
    where
        T: DeserializeOwned,
        F: Fn(T) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        for entry in self.file_entries(ws, directory, &[".json"]).await {
            match ws.read(&entry.path).await {
                Ok(doc) => match serde_json::from_str::<T>(&doc.content) {
                    Ok(value) => on_value(value).await,
                    Err(e) => debug!(path = %entry.path, "failed to parse engine state: {e}"),
                },
                Err(e) => debug!(path = %entry.path, "failed to read engine state: {e}"),
            }
        }
    }

    /// List files under a directory, recursing one level into subdirectories.
    async fn file_entries(
        &self,
        ws: &Workspace,
        directory: &str,
        extensions: &[&str],
    ) -> Vec<WorkspaceEntry> {
        let top = match ws.list(directory).await {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };

        let mut files = Vec::new();
        for entry in top {
            if entry.is_directory {
                if let Ok(children) = ws.list(&entry.path).await {
                    files.extend(children.into_iter().filter(|child| {
                        !child.is_directory
                            && extensions.iter().any(|ext| child.path.ends_with(ext))
                    }));
                }
            } else if extensions.iter().any(|ext| entry.path.ends_with(ext)) {
                files.push(entry);
            }
        }
        files
    }

    async fn persist_json<T: serde::Serialize>(&self, path: String, value: &T) {
        if let Err(error) = self.persist_json_result(&path, value).await {
            debug!(path = %path, %error, "failed to persist engine state");
        }
    }

    async fn persist_json_result<T: serde::Serialize>(
        &self,
        path: &str,
        value: &T,
    ) -> Result<(), EngineError> {
        let Some(ws) = self.workspace.as_ref() else {
            return Ok(());
        };
        let json = serde_json::to_string_pretty(value).map_err(|error| EngineError::Store {
            reason: format!("failed to serialize engine state at {path}: {error}"),
        })?;
        ws.write(path, &json)
            .await
            .map(|_| ())
            .map_err(|error| EngineError::Store {
                reason: format!("failed to persist engine state at {path}: {error}"),
            })
    }

    async fn persist_text(&self, path: String, content: &str) {
        let Some(ws) = self.workspace.as_ref() else {
            return;
        };
        if let Err(e) = ws.write(&path, content).await {
            debug!(path = %path, "failed to persist engine text: {e}");
        }
    }

    fn mission_secret_prefix(id: MissionId) -> String {
        format!("engine_v2_mission_webhook_{}_", id.0.simple())
    }

    fn mission_secret_ref_from_json(value: &serde_json::Value) -> Option<String> {
        value
            .pointer("/cadence/Webhook/secret_ref")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    }

    fn validate_mission_secret_ref(id: MissionId, name: &str) -> Result<(), EngineError> {
        if name.starts_with(&Self::mission_secret_prefix(id)) {
            return Ok(());
        }
        Err(EngineError::Store {
            reason: format!("invalid webhook secret reference for mission {id}"),
        })
    }

    fn mission_webhook_secret(mission: &Mission) -> Option<&str> {
        match &mission.cadence {
            lunarwing_engine::MissionCadence::Webhook { secret, .. } => secret.as_deref(),
            _ => None,
        }
    }

    fn mission_json(
        mission: &Mission,
        secret_ref: Option<&MissionSecretRef>,
    ) -> Result<String, EngineError> {
        let mut value = serde_json::to_value(mission).map_err(|error| EngineError::Store {
            reason: format!("failed to serialize mission {}: {error}", mission.id),
        })?;
        if let Some(secret_ref) = secret_ref {
            let webhook = value
                .pointer_mut("/cadence/Webhook")
                .and_then(serde_json::Value::as_object_mut)
                .ok_or_else(|| EngineError::Store {
                    reason: format!(
                        "mission {} secret reference has no webhook cadence",
                        mission.id
                    ),
                })?;
            webhook.remove("secret");
            webhook.insert(
                "secret_ref".to_string(),
                serde_json::Value::String(secret_ref.name.clone()),
            );
        }
        serde_json::to_string_pretty(&value).map_err(|error| EngineError::Store {
            reason: format!("failed to serialize mission {}: {error}", mission.id),
        })
    }

    async fn create_mission_secret(
        &self,
        mission: &Mission,
        plaintext: &str,
    ) -> Result<MissionSecretRef, EngineError> {
        let store = self
            .secrets_store
            .as_ref()
            .ok_or_else(|| EngineError::Store {
                reason: format!(
                    "secure secrets storage is required for webhook mission {}",
                    mission.id
                ),
            })?;
        let name = format!(
            "{}{}",
            Self::mission_secret_prefix(mission.id),
            Uuid::new_v4().simple()
        );
        store
            .create(
                &mission.user_id,
                CreateSecretParams::new(&name, plaintext)
                    .with_provider(MISSION_WEBHOOK_SECRET_PROVIDER),
            )
            .await
            .map_err(|error| EngineError::Store {
                reason: format!(
                    "failed to encrypt webhook secret for mission {}: {error}",
                    mission.id
                ),
            })?;
        Ok(MissionSecretRef {
            user_id: mission.user_id.clone(),
            name,
        })
    }

    async fn remove_mission_secret(&self, mission_id: MissionId, secret_ref: &MissionSecretRef) {
        let Some(store) = self.secrets_store.as_ref() else {
            return;
        };
        if let Err(error) = store.delete(&secret_ref.user_id, &secret_ref.name).await {
            warn!(%mission_id, %error, "failed to remove obsolete encrypted mission secret");
        }
    }

    async fn prepare_mission_secret(
        &self,
        mission: &Mission,
        previous: Option<&MissionSecretRef>,
    ) -> Result<(Option<MissionSecretRef>, Option<MissionSecretRef>), EngineError> {
        let Some(plaintext) = Self::mission_webhook_secret(mission) else {
            return Ok((None, None));
        };
        if self.workspace.is_none() {
            return Ok((None, None));
        }
        if let Some(previous) = previous
            && previous.user_id == mission.user_id
        {
            Self::validate_mission_secret_ref(mission.id, &previous.name)?;
            let decrypted = self
                .secrets_store
                .as_ref()
                .ok_or_else(|| EngineError::Store {
                    reason: format!(
                        "secure secrets storage is required for webhook mission {}",
                        mission.id
                    ),
                })?
                .get_decrypted(&previous.user_id, &previous.name)
                .await
                .map_err(|error| EngineError::Store {
                    reason: format!(
                        "failed to read webhook secret for mission {}: {error}",
                        mission.id
                    ),
                })?;
            if bool::from(decrypted.expose().as_bytes().ct_eq(plaintext.as_bytes())) {
                return Ok((Some(previous.clone()), None));
            }
        }
        let created = self.create_mission_secret(mission, plaintext).await?;
        Ok((Some(created.clone()), Some(created)))
    }

    async fn persist_mission_json(
        &self,
        path: &str,
        mission: &Mission,
        secret_ref: Option<&MissionSecretRef>,
    ) -> Result<(), EngineError> {
        let Some(ws) = self.workspace.as_ref() else {
            return Ok(());
        };
        let json = Self::mission_json(mission, secret_ref)?;
        ws.write(path, &json)
            .await
            .map_err(|error| EngineError::Store {
                reason: format!("failed to persist mission {}: {error}", mission.id),
            })?;
        Ok(())
    }

    async fn hydrate_mission_secret(
        &self,
        path: &str,
        raw: &serde_json::Value,
        mut mission: Mission,
    ) -> Result<(Mission, Option<MissionSecretRef>), EngineError> {
        if !matches!(
            &mission.cadence,
            lunarwing_engine::MissionCadence::Webhook { .. }
        ) {
            return Ok((mission, None));
        }

        if let Some(name) = Self::mission_secret_ref_from_json(raw) {
            Self::validate_mission_secret_ref(mission.id, &name)?;
            let secret_ref = MissionSecretRef {
                user_id: mission.user_id.clone(),
                name,
            };
            let decrypted = self
                .secrets_store
                .as_ref()
                .ok_or_else(|| EngineError::Store {
                    reason: format!(
                        "secure secrets storage is required to load webhook mission {}",
                        mission.id
                    ),
                })?
                .get_decrypted(&secret_ref.user_id, &secret_ref.name)
                .await
                .map_err(|error| EngineError::Store {
                    reason: format!(
                        "failed to decrypt webhook secret for mission {}: {error}",
                        mission.id
                    ),
                })?;
            if let lunarwing_engine::MissionCadence::Webhook { secret, .. } = &mut mission.cadence {
                *secret = Some(decrypted.expose().to_string());
            }
            if raw.pointer("/cadence/Webhook/secret").is_some() {
                self.persist_mission_json(path, &mission, Some(&secret_ref))
                    .await?;
            }
            return Ok((mission, Some(secret_ref)));
        }

        let Some(plaintext) = Self::mission_webhook_secret(&mission).map(str::to_string) else {
            return Ok((mission, None));
        };
        let secret_ref = self.create_mission_secret(&mission, &plaintext).await?;
        if let Err(error) = self
            .persist_mission_json(path, &mission, Some(&secret_ref))
            .await
        {
            self.remove_mission_secret(mission.id, &secret_ref).await;
            return Err(error);
        }
        Ok((mission, Some(secret_ref)))
    }

    async fn cleanup_stale_mission(
        &self,
        ws: &Workspace,
        path: &str,
        mut mission: Mission,
    ) -> Result<bool, EngineError> {
        match ws.delete(path).await {
            Ok(()) => Ok(true),
            Err(delete_error) => {
                // If stale deletion is unavailable, fail the duplicate closed and
                // overwrite it without plaintext or a secret reference.
                mission.status = MissionStatus::Failed;
                self.persist_mission_json(path, &mission, None)
                    .await
                    .map_err(|scrub_error| EngineError::Store {
                        reason: format!(
                            "failed to delete stale mission {path}: {delete_error}; \
                             failed to scrub stale mission: {scrub_error}"
                        ),
                    })?;
                warn!(
                    %path,
                    %delete_error,
                    "stale mission could not be deleted; scrubbed and disabled instead"
                );
                Ok(false)
            }
        }
    }

    /// Load missions from within each project directory.
    ///
    /// Scans `engine/projects/*/missions/*/mission.json`.
    async fn load_missions_from_projects(&self, ws: &Workspace) -> Result<(), EngineError> {
        let project_dirs = match ws.list_primary(PROJECTS_PREFIX).await {
            Ok(entries) => entries,
            Err(_) => return Ok(()),
        };

        let mut candidates = Vec::new();
        for proj_entry in project_dirs {
            if !proj_entry.is_directory {
                continue;
            }
            let missions_dir = format!("{}/missions", proj_entry.path);
            let mission_dirs = match ws.list_primary(&missions_dir).await {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for mission_entry in mission_dirs {
                if !mission_entry.is_directory {
                    continue;
                }
                let mission_file = format!("{}/mission.json", mission_entry.path);
                if let Ok(doc) = ws.read_primary(&mission_file).await {
                    match serde_json::from_str::<serde_json::Value>(&doc.content) {
                        Ok(raw) => match serde_json::from_value::<Mission>(raw.clone()) {
                            Ok(mission) => {
                                candidates.push((mission_file, raw, mission));
                            }
                            Err(e) => {
                                debug!(path = %mission_file, "failed to parse mission: {e}")
                            }
                        },
                        Err(e) => {
                            debug!(path = %mission_file, "failed to parse mission JSON: {e}")
                        }
                    }
                }
            }
        }

        // Select the authoritative document before secret hydration. A broken
        // newest secret must not reactivate an older duplicate mission.
        candidates.sort_by(|(left, _, _), (right, _, _)| left.cmp(right));
        let mut selected: HashMap<MissionId, (String, serde_json::Value, Mission)> = HashMap::new();
        let mut discovered_paths: HashMap<MissionId, HashSet<String>> = HashMap::new();
        let mut stale_candidates = Vec::new();
        for (path, raw, mission) in candidates {
            discovered_paths
                .entry(mission.id)
                .or_default()
                .insert(path.clone());
            let replace = selected
                .get(&mission.id)
                .is_none_or(|(_, _, current)| mission.updated_at >= current.updated_at);
            if replace {
                if let Some(previous) = selected.insert(mission.id, (path, raw, mission)) {
                    stale_candidates.push(previous);
                }
            } else {
                stale_candidates.push((path, raw, mission));
            }
        }

        for (path, _, mission) in stale_candidates {
            let mission_id = mission.id;
            if self.cleanup_stale_mission(ws, &path, mission).await?
                && let Some(paths) = discovered_paths.get_mut(&mission_id)
            {
                paths.remove(&path);
            }
        }

        let mut loaded = Vec::new();
        for (path, raw, mission) in selected.into_values() {
            match self.hydrate_mission_secret(&path, &raw, mission).await {
                Ok((mission, secret_ref)) => loaded.push((mission, secret_ref)),
                Err(error) => warn!(
                    %path,
                    %error,
                    "skipping mission whose webhook secret could not be loaded"
                ),
            }
        }

        let mut missions = self.missions.write().await;
        let mut paths = self.mission_paths.write().await;
        let mut secret_refs = self.mission_secret_refs.write().await;
        for (mission, secret_ref) in loaded {
            if let Some(mission_paths) = discovered_paths.remove(&mission.id) {
                paths.insert(mission.id, mission_paths);
            }
            if let Some(secret_ref) = secret_ref {
                secret_refs.insert(mission.id, secret_ref);
            } else {
                secret_refs.remove(&mission.id);
            }
            missions.insert(mission.id, mission);
        }
        Ok(())
    }

    /// Backfill threads referenced by missions but not yet in the in-memory map.
    ///
    /// Tries the full thread file first (active path in DB), then falls back to
    /// archive summaries for threads that were deleted before data-retention was
    /// fixed.
    async fn backfill_archived_threads(&self, ws: &Workspace) {
        // Collect thread IDs referenced by missions but missing from threads map
        let missions = self.missions.read().await.clone();
        let threads = self.threads.read().await;
        let missing: Vec<ThreadId> = missions
            .values()
            .flat_map(|m| m.thread_history.iter().copied())
            .filter(|tid| !threads.contains_key(tid))
            .collect();
        drop(threads);

        if missing.is_empty() {
            return;
        }

        let mut backfilled = 0usize;

        // First pass: try loading full Thread from active path in DB
        let mut still_missing = Vec::new();
        for tid in &missing {
            if let Ok(doc) = ws.read(&thread_path(*tid)).await
                && let Ok(thread) = serde_json::from_str::<Thread>(&doc.content)
            {
                self.threads.write().await.insert(thread.id, thread);
                backfilled += 1;
            } else {
                still_missing.push(tid.0.to_string());
            }
        }

        // Second pass: fall back to archive summaries for legacy-deleted threads
        if !still_missing.is_empty() {
            let missing_set: std::collections::HashSet<String> =
                still_missing.into_iter().collect();
            if let Ok(archive_entries) = ws.list(THREAD_ARCHIVE_PREFIX).await {
                for entry in archive_entries {
                    if entry.is_directory {
                        continue;
                    }
                    let Ok(doc) = ws.read(&entry.path).await else {
                        continue;
                    };
                    if let Ok(summary) = serde_json::from_str::<ThreadArchiveSummary>(&doc.content)
                        && missing_set.contains(&summary.thread_id)
                        && let Some(thread) = thread_from_archive(&summary)
                    {
                        self.threads.write().await.insert(thread.id, thread);
                        backfilled += 1;
                    }
                }
            }
        }

        if backfilled > 0 {
            debug!(backfilled, "backfilled mission threads from database");
        }
    }

    /// Get the project slug for a project_id, falling back to a short UUID.
    async fn project_slug(&self, project_id: ProjectId) -> String {
        self.projects
            .read()
            .await
            .get(&project_id)
            .map(|p| slugify(&p.name, &p.id.0.to_string()))
            .unwrap_or_else(|| {
                let short = &project_id.0.to_string()[..8];
                format!("unknown--{short}")
            })
    }

    async fn delete_workspace_file(&self, path: &str) {
        let Some(ws) = self.workspace.as_ref() else {
            return;
        };
        if let Err(e) = ws.delete(path).await {
            debug!(path = %path, "failed to delete engine file: {e}");
        }
    }

    /// Persist a MemoryDoc to workspace. Knowledge docs use frontmatter+markdown,
    /// special docs (orchestrator, prompts) use their native format, and internal
    /// docs use JSON.
    async fn persist_doc(&self, doc: &MemoryDoc) {
        let new_path = doc_workspace_path(doc);

        // If the doc previously existed at a different path, delete the old one
        if let Some(ref old) = self.doc_paths.read().await.get(&doc.id).cloned()
            && *old != new_path
        {
            self.delete_workspace_file(old).await;
        }

        // Choose serialization format based on path
        let content = if is_orchestrator_code_path(&new_path) {
            // Orchestrator Python: store raw content (the Python source code)
            doc.content.clone()
        } else if new_path.ends_with(".md") {
            // Knowledge docs and prompt overlays: frontmatter + content
            serialize_knowledge_doc(doc)
        } else {
            // Everything else: JSON
            match serde_json::to_string_pretty(doc) {
                Ok(json) => json,
                Err(e) => {
                    debug!(path = %new_path, "failed to serialize doc: {e}");
                    return;
                }
            }
        };

        self.persist_text(new_path.clone(), &content).await;
        self.doc_paths.write().await.insert(doc.id, new_path);
    }
}

// ── Path helpers ────────────────────────────────────────────

/// Map a MemoryDoc to its workspace path based on title and type.
fn doc_workspace_path(doc: &MemoryDoc) -> String {
    let id_str = doc.id.0.to_string();

    // Orchestrator code versions → engine/orchestrator/v{N}.py
    if doc.title == ORCHESTRATOR_MAIN_TITLE && doc.tags.contains(&ORCHESTRATOR_CODE_TAG.to_string())
    {
        let version = doc
            .metadata
            .get("version")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        return format!("{ORCHESTRATOR_PREFIX}/v{version}.py");
    }

    // Orchestrator failure tracker → engine/orchestrator/failures.json
    if doc.title == ORCHESTRATOR_FAILURES_TITLE {
        return format!("{ORCHESTRATOR_PREFIX}/failures.json");
    }

    // Prompt overlays → engine/prompts/{slug}.md
    if doc.title == PREAMBLE_OVERLAY_TITLE {
        return format!("{ORCHESTRATOR_PREFIX}/codeact-preamble-overlay.md");
    }

    // Fix pattern database → engine/knowledge/notes/{slug}.md
    if doc.title == FIX_PATTERN_TITLE {
        let slug = slugify(&doc.title, &id_str);
        return format!("{KNOWLEDGE_PREFIX}/notes/{slug}.md");
    }

    // Knowledge docs → engine/knowledge/{type}/{slug}.md
    let type_dir = match doc.doc_type {
        DocType::Summary => "summaries",
        DocType::Lesson => "lessons",
        DocType::Issue => "issues",
        DocType::Spec => "specs",
        DocType::Note => "notes",
        DocType::Skill => "skills",
        DocType::Plan => "plans",
    };
    let slug = slugify(&doc.title, &id_str);
    format!("{KNOWLEDGE_PREFIX}/{type_dir}/{slug}.md")
}

fn is_orchestrator_code_path(path: &str) -> bool {
    path.starts_with(ORCHESTRATOR_PREFIX) && path.ends_with(".py")
}

/// Check if a MemoryDoc is a protected orchestrator or prompt overlay document.
fn is_protected_orchestrator_doc(doc: &MemoryDoc) -> bool {
    doc.title.starts_with("orchestrator:") || doc.title.starts_with("prompt:")
}

fn project_dir(name: &str, project_id: ProjectId) -> String {
    let slug = slugify(name, &project_id.0.to_string());
    format!("{PROJECTS_PREFIX}/{slug}")
}

fn project_path(name: &str, project_id: ProjectId) -> String {
    format!("{}/project.json", project_dir(name, project_id))
}

fn thread_path(thread_id: ThreadId) -> String {
    format!("{THREADS_PREFIX}/{}.json", thread_id.0)
}

fn conversation_path(conversation_id: ConversationId) -> String {
    format!("{CONVERSATIONS_PREFIX}/{}.json", conversation_id.0)
}

fn step_path(thread_id: ThreadId) -> String {
    format!("{STEPS_PREFIX}/{}.json", thread_id.0)
}

fn event_path(thread_id: ThreadId) -> String {
    format!("{EVENTS_PREFIX}/{}.json", thread_id.0)
}

fn lease_path(lease_id: LeaseId) -> String {
    format!("{LEASES_PREFIX}/{}.json", lease_id.0)
}

fn mission_dir(project_slug: &str, name: &str, mission_id: MissionId) -> String {
    let slug = slugify(name, &mission_id.0.to_string());
    format!("{PROJECTS_PREFIX}/{project_slug}/missions/{slug}")
}

fn mission_path(project_slug: &str, name: &str, mission_id: MissionId) -> String {
    format!(
        "{}/mission.json",
        mission_dir(project_slug, name, mission_id)
    )
}

// ── Slugify ─────────────────────────────────────────────────

/// Create a human-readable filename slug from a title with a short ID suffix.
///
/// `"Validate tool names before first call"` + `"65c9f5cd-..."` →
/// `"validate-tool-names-before-first-call--65c9f5cd"`
fn slugify(title: &str, id: &str) -> String {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();

    // Collapse runs of dashes and trim
    let mut collapsed = String::with_capacity(slug.len());
    let mut prev_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_dash && !collapsed.is_empty() {
                collapsed.push('-');
            }
            prev_dash = true;
        } else {
            collapsed.push(c);
            prev_dash = false;
        }
    }
    let collapsed = collapsed.trim_end_matches('-');

    // Truncate slug to 60 chars, append 8-char ID suffix
    let max_slug = 60;
    let truncated = if collapsed.len() > max_slug {
        // Don't cut in the middle of a word — find last dash before limit
        match collapsed[..max_slug].rfind('-') {
            Some(pos) if pos > 20 => &collapsed[..pos],
            _ => &collapsed[..max_slug],
        }
    } else {
        collapsed
    };

    let short_id = if id.len() >= 8 { &id[..8] } else { id };
    format!("{truncated}--{short_id}")
}

// ── Frontmatter serialization ───────────────────────────────

/// Serialize a MemoryDoc as YAML frontmatter + markdown content.
fn serialize_knowledge_doc(doc: &MemoryDoc) -> String {
    let mut frontmatter = String::from("---\n");
    frontmatter.push_str(&format!("id: \"{}\"\n", doc.id.0));
    frontmatter.push_str(&format!("doc_type: \"{:?}\"\n", doc.doc_type));
    frontmatter.push_str(&format!("title: \"{}\"\n", doc.title.replace('"', "\\\"")));
    if !doc.tags.is_empty() {
        frontmatter.push_str(&format!(
            "tags: [{}]\n",
            doc.tags
                .iter()
                .map(|t| format!("\"{t}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(ref tid) = doc.source_thread_id {
        frontmatter.push_str(&format!("source_thread: \"{}\"\n", tid.0));
    }
    frontmatter.push_str(&format!("created: \"{}\"\n", doc.created_at.to_rfc3339()));
    frontmatter.push_str(&format!("updated: \"{}\"\n", doc.updated_at.to_rfc3339()));
    if doc.metadata != serde_json::json!({})
        && let Ok(meta_str) = serde_json::to_string(&doc.metadata)
    {
        frontmatter.push_str(&format!("metadata: {meta_str}\n"));
    }
    frontmatter.push_str("---\n\n");
    frontmatter.push_str(&doc.content);
    frontmatter
}

/// Deserialize a frontmatter+markdown string back to a MemoryDoc.
fn deserialize_knowledge_doc(content: &str) -> Option<MemoryDoc> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return None;
    }

    // Find closing ---
    // All slice points are at ASCII boundaries (---, \n) so UTF-8 safe.
    let after_first = content.get(3..)?;
    let nl_pos = after_first.find('\n')?;
    let after_first_line = after_first.get(nl_pos + 1..)?;
    let yaml_end = after_first_line.find("\n---")?;
    let yaml_str = after_first_line.get(..yaml_end)?;
    let body_start = yaml_end + 4; // skip \n---
    let body = after_first_line.get(body_start..)?.trim_start_matches('\n');

    // Parse YAML frontmatter
    let yaml: serde_json::Value = serde_norway::from_str(yaml_str).ok()?;

    let id_str = yaml.get("id")?.as_str()?;
    let id = uuid::Uuid::parse_str(id_str).ok()?;
    let title = yaml.get("title")?.as_str()?.to_string();

    let doc_type_str = yaml
        .get("doc_type")
        .and_then(|v| v.as_str())
        .unwrap_or("Note");
    let doc_type = match doc_type_str {
        "Summary" => DocType::Summary,
        "Lesson" => DocType::Lesson,
        "Issue" => DocType::Issue,
        "Spec" => DocType::Spec,
        "Skill" => DocType::Skill,
        "Plan" => DocType::Plan,
        _ => DocType::Note,
    };

    let tags: Vec<String> = yaml
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let source_thread_id = yaml
        .get("source_thread")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .map(ThreadId);

    let created_at = yaml
        .get("created")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(chrono::Utc::now);

    let updated_at = yaml
        .get("updated")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(chrono::Utc::now);

    let metadata = yaml
        .get("metadata")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    // project_id is not in frontmatter — use nil UUID (assigned at load time)
    Some(MemoryDoc {
        id: DocId(id),
        project_id: ProjectId(uuid::Uuid::nil()),
        user_id: "legacy".to_string(),
        doc_type,
        title,
        content: body.to_string(),
        source_thread_id,
        tags,
        metadata,
        created_at,
        updated_at,
    })
}

// ── Thread archival ─────────────────────────────────────────

/// Compact summary of a completed thread for archival.
#[derive(serde::Serialize, serde::Deserialize)]
struct ThreadArchiveSummary {
    thread_id: String,
    goal: String,
    state: String,
    created_at: String,
    completed_at: Option<String>,
    step_count: usize,
    total_tokens: u64,
    #[serde(default)]
    outcome_preview: String,
}

fn compact_thread_summary(thread: &Thread) -> ThreadArchiveSummary {
    // Extract last assistant message as outcome preview
    let outcome = thread
        .messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, lunarwing_engine::MessageRole::Assistant))
        .map(|m| truncate_for_readme(&m.content, 200))
        .unwrap_or_default();

    ThreadArchiveSummary {
        thread_id: thread.id.0.to_string(),
        goal: truncate_for_readme(&thread.goal, 200),
        state: format!("{:?}", thread.state),
        created_at: thread.created_at.to_rfc3339(),
        completed_at: thread.completed_at.map(|dt| dt.to_rfc3339()),
        step_count: thread.step_count,
        total_tokens: thread.total_tokens_used,
        outcome_preview: outcome,
    }
}

/// Reconstruct a minimal Thread from an archive summary (for mission detail pages).
fn thread_from_archive(summary: &ThreadArchiveSummary) -> Option<Thread> {
    let id = uuid::Uuid::parse_str(&summary.thread_id).ok()?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&summary.created_at)
        .ok()?
        .with_timezone(&chrono::Utc);
    let completed_at = summary
        .completed_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let state = match summary.state.as_str() {
        "Done" => ThreadState::Done,
        "Failed" => ThreadState::Failed,
        "Completed" => ThreadState::Completed,
        _ => ThreadState::Done,
    };
    Some(Thread {
        id: ThreadId(id),
        goal: summary.goal.clone(),
        thread_type: lunarwing_engine::ThreadType::Mission,
        state,
        project_id: lunarwing_engine::ProjectId(uuid::Uuid::nil()),
        user_id: "default".to_string(),
        parent_id: None,
        config: lunarwing_engine::ThreadConfig::default(),
        messages: Vec::new(),
        internal_messages: Vec::new(),
        events: Vec::new(),
        capability_leases: Vec::new(),
        metadata: serde_json::Value::Object(serde_json::Map::new()),
        created_at,
        updated_at: completed_at.unwrap_or(created_at),
        completed_at,
        step_count: summary.step_count,
        total_tokens_used: summary.total_tokens,
        total_cost_usd: 0.0,
    })
}

fn truncate_for_readme(s: &str, max: usize) -> String {
    let trimmed = s.trim().replace('\n', " ");
    if trimmed.chars().count() <= max {
        trimmed
    } else {
        let truncated: String = trimmed.chars().take(max).collect();
        format!("{truncated}...")
    }
}

// ── Store trait implementation ───────────────────────────────

#[async_trait::async_trait]
impl Store for HybridStore {
    async fn save_thread(&self, thread: &Thread) -> Result<(), EngineError> {
        self.persist_json_result(&thread_path(thread.id), thread)
            .await?;
        self.threads.write().await.insert(thread.id, thread.clone());
        Ok(())
    }

    async fn load_thread(&self, id: ThreadId) -> Result<Option<Thread>, EngineError> {
        // Fast path: check in-memory cache
        if let Some(thread) = self.threads.read().await.get(&id).cloned() {
            return Ok(Some(thread));
        }
        // Slow path: reload from database (thread may have been evicted from memory)
        if let Some(ws) = self.workspace.as_ref()
            && let Ok(doc) = ws.read(&thread_path(id)).await
            && let Ok(thread) = serde_json::from_str::<Thread>(&doc.content)
        {
            self.threads.write().await.insert(thread.id, thread.clone());
            return Ok(Some(thread));
        }
        Ok(None)
    }

    async fn list_threads(
        &self,
        project_id: ProjectId,
        user_id: &str,
    ) -> Result<Vec<Thread>, EngineError> {
        Ok(self
            .threads
            .read()
            .await
            .values()
            .filter(|thread| thread.project_id == project_id && thread.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn update_thread_state(
        &self,
        id: ThreadId,
        state: ThreadState,
    ) -> Result<(), EngineError> {
        let updated = {
            let mut threads = self.threads.write().await;
            if let Some(thread) = threads.get_mut(&id) {
                thread.state = state;
                Some(thread.clone())
            } else {
                None
            }
        };
        if let Some(thread) = updated.as_ref() {
            self.persist_json(thread_path(id), thread).await;
        }
        Ok(())
    }

    async fn save_step(&self, step: &Step) -> Result<(), EngineError> {
        let snapshot = {
            let mut steps = self.steps.write().await;
            let thread_steps = steps.entry(step.thread_id).or_default();
            if let Some(existing) = thread_steps
                .iter_mut()
                .find(|existing| existing.id == step.id)
            {
                *existing = step.clone();
            } else {
                thread_steps.push(step.clone());
                thread_steps.sort_by_key(|saved| saved.sequence);
            }
            thread_steps.clone()
        };
        self.persist_json(step_path(step.thread_id), &snapshot)
            .await;
        Ok(())
    }

    async fn load_steps(&self, thread_id: ThreadId) -> Result<Vec<Step>, EngineError> {
        if let Some(steps) = self.steps.read().await.get(&thread_id).cloned() {
            return Ok(steps);
        }
        // Reload from database (may have been evicted from memory)
        if let Some(ws) = self.workspace.as_ref()
            && let Ok(doc) = ws.read(&step_path(thread_id)).await
            && let Ok(steps) = serde_json::from_str::<Vec<Step>>(&doc.content)
        {
            self.steps.write().await.insert(thread_id, steps.clone());
            return Ok(steps);
        }
        Ok(Vec::new())
    }

    async fn append_events(&self, events: &[ThreadEvent]) -> Result<(), EngineError> {
        let mut grouped: HashMap<ThreadId, Vec<ThreadEvent>> = HashMap::new();
        for event in events {
            grouped
                .entry(event.thread_id)
                .or_default()
                .push(event.clone());
        }

        for (thread_id, new_events) in grouped {
            let snapshot = {
                let mut stored = self.events.write().await;
                let thread_events = stored.entry(thread_id).or_default();
                for event in new_events {
                    if !thread_events.iter().any(|existing| existing.id == event.id) {
                        thread_events.push(event);
                    }
                }
                thread_events.sort_by_key(|event| event.timestamp);
                thread_events.clone()
            };
            self.persist_json(event_path(thread_id), &snapshot).await;
        }
        Ok(())
    }

    async fn load_events(&self, thread_id: ThreadId) -> Result<Vec<ThreadEvent>, EngineError> {
        if let Some(events) = self.events.read().await.get(&thread_id).cloned() {
            return Ok(events);
        }
        // Reload from database (may have been evicted from memory)
        if let Some(ws) = self.workspace.as_ref()
            && let Ok(doc) = ws.read(&event_path(thread_id)).await
            && let Ok(events) = serde_json::from_str::<Vec<ThreadEvent>>(&doc.content)
        {
            self.events.write().await.insert(thread_id, events.clone());
            return Ok(events);
        }
        Ok(Vec::new())
    }

    async fn save_project(&self, project: &Project) -> Result<(), EngineError> {
        self.projects
            .write()
            .await
            .insert(project.id, project.clone());
        self.persist_json(project_path(&project.name, project.id), project)
            .await;
        Ok(())
    }

    async fn load_project(&self, id: ProjectId) -> Result<Option<Project>, EngineError> {
        Ok(self.projects.read().await.get(&id).cloned())
    }

    async fn list_projects(&self, user_id: &str) -> Result<Vec<Project>, EngineError> {
        Ok(self
            .projects
            .read()
            .await
            .values()
            .filter(|p| p.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn list_all_projects(&self) -> Result<Vec<Project>, EngineError> {
        Ok(self.projects.read().await.values().cloned().collect())
    }

    async fn save_conversation(
        &self,
        conversation: &ConversationSurface,
    ) -> Result<(), EngineError> {
        self.conversations
            .write()
            .await
            .insert(conversation.id, conversation.clone());
        self.persist_json(conversation_path(conversation.id), conversation)
            .await;
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

    async fn save_memory_doc(&self, doc: &MemoryDoc) -> Result<(), EngineError> {
        // Defense-in-depth: block orchestrator/prompt writes when self-modify is
        // disabled, even if the caller bypassed tool-level checks.
        if is_protected_orchestrator_doc(doc) {
            let allow = std::env::var("ORCHESTRATOR_SELF_MODIFY")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false);
            if !allow {
                // Allow system-internal writes (v0 seeding, failure tracking) but
                // block LLM-authored patches (version > 0, non-meta titles).
                let is_system_internal = doc.title == "orchestrator:failures"
                    || doc
                        .metadata
                        .get("source")
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| s == "compiled_in");
                if !is_system_internal {
                    return Err(EngineError::AccessDenied {
                        user_id: doc.user_id.clone(),
                        entity: format!(
                            "orchestrator doc '{}' (self-modification disabled)",
                            doc.title
                        ),
                    });
                }
            }
        }

        self.docs.write().await.insert(doc.id, doc.clone());
        self.persist_doc(doc).await;
        Ok(())
    }

    async fn load_memory_doc(&self, id: DocId) -> Result<Option<MemoryDoc>, EngineError> {
        Ok(self.docs.read().await.get(&id).cloned())
    }

    async fn list_memory_docs(
        &self,
        project_id: ProjectId,
        user_id: &str,
    ) -> Result<Vec<MemoryDoc>, EngineError> {
        Ok(self
            .docs
            .read()
            .await
            .values()
            .filter(|doc| doc.project_id == project_id && doc.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn save_lease(&self, lease: &CapabilityLease) -> Result<(), EngineError> {
        self.persist_json_result(&lease_path(lease.id), lease)
            .await?;
        self.leases.write().await.insert(lease.id, lease.clone());
        Ok(())
    }

    async fn load_active_leases(
        &self,
        thread_id: ThreadId,
    ) -> Result<Vec<CapabilityLease>, EngineError> {
        Ok(self
            .leases
            .read()
            .await
            .values()
            .filter(|lease| lease.thread_id == thread_id && lease.is_valid())
            .cloned()
            .collect())
    }

    async fn revoke_lease(&self, lease_id: LeaseId, reason: &str) -> Result<(), EngineError> {
        let Some(mut lease) = self.leases.read().await.get(&lease_id).cloned() else {
            return Ok(());
        };
        lease.revoked = true;
        lease.revoked_reason = Some(reason.to_string());
        self.persist_json_result(&lease_path(lease_id), &lease)
            .await?;
        self.leases.write().await.insert(lease_id, lease);
        Ok(())
    }

    async fn save_mission(&self, mission: &Mission) -> Result<(), EngineError> {
        let proj_slug = self.project_slug(mission.project_id).await;
        let new_path = mission_path(&proj_slug, &mission.name, mission.id);
        let previous_secret_ref = self
            .mission_secret_refs
            .read()
            .await
            .get(&mission.id)
            .cloned();
        let (next_secret_ref, created_secret_ref) = self
            .prepare_mission_secret(mission, previous_secret_ref.as_ref())
            .await?;
        if let Err(error) = self
            .persist_mission_json(&new_path, mission, next_secret_ref.as_ref())
            .await
        {
            if let Some(created_secret_ref) = created_secret_ref.as_ref() {
                self.remove_mission_secret(mission.id, created_secret_ref)
                    .await;
            }
            return Err(error);
        }

        let stale_paths = {
            let mut paths = self.mission_paths.write().await;
            let tracked = paths.entry(mission.id).or_default();
            tracked.insert(new_path.clone());
            tracked
                .iter()
                .filter(|path| path.as_str() != new_path)
                .cloned()
                .collect::<Vec<_>>()
        };
        self.missions
            .write()
            .await
            .insert(mission.id, mission.clone());
        {
            let mut secret_refs = self.mission_secret_refs.write().await;
            if let Some(secret_ref) = next_secret_ref.as_ref() {
                secret_refs.insert(mission.id, secret_ref.clone());
            } else {
                secret_refs.remove(&mission.id);
            }
        }

        let mut stale_cleanup_failed = false;
        if let Some(ws) = self.workspace.as_ref() {
            for stale_path in stale_paths {
                let result = match ws.exists(&stale_path).await {
                    Ok(true) => ws.delete(&stale_path).await,
                    Ok(false) => Ok(()),
                    Err(error) => Err(error),
                };
                match result {
                    Ok(()) => {
                        if let Some(paths) = self.mission_paths.write().await.get_mut(&mission.id) {
                            paths.remove(&stale_path);
                        }
                    }
                    Err(error) => {
                        stale_cleanup_failed = true;
                        warn!(
                            mission_id = %mission.id,
                            path = %stale_path,
                            %error,
                            "failed to remove stale mission path; retaining it for retry"
                        );
                    }
                }
            }
        }
        if let Some(previous_secret_ref) = previous_secret_ref.as_ref()
            && Some(previous_secret_ref) != next_secret_ref.as_ref()
            && !stale_cleanup_failed
        {
            self.remove_mission_secret(mission.id, previous_secret_ref)
                .await;
        }
        Ok(())
    }

    async fn load_mission(&self, id: MissionId) -> Result<Option<Mission>, EngineError> {
        Ok(self.missions.read().await.get(&id).cloned())
    }

    async fn list_missions(
        &self,
        project_id: ProjectId,
        user_id: &str,
    ) -> Result<Vec<Mission>, EngineError> {
        Ok(self
            .missions
            .read()
            .await
            .values()
            .filter(|mission| mission.project_id == project_id && mission.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn list_all_threads(&self, project_id: ProjectId) -> Result<Vec<Thread>, EngineError> {
        Ok(self
            .threads
            .read()
            .await
            .values()
            .filter(|thread| thread.project_id == project_id)
            .cloned()
            .collect())
    }

    async fn list_all_missions(&self, project_id: ProjectId) -> Result<Vec<Mission>, EngineError> {
        Ok(self
            .missions
            .read()
            .await
            .values()
            .filter(|mission| mission.project_id == project_id)
            .cloned()
            .collect())
    }

    async fn update_mission_status(
        &self,
        id: MissionId,
        status: MissionStatus,
    ) -> Result<(), EngineError> {
        let updated = self.missions.read().await.get(&id).cloned();
        if let Some(mut mission) = updated {
            mission.status = status;
            mission.updated_at = chrono::Utc::now();
            self.save_mission(&mission).await?;
        }
        Ok(())
    }

    async fn delete_mission(&self, id: MissionId) -> Result<(), EngineError> {
        let Some(mission) = self.missions.read().await.get(&id).cloned() else {
            return Ok(());
        };
        let proj_slug = self.project_slug(mission.project_id).await;
        let current_path = mission_path(&proj_slug, &mission.name, id);
        let mut paths = self
            .mission_paths
            .read()
            .await
            .get(&id)
            .cloned()
            .unwrap_or_default();
        paths.remove(&current_path);
        let mut first_error = None;
        if let Some(ws) = self.workspace.as_ref() {
            for path in paths {
                let result = match ws.exists(&path).await {
                    Ok(true) => ws.delete(&path).await,
                    Ok(false) => Ok(()),
                    Err(error) => Err(error),
                };
                match result {
                    Ok(()) => {
                        if let Some(paths) = self.mission_paths.write().await.get_mut(&id) {
                            paths.remove(&path);
                        }
                    }
                    Err(error) if first_error.is_none() => first_error = Some(error),
                    Err(_) => {}
                }
            }
            if let Some(error) = first_error {
                return Err(EngineError::Store {
                    reason: format!("failed to delete stale persisted mission {id}: {error}"),
                });
            }

            let current_result = match ws.exists(&current_path).await {
                Ok(true) => ws.delete(&current_path).await,
                Ok(false) => Ok(()),
                Err(error) => Err(error),
            };
            if let Err(error) = current_result {
                return Err(EngineError::Store {
                    reason: format!("failed to delete persisted mission {id}: {error}"),
                });
            }
        }
        let secret_ref = self.mission_secret_refs.write().await.remove(&id);
        self.missions.write().await.remove(&id);
        self.mission_paths.write().await.remove(&id);
        if let Some(secret_ref) = secret_ref.as_ref() {
            self.remove_mission_secret(id, secret_ref).await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mission_delete_path_targets_persisted_document() {
        let id = MissionId::new();
        let path = mission_path("default--project", "Daily report", id);
        assert!(path.ends_with("/mission.json"));
        assert_eq!(
            path,
            format!(
                "{}/mission.json",
                mission_dir("default--project", "Daily report", id)
            )
        );
    }

    #[tokio::test]
    async fn cleanup_revokes_valid_leases_for_terminal_threads() {
        let store = HybridStore::new(None);
        let mut thread = Thread::new(
            "finished work",
            lunarwing_engine::ThreadType::Foreground,
            ProjectId::new(),
            "test-user",
            lunarwing_engine::ThreadConfig::default(),
        );
        thread.state = ThreadState::Done;
        thread.completed_at = Some(chrono::Utc::now());
        store.save_thread(&thread).await.unwrap();

        let lease = CapabilityLease {
            id: LeaseId::new(),
            thread_id: thread.id,
            capability_name: "tools".into(),
            granted_actions: lunarwing_engine::GrantedActions::All,
            granted_at: chrono::Utc::now(),
            expires_at: None,
            max_uses: None,
            uses_remaining: None,
            revoked: false,
            revoked_reason: None,
        };
        store.save_lease(&lease).await.unwrap();
        assert_eq!(store.load_active_leases(thread.id).await.unwrap().len(), 1);

        let cleaned = store
            .cleanup_terminal_state(chrono::Duration::hours(1))
            .await;

        assert_eq!(
            cleaned, 1,
            "fresh thread stays cached; its lease is cleaned"
        );
        assert!(
            store
                .load_active_leases(thread.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn cleanup_revokes_valid_leases_without_owner_threads() {
        let store = HybridStore::new(None);
        let missing_thread_id = ThreadId::new();
        let lease = CapabilityLease {
            id: LeaseId::new(),
            thread_id: missing_thread_id,
            capability_name: "tools".into(),
            granted_actions: lunarwing_engine::GrantedActions::All,
            granted_at: chrono::Utc::now(),
            expires_at: None,
            max_uses: None,
            uses_remaining: None,
            revoked: false,
            revoked_reason: None,
        };
        store.save_lease(&lease).await.unwrap();

        let cleaned = store
            .cleanup_terminal_state(chrono::Duration::hours(1))
            .await;

        assert_eq!(cleaned, 1);
        assert!(
            store
                .load_active_leases(missing_thread_id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn cleanup_revokes_live_thread_leases_missing_from_thread_record() {
        let store = HybridStore::new(None);
        let mut thread = Thread::new(
            "waiting work",
            lunarwing_engine::ThreadType::Foreground,
            ProjectId::new(),
            "test-user",
            lunarwing_engine::ThreadConfig::default(),
        );
        thread.state = ThreadState::Waiting;
        store.save_thread(&thread).await.unwrap();
        let lease = CapabilityLease {
            id: LeaseId::new(),
            thread_id: thread.id,
            capability_name: "tools".into(),
            granted_actions: lunarwing_engine::GrantedActions::All,
            granted_at: chrono::Utc::now(),
            expires_at: None,
            max_uses: None,
            uses_remaining: None,
            revoked: false,
            revoked_reason: None,
        };
        store.save_lease(&lease).await.unwrap();

        let cleaned = store
            .cleanup_terminal_state(chrono::Duration::hours(1))
            .await;

        assert_eq!(cleaned, 1);
        assert!(
            store
                .load_active_leases(thread.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn terminal_lease_cleanup_survives_workspace_reload() {
        use crate::db::Database;
        use crate::db::libsql::LibSqlBackend;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let backend = LibSqlBackend::new_local(&temp_dir.path().join("lease-cleanup.db"))
            .await
            .expect("libsql backend");
        backend.run_migrations().await.expect("migrations");
        let workspace = Arc::new(Workspace::new_with_db(
            "lease-cleanup-test",
            Arc::new(backend) as Arc<dyn Database>,
        ));
        let store = HybridStore::new(Some(Arc::clone(&workspace)));
        let mut thread = Thread::new(
            "finished work",
            lunarwing_engine::ThreadType::Foreground,
            ProjectId::new(),
            "test-user",
            lunarwing_engine::ThreadConfig::default(),
        );
        thread.state = ThreadState::Done;
        thread.completed_at = Some(chrono::Utc::now());
        let lease = CapabilityLease {
            id: LeaseId::new(),
            thread_id: thread.id,
            capability_name: "tools".into(),
            granted_actions: lunarwing_engine::GrantedActions::All,
            granted_at: chrono::Utc::now(),
            expires_at: None,
            max_uses: None,
            uses_remaining: None,
            revoked: false,
            revoked_reason: None,
        };
        thread.capability_leases.push(lease.id);
        store.save_thread(&thread).await.unwrap();
        store.save_lease(&lease).await.unwrap();

        store
            .cleanup_terminal_state(chrono::Duration::hours(1))
            .await;

        let reloaded = HybridStore::new(Some(workspace));
        reloaded.load_state_from_workspace().await.unwrap();
        assert!(
            reloaded
                .load_active_leases(thread.id)
                .await
                .unwrap()
                .is_empty()
        );
        let reloaded_lease = reloaded
            .leases
            .read()
            .await
            .get(&lease.id)
            .cloned()
            .expect("revoked lease should remain as audit history");
        assert!(reloaded_lease.revoked);
        assert_eq!(
            reloaded_lease.revoked_reason.as_deref(),
            Some("owning thread reached terminal state")
        );
    }

    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn cleanup_terminal_state_evicts_orphaned_leases_and_retains_files() {
        use crate::db::Database;
        use crate::db::libsql::LibSqlBackend;
        use lunarwing_engine::types::capability::GrantedActions;
        use lunarwing_engine::types::thread::{ThreadConfig, ThreadType};

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let backend = LibSqlBackend::new_local(&temp_dir.path().join("lease-sweep.db"))
            .await
            .expect("libsql backend");
        backend.run_migrations().await.expect("migrations");
        let workspace = Arc::new(Workspace::new_with_db(
            "lease-sweep-test",
            Arc::new(backend) as Arc<dyn Database>,
        ));
        let store = HybridStore::new(Some(Arc::clone(&workspace)));

        let make_lease = |thread_id: ThreadId| CapabilityLease {
            id: LeaseId::new(),
            thread_id,
            capability_name: "test".into(),
            granted_actions: GrantedActions::All,
            granted_at: chrono::Utc::now(),
            expires_at: None,
            max_uses: None,
            uses_remaining: None,
            revoked: false,
            revoked_reason: None,
        };

        // A live (running) thread, a terminal (done) thread, and a lease whose
        // thread no longer exists at all.
        let mut live_thread = Thread::new(
            "live",
            ThreadType::Foreground,
            ProjectId::new(),
            "test-user",
            ThreadConfig::default(),
        );
        live_thread.state = ThreadState::Running;
        let mut dead_thread = Thread::new(
            "dead",
            ThreadType::Foreground,
            ProjectId::new(),
            "test-user",
            ThreadConfig::default(),
        );
        dead_thread.state = ThreadState::Done;

        let live_lease = make_lease(live_thread.id);
        let dead_lease = make_lease(dead_thread.id);
        let missing_lease = make_lease(ThreadId::new());

        // Real spawns record granted lease IDs on the owning thread. A lease its
        // thread does not reference is itself treated as orphaned, so the
        // fixture must mirror that or the live case is indistinguishable.
        live_thread.capability_leases.push(live_lease.id);
        dead_thread.capability_leases.push(dead_lease.id);
        store.save_thread(&live_thread).await.expect("save live");
        store.save_thread(&dead_thread).await.expect("save dead");

        for lease in [&live_lease, &dead_lease, &missing_lease] {
            store.save_lease(lease).await.expect("save lease");
        }
        assert!(
            workspace.read(&lease_path(dead_lease.id)).await.is_ok(),
            "lease file persisted before sweep"
        );

        let cleaned = store.cleanup_terminal_state(chrono::Duration::zero()).await;

        // Orphaned leases (terminal or missing thread) are evicted from memory;
        // the live thread's lease survives.
        assert!(cleaned >= 3, "expected terminal thread + 2 orphaned leases");
        assert_eq!(
            store
                .load_active_leases(live_thread.id)
                .await
                .unwrap()
                .len(),
            1,
            "live thread keeps its lease"
        );
        assert!(
            store
                .load_active_leases(dead_thread.id)
                .await
                .unwrap()
                .is_empty(),
            "terminal thread's lease evicted"
        );

        // Files are retained as audit history, but durably marked revoked so a
        // restart reloads them as revoked rather than counting them active.
        for (lease_id, label) in [
            (dead_lease.id, "terminal-thread lease"),
            (missing_lease.id, "missing-thread lease"),
        ] {
            let doc = workspace
                .read(&lease_path(lease_id))
                .await
                .unwrap_or_else(|_| panic!("{label} file retained"));
            let persisted: CapabilityLease =
                serde_json::from_str(&doc.content).expect("lease deserializes");
            assert!(persisted.revoked, "{label} persisted as revoked");
        }

        let live_doc = workspace
            .read(&lease_path(live_lease.id))
            .await
            .expect("live lease file preserved");
        let persisted_live: CapabilityLease =
            serde_json::from_str(&live_doc.content).expect("lease deserializes");
        assert!(
            !persisted_live.revoked,
            "live thread's lease must not be revoked"
        );
    }

    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn renamed_then_deleted_mission_does_not_reload_from_workspace() {
        use crate::db::Database;
        use crate::db::libsql::LibSqlBackend;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let backend = LibSqlBackend::new_local(&temp_dir.path().join("mission-delete.db"))
            .await
            .expect("libsql backend");
        backend.run_migrations().await.expect("migrations");
        let workspace = Arc::new(Workspace::new_with_db(
            "mission-delete-test",
            Arc::new(backend) as Arc<dyn Database>,
        ));
        let store = HybridStore::new(Some(Arc::clone(&workspace)));
        let project = Project::new("test-user", "delete project", "test");
        let mut mission = Mission::new(
            project.id,
            "test-user",
            "delete me",
            "test deletion",
            lunarwing_engine::MissionCadence::Manual,
        );
        store.save_project(&project).await.expect("save project");
        store.save_mission(&mission).await.expect("save mission");
        let stale_mission = mission.clone();
        let original_path = mission_path(
            &store.project_slug(project.id).await,
            &mission.name,
            mission.id,
        );
        assert!(
            workspace
                .exists(&mission_path(
                    &store.project_slug(project.id).await,
                    &mission.name,
                    mission.id,
                ))
                .await
                .expect("mission existence")
        );

        mission.name = "renamed mission".into();
        mission.updated_at = chrono::Utc::now() + chrono::Duration::seconds(1);
        store.save_mission(&mission).await.expect("rename mission");
        assert!(
            !workspace
                .exists(&original_path)
                .await
                .expect("old path existence")
        );

        // Simulate legacy duplicate left by an older rename implementation.
        workspace
            .write(
                &original_path,
                &serde_json::to_string_pretty(&stale_mission).unwrap(),
            )
            .await
            .expect("write stale duplicate");
        let reloaded = HybridStore::new(Some(Arc::clone(&workspace)));
        reloaded.load_state_from_workspace().await.unwrap();
        assert_eq!(
            reloaded
                .load_mission(mission.id)
                .await
                .unwrap()
                .unwrap()
                .name,
            "renamed mission"
        );

        reloaded
            .delete_mission(mission.id)
            .await
            .expect("delete mission");

        let final_reload = HybridStore::new(Some(workspace));
        final_reload.load_state_from_workspace().await.unwrap();
        assert!(
            final_reload
                .load_mission(mission.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn stale_cleanup_failure_keeps_authoritative_mission_consistent() {
        use crate::db::Database;
        use crate::db::libsql::LibSqlBackend;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let backend = Arc::new(
            LibSqlBackend::new_local(&temp_dir.path().join("mission-cleanup.db"))
                .await
                .expect("libsql backend"),
        );
        backend.run_migrations().await.expect("migrations");
        let workspace = Arc::new(Workspace::new_with_db(
            "mission-cleanup-test",
            Arc::clone(&backend) as Arc<dyn Database>,
        ));
        let store = HybridStore::new(Some(Arc::clone(&workspace)));
        let project = Project::new("test-user", "cleanup project", "test");
        let mut mission = Mission::new(
            project.id,
            "test-user",
            "old name",
            "preserve authoritative state",
            lunarwing_engine::MissionCadence::Manual,
        );
        store.save_project(&project).await.unwrap();
        store.save_mission(&mission).await.unwrap();
        let project_slug = store.project_slug(project.id).await;
        let stale_path = mission_path(&project_slug, &mission.name, mission.id);
        let connection = backend.connect().await.unwrap();
        let trigger_sql = format!(
            "CREATE TRIGGER fail_mission_stale_delete \
             BEFORE DELETE ON memory_documents \
             WHEN OLD.path = '{}' \
             BEGIN SELECT RAISE(FAIL, 'injected stale delete failure'); END",
            stale_path
        );
        connection.execute(&trigger_sql, ()).await.unwrap();

        mission.name = "new name".into();
        mission.updated_at = chrono::Utc::now() + chrono::Duration::seconds(1);
        store.save_mission(&mission).await.unwrap();
        let current_path = mission_path(&project_slug, &mission.name, mission.id);
        assert!(workspace.exists(&stale_path).await.unwrap());
        assert!(workspace.exists(&current_path).await.unwrap());
        assert_eq!(
            store.load_mission(mission.id).await.unwrap().unwrap().name,
            "new name"
        );

        let reloaded = HybridStore::new(Some(Arc::clone(&workspace)));
        reloaded.load_state_from_workspace().await.unwrap();
        assert_eq!(
            reloaded
                .load_mission(mission.id)
                .await
                .unwrap()
                .unwrap()
                .name,
            "new name"
        );
        assert!(reloaded.delete_mission(mission.id).await.is_err());
        assert!(workspace.exists(&current_path).await.unwrap());
        assert_eq!(
            reloaded
                .load_mission(mission.id)
                .await
                .unwrap()
                .unwrap()
                .name,
            "new name"
        );

        connection
            .execute("DROP TRIGGER fail_mission_stale_delete", ())
            .await
            .unwrap();
        reloaded.delete_mission(mission.id).await.unwrap();
        let final_reload = HybridStore::new(Some(workspace));
        final_reload.load_state_from_workspace().await.unwrap();
        assert!(
            final_reload
                .load_mission(mission.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn mission_write_failure_does_not_commit_create_or_secret_rotation() {
        use crate::db::Database;
        use crate::db::libsql::LibSqlBackend;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let backend = Arc::new(
            LibSqlBackend::new_local(&temp_dir.path().join("mission-write.db"))
                .await
                .expect("libsql backend"),
        );
        backend.run_migrations().await.expect("migrations");
        let workspace = Arc::new(Workspace::new_with_db(
            "mission-write-test",
            Arc::clone(&backend) as Arc<dyn Database>,
        ));
        let secrets_store: Arc<dyn SecretsStore + Send + Sync> =
            Arc::new(crate::testing::credentials::test_secrets_store());
        let store = HybridStore::new_with_secrets_store(
            Some(Arc::clone(&workspace)),
            Some(Arc::clone(&secrets_store)),
        );
        let project = Project::new("test-user", "write project", "test");
        let mission = Mission::new(
            project.id,
            "test-user",
            "secure hook",
            "test rotation",
            lunarwing_engine::MissionCadence::Webhook {
                path: "secure".into(),
                secret: Some("old-secret".into()),
            },
        );
        store.save_project(&project).await.expect("save project");
        store.save_mission(&mission).await.expect("save mission");
        let mission_file = mission_path(
            &store.project_slug(project.id).await,
            &mission.name,
            mission.id,
        );
        let persisted = workspace.read(&mission_file).await.unwrap().content;
        assert!(!persisted.contains("old-secret"));
        assert!(!persisted.contains("\"secret\""));
        assert!(persisted.contains("\"secret_ref\""));
        assert_eq!(secrets_store.list("test-user").await.unwrap().len(), 1);

        let connection = backend.connect().await.expect("connection");
        connection
            .execute(
                "ALTER TABLE memory_documents RENAME TO unavailable_memory_documents",
                (),
            )
            .await
            .expect("disable workspace writes");

        let failed_create = Mission::new(
            project.id,
            "test-user",
            "must not appear",
            "failed create",
            lunarwing_engine::MissionCadence::Manual,
        );
        assert!(store.save_mission(&failed_create).await.is_err());
        assert!(
            store
                .load_mission(failed_create.id)
                .await
                .unwrap()
                .is_none()
        );

        let mut rotated = mission.clone();
        rotated.cadence = lunarwing_engine::MissionCadence::Webhook {
            path: "secure".into(),
            secret: Some("new-secret".into()),
        };
        rotated.updated_at = chrono::Utc::now() + chrono::Duration::seconds(1);
        assert!(store.save_mission(&rotated).await.is_err());
        let stored = store.load_mission(mission.id).await.unwrap().unwrap();
        assert!(matches!(
            stored.cadence,
            lunarwing_engine::MissionCadence::Webhook { path, secret }
                if path == "secure" && secret.as_deref() == Some("old-secret")
        ));
        assert_eq!(secrets_store.list("test-user").await.unwrap().len(), 1);

        connection
            .execute(
                "ALTER TABLE unavailable_memory_documents RENAME TO memory_documents",
                (),
            )
            .await
            .expect("restore workspace writes");
        let reloaded =
            HybridStore::new_with_secrets_store(Some(workspace), Some(Arc::clone(&secrets_store)));
        reloaded.load_state_from_workspace().await.unwrap();
        let stored = reloaded.load_mission(mission.id).await.unwrap().unwrap();
        assert!(matches!(
            stored.cadence,
            lunarwing_engine::MissionCadence::Webhook { path, secret }
                if path == "secure" && secret.as_deref() == Some("old-secret")
        ));
    }

    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn legacy_plaintext_webhook_secret_is_migrated_and_scrubbed() {
        use crate::db::Database;
        use crate::db::libsql::LibSqlBackend;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let backend = LibSqlBackend::new_local(&temp_dir.path().join("mission-migrate.db"))
            .await
            .expect("libsql backend");
        backend.run_migrations().await.expect("migrations");
        let workspace = Arc::new(Workspace::new_with_db(
            "mission-migrate-test",
            Arc::new(backend) as Arc<dyn Database>,
        ));
        let secrets_store: Arc<dyn SecretsStore + Send + Sync> =
            Arc::new(crate::testing::credentials::test_secrets_store());
        let seed = HybridStore::new(Some(Arc::clone(&workspace)));
        let project = Project::new("test-user", "migration project", "test");
        seed.save_project(&project).await.unwrap();
        let mission = Mission::new(
            project.id,
            "test-user",
            "legacy hook",
            "migrate secret",
            lunarwing_engine::MissionCadence::Webhook {
                path: "legacy".into(),
                secret: Some("legacyplaintextsecret".into()),
            },
        );
        let path = mission_path(
            &seed.project_slug(project.id).await,
            &mission.name,
            mission.id,
        );
        let mut legacy = serde_json::to_value(&mission).unwrap();
        legacy
            .pointer_mut("/cadence/Webhook")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .insert(
                "secret".into(),
                serde_json::Value::String("legacyplaintextsecret".into()),
            );
        workspace
            .write(&path, &serde_json::to_string_pretty(&legacy).unwrap())
            .await
            .unwrap();

        let reloaded = HybridStore::new_with_secrets_store(
            Some(Arc::clone(&workspace)),
            Some(Arc::clone(&secrets_store)),
        );
        reloaded.load_state_from_workspace().await.unwrap();

        let persisted = workspace.read(&path).await.unwrap().content;
        assert!(!persisted.contains("legacyplaintextsecret"));
        assert!(!persisted.contains("\"secret\""));
        assert!(persisted.contains("\"secret_ref\""));
        assert_eq!(secrets_store.list("test-user").await.unwrap().len(), 1);
        let restored = reloaded.load_mission(mission.id).await.unwrap().unwrap();
        assert!(matches!(
            restored.cadence,
            lunarwing_engine::MissionCadence::Webhook { secret, .. }
                if secret.as_deref() == Some("legacyplaintextsecret")
        ));
        assert!(
            workspace
                .search("legacyplaintextsecret", 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn missing_webhook_secret_skips_only_affected_mission() {
        use crate::db::Database;
        use crate::db::libsql::LibSqlBackend;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let backend = LibSqlBackend::new_local(&temp_dir.path().join("mission-missing-secret.db"))
            .await
            .expect("libsql backend");
        backend.run_migrations().await.expect("migrations");
        let workspace = Arc::new(Workspace::new_with_db(
            "mission-missing-secret-test",
            Arc::new(backend) as Arc<dyn Database>,
        ));
        let seed = HybridStore::new(Some(Arc::clone(&workspace)));
        let project = Project::new("test-user", "secret recovery project", "test");
        seed.save_project(&project).await.unwrap();

        let healthy = Mission::new(
            project.id,
            "test-user",
            "healthy mission",
            "keep running",
            lunarwing_engine::MissionCadence::Manual,
        );
        seed.save_mission(&healthy).await.unwrap();

        let broken = Mission::new(
            project.id,
            "test-user",
            "broken webhook",
            "must fail closed",
            lunarwing_engine::MissionCadence::Webhook {
                path: "broken".into(),
                secret: None,
            },
        );
        let broken_path = mission_path(
            &seed.project_slug(project.id).await,
            &broken.name,
            broken.id,
        );
        let mut broken_json = serde_json::to_value(&broken).unwrap();
        broken_json
            .pointer_mut("/cadence/Webhook")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .insert(
                "secret_ref".into(),
                serde_json::Value::String(format!(
                    "{}{}",
                    HybridStore::mission_secret_prefix(broken.id),
                    Uuid::new_v4().simple()
                )),
            );
        workspace
            .write(
                &broken_path,
                &serde_json::to_string_pretty(&broken_json).unwrap(),
            )
            .await
            .unwrap();

        let mut stale = broken.clone();
        stale.name = "stale fallback".into();
        stale.cadence = lunarwing_engine::MissionCadence::Webhook {
            path: "stale".into(),
            secret: Some("staleplaintextsecret".into()),
        };
        stale.updated_at = broken.updated_at - chrono::Duration::seconds(1);
        let stale_path = mission_path(&seed.project_slug(project.id).await, &stale.name, stale.id);
        let mut stale_json = serde_json::to_value(&stale).unwrap();
        stale_json
            .pointer_mut("/cadence/Webhook")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .insert(
                "secret".into(),
                serde_json::Value::String("staleplaintextsecret".into()),
            );
        workspace
            .write(
                &stale_path,
                &serde_json::to_string_pretty(&stale_json).unwrap(),
            )
            .await
            .unwrap();

        let reloaded = HybridStore::new_with_secrets_store(
            Some(Arc::clone(&workspace)),
            Some(Arc::new(crate::testing::credentials::test_secrets_store())),
        );
        reloaded.load_state_from_workspace().await.unwrap();

        assert!(reloaded.load_mission(healthy.id).await.unwrap().is_some());
        assert!(reloaded.load_mission(broken.id).await.unwrap().is_none());
        assert!(!workspace.exists(&stale_path).await.unwrap());
        assert!(
            workspace
                .search("staleplaintextsecret", 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn mission_loader_ignores_additional_read_scopes() {
        use crate::db::Database;
        use crate::db::libsql::LibSqlBackend;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let backend = Arc::new(
            LibSqlBackend::new_local(&temp_dir.path().join("mission-scope.db"))
                .await
                .expect("libsql backend"),
        );
        backend.run_migrations().await.expect("migrations");
        let database: Arc<dyn Database> = backend;

        let secondary_workspace = Arc::new(Workspace::new_with_db(
            "secondary-user",
            Arc::clone(&database),
        ));
        let secondary_store = HybridStore::new(Some(secondary_workspace));
        let project = Project::new("secondary-user", "secondary project", "test");
        let mission = Mission::new(
            project.id,
            "secondary-user",
            "secondary mission",
            "must stay isolated",
            lunarwing_engine::MissionCadence::Manual,
        );
        secondary_store.save_project(&project).await.unwrap();
        secondary_store.save_mission(&mission).await.unwrap();

        let primary_workspace = Arc::new(
            Workspace::new_with_db("primary-user", database)
                .with_additional_read_scopes(vec!["secondary-user".into()]),
        );
        let primary_store = HybridStore::new(Some(primary_workspace));
        primary_store.load_state_from_workspace().await.unwrap();

        assert!(
            primary_store
                .load_mission(mission.id)
                .await
                .unwrap()
                .is_none()
        );
    }
}
