//! Regressions for !1542 verdict: runtime ownership, terminal races, snapshots.

use super::*;

#[test]
fn staging_rejects_unsupported_uid_mappings() {
    let docker = RuntimeFlavor {
        kind: WorkerRuntimeKind::Docker,
        podman_version: None,
    };
    let podman = RuntimeFlavor {
        kind: WorkerRuntimeKind::Podman,
        podman_version: Some((5, 2)),
    };
    let rootful = vec!["name=seccomp,profile=builtin".to_string()];
    let rootless = vec!["name=rootless".to_string()];
    let remapped = vec!["name=userns".to_string()];
    assert_eq!(
        staging_userns_mode(docker, Some(&rootful), Some(1000)),
        Ok(None)
    );
    for options in [Some(rootless.as_slice()), Some(remapped.as_slice()), None] {
        assert!(staging_userns_mode(docker, options, Some(1000)).is_err());
    }
    for uid in [Some(0), Some(1001), None] {
        assert!(staging_userns_mode(docker, Some(&rootful), uid).is_err());
    }
    assert_eq!(
        staging_userns_mode(podman, Some(&rootless), Some(1001)),
        Ok(Some("keep-id:uid=1000,gid=1000".to_string()))
    );
    assert!(staging_userns_mode(podman, Some(&rootful), Some(1000)).is_err());
    assert!(staging_userns_mode(podman, Some(&rootless), Some(0)).is_err());
    assert!(staging_userns_mode(podman, None, Some(1000)).is_err());
    for version in [None, Some((4, 9))] {
        assert!(
            staging_userns_mode(
                RuntimeFlavor {
                    podman_version: version,
                    ..podman
                },
                Some(&rootless),
                Some(1000),
            )
            .is_err()
        );
    }
}

fn running_handle(job_id: Uuid) -> ContainerHandle {
    ContainerHandle {
        job_id,
        container_id: "review-test-container".to_string(),
        state: ContainerState::Running,
        mode: JobMode::Harness,
        owner_id: "owner".to_string(),
        mcp_servers: Vec::new(),
        created_at: Utc::now(),
        project_dir: None,
        task_description: "review regression".to_string(),
        last_worker_status: None,
        worker_iteration: 0,
        completion_result: None,
        harness: Some(HarnessJobState {
            staging_dir: Some(PathBuf::from("unused-review-staging")),
            write_back: Some(harness::WriteBackPolicy::default()),
            promotion: None,
        }),
    }
}

#[tokio::test]
async fn stop_claims_terminal_state_before_container_io() {
    let mgr = ContainerJobManager::new(ContainerJobConfig::default(), TokenStore::new());
    let job_id = Uuid::new_v4();
    mgr.containers
        .write()
        .await
        .insert(job_id, running_handle(job_id));
    // Pause stop at its first container-I/O await, without a Docker runtime.
    let _docker_guard = mgr.docker.write().await;
    let stop = mgr.stop_job(job_id);
    tokio::pin!(stop);
    assert!(futures::poll!(&mut stop).is_pending());
    let handle = mgr.get_handle(job_id).await.unwrap();
    assert_eq!(handle.state, ContainerState::Stopped);
    assert!(!handle.completion_result.unwrap().success);
    // A /complete arriving during the SIGTERM grace period cannot promote.
    mgr.complete_job(
        job_id,
        CompletionResult {
            success: true,
            message: None,
        },
    )
    .await
    .unwrap();
    assert!(!claim_unreported_exit(&mgr.containers, job_id, "late exit").await);
    let handle = mgr.get_handle(job_id).await.unwrap();
    assert_eq!(handle.state, ContainerState::Stopped);
    assert!(!handle.completion_result.unwrap().success);
    assert!(handle.harness.unwrap().promotion.is_none());
}

#[tokio::test]
async fn finalizing_blocks_stop_cleanup_and_exit_watcher() {
    let mgr = ContainerJobManager::new(ContainerJobConfig::default(), TokenStore::new());
    let job_id = Uuid::new_v4();
    mgr.containers
        .write()
        .await
        .insert(job_id, running_handle(job_id));
    mgr.token_store
        .store_harness_secrets(
            job_id,
            vec![(
                "FIXTURE".to_string(),
                secrecy::SecretString::from("harness-review-fixture".to_string()),
            )],
        )
        .await;
    // Pause /complete after it claims Finalizing, before Docker teardown.
    let _docker_guard = mgr.docker.write().await;
    let complete = mgr.complete_job(
        job_id,
        CompletionResult {
            success: true,
            message: None,
        },
    );
    tokio::pin!(complete);
    assert!(futures::poll!(&mut complete).is_pending());
    assert!(matches!(
        mgr.stop_job(job_id).await,
        Err(OrchestratorError::InvalidContainerState { .. })
    ));
    mgr.cleanup_job(job_id).await;
    assert!(!claim_unreported_exit(&mgr.containers, job_id, "late exit").await);
    // A duplicate completion also cannot replace the accepted result.
    mgr.complete_job(
        job_id,
        CompletionResult {
            success: false,
            message: None,
        },
    )
    .await
    .unwrap();
    let handle = mgr.get_handle(job_id).await.unwrap();
    assert_eq!(handle.state, ContainerState::Finalizing);
    assert!(handle.completion_result.unwrap().success);
    assert!(mgr.token_store.harness_secrets(job_id).await.is_some());
}

#[tokio::test]
async fn watcher_claim_prevents_late_completion_and_cleanup_removes_handle() {
    let mgr = ContainerJobManager::new(ContainerJobConfig::default(), TokenStore::new());
    let job_id = Uuid::new_v4();
    mgr.containers
        .write()
        .await
        .insert(job_id, running_handle(job_id));
    assert!(claim_unreported_exit(&mgr.containers, job_id, "exit code 1").await);
    mgr.complete_job(
        job_id,
        CompletionResult {
            success: true,
            message: None,
        },
    )
    .await
    .unwrap();
    let handle = mgr.get_handle(job_id).await.unwrap();
    assert_eq!(handle.state, ContainerState::Stopped);
    assert!(!handle.completion_result.unwrap().success);
    assert!(handle.harness.unwrap().promotion.is_none());
    mgr.cleanup_job(job_id).await;
    assert!(mgr.get_handle(job_id).await.is_none());
    assert!(!claim_unreported_exit(&mgr.containers, job_id, "late exit").await);
}

#[cfg(all(test, feature = "libsql"))]
mod promotion {
    use super::*;
    use crate::error::WorkspaceError;
    use crate::testing::FaultInjectingTestDb;
    use crate::tools::builtin::memory::FixedWorkspaceResolver;
    use crate::workspace::Workspace;

    struct Fixture {
        db: FaultInjectingTestDb,
        workspace: Arc<Workspace>,
        staging: PathBuf,
        job_id: Uuid,
    }

    impl Fixture {
        async fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let staging = temp.path().join("staging");
            std::fs::create_dir_all(staging.join("projects")).unwrap();
            std::fs::write(staging.join("projects/out.md"), "replacement").unwrap();
            let db = FaultInjectingTestDb::new_in(temp).await;
            let workspace = Arc::new(
                Workspace::new_with_db("owner", db.database())
                    .with_additional_read_scopes(vec!["shared".to_string()]),
            );
            Self {
                db,
                workspace,
                staging,
                job_id: Uuid::new_v4(),
            }
        }

        async fn run(&self) -> harness::PromotionSummary {
            let deps = PromotionDeps {
                containers: Arc::new(RwLock::new(HashMap::new())),
                token_store: TokenStore::new(),
                workspace: Some(Arc::new(FixedWorkspaceResolver::new(
                    self.workspace.clone(),
                ))),
                store: None,
            };
            run_promotion(
                &deps,
                self.job_id,
                "owner",
                &self.staging,
                &harness::WriteBackPolicy::default(),
            )
            .await
        }

        fn backup_path(&self) -> String {
            format!("backups/harness/{}/projects/out.md", self.job_id)
        }
    }

    #[tokio::test]
    async fn replacement_snapshot_is_recoverable_but_not_searchable() {
        let fixture = Fixture::new().await;
        let prior = "harnessrollbacksentinel prior project notes";
        fixture
            .workspace
            .write("projects/out.md", prior)
            .await
            .unwrap();
        assert!(
            !fixture
                .workspace
                .search("harnessrollbacksentinel", 10)
                .await
                .unwrap()
                .is_empty()
        );
        let summary = fixture.run().await;
        assert!(summary.rejections.is_empty());
        assert_eq!(summary.records.len(), 1);
        let record = &summary.records[0];
        assert_eq!(
            record.previous_sha256,
            Some(harness::sha256_hex(prior.as_bytes()))
        );
        assert_eq!(
            record.backup_path.as_deref(),
            Some(fixture.backup_path().as_str())
        );
        let backup = fixture
            .workspace
            .read(&fixture.backup_path())
            .await
            .unwrap();
        assert_eq!(backup.content, prior);
        assert_eq!(
            fixture
                .workspace
                .read("projects/out.md")
                .await
                .unwrap()
                .content,
            "replacement"
        );
        assert!(
            fixture
                .workspace
                .search("harnessrollbacksentinel", 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fixture
                .db
                .query_scalar_i64(
                    "SELECT COUNT(*) FROM memory_chunks WHERE document_id = ?1",
                    &backup.id.to_string(),
                )
                .await,
            0
        );
    }

    #[tokio::test]
    async fn missing_primary_document_does_not_snapshot_an_inherited_document() {
        let fixture = Fixture::new().await;
        let shared = Workspace::new_with_db("shared", fixture.db.database());
        shared
            .write("projects/out.md", "inherited project notes")
            .await
            .unwrap();
        let summary = fixture.run().await;
        assert_eq!(summary.records.len(), 1);
        assert!(summary.records[0].previous_sha256.is_none());
        assert!(summary.records[0].backup_path.is_none());
        assert_eq!(
            shared
                .read_primary("projects/out.md")
                .await
                .unwrap()
                .content,
            "inherited project notes"
        );
        assert_eq!(
            fixture
                .workspace
                .read_primary("projects/out.md")
                .await
                .unwrap()
                .content,
            "replacement"
        );
    }

    #[tokio::test]
    async fn source_read_failure_rejects_instead_of_treating_document_as_missing() {
        let fixture = Fixture::new().await;
        fixture
            .workspace
            .write("projects/out.md", "prior notes")
            .await
            .unwrap();
        fixture
            .db
            .execute_batch("ALTER TABLE memory_documents RENAME TO unavailable_documents")
            .await;
        let summary = fixture.run().await;
        assert!(summary.records.is_empty());
        assert_eq!(summary.rejections.len(), 1);
        assert!(
            summary.rejections[0]
                .reason
                .starts_with("rollback source read failed:")
        );
        assert_eq!(
            fixture
                .db
                .query_scalar_string(
                    "SELECT content FROM unavailable_documents WHERE path = 'projects/out.md'",
                )
                .await,
            "prior notes"
        );
    }

    #[tokio::test]
    async fn snapshot_failure_leaves_original_untouched() {
        let fixture = Fixture::new().await;
        fixture
            .workspace
            .write("projects/out.md", "prior notes")
            .await
            .unwrap();
        fixture
            .db
            .execute_batch(
                "CREATE TRIGGER reject_backup BEFORE INSERT ON memory_documents
             WHEN NEW.path LIKE 'backups/harness/%'
             BEGIN SELECT RAISE(ABORT, 'snapshot fault'); END;",
            )
            .await;
        let summary = fixture.run().await;
        assert!(summary.records.is_empty());
        assert!(
            summary.rejections[0]
                .reason
                .starts_with("rollback snapshot failed:")
        );
        assert_eq!(
            fixture
                .workspace
                .read("projects/out.md")
                .await
                .unwrap()
                .content,
            "prior notes"
        );
    }

    #[tokio::test]
    async fn failed_replacement_removes_unused_snapshot() {
        let fixture = Fixture::new().await;
        fixture
            .workspace
            .write("projects/out.md", "prior notes")
            .await
            .unwrap();
        fixture
            .db
            .execute_batch(
                "CREATE TRIGGER reject_replacement BEFORE UPDATE ON memory_documents
             WHEN NEW.path = 'projects/out.md' AND NEW.content = 'replacement'
             BEGIN SELECT RAISE(ABORT, 'write fault'); END;",
            )
            .await;
        let summary = fixture.run().await;
        assert!(summary.records.is_empty());
        assert!(
            summary.rejections[0]
                .reason
                .starts_with("workspace write rejected:")
        );
        assert_eq!(
            fixture
                .workspace
                .read("projects/out.md")
                .await
                .unwrap()
                .content,
            "prior notes"
        );
        assert!(matches!(
            fixture.workspace.read(&fixture.backup_path()).await,
            Err(WorkspaceError::DocumentNotFound { .. })
        ));
    }
}
