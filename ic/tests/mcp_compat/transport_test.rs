#[tokio::test]
async fn engine_v2_mcp_transport_executes_real_mock_tool_once() {
    let _lock = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("gateway"));
    lunarwing::bridge::reset_engine_state().await;

    let server = start_mock_mcp_server(vec![mock_tool_response("mock-result")]).await;
    server.require_token(false);
    let sessions = Arc::new(McpSessionManager::new());
    let client = McpClient::new_with_config(McpServerConfig::new(SERVER_NAME, server.mcp_url()))
        .expect("mock MCP config should be valid")
        .with_session_manager(sessions);
    let mcp_tools = client
        .create_tools()
        .await
        .expect("mock MCP list_tools should succeed");
    assert_eq!(mcp_tools.len(), 1);
    assert_eq!(mcp_tools[0].name(), FULL_TOOL_NAME);
    assert_eq!(server.initialize_count(), 1);
    assert_eq!(server.tools_list_count(), 1);

    let provider = Arc::new(McpTransportLlm::new());
    let llm: Arc<dyn LlmProvider> = provider.clone();
    let rig = TestRigBuilder::new()
        .with_channel_name("gateway")
        .with_llm(llm)
        .with_extra_tools(mcp_tools)
        .build()
        .await;
    let thread_id = Uuid::new_v4();

    rig.send_incoming(gateway_message(
        thread_id,
        "user-transport",
        "Use the MCP mock search tool and report the result.",
    ))
    .await;
    let responses = rig.wait_for_responses(1, Duration::from_secs(15)).await;

    assert_eq!(server.call_count(), 1);
    assert_eq!(
        server.calls(),
        vec![(TOOL_NAME.to_string(), serde_json::json!({"query": "test"}))]
    );
    assert!(provider.saw_advertised_tool.load(Ordering::SeqCst));
    assert!(provider.saw_expected_call_id.load(Ordering::SeqCst));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].content, TERMINAL_RESPONSE);

    let conversation = rig
        .database()
        .get_or_create_scoped_conversation(
            "gateway",
            "user-transport",
            &thread_id.to_string(),
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
        1
    );
    assert!(history.iter().any(|message| {
        message.role == "assistant" && message.content == TERMINAL_RESPONSE
    }));

    let threads = engine_threads_for_user("user-transport").await;
    assert_eq!(threads.len(), 1);
    let detail = wait_for_engine_thread_done(&threads[0].id, "user-transport").await;
    assert!(detail.messages.iter().any(|message| {
        message.get("role").and_then(serde_json::Value::as_str) == Some("Assistant")
            && message
                .get("content")
                .and_then(serde_json::Value::as_str)
                == Some(TERMINAL_RESPONSE)
    }));

    assert!(!server.saw_token_in_non_authorization_field());
    assert!(
        responses
            .iter()
            .all(|message| !message.content.contains(MOCK_MCP_TOKEN))
    );
    assert!(
        history
            .iter()
            .all(|message| !message.content.contains(MOCK_MCP_TOKEN))
    );
    assert_engine_threads_do_not_contain_token("user-transport").await;

    rig.shutdown_and_wait().await;
    env.cleanup().await;
    server.shutdown().await;
}
