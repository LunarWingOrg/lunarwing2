async fn wait_for_oauth_message(rig: &support::test_rig::TestRig) -> StatusUpdate {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let auth_statuses = rig
                .captured_status_events()
                .into_iter()
                .filter(|status| matches!(status, StatusUpdate::AuthRequired { .. }))
                .collect::<Vec<_>>();
            if auth_statuses.len() == 1 {
                return auth_statuses.into_iter().next().expect("one auth status");
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("exactly one OAuth status should be emitted")
}

fn oauth_callback_params(auth_status: &StatusUpdate) -> (String, String) {
    let StatusUpdate::AuthRequired {
        auth_url: Some(auth_url),
        ..
    } = auth_status
    else {
        panic!("auth status should contain an authorization URL")
    };

    let auth_url = url::Url::parse(auth_url).expect("authorization URL should parse");
    let state = auth_url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .expect("authorization URL should contain state");
    let redirect_uri = auth_url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .map(|(_, value)| value.into_owned())
        .expect("authorization URL should contain redirect_uri");
    (state, redirect_uri)
}

async fn invoke_oauth_callback(
    extension_manager: &Arc<lunarwing::extensions::ExtensionManager>,
    state: &str,
) {
    let response = lunarwing::channels::web::server::oauth_callback_for_test(
        extension_manager,
        std::collections::HashMap::from([
            ("code".to_string(), "auth-code".to_string()),
            ("state".to_string(), state.to_string()),
        ]),
    )
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
}

async fn pending_oauth_secret_store(
    rig: &support::test_rig::TestRig,
) -> Arc<dyn SecretsStore + Send + Sync> {
    let extension_manager = rig
        .extension_manager()
        .expect("extension manager should be present");
    let flows = extension_manager.pending_oauth_flows().read().await;
    assert_eq!(flows.len(), 1);
    Arc::clone(&flows.values().next().expect("pending OAuth flow").secrets)
}

async fn assert_encrypted_oauth_token_stored(
    secrets: &Arc<dyn SecretsStore + Send + Sync>,
    token_name: &str,
) {
    let oauth_token = secrets
        .get_decrypted(AUTH_USER_ID, token_name)
        .await
        .expect("encrypted OAuth token should be stored after callback");
    assert_eq!(oauth_token.expose(), MOCK_MCP_TOKEN);
    let stored_secret = secrets
        .get(AUTH_USER_ID, token_name)
        .await
        .expect("OAuth token metadata should be persisted");
    assert_ne!(stored_secret.encrypted_value, MOCK_MCP_TOKEN.as_bytes());
}

async fn wait_for_original_engine_thread(user_id: &str) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let threads = engine_threads_for_user(user_id).await;
            if let Some(thread) = threads.first() {
                return thread.id.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("original Engine V2 thread should be discoverable")
}

async fn assert_engine_history_has_single_terminal(thread_id: &str, user_id: &str) {
    let detail = wait_for_engine_thread_done(thread_id, user_id).await;
    assert_eq!(detail.info.id, thread_id);
    assert_eq!(
        detail
            .messages
            .iter()
            .filter(|message| {
                message.get("role").and_then(serde_json::Value::as_str)
                    == Some("ActionResult")
                    && message
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|content| content.contains("awaiting_authorization"))
            })
            .count(),
        1
    );
    assert_eq!(
        detail
            .messages
            .iter()
            .filter(|message| {
                message.get("role").and_then(serde_json::Value::as_str) == Some("Assistant")
                    && message
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        == Some(AUTH_TERMINAL_RESPONSE)
            })
            .count(),
        1
    );
}
