#[tokio::test]
async fn engine_v2_mcp_auth_resumes_same_thread_once_after_real_oauth_callback() {
    let _lock = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("gateway"));
    let oauth_env = OAuthEnvGuard::enable_gateway_callback();
    lunarwing::bridge::reset_engine_state().await;

    let server = start_mock_mcp_server(vec![mock_tool_response("authenticated-search-result")]).await;
    let config = McpServerConfig::new(SERVER_NAME, server.mcp_url());
    let token_name = config.token_secret_name();
    let provider = Arc::new(McpAuthLlm::new());
    let llm: Arc<dyn LlmProvider> = provider.clone();
    let rig = TestRigBuilder::new()
        .with_channel_name("gateway")
        .with_llm(llm)
        .with_mcp_server_config(config)
        .build()
        .await;
    let v1_thread_id = Uuid::new_v4();

    rig.send_incoming(gateway_message(
        v1_thread_id,
        AUTH_USER_ID,
        "Activate the MCP server and run its search tool.",
    ))
    .await;

    let auth_status = wait_for_oauth_message(&rig).await;
    assert_eq!(server.call_count(), 0);
    let (state, redirect_uri) = oauth_callback_params(&auth_status);
    assert_eq!(server.discovery_count(), 1);
    assert_eq!(server.registration_count(), 1);
    assert_eq!(server.token_count(), 0);
    assert_eq!(server.registered_redirect_uris(), vec![redirect_uri]);
    let secrets = pending_oauth_secret_store(&rig).await;
    let engine_thread_id = wait_for_original_engine_thread(AUTH_USER_ID).await;
    assert_eq!(engine_threads_for_user(AUTH_USER_ID).await.len(), 1);
    let pending_gate = lunarwing::bridge::get_engine_pending_gate(
        AUTH_USER_ID,
        Some(&engine_thread_id),
    )
    .await
    .expect("pending gate lookup should work")
    .expect("OAuth should pause an Engine V2 gate");
    assert!(matches!(
        pending_gate.resume_kind,
        lunarwing_engine::ResumeKind::Authentication {
            ref credential_name,
            ..
        } if credential_name == SERVER_NAME
    ));

    invoke_oauth_callback(
        rig.extension_manager()
            .expect("extension manager should be present"),
        &state,
    )
    .await;
    assert_encrypted_oauth_token_stored(&secrets, &token_name).await;
    assert_eq!(server.token_count(), 1);

    let detail = wait_for_engine_thread_done(&engine_thread_id, AUTH_USER_ID).await;
    assert_eq!(detail.info.id, engine_thread_id);
    assert_eq!(engine_threads_for_user(AUTH_USER_ID).await.len(), 1);
    assert_eq!(server.call_count(), 1);
    assert_eq!(
        server.calls(),
        vec![(
            TOOL_NAME.to_string(),
            serde_json::json!({"query": "after-auth"})
        )]
    );
    assert_eq!(
        server.last_authorization(),
        Some(format!("Bearer {MOCK_MCP_TOKEN}"))
    );
    assert_eq!(
        provider
            .last_mcp_result
            .lock()
            .expect("MCP result mutex")
            .as_deref(),
        Some("{\"result\":\"authenticated-search-result\"}")
    );
    assert_engine_history_has_single_terminal(&engine_thread_id, AUTH_USER_ID).await;

    assert!(rig.captured_responses().is_empty());
    let conversation = rig
        .database()
        .get_or_create_scoped_conversation(
            "gateway",
            AUTH_USER_ID,
            &v1_thread_id.to_string(),
        )
        .await
        .expect("conversation should resolve");
    let history = rig
        .database()
        .list_conversation_messages(conversation)
        .await
        .expect("history should load");
    assert_eq!(history.iter().filter(|message| message.role == "user").count(), 1);
    assert_eq!(
        history
            .iter()
            .filter(|message| message.role == "assistant")
            .count(),
        0
    );
    assert!(
        history
            .iter()
            .all(|message| !message.content.contains(MOCK_MCP_TOKEN))
    );
    assert!(!server.saw_token_in_non_authorization_field());
    assert_engine_threads_do_not_contain_token(AUTH_USER_ID).await;

    rig.shutdown_and_wait().await;
    env.cleanup().await;
    drop(oauth_env);
    server.shutdown().await;
}
