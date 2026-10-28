mod bridge;
mod executor;
mod health;
mod mcp_shim;
mod protocol;
mod task_runner;
mod tool_proxy;
mod workspace_path;

use std::net::SocketAddr;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    // WI#324: `pebble4lunarwing mcp-shim` is the stdio MCP server the pebble
    // CLI spawns per task via the baked mcpServers.lunarwing registration.
    if std::env::args().nth(1).as_deref() == Some("mcp-shim") {
        mcp_shim::run().await;
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("task-runner") {
        let socket = std::env::args()
            .nth(2)
            .or_else(|| std::env::var("LUNARWING_TASK_RUNNER_SOCKET").ok())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                eprintln!("[pebble4lunarwing] task-runner requires a socket path");
                std::process::exit(1);
            });
        let env_or =
            |key: &str, default: &str| std::env::var(key).unwrap_or_else(|_| default.to_string());
        let config = executor::ExecutorConfig {
            pebble_bin: env_or("PEBBLE_BIN", "pebble"),
            model: env_or("PEBBLE_MODEL", "openai/gpt-5.2"),
            permission_mode: env_or("PEBBLE_PERMISSION_MODE", "danger-full-access"),
            workspace_root: env_or("WORKSPACE_ROOT", "/workspace"),
        };
        if let Err(error) = task_runner::run(config, socket.into()).await {
            eprintln!("[pebble4lunarwing] task runner error: {error}");
            std::process::exit(1);
        }
        return;
    }

    let config = match bridge::BridgeConfig::from_env() {
        Ok(config) => config,
        Err(e) => {
            // KW-8/LD-10: a broken AGENT_AUTH_TOKEN_FILE must stop startup —
            // never come up with no bearer check.
            eprintln!("[pebble4lunarwing] refusing to start: {e}");
            std::process::exit(1);
        }
    };
    eprintln!(
        "[pebble4lunarwing] worker_id={} version={}",
        config.worker_id,
        protocol::WORKER_VERSION
    );
    eprintln!(
        "[pebble4lunarwing] ws={}:{}{} health=:{} auth={}",
        config.bind_host,
        config.ws_port,
        config.ws_path,
        config.health_port,
        if config.auth_token.is_some() {
            "enabled"
        } else {
            "disabled"
        }
    );
    eprintln!(
        "[pebble4lunarwing] pebble_bin={} model={} permission_mode={}",
        config.pebble_bin, config.model, config.permission_mode
    );

    let state = Arc::new(health::WorkerState::new(&config.worker_id));

    let health_addr: SocketAddr = ([0, 0, 0, 0], config.health_port).into();
    let health_state = Arc::clone(&state);
    tokio::spawn(async move {
        if let Err(e) = health::serve(health_addr, health_state).await {
            eprintln!("[pebble4lunarwing] health server error: {e}");
        }
    });

    if let Err(e) = bridge::run(config, state).await {
        eprintln!("[pebble4lunarwing] bridge error: {e}");
        std::process::exit(1);
    }
}
