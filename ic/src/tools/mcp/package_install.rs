//! Opt-in automatic package installation for stdio MCP servers.
//!
//! When a stdio MCP server config declares `auto_install`, the factory calls
//! [`ensure_package_installed`] before spawning: if the configured command is
//! not resolvable, the named package is installed once with the configured
//! package manager. Installation is always explicit per server config — it
//! never happens implicitly — and installers are spawned with argument
//! arrays, never through a shell.

use std::time::Duration;

use tokio::process::Command;

use crate::tools::mcp::config::{McpAutoInstall, McpPackageManager};
use crate::tools::tool::ToolError;

/// Maximum time a single package installation may run before it is aborted.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

/// Installer stderr captured into error messages is capped at this length.
const MAX_INSTALL_OUTPUT_CHARS: usize = 500;

/// Check whether `command` can be resolved: either an explicit path that
/// exists, or a bare executable name found on `PATH`.
pub fn command_available(command: &str) -> bool {
    if command.contains(std::path::MAIN_SEPARATOR) || command.contains('/') {
        return std::path::Path::new(command).exists();
    }
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| dir.join(command).is_file())
}

/// Ensure the package backing a stdio MCP server is installed.
///
/// No-op when `command` already resolves. Otherwise runs the configured
/// package manager (`npm install -g <package>` or
/// `pip[3] install --user <package>`) and verifies the command resolves
/// afterwards.
pub async fn ensure_package_installed(
    server_name: &str,
    command: &str,
    install: &McpAutoInstall,
) -> Result<(), ToolError> {
    if command_available(command) {
        return Ok(());
    }

    let (program, args): (&str, Vec<String>) = match install.manager {
        McpPackageManager::Npm => (
            "npm",
            vec!["install".into(), "-g".into(), install.package.clone()],
        ),
        McpPackageManager::Pip => (
            "pip",
            vec![
                "install".into(),
                "--user".into(),
                install.package.clone(),
            ],
        ),
    };

    tracing::info!(
        "MCP server '{}': command '{}' not found on PATH; auto-installing '{}' via {}",
        server_name,
        command,
        install.package,
        program
    );

    run_installer(server_name, program, &args).await?;

    if !command_available(command) {
        return Err(ToolError::ExternalService(format!(
            "[{server_name}] auto-install of '{}' succeeded but command '{command}' is still not on PATH",
            install.package
        )));
    }

    tracing::info!(
        "MCP server '{}': auto-installed '{}' successfully",
        server_name,
        install.package
    );
    Ok(())
}

/// Run one installer command with a timeout, mapping failures to ToolError.
async fn run_installer(
    server_name: &str,
    program: &str,
    args: &[String],
) -> Result<(), ToolError> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    let spawn_result = cmd.spawn();
    // On pip-bearing systems the binary is often `pip3`; fall back once.
    let mut child = match spawn_result {
        Ok(child) => child,
        Err(e) if program == "pip" && e.kind() == std::io::ErrorKind::NotFound => {
            let mut fallback = Command::new("pip3");
            fallback
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped());
            fallback.spawn().map_err(|e2| {
                ToolError::ExternalService(format!(
                    "[{server_name}] failed to launch 'pip' or 'pip3' for MCP auto-install: {e2}"
                ))
            })?
        }
        Err(e) => {
            return Err(ToolError::ExternalService(format!(
                "[{server_name}] failed to launch '{program}' for MCP auto-install: {e}"
            )));
        }
    };

    // Drain stderr concurrently so a chatty installer cannot block on a full
    // pipe, and so we can include the tail of it in error messages.
    let stderr_pipe = child.stderr.take();
    let drain = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut pipe) = stderr_pipe {
            use tokio::io::AsyncReadExt;
            let _ = pipe.read_to_end(&mut buf).await;
        }
        buf
    });

    let wait_result = tokio::time::timeout(INSTALL_TIMEOUT, child.wait()).await;
    let status = match wait_result {
        Ok(result) => result.map_err(|e| {
            drain.abort();
            ToolError::ExternalService(format!(
                "[{server_name}] MCP auto-install via '{program}' failed to complete: {e}"
            ))
        })?,
        Err(_) => {
            let _ = child.start_kill();
            drain.abort();
            return Err(ToolError::ExternalService(format!(
                "[{server_name}] MCP auto-install via '{program}' timed out after {}s",
                INSTALL_TIMEOUT.as_secs()
            )));
        }
    };

    let stderr_buf = drain.await.unwrap_or_default();
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_buf);
        let truncated: String = stderr.chars().take(MAX_INSTALL_OUTPUT_CHARS).collect();
        return Err(ToolError::ExternalService(format!(
            "[{server_name}] MCP auto-install via '{program}' exited with {}: {}",
            status, truncated
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_available_finds_common_binary() {
        // `sh` exists on every supported host.
        assert!(command_available("sh"));
    }

    #[test]
    fn test_command_available_rejects_missing_binary() {
        assert!(!command_available(
            "lunarwing-test-binary-that-does-not-exist"
        ));
    }

    #[test]
    fn test_command_available_explicit_path() {
        assert!(command_available("/bin/sh"));
        assert!(!command_available("/bin/lunarwing-test-missing"));
    }

    #[tokio::test]
    async fn test_ensure_package_installed_noop_when_command_present() {
        let install = McpAutoInstall {
            manager: McpPackageManager::Npm,
            package: "unused".to_string(),
        };
        // `sh` resolves, so no installer is ever launched.
        ensure_package_installed("test-server", "sh", &install)
            .await
            .expect("no-op install must succeed");
    }

    #[tokio::test]
    async fn test_ensure_package_installed_missing_installer_fails_closed() {
        let install = McpAutoInstall {
            manager: McpPackageManager::Npm,
            package: "lunarwing-test-bogus-package-9f8e7d6c".to_string(),
        };
        // If npm itself is absent this errors at launch; if present, the bogus
        // package fails at the registry — either way it must error, not panic.
        let result =
            ensure_package_installed("test-server", "lunarwing-test-missing-cmd", &install).await;
        if command_available("npm") {
            assert!(result.is_err());
        }
    }
}
