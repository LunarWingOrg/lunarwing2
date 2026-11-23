//! Versioned capability requests, host authorization, and secret-free decisions.
//! This belongs to the generic shell sandbox, not persistent worker containers.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::sandbox::config::{
    DEFAULT_PIDS_LIMIT, ResourceLimits, SandboxNetworkMode, SandboxPolicy,
};
use crate::sandbox::error::{Result, SandboxError};
use crate::sandbox::git_grant::{self, GIT_REMOTE_EXTENSION, GitGrantAuthorization};

pub const CAPABILITY_MANIFEST_VERSION: u16 = 1;
const MAX_MANIFEST_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityManifest {
    pub manifest_version: u16,
    #[serde(default)]
    pub network: NetworkCapabilityRequest,
    #[serde(default)]
    pub filesystem: FilesystemCapabilityRequest,
    #[serde(default)]
    pub resources: ResourceCapabilityRequest,
    #[serde(default)]
    pub secrets: SecretCapabilityRequest,
    /// New well-known capabilities must be reviewed before a backend may grant them.
    #[serde(default)]
    pub extensions: BTreeSet<String>,
    /// Optional narrowing of a granted git remote set (WI#816). Naming a
    /// remote never grants one the host policy does not already allow.
    #[serde(default)]
    pub git: GitCapabilityRequest,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitCapabilityRequest {
    #[serde(default)]
    pub remotes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkRequestMode {
    #[default]
    Disabled,
    None,
    ProxyEnforced,
    Tunnel,
    Proxy,
}

impl NetworkRequestMode {
    fn mode(self) -> SandboxNetworkMode {
        match self {
            Self::Disabled | Self::None => SandboxNetworkMode::None,
            Self::ProxyEnforced => SandboxNetworkMode::ProxyEnforced,
            Self::Tunnel => SandboxNetworkMode::Tunnel,
            Self::Proxy => SandboxNetworkMode::Proxy,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DnsRequestMode {
    #[default]
    Default,
    Provider,
    Disabled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkProvider {
    Netbird,
    Wireguard,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NetworkCapabilityRequest {
    #[serde(default)]
    pub mode: NetworkRequestMode,
    #[serde(default)]
    pub overlay: Option<NetworkProvider>,
    #[serde(default)]
    pub dns: DnsRequestMode,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilesystemCapabilityRequest {
    /// Virtual destinations only. The host source of `/workspace` is the
    /// call's working directory, checked by `mount_guard`; the manifest does
    /// not name or narrow it. An omitted `read_only` means `["/workspace"]`
    /// unless `read_write` asks for the workspace (WI#797 review AIL-4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<Vec<PathBuf>>,
    #[serde(default)]
    pub read_write: Vec<PathBuf>,
}

impl FilesystemCapabilityRequest {
    /// The read-only paths in effect: an omitted list is the read-only
    /// workspace, or nothing when the workspace is requested writable.
    pub fn read_only_paths(&self) -> Vec<PathBuf> {
        match &self.read_only {
            Some(paths) => paths.clone(),
            None if self.read_write.is_empty() => workspace_read_only(),
            None => Vec::new(),
        }
    }
}

fn workspace_read_only() -> Vec<PathBuf> {
    vec![PathBuf::from("/workspace")]
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourceCapabilityRequest {
    /// CPU shares are a scheduler weight, not a dedicated CPU count.
    pub cpu_shares: Option<u32>,
    pub memory_bytes: Option<u64>,
    pub timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SecretCapabilityRequest {
    #[serde(default)]
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityOrigin {
    Declared,
    Compatibility,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum EnforcementStatus {
    Enforced,
    Advisory,
    NotRequested,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectiveNetworkCapability {
    pub mode: SandboxNetworkMode,
    pub provider: Option<NetworkProvider>,
    pub dns: DnsRequestMode,
    pub enforcement: EnforcementStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectiveFilesystemCapability {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub writable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectiveResourceCapability {
    pub memory_bytes: u64,
    pub cpu_shares: u32,
    pub pids_limit: u32,
    pub timeout_seconds: u64,
    pub disk_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SecretInjectionStatus {
    None,
    LegacyConfiguredMappings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectiveSecretCapability {
    pub names: Vec<String>,
    pub injection: SecretInjectionStatus,
}

/// The only part of a decision intended for logs, metadata, and snapshots.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityEvidence {
    pub manifest_version: u16,
    pub origin: CapabilityOrigin,
    pub policy: String,
    pub network: EffectiveNetworkCapability,
    pub filesystem: EffectiveFilesystemCapability,
    pub resources: EffectiveResourceCapability,
    pub secrets: EffectiveSecretCapability,
    pub extensions: Vec<String>,
    /// The applied git remote grant, or `None` when the request did not ask
    /// for one (WI#816). Names, transports, hosts and operation classes only.
    #[serde(default)]
    pub git: Option<git_grant::EffectiveGitCapability>,
}

/// Reconstruction fields are host-derived, never request-controlled runtime flags.
#[derive(Debug, Clone)]
pub struct CapabilityDecision {
    pub policy: SandboxPolicy,
    pub network: SandboxNetworkMode,
    pub limits: ResourceLimits,
    pub evidence: CapabilityEvidence,
    /// Authorized git remotes for command validation and container env. This
    /// is host-derived and intentionally not part of the serialized evidence.
    pub git: Option<GitGrantAuthorization>,
}

impl SandboxNetworkMode {
    fn as_enforced_str(self) -> String {
        self.as_str().to_string()
    }
}

impl CapabilityManifest {
    /// Decode one JSON object from a tool call; host configuration is not involved.
    pub fn from_value(value: serde_json::Value) -> std::result::Result<Self, String> {
        let serialized = serde_json::to_vec(&value).map_err(|error| {
            format!("sandbox capability manifest version 1: serialization failed: {error}")
        })?;
        if serialized.len() > MAX_MANIFEST_BYTES {
            return Err(format!(
                "sandbox capability manifest version 1: exceeds {MAX_MANIFEST_BYTES} bytes"
            ));
        }
        serde_json::from_slice(&serialized).map_err(|error| {
            format!("sandbox capability manifest version 1: invalid JSON: {error}")
        })
    }

    pub fn from_json(input: &str) -> std::result::Result<Self, String> {
        if input.len() > MAX_MANIFEST_BYTES {
            return Err(format!(
                "sandbox capability manifest version 1: exceeds {MAX_MANIFEST_BYTES} bytes"
            ));
        }
        serde_json::from_str(input).map_err(|error| {
            format!("sandbox capability manifest version 1: invalid JSON: {error}")
        })
    }

    pub fn validate(&self, config: &crate::sandbox::SandboxConfig) -> Result<()> {
        if self.manifest_version != CAPABILITY_MANIFEST_VERSION {
            return Err(manifest_error(
                "manifest_version",
                format!(
                    "unsupported version {}; supported version is {CAPABILITY_MANIFEST_VERSION}",
                    self.manifest_version
                ),
            ));
        }
        self.network.validate(config)?;
        if config.policy == SandboxPolicy::FullAccess {
            return Err(manifest_error(
                "filesystem",
                "manifest execution does not support FullAccess; use the explicit compatibility policy",
            ));
        }
        self.filesystem.validate(&config.policy)?;
        self.validate_secrets_and_extensions(config)
    }

    fn validate_secrets_and_extensions(
        &self,
        config: &crate::sandbox::SandboxConfig,
    ) -> Result<()> {
        if let Some(name) = self.secrets.names.first() {
            validate_capability_name(name, "secrets.names")?;
            return Err(manifest_error(
                "secrets.names",
                "generic shell secret injection is unsupported; worker jobs retain their host-issued credential grants",
            ));
        }
        for extension in &self.extensions {
            validate_capability_name(extension, "extensions")?;
            if extension != GIT_REMOTE_EXTENSION {
                return Err(manifest_error(
                    "extensions",
                    format!("no host policy grants extension '{extension}'"),
                ));
            }
        }
        if !self.extensions.is_empty() && config.git.is_none() {
            return Err(manifest_error(
                "extensions",
                "no host policy grants extension 'git-remotes' for this tenant; \
                 SANDBOX_GIT_REMOTES is unset or empty",
            ));
        }
        if !self.git.remotes.is_empty() && !self.extensions.contains(GIT_REMOTE_EXTENSION) {
            return Err(manifest_error(
                "git.remotes",
                "naming git remotes requires extensions=[\"git-remotes\"]",
            ));
        }
        if self.git.remotes.len() > git_grant::MAX_GIT_REMOTES {
            return Err(manifest_error(
                "git.remotes",
                format!(
                    "at most {} remotes may be named",
                    git_grant::MAX_GIT_REMOTES
                ),
            ));
        }
        for name in &self.git.remotes {
            validate_capability_name(name, "git.remotes")?;
        }
        Ok(())
    }
}

impl NetworkCapabilityRequest {
    fn validate(&self, config: &crate::sandbox::SandboxConfig) -> Result<()> {
        let requested = self.mode.mode();
        if self.mode == NetworkRequestMode::Proxy {
            return Err(manifest_error(
                "network.mode",
                "advisory proxy requests are not allowed in manifest version 1; request proxy-enforced or none",
            ));
        }
        if requested == SandboxNetworkMode::None {
            if self.overlay.is_some() || self.dns != DnsRequestMode::Default {
                return Err(manifest_error(
                    "network",
                    "overlay or DNS selection conflicts with network.mode=none",
                ));
            }
        } else if requested != config.network {
            return Err(manifest_error(
                "network.mode",
                format!(
                    "requested '{}' but host policy permits only '{}'; no fallback is allowed",
                    requested.as_enforced_str(),
                    config.network.as_enforced_str()
                ),
            ));
        }
        self.validate_overlay_and_dns(config)?;
        Ok(())
    }

    fn validate_overlay_and_dns(&self, config: &crate::sandbox::SandboxConfig) -> Result<()> {
        match self.mode.mode() {
            SandboxNetworkMode::Tunnel => {
                let tunnel = config.tunnel.as_ref().ok_or_else(|| {
                    manifest_error(
                        "network.overlay",
                        "host has no tunnel capability configured",
                    )
                })?;
                validate_tunnel_request(self, tunnel)?;
            }
            SandboxNetworkMode::ProxyEnforced
                if self.overlay.is_some() || self.dns == DnsRequestMode::Provider =>
            {
                return Err(manifest_error(
                    "network",
                    "proxy-enforced does not support overlay or provider DNS selection",
                ));
            }
            SandboxNetworkMode::ProxyEnforced if self.dns == DnsRequestMode::Disabled => {
                return Err(manifest_error(
                    "network.dns",
                    "disabling DNS would not disable the public-only proxy's host-side DNS",
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

fn validate_tunnel_request(
    request: &NetworkCapabilityRequest,
    tunnel: &crate::sandbox::TunnelSidecarConfig,
) -> Result<()> {
    let provider = tunnel.provider.as_str();
    if request
        .overlay
        .is_some_and(|requested| requested.as_str() != provider)
    {
        return Err(manifest_error(
            "network.overlay",
            format!("overlay does not match the host-authorized provider '{provider}'"),
        ));
    }
    match (provider, request.dns) {
        ("wireguard", DnsRequestMode::Default | DnsRequestMode::Provider) => {}
        ("netbird", DnsRequestMode::Provider) if tunnel.dns.is_empty() => {}
        (_, DnsRequestMode::Default) => {}
        (_, DnsRequestMode::Provider) => {
            return Err(manifest_error(
                "network.dns",
                "provider DNS is unavailable without host-managed WireGuard resolvers",
            ));
        }
        (_, DnsRequestMode::Disabled) => {
            return Err(manifest_error(
                "network.dns",
                "tunnel DNS cannot be disabled by an untrusted request",
            ));
        }
    }
    Ok(())
}

impl From<crate::sandbox::TunnelProvider> for NetworkProvider {
    fn from(provider: crate::sandbox::TunnelProvider) -> Self {
        match provider {
            crate::sandbox::TunnelProvider::Netbird => Self::Netbird,
            crate::sandbox::TunnelProvider::Wireguard => Self::Wireguard,
        }
    }
}

impl NetworkProvider {
    fn as_str(self) -> &'static str {
        match self {
            Self::Netbird => "netbird",
            Self::Wireguard => "wireguard",
        }
    }
}

impl FilesystemCapabilityRequest {
    fn validate(&self, host_policy: &SandboxPolicy) -> Result<()> {
        let read_only = self.read_only_paths();
        if !read_only.is_empty() && !self.read_write.is_empty() {
            return Err(manifest_error(
                "filesystem",
                "read_only and read_write cannot both request workspace paths",
            ));
        }
        let requested = read_only.iter().chain(&self.read_write).collect::<Vec<_>>();
        if requested.len() != 1 {
            return Err(manifest_error(
                "filesystem",
                "version 1 supports one virtual /workspace mount, not arbitrary host mounts",
            ));
        }
        for path in &requested {
            validate_virtual_mount(path)?;
        }
        if !self.read_write.is_empty() && *host_policy != SandboxPolicy::WorkspaceWrite {
            return Err(manifest_error(
                "filesystem.read_write",
                "host policy does not authorize a writable workspace",
            ));
        }
        Ok(())
    }
}

fn validate_virtual_mount(path: &Path) -> Result<()> {
    if path.as_os_str() != "/workspace" {
        return Err(manifest_error(
            "filesystem",
            format!(
                "refusing '{}': only the exact virtual path /workspace is supported; ambiguous or relative paths are not accepted",
                path.display()
            ),
        ));
    }
    let mut components = path.components();
    if components.next() != Some(Component::RootDir)
        || components.next() != Some(Component::Normal("workspace".as_ref()))
        || components.next().is_some()
    {
        return Err(manifest_error(
            "filesystem",
            format!(
                "refusing '{}': only the canonical virtual path /workspace is supported; host paths, traversal, and symlinks are not accepted",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn validate_capability_name(name: &str, field: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
        });
    if valid {
        Ok(())
    } else {
        Err(manifest_error(
            field,
            "capability names must be 1-128 lowercase ASCII letters, digits, '.', '_' or '-'; wildcards are forbidden",
        ))
    }
}

pub fn authorize_manifest(
    manifest: &CapabilityManifest,
    config: &crate::sandbox::SandboxConfig,
    working_dir: &Path,
) -> Result<CapabilityDecision> {
    manifest.validate(config)?;
    let canonical =
        crate::sandbox::mount_guard::validate_mount_dir(working_dir, &config.protected_paths)?;
    authorize_validated(manifest, config, canonical)
}

fn authorize_validated(
    manifest: &CapabilityManifest,
    config: &crate::sandbox::SandboxConfig,
    canonical: PathBuf,
) -> Result<CapabilityDecision> {
    let mode = manifest.network.mode.mode();
    let policy = if manifest.filesystem.read_write.is_empty() {
        SandboxPolicy::ReadOnly
    } else {
        SandboxPolicy::WorkspaceWrite
    };
    let network = EffectiveNetworkCapability {
        mode,
        provider: config
            .tunnel
            .as_ref()
            .filter(|_| mode == SandboxNetworkMode::Tunnel)
            .map(|tunnel| tunnel.provider.into()),
        dns: manifest.network.dns,
        enforcement: if mode == SandboxNetworkMode::None {
            tracing::debug!("Sandbox network disabled by manifest policy");
            EnforcementStatus::NotRequested
        } else {
            EnforcementStatus::Enforced
        },
    };
    let git = if manifest.extensions.contains(GIT_REMOTE_EXTENSION) {
        let host = config.git.as_ref().ok_or_else(|| {
            manifest_error(
                "extensions",
                "no host policy grants extension 'git-remotes' for this tenant",
            )
        })?;
        Some(git_grant::authorize_grant(
            host,
            &manifest.git.remotes,
            &canonical,
            mode,
        )?)
    } else {
        None
    };
    let limits = resource_decision(manifest.resources.clone(), config)?;
    let evidence = CapabilityEvidence {
        manifest_version: CAPABILITY_MANIFEST_VERSION,
        origin: CapabilityOrigin::Declared,
        policy: "host:v1".into(),
        network,
        filesystem: EffectiveFilesystemCapability {
            source: canonical,
            destination: PathBuf::from("/workspace"),
            writable: policy.allows_writes(),
        },
        resources: EffectiveResourceCapability {
            memory_bytes: limits.memory_bytes,
            cpu_shares: limits.cpu_shares,
            pids_limit: DEFAULT_PIDS_LIMIT,
            timeout_seconds: limits.timeout.as_secs(),
            disk_bytes: None,
        },
        secrets: EffectiveSecretCapability {
            names: Vec::new(),
            injection: SecretInjectionStatus::None,
        },
        extensions: manifest.extensions.iter().cloned().collect(),
        git: git.as_ref().map(GitGrantAuthorization::evidence),
    };
    Ok(CapabilityDecision {
        policy,
        network: mode,
        limits,
        evidence,
        git,
    })
}

fn resource_decision(
    request: ResourceCapabilityRequest,
    config: &crate::sandbox::SandboxConfig,
) -> Result<ResourceLimits> {
    let mut limits = ResourceLimits {
        memory_bytes: config
            .memory_limit_mb
            .checked_mul(1024 * 1024)
            .ok_or_else(|| resource_error("memory_bytes", "host memory ceiling overflows"))?,
        cpu_shares: config.cpu_shares,
        timeout: config.timeout,
        max_output_bytes: 64 * 1024,
    };
    if let Some(cpu) = request.cpu_shares {
        if cpu == 0 || cpu > config.cpu_shares {
            return Err(resource_error(
                "cpu_shares",
                format!("must be 1..={}", config.cpu_shares),
            ));
        }
        limits.cpu_shares = cpu;
    }
    if let Some(memory) = request.memory_bytes {
        if memory == 0 || memory > limits.memory_bytes {
            return Err(resource_error(
                "memory_bytes",
                format!("must be 1..={}", limits.memory_bytes),
            ));
        }
        if memory % (1024 * 1024) != 0 {
            return Err(resource_error(
                "memory_bytes",
                "must be a whole number of mebibytes",
            ));
        }
        limits.memory_bytes = memory;
    }
    if let Some(seconds) = request.timeout_seconds {
        if seconds == 0 || seconds > config.timeout.as_secs() {
            return Err(resource_error(
                "timeout_seconds",
                format!("must be 1..={}", config.timeout.as_secs()),
            ));
        }
        limits.timeout = Duration::from_secs(seconds);
    }
    Ok(limits)
}

/// Older callers use a separate, explicit compatibility decision. It is not a
/// capabilityManifest and never purports to restrict full host access.
pub fn compatibility_decision(
    config: &crate::sandbox::SandboxConfig,
    working_dir: &Path,
) -> CapabilityDecision {
    let network = if config.policy == SandboxPolicy::FullAccess {
        None
    } else {
        Some(config.network)
    };
    let effective_network = EffectiveNetworkCapability {
        mode: network.unwrap_or(SandboxNetworkMode::None),
        provider: config.tunnel.as_ref().map(|tunnel| tunnel.provider.into()),
        dns: DnsRequestMode::Default,
        enforcement: if config.policy == SandboxPolicy::FullAccess {
            EnforcementStatus::Advisory
        } else if network == Some(SandboxNetworkMode::None) {
            EnforcementStatus::NotRequested
        } else if network == Some(SandboxNetworkMode::Proxy) {
            EnforcementStatus::Advisory
        } else {
            EnforcementStatus::Enforced
        },
    };
    let limits = ResourceLimits {
        memory_bytes: config.memory_limit_mb.saturating_mul(1024 * 1024),
        cpu_shares: config.cpu_shares,
        timeout: config.timeout,
        max_output_bytes: 64 * 1024,
    };
    CapabilityDecision {
        policy: config.policy,
        network: config.network,
        limits: limits.clone(),
        evidence: CapabilityEvidence {
            manifest_version: CAPABILITY_MANIFEST_VERSION,
            origin: CapabilityOrigin::Compatibility,
            policy: "legacy:configured-worker-policy".into(),
            network: effective_network,
            filesystem: EffectiveFilesystemCapability {
                source: working_dir.to_path_buf(),
                destination: PathBuf::from("/workspace"),
                writable: config.policy.allows_writes(),
            },
            resources: EffectiveResourceCapability {
                memory_bytes: limits.memory_bytes,
                cpu_shares: limits.cpu_shares,
                pids_limit: DEFAULT_PIDS_LIMIT,
                timeout_seconds: limits.timeout.as_secs(),
                disk_bytes: None,
            },
            secrets: EffectiveSecretCapability {
                names: Vec::new(),
                injection: SecretInjectionStatus::LegacyConfiguredMappings,
            },
            extensions: Vec::new(),
            git: None,
        },
        git: None,
    }
}

/// Wire contract for version 1. Serde remains the authoritative parser; this
/// schema is deliberately synchronized by a structural unit test.
pub fn capability_manifest_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["manifest_version"],
        "additionalProperties": false,
        "properties": {
            "manifest_version": {"type": "integer", "const": CAPABILITY_MANIFEST_VERSION},
            "network": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "mode": {"enum": ["disabled", "none", "proxy-enforced", "tunnel"], "default": "disabled"},
                    "overlay": {"type": ["string", "null"], "enum": ["netbird", "wireguard", null]},
                    "dns": {"enum": ["default", "provider", "disabled"], "default": "default"}
                }
            },
            "filesystem": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "read_only": {"type": "array", "items": {"const": "/workspace"}, "maxItems": 1, "description": "Omitted: [\"/workspace\"], unless read_write requests the workspace"},
                    "read_write": {"type": "array", "items": {"const": "/workspace"}, "maxItems": 1, "default": []}
                }
            },
            "resources": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "cpu_shares": {"type": ["integer", "null"], "minimum": 1},
                    "memory_bytes": {"type": ["integer", "null"], "minimum": 1048576, "multipleOf": 1048576},
                    "timeout_seconds": {"type": ["integer", "null"], "minimum": 1}
                }
            },
            "secrets": {
                "type": "object", "additionalProperties": false,
                "properties": {"names": {"type": "array", "items": {"type": "string"}, "maxItems": 0, "default": []}}
            },
            "extensions": {
                "type": "array",
                "items": {"const": "git-remotes"},
                "maxItems": 1,
                "default": [],
                "description": "The only grantable extension is 'git-remotes', and only when host policy (SANDBOX_GIT_REMOTES) grants it"
            },
            "git": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "remotes": {
                        "type": "array",
                        "items": {"type": "string"},
                        "maxItems": 32,
                        "default": [],
                        "description": "Narrows the host-granted remote set; it can never add a remote"
                    }
                }
            }
        }
    })
}

pub fn audit_json(evidence: &CapabilityEvidence) -> Result<String> {
    serde_json::to_string(evidence).map_err(|error| manifest_error("audit", error.to_string()))
}

fn resource_error(field: &str, message: impl std::fmt::Display) -> SandboxError {
    manifest_error(&format!("resources.{field}"), message)
}

fn manifest_error(field: &str, message: impl std::fmt::Display) -> SandboxError {
    SandboxError::CapabilityDenied {
        field: field.to_string(),
        reason: format!("sandbox capability manifest version 1: {field}: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    static OVERHEAD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn minimal() -> CapabilityManifest {
        CapabilityManifest {
            manifest_version: 1,
            ..CapabilityManifest::default()
        }
    }

    fn decision_with(network: &str) -> CapabilityDecision {
        let manifest = CapabilityManifest {
            network: NetworkCapabilityRequest {
                mode: serde_json::from_str(&format!("\"{network}\"")).unwrap(),
                ..NetworkCapabilityRequest::default()
            },
            ..minimal()
        };
        let config = crate::sandbox::SandboxConfig::default();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("repo");
        std::fs::create_dir(&path).unwrap();
        authorize_manifest(&manifest, &config, &path).unwrap()
    }

    #[test]
    fn omitted_manifest_fields_are_deny_by_default() {
        let manifest: CapabilityManifest =
            serde_json::from_str("{\"manifest_version\":1}").unwrap();
        assert_eq!(manifest.network.mode, NetworkRequestMode::Disabled);
        assert_eq!(manifest.filesystem.read_only, None);
        assert_eq!(manifest.filesystem.read_only_paths(), workspace_read_only());
        assert!(manifest.secrets.names.is_empty());
        assert!(manifest.extensions.is_empty());
        assert!(manifest.resources.timeout_seconds.is_none());
    }

    #[test]
    fn unknown_fields_versions_and_malformed_json_name_version_one() {
        for input in [
            "{",
            "{\"manifest_version\":0}",
            "{\"manifest_version\":2}",
            "{\"manifest_version\":1,\"unknown\":true}",
            "{\"manifest_version\":1,\"network\":{\"mode\":\"none\",\"dns\":\"custom\"}}",
        ] {
            let manifest = CapabilityManifest::from_json(input);
            let error = match manifest {
                Ok(manifest) => manifest
                    .validate(&crate::sandbox::SandboxConfig::default())
                    .unwrap_err(),
                Err(error) => SandboxError::CapabilityDenied {
                    field: "manifest_version".into(),
                    reason: error,
                },
            };
            assert!(error.to_string().contains("version 1"), "{error}");
        }
    }

    #[test]
    fn network_is_reduced_not_silently_fallback() {
        let decision = decision_with("none");
        assert_eq!(decision.network, SandboxNetworkMode::None);
        assert_eq!(
            decision.evidence.network.enforcement,
            EnforcementStatus::NotRequested
        );
        let manifest = serde_json::from_str::<CapabilityManifest>(
            "{\"manifest_version\":1,\"network\":{\"mode\":\"none\",\"overlay\":\"netbird\"}}",
        )
        .unwrap();
        let error = manifest
            .validate(&crate::sandbox::SandboxConfig::default())
            .unwrap_err();
        assert!(error.to_string().contains("conflicts"));
    }

    #[test]
    fn published_schema_has_no_placeholder_and_matches_decoder() {
        let schema = capability_manifest_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["manifest_version"]["const"], 1);
        assert_eq!(
            schema["properties"]["secrets"]["properties"]["names"]["maxItems"],
            0
        );
        assert_eq!(
            schema["properties"]["extensions"]["items"]["const"],
            "git-remotes"
        );
        assert_eq!(
            schema["properties"]["git"]["properties"]["remotes"]["maxItems"],
            git_grant::MAX_GIT_REMOTES
        );
        let encoded = serde_json::to_string(&schema).unwrap();
        assert!(!encoded.contains("TODO"));
        assert!(
            CapabilityManifest::from_value(serde_json::json!({
                "manifest_version": 1,
                "network": {"mode": "proxy"}
            }))
            .is_ok()
        );
        assert!(
            CapabilityManifest::from_value(serde_json::json!({
                "manifest_version": 1,
                "resources": {"memory": "4GiB"}
            }))
            .is_err()
        );
    }

    /// WI#820: the published schema must describe what `resource_decision`
    /// enforces. `cpu_shares` accepts any value from 1 up to the host ceiling
    /// and only that range is checked, so the schema carries no divisibility
    /// rule; the test pins both sides together.
    #[test]
    fn cpu_shares_schema_matches_decoder() {
        let schema = capability_manifest_schema();
        let cpu = &schema["properties"]["resources"]["properties"]["cpu_shares"];
        assert_eq!(cpu["minimum"], 1);
        assert!(
            cpu.get("multipleOf").is_none(),
            "cpu_shares schema must not require a multiple the decoder does not enforce"
        );
        let config = crate::sandbox::SandboxConfig::default();
        let accepted = ResourceCapabilityRequest {
            cpu_shares: Some(32),
            memory_bytes: None,
            timeout_seconds: None,
        };
        let limits = resource_decision(accepted, &config).unwrap();
        assert_eq!(limits.cpu_shares, 32);
        for rejected in [0, config.cpu_shares + 1] {
            let request = ResourceCapabilityRequest {
                cpu_shares: Some(rejected),
                memory_bytes: None,
                timeout_seconds: None,
            };
            let error = resource_decision(request, &config).unwrap_err();
            assert!(
                error.to_string().contains("resources.cpu_shares"),
                "cpu_shares {rejected} must be refused"
            );
        }
    }

    #[test]
    fn empty_workspace_mode_is_not_silently_broadened() {
        for value in [
            serde_json::json!({"manifest_version":1,"filesystem":{"read_only":[],"read_write":[]}}),
            serde_json::json!({"manifest_version":1,"filesystem":{"read_only":[null],"read_write":[]}}),
        ] {
            let parsed = CapabilityManifest::from_value(value);
            if let Ok(manifest) = parsed {
                assert!(
                    manifest
                        .validate(&crate::sandbox::SandboxConfig::default())
                        .is_err()
                );
            }
        }
    }

    #[test]
    fn hostile_paths_and_writes_are_denied_before_runtime() {
        let temp = tempfile::tempdir().unwrap();
        for path in [
            "/etc",
            "/workspace/../env",
            "workspace",
            "//workspace",
            "/workspace/link",
        ] {
            let manifest = CapabilityManifest {
                filesystem: FilesystemCapabilityRequest {
                    read_only: Some(vec![path.into()]),
                    read_write: vec![],
                },
                ..minimal()
            };
            assert!(
                authorize_manifest(
                    &manifest,
                    &crate::sandbox::SandboxConfig::default(),
                    temp.path()
                )
                .is_err(),
                "accepted {path}"
            );
        }
        let write = CapabilityManifest {
            filesystem: FilesystemCapabilityRequest {
                read_only: Some(vec![]),
                read_write: workspace_read_only(),
            },
            ..minimal()
        };
        let writable = crate::sandbox::SandboxConfig {
            policy: SandboxPolicy::WorkspaceWrite,
            ..crate::sandbox::SandboxConfig::default()
        };
        assert!(authorize_manifest(&write, &writable, temp.path()).is_ok());
        let readonly = crate::sandbox::SandboxConfig {
            policy: SandboxPolicy::ReadOnly,
            ..Default::default()
        };
        assert!(authorize_manifest(&write, &readonly, temp.path()).is_err());
    }

    #[test]
    fn hidden_workspace_symlinks_use_existing_containment_validation() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("repo")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), temp.path().join("repo/link")).unwrap();
        let request = minimal();
        let decision = authorize_manifest(
            &request,
            &crate::sandbox::SandboxConfig::default(),
            &temp.path().join("repo"),
        )
        .unwrap();
        assert!(decision.evidence.filesystem.source.is_absolute());
        #[cfg(unix)]
        {
            std::fs::create_dir(temp.path().join("private")).unwrap();
            std::os::unix::fs::symlink(temp.path().join("private"), temp.path().join("hostcwd"))
                .unwrap();
            let config = crate::sandbox::SandboxConfig {
                protected_paths: vec![temp.path().join("private")],
                ..crate::sandbox::SandboxConfig::default()
            };
            let error = authorize_manifest(&minimal(), &config, &temp.path().join("hostcwd"))
                .expect_err("host symlink must be canonicalized and refused");
            assert!(matches!(error, SandboxError::MountPathDenied { .. }));
        }
        // WI#797 review AIL-3: the guard refuses what is protected (and `/`,
        // `/run`, `/var/run`), not every system path; a `..` walk into a
        // protected directory is refused once it is canonicalized.
        #[cfg(unix)]
        {
            let walk = temp.path().join("repo/../private");
            let refused = crate::sandbox::mount_guard::validate_mount_dir(
                &walk,
                &[temp.path().join("private")],
            );
            assert!(
                matches!(
                    &refused,
                    Err(SandboxError::MountPathDenied { reason, .. })
                        if reason.contains("protected directory")
                ),
                "{refused:?}"
            );
        }
        assert!(
            crate::sandbox::mount_guard::validate_mount_dir(
                &decision.evidence.filesystem.source,
                &[outside.path().to_path_buf()]
            )
            .is_ok()
        );
    }

    #[test]
    fn writable_workspace_needs_no_explicit_empty_read_only_list() {
        // WI#797 review AIL-4: `{"read_write": ["/workspace"]}` alone asks for
        // a writable workspace; naming both lists stays a conflict.
        let temp = tempfile::tempdir().unwrap();
        let writable = crate::sandbox::SandboxConfig {
            policy: SandboxPolicy::WorkspaceWrite,
            ..crate::sandbox::SandboxConfig::default()
        };
        let manifest = CapabilityManifest::from_value(serde_json::json!({
            "manifest_version": 1,
            "filesystem": {"read_write": ["/workspace"]}
        }))
        .unwrap();
        let decision = authorize_manifest(&manifest, &writable, temp.path()).unwrap();
        assert_eq!(decision.policy, SandboxPolicy::WorkspaceWrite);
        assert!(decision.evidence.filesystem.writable);
        let both = CapabilityManifest::from_value(serde_json::json!({
            "manifest_version": 1,
            "filesystem": {"read_only": ["/workspace"], "read_write": ["/workspace"]}
        }))
        .unwrap();
        assert!(authorize_manifest(&both, &writable, temp.path()).is_err());
        let default = authorize_manifest(&minimal(), &writable, temp.path()).unwrap();
        assert_eq!(default.policy, SandboxPolicy::ReadOnly);
        let readonly_host = crate::sandbox::SandboxConfig {
            policy: SandboxPolicy::ReadOnly,
            ..crate::sandbox::SandboxConfig::default()
        };
        assert!(authorize_manifest(&manifest, &readonly_host, temp.path()).is_err());
    }

    #[test]
    fn secret_references_and_extensions_fail_closed() {
        for secret in ["*", "git_credentials", "other_tenant.git"] {
            let manifest: CapabilityManifest = serde_json::from_str(&format!(
                "{{\"manifest_version\":1,\"secrets\":{{\"names\":[\"{secret}\"]}}}}"
            ))
            .unwrap();
            let error = manifest
                .validate(&crate::sandbox::SandboxConfig::default())
                .unwrap_err();
            assert!(error.to_string().contains("secrets"), "{error}");
        }
    }

    #[test]
    fn tunnel_provider_and_dns_must_match_the_host_policy() {
        let tunnel = crate::sandbox::TunnelSidecarConfig::default();
        let config = crate::sandbox::SandboxConfig {
            network: SandboxNetworkMode::Tunnel,
            tunnel: Some(tunnel),
            ..crate::sandbox::SandboxConfig::default()
        };
        let tunnel_manifest = serde_json::from_value::<CapabilityManifest>(serde_json::json!({
            "manifest_version":1,
            "network":{"mode":"tunnel","overlay":"netbird","dns":"provider"}
        }))
        .unwrap();
        assert!(tunnel_manifest.validate(&config).is_ok());
        let mut conflict = tunnel_manifest.clone();
        conflict.network.overlay = Some(NetworkProvider::Netbird);
        assert!(
            conflict
                .validate(&crate::sandbox::SandboxConfig::default())
                .is_err()
        );
        conflict.network.overlay = Some(NetworkProvider::Wireguard);
        assert!(conflict.validate(&config).is_err());
        let mut disabled = tunnel_manifest;
        disabled.network.dns = DnsRequestMode::Disabled;
        assert!(disabled.validate(&config).is_err());
    }

    #[test]
    fn resource_limits_must_reduce_host_ceiling() {
        let mut request = minimal();
        request.resources = ResourceCapabilityRequest {
            cpu_shares: Some(4097),
            memory_bytes: Some(123),
            timeout_seconds: Some(0),
        };
        let error = resource_decision(request.resources, &crate::sandbox::SandboxConfig::default())
            .unwrap_err();
        assert!(error.to_string().contains("resources.cpu_shares"));
        let reduced = ResourceCapabilityRequest {
            cpu_shares: Some(32),
            memory_bytes: Some(512 * 1024 * 1024),
            timeout_seconds: Some(1),
        };
        let limits = resource_decision(reduced, &crate::sandbox::SandboxConfig::default()).unwrap();
        assert_eq!(limits.memory_bytes, 512 * 1024 * 1024);
        assert_eq!(limits.timeout, Duration::from_secs(1));
    }

    #[test]
    fn audit_contains_no_secret_values_or_injectable_json() {
        let decision = decision_with("none");
        let json = audit_json(&decision.evidence).unwrap();
        let copy: CapabilityEvidence = serde_json::from_str(&json).unwrap();
        assert_eq!(copy, decision.evidence);
        assert!(json.contains("\"origin\":\"declared\""));
        assert!(!json.contains("credential-value"));
        assert!(json.contains("\"disk_bytes\":null"));
        let untrusted = CapabilityEvidence {
            policy: "\"\nsecret".into(),
            ..decision.evidence
        };
        assert!(
            serde_json::from_str::<CapabilityEvidence>(&audit_json(&untrusted).unwrap()).is_ok()
        );
    }

    #[test]
    fn compatibility_is_explicit_never_a_default_manifest_grant() {
        let config = crate::sandbox::SandboxConfig::default();
        let decision = compatibility_decision(&config, Path::new("/tmp/workspace"));
        assert_eq!(decision.evidence.origin, CapabilityOrigin::Compatibility);
        assert_eq!(decision.evidence.policy, "legacy:configured-worker-policy");
        assert_eq!(
            decision.evidence.secrets.injection,
            SecretInjectionStatus::LegacyConfiguredMappings
        );
        assert_eq!(minimal().network.mode, NetworkRequestMode::Disabled);
    }

    fn git_config(remotes: &str) -> crate::sandbox::SandboxConfig {
        crate::sandbox::SandboxConfig {
            network: SandboxNetworkMode::ProxyEnforced,
            git: Some(
                crate::sandbox::GitGrantConfig::from_spec(remotes)
                    .expect("grant spec")
                    .expect("grant"),
            ),
            ..crate::sandbox::SandboxConfig::default()
        }
    }

    fn git_workspace() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(temp.path().join("remotes/origin.git")).expect("remote");
        temp
    }

    fn git_manifest(value: serde_json::Value) -> CapabilityManifest {
        CapabilityManifest::from_value(value).expect("manifest")
    }

    #[test]
    fn git_grant_is_denied_without_host_policy_and_other_extensions_stay_closed() {
        let temp = git_workspace();
        let manifest = git_manifest(serde_json::json!({
            "manifest_version": 1,
            "extensions": ["git-remotes"]
        }));
        let error = authorize_manifest(
            &manifest,
            &crate::sandbox::SandboxConfig::default(),
            temp.path(),
        )
        .expect_err("no host grant");
        assert!(matches!(error, SandboxError::CapabilityDenied { .. }));
        assert!(error.to_string().contains("git-remotes"), "{error}");
        for extension in ["browser", "devices", "gui"] {
            let manifest = git_manifest(serde_json::json!({
                "manifest_version": 1,
                "extensions": [extension]
            }));
            let config = git_config("origin=file:///workspace/remotes/origin.git");
            let error = authorize_manifest(&manifest, &config, temp.path())
                .expect_err("unreviewed extension");
            assert!(error.to_string().contains(extension), "{error}");
        }
    }

    #[test]
    fn git_grant_records_the_write_class_and_credential_names_without_values() {
        let temp = git_workspace();
        let mut config = git_config("origin=http://git.internal/org/repo.git");
        config.git = Some(
            crate::sandbox::GitGrantConfig::from_full_spec(
                "origin=http://git.internal/org/repo.git",
                "origin",
                "origin=GIT_ORIGIN_TOKEN",
                false,
            )
            .expect("grant spec")
            .expect("grant"),
        );
        let manifest = git_manifest(serde_json::json!({
            "manifest_version": 1,
            "network": {"mode": "proxy-enforced"},
            "extensions": ["git-remotes"]
        }));
        let decision = authorize_manifest(&manifest, &config, temp.path()).expect("grant");
        let git = decision.evidence.git.as_ref().expect("git evidence");
        assert_eq!(
            git.remotes[0].operations,
            vec![
                crate::sandbox::git_grant::GitOperationClass::Read,
                crate::sandbox::git_grant::GitOperationClass::Write,
            ]
        );
        assert_eq!(git.credentials.len(), 1);
        assert_eq!(git.credentials[0].remote, "origin");
        assert_eq!(git.credentials[0].secret_env, "GIT_ORIGIN_TOKEN");
        let json = audit_json(&decision.evidence).expect("audit");
        assert!(
            json.contains("\"operations\":[\"read\",\"write\"]"),
            "{json}"
        );
        assert!(json.contains("GIT_ORIGIN_TOKEN"), "{json}");
        // The variable name is recorded; no value is.
        assert!(!json.contains("Basic "), "{json}");
    }

    #[test]
    fn git_grant_records_host_operations_and_no_secret_values() {
        let temp = git_workspace();
        let config = git_config(
            "origin=file:///workspace/remotes/origin.git,\
             upstream=https://git.example.test/org/repo.git",
        );
        let manifest = git_manifest(serde_json::json!({
            "manifest_version": 1,
            "network": {"mode": "proxy-enforced"},
            "extensions": ["git-remotes"],
            "git": {"remotes": ["origin"]}
        }));
        let decision = authorize_manifest(&manifest, &config, temp.path()).expect("grant");
        let git = decision.evidence.git.as_ref().expect("git evidence");
        assert_eq!(git.grant, "host:sandbox-git-remotes");
        assert_eq!(git.remotes.len(), 1);
        assert_eq!(git.remotes[0].name, "origin");
        assert_eq!(
            git.remotes[0].operations,
            vec![crate::sandbox::git_grant::GitOperationClass::Read]
        );
        assert_eq!(
            decision.evidence.extensions,
            vec!["git-remotes".to_string()]
        );
        let json = audit_json(&decision.evidence).expect("audit");
        assert!(json.contains("host:sandbox-git-remotes"));
        assert!(json.contains("\"transport\":\"file\""));
        assert!(json.contains("\"operations\":[\"read\"]"));
        assert!(
            !json.contains("git.example.test"),
            "an unselected remote leaked into the evidence: {json}"
        );
        for needle in [
            "token",
            "credential-value",
            "password",
            "PRIVATE KEY",
            "user:",
        ] {
            assert!(
                !json
                    .to_ascii_lowercase()
                    .contains(&needle.to_ascii_lowercase()),
                "{json}"
            );
        }
        let host_side = decision.git.as_ref().expect("host authorization");
        assert_eq!(host_side.remotes.len(), 1);
        assert_eq!(
            host_side.cwd,
            temp.path().canonicalize().expect("canonical")
        );
    }

    #[test]
    fn git_remotes_cannot_exceed_host_policy_or_bypass_the_extension() {
        let temp = git_workspace();
        let config = git_config("origin=file:///workspace/remotes/origin.git");
        let outside = git_manifest(serde_json::json!({
            "manifest_version": 1,
            "extensions": ["git-remotes"],
            "git": {"remotes": ["other"]}
        }));
        let error = authorize_manifest(&outside, &config, temp.path()).expect_err("unknown remote");
        assert!(error.to_string().contains("other"), "{error}");
        let unnamed = git_manifest(serde_json::json!({
            "manifest_version": 1,
            "git": {"remotes": ["origin"]}
        }));
        let error = authorize_manifest(&unnamed, &config, temp.path()).expect_err("no extension");
        assert!(error.to_string().contains("extensions"), "{error}");
        let bare = git_manifest(serde_json::json!({"manifest_version": 1}));
        let decision = authorize_manifest(&bare, &config, temp.path()).expect("compat decision");
        assert!(decision.git.is_none());
        assert!(decision.evidence.git.is_none());
    }

    #[test]
    fn git_network_remotes_fail_closed_under_none_and_tunnel() {
        let temp = git_workspace();
        let mut config = git_config("upstream=https://git.example.test/org/repo.git");
        for (mode, request) in [
            (SandboxNetworkMode::None, "none"),
            (SandboxNetworkMode::Tunnel, "tunnel"),
        ] {
            config.network = mode;
            config.tunnel = (mode == SandboxNetworkMode::Tunnel)
                .then(crate::sandbox::TunnelSidecarConfig::default);
            let manifest = git_manifest(serde_json::json!({
                "manifest_version": 1,
                "network": {"mode": request},
                "extensions": ["git-remotes"]
            }));
            let error = authorize_manifest(&manifest, &config, temp.path())
                .expect_err("unsupported egress mode");
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{error}"
            );
        }
        // A file-only grant still works without egress.
        config.network = SandboxNetworkMode::None;
        config.tunnel = None;
        let config = crate::sandbox::SandboxConfig {
            git: Some(
                crate::sandbox::GitGrantConfig::from_spec(
                    "origin=file:///workspace/remotes/origin.git",
                )
                .expect("spec")
                .expect("grant"),
            ),
            ..config
        };
        let manifest = git_manifest(serde_json::json!({
            "manifest_version": 1,
            "network": {"mode": "none"},
            "extensions": ["git-remotes"]
        }));
        let decision = authorize_manifest(&manifest, &config, temp.path()).expect("file grant");
        assert_eq!(decision.network, SandboxNetworkMode::None);
        assert!(decision.evidence.git.is_some());
    }

    #[test]
    fn validation_and_serialization_overhead_is_bounded() {
        let _guard = OVERHEAD_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("repo");
        std::fs::create_dir(&path).unwrap();
        let config = crate::sandbox::SandboxConfig::default();
        // Includes canonical filesystem checks in the end-to-end path.
        let start = Instant::now();
        for _ in 0..20 {
            authorize_manifest(&minimal(), &config, &path).unwrap();
        }
        let validation = start.elapsed() / 20;
        let decision = authorize_manifest(&minimal(), &config, &path).unwrap();
        let start = Instant::now();
        for _ in 0..1000 {
            serde_json::to_string(&decision.evidence).unwrap();
        }
        let serialization = start.elapsed() / 1000;
        eprintln!(
            "capability decision: validation {validation:?}, serialization {serialization:?}"
        );
        // A report is asserted below, rather than running an unattended performance test.
        assert!(validation.as_secs() < 120);
        assert!(serialization.as_secs() < 120);
    }
}
