//! `lunarwing doctor` - active health diagnostics.
//!
//! Probes external dependencies and validates configuration to surface
//! problems before they bite during normal operation. Each check reports
//! pass/fail with actionable guidance on failures.

use std::path::PathBuf;

use crate::bootstrap::lunarwing_base_dir;
use crate::cli::fmt;
use crate::settings::Settings;

/// Run all diagnostic checks and print results.
pub async fn run_doctor_command() -> anyhow::Result<()> {
    println!();
    println!("  {}LunarWing Doctor{}", fmt::bold(), fmt::reset());

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;

    // Load settings once for checks that need them.
    let settings = Settings::load();

    // ── Core ─────────────────────────────────────────────────

    section_header("Core");

    check(
        "Settings file",
        check_settings_file(),
        &mut passed,
        &mut failed,
        &mut skipped,
    );

    check(
        "LunarWing Cloud session",
        check_lunarwing_cloud_session(&settings).await,
        &mut passed,
        &mut failed,
        &mut skipped,
    );

    check(
        "LLM configuration",
        check_llm_config(&settings),
        &mut passed,
        &mut failed,
        &mut skipped,
    );

    check(
        "Database backend",
        check_database().await,
        &mut passed,
        &mut failed,
        &mut skipped,
    );

    check(
        "Workspace directory",
        check_workspace_dir(),
        &mut passed,
        &mut failed,
        &mut skipped,
    );

    // ── Features ─────────────────────────────────────────────

    section_header("Features");

    check(
        "Embeddings",
        check_embeddings(&settings),
        &mut passed,
        &mut failed,
        &mut skipped,
    );

    check(
        "Routines config",
        check_routines_config(),
        &mut passed,
        &mut failed,
        &mut skipped,
    );

    check(
        "Gateway config",
        check_gateway_config(&settings),
        &mut passed,
        &mut failed,
        &mut skipped,
    );

    check(
        "MCP servers",
        check_mcp_config(&settings).await,
        &mut passed,
        &mut failed,
        &mut skipped,
    );

    check(
        "Skills",
        check_skills().await,
        &mut passed,
        &mut failed,
        &mut skipped,
    );

    check(
        "Secrets",
        check_secrets(&settings),
        &mut passed,
        &mut failed,
        &mut skipped,
    );

    check(
        "Service",
        check_service_installed(),
        &mut passed,
        &mut failed,
        &mut skipped,
    );

    // ── External ─────────────────────────────────────────────

    section_header("External");

    check(
        "Docker daemon",
        check_docker_daemon().await,
        &mut passed,
        &mut failed,
        &mut skipped,
    );

    let (tunnel_name, tunnel_result) = check_tunnel_provider(&settings).await;
    check(
        tunnel_name,
        tunnel_result,
        &mut passed,
        &mut failed,
        &mut skipped,
    );

    // ── Summary ───────────────────────────────────────────────

    println!();
    println!(
        "  {}{} passed{}, {}{} failed{}, {}{} skipped{}",
        fmt::success(),
        passed,
        fmt::reset(),
        if failed > 0 { fmt::error() } else { fmt::dim() },
        failed,
        fmt::reset(),
        fmt::dim(),
        skipped,
        fmt::reset(),
    );

    if failed > 0 {
        println!("\n  Some checks failed. This is normal if you don't use those features.");
    }

    Ok(())
}

/// Print a section header with a separator and bold group name.
fn section_header(name: &str) {
    println!();
    println!("  {}", fmt::separator(36));
    println!("  {}{}{}", fmt::bold(), name, fmt::reset());
    println!();
}

// ── Individual checks ───────────────────────────────────────

fn check(name: &str, result: CheckResult, passed: &mut u32, failed: &mut u32, skipped: &mut u32) {
    match result {
        CheckResult::Pass(detail) => {
            *passed += 1;
            println!(
                "{}",
                fmt::check_line(fmt::StatusKind::Pass, name, &detail, 18)
            );
        }
        CheckResult::Fail(detail) => {
            *failed += 1;
            println!(
                "{}",
                fmt::check_line(fmt::StatusKind::Fail, name, &detail, 18)
            );
        }
        CheckResult::Skip(reason) => {
            *skipped += 1;
            println!(
                "{}",
                fmt::check_line(fmt::StatusKind::Skip, name, &reason, 18)
            );
        }
    }
}

enum CheckResult {
    Pass(String),
    Fail(String),
    Skip(String),
}

// ── Settings file ───────────────────────────────────────────

fn check_settings_file() -> CheckResult {
    let path = Settings::default_path();
    if !path.exists() {
        return CheckResult::Pass("no settings file (defaults will be used)".into());
    }

    match std::fs::read_to_string(&path) {
        Ok(data) => match serde_json::from_str::<serde_json::Value>(&data) {
            Ok(_) => CheckResult::Pass(format!("valid ({})", path.display())),
            Err(e) => CheckResult::Fail(format!(
                "settings.json is malformed: {}. Fix or delete {}",
                e,
                path.display()
            )),
        },
        Err(e) => CheckResult::Fail(format!("cannot read {}: {}", path.display(), e)),
    }
}

// ── LunarWing Cloud session ─────────────────────────────────────────

async fn check_lunarwing_cloud_session(settings: &Settings) -> CheckResult {
    // Skip entirely when the configured backend is not LunarWing Cloud.
    let llm_config = match crate::config::LlmConfig::resolve(settings) {
        Ok(config) => config,
        Err(e) => {
            // check_llm_config will report the full error; just skip here.
            return CheckResult::Skip(format!("LLM config error: {e}"));
        }
    };
    if llm_config.backend != "lunarwing_cloud" {
        return CheckResult::Skip(format!(
            "not using LunarWing Cloud backend (backend={})",
            llm_config.backend
        ));
    }

    // Check if session file exists
    let session_path = crate::config::llm::default_session_path();
    if !session_path.exists() {
        // Check for API key mode
        if crate::config::helpers::env_or_override("LUNARWING_CLOUD_API_KEY").is_some() {
            return CheckResult::Pass("API key configured".into());
        }
        return CheckResult::Fail(format!(
            "session file not found at {}. Run `lunarwing onboard`",
            session_path.display()
        ));
    }

    // Verify the session file is readable and non-empty
    match std::fs::read_to_string(&session_path) {
        Ok(content) if content.trim().is_empty() => {
            CheckResult::Fail("session file is empty".into())
        }
        Ok(_) => CheckResult::Pass(format!("session found ({})", session_path.display())),
        Err(e) => CheckResult::Fail(format!("cannot read session file: {e}")),
    }
}

// ── LLM configuration ──────────────────────────────────────

fn check_llm_config(settings: &Settings) -> CheckResult {
    match crate::llm::LlmConfig::resolve(settings) {
        Ok(config) => {
            // Show the model for the active backend, not always lunarwing_cloud.model.
            let model = if let Some(ref provider) = config.provider {
                &provider.model
            } else {
                &config.lunarwing_cloud.model
            };
            CheckResult::Pass(format!("backend={}, model={}", config.backend, model))
        }
        Err(e) => CheckResult::Fail(format!("LLM config error: {e}")),
    }
}

// ── Database ────────────────────────────────────────────────

async fn check_database() -> CheckResult {
    let backend = std::env::var("DATABASE_BACKEND")
        .ok()
        .unwrap_or_else(|| "postgres".into());

    match backend.as_str() {
        "libsql" | "turso" | "sqlite" => {
            let path = std::env::var("LIBSQL_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| crate::config::default_libsql_path());

            if path.exists() {
                CheckResult::Pass(format!("libSQL database exists ({})", path.display()))
            } else {
                CheckResult::Pass(format!(
                    "libSQL database not found at {} (will be created on first run)",
                    path.display()
                ))
            }
        }
        _ => {
            if std::env::var("DATABASE_URL").is_ok() {
                // Try to connect
                match try_pg_connect().await {
                    Ok(()) => CheckResult::Pass("PostgreSQL connected".into()),
                    Err(e) => CheckResult::Fail(format!("PostgreSQL connection failed: {e}")),
                }
            } else {
                CheckResult::Fail("DATABASE_URL not set".into())
            }
        }
    }
}

#[cfg(feature = "postgres")]
async fn try_pg_connect() -> Result<(), String> {
    let url = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL not set".to_string())?;

    let config = deadpool_postgres::Config {
        url: Some(url),
        ..Default::default()
    };
    let pool = crate::db::tls::create_pool(&config, crate::config::SslMode::from_env())
        .map_err(|e| format!("pool error: {e}"))?;

    let client = tokio::time::timeout(std::time::Duration::from_secs(5), pool.get())
        .await
        .map_err(|_| "connection timeout (5s)".to_string())?
        .map_err(|e| format!("{e}"))?;

    client
        .execute("SELECT 1", &[])
        .await
        .map_err(|e| format!("{e}"))?;

    Ok(())
}

#[cfg(not(feature = "postgres"))]
async fn try_pg_connect() -> Result<(), String> {
    Err("postgres feature not compiled in".into())
}

// ── Workspace directory ─────────────────────────────────────

fn check_workspace_dir() -> CheckResult {
    let dir = lunarwing_base_dir();

    if dir.exists() {
        if dir.is_dir() {
            CheckResult::Pass(format!("{}", dir.display()))
        } else {
            CheckResult::Fail(format!("{} exists but is not a directory", dir.display()))
        }
    } else {
        CheckResult::Pass(format!("{} will be created on first run", dir.display()))
    }
}

// ── Embeddings ──────────────────────────────────────────────

fn check_embeddings(settings: &Settings) -> CheckResult {
    match crate::config::EmbeddingsConfig::resolve(settings) {
        Ok(config) => {
            if !config.enabled {
                return CheckResult::Skip("disabled (set EMBEDDING_ENABLED=true)".into());
            }
            let has_creds = match config.provider.as_str() {
                "openai" => config.openai_api_key().is_some(),
                "lunarwing_cloud" => {
                    // LunarWingCloudEmbeddings uses SessionManager::get_token() which
                    // only returns session tokens, NOT LUNARWING_CLOUD_API_KEY
                    // (src/workspace/embeddings.rs:309, src/llm/session.rs:132).
                    let session_path = crate::config::llm::default_session_path();
                    session_path.exists()
                        && std::fs::read_to_string(&session_path)
                            .map(|s| !s.trim().is_empty())
                            .unwrap_or(false)
                }
                "ollama" => true, // local, no creds needed
                _ => config.openai_api_key().is_some(),
            };
            if has_creds {
                CheckResult::Pass(format!(
                    "provider={}, model={}",
                    config.provider, config.model
                ))
            } else {
                let hint = match config.provider.as_str() {
                    "lunarwing_cloud" => "run `lunarwing onboard` to create a session",
                    _ => "set OPENAI_API_KEY",
                };
                CheckResult::Fail(format!(
                    "provider={} but credentials missing ({})",
                    config.provider, hint
                ))
            }
        }
        Err(e) => CheckResult::Fail(format!("config error: {e}")),
    }
}

// ── Routines config ─────────────────────────────────────────

fn check_routines_config() -> CheckResult {
    match crate::config::RoutineConfig::resolve() {
        Ok(config) => {
            if config.enabled {
                CheckResult::Pass(format!(
                    "enabled (interval={}s, max_concurrent={})",
                    config.cron_check_interval_secs, config.max_concurrent_routines
                ))
            } else {
                CheckResult::Skip("disabled".into())
            }
        }
        Err(e) => CheckResult::Fail(format!("config error: {e}")),
    }
}

// ── Gateway config ──────────────────────────────────────────

fn check_gateway_config(settings: &Settings) -> CheckResult {
    // Use the same resolve() path as runtime so invalid env values
    // (e.g. GATEWAY_PORT=abc) are caught here too.
    let owner_id = match crate::config::resolve_owner_id(settings) {
        Ok(owner_id) => owner_id,
        Err(e) => return CheckResult::Fail(format!("config error: {e}")),
    };
    match crate::config::ChannelsConfig::resolve(settings, &owner_id) {
        Ok(channels) => match channels.gateway {
            Some(gw) => {
                if gw.auth_token.is_some() {
                    CheckResult::Pass(format!(
                        "enabled at {}:{} (auth token set)",
                        gw.host, gw.port
                    ))
                } else {
                    CheckResult::Pass(format!(
                        "enabled at {}:{} (no auth token — random token will be generated)",
                        gw.host, gw.port
                    ))
                }
            }
            None => CheckResult::Skip("disabled (GATEWAY_ENABLED=false)".into()),
        },
        Err(e) => CheckResult::Fail(format!("config error: {e}")),
    }
}

// ── MCP servers ─────────────────────────────────────────────

async fn check_mcp_config(settings: &Settings) -> CheckResult {
    let file = match crate::tools::mcp::config::load_mcp_servers().await {
        Ok(f) => f,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") || msg.contains("No such file") {
                return CheckResult::Skip("no MCP config file".into());
            }
            return CheckResult::Fail(format!("config error: {e}"));
        }
    };

    let servers = &file.servers;
    if servers.is_empty() {
        return CheckResult::Skip("no MCP servers configured".into());
    }

    let mut details: Vec<String> = Vec::with_capacity(servers.len());
    let mut invalid = 0u32;
    let mut missing_enabled = 0u32;

    for server in servers {
        let pre = preflight_mcp_server(server);
        if pre.invalid {
            invalid += 1;
        } else if pre.missing && server.enabled {
            missing_enabled += 1;
        }
        details.push(pre.detail);
    }

    // Cross-check external-worker placements against configured workers so a
    // typo'd worker name surfaces here instead of at worker activation time.
    let external_workers = crate::config::ExternalWorkerConfig::resolve_from_settings(settings);
    let worker_names: std::collections::HashSet<&str> = external_workers
        .iter()
        .map(|worker| worker.name.as_str())
        .collect();
    for server in file.external_worker_servers() {
        let worker = server.external_worker.as_deref().unwrap_or("");
        if !worker_names.contains(worker) {
            invalid += 1;
            details.push(format!(
                "{} — external worker '{worker}' is not configured",
                server.name
            ));
        }
    }

    // Cross-check external transports against the declared runtime registry;
    // activation fails closed on an unknown runtime.
    let external_registry = file.external_runtime_registry().ok();
    for server in servers {
        if let crate::tools::mcp::config::EffectiveTransport::External { runtime, .. } =
            server.effective_transport()
            && external_registry
                .as_ref()
                .is_none_or(|registry| registry.get(runtime).is_none())
        {
            invalid += 1;
            details.push(format!(
                "{} — external runtime '{runtime}' has no descriptor in external_runtimes",
                server.name
            ));
        }
    }

    if invalid > 0 || missing_enabled > 0 {
        CheckResult::Fail(format!(
            "{} server(s); {} invalid, {} enabled missing command: {}",
            servers.len(),
            invalid,
            missing_enabled,
            details.join("; ")
        ))
    } else {
        CheckResult::Pass(format!(
            "{} server(s) configured: {}",
            servers.len(),
            details.join("; ")
        ))
    }
}

struct McpPreflight {
    detail: String,
    invalid: bool,
    missing: bool,
}

fn preflight_mcp_server(server: &crate::tools::mcp::config::McpServerConfig) -> McpPreflight {
    use crate::tools::mcp::config::{EffectiveTransport, McpPlacement};

    let transport = match server.effective_transport() {
        EffectiveTransport::Http => "http",
        EffectiveTransport::Stdio { .. } => "stdio",
        EffectiveTransport::Unix { .. } => "unix",
        EffectiveTransport::Wasm { .. } => "wasm",
        EffectiveTransport::Gateway { .. } => "gateway",
        EffectiveTransport::External { .. } => "external",
    };
    let state = if server.enabled {
        "enabled"
    } else {
        "disabled"
    };
    let placement = match server.placement {
        McpPlacement::Host => "host",
        McpPlacement::JobWorker => "job-worker",
        McpPlacement::ExternalWorker => "external-worker",
    };
    let mut detail = format!("{} [{} {} {}]", server.name, transport, placement, state);
    let mut invalid = false;
    let mut missing = false;

    if let Err(e) = server.validate() {
        invalid = true;
        detail.push_str(&format!(" — invalid: {e}"));
        return McpPreflight {
            detail,
            invalid,
            missing,
        };
    }

    if server.placement == McpPlacement::JobWorker {
        detail.push_str(" — command availability must be checked in worker image");
        return McpPreflight {
            detail,
            invalid,
            missing,
        };
    }

    if let EffectiveTransport::Stdio { command, .. } = server.effective_transport() {
        match resolve_command_in_path(command) {
            Some(path) => detail.push_str(&format!(" — resolves to {}", path.display())),
            None => {
                missing = true;
                detail.push_str(&format!(" — command '{command}' not found in PATH"));
            }
        }
    }
    if let EffectiveTransport::Wasm { component_path, .. } = server.effective_transport() {
        match component_path.canonicalize() {
            Ok(canonical) if canonical == component_path => {
                detail.push_str(&format!(" — component {}", canonical.display()));
            }
            Ok(_) => {
                missing = true;
                detail.push_str(" — component path is not canonical or is a symlink");
            }
            Err(error) => {
                missing = true;
                detail.push_str(&format!(" — component unavailable: {error}"));
            }
        }
    }

    if let Some(sandbox) = &server.sandbox {
        detail.push_str(&format!(" — sandbox: {}", sandbox.describe()));
        #[cfg(target_os = "linux")]
        if sandbox.read_only_root && crate::tools::mcp::sandbox::landlock_abi_version().is_none() {
            missing = true;
            detail.push_str(" (kernel lacks Landlock; activation will fail)");
        }
        #[cfg(not(target_os = "linux"))]
        if sandbox.requires_linux() {
            missing = true;
            detail.push_str(" (requires Linux; activation will fail)");
        }
    }

    McpPreflight {
        detail,
        invalid,
        missing,
    }
}

fn resolve_command_in_path(command: &str) -> Option<PathBuf> {
    if command.contains(std::path::MAIN_SEPARATOR) {
        let path = PathBuf::from(command);
        return if is_executable(&path) {
            Some(path)
        } else {
            None
        };
    }
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join(command);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if !path.exists() {
        return false;
    }
    match std::fs::metadata(path) {
        Ok(md) => md.is_file() && (md.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.exists()
}

// ── Skills ──────────────────────────────────────────────────

async fn check_skills() -> CheckResult {
    let user_dir = lunarwing_base_dir().join("skills");
    let installed_dir = lunarwing_base_dir().join("installed_skills");

    let mut registry = crate::skills::SkillRegistry::new(user_dir.clone());
    registry = registry.with_installed_dir(installed_dir);

    // discover_all() returns loaded skill names (not warnings).
    let _loaded_names = registry.discover_all().await;

    let count = registry.count();
    if count == 0 {
        return CheckResult::Skip("no skills discovered".into());
    }

    CheckResult::Pass(format!("{count} skill(s) loaded"))
}

// ── Secrets ─────────────────────────────────────────────────

fn check_secrets(settings: &Settings) -> CheckResult {
    match settings.secrets_master_key_source {
        crate::settings::KeySource::Keychain => {
            CheckResult::Pass("master key source: OS keychain".into())
        }
        crate::settings::KeySource::Env => {
            if std::env::var("SECRETS_MASTER_KEY").is_ok() {
                CheckResult::Pass("master key source: env var (set)".into())
            } else {
                CheckResult::Fail(
                    "master key source: env var but SECRETS_MASTER_KEY not set".into(),
                )
            }
        }
        crate::settings::KeySource::None => {
            CheckResult::Skip("secrets not configured (run `lunarwing onboard`)".into())
        }
    }
}

// ── Service ─────────────────────────────────────────────────

fn check_service_installed() -> CheckResult {
    match crate::service::service_installation() {
        Ok(installation) if installation.path.exists() => CheckResult::Pass(format!(
            "{} installed ({})",
            installation.manager.install_artifact(),
            installation.path.display()
        )),
        Ok(_) => CheckResult::Skip("not installed (run `lunarwing service install`)".into()),
        Err(err) => CheckResult::Skip(err.to_string()),
    }
}

// ── Container runtime ───────────────────────────────────────

async fn check_docker_daemon() -> CheckResult {
    // The generic sandbox drives the podman CLI directly (no daemon), so a
    // usable podman satisfies sandboxing even without a docker daemon socket.
    if crate::sandbox::check_podman().await.status.is_ok() {
        return CheckResult::Pass("podman CLI available".into());
    }
    let detection = crate::sandbox::check_docker().await;
    match detection.status {
        crate::sandbox::DockerStatus::Available => CheckResult::Pass("running".into()),
        crate::sandbox::DockerStatus::NotInstalled => CheckResult::Skip(format!(
            "not installed. {} (or {})",
            detection.platform.install_hint(),
            crate::sandbox::podman_install_hint()
        )),
        crate::sandbox::DockerStatus::NotRunning => CheckResult::Fail(format!(
            "installed but not running. {}",
            detection.platform.start_hint()
        )),
        crate::sandbox::DockerStatus::Disabled => CheckResult::Skip("sandbox disabled".into()),
    }
}

// ── External binary ─────────────────────────────────────────

async fn check_tunnel_provider(settings: &Settings) -> (&'static str, CheckResult) {
    let static_url = crate::config::helpers::env_or_override("TUNNEL_URL")
        .or_else(|| settings.tunnel.public_url.clone());
    if let Some(url) = static_url.filter(|url| !url.is_empty()) {
        let result = match crate::config::validate_public_url(&url) {
            Ok(()) => CheckResult::Pass("static URL configured".to_string()),
            Err(error) => CheckResult::Fail(error.to_string()),
        };
        return ("Tunnel provider", result);
    }

    let provider = crate::config::helpers::env_or_override("TUNNEL_PROVIDER")
        .or_else(|| settings.tunnel.provider.clone());

    match provider.as_deref() {
        Some("netbird") => {
            let prefix = crate::config::helpers::env_or_override("TUNNEL_NETBIRD_NAME_PREFIX")
                .or_else(|| settings.tunnel.netbird_name_prefix.clone())
                .unwrap_or_else(|| "lunarwing".to_string());
            if let Err(error) = crate::tunnel::validate_netbird_name_prefix(&prefix) {
                return ("netbird", CheckResult::Fail(error.to_string()));
            }
            ("netbird", check_netbird().await)
        }
        Some(provider @ ("cloudflare" | "ngrok" | "tailscale")) => (
            "Tunnel provider",
            CheckResult::Fail(format!(
                "managed provider '{provider}' was removed; configure netbird, custom, or an externally managed HTTPS TUNNEL_URL"
            )),
        ),
        Some("custom") => {
            let command = crate::config::helpers::env_or_override("TUNNEL_CUSTOM_COMMAND")
                .or_else(|| settings.tunnel.custom_command.clone());
            if command.is_some_and(|command| !command.trim().is_empty()) {
                (
                    "Tunnel provider",
                    CheckResult::Pass("custom command configured".into()),
                )
            } else {
                (
                    "Tunnel provider",
                    CheckResult::Fail("custom provider has no command".into()),
                )
            }
        }
        Some("none") | None => (
            "Tunnel provider",
            CheckResult::Skip("not configured".into()),
        ),
        Some(other) => (
            "Tunnel provider",
            CheckResult::Fail(format!(
                "unknown provider: {other}; valid providers: none, netbird, custom"
            )),
        ),
    }
}

async fn check_netbird() -> CheckResult {
    check_netbird_available(crate::skills::gating::binary_exists("netbird")).await
}

async fn check_netbird_available(binary_available: bool) -> CheckResult {
    if !binary_available {
        return CheckResult::Fail("netbird not found in PATH".to_string());
    }

    let version = match run_external_command("netbird", &["version"]).await {
        Ok(version) => version,
        Err(detail) => return CheckResult::Fail(format!("netbird version failed: {detail}")),
    };

    let expose = run_external_command("netbird", &["expose", "--help"]).await;
    if let Err(detail) = expose {
        return CheckResult::Fail(format!(
            "{version}; `netbird expose` unavailable: {detail}. Upgrade NetBird"
        ));
    }

    match run_external_command("netbird", &["status", "--check", "startup"]).await {
        Ok(_) => CheckResult::Pass(format!("{version}; expose available; peer connected")),
        Err(detail) => CheckResult::Fail(format!(
            "{version}; peer not ready: {detail}. Connect it to the management server"
        )),
    }
}

async fn run_external_command(name: &str, args: &[&str]) -> Result<String, String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::process::Command::new(name)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| format!("{name} command timed out after 5s"))?
    .map_err(|error| error.to_string())?;
    let detail = first_command_line(&output);
    if output.status.success() {
        Ok(detail)
    } else {
        Err(detail)
    }
}

fn first_command_line(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    stderr
        .lines()
        .chain(stdout.lines())
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or("no output")
        .to_string()
}

#[cfg(test)]
mod tests {
    use crate::cli::doctor::*;

    struct TunnelEnvGuard(Vec<(&'static str, Option<String>)>);

    impl TunnelEnvGuard {
        fn clear() -> Self {
            let keys = [
                "TUNNEL_URL",
                "TUNNEL_PROVIDER",
                "TUNNEL_CUSTOM_COMMAND",
                "TUNNEL_NETBIRD_NAME_PREFIX",
            ];
            let previous = keys
                .into_iter()
                .map(|key| (key, std::env::var(key).ok()))
                .collect();
            // SAFETY: Callers hold the shared environment lock.
            unsafe {
                for key in keys {
                    std::env::remove_var(key);
                }
            }
            Self(previous)
        }
    }

    impl Drop for TunnelEnvGuard {
        fn drop(&mut self) {
            // SAFETY: Guard is dropped while caller still holds environment lock.
            unsafe {
                for (key, value) in &self.0 {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    fn check_tunnel_without_env(settings: &Settings) -> (&'static str, CheckResult) {
        let _mutex = crate::config::helpers::lock_env();
        let _env = TunnelEnvGuard::clear();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(check_tunnel_provider(settings))
    }

    #[test]
    fn tunnel_provider_skips_when_unconfigured() {
        let settings = Settings::default();
        let (_, result) = check_tunnel_without_env(&settings);
        assert!(matches!(result, CheckResult::Skip(_)));
    }

    #[test]
    fn tunnel_provider_accepts_configured_custom_command() {
        let mut settings = Settings::default();
        settings.tunnel.provider = Some("custom".to_string());
        settings.tunnel.custom_command = Some("example-tunnel {port}".to_string());
        let (_, result) = check_tunnel_without_env(&settings);
        assert!(matches!(result, CheckResult::Pass(_)));
    }

    #[test]
    fn tunnel_provider_prefers_static_url() {
        let mut settings = Settings::default();
        settings.tunnel.public_url = Some("https://static.example.com".to_string());
        settings.tunnel.provider = Some("netbird".to_string());
        let (name, result) = check_tunnel_without_env(&settings);
        assert_eq!(name, "Tunnel provider");
        assert!(matches!(result, CheckResult::Pass(detail) if detail.contains("static URL")));
    }

    #[test]
    fn tunnel_provider_rejects_invalid_static_url() {
        let mut settings = Settings::default();
        settings.tunnel.public_url = Some("http://static.example.com".to_string());
        settings.tunnel.provider = Some("netbird".to_string());
        let (_, result) = check_tunnel_without_env(&settings);
        assert!(matches!(result, CheckResult::Fail(detail) if detail.contains("HTTPS")));
    }

    #[test]
    fn tunnel_provider_reports_removed_managed_providers() {
        for provider in ["cloudflare", "ngrok", "tailscale"] {
            let mut settings = Settings::default();
            settings.tunnel.provider = Some(provider.to_string());
            let (name, result) = check_tunnel_without_env(&settings);
            assert_eq!(name, "Tunnel provider");
            assert!(matches!(result, CheckResult::Fail(detail) if detail.contains("was removed")));
        }
    }

    #[test]
    fn tunnel_provider_rejects_invalid_netbird_prefix_before_binary_probe() {
        let mut settings = Settings::default();
        settings.tunnel.provider = Some("netbird".to_string());
        settings.tunnel.netbird_name_prefix = Some("Invalid Prefix".to_string());
        let (name, result) = check_tunnel_without_env(&settings);
        assert_eq!(name, "netbird");
        assert!(matches!(result, CheckResult::Fail(detail) if detail.contains("name prefix")));
    }

    #[tokio::test]
    async fn configured_netbird_missing_binary_fails() {
        let result = check_netbird_available(false).await;
        assert!(matches!(result, CheckResult::Fail(detail) if detail.contains("not found")));
    }

    #[test]
    fn check_workspace_dir_does_not_panic() {
        let result = check_workspace_dir();
        match result {
            CheckResult::Pass(_) | CheckResult::Fail(_) | CheckResult::Skip(_) => {}
        }
    }

    #[tokio::test]
    async fn check_lunarwing_cloud_session_does_not_panic() {
        let settings = Settings::default();
        let result = check_lunarwing_cloud_session(&settings).await;
        match result {
            CheckResult::Pass(_) | CheckResult::Fail(_) | CheckResult::Skip(_) => {}
        }
    }

    #[test]
    fn check_lunarwing_cloud_session_skips_for_non_lunarwing_cloud_backend() {
        struct EnvGuard(&'static str, Option<String>);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                // SAFETY: Under ENV_MUTEX.
                unsafe {
                    match &self.1 {
                        Some(val) => std::env::set_var(self.0, val),
                        None => std::env::remove_var(self.0),
                    }
                }
            }
        }

        let _mutex = crate::config::helpers::lock_env();
        let prev = std::env::var("LLM_BACKEND").ok();
        // SAFETY: Under ENV_MUTEX, no concurrent env access.
        unsafe {
            std::env::set_var("LLM_BACKEND", "ollama");
        }
        let _env_guard = EnvGuard("LLM_BACKEND", prev);

        let settings = Settings::default();
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let result = rt.block_on(check_lunarwing_cloud_session(&settings));
        match result {
            CheckResult::Skip(msg) => {
                assert!(
                    msg.contains("backend=ollama"),
                    "expected backend name in skip message, got: {msg}"
                );
            }
            other => panic!(
                "expected Skip for non-lunarwing_cloud backend, got: {}",
                format_result(&other)
            ),
        }
    }

    #[test]
    fn check_settings_file_handles_missing() {
        // Settings::default_path() might or might not exist, but must not panic
        let result = check_settings_file();
        match result {
            CheckResult::Pass(_) | CheckResult::Fail(_) | CheckResult::Skip(_) => {}
        }
    }

    #[test]
    fn check_llm_config_does_not_panic() {
        let settings = Settings::default();
        let result = check_llm_config(&settings);
        match result {
            CheckResult::Pass(_) | CheckResult::Fail(_) | CheckResult::Skip(_) => {}
        }
    }

    #[test]
    fn check_routines_config_does_not_panic() {
        let result = check_routines_config();
        match result {
            CheckResult::Pass(_) | CheckResult::Fail(_) | CheckResult::Skip(_) => {}
        }
    }

    #[test]
    fn check_gateway_config_does_not_panic() {
        let settings = Settings::default();
        let result = check_gateway_config(&settings);
        match result {
            CheckResult::Pass(_) | CheckResult::Fail(_) | CheckResult::Skip(_) => {}
        }
    }

    #[test]
    fn check_embeddings_does_not_panic() {
        let settings = Settings::default();
        let result = check_embeddings(&settings);
        match result {
            CheckResult::Pass(_) | CheckResult::Fail(_) | CheckResult::Skip(_) => {}
        }
    }

    #[test]
    fn check_secrets_none_returns_skip() {
        let settings = Settings::default();
        match check_secrets(&settings) {
            CheckResult::Skip(msg) => {
                assert!(
                    msg.contains("not configured"),
                    "expected 'not configured' in skip message, got: {msg}"
                );
            }
            other => panic!(
                "expected Skip for default settings, got: {}",
                format_result(&other)
            ),
        }
    }

    #[test]
    fn check_service_installed_does_not_panic() {
        let result = check_service_installed();
        match result {
            CheckResult::Pass(_) | CheckResult::Fail(_) | CheckResult::Skip(_) => {}
        }
    }

    #[tokio::test]
    async fn check_docker_daemon_does_not_panic() {
        let result = check_docker_daemon().await;
        match result {
            CheckResult::Pass(_) | CheckResult::Fail(_) | CheckResult::Skip(_) => {}
        }
    }

    #[tokio::test]
    async fn check_mcp_config_does_not_panic() {
        let settings = Settings::load();
        let result = check_mcp_config(&settings).await;
        match result {
            CheckResult::Pass(_) | CheckResult::Fail(_) | CheckResult::Skip(_) => {}
        }
    }

    #[tokio::test]
    async fn check_skills_does_not_panic() {
        let result = check_skills().await;
        match result {
            CheckResult::Pass(_) | CheckResult::Fail(_) | CheckResult::Skip(_) => {}
        }
    }

    #[test]
    fn check_llm_config_shows_lunarwing_cloud_model_for_lunarwing_cloud_backend() {
        let _guard = crate::config::helpers::lock_env();
        // SAFETY: Under ENV_MUTEX, no concurrent env access.
        unsafe {
            std::env::remove_var("LLM_BACKEND");
            // Loopback URLs: config-time validation must not depend on
            // external private DNS (http://127.0.0.1 passes via the
            // localhost exemption).
            std::env::set_var("LUNARWING_CLOUD_AUTH_URL", "http://127.0.0.1");
            std::env::set_var("LUNARWING_CLOUD_BASE_URL", "http://127.0.0.1");
        }
        let settings = Settings::default();
        match check_llm_config(&settings) {
            CheckResult::Pass(msg) => {
                assert!(
                    msg.contains("backend=lunarwing_cloud"),
                    "expected lunarwing_cloud backend, got: {msg}"
                );
                // Must NOT show a bedrock or registry model when backend is lunarwing_cloud
                assert!(
                    !msg.contains("anthropic.claude"),
                    "should not show bedrock model for lunarwing_cloud backend: {msg}"
                );
            }
            other => panic!(
                "expected Pass for default LLM config, got: {}",
                format_result(&other)
            ),
        }
    }

    #[test]
    fn check_embeddings_disabled_by_default_returns_skip() {
        let _guard = crate::config::helpers::lock_env();
        // SAFETY: Under ENV_MUTEX.
        unsafe {
            std::env::remove_var("EMBEDDING_ENABLED");
        }
        let settings = Settings::default();
        match check_embeddings(&settings) {
            CheckResult::Skip(msg) => {
                assert!(
                    msg.contains("disabled"),
                    "expected 'disabled' in skip message, got: {msg}"
                );
            }
            other => panic!(
                "expected Skip for disabled embeddings, got: {}",
                format_result(&other)
            ),
        }
    }

    #[test]
    fn check_routines_enabled_by_default() {
        let _guard = crate::config::helpers::lock_env();
        // SAFETY: Under ENV_MUTEX.
        unsafe {
            std::env::remove_var("ROUTINES_ENABLED");
        }
        match check_routines_config() {
            CheckResult::Pass(msg) => {
                assert!(
                    msg.contains("enabled"),
                    "routines should be enabled by default, got: {msg}"
                );
            }
            other => panic!(
                "expected Pass for default routines, got: {}",
                format_result(&other)
            ),
        }
    }

    #[test]
    fn check_secrets_env_without_var_returns_fail() {
        let settings = Settings {
            secrets_master_key_source: crate::settings::KeySource::Env,
            ..Default::default()
        };
        match check_secrets(&settings) {
            CheckResult::Fail(msg) => {
                assert!(
                    msg.contains("SECRETS_MASTER_KEY not set"),
                    "expected mention of missing env var, got: {msg}"
                );
            }
            CheckResult::Pass(_) => {
                // If SECRETS_MASTER_KEY happens to be set in the environment,
                // Pass is correct — don't fail the test.
            }
            other => panic!(
                "expected Fail or Pass for env key source, got: {}",
                format_result(&other)
            ),
        }
    }

    #[test]
    fn resolve_command_in_path_finds_sh() {
        // sh is present on every POSIX system; resolve by bare name.
        let resolved = resolve_command_in_path("sh");
        let Some(path) = resolved else {
            // On exotic test runners without sh, don't fail — just report.
            eprintln!("note: 'sh' not resolvable in this environment");
            return;
        };
        assert!(
            path.is_absolute(),
            "resolved sh should be absolute: {}",
            path.display()
        );
    }

    #[test]
    fn resolve_command_in_path_returns_none_for_nonexistent() {
        let resolved = resolve_command_in_path("__lunarwing_definitely_missing_bin__");
        assert!(
            resolved.is_none(),
            "expected None for nonexistent command, got {:?}",
            resolved
        );
    }

    #[test]
    fn preflight_mcp_server_http_enabled_passes_with_transport_state() {
        use crate::tools::mcp::config::McpServerConfig;
        let server = McpServerConfig::new("demo", "https://example.invalid/mcp");
        let pf = preflight_mcp_server(&server);
        assert!(
            !pf.invalid,
            "expected valid http server, detail: {}",
            pf.detail
        );
        assert!(
            pf.detail.contains("demo")
                && pf.detail.contains("http")
                && pf.detail.contains("enabled"),
            "expected transport+state in detail, got: {}",
            pf.detail
        );
    }

    #[test]
    fn preflight_mcp_server_stdio_disabled_marks_missing_as_not_counted() {
        use crate::tools::mcp::config::McpServerConfig;
        let mut server = McpServerConfig::new_stdio(
            "ghost",
            "__lunarwing_missing_stdio_cmd__",
            vec![],
            std::collections::HashMap::new(),
        );
        server.enabled = false;
        let pf = preflight_mcp_server(&server);
        assert!(
            !pf.invalid,
            "disabled stdio with missing cmd is still valid config"
        );
        assert!(
            pf.missing,
            "disabled stdio should still flag missing command"
        );
        assert!(
            pf.detail.contains("disabled"),
            "expected 'disabled' in detail, got: {}",
            pf.detail
        );
    }

    #[test]
    fn preflight_worker_stdio_requires_worker_image_check_without_host_path_failure() {
        use crate::tools::mcp::config::{McpPlacement, McpServerConfig};

        let server = McpServerConfig::new_stdio(
            "worker-tools",
            "__lunarwing_missing_only_on_host__",
            vec![],
            std::collections::HashMap::new(),
        )
        .with_placement(McpPlacement::JobWorker);
        let pf = preflight_mcp_server(&server);
        assert!(!pf.invalid);
        assert!(!pf.missing);
        assert!(pf.detail.contains("job-worker"));
        assert!(pf.detail.contains("checked in worker image"));
        assert!(!pf.detail.contains("not found in PATH"));
    }

    fn format_result(r: &CheckResult) -> String {
        match r {
            CheckResult::Pass(s) => format!("Pass({s})"),
            CheckResult::Fail(s) => format!("Fail({s})"),
            CheckResult::Skip(s) => format!("Skip({s})"),
        }
    }
}
