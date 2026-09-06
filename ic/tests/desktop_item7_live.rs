#![cfg(feature = "libsql")]

mod support;

use std::path::PathBuf;
use std::time::Duration;

use lunarwing::channels::StatusUpdate;
use lunarwing::channels::web::types::SseEvent;
use support::gateway_workflow_harness::GatewayWorkflowHarness;
use support::mock_openai_server::{MockOpenAiResponse, MockOpenAiServerBuilder};
use support::test_channel::CapturedDelivery;

async fn current_thread(harness: &GatewayWorkflowHarness) -> Option<String> {
    let threads = harness
        .client
        .get(format!("{}/api/chat/threads", harness.base_url()))
        .bearer_auth(&harness.auth_token)
        .send()
        .await
        .ok()?
        .json::<serde_json::Value>()
        .await
        .ok()?;
    threads["active_thread"]
        .as_str()
        .or_else(|| threads["threads"].as_array()?.first()?.get("id")?.as_str())
        .or_else(|| threads["assistant_thread"].get("id")?.as_str())
        .map(str::to_string)
}

fn streamed_status(status: StatusUpdate, thread_id: &str) -> Option<SseEvent> {
    match status {
        StatusUpdate::Thinking(message) => Some(SseEvent::Thinking {
            message,
            thread_id: Some(thread_id.to_string()),
        }),
        StatusUpdate::StreamChunk(content) => Some(SseEvent::StreamChunk {
            content,
            thread_id: Some(thread_id.to_string()),
        }),
        StatusUpdate::Status(message) => Some(SseEvent::Status {
            message,
            thread_id: Some(thread_id.to_string()),
        }),
        _ => None,
    }
}

async fn forward_events(
    harness: &GatewayWorkflowHarness,
    delivered: &mut usize,
    event_types: &mut Vec<String>,
) {
    let deliveries = harness.test_channel.captured_deliveries();
    if *delivered >= deliveries.len() {
        return;
    }
    let active = current_thread(harness).await;
    for delivery in deliveries.into_iter().skip(*delivered) {
        let (event_type, event) = match delivery {
            CapturedDelivery::Status(status) => {
                let Some(thread_id) = active.as_deref() else {
                    continue;
                };
                let Some(event) = streamed_status(status, thread_id) else {
                    continue;
                };
                ("status", event)
            }
            CapturedDelivery::Response { message, response } => {
                let Some(thread_id) = message.thread_id else {
                    continue;
                };
                (
                    "response",
                    SseEvent::Response {
                        content: response.content,
                        thread_id,
                        emotes: Vec::new(),
                    },
                )
            }
        };
        harness
            .sse_manager()
            .broadcast_for_user(&harness.user_id, event);
        event_types.push(event_type.to_string());
    }
    *delivered = harness.test_channel.captured_deliveries().len();
}

#[tokio::test]
#[ignore = "manual desktop integration harness; requires DESKTOP_ITEM7_CONTROL"]
async fn serve_desktop_item7() {
    let control = PathBuf::from(
        std::env::var("DESKTOP_ITEM7_CONTROL")
            .expect("DESKTOP_ITEM7_CONTROL must name a temporary control directory"),
    );
    std::fs::create_dir_all(&control).expect("create control directory");
    let ready = control.join("ready.json");
    let stop = control.join("stop");
    let result = control.join("result.json");
    let _ = std::fs::remove_file(&ready);
    let _ = std::fs::remove_file(&stop);
    let _ = std::fs::remove_file(&result);

    let mock = MockOpenAiServerBuilder::new()
        .with_default_response(MockOpenAiResponse::Text(
            "Installed LunarWing desktop conversation verified.".to_string(),
        ))
        .start()
        .await;
    let harness =
        GatewayWorkflowHarness::start_openai_compatible(&mock.openai_base_url(), "mock-model")
            .await;

    let ready_body = serde_json::json!({
        "base_url": harness.base_url(),
        "token": harness.auth_token,
        "backend_commit": "90fdceaf279cf82cb5fb6719c54d29df9109a274"
    });
    std::fs::write(
        &ready,
        serde_json::to_vec(&ready_body).expect("serialize ready record"),
    )
    .expect("write ready record");

    let mut delivered = 0;
    let mut event_types = Vec::new();
    while !stop.exists() {
        forward_events(&harness, &mut delivered, &mut event_types).await;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    forward_events(&harness, &mut delivered, &mut event_types).await;

    let thread_id = current_thread(&harness).await;
    let history = if let Some(thread_id) = thread_id.as_deref() {
        harness.history(thread_id).await
    } else {
        serde_json::json!({"turns": []})
    };
    std::fs::write(
        &result,
        serde_json::to_vec(&serde_json::json!({
            "thread_id": thread_id,
            "history": history,
            "mock_requests": mock.requests().await.len(),
            "forwarded_events": event_types
        }))
        .expect("serialize result"),
    )
    .expect("write result");

    harness.shutdown().await;
    mock.shutdown().await;
}
