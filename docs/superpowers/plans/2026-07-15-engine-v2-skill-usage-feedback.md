# Engine V2 Skill Usage Feedback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record one success or failure observation for every V2 skill activated by the default Engine V2 orchestrator when its logical thread reaches a classified terminal outcome.

**Architecture:** The default orchestrator emits stable MemoryDoc UUIDs with the existing skill-activation event. Rust stores those UUIDs in namespaced thread metadata outside the runtime checkpoint. A focused runtime helper applies the terminal-outcome policy and updates `SkillTracker` immediately before `ThreadManager` performs its final thread save.

**Tech Stack:** Rust 2024, Tokio, serde/serde_json, Monty Python runtime, `lunarwing_engine`, `lunarwing_skills`, in-memory Store test fixture.

## Global Constraints

- Run every Cargo command from `ic/` with `taskset -c 0-5` and `-j6` where Cargo accepts it.
- Use `cargo check`, never a full debug `cargo build`.
- `Completed` records success; `Failed` and `MaxIterations` record failure; `Stopped` and `GatePaused` record no observation.
- `GatePaused` preserves activated IDs for resume; every terminal outcome clears them.
- This guarantees lifecycle-exactly-once behavior during normal execution and gate resume, not transactional crash recovery.
- `SKILL_SELF_IMPROVEMENT` remains disabled by default and is captured when `ThreadManager` is constructed; tests inject the captured value without mutating process environment.
- Never resolve skill names back to IDs or parse Python checkpoint internals.
- Metric-recording errors warn and continue; they never alter the thread outcome.
- Orchestrator-emitted UUIDs are authorized through
  `list_memory_docs_with_shared(thread.project_id, thread.user_id)` before any
  metric mutation; lookup errors fail closed.
- Process at most 64 unique emitted IDs per terminal feedback pass.
- Do not modify B-1/B-2/B-3 proposal behavior, web handlers, tenant scripts, or the legacy runtime.
- Do not commit unless the maintainer explicitly requests it.

## File Structure

- Modify `ic/crates/lunarwing_engine/orchestrator/default.py`: emit selected skill document UUIDs with the existing activation event.
- Modify `ic/crates/lunarwing_engine/src/executor/orchestrator.rs`: capture emitted UUIDs in namespaced thread metadata.
- Create `ic/crates/lunarwing_engine/src/runtime/skill_feedback.rs`: own the metadata key and terminal feedback policy.
- Create `ic/crates/lunarwing_engine/src/runtime/skill_feedback_tests.rs`: deterministic outcome, gate, deduplication, cleanup, and feature-gate tests.
- Modify `ic/crates/lunarwing_engine/src/runtime/mod.rs`: register the focused runtime module and its tests.
- Modify `ic/crates/lunarwing_engine/src/runtime/manager.rs`: invoke feedback before the final thread save.
- Modify `ic/crates/lunarwing_engine/src/executor/loop_engine.rs`: add one real-default-orchestrator regression test proving stable UUID transport.

---

### Task 1: Transport Stable Skill IDs Out of the Default Orchestrator

**Files:**
- Modify: `ic/crates/lunarwing_engine/orchestrator/default.py:512-526`
- Modify: `ic/crates/lunarwing_engine/src/executor/orchestrator.rs:1532-1610`
- Modify: `ic/crates/lunarwing_engine/src/executor/loop_engine.rs` test module
- Create: `ic/crates/lunarwing_engine/src/runtime/skill_feedback.rs`
- Modify: `ic/crates/lunarwing_engine/src/runtime/mod.rs`

**Interfaces:**
- Produces: `runtime::skill_feedback::ACTIVE_SKILL_DOC_IDS_METADATA_KEY: &str`
- Produces metadata: `thread.metadata[ACTIVE_SKILL_DOC_IDS_METADATA_KEY] = Vec<String>`
- Consumes Python event kwarg: `skill_doc_ids: String` containing comma-separated MemoryDoc UUIDs

- [ ] **Step 1: Add the metadata-key module and register it**

Create `runtime/skill_feedback.rs` with only the shared key initially:

```rust
//! Terminal feedback for skills activated by Engine V2 threads.

pub(crate) const ACTIVE_SKILL_DOC_IDS_METADATA_KEY: &str =
    "lunarwing.skill_feedback.active_doc_ids";
```

Register it in `runtime/mod.rs`:

```rust
pub(crate) mod skill_feedback;
```

- [ ] **Step 2: Write the failing real-orchestrator metadata test**

In the `loop_engine.rs` test module, add a helper that seeds one extracted skill matching the existing goal `"test goal"`:

```rust
fn make_selectable_skill(project_id: ProjectId) -> crate::types::memory::MemoryDoc {
    use lunarwing_skills::types::ActivationCriteria;
    use lunarwing_skills::v2::{SkillMetrics, V2SkillMetadata, V2SkillSource};
    use lunarwing_skills::SkillTrust;

    let metadata = V2SkillMetadata {
        name: "feedback-skill".into(),
        activation: ActivationCriteria {
            keywords: vec!["test".into()],
            ..Default::default()
        },
        source: V2SkillSource::Extracted,
        trust: SkillTrust::Trusted,
        metrics: SkillMetrics::default(),
        ..serde_json::from_str::<V2SkillMetadata>("{}").unwrap()
    };
    let mut doc = crate::types::memory::MemoryDoc::new(
        project_id,
        "test-user",
        crate::types::memory::DocType::Skill,
        "skill:feedback-skill",
        "Use this deterministic test skill.",
    );
    doc.metadata = serde_json::to_value(metadata).unwrap();
    doc
}
```

Add a test that uses the real bundled `default.py` through `ExecutionLoop::run()`:

```rust
#[tokio::test]
async fn default_orchestrator_persists_activated_skill_doc_ids() {
    let llm = Arc::new(MockLlm::new(vec![text_response("FINAL('done')")]));
    let (exec, _signal_tx) =
        make_loop_with_llm(llm, Vec::new(), ThreadConfig::default()).await;
    let project_id = exec.thread.project_id;
    let skill = make_selectable_skill(project_id);
    let skill_id = skill.id.to_string();
    let store: Arc<dyn Store> =
        Arc::new(crate::tests::InMemoryStore::with_docs(vec![skill]));
    let mut exec = exec.with_store(store);

    let outcome = exec.run().await.expect("default orchestrator should complete");

    assert!(matches!(outcome, ThreadOutcome::Completed { .. }));
    assert_eq!(
        exec.thread
            .metadata
            .get(crate::runtime::skill_feedback::ACTIVE_SKILL_DOC_IDS_METADATA_KEY),
        Some(&serde_json::json!([skill_id]))
    );
}
```

- [ ] **Step 3: Run the test and confirm the missing transport**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  default_orchestrator_persists_activated_skill_doc_ids -- --nocapture
```

Expected: FAIL because the metadata key is absent.

- [ ] **Step 4: Emit UUIDs from the default orchestrator**

Change the activation block in `default.py` to preserve the existing names and add IDs:

```python
skill_names = ",".join(
    s.get("metadata", {}).get("name", "?") for s in active_skills
)
skill_doc_ids = ",".join(
    s.get("doc_id", "") for s in active_skills if s.get("doc_id", "")
)
__emit_event__(
    "skill_activated",
    skill_names=skill_names,
    skill_doc_ids=skill_doc_ids,
)
state["active_skill_ids"] = [s.get("doc_id", "") for s in active_skills]
```

- [ ] **Step 5: Persist emitted UUIDs in thread metadata**

Extend the `"skill_activated"` branch in `handle_emit_event()`:

```rust
"skill_activated" => {
    let names_str = extract_string_kwarg(kwargs, "skill_names").unwrap_or_default();
    let skill_names: Vec<String> = names_str
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let skill_doc_ids: Vec<String> = extract_string_kwarg(kwargs, "skill_doc_ids")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if !skill_doc_ids.is_empty()
        && let Some(metadata) = thread.metadata.as_object_mut()
    {
        metadata.insert(
            crate::runtime::skill_feedback::ACTIVE_SKILL_DOC_IDS_METADATA_KEY.into(),
            serde_json::json!(skill_doc_ids),
        );
    }
    EventKind::SkillActivated { skill_names }
}
```

Do not change `EventKind::SkillActivated`; missions and user-facing traces continue using names.

- [ ] **Step 6: Run the focused and surrounding executor tests**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  default_orchestrator_persists_activated_skill_doc_ids -- --nocapture
taskset -c 0-5 cargo test -j6 -p lunarwing_engine executor:: -- --test-threads=6
```

Expected: both commands exit 0; the metadata test observes the selected skill UUID.

---

### Task 2: Implement and Test the Terminal Feedback Policy

**Files:**
- Modify: `ic/crates/lunarwing_engine/src/runtime/skill_feedback.rs`
- Create: `ic/crates/lunarwing_engine/src/runtime/skill_feedback_tests.rs`
- Modify: `ic/crates/lunarwing_engine/src/runtime/mod.rs`

**Interfaces:**
- Consumes: `Thread`, `ThreadOutcome`, `Arc<dyn Store>`, and an injected `enabled: bool`
- Produces: `record_terminal_skill_usage(thread, outcome, store, enabled) -> ()`
- Mutation: terminal outcomes remove `ACTIVE_SKILL_DOC_IDS_METADATA_KEY`; only classified success/failure outcomes update skill metrics

- [ ] **Step 1: Register the test module**

In `runtime/mod.rs`, add:

```rust
#[cfg(test)]
mod skill_feedback_tests;
```

- [ ] **Step 2: Write the failing outcome-matrix tests**

In `runtime/skill_feedback_tests.rs`, add these shared fixtures:

```rust
use std::sync::Arc;

use lunarwing_skills::v2::{SkillMetrics, V2SkillMetadata, V2SkillSource};
use lunarwing_skills::SkillTrust;

use super::skill_feedback::{
    ACTIVE_SKILL_DOC_IDS_METADATA_KEY, record_terminal_skill_usage,
};
use crate::runtime::messaging::ThreadOutcome;
use crate::traits::store::Store;
use crate::types::memory::{DocId, DocType, MemoryDoc};
use crate::types::project::ProjectId;
use crate::types::thread::{Thread, ThreadConfig, ThreadType};

fn skill_doc(project_id: ProjectId) -> MemoryDoc {
    let metadata = V2SkillMetadata {
        name: "feedback-skill".into(),
        source: V2SkillSource::Extracted,
        trust: SkillTrust::Trusted,
        metrics: SkillMetrics::default(),
        ..serde_json::from_str::<V2SkillMetadata>("{}").unwrap()
    };
    let mut doc = MemoryDoc::new(
        project_id,
        "test-user",
        DocType::Skill,
        "skill:feedback-skill",
        "feedback",
    );
    doc.metadata = serde_json::to_value(metadata).unwrap();
    doc
}

fn thread_with_ids(project_id: ProjectId, ids: &[DocId]) -> Thread {
    let mut thread = Thread::new(
        "feedback",
        ThreadType::Foreground,
        project_id,
        "test-user",
        ThreadConfig::default(),
    );
    thread.metadata.as_object_mut().unwrap().insert(
        ACTIVE_SKILL_DOC_IDS_METADATA_KEY.into(),
        serde_json::json!(ids.iter().map(ToString::to_string).collect::<Vec<_>>()),
    );
    thread
}

async fn metrics(store: &Arc<dyn Store>, doc_id: DocId) -> SkillMetrics {
    let doc = store.load_memory_doc(doc_id).await.unwrap().unwrap();
    serde_json::from_value::<V2SkillMetadata>(doc.metadata)
        .unwrap()
        .metrics
}

fn gate_paused() -> ThreadOutcome {
    ThreadOutcome::GatePaused {
        gate_name: "approval".into(),
        action_name: "test_tool".into(),
        call_id: "call-1".into(),
        parameters: serde_json::json!({}),
        resume_kind: crate::gate::ResumeKind::Approval {
            allow_always: false,
        },
        resume_output: None,
    }
}
```

Add a table-driven assertion helper and the complete outcome matrix:

```rust
async fn assert_feedback_case(
    outcome: ThreadOutcome,
    enabled: bool,
    duplicate_id: bool,
    expected: (u64, u64, u64),
    expect_ids: bool,
) {
    let project_id = ProjectId::new();
    let doc = skill_doc(project_id);
    let doc_id = doc.id;
    let store: Arc<dyn Store> =
        Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
    let ids = if duplicate_id {
        vec![doc_id, doc_id]
    } else {
        vec![doc_id]
    };
    let mut thread = thread_with_ids(project_id, &ids);

    record_terminal_skill_usage(&mut thread, &outcome, &store, enabled).await;

    let metrics = metrics(&store, doc_id).await;
    assert_eq!(
        (metrics.usage_count, metrics.success_count, metrics.failure_count),
        expected,
    );
    assert_eq!(
        thread
            .metadata
            .get(ACTIVE_SKILL_DOC_IDS_METADATA_KEY)
            .is_some(),
        expect_ids,
    );
}

#[tokio::test]
async fn completed_records_one_success_and_clears_ids() {
    assert_feedback_case(
        ThreadOutcome::Completed { response: Some("done".into()) },
        true,
        false,
        (1, 1, 0),
        false,
    )
    .await;
}

#[tokio::test]
async fn failed_records_one_failure_and_clears_ids() {
    assert_feedback_case(
        ThreadOutcome::Failed { error: "boom".into() }, true, false, (1, 0, 1), false,
    ).await;
}

#[tokio::test]
async fn max_iterations_records_one_failure_and_clears_ids() {
    assert_feedback_case(ThreadOutcome::MaxIterations, true, false, (1, 0, 1), false).await;
}

#[tokio::test]
async fn stopped_records_nothing_and_clears_ids() {
    assert_feedback_case(ThreadOutcome::Stopped, true, false, (0, 0, 0), false).await;
}

#[tokio::test]
async fn gate_paused_records_nothing_and_preserves_ids() {
    assert_feedback_case(gate_paused(), true, false, (0, 0, 0), true).await;
}

#[tokio::test]
async fn disabled_feedback_records_nothing_and_clears_terminal_ids() {
    assert_feedback_case(
        ThreadOutcome::Completed { response: Some("done".into()) },
        false,
        false,
        (0, 0, 0),
        false,
    ).await;
}

#[tokio::test]
async fn duplicate_ids_are_recorded_once() {
    assert_feedback_case(
        ThreadOutcome::Completed { response: Some("done".into()) },
        true,
        true,
        (1, 1, 0),
        false,
    ).await;
}

#[tokio::test]
async fn gate_pause_then_completion_records_exactly_once() {
    let project_id = ProjectId::new();
    let doc = skill_doc(project_id);
    let doc_id = doc.id;
    let store: Arc<dyn Store> =
        Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
    let mut thread = thread_with_ids(project_id, &[doc_id]);

    record_terminal_skill_usage(&mut thread, &gate_paused(), &store, true).await;
    record_terminal_skill_usage(
        &mut thread,
        &ThreadOutcome::Completed { response: Some("done".into()) },
        &store,
        true,
    ).await;
    record_terminal_skill_usage(
        &mut thread,
        &ThreadOutcome::Completed { response: Some("done again".into()) },
        &store,
        true,
    ).await;

    let metrics = metrics(&store, doc_id).await;
    assert_eq!(
        (metrics.usage_count, metrics.success_count, metrics.failure_count),
        (1, 1, 0),
    );
    assert!(thread.metadata.get(ACTIVE_SKILL_DOC_IDS_METADATA_KEY).is_none());
}
```

These tests inject `enabled: bool`; they must not mutate process environment.

- [ ] **Step 3: Run the tests and verify the helper is missing**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  runtime::skill_feedback_tests -- --test-threads=6
```

Expected: compilation fails because `record_terminal_skill_usage` is not defined.

- [ ] **Step 4: Implement the minimal policy helper**

Expand `runtime/skill_feedback.rs`:

```rust
//! Terminal feedback for skills activated by Engine V2 threads.

use std::collections::HashSet;
use std::sync::Arc;

use tracing::warn;

use crate::memory::SkillTracker;
use crate::runtime::messaging::ThreadOutcome;
use crate::traits::store::Store;
use crate::types::memory::DocId;
use crate::types::thread::Thread;

pub(crate) const ACTIVE_SKILL_DOC_IDS_METADATA_KEY: &str =
    "lunarwing.skill_feedback.active_doc_ids";

pub(crate) async fn record_terminal_skill_usage(
    thread: &mut Thread,
    outcome: &ThreadOutcome,
    store: &Arc<dyn Store>,
    enabled: bool,
) {
    if matches!(outcome, ThreadOutcome::GatePaused { .. }) {
        return;
    }

    let ids = thread
        .metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(ACTIVE_SKILL_DOC_IDS_METADATA_KEY));
    let Some(ids) = ids.and_then(|value| value.as_array().cloned()) else {
        return;
    };

    let success = match outcome {
        ThreadOutcome::Completed { .. } => Some(true),
        ThreadOutcome::Failed { .. } | ThreadOutcome::MaxIterations => Some(false),
        ThreadOutcome::Stopped => None,
    };
    let Some(success) = success.filter(|_| enabled) else {
        return;
    };

    let tracker = SkillTracker::new(Arc::clone(store));
    let mut seen = HashSet::new();
    for value in ids {
        let Some(raw_id) = value.as_str() else {
            warn!(thread_id = %thread.id, "ignoring non-string activated skill id");
            continue;
        };
        let Ok(uuid) = uuid::Uuid::parse_str(raw_id) else {
            warn!(thread_id = %thread.id, skill_id = %raw_id, "ignoring malformed activated skill id");
            continue;
        };
        let doc_id = DocId(uuid);
        if !seen.insert(doc_id) {
            continue;
        }
        if let Err(error) = tracker.record_usage(doc_id, success).await {
            warn!(thread_id = %thread.id, skill_id = %raw_id, %error, "failed to record skill usage");
        }
    }
}
```

- [ ] **Step 5: Run the outcome-matrix tests**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  runtime::skill_feedback_tests -- --test-threads=6
```

Expected: all feedback-policy tests pass with exact metric counts and metadata behavior.

---

### Task 3: Wire Feedback into the Authoritative ThreadManager Boundary

**Files:**
- Modify: `ic/crates/lunarwing_engine/src/runtime/manager.rs:346-401`
- Modify: `ic/crates/lunarwing_engine/src/runtime/manager.rs` test module

**Interfaces:**
- Consumes: `record_terminal_skill_usage()` from Task 2
- Produces: production lifecycle invocation before the final `save_thread()`

- [ ] **Step 1: Add a failing real-manager completion test**

In the `manager.rs` test module, add this store-agnostic helper next to `make_manager_with_store`:

```rust
fn make_manager_with_dyn_store(
    llm: Arc<dyn LlmBackend>,
    store: Arc<dyn Store>,
) -> ThreadManager {
    let mut caps = CapabilityRegistry::new();
    caps.register(Capability {
        name: "test".into(),
        description: "Test capability".into(),
        actions: vec![],
        knowledge: vec![],
        policies: vec![],
    });
    ThreadManager::new(
        llm,
        Arc::new(MockEffects),
        store,
        Arc::new(caps),
        Arc::new(LeaseManager::new()),
        Arc::new(PolicyEngine::new()),
    )
    .with_skill_feedback_enabled(true)
}
```

Seed an extracted skill with activation keyword `"feedback"`, use goal `"feedback task"`, and use `MockLlm::text("FINAL('done')")`.

The test must:

```rust
#[tokio::test]
async fn completed_thread_records_selected_skill_once() {
    let project_id = ProjectId::new();
    let skill = make_feedback_skill_doc(project_id);
    let skill_id = skill.id;
    let store: Arc<dyn Store> =
        Arc::new(crate::tests::InMemoryStore::with_docs(vec![skill]));
    let manager = make_manager_with_dyn_store(MockLlm::text("FINAL('done')"), Arc::clone(&store));

    let thread_id = manager
        .spawn_thread(
            "feedback task",
            ThreadType::Foreground,
            project_id,
            ThreadConfig::default(),
            None,
            "test-user",
        )
        .await
        .unwrap();
    let outcome = manager.join_thread(thread_id).await.unwrap();

    assert!(matches!(outcome, ThreadOutcome::Completed { .. }));
    let skill = store.load_memory_doc(skill_id).await.unwrap().unwrap();
    let metadata: V2SkillMetadata = serde_json::from_value(skill.metadata).unwrap();
    assert_eq!(metadata.metrics.usage_count, 1);
    assert_eq!(metadata.metrics.success_count, 1);
    assert_eq!(metadata.metrics.failure_count, 0);
    let thread = store.load_thread(thread_id).await.unwrap().unwrap();
    assert!(thread
        .metadata
        .get(ACTIVE_SKILL_DOC_IDS_METADATA_KEY)
        .is_none());
}
```

Add this fixture in the `manager.rs` test module:

```rust
fn make_feedback_skill_doc(project_id: ProjectId) -> MemoryDoc {
    use lunarwing_skills::types::ActivationCriteria;
    use lunarwing_skills::v2::{SkillMetrics, V2SkillMetadata, V2SkillSource};
    use lunarwing_skills::SkillTrust;

    let metadata = V2SkillMetadata {
        name: "feedback-skill".into(),
        activation: ActivationCriteria {
            keywords: vec!["feedback".into()],
            ..Default::default()
        },
        source: V2SkillSource::Extracted,
        trust: SkillTrust::Trusted,
        metrics: SkillMetrics::default(),
        ..serde_json::from_str::<V2SkillMetadata>("{}").unwrap()
    };
    let mut doc = MemoryDoc::new(
        project_id,
        "test-user",
        crate::types::memory::DocType::Skill,
        "skill:feedback-skill",
        "Use this deterministic feedback skill.",
    );
    doc.metadata = serde_json::to_value(metadata).unwrap();
    doc
}
```

Introduce a private field on `ThreadManager`:

```rust
pub struct ThreadManager {
    // Existing fields remain unchanged.
    skill_feedback_enabled: bool,
}
```

Initialize it in `ThreadManager::new()`:

```rust
Self {
    // Existing initializers remain unchanged.
    skill_feedback_enabled: crate::skill_self_improvement_enabled(),
}
```

Add this crate-visible builder so tests and future composition code can inject a stable value without changing process environment:

```rust
pub(crate) fn with_skill_feedback_enabled(mut self, enabled: bool) -> Self {
    self.skill_feedback_enabled = enabled;
    self
}
```

- [ ] **Step 2: Run the manager test and confirm production is not wired**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  completed_thread_records_selected_skill_once -- --nocapture
```

Expected: FAIL because metrics remain unchanged.

- [ ] **Step 3: Resolve outcome before the final save and invoke feedback**

In the spawned task, move outcome normalization before final persistence and call the helper:

```rust
let outcome = match result {
    Ok(outcome) => outcome,
    Err(error) => ThreadOutcome::Failed {
        error: error.to_string(),
    },
};

crate::runtime::skill_feedback::record_terminal_skill_usage(
    &mut exec.thread,
    &outcome,
    &store_for_task,
    skill_feedback_enabled,
)
.await;

if let Err(e) = store_for_task.append_events(&exec.thread.events).await {
    tracing::debug!(thread_id = %thread_id, "failed to persist thread events: {e}");
}
if let Err(e) = store_for_task.save_thread(&exec.thread).await {
    tracing::debug!(thread_id = %thread_id, "failed to save final thread state: {e}");
}

completed.write().await.insert(thread_id, outcome.clone());
```

Capture `let skill_feedback_enabled = self.skill_feedback_enabled;` before the `tokio::spawn`. Remove the old later outcome-normalization block so each outcome is constructed once.

- [ ] **Step 4: Run manager and runtime tests**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  completed_thread_records_selected_skill_once -- --nocapture
taskset -c 0-5 cargo test -j6 -p lunarwing_engine runtime:: -- --test-threads=6
```

Expected: both commands exit 0; existing stop/resume tests remain green.

---

### Task 4: Focused Regression and Quality Gates

**Files:**
- Verify: all files changed by Tasks 1-3
- Update only if behavior differs: `docs/superpowers/specs/2026-07-15-engine-v2-skill-usage-feedback-design.md`

**Interfaces:**
- Consumes: completed implementation and tests
- Produces: verified, formatted, warning-free Engine V2 feedback loop

- [ ] **Step 1: Format and inspect the diff**

Run:

```bash
taskset -c 0-5 cargo fmt --all
```

Then inspect only the scoped files. Confirm no B-2/B-3 proposal, web, tenant, or legacy-runtime files changed.

- [ ] **Step 2: Run the engine crate test suite**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine -- --test-threads=6
```

Expected: all `lunarwing_engine` tests pass, including the new metadata, policy, manager, and resume cases.

- [ ] **Step 3: Run all-feature compile verification**

Run:

```bash
taskset -c 0-5 cargo check -j6 --all-features
```

Expected: exit code 0 with no compile errors.

- [ ] **Step 4: Run clippy for affected targets**

Run:

```bash
taskset -c 0-5 cargo clippy -j6 -p lunarwing_engine --all-targets --all-features -- -D warnings
```

Expected: exit code 0 with zero warnings.

- [ ] **Step 5: Verify formatting without mutation**

Run:

```bash
taskset -c 0-5 cargo fmt --all -- --check
```

Expected: exit code 0.

- [ ] **Step 6: Record residual risk in the final report**

Report that lifecycle pause/resume deduplication is covered, while transactional crash-safe exactly-once remains explicitly outside scope. Do not claim live-tenant verification unless it was separately performed.

---

## Review-Driven Security Hardening Addendum

Final security review identified that syntactically valid orchestrator-emitted
UUIDs were not authorized against the executing thread's project/user scope.
The implementation therefore also requires:

- A shared scoped-visibility helper backed by `Store::list_memory_docs_with_shared`.
- Fail-closed behavior when scoped visibility cannot be loaded.
- Wrong-project and wrong-user UUID rejection while preserving same-project
  shared-owner support.
- A 64-unique-ID processing cap for terminal feedback.
- The same scoped authorization for direct `__record_skill_usage__` host calls,
  so custom/self-modified orchestrators cannot bypass the terminal path.
- TDD coverage for wrong project, wrong user, shared owner, cap overflow,
  disabled host calls, and allowed same-user host calls.
