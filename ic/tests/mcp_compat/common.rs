const SERVER_NAME: &str = "mock-mcp";
const TOOL_NAME: &str = "mock_search";
const FULL_TOOL_NAME: &str = "mock-mcp_mock_search";
const ENGINE_ACTION_NAME: &str = "mock_mcp_mock_search";
const CALL_ID: &str = "callmcp1";
const TERMINAL_RESPONSE: &str = "mcp-transport-ok";
const ACTIVATE_CALL_ID: &str = "activate1";
const AUTH_MCP_CALL_ID: &str = "mcpcall02";
const AUTH_TERMINAL_RESPONSE: &str = "mcp-auth-resume-ok";
const AUTH_USER_ID: &str = "default";

struct OAuthEnvGuard {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl OAuthEnvGuard {
    fn enable_gateway_callback() -> Self {
        let keys = [
            "LUNARWING_OAUTH_CALLBACK_URL",
            "LUNARWING_OAUTH_EXCHANGE_URL",
        ];
        let saved = keys
            .iter()
            .map(|key| (*key, std::env::var_os(key)))
            .collect();

        // SAFETY: ENGINE_V2_ENV_LOCK serializes every test in this integration
        // binary, and Drop restores both variables before releasing that lock.
        unsafe {
            std::env::set_var(
                "LUNARWING_OAUTH_CALLBACK_URL",
                "https://integration.lunarwing.test/oauth/callback",
            );
            std::env::remove_var("LUNARWING_OAUTH_EXCHANGE_URL");
        }

        Self { saved }
    }
}

impl Drop for OAuthEnvGuard {
    fn drop(&mut self) {
        // SAFETY: same serialized ownership contract as enable_gateway_callback.
        unsafe {
            for (key, value) in self.saved.drain(..) {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

fn mock_tool_response(result: &str) -> MockToolResponse {
    MockToolResponse {
        name: TOOL_NAME.to_string(),
        content: serde_json::json!({"result": result}),
    }
}

fn gateway_message(thread_id: Uuid, user_id: &str, content: &str) -> IncomingMessage {
    IncomingMessage::new("gateway", user_id, content)
        .with_thread(thread_id.to_string())
        .with_metadata(serde_json::json!({
            "thread_id": thread_id,
            "user_id": user_id,
        }))
}

async fn assert_engine_threads_do_not_contain_token(user_id: &str) {
    for thread in engine_threads_for_user(user_id).await {
        let detail = lunarwing::bridge::get_engine_thread(&thread.id, user_id)
            .await
            .expect("engine thread lookup should work")
            .expect("engine thread should exist");
        assert!(detail.messages.iter().all(|message| {
            message
                .get("content")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|content| !content.contains(MOCK_MCP_TOKEN))
        }));
    }
}

async fn engine_threads_for_user(user_id: &str) -> Vec<lunarwing::bridge::EngineThreadInfo> {
    let projects = lunarwing::bridge::list_engine_projects(user_id)
        .await
        .expect("engine project lookup should work");
    let mut all_threads = Vec::new();
    for project in projects {
        let threads = lunarwing::bridge::list_engine_threads(Some(&project.id), user_id)
            .await
            .expect("engine thread lookup should work");
        all_threads.extend(threads);
    }
    all_threads
}

async fn wait_for_engine_thread_done(
    thread_id: &str,
    user_id: &str,
) -> lunarwing::bridge::EngineThreadDetail {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(detail) = lunarwing::bridge::get_engine_thread(thread_id, user_id)
                .await
                .expect("engine thread lookup should work")
                && detail.info.state == "Done"
            {
                return detail;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Engine V2 thread should finish")
}
