//! Error types for the sandbox execution subsystem.
//!
//! Covers both the generic CLI-driven sandbox (podman/docker) and the legacy
//! bollard-based container path shared with the orchestrator.

use std::time::Duration;

/// Errors that can occur in the sandbox system.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// Docker daemon is not available or not running.
    #[error("Docker not available: {reason}")]
    DockerNotAvailable { reason: String },

    /// No container runtime CLI (podman or docker) is available on PATH.
    #[error("Container runtime not available: {reason}")]
    RuntimeNotAvailable { reason: String },

    /// The sandbox image is absent locally and could not be pulled.
    #[error("Sandbox image unavailable: {image}: {reason}")]
    ImageUnavailable { image: String, reason: String },

    /// Failed to create container.
    #[error("Container creation failed: {reason}")]
    ContainerCreationFailed { reason: String },

    /// Failed to start container.
    #[error("Container start failed: {reason}")]
    ContainerStartFailed { reason: String },

    /// Command execution failed inside container.
    #[error("Execution failed: {reason}")]
    ExecutionFailed { reason: String },

    /// Command timed out.
    #[error("Command timed out after {0:?}")]
    Timeout(Duration),

    /// Container resource limit exceeded.
    #[error("Resource limit exceeded: {resource} limit of {limit}")]
    ResourceLimitExceeded { resource: String, limit: String },

    /// Network proxy error.
    #[error("Proxy error: {reason}")]
    ProxyError { reason: String },

    /// Network request blocked by policy.
    #[error("Network request blocked: {reason}")]
    NetworkBlocked { reason: String },

    /// Credential injection failed.
    #[error("Credential injection failed for {domain}: {reason}")]
    CredentialInjectionFailed { domain: String, reason: String },

    /// Docker API error.
    #[error("Docker API error: {0}")]
    Docker(#[from] bollard::errors::Error),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Configuration error.
    #[error("Configuration error: {reason}")]
    Config { reason: String },
}

impl SandboxError {
    /// Stable, machine-readable error class.
    ///
    /// This taxonomy is surfaced to agents via the shell tool's structured
    /// error envelope (`sandbox_error_kind`) so a contained agent can tell
    /// "the proxy denied my egress" (`network_blocked`) apart from "the
    /// sandbox itself is down" (`sandbox_unavailable`). The values are a
    /// contract: never rename an existing kind, only add new ones — the
    /// mapping is pinned by tests in this module.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::DockerNotAvailable { .. }
            | Self::RuntimeNotAvailable { .. }
            | Self::Docker(_)
            | Self::Io(_) => "sandbox_unavailable",
            Self::ImageUnavailable { .. } => "image_unavailable",
            Self::ContainerCreationFailed { .. }
            | Self::ContainerStartFailed { .. }
            | Self::ExecutionFailed { .. } => "execution_failed",
            Self::Timeout(_) => "timeout",
            Self::ResourceLimitExceeded { .. } => "resource_limit_exceeded",
            Self::ProxyError { .. } => "proxy_error",
            Self::NetworkBlocked { .. } => "network_blocked",
            Self::CredentialInjectionFailed { .. } => "credential_injection_failed",
            Self::Config { .. } => "config",
        }
    }

    /// One-line human remediation hint for error classes that have one.
    pub fn remediation_hint(&self) -> Option<&'static str> {
        match self {
            Self::DockerNotAvailable { .. } | Self::RuntimeNotAvailable { .. } => Some(
                "install podman (preferred) or docker, or disable the sandbox with \
                 SANDBOX_ENABLED=false",
            ),
            Self::ImageUnavailable { .. } => Some(
                "build the image locally (e.g. `podman build -t lunarwing-worker:latest \
                 ic/ -f ic/docker/sandbox.Dockerfile`) or point SANDBOX_IMAGE at an \
                 existing image / enable SANDBOX_AUTO_PULL for registry images",
            ),
            _ => None,
        }
    }
}

/// Result type for sandbox operations.
pub type Result<T> = std::result::Result<T, SandboxError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant maps to a kind — pins the full taxonomy so variants can
    /// never be added without consciously choosing a kind.
    #[test]
    fn every_variant_has_a_kind() {
        let cases: Vec<(SandboxError, &str)> = vec![
            (
                SandboxError::DockerNotAvailable { reason: "x".into() },
                "sandbox_unavailable",
            ),
            (
                SandboxError::RuntimeNotAvailable { reason: "x".into() },
                "sandbox_unavailable",
            ),
            (
                SandboxError::ImageUnavailable {
                    image: "img".into(),
                    reason: "x".into(),
                },
                "image_unavailable",
            ),
            (
                SandboxError::ContainerCreationFailed { reason: "x".into() },
                "execution_failed",
            ),
            (
                SandboxError::ContainerStartFailed { reason: "x".into() },
                "execution_failed",
            ),
            (
                SandboxError::ExecutionFailed { reason: "x".into() },
                "execution_failed",
            ),
            (SandboxError::Timeout(Duration::from_secs(1)), "timeout"),
            (
                SandboxError::ResourceLimitExceeded {
                    resource: "mem".into(),
                    limit: "2g".into(),
                },
                "resource_limit_exceeded",
            ),
            (
                SandboxError::ProxyError { reason: "x".into() },
                "proxy_error",
            ),
            (
                SandboxError::NetworkBlocked { reason: "x".into() },
                "network_blocked",
            ),
            (
                SandboxError::CredentialInjectionFailed {
                    domain: "d".into(),
                    reason: "x".into(),
                },
                "credential_injection_failed",
            ),
            (
                SandboxError::Io(std::io::Error::other("x")),
                "sandbox_unavailable",
            ),
            (SandboxError::Config { reason: "x".into() }, "config"),
        ];

        for (err, expected_kind) in cases {
            assert_eq!(
                err.kind(),
                expected_kind,
                "kind mismatch for {err:?}: expected {expected_kind}"
            );
        }
    }

    #[test]
    fn remediation_hints_cover_availability_classes() {
        assert!(
            SandboxError::RuntimeNotAvailable {
                reason: "no binary".into()
            }
            .remediation_hint()
            .is_some()
        );
        assert!(
            SandboxError::ImageUnavailable {
                image: "img".into(),
                reason: "no pull".into()
            }
            .remediation_hint()
            .is_some()
        );
        assert!(
            SandboxError::Timeout(Duration::from_secs(1))
                .remediation_hint()
                .is_none()
        );
    }
}
