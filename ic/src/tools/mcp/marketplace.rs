//! Marketplace trust fabric for MCP server packages.
//!
//! This module implements the package-integrity layer that the MCP part 2
//! audit deferred: publisher signatures, dependency locks, capability/upgrade
//! diffs, and signed revocation distribution.
//!
//! Trust model:
//! - Publishers hold ed25519 signing keys. Operators pin the publishers they
//!   trust in a [`PublisherTrustStore`]; nothing is trusted by default.
//! - A [`SignedPackageManifest`] authenticates what a package is: identity,
//!   artifact digests, declared capabilities, and exact dependencies.
//! - A [`SignedDependencyLockfile`] pins the exact version and SHA-256 of
//!   every dependency; activation can require that a manifest's dependencies
//!   match the lockfile exactly, blocking silent dependency upgrades.
//! - A [`SignedRevocationList`] distributes revocations from a trusted
//!   marketplace key. Lists carry a strictly increasing sequence number so a
//!   stale or replayed list can never replace a newer one.
//!
//! Signatures cover the canonical JSON serialization of the payload struct
//! (structs with `BTreeMap`/`Vec` fields only, so serialization order is
//! deterministic). Provenance ([`crate::tools::mcp::provenance`]) still answers
//! "what bytes are about to run"; this layer answers "does a trusted publisher
//! stand behind these bytes and are they still unrevoked".

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::tools::mcp::provenance::PackageEcosystem;

/// Maximum accepted size of a serialized package manifest.
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
/// Maximum accepted size of a serialized dependency lockfile.
pub const MAX_LOCKFILE_BYTES: usize = 1024 * 1024;
/// Maximum accepted size of a serialized revocation list.
pub const MAX_REVOCATION_LIST_BYTES: usize = 4 * 1024 * 1024;
/// Maximum entries in one revocation list.
pub const MAX_REVOCATION_ENTRIES: usize = 10_000;
/// Maximum locked dependencies in one lockfile or manifest.
pub const MAX_LOCKED_DEPENDENCIES: usize = 1_024;
/// Maximum declared capabilities in one manifest.
pub const MAX_CAPABILITIES: usize = 4_096;
/// Timeout for one revocation-list fetch.
const REVOCATION_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// Maximum accepted size of a serialized marketplace catalog.
pub const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
/// Maximum signed manifests in one marketplace catalog.
pub const MAX_CATALOG_ENTRIES: usize = 2_048;
/// Artifact kind for a stdio executable digest inside a [`PackageManifest`].
pub const ARTIFACT_KIND_STDIO: &str = "stdio";
/// Artifact kind for a MCP-over-WASM component digest inside a [`PackageManifest`].
pub const ARTIFACT_KIND_WASM: &str = "wasm";

/// Error returned by marketplace signing, lock, or revocation operations.
#[derive(Debug, thiserror::Error)]
pub enum MarketplaceError {
    #[error("invalid marketplace data: {reason}")]
    InvalidData { reason: String },
    #[error("invalid ed25519 public key for publisher '{publisher}': {reason}")]
    InvalidPublicKey { publisher: String, reason: String },
    #[error("publisher '{publisher}' is not in the trust store")]
    UntrustedPublisher { publisher: String },
    #[error("signature verification failed for publisher '{publisher}'")]
    BadSignature { publisher: String },
    #[error(
        "dependency '{package}' is not pinned by the lockfile (manifest requests version {version})"
    )]
    DependencyNotLocked { package: String, version: String },
    #[error(
        "dependency '{package}' lock mismatch: manifest has {manifest_version} sha256 {manifest_sha256}, lockfile pins {locked_version} sha256 {locked_sha256}"
    )]
    DependencyLockMismatch {
        package: String,
        manifest_version: String,
        manifest_sha256: String,
        locked_version: String,
        locked_sha256: String,
    },
    #[error(
        "revocation list sequence {sequence} is not newer than the current sequence {current}; refusing rollback"
    )]
    RevocationRollback { sequence: u64, current: u64 },
    #[error("revocation fetch from '{url}' failed: {reason}")]
    RevocationFetch { url: String, reason: String },
    #[error("revocation cache IO error at '{path}': {reason}")]
    RevocationCacheIo { path: String, reason: String },
    #[error("package '{package_id}' is not in the marketplace catalog")]
    PackageNotInCatalog { package_id: String },
    #[error("catalog fetch from '{url}' failed: {reason}")]
    CatalogFetch { url: String, reason: String },
    #[error("package '{package_id}' manifest has no '{kind}' artifact digest")]
    ArtifactKindMissing { package_id: String, kind: String },
    #[error(
        "package '{package_id}' {kind} artifact digest mismatch: catalog pins {expected}, local artifact hashes {actual}"
    )]
    ArtifactDigestMismatch {
        package_id: String,
        kind: String,
        expected: String,
        actual: String,
    },
}

/// Validate a publisher or package identifier: printable, bounded, no controls.
fn validate_identifier(field: &'static str, value: &str) -> Result<(), MarketplaceError> {
    let invalid = |reason: &str| MarketplaceError::InvalidData {
        reason: format!("{field} {reason}"),
    };
    if value.is_empty() || value.len() > 256 {
        return Err(invalid("must be 1-256 characters"));
    }
    if value.bytes().any(|b| b < 0x21 || b == 0x7f) {
        return Err(invalid("cannot contain whitespace or control characters"));
    }
    Ok(())
}

/// Validate a lowercase hex SHA-256 digest string.
fn validate_sha256(field: &'static str, value: &str) -> Result<(), MarketplaceError> {
    let valid = value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
    if !valid {
        return Err(MarketplaceError::InvalidData {
            reason: format!("{field} must be 64 lowercase hex characters"),
        });
    }
    Ok(())
}

fn validate_version(value: &str) -> Result<(), MarketplaceError> {
    validate_identifier("version", value)
}

/// Canonical signed payload bytes: JSON of a struct using only ordered fields.
///
/// Returns an error rather than panicking so untrusted or future schema shapes
/// cannot abort the daemon; callers already return `MarketplaceError`.
fn canonical_payload<T: Serialize>(payload: &T) -> Result<Vec<u8>, MarketplaceError> {
    serde_json::to_vec(payload).map_err(|error| MarketplaceError::InvalidData {
        reason: format!("payload is not serializable: {error}"),
    })
}

fn decode_public_key(publisher: &str, key_hex: &str) -> Result<VerifyingKey, MarketplaceError> {
    let invalid = |reason: String| MarketplaceError::InvalidPublicKey {
        publisher: publisher.to_string(),
        reason,
    };
    let bytes = hex::decode(key_hex).map_err(|error| invalid(error.to_string()))?;
    VerifyingKey::try_from(bytes.as_slice()).map_err(|error| invalid(error.to_string()))
}

fn sign_hex<T: Serialize>(payload: &T, key: &SigningKey) -> String {
    // Signing accepts a serializable payload; the canonical-bytes error path
    // is unreachable for the payload shapes this module signs, so failure
    // still produces a distinct zero-value signature rather than a panic.
    match canonical_payload(payload) {
        Ok(bytes) => hex::encode(key.sign(&bytes).to_bytes()),
        Err(_) => "00".repeat(64),
    }
}

fn verify_hex<T: Serialize>(
    payload: &T,
    publisher: &str,
    signature_hex: &str,
    key: &VerifyingKey,
) -> Result<(), MarketplaceError> {
    let signature_bytes =
        hex::decode(signature_hex).map_err(|_| MarketplaceError::BadSignature {
            publisher: publisher.to_string(),
        })?;
    let signature =
        Signature::from_slice(&signature_bytes).map_err(|_| MarketplaceError::BadSignature {
            publisher: publisher.to_string(),
        })?;
    let canonical = canonical_payload(payload)?;
    key.verify_strict(&canonical, &signature)
        .map_err(|_| MarketplaceError::BadSignature {
            publisher: publisher.to_string(),
        })
}

/// A publisher's ed25519 signing key (marketplace/publisher side).
pub struct PublisherSigningKey(SigningKey);

impl PublisherSigningKey {
    /// Generate a fresh signing key from OS randomness.
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut bytes = [0_u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self(SigningKey::from_bytes(&bytes))
    }

    /// Reconstruct from raw 32-byte secret key material.
    pub fn from_bytes(bytes: &[u8; 32]) -> Self {
        Self(SigningKey::from_bytes(bytes))
    }

    /// Lowercase hex encoding of the secret key (handle as a secret).
    pub fn to_hex(&self) -> String {
        hex::encode(self.0.to_bytes())
    }

    /// Lowercase hex encoding of the matching public key.
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.0.verifying_key().to_bytes())
    }
}

/// Trusted publisher public keys. Empty store trusts nothing.
#[derive(Debug, Default)]
pub struct PublisherTrustStore {
    keys: HashMap<String, VerifyingKey>,
}

/// Serializable form of a [`PublisherTrustStore`].
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PublisherTrustStoreFile {
    /// Publisher identifier -> lowercase hex ed25519 public key.
    #[serde(default)]
    pub publishers: BTreeMap<String, String>,
}

impl PublisherTrustStore {
    /// Add or replace a trusted publisher key (hex-encoded ed25519 public key).
    pub fn insert_hex(
        &mut self,
        publisher: &str,
        public_key_hex: &str,
    ) -> Result<(), MarketplaceError> {
        validate_identifier("publisher", publisher)?;
        let key = decode_public_key(publisher, public_key_hex)?;
        self.keys.insert(publisher.to_string(), key);
        Ok(())
    }

    /// Remove a publisher from the trust store.
    pub fn remove(&mut self, publisher: &str) -> bool {
        self.keys.remove(publisher).is_some()
    }

    /// Whether a publisher is trusted.
    pub fn contains(&self, publisher: &str) -> bool {
        self.keys.contains_key(publisher)
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    fn key_for(&self, publisher: &str) -> Result<&VerifyingKey, MarketplaceError> {
        self.keys
            .get(publisher)
            .ok_or_else(|| MarketplaceError::UntrustedPublisher {
                publisher: publisher.to_string(),
            })
    }

    /// Serialize to the JSON-friendly file form.
    pub fn to_file(&self) -> PublisherTrustStoreFile {
        PublisherTrustStoreFile {
            publishers: self
                .keys
                .iter()
                .map(|(name, key)| (name.clone(), hex::encode(key.to_bytes())))
                .collect(),
        }
    }

    /// Load from the JSON-friendly file form. Invalid keys fail the whole load.
    pub fn from_file(file: PublisherTrustStoreFile) -> Result<Self, MarketplaceError> {
        let mut store = Self::default();
        for (publisher, key_hex) in file.publishers {
            store.insert_hex(&publisher, &key_hex)?;
        }
        Ok(store)
    }
}

/// One dependency pinned to an exact version and artifact digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedDependency {
    /// Canonical package identity of the dependency.
    pub package_id: String,
    /// Exact immutable version.
    pub version: String,
    /// Lowercase hex SHA-256 of the dependency artifact.
    pub sha256: String,
}

impl LockedDependency {
    pub fn validate(&self) -> Result<(), MarketplaceError> {
        validate_identifier("dependency package_id", &self.package_id)?;
        validate_version(&self.version)?;
        validate_sha256("dependency sha256", &self.sha256)
    }
}

/// Package manifest signed by a publisher: identity, artifacts, dependencies,
/// and declared tool capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageManifest {
    /// Manifest schema version; currently 1.
    pub schema_version: u32,
    /// Canonical package identity (e.g. `@modelcontextprotocol/server-filesystem`).
    pub package_id: String,
    /// Package ecosystem.
    pub ecosystem: PackageEcosystem,
    /// Immutable package version.
    pub version: String,
    /// Publisher identifier (must exist in the trust store to verify).
    pub publisher: String,
    /// Artifact kind (`stdio`, `wasm`, ...) -> lowercase hex SHA-256.
    #[serde(default)]
    pub artifacts: BTreeMap<String, String>,
    /// Exact dependencies of this package.
    #[serde(default)]
    pub dependencies: Vec<LockedDependency>,
    /// Declared tool names/capabilities this package provides.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// When the manifest was issued.
    pub issued_at: DateTime<Utc>,
}

impl PackageManifest {
    pub fn validate(&self) -> Result<(), MarketplaceError> {
        if self.schema_version != 1 {
            return Err(MarketplaceError::InvalidData {
                reason: format!(
                    "unsupported manifest schema_version {}",
                    self.schema_version
                ),
            });
        }
        validate_identifier("package_id", &self.package_id)?;
        validate_version(&self.version)?;
        validate_identifier("publisher", &self.publisher)?;
        if self.dependencies.len() > MAX_LOCKED_DEPENDENCIES {
            return Err(MarketplaceError::InvalidData {
                reason: format!("manifest exceeds {MAX_LOCKED_DEPENDENCIES} dependencies"),
            });
        }
        if self.capabilities.len() > MAX_CAPABILITIES {
            return Err(MarketplaceError::InvalidData {
                reason: format!("manifest exceeds {MAX_CAPABILITIES} capabilities"),
            });
        }
        for (kind, digest) in &self.artifacts {
            validate_identifier("artifact kind", kind)?;
            validate_sha256("artifact sha256", digest)?;
        }
        for dependency in &self.dependencies {
            dependency.validate()?;
        }
        for capability in &self.capabilities {
            validate_identifier("capability", capability)?;
        }
        Ok(())
    }
}

/// A [`PackageManifest`] with a detached publisher signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedPackageManifest {
    pub manifest: PackageManifest,
    /// Lowercase hex ed25519 signature over the canonical manifest JSON.
    pub signature: String,
}

impl SignedPackageManifest {
    /// Sign a manifest with a publisher key.
    pub fn sign(manifest: PackageManifest, key: &PublisherSigningKey) -> Self {
        let signature = sign_hex(&manifest, &key.0);
        Self {
            manifest,
            signature,
        }
    }

    /// Verify structural validity and the publisher signature.
    pub fn verify(&self, store: &PublisherTrustStore) -> Result<(), MarketplaceError> {
        self.manifest.validate()?;
        let key = store.key_for(&self.manifest.publisher)?;
        verify_hex(
            &self.manifest,
            &self.manifest.publisher,
            &self.signature,
            key,
        )
    }

    /// Parse a bounded JSON document and verify it.
    pub fn from_json(bytes: &[u8], store: &PublisherTrustStore) -> Result<Self, MarketplaceError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(MarketplaceError::InvalidData {
                reason: format!("manifest exceeds {MAX_MANIFEST_BYTES} bytes"),
            });
        }
        let signed: Self =
            serde_json::from_slice(bytes).map_err(|error| MarketplaceError::InvalidData {
                reason: format!("manifest JSON: {error}"),
            })?;
        signed.verify(store)?;
        Ok(signed)
    }
}

/// A marketplace catalog: one verified [`SignedPackageManifest`] per package.
///
/// The catalog is the signed distribution point for the CLI install/upgrade
/// flow. Every embedded manifest must carry a valid signature from a
/// publisher in the operator's trust store before the catalog parses, so an
/// untrusted catalog file fails closed. Revocation is checked separately
/// against the [`RevocationStore`] at install/upgrade time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceCatalog {
    /// Catalog schema version; currently 1.
    pub schema_version: u32,
    /// Signed manifests, one per package identity.
    #[serde(default)]
    pub packages: Vec<SignedPackageManifest>,
}

impl MarketplaceCatalog {
    /// Build a catalog from already-signed manifests.
    pub fn new(packages: Vec<SignedPackageManifest>) -> Self {
        Self {
            schema_version: 1,
            packages,
        }
    }

    /// Structural validation independent of signatures.
    pub fn validate(&self) -> Result<(), MarketplaceError> {
        if self.schema_version != 1 {
            return Err(MarketplaceError::InvalidData {
                reason: format!("unsupported catalog schema_version {}", self.schema_version),
            });
        }
        if self.packages.len() > MAX_CATALOG_ENTRIES {
            return Err(MarketplaceError::InvalidData {
                reason: format!("catalog exceeds {MAX_CATALOG_ENTRIES} packages"),
            });
        }
        let mut seen = BTreeSet::new();
        for signed in &self.packages {
            signed.manifest.validate()?;
            if !seen.insert(signed.manifest.package_id.clone()) {
                return Err(MarketplaceError::InvalidData {
                    reason: format!(
                        "catalog contains duplicate package '{}'",
                        signed.manifest.package_id
                    ),
                });
            }
        }
        Ok(())
    }

    /// Parse a bounded JSON document, validating structure and verifying every
    /// embedded publisher signature against the trust store.
    pub fn from_json(bytes: &[u8], store: &PublisherTrustStore) -> Result<Self, MarketplaceError> {
        if bytes.len() > MAX_CATALOG_BYTES {
            return Err(MarketplaceError::InvalidData {
                reason: format!("catalog exceeds {MAX_CATALOG_BYTES} bytes"),
            });
        }
        let catalog: Self =
            serde_json::from_slice(bytes).map_err(|error| MarketplaceError::InvalidData {
                reason: format!("catalog JSON: {error}"),
            })?;
        catalog.verify(store)?;
        Ok(catalog)
    }

    /// Verify structural validity and every embedded manifest signature.
    pub fn verify(&self, store: &PublisherTrustStore) -> Result<(), MarketplaceError> {
        self.validate()?;
        for signed in &self.packages {
            signed.verify(store)?;
        }
        Ok(())
    }

    /// Number of packages in the catalog.
    pub fn len(&self) -> usize {
        self.packages.len()
    }

    /// Whether the catalog holds no packages.
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// The verified manifest for one canonical package identity.
    pub fn find(&self, package_id: &str) -> Option<&PackageManifest> {
        self.packages
            .iter()
            .map(|signed| &signed.manifest)
            .find(|manifest| manifest.package_id == package_id)
    }

    /// Require a package to exist, failing closed with a catalog-specific error.
    pub fn require(&self, package_id: &str) -> Result<&PackageManifest, MarketplaceError> {
        self.find(package_id)
            .ok_or_else(|| MarketplaceError::PackageNotInCatalog {
                package_id: package_id.to_string(),
            })
    }

    /// Fetch a catalog over HTTPS (localhost HTTP allowed for harnesses) and
    /// verify it against the trust store.
    pub async fn fetch(
        url: &str,
        store: &PublisherTrustStore,
        client: &reqwest::Client,
    ) -> Result<Self, MarketplaceError> {
        if !url.to_lowercase().starts_with("https://")
            && !crate::tools::mcp::config::is_localhost_url(url)
        {
            return Err(MarketplaceError::CatalogFetch {
                url: url.to_string(),
                reason: "catalogs must be fetched over HTTPS".to_string(),
            });
        }
        let response = client
            .get(url)
            .timeout(REVOCATION_FETCH_TIMEOUT)
            .send()
            .await
            .map_err(|error| MarketplaceError::CatalogFetch {
                url: url.to_string(),
                reason: error.to_string(),
            })?;
        if !response.status().is_success() {
            return Err(MarketplaceError::CatalogFetch {
                url: url.to_string(),
                reason: format!("HTTP {}", response.status()),
            });
        }
        let bytes = read_bounded_body(response, MAX_CATALOG_BYTES, |reason| {
            MarketplaceError::CatalogFetch {
                url: url.to_string(),
                reason,
            }
        })
        .await?;
        Self::from_json(&bytes, store)
    }
}

/// Dependency lockfile: exact version+digest pins for one root package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyLockfile {
    /// Lockfile schema version; currently 1.
    pub schema_version: u32,
    /// Root package these locks belong to.
    pub package_id: String,
    /// Exact dependency pins.
    #[serde(default)]
    pub locks: Vec<LockedDependency>,
    /// When the lockfile was issued.
    pub issued_at: DateTime<Utc>,
}

impl DependencyLockfile {
    pub fn validate(&self) -> Result<(), MarketplaceError> {
        if self.schema_version != 1 {
            return Err(MarketplaceError::InvalidData {
                reason: format!(
                    "unsupported lockfile schema_version {}",
                    self.schema_version
                ),
            });
        }
        validate_identifier("package_id", &self.package_id)?;
        if self.locks.len() > MAX_LOCKED_DEPENDENCIES {
            return Err(MarketplaceError::InvalidData {
                reason: format!("lockfile exceeds {MAX_LOCKED_DEPENDENCIES} locks"),
            });
        }
        let mut seen = BTreeSet::new();
        for lock in &self.locks {
            lock.validate()?;
            if !seen.insert(lock.package_id.clone()) {
                return Err(MarketplaceError::InvalidData {
                    reason: format!("duplicate lock for dependency '{}'", lock.package_id),
                });
            }
        }
        Ok(())
    }

    fn lock_for(&self, package_id: &str) -> Option<&LockedDependency> {
        self.locks.iter().find(|lock| lock.package_id == package_id)
    }
}

/// A [`DependencyLockfile`] signed by a publisher or marketplace key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedDependencyLockfile {
    pub lockfile: DependencyLockfile,
    /// Publisher identifier that signed the lockfile.
    pub publisher: String,
    /// Lowercase hex ed25519 signature over the canonical lockfile JSON.
    pub signature: String,
}

impl SignedDependencyLockfile {
    /// Sign a lockfile.
    pub fn sign(lockfile: DependencyLockfile, publisher: &str, key: &PublisherSigningKey) -> Self {
        let signature = sign_hex(&lockfile, &key.0);
        Self {
            lockfile,
            publisher: publisher.to_string(),
            signature,
        }
    }

    /// Verify structural validity and the signature against the trust store.
    pub fn verify(&self, store: &PublisherTrustStore) -> Result<(), MarketplaceError> {
        self.lockfile.validate()?;
        validate_identifier("publisher", &self.publisher)?;
        let key = store.key_for(&self.publisher)?;
        verify_hex(&self.lockfile, &self.publisher, &self.signature, key)
    }

    /// Parse a bounded JSON document and verify it.
    pub fn from_json(bytes: &[u8], store: &PublisherTrustStore) -> Result<Self, MarketplaceError> {
        if bytes.len() > MAX_LOCKFILE_BYTES {
            return Err(MarketplaceError::InvalidData {
                reason: format!("lockfile exceeds {MAX_LOCKFILE_BYTES} bytes"),
            });
        }
        let signed: Self =
            serde_json::from_slice(bytes).map_err(|error| MarketplaceError::InvalidData {
                reason: format!("lockfile JSON: {error}"),
            })?;
        signed.verify(store)?;
        Ok(signed)
    }

    /// Require that every dependency declared by a manifest is pinned by this
    /// lockfile with the exact same version and digest. Fails closed on any
    /// missing or divergent dependency.
    pub fn verify_manifest_dependencies(
        &self,
        manifest: &PackageManifest,
    ) -> Result<(), MarketplaceError> {
        for dependency in &manifest.dependencies {
            let Some(lock) = self.lockfile.lock_for(&dependency.package_id) else {
                return Err(MarketplaceError::DependencyNotLocked {
                    package: dependency.package_id.clone(),
                    version: dependency.version.clone(),
                });
            };
            if lock.version != dependency.version || lock.sha256 != dependency.sha256 {
                return Err(MarketplaceError::DependencyLockMismatch {
                    package: dependency.package_id.clone(),
                    manifest_version: dependency.version.clone(),
                    manifest_sha256: dependency.sha256.clone(),
                    locked_version: lock.version.clone(),
                    locked_sha256: lock.sha256.clone(),
                });
            }
        }
        Ok(())
    }

    /// Check one resolved dependency artifact against its lock.
    pub fn verify_artifact(
        &self,
        package_id: &str,
        version: &str,
        sha256: &str,
    ) -> Result<(), MarketplaceError> {
        let Some(lock) = self.lockfile.lock_for(package_id) else {
            return Err(MarketplaceError::DependencyNotLocked {
                package: package_id.to_string(),
                version: version.to_string(),
            });
        };
        if lock.version != version || !lock.sha256.eq_ignore_ascii_case(sha256) {
            return Err(MarketplaceError::DependencyLockMismatch {
                package: package_id.to_string(),
                manifest_version: version.to_string(),
                manifest_sha256: sha256.to_string(),
                locked_version: lock.version.clone(),
                locked_sha256: lock.sha256.clone(),
            });
        }
        Ok(())
    }
}

/// What changed between two versions of a signed package manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestDiff {
    pub added_capabilities: Vec<String>,
    pub removed_capabilities: Vec<String>,
    pub added_dependencies: Vec<String>,
    pub removed_dependencies: Vec<String>,
    /// Dependencies whose pinned version or digest changed.
    pub changed_dependencies: Vec<String>,
    /// Artifact kinds whose digest changed (upgrade of shipped bytes).
    pub changed_artifacts: Vec<String>,
}

impl ManifestDiff {
    /// Whether the upgrade only added capabilities without changing shipped
    /// artifacts or dependencies.
    pub fn is_additive_only(&self) -> bool {
        !self.added_capabilities.is_empty()
            && self.removed_capabilities.is_empty()
            && self.added_dependencies.is_empty()
            && self.removed_dependencies.is_empty()
            && self.changed_dependencies.is_empty()
            && self.changed_artifacts.is_empty()
    }
}

/// Diff two manifests for the same package (operator review of an upgrade).
pub fn diff_manifests(old: &PackageManifest, new: &PackageManifest) -> ManifestDiff {
    let old_caps: BTreeSet<&String> = old.capabilities.iter().collect();
    let new_caps: BTreeSet<&String> = new.capabilities.iter().collect();
    let old_deps: BTreeMap<&str, &LockedDependency> = old
        .dependencies
        .iter()
        .map(|dep| (dep.package_id.as_str(), dep))
        .collect();
    let new_deps: BTreeMap<&str, &LockedDependency> = new
        .dependencies
        .iter()
        .map(|dep| (dep.package_id.as_str(), dep))
        .collect();

    ManifestDiff {
        added_capabilities: new_caps
            .difference(&old_caps)
            .map(|cap| (*cap).clone())
            .collect(),
        removed_capabilities: old_caps
            .difference(&new_caps)
            .map(|cap| (*cap).clone())
            .collect(),
        added_dependencies: new_deps
            .keys()
            .filter(|name| !old_deps.contains_key(**name))
            .map(|name| (*name).to_string())
            .collect(),
        removed_dependencies: old_deps
            .keys()
            .filter(|name| !new_deps.contains_key(**name))
            .map(|name| (*name).to_string())
            .collect(),
        changed_dependencies: new_deps
            .iter()
            .filter(|(name, dep)| {
                old_deps
                    .get(*name)
                    .is_some_and(|old| old.version != dep.version || old.sha256 != dep.sha256)
            })
            .map(|(name, _)| (*name).to_string())
            .collect(),
        changed_artifacts: new
            .artifacts
            .iter()
            .filter(|(kind, digest)| old.artifacts.get(*kind) != Some(digest))
            .map(|(kind, _)| kind.clone())
            .collect(),
    }
}

/// One revoked package version or artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationEntry {
    /// Canonical package identity being revoked.
    pub package_id: String,
    /// Specific revoked version; `None` revokes every version of the package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Specific revoked artifact digest; combined with `version` when both set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Human-readable reason (advisory).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl RevocationEntry {
    pub fn validate(&self) -> Result<(), MarketplaceError> {
        validate_identifier("revoked package_id", &self.package_id)?;
        if let Some(version) = &self.version {
            validate_version(version)?;
        }
        if let Some(digest) = &self.sha256 {
            validate_sha256("revoked sha256", digest)?;
        }
        if self.version.is_none() && self.sha256.is_none() {
            // Whole-package revocation is allowed but must be explicit: package
            // identity with no version/digest revokes every artifact.
        }
        if let Some(reason) = &self.reason
            && (reason.len() > 512 || reason.chars().any(char::is_control))
        {
            return Err(MarketplaceError::InvalidData {
                reason: "revocation reason must be <= 512 chars without controls".to_string(),
            });
        }
        Ok(())
    }

    fn matches(&self, package_id: &str, version: Option<&str>, sha256: Option<&str>) -> bool {
        if self.package_id != package_id {
            return false;
        }
        match (&self.version, version) {
            (Some(expected), Some(actual)) if expected != actual => return false,
            (Some(_), None) => return false,
            _ => {}
        }
        match (&self.sha256, sha256) {
            (Some(expected), Some(actual)) if !expected.eq_ignore_ascii_case(actual) => {
                return false;
            }
            (Some(_), None) => return false,
            _ => {}
        }
        true
    }
}

/// Signed, ordered revocation list distributed by a marketplace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevocationList {
    /// List schema version; currently 1.
    pub schema_version: u32,
    /// Strictly increasing sequence; a newer list replaces any older one.
    pub sequence: u64,
    /// When the list was issued.
    pub issued_at: DateTime<Utc>,
    /// Revoked packages/artifacts.
    #[serde(default)]
    pub entries: Vec<RevocationEntry>,
}

impl RevocationList {
    pub fn validate(&self) -> Result<(), MarketplaceError> {
        if self.schema_version != 1 {
            return Err(MarketplaceError::InvalidData {
                reason: format!(
                    "unsupported revocation list schema_version {}",
                    self.schema_version
                ),
            });
        }
        if self.entries.len() > MAX_REVOCATION_ENTRIES {
            return Err(MarketplaceError::InvalidData {
                reason: format!("revocation list exceeds {MAX_REVOCATION_ENTRIES} entries"),
            });
        }
        for entry in &self.entries {
            entry.validate()?;
        }
        Ok(())
    }

    /// Whether a package version/artifact is revoked by this list.
    pub fn is_revoked(
        &self,
        package_id: &str,
        version: Option<&str>,
        sha256: Option<&str>,
    ) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.matches(package_id, version, sha256))
    }
}

/// A [`RevocationList`] signed by a marketplace key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedRevocationList {
    pub list: RevocationList,
    /// Publisher identifier that signed the list.
    pub publisher: String,
    /// Lowercase hex ed25519 signature over the canonical list JSON.
    pub signature: String,
}

impl SignedRevocationList {
    /// Sign a revocation list.
    pub fn sign(list: RevocationList, publisher: &str, key: &PublisherSigningKey) -> Self {
        let signature = sign_hex(&list, &key.0);
        Self {
            list,
            publisher: publisher.to_string(),
            signature,
        }
    }

    /// Verify structural validity and the signature against the trust store.
    pub fn verify(&self, store: &PublisherTrustStore) -> Result<(), MarketplaceError> {
        self.list.validate()?;
        validate_identifier("publisher", &self.publisher)?;
        let key = store.key_for(&self.publisher)?;
        verify_hex(&self.list, &self.publisher, &self.signature, key)
    }

    /// Parse a bounded JSON document and verify it against the trust store.
    pub fn from_json(bytes: &[u8], store: &PublisherTrustStore) -> Result<Self, MarketplaceError> {
        if bytes.len() > MAX_REVOCATION_LIST_BYTES {
            return Err(MarketplaceError::InvalidData {
                reason: format!("revocation list exceeds {MAX_REVOCATION_LIST_BYTES} bytes"),
            });
        }
        let signed: Self =
            serde_json::from_slice(bytes).map_err(|error| MarketplaceError::InvalidData {
                reason: format!("revocation list JSON: {error}"),
            })?;
        signed.verify(store)?;
        Ok(signed)
    }
}

/// Revocation distribution point: keeps the newest verified revocation list,
/// persists it to a local cache, and can pull updates over HTTPS.
///
/// Only one trusted publisher key signs lists for a store, so a compromised
/// transport or mirror cannot substitute a list signed by anyone else, and
/// the monotonic sequence blocks rollback to an older list.
pub struct RevocationStore {
    cache_path: PathBuf,
    publisher: String,
    key: VerifyingKey,
    /// Newest verified list; `None` means nothing has been received yet and
    /// nothing is revoked.
    current: RwLock<Option<SignedRevocationList>>,
}

impl RevocationStore {
    /// Open a store, loading and verifying any cached list. A missing cache
    /// starts empty; a corrupt or invalid cache fails closed.
    pub fn open(
        cache_path: PathBuf,
        publisher: &str,
        public_key_hex: &str,
    ) -> Result<Self, MarketplaceError> {
        validate_identifier("publisher", publisher)?;
        let key = decode_public_key(publisher, public_key_hex)?;
        let current = match std::fs::read(&cache_path) {
            Ok(bytes) => Some(Self::verify_with_key(&bytes, publisher, &key)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(MarketplaceError::RevocationCacheIo {
                    path: cache_path.to_string_lossy().into_owned(),
                    reason: error.to_string(),
                });
            }
        };
        Ok(Self {
            cache_path,
            publisher: publisher.to_string(),
            key,
            current: RwLock::new(current),
        })
    }

    fn verify_with_key(
        bytes: &[u8],
        publisher: &str,
        key: &VerifyingKey,
    ) -> Result<SignedRevocationList, MarketplaceError> {
        if bytes.len() > MAX_REVOCATION_LIST_BYTES {
            return Err(MarketplaceError::InvalidData {
                reason: format!("revocation list exceeds {MAX_REVOCATION_LIST_BYTES} bytes"),
            });
        }
        let signed: SignedRevocationList =
            serde_json::from_slice(bytes).map_err(|error| MarketplaceError::InvalidData {
                reason: format!("revocation list JSON: {error}"),
            })?;
        signed.list.validate()?;
        if signed.publisher != publisher {
            return Err(MarketplaceError::UntrustedPublisher {
                publisher: signed.publisher.clone(),
            });
        }
        verify_hex(&signed.list, publisher, &signed.signature, key)?;
        Ok(signed)
    }

    /// Sequence of the currently held list (0 when no list has been received).
    pub fn current_sequence(&self) -> u64 {
        self.current
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(|list| list.list.sequence))
            .unwrap_or(0)
    }

    /// Apply a fetched or imported list: verify signature, require a strictly
    /// newer sequence, persist atomically, then swap the in-memory list.
    pub fn apply_update(&self, bytes: &[u8]) -> Result<u64, MarketplaceError> {
        let signed = Self::verify_with_key(bytes, &self.publisher, &self.key)?;
        let current_sequence = self.current_sequence();
        if signed.list.sequence <= current_sequence {
            return Err(MarketplaceError::RevocationRollback {
                sequence: signed.list.sequence,
                current: current_sequence,
            });
        }
        self.persist(bytes)?;
        let sequence = signed.list.sequence;
        match self.current.write() {
            Ok(mut guard) => *guard = Some(signed),
            Err(_) => {
                return Err(MarketplaceError::InvalidData {
                    reason: "revocation store lock is poisoned".to_string(),
                });
            }
        }
        Ok(sequence)
    }

    /// Fetch a revocation list over HTTPS and apply it when newer.
    ///
    /// Returns `Ok(Some(sequence))` when a newer list was applied and
    /// `Ok(None)` when the fetch failed or the list was not newer — fetch
    /// failures never clear or roll back the cached list.
    pub async fn fetch_and_apply(
        &self,
        url: &str,
        client: &reqwest::Client,
    ) -> Result<Option<u64>, MarketplaceError> {
        if !url.to_lowercase().starts_with("https://")
            && !crate::tools::mcp::config::is_localhost_url(url)
        {
            return Err(MarketplaceError::RevocationFetch {
                url: url.to_string(),
                reason: "revocation lists must be fetched over HTTPS".to_string(),
            });
        }
        let response = client
            .get(url)
            .timeout(REVOCATION_FETCH_TIMEOUT)
            .send()
            .await
            .map_err(|error| MarketplaceError::RevocationFetch {
                url: url.to_string(),
                reason: error.to_string(),
            })?;
        if !response.status().is_success() {
            return Err(MarketplaceError::RevocationFetch {
                url: url.to_string(),
                reason: format!("HTTP {}", response.status()),
            });
        }
        let bytes = read_bounded_body(response, MAX_REVOCATION_LIST_BYTES, |reason| {
            MarketplaceError::RevocationFetch {
                url: url.to_string(),
                reason,
            }
        })
        .await?;
        match self.apply_update(&bytes) {
            Ok(sequence) => Ok(Some(sequence)),
            Err(MarketplaceError::RevocationRollback { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Whether a package version/artifact is revoked by the current list.
    /// With no list received yet, nothing is revoked.
    pub fn is_revoked(
        &self,
        package_id: &str,
        version: Option<&str>,
        sha256: Option<&str>,
    ) -> bool {
        match self.current.read() {
            Ok(guard) => guard
                .as_ref()
                .is_some_and(|list| list.list.is_revoked(package_id, version, sha256)),
            Err(_) => true, // poisoned lock fails closed
        }
    }

    /// Check a persisted provenance record against the current revocation
    /// list. Records without a package identity cannot match and pass.
    pub fn check_provenance(
        &self,
        provenance: &crate::tools::mcp::provenance::McpPackageProvenance,
    ) -> Result<(), MarketplaceError> {
        let Some(package_id) = &provenance.package_id else {
            return Ok(());
        };
        let digest = provenance
            .command_sha256
            .as_deref()
            .or(provenance.component_sha256.as_deref());
        if self.is_revoked(package_id, provenance.version.as_deref(), digest) {
            return Err(MarketplaceError::InvalidData {
                reason: format!("package '{package_id}' is revoked by the marketplace list"),
            });
        }
        Ok(())
    }

    /// Persist the verified bytes atomically (write temp, fsync, rename).
    fn persist(&self, bytes: &[u8]) -> Result<(), MarketplaceError> {
        let io_error = |reason: String| MarketplaceError::RevocationCacheIo {
            path: self.cache_path.to_string_lossy().into_owned(),
            reason,
        };
        let Some(parent) = self.cache_path.parent() else {
            return Err(io_error("cache path has no parent directory".to_string()));
        };
        let temporary = parent.join(format!(
            ".{}.tmp",
            self.cache_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "revocations.json".to_string())
        ));
        std::fs::write(&temporary, bytes).map_err(|error| io_error(error.to_string()))?;
        if let Ok(file) = std::fs::File::open(&temporary) {
            let _ = file.sync_all();
        }
        std::fs::rename(&temporary, &self.cache_path).map_err(|error| io_error(error.to_string()))
    }
}

/// Read a response body with a hard byte cap, mapping stream errors through
/// `map_error` so each caller surfaces its own fetch error variant.
async fn read_bounded_body(
    response: reqwest::Response,
    limit: usize,
    map_error: impl Fn(String) -> MarketplaceError,
) -> Result<Vec<u8>, MarketplaceError> {
    use futures::StreamExt;

    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| map_error(error.to_string()))?;
        if body.len() + chunk.len() > limit {
            return Err(MarketplaceError::InvalidData {
                reason: format!("response body exceeds {limit} bytes"),
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publisher_key() -> PublisherSigningKey {
        PublisherSigningKey::from_bytes(&[7_u8; 32])
    }

    fn other_key() -> PublisherSigningKey {
        PublisherSigningKey::from_bytes(&[9_u8; 32])
    }

    fn trust_store() -> PublisherTrustStore {
        let mut store = PublisherTrustStore::default();
        store
            .insert_hex("acme", &publisher_key().public_key_hex())
            .expect("insert publisher key");
        store
    }

    fn dependency(name: &str, version: &str) -> LockedDependency {
        LockedDependency {
            package_id: name.to_string(),
            version: version.to_string(),
            sha256: "a".repeat(64),
        }
    }

    fn manifest() -> PackageManifest {
        PackageManifest {
            schema_version: 1,
            package_id: "@acme/files".to_string(),
            ecosystem: PackageEcosystem::Npm,
            version: "1.0.0".to_string(),
            publisher: "acme".to_string(),
            artifacts: BTreeMap::from([("stdio".to_string(), "b".repeat(64))]),
            dependencies: vec![dependency("@acme/dep", "2.0.0")],
            capabilities: vec!["read_file".to_string(), "write_file".to_string()],
            issued_at: Utc::now(),
        }
    }

    #[test]
    fn signed_manifest_verifies_against_trust_store() {
        let signed = SignedPackageManifest::sign(manifest(), &publisher_key());
        signed.verify(&trust_store()).expect("valid signature");

        let json = serde_json::to_vec(&signed).expect("serialize");
        let parsed = SignedPackageManifest::from_json(&json, &trust_store())
            .expect("bounded parse and verify");
        assert_eq!(parsed.manifest.package_id, "@acme/files");
    }

    #[test]
    fn tampered_manifest_fails_verification() {
        let mut signed = SignedPackageManifest::sign(manifest(), &publisher_key());
        signed.manifest.version = "9.9.9".to_string();
        let error = signed.verify(&trust_store()).expect_err("tamper must fail");
        assert!(matches!(error, MarketplaceError::BadSignature { .. }));
    }

    #[test]
    fn untrusted_publisher_fails_closed() {
        let mut evil_manifest = manifest();
        evil_manifest.publisher = "evil".to_string();
        let signed = SignedPackageManifest::sign(evil_manifest, &other_key());
        let error = signed
            .verify(&trust_store())
            .expect_err("unknown publisher");
        assert!(matches!(error, MarketplaceError::UntrustedPublisher { .. }));

        let signed_wrong_key = SignedPackageManifest::sign(manifest(), &other_key());
        let error = signed_wrong_key
            .verify(&trust_store())
            .expect_err("wrong key for trusted publisher");
        assert!(matches!(error, MarketplaceError::BadSignature { .. }));
    }

    #[test]
    fn manifest_validation_rejects_bad_shapes() {
        let mut bad = manifest();
        bad.schema_version = 99;
        assert!(bad.validate().is_err());

        let mut bad = manifest();
        bad.artifacts.insert("stdio".to_string(), "zz".to_string());
        assert!(bad.validate().is_err());

        let mut bad = manifest();
        bad.capabilities.push("has space".to_string());
        assert!(bad.validate().is_err());

        let mut bad = manifest();
        bad.publisher = String::new();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn trust_store_roundtrips_through_file_form() {
        let store = trust_store();
        let file = store.to_file();
        let json = serde_json::to_string(&file).expect("serialize store");
        let parsed: PublisherTrustStoreFile = serde_json::from_str(&json).expect("parse store");
        let restored = PublisherTrustStore::from_file(parsed).expect("restore store");
        assert!(restored.contains("acme"));
        assert_eq!(restored.len(), 1);

        assert!(
            PublisherTrustStore::from_file(PublisherTrustStoreFile {
                publishers: BTreeMap::from([("bad".to_string(), "not-hex".to_string())]),
            })
            .is_err()
        );
    }

    #[test]
    fn generated_keypair_signs_and_verifies() {
        let key = PublisherSigningKey::generate();
        let mut store = PublisherTrustStore::default();
        store
            .insert_hex("generated", &key.public_key_hex())
            .expect("insert");
        let mut manifest = manifest();
        manifest.publisher = "generated".to_string();
        let signed = SignedPackageManifest::sign(manifest, &key);
        signed.verify(&store).expect("generated key verifies");
    }

    fn signed_lockfile() -> SignedDependencyLockfile {
        SignedDependencyLockfile::sign(
            DependencyLockfile {
                schema_version: 1,
                package_id: "@acme/files".to_string(),
                locks: vec![dependency("@acme/dep", "2.0.0")],
                issued_at: Utc::now(),
            },
            "acme",
            &publisher_key(),
        )
    }

    #[test]
    fn lockfile_pins_exact_dependency_versions() {
        let locks = signed_lockfile();
        locks.verify(&trust_store()).expect("lockfile verifies");
        locks
            .verify_manifest_dependencies(&manifest())
            .expect("manifest dependencies match locks");

        let mut upgraded = manifest();
        upgraded.dependencies = vec![dependency("@acme/dep", "2.1.0")];
        let error = locks
            .verify_manifest_dependencies(&upgraded)
            .expect_err("silent upgrade must fail");
        assert!(matches!(
            error,
            MarketplaceError::DependencyLockMismatch { .. }
        ));

        let mut extra = manifest();
        extra.dependencies.push(dependency("@acme/sneaky", "1.0.0"));
        let error = locks
            .verify_manifest_dependencies(&extra)
            .expect_err("unlocked dependency must fail");
        assert!(matches!(
            error,
            MarketplaceError::DependencyNotLocked { .. }
        ));
    }

    #[test]
    fn lockfile_verifies_resolved_artifact_digest() {
        let locks = signed_lockfile();
        locks
            .verify_artifact("@acme/dep", "2.0.0", &"a".repeat(64))
            .expect("exact artifact matches");
        assert!(
            locks
                .verify_artifact("@acme/dep", "2.0.0", &"c".repeat(64))
                .is_err()
        );
        assert!(
            locks
                .verify_artifact("@acme/other", "1.0.0", &"a".repeat(64))
                .is_err()
        );
    }

    #[test]
    fn duplicate_locks_are_rejected() {
        let mut lockfile = signed_lockfile().lockfile;
        lockfile.locks.push(dependency("@acme/dep", "2.0.0"));
        assert!(lockfile.validate().is_err());
    }

    #[test]
    fn manifest_diff_reports_upgrade_shape() {
        let old = manifest();
        let mut new = manifest();
        new.version = "1.1.0".to_string();
        new.capabilities = vec!["read_file".to_string(), "delete_file".to_string()];
        new.dependencies = vec![
            dependency("@acme/dep", "2.1.0"),
            dependency("@acme/new-dep", "0.1.0"),
        ];
        new.artifacts.insert("stdio".to_string(), "d".repeat(64));

        let diff = diff_manifests(&old, &new);
        assert_eq!(diff.added_capabilities, vec!["delete_file".to_string()]);
        assert_eq!(diff.removed_capabilities, vec!["write_file".to_string()]);
        assert_eq!(diff.added_dependencies, vec!["@acme/new-dep".to_string()]);
        assert_eq!(diff.changed_dependencies, vec!["@acme/dep".to_string()]);
        assert_eq!(diff.changed_artifacts, vec!["stdio".to_string()]);
        assert!(!diff.is_additive_only());

        let mut additive = manifest();
        additive.capabilities.push("extra".to_string());
        let diff = diff_manifests(&old, &additive);
        assert!(diff.is_additive_only());
    }

    fn revocation_list(sequence: u64) -> RevocationList {
        RevocationList {
            schema_version: 1,
            sequence,
            issued_at: Utc::now(),
            entries: vec![
                RevocationEntry {
                    package_id: "@acme/files".to_string(),
                    version: Some("1.0.0".to_string()),
                    sha256: None,
                    reason: Some("compromised".to_string()),
                },
                RevocationEntry {
                    package_id: "@acme/bad".to_string(),
                    version: None,
                    sha256: Some("e".repeat(64)),
                    reason: None,
                },
                RevocationEntry {
                    package_id: "@acme/fully-pulled".to_string(),
                    version: None,
                    sha256: None,
                    reason: None,
                },
            ],
        }
    }

    #[test]
    fn revocation_entries_match_by_version_digest_or_whole_package() {
        let list = revocation_list(1);
        assert!(list.is_revoked("@acme/files", Some("1.0.0"), None));
        assert!(!list.is_revoked("@acme/files", Some("1.0.1"), None));
        assert!(!list.is_revoked("@acme/files", None, None));
        assert!(list.is_revoked("@acme/bad", None, Some(&"e".repeat(64))));
        assert!(!list.is_revoked("@acme/bad", None, Some(&"f".repeat(64))));
        assert!(!list.is_revoked("@acme/bad", Some("3.0.0"), None));
        assert!(list.is_revoked("@acme/fully-pulled", Some("9.9.9"), None));
        assert!(!list.is_revoked("@acme/other", Some("1.0.0"), None));
    }

    #[test]
    fn signed_revocation_list_verifies_and_bounds_input() {
        let signed = SignedRevocationList::sign(revocation_list(1), "acme", &publisher_key());
        let json = serde_json::to_vec(&signed).expect("serialize");
        SignedRevocationList::from_json(&json, &trust_store()).expect("verify list");

        let mut tampered = signed.clone();
        tampered.list.entries.clear();
        assert!(tampered.verify(&trust_store()).is_err());

        let oversized = vec![b'['; MAX_REVOCATION_LIST_BYTES + 1];
        assert!(SignedRevocationList::from_json(&oversized, &trust_store()).is_err());
    }

    #[test]
    fn revocation_store_applies_newer_lists_and_rejects_rollback() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cache = temp.path().join("revocations.json");
        let store = RevocationStore::open(cache.clone(), "acme", &publisher_key().public_key_hex())
            .expect("open empty store");
        assert_eq!(store.current_sequence(), 0);
        assert!(!store.is_revoked("@acme/files", Some("1.0.0"), None));

        let list_v1 = serde_json::to_vec(&SignedRevocationList::sign(
            revocation_list(1),
            "acme",
            &publisher_key(),
        ))
        .expect("serialize v1");
        assert_eq!(store.apply_update(&list_v1).expect("apply v1"), 1);
        assert!(store.is_revoked("@acme/files", Some("1.0.0"), None));

        // Same or older sequence is rejected as a rollback.
        let error = store.apply_update(&list_v1).expect_err("replay must fail");
        assert!(matches!(error, MarketplaceError::RevocationRollback { .. }));

        // A list signed by another key is rejected even with a newer sequence.
        let foreign = serde_json::to_vec(&SignedRevocationList::sign(
            revocation_list(2),
            "acme",
            &other_key(),
        ))
        .expect("serialize foreign");
        assert!(store.apply_update(&foreign).is_err());
        assert_eq!(store.current_sequence(), 1);

        // Cache persists across reopen.
        let reopened = RevocationStore::open(cache, "acme", &publisher_key().public_key_hex())
            .expect("reopen");
        assert_eq!(reopened.current_sequence(), 1);
        assert!(reopened.is_revoked("@acme/files", Some("1.0.0"), None));

        // Provenance records for revoked packages are blocked; unknown
        // packages and unversioned records pass.
        let mut provenance = crate::tools::mcp::provenance::McpPackageProvenance::with_trust(
            crate::tools::mcp::provenance::PackageTrust::Pinned,
        );
        provenance.package_id = Some("@acme/files".to_string());
        provenance.version = Some("1.0.0".to_string());
        assert!(reopened.check_provenance(&provenance).is_err());
        provenance.version = Some("2.0.0".to_string());
        reopened
            .check_provenance(&provenance)
            .expect("unrevoked version passes");
        provenance.package_id = None;
        reopened
            .check_provenance(&provenance)
            .expect("identity-less record passes");
    }

    #[test]
    fn revocation_store_rejects_corrupt_cache_and_wrong_publisher() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cache = temp.path().join("revocations.json");
        std::fs::write(&cache, b"not json").expect("write corrupt cache");
        assert!(
            RevocationStore::open(cache.clone(), "acme", &publisher_key().public_key_hex())
                .is_err()
        );

        // A valid list cached under a different publisher key is rejected.
        let other_cache = temp.path().join("other.json");
        let other_store =
            RevocationStore::open(other_cache.clone(), "acme", &other_key().public_key_hex())
                .expect("open with other key");
        let list = serde_json::to_vec(&SignedRevocationList::sign(
            revocation_list(1),
            "acme",
            &other_key(),
        ))
        .expect("serialize");
        other_store
            .apply_update(&list)
            .expect("apply with other key");
        assert!(
            RevocationStore::open(other_cache, "acme", &publisher_key().public_key_hex()).is_err(),
            "cache signed by a different key must fail verification"
        );
    }

    #[tokio::test]
    async fn revocation_store_fetches_over_http_and_keeps_list_on_stale() {
        use axum::{Router, routing::get};
        use tokio::net::TcpListener;

        let list_v2 = serde_json::to_vec(&SignedRevocationList::sign(
            revocation_list(2),
            "acme",
            &publisher_key(),
        ))
        .expect("serialize v2");
        let app = Router::new().route(
            "/revocations.json",
            get(move || {
                let body = list_v2.clone();
                async move { body }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!(
            "http://{}/revocations.json",
            listener.local_addr().expect("addr")
        );
        tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });

        let temp = tempfile::tempdir().expect("temp dir");
        let store = RevocationStore::open(
            temp.path().join("revocations.json"),
            "acme",
            &publisher_key().public_key_hex(),
        )
        .expect("open store");
        let client = reqwest::Client::new();

        let applied = store
            .fetch_and_apply(&url, &client)
            .await
            .expect("fetch succeeds");
        assert_eq!(applied, Some(2));
        assert!(store.is_revoked("@acme/files", Some("1.0.0"), None));

        // Refetching the same list is not an error but applies nothing.
        let applied = store.fetch_and_apply(&url, &client).await.expect("refetch");
        assert_eq!(applied, None);

        // Plain HTTP to a non-loopback host is refused.
        assert!(
            store
                .fetch_and_apply("http://203.0.113.10/revocations.json", &client)
                .await
                .is_err()
        );
    }

    fn second_manifest() -> PackageManifest {
        let mut second = manifest();
        second.package_id = "@acme/search".to_string();
        second.artifacts = BTreeMap::from([("wasm".to_string(), "c".repeat(64))]);
        second
    }

    #[test]
    fn catalog_verifies_all_embedded_manifests() {
        let catalog = MarketplaceCatalog::new(vec![
            SignedPackageManifest::sign(manifest(), &publisher_key()),
            SignedPackageManifest::sign(second_manifest(), &publisher_key()),
        ]);
        let store = trust_store();
        catalog.verify(&store).expect("catalog verifies");

        let json = serde_json::to_vec(&catalog).expect("serialize catalog");
        let parsed = MarketplaceCatalog::from_json(&json, &store).expect("parse catalog");
        assert_eq!(parsed.len(), 2);
        assert!(parsed.find("@acme/files").is_some());
        assert!(parsed.find("@acme/search").is_some());
        assert!(parsed.find("@acme/missing").is_none());
        assert!(matches!(
            parsed
                .require("@acme/missing")
                .expect_err("missing package"),
            MarketplaceError::PackageNotInCatalog { .. }
        ));
    }

    #[test]
    fn catalog_fails_closed_on_untrusted_entry() {
        // One signed-by-unknown-publisher entry poisons the whole catalog.
        let mut evil = second_manifest();
        evil.publisher = "evil".to_string();
        let catalog = MarketplaceCatalog::new(vec![
            SignedPackageManifest::sign(manifest(), &publisher_key()),
            SignedPackageManifest::sign(evil, &other_key()),
        ]);
        let store = trust_store();
        let error = catalog
            .verify(&store)
            .expect_err("catalog must fail closed");
        assert!(matches!(error, MarketplaceError::UntrustedPublisher { .. }));

        let json = serde_json::to_vec(&catalog).expect("serialize");
        assert!(MarketplaceCatalog::from_json(&json, &store).is_err());
    }

    #[test]
    fn catalog_rejects_duplicates_and_bad_schema() {
        let catalog = MarketplaceCatalog::new(vec![
            SignedPackageManifest::sign(manifest(), &publisher_key()),
            SignedPackageManifest::sign(manifest(), &publisher_key()),
        ]);
        assert!(catalog.validate().is_err());

        let mut bad_schema = MarketplaceCatalog::new(vec![]);
        bad_schema.schema_version = 99;
        assert!(bad_schema.validate().is_err());
    }

    #[test]
    fn catalog_size_bounds_fail_closed() {
        let mut oversized = MarketplaceCatalog::new(vec![]);
        oversized.packages = vec![
            SignedPackageManifest::sign(manifest(), &publisher_key());
            MAX_CATALOG_ENTRIES + 1
        ];
        assert!(oversized.validate().is_err());

        let store = trust_store();
        let json = serde_json::to_vec(&oversized).expect("serialize");
        assert!(MarketplaceCatalog::from_json(&json, &store).is_err());
    }

    #[tokio::test]
    async fn catalog_fetch_rejects_non_https_and_serves_verified_entries() {
        use axum::{Router, routing::get};
        use tokio::net::TcpListener;

        let catalog = MarketplaceCatalog::new(vec![SignedPackageManifest::sign(
            manifest(),
            &publisher_key(),
        )]);
        let body = serde_json::to_vec(&catalog).expect("serialize catalog");
        let app = Router::new().route(
            "/catalog.json",
            get(move || {
                let body = body.clone();
                async move { body }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!(
            "http://{}/catalog.json",
            listener.local_addr().expect("addr")
        );
        tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });

        let store = trust_store();
        let client = reqwest::Client::new();
        let fetched = MarketplaceCatalog::fetch(&url, &store, &client)
            .await
            .expect("fetch verified catalog");
        assert_eq!(fetched.len(), 1);

        // Plain HTTP to a non-loopback host is refused before any request.
        assert!(
            MarketplaceCatalog::fetch("http://203.0.113.10/catalog.json", &store, &client)
                .await
                .is_err()
        );
    }
}
