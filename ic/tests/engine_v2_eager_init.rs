//! Regression test for work item #69: Engine V2 must initialize eagerly at
//! agent startup, not lazily on the first engine-routed message.
//!
//! Before the fix, a daemon restart left cron/webhook/event-driven missions
//! dead until a chat message happened to arrive, and the public mission
//! webhook endpoint returned 500 "engine v2 is not running". The eager
//! initialization in `Agent::run()` (see `ic/src/agent/agent_loop.rs`) must
//! bring the engine up without any inbound message.

mod support;

use std::time::{Duration, Instant};

use support::engine_v2_env::{ENGINE_V2_ENV_LOCK, EngineV2EnvGuard};
use support::test_rig::TestRigBuilder;

#[tokio::test]
async fn engine_v2_initializes_without_any_message() {
    let _lock = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(None);
    let rig = TestRigBuilder::new().build().await;

    // No message is ever sent. `fire_engine_webhook_mission` errors while the
    // engine state is uninitialized and returns `Ok(0)` once the engine is up
    // (no mission matches the probe path), so it doubles as a readiness probe
    // for the exact endpoint that 500s on a lazy-only boot.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match lunarwing::bridge::router::fire_engine_webhook_mission(
            "wi69-eager-init-probe",
            None,
            serde_json::json!({}),
        )
        .await
        {
            Ok(0) => break,
            Ok(matched) => panic!("no mission should match the probe path, got {matched}"),
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("engine did not initialize eagerly at startup: {error}"),
        }
    }

    rig.shutdown_and_wait().await;
    env.cleanup().await;
}
