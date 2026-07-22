//! Tenant-scoped DarkIRC configuration ownership and legacy-contact adoption.
//!
//! This module intentionally does not implement the public key exchange state
//! machine.  It owns the prerequisite shared updater and the read-only/legacy
//! contact inventory needed before exchange automation can be enabled.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use fs4::FileExt;
use hmac::{Hmac, Mac};
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use toml::{Table, Value};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const KEY_EXCHANGE_DIR: &str = "key-exchange";
const CANDIDATES_DIR: &str = "key-exchange/candidates";
const TRANSACTIONS_DIR: &str = "key-exchange/transactions";
const PENDING_DIR: &str = "key-exchange/pending";
const ROLLBACK_DIR: &str = "key-exchange/rollback";
const LEDGER_FILE: &str = "key-exchange/ledger.json";
const CONFIG_FILE: &str = "darkirc_config.toml";
const LEDGER_SCHEMA: &str = "lunarwing.darkirc-ledger/v1";
pub const MIGRATION_FILE: &str = "key-exchange/darkirc-contacts-v1.json";
const STAGED_MIGRATION_FILE: &str = "key-exchange/darkirc-contacts-v1.staged.json";
const MIGRATION_FILE_NEXT: &str = "key-exchange/darkirc-contacts-v1.json.next";
const STAGED_MIGRATION_FILE_NEXT: &str = "key-exchange/darkirc-contacts-v1.staged.json.next";
pub const MIGRATION_SCHEMA: &str = "darkirc-contacts-v1";
pub const EXCHANGE_SCHEMA: &str = "lunarwing.darkirc.exchange.v1";
pub const EXCHANGE_SCHEMA_VERSION: &str = "1";
pub const MAX_PENDING_BYTES: usize = 64 * 1024;
pub const ACTIVATION_WINDOW_SECONDS: i64 = 24 * 60 * 60;
pub const EXTENSION_WINDOW_SECONDS: i64 = 7 * 24 * 60 * 60;
pub const DEFAULT_OFFER_TTL_SECONDS: i64 = 30 * 60;

pub const EXCHANGE_V1_DOMAIN_SEPARATOR: &str = "lunarwing.darkirc.exchange.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExchangeRole {
    InitiatorOffer,
    ResponderResponse,
}

impl ExchangeRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::InitiatorOffer => "initiator_offer",
            Self::ResponderResponse => "responder_response",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExchangeOperation {
    Initial,
    Rotate,
}

impl ExchangeOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Rotate => "rotate",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExchangeState {
    Prepared,
    PeerReceived,
    Confirmed,
    Installed,
    LocallyActivated,
    PeerVerified,
    VerificationOverdue,
    RolledBack,
    Expired,
    Cancelled,
    Revoked,
}

impl ExchangeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "Prepared",
            Self::PeerReceived => "PeerReceived",
            Self::Confirmed => "Confirmed",
            Self::Installed => "Installed",
            Self::LocallyActivated => "LocallyActivated",
            Self::PeerVerified => "PeerVerified",
            Self::VerificationOverdue => "VerificationOverdue",
            Self::RolledBack => "RolledBack",
            Self::Expired => "Expired",
            Self::Cancelled => "Cancelled",
            Self::Revoked => "Revoked",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreparedExchange {
    pub schema: String,
    pub version: String,
    pub artifact_role: String,
    pub offer_id: String,
    pub sender_contact_id: String,
    pub intended_peer_contact_id: Option<String>,
    pub intended_peer_fingerprint: Option<String>,
    pub generator_profile: String,
    pub binary_sha256: String,
    pub key_format: String,
    pub source_revision: String,
    pub public_key: String,
    pub public_fingerprint: String,
    pub generation: u64,
    pub created_at: i64,
    pub expires_at: i64,
    pub label: Option<String>,
    pub previous_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResponderExchange {
    pub schema: String,
    pub version: String,
    pub artifact_role: String,
    pub offer_id: String,
    pub in_reply_to: String,
    pub sender_contact_id: String,
    pub intended_peer_contact_id: String,
    pub intended_peer_fingerprint: String,
    pub generator_profile: String,
    pub binary_sha256: String,
    pub key_format: String,
    pub source_revision: String,
    pub public_key: String,
    pub public_fingerprint: String,
    pub generation: u64,
    pub created_at: i64,
    pub expires_at: i64,
    pub label: Option<String>,
    pub previous_fingerprint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedPublicArtifact {
    pub role: ExchangeRole,
    pub offer_id: String,
    pub sender_contact_id: String,
    pub intended_peer_contact_id: Option<String>,
    pub public_fingerprint: PublicFingerprint,
    pub generation: u64,
    pub created_at: i64,
    pub expires_at: i64,
    pub canonical: Vec<u8>,
    pub in_reply_to: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretExchange {
    pub schema: String,
    pub offer_id: String,
    pub role: String,
    pub operation: String,
    pub contact_id: String,
    pub peer_contact_id: Option<String>,
    pub local_public: String,
    #[serde(serialize_with = "serialize_secret_string")]
    pub local_private: SecretString,
    pub peer_public: Option<String>,
    pub peer_fingerprint: Option<String>,
    pub state: String,
    pub scope_id: String,
    pub contact_name: String,
    pub generation: u64,
    pub created_at: i64,
    pub expires_at: i64,
    pub transcript: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ExchangeStatus {
    pub contact_name: String,
    pub contact_id: String,
    pub offer_id: String,
    pub state: String,
    pub peer_fingerprint: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub confirmed_at: Option<i64>,
    pub installed_at: Option<i64>,
    pub activation_deadline: Option<i64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PreparedResult {
    pub offer_id: String,
    pub contact_id: String,
    pub role: String,
    pub public_artifact: String,
    pub public_fingerprint: String,
    pub expires_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevocationDisposition {
    Routine,
    Compromised,
}
pub const MIGRATION_GENERATOR_PROFILE: &str = "darkfi-a05956d41-chacha-v1";
pub const MIGRATION_KEY_FORMAT: &str = "darkfi-chacha-base58-32";
pub const MIGRATION_SOURCE_REVISION: &str = "a05956d412a091e8b54c1cd4f4264c33b941203d";
pub const MAX_MIGRATION_BYTES: usize = 1024 * 1024;
pub const MAX_MIGRATION_CONTACTS: usize = 1024;
pub const MAX_BASELINE_BYTES: usize = 1024 * 1024;
const MAX_STATE_FILE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum KeyManagerError {
    #[error("filesystem operation failed")]
    Io(#[source] io::Error),
    #[error("invalid tenant identifier")]
    InvalidTenant,
    #[error("invalid tenant scope identifier")]
    InvalidScope,
    #[error("invalid contact name")]
    InvalidContact,
    #[error("invalid DarkIRC TOML")]
    InvalidToml,
    #[error("invalid DarkIRC ledger")]
    InvalidLedger,
    #[error("unsupported DarkIRC binary compatibility profile")]
    InvalidCompatibility,
    #[error("tenant scope conflict")]
    ScopeConflict,
    #[error("contact conflict")]
    ContactConflict,
    #[error("DarkIRC contact not found")]
    ContactNotFound,
    #[error("unsupported or malformed public key")]
    InvalidPublicKey,
    #[error("unsafe tenant state path")]
    UnsafePath,
    #[error("unfinished transaction requires recovery")]
    RecoveryRequired,
    #[error("DarkIRC config update is busy")]
    Busy,
    #[error("crash point injected for recovery test")]
    CrashInjected,
    #[error("public exchange artifact is invalid")]
    InvalidExchangeArtifact,
    #[error("exchange fingerprint mismatch")]
    FingerprintMismatch,
    #[error("exchange offer ID conflict")]
    OfferConflict,
    #[error("exchange state transition not allowed")]
    InvalidExchangeState,
    #[error("activation health gate failed")]
    ActivationFailed,
    #[error("verification deadline exceeded")]
    VerificationOverdue,
    #[error("compromised key cannot be restored")]
    CompromisedKey,
    #[error("exchange operation not found")]
    ExchangeNotFound,
}

impl From<io::Error> for KeyManagerError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub type Result<T> = std::result::Result<T, KeyManagerError>;

#[derive(Clone, Debug)]
pub struct TenantPaths {
    darkirc_dir: PathBuf,
    expected_uid: Option<u32>,
    expected_gid: Option<u32>,
}

impl TenantPaths {
    /// Resolve the only production path accepted by the helper.
    pub fn for_tenant(
        tenant: &str,
        expected_uid: Option<u32>,
        expected_gid: Option<u32>,
    ) -> Result<Self> {
        validate_tenant_name(tenant)?;
        if expected_uid.is_none() || expected_gid.is_none() {
            return Err(KeyManagerError::UnsafePath);
        }
        Ok(Self {
            darkirc_dir: PathBuf::from("/home")
                .join(tenant)
                .join("lunarwing/state/darkirc"),
            expected_uid,
            expected_gid,
        })
    }

    /// Test-only and embedding entry point for an already-resolved directory.
    /// The standalone CLI never accepts an arbitrary path from its operator.
    #[cfg(test)]
    fn from_darkirc_dir(path: impl Into<PathBuf>) -> Self {
        Self {
            darkirc_dir: path.into(),
            expected_uid: None,
            expected_gid: None,
        }
    }

    pub fn darkirc_dir(&self) -> &Path {
        &self.darkirc_dir
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityAttestation {
    generator_profile: String,
    binary_sha256: String,
    key_format: String,
    source_revision: String,
}

impl CompatibilityAttestation {
    pub fn new(
        generator_profile: &str,
        binary_sha256: &str,
        key_format: &str,
        source_revision: &str,
    ) -> Result<Self> {
        if generator_profile != MIGRATION_GENERATOR_PROFILE
            || key_format != MIGRATION_KEY_FORMAT
            || source_revision != MIGRATION_SOURCE_REVISION
            || !valid_hash(binary_sha256)
        {
            return Err(KeyManagerError::InvalidCompatibility);
        }
        Ok(Self {
            generator_profile: generator_profile.to_owned(),
            binary_sha256: binary_sha256.to_owned(),
            key_format: key_format.to_owned(),
            source_revision: source_revision.to_owned(),
        })
    }

    pub fn binary_sha256(&self) -> &str {
        &self.binary_sha256
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    pub schema: String,
    pub scope_id: String,
    #[serde(default)]
    pub contacts: BTreeMap<String, LedgerContact>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LedgerContact {
    pub state: String,
    pub peer_fingerprint: String,
    pub generation: u64,
    /// Stable 128-bit hex contact identifier assigned at exchange time.
    /// Empty for legacy contacts (pre-exchange).
    #[serde(default)]
    pub contact_id: String,
    /// Local public fingerprint (sha256 of decoded local public key).
    /// Empty for legacy contacts.
    #[serde(default)]
    pub local_fingerprint: String,
    /// POSIX timestamp deadline for peer verification after local activation.
    /// 0 means no deadline set.
    #[serde(default)]
    pub activation_deadline: i64,
    /// Transaction ID of the rollback snapshot, if a live rollback exists.
    #[serde(default)]
    pub rollback_tx_id: String,
    /// Hash of the rollback contact TOML, for semantic rollback validation.
    #[serde(default)]
    pub rollback_contact_toml_hash: String,
    /// Pending offer ID, if an exchange is in progress for this contact.
    #[serde(default)]
    pub pending_offer_id: String,
    /// Monotonic compromise marker — once true, rollback is forbidden.
    #[serde(default)]
    pub compromised: bool,
    /// Revocation tombstone generation, if the contact is revoked.
    #[serde(default)]
    pub revocation_generation: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContactSummary {
    pub name: String,
    pub peer_fingerprint: String,
    pub state: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DoctorIssue {
    pub code: String,
    pub contacts: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DoctorReport {
    pub contacts: Vec<ContactSummary>,
    pub issues: Vec<DoctorIssue>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateResult {
    pub transaction_id: Option<String>,
    pub config_hash: String,
    pub ledger_hash: String,
    pub changed: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct MigrationValidation {
    pub scope_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashPoint {
    AfterJournal,
    AfterLedger,
    AfterConfig,
    AfterLedgerCommit,
    AfterActivationStart,
    AfterSuccess,
    AfterRollbackStart,
    AfterRevocationTombstone,
    AfterRevocationRemoval,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JournalPhase {
    Prepared,
    LedgerCommitted,
    ConfigCommitted,
    ActivationStarted,
    LocallyActivated,
    RollbackStarted,
    RolledBack,
    RevocationTombstoneCommitted,
    RevocationRemovalCommitted,
    PairLocalCoordinator,
}

impl JournalPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::LedgerCommitted => "ledger_committed",
            Self::ConfigCommitted => "config_committed",
            Self::ActivationStarted => "activation_started",
            Self::LocallyActivated => "locally_activated",
            Self::RollbackStarted => "rollback_started",
            Self::RolledBack => "rolled_back",
            Self::RevocationTombstoneCommitted => "revocation_tombstone_committed",
            Self::RevocationRemovalCommitted => "revocation_removal_committed",
            Self::PairLocalCoordinator => "pair_local_coordinator",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "prepared" => Ok(Self::Prepared),
            "ledger_committed" => Ok(Self::LedgerCommitted),
            "config_committed" => Ok(Self::ConfigCommitted),
            "activation_started" => Ok(Self::ActivationStarted),
            "locally_activated" => Ok(Self::LocallyActivated),
            "rollback_started" => Ok(Self::RollbackStarted),
            "rolled_back" => Ok(Self::RolledBack),
            "revocation_tombstone_committed" => Ok(Self::RevocationTombstoneCommitted),
            "revocation_removal_committed" => Ok(Self::RevocationRemovalCommitted),
            "pair_local_coordinator" => Ok(Self::PairLocalCoordinator),
            _ => Err(KeyManagerError::RecoveryRequired),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalFile {
    schema: String,
    transaction_id: String,
    operation: String,
    scope_id: String,
    old_config_hash: Option<String>,
    old_ledger_hash: Option<String>,
    new_config_hash: String,
    new_ledger_hash: String,
    config_candidate: String,
    ledger_candidate: String,
    phase: String,
    /// Stable contact ID for exchange-related operations.
    #[serde(default)]
    contact_id: String,
    /// Offer ID bound to this exchange (prepare/respond/complete).
    #[serde(default)]
    offer_id: String,
    /// Peer's contact ID for the exchange.
    #[serde(default)]
    peer_contact_id: String,
    /// SHA-256 transcript hash binding the exchange artifacts.
    #[serde(default)]
    transcript: String,
    /// Locally authorized generation for this operation.
    #[serde(default)]
    generation: u64,
    /// Kind of exchange operation for recovery routing.
    /// One of: exchange-prepare, exchange-respond, exchange-complete,
    /// exchange-cancel, exchange-activation, exchange-rollback,
    /// exchange-roundtrip, rotation-prepare, rotation-respond,
    /// rotation-complete, revocation-tombstone, revocation-removal,
    /// pair-local-coordinator.
    #[serde(default)]
    operation_kind: String,
    /// Hash of the contact TOML captured for semantic rollback.
    #[serde(default)]
    rollback_contact_toml_hash: String,
    /// Peer fingerprint captured at confirmation.
    #[serde(default)]
    confirmed_peer_fingerprint: String,
    /// Transaction ID of the rollback snapshot.
    #[serde(default)]
    rollback_tx_id: String,
    /// Contact name for exchange operations.
    #[serde(default)]
    contact_name: String,
    /// Generation counter of the peer at the time of exchange.
    #[serde(default)]
    peer_generation: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MigrationManifest {
    schema: String,
    scope_id: String,
    generator_profile: String,
    binary_sha256: String,
    key_format: String,
    source_revision: String,
    #[serde(serialize_with = "serialize_secret_string")]
    contacts_toml: SecretString,
    contacts_hash: String,
    ledger: Ledger,
    ledger_hash: String,
}

fn serialize_secret_string<S>(
    secret: &SecretString,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    secret.expose_secret().serialize(serializer)
}

impl JournalFile {
    fn phase(&self) -> Result<JournalPhase> {
        JournalPhase::parse(&self.phase)
    }
}

#[derive(Debug)]
struct Workspace {
    paths: TenantPaths,
    dir: Dir,
}

impl Workspace {
    fn open(paths: TenantPaths) -> Result<Self> {
        prepare_directories(&paths)?;
        Self::open_read_only(paths)
    }

    fn open_read_only(paths: TenantPaths) -> Result<Self> {
        validate_path_components(&paths.darkirc_dir)?;
        validate_secure_entry(
            &paths.darkirc_dir,
            paths.expected_uid,
            paths.expected_gid,
            true,
        )?;
        let dir = Dir::open_ambient_dir(&paths.darkirc_dir, ambient_authority())
            .map_err(KeyManagerError::Io)?;
        Ok(Self { paths, dir })
    }

    fn exchange_dir(&self) -> Result<Dir> {
        self.open_relative_dir(KEY_EXCHANGE_DIR)
    }

    fn open_relative_dir(&self, relative: &str) -> Result<Dir> {
        let mut directory = self.dir.try_clone().map_err(KeyManagerError::Io)?;
        if relative.is_empty() {
            return Ok(directory);
        }
        for component in relative.split('/') {
            if component.is_empty() || component == "." || component == ".." {
                return Err(KeyManagerError::UnsafePath);
            }
            directory = directory
                .open_dir_nofollow(component)
                .map_err(KeyManagerError::Io)?;
            validate_open_directory(&directory, self.paths.expected_uid, self.paths.expected_gid)?;
        }
        Ok(directory)
    }

    fn lock(&self) -> Result<FileLock> {
        let parent_lock = File::open(&self.paths.darkirc_dir).map_err(KeyManagerError::Io)?;
        lock_with_timeout(&parent_lock, true)?;
        let exchange = self.exchange_dir()?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .follow(FollowSymlinks::No)
            .sync(true);
        let cap_file = exchange
            .open_with("update.lock", &options)
            .map_err(KeyManagerError::Io)?;
        let file = cap_file.into_std();
        validate_open_file(
            &file,
            self.paths.expected_uid,
            self.paths.expected_gid,
            0o600,
        )?;
        lock_with_timeout(&file, true)?;
        Ok(FileLock {
            file,
            parent: Some(parent_lock),
        })
    }

    fn read_snapshot(&self) -> Result<Option<FileLock>> {
        let lock = self.read_lock()?;
        // Inspection must never present a partially committed config/ledger
        // pair as a settled snapshot.  The shared lock prevents writers that
        // use this module from changing the state while this check and the
        // subsequent reads run.
        ensure_migration_settled(self)?;
        Ok(lock)
    }

    fn read_lock(&self) -> Result<Option<FileLock>> {
        let parent_lock = File::open(&self.paths.darkirc_dir).map_err(KeyManagerError::Io)?;
        lock_with_timeout(&parent_lock, false)?;
        let exchange = match self.open_relative_dir(KEY_EXCHANGE_DIR) {
            Ok(directory) => directory,
            Err(KeyManagerError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Some(FileLock {
                    file: parent_lock,
                    parent: None,
                }));
            }
            Err(error) => return Err(error),
        };
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let cap_file = match exchange.open_with("update.lock", &options) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Some(FileLock {
                    file: parent_lock,
                    parent: None,
                }));
            }
            Err(error) => return Err(KeyManagerError::Io(error)),
        };
        let file = cap_file.into_std();
        validate_open_file(
            &file,
            self.paths.expected_uid,
            self.paths.expected_gid,
            0o600,
        )?;
        lock_with_timeout(&file, false)?;
        Ok(Some(FileLock {
            file,
            parent: Some(parent_lock),
        }))
    }

    fn read_optional(&self, relative: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        let parent_dir = match self.open_relative_dir(parent_relative(relative)) {
            Ok(directory) => directory,
            Err(KeyManagerError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let name = file_name(relative)?;
        let metadata = match parent_dir.symlink_metadata(name) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(KeyManagerError::Io(error)),
        };
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(KeyManagerError::UnsafePath);
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let mut file = parent_dir
            .open_with(name, &options)
            .map_err(KeyManagerError::Io)?
            .into_std();
        validate_open_file(
            &file,
            self.paths.expected_uid,
            self.paths.expected_gid,
            0o600,
        )?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_STATE_FILE_BYTES + 1));
        let max_read = u64::try_from(MAX_STATE_FILE_BYTES)
            .map_err(|_| KeyManagerError::InvalidLedger)?
            .saturating_add(1);
        (&mut file)
            .take(max_read)
            .read_to_end(&mut bytes)
            .map_err(KeyManagerError::Io)?;
        if bytes.len() > MAX_STATE_FILE_BYTES {
            return Err(KeyManagerError::InvalidLedger);
        }
        Ok(Some(bytes))
    }

    fn read_required(&self, relative: &str) -> Result<Zeroizing<Vec<u8>>> {
        self.read_optional(relative)?
            .ok_or(KeyManagerError::InvalidLedger)
    }

    fn remove_if_present(&self, relative: &str) -> Result<()> {
        let parent_dir = self.open_relative_dir(parent_relative(relative))?;
        let name = file_name(relative)?;
        match parent_dir.symlink_metadata(name) {
            Ok(metadata) => {
                if !metadata.is_file() || metadata.nlink() != 1 {
                    return Err(KeyManagerError::UnsafePath);
                }
                parent_dir.remove_file(name).map_err(KeyManagerError::Io)?;
                sync_dir_path(&self.paths.darkirc_dir.join(parent_relative(relative)))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(KeyManagerError::Io(error)),
        }
    }

    fn write_new(&self, relative: &str, bytes: &[u8]) -> Result<()> {
        let parent = parent_relative(relative);
        let parent_dir = self.open_relative_dir(parent)?;
        let name = file_name(relative)?;
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .follow(FollowSymlinks::No)
            .sync(true);
        let mut file = parent_dir
            .open_with(name, &options)
            .map_err(KeyManagerError::Io)?
            .into_std();
        file.write_all(bytes).map_err(KeyManagerError::Io)?;
        file.sync_all().map_err(KeyManagerError::Io)?;
        validate_open_file(
            &file,
            self.paths.expected_uid,
            self.paths.expected_gid,
            0o600,
        )?;
        sync_dir_path(&self.paths.darkirc_dir.join(parent))?;
        Ok(())
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        let from_parent = parent_relative(from);
        let to_parent = parent_relative(to);
        let from_dir = self.open_relative_dir(from_parent)?;
        let to_dir = self.open_relative_dir(to_parent)?;
        from_dir
            .rename(file_name(from)?, &to_dir, file_name(to)?)
            .map_err(KeyManagerError::Io)?;
        sync_dir_path(&self.paths.darkirc_dir.join(from_parent))?;
        sync_dir_path(&self.paths.darkirc_dir.join(to_parent))
    }
}

struct FileLock {
    file: File,
    parent: Option<File>,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
        if let Some(parent) = &self.parent {
            let _ = FileExt::unlock(parent);
        }
    }
}

fn lock_with_timeout(file: &File, exclusive: bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let result = if exclusive {
            file.try_lock_exclusive()
        } else {
            FileExt::try_lock_shared(file)
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(KeyManagerError::Busy);
                }
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(KeyManagerError::Io(error)),
        }
    }
}

fn sync_dir_path(path: &Path) -> Result<()> {
    // cap-std directory handles may be O_PATH handles on Linux, which cannot
    // be fsync'd directly. Re-open the already validated directory read-only
    // solely for the POSIX directory fsync after a rename.
    File::open(path)
        .map_err(KeyManagerError::Io)?
        .sync_all()
        .map_err(KeyManagerError::Io)
}

fn parent_relative(relative: &str) -> &str {
    relative
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("")
}

fn file_name(relative: &str) -> Result<&str> {
    let name = relative
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(relative);
    if name.is_empty() || name == "." || name == ".." {
        return Err(KeyManagerError::UnsafePath);
    }
    Ok(name)
}

fn validate_tenant_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || name.starts_with('-')
        || name.ends_with('-')
    {
        return Err(KeyManagerError::InvalidTenant);
    }
    Ok(())
}

fn validate_scope_id(scope_id: &str) -> Result<()> {
    if scope_id.len() != 32
        || scope_id.bytes().all(|byte| byte == b'0')
        || !scope_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(KeyManagerError::InvalidScope);
    }
    Ok(())
}

fn validate_contact_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || name
            .chars()
            .any(|ch| ch.is_control() || ch == '/' || ch == '\\' || ch == '\n' || ch == '\r')
    {
        return Err(KeyManagerError::InvalidContact);
    }
    Ok(())
}

fn validate_path_components(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => return Err(KeyManagerError::UnsafePath),
            Component::Normal(part) => {
                current.push(part);
                let metadata = fs::symlink_metadata(&current).map_err(KeyManagerError::Io)?;
                if metadata.file_type().is_symlink() {
                    return Err(KeyManagerError::UnsafePath);
                }
            }
        }
    }
    Ok(())
}

fn prepare_directories(paths: &TenantPaths) -> Result<()> {
    validate_path_components(&paths.darkirc_dir)?;
    validate_secure_entry(
        &paths.darkirc_dir,
        paths.expected_uid,
        paths.expected_gid,
        true,
    )?;
    for relative in [
        KEY_EXCHANGE_DIR,
        CANDIDATES_DIR,
        TRANSACTIONS_DIR,
        PENDING_DIR,
        ROLLBACK_DIR,
    ] {
        let path = paths.darkirc_dir.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&path).map_err(KeyManagerError::Io)?;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                    .map_err(KeyManagerError::Io)?;
            }
            Err(error) => return Err(KeyManagerError::Io(error)),
        }
        validate_path_components(&path)?;
        validate_secure_entry(&path, paths.expected_uid, paths.expected_gid, true)?;
    }
    Ok(())
}

fn validate_secure_entry(
    path: &Path,
    expected_uid: Option<u32>,
    expected_gid: Option<u32>,
    directory: bool,
) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(KeyManagerError::Io)?;
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(KeyManagerError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let expected_mode = if directory { 0o700 } else { 0o600 };
        if metadata.mode() & 0o7777 != expected_mode
            || expected_uid.is_some_and(|uid| metadata.uid() != uid)
            || expected_gid.is_some_and(|gid| metadata.gid() != gid)
        {
            return Err(KeyManagerError::UnsafePath);
        }
        if !directory && std::os::unix::fs::MetadataExt::nlink(&metadata) != 1 {
            return Err(KeyManagerError::UnsafePath);
        }
    }
    Ok(())
}

fn validate_open_directory(
    directory: &Dir,
    expected_uid: Option<u32>,
    expected_gid: Option<u32>,
) -> Result<()> {
    let metadata = directory.dir_metadata().map_err(KeyManagerError::Io)?;
    if !metadata.is_dir() {
        return Err(KeyManagerError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt as CapMetadataExt;
        if CapMetadataExt::mode(&metadata) & 0o7777 != 0o700
            || expected_uid.is_some_and(|uid| CapMetadataExt::uid(&metadata) != uid)
            || expected_gid.is_some_and(|gid| CapMetadataExt::gid(&metadata) != gid)
        {
            return Err(KeyManagerError::UnsafePath);
        }
    }
    Ok(())
}

fn validate_open_file(
    file: &File,
    expected_uid: Option<u32>,
    expected_gid: Option<u32>,
    mode: u32,
) -> Result<()> {
    let metadata = file.metadata().map_err(KeyManagerError::Io)?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(KeyManagerError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o7777 != mode
            || expected_uid.is_some_and(|uid| metadata.uid() != uid)
            || expected_gid.is_some_and(|gid| metadata.gid() != gid)
        {
            return Err(KeyManagerError::UnsafePath);
        }
    }
    Ok(())
}

struct SensitiveToml {
    table: Table,
}

impl Drop for SensitiveToml {
    fn drop(&mut self) {
        zeroize_table(&mut self.table);
    }
}

fn zeroize_table(table: &mut Table) {
    let values = std::mem::take(table);
    for (mut key, mut value) in values {
        key.zeroize();
        zeroize_value(&mut value);
    }
}

fn zeroize_value(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => {
            for value in values {
                zeroize_value(value);
            }
        }
        Value::Table(table) => zeroize_table(table),
        Value::Integer(_) | Value::Float(_) | Value::Boolean(_) | Value::Datetime(_) => {}
    }
}

fn parse_document(bytes: &[u8]) -> Result<SensitiveToml> {
    let text = std::str::from_utf8(bytes).map_err(|_| KeyManagerError::InvalidToml)?;
    let table = toml::from_str::<Table>(text).map_err(|_| KeyManagerError::InvalidToml)?;
    Ok(SensitiveToml { table })
}

fn serialize_table(table: &Table) -> Result<Zeroizing<Vec<u8>>> {
    let rendered = toml::to_string(table).map_err(|_| KeyManagerError::InvalidToml)?;
    Ok(Zeroizing::new(rendered.into_bytes()))
}

fn merge_missing(existing: Table, generated: &mut Table) {
    for (key, mut old_value) in existing {
        match generated.get_mut(&key) {
            Some(Value::Table(new_table)) => match old_value {
                Value::Table(old_table) => merge_missing(old_table, new_table),
                mut other => zeroize_value(&mut other),
            },
            Some(_) => zeroize_value(&mut old_value),
            None => {
                generated.insert(key, old_value);
            }
        }
    }
}

fn render_contact_preserving(
    existing: Option<&[u8]>,
    baseline: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let mut generated = parse_document(baseline)?;
    if let Some(existing_bytes) = existing {
        let mut old = parse_document(existing_bytes)?;
        let contacts = old.table.remove("contact");
        let old_table = std::mem::take(&mut old.table);
        merge_missing(old_table, &mut generated.table);
        if let Some(contacts) = contacts {
            generated.table.insert("contact".to_owned(), contacts);
        }
    }
    validate_document(&generated.table)?;
    let rendered = serialize_table(&generated.table)?;
    let check = parse_document(&rendered)?;
    validate_document(&check.table)?;
    validate_contact_uniqueness(&rendered)?;
    Ok(rendered)
}

fn validate_document(document: &Table) -> Result<()> {
    let Some(contact_item) = document.get("contact") else {
        return Ok(());
    };
    let contacts = contact_item
        .as_table()
        .ok_or(KeyManagerError::InvalidToml)?;
    let mut normalized = HashSet::new();
    for (name, contact_item) in contacts {
        validate_contact_name(name)?;
        if !normalized.insert(name.to_lowercase()) {
            return Err(KeyManagerError::InvalidContact);
        }
        let contact = contact_item
            .as_table()
            .ok_or(KeyManagerError::InvalidToml)?;
        require_contact_string(contact, "dm_chacha_public")?;
        require_contact_string(contact, "my_dm_chacha_secret")?;
    }
    Ok(())
}

fn require_contact_string<'a>(contact: &'a Table, field: &str) -> Result<&'a str> {
    contact
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(KeyManagerError::InvalidToml)
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}

fn public_fingerprint(encoded: &str) -> Result<String> {
    let decoded = bs58::decode(encoded)
        .into_vec()
        .map_err(|_| KeyManagerError::InvalidPublicKey)?;
    if decoded.len() != 32
        || decoded.iter().all(|byte| *byte == 0)
        || bs58::encode(&decoded).into_string() != encoded
    {
        return Err(KeyManagerError::InvalidPublicKey);
    }
    Ok(sha256(&decoded))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicFingerprint(String);

impl PublicFingerprint {
    pub fn parse(value: &str) -> Result<Self> {
        if !valid_hash(value) {
            return Err(KeyManagerError::InvalidPublicKey);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn from_encoded_public_key(encoded: &str) -> Result<Self> {
        Ok(Self(public_fingerprint(encoded)?))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn private_key_bytes(encoded: &str) -> Result<Zeroizing<Vec<u8>>> {
    let decoded = Zeroizing::new(
        bs58::decode(encoded)
            .into_vec()
            .map_err(|_| KeyManagerError::InvalidToml)?,
    );
    let canonical = Zeroizing::new(bs58::encode(decoded.as_slice()).into_string());
    if decoded.len() != 32 || decoded.iter().all(|byte| *byte == 0) || canonical.as_str() != encoded
    {
        return Err(KeyManagerError::InvalidToml);
    }
    Ok(decoded)
}

#[derive(Debug)]
struct ParsedContact {
    summary: ContactSummary,
    private_equality_tag: [u8; 32],
}

fn collect_contacts(config: &[u8], equality_key: &[u8; 32]) -> Result<Vec<ParsedContact>> {
    let document = parse_document(config)?;
    validate_document(&document.table)?;
    let Some(contact_item) = document.table.get("contact") else {
        return Ok(Vec::new());
    };
    let contacts = contact_item
        .as_table()
        .ok_or(KeyManagerError::InvalidToml)?;
    let mut parsed = Vec::with_capacity(contacts.len());
    for (name, item) in contacts.iter() {
        let contact = item.as_table().ok_or(KeyManagerError::InvalidToml)?;
        let peer_public = require_contact_string(contact, "dm_chacha_public")?;
        let private = require_contact_string(contact, "my_dm_chacha_secret")?;
        let private = private_key_bytes(private)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(equality_key)
            .map_err(|_| KeyManagerError::InvalidToml)?;
        mac.update(&private);
        let private_equality_tag: [u8; 32] = mac.finalize().into_bytes().into();
        parsed.push(ParsedContact {
            summary: ContactSummary {
                name: name.to_owned(),
                peer_fingerprint: public_fingerprint(peer_public)?,
                state: "legacy-unmanaged".to_owned(),
            },
            private_equality_tag,
        });
    }
    Ok(parsed)
}

fn duplicate_peer_fingerprints(contacts: &[ParsedContact]) -> HashSet<String> {
    let mut seen = HashSet::new();
    let mut duplicates = HashSet::new();
    for contact in contacts {
        if !seen.insert(contact.summary.peer_fingerprint.clone()) {
            duplicates.insert(contact.summary.peer_fingerprint.clone());
        }
    }
    duplicates
}

fn validate_contact_uniqueness(config: &[u8]) -> Result<()> {
    let equality_key = private_equality_key();
    let contacts = collect_contacts(config, &equality_key)?;
    if !reused_private_tags(&contacts).is_empty()
        || !duplicate_peer_fingerprints(&contacts).is_empty()
    {
        return Err(KeyManagerError::ContactConflict);
    }
    Ok(())
}

pub fn inspect_contacts(
    paths: &TenantPaths,
    expected_scope_id: Option<&str>,
) -> Result<Vec<ContactSummary>> {
    if let Some(scope_id) = expected_scope_id {
        validate_scope_id(scope_id)?;
    }
    let workspace = Workspace::open_read_only(paths.clone())?;
    let _snapshot = workspace.read_snapshot()?;
    let config = workspace
        .read_optional(CONFIG_FILE)?
        .ok_or(KeyManagerError::InvalidToml)?;
    let equality_key = private_equality_key();
    let mut contacts = collect_contacts(&config, &equality_key)?;
    let ledger = match workspace.read_optional(LEDGER_FILE)? {
        Some(bytes) => {
            let scope_id = expected_scope_id.ok_or(KeyManagerError::ScopeConflict)?;
            Some(parse_ledger(&bytes, scope_id)?)
        }
        None => None,
    };
    if let Some(ledger) = ledger {
        for contact in &mut contacts {
            let Some(entry) = ledger.contacts.get(&contact.summary.name) else {
                continue;
            };
            if entry.peer_fingerprint != contact.summary.peer_fingerprint {
                return Err(KeyManagerError::ContactConflict);
            }
            contact.summary.state.clone_from(&entry.state);
        }
    }
    Ok(contacts
        .into_iter()
        .map(|contact| contact.summary)
        .collect())
}

pub fn doctor_contacts(
    paths: &TenantPaths,
    expected_scope_id: Option<&str>,
) -> Result<DoctorReport> {
    if let Some(scope_id) = expected_scope_id {
        validate_scope_id(scope_id)?;
    }
    let workspace = Workspace::open_read_only(paths.clone())?;
    let _snapshot = workspace.read_snapshot()?;
    let config = workspace
        .read_optional(CONFIG_FILE)?
        .ok_or(KeyManagerError::InvalidToml)?;
    let equality_key = private_equality_key();
    let parsed = collect_contacts(&config, &equality_key)?;
    let mut contacts = parsed
        .iter()
        .map(|contact| contact.summary.clone())
        .collect::<Vec<_>>();
    let mut issues = Vec::new();

    let ledger = match workspace.read_optional(LEDGER_FILE)? {
        Some(bytes) => {
            let scope_id = expected_scope_id.ok_or(KeyManagerError::ScopeConflict)?;
            Some(parse_ledger(&bytes, scope_id)?)
        }
        None => None,
    };
    if let Some(ledger) = &ledger {
        for contact in &mut contacts {
            match ledger.contacts.get(&contact.name) {
                Some(entry) if entry.peer_fingerprint == contact.peer_fingerprint => {
                    contact.state.clone_from(&entry.state);
                }
                Some(_) => issues.push(DoctorIssue {
                    code: "ledger-fingerprint-mismatch".to_owned(),
                    contacts: vec![contact.name.clone()],
                }),
                None => issues.push(DoctorIssue {
                    code: "ledger-entry-missing".to_owned(),
                    contacts: vec![contact.name.clone()],
                }),
            }
        }
        let config_names = contacts
            .iter()
            .map(|contact| contact.name.as_str())
            .collect::<HashSet<_>>();
        for name in ledger.contacts.keys() {
            if !config_names.contains(name.as_str()) {
                issues.push(DoctorIssue {
                    code: "ledger-entry-orphan".to_owned(),
                    contacts: vec![name.clone()],
                });
            }
        }
    } else if !contacts.is_empty() {
        issues.push(DoctorIssue {
            code: "ledger-missing".to_owned(),
            contacts: contacts
                .iter()
                .map(|contact| contact.name.clone())
                .collect(),
        });
    }

    if !reused_private_tags(&parsed).is_empty() {
        issues.push(DoctorIssue {
            code: "private-key-reuse".to_owned(),
            contacts: parsed
                .iter()
                .map(|contact| contact.summary.name.clone())
                .collect(),
        });
    }
    if !duplicate_peer_fingerprints(&parsed).is_empty() {
        issues.push(DoctorIssue {
            code: "peer-key-reuse".to_owned(),
            contacts: parsed
                .iter()
                .map(|contact| contact.summary.name.clone())
                .collect(),
        });
    }

    let document = parse_document(&config)?;
    if let Some(Value::Table(contact_table)) = document.table.get("contact") {
        for (name, value) in contact_table {
            let Some(contact) = value.as_table() else {
                continue;
            };
            let unknown = contact
                .keys()
                .filter(|field| *field != "dm_chacha_public" && *field != "my_dm_chacha_secret")
                .cloned()
                .collect::<Vec<_>>();
            if !unknown.is_empty() {
                issues.push(DoctorIssue {
                    code: "unknown-contact-fields".to_owned(),
                    contacts: vec![format!("{name}:{}", unknown.join(","))],
                });
            }
        }
    }

    for relative in [CANDIDATES_DIR, TRANSACTIONS_DIR, PENDING_DIR, ROLLBACK_DIR] {
        let path = paths.darkirc_dir.join(relative);
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                issues.push(DoctorIssue {
                    code: "unsafe-transaction-path".to_owned(),
                    contacts: vec![relative.to_owned()],
                });
            } else if fs::read_dir(&path)
                .map_err(KeyManagerError::Io)?
                .next()
                .transpose()
                .map_err(KeyManagerError::Io)?
                .is_some()
            {
                issues.push(DoctorIssue {
                    code: "transactional-state-present".to_owned(),
                    contacts: vec![relative.to_owned()],
                });
            }
        }
    }
    issues.sort_by(|left, right| left.code.cmp(&right.code));
    Ok(DoctorReport { contacts, issues })
}

fn empty_ledger(scope_id: &str) -> Ledger {
    Ledger {
        schema: LEDGER_SCHEMA.to_owned(),
        scope_id: scope_id.to_owned(),
        contacts: BTreeMap::new(),
    }
}

fn parse_ledger(bytes: &[u8], scope_id: &str) -> Result<Ledger> {
    let ledger = parse_ledger_unscoped(bytes)?;
    if ledger.scope_id != scope_id {
        return Err(KeyManagerError::ScopeConflict);
    }
    Ok(ledger)
}

fn parse_ledger_unscoped(bytes: &[u8]) -> Result<Ledger> {
    let ledger: Ledger =
        serde_json::from_slice(bytes).map_err(|_| KeyManagerError::InvalidLedger)?;
    validate_ledger(&ledger)?;
    Ok(ledger)
}

fn validate_ledger(ledger: &Ledger) -> Result<()> {
    if ledger.schema != LEDGER_SCHEMA || validate_scope_id(&ledger.scope_id).is_err() {
        return Err(KeyManagerError::InvalidLedger);
    }
    for (name, contact) in &ledger.contacts {
        validate_contact_name(name).map_err(|_| KeyManagerError::InvalidLedger)?;
        if !matches!(
            contact.state.as_str(),
            "legacy-active"
                | "legacy-noncompliant"
                | "Installing"
                | "Installed"
                | "LocallyActivated"
                | "PeerVerified"
                | "VerificationOverdue"
                | "RolledBack"
                | "Expired"
                | "Cancelled"
                | "Revoked"
        ) || !valid_hash(&contact.peer_fingerprint)
        {
            return Err(KeyManagerError::InvalidLedger);
        }
        if contact.state.starts_with("legacy-") && contact.generation != 0 {
            return Err(KeyManagerError::InvalidLedger);
        }
    }
    Ok(())
}

fn ledger_bytes(ledger: &Ledger) -> Result<Vec<u8>> {
    let mut bytes =
        serde_json::to_vec_pretty(ledger).map_err(|_| KeyManagerError::InvalidLedger)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn validate_state_directory(workspace: &Workspace, relative: &str) -> Result<()> {
    let path = workspace.paths.darkirc_dir.join(relative);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            validate_path_components(&path)?;
            validate_secure_entry(
                &path,
                workspace.paths.expected_uid,
                workspace.paths.expected_gid,
                true,
            )?;
            let directory = workspace.open_relative_dir(relative)?;
            let mut entries = directory.read_dir(".").map_err(KeyManagerError::Io)?;
            if entries
                .next()
                .transpose()
                .map_err(KeyManagerError::Io)?
                .is_some()
            {
                return Err(KeyManagerError::RecoveryRequired);
            }
            let _ = metadata;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(KeyManagerError::Io(error)),
    }
}

fn ensure_migration_settled(workspace: &Workspace) -> Result<()> {
    ensure_migration_directories_settled(workspace)?;
    for relative in [
        STAGED_MIGRATION_FILE,
        MIGRATION_FILE_NEXT,
        STAGED_MIGRATION_FILE_NEXT,
    ] {
        if workspace.read_optional(relative)?.is_some() {
            return Err(KeyManagerError::RecoveryRequired);
        }
    }
    Ok(())
}

fn ensure_migration_directories_settled(workspace: &Workspace) -> Result<()> {
    for relative in [CANDIDATES_DIR, TRANSACTIONS_DIR, PENDING_DIR, ROLLBACK_DIR] {
        validate_state_directory(workspace, relative)?;
    }
    Ok(())
}

fn contacts_toml(config: &[u8]) -> Result<SecretString> {
    let mut document = parse_document(config)?;
    validate_document(&document.table)?;
    let mut contact_document = SensitiveToml {
        table: Table::new(),
    };
    if let Some(contact) = document.table.remove("contact") {
        contact_document.table.insert("contact".to_owned(), contact);
    }
    let rendered =
        toml::to_string(&contact_document.table).map_err(|_| KeyManagerError::InvalidToml)?;
    let parsed = parse_document(rendered.as_bytes())?;
    validate_document(&parsed.table)?;
    enforce_migration_contact_limit(&parsed.table)?;
    let equality_key = [0_u8; 32];
    collect_contacts(rendered.as_bytes(), &equality_key)?;
    Ok(rendered.into())
}

fn validate_manifest_ledger(
    config: &[u8],
    ledger: &Ledger,
    require_legacy_state: bool,
) -> Result<()> {
    validate_ledger(ledger)?;
    let contacts = collect_contacts(config, &[0_u8; 32])?;
    if ledger.contacts.len() != contacts.len() {
        return Err(KeyManagerError::ContactConflict);
    }
    for contact in contacts {
        let Some(entry) = ledger.contacts.get(&contact.summary.name) else {
            return Err(KeyManagerError::ContactConflict);
        };
        if entry.peer_fingerprint != contact.summary.peer_fingerprint {
            return Err(KeyManagerError::ContactConflict);
        }
        if require_legacy_state
            && !matches!(
                entry.state.as_str(),
                "legacy-active" | "legacy-noncompliant"
            )
        {
            return Err(KeyManagerError::ContactConflict);
        }
    }
    Ok(())
}

// Baseline regeneration must not make explicit adoption a prerequisite for
// every future patch.  A ledger entry is authoritative once present, while a
// contact absent from the ledger is still a legacy contact awaiting adoption.
fn validate_baseline_ledger(config: &[u8], ledger: &Ledger) -> Result<()> {
    validate_ledger(ledger)?;
    let contacts = collect_contacts(config, &[0_u8; 32])?;
    let by_name = contacts
        .into_iter()
        .map(|contact| (contact.summary.name, contact.summary.peer_fingerprint))
        .collect::<HashMap<_, _>>();
    for (name, entry) in &ledger.contacts {
        let Some(fingerprint) = by_name.get(name) else {
            return Err(KeyManagerError::ContactConflict);
        };
        if fingerprint != &entry.peer_fingerprint {
            return Err(KeyManagerError::ContactConflict);
        }
    }
    Ok(())
}

fn parse_migration_contacts(value: &str) -> Result<SensitiveToml> {
    let document = parse_document(value.as_bytes()).map_err(|_| KeyManagerError::InvalidLedger)?;
    if document.table.iter().any(|(key, _)| key != "contact") {
        return Err(KeyManagerError::InvalidLedger);
    }
    validate_document(&document.table).map_err(|_| KeyManagerError::InvalidLedger)?;
    enforce_migration_contact_limit(&document.table).map_err(|_| KeyManagerError::InvalidLedger)?;
    let equality_key = [0_u8; 32];
    collect_contacts(value.as_bytes(), &equality_key)
        .map_err(|_| KeyManagerError::InvalidLedger)?;
    validate_contact_uniqueness(value.as_bytes()).map_err(|_| KeyManagerError::InvalidLedger)?;
    Ok(document)
}

fn enforce_migration_contact_limit(document: &Table) -> Result<()> {
    let count = document
        .get("contact")
        .and_then(Value::as_table)
        .map_or(0, Table::len);
    if count > MAX_MIGRATION_CONTACTS {
        return Err(KeyManagerError::InvalidLedger);
    }
    Ok(())
}

fn migration_bytes(manifest: &MigrationManifest) -> Result<Zeroizing<Vec<u8>>> {
    let mut bytes =
        serde_json::to_vec_pretty(manifest).map_err(|_| KeyManagerError::InvalidLedger)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_MIGRATION_BYTES {
        bytes.zeroize();
        return Err(KeyManagerError::InvalidLedger);
    }
    Ok(Zeroizing::new(bytes))
}

fn parse_migration_manifest(
    bytes: &[u8],
    attestation: &CompatibilityAttestation,
) -> Result<MigrationManifest> {
    if bytes.len() > MAX_MIGRATION_BYTES {
        return Err(KeyManagerError::InvalidLedger);
    }
    let manifest: MigrationManifest =
        serde_json::from_slice(bytes).map_err(|_| KeyManagerError::InvalidLedger)?;
    if manifest.schema != MIGRATION_SCHEMA {
        return Err(KeyManagerError::InvalidLedger);
    }
    let manifest_attestation = CompatibilityAttestation::new(
        &manifest.generator_profile,
        &manifest.binary_sha256,
        &manifest.key_format,
        &manifest.source_revision,
    )?;
    if &manifest_attestation != attestation {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    validate_scope_id(&manifest.scope_id).map_err(|_| KeyManagerError::InvalidLedger)?;
    let contacts_toml = manifest.contacts_toml.expose_secret();
    let _contacts = parse_migration_contacts(contacts_toml)?;
    if sha256(contacts_toml.as_bytes()) != manifest.contacts_hash {
        return Err(KeyManagerError::InvalidLedger);
    }
    let ledger_bytes = ledger_bytes(&manifest.ledger)?;
    if sha256(&ledger_bytes) != manifest.ledger_hash {
        return Err(KeyManagerError::InvalidLedger);
    }
    validate_ledger(&manifest.ledger).map_err(|_| KeyManagerError::InvalidLedger)?;
    if manifest.ledger.scope_id != manifest.scope_id {
        return Err(KeyManagerError::InvalidLedger);
    }
    validate_manifest_ledger(contacts_toml.as_bytes(), &manifest.ledger, true)
        .map_err(|_| KeyManagerError::InvalidLedger)?;
    Ok(manifest)
}

fn parse_migration_manifest_unbound(bytes: &[u8]) -> Result<MigrationManifest> {
    if bytes.len() > MAX_MIGRATION_BYTES {
        return Err(KeyManagerError::InvalidLedger);
    }
    let raw: MigrationManifest =
        serde_json::from_slice(bytes).map_err(|_| KeyManagerError::InvalidLedger)?;
    let attestation = CompatibilityAttestation::new(
        &raw.generator_profile,
        &raw.binary_sha256,
        &raw.key_format,
        &raw.source_revision,
    )?;
    parse_migration_manifest(bytes, &attestation)
}

pub fn validate_migration(
    bytes: &[u8],
    attestation: &CompatibilityAttestation,
) -> Result<MigrationValidation> {
    let manifest = parse_migration_manifest(bytes, attestation)?;
    Ok(MigrationValidation {
        scope_id: manifest.scope_id,
    })
}

pub fn validate_migration_unbound(bytes: &[u8]) -> Result<MigrationValidation> {
    let manifest = parse_migration_manifest_unbound(bytes)?;
    Ok(MigrationValidation {
        scope_id: manifest.scope_id,
    })
}

fn replace_file(workspace: &Workspace, path: &str, bytes: &[u8]) -> Result<()> {
    let candidate = format!("{path}.next");
    workspace.remove_if_present(&candidate)?;
    workspace.write_new(&candidate, bytes)?;
    workspace.rename(&candidate, path)
}

pub fn read_ledger(paths: &TenantPaths, scope_id: &str) -> Result<Ledger> {
    validate_scope_id(scope_id)?;
    let workspace = Workspace::open_read_only(paths.clone())?;
    let _snapshot = workspace.read_snapshot()?;
    let bytes = workspace
        .read_optional(LEDGER_FILE)?
        .ok_or(KeyManagerError::InvalidLedger)?;
    parse_ledger(&bytes, scope_id)
}

pub fn export_migration(
    paths: &TenantPaths,
    scope_id: &str,
    attestation: &CompatibilityAttestation,
) -> Result<()> {
    validate_scope_id(scope_id)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;

    let config = workspace
        .read_optional(CONFIG_FILE)?
        .ok_or(KeyManagerError::InvalidToml)?;
    let ledger = workspace
        .read_optional(LEDGER_FILE)?
        .ok_or(KeyManagerError::InvalidLedger)?;
    let ledger = parse_ledger(&ledger, scope_id)?;
    let contacts = contacts_toml(&config)?;
    validate_manifest_ledger(contacts.expose_secret().as_bytes(), &ledger, true)?;
    let ledger = ledger_bytes(&ledger)?;
    let manifest = MigrationManifest {
        schema: MIGRATION_SCHEMA.to_owned(),
        scope_id: scope_id.to_owned(),
        generator_profile: attestation.generator_profile.clone(),
        binary_sha256: attestation.binary_sha256.clone(),
        key_format: attestation.key_format.clone(),
        source_revision: attestation.source_revision.clone(),
        contacts_hash: sha256(contacts.expose_secret().as_bytes()),
        contacts_toml: contacts,
        ledger: serde_json::from_slice(&ledger).map_err(|_| KeyManagerError::InvalidLedger)?,
        ledger_hash: sha256(&ledger),
    };
    let bytes = migration_bytes(&manifest)?;
    if let Some(existing) = workspace.read_optional(MIGRATION_FILE)? {
        if existing == bytes {
            return Ok(());
        }
        return Err(KeyManagerError::ContactConflict);
    }
    replace_file(&workspace, MIGRATION_FILE, &bytes)
}

pub fn stage_migration(
    paths: &TenantPaths,
    scope_id: &str,
    manifest_bytes: &[u8],
    attestation: &CompatibilityAttestation,
) -> Result<()> {
    validate_scope_id(scope_id)?;
    let manifest = parse_migration_manifest(manifest_bytes, attestation)?;
    if manifest.scope_id != scope_id {
        return Err(KeyManagerError::ScopeConflict);
    }
    let canonical = migration_bytes(&manifest)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_directories_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;

    if let Some(candidate) = workspace.read_optional(STAGED_MIGRATION_FILE_NEXT)? {
        // Only the byte-identical, fully validated retry may consume an
        // interrupted staged candidate. Foreign, malformed, or conflicting
        // secret-bearing candidates remain untouched for operator recovery.
        parse_migration_manifest(&candidate, attestation)?;
        if candidate != canonical {
            return Err(KeyManagerError::ContactConflict);
        }
        match workspace.read_optional(STAGED_MIGRATION_FILE)? {
            Some(existing) if existing == canonical => {
                workspace.remove_if_present(STAGED_MIGRATION_FILE_NEXT)?;
                return Ok(());
            }
            Some(_) => return Err(KeyManagerError::ContactConflict),
            None => {
                workspace.rename(STAGED_MIGRATION_FILE_NEXT, STAGED_MIGRATION_FILE)?;
                return Ok(());
            }
        }
    }

    match workspace.read_optional(STAGED_MIGRATION_FILE)? {
        Some(existing) if existing == canonical => Ok(()),
        Some(_) => Err(KeyManagerError::ContactConflict),
        None => replace_file(&workspace, STAGED_MIGRATION_FILE, &canonical),
    }
}

fn merge_migration_contacts(target: &[u8], source: &str) -> Result<Zeroizing<Vec<u8>>> {
    let mut target_document = parse_document(target)?;
    validate_document(&target_document.table)?;
    let mut source_document = parse_migration_contacts(source)?;
    let Some(source_item) = source_document.table.remove("contact") else {
        return Ok(Zeroizing::new(target.to_vec()));
    };
    let source_contacts = match source_item {
        Value::Table(source_contacts) => source_contacts,
        mut other => {
            zeroize_value(&mut other);
            return Err(KeyManagerError::InvalidLedger);
        }
    };
    if source_contacts.is_empty() {
        return Ok(Zeroizing::new(target.to_vec()));
    }

    if target_document.table.get("contact").is_none() {
        target_document
            .table
            .insert("contact".to_owned(), Value::Table(Table::new()));
    }
    let target_item = target_document
        .table
        .get_mut("contact")
        .ok_or(KeyManagerError::InvalidLedger)?;
    let target_contacts = target_item
        .as_table_mut()
        .ok_or(KeyManagerError::InvalidLedger)?;
    for (name, mut source_contact) in source_contacts {
        if let Some(target_contact) = target_contacts.get(&name) {
            if target_contact != &source_contact {
                zeroize_value(&mut source_contact);
                return Err(KeyManagerError::ContactConflict);
            }
            zeroize_value(&mut source_contact);
        } else {
            target_contacts.insert(name, source_contact);
        }
    }
    validate_document(&target_document.table)?;
    let rendered = serialize_table(&target_document.table)?;
    let check = parse_document(&rendered)?;
    validate_document(&check.table)?;
    validate_contact_uniqueness(&rendered)?;
    Ok(rendered)
}

fn reject_private_reuse(target: &[u8], source: &str) -> Result<()> {
    let equality_key = private_equality_key();
    let target_contacts = collect_contacts(target, &equality_key)?;
    let source_contacts = collect_contacts(source.as_bytes(), &equality_key)?;
    if !reused_private_tags(&target_contacts).is_empty()
        || !reused_private_tags(&source_contacts).is_empty()
    {
        return Err(KeyManagerError::ContactConflict);
    }
    let mut target_tags = HashMap::with_capacity(target_contacts.len());
    for target_contact in &target_contacts {
        target_tags.insert(
            target_contact.private_equality_tag,
            target_contact.summary.name.as_str(),
        );
    }
    for source_contact in &source_contacts {
        if target_tags
            .get(&source_contact.private_equality_tag)
            .is_some_and(|target_name| *target_name != source_contact.summary.name)
        {
            return Err(KeyManagerError::ContactConflict);
        }
    }
    Ok(())
}

fn merge_migration_ledger(target: &mut Ledger, source: &Ledger) -> Result<()> {
    for (name, source_contact) in &source.contacts {
        match target.contacts.get(name) {
            Some(target_contact) if target_contact != source_contact => {
                return Err(KeyManagerError::ContactConflict);
            }
            Some(_) => {}
            None => {
                target.contacts.insert(name.clone(), source_contact.clone());
            }
        }
    }
    Ok(())
}

pub fn import_migration(
    paths: &TenantPaths,
    scope_id: &str,
    attestation: &CompatibilityAttestation,
) -> Result<UpdateResult> {
    validate_scope_id(scope_id)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    if workspace
        .read_optional(STAGED_MIGRATION_FILE_NEXT)?
        .is_some()
    {
        return Err(KeyManagerError::RecoveryRequired);
    }
    let manifest_bytes = workspace.read_required(STAGED_MIGRATION_FILE)?;
    let manifest = parse_migration_manifest(&manifest_bytes, attestation)?;
    if manifest.scope_id != scope_id {
        return Err(KeyManagerError::ScopeConflict);
    }
    ensure_migration_directories_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;

    let old_config = workspace
        .read_optional(CONFIG_FILE)?
        .ok_or(KeyManagerError::InvalidToml)?;
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    let mut target_ledger = match old_ledger.as_ref().map(|bytes| bytes.as_slice()) {
        Some(bytes) => parse_ledger(bytes, scope_id)?,
        None => empty_ledger(scope_id),
    };
    reject_private_reuse(&old_config, manifest.contacts_toml.expose_secret())?;
    merge_migration_ledger(&mut target_ledger, &manifest.ledger)?;
    let new_config = merge_migration_contacts(&old_config, manifest.contacts_toml.expose_secret())?;
    validate_manifest_ledger(&new_config, &target_ledger, false)?;
    let new_ledger = ledger_bytes(&target_ledger)?;
    let result = commit_transaction(
        &workspace,
        scope_id,
        "migration-import",
        Some(&old_config),
        old_ledger.as_ref().map(|bytes| bytes.as_slice()),
        &new_config,
        &new_ledger,
        None,
    )?;
    workspace.remove_if_present(STAGED_MIGRATION_FILE)?;
    Ok(result)
}

pub fn migration_ready(paths: &TenantPaths, scope_id: &str) -> Result<()> {
    validate_scope_id(scope_id)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;
    let config = workspace
        .read_optional(CONFIG_FILE)?
        .ok_or(KeyManagerError::InvalidToml)?;
    let document = parse_document(&config)?;
    validate_document(&document.table)?;
    validate_contact_uniqueness(&config)?;
    let ledger = workspace
        .read_optional(LEDGER_FILE)?
        .ok_or(KeyManagerError::InvalidLedger)?;
    let ledger = parse_ledger(&ledger, scope_id)?;
    validate_manifest_ledger(&config, &ledger, false)?;
    Ok(())
}

pub fn update_baseline(
    paths: &TenantPaths,
    scope_id: &str,
    baseline: &[u8],
) -> Result<UpdateResult> {
    update_baseline_at(paths, scope_id, baseline, None)
}

pub fn update_baseline_at(
    paths: &TenantPaths,
    scope_id: &str,
    baseline: &[u8],
    crash_point: Option<CrashPoint>,
) -> Result<UpdateResult> {
    validate_scope_id(scope_id)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;

    let old_config = workspace.read_optional(CONFIG_FILE)?;
    let new_config =
        render_contact_preserving(old_config.as_ref().map(|bytes| bytes.as_slice()), baseline)?;
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    let ledger = match old_ledger.as_ref().map(|bytes| bytes.as_slice()) {
        Some(bytes) => parse_ledger(bytes, scope_id)?,
        None => empty_ledger(scope_id),
    };
    if old_ledger.is_some() {
        validate_baseline_ledger(&new_config, &ledger)?;
    }
    let new_ledger = ledger_bytes(&ledger)?;

    commit_transaction(
        &workspace,
        scope_id,
        "baseline-update",
        old_config.as_ref().map(|bytes| bytes.as_slice()),
        old_ledger.as_ref().map(|bytes| bytes.as_slice()),
        &new_config,
        &new_ledger,
        crash_point,
    )
}

pub fn adopt_contacts(paths: &TenantPaths, scope_id: &str) -> Result<UpdateResult> {
    adopt_contacts_at(paths, scope_id, None)
}

pub fn adopt_contacts_at(
    paths: &TenantPaths,
    scope_id: &str,
    crash_point: Option<CrashPoint>,
) -> Result<UpdateResult> {
    validate_scope_id(scope_id)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;

    let config = workspace
        .read_optional(CONFIG_FILE)?
        .ok_or(KeyManagerError::InvalidToml)?;
    let equality_key = private_equality_key();
    let contacts = collect_contacts(&config, &equality_key)?;
    if !reused_private_tags(&contacts).is_empty()
        || !duplicate_peer_fingerprints(&contacts).is_empty()
    {
        return Err(KeyManagerError::ContactConflict);
    }
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    let mut ledger = match old_ledger.as_ref().map(|bytes| bytes.as_slice()) {
        Some(bytes) => parse_ledger(bytes, scope_id)?,
        None => empty_ledger(scope_id),
    };

    for contact in contacts {
        let proposed = LedgerContact {
            state: "legacy-noncompliant".to_owned(),
            peer_fingerprint: contact.summary.peer_fingerprint,
            generation: 0,
            contact_id: String::new(),
            local_fingerprint: String::new(),
            activation_deadline: 0,
            rollback_tx_id: String::new(),
            rollback_contact_toml_hash: String::new(),
            pending_offer_id: String::new(),
            compromised: false,
            revocation_generation: 0,
        };
        match ledger.contacts.get(&contact.summary.name) {
            Some(existing) if existing != &proposed => {
                return Err(KeyManagerError::ContactConflict);
            }
            Some(_) => {}
            None => {
                ledger.contacts.insert(contact.summary.name, proposed);
            }
        }
    }

    if ledger.contacts.len() != collect_contacts(&config, &[0_u8; 32])?.len() {
        return Err(KeyManagerError::ContactConflict);
    }

    let new_ledger = ledger_bytes(&ledger)?;
    commit_transaction(
        &workspace,
        scope_id,
        "legacy-adopt",
        Some(&config),
        old_ledger.as_ref().map(|bytes| bytes.as_slice()),
        &config,
        &new_ledger,
        crash_point,
    )
}

fn reused_private_tags(contacts: &[ParsedContact]) -> HashSet<[u8; 32]> {
    let mut once = HashSet::new();
    let mut reused = HashSet::new();
    for contact in contacts {
        if !once.insert(contact.private_equality_tag) {
            reused.insert(contact.private_equality_tag);
        }
    }
    reused
}

fn private_equality_key() -> Zeroizing<[u8; 32]> {
    let mut key = Zeroizing::new([0_u8; 32]);
    rand::rngs::OsRng.fill_bytes(&mut *key);
    key
}

#[allow(clippy::too_many_arguments)]
fn commit_transaction(
    workspace: &Workspace,
    scope_id: &str,
    operation: &str,
    old_config: Option<&[u8]>,
    old_ledger: Option<&[u8]>,
    new_config: &[u8],
    new_ledger: &[u8],
    crash_point: Option<CrashPoint>,
) -> Result<UpdateResult> {
    let config_hash = sha256(new_config);
    let ledger_hash = sha256(new_ledger);
    if old_config == Some(new_config) && old_ledger == Some(new_ledger) {
        return Ok(UpdateResult {
            transaction_id: None,
            config_hash,
            ledger_hash,
            changed: false,
        });
    }

    let transaction_id = Uuid::new_v4().simple().to_string();
    let config_candidate = format!("{CANDIDATES_DIR}/{transaction_id}.toml");
    let ledger_candidate = format!("{CANDIDATES_DIR}/{transaction_id}.ledger.json");
    let journal_path = format!("{TRANSACTIONS_DIR}/{transaction_id}.json");

    workspace.write_new(&config_candidate, new_config)?;
    workspace.write_new(&ledger_candidate, new_ledger)?;
    validate_transaction_candidates(
        workspace,
        scope_id,
        &config_candidate,
        &ledger_candidate,
        &config_hash,
        &ledger_hash,
    )?;
    let mut journal = JournalFile {
        schema: "lunarwing.darkirc-transaction/v1".to_owned(),
        transaction_id: transaction_id.clone(),
        operation: operation.to_owned(),
        scope_id: scope_id.to_owned(),
        old_config_hash: old_config.map(sha256),
        old_ledger_hash: old_ledger.map(sha256),
        new_config_hash: config_hash.clone(),
        new_ledger_hash: ledger_hash.clone(),
        config_candidate: config_candidate.clone(),
        ledger_candidate: ledger_candidate.clone(),
        phase: JournalPhase::Prepared.as_str().to_owned(),
        contact_id: String::new(),
        offer_id: String::new(),
        peer_contact_id: String::new(),
        transcript: String::new(),
        generation: 0,
        operation_kind: String::new(),
        rollback_contact_toml_hash: String::new(),
        confirmed_peer_fingerprint: String::new(),
        rollback_tx_id: String::new(),
        contact_name: String::new(),
        peer_generation: 0,
    };
    write_journal(workspace, &journal_path, &journal)?;
    inject_crash(crash_point, CrashPoint::AfterJournal)?;

    workspace.rename(&ledger_candidate, LEDGER_FILE)?;
    journal.phase = JournalPhase::LedgerCommitted.as_str().to_owned();
    replace_journal(workspace, &journal_path, &journal)?;
    inject_crash(crash_point, CrashPoint::AfterLedger)?;

    workspace.rename(&config_candidate, CONFIG_FILE)?;
    journal.phase = JournalPhase::ConfigCommitted.as_str().to_owned();
    replace_journal(workspace, &journal_path, &journal)?;
    inject_crash(crash_point, CrashPoint::AfterConfig)?;

    finish_journal(workspace, &journal_path, &journal)?;
    Ok(UpdateResult {
        transaction_id: Some(transaction_id),
        config_hash,
        ledger_hash,
        changed: true,
    })
}

fn validate_transaction_candidates(
    workspace: &Workspace,
    scope_id: &str,
    config_path: &str,
    ledger_path: &str,
    config_hash: &str,
    ledger_hash: &str,
) -> Result<()> {
    let config = workspace.read_required(config_path)?;
    let document = parse_document(&config)?;
    validate_document(&document.table)?;
    validate_contact_uniqueness(&config)?;
    if sha256(&config) != config_hash {
        return Err(KeyManagerError::RecoveryRequired);
    }

    let ledger = workspace.read_required(ledger_path)?;
    parse_ledger(&ledger, scope_id)?;
    if sha256(&ledger) != ledger_hash {
        return Err(KeyManagerError::RecoveryRequired);
    }
    Ok(())
}

fn inject_crash(requested: Option<CrashPoint>, current: CrashPoint) -> Result<()> {
    if requested == Some(current) {
        return Err(KeyManagerError::CrashInjected);
    }
    Ok(())
}

fn journal_bytes(journal: &JournalFile) -> Result<Vec<u8>> {
    let mut bytes =
        serde_json::to_vec_pretty(journal).map_err(|_| KeyManagerError::InvalidLedger)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn write_journal(workspace: &Workspace, path: &str, journal: &JournalFile) -> Result<()> {
    let candidate = format!("{path}.next");
    workspace.remove_if_present(&candidate)?;
    workspace.write_new(&candidate, &journal_bytes(journal)?)?;
    workspace.rename(&candidate, path)
}

fn replace_journal(workspace: &Workspace, path: &str, journal: &JournalFile) -> Result<()> {
    let candidate = format!("{path}.next");
    workspace.remove_if_present(&candidate)?;
    workspace.write_new(&candidate, &journal_bytes(journal)?)?;
    workspace.rename(&candidate, path)
}

fn finish_journal(workspace: &Workspace, path: &str, journal: &JournalFile) -> Result<()> {
    workspace.remove_if_present(&journal.config_candidate)?;
    workspace.remove_if_present(&journal.ledger_candidate)?;
    workspace.remove_if_present(path)?;
    sync_dir_path(&workspace.paths.darkirc_dir)
}

fn canonical_artifact_bytes(artifact: &impl serde::Serialize) -> Result<Vec<u8>> {
    serde_json::to_vec(artifact).map_err(|_| KeyManagerError::InvalidExchangeArtifact)
}

fn compute_transcript(initiator: &[u8], responder: &[u8]) -> Result<String> {
    if initiator.len() > MAX_PENDING_BYTES || responder.len() > MAX_PENDING_BYTES {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    let mut hasher = Sha256::new();
    hasher.update(EXCHANGE_V1_DOMAIN_SEPARATOR.as_bytes());
    hasher.update(initiator);
    hasher.update(responder);
    let mut bytes = Vec::from("sha256:");
    bytes.extend(format!("{:x}", hasher.finalize()).into_bytes());
    String::from_utf8(bytes).map_err(|_| KeyManagerError::InvalidExchangeArtifact)
}

fn validate_offer_id(value: &str) -> Result<()> {
    if value.is_empty() || value.len() != 32 || !value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    Ok(())
}

pub fn validate_public_exchange(bytes: &[u8], expected_role: ExchangeRole) -> Result<ValidatedPublicArtifact> {
    if bytes.len() > MAX_PENDING_BYTES {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
    let schema = value.get("schema").and_then(|v| v.as_str()).ok_or(KeyManagerError::InvalidExchangeArtifact)?;
    let version = value.get("version").and_then(|v| v.as_str()).ok_or(KeyManagerError::InvalidExchangeArtifact)?;
    let role = value.get("artifact_role").and_then(|v| v.as_str()).ok_or(KeyManagerError::InvalidExchangeArtifact)?;
    let observed = match role {
        "initiator_offer" => ExchangeRole::InitiatorOffer,
        "responder_response" => ExchangeRole::ResponderResponse,
        _ => return Err(KeyManagerError::InvalidExchangeArtifact),
    };
    if observed != expected_role {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    if schema != EXCHANGE_SCHEMA {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    if version != EXCHANGE_SCHEMA_VERSION {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    let generator_profile = value.get("generator_profile").and_then(|v| v.as_str()).ok_or(KeyManagerError::InvalidExchangeArtifact)?;
    let binary_sha256 = value.get("binary_sha256").and_then(|v| v.as_str()).ok_or(KeyManagerError::InvalidExchangeArtifact)?;
    let key_format = value.get("key_format").and_then(|v| v.as_str()).ok_or(KeyManagerError::InvalidExchangeArtifact)?;
    let source_revision = value.get("source_revision").and_then(|v| v.as_str()).ok_or(KeyManagerError::InvalidExchangeArtifact)?;
    if generator_profile != MIGRATION_GENERATOR_PROFILE || key_format != MIGRATION_KEY_FORMAT || source_revision != MIGRATION_SOURCE_REVISION {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    if !valid_hash(binary_sha256) {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    let offer_id = value.get("offer_id").and_then(|v| v.as_str()).ok_or(KeyManagerError::InvalidExchangeArtifact)?;
    let sender_contact_id = value.get("sender_contact_id").and_then(|v| v.as_str()).ok_or(KeyManagerError::InvalidExchangeArtifact)?;
    validate_offer_id(offer_id)?;
    validate_offer_id(sender_contact_id)?;
    let public_key = value.get("public_key").and_then(|v| v.as_str()).ok_or(KeyManagerError::InvalidExchangeArtifact)?;
    let public_fingerprint_field = value.get("public_fingerprint").and_then(|v| v.as_str()).ok_or(KeyManagerError::InvalidExchangeArtifact)?;
    let computed = public_fingerprint(public_key)?;
    if computed != public_fingerprint_field {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    let generation = value.get("generation").and_then(|v| v.as_u64()).ok_or(KeyManagerError::InvalidExchangeArtifact)?;
    let created_at = value.get("created_at").and_then(|v| v.as_i64()).ok_or(KeyManagerError::InvalidExchangeArtifact)?;
    let expires_at = value.get("expires_at").and_then(|v| v.as_i64()).ok_or(KeyManagerError::InvalidExchangeArtifact)?;
    if expires_at <= created_at {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    let intended_peer_fingerprint = value.get("intended_peer_fingerprint").and_then(|v| v.as_str());
    if let Some(peer_f) = intended_peer_fingerprint {
        if !valid_hash(peer_f) {
            return Err(KeyManagerError::InvalidExchangeArtifact);
        }
    }
    let canonical = canonical_artifact_bytes(&value)?;
    Ok(ValidatedPublicArtifact {
        role: observed,
        offer_id: offer_id.to_owned(),
        sender_contact_id: sender_contact_id.to_owned(),
        intended_peer_contact_id: value.get("intended_peer_contact_id").and_then(|v| v.as_str()).map(|s| s.to_owned()),
        public_fingerprint: PublicFingerprint::parse(&computed)?,
        generation,
        created_at,
        expires_at,
        canonical,
        in_reply_to: value.get("in_reply_to").and_then(|v| v.as_str()).map(|s| s.to_owned()),
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// Exchange state machine: Stage A (Public Exchange V1),
// Stage C (Activation & Rollback), Stage B (Rotation & Revocation)
// ═══════════════════════════════════════════════════════════════════════════

fn offer_id_path(workspace: &Workspace, offer_id: &str) -> Result<String> {
    validate_offer_id(offer_id)?;
    Ok(format!("{}/{}.json", PENDING_DIR, offer_id))
}

fn rollback_path(workspace: &Workspace, transaction_id: &str) -> Result<String> {
    validate_transaction_id(transaction_id)?;
    Ok(format!("{}/{}.toml", ROLLBACK_DIR, transaction_id))
}

fn read_pending_secret(workspace: &Workspace, offer_id: &str) -> Result<SecretExchange> {
    let path = offer_id_path(workspace, offer_id)?;
    let bytes = workspace.read_required(&path)?;
    serde_json::from_slice(&bytes).map_err(|_| KeyManagerError::InvalidExchangeArtifact)
}

fn write_pending_secret(workspace: &Workspace, secret: &SecretExchange) -> Result<()> {
    let path = offer_id_path(workspace, &secret.offer_id)?;
    let bytes = serde_json::to_vec(secret).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
    if bytes.len() > MAX_PENDING_BYTES {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    workspace.write_new(&path, &bytes)
}

fn remove_pending(workspace: &Workspace, offer_id: &str) -> Result<()> {
    let path = offer_id_path(workspace, offer_id)?;
    workspace.remove_if_present(&path)
}

fn contact_occupied(workspace: &Workspace, contact_name: &str, ledger: &Ledger) -> bool {
    let config_match = workspace.read_optional(CONFIG_FILE).ok().flatten().map_or(false, |bytes| {
        parse_document(&bytes).ok().map_or(false, |doc| {
            doc.table.get("contact").and_then(|v| v.as_table()).map_or(false, |t| t.contains_key(contact_name))
        })
    });
    let ledger_match = ledger.contacts.contains_key(contact_name);
    config_match || ledger_match
}

fn is_terminal_exchange(state: &str) -> bool {
    matches!(state, "Expired" | "Cancelled" | "Revoked")
}

fn is_settled_for_migration(state: &str) -> bool {
    matches!(state, "legacy-active" | "legacy-noncompliant" | "PeerVerified" | "RolledBack" | "Expired" | "Cancelled" | "Revoked")
}

fn is_unresolved_ledger_state(state: &str) -> bool {
    matches!(state, "Installing" | "Prepared" | "PeerReceived" | "Confirmed" | "Installed" | "LocallyActivated" | "VerificationOverdue")
}

fn append_ledger_contact(ledger: &mut Ledger, name: &str, contact: LedgerContact) -> Result<()> {
    match ledger.contacts.get(name) {
        Some(existing) if existing != &contact => Err(KeyManagerError::ContactConflict),
        Some(_) => Ok(()),
        None => {
            ledger.contacts.insert(name.to_owned(), contact);
            Ok(())
        }
    }
}

fn write_rollback_snapshot(workspace: &Workspace, transaction_id: &str, contact_name: &str, contact_toml: &SecretString) -> Result<()> {
    validate_transaction_id(transaction_id)?;
    validate_contact_name(contact_name)?;
    let path = format!("{}/{}.toml", ROLLBACK_DIR, transaction_id);
    let bytes = contact_toml.expose_secret().as_bytes();
    if bytes.len() > MAX_PENDING_BYTES {
        return Err(KeyManagerError::InvalidLedger);
    }
    workspace.write_new(&path, bytes)
}

fn contact_toml_for_new_contact(contact_name: &str, public: &str, private: &str) -> Result<Zeroizing<Vec<u8>>> {
    validate_contact_name(contact_name)?;
    public_fingerprint(public)?;
    let _ = private_key_bytes(private)?;
    let rendered = format!("[contact.\"{}\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{}\"\n", contact_name, public, private);
    Ok(Zeroizing::new(rendered.into_bytes()))
}

fn contact_toml_delete_contact(existing_config: &[u8], contact_name: &str) -> Result<Zeroizing<Vec<u8>>> {
    let mut document = parse_document(existing_config)?;
    let contacts = document.table.get_mut("contact").and_then(|v| v.as_table_mut()).ok_or(KeyManagerError::InvalidToml)?;
    contacts.remove(contact_name);
    validate_document(&document.table)?;
    serialize_table(&document.table)
}

fn new_contact_id() -> String {
    let mut bytes = vec![0_u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn new_offer_id() -> String {
    new_contact_id()
}

fn make_initiator_secret(
    offer_id: &str,
    contact_id: &str,
    scope_id: &str,
    contact_name: &str,
    local_public: &str,
    local_private: &str,
    generation: u64,
    created_at: i64,
    expires_at: i64,
) -> Result<SecretExchange> {
    validate_offer_id(offer_id)?;
    validate_offer_id(contact_id)?;
    Ok(SecretExchange {
        schema: EXCHANGE_SCHEMA.to_owned(),
        offer_id: offer_id.to_owned(),
        role: ExchangeRole::InitiatorOffer.as_str().to_owned(),
        operation: ExchangeOperation::Initial.as_str().to_owned(),
        contact_id: contact_id.to_owned(),
        peer_contact_id: None,
        local_public: local_public.to_owned(),
        local_private: SecretString::from(local_private.to_owned()),
        peer_public: None,
        peer_fingerprint: None,
        state: ExchangeState::Prepared.as_str().to_owned(),
        scope_id: scope_id.to_owned(),
        contact_name: contact_name.to_owned(),
        generation,
        created_at,
        expires_at,
        transcript: None,
    })
}

fn make_initiator_artifact(secret: &SecretExchange, intended_peer_contact_id: Option<&str>, intended_peer_fingerprint: Option<&str>, label: Option<&str>, previous_fingerprint: Option<&str>) -> Result<PreparedExchange> {
    Ok(PreparedExchange {
        schema: secret.schema.clone(),
        version: EXCHANGE_SCHEMA_VERSION.to_owned(),
        artifact_role: ExchangeRole::InitiatorOffer.as_str().to_owned(),
        offer_id: secret.offer_id.clone(),
        sender_contact_id: secret.contact_id.clone(),
        intended_peer_contact_id: intended_peer_contact_id.map(|s| s.to_owned()),
        intended_peer_fingerprint: intended_peer_fingerprint.map(|s| s.to_owned()),
        generator_profile: MIGRATION_GENERATOR_PROFILE.to_owned(),
        binary_sha256: String::new(),
        key_format: MIGRATION_KEY_FORMAT.to_owned(),
        source_revision: MIGRATION_SOURCE_REVISION.to_owned(),
        public_key: secret.local_public.clone(),
        public_fingerprint: public_fingerprint(&secret.local_public)?,
        generation: secret.generation,
        created_at: secret.created_at,
        expires_at: secret.expires_at,
        label: label.map(|s| s.to_owned()),
        previous_fingerprint: previous_fingerprint.map(|s| s.to_owned()),
    })
}

fn make_responder_secret(
    offer_id: &str,
    contact_id: &str,
    scope_id: &str,
    contact_name: &str,
    peer_contact_id: &str,
    initiator: &ValidatedPublicArtifact,
    local_public: &str,
    local_private: &str,
    created_at: i64,
    expires_at: i64,
) -> Result<SecretExchange> {
    Ok(SecretExchange {
        schema: EXCHANGE_SCHEMA.to_owned(),
        offer_id: offer_id.to_owned(),
        role: ExchangeRole::ResponderResponse.as_str().to_owned(),
        operation: ExchangeOperation::Initial.as_str().to_owned(),
        contact_id: contact_id.to_owned(),
        peer_contact_id: Some(peer_contact_id.to_owned()),
        local_public: local_public.to_owned(),
        local_private: SecretString::from(local_private.to_owned()),
        peer_public: Some(String::new()),
        peer_fingerprint: Some(initiator.public_fingerprint.as_str().to_owned()),
        state: ExchangeState::Prepared.as_str().to_owned(),
        scope_id: scope_id.to_owned(),
        contact_name: contact_name.to_owned(),
        generation: initiator.generation,
        created_at,
        expires_at,
        transcript: None,
    })
}

fn make_responder_artifact(secret: &SecretExchange, initiator_offer_id: &str, intended_peer_contact_id: &str, intended_peer_fingerprint: &str, label: Option<&str>, previous_fingerprint: Option<&str>) -> Result<ResponderExchange> {
    Ok(ResponderExchange {
        schema: secret.schema.clone(),
        version: EXCHANGE_SCHEMA_VERSION.to_owned(),
        artifact_role: ExchangeRole::ResponderResponse.as_str().to_owned(),
        offer_id: secret.offer_id.clone(),
        in_reply_to: initiator_offer_id.to_owned(),
        sender_contact_id: secret.contact_id.clone(),
        intended_peer_contact_id: intended_peer_contact_id.to_owned(),
        intended_peer_fingerprint: intended_peer_fingerprint.to_owned(),
        generator_profile: MIGRATION_GENERATOR_PROFILE.to_owned(),
        binary_sha256: String::new(),
        key_format: MIGRATION_KEY_FORMAT.to_owned(),
        source_revision: MIGRATION_SOURCE_REVISION.to_owned(),
        public_key: secret.local_public.clone(),
        public_fingerprint: public_fingerprint(&secret.local_public)?,
        generation: secret.generation,
        created_at: secret.created_at,
        expires_at: secret.expires_at,
        label: label.map(|s| s.to_owned()),
        previous_fingerprint: previous_fingerprint.map(|s| s.to_owned()),
    })
}

fn build_install_ledger_contact(fingerprint: &str, contact_id: &str, generation: u64, local_fingerprint: &str) -> LedgerContact {
    LedgerContact {
        state: ExchangeState::Installed.as_str().to_owned(),
        peer_fingerprint: fingerprint.to_owned(),
        generation,
        contact_id: contact_id.to_owned(),
        local_fingerprint: local_fingerprint.to_owned(),
        activation_deadline: 0,
        rollback_tx_id: String::new(),
        rollback_contact_toml_hash: String::new(),
        pending_offer_id: String::new(),
        compromised: false,
        revocation_generation: 0,
    }
}

fn exchange_journal(
    workspace: &Workspace,
    scope_id: &str,
    operation: &str,
    contact_name: &str,
    contact_id: &str,
    offer_id: &str,
    peer_contact_id: Option<&str>,
    transcript: Option<&str>,
    generation: u64,
    peer_generation: u64,
    old_config: Option<&[u8]>,
    old_ledger: Option<&[u8]>,
    new_config: &[u8],
    new_ledger: &[u8],
    operation_kind: &str,
    rollback_contact_toml_hash: Option<&str>,
    confirmed_peer_fingerprint: Option<&str>,
    rollback_tx_id: Option<&str>,
    crash_point: Option<CrashPoint>,
) -> Result<UpdateResult> {
    let config_hash = sha256(new_config);
    let ledger_hash = sha256(new_ledger);
    if old_config == Some(new_config) && old_ledger == Some(new_ledger) && operation_kind == "exchange-roundtrip" {
        return Ok(UpdateResult {
            transaction_id: None,
            config_hash,
            ledger_hash,
            changed: false,
        });
    }
    let transaction_id = Uuid::new_v4().simple().to_string();
    let config_candidate = format!("{}/{}.toml", CANDIDATES_DIR, transaction_id);
    let ledger_candidate = format!("{}/{}.ledger.json", CANDIDATES_DIR, transaction_id);
    let journal_path = format!("{}/{}.json", TRANSACTIONS_DIR, transaction_id);
    workspace.write_new(&config_candidate, new_config)?;
    workspace.write_new(&ledger_candidate, new_ledger)?;
    validate_transaction_candidates(workspace, scope_id, &config_candidate, &ledger_candidate, &config_hash, &ledger_hash)?;
    let mut journal = JournalFile {
        schema: "lunarwing.darkirc-transaction/v1".to_owned(),
        transaction_id: transaction_id.clone(),
        operation: operation.to_owned(),
        scope_id: scope_id.to_owned(),
        old_config_hash: old_config.map(sha256),
        old_ledger_hash: old_ledger.map(sha256),
        new_config_hash: config_hash.clone(),
        new_ledger_hash: ledger_hash.clone(),
        config_candidate: config_candidate.clone(),
        ledger_candidate: ledger_candidate.clone(),
        phase: JournalPhase::Prepared.as_str().to_owned(),
        contact_id: contact_id.to_owned(),
        offer_id: offer_id.to_owned(),
        peer_contact_id: peer_contact_id.unwrap_or_default().to_owned(),
        transcript: transcript.unwrap_or_default().to_owned(),
        generation,
        operation_kind: operation_kind.to_owned(),
        rollback_contact_toml_hash: rollback_contact_toml_hash.unwrap_or_default().to_owned(),
        confirmed_peer_fingerprint: confirmed_peer_fingerprint.unwrap_or_default().to_owned(),
        rollback_tx_id: rollback_tx_id.unwrap_or_default().to_owned(),
        contact_name: contact_name.to_owned(),
        peer_generation,
    };
    write_journal(workspace, &journal_path, &journal)?;
    inject_crash(crash_point, CrashPoint::AfterJournal)?;
    workspace.rename(&ledger_candidate, LEDGER_FILE)?;
    journal.phase = JournalPhase::LedgerCommitted.as_str().to_owned();
    replace_journal(workspace, &journal_path, &journal)?;
    inject_crash(crash_point, CrashPoint::AfterLedgerCommit)?;
    inject_crash(crash_point, CrashPoint::AfterLedger)?;
    workspace.rename(&config_candidate, CONFIG_FILE)?;
    journal.phase = JournalPhase::ConfigCommitted.as_str().to_owned();
    replace_journal(workspace, &journal_path, &journal)?;
    inject_crash(crash_point, CrashPoint::AfterConfig)?;
    finish_journal(workspace, &journal_path, &journal)?;
    Ok(UpdateResult {
        transaction_id: Some(transaction_id),
        config_hash,
        ledger_hash,
        changed: true,
    })
}

// ── Stage A: prepare/respond/complete/cancel ────────────────────────────────

pub struct PrepareExchange<'a> {
    pub contact_name: &'a str,
    pub offer_id: &'a str,
    pub contact_id: &'a str,
    pub local_public: &'a str,
    pub local_private: &'a str,
    pub intended_peer_contact_id: Option<&'a str>,
    pub intended_peer_fingerprint: Option<&'a str>,
    pub label: Option<&'a str>,
    pub previous_fingerprint: Option<&'a str>,
    pub generation: u64,
    pub created_at: i64,
    pub expires_at: i64,
}

pub fn prepare_exchange(
    paths: &TenantPaths,
    scope_id: &str,
    request: &PrepareExchange<'_>,
) -> Result<PreparedResult> {
    validate_scope_id(scope_id)?;
    validate_contact_name(request.contact_name)?;
    validate_offer_id(request.offer_id)?;
    validate_offer_id(request.contact_id)?;
    if request.expires_at <= request.created_at {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    public_fingerprint(request.local_public)?;
    let local_fingerprint = public_fingerprint(request.local_public)?;
    let _ = private_key_bytes(request.local_private)?;
    if let Some(peer_f) = request.intended_peer_fingerprint {
        PublicFingerprint::parse(peer_f)?;
    }
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;
    let old_config = workspace.read_optional(CONFIG_FILE)?;
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    let ledger = match old_ledger.as_ref().map(|b| b.as_slice()) {
        Some(bytes) => parse_ledger(bytes, scope_id)?,
        None => empty_ledger(scope_id),
    };
    if contact_occupied(&workspace, request.contact_name, &ledger) {
        return Err(KeyManagerError::ContactConflict);
    }
    if let Some(pending) = workspace.read_optional(&offer_id_path(&workspace, request.offer_id)?)? {
        let existing: SecretExchange = serde_json::from_slice(&pending).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
        if existing.contact_name != request.contact_name || existing.local_public != request.local_public {
            return Err(KeyManagerError::OfferConflict);
        }
        let artifact = make_initiator_artifact(
            &existing,
            request.intended_peer_contact_id,
            request.intended_peer_fingerprint,
            request.label,
            request.previous_fingerprint,
        )?;
        let public_artifact = serde_json::to_string(&artifact).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
        return Ok(PreparedResult {
            offer_id: request.offer_id.to_owned(),
            contact_id: request.contact_id.to_owned(),
            role: ExchangeRole::InitiatorOffer.as_str().to_owned(),
            public_artifact,
            public_fingerprint: existing.peer_fingerprint.clone().unwrap_or_default(),
            expires_at: request.expires_at,
        });
    }
    let secret = make_initiator_secret(
        request.offer_id,
        request.contact_id,
        scope_id,
        request.contact_name,
        request.local_public,
        request.local_private,
        request.generation,
        request.created_at,
        request.expires_at,
    )?;
    write_pending_secret(&workspace, &secret)?;
    let artifact = make_initiator_artifact(
        &secret,
        request.intended_peer_contact_id,
        request.intended_peer_fingerprint,
        request.label,
        request.previous_fingerprint,
    )?;
    let public_artifact = serde_json::to_string(&artifact).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
    let _ = local_fingerprint;
    Ok(PreparedResult {
        offer_id: request.offer_id.to_owned(),
        contact_id: request.contact_id.to_owned(),
        role: ExchangeRole::InitiatorOffer.as_str().to_owned(),
        public_artifact,
        public_fingerprint: artifact.public_fingerprint,
        expires_at: request.expires_at,
    })
}

pub struct RespondExchange<'a> {
    pub contact_name: &'a str,
    pub offer_id: &'a str,
    pub contact_id: &'a str,
    pub local_public: &'a str,
    pub local_private: &'a str,
    pub initiator_offer_id: &'a str,
    pub expected_initiator_fingerprint: &'a PublicFingerprint,
    pub label: Option<&'a str>,
    pub created_at: i64,
    pub expires_at: i64,
}

pub fn respond_exchange(
    paths: &TenantPaths,
    scope_id: &str,
    offer_bytes: &[u8],
    request: &RespondExchange<'_>,
) -> Result<PreparedResult> {
    validate_scope_id(scope_id)?;
    validate_contact_name(request.contact_name)?;
    validate_offer_id(request.offer_id)?;
    validate_offer_id(request.contact_id)?;
    if request.expires_at <= request.created_at {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    public_fingerprint(request.local_public)?;
    let local_fingerprint = public_fingerprint(request.local_public)?;
    let _ = private_key_bytes(request.local_private)?;
    let initiator = validate_public_exchange(offer_bytes, ExchangeRole::InitiatorOffer)?;
    if initiator.public_fingerprint != *request.expected_initiator_fingerprint {
        return Err(KeyManagerError::FingerprintMismatch);
    }
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;
    let old_config = workspace.read_optional(CONFIG_FILE)?;
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    let ledger = match old_ledger.as_ref().map(|b| b.as_slice()) {
        Some(bytes) => parse_ledger(bytes, scope_id)?,
        None => empty_ledger(scope_id),
    };
    if contact_occupied(&workspace, request.contact_name, &ledger) {
        return Err(KeyManagerError::ContactConflict);
    }
    if let Some(pending) = workspace.read_optional(&offer_id_path(&workspace, request.offer_id)?)? {
        let existing: SecretExchange = serde_json::from_slice(&pending).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
        if existing.contact_name != request.contact_name || existing.local_public != request.local_public {
            return Err(KeyManagerError::OfferConflict);
        }
        let artifact = make_responder_artifact(&existing, &initiator.offer_id, &initiator.sender_contact_id, initiator.public_fingerprint.as_str(), request.label, None)?;
        let public_artifact = serde_json::to_string(&artifact).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
        return Ok(PreparedResult {
            offer_id: request.offer_id.to_owned(),
            contact_id: request.contact_id.to_owned(),
            role: ExchangeRole::ResponderResponse.as_str().to_owned(),
            public_artifact,
            public_fingerprint: existing.peer_fingerprint.clone().unwrap_or_default(),
            expires_at: request.expires_at,
        });
    }
    let secret = make_responder_secret(
        request.offer_id,
        request.contact_id,
        scope_id,
        request.contact_name,
        &initiator.sender_contact_id,
        &initiator,
        request.local_public,
        request.local_private,
        request.created_at,
        request.expires_at,
    )?;
    write_pending_secret(&workspace, &secret)?;
    let artifact = make_responder_artifact(&secret, &initiator.offer_id, &initiator.sender_contact_id, initiator.public_fingerprint.as_str(), request.label, None)?;
    let public_artifact = serde_json::to_string(&artifact).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
    let _ = local_fingerprint;
    Ok(PreparedResult {
        offer_id: request.offer_id.to_owned(),
        contact_id: request.contact_id.to_owned(),
        role: ExchangeRole::ResponderResponse.as_str().to_owned(),
        public_artifact,
        public_fingerprint: artifact.public_fingerprint,
        expires_at: request.expires_at,
    })
}

pub struct CompleteExchange<'a> {
    pub exchange_id: &'a str,
    pub expected_peer_fingerprint: &'a PublicFingerprint,
    pub defer_apply: bool,
}

pub fn complete_exchange(
    paths: &TenantPaths,
    scope_id: &str,
    peer_artifact_bytes: &[u8],
    request: &CompleteExchange<'_>,
) -> Result<ExchangeStatus> {
    validate_scope_id(scope_id)?;
    validate_offer_id(request.exchange_id)?;
    let peer = validate_public_exchange(peer_artifact_bytes, ExchangeRole::ResponderResponse)?;
    if peer.public_fingerprint != *request.expected_peer_fingerprint {
        return Err(KeyManagerError::FingerprintMismatch);
    }
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;
    let secret = read_pending_secret(&workspace, request.exchange_id)?;
    if secret.state != ExchangeState::Prepared.as_str() {
        return Err(KeyManagerError::InvalidExchangeState);
    }
    if peer.in_reply_to.as_deref() != Some(request.exchange_id) {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    let initiator_artifact = make_initiator_artifact(&secret, None, None, None, None)?;
    let initiator_canonical = serde_json::to_string(&initiator_artifact).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
    let transcript = compute_transcript(initiator_canonical.as_bytes(), &peer.canonical)?;
    let mut new_secret = secret.clone();
    new_secret.peer_public = Some(peer.public_fingerprint.as_str().to_owned());
    new_secret.peer_fingerprint = Some(peer.public_fingerprint.as_str().to_owned());
    new_secret.state = ExchangeState::Confirmed.as_str().to_owned();
    new_secret.transcript = Some(transcript.clone());
    write_pending_secret(&workspace, &new_secret)?;
    let old_config = workspace.read_optional(CONFIG_FILE)?;
    let public_fingerprint = public_fingerprint(&secret.local_public)?;
    let candidate = match old_config.as_ref() {
        Some(config) => {
            let rendered = contact_toml_for_new_contact(&secret.contact_name, &secret.local_public, secret.local_private.expose_secret())?;
            let source = parse_document(&rendered)?;
            let mut target = parse_document(config)?;
            let target_contacts = target.table.entry("contact".to_owned()).or_insert_with(|| Value::Table(Table::new()));
            let target_contacts = target_contacts.as_table_mut().ok_or(KeyManagerError::InvalidToml)?;
            if let Some(contact) = source.table.get("contact").and_then(|v| v.as_table()) {
                for (name, value) in contact {
                    target_contacts.insert(name.clone(), value.clone());
                }
            }
            validate_document(&target.table)?;
            serialize_table(&target.table)?
        }
        None => contact_toml_for_new_contact(&secret.contact_name, &secret.local_public, secret.local_private.expose_secret())?,
    };
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    let mut ledger = match old_ledger.as_ref().map(|b| b.as_slice()) {
        Some(bytes) => parse_ledger(bytes, scope_id)?,
        None => empty_ledger(scope_id),
    };
    let contact = build_install_ledger_contact(&peer.public_fingerprint.as_str(), &secret.contact_id, secret.generation, &public_fingerprint);
    append_ledger_contact(&mut ledger, &secret.contact_name, contact)?;
    let new_ledger = ledger_bytes(&ledger)?;
    let op = if request.defer_apply { "exchange-complete-deferred" } else { "exchange-complete" };
    let result = exchange_journal(
        &workspace,
        scope_id,
        op,
        &secret.contact_name,
        &secret.contact_id,
        &secret.offer_id,
        Some(&peer.sender_contact_id),
        Some(&transcript),
        secret.generation,
        peer.generation,
        old_config.as_ref().map(|b| b.as_slice()),
        old_ledger.as_ref().map(|b| b.as_slice()),
        &candidate,
        &new_ledger,
        "exchange-complete",
        None,
        Some(peer.public_fingerprint.as_str()),
        None,
        None,
    )?;
    let mut post_secret = new_secret.clone();
    post_secret.state = ExchangeState::Installed.as_str().to_owned();
    write_pending_secret(&workspace, &post_secret)?;
    let _ = result;
    Ok(ExchangeStatus {
        contact_name: secret.contact_name.clone(),
        contact_id: secret.contact_id.clone(),
        offer_id: secret.offer_id.clone(),
        state: ExchangeState::Installed.as_str().to_owned(),
        peer_fingerprint: peer.public_fingerprint.as_str().to_owned(),
        created_at: secret.created_at,
        expires_at: secret.expires_at,
        confirmed_at: Some(secret.created_at),
        installed_at: Some(secret.created_at),
        activation_deadline: None,
    })
}

pub fn cancel_exchange(
    paths: &TenantPaths,
    scope_id: &str,
    exchange_id: &str,
) -> Result<ExchangeStatus> {
    validate_scope_id(scope_id)?;
    validate_offer_id(exchange_id)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;
    let secret = read_pending_secret(&workspace, exchange_id)?;
    if is_terminal_exchange(&secret.state) {
        return Err(KeyManagerError::InvalidExchangeState);
    }
    let contact_name = secret.contact_name.clone();
    let contact_id = secret.contact_id.clone();
    let offer_id = secret.offer_id.clone();
    let created_at = secret.created_at;
    let expires_at = secret.expires_at;
    let peer_fingerprint = secret.peer_fingerprint.clone().unwrap_or_default();
    remove_pending(&workspace, exchange_id)?;
    Ok(ExchangeStatus {
        contact_name,
        contact_id,
        offer_id,
        state: ExchangeState::Cancelled.as_str().to_owned(),
        peer_fingerprint,
        created_at,
        expires_at,
        confirmed_at: None,
        installed_at: None,
        activation_deadline: None,
    })
}

pub fn inspect_exchanges(
    paths: &TenantPaths,
    scope_id: &str,
) -> Result<Vec<ExchangeStatus>> {
    validate_scope_id(scope_id)?;
    let workspace = Workspace::open_read_only(paths.clone())?;
    let _snapshot = workspace.read_snapshot()?;
    let pending_dir = workspace.open_relative_dir(PENDING_DIR)?;
    let mut entries = Vec::new();
    for entry in pending_dir.read_dir(".").map_err(KeyManagerError::Io)? {
        let entry = entry.map_err(KeyManagerError::Io)?;
        let name = entry.file_name().into_string().map_err(|_| KeyManagerError::UnsafePath)?;
        if !name.ends_with(".json") {
            continue;
        }
        let path = format!("{}/{}", PENDING_DIR, name);
        let bytes = match workspace.read_optional(&path)? {
            Some(b) => b,
            None => continue,
        };
        let secret: SecretExchange = match serde_json::from_slice(&bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };
        entries.push(ExchangeStatus {
            contact_name: secret.contact_name,
            contact_id: secret.contact_id,
            offer_id: secret.offer_id,
            state: secret.state,
            peer_fingerprint: secret.peer_fingerprint.unwrap_or_default(),
            created_at: secret.created_at,
            expires_at: secret.expires_at,
            confirmed_at: None,
            installed_at: None,
            activation_deadline: None,
        });
    }
    Ok(entries)
}

// ── Stage C: activation and rollback ─────────────────────────────────────────

pub struct ActivationRequest {
    pub transaction_id: String,
    pub contact_name: String,
    pub contact_id: String,
}

pub fn begin_activation(
    paths: &TenantPaths,
    scope_id: &str,
    contact: &str,
    transaction_id: &str,
    expected_peer_fingerprint: &PublicFingerprint,
) -> Result<ActivationRequest> {
    validate_scope_id(scope_id)?;
    validate_contact_name(contact)?;
    validate_transaction_id(transaction_id)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;
    let ledger = workspace.read_optional(LEDGER_FILE)?.ok_or(KeyManagerError::InvalidLedger)?;
    let ledger = parse_ledger(&ledger, scope_id)?;
    let entry = ledger.contacts.get(contact).ok_or(KeyManagerError::ContactNotFound)?;
    if entry.state != ExchangeState::Installed.as_str() {
        return Err(KeyManagerError::InvalidExchangeState);
    }
    if entry.peer_fingerprint != expected_peer_fingerprint.as_str() {
        return Err(KeyManagerError::FingerprintMismatch);
    }
    Ok(ActivationRequest {
        transaction_id: transaction_id.to_owned(),
        contact_name: contact.to_owned(),
        contact_id: entry.contact_id.clone(),
    })
}

pub fn mark_locally_activated(
    paths: &TenantPaths,
    scope_id: &str,
    contact: &str,
    transaction_id: &str,
    activated_at: i64,
) -> Result<ExchangeStatus> {
    validate_scope_id(scope_id)?;
    validate_contact_name(contact)?;
    validate_transaction_id(transaction_id)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;
    let ledger = workspace.read_optional(LEDGER_FILE)?.ok_or(KeyManagerError::InvalidLedger)?;
    let mut ledger = parse_ledger(&ledger, scope_id)?;
    let mut entry = ledger.contacts.get(contact).cloned().ok_or(KeyManagerError::ContactNotFound)?;
    if entry.state != ExchangeState::Installed.as_str() {
        return Err(KeyManagerError::InvalidExchangeState);
    }
    entry.state = ExchangeState::LocallyActivated.as_str().to_owned();
    entry.activation_deadline = activated_at + ACTIVATION_WINDOW_SECONDS;
    entry.pending_offer_id = transaction_id.to_owned();
    let contact_id = entry.contact_id.clone();
    let generation = entry.generation;
    let peer_fingerprint = entry.peer_fingerprint.clone();
    let activation_deadline = entry.activation_deadline;
    drop(entry);
    let new_ledger = ledger_bytes(&ledger)?;
    let old_config = workspace.read_optional(CONFIG_FILE)?;
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    exchange_journal(
        &workspace,
        scope_id,
        "exchange-activation",
        contact,
        &contact_id,
        transaction_id,
        None,
        None,
        generation,
        0,
        old_config.as_ref().map(|b| b.as_slice()),
        old_ledger.as_ref().map(|b| b.as_slice()),
        old_config.as_ref().map(|b| b.as_slice()).unwrap_or(b""),
        &new_ledger,
        "exchange-activation",
        None,
        None,
        Some(transaction_id),
        None,
    )?;
    Ok(ExchangeStatus {
        contact_name: contact.to_owned(),
        contact_id,
        offer_id: transaction_id.to_owned(),
        state: ExchangeState::LocallyActivated.as_str().to_owned(),
        peer_fingerprint,
        created_at: 0,
        expires_at: 0,
        confirmed_at: None,
        installed_at: None,
        activation_deadline: Some(activation_deadline),
    })
}

pub fn rollback_activation(
    paths: &TenantPaths,
    scope_id: &str,
    contact: &str,
    transaction_id: &str,
    compromised: bool,
    rollback_toml: Option<&SecretString>,
) -> Result<ExchangeStatus> {
    validate_scope_id(scope_id)?;
    validate_contact_name(contact)?;
    validate_transaction_id(transaction_id)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;
    let ledger = workspace.read_optional(LEDGER_FILE)?.ok_or(KeyManagerError::InvalidLedger)?;
    let mut ledger = parse_ledger(&ledger, scope_id)?;
    let mut entry = ledger.contacts.get(contact).cloned().ok_or(KeyManagerError::ContactNotFound)?;
    if entry.state != ExchangeState::Installed.as_str() && entry.state != ExchangeState::LocallyActivated.as_str() && entry.state != ExchangeState::VerificationOverdue.as_str() {
        return Err(KeyManagerError::InvalidExchangeState);
    }
    if compromised {
        entry.compromised = true;
        entry.state = ExchangeState::RolledBack.as_str().to_owned();
        ledger.contacts.insert(contact.to_owned(), entry.clone());
    ledger.contacts.insert(contact.to_owned(), entry.clone());
        let new_ledger = ledger_bytes(&ledger)?;
        let old_config = workspace.read_optional(CONFIG_FILE)?;
        let old_ledger = workspace.read_optional(LEDGER_FILE)?;
        exchange_journal(
            &workspace,
            scope_id,
            "exchange-rollback",
            contact,
            &entry.contact_id,
            transaction_id,
            None,
            None,
            entry.generation,
            0,
            old_config.as_ref().map(|b| b.as_slice()),
            old_ledger.as_ref().map(|b| b.as_slice()),
            old_config.as_ref().map(|b| b.as_slice()).unwrap_or(b""),
            &new_ledger,
            "exchange-rollback",
            None,
            None,
            Some(transaction_id),
            None,
        )?;
        return Ok(ExchangeStatus {
            contact_name: contact.to_owned(),
            contact_id: entry.contact_id.clone(),
            offer_id: transaction_id.to_owned(),
            state: ExchangeState::RolledBack.as_str().to_owned(),
            peer_fingerprint: entry.peer_fingerprint.clone(),
            created_at: 0,
            expires_at: 0,
            confirmed_at: None,
            installed_at: None,
            activation_deadline: None,
        });
    }
    let _ = rollback_toml.ok_or(KeyManagerError::InvalidLedger)?;
    entry.state = ExchangeState::RolledBack.as_str().to_owned();
    entry.rollback_tx_id = transaction_id.to_owned();
    ledger.contacts.insert(contact.to_owned(), entry.clone());
    ledger.contacts.insert(contact.to_owned(), entry.clone());
    let new_ledger = ledger_bytes(&ledger)?;
    let old_config = workspace.read_optional(CONFIG_FILE)?;
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    exchange_journal(
        &workspace,
        scope_id,
        "exchange-rollback",
        contact,
        &entry.contact_id,
        transaction_id,
        None,
        None,
        entry.generation,
        0,
        old_config.as_ref().map(|b| b.as_slice()),
        old_ledger.as_ref().map(|b| b.as_slice()),
        old_config.as_ref().map(|b| b.as_slice()).unwrap_or(b""),
        &new_ledger,
        "exchange-rollback",
        None,
        None,
        Some(transaction_id),
        None,
    )?;
    Ok(ExchangeStatus {
        contact_name: contact.to_owned(),
        contact_id: entry.contact_id.clone(),
        offer_id: transaction_id.to_owned(),
        state: ExchangeState::RolledBack.as_str().to_owned(),
        peer_fingerprint: entry.peer_fingerprint.clone(),
        created_at: 0,
        expires_at: 0,
        confirmed_at: None,
        installed_at: None,
        activation_deadline: None,
    })
}

pub fn confirm_roundtrip(
    paths: &TenantPaths,
    scope_id: &str,
    contact: &str,
    exchange_id: &str,
    expected_peer_fingerprint: &PublicFingerprint,
) -> Result<ExchangeStatus> {
    validate_scope_id(scope_id)?;
    validate_contact_name(contact)?;
    validate_offer_id(exchange_id)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;
    let ledger = workspace.read_optional(LEDGER_FILE)?.ok_or(KeyManagerError::InvalidLedger)?;
    let mut ledger = parse_ledger(&ledger, scope_id)?;
    let mut entry = ledger.contacts.get(contact).cloned().ok_or(KeyManagerError::ContactNotFound)?;
    if entry.state != ExchangeState::LocallyActivated.as_str() && entry.state != ExchangeState::PeerVerified.as_str() {
        return Err(KeyManagerError::InvalidExchangeState);
    }
    if entry.peer_fingerprint != expected_peer_fingerprint.as_str() {
        return Err(KeyManagerError::FingerprintMismatch);
    }
    entry.state = ExchangeState::PeerVerified.as_str().to_owned();
    entry.activation_deadline = 0;
    let new_ledger = ledger_bytes(&ledger)?;
    let old_config = workspace.read_optional(CONFIG_FILE)?;
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    exchange_journal(
        &workspace,
        scope_id,
        "exchange-roundtrip",
        contact,
        &entry.contact_id,
        exchange_id,
        None,
        None,
        entry.generation,
        0,
        old_config.as_ref().map(|b| b.as_slice()),
        old_ledger.as_ref().map(|b| b.as_slice()),
        old_config.as_ref().map(|b| b.as_slice()).unwrap_or(b""),
        &new_ledger,
        "exchange-roundtrip",
        None,
        Some(expected_peer_fingerprint.as_str()),
        None,
        None,
    )?;
    remove_pending(&workspace, exchange_id)?;
    Ok(ExchangeStatus {
        contact_name: contact.to_owned(),
        contact_id: entry.contact_id.clone(),
        offer_id: exchange_id.to_owned(),
        state: ExchangeState::PeerVerified.as_str().to_owned(),
        peer_fingerprint: entry.peer_fingerprint.clone(),
        created_at: 0,
        expires_at: 0,
        confirmed_at: None,
        installed_at: None,
        activation_deadline: None,
    })
}

pub enum OverdueDecision {
    Keep,
    Rollback,
    Revoke,
}

pub fn resolve_verification_overdue(
    paths: &TenantPaths,
    scope_id: &str,
    contact: &str,
    decision: OverdueDecision,
    current_peer_fingerprint: &PublicFingerprint,
    extend_seconds: Option<i64>,
) -> Result<ExchangeStatus> {
    validate_scope_id(scope_id)?;
    validate_contact_name(contact)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;
    let ledger = workspace.read_optional(LEDGER_FILE)?.ok_or(KeyManagerError::InvalidLedger)?;
    let mut ledger = parse_ledger(&ledger, scope_id)?;
    let mut entry = ledger.contacts.get(contact).cloned().ok_or(KeyManagerError::ContactNotFound)?;
    if entry.state != ExchangeState::LocallyActivated.as_str() && entry.state != ExchangeState::VerificationOverdue.as_str() {
        return Err(KeyManagerError::InvalidExchangeState);
    }
    if entry.peer_fingerprint != current_peer_fingerprint.as_str() {
        return Err(KeyManagerError::FingerprintMismatch);
    }
    match decision {
        OverdueDecision::Keep => {
            if entry.state == ExchangeState::LocallyActivated.as_str() {
                entry.state = ExchangeState::VerificationOverdue.as_str().to_owned();
            }
            if let Some(ext) = extend_seconds {
                if ext < 0 || ext > EXTENSION_WINDOW_SECONDS {
                    return Err(KeyManagerError::InvalidExchangeArtifact);
                }
                entry.activation_deadline = entry.activation_deadline + ext;
            }
            let new_ledger = ledger_bytes(&ledger)?;
            let old_config = workspace.read_optional(CONFIG_FILE)?;
            let old_ledger = workspace.read_optional(LEDGER_FILE)?;
            exchange_journal(
                &workspace,
                scope_id,
                "exchange-keep",
                contact,
                &entry.contact_id,
                &entry.pending_offer_id,
                None,
                None,
                entry.generation,
                0,
                old_config.as_ref().map(|b| b.as_slice()),
                old_ledger.as_ref().map(|b| b.as_slice()),
                old_config.as_ref().map(|b| b.as_slice()).unwrap_or(b""),
                &new_ledger,
                "exchange-keep",
                None,
                None,
                None,
                None,
            )?;
            Ok(ExchangeStatus {
                contact_name: contact.to_owned(),
                contact_id: entry.contact_id.clone(),
                offer_id: entry.pending_offer_id.clone(),
                state: entry.state.clone(),
                peer_fingerprint: entry.peer_fingerprint.clone(),
                created_at: 0,
                expires_at: 0,
                confirmed_at: None,
                installed_at: None,
                activation_deadline: Some(entry.activation_deadline),
            })
        }
        OverdueDecision::Rollback => {
            if entry.compromised {
                return Err(KeyManagerError::CompromisedKey);
            }
            entry.state = ExchangeState::RolledBack.as_str().to_owned();
            let new_ledger = ledger_bytes(&ledger)?;
            let old_config = workspace.read_optional(CONFIG_FILE)?;
            let old_ledger = workspace.read_optional(LEDGER_FILE)?;
            exchange_journal(
                &workspace,
                scope_id,
                "exchange-rollback",
                contact,
                &entry.contact_id,
                &entry.pending_offer_id,
                None,
                None,
                entry.generation,
                0,
                old_config.as_ref().map(|b| b.as_slice()),
                old_ledger.as_ref().map(|b| b.as_slice()),
                old_config.as_ref().map(|b| b.as_slice()).unwrap_or(b""),
                &new_ledger,
                "exchange-rollback",
                None,
                None,
                None,
                None,
            )?;
            Ok(ExchangeStatus {
                contact_name: contact.to_owned(),
                contact_id: entry.contact_id.clone(),
                offer_id: entry.pending_offer_id.clone(),
                state: ExchangeState::RolledBack.as_str().to_owned(),
                peer_fingerprint: entry.peer_fingerprint.clone(),
                created_at: 0,
                expires_at: 0,
                confirmed_at: None,
                installed_at: None,
                activation_deadline: None,
            })
        }
        OverdueDecision::Revoke => {
            entry.state = ExchangeState::Revoked.as_str().to_owned();
            entry.revocation_generation = entry.generation;
            let new_ledger = ledger_bytes(&ledger)?;
            let old_config = workspace.read_optional(CONFIG_FILE)?;
            let old_ledger = workspace.read_optional(LEDGER_FILE)?;
            exchange_journal(
                &workspace,
                scope_id,
                "revocation-tombstone",
                contact,
                &entry.contact_id,
                &entry.pending_offer_id,
                None,
                None,
                entry.generation,
                0,
                old_config.as_ref().map(|b| b.as_slice()),
                old_ledger.as_ref().map(|b| b.as_slice()),
                old_config.as_ref().map(|b| b.as_slice()).unwrap_or(b""),
                &new_ledger,
                "revocation-tombstone",
                None,
                None,
                None,
                None,
            )?;
            Ok(ExchangeStatus {
                contact_name: contact.to_owned(),
                contact_id: entry.contact_id.clone(),
                offer_id: entry.pending_offer_id.clone(),
                state: ExchangeState::Revoked.as_str().to_owned(),
                peer_fingerprint: entry.peer_fingerprint.clone(),
                created_at: 0,
                expires_at: 0,
                confirmed_at: None,
                installed_at: None,
                activation_deadline: None,
            })
        }
    }
}

// ── Stage B: rotation and revocation ─────────────────────────────────────────

pub fn prepare_rotation(
    paths: &TenantPaths,
    scope_id: &str,
    contact: &str,
    current_peer_fingerprint: &PublicFingerprint,
    offer_id: &str,
    contact_id: &str,
    local_public: &str,
    local_private: &str,
    created_at: i64,
    expires_at: i64,
) -> Result<PreparedResult> {
    validate_scope_id(scope_id)?;
    validate_contact_name(contact)?;
    validate_offer_id(offer_id)?;
    validate_offer_id(contact_id)?;
    if expires_at <= created_at {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    public_fingerprint(local_public)?;
    let _ = private_key_bytes(local_private)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;
    let ledger = workspace.read_optional(LEDGER_FILE)?.ok_or(KeyManagerError::InvalidLedger)?;
    let ledger = parse_ledger(&ledger, scope_id)?;
    let entry = ledger.contacts.get(contact).ok_or(KeyManagerError::ContactNotFound)?;
    if entry.state != ExchangeState::PeerVerified.as_str() && entry.state != ExchangeState::Installed.as_str() {
        return Err(KeyManagerError::InvalidExchangeState);
    }
    if entry.peer_fingerprint != current_peer_fingerprint.as_str() {
        return Err(KeyManagerError::FingerprintMismatch);
    }
    let local_fingerprint = public_fingerprint(local_public)?;
    let mut secret = make_initiator_secret(
        offer_id,
        contact_id,
        scope_id,
        contact,
        local_public,
        local_private,
        entry.generation + 1,
        created_at,
        expires_at,
    )?;
    secret.operation = ExchangeOperation::Rotate.as_str().to_owned();
    secret.peer_fingerprint = Some(current_peer_fingerprint.as_str().to_owned());
    secret.peer_contact_id = Some(entry.contact_id.clone());
    write_pending_secret(&workspace, &secret)?;
    let artifact = make_initiator_artifact(
        &secret,
        Some(&entry.contact_id),
        Some(current_peer_fingerprint.as_str()),
        None,
        Some(&entry.peer_fingerprint),
    )?;
    let public_artifact = serde_json::to_string(&artifact).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
    let _ = local_fingerprint;
    Ok(PreparedResult {
        offer_id: offer_id.to_owned(),
        contact_id: contact_id.to_owned(),
        role: ExchangeRole::InitiatorOffer.as_str().to_owned(),
        public_artifact,
        public_fingerprint: artifact.public_fingerprint,
        expires_at,
    })
}

pub fn respond_rotation(
    paths: &TenantPaths,
    scope_id: &str,
    contact: &str,
    offer_bytes: &[u8],
    offer_id: &str,
    contact_id: &str,
    current_peer_fingerprint: &PublicFingerprint,
    expected_new_peer_fingerprint: &PublicFingerprint,
    local_public: &str,
    local_private: &str,
    created_at: i64,
    expires_at: i64,
) -> Result<PreparedResult> {
    validate_scope_id(scope_id)?;
    validate_contact_name(contact)?;
    validate_offer_id(offer_id)?;
    validate_offer_id(contact_id)?;
    if expires_at <= created_at {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    public_fingerprint(local_public)?;
    let _ = private_key_bytes(local_private)?;
    let initiator = validate_public_exchange(offer_bytes, ExchangeRole::InitiatorOffer)?;
    if initiator.public_fingerprint != *expected_new_peer_fingerprint {
        return Err(KeyManagerError::FingerprintMismatch);
    }
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;
    let ledger = workspace.read_optional(LEDGER_FILE)?.ok_or(KeyManagerError::InvalidLedger)?;
    let ledger = parse_ledger(&ledger, scope_id)?;
    let entry = ledger.contacts.get(contact).ok_or(KeyManagerError::ContactNotFound)?;
    if entry.state != ExchangeState::PeerVerified.as_str() && entry.state != ExchangeState::Installed.as_str() {
        return Err(KeyManagerError::InvalidExchangeState);
    }
    if entry.peer_fingerprint != current_peer_fingerprint.as_str() {
        return Err(KeyManagerError::FingerprintMismatch);
    }
    let local_fingerprint = public_fingerprint(local_public)?;
    let mut secret = make_responder_secret(
        offer_id,
        contact_id,
        scope_id,
        contact,
        &initiator.sender_contact_id,
        &initiator,
        local_public,
        local_private,
        created_at,
        expires_at,
    )?;
    secret.operation = ExchangeOperation::Rotate.as_str().to_owned();
    write_pending_secret(&workspace, &secret)?;
    let artifact = make_responder_artifact(&secret, &initiator.offer_id, &initiator.sender_contact_id, initiator.public_fingerprint.as_str(), None, Some(current_peer_fingerprint.as_str()))?;
    let public_artifact = serde_json::to_string(&artifact).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
    let _ = local_fingerprint;
    Ok(PreparedResult {
        offer_id: offer_id.to_owned(),
        contact_id: contact_id.to_owned(),
        role: ExchangeRole::ResponderResponse.as_str().to_owned(),
        public_artifact,
        public_fingerprint: artifact.public_fingerprint,
        expires_at,
    })
}

pub fn complete_rotation(
    paths: &TenantPaths,
    scope_id: &str,
    contact: &str,
    peer_artifact_bytes: &[u8],
    current_peer_fingerprint: &PublicFingerprint,
    expected_new_peer_fingerprint: &PublicFingerprint,
) -> Result<ExchangeStatus> {
    validate_scope_id(scope_id)?;
    validate_contact_name(contact)?;
    let peer = validate_public_exchange(peer_artifact_bytes, ExchangeRole::ResponderResponse)?;
    if peer.public_fingerprint != *expected_new_peer_fingerprint {
        return Err(KeyManagerError::FingerprintMismatch);
    }
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;
    let secret = read_pending_secret(&workspace, peer.in_reply_to.as_deref().ok_or(KeyManagerError::InvalidExchangeArtifact)?)?;
    if secret.state != ExchangeState::Prepared.as_str() {
        return Err(KeyManagerError::InvalidExchangeState);
    }
    if peer.in_reply_to.as_deref() != Some(secret.offer_id.as_str()) {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    if secret.peer_fingerprint.as_deref() != Some(current_peer_fingerprint.as_str()) {
        return Err(KeyManagerError::FingerprintMismatch);
    }
    let initiator_artifact = make_initiator_artifact(&secret, None, None, None, None)?;
    let initiator_canonical = serde_json::to_string(&initiator_artifact).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
    let transcript = compute_transcript(initiator_canonical.as_bytes(), &peer.canonical)?;
    let ledger = workspace.read_optional(LEDGER_FILE)?.ok_or(KeyManagerError::InvalidLedger)?;
    let mut ledger = parse_ledger(&ledger, scope_id)?;
    let mut entry = ledger.contacts.get(contact).cloned().ok_or(KeyManagerError::ContactNotFound)?;
    let old_fingerprint = entry.peer_fingerprint.clone();
    entry.state = ExchangeState::Installed.as_str().to_owned();
    entry.peer_fingerprint = expected_new_peer_fingerprint.as_str().to_owned();
    entry.generation = secret.generation;
    entry.contact_id = secret.contact_id.clone();
    let local_fingerprint = public_fingerprint(&secret.local_public)?;
    entry.local_fingerprint = local_fingerprint;
    let new_ledger = ledger_bytes(&ledger)?;
    let old_config = workspace.read_optional(CONFIG_FILE)?;
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    let _ = old_fingerprint;
    exchange_journal(
        &workspace,
        scope_id,
        "rotation-complete",
        contact,
        &entry.contact_id,
        &secret.offer_id,
        Some(&peer.sender_contact_id),
        Some(&transcript),
        secret.generation,
        peer.generation,
        old_config.as_ref().map(|b| b.as_slice()),
        old_ledger.as_ref().map(|b| b.as_slice()),
        old_config.as_ref().map(|b| b.as_slice()).unwrap_or(b""),
        &new_ledger,
        "rotation-complete",
        None,
        Some(expected_new_peer_fingerprint.as_str()),
        None,
        None,
    )?;
    remove_pending(&workspace, &secret.offer_id)?;
    Ok(ExchangeStatus {
        contact_name: contact.to_owned(),
        contact_id: entry.contact_id.clone(),
        offer_id: secret.offer_id.clone(),
        state: ExchangeState::Installed.as_str().to_owned(),
        peer_fingerprint: entry.peer_fingerprint.clone(),
        created_at: secret.created_at,
        expires_at: secret.expires_at,
        confirmed_at: None,
        installed_at: None,
        activation_deadline: None,
    })
}

pub fn revoke_contact(
    paths: &TenantPaths,
    scope_id: &str,
    contact: &str,
    current_peer_fingerprint: &PublicFingerprint,
    disposition: RevocationDisposition,
) -> Result<ExchangeStatus> {
    validate_scope_id(scope_id)?;
    validate_contact_name(contact)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;
    let ledger = workspace.read_optional(LEDGER_FILE)?.ok_or(KeyManagerError::InvalidLedger)?;
    let mut ledger = parse_ledger(&ledger, scope_id)?;
    let mut entry = ledger.contacts.get(contact).cloned().ok_or(KeyManagerError::ContactNotFound)?;
    if entry.peer_fingerprint != current_peer_fingerprint.as_str() {
        return Err(KeyManagerError::FingerprintMismatch);
    }
    if is_terminal_exchange(&entry.state) {
        return Err(KeyManagerError::InvalidExchangeState);
    }
    entry.state = ExchangeState::Revoked.as_str().to_owned();
    entry.revocation_generation = entry.generation;
    let compromised = matches!(disposition, RevocationDisposition::Compromised);
    entry.compromised = compromised;
    let new_ledger = ledger_bytes(&ledger)?;
    let old_config = workspace.read_optional(CONFIG_FILE)?;
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    exchange_journal(
        &workspace,
        scope_id,
        "revocation-tombstone",
        contact,
        &entry.contact_id,
        &entry.pending_offer_id,
        None,
        None,
        entry.generation,
        0,
        old_config.as_ref().map(|b| b.as_slice()),
        old_ledger.as_ref().map(|b| b.as_slice()),
        old_config.as_ref().map(|b| b.as_slice()).unwrap_or(b""),
        &new_ledger,
        "revocation-tombstone",
        None,
        None,
        None,
        None,
    )?;
    Ok(ExchangeStatus {
        contact_name: contact.to_owned(),
        contact_id: entry.contact_id.clone(),
        offer_id: entry.pending_offer_id.clone(),
        state: ExchangeState::Revoked.as_str().to_owned(),
        peer_fingerprint: entry.peer_fingerprint.clone(),
        created_at: 0,
        expires_at: 0,
        confirmed_at: None,
        installed_at: None,
        activation_deadline: None,
    })
}

// ── Exchange-aware ledger validation for migrations ──────────────────────────

pub fn migration_ready_exchange(paths: &TenantPaths, scope_id: &str) -> Result<()> {
    validate_scope_id(scope_id)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    ensure_migration_settled(&workspace)?;
    recover_locked(&workspace, scope_id)?;
    let ledger = workspace.read_optional(LEDGER_FILE)?.ok_or(KeyManagerError::InvalidLedger)?;
    let ledger = parse_ledger(&ledger, scope_id)?;
    for (name, contact) in &ledger.contacts {
        if is_unresolved_ledger_state(&contact.state) {
            return Err(KeyManagerError::ContactConflict);
        }
        if !is_settled_for_migration(&contact.state) && !is_terminal_exchange(&contact.state) && !matches!(contact.state.as_str(), "PeerVerified" | "RolledBack") {
            return Err(KeyManagerError::ContactConflict);
        }
        let _ = name;
    }
    Ok(())
}


pub fn recover(paths: &TenantPaths, scope_id: &str) -> Result<()> {
    validate_scope_id(scope_id)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    recover_locked(&workspace, scope_id)
}

fn recover_locked(workspace: &Workspace, scope_id: &str) -> Result<()> {
    recovery_scope_preflight(workspace, scope_id)?;

    let candidate_names = secure_directory_files(workspace, CANDIDATES_DIR)?;
    for name in &candidate_names {
        validate_candidate_name(name)?;
    }

    let transaction_names = secure_directory_files(workspace, TRANSACTIONS_DIR)?;
    let mut journals = Vec::new();
    let mut journal_candidates = Vec::new();
    for name in transaction_names {
        if let Some(base) = name.strip_suffix(".next") {
            validate_journal_name(base)?;
            let next_path = format!("{TRANSACTIONS_DIR}/{name}");
            journal_candidates.push(next_path);
        } else {
            validate_journal_name(&name)?;
            journals.push(name);
        }
    }
    journals.sort();
    if journals.len() > 1 {
        return Err(KeyManagerError::RecoveryRequired);
    }

    let Some(name) = journals.first() else {
        recover_migration_candidate(workspace, scope_id)?;
        for path in journal_candidates {
            workspace.remove_if_present(&path)?;
        }
        for name in candidate_names {
            workspace.remove_if_present(&format!("{CANDIDATES_DIR}/{name}"))?;
        }
        return Ok(());
    };
    let transaction_id = name
        .strip_suffix(".json")
        .ok_or(KeyManagerError::RecoveryRequired)?;
    let expected_config = format!("{transaction_id}.toml");
    let expected_ledger = format!("{transaction_id}.ledger.json");
    if candidate_names
        .iter()
        .any(|name| name != &expected_config && name != &expected_ledger)
    {
        return Err(KeyManagerError::RecoveryRequired);
    }
    let path = format!("{TRANSACTIONS_DIR}/{name}");
    recover_journal(workspace, &path, scope_id)?;
    recover_migration_candidate(workspace, scope_id)?;
    for path in journal_candidates {
        workspace.remove_if_present(&path)?;
    }
    Ok(())
}

fn recovery_scope_preflight(workspace: &Workspace, scope_id: &str) -> Result<()> {
    let ledger = workspace.read_optional(LEDGER_FILE)?;
    if let Some(ledger) = ledger.as_ref() {
        // A ledger is the authoritative scope anchor for unjournaled candidates.
        // Check it before any cleanup so a caller for another tenant cannot
        // discard state that belongs to this tenant.
        parse_ledger(ledger, scope_id)?;
    }

    let candidate_names = secure_directory_files(workspace, CANDIDATES_DIR)?;
    for name in &candidate_names {
        validate_candidate_name(name)?;
    }

    let transaction_names = secure_directory_files(workspace, TRANSACTIONS_DIR)?;
    for name in transaction_names {
        let (journal_name, is_candidate) = match name.strip_suffix(".next") {
            Some(base) => (base.to_owned(), true),
            None => (name.clone(), false),
        };
        validate_journal_name(&journal_name)?;
        let path = format!("{TRANSACTIONS_DIR}/{journal_name}");
        let stored_path = if is_candidate {
            format!("{TRANSACTIONS_DIR}/{name}")
        } else {
            path.clone()
        };
        let bytes = workspace.read_required(&stored_path)?;
        let journal = parse_journal_unbound(&bytes, &path)?;
        if journal.scope_id != scope_id {
            return Err(KeyManagerError::ScopeConflict);
        }
    }

    validate_migration_candidate(workspace, scope_id)?;

    // A candidate with no ledger or journal is from a pre-commit crash.  The
    // tenant directory itself is the validated ownership boundary, so recovery
    // can deterministically discard it.  An existing ledger or journal was
    // scope-checked above before any cleanup is allowed.
    Ok(())
}

fn validate_migration_candidate(workspace: &Workspace, scope_id: &str) -> Result<Option<bool>> {
    let Some(candidate) = workspace.read_optional(MIGRATION_FILE_NEXT)? else {
        return Ok(None);
    };
    let manifest = parse_migration_manifest_unbound(&candidate)?;
    if manifest.scope_id != scope_id {
        return Err(KeyManagerError::ScopeConflict);
    }
    let canonical = migration_bytes(&manifest)?;
    if candidate != canonical {
        return Err(KeyManagerError::InvalidLedger);
    }

    let Some(existing) = workspace.read_optional(MIGRATION_FILE)? else {
        return Ok(Some(false));
    };
    let existing_manifest = parse_migration_manifest_unbound(&existing)?;
    if existing_manifest.scope_id != scope_id {
        return Err(KeyManagerError::ScopeConflict);
    }
    if existing != candidate {
        return Err(KeyManagerError::ContactConflict);
    }
    Ok(Some(true))
}

fn recover_migration_candidate(workspace: &Workspace, scope_id: &str) -> Result<()> {
    match validate_migration_candidate(workspace, scope_id)? {
        None => Ok(()),
        Some(true) => workspace.remove_if_present(MIGRATION_FILE_NEXT),
        Some(false) => workspace.rename(MIGRATION_FILE_NEXT, MIGRATION_FILE),
    }
}

fn secure_directory_files(workspace: &Workspace, relative: &str) -> Result<Vec<String>> {
    let directory = workspace.open_relative_dir(relative)?;
    let mut names = Vec::new();
    for entry in directory.read_dir(".").map_err(KeyManagerError::Io)? {
        let entry = entry.map_err(KeyManagerError::Io)?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| KeyManagerError::RecoveryRequired)?;
        let path = format!("{relative}/{name}");
        workspace.read_required(&path)?;
        names.push(name);
    }
    names.sort();
    Ok(names)
}

fn validate_candidate_name(name: &str) -> Result<()> {
    let transaction_id = name
        .strip_suffix(".ledger.json")
        .or_else(|| name.strip_suffix(".toml"))
        .ok_or(KeyManagerError::RecoveryRequired)?;
    validate_transaction_id(transaction_id)
}

fn validate_journal_name(name: &str) -> Result<()> {
    let transaction_id = name
        .strip_suffix(".json")
        .ok_or(KeyManagerError::RecoveryRequired)?;
    validate_transaction_id(transaction_id)
}

fn validate_transaction_id(transaction_id: &str) -> Result<()> {
    if transaction_id.len() != 32
        || !transaction_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(KeyManagerError::RecoveryRequired);
    }
    Ok(())
}

fn recover_journal(workspace: &Workspace, path: &str, scope_id: &str) -> Result<()> {
    let journal_bytes = workspace.read_required(path)?;
    let mut journal = parse_journal_file(&journal_bytes, path, scope_id)?;

    let ledger = workspace.read_optional(LEDGER_FILE)?;
    let config = workspace.read_optional(CONFIG_FILE)?;
    let ledger_hash = hash_optional(ledger.as_ref().map(|bytes| bytes.as_slice()));
    let config_hash = hash_optional(config.as_ref().map(|bytes| bytes.as_slice()));
    match journal.phase()? {
        JournalPhase::Prepared => {
            recover_prepared(workspace, path, &mut journal, ledger_hash, config_hash)
        }
        JournalPhase::LedgerCommitted => {
            recover_ledger_committed(workspace, path, &mut journal, ledger_hash, config_hash)
        }
        JournalPhase::ConfigCommitted => {
            if ledger_hash != Some(journal.new_ledger_hash.clone())
                || config_hash != Some(journal.new_config_hash.clone())
            {
                return Err(KeyManagerError::RecoveryRequired);
            }
            finish_journal(workspace, path, &journal)
        }
        JournalPhase::ActivationStarted | JournalPhase::RollbackStarted => {
            recover_exchange_activation(workspace, path, &mut journal, ledger_hash, config_hash)
        }
        JournalPhase::LocallyActivated | JournalPhase::RolledBack => {
            if ledger_hash != Some(journal.new_ledger_hash.clone()) {
                return Err(KeyManagerError::RecoveryRequired);
            }
            finish_journal(workspace, path, &journal)
        }
        JournalPhase::RevocationTombstoneCommitted | JournalPhase::RevocationRemovalCommitted => {
            if ledger_hash != Some(journal.new_ledger_hash.clone()) {
                return Err(KeyManagerError::RecoveryRequired);
            }
            finish_journal(workspace, path, &journal)
        }
        JournalPhase::PairLocalCoordinator => {
            if ledger_hash != Some(journal.new_ledger_hash.clone())
                || config_hash != Some(journal.new_config_hash.clone())
            {
                return Err(KeyManagerError::RecoveryRequired);
            }
            finish_journal(workspace, path, &journal)
        }
    }
}

fn recover_exchange_activation(
    workspace: &Workspace,
    path: &str,
    journal: &mut JournalFile,
    ledger_hash: Option<String>,
    config_hash: Option<String>,
) -> Result<()> {
    if ledger_hash != Some(journal.new_ledger_hash.clone())
        || config_hash != Some(journal.new_config_hash.clone())
    {
        return Err(KeyManagerError::RecoveryRequired);
    }
    finish_journal(workspace, path, journal)
}

fn parse_journal_file(bytes: &[u8], path: &str, scope_id: &str) -> Result<JournalFile> {
    let journal = parse_journal_unbound(bytes, path)?;
    if journal.scope_id != scope_id {
        return Err(KeyManagerError::ScopeConflict);
    }
    Ok(journal)
}

fn parse_journal_unbound(bytes: &[u8], path: &str) -> Result<JournalFile> {
    let journal: JournalFile =
        serde_json::from_slice(bytes).map_err(|_| KeyManagerError::RecoveryRequired)?;
    if journal.schema != "lunarwing.darkirc-transaction/v1" {
        return Err(KeyManagerError::RecoveryRequired);
    }
    validate_journal_paths(&journal, path)?;
    validate_scope_id(&journal.scope_id)?;
    if !matches!(
        journal.operation.as_str(),
        "baseline-update"
            | "legacy-adopt"
            | "migration-import"
            | "exchange-prepare"
            | "exchange-respond"
            | "exchange-complete"
            | "exchange-cancel"
            | "exchange-activation"
            | "exchange-rollback"
            | "exchange-roundtrip"
            | "rotation-prepare"
            | "rotation-respond"
            | "rotation-complete"
            | "revocation-tombstone"
            | "revocation-removal"
            | "pair-local-coordinator"
    ) || !valid_hash(&journal.new_config_hash)
        || !valid_hash(&journal.new_ledger_hash)
        || journal
            .old_config_hash
            .as_deref()
            .is_some_and(|hash| !valid_hash(hash))
        || journal
            .old_ledger_hash
            .as_deref()
            .is_some_and(|hash| !valid_hash(hash))
    {
        return Err(KeyManagerError::RecoveryRequired);
    }
    journal.phase()?;
    Ok(journal)
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 71
        && hash.starts_with("sha256:")
        && hash[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_journal_paths(journal: &JournalFile, journal_path: &str) -> Result<()> {
    let expected_journal = format!("{TRANSACTIONS_DIR}/{}.json", journal.transaction_id);
    let expected_config = format!("{CANDIDATES_DIR}/{}.toml", journal.transaction_id);
    let expected_ledger = format!("{CANDIDATES_DIR}/{}.ledger.json", journal.transaction_id);
    if journal_path != expected_journal
        || journal.config_candidate != expected_config
        || journal.ledger_candidate != expected_ledger
    {
        return Err(KeyManagerError::RecoveryRequired);
    }
    Ok(())
}

fn recover_prepared(
    workspace: &Workspace,
    path: &str,
    journal: &mut JournalFile,
    ledger_hash: Option<String>,
    config_hash: Option<String>,
) -> Result<()> {
    if ledger_hash == journal.old_ledger_hash && config_hash == journal.old_config_hash {
        return finish_journal(workspace, path, journal);
    }
    if ledger_hash != Some(journal.new_ledger_hash.clone()) {
        return Err(KeyManagerError::RecoveryRequired);
    }
    journal.phase = JournalPhase::LedgerCommitted.as_str().to_owned();
    replace_journal(workspace, path, journal)?;
    recover_ledger_committed(workspace, path, journal, ledger_hash, config_hash)
}

fn recover_ledger_committed(
    workspace: &Workspace,
    path: &str,
    journal: &mut JournalFile,
    ledger_hash: Option<String>,
    config_hash: Option<String>,
) -> Result<()> {
    if ledger_hash != Some(journal.new_ledger_hash.clone()) {
        return Err(KeyManagerError::RecoveryRequired);
    }
    if config_hash == journal.old_config_hash {
        let candidate = workspace
            .read_optional(&journal.config_candidate)?
            .ok_or(KeyManagerError::RecoveryRequired)?;
        if sha256(&candidate) != journal.new_config_hash {
            return Err(KeyManagerError::RecoveryRequired);
        }
        workspace.rename(&journal.config_candidate, CONFIG_FILE)?;
    } else if config_hash != Some(journal.new_config_hash.clone()) {
        return Err(KeyManagerError::RecoveryRequired);
    }
    journal.phase = JournalPhase::ConfigCommitted.as_str().to_owned();
    replace_journal(workspace, path, journal)?;
    finish_journal(workspace, path, journal)
}

fn hash_optional(bytes: Option<&[u8]>) -> Option<String> {
    bytes.map(sha256)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    const SCOPE: &str = "00112233445566778899aabbccddeeff";

    fn fixture() -> (TempDir, TenantPaths) {
        let temp = tempfile::tempdir().expect("fixture tempdir");
        let darkirc = temp.path().join("darkirc");
        fs::create_dir(&darkirc).expect("create darkirc fixture");
        fs::set_permissions(&darkirc, fs::Permissions::from_mode(0o700))
            .expect("secure darkirc fixture");
        (temp, TenantPaths::from_darkirc_dir(darkirc))
    }

    fn write_config(paths: &TenantPaths, contents: &str) {
        let path = paths.darkirc_dir().join(CONFIG_FILE);
        fs::write(&path, contents).expect("write config fixture");
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .expect("secure config fixture");
    }

    #[cfg(unix)]
    #[test]
    fn read_only_open_rejects_non_exact_directory_mode() {
        let (_temp, paths) = fixture();
        write_config(&paths, "irc_listen = \"tcp://127.0.0.1:21001\"\n");
        fs::set_permissions(paths.darkirc_dir(), fs::Permissions::from_mode(0o500))
            .expect("make state directory read-only");

        let error = inspect_contacts(&paths, None).expect_err("mode 0500 must fail closed");

        assert!(matches!(error, KeyManagerError::UnsafePath));
    }

    #[test]
    fn read_only_legacy_snapshot_has_writer_coordination() {
        let (_temp, paths) = fixture();
        write_config(&paths, "irc_listen = \"tcp://127.0.0.1:21001\"\n");

        let workspace = Workspace::open_read_only(paths).expect("open legacy workspace");
        let snapshot = workspace
            .read_snapshot()
            .expect("legacy snapshot should be readable");

        assert!(snapshot.is_some(), "legacy reads must hold a shared lock");
    }

    #[test]
    fn production_tenant_paths_require_expected_owner() {
        let error = TenantPaths::for_tenant("tenant", None, None)
            .expect_err("production paths must require owner identity");

        assert!(matches!(error, KeyManagerError::UnsafePath));
    }

    fn valid_public(byte: u8) -> String {
        bs58::encode([byte; 32]).into_string()
    }

    fn valid_private(byte: u8) -> String {
        bs58::encode([byte; 32]).into_string()
    }

    fn indexed_key(index: usize, seed: u8) -> String {
        let mut bytes = [seed; 32];
        bytes[..8].copy_from_slice(&(index as u64).to_be_bytes());
        bs58::encode(bytes).into_string()
    }

    fn manifest_with_contacts(count: usize) -> Vec<u8> {
        let attestation = attestation('a');
        let mut contacts = String::new();
        let mut ledger = empty_ledger(SCOPE);
        for index in 0..count {
            let name = format!("contact-{index:04}");
            let public = indexed_key(index, 1);
            let private = indexed_key(index, 2);
            contacts.push_str(&format!(
                "[contact.\"{name}\"]\ndm_chacha_public = \"{public}\"\nmy_dm_chacha_secret = \"{private}\"\n\n"
            ));
            ledger.contacts.insert(
                name,
                LedgerContact {
                    state: "legacy-active".to_owned(),
                    peer_fingerprint: public_fingerprint(&public).expect("valid public key"),
                    generation: 0,
                    contact_id: String::new(),
                    local_fingerprint: String::new(),
                    activation_deadline: 0,
                    rollback_tx_id: String::new(),
                    rollback_contact_toml_hash: String::new(),
                    pending_offer_id: String::new(),
                    compromised: false,
                    revocation_generation: 0,
                },
            );
        }
        let ledger_rendered = ledger_bytes(&ledger).expect("render contact ledger");
        let manifest = MigrationManifest {
            schema: MIGRATION_SCHEMA.to_owned(),
            scope_id: SCOPE.to_owned(),
            generator_profile: attestation.generator_profile,
            binary_sha256: attestation.binary_sha256,
            key_format: attestation.key_format,
            source_revision: attestation.source_revision,
            contacts_hash: sha256(contacts.as_bytes()),
            contacts_toml: contacts.into(),
            ledger,
            ledger_hash: sha256(&ledger_rendered),
        };
        serde_json::to_vec(&manifest).expect("serialize contact manifest")
    }

    fn attestation(digit: char) -> CompatibilityAttestation {
        CompatibilityAttestation::new(
            MIGRATION_GENERATOR_PROFILE,
            &format!("sha256:{}", digit.to_string().repeat(64)),
            MIGRATION_KEY_FORMAT,
            MIGRATION_SOURCE_REVISION,
        )
        .expect("valid test attestation")
    }

    fn baseline(port: u16) -> String {
        format!(
            "irc_listen = \"tcp://127.0.0.1:{port}\"\n\n[rpc]\nrpc_listen = \"tcp://127.0.0.1:{}\"\n\n[net]\noutbound_connections = 7\n",
            port + 1
        )
    }

    fn assert_contact_field(config: &[u8], name: &str, field: &str, expected: &str) {
        let document = parse_document(config).expect("parse contact config");
        let contacts = document
            .table
            .get("contact")
            .and_then(Value::as_table)
            .expect("contact table");
        let contact = contacts
            .get(name)
            .and_then(Value::as_table)
            .expect("named contact");
        assert_eq!(contact.get(field).and_then(Value::as_str), Some(expected));
    }

    #[test]
    fn baseline_update_preserves_contacts_and_unknown_fields() {
        let (_temp, paths) = fixture();
        let public = valid_public(42);
        let private = valid_private(43);
        write_config(
            &paths,
            &format!(
                r#"irc_listen = "old"
operator_extension = "keep"

[rpc]
rpc_listen = "old"
operator_rpc_extension = true

[net]
contact = "keep-nested"

[contact."alice"]
dm_chacha_public = "{public}"
my_dm_chacha_secret = "{private}"
legacy_field = "keep"
"#
            ),
        );

        let result = update_baseline(&paths, SCOPE, baseline(21001).as_bytes())
            .expect("contact-preserving update");

        assert!(result.changed);
        let rendered = fs::read_to_string(paths.darkirc_dir().join(CONFIG_FILE))
            .expect("read rendered config");
        assert!(rendered.contains("tcp://127.0.0.1:21001"));
        assert!(rendered.contains("operator_extension = \"keep\""));
        assert!(rendered.contains("operator_rpc_extension = true"));
        assert!(rendered.contains("contact = \"keep-nested\""));
        assert_contact_field(rendered.as_bytes(), "alice", "dm_chacha_public", &public);
        assert!(rendered.contains(&private));
        assert!(rendered.contains("legacy_field = \"keep\""));
        assert_eq!(
            fs::metadata(paths.darkirc_dir().join(CONFIG_FILE))
                .expect("config metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn baseline_rewrites_preserve_unadopted_contacts_before_adoption() {
        let (_temp, paths) = fixture();
        let public = valid_public(44);
        let private = valid_private(45);
        write_config(
            &paths,
            &format!(
                r#"irc_listen = "old"

[contact."alice"]
dm_chacha_public = "{public}"
my_dm_chacha_secret = "{private}"
"#
            ),
        );

        update_baseline(&paths, SCOPE, baseline(21001).as_bytes())
            .expect("first baseline update preserves legacy contact");
        update_baseline(&paths, SCOPE, baseline(22001).as_bytes())
            .expect("second baseline update must remain contact-preserving");

        let rendered = fs::read(paths.darkirc_dir().join(CONFIG_FILE)).expect("read config");
        assert!(
            rendered
                .windows(b"22001".len())
                .any(|window| window == b"22001")
        );
        assert_contact_field(&rendered, "alice", "dm_chacha_public", &public);
        assert_contact_field(&rendered, "alice", "my_dm_chacha_secret", &private);
    }

    #[test]
    fn invalid_baseline_leaves_existing_files_untouched() {
        let (_temp, paths) = fixture();
        let original = "irc_listen = \"old\"\n";
        write_config(&paths, original);

        let error =
            update_baseline(&paths, SCOPE, b"not = [valid").expect_err("invalid TOML must fail");

        assert!(matches!(error, KeyManagerError::InvalidToml));
        assert_eq!(
            fs::read_to_string(paths.darkirc_dir().join(CONFIG_FILE)).expect("read original"),
            original
        );
        assert!(!paths.darkirc_dir().join(LEDGER_FILE).exists());
    }

    #[test]
    fn recovery_finishes_config_after_ledger_commit() {
        let (_temp, paths) = fixture();
        update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("initial update");

        let error = update_baseline_at(
            &paths,
            SCOPE,
            baseline(22001).as_bytes(),
            Some(CrashPoint::AfterLedger),
        )
        .expect_err("inject crash after ledger");
        assert!(matches!(error, KeyManagerError::CrashInjected));
        let before = fs::read_to_string(paths.darkirc_dir().join(CONFIG_FILE))
            .expect("read pre-recovery config");
        assert!(before.contains("21001"));

        recover(&paths, SCOPE).expect("recover exact journaled config");

        let after = fs::read_to_string(paths.darkirc_dir().join(CONFIG_FILE))
            .expect("read recovered config");
        assert!(after.contains("22001"));
        assert_eq!(
            fs::read_dir(paths.darkirc_dir().join(TRANSACTIONS_DIR))
                .expect("read transactions")
                .count(),
            0
        );
    }

    #[test]
    fn recovery_finishes_cleanup_after_config_commit() {
        let (_temp, paths) = fixture();
        update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("initial update");

        let error = update_baseline_at(
            &paths,
            SCOPE,
            baseline(22001).as_bytes(),
            Some(CrashPoint::AfterConfig),
        )
        .expect_err("inject crash after config");
        assert!(matches!(error, KeyManagerError::CrashInjected));

        let installed = fs::read_to_string(paths.darkirc_dir().join(CONFIG_FILE))
            .expect("read installed config");
        assert!(installed.contains("22001"));

        recover(&paths, SCOPE).expect("finish committed transaction cleanup");

        let recovered = fs::read_to_string(paths.darkirc_dir().join(CONFIG_FILE))
            .expect("read recovered config");
        assert_eq!(installed, recovered);
        assert_eq!(
            fs::read_dir(paths.darkirc_dir().join(TRANSACTIONS_DIR))
                .expect("read transactions")
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(paths.darkirc_dir().join(CANDIDATES_DIR))
                .expect("read candidates")
                .count(),
            0
        );
    }

    #[test]
    fn recovery_completes_valid_interrupted_migration_export() {
        let (_temp, paths) = fixture();
        update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("initial update");
        export_migration(&paths, SCOPE, &attestation('a')).expect("export migration");
        let final_path = paths.darkirc_dir().join(MIGRATION_FILE);
        let candidate_path = paths.darkirc_dir().join(MIGRATION_FILE_NEXT);
        let expected = fs::read(&final_path).expect("read exported migration");
        fs::rename(&final_path, &candidate_path).expect("simulate crash before export rename");

        recover(&paths, SCOPE).expect("complete interrupted migration export");

        assert_eq!(
            fs::read(&final_path).expect("read recovered migration"),
            expected
        );
        assert!(!candidate_path.exists());
    }

    #[test]
    fn failed_journal_recovery_preserves_migration_candidate() {
        let (_temp, paths) = fixture();
        update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("initial update");
        let error = update_baseline_at(
            &paths,
            SCOPE,
            baseline(22001).as_bytes(),
            Some(CrashPoint::AfterLedger),
        )
        .expect_err("inject crash after ledger");
        assert!(matches!(error, KeyManagerError::CrashInjected));

        let migration_candidate = paths.darkirc_dir().join(MIGRATION_FILE_NEXT);
        let manifest: MigrationManifest =
            serde_json::from_slice(&manifest_with_contacts(0)).expect("parse migration manifest");
        let migration_bytes = migration_bytes(&manifest)
            .expect("render canonical migration manifest")
            .to_vec();
        fs::write(&migration_candidate, &migration_bytes).expect("write migration candidate");
        fs::set_permissions(&migration_candidate, fs::Permissions::from_mode(0o600))
            .expect("secure migration candidate");
        fs::write(paths.darkirc_dir().join(CONFIG_FILE), baseline(23001))
            .expect("write unexpected current config");

        let error = recover(&paths, SCOPE).expect_err("ambiguous hashes must fail closed");

        assert!(matches!(error, KeyManagerError::RecoveryRequired));
        assert_eq!(
            fs::read(&migration_candidate).expect("read preserved migration candidate"),
            migration_bytes
        );
        assert_eq!(
            fs::read_dir(paths.darkirc_dir().join(TRANSACTIONS_DIR))
                .expect("read preserved journal")
                .filter_map(std::result::Result::ok)
                .count(),
            1
        );
    }

    #[test]
    fn read_only_snapshots_fail_closed_without_recovering_transaction() {
        let (_temp, paths) = fixture();
        update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("initial update");
        let error = update_baseline_at(
            &paths,
            SCOPE,
            baseline(22001).as_bytes(),
            Some(CrashPoint::AfterJournal),
        )
        .expect_err("inject crash after journal");
        assert!(matches!(error, KeyManagerError::CrashInjected));

        let config_before = fs::read(paths.darkirc_dir().join(CONFIG_FILE)).expect("config before");
        let ledger_before = fs::read(paths.darkirc_dir().join(LEDGER_FILE)).expect("ledger before");
        let candidate_count = fs::read_dir(paths.darkirc_dir().join(CANDIDATES_DIR))
            .expect("candidate directory")
            .count();
        let journal_count = fs::read_dir(paths.darkirc_dir().join(TRANSACTIONS_DIR))
            .expect("transaction directory")
            .count();

        let inspect_error = inspect_contacts(&paths, Some(SCOPE))
            .expect_err("inspection must not expose an unfinished transaction");
        let doctor_error = doctor_contacts(&paths, Some(SCOPE))
            .expect_err("doctor must not expose an unfinished transaction");
        let ledger_error =
            read_ledger(&paths, SCOPE).expect_err("ledger read must require recovery");

        assert!(matches!(inspect_error, KeyManagerError::RecoveryRequired));
        assert!(matches!(doctor_error, KeyManagerError::RecoveryRequired));
        assert!(matches!(ledger_error, KeyManagerError::RecoveryRequired));
        assert_eq!(
            fs::read(paths.darkirc_dir().join(CONFIG_FILE)).expect("config after"),
            config_before
        );
        assert_eq!(
            fs::read(paths.darkirc_dir().join(LEDGER_FILE)).expect("ledger after"),
            ledger_before
        );
        assert_eq!(
            fs::read_dir(paths.darkirc_dir().join(CANDIDATES_DIR))
                .expect("candidate directory after")
                .count(),
            candidate_count
        );
        assert_eq!(
            fs::read_dir(paths.darkirc_dir().join(TRANSACTIONS_DIR))
                .expect("transaction directory after")
                .count(),
            journal_count
        );
    }

    #[test]
    fn read_only_snapshot_rejects_interrupted_manifest_candidate() {
        let (_temp, paths) = fixture();
        update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("initial update");
        let candidate = paths.darkirc_dir().join(format!("{MIGRATION_FILE}.next"));
        fs::write(&candidate, b"partial manifest").expect("write manifest candidate");
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600))
            .expect("secure manifest candidate");

        let error = inspect_contacts(&paths, Some(SCOPE))
            .expect_err("read-only inspection must reject an interrupted manifest write");

        assert!(matches!(error, KeyManagerError::RecoveryRequired));
        assert!(candidate.exists());
    }

    #[test]
    fn crash_after_journal_keeps_old_state_and_replay_succeeds() {
        let (_temp, paths) = fixture();
        update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("initial update");

        let error = update_baseline_at(
            &paths,
            SCOPE,
            baseline(22001).as_bytes(),
            Some(CrashPoint::AfterJournal),
        )
        .expect_err("inject crash after journal");
        assert!(matches!(error, KeyManagerError::CrashInjected));

        recover(&paths, SCOPE).expect("discard uncommitted candidates");
        let old =
            fs::read_to_string(paths.darkirc_dir().join(CONFIG_FILE)).expect("read old config");
        assert!(old.contains("21001"));
        update_baseline(&paths, SCOPE, baseline(22001).as_bytes()).expect("replay update");
        let new = fs::read_to_string(paths.darkirc_dir().join(CONFIG_FILE))
            .expect("read replayed config");
        assert!(new.contains("22001"));
    }

    #[test]
    fn repeated_identical_update_does_not_create_a_transaction() {
        let (_temp, paths) = fixture();
        update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("initial update");

        let replay =
            update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("idempotent replay");

        assert!(!replay.changed);
        assert!(replay.transaction_id.is_none());
        assert_eq!(
            fs::read_dir(paths.darkirc_dir().join(TRANSACTIONS_DIR))
                .expect("read transactions")
                .count(),
            0
        );
    }

    #[test]
    fn adoption_writes_metadata_only_and_marks_unattested_contacts_noncompliant() {
        let (_temp, paths) = fixture();
        let alice_public = valid_public(1);
        let bob_public = valid_public(2);
        let alice_private = valid_private(3);
        let bob_private = valid_private(4);
        write_config(
            &paths,
            &format!(
                r#"[contact."alice"]
dm_chacha_public = "{alice_public}"
my_dm_chacha_secret = "{alice_private}"

[contact."bob"]
dm_chacha_public = "{bob_public}"
my_dm_chacha_secret = "{bob_private}"
"#
            ),
        );

        adopt_contacts(&paths, SCOPE).expect("adopt legacy contacts");

        let ledger_bytes =
            fs::read(paths.darkirc_dir().join(LEDGER_FILE)).expect("read adopted ledger");
        let ledger_text = std::str::from_utf8(&ledger_bytes).expect("ledger utf8");
        assert!(!ledger_text.contains(&alice_private));
        assert!(!ledger_text.contains(&bob_private));
        assert!(!ledger_text.contains(&alice_public));
        let ledger = parse_ledger(&ledger_bytes, SCOPE).expect("parse adopted ledger");
        assert_eq!(ledger.contacts["alice"].state, "legacy-noncompliant");
        assert_eq!(ledger.contacts["bob"].state, "legacy-noncompliant");
        assert!(
            ledger.contacts["alice"]
                .peer_fingerprint
                .starts_with("sha256:")
        );
    }

    #[test]
    fn adoption_is_idempotent_and_scope_conflicts_do_not_mutate() {
        let (_temp, paths) = fixture();
        write_config(
            &paths,
            &format!(
                "[contact.\"alice\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{}\"\n",
                valid_public(3),
                valid_private(5)
            ),
        );
        adopt_contacts(&paths, SCOPE).expect("first adoption");
        let before = fs::read(paths.darkirc_dir().join(LEDGER_FILE)).expect("read ledger");

        let replay = adopt_contacts(&paths, SCOPE).expect("idempotent adoption");
        assert!(!replay.changed);
        let error =
            adopt_contacts(&paths, "ffeeddccbbaa99887766554433221100").expect_err("scope conflict");
        assert!(matches!(error, KeyManagerError::ScopeConflict));
        let after = fs::read(paths.darkirc_dir().join(LEDGER_FILE)).expect("read ledger again");
        assert_eq!(before, after);
    }

    #[test]
    fn inspection_reports_adopted_ledger_state() {
        let (_temp, paths) = fixture();
        write_config(
            &paths,
            &format!(
                "[contact.\"alice\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{}\"\n",
                valid_public(6),
                valid_private(7)
            ),
        );
        adopt_contacts(&paths, SCOPE).expect("adopt contact");

        let contacts = inspect_contacts(&paths, Some(SCOPE)).expect("inspect adopted contact");
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].state, "legacy-noncompliant");
    }

    #[test]
    fn inspection_rejects_foreign_scope_ledger() {
        let (_temp, paths) = fixture();
        write_config(
            &paths,
            &format!(
                "[contact.\"alice\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{}\"\n",
                valid_public(6),
                valid_private(7)
            ),
        );
        adopt_contacts(&paths, SCOPE).expect("adopt contact");

        let error = inspect_contacts(&paths, Some("ffeeddccbbaa99887766554433221100"))
            .expect_err("foreign ledger scope must fail closed");

        assert!(matches!(error, KeyManagerError::ScopeConflict));
    }

    #[test]
    fn doctor_reports_reuse_unknown_fields_and_missing_ledger_read_only() {
        let (_temp, paths) = fixture();
        write_config(
            &paths,
            &format!(
                "[contact.\"alice\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{private}\"\nlegacy_field = \"keep\"\n\n[contact.\"bob\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{private}\"\n",
                valid_public(21),
                valid_public(22),
                private = valid_private(23)
            ),
        );

        let report = doctor_contacts(&paths, None).expect("read-only legacy doctor");
        let codes = report
            .issues
            .iter()
            .map(|issue| issue.code.as_str())
            .collect::<HashSet<_>>();

        assert!(codes.contains("private-key-reuse"));
        assert!(codes.contains("unknown-contact-fields"));
        assert!(codes.contains("ledger-missing"));
        assert!(!paths.darkirc_dir().join(LEDGER_FILE).exists());
    }

    #[test]
    fn adoption_rejects_private_reuse_without_creating_ledger() {
        let (_temp, paths) = fixture();
        write_config(
            &paths,
            &format!(
                "[contact.\"alice\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{private}\"\n\n[contact.\"bob\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{private}\"\n",
                valid_public(1),
                valid_public(2),
                private = valid_private(24)
            ),
        );

        let error = adopt_contacts(&paths, SCOPE).expect_err("private reuse must block adoption");

        assert!(matches!(error, KeyManagerError::ContactConflict));
        assert!(!paths.darkirc_dir().join(LEDGER_FILE).exists());
    }

    #[test]
    fn adoption_rejects_duplicate_peer_keys_without_creating_ledger() {
        let (_temp, paths) = fixture();
        let public = valid_public(12);
        let alice_private = valid_private(25);
        let bob_private = valid_private(26);
        write_config(
            &paths,
            &format!(
                "[contact.\"alice\"]\ndm_chacha_public = \"{public}\"\nmy_dm_chacha_secret = \"{alice_private}\"\n\n[contact.\"bob\"]\ndm_chacha_public = \"{public}\"\nmy_dm_chacha_secret = \"{bob_private}\"\n"
            ),
        );

        let error = adopt_contacts(&paths, SCOPE).expect_err("peer key reuse must block adoption");

        assert!(matches!(error, KeyManagerError::ContactConflict));
        assert!(!paths.darkirc_dir().join(LEDGER_FILE).exists());
    }

    #[test]
    fn adoption_rejects_malformed_private_key_without_creating_ledger() {
        let (_temp, paths) = fixture();
        write_config(
            &paths,
            &format!(
                "[contact.\"alice\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"not-base58!\"\n",
                valid_public(14)
            ),
        );

        let error = adopt_contacts(&paths, SCOPE).expect_err("private key format must be checked");

        assert!(matches!(error, KeyManagerError::InvalidToml));
        assert!(!paths.darkirc_dir().join(LEDGER_FILE).exists());
    }

    #[test]
    fn recovery_cleans_unjournaled_candidate_after_precommit_crash() {
        let (_temp, paths) = fixture();
        update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("initial update");
        let candidate = paths
            .darkirc_dir()
            .join(CANDIDATES_DIR)
            .join("0123456789abcdef0123456789abcdef.toml");
        fs::write(&candidate, b"uncommitted").expect("write orphan candidate");
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600))
            .expect("secure orphan candidate");

        recover(&paths, SCOPE).expect("orphan candidate is safe to discard");

        assert!(!candidate.exists());
    }

    #[test]
    fn recovery_rejects_foreign_scope_before_mutation() {
        let (_temp, paths) = fixture();
        update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("initial update");
        let error = update_baseline_at(
            &paths,
            SCOPE,
            baseline(22001).as_bytes(),
            Some(CrashPoint::AfterLedger),
        )
        .expect_err("inject crash after ledger");
        assert!(matches!(error, KeyManagerError::CrashInjected));
        let config_before = fs::read(paths.darkirc_dir().join(CONFIG_FILE)).expect("config before");

        let error = recover(&paths, "ffeeddccbbaa99887766554433221100")
            .expect_err("foreign scope recovery must fail closed");

        assert!(matches!(error, KeyManagerError::ScopeConflict));
        assert_eq!(
            fs::read(paths.darkirc_dir().join(CONFIG_FILE)).expect("config after"),
            config_before
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_symlink_is_rejected_without_touching_target() {
        use std::os::unix::fs::symlink;

        let (_temp, paths) = fixture();
        let outside = paths
            .darkirc_dir()
            .parent()
            .expect("fixture parent")
            .join("outside");
        fs::write(&outside, "unchanged").expect("write outside target");
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600))
            .expect("secure outside target");
        symlink(&outside, paths.darkirc_dir().join(CONFIG_FILE)).expect("create config symlink");

        let error = update_baseline(&paths, SCOPE, baseline(21001).as_bytes())
            .expect_err("reject config symlink");

        assert!(matches!(error, KeyManagerError::UnsafePath));
        assert_eq!(
            fs::read_to_string(outside).expect("read outside target"),
            "unchanged"
        );
    }

    #[test]
    fn nickname_path_injection_is_rejected_without_mutation() {
        let (_temp, paths) = fixture();
        let original = format!(
            "[contact.\"../escape\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{}\"\n",
            valid_public(4),
            valid_private(27)
        );
        write_config(&paths, &original);

        let error = update_baseline(&paths, SCOPE, baseline(21001).as_bytes())
            .expect_err("reject nickname path injection");

        assert!(matches!(error, KeyManagerError::InvalidContact));
        assert_eq!(
            fs::read_to_string(paths.darkirc_dir().join(CONFIG_FILE)).expect("read original"),
            original
        );
    }

    #[test]
    fn structured_migration_preserves_contacts_but_keeps_target_ports() {
        let (_source_temp, source) = fixture();
        let public = valid_public(5);
        let private = valid_private(28);
        write_config(
            &source,
            &format!(
                "irc_listen = \"tcp://127.0.0.1:21001\"\n\n[contact.\"alice\"]\ndm_chacha_public = \"{public}\"\nmy_dm_chacha_secret = \"{private}\"\n"
            ),
        );
        adopt_contacts(&source, SCOPE).expect("source adoption");

        export_migration(&source, SCOPE, &attestation('a')).expect("settled source export");
        let manifest =
            fs::read(source.darkirc_dir().join(MIGRATION_FILE)).expect("read migration manifest");
        let manifest_json: serde_json::Value =
            serde_json::from_slice(&manifest).expect("parse migration manifest");
        assert_eq!(manifest_json["schema"], MIGRATION_SCHEMA);
        assert_eq!(manifest_json["scope_id"], SCOPE);
        assert_eq!(
            manifest_json["generator_profile"],
            MIGRATION_GENERATOR_PROFILE
        );
        assert!(
            manifest_json["contacts_toml"]
                .as_str()
                .is_some_and(|contacts| {
                    contacts.contains(&format!("my_dm_chacha_secret = \"{private}\""))
                        && !contacts.contains("tcp://127.0.0.1:21001")
                })
        );
        assert!(
            manifest_json["contacts_hash"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("sha256:"))
        );
        assert!(
            manifest_json["ledger_hash"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("sha256:"))
        );
        assert_eq!(
            fs::metadata(source.darkirc_dir().join(MIGRATION_FILE))
                .expect("migration manifest metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let (_target_temp, target) = fixture();
        update_baseline(&target, SCOPE, baseline(31001).as_bytes()).expect("fresh target baseline");
        stage_migration(&target, SCOPE, &manifest, &attestation('a'))
            .expect("stage target manifest");
        import_migration(&target, SCOPE, &attestation('a')).expect("import settled contacts");

        let target_config =
            fs::read_to_string(target.darkirc_dir().join(CONFIG_FILE)).expect("read target config");
        assert!(target_config.contains("tcp://127.0.0.1:31001"));
        assert!(target_config.contains("tcp://127.0.0.1:31002"));
        assert!(!target_config.contains("tcp://127.0.0.1:21001"));
        assert!(!target_config.contains("tcp://127.0.0.1:21002"));
        assert_contact_field(
            target_config.as_bytes(),
            "alice",
            "my_dm_chacha_secret",
            &private,
        );
        assert!(target_config.contains(&private));
        assert_eq!(
            read_ledger(&target, SCOPE)
                .expect("target ledger")
                .contacts
                .len(),
            1
        );
        assert!(!target.darkirc_dir().join(MIGRATION_FILE).exists());
    }

    #[test]
    fn migration_attestation_is_bound_and_mismatch_does_not_stage() {
        let (_source_temp, source) = fixture();
        write_config(
            &source,
            &format!(
                "[contact.\"alice\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{}\"\n",
                valid_public(36),
                valid_private(37)
            ),
        );
        adopt_contacts(&source, SCOPE).expect("source adoption");
        let source_attestation = attestation('a');
        export_migration(&source, SCOPE, &source_attestation).expect("source export");
        let manifest =
            fs::read(source.darkirc_dir().join(MIGRATION_FILE)).expect("read migration manifest");
        let manifest_json: serde_json::Value =
            serde_json::from_slice(&manifest).expect("parse migration manifest");
        assert_eq!(
            manifest_json["binary_sha256"],
            source_attestation.binary_sha256()
        );
        assert_eq!(manifest_json["key_format"], MIGRATION_KEY_FORMAT);
        assert_eq!(manifest_json["source_revision"], MIGRATION_SOURCE_REVISION);

        let (_target_temp, target) = fixture();
        update_baseline(&target, SCOPE, baseline(31001).as_bytes()).expect("target baseline");
        let target_attestation = attestation('b');
        let error = stage_migration(&target, SCOPE, &manifest, &target_attestation)
            .expect_err("different binary digest must fail before staging");

        assert!(matches!(error, KeyManagerError::InvalidCompatibility));
        assert!(!target.darkirc_dir().join(MIGRATION_FILE).exists());
    }

    #[test]
    fn migration_merges_nonconflicting_target_contacts_and_ledger() {
        let (_source_temp, source) = fixture();
        let alice_private = valid_private(29);
        write_config(
            &source,
            &format!(
                "irc_listen = \"tcp://127.0.0.1:21001\"\n\n[contact.\"alice\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{alice_private}\"\n",
                valid_public(5)
            ),
        );
        adopt_contacts(&source, SCOPE).expect("source adoption");
        export_migration(&source, SCOPE, &attestation('a')).expect("source export");
        let manifest =
            fs::read(source.darkirc_dir().join(MIGRATION_FILE)).expect("read migration manifest");

        let (_target_temp, target) = fixture();
        let bob_private = valid_private(30);
        write_config(
            &target,
            &format!(
                "irc_listen = \"tcp://127.0.0.1:31001\"\n\n[contact.\"bob\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{bob_private}\"\n",
                valid_public(6)
            ),
        );
        adopt_contacts(&target, SCOPE).expect("target adoption");
        stage_migration(&target, SCOPE, &manifest, &attestation('a'))
            .expect("stage target manifest");
        import_migration(&target, SCOPE, &attestation('a')).expect("merge settled contacts");

        let target_config =
            fs::read_to_string(target.darkirc_dir().join(CONFIG_FILE)).expect("read target config");
        assert_contact_field(
            target_config.as_bytes(),
            "alice",
            "my_dm_chacha_secret",
            &alice_private,
        );
        assert_contact_field(
            target_config.as_bytes(),
            "bob",
            "my_dm_chacha_secret",
            &bob_private,
        );
        let ledger = read_ledger(&target, SCOPE).expect("merged target ledger");
        assert!(ledger.contacts.contains_key("alice"));
        assert!(ledger.contacts.contains_key("bob"));
    }

    #[test]
    fn migration_rejects_unsettled_transaction_state() {
        let (_temp, paths) = fixture();
        update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("baseline");
        fs::write(
            paths.darkirc_dir().join(CANDIDATES_DIR).join("orphan.toml"),
            "private",
        )
        .expect("write orphan candidate");
        fs::set_permissions(
            paths.darkirc_dir().join(CANDIDATES_DIR).join("orphan.toml"),
            fs::Permissions::from_mode(0o600),
        )
        .expect("secure orphan candidate");

        let error = export_migration(&paths, SCOPE, &attestation('a'))
            .expect_err("unsettled state must block");
        assert!(matches!(error, KeyManagerError::RecoveryRequired));
        assert!(!paths.darkirc_dir().join(MIGRATION_FILE).exists());
    }

    #[test]
    fn staged_manifest_blocks_readers_and_new_writes_until_import() {
        let (_temp, paths) = fixture();
        update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("baseline");
        let manifest = manifest_with_contacts(1);
        stage_migration(&paths, SCOPE, &manifest, &attestation('a')).expect("stage manifest");

        let inspect_error = inspect_contacts(&paths, Some(SCOPE))
            .expect_err("read-only inspection must not hide staged migration state");
        assert!(matches!(inspect_error, KeyManagerError::RecoveryRequired));

        let update_error =
            update_baseline(&paths, SCOPE, baseline(22001).as_bytes()).expect_err("block write");
        assert!(matches!(update_error, KeyManagerError::RecoveryRequired));
        assert!(paths.darkirc_dir().join(STAGED_MIGRATION_FILE).exists());
    }

    #[test]
    fn migration_preflight_preserves_valid_orphan_candidate() {
        let (_temp, paths) = fixture();
        update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("baseline");
        let candidate = paths
            .darkirc_dir()
            .join(CANDIDATES_DIR)
            .join("0123456789abcdef0123456789abcdef.toml");
        fs::write(&candidate, b"orphan candidate").expect("write orphan candidate");
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600))
            .expect("secure orphan candidate");

        let error = export_migration(&paths, SCOPE, &attestation('a'))
            .expect_err("valid orphan candidate must block migration");

        assert!(matches!(error, KeyManagerError::RecoveryRequired));
        assert!(candidate.exists(), "preflight must not discard candidate");
        assert!(!paths.darkirc_dir().join(MIGRATION_FILE).exists());
    }

    #[test]
    fn migration_rejects_pending_journal_and_rollback_state() {
        for (directory, file_name) in [
            (TRANSACTIONS_DIR, "unfinished.json"),
            (PENDING_DIR, "offer.json"),
            (ROLLBACK_DIR, "old-config.toml"),
        ] {
            let (_temp, paths) = fixture();
            update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("baseline");
            let state_dir = paths.darkirc_dir().join(directory);
            if !state_dir.exists() {
                fs::create_dir(&state_dir).expect("create excluded state directory");
                fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700))
                    .expect("secure excluded state directory");
            }
            let state_file = state_dir.join(file_name);
            fs::write(&state_file, "secret state").expect("write excluded state");
            fs::set_permissions(&state_file, fs::Permissions::from_mode(0o600))
                .expect("secure excluded state");

            let error = export_migration(&paths, SCOPE, &attestation('a'))
                .expect_err("unsettled state must block");
            assert!(matches!(error, KeyManagerError::RecoveryRequired));
            assert!(!paths.darkirc_dir().join(MIGRATION_FILE).exists());
        }
    }

    #[test]
    fn staging_invalid_or_tampered_manifest_does_not_write_state() {
        let (_target_temp, target) = fixture();
        update_baseline(&target, SCOPE, baseline(31001).as_bytes()).expect("target baseline");

        let invalid = stage_migration(&target, SCOPE, b"not-json", &attestation('a'))
            .expect_err("invalid manifest");
        assert!(matches!(invalid, KeyManagerError::InvalidLedger));
        assert!(!target.darkirc_dir().join(MIGRATION_FILE).exists());
        assert!(!target.darkirc_dir().join(STAGED_MIGRATION_FILE).exists());

        let (_source_temp, source) = fixture();
        update_baseline(&source, SCOPE, baseline(21001).as_bytes()).expect("source baseline");
        export_migration(&source, SCOPE, &attestation('a')).expect("source export");
        let mut manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(source.darkirc_dir().join(MIGRATION_FILE)).expect("read source manifest"),
        )
        .expect("parse source manifest");
        manifest["contacts_toml"] = serde_json::Value::String(
            "[contact.\"mallory\"]\ndm_chacha_public = \"bad\"\nmy_dm_chacha_secret = \"stolen\"\n"
                .to_owned(),
        );
        let tampered = serde_json::to_vec(&manifest).expect("encode tampered manifest");

        let error = stage_migration(&target, SCOPE, &tampered, &attestation('a'))
            .expect_err("checksum mismatch");
        assert!(matches!(error, KeyManagerError::InvalidLedger));
        assert!(!target.darkirc_dir().join(MIGRATION_FILE).exists());
        assert!(!target.darkirc_dir().join(STAGED_MIGRATION_FILE).exists());
    }

    #[test]
    fn staging_recovers_orphaned_manifest_candidate_before_retry() {
        let (_source_temp, source) = fixture();
        write_config(
            &source,
            &format!(
                "[contact.\"alice\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{}\"\n",
                valid_public(13),
                valid_private(31)
            ),
        );
        adopt_contacts(&source, SCOPE).expect("source adoption");
        export_migration(&source, SCOPE, &attestation('a')).expect("source export");
        let manifest =
            fs::read(source.darkirc_dir().join(MIGRATION_FILE)).expect("read source manifest");

        let (_target_temp, target) = fixture();
        update_baseline(&target, SCOPE, baseline(31001).as_bytes()).expect("target baseline");
        let candidate = target.darkirc_dir().join(STAGED_MIGRATION_FILE_NEXT);
        fs::write(&candidate, &manifest).expect("write complete manifest candidate");
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600))
            .expect("secure interrupted manifest candidate");

        stage_migration(&target, SCOPE, &manifest, &attestation('a'))
            .expect("retry after interrupted staging");

        assert_eq!(
            fs::read(target.darkirc_dir().join(STAGED_MIGRATION_FILE))
                .expect("read staged manifest"),
            manifest
        );
        assert!(!candidate.exists());
    }

    #[test]
    fn staging_preserves_malformed_candidate_without_mutation() {
        let (_target_temp, target) = fixture();
        update_baseline(&target, SCOPE, baseline(31001).as_bytes()).expect("target baseline");
        let candidate = target.darkirc_dir().join(STAGED_MIGRATION_FILE_NEXT);
        fs::write(&candidate, b"partial secret-bearing manifest")
            .expect("write interrupted manifest candidate");
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600))
            .expect("secure interrupted manifest candidate");
        let before = fs::read(&candidate).expect("read candidate before retry");

        let error = stage_migration(
            &target,
            SCOPE,
            &manifest_with_contacts(1),
            &attestation('a'),
        )
        .expect_err("malformed staged candidate must block retry");

        assert!(matches!(error, KeyManagerError::InvalidLedger));
        assert_eq!(
            fs::read(&candidate).expect("read candidate after retry"),
            before
        );
        assert!(!target.darkirc_dir().join(STAGED_MIGRATION_FILE).exists());
    }

    #[test]
    fn migration_validation_rejects_unknown_fields_and_oversized_payloads() {
        let (_source_temp, source) = fixture();
        update_baseline(&source, SCOPE, baseline(21001).as_bytes()).expect("source baseline");
        export_migration(&source, SCOPE, &attestation('a')).expect("source export");
        let mut manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(source.darkirc_dir().join(MIGRATION_FILE)).expect("read source manifest"),
        )
        .expect("parse source manifest");
        manifest["unexpected"] = serde_json::Value::Bool(true);
        let unknown_field = serde_json::to_vec(&manifest).expect("encode unknown field");

        let error =
            validate_migration(&unknown_field, &attestation('a')).expect_err("deny unknown field");
        assert!(matches!(error, KeyManagerError::InvalidLedger));

        let oversized = vec![b' '; MAX_MIGRATION_BYTES + 1];
        let error =
            validate_migration(&oversized, &attestation('a')).expect_err("deny oversized manifest");
        assert!(matches!(error, KeyManagerError::InvalidLedger));
    }

    #[test]
    fn migration_validation_rejects_nonzero_legacy_generation() {
        let mut manifest: MigrationManifest = serde_json::from_slice(&manifest_with_contacts(1))
            .expect("parse valid contact manifest");
        manifest
            .ledger
            .contacts
            .values_mut()
            .next()
            .expect("manifest contact")
            .generation = 1;
        let rendered_ledger = ledger_bytes(&manifest.ledger).expect("render mutated ledger");
        manifest.ledger_hash = sha256(&rendered_ledger);
        let bytes = serde_json::to_vec(&manifest).expect("serialize mutated manifest");

        let error = validate_migration(&bytes, &attestation('a'))
            .expect_err("legacy contacts must have generation zero");

        assert!(matches!(error, KeyManagerError::InvalidLedger));

        let (_temp, paths) = fixture();
        let error = stage_migration(&paths, SCOPE, &bytes, &attestation('a'))
            .expect_err("legacy generation must fail before staging");
        assert!(matches!(error, KeyManagerError::InvalidLedger));
        assert!(!paths.darkirc_dir().join(MIGRATION_FILE).exists());
        assert!(!paths.darkirc_dir().join(STAGED_MIGRATION_FILE).exists());
    }

    #[test]
    fn recovery_cleans_orphan_candidate_without_scope_anchor() {
        let (_temp, paths) = fixture();
        let exchange = paths.darkirc_dir().join(KEY_EXCHANGE_DIR);
        let candidates = exchange.join("candidates");
        fs::create_dir_all(&candidates).expect("create candidate directory");
        fs::set_permissions(&exchange, fs::Permissions::from_mode(0o700))
            .expect("secure exchange directory");
        fs::set_permissions(&candidates, fs::Permissions::from_mode(0o700))
            .expect("secure candidate directory");
        let candidate = candidates.join("0123456789abcdef0123456789abcdef.toml");
        fs::write(&candidate, b"orphan candidate").expect("write orphan candidate");
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600))
            .expect("secure orphan candidate");

        recover(&paths, SCOPE).expect("tenant-local pre-journal candidate is safe to discard");

        assert!(!candidate.exists());
    }

    #[test]
    fn recovery_rejects_foreign_scope_before_unjournaled_cleanup() {
        let (_temp, paths) = fixture();
        update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("initial update");
        let candidate = paths
            .darkirc_dir()
            .join(CANDIDATES_DIR)
            .join("0123456789abcdef0123456789abcdef.toml");
        fs::write(&candidate, b"uncommitted").expect("write orphan candidate");
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600))
            .expect("secure orphan candidate");

        let error = recover(&paths, "ffeeddccbbaa99887766554433221100")
            .expect_err("foreign scope must fail before orphan cleanup");

        assert!(matches!(error, KeyManagerError::ScopeConflict));
        assert!(candidate.exists());
    }

    #[cfg(unix)]
    #[test]
    fn special_permission_bits_are_rejected() {
        let (_temp, paths) = fixture();
        write_config(&paths, "irc_listen = \"tcp://127.0.0.1:21001\"\n");
        fs::set_permissions(paths.darkirc_dir(), fs::Permissions::from_mode(0o4700))
            .expect("setuid state directory");

        let error = inspect_contacts(&paths, None)
            .expect_err("setuid bit on state directory must fail closed");

        assert!(matches!(error, KeyManagerError::UnsafePath));

        let (_temp, paths) = fixture();
        write_config(&paths, "irc_listen = \"tcp://127.0.0.1:21001\"\n");
        fs::set_permissions(
            paths.darkirc_dir().join(CONFIG_FILE),
            fs::Permissions::from_mode(0o4600),
        )
        .expect("setgid config file");

        let error =
            inspect_contacts(&paths, None).expect_err("setgid bit on config file must fail closed");

        assert!(matches!(error, KeyManagerError::UnsafePath));

        let (_temp, paths) = fixture();
        update_baseline(&paths, SCOPE, baseline(21001).as_bytes()).expect("baseline");
        fs::set_permissions(
            paths.darkirc_dir().join(KEY_EXCHANGE_DIR),
            fs::Permissions::from_mode(0o4700),
        )
        .expect("setuid exchange directory");

        let error = inspect_contacts(&paths, None)
            .expect_err("setuid bit on exchange directory must fail closed");

        assert!(matches!(error, KeyManagerError::UnsafePath));
    }

    #[test]
    fn staging_rejects_manifest_over_contact_limit_before_mutation() {
        const EXPECTED_MAX_MIGRATION_CONTACTS: usize = 1024;
        let (_temp, paths) = fixture();
        let manifest = manifest_with_contacts(EXPECTED_MAX_MIGRATION_CONTACTS + 1);

        let error = stage_migration(&paths, SCOPE, &manifest, &attestation('a'))
            .expect_err("manifest contact limit must fail before staging");

        assert!(matches!(error, KeyManagerError::InvalidLedger));
        assert!(
            !paths.darkirc_dir().join(KEY_EXCHANGE_DIR).exists(),
            "rejected manifest must not create migration state"
        );
    }

    #[test]
    fn validation_accepts_manifest_at_contact_limit() {
        const EXPECTED_MAX_MIGRATION_CONTACTS: usize = 1024;
        let manifest = manifest_with_contacts(EXPECTED_MAX_MIGRATION_CONTACTS);

        validate_migration(&manifest, &attestation('a')).expect("maximum contact count is valid");
    }

    #[test]
    fn export_rejects_ledger_contact_mismatch_without_writing_manifest() {
        let (_temp, paths) = fixture();
        write_config(
            &paths,
            &format!(
                "[contact.\"alice\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{}\"\n",
                valid_public(8),
                valid_private(32)
            ),
        );
        adopt_contacts(&paths, SCOPE).expect("adopt contact");
        let ledger_path = paths.darkirc_dir().join(LEDGER_FILE);
        let mut ledger = read_ledger(&paths, SCOPE).expect("read adopted ledger");
        ledger
            .contacts
            .get_mut("alice")
            .expect("alice ledger entry")
            .peer_fingerprint = sha256(&[9_u8; 32]);
        fs::write(
            &ledger_path,
            ledger_bytes(&ledger).expect("encode mismatched ledger"),
        )
        .expect("write mismatched ledger");

        let error = export_migration(&paths, SCOPE, &attestation('a'))
            .expect_err("reject fingerprint mismatch");
        assert!(matches!(error, KeyManagerError::ContactConflict));
        assert!(!paths.darkirc_dir().join(MIGRATION_FILE).exists());
    }

    #[test]
    fn export_rejects_missing_or_unsettled_ledger_contact() {
        for ledger_mutation in ["missing", "installing"] {
            let (_temp, paths) = fixture();
            write_config(
                &paths,
                &format!(
                    "[contact.\"alice\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{}\"\n",
                    valid_public(9),
                    valid_private(33)
                ),
            );
            adopt_contacts(&paths, SCOPE).expect("adopt contact");
            let ledger_path = paths.darkirc_dir().join(LEDGER_FILE);
            let mut ledger = read_ledger(&paths, SCOPE).expect("read adopted ledger");
            if ledger_mutation == "missing" {
                ledger.contacts.remove("alice");
            } else {
                ledger
                    .contacts
                    .get_mut("alice")
                    .expect("alice ledger entry")
                    .state = "installing".to_owned();
            }
            fs::write(
                &ledger_path,
                ledger_bytes(&ledger).expect("encode invalid ledger"),
            )
            .expect("write invalid ledger");

            let error = export_migration(&paths, SCOPE, &attestation('a'))
                .expect_err("reject ledger mismatch");
            assert!(matches!(
                error,
                KeyManagerError::ContactConflict | KeyManagerError::InvalidLedger
            ));
            assert!(!paths.darkirc_dir().join(MIGRATION_FILE).exists());
        }
    }

    #[test]
    fn import_rejects_cross_contact_private_reuse_without_mutation() {
        let (_source_temp, source) = fixture();
        let reused_private = valid_private(34);
        write_config(
            &source,
            &format!(
                "[contact.\"alice\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{reused_private}\"\n",
                valid_public(10)
            ),
        );
        adopt_contacts(&source, SCOPE).expect("source adoption");
        export_migration(&source, SCOPE, &attestation('a')).expect("source export");
        let manifest =
            fs::read(source.darkirc_dir().join(MIGRATION_FILE)).expect("read migration manifest");

        let (_target_temp, target) = fixture();
        write_config(
            &target,
            &format!(
                "[contact.\"bob\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{reused_private}\"\n",
                valid_public(11)
            ),
        );
        adopt_contacts(&target, SCOPE).expect("target adoption");
        stage_migration(&target, SCOPE, &manifest, &attestation('a')).expect("stage manifest");
        let config_before =
            fs::read(target.darkirc_dir().join(CONFIG_FILE)).expect("config before");
        let ledger_before =
            fs::read(target.darkirc_dir().join(LEDGER_FILE)).expect("ledger before");

        let error = import_migration(&target, SCOPE, &attestation('a'))
            .expect_err("reject reused private key");
        assert!(matches!(error, KeyManagerError::ContactConflict));
        assert_eq!(
            fs::read(target.darkirc_dir().join(CONFIG_FILE)).expect("config after"),
            config_before
        );
        assert_eq!(
            fs::read(target.darkirc_dir().join(LEDGER_FILE)).expect("ledger after"),
            ledger_before
        );
        assert!(target.darkirc_dir().join(STAGED_MIGRATION_FILE).exists());
    }

    #[test]
    fn migration_validation_returns_only_public_scope_metadata() {
        let (_source_temp, source) = fixture();
        let private = valid_private(35);
        write_config(
            &source,
            &format!(
                "irc_listen = \"tcp://127.0.0.1:21001\"\n\n[contact.\"alice\"]\ndm_chacha_public = \"{}\"\nmy_dm_chacha_secret = \"{private}\"\n",
                valid_public(7)
            ),
        );
        adopt_contacts(&source, SCOPE).expect("source adoption");
        export_migration(&source, SCOPE, &attestation('a')).expect("source export");
        let manifest =
            fs::read(source.darkirc_dir().join(MIGRATION_FILE)).expect("read source manifest");

        let validation =
            validate_migration(&manifest, &attestation('a')).expect("validate manifest in memory");
        let public_json = serde_json::to_string(&validation).expect("serialize validation result");

        assert_eq!(validation.scope_id, SCOPE);
        assert_eq!(public_json, format!(r#"{{"scope_id":"{SCOPE}"}}"#));
        assert!(!public_json.contains("contacts_toml"));
        assert!(!public_json.contains(&private));
    }

    #[test]
    fn migration_scope_conflict_does_not_mutate_target() {
        let (_source_temp, source) = fixture();
        update_baseline(&source, SCOPE, baseline(21001).as_bytes()).expect("source baseline");
        export_migration(&source, SCOPE, &attestation('a')).expect("source export");
        let manifest =
            fs::read(source.darkirc_dir().join(MIGRATION_FILE)).expect("read source manifest");

        let (_target_temp, target) = fixture();
        let other_scope = "ffeeddccbbaa99887766554433221100";
        update_baseline(&target, other_scope, baseline(31001).as_bytes()).expect("target baseline");
        let before = fs::read(target.darkirc_dir().join(CONFIG_FILE)).expect("target before");
        let ledger_before =
            fs::read(target.darkirc_dir().join(LEDGER_FILE)).expect("target ledger before");

        let error = stage_migration(&target, other_scope, &manifest, &attestation('a'))
            .expect_err("scope conflict must fail before staging");
        assert!(matches!(error, KeyManagerError::ScopeConflict));
        assert_eq!(
            fs::read(target.darkirc_dir().join(CONFIG_FILE)).expect("target after"),
            before
        );
        assert_eq!(
            fs::read(target.darkirc_dir().join(LEDGER_FILE)).expect("target ledger after"),
            ledger_before
        );
        assert!(!target.darkirc_dir().join(MIGRATION_FILE).exists());
        assert!(!target.darkirc_dir().join(STAGED_MIGRATION_FILE).exists());
    }

    fn valid_exchange_public() -> String {
        bs58::encode([7_u8; 32]).into_string()
    }

    fn valid_exchange_private() -> String {
        bs58::encode([8_u8; 32]).into_string()
    }

    fn make_initiator_artifact_for_test(
        offer_id: &str,
        contact_id: &str,
        public: &str,
        created: i64,
        expires: i64,
    ) -> String {
        let fp = public_fingerprint(public).expect("valid public key");
        let artifact = serde_json::json!({
            "schema": EXCHANGE_SCHEMA,
            "version": EXCHANGE_SCHEMA_VERSION,
            "artifact_role": "initiator_offer",
            "offer_id": offer_id,
            "sender_contact_id": contact_id,
            "intended_peer_contact_id": null,
            "intended_peer_fingerprint": serde_json::Value::Null,
            "generator_profile": MIGRATION_GENERATOR_PROFILE,
            "binary_sha256": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "key_format": MIGRATION_KEY_FORMAT,
            "source_revision": MIGRATION_SOURCE_REVISION,
            "public_key": public,
            "public_fingerprint": fp,
            "generation": 1_u64,
            "created_at": created,
            "expires_at": expires,
            "label": null,
            "previous_fingerprint": serde_json::Value::Null,
        });
        serde_json::to_string(&artifact).expect("serialize artifact")
    }

    #[test]
    fn prepare_exchange_creates_pending_and_valid_artifact() {
        let (_temp, paths) = fixture();
        let public = valid_exchange_public();
        let private = valid_exchange_private();
        let offer_id = "a".repeat(32);
        let contact_id = "b".repeat(32);
        let request = PrepareExchange {
            contact_name: "alice",
            offer_id: &offer_id,
            contact_id: &contact_id,
            local_public: &public,
            local_private: &private,
            intended_peer_contact_id: None,
            intended_peer_fingerprint: None,
            label: None,
            previous_fingerprint: None,
            generation: 1,
            created_at: 1000,
            expires_at: 2000,
        };
        let result = prepare_exchange(&paths, SCOPE, &request).expect("prepare exchange");
        assert_eq!(result.offer_id, offer_id);
        assert_eq!(result.contact_id, contact_id);
        assert_eq!(result.role, "initiator_offer");
        assert!(result.public_artifact.contains(EXCHANGE_SCHEMA));
        assert!(result.public_artifact.contains("initiator_offer"));
        assert!(!result.public_artifact.contains(&private));
        let parsed: serde_json::Value =
            serde_json::from_str(&result.public_artifact).expect("parse artifact");
        assert_eq!(parsed["public_key"], public);
        assert_eq!(parsed["offer_id"], offer_id);
    }

    #[test]
    fn verify_initiator_rejects_wrong_schema_and_role() {
        let good = make_initiator_artifact_for_test(
            &"a".repeat(32),
            &"b".repeat(32),
            &valid_exchange_public(),
            1000,
            2000,
        );
        validate_public_exchange(good.as_bytes(), ExchangeRole::InitiatorOffer)
            .expect("valid initiator");
        validate_public_exchange(good.as_bytes(), ExchangeRole::ResponderResponse)
            .expect_err("wrong role must fail");

        let mut bad: serde_json::Value = serde_json::from_str(&good).unwrap();
        bad["schema"] = serde_json::Value::String("wrong/v1".to_owned());
        let bad_bytes = serde_json::to_vec(&bad).unwrap();
        validate_public_exchange(&bad_bytes, ExchangeRole::InitiatorOffer)
            .expect_err("wrong schema must fail");

        let mut bad: serde_json::Value = serde_json::from_str(&good).unwrap();
        bad["public_key"] = serde_json::Value::String("wrong".to_owned());
        let bad_bytes = serde_json::to_vec(&bad).unwrap();
        validate_public_exchange(&bad_bytes, ExchangeRole::InitiatorOffer)
            .expect_err("fingerprint mismatch must fail");
    }

    #[test]
    fn respond_exchange_requires_expected_initiator_fingerprint() {
        let (_temp, paths) = fixture();
        let init_public = valid_exchange_public();
        let init_artifact = make_initiator_artifact_for_test(
            &"a".repeat(32),
            &"b".repeat(32),
            &init_public,
            1000,
            2000,
        );
        let expected = validate_public_exchange(init_artifact.as_bytes(), ExchangeRole::InitiatorOffer)
            .expect("valid initiator")
            .public_fingerprint;
        let local_public = bs58::encode([9_u8; 32]).into_string();
        let local_private = bs58::encode([10_u8; 32]).into_string();
        let request = RespondExchange {
            contact_name: "bob",
            offer_id: &"c".repeat(32),
            contact_id: &"d".repeat(32),
            local_public: &local_public,
            local_private: &local_private,
            initiator_offer_id: &"a".repeat(32),
            expected_initiator_fingerprint: &expected,
            label: None,
            created_at: 1000,
            expires_at: 2000,
        };
        let result = respond_exchange(&paths, SCOPE, init_artifact.as_bytes(), &request)
            .expect("respond exchange");
        assert_eq!(result.role, "responder_response");
        assert!(result.public_artifact.contains("responder_response"));
        assert!(!result.public_artifact.contains(&local_private));
        // Mismatched fingerprint
        let wrong_fp = PublicFingerprint::parse("sha256aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .expect("parse test fp");
        let request_bad = RespondExchange {
            expected_initiator_fingerprint: &wrong_fp,
            ..request
        };
        let error = respond_exchange(&paths, SCOPE, init_artifact.as_bytes(), &request_bad)
            .expect_err("mismatched fingerprint must fail");
        assert!(matches!(error, KeyManagerError::FingerprintMismatch));
    }

    #[test]
    fn complete_exchange_installs_contact_as_installed() {
        let (_temp, paths) = fixture();
        let init_public = valid_exchange_public();
        let init_private = valid_exchange_private();
        let resp_public = bs58::encode([11_u8; 32]).into_string();
        let resp_private = bs58::encode([12_u8; 32]).into_string();
        let initiator_offer_id = "a".repeat(32);
        let initiator_contact_id = "b".repeat(32);
        let responder_offer_id = "c".repeat(32);
        let responder_contact_id = "d".repeat(32);

        let init_req = PrepareExchange {
            contact_name: "alice",
            offer_id: &initiator_offer_id,
            contact_id: &initiator_contact_id,
            local_public: &init_public,
            local_private: &init_private,
            intended_peer_contact_id: None,
            intended_peer_fingerprint: None,
            label: None,
            previous_fingerprint: None,
            generation: 1,
            created_at: 1000,
            expires_at: 2000,
        };
        let init_result = prepare_exchange(&paths, SCOPE, &init_req).expect("prepare");

        let initiator_artifact_bytes = init_result.public_artifact.as_bytes();
        let expected_initiator_fp = validate_public_exchange(initiator_artifact_bytes, ExchangeRole::InitiatorOffer)
            .expect("initiator valid").public_fingerprint;
        let resp_req = RespondExchange {
            contact_name: "bob",
            offer_id: &responder_offer_id,
            contact_id: &responder_contact_id,
            local_public: &resp_public,
            local_private: &resp_private,
            initiator_offer_id: &initiator_offer_id,
            expected_initiator_fingerprint: &expected_initiator_fp,
            label: None,
            created_at: 1000,
            expires_at: 2000,
        };
        let resp_secret = SecretExchange {
            schema: EXCHANGE_SCHEMA.to_owned(),
            offer_id: responder_offer_id.clone(),
            role: ExchangeRole::ResponderResponse.as_str().to_owned(),
            operation: "initial".to_owned(),
            contact_id: responder_contact_id.clone(),
            peer_contact_id: Some(initiator_contact_id.clone()),
            local_public: resp_public.clone(),
            local_private: SecretString::from(resp_private.clone()),
            peer_public: Some(init_public.clone()),
            peer_fingerprint: Some(expected_initiator_fp.as_str().to_owned()),
            state: ExchangeState::Prepared.as_str().to_owned(),
            scope_id: SCOPE.to_owned(),
            contact_name: "alice".to_owned(),
            generation: 1,
            created_at: 1000,
            expires_at: 2000,
            transcript: None,
        };
        let pending_path = format!("{}/{}.json", PENDING_DIR, responder_offer_id);
        let pending_bytes = serde_json::to_vec(&resp_secret).expect("serialize resp secret");
        let ws = Workspace::open(paths.clone()).expect("ws");
        ws.write_new(&pending_path, &pending_bytes).expect("write resp pending");

        let resp_artifact = ResponderExchange {
            schema: EXCHANGE_SCHEMA.to_owned(),
            version: EXCHANGE_SCHEMA_VERSION.to_owned(),
            artifact_role: "responder_response".to_owned(),
            offer_id: responder_offer_id.clone(),
            in_reply_to: initiator_offer_id.clone(),
            sender_contact_id: responder_contact_id.clone(),
            intended_peer_contact_id: initiator_contact_id.clone(),
            intended_peer_fingerprint: expected_initiator_fp.as_str().to_owned(),
            generator_profile: MIGRATION_GENERATOR_PROFILE.to_owned(),
            binary_sha256: String::new(),
            key_format: MIGRATION_KEY_FORMAT.to_owned(),
            source_revision: MIGRATION_SOURCE_REVISION.to_owned(),
            public_key: resp_public.clone(),
            public_fingerprint: public_fingerprint(&resp_public).unwrap(),
            generation: 1,
            created_at: 1000,
            expires_at: 2000,
            label: None,
            previous_fingerprint: None,
        };
        let resp_artifact_bytes = serde_json::to_vec(&resp_artifact).expect("serialize resp artifact");
        let initiator_fp_value = PublicFingerprint::parse(&public_fingerprint(&init_public).unwrap()).unwrap();
        let complete_req = CompleteExchange {
            exchange_id: &responder_offer_id,
            expected_peer_fingerprint: &initiator_fp_value,
            defer_apply: true,
        };
        let status = complete_exchange(&paths, SCOPE, &resp_artifact_bytes, &complete_req).expect("complete exchange");
        assert_eq!(status.state, "Installed");
    }

    #[test]
    fn cancel_exchange_removes_pending_and_returns_cancelled() {
        let (_temp, paths) = fixture();
        let offer_id = "a".repeat(32);
        let contact_id = "b".repeat(32);
        let public = valid_exchange_public();
        let private = valid_exchange_private();
        let request = PrepareExchange {
            contact_name: "alice",
            offer_id: &offer_id,
            contact_id: &contact_id,
            local_public: &public,
            local_private: &private,
            intended_peer_contact_id: None,
            intended_peer_fingerprint: None,
            label: None,
            previous_fingerprint: None,
            generation: 1,
            created_at: 1000,
            expires_at: 2000,
        };
        prepare_exchange(&paths, SCOPE, &request).expect("prepare");
        let status = cancel_exchange(&paths, SCOPE, &offer_id).expect("cancel");
        assert_eq!(status.state, "Cancelled");
        let pending_path = format!("{}/{}.json", PENDING_DIR, offer_id);
        assert!(!paths.darkirc_dir().join(pending_path).exists());
    }

    #[test]
    fn compute_transcript_is_deterministic_and_role_ordered() {
        let a = make_initiator_artifact_for_test(&"a".repeat(32), &"b".repeat(32), &valid_exchange_public(), 1, 2);
        let b_public = bs58::encode([0x11_u8; 32]).into_string();
        let b = make_initiator_artifact_for_test(&"c".repeat(32), &"d".repeat(32), &b_public, 1, 2);
        let t1 = compute_transcript(a.as_bytes(), b.as_bytes()).expect("t1");
        let t2 = compute_transcript(a.as_bytes(), b.as_bytes()).expect("t2");
        assert_eq!(t1, t2);
        assert!(t1.starts_with("sha256:"));
        let t_swapped = compute_transcript(b.as_bytes(), a.as_bytes()).expect("t swapped");
        assert_ne!(t1, t_swapped, "transcript is role-order-sensitive");
    }

    #[test]
    fn prepare_exchange_rejects_occupied_contact() {
        let (_temp, paths) = fixture();
        let public = valid_exchange_public();
        let private = valid_exchange_private();
        let request = PrepareExchange {
            contact_name: "alice",
            offer_id: &"a".repeat(32),
            contact_id: &"b".repeat(32),
            local_public: &public,
            local_private: &private,
            intended_peer_contact_id: None,
            intended_peer_fingerprint: None,
            label: None,
            previous_fingerprint: None,
            generation: 1,
            created_at: 1000,
            expires_at: 2000,
        };
        prepare_exchange(&paths, SCOPE, &request).expect("first prepare");
        let request2 = PrepareExchange {
            offer_id: &"x".repeat(32),
            contact_id: &"y".repeat(32),
            ..request
        };
        let error = prepare_exchange(&paths, SCOPE, &request2).expect_err("occupied contact must fail");
        assert!(matches!(error, KeyManagerError::ContactConflict) || matches!(error, KeyManagerError::OfferConflict));
    }

    #[test]
    fn mark_locally_activated_advances_to_locally_activated() {
        let (_temp, paths) = fixture();
        let contact = LedgerContact {
            state: ExchangeState::Installed.as_str().to_owned(),
            peer_fingerprint: "sha256:abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234".to_owned(),
            generation: 1,
            contact_id: "contact1234567890abcdef1234567890abcd".to_owned(),
            local_fingerprint: "sha256:1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd".to_owned(),
            activation_deadline: 0,
            rollback_tx_id: String::new(),
            rollback_contact_toml_hash: String::new(),
            pending_offer_id: String::new(),
            compromised: false,
            revocation_generation: 0,
        };
        let mut ledger = empty_ledger(SCOPE);
        ledger.contacts.insert("alice".to_owned(), contact);
        let ledger_bytes = ledger_bytes(&ledger).expect("render ledger");
        let path = paths.darkirc_dir().join(LEDGER_FILE);
        std::fs::write(&path, &ledger_bytes).expect("write ledger");
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

        let fp = PublicFingerprint::parse("sha256:abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234").unwrap();
        let tx = "tx1234567890abcdef1234567890abcd".to_owned();
        let status = mark_locally_activated(&paths, SCOPE, "alice", &tx, 1000).expect("mark activated");
        assert_eq!(status.state, "LocallyActivated");
        assert_eq!(status.activation_deadline, Some(1000 + ACTIVATION_WINDOW_SECONDS));
    }

    #[test]
    fn rollback_activation_for_compromised_key_sets_marker() {
        let (_temp, paths) = fixture();
        let contact = LedgerContact {
            state: ExchangeState::Installed.as_str().to_owned(),
            peer_fingerprint: "sha256:abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234".to_owned(),
            generation: 1,
            contact_id: "contact1234567890abcdef1234567890abcd".to_owned(),
            local_fingerprint: "sha256:1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd".to_owned(),
            activation_deadline: 0,
            rollback_tx_id: String::new(),
            rollback_contact_toml_hash: String::new(),
            pending_offer_id: String::new(),
            compromised: false,
            revocation_generation: 0,
        };
        let mut ledger = empty_ledger(SCOPE);
        ledger.contacts.insert("mallory".to_owned(), contact);
        let ledger_bytes = ledger_bytes(&ledger).expect("render ledger");
        let path = paths.darkirc_dir().join(LEDGER_FILE);
        std::fs::write(&path, &ledger_bytes).expect("write ledger");
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

        let fp = PublicFingerprint::parse("sha256:abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234").unwrap();
        let tx = "tx1234567890abcdef1234567890abcd".to_owned();
        let status = rollback_activation(&paths, SCOPE, "mallory", &tx, true, None).expect("rollback");
        assert_eq!(status.state, "RolledBack");
        let ledger_after = read_ledger(&paths, SCOPE).expect("read ledger");
        assert!(ledger_after.contacts["mallory"].compromised);
    }

    #[test]
    fn revoke_contact_sets_revoked_and_monotonic_compromised() {
        let (_temp, paths) = fixture();
        let contact = LedgerContact {
            state: ExchangeState::Installed.as_str().to_owned(),
            peer_fingerprint: "sha256:abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234".to_owned(),
            generation: 1,
            contact_id: "contact1234567890abcdef1234567890abcd".to_owned(),
            local_fingerprint: "sha256:1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd".to_owned(),
            activation_deadline: 0,
            rollback_tx_id: String::new(),
            rollback_contact_toml_hash: String::new(),
            pending_offer_id: String::new(),
            compromised: false,
            revocation_generation: 0,
        };
        let mut ledger = empty_ledger(SCOPE);
        ledger.contacts.insert("mallory".to_owned(), contact);
        let ledger_bytes = ledger_bytes(&ledger).expect("render ledger");
        let path = paths.darkirc_dir().join(LEDGER_FILE);
        std::fs::write(&path, &ledger_bytes).expect("write ledger");
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

        let fp = PublicFingerprint::parse("sha256:abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234").unwrap();
        let status = revoke_contact(&paths, SCOPE, "mallory", &fp, RevocationDisposition::Compromised)
            .expect("revoke contact");
        assert_eq!(status.state, "Revoked");
        let ledger_after = read_ledger(&paths, SCOPE).expect("read ledger");
        assert!(ledger_after.contacts["mallory"].compromised);
        assert_eq!(ledger_after.contacts["mallory"].revocation_generation, 1);
    }

    #[test]
    fn confirm_roundtrip_advances_to_peer_verified_and_clears_pending() {
        let (_temp, paths) = fixture();
        let contact = LedgerContact {
            state: ExchangeState::LocallyActivated.as_str().to_owned(),
            peer_fingerprint: "sha256:abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234".to_owned(),
            generation: 1,
            contact_id: "contact1234567890abcdef1234567890abcd".to_owned(),
            local_fingerprint: "sha256:1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd".to_owned(),
            activation_deadline: 0,
            rollback_tx_id: String::new(),
            rollback_contact_toml_hash: String::new(),
            pending_offer_id: "offer1234567890abcdef1234567890abcd".to_owned(),
            compromised: false,
            revocation_generation: 0,
        };
        let mut ledger = empty_ledger(SCOPE);
        ledger.contacts.insert("alice".to_owned(), contact);
        let ledger_bytes = ledger_bytes(&ledger).expect("render ledger");
        let path = paths.darkirc_dir().join(LEDGER_FILE);
        std::fs::write(&path, &ledger_bytes).expect("write ledger");
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        let ws = Workspace::open(paths.clone()).expect("ws open");
        let pending_path = format!("{}/offer1234567890abcdef1234567890abcd.json", PENDING_DIR);
        ws.write_new(&pending_path, b"{\"schema\":\"test\"}").expect("write pending");

        let fp = PublicFingerprint::parse("sha256:abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234").unwrap();
        let status = confirm_roundtrip(&paths, SCOPE, "alice", &"offer1234567890abcdef1234567890abcd", &fp)
            .expect("roundtrip");
        assert_eq!(status.state, "PeerVerified");
        let ledger_after = read_ledger(&paths, SCOPE).expect("ledger");
        assert_eq!(ledger_after.contacts["alice"].state, "PeerVerified");
        assert!(!paths.darkirc_dir().join(&pending_path).exists());
    }

    #[test]
    fn validate_public_exchange_rejects_all_zero_and_wrong_length() {
        let zero_public = bs58::encode([0_u8; 32]).into_string();
        let artifact = make_initiator_artifact_for_test(&"a".repeat(32), &"b".repeat(32), &zero_public, 1000, 2000);
        let error = validate_public_exchange(artifact.as_bytes(), ExchangeRole::InitiatorOffer)
            .expect_err("all-zero key must fail");
        assert!(matches!(error, KeyManagerError::InvalidPublicKey) || matches!(error, KeyManagerError::InvalidExchangeArtifact));
    }

    #[test]
    fn validate_public_exchange_rejects_oversized_artifact() {
        let huge = vec![b'{'; MAX_PENDING_BYTES + 1];
        let error = validate_public_exchange(&huge, ExchangeRole::InitiatorOffer)
            .expect_err("oversized must fail");
        assert!(matches!(error, KeyManagerError::InvalidExchangeArtifact));
    }
}
