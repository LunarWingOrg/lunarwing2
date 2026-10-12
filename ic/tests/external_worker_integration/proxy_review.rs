//! MR !1540 DX regressions, through the production worker WebSocket path.
use super::*;

#[tokio::test]
async fn large_task_results_and_progress_complete_with_and_without_proxy() {
    let output = "large-worker-result\n".repeat(90_000); // > 1.5 MiB
    assert!(output.len() > 1536 * 1024);
    for enabled in [false, true] {
        for streamed in [false, true] {
            let (url, connections) = spawn_mock_worker_async(MockBehavior::Success {
                progress: if streamed {
                    vec![output.clone()]
                } else {
                    Vec::new()
                },
                output: if streamed {
                    String::new()
                } else {
                    output.clone()
                },
            })
            .await;
            let mut config = config_for(&url);
            if enabled {
                config.tool_allowlist = vec!["memory_read".to_string()];
            }
            let mgr = ExternalWorkerManager::new(vec![config]);
            let mgr = if enabled {
                mgr.with_tool_proxy(test_proxy(Arc::new(lunarwing::tools::ToolRegistry::new())))
            } else {
                mgr
            };
            // Successful large results also leave a reusable connection.
            for _ in 0..2 {
                let result = timeout(
                    TEST_BOUND,
                    mgr.execute_task(
                        Uuid::new_v4(),
                        "mock",
                        "task",
                        Some(10_000),
                        true,
                        TaskContext::default(),
                    ),
                )
                .await
                .unwrap()
                .unwrap()
                .unwrap();
                assert_eq!(result.status, ExternalTaskStatus::Success);
                assert_eq!(result.output, output);
            }
            assert_eq!(connections.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn large_worker_error_is_preserved() {
    let error = "worker failure details\n".repeat(80_000);
    assert!(error.len() > 1536 * 1024);
    let (url, _) = spawn_mock_worker_async(MockBehavior::Failed {
        error: error.clone(),
    })
    .await;
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)]);
    let result = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(10_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(result.status, ExternalTaskStatus::Failed);
    assert_eq!(result.error.as_deref(), Some(error.as_str()));
}

#[tokio::test]
async fn fragmented_messages_obey_the_total_message_limit() {
    for bytes in [1536 * 1024, 17 * 1024 * 1024] {
        let output = "A".repeat(bytes);
        let text = envelope(
            "task_result",
            serde_json::json!({
                "task_id": "t", "status": "success", "output": output,
                "error": null, "duration_ms": 1,
            }),
        );
        let (url, _) = spawn_mock_worker_async(MockBehavior::FragmentedText(text)).await;
        let mgr = ExternalWorkerManager::new(vec![config_for(&url)]);
        let result = timeout(
            TEST_BOUND,
            mgr.execute_task(
                Uuid::new_v4(),
                "mock",
                "task",
                Some(10_000),
                true,
                TaskContext::default(),
            ),
        )
        .await
        .expect("test timed out");
        if bytes < 16 * 1024 * 1024 {
            let result = result.unwrap().unwrap();
            assert_eq!(result.status, ExternalTaskStatus::Success);
            assert_eq!(result.output, output);
        } else {
            let err = result.expect_err("fragmentation must not bypass the message limit");
            assert!(
                matches!(err, OrchestratorError::ExternalWorkerProtocolError { .. }),
                "{err}"
            );
            assert!(err.to_string().contains("Message too long"), "{err}");
        }
    }
}

#[tokio::test]
async fn gitlab_registry_aliases_preserve_grants_and_catalog_names() {
    for registered in ["gitlab_tool", "gitlab-tool"] {
        for granted in ["gitlab_tool", "gitlab-tool"] {
            let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
            let probe = Arc::new(GateProbeTool {
                tool_name: registered.to_string(),
                output: "ok".to_string(),
                autonomous: true,
                calls: AtomicUsize::new(0),
            });
            tools.register(probe.clone()).await;
            let (replies, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let (url, _) = spawn_mock_worker_async(MockBehavior::ToolProxyRaw {
                frames: vec![
                    ("tool_list".to_string(), serde_json::json!({})),
                    ("tool_call".to_string(), serde_json::json!({
                        "task_id": "@task_id@", "call_id": "alias", "name": granted, "arguments": {},
                    })),
                    ("tool_call".to_string(), serde_json::json!({
                        "task_id": "@task_id@", "call_id": "not-granted", "name": "memory_read", "arguments": {},
                    })),
                ], replies,
            }).await;
            let mut config = config_for(&url);
            config.tool_allowlist = vec![granted.to_string()];
            let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools));
            let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut rx, 3).await;
            // Registry aliases run from provider-safe spelling to legacy,
            // not the reverse. An unresolvable grant must stay fail-closed.
            if registered == "gitlab_tool" && granted == "gitlab-tool" {
                assert_eq!(replies[0]["payload"]["tools"], serde_json::json!([]));
                assert_eq!(replies[1]["payload"]["status"], "error");
                assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
            } else {
                assert_eq!(replies[0]["payload"]["tools"][0]["name"], granted);
                assert_eq!(
                    replies[1]["payload"]["status"], "success",
                    "{registered}/{granted}: {}",
                    replies[1]
                );
                assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
            }
            assert_eq!(replies[2]["payload"]["status"], "denied");
        }
    }
}

#[cfg(feature = "libsql")]
#[tokio::test]
async fn legacy_gitlab_resolution_keeps_persisted_permission_gates() {
    for permission_name in ["gitlab_tool", "gitlab-tool"] {
        let (db, _tmp) = test_db().await;
        db.set_setting(
            "tenant-user",
            &format!("tool_permissions.{permission_name}"),
            &serde_json::json!("disabled"),
        )
        .await
        .unwrap();
        let probe = Arc::new(GateProbeTool {
            tool_name: "gitlab-tool".to_string(),
            output: "ok".to_string(),
            autonomous: true,
            calls: AtomicUsize::new(0),
        });
        let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
        tools.register(probe.clone()).await;
        let (replies, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (url, _) = spawn_mock_worker_async(MockBehavior::ToolProxy {
            calls: vec![("gitlab_tool".to_string(), serde_json::json!({}))],
            replies,
        })
        .await;
        let mut config = config_for(&url);
        config.tool_allowlist = vec!["gitlab_tool".to_string()];
        let mgr = ExternalWorkerManager::new(vec![config])
            .with_tool_proxy(test_proxy(tools))
            .with_store(db);
        let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut rx, 1).await;
        assert_eq!(replies[0]["payload"]["status"], "denied");
        assert!(
            replies[0]["payload"]["error"]
                .as_str()
                .unwrap()
                .contains("disabled")
        );
        assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    }
}

#[cfg(feature = "libsql")]
mod memory_policy {
    use super::*;
    use lunarwing::hooks::{Hook, HookContext, HookError, HookEvent, HookOutcome, HookPoint};
    use lunarwing::workspace::paths;

    async fn memory_tools(workspace: &Arc<Workspace>) -> Arc<lunarwing::tools::ToolRegistry> {
        let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
        tools
            .register(Arc::new(MemoryWriteTool::from_workspace(workspace.clone())))
            .await;
        tools
            .register(Arc::new(MemoryReadTool::from_workspace(workspace.clone())))
            .await;
        tools
    }

    async fn manager(
        calls: Vec<(String, serde_json::Value)>,
        tools: Arc<lunarwing::tools::ToolRegistry>,
    ) -> (
        ExternalWorkerManager,
        tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    ) {
        let (replies, rx) = tokio::sync::mpsc::unbounded_channel();
        let (url, _) = spawn_mock_worker_async(MockBehavior::ToolProxy { calls, replies }).await;
        let mut config = config_for(&url);
        config.tool_allowlist = vec!["memory_write".to_string(), "memory_read".to_string()];
        (
            ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools)),
            rx,
        )
    }

    #[tokio::test]
    async fn prompt_targets_are_denied_but_ordinary_notes_roundtrip() {
        let (db, _tmp) = test_db().await;
        let workspace = Arc::new(Workspace::new_with_db("tenant-user", db));
        let daily = format!("daily/{}.md", chrono::Utc::now().format("%Y-%m-%d"));
        let protected = [
            paths::SOUL,
            paths::AGENTS,
            paths::USER,
            paths::IDENTITY,
            paths::TOOLS,
            paths::MEMORY,
            paths::HEARTBEAT,
            paths::BOOTSTRAP,
            paths::ASSISTANT_DIRECTIVES,
            paths::PROFILE,
            "lorebooks/x.md",
            &daily,
        ];
        for path in protected {
            workspace
                .write(path, "original owner content")
                .await
                .unwrap();
        }
        let mut targets: Vec<&str> = protected.to_vec();
        targets.extend([
            "memory",
            "heartbeat",
            "daily_log",
            "bootstrap",
            " notes/../SOUL.md ",
            "./LOREBOOKS//x.md",
            "context/../context/profile.json",
        ]);
        let mut calls: Vec<_> = targets
            .iter()
            .map(|target| {
                (
                    "memory_write".to_string(),
                    serde_json::json!({
                        "target": target, "content": "worker supplied content", "append": false,
                    }),
                )
            })
            .collect();
        // Omitted target defaults to the prompt-visible daily log.
        calls.push((
            "memory_write".to_string(),
            serde_json::json!({"content": "default daily target"}),
        ));
        let denied = calls.len();
        calls.extend([
            ("memory_write".to_string(), serde_json::json!({"target": "notes/proxied.md", "content": "safe report", "append": false})),
            ("memory_read".to_string(), serde_json::json!({"path": "notes/proxied.md"})),
        ]);
        let (mgr, mut rx) = manager(calls, memory_tools(&workspace).await).await;
        let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut rx, denied + 2).await;
        for reply in &replies[..denied] {
            assert_eq!(reply["payload"]["status"], "denied", "{reply}");
            assert!(
                reply["payload"]["error"]
                    .as_str()
                    .unwrap()
                    .contains("system-prompt"),
                "{reply}"
            );
        }
        assert_eq!(replies[denied]["payload"]["status"], "success");
        assert_eq!(replies[denied + 1]["payload"]["status"], "success");
        assert!(
            replies[denied + 1]["payload"]["output"]
                .as_str()
                .unwrap()
                .contains("safe report")
        );
        for path in protected {
            assert_eq!(
                workspace.read(path).await.unwrap().content,
                "original owner content",
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn layer_selection_is_denied_even_for_ordinary_notes() {
        let (db, _tmp) = test_db().await;
        let workspace = Arc::new(Workspace::new_with_db("tenant-user", db));
        let layers = [
            serde_json::json!("private"),
            serde_json::json!("household"),
            serde_json::json!("finance"),
            serde_json::json!(""),
            serde_json::Value::Null,
        ];
        let calls = layers.iter().map(|layer| ("memory_write".to_string(), serde_json::json!({
            "target": "notes/proxied.md", "content": "worker content", "layer": layer, "force": true,
        }))).collect();
        let (mgr, mut rx) = manager(calls, memory_tools(&workspace).await).await;
        let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut rx, layers.len()).await;
        for reply in replies {
            assert_eq!(reply["payload"]["status"], "denied", "{reply}");
            assert!(
                reply["payload"]["error"]
                    .as_str()
                    .unwrap()
                    .contains("memory layer"),
                "{reply}"
            );
        }
        assert!(workspace.read("notes/proxied.md").await.is_err());
    }

    struct RewriteHook(serde_json::Value);
    #[async_trait::async_trait]
    impl Hook for RewriteHook {
        fn name(&self) -> &str {
            "rewrite-memory-destination"
        }
        fn hook_points(&self) -> &[HookPoint] {
            &[HookPoint::BeforeToolCall]
        }
        async fn execute(&self, _: &HookEvent, _: &HookContext) -> Result<HookOutcome, HookError> {
            Ok(HookOutcome::modify(self.0.to_string()))
        }
    }

    #[tokio::test]
    async fn hook_final_parameters_cannot_bypass_memory_policy() {
        let (db, _tmp) = test_db().await;
        let workspace = Arc::new(Workspace::new_with_db("tenant-user", db));
        workspace
            .write(paths::SOUL, "original owner content")
            .await
            .unwrap();
        for params in [
            serde_json::json!({"target": "SOUL.md", "content": "rewritten", "append": false}),
            serde_json::json!({"target": "notes/proxied.md", "content": "rewritten", "layer": "household"}),
        ] {
            let tools = memory_tools(&workspace).await;
            let hooks = Arc::new(HookRegistry::new());
            hooks.register(Arc::new(RewriteHook(params))).await;
            let (mgr, mut rx) = manager(
                vec![(
                    "memory_write".to_string(),
                    serde_json::json!({
                        "target": "notes/proxied.md", "content": "safe initial report",
                    }),
                )],
                tools.clone(),
            )
            .await;
            let mgr = mgr.with_tool_proxy(ToolProxy::new(tools, test_safety(), hooks));
            let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut rx, 1).await;
            assert_eq!(replies[0]["payload"]["status"], "denied", "{}", replies[0]);
        }
        assert_eq!(
            workspace.read(paths::SOUL).await.unwrap().content,
            "original owner content"
        );
        assert!(workspace.read("notes/proxied.md").await.is_err());
    }
}
