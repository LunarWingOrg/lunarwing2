//! Package provenance for MCP servers.
//!
//! Records what package backs an MCP server and who supplied it: canonical
//! package identity, ecosystem, immutable version, publisher, and — for
//! host-local stdio and WASM servers — a SHA-256 digest of the executable or
//! component artifact.
//!
//! Provenance answers **what code is this and who supplied it**. It does not
//! prove that code is safe; containment (sandboxing) is a separate control.
//! Digest pinning closes the gap where WASM artifact checksums authenticate
//! WASM extensions but nothing authenticated the executable behind a stdio
//! command.
//!
//! Trust states:
//! - `Unverified` (default): no provenance requirements; legacy behavior.
//! - `Pinned`: activation re-hashes the selected runtime artifact and fails
//!   closed when it no longer matches the recorded digest.
//! - `Revoked`: activation is blocked for every transport.

use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use std::io::{Seek, Write};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::tools::mcp::config::ConfigError;

const MAX_MCP_WASM_COMPONENT_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum bytes accepted for an atomic pinned stdio executable snapshot.
pub const MAX_PINNED_STDIO_EXECUTABLE_BYTES: u64 = 128 * 1024 * 1024;

/// Package ecosystem a server package came from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageEcosystem {
    /// npm (Node.js) package.
    Npm,
    /// PyPI (Python) package.
    Pypi,
    /// Standalone binary or OS package.
    Binary,
    /// Ecosystem not recorded or not applicable.
    #[default]
    Unknown,
}

impl std::fmt::Display for PackageEcosystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageEcosystem::Npm => write!(f, "npm"),
            PackageEcosystem::Pypi => write!(f, "pypi"),
            PackageEcosystem::Binary => write!(f, "binary"),
            PackageEcosystem::Unknown => write!(f, "unknown"),
        }
    }
}

impl std::str::FromStr for PackageEcosystem {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "npm" => Ok(PackageEcosystem::Npm),
            "pypi" | "pip" => Ok(PackageEcosystem::Pypi),
            "binary" | "bin" => Ok(PackageEcosystem::Binary),
            "unknown" => Ok(PackageEcosystem::Unknown),
            other => Err(format!(
                "unknown package ecosystem '{other}' (expected npm, pypi, binary, or unknown)"
            )),
        }
    }
}

/// Trust state recorded for a package.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageTrust {
    /// No provenance requirements recorded; activation proceeds unverified.
    #[default]
    Unverified,
    /// Operator pinned the package identity and executable digest; activation
    /// verifies the digest and fails closed on mismatch.
    Pinned,
    /// Operator revoked this package; activation is blocked.
    Revoked,
}

impl std::fmt::Display for PackageTrust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageTrust::Unverified => write!(f, "unverified"),
            PackageTrust::Pinned => write!(f, "pinned"),
            PackageTrust::Revoked => write!(f, "revoked"),
        }
    }
}

/// Error returned by provenance recording or verification.
#[derive(Debug, thiserror::Error)]
pub enum ProvenanceError {
    #[error("package '{package}' is revoked and cannot be activated")]
    Revoked { package: String },
    #[error(
        "executable digest mismatch for '{command}' (resolved '{path}'): expected sha256 {expected}, got {actual}"
    )]
    DigestMismatch {
        command: String,
        path: String,
        expected: String,
        actual: String,
    },
    #[error("pinned package has no recorded runtime artifact digest")]
    MissingPin,
    #[error("component digest mismatch for '{path}': expected sha256 {expected}, got {actual}")]
    ComponentDigestMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("cannot resolve runtime artifact '{command}': {reason}")]
    Resolve { command: String, reason: String },
    #[error("cannot hash runtime artifact '{path}': {reason}")]
    Hash { path: String, reason: String },
    #[error("atomic execution of pinned stdio commands is unsupported on this platform")]
    AtomicExecutionUnsupported,
    #[error(
        "pinned stdio command '{path}' is a shebang script; use a pinned native launcher that authenticates its interpreter and dependencies, or unpin the command"
    )]
    PinnedScriptUnsupported { path: String },
}

/// Immutable executable snapshot whose descriptor must be used for exec.
pub(crate) struct VerifiedStdioExecutable {
    #[cfg(target_os = "linux")]
    file: std::fs::File,
}

#[cfg(target_os = "linux")]
impl VerifiedStdioExecutable {
    pub(crate) fn raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.file.as_raw_fd()
    }
}

/// Optional package identity fields supplied when pinning a server.
#[derive(Debug, Clone, Default)]
pub struct ProvenanceIdentity {
    /// Canonical package identity (e.g. `@modelcontextprotocol/server-filesystem`).
    pub package_id: Option<String>,
    /// Package ecosystem.
    pub ecosystem: PackageEcosystem,
    /// Immutable package version.
    pub version: Option<String>,
    /// Source registry or repository URL.
    pub source: Option<String>,
    /// Publisher name or key identifier.
    pub publisher: Option<String>,
}

/// Provenance record persisted alongside an MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPackageProvenance {
    /// Canonical package identity, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_id: Option<String>,

    /// Package ecosystem.
    #[serde(default)]
    pub ecosystem: PackageEcosystem,

    /// Immutable package version, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Source registry or repository URL, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,

    /// Publisher name or key identifier, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,

    /// Lowercase hex SHA-256 of the resolved stdio executable at pin time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_sha256: Option<String>,

    /// Informational path reported by the opened executable descriptor. Runtime
    /// identity comes only from the digest and retained descriptor, not this path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_command: Option<String>,

    /// Lowercase hex SHA-256 of the MCP-over-WASM component at pin time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_sha256: Option<String>,

    /// Canonical component path the digest was recorded for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_component: Option<String>,

    /// Trust state.
    #[serde(default)]
    pub trust: PackageTrust,

    /// When this record was written.
    pub recorded_at: DateTime<Utc>,
}

impl McpPackageProvenance {
    /// Create a minimal record carrying only a trust state (no identity or
    /// digest). Used when revoking a server that has no provenance yet.
    pub fn with_trust(trust: PackageTrust) -> Self {
        Self {
            package_id: None,
            ecosystem: PackageEcosystem::Unknown,
            version: None,
            source: None,
            publisher: None,
            command_sha256: None,
            resolved_command: None,
            component_sha256: None,
            resolved_component: None,
            trust,
            recorded_at: Utc::now(),
        }
    }

    /// Record provenance for a stdio command by resolving the executable that
    /// activation would run and hashing it. The record starts out `Pinned`.
    pub fn pin_for_command(
        command: &str,
        identity: ProvenanceIdentity,
    ) -> Result<Self, ProvenanceError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (command, identity);
            Err(ProvenanceError::AtomicExecutionUnsupported)
        }

        #[cfg(target_os = "linux")]
        {
            let path = resolve_command_path(command)?;
            let (_, digest, resolved) = open_and_hash_executable(command, &path)?;
            Ok(Self {
                package_id: identity.package_id,
                ecosystem: identity.ecosystem,
                version: identity.version,
                source: identity.source,
                publisher: identity.publisher,
                command_sha256: Some(digest),
                resolved_command: Some(resolved),
                component_sha256: None,
                resolved_component: None,
                trust: PackageTrust::Pinned,
                recorded_at: Utc::now(),
            })
        }
    }

    /// Pin a stateful MCP-over-WASM component by its current file digest.
    pub fn pin_for_component(
        component_path: &Path,
        identity: ProvenanceIdentity,
    ) -> Result<Self, ProvenanceError> {
        let digest = sha256_component_file(component_path)?;
        let resolved = component_path
            .canonicalize()
            .map_err(|error| ProvenanceError::Resolve {
                command: component_path.to_string_lossy().into_owned(),
                reason: error.to_string(),
            })?;
        Ok(Self {
            package_id: identity.package_id,
            ecosystem: identity.ecosystem,
            version: identity.version,
            source: identity.source,
            publisher: identity.publisher,
            command_sha256: None,
            resolved_command: None,
            component_sha256: Some(digest),
            resolved_component: Some(resolved.to_string_lossy().into_owned()),
            trust: PackageTrust::Pinned,
            recorded_at: Utc::now(),
        })
    }

    /// Validate persisted provenance fields.
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (field, value) in [
            ("package_id", &self.package_id),
            ("version", &self.version),
            ("source", &self.source),
            ("publisher", &self.publisher),
        ] {
            if let Some(value) = value {
                validate_metadata_field(field, value)?;
            }
        }
        for (field, digest) in [
            ("command_sha256", &self.command_sha256),
            ("component_sha256", &self.component_sha256),
        ] {
            let Some(digest) = digest else { continue };
            let valid = digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
            if !valid {
                return Err(ConfigError::InvalidConfig {
                    reason: format!(
                        "provenance {field} must be 64 lowercase hex characters (got '{digest}')"
                    ),
                });
            }
        }
        for (field, resolved) in [
            ("resolved_command", &self.resolved_command),
            ("resolved_component", &self.resolved_component),
        ] {
            if let Some(resolved) = resolved
                && (resolved.is_empty() || resolved.contains('\0'))
            {
                return Err(ConfigError::InvalidConfig {
                    reason: format!("provenance {field} cannot be empty or contain NUL bytes"),
                });
            }
        }
        let pin_count = usize::from(self.command_sha256.is_some())
            + usize::from(self.component_sha256.is_some());
        if self.trust == PackageTrust::Pinned && pin_count != 1 {
            return Err(ConfigError::InvalidConfig {
                reason: "pinned provenance requires exactly one runtime artifact digest"
                    .to_string(),
            });
        }
        Ok(())
    }

    /// Whether this record blocks activation outright.
    pub fn is_revoked(&self) -> bool {
        self.trust == PackageTrust::Revoked
    }

    /// Verify the trust state for any transport. Revoked packages fail closed.
    pub fn verify_not_revoked(&self) -> Result<(), ProvenanceError> {
        if self.is_revoked() {
            return Err(ProvenanceError::Revoked {
                package: self
                    .package_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
            });
        }
        Ok(())
    }

    /// Verify the stdio command that activation is about to run against this
    /// record. Unverified records pass; pinned records re-hash the resolved
    /// executable and fail closed on mismatch; revoked records always fail.
    pub fn verify_stdio_command(&self, command: &str) -> Result<(), ProvenanceError> {
        self.prepare_stdio_command(command).map(drop)
    }

    /// Verify and retain pinned executable object for atomic descriptor exec.
    pub(crate) fn prepare_stdio_command(
        &self,
        command: &str,
    ) -> Result<Option<VerifiedStdioExecutable>, ProvenanceError> {
        self.verify_not_revoked()?;
        if self.trust != PackageTrust::Pinned {
            return Ok(None);
        }
        #[cfg(not(target_os = "linux"))]
        return Err(ProvenanceError::AtomicExecutionUnsupported);

        #[cfg(target_os = "linux")]
        {
            let expected = self
                .command_sha256
                .as_deref()
                .ok_or(ProvenanceError::MissingPin)?;
            let path = resolve_command_path(command)?;
            let (file, actual, _) = open_and_hash_executable(command, &path)?;
            // Local digest comparison is not exposed to timing oracles; a plain
            // case-insensitive string compare is sufficient here.
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(ProvenanceError::DigestMismatch {
                    command: command.to_string(),
                    path: path.to_string_lossy().into_owned(),
                    expected: expected.to_string(),
                    actual,
                });
            }
            Ok(Some(VerifiedStdioExecutable { file }))
        }
    }

    /// Verify the WASM component that activation is about to instantiate.
    pub fn verify_wasm_component(&self, component_path: &Path) -> Result<(), ProvenanceError> {
        self.verify_not_revoked()?;
        if self.trust != PackageTrust::Pinned {
            return Ok(());
        }
        let expected = self
            .component_sha256
            .as_deref()
            .ok_or(ProvenanceError::MissingPin)?;
        let actual = sha256_component_file(component_path)?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(ProvenanceError::ComponentDigestMismatch {
                path: component_path.to_string_lossy().into_owned(),
                expected: expected.to_string(),
                actual,
            });
        }
        Ok(())
    }
}

fn sha256_component_file(path: &Path) -> Result<String, ProvenanceError> {
    let metadata = path
        .symlink_metadata()
        .map_err(|error| ProvenanceError::Resolve {
            command: path.to_string_lossy().into_owned(),
            reason: error.to_string(),
        })?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_MCP_WASM_COMPONENT_BYTES
    {
        return Err(ProvenanceError::Resolve {
            command: path.to_string_lossy().into_owned(),
            reason: format!(
                "component must be a regular, non-symlink file no larger than {MAX_MCP_WASM_COMPONENT_BYTES} bytes"
            ),
        });
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path).map_err(|error| ProvenanceError::Hash {
        path: path.to_string_lossy().into_owned(),
        reason: error.to_string(),
    })?;
    let mut hasher = Sha256::new();
    let copied = std::io::copy(
        &mut file.take(MAX_MCP_WASM_COMPONENT_BYTES + 1),
        &mut hasher,
    )
    .map_err(|error| ProvenanceError::Hash {
        path: path.to_string_lossy().into_owned(),
        reason: error.to_string(),
    })?;
    if copied > MAX_MCP_WASM_COMPONENT_BYTES {
        return Err(ProvenanceError::Hash {
            path: path.to_string_lossy().into_owned(),
            reason: "component grew beyond the size limit while hashing".to_string(),
        });
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_metadata_field(field: &'static str, value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(ConfigError::InvalidConfig {
            reason: format!("provenance {field} cannot be empty or contain control characters"),
        });
    }
    Ok(())
}

/// Resolve a stdio command to the executable path the OS would run:
/// explicit paths are used as-is; bare names are searched along `PATH`.
pub fn resolve_command_path(command: &str) -> Result<PathBuf, ProvenanceError> {
    let resolve_error = |reason: &str| ProvenanceError::Resolve {
        command: command.to_string(),
        reason: reason.to_string(),
    };
    if command.is_empty() || command.contains('\0') {
        return Err(resolve_error("command is empty or contains a NUL byte"));
    }

    if command.contains(['/', '\\']) {
        let path = PathBuf::from(command);
        if path.is_file() {
            return Ok(path);
        }
        return Err(resolve_error("explicit path is not an existing file"));
    }

    let Some(path_var) = std::env::var_os("PATH") else {
        return Err(resolve_error("PATH is not set"));
    };
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(command);
        if is_executable_file(&candidate) {
            return Ok(candidate);
        }
    }
    Err(resolve_error("no executable found on PATH"))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && std::fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// Compute the lowercase hex SHA-256 of a file, streaming its contents.
pub fn sha256_file(path: &Path) -> Result<String, ProvenanceError> {
    let hash_error = |reason: &str| ProvenanceError::Hash {
        path: path.to_string_lossy().into_owned(),
        reason: reason.to_string(),
    };
    let mut file = std::fs::File::open(path).map_err(|e| hash_error(&e.to_string()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| hash_error(&e.to_string()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(target_os = "linux")]
fn open_and_hash_executable(
    command: &str,
    path: &Path,
) -> Result<(std::fs::File, String, String), ProvenanceError> {
    let resolve_error = |reason: String| ProvenanceError::Resolve {
        command: command.to_string(),
        reason,
    };
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options.open(path).map_err(|error| {
        resolve_error(format!(
            "cannot open executable without following symlinks: {error}"
        ))
    })?;
    let before = file
        .metadata()
        .map_err(|error| resolve_error(format!("cannot inspect executable: {error}")))?;
    if !before.is_file() {
        return Err(resolve_error(
            "runtime artifact is not a regular file".to_string(),
        ));
    }
    if before.len() > MAX_PINNED_STDIO_EXECUTABLE_BYTES {
        return Err(resolve_error(format!(
            "runtime artifact is {} bytes; pinned stdio executables cannot exceed {MAX_PINNED_STDIO_EXECUTABLE_BYTES} bytes",
            before.len()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if before.permissions().mode() & 0o111 == 0 {
            return Err(resolve_error(
                "runtime artifact is not executable".to_string(),
            ));
        }
    }

    let mut verified_file = create_sealable_executable(command)?;
    let digest = copy_and_hash_executable(
        &mut file,
        &mut verified_file,
        path,
        MAX_PINNED_STDIO_EXECUTABLE_BYTES,
    )?;
    let after = file
        .metadata()
        .map_err(|error| resolve_error(format!("cannot re-inspect executable: {error}")))?;
    if !same_file_version(&before, &after) {
        return Err(resolve_error(
            "runtime artifact changed while being verified".to_string(),
        ));
    }
    seal_executable(&mut verified_file, path)?;
    let resolved = opened_descriptor_path(&file, path);
    Ok((verified_file, digest, resolved))
}

#[cfg(target_os = "linux")]
fn copy_and_hash_executable(
    reader: &mut impl Read,
    writer: &mut impl Write,
    path: &Path,
    limit: u64,
) -> Result<String, ProvenanceError> {
    let hash_error = |reason: String| ProvenanceError::Hash {
        path: path.to_string_lossy().into_owned(),
        reason,
    };
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    let mut prefix = [0_u8; 2];
    let mut prefix_len = 0;
    loop {
        let remaining = limit - total;
        let read_limit = buffer.len().min((remaining + 1) as usize);
        let bytes = reader
            .read(&mut buffer[..read_limit])
            .map_err(|error| hash_error(error.to_string()))?;
        if bytes == 0 {
            break;
        }
        let prefix_bytes = (prefix.len() - prefix_len).min(bytes);
        prefix[prefix_len..prefix_len + prefix_bytes].copy_from_slice(&buffer[..prefix_bytes]);
        prefix_len += prefix_bytes;
        if prefix_len == prefix.len() && prefix == *b"#!" {
            return Err(ProvenanceError::PinnedScriptUnsupported {
                path: path.to_string_lossy().into_owned(),
            });
        }
        total += bytes as u64;
        if total > limit {
            return Err(hash_error(format!(
                "runtime artifact grew beyond {limit} byte limit while hashing"
            )));
        }
        hasher.update(&buffer[..bytes]);
        writer
            .write_all(&buffer[..bytes])
            .map_err(|error| hash_error(format!("cannot snapshot verified executable: {error}")))?;
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(target_os = "linux")]
fn opened_descriptor_path(file: &std::fs::File, fallback: &Path) -> String {
    use std::os::fd::AsRawFd;

    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .unwrap_or_else(|_| fallback.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[cfg(target_os = "linux")]
fn create_sealable_executable(command: &str) -> Result<std::fs::File, ProvenanceError> {
    use std::os::fd::FromRawFd;

    let name = c"lunarwing-mcp-pinned";
    let fd = create_executable_memfd(|flags| {
        let fd = unsafe { libc::memfd_create(name.as_ptr(), flags) };
        if fd == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(fd)
        }
    })
    .map_err(|error| ProvenanceError::Resolve {
        command: command.to_string(),
        reason: executable_memfd_error(&error),
    })?;
    let fd = duplicate_descriptor_above_stdio(fd).map_err(|error| ProvenanceError::Resolve {
        command: command.to_string(),
        reason: format!("cannot secure executable snapshot descriptor: {error}"),
    })?;
    // SAFETY: helper returned a new owned descriptor and closed original.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn create_executable_memfd(
    mut create: impl FnMut(libc::c_uint) -> std::io::Result<std::os::fd::RawFd>,
) -> std::io::Result<std::os::fd::RawFd> {
    let base_flags = libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING;
    // Never retry without MFD_EXEC: EINVAL can also represent a policy or LSM denial.
    create(base_flags | libc::MFD_EXEC)
}

#[cfg(target_os = "linux")]
fn executable_memfd_error(error: &std::io::Error) -> String {
    format!(
        "cannot create executable sealed snapshot: required MFD_EXEC is unsupported by the running kernel or denied by vm.memfd_noexec, security policy, or LSM: {error}"
    )
}

#[cfg(target_os = "linux")]
fn duplicate_descriptor_above_stdio(fd: std::os::fd::RawFd) -> std::io::Result<std::os::fd::RawFd> {
    let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    let duplicate_error = std::io::Error::last_os_error();
    unsafe { libc::close(fd) };
    if duplicated == -1 {
        Err(duplicate_error)
    } else {
        Ok(duplicated)
    }
}

#[cfg(target_os = "linux")]
fn seal_executable(file: &mut std::fs::File, path: &Path) -> Result<(), ProvenanceError> {
    use std::os::fd::AsRawFd;

    let fd = file.as_raw_fd();
    if unsafe { libc::fchmod(fd, 0o500) } == -1 {
        return Err(ProvenanceError::Hash {
            path: path.to_string_lossy().into_owned(),
            reason: format!(
                "cannot mark verified snapshot executable: {}",
                std::io::Error::last_os_error()
            ),
        });
    }
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, seals) } == -1 {
        return Err(ProvenanceError::Hash {
            path: path.to_string_lossy().into_owned(),
            reason: format!(
                "cannot seal verified executable snapshot: {}",
                std::io::Error::last_os_error()
            ),
        });
    }
    file.rewind().map_err(|error| ProvenanceError::Hash {
        path: path.to_string_lossy().into_owned(),
        reason: format!("cannot rewind verified executable snapshot: {error}"),
    })
}

#[cfg(target_os = "linux")]
fn same_file_version(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ProvenanceIdentity {
        ProvenanceIdentity {
            package_id: Some("@example/test-server".to_string()),
            ecosystem: PackageEcosystem::Npm,
            version: Some("1.2.3".to_string()),
            source: Some("https://registry.npmjs.org/@example/test-server".to_string()),
            publisher: Some("example".to_string()),
        }
    }

    #[test]
    fn ecosystem_parses_known_values() {
        assert_eq!("npm".parse::<PackageEcosystem>(), Ok(PackageEcosystem::Npm));
        assert_eq!(
            "pip".parse::<PackageEcosystem>(),
            Ok(PackageEcosystem::Pypi)
        );
        assert_eq!(
            "BINARY".parse::<PackageEcosystem>(),
            Ok(PackageEcosystem::Binary)
        );
        assert!("maven".parse::<PackageEcosystem>().is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pin_records_digest_and_identity() {
        let provenance =
            McpPackageProvenance::pin_for_command("cat", identity()).expect("pin cat on PATH");
        assert_eq!(provenance.trust, PackageTrust::Pinned);
        assert_eq!(
            provenance.package_id.as_deref(),
            Some("@example/test-server")
        );
        let digest = provenance.command_sha256.expect("digest recorded");
        assert_eq!(digest.len(), 64);
        let resolved = provenance.resolved_command.expect("resolved path");
        assert!(resolved.contains("cat"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pinned_record_verifies_unchanged_command() {
        let provenance = McpPackageProvenance::pin_for_command("cat", identity()).expect("pin cat");
        provenance
            .verify_stdio_command("cat")
            .expect("unchanged executable verifies");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn digest_mismatch_fails_closed() {
        let mut provenance =
            McpPackageProvenance::pin_for_command("cat", identity()).expect("pin cat");
        provenance.command_sha256 = Some("0".repeat(64));
        let error = provenance
            .verify_stdio_command("cat")
            .expect_err("doctored digest must fail");
        assert!(matches!(error, ProvenanceError::DigestMismatch { .. }));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn revoked_record_blocks_any_transport() {
        let mut provenance =
            McpPackageProvenance::pin_for_command("cat", identity()).expect("pin cat");
        provenance.trust = PackageTrust::Revoked;
        let error = provenance
            .verify_stdio_command("cat")
            .expect_err("revoked package must fail");
        assert!(matches!(error, ProvenanceError::Revoked { .. }));
        assert!(provenance.verify_not_revoked().is_err());
    }

    #[test]
    fn unverified_record_passes_without_digest() {
        let provenance = McpPackageProvenance {
            package_id: None,
            ecosystem: PackageEcosystem::Unknown,
            version: None,
            source: None,
            publisher: None,
            command_sha256: None,
            resolved_command: None,
            component_sha256: None,
            resolved_component: None,
            trust: PackageTrust::Unverified,
            recorded_at: Utc::now(),
        };
        provenance
            .verify_stdio_command("definitely-not-a-real-command-lw")
            .expect("unverified record does not check digests");
        provenance.validate().expect("unverified record is valid");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn validate_rejects_bad_digest_and_pin_without_digest() {
        let mut provenance =
            McpPackageProvenance::pin_for_command("cat", identity()).expect("pin cat");

        provenance.command_sha256 = Some("zz".to_string());
        assert!(provenance.validate().is_err());

        provenance.command_sha256 = Some("A".repeat(64));
        assert!(
            provenance.validate().is_err(),
            "uppercase hex must be rejected"
        );

        provenance.command_sha256 = None;
        assert!(
            provenance.validate().is_err(),
            "pinned without digest must be rejected"
        );

        provenance.trust = PackageTrust::Unverified;
        provenance
            .validate()
            .expect("unverified without digest is valid");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn validate_rejects_empty_metadata() {
        let mut provenance =
            McpPackageProvenance::pin_for_command("cat", identity()).expect("pin cat");
        provenance.publisher = Some("   ".to_string());
        assert!(provenance.validate().is_err());
        provenance.publisher = Some("bad\npublisher".to_string());
        assert!(provenance.validate().is_err());
    }

    #[test]
    fn resolve_command_path_finds_path_entries_and_rejects_missing() {
        let cat = resolve_command_path("cat").expect("cat resolves via PATH");
        assert!(cat.is_file());

        let explicit =
            resolve_command_path(cat.to_str().expect("utf8 path")).expect("explicit path resolves");
        assert_eq!(explicit, cat);

        assert!(resolve_command_path("no-such-command-lw-test").is_err());
        assert!(resolve_command_path("").is_err());
        assert!(resolve_command_path("/definitely/missing/lw-test").is_err());
    }

    #[test]
    fn sha256_file_hashes_contents() {
        use sha2::{Digest, Sha256};
        let temp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(temp.path(), b"lunarwing provenance test").expect("write");
        let digest = sha256_file(temp.path()).expect("hash file");
        let mut hasher = Sha256::new();
        hasher.update(b"lunarwing provenance test");
        assert_eq!(digest, format!("{:x}", hasher.finalize()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn command_pin_rejects_symlink_and_non_executable_file() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = tempfile::tempdir().expect("temp dir");
        let executable = temp.path().join("server");
        let link = temp.path().join("server-link");
        std::fs::write(&executable, b"#!/bin/sh\nexit 0\n").expect("write executable");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .expect("chmod executable");
        symlink(&executable, &link).expect("create symlink");

        assert!(
            McpPackageProvenance::pin_for_command(link.to_str().expect("utf-8 path"), identity(),)
                .is_err()
        );

        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o644))
            .expect("remove execute bits");
        assert!(
            McpPackageProvenance::pin_for_command(
                executable.to_str().expect("utf-8 path"),
                identity(),
            )
            .is_err()
        );
        assert!(
            McpPackageProvenance::pin_for_command(
                temp.path().to_str().expect("utf-8 path"),
                identity(),
            )
            .is_err()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn command_pin_rejects_shebang_script_with_actionable_error() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::NamedTempFile::new().expect("temp script");
        std::fs::write(temp.path(), b"#!/bin/sh\nexit 0\n").expect("write script");
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod script");

        let error = McpPackageProvenance::pin_for_command(
            temp.path().to_str().expect("utf-8 path"),
            identity(),
        )
        .expect_err("pinned scripts must fail closed");
        assert!(matches!(
            error,
            ProvenanceError::PinnedScriptUnsupported { .. }
        ));
        let message = error.to_string();
        assert!(message.contains("native launcher"));
        assert!(message.contains("unpin"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn command_pin_rejects_oversized_executable_before_snapshot() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::NamedTempFile::new().expect("temp executable");
        temp.as_file()
            .set_len(MAX_PINNED_STDIO_EXECUTABLE_BYTES + 1)
            .expect("size sparse executable");
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod executable");

        let error = McpPackageProvenance::pin_for_command(
            temp.path().to_str().expect("utf-8 path"),
            identity(),
        )
        .expect_err("oversized executable must fail before snapshot");
        assert!(error.to_string().contains("cannot exceed"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn executable_stream_rejects_limit_plus_one_without_copying_extra_byte() {
        let mut input = std::io::Cursor::new(b"123456789");
        let mut snapshot = Vec::new();
        let error = copy_and_hash_executable(
            &mut input,
            &mut snapshot,
            Path::new("growing-executable"),
            8,
        )
        .expect_err("limit plus one must fail closed");

        assert!(error.to_string().contains("grew beyond 8 byte limit"));
        assert!(snapshot.len() <= 8);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn executable_stream_detects_shebang_across_one_byte_reads() {
        struct OneByteReader(std::io::Cursor<Vec<u8>>);

        impl Read for OneByteReader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let read_limit = buffer.len().min(1);
                self.0.read(&mut buffer[..read_limit])
            }
        }

        let mut input = OneByteReader(std::io::Cursor::new(b"#!/bin/sh\n".to_vec()));
        let mut snapshot = Vec::new();
        let error = copy_and_hash_executable(
            &mut input,
            &mut snapshot,
            Path::new("fragmented-script"),
            64,
        )
        .expect_err("fragmented shebang must fail closed");

        assert!(matches!(
            error,
            ProvenanceError::PinnedScriptUnsupported { .. }
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn executable_memfd_does_not_retry_without_exec_on_einval() {
        let mut calls = Vec::new();
        let error = create_executable_memfd(|flags| {
            calls.push(flags);
            Err(std::io::Error::from_raw_os_error(libc::EINVAL))
        })
        .expect_err("unsupported or denied MFD_EXEC must fail closed");

        assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
        assert_eq!(calls.len(), 1);
        assert_ne!(calls[0] & libc::MFD_EXEC, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn executable_memfd_does_not_retry_policy_denial() {
        let mut calls = 0;
        let error = create_executable_memfd(|_| {
            calls += 1;
            Err(std::io::Error::from_raw_os_error(libc::EACCES))
        })
        .expect_err("policy denial must fail closed");

        assert_eq!(calls, 1);
        let message = executable_memfd_error(&error);
        assert!(message.contains("vm.memfd_noexec, security policy, or LSM"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn executable_memfd_error_explains_old_kernel_or_policy_for_any_failure() {
        for errno in [libc::EINVAL, libc::EPERM, libc::EACCES, libc::ENOMEM] {
            let message = executable_memfd_error(&std::io::Error::from_raw_os_error(errno));
            assert!(message.contains("required MFD_EXEC is unsupported by the running kernel"));
            assert!(message.contains("security policy, or LSM"));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn executable_snapshot_descriptor_is_above_stdio_and_cloexec() {
        use std::os::fd::FromRawFd;

        let original =
            unsafe { libc::memfd_create(c"descriptor-test".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(original >= 0, "memfd_create failed");
        let duplicated = duplicate_descriptor_above_stdio(original).expect("duplicate descriptor");
        assert!(duplicated >= 3);
        assert_eq!(unsafe { libc::fcntl(original, libc::F_GETFD) }, -1);
        assert_ne!(
            unsafe { libc::fcntl(duplicated, libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        // SAFETY: helper returned owned duplicated descriptor.
        drop(unsafe { std::fs::File::from_raw_fd(duplicated) });
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn command_pin_rejects_unsupported_platform_before_resolution() {
        let error =
            McpPackageProvenance::pin_for_command("definitely-not-a-real-command-lw", identity())
                .expect_err("non-Linux pin must fail before persistence");
        assert!(matches!(error, ProvenanceError::AtomicExecutionUnsupported));
    }

    #[test]
    fn pinned_component_verifies_and_detects_replacement() {
        let temp = tempfile::NamedTempFile::new().expect("temp component");
        std::fs::write(temp.path(), b"component-v1").expect("write component");
        let provenance = McpPackageProvenance::pin_for_component(temp.path(), identity())
            .expect("pin component");
        provenance
            .verify_wasm_component(temp.path())
            .expect("unchanged component verifies");

        std::fs::write(temp.path(), b"component-v2").expect("replace component");
        assert!(matches!(
            provenance
                .verify_wasm_component(temp.path())
                .expect_err("replacement must fail closed"),
            ProvenanceError::ComponentDigestMismatch { .. }
        ));
    }

    #[test]
    fn component_pin_rejects_oversized_artifact() {
        let temp = tempfile::NamedTempFile::new().expect("temp component");
        temp.as_file()
            .set_len(MAX_MCP_WASM_COMPONENT_BYTES + 1)
            .expect("size sparse component");
        assert!(McpPackageProvenance::pin_for_component(temp.path(), identity()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn component_pin_rejects_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("temp dir");
        let component = temp.path().join("component.wasm");
        let link = temp.path().join("component-link.wasm");
        std::fs::write(&component, b"component").expect("write component");
        symlink(&component, &link).expect("create symlink");
        assert!(McpPackageProvenance::pin_for_component(&link, identity()).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn provenance_serde_roundtrip() {
        let provenance = McpPackageProvenance::pin_for_command("cat", identity()).expect("pin cat");
        let json = serde_json::to_string(&provenance).expect("serialize");
        let parsed: McpPackageProvenance = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.trust, PackageTrust::Pinned);
        assert_eq!(parsed.command_sha256, provenance.command_sha256);
        assert_eq!(parsed.ecosystem, PackageEcosystem::Npm);
    }
}
