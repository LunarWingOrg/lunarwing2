//! WI#535: exercise the real boot/message paths against durable workspace state.

use super::*;
use lunarwing_engine::{Mission, MissionCadence, ProjectId};
use tracing::Instrument;
use tracing_test::traced_test;

use crate::testing::FaultInjectingTestDb;
use crate::workspace::Workspace;

const RECOVERED: &str = "re-created default project; unloadable record preserved";
const PRIVATE_CONTENT: &str = "PRIVATE_PROJECT_METADATA_CANARY";

fn default_path(project: &Project) -> String {
    format!(
        "engine/projects/default--{}/project.json",
        &project.id.to_string()[..8]
    )
}

#[cfg(test)]
async fn backup_paths(workspace: &Workspace, path: &str) -> Vec<String> {
    let prefix = format!("backups/{path}.unloadable-");
    workspace
        .list_all()
        .await
        .unwrap()
        .into_iter()
        .filter(|path| path.starts_with(&prefix))
        .collect()
}

#[cfg(test)]
async fn reloaded_store(workspace: &Arc<Workspace>) -> Arc<HybridStore> {
    let store = Arc::new(HybridStore::new(Some(workspace.clone())));
    store.load_state_from_workspace().await.unwrap();
    store
}

#[cfg(test)]
async fn assert_recovery(owner_boot: bool, damaged: &str) -> (String, String, ProjectId) {
    let db = FaultInjectingTestDb::new().await;
    let workspace = Arc::new(Workspace::new_with_db("recovery", db.database()));
    let store = HybridStore::new(Some(workspace.clone()));
    let project = new_default_project("recovery-user", "custom description");
    let path = default_path(&project);
    let mission = Mission::new(
        project.id,
        "recovery-user",
        "preserved mission",
        "must stay attached",
        MissionCadence::Manual,
    );
    store.save_project(&project).await.unwrap();
    store.save_mission(&mission).await.unwrap();
    workspace.write(&path, damaged).await.unwrap();

    let reloaded = reloaded_store(&workspace).await;
    assert!(reloaded.load_project(project.id).await.unwrap().is_none());
    let store_dyn: Arc<dyn Store> = reloaded.clone();
    let resolved = if owner_boot {
        resolve_owner_default_project(&store_dyn, "recovery-user")
            .await
            .unwrap()
    } else {
        let owner = new_default_project("another-owner", "owner project");
        store_dyn.save_project(&owner).await.unwrap();
        resolve_user_project(&store_dyn, "recovery-user", owner.id)
            .await
            .unwrap()
    };
    assert_eq!(resolved, project.id);
    let backups = backup_paths(&workspace, &path).await;
    assert_eq!(backups.len(), 1);
    let backup = &backups[0];
    assert_eq!(
        workspace
            .read_primary(backup)
            .await
            .unwrap()
            .content
            .as_bytes(),
        damaged.as_bytes()
    );
    assert_eq!(
        db.query_scalar_i64(
            "SELECT count(*) FROM memory_chunks c JOIN memory_documents d \
             ON c.document_id = d.id WHERE d.path = ?",
            backup,
        )
        .await,
        0,
        "recovery evidence must not be search-indexed"
    );
    let fresh = reloaded_store(&workspace).await;
    let recovered = fresh.load_project(project.id).await.unwrap().unwrap();
    assert_eq!(recovered.name, "default");
    assert_eq!(recovered.user_id, "recovery-user");
    let missions = fresh
        .list_missions(project.id, "recovery-user")
        .await
        .unwrap();
    assert_eq!(missions.len(), 1);
    assert_eq!(missions[0].id, mission.id);
    let fresh_dyn: Arc<dyn Store> = fresh;
    resolve_owner_default_project(&fresh_dyn, "recovery-user")
        .await
        .unwrap();
    assert_eq!(backup_paths(&workspace, &path).await, backups);
    (path, backup.clone(), project.id)
}

#[cfg(test)]
fn assert_recovery_warning(
    lines: &[&str],
    path: &str,
    backup: &str,
    id: ProjectId,
) -> Result<(), String> {
    let warnings: Vec<_> = lines
        .iter()
        .filter(|line| line.contains(" WARN ") && line.contains(RECOVERED))
        .collect();
    if warnings.len() != 1
        || ![path, backup, &id.to_string()]
            .iter()
            .all(|needle| warnings[0].contains(needle))
    {
        return Err(format!(
            "expected one recovery warning with both paths/id: {warnings:?}"
        ));
    }
    if lines.iter().any(|line| {
        (line.contains(" WARN ") || line.contains("ERROR ")) && line.contains(PRIVATE_CONTENT)
    }) {
        return Err("record content reached a warning/error".into());
    }
    Ok(())
}

#[tokio::test]
#[traced_test]
async fn owner_boot_preserves_unloadable_default_project() {
    let (path, backup, id) = assert_recovery(
        true,
        &format!("{{\n\"description\": \"{PRIVATE_CONTENT} Ω\""),
    )
    .await;
    logs_assert(|lines| assert_recovery_warning(lines, &path, &backup, id));
}

#[tokio::test]
#[traced_test]
async fn message_resolution_preserves_unloadable_default_project() {
    // Valid JSON with a wrong typed field must also be preserved, not merely
    // syntactically corrupt JSON. The raw serde error would quote the canary.
    let mut raw = serde_json::to_value(new_default_project("recovery-user", "custom")).unwrap();
    raw["created_at"] = serde_json::json!(PRIVATE_CONTENT);
    let (path, backup, id) = assert_recovery(false, &raw.to_string()).await;
    logs_assert(|lines| assert_recovery_warning(lines, &path, &backup, id));
}

#[tokio::test]
#[traced_test]
async fn concurrent_default_project_recovery_preserves_each_generation_once() {
    let (db, _dir) = crate::testing::test_db().await;
    let workspace = Arc::new(Workspace::new_with_db("repeated-recovery", db));
    let project = new_default_project("user", "default");
    let path = default_path(&project);
    let store = HybridStore::new(Some(workspace.clone()));
    store.save_project(&project).await.unwrap();

    // Includes an empty/truncated-to-zero file. Concurrent saves race the
    // same recovery; a subsequent corrupt generation must retain both copies.
    for damaged in ["", "second generation {"] {
        workspace.write(&path, damaged).await.unwrap();
        let reloaded = reloaded_store(&workspace).await;
        let mut saves = Vec::new();
        let barrier = Arc::new(tokio::sync::Barrier::new(16));
        for _ in 0..16 {
            let store = reloaded.clone();
            let barrier = barrier.clone();
            let project = project.clone();
            saves.push(tokio::spawn(
                async move {
                    barrier.wait().await;
                    store.save_project(&project).await.unwrap();
                }
                .in_current_span(),
            ));
        }
        for save in saves {
            save.await.unwrap();
        }
        assert_eq!(reloaded.list_projects("user").await.unwrap().len(), 1);
    }
    let paths = backup_paths(&workspace, &path).await;
    assert_eq!(paths.len(), 2);
    let mut contents = Vec::new();
    for path in paths {
        contents.push(workspace.read_primary(&path).await.unwrap().content);
    }
    contents.sort();
    assert_eq!(contents, ["", "second generation {"]);
    logs_assert(|lines| {
        let count = lines
            .iter()
            .filter(|line| line.contains(" WARN ") && line.contains(RECOVERED))
            .count();
        if count == 2 {
            Ok(())
        } else {
            Err(format!("expected two recovery warnings, got {count}"))
        }
    });
}

#[tokio::test]
#[traced_test]
async fn default_project_backup_failure_refuses_replacement_and_cache_publication() {
    let db = FaultInjectingTestDb::new().await;
    let workspace = Arc::new(Workspace::new_with_db("backup-failure", db.database()));
    let project = new_default_project("user", "default");
    let path = default_path(&project);
    workspace.write(&path, "{original bytes").await.unwrap();
    let store = reloaded_store(&workspace).await;
    db.execute_batch(
        "CREATE TRIGGER reject_project_backup BEFORE UPDATE OF content ON memory_documents
         WHEN NEW.path LIKE 'backups/engine/projects/%'
         BEGIN SELECT RAISE(FAIL, 'backup rejected'); END;",
    )
    .await;

    assert!(store.save_project(&project).await.is_err());
    assert_eq!(
        workspace.read_primary(&path).await.unwrap().content,
        "{original bytes"
    );
    assert!(store.load_project(project.id).await.unwrap().is_none());
    logs_assert(|lines| {
        if lines.iter().any(|line| line.contains(RECOVERED)) {
            Err("failed recovery was logged as successful".into())
        } else if lines
            .iter()
            .any(|line| line.contains(" WARN ") && line.contains("preservation failed"))
        {
            Ok(())
        } else {
            Err("missing preservation failure warning".into())
        }
    });
    db.execute_batch("DROP TRIGGER reject_project_backup;")
        .await;
    store.save_project(&project).await.unwrap();
    assert!(
        reloaded_store(&workspace)
            .await
            .load_project(project.id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
#[traced_test]
async fn default_project_replacement_failure_retains_source_backup_and_cache() {
    let db = FaultInjectingTestDb::new().await;
    let workspace = Arc::new(Workspace::new_with_db("replacement-failure", db.database()));
    let mut project = new_default_project("user", "cached version");
    let path = default_path(&project);
    let store = HybridStore::new(Some(workspace.clone()));
    store.save_project(&project).await.unwrap();
    workspace.write(&path, "{damaged after load").await.unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_project_replacement BEFORE UPDATE OF content ON memory_documents
         WHEN NEW.path LIKE 'engine/projects/%/project.json'
         BEGIN SELECT RAISE(FAIL, 'replacement rejected'); END;",
    )
    .await;

    project.description = "new version".into();
    assert!(store.save_project(&project).await.is_err());
    assert_eq!(
        store
            .load_project(project.id)
            .await
            .unwrap()
            .unwrap()
            .description,
        "cached version"
    );
    assert_eq!(
        workspace.read_primary(&path).await.unwrap().content,
        "{damaged after load"
    );
    let backups = backup_paths(&workspace, &path).await;
    assert_eq!(backups.len(), 1);
    assert_eq!(
        workspace.read_primary(&backups[0]).await.unwrap().content,
        "{damaged after load"
    );
    logs_assert(|lines| {
        if lines.iter().any(|line| line.contains(RECOVERED)) {
            Err("failed replacement was logged as successful".into())
        } else if lines.iter().any(|line| {
            line.contains(" WARN ")
                && line.contains("failed to persist project")
                && line.contains(&backups[0])
        }) {
            Ok(())
        } else {
            Err("missing persistence failure warning with backup path".into())
        }
    });
}

#[tokio::test]
#[traced_test]
async fn readable_default_project_updates_do_not_create_backups() {
    let (db, _dir) = crate::testing::test_db().await;
    let workspace = Arc::new(Workspace::new_with_db("healthy-default", db));
    let store = HybridStore::new(Some(workspace.clone()));
    let mut project = new_default_project("user", "first");
    let path = default_path(&project);
    store.save_project(&project).await.unwrap();
    project.description = "updated".into();
    store.save_project(&project).await.unwrap();
    assert!(backup_paths(&workspace, &path).await.is_empty());
    assert_eq!(
        reloaded_store(&workspace)
            .await
            .load_project(project.id)
            .await
            .unwrap()
            .unwrap()
            .description,
        "updated"
    );
    assert!(!logs_contain(RECOVERED));
}

#[tokio::test]
async fn default_project_preservation_reads_only_the_written_scope() {
    let (db, _dir) = crate::testing::test_db().await;
    let project = new_default_project("user", "default");
    let path = default_path(&project);
    let foreign = Workspace::new_with_db("foreign", db.clone());
    foreign.write(&path, "{foreign bytes").await.unwrap();
    let workspace = Arc::new(
        Workspace::new_with_db("primary", db).with_additional_read_scopes(vec!["foreign".into()]),
    );
    let store = HybridStore::new(Some(workspace.clone()));
    store.save_project(&project).await.unwrap();
    assert!(backup_paths(&workspace, &path).await.is_empty());
    assert_eq!(
        foreign.read_primary(&path).await.unwrap().content,
        "{foreign bytes"
    );
    assert!(
        serde_json::from_str::<Project>(&workspace.read_primary(&path).await.unwrap().content)
            .is_ok()
    );
}
