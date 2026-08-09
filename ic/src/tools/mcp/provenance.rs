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

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::tools::mcp::config::ConfigError;

const MAX_MCP_WASM_COMPONENT_BYTES: u64 = 64 * 1024 * 1024;

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

    /// Absolute path of the executable the digest was recorded for.
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
        let path = resolve_command_path(command)?;
        let digest = sha256_file(&path)?;
        let resolved = path.canonicalize().unwrap_or(path);
        Ok(Self {
            package_id: identity.package_id,
            ecosystem: identity.ecosystem,
            version: identity.version,
            source: identity.source,
            publisher: identity.publisher,
            command_sha256: Some(digest),
            resolved_command: Some(resolved.to_string_lossy().into_owned()),
            component_sha256: None,
            resolved_component: None,
            trust: PackageTrust::Pinned,
            recorded_at: Utc::now(),
        })
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
        self.verify_not_revoked()?;
        if self.trust != PackageTrust::Pinned {
            return Ok(());
        }
        let expected = self
            .command_sha256
            .as_deref()
            .ok_or(ProvenanceError::MissingPin)?;
        let path = resolve_command_path(command)?;
        let actual = sha256_file(&path)?;
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
        Ok(())
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

    #[test]
    fn pinned_record_verifies_unchanged_command() {
        let provenance = McpPackageProvenance::pin_for_command("cat", identity()).expect("pin cat");
        provenance
            .verify_stdio_command("cat")
            .expect("unchanged executable verifies");
    }

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
