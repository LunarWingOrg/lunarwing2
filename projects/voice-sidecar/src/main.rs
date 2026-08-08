use std::sync::Arc;

use lunarwing_voice_sidecar::{AppState, Config, SherpaVoiceEngine, api_router, health_router};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lunarwing_voice_sidecar=info".into()),
        )
        .init();

    let config = Arc::new(Config::from_env()?);
    let engine = Arc::new(SherpaVoiceEngine::load(&config)?);
    let state = AppState::new(Arc::clone(&config), engine);
    let api_address = (config.bind_host, config.port);
    let health_address = (config.bind_host, config.health_port);
    let api_listener = TcpListener::bind(api_address).await?;
    let health_listener = TcpListener::bind(health_address).await?;

    tracing::info!(address = ?api_address, "LunarVoice API listening");
    tracing::info!(address = ?health_address, "LunarVoice health listener ready");

    tokio::try_join!(
        axum::serve(api_listener, api_router(state.clone())),
        axum::serve(health_listener, health_router(state)),
    )?;
    Ok(())
}
