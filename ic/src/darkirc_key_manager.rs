//! Tenant-scoped DarkIRC configuration ownership, legacy-contact adoption, and
//! Stage A public key exchange.
//!
//! Public artifacts contain only attested public keys and exchange bindings.
//! Local private keys remain in mode-0600 pending records until an exchange is
//! completed or cancelled. Rotation, activation, rollback, and revocation are
//! not implemented here.

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
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt, Permissions};
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
pub const MAX_PENDING_EXCHANGES: usize = 128;
pub const MAX_PENDING_TOTAL_BYTES: usize = 4 * 1024 * 1024;
pub const ACTIVATION_WINDOW_SECONDS: i64 = 24 * 60 * 60;
pub const EXTENSION_WINDOW_SECONDS: i64 = 7 * 24 * 60 * 60;
pub const DEFAULT_OFFER_TTL_SECONDS: i64 = 30 * 60;
pub const MAX_CLOCK_SKEW_SECONDS: i64 = 5 * 60;
const INITIAL_EXCHANGE_GENERATION: u64 = 1;

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
    pub intended_peer_fingerprint: Option<PublicFingerprint>,
    pub public_key: String,
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
    pub public_artifact: String,
    pub peer_artifact: Option<String>,
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
    #[error("pending exchange capacity exceeded")]
    PendingLimit,
    #[error(
        "exchange committed but public artifact delivery failed; rerun same operation for offer {offer_id}"
    )]
    ArtifactDeliveryFailed { offer_id: String },
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

    pub fn generator_profile(&self) -> &str {
        &self.generator_profile
    }

    pub fn key_format(&self) -> &str {
        &self.key_format
    }

    pub fn source_revision(&self) -> &str {
        &self.source_revision
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    pub schema: String,
    pub scope_id: String,
    #[serde(default)]
    pub contacts: BTreeMap<String, LedgerContact>,
    /// Durable public replay records for cancelled Stage A exchanges.
    #[serde(default)]
    pub consumed_exchange_ids: BTreeMap<String, ExchangeTombstone>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExchangeTombstone {
    pub state: String,
    pub contact_name: String,
    pub contact_id: String,
    pub created_at: i64,
    pub expires_at: i64,
    #[serde(default)]
    pub peer_fingerprint: String,
    /// Local public fingerprint permanently reserved by this exchange.
    #[serde(default)]
    pub local_fingerprint: String,
    #[serde(default)]
    pub local_offer_id: String,
    #[serde(default)]
    pub peer_offer_id: String,
    #[serde(default)]
    pub peer_contact_id: String,
    #[serde(default)]
    pub transcript: String,
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
    /// Canonical public peer artifact needed to settle completion recovery.
    #[serde(default)]
    peer_artifact: String,
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
        let workspace = Self::open_read_only(paths)?;
        let parent_lock = reopen_dir(&workspace.dir)?;
        lock_with_timeout(&parent_lock, true)?;
        let _bootstrap_lock = FileLock {
            file: parent_lock,
            parent: None,
        };
        workspace.prepare_directories()?;
        Ok(workspace)
    }

    fn open_read_only(paths: TenantPaths) -> Result<Self> {
        let dir = open_absolute_dir_nofollow(&paths.darkirc_dir)?;
        validate_open_directory(&dir, paths.expected_uid, paths.expected_gid)?;
        Ok(Self { paths, dir })
    }

    fn prepare_directories(&self) -> Result<()> {
        for relative in [
            KEY_EXCHANGE_DIR,
            CANDIDATES_DIR,
            TRANSACTIONS_DIR,
            PENDING_DIR,
            ROLLBACK_DIR,
        ] {
            let mut directory = self.dir.try_clone().map_err(KeyManagerError::Io)?;
            for component in relative.split('/') {
                let next = match directory.open_dir_nofollow(component) {
                    Ok(next) => next,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        match directory.create_dir(component) {
                            Ok(()) => {
                                directory
                                    .set_permissions(
                                        component,
                                        Permissions::from_std(fs::Permissions::from_mode(0o700)),
                                    )
                                    .map_err(KeyManagerError::Io)?;
                                sync_dir(&directory)?;
                            }
                            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                            Err(error) => return Err(KeyManagerError::Io(error)),
                        }
                        directory
                            .open_dir_nofollow(component)
                            .map_err(KeyManagerError::Io)?
                    }
                    Err(error) => return Err(KeyManagerError::Io(error)),
                };
                validate_open_directory(&next, self.paths.expected_uid, self.paths.expected_gid)?;
                directory = next;
            }
        }
        Ok(())
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
        let parent_lock = reopen_dir(&self.dir)?;
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
        ensure_snapshot_settled(self)?;
        Ok(lock)
    }

    fn read_lock(&self) -> Result<Option<FileLock>> {
        let parent_lock = reopen_dir(&self.dir)?;
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
                sync_dir(&parent_dir)
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
        sync_dir(&parent_dir)?;
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
        sync_dir(&from_dir)?;
        sync_dir(&to_dir)
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

fn reopen_dir(directory: &Dir) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = directory
        .open_with(".", &options)
        .map_err(KeyManagerError::Io)?
        .into_std();
    if !file.metadata().map_err(KeyManagerError::Io)?.is_dir() {
        return Err(KeyManagerError::UnsafePath);
    }
    Ok(file)
}

fn sync_dir(directory: &Dir) -> Result<()> {
    reopen_dir(directory)?
        .sync_all()
        .map_err(KeyManagerError::Io)
}

fn open_absolute_dir_nofollow(path: &Path) -> Result<Dir> {
    if !path.is_absolute() {
        return Err(KeyManagerError::UnsafePath);
    }
    let mut directory =
        Dir::open_ambient_dir("/", ambient_authority()).map_err(KeyManagerError::Io)?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                directory = directory
                    .open_dir_nofollow(name)
                    .map_err(KeyManagerError::Io)?;
            }
            Component::Prefix(_) | Component::ParentDir => {
                return Err(KeyManagerError::UnsafePath);
            }
        }
    }
    Ok(directory)
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
        consumed_exchange_ids: BTreeMap::new(),
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
    let mut contact_ids = HashSet::new();
    let mut local_fingerprints = HashSet::new();
    let mut peer_fingerprints = HashSet::new();
    for (name, contact) in &ledger.contacts {
        validate_contact_name(name).map_err(|_| KeyManagerError::InvalidLedger)?;
        if !matches!(
            contact.state.as_str(),
            "legacy-active" | "legacy-noncompliant" | "Installed"
        ) || !valid_hash(&contact.peer_fingerprint)
            || contact.activation_deadline != 0
            || !contact.rollback_tx_id.is_empty()
            || !contact.rollback_contact_toml_hash.is_empty()
            || !contact.pending_offer_id.is_empty()
            || contact.compromised
            || contact.revocation_generation != 0
        {
            return Err(KeyManagerError::InvalidLedger);
        }
        if contact.state.starts_with("legacy-") {
            if contact.generation != 0
                || !contact.contact_id.is_empty()
                || !contact.local_fingerprint.is_empty()
            {
                return Err(KeyManagerError::InvalidLedger);
            }
        } else if contact.generation == 0
            || validate_offer_id(&contact.contact_id).is_err()
            || !valid_hash(&contact.local_fingerprint)
            || !contact_ids.insert(contact.contact_id.clone())
            || !local_fingerprints.insert(contact.local_fingerprint.clone())
        {
            return Err(KeyManagerError::InvalidLedger);
        }
        if !peer_fingerprints.insert(contact.peer_fingerprint.clone()) {
            return Err(KeyManagerError::InvalidLedger);
        }
    }
    for (exchange_id, tombstone) in &ledger.consumed_exchange_ids {
        validate_offer_id(exchange_id).map_err(|_| KeyManagerError::InvalidLedger)?;
        validate_offer_id(&tombstone.contact_id).map_err(|_| KeyManagerError::InvalidLedger)?;
        validate_contact_name(&tombstone.contact_name)
            .map_err(|_| KeyManagerError::InvalidLedger)?;
        if !matches!(
            tombstone.state.as_str(),
            "Cancelled" | "Expired" | "Installed"
        ) || tombstone.expires_at <= tombstone.created_at
            || (tombstone.state == "Installed" && !valid_hash(&tombstone.peer_fingerprint))
            || (!tombstone.peer_fingerprint.is_empty() && !valid_hash(&tombstone.peer_fingerprint))
            || (!tombstone.local_fingerprint.is_empty()
                && !valid_hash(&tombstone.local_fingerprint))
            || (!tombstone.local_offer_id.is_empty()
                && validate_offer_id(&tombstone.local_offer_id).is_err())
            || (!tombstone.peer_offer_id.is_empty()
                && validate_offer_id(&tombstone.peer_offer_id).is_err())
            || (!tombstone.peer_contact_id.is_empty()
                && validate_offer_id(&tombstone.peer_contact_id).is_err())
            || (!tombstone.transcript.is_empty() && !valid_hash(&tombstone.transcript))
        {
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
    match workspace.open_relative_dir(relative) {
        Ok(directory) => {
            let mut entries = directory.read_dir(".").map_err(KeyManagerError::Io)?;
            if entries
                .next()
                .transpose()
                .map_err(KeyManagerError::Io)?
                .is_some()
            {
                return Err(KeyManagerError::RecoveryRequired);
            }
            Ok(())
        }
        Err(KeyManagerError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn ensure_migration_settled(workspace: &Workspace) -> Result<()> {
    ensure_snapshot_settled(workspace)?;
    validate_state_directory(workspace, PENDING_DIR)?;
    Ok(())
}

fn ensure_snapshot_settled(workspace: &Workspace) -> Result<()> {
    for relative in [CANDIDATES_DIR, TRANSACTIONS_DIR, ROLLBACK_DIR] {
        validate_state_directory(workspace, relative)?;
    }
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
    validate_manifest_ledger(contacts_toml.as_bytes(), &manifest.ledger, false)
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
    validate_manifest_ledger(contacts.expose_secret().as_bytes(), &ledger, false)?;
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
    let mut source_contacts = match source_item {
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
    if source_contacts.iter().any(|(name, source_contact)| {
        target_contacts
            .get(name)
            .is_some_and(|target_contact| target_contact != source_contact)
    }) {
        zeroize_table(&mut source_contacts);
        return Err(KeyManagerError::ContactConflict);
    }
    for (name, mut source_contact) in source_contacts {
        if target_contacts.contains_key(&name) {
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
    for (exchange_id, source_tombstone) in &source.consumed_exchange_ids {
        match target.consumed_exchange_ids.get(exchange_id) {
            Some(target_tombstone) if target_tombstone != source_tombstone => {
                return Err(KeyManagerError::OfferConflict);
            }
            Some(_) => {}
            None => {
                target
                    .consumed_exchange_ids
                    .insert(exchange_id.clone(), source_tombstone.clone());
            }
        }
    }
    validate_ledger(target)
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
        peer_artifact: String::new(),
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
    sync_dir(&workspace.dir)
}

fn finish_committed_journal(
    workspace: &Workspace,
    path: &str,
    journal: &JournalFile,
) -> Result<()> {
    finalize_exchange_pending(workspace, journal)?;
    finish_journal(workspace, path, journal)
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
    if value.len() != 32
        || value.bytes().all(|byte| byte == b'0')
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    Ok(())
}

fn unix_now() -> Result<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .map_err(|_| KeyManagerError::InvalidExchangeArtifact)
}

fn validate_exchange_times(created_at: i64, expires_at: i64, now: i64) -> Result<()> {
    if created_at < 0
        || expires_at <= created_at
        || expires_at - created_at > DEFAULT_OFFER_TTL_SECONDS
        || created_at > now.saturating_add(MAX_CLOCK_SKEW_SECONDS)
        || expires_at < now.saturating_sub(MAX_CLOCK_SKEW_SECONDS)
    {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    Ok(())
}

fn require_live_exchange(expires_at: i64, now: i64) -> Result<()> {
    if now < 0 || now >= expires_at {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validated_artifact(
    role: ExchangeRole,
    offer_id: &str,
    sender_contact_id: &str,
    intended_peer_contact_id: Option<&str>,
    intended_peer_fingerprint: Option<&str>,
    public_key: &str,
    public_fingerprint_field: &str,
    generation: u64,
    created_at: i64,
    expires_at: i64,
    in_reply_to: Option<&str>,
    canonical: Vec<u8>,
    now: i64,
) -> Result<ValidatedPublicArtifact> {
    validate_offer_id(offer_id)?;
    validate_offer_id(sender_contact_id)?;
    if let Some(value) = intended_peer_contact_id {
        validate_offer_id(value)?;
    }
    if let Some(value) = in_reply_to {
        validate_offer_id(value)?;
    }
    validate_exchange_times(created_at, expires_at, now)?;
    let computed = PublicFingerprint::from_encoded_public_key(public_key)?;
    if computed.as_str() != public_fingerprint_field {
        return Err(KeyManagerError::FingerprintMismatch);
    }
    let intended_peer_fingerprint = intended_peer_fingerprint
        .map(PublicFingerprint::parse)
        .transpose()?;
    Ok(ValidatedPublicArtifact {
        role,
        offer_id: offer_id.to_owned(),
        sender_contact_id: sender_contact_id.to_owned(),
        intended_peer_contact_id: intended_peer_contact_id.map(str::to_owned),
        intended_peer_fingerprint,
        public_key: public_key.to_owned(),
        public_fingerprint: computed,
        generation,
        created_at,
        expires_at,
        canonical,
        in_reply_to: in_reply_to.map(str::to_owned),
    })
}

fn validate_artifact_attestation(
    generator_profile: &str,
    binary_sha256: &str,
    key_format: &str,
    source_revision: &str,
    expected: &CompatibilityAttestation,
) -> Result<()> {
    let supplied = CompatibilityAttestation::new(
        generator_profile,
        binary_sha256,
        key_format,
        source_revision,
    )?;
    if &supplied != expected {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    Ok(())
}

pub fn validate_public_exchange(
    bytes: &[u8],
    expected_role: ExchangeRole,
    attestation: &CompatibilityAttestation,
) -> Result<ValidatedPublicArtifact> {
    validate_public_exchange_at(bytes, expected_role, attestation, unix_now()?)
}

pub fn validate_public_exchange_at(
    bytes: &[u8],
    expected_role: ExchangeRole,
    attestation: &CompatibilityAttestation,
    now: i64,
) -> Result<ValidatedPublicArtifact> {
    if bytes.len() > MAX_PENDING_BYTES {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    match expected_role {
        ExchangeRole::InitiatorOffer => {
            let artifact: PreparedExchange = serde_json::from_slice(bytes)
                .map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
            if artifact.schema != EXCHANGE_SCHEMA
                || artifact.version != EXCHANGE_SCHEMA_VERSION
                || artifact.artifact_role != expected_role.as_str()
            {
                return Err(KeyManagerError::InvalidExchangeArtifact);
            }
            validate_artifact_attestation(
                &artifact.generator_profile,
                &artifact.binary_sha256,
                &artifact.key_format,
                &artifact.source_revision,
                attestation,
            )?;
            let canonical = canonical_artifact_bytes(&artifact)?;
            validated_artifact(
                expected_role,
                &artifact.offer_id,
                &artifact.sender_contact_id,
                artifact.intended_peer_contact_id.as_deref(),
                artifact.intended_peer_fingerprint.as_deref(),
                &artifact.public_key,
                &artifact.public_fingerprint,
                artifact.generation,
                artifact.created_at,
                artifact.expires_at,
                None,
                canonical,
                now,
            )
        }
        ExchangeRole::ResponderResponse => {
            let artifact: ResponderExchange = serde_json::from_slice(bytes)
                .map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
            if artifact.schema != EXCHANGE_SCHEMA
                || artifact.version != EXCHANGE_SCHEMA_VERSION
                || artifact.artifact_role != expected_role.as_str()
            {
                return Err(KeyManagerError::InvalidExchangeArtifact);
            }
            validate_artifact_attestation(
                &artifact.generator_profile,
                &artifact.binary_sha256,
                &artifact.key_format,
                &artifact.source_revision,
                attestation,
            )?;
            let canonical = canonical_artifact_bytes(&artifact)?;
            validated_artifact(
                expected_role,
                &artifact.offer_id,
                &artifact.sender_contact_id,
                Some(&artifact.intended_peer_contact_id),
                Some(&artifact.intended_peer_fingerprint),
                &artifact.public_key,
                &artifact.public_fingerprint,
                artifact.generation,
                artifact.created_at,
                artifact.expires_at,
                Some(&artifact.in_reply_to),
                canonical,
                now,
            )
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Exchange state machine: Stage A (Public Exchange V1),
// Stage C (Activation & Rollback), Stage B (Rotation & Revocation)
// ═══════════════════════════════════════════════════════════════════════════

fn offer_id_path(_workspace: &Workspace, offer_id: &str) -> Result<String> {
    validate_offer_id(offer_id)?;
    Ok(format!("{}/{}.json", PENDING_DIR, offer_id))
}

fn validate_stored_public_exchange(
    bytes: &[u8],
    role: ExchangeRole,
) -> Result<ValidatedPublicArtifact> {
    let (attestation, created_at) = match role {
        ExchangeRole::InitiatorOffer => {
            let artifact: PreparedExchange = serde_json::from_slice(bytes)
                .map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
            (
                CompatibilityAttestation::new(
                    &artifact.generator_profile,
                    &artifact.binary_sha256,
                    &artifact.key_format,
                    &artifact.source_revision,
                )?,
                artifact.created_at,
            )
        }
        ExchangeRole::ResponderResponse => {
            let artifact: ResponderExchange = serde_json::from_slice(bytes)
                .map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
            (
                CompatibilityAttestation::new(
                    &artifact.generator_profile,
                    &artifact.binary_sha256,
                    &artifact.key_format,
                    &artifact.source_revision,
                )?,
                artifact.created_at,
            )
        }
    };
    let artifact = validate_public_exchange_at(bytes, role, &attestation, created_at)?;
    if artifact.canonical != bytes {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    Ok(artifact)
}

fn public_artifact_created_at(bytes: &[u8], role: ExchangeRole) -> Result<i64> {
    match role {
        ExchangeRole::InitiatorOffer => serde_json::from_slice::<PreparedExchange>(bytes)
            .map(|artifact| artifact.created_at)
            .map_err(|_| KeyManagerError::InvalidExchangeArtifact),
        ExchangeRole::ResponderResponse => serde_json::from_slice::<ResponderExchange>(bytes)
            .map(|artifact| artifact.created_at)
            .map_err(|_| KeyManagerError::InvalidExchangeArtifact),
    }
}

fn public_artifact_attestation(
    bytes: &[u8],
    role: ExchangeRole,
) -> Result<(CompatibilityAttestation, i64)> {
    match role {
        ExchangeRole::InitiatorOffer => {
            let artifact: PreparedExchange = serde_json::from_slice(bytes)
                .map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
            Ok((
                CompatibilityAttestation::new(
                    &artifact.generator_profile,
                    &artifact.binary_sha256,
                    &artifact.key_format,
                    &artifact.source_revision,
                )?,
                artifact.created_at,
            ))
        }
        ExchangeRole::ResponderResponse => {
            let artifact: ResponderExchange = serde_json::from_slice(bytes)
                .map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
            Ok((
                CompatibilityAttestation::new(
                    &artifact.generator_profile,
                    &artifact.binary_sha256,
                    &artifact.key_format,
                    &artifact.source_revision,
                )?,
                artifact.created_at,
            ))
        }
    }
}

fn validate_pending_peer(secret: &SecretExchange) -> Result<Option<ValidatedPublicArtifact>> {
    let Some(peer_bytes) = secret.peer_artifact.as_deref() else {
        return Ok(None);
    };
    let role = match secret.role.as_str() {
        "initiator_offer" => ExchangeRole::ResponderResponse,
        "responder_response" => ExchangeRole::InitiatorOffer,
        _ => return Err(KeyManagerError::InvalidExchangeArtifact),
    };
    let peer = validate_stored_public_exchange(peer_bytes.as_bytes(), role)?;
    if secret.peer_contact_id.as_deref() != Some(peer.sender_contact_id.as_str())
        || secret.peer_public.as_deref() != Some(peer.public_key.as_str())
        || secret.peer_fingerprint.as_deref() != Some(peer.public_fingerprint.as_str())
    {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    Ok(Some(peer))
}

fn validate_pending_secret(
    secret: &SecretExchange,
    path_offer_id: &str,
    scope_id: &str,
) -> Result<()> {
    validate_offer_id(path_offer_id)?;
    validate_offer_id(&secret.offer_id)?;
    validate_offer_id(&secret.contact_id)?;
    validate_contact_name(&secret.contact_name)?;
    validate_scope_id(&secret.scope_id)?;
    validate_exchange_times(secret.created_at, secret.expires_at, secret.created_at)?;
    public_fingerprint(&secret.local_public)?;
    private_key_bytes(secret.local_private.expose_secret())?;
    if secret.schema != EXCHANGE_SCHEMA
        || secret.offer_id != path_offer_id
        || secret.scope_id != scope_id
        || secret.operation != ExchangeOperation::Initial.as_str()
        || secret.generation != INITIAL_EXCHANGE_GENERATION
        || !matches!(
            secret.state.as_str(),
            "Prepared" | "Confirmed" | "Installed"
        )
    {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    let role = match secret.role.as_str() {
        "initiator_offer" => ExchangeRole::InitiatorOffer,
        "responder_response" => ExchangeRole::ResponderResponse,
        _ => return Err(KeyManagerError::InvalidExchangeArtifact),
    };
    let local = validate_stored_public_exchange(secret.public_artifact.as_bytes(), role)?;
    if local.offer_id != secret.offer_id
        || local.sender_contact_id != secret.contact_id
        || local.public_key != secret.local_public
        || local.generation != secret.generation
        || local.created_at != secret.created_at
        || local.expires_at != secret.expires_at
    {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    let peer = validate_pending_peer(secret)?;
    match (role, secret.state.as_str(), peer.as_ref()) {
        (ExchangeRole::InitiatorOffer, "Prepared", None) => {}
        (ExchangeRole::InitiatorOffer, "Confirmed", Some(response))
            if response.in_reply_to.as_deref() == Some(secret.offer_id.as_str()) => {}
        (ExchangeRole::InitiatorOffer, "Installed", Some(response))
            if response.in_reply_to.as_deref() == Some(secret.offer_id.as_str()) => {}
        (ExchangeRole::ResponderResponse, _, Some(offer))
            if local.in_reply_to.as_deref() == Some(offer.offer_id.as_str())
                && local.intended_peer_contact_id.as_deref()
                    == Some(offer.sender_contact_id.as_str())
                && local.intended_peer_fingerprint.as_ref() == Some(&offer.public_fingerprint) => {}
        _ => return Err(KeyManagerError::InvalidExchangeArtifact),
    }
    if matches!(secret.state.as_str(), "Confirmed" | "Installed") != secret.transcript.is_some()
        || secret
            .transcript
            .as_deref()
            .is_some_and(|value| !valid_hash(value))
    {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    Ok(())
}

struct PendingEntry {
    secret: SecretExchange,
}

struct PendingInventory {
    entries: Vec<PendingEntry>,
    total_bytes: usize,
}

fn pending_inventory(workspace: &Workspace, scope_id: &str) -> Result<PendingInventory> {
    let names = secure_directory_files(workspace, PENDING_DIR)?;
    pending_inventory_from_names(workspace, scope_id, names)
}

fn pending_inventory_from_names(
    workspace: &Workspace,
    scope_id: &str,
    names: Vec<String>,
) -> Result<PendingInventory> {
    if names.len() > MAX_PENDING_EXCHANGES {
        return Err(KeyManagerError::PendingLimit);
    }
    let mut entries = Vec::with_capacity(names.len());
    let mut total_bytes = 0_usize;
    for name in names {
        let offer_id = name
            .strip_suffix(".json")
            .ok_or(KeyManagerError::RecoveryRequired)?;
        validate_offer_id(offer_id).map_err(|_| KeyManagerError::RecoveryRequired)?;
        let path = format!("{PENDING_DIR}/{name}");
        let bytes = workspace.read_required(&path)?;
        if bytes.len() > MAX_PENDING_BYTES {
            return Err(KeyManagerError::InvalidExchangeArtifact);
        }
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .ok_or(KeyManagerError::PendingLimit)?;
        if total_bytes > MAX_PENDING_TOTAL_BYTES {
            return Err(KeyManagerError::PendingLimit);
        }
        let secret: SecretExchange =
            serde_json::from_slice(&bytes).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
        validate_pending_secret(&secret, offer_id, scope_id)?;
        entries.push(PendingEntry { secret });
    }
    validate_pending_entries(&entries)?;
    Ok(PendingInventory {
        entries,
        total_bytes,
    })
}

fn validate_pending_entries(entries: &[PendingEntry]) -> Result<()> {
    let equality_key = private_equality_key();
    let mut contact_names = HashSet::new();
    let mut contact_ids = HashSet::new();
    let mut exchange_ids = HashSet::new();
    let mut private_tags = HashSet::new();
    for entry in entries {
        if !contact_names.insert(entry.secret.contact_name.to_lowercase())
            || !contact_ids.insert(entry.secret.contact_id.clone())
            || !exchange_ids.insert(entry.secret.offer_id.clone())
            || !private_tags.insert(private_equality_tag(
                entry.secret.local_private.expose_secret(),
                &equality_key,
            )?)
        {
            return Err(KeyManagerError::ContactConflict);
        }
        if let Some(peer_offer_id) = pending_peer_offer_id(&entry.secret)?
            && !exchange_ids.insert(peer_offer_id)
        {
            return Err(KeyManagerError::OfferConflict);
        }
        if let Some(peer_contact_id) = entry.secret.peer_contact_id.as_ref()
            && !contact_ids.insert(peer_contact_id.clone())
        {
            return Err(KeyManagerError::OfferConflict);
        }
    }
    Ok(())
}

fn read_pending_secret(
    workspace: &Workspace,
    offer_id: &str,
    scope_id: &str,
) -> Result<SecretExchange> {
    let path = offer_id_path(workspace, offer_id)?;
    let bytes = workspace.read_required(&path)?;
    if bytes.len() > MAX_PENDING_BYTES {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    let secret: SecretExchange =
        serde_json::from_slice(&bytes).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
    validate_pending_secret(&secret, offer_id, scope_id)?;
    Ok(secret)
}

fn pending_secret_bytes(secret: &SecretExchange) -> Result<Zeroizing<Vec<u8>>> {
    validate_pending_secret(secret, &secret.offer_id, &secret.scope_id)?;
    let bytes = Zeroizing::new(
        serde_json::to_vec(secret).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?,
    );
    if bytes.len() > MAX_PENDING_BYTES {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    Ok(bytes)
}

fn write_new_pending_secret(
    workspace: &Workspace,
    secret: &SecretExchange,
    inventory: &PendingInventory,
) -> Result<()> {
    let path = offer_id_path(workspace, &secret.offer_id)?;
    let candidate = format!("{path}.next");
    let bytes = pending_secret_bytes(secret)?;
    let projected_bytes = inventory
        .total_bytes
        .checked_add(bytes.len())
        .ok_or(KeyManagerError::PendingLimit)?;
    if inventory.entries.len() >= MAX_PENDING_EXCHANGES || projected_bytes > MAX_PENDING_TOTAL_BYTES
    {
        return Err(KeyManagerError::PendingLimit);
    }
    if workspace.read_optional(&candidate)?.is_some() {
        return Err(KeyManagerError::RecoveryRequired);
    }
    workspace.write_new(&candidate, &bytes)?;
    workspace.rename(&candidate, &path)
}

fn replace_pending_secret(workspace: &Workspace, secret: &SecretExchange) -> Result<()> {
    let path = offer_id_path(workspace, &secret.offer_id)?;
    let bytes = pending_secret_bytes(secret)?;
    replace_file(workspace, &path, &bytes)
}

fn remove_pending(workspace: &Workspace, offer_id: &str) -> Result<()> {
    let path = offer_id_path(workspace, offer_id)?;
    workspace.remove_if_present(&path)
}

fn validate_active_config(workspace: &Workspace) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let Some(config) = workspace.read_optional(CONFIG_FILE)? else {
        return Ok(None);
    };
    let document = parse_document(&config)?;
    validate_document(&document.table)?;
    validate_contact_uniqueness(&config)?;
    Ok(Some(config))
}

fn contact_occupied(workspace: &Workspace, contact_name: &str, ledger: &Ledger) -> Result<bool> {
    let normalized = contact_name.to_lowercase();
    let config_match = match validate_active_config(workspace)? {
        Some(config) => {
            let document = parse_document(&config)?;
            document
                .table
                .get("contact")
                .and_then(Value::as_table)
                .is_some_and(|contacts| {
                    contacts
                        .keys()
                        .any(|name| name.to_lowercase() == normalized)
                })
        }
        None => false,
    };
    let ledger_match = ledger
        .contacts
        .keys()
        .any(|name| name.to_lowercase() == normalized);
    Ok(config_match || ledger_match)
}

fn private_equality_tag(encoded: &str, equality_key: &[u8; 32]) -> Result<[u8; 32]> {
    let private = private_key_bytes(encoded)?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(equality_key).map_err(|_| KeyManagerError::InvalidToml)?;
    mac.update(&private);
    Ok(mac.finalize().into_bytes().into())
}

fn reject_local_private_reuse(
    workspace: &Workspace,
    inventory: &PendingInventory,
    ledger: &Ledger,
    local_public: &str,
    local_private: &str,
) -> Result<()> {
    let equality_key = private_equality_key();
    let candidate = private_equality_tag(local_private, &equality_key)?;
    if let Some(config) = workspace.read_optional(CONFIG_FILE)? {
        let contacts = collect_contacts(&config, &equality_key)?;
        if contacts
            .iter()
            .any(|contact| contact.private_equality_tag == candidate)
        {
            return Err(KeyManagerError::ContactConflict);
        }
    }
    for entry in &inventory.entries {
        if private_equality_tag(entry.secret.local_private.expose_secret(), &equality_key)?
            == candidate
        {
            return Err(KeyManagerError::ContactConflict);
        }
    }
    let local_fingerprint = public_fingerprint(local_public)?;
    if ledger
        .consumed_exchange_ids
        .values()
        .any(|tombstone| tombstone.local_fingerprint == local_fingerprint)
    {
        return Err(KeyManagerError::ContactConflict);
    }
    Ok(())
}

fn reject_pending_reservation(
    inventory: &PendingInventory,
    contact_name: &str,
    contact_id: &str,
    offer_id: &str,
) -> Result<()> {
    let normalized = contact_name.to_lowercase();
    for entry in &inventory.entries {
        if entry.secret.offer_id == offer_id
            || entry.secret.contact_id == contact_id
            || entry.secret.peer_contact_id.as_deref() == Some(contact_id)
            || pending_peer_offer_id(&entry.secret)?.as_deref() == Some(offer_id)
        {
            return Err(KeyManagerError::OfferConflict);
        }
        if entry.secret.contact_name.to_lowercase() == normalized {
            return Err(KeyManagerError::ContactConflict);
        }
    }
    Ok(())
}

fn append_ledger_contact(ledger: &mut Ledger, name: &str, contact: LedgerContact) -> Result<()> {
    match ledger.contacts.get(name) {
        Some(existing) if existing != &contact => Err(KeyManagerError::ContactConflict),
        Some(_) => Ok(()),
        None => {
            if ledger
                .contacts
                .values()
                .any(|existing| existing.peer_fingerprint == contact.peer_fingerprint)
            {
                return Err(KeyManagerError::ContactConflict);
            }
            ledger.contacts.insert(name.to_owned(), contact);
            Ok(())
        }
    }
}

fn exchange_tombstone(
    secret: &SecretExchange,
    state: ExchangeState,
    peer_fingerprint: &str,
) -> Result<ExchangeTombstone> {
    let peer_offer_id = validate_pending_peer(secret)?
        .map(|peer| peer.offer_id)
        .unwrap_or_default();
    Ok(ExchangeTombstone {
        state: state.as_str().to_owned(),
        contact_name: secret.contact_name.clone(),
        contact_id: secret.contact_id.clone(),
        created_at: secret.created_at,
        expires_at: secret.expires_at,
        peer_fingerprint: peer_fingerprint.to_owned(),
        local_fingerprint: public_fingerprint(&secret.local_public)?,
        local_offer_id: secret.offer_id.clone(),
        peer_offer_id,
        peer_contact_id: secret.peer_contact_id.clone().unwrap_or_default(),
        transcript: secret.transcript.clone().unwrap_or_default(),
    })
}

fn insert_exchange_tombstone(
    ledger: &mut Ledger,
    exchange_id: &str,
    tombstone: &ExchangeTombstone,
) -> Result<()> {
    validate_offer_id(exchange_id)?;
    match ledger.consumed_exchange_ids.get(exchange_id) {
        Some(existing) if existing != tombstone => Err(KeyManagerError::OfferConflict),
        Some(_) => Ok(()),
        None => {
            ledger
                .consumed_exchange_ids
                .insert(exchange_id.to_owned(), tombstone.clone());
            Ok(())
        }
    }
}

fn pending_peer_offer_id(secret: &SecretExchange) -> Result<Option<String>> {
    if secret.role != ExchangeRole::ResponderResponse.as_str() {
        return Ok(None);
    }
    Ok(validate_pending_peer(secret)?.map(|peer| peer.offer_id))
}

fn expire_pending_locked(workspace: &Workspace, scope_id: &str, now: i64) -> Result<()> {
    let inventory = pending_inventory(workspace, scope_id)?;
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    let mut ledger = match old_ledger.as_deref() {
        Some(bytes) => parse_ledger(bytes, scope_id)?,
        None => empty_ledger(scope_id),
    };
    let mut expired = Vec::new();
    let mut settled = Vec::new();
    for entry in &inventory.entries {
        let secret = &entry.secret;
        if installed_reservation_matches(secret, &ledger)? {
            continue;
        }
        if let Some(tombstone) = ledger.consumed_exchange_ids.get(&secret.offer_id) {
            let local_fingerprint = public_fingerprint(&secret.local_public)?;
            if !matches!(tombstone.state.as_str(), "Cancelled" | "Expired")
                || tombstone.contact_name != secret.contact_name
                || tombstone.contact_id != secret.contact_id
                || tombstone.created_at != secret.created_at
                || tombstone.expires_at != secret.expires_at
                || (!tombstone.local_fingerprint.is_empty()
                    && tombstone.local_fingerprint != local_fingerprint)
            {
                return Err(KeyManagerError::OfferConflict);
            }
            settled.push(entry);
            continue;
        }
        if matches!(secret.state.as_str(), "Prepared" | "Confirmed") && now >= secret.expires_at {
            expired.push(entry);
        }
    }
    if expired.is_empty() && settled.is_empty() {
        return Ok(());
    }
    for entry in &expired {
        let secret = &entry.secret;
        let peer_fingerprint = secret.peer_fingerprint.as_deref().unwrap_or_default();
        let tombstone = exchange_tombstone(secret, ExchangeState::Expired, peer_fingerprint)?;
        insert_exchange_tombstone(&mut ledger, &secret.offer_id, &tombstone)?;
        if let Some(peer_offer_id) = pending_peer_offer_id(secret)? {
            insert_exchange_tombstone(&mut ledger, &peer_offer_id, &tombstone)?;
        }
    }
    if !expired.is_empty() {
        replace_file(workspace, LEDGER_FILE, &ledger_bytes(&ledger)?)?;
    }
    for entry in expired.into_iter().chain(settled) {
        remove_pending(workspace, &entry.secret.offer_id)?;
    }
    Ok(())
}

fn installed_reservation_matches(secret: &SecretExchange, ledger: &Ledger) -> Result<bool> {
    let Some(tombstone) = ledger.consumed_exchange_ids.get(&secret.offer_id) else {
        return Ok(false);
    };
    let Some(contact) = ledger.contacts.get(&secret.contact_name) else {
        return Ok(false);
    };
    let local_fingerprint = public_fingerprint(&secret.local_public)?;
    Ok(tombstone.state == ExchangeState::Installed.as_str()
        && tombstone.contact_name == secret.contact_name
        && tombstone.contact_id == secret.contact_id
        && tombstone.peer_fingerprint == contact.peer_fingerprint
        && (tombstone.local_offer_id.is_empty() || tombstone.local_offer_id == secret.offer_id)
        && (secret.state == ExchangeState::Prepared.as_str()
            || ((tombstone.peer_contact_id.is_empty()
                || secret.peer_contact_id.as_deref() == Some(tombstone.peer_contact_id.as_str()))
                && (tombstone.transcript.is_empty()
                    || secret.transcript.as_deref() == Some(tombstone.transcript.as_str()))))
        && contact.state == ExchangeState::Installed.as_str()
        && contact.contact_id == secret.contact_id
        && contact.generation == secret.generation
        && contact.local_fingerprint == local_fingerprint)
}

fn installed_contact_matches(
    workspace: &Workspace,
    secret: &SecretExchange,
    peer: &ValidatedPublicArtifact,
    ledger: &Ledger,
) -> Result<bool> {
    let Some(entry) = ledger.contacts.get(&secret.contact_name) else {
        return Ok(false);
    };
    let local_fingerprint = public_fingerprint(&secret.local_public)?;
    if entry.state != ExchangeState::Installed.as_str()
        || entry.contact_id != secret.contact_id
        || entry.generation != secret.generation
        || entry.peer_fingerprint != peer.public_fingerprint.as_str()
        || entry.local_fingerprint != local_fingerprint
    {
        return Ok(false);
    }

    let Some(config) = workspace.read_optional(CONFIG_FILE)? else {
        return Ok(false);
    };
    let document = parse_document(&config)?;
    let Some(contact) = document
        .table
        .get("contact")
        .and_then(Value::as_table)
        .and_then(|contacts| contacts.get(&secret.contact_name))
        .and_then(Value::as_table)
    else {
        return Ok(false);
    };
    Ok(
        contact.get("dm_chacha_public").and_then(Value::as_str) == Some(peer.public_key.as_str())
            && contact.get("my_dm_chacha_secret").and_then(Value::as_str)
                == Some(secret.local_private.expose_secret()),
    )
}

fn reconcile_installed_pending(workspace: &Workspace, scope_id: &str) -> Result<()> {
    let inventory = pending_inventory(workspace, scope_id)?;
    let Some(ledger_bytes) = workspace.read_optional(LEDGER_FILE)? else {
        return Ok(());
    };
    let ledger = parse_ledger(&ledger_bytes, scope_id)?;
    for entry in inventory.entries {
        let secret = entry.secret;
        if secret.state != ExchangeState::Confirmed.as_str()
            || !installed_reservation_matches(&secret, &ledger)?
        {
            continue;
        }
        let peer =
            validate_pending_peer(&secret)?.ok_or(KeyManagerError::InvalidExchangeArtifact)?;
        if !installed_contact_matches(workspace, &secret, &peer, &ledger)? {
            return Err(KeyManagerError::RecoveryRequired);
        }
        let mut installed = secret;
        installed.state = ExchangeState::Installed.as_str().to_owned();
        replace_pending_secret(workspace, &installed)?;
    }
    Ok(())
}

fn finalize_exchange_pending(workspace: &Workspace, journal: &JournalFile) -> Result<()> {
    if journal.operation_kind != "exchange-complete" {
        return Ok(());
    }
    if journal.peer_artifact.is_empty()
        || journal.peer_artifact.len() > MAX_PENDING_BYTES
        || !valid_hash(&journal.transcript)
        || !valid_hash(&journal.confirmed_peer_fingerprint)
    {
        return Err(KeyManagerError::RecoveryRequired);
    }
    let mut secret = read_pending_secret(workspace, &journal.offer_id, &journal.scope_id)?;
    if !matches!(
        secret.state.as_str(),
        "Prepared" | "Confirmed" | "Installed"
    ) {
        return Err(KeyManagerError::RecoveryRequired);
    }
    let (local_role, peer_role) = match secret.role.as_str() {
        "initiator_offer" => (
            ExchangeRole::InitiatorOffer,
            ExchangeRole::ResponderResponse,
        ),
        "responder_response" => (
            ExchangeRole::ResponderResponse,
            ExchangeRole::InitiatorOffer,
        ),
        _ => return Err(KeyManagerError::RecoveryRequired),
    };
    let (attestation, local_created_at) =
        public_artifact_attestation(secret.public_artifact.as_bytes(), local_role)?;
    let local = validate_public_exchange_at(
        secret.public_artifact.as_bytes(),
        local_role,
        &attestation,
        local_created_at,
    )?;
    let peer_created_at = public_artifact_created_at(journal.peer_artifact.as_bytes(), peer_role)?;
    let peer = validate_public_exchange_at(
        journal.peer_artifact.as_bytes(),
        peer_role,
        &attestation,
        peer_created_at,
    )?;
    let (initiator, responder) = if local_role == ExchangeRole::InitiatorOffer {
        (&local.canonical, &peer.canonical)
    } else {
        (&peer.canonical, &local.canonical)
    };
    if secret.contact_name != journal.contact_name
        || secret.contact_id != journal.contact_id
        || secret.generation != journal.generation
        || peer.sender_contact_id != journal.peer_contact_id
        || peer.public_fingerprint.as_str() != journal.confirmed_peer_fingerprint
        || compute_transcript(initiator, responder)? != journal.transcript
    {
        return Err(KeyManagerError::RecoveryRequired);
    }
    if secret.state != ExchangeState::Prepared.as_str()
        && (secret.peer_artifact.as_deref() != Some(journal.peer_artifact.as_str())
            || secret.transcript.as_deref() != Some(journal.transcript.as_str()))
    {
        return Err(KeyManagerError::RecoveryRequired);
    }
    secret.peer_public = Some(peer.public_key.clone());
    secret.peer_fingerprint = Some(peer.public_fingerprint.as_str().to_owned());
    secret.peer_contact_id = Some(peer.sender_contact_id.clone());
    secret.peer_artifact = Some(journal.peer_artifact.clone());
    secret.transcript = Some(journal.transcript.clone());
    secret.state = ExchangeState::Installed.as_str().to_owned();
    validate_pending_secret(&secret, &secret.offer_id, &secret.scope_id)?;
    let ledger = workspace
        .read_optional(LEDGER_FILE)?
        .ok_or(KeyManagerError::RecoveryRequired)?;
    let ledger = parse_ledger(&ledger, &journal.scope_id)?;
    if !installed_reservation_matches(&secret, &ledger)?
        || !installed_contact_matches(workspace, &secret, &peer, &ledger)?
    {
        return Err(KeyManagerError::RecoveryRequired);
    }
    replace_pending_secret(workspace, &secret)
}

fn contact_toml_for_new_contact(
    contact_name: &str,
    public: &str,
    private: &str,
) -> Result<Zeroizing<Vec<u8>>> {
    validate_contact_name(contact_name)?;
    public_fingerprint(public)?;
    let _ = private_key_bytes(private)?;
    let mut fields = Table::new();
    fields.insert(
        "dm_chacha_public".to_owned(),
        Value::String(public.to_owned()),
    );
    fields.insert(
        "my_dm_chacha_secret".to_owned(),
        Value::String(private.to_owned()),
    );
    let mut contacts = Table::new();
    contacts.insert(contact_name.to_owned(), Value::Table(fields));
    let mut document = SensitiveToml {
        table: Table::new(),
    };
    document
        .table
        .insert("contact".to_owned(), Value::Table(contacts));
    validate_document(&document.table)?;
    let rendered = serialize_table(&document.table)?;
    let check = parse_document(&rendered)?;
    let parsed_contacts = check
        .table
        .get("contact")
        .and_then(Value::as_table)
        .ok_or(KeyManagerError::InvalidToml)?;
    if parsed_contacts.len() != 1 || !parsed_contacts.contains_key(contact_name) {
        return Err(KeyManagerError::InvalidContact);
    }
    Ok(rendered)
}

fn make_initiator_secret(scope_id: &str, request: &PrepareExchange<'_>) -> Result<SecretExchange> {
    validate_offer_id(request.offer_id)?;
    validate_offer_id(request.contact_id)?;
    Ok(SecretExchange {
        schema: EXCHANGE_SCHEMA.to_owned(),
        offer_id: request.offer_id.to_owned(),
        role: ExchangeRole::InitiatorOffer.as_str().to_owned(),
        operation: ExchangeOperation::Initial.as_str().to_owned(),
        contact_id: request.contact_id.to_owned(),
        peer_contact_id: None,
        local_public: request.local_public.to_owned(),
        local_private: SecretString::from(request.local_private.to_owned()),
        peer_public: None,
        peer_fingerprint: None,
        state: ExchangeState::Prepared.as_str().to_owned(),
        scope_id: scope_id.to_owned(),
        contact_name: request.contact_name.to_owned(),
        generation: request.generation,
        created_at: request.created_at,
        expires_at: request.expires_at,
        transcript: None,
        public_artifact: String::new(),
        peer_artifact: None,
    })
}

fn make_initiator_artifact(
    secret: &SecretExchange,
    intended_peer_contact_id: Option<&str>,
    intended_peer_fingerprint: Option<&str>,
    label: Option<&str>,
    previous_fingerprint: Option<&str>,
    attestation: &CompatibilityAttestation,
) -> Result<PreparedExchange> {
    Ok(PreparedExchange {
        schema: secret.schema.clone(),
        version: EXCHANGE_SCHEMA_VERSION.to_owned(),
        artifact_role: ExchangeRole::InitiatorOffer.as_str().to_owned(),
        offer_id: secret.offer_id.clone(),
        sender_contact_id: secret.contact_id.clone(),
        intended_peer_contact_id: intended_peer_contact_id.map(|s| s.to_owned()),
        intended_peer_fingerprint: intended_peer_fingerprint.map(|s| s.to_owned()),
        generator_profile: attestation.generator_profile().to_owned(),
        binary_sha256: attestation.binary_sha256().to_owned(),
        key_format: attestation.key_format().to_owned(),
        source_revision: attestation.source_revision().to_owned(),
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
    scope_id: &str,
    initiator: &ValidatedPublicArtifact,
    request: &RespondExchange<'_>,
) -> Result<SecretExchange> {
    Ok(SecretExchange {
        schema: EXCHANGE_SCHEMA.to_owned(),
        offer_id: request.offer_id.to_owned(),
        role: ExchangeRole::ResponderResponse.as_str().to_owned(),
        operation: ExchangeOperation::Initial.as_str().to_owned(),
        contact_id: request.contact_id.to_owned(),
        peer_contact_id: Some(initiator.sender_contact_id.clone()),
        local_public: request.local_public.to_owned(),
        local_private: SecretString::from(request.local_private.to_owned()),
        peer_public: Some(initiator.public_key.clone()),
        peer_fingerprint: Some(initiator.public_fingerprint.as_str().to_owned()),
        state: ExchangeState::Prepared.as_str().to_owned(),
        scope_id: scope_id.to_owned(),
        contact_name: request.contact_name.to_owned(),
        generation: INITIAL_EXCHANGE_GENERATION,
        created_at: request.created_at,
        expires_at: request.expires_at,
        transcript: None,
        public_artifact: String::new(),
        peer_artifact: Some(
            String::from_utf8(initiator.canonical.clone())
                .map_err(|_| KeyManagerError::InvalidExchangeArtifact)?,
        ),
    })
}

fn make_responder_artifact(
    secret: &SecretExchange,
    initiator_offer_id: &str,
    intended_peer_contact_id: &str,
    intended_peer_fingerprint: &str,
    label: Option<&str>,
    previous_fingerprint: Option<&str>,
    attestation: &CompatibilityAttestation,
) -> Result<ResponderExchange> {
    Ok(ResponderExchange {
        schema: secret.schema.clone(),
        version: EXCHANGE_SCHEMA_VERSION.to_owned(),
        artifact_role: ExchangeRole::ResponderResponse.as_str().to_owned(),
        offer_id: secret.offer_id.clone(),
        in_reply_to: initiator_offer_id.to_owned(),
        sender_contact_id: secret.contact_id.clone(),
        intended_peer_contact_id: intended_peer_contact_id.to_owned(),
        intended_peer_fingerprint: intended_peer_fingerprint.to_owned(),
        generator_profile: attestation.generator_profile().to_owned(),
        binary_sha256: attestation.binary_sha256().to_owned(),
        key_format: attestation.key_format().to_owned(),
        source_revision: attestation.source_revision().to_owned(),
        public_key: secret.local_public.clone(),
        public_fingerprint: public_fingerprint(&secret.local_public)?,
        generation: secret.generation,
        created_at: secret.created_at,
        expires_at: secret.expires_at,
        label: label.map(|s| s.to_owned()),
        previous_fingerprint: previous_fingerprint.map(|s| s.to_owned()),
    })
}

fn prepared_result_from_secret(
    secret: &SecretExchange,
    role: ExchangeRole,
    attestation: &CompatibilityAttestation,
) -> Result<PreparedResult> {
    let parsed = validate_public_exchange_at(
        secret.public_artifact.as_bytes(),
        role,
        attestation,
        secret.created_at,
    )?;
    Ok(PreparedResult {
        offer_id: secret.offer_id.clone(),
        contact_id: secret.contact_id.clone(),
        role: role.as_str().to_owned(),
        public_artifact: secret.public_artifact.clone(),
        public_fingerprint: parsed.public_fingerprint.as_str().to_owned(),
        expires_at: secret.expires_at,
    })
}

fn replay_prepared_exchange(
    inventory: &PendingInventory,
    request: &PrepareExchange<'_>,
) -> Result<Option<PreparedResult>> {
    let mut replay = None;
    for entry in &inventory.entries {
        if entry.secret.contact_name.to_lowercase() != request.contact_name.to_lowercase() {
            continue;
        }
        if entry.secret.contact_name != request.contact_name
            || entry.secret.role != ExchangeRole::InitiatorOffer.as_str()
        {
            return Err(KeyManagerError::ContactConflict);
        }
        let artifact: PreparedExchange = serde_json::from_str(&entry.secret.public_artifact)
            .map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
        if artifact.intended_peer_contact_id.as_deref() != request.intended_peer_contact_id
            || artifact.intended_peer_fingerprint.as_deref() != request.intended_peer_fingerprint
            || artifact.label.as_deref() != request.label
            || artifact.previous_fingerprint.as_deref() != request.previous_fingerprint
        {
            return Err(KeyManagerError::ContactConflict);
        }
        if replay.is_some() {
            return Err(KeyManagerError::RecoveryRequired);
        }
        replay = Some(prepared_result_from_secret(
            &entry.secret,
            ExchangeRole::InitiatorOffer,
            request.attestation,
        )?);
    }
    Ok(replay)
}

fn replay_responded_exchange(
    inventory: &PendingInventory,
    request: &RespondExchange<'_>,
    initiator: &ValidatedPublicArtifact,
) -> Result<Option<PreparedResult>> {
    let mut replay = None;
    for entry in &inventory.entries {
        let peer_offer_id = pending_peer_offer_id(&entry.secret)?;
        if peer_offer_id.as_deref() == Some(initiator.offer_id.as_str()) {
            if entry.secret.role != ExchangeRole::ResponderResponse.as_str()
                || entry.secret.contact_name != request.contact_name
                || entry.secret.peer_artifact.as_deref()
                    != std::str::from_utf8(&initiator.canonical).ok()
            {
                return Err(KeyManagerError::OfferConflict);
            }
            if replay.is_some() {
                return Err(KeyManagerError::RecoveryRequired);
            }
            replay = Some(prepared_result_from_secret(
                &entry.secret,
                ExchangeRole::ResponderResponse,
                request.attestation,
            )?);
            continue;
        }
        if entry.secret.offer_id == initiator.offer_id {
            return Err(KeyManagerError::OfferConflict);
        }
        if entry.secret.contact_name.to_lowercase() == request.contact_name.to_lowercase() {
            return Err(KeyManagerError::ContactConflict);
        }
    }
    Ok(replay)
}

fn build_install_ledger_contact(
    fingerprint: &str,
    contact_id: &str,
    generation: u64,
    local_fingerprint: &str,
) -> LedgerContact {
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

struct ExchangeJournalRequest<'a> {
    scope_id: &'a str,
    operation: &'a str,
    contact_name: &'a str,
    contact_id: &'a str,
    offer_id: &'a str,
    peer_contact_id: Option<&'a str>,
    transcript: Option<&'a str>,
    generation: u64,
    peer_generation: u64,
    peer_artifact: Option<&'a str>,
    old_config: Option<&'a [u8]>,
    old_ledger: Option<&'a [u8]>,
    new_config: &'a [u8],
    new_ledger: &'a [u8],
    operation_kind: &'a str,
    rollback_contact_toml_hash: Option<&'a str>,
    confirmed_peer_fingerprint: Option<&'a str>,
    rollback_tx_id: Option<&'a str>,
    crash_point: Option<CrashPoint>,
}

fn exchange_journal(
    workspace: &Workspace,
    request: ExchangeJournalRequest<'_>,
) -> Result<UpdateResult> {
    let config_hash = sha256(request.new_config);
    let ledger_hash = sha256(request.new_ledger);
    if request.old_config == Some(request.new_config)
        && request.old_ledger == Some(request.new_ledger)
        && request.operation_kind == "exchange-roundtrip"
    {
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
    workspace.write_new(&config_candidate, request.new_config)?;
    workspace.write_new(&ledger_candidate, request.new_ledger)?;
    validate_transaction_candidates(
        workspace,
        request.scope_id,
        &config_candidate,
        &ledger_candidate,
        &config_hash,
        &ledger_hash,
    )?;
    let mut journal = JournalFile {
        schema: "lunarwing.darkirc-transaction/v1".to_owned(),
        transaction_id: transaction_id.clone(),
        operation: request.operation.to_owned(),
        scope_id: request.scope_id.to_owned(),
        old_config_hash: request.old_config.map(sha256),
        old_ledger_hash: request.old_ledger.map(sha256),
        new_config_hash: config_hash.clone(),
        new_ledger_hash: ledger_hash.clone(),
        config_candidate: config_candidate.clone(),
        ledger_candidate: ledger_candidate.clone(),
        phase: JournalPhase::Prepared.as_str().to_owned(),
        contact_id: request.contact_id.to_owned(),
        offer_id: request.offer_id.to_owned(),
        peer_contact_id: request.peer_contact_id.unwrap_or_default().to_owned(),
        transcript: request.transcript.unwrap_or_default().to_owned(),
        generation: request.generation,
        operation_kind: request.operation_kind.to_owned(),
        rollback_contact_toml_hash: request
            .rollback_contact_toml_hash
            .unwrap_or_default()
            .to_owned(),
        confirmed_peer_fingerprint: request
            .confirmed_peer_fingerprint
            .unwrap_or_default()
            .to_owned(),
        rollback_tx_id: request.rollback_tx_id.unwrap_or_default().to_owned(),
        contact_name: request.contact_name.to_owned(),
        peer_generation: request.peer_generation,
        peer_artifact: request.peer_artifact.unwrap_or_default().to_owned(),
    };
    write_journal(workspace, &journal_path, &journal)?;
    inject_crash(request.crash_point, CrashPoint::AfterJournal)?;
    workspace.rename(&ledger_candidate, LEDGER_FILE)?;
    journal.phase = JournalPhase::LedgerCommitted.as_str().to_owned();
    replace_journal(workspace, &journal_path, &journal)?;
    inject_crash(request.crash_point, CrashPoint::AfterLedgerCommit)?;
    inject_crash(request.crash_point, CrashPoint::AfterLedger)?;
    workspace.rename(&config_candidate, CONFIG_FILE)?;
    journal.phase = JournalPhase::ConfigCommitted.as_str().to_owned();
    replace_journal(workspace, &journal_path, &journal)?;
    inject_crash(request.crash_point, CrashPoint::AfterConfig)?;
    finish_committed_journal(workspace, &journal_path, &journal)?;
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
    pub now: i64,
    pub attestation: &'a CompatibilityAttestation,
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
    validate_exchange_times(request.created_at, request.expires_at, request.now)?;
    require_live_exchange(request.expires_at, request.now)?;
    if request.generation != INITIAL_EXCHANGE_GENERATION {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    if let Some(peer_contact_id) = request.intended_peer_contact_id {
        validate_offer_id(peer_contact_id)?;
    }
    let local_fingerprint = public_fingerprint(request.local_public)?;
    let _ = private_key_bytes(request.local_private)?;
    if let Some(peer_f) = request.intended_peer_fingerprint {
        PublicFingerprint::parse(peer_f)?;
    }
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    recover_locked(&workspace, scope_id)?;
    expire_pending_locked(&workspace, scope_id, request.now)?;
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    let ledger = match old_ledger.as_ref().map(|b| b.as_slice()) {
        Some(bytes) => parse_ledger(bytes, scope_id)?,
        None => empty_ledger(scope_id),
    };
    let inventory = pending_inventory(&workspace, scope_id)?;
    if let Some(existing) = replay_prepared_exchange(&inventory, request)? {
        return Ok(existing);
    }
    if ledger.consumed_exchange_ids.contains_key(request.offer_id) {
        return Err(KeyManagerError::OfferConflict);
    }
    if contact_occupied(&workspace, request.contact_name, &ledger)? {
        return Err(KeyManagerError::ContactConflict);
    }
    reject_pending_reservation(
        &inventory,
        request.contact_name,
        request.contact_id,
        request.offer_id,
    )?;
    reject_local_private_reuse(
        &workspace,
        &inventory,
        &ledger,
        request.local_public,
        request.local_private,
    )?;
    let secret = make_initiator_secret(scope_id, request)?;
    let artifact = make_initiator_artifact(
        &secret,
        request.intended_peer_contact_id,
        request.intended_peer_fingerprint,
        request.label,
        request.previous_fingerprint,
        request.attestation,
    )?;
    let public_artifact = String::from_utf8(canonical_artifact_bytes(&artifact)?)
        .map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
    let mut secret = secret;
    secret.public_artifact.clone_from(&public_artifact);
    write_new_pending_secret(&workspace, &secret, &inventory)?;
    Ok(PreparedResult {
        offer_id: request.offer_id.to_owned(),
        contact_id: request.contact_id.to_owned(),
        role: ExchangeRole::InitiatorOffer.as_str().to_owned(),
        public_artifact,
        public_fingerprint: local_fingerprint,
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
    pub now: i64,
    pub attestation: &'a CompatibilityAttestation,
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
    validate_offer_id(request.initiator_offer_id)?;
    validate_exchange_times(request.created_at, request.expires_at, request.now)?;
    require_live_exchange(request.expires_at, request.now)?;
    let local_fingerprint = public_fingerprint(request.local_public)?;
    let _ = private_key_bytes(request.local_private)?;
    let initiator = validate_public_exchange_at(
        offer_bytes,
        ExchangeRole::InitiatorOffer,
        request.attestation,
        request.now,
    )?;
    require_live_exchange(initiator.expires_at, request.now)?;
    // Each artifact has an independently bounded lifetime. A delayed but still-live
    // initiator offer must not become unusable merely because the responder's fresh
    // expiry extends beyond it; completion rechecks both artifacts at that time.
    if initiator.offer_id != request.initiator_offer_id
        || initiator.generation != INITIAL_EXCHANGE_GENERATION
    {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    if initiator.public_fingerprint != *request.expected_initiator_fingerprint {
        return Err(KeyManagerError::FingerprintMismatch);
    }
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    recover_locked(&workspace, scope_id)?;
    expire_pending_locked(&workspace, scope_id, request.now)?;
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    let ledger = match old_ledger.as_ref().map(|b| b.as_slice()) {
        Some(bytes) => parse_ledger(bytes, scope_id)?,
        None => empty_ledger(scope_id),
    };
    let inventory = pending_inventory(&workspace, scope_id)?;
    if let Some(existing) = replay_responded_exchange(&inventory, request, &initiator)? {
        return Ok(existing);
    }
    if initiator
        .intended_peer_contact_id
        .as_deref()
        .is_some_and(|id| id != request.contact_id)
        || initiator
            .intended_peer_fingerprint
            .as_ref()
            .is_some_and(|fingerprint| fingerprint.as_str() != local_fingerprint)
    {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    if request.offer_id == initiator.offer_id
        || ledger.consumed_exchange_ids.contains_key(request.offer_id)
        || ledger
            .consumed_exchange_ids
            .contains_key(&initiator.offer_id)
    {
        return Err(KeyManagerError::OfferConflict);
    }
    if contact_occupied(&workspace, request.contact_name, &ledger)? {
        return Err(KeyManagerError::ContactConflict);
    }
    reject_pending_reservation(
        &inventory,
        request.contact_name,
        request.contact_id,
        request.offer_id,
    )?;
    reject_local_private_reuse(
        &workspace,
        &inventory,
        &ledger,
        request.local_public,
        request.local_private,
    )?;
    let secret = make_responder_secret(scope_id, &initiator, request)?;
    let artifact = make_responder_artifact(
        &secret,
        &initiator.offer_id,
        &initiator.sender_contact_id,
        initiator.public_fingerprint.as_str(),
        request.label,
        None,
        request.attestation,
    )?;
    let public_artifact = String::from_utf8(canonical_artifact_bytes(&artifact)?)
        .map_err(|_| KeyManagerError::InvalidExchangeArtifact)?;
    let mut secret = secret;
    secret.public_artifact.clone_from(&public_artifact);
    write_new_pending_secret(&workspace, &secret, &inventory)?;
    Ok(PreparedResult {
        offer_id: request.offer_id.to_owned(),
        contact_id: request.contact_id.to_owned(),
        role: ExchangeRole::ResponderResponse.as_str().to_owned(),
        public_artifact,
        public_fingerprint: local_fingerprint,
        expires_at: request.expires_at,
    })
}

pub struct CompleteExchange<'a> {
    pub exchange_id: &'a str,
    pub contact_name: &'a str,
    pub expected_peer_fingerprint: &'a PublicFingerprint,
    pub defer_apply: bool,
    pub now: i64,
    pub attestation: &'a CompatibilityAttestation,
}

fn installed_status(secret: &SecretExchange) -> Result<ExchangeStatus> {
    Ok(ExchangeStatus {
        contact_name: secret.contact_name.clone(),
        contact_id: secret.contact_id.clone(),
        offer_id: secret.offer_id.clone(),
        state: ExchangeState::Installed.as_str().to_owned(),
        peer_fingerprint: secret
            .peer_fingerprint
            .clone()
            .ok_or(KeyManagerError::InvalidExchangeArtifact)?,
        created_at: secret.created_at,
        expires_at: secret.expires_at,
        confirmed_at: Some(secret.created_at),
        installed_at: Some(secret.created_at),
        activation_deadline: None,
    })
}

pub fn complete_exchange(
    paths: &TenantPaths,
    scope_id: &str,
    peer_artifact_bytes: &[u8],
    request: &CompleteExchange<'_>,
) -> Result<ExchangeStatus> {
    complete_exchange_with_crash(paths, scope_id, peer_artifact_bytes, request, None)
}

fn complete_exchange_with_crash(
    paths: &TenantPaths,
    scope_id: &str,
    peer_artifact_bytes: &[u8],
    request: &CompleteExchange<'_>,
    crash_point: Option<CrashPoint>,
) -> Result<ExchangeStatus> {
    validate_scope_id(scope_id)?;
    validate_offer_id(request.exchange_id)?;
    validate_contact_name(request.contact_name)?;
    if !request.defer_apply {
        return Err(KeyManagerError::InvalidExchangeState);
    }
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    recover_locked(&workspace, scope_id)?;
    expire_pending_locked(&workspace, scope_id, request.now)?;
    let secret = read_pending_secret(&workspace, request.exchange_id, scope_id)?;
    if secret.scope_id != scope_id {
        return Err(KeyManagerError::ScopeConflict);
    }
    if secret.contact_name != request.contact_name {
        return Err(KeyManagerError::ContactConflict);
    }
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    let mut ledger = match old_ledger.as_ref().map(|bytes| bytes.as_slice()) {
        Some(bytes) => parse_ledger(bytes, scope_id)?,
        None => empty_ledger(scope_id),
    };
    let pending_marked_installed = secret.state == ExchangeState::Installed.as_str();
    let was_installed =
        pending_marked_installed || installed_reservation_matches(&secret, &ledger)?;
    let validation_now = if was_installed {
        secret.created_at
    } else {
        request.now
    };
    validate_exchange_times(secret.created_at, secret.expires_at, validation_now)?;
    if !was_installed {
        require_live_exchange(secret.expires_at, request.now)?;
    }
    let expected_role = match secret.role.as_str() {
        "initiator_offer" => ExchangeRole::ResponderResponse,
        "responder_response" => ExchangeRole::InitiatorOffer,
        _ => return Err(KeyManagerError::InvalidExchangeArtifact),
    };
    let peer_validation_now = if was_installed {
        public_artifact_created_at(peer_artifact_bytes, expected_role)?
    } else {
        request.now
    };
    let peer = validate_public_exchange_at(
        peer_artifact_bytes,
        expected_role,
        request.attestation,
        peer_validation_now,
    )?;
    if !was_installed {
        require_live_exchange(peer.expires_at, request.now)?;
    }
    if peer.generation != INITIAL_EXCHANGE_GENERATION {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    if peer.public_fingerprint != *request.expected_peer_fingerprint {
        return Err(KeyManagerError::FingerprintMismatch);
    }
    if pending_marked_installed {
        if secret.peer_artifact.as_deref() != std::str::from_utf8(&peer.canonical).ok() {
            return Err(KeyManagerError::OfferConflict);
        }
    } else if !matches!(secret.state.as_str(), "Prepared" | "Confirmed") {
        return Err(KeyManagerError::InvalidExchangeState);
    }
    let local_fingerprint = PublicFingerprint::from_encoded_public_key(&secret.local_public)?;
    let local_artifact = match secret.role.as_str() {
        "initiator_offer" => {
            if peer.in_reply_to.as_deref() != Some(secret.offer_id.as_str())
                || peer.intended_peer_contact_id.as_deref() != Some(secret.contact_id.as_str())
                || peer.intended_peer_fingerprint.as_ref() != Some(&local_fingerprint)
            {
                return Err(KeyManagerError::InvalidExchangeArtifact);
            }
            let local = validate_public_exchange_at(
                secret.public_artifact.as_bytes(),
                ExchangeRole::InitiatorOffer,
                request.attestation,
                validation_now,
            )?;
            if local
                .intended_peer_contact_id
                .as_deref()
                .is_some_and(|id| id != peer.sender_contact_id)
                || local
                    .intended_peer_fingerprint
                    .as_ref()
                    .is_some_and(|fingerprint| fingerprint != &peer.public_fingerprint)
            {
                return Err(KeyManagerError::InvalidExchangeArtifact);
            }
            local
        }
        "responder_response" => {
            let stored_peer = secret
                .peer_artifact
                .as_deref()
                .ok_or(KeyManagerError::InvalidExchangeArtifact)?;
            if stored_peer.as_bytes() != peer.canonical
                || secret.peer_contact_id.as_deref() != Some(peer.sender_contact_id.as_str())
                || peer.offer_id
                    != validate_public_exchange_at(
                        stored_peer.as_bytes(),
                        ExchangeRole::InitiatorOffer,
                        request.attestation,
                        validation_now,
                    )?
                    .offer_id
            {
                return Err(KeyManagerError::InvalidExchangeArtifact);
            }
            let local = validate_public_exchange_at(
                secret.public_artifact.as_bytes(),
                ExchangeRole::ResponderResponse,
                request.attestation,
                validation_now,
            )?;
            if local.in_reply_to.as_deref() != Some(peer.offer_id.as_str())
                || local.intended_peer_contact_id.as_deref()
                    != Some(peer.sender_contact_id.as_str())
                || local.intended_peer_fingerprint.as_ref() != Some(&peer.public_fingerprint)
            {
                return Err(KeyManagerError::InvalidExchangeArtifact);
            }
            local
        }
        _ => return Err(KeyManagerError::InvalidExchangeArtifact),
    };
    let (initiator, responder) = if secret.role == ExchangeRole::InitiatorOffer.as_str() {
        (&local_artifact.canonical, &peer.canonical)
    } else {
        (&peer.canonical, &local_artifact.canonical)
    };
    let transcript = compute_transcript(initiator, responder)?;
    if peer.offer_id == secret.offer_id {
        return Err(KeyManagerError::OfferConflict);
    }
    let mut confirmed_secret = secret.clone();
    confirmed_secret.peer_public = Some(peer.public_key.clone());
    confirmed_secret.peer_fingerprint = Some(peer.public_fingerprint.as_str().to_owned());
    confirmed_secret.peer_contact_id = Some(peer.sender_contact_id.clone());
    confirmed_secret.peer_artifact = Some(
        String::from_utf8(peer.canonical.clone())
            .map_err(|_| KeyManagerError::InvalidExchangeArtifact)?,
    );
    confirmed_secret.transcript = Some(transcript.clone());
    confirmed_secret.state = ExchangeState::Confirmed.as_str().to_owned();
    if secret.state == ExchangeState::Confirmed.as_str()
        && (secret.peer_public != confirmed_secret.peer_public
            || secret.peer_fingerprint != confirmed_secret.peer_fingerprint
            || secret.peer_contact_id != confirmed_secret.peer_contact_id
            || secret.peer_artifact != confirmed_secret.peer_artifact
            || secret.transcript != confirmed_secret.transcript)
    {
        return Err(KeyManagerError::OfferConflict);
    }
    let old_config = workspace.read_optional(CONFIG_FILE)?;
    let installed_tombstone = exchange_tombstone(
        &confirmed_secret,
        ExchangeState::Installed,
        peer.public_fingerprint.as_str(),
    )?;
    if ledger.contacts.contains_key(&secret.contact_name) {
        let ids_consumed = [&secret.offer_id, &peer.offer_id]
            .into_iter()
            .all(|id| ledger.consumed_exchange_ids.get(id) == Some(&installed_tombstone));
        if !ids_consumed || !installed_contact_matches(&workspace, &secret, &peer, &ledger)? {
            return Err(KeyManagerError::ContactConflict);
        }
        let mut post_secret = confirmed_secret.clone();
        post_secret.peer_public = Some(peer.public_key.clone());
        post_secret.peer_fingerprint = Some(peer.public_fingerprint.as_str().to_owned());
        post_secret.peer_contact_id = Some(peer.sender_contact_id.clone());
        post_secret.peer_artifact = Some(
            String::from_utf8(peer.canonical.clone())
                .map_err(|_| KeyManagerError::InvalidExchangeArtifact)?,
        );
        post_secret.transcript = Some(transcript);
        post_secret.state = ExchangeState::Installed.as_str().to_owned();
        if !pending_marked_installed {
            replace_pending_secret(&workspace, &post_secret)?;
        }
        return installed_status(&post_secret);
    }
    if was_installed {
        return Err(KeyManagerError::InvalidLedger);
    }
    let candidate = match old_config.as_ref() {
        Some(config) => {
            let rendered = contact_toml_for_new_contact(
                &secret.contact_name,
                &peer.public_key,
                secret.local_private.expose_secret(),
            )?;
            let source = parse_document(&rendered)?;
            let mut target = parse_document(config)?;
            let target_contacts = target
                .table
                .entry("contact".to_owned())
                .or_insert_with(|| Value::Table(Table::new()));
            let target_contacts = target_contacts
                .as_table_mut()
                .ok_or(KeyManagerError::InvalidToml)?;
            if target_contacts.contains_key(&secret.contact_name) {
                return Err(KeyManagerError::ContactConflict);
            }
            let source_contacts = source
                .table
                .get("contact")
                .and_then(Value::as_table)
                .ok_or(KeyManagerError::InvalidToml)?;
            if source_contacts.len() != 1 {
                return Err(KeyManagerError::InvalidToml);
            }
            let value = source_contacts
                .get(&secret.contact_name)
                .ok_or(KeyManagerError::InvalidContact)?
                .clone();
            target_contacts.insert(secret.contact_name.clone(), value);
            validate_document(&target.table)?;
            serialize_table(&target.table)?
        }
        None => contact_toml_for_new_contact(
            &secret.contact_name,
            &peer.public_key,
            secret.local_private.expose_secret(),
        )?,
    };
    let contact = build_install_ledger_contact(
        peer.public_fingerprint.as_str(),
        &secret.contact_id,
        secret.generation,
        local_fingerprint.as_str(),
    );
    append_ledger_contact(&mut ledger, &secret.contact_name, contact)?;
    insert_exchange_tombstone(&mut ledger, &secret.offer_id, &installed_tombstone)?;
    insert_exchange_tombstone(&mut ledger, &peer.offer_id, &installed_tombstone)?;
    let new_ledger = ledger_bytes(&ledger)?;
    exchange_journal(
        &workspace,
        ExchangeJournalRequest {
            scope_id,
            operation: "exchange-complete",
            contact_name: &secret.contact_name,
            contact_id: &secret.contact_id,
            offer_id: &secret.offer_id,
            peer_contact_id: Some(&peer.sender_contact_id),
            transcript: Some(&transcript),
            generation: secret.generation,
            peer_generation: peer.generation,
            peer_artifact: confirmed_secret.peer_artifact.as_deref(),
            old_config: old_config.as_ref().map(|bytes| bytes.as_slice()),
            old_ledger: old_ledger.as_ref().map(|bytes| bytes.as_slice()),
            new_config: &candidate,
            new_ledger: &new_ledger,
            operation_kind: "exchange-complete",
            rollback_contact_toml_hash: None,
            confirmed_peer_fingerprint: Some(peer.public_fingerprint.as_str()),
            rollback_tx_id: None,
            crash_point,
        },
    )?;
    let mut post_secret = confirmed_secret;
    post_secret.peer_public = Some(peer.public_key);
    post_secret.peer_fingerprint = Some(peer.public_fingerprint.as_str().to_owned());
    post_secret.peer_contact_id = Some(peer.sender_contact_id);
    post_secret.peer_artifact = Some(
        String::from_utf8(peer.canonical).map_err(|_| KeyManagerError::InvalidExchangeArtifact)?,
    );
    post_secret.transcript = Some(transcript);
    post_secret.state = ExchangeState::Installed.as_str().to_owned();
    replace_pending_secret(&workspace, &post_secret)?;
    installed_status(&post_secret)
}

pub fn cancel_exchange(
    paths: &TenantPaths,
    scope_id: &str,
    exchange_id: &str,
) -> Result<ExchangeStatus> {
    cancel_exchange_at(paths, scope_id, exchange_id, unix_now()?)
}

pub fn cancel_exchange_at(
    paths: &TenantPaths,
    scope_id: &str,
    exchange_id: &str,
    now: i64,
) -> Result<ExchangeStatus> {
    validate_scope_id(scope_id)?;
    validate_offer_id(exchange_id)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    recover_locked(&workspace, scope_id)?;
    expire_pending_locked(&workspace, scope_id, now)?;
    let old_ledger = workspace.read_optional(LEDGER_FILE)?;
    let mut ledger = match old_ledger.as_deref() {
        Some(bytes) => parse_ledger(bytes, scope_id)?,
        None => empty_ledger(scope_id),
    };
    if let Some(tombstone) = ledger.consumed_exchange_ids.get(exchange_id).cloned() {
        if tombstone.state != ExchangeState::Cancelled.as_str() {
            return Err(KeyManagerError::InvalidExchangeState);
        }
        remove_pending(&workspace, exchange_id)?;
        return Ok(ExchangeStatus {
            contact_name: tombstone.contact_name,
            contact_id: tombstone.contact_id,
            offer_id: exchange_id.to_owned(),
            state: tombstone.state,
            peer_fingerprint: tombstone.peer_fingerprint,
            created_at: tombstone.created_at,
            expires_at: tombstone.expires_at,
            confirmed_at: None,
            installed_at: None,
            activation_deadline: None,
        });
    }
    let secret = read_pending_secret(&workspace, exchange_id, scope_id)?;
    if !matches!(secret.state.as_str(), "Prepared" | "Confirmed") {
        return Err(KeyManagerError::InvalidExchangeState);
    }
    let contact_name = secret.contact_name.clone();
    let contact_id = secret.contact_id.clone();
    let offer_id = secret.offer_id.clone();
    let created_at = secret.created_at;
    let expires_at = secret.expires_at;
    let peer_fingerprint = secret.peer_fingerprint.clone().unwrap_or_default();
    let tombstone = exchange_tombstone(&secret, ExchangeState::Cancelled, &peer_fingerprint)?;
    insert_exchange_tombstone(&mut ledger, exchange_id, &tombstone)?;
    if let Some(peer) = validate_pending_peer(&secret)? {
        insert_exchange_tombstone(&mut ledger, &peer.offer_id, &tombstone)?;
    }
    replace_file(&workspace, LEDGER_FILE, &ledger_bytes(&ledger)?)?;
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

pub fn inspect_exchanges(paths: &TenantPaths, scope_id: &str) -> Result<Vec<ExchangeStatus>> {
    validate_scope_id(scope_id)?;
    let workspace = Workspace::open_read_only(paths.clone())?;
    let _snapshot = workspace.read_lock()?;
    for relative in [CANDIDATES_DIR, TRANSACTIONS_DIR] {
        if !secure_directory_files(&workspace, relative)?.is_empty() {
            return Err(KeyManagerError::RecoveryRequired);
        }
    }
    let now = unix_now()?;
    let inventory = pending_inventory(&workspace, scope_id)?;
    let ledger = match workspace.read_optional(LEDGER_FILE)? {
        Some(bytes) => parse_ledger(&bytes, scope_id)?,
        None => empty_ledger(scope_id),
    };
    let mut entries = Vec::with_capacity(inventory.entries.len());
    for entry in inventory.entries {
        let secret = entry.secret;
        let state = if secret.state == ExchangeState::Prepared.as_str()
            && installed_reservation_matches(&secret, &ledger)?
        {
            ExchangeState::Installed.as_str().to_owned()
        } else if secret.state == ExchangeState::Prepared.as_str() && now >= secret.expires_at {
            ExchangeState::Expired.as_str().to_owned()
        } else {
            secret.state
        };
        entries.push(ExchangeStatus {
            contact_name: secret.contact_name,
            contact_id: secret.contact_id,
            offer_id: secret.offer_id,
            state,
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

// Stage B/C prototypes remain excluded until their state transitions and
// recovery contracts receive dedicated implementation and review.

pub fn recover(paths: &TenantPaths, scope_id: &str) -> Result<()> {
    validate_scope_id(scope_id)?;
    let workspace = Workspace::open(paths.clone())?;
    let _lock = workspace.lock()?;
    recover_locked(&workspace, scope_id)?;
    expire_pending_locked(&workspace, scope_id, unix_now()?)
}

fn recover_locked(workspace: &Workspace, scope_id: &str) -> Result<()> {
    recovery_scope_preflight(workspace, scope_id)?;
    recover_pending_candidates(workspace, scope_id)?;

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
        return reconcile_installed_pending(workspace, scope_id);
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
    reconcile_installed_pending(workspace, scope_id)
}

struct PendingCandidate {
    candidate_path: String,
    final_path: String,
    entry: PendingEntry,
    bytes_len: usize,
}

// A crash between a fsynced pending candidate and its atomic rename leaves
// `<offer>.json.next`. An existing record remains authoritative. A first-create
// candidate is promoted only after every candidate passes scope, capacity,
// active-config, replay, contact, and private-key-reuse validation as one set.
fn recover_pending_candidates(workspace: &Workspace, scope_id: &str) -> Result<()> {
    let names = secure_directory_files(workspace, PENDING_DIR)?;
    let (final_names, candidate_names) = classify_pending_names(names)?;
    let inventory = pending_inventory_from_names(workspace, scope_id, final_names)?;
    let mut discard = Vec::new();
    let mut promote = Vec::new();
    for name in candidate_names {
        let candidate = read_pending_candidate(workspace, scope_id, &name)?;
        match inventory
            .entries
            .iter()
            .find(|entry| entry.secret.offer_id == candidate.entry.secret.offer_id)
        {
            Some(current) if same_pending_identity(&current.secret, &candidate.entry.secret) => {
                discard.push(candidate.candidate_path);
            }
            Some(_) => return Err(KeyManagerError::RecoveryRequired),
            None => promote.push(candidate),
        }
    }
    validate_pending_promotions(workspace, scope_id, &inventory, &promote)?;
    for path in discard {
        workspace.remove_if_present(&path)?;
    }
    for candidate in promote {
        workspace.rename(&candidate.candidate_path, &candidate.final_path)?;
    }
    Ok(())
}

fn classify_pending_names(names: Vec<String>) -> Result<(Vec<String>, Vec<String>)> {
    let mut finals = Vec::new();
    let mut candidates = Vec::new();
    for name in names {
        if let Some(base) = name.strip_suffix(".json.next") {
            validate_offer_id(base).map_err(|_| KeyManagerError::RecoveryRequired)?;
            candidates.push(name);
        } else if let Some(base) = name.strip_suffix(".json") {
            validate_offer_id(base).map_err(|_| KeyManagerError::RecoveryRequired)?;
            finals.push(name);
        } else {
            return Err(KeyManagerError::RecoveryRequired);
        }
    }
    Ok((finals, candidates))
}

fn read_pending_candidate(
    workspace: &Workspace,
    scope_id: &str,
    name: &str,
) -> Result<PendingCandidate> {
    let offer_id = name
        .strip_suffix(".json.next")
        .ok_or(KeyManagerError::RecoveryRequired)?;
    let candidate_path = format!("{PENDING_DIR}/{name}");
    let bytes = workspace.read_required(&candidate_path)?;
    if bytes.len() > MAX_PENDING_BYTES {
        return Err(KeyManagerError::RecoveryRequired);
    }
    let secret: SecretExchange =
        serde_json::from_slice(&bytes).map_err(|_| KeyManagerError::RecoveryRequired)?;
    validate_pending_secret(&secret, offer_id, scope_id)
        .map_err(|_| KeyManagerError::RecoveryRequired)?;
    Ok(PendingCandidate {
        candidate_path,
        final_path: format!("{PENDING_DIR}/{offer_id}.json"),
        entry: PendingEntry { secret },
        bytes_len: bytes.len(),
    })
}

fn same_pending_identity(current: &SecretExchange, candidate: &SecretExchange) -> bool {
    current.offer_id == candidate.offer_id
        && current.contact_id == candidate.contact_id
        && current.contact_name == candidate.contact_name
        && current.local_public == candidate.local_public
        && current.local_private.expose_secret() == candidate.local_private.expose_secret()
}

fn validate_pending_promotions(
    workspace: &Workspace,
    scope_id: &str,
    inventory: &PendingInventory,
    candidates: &[PendingCandidate],
) -> Result<()> {
    if candidates.is_empty() {
        return Ok(());
    }
    validate_active_config(workspace)?;
    let ledger_bytes = workspace.read_optional(LEDGER_FILE)?;
    let ledger = match ledger_bytes.as_deref() {
        Some(bytes) => parse_ledger(bytes, scope_id)?,
        None => empty_ledger(scope_id),
    };
    let added_bytes = candidates.iter().try_fold(0_usize, |total, candidate| {
        total
            .checked_add(candidate.bytes_len)
            .ok_or(KeyManagerError::PendingLimit)
    })?;
    if inventory.entries.len().saturating_add(candidates.len()) > MAX_PENDING_EXCHANGES
        || inventory.total_bytes.saturating_add(added_bytes) > MAX_PENDING_TOTAL_BYTES
    {
        return Err(KeyManagerError::PendingLimit);
    }
    let mut combined = inventory
        .entries
        .iter()
        .map(|entry| PendingEntry {
            secret: entry.secret.clone(),
        })
        .collect::<Vec<_>>();
    combined.extend(candidates.iter().map(|candidate| PendingEntry {
        secret: candidate.entry.secret.clone(),
    }));
    validate_pending_entries(&combined)?;
    for candidate in candidates {
        validate_pending_promotion(workspace, inventory, &ledger, &candidate.entry.secret)?;
    }
    Ok(())
}

fn validate_pending_promotion(
    workspace: &Workspace,
    inventory: &PendingInventory,
    ledger: &Ledger,
    secret: &SecretExchange,
) -> Result<()> {
    if secret.state != ExchangeState::Prepared.as_str()
        || ledger.consumed_exchange_ids.contains_key(&secret.offer_id)
        || pending_peer_offer_id(secret)?
            .is_some_and(|offer_id| ledger.consumed_exchange_ids.contains_key(&offer_id))
        || contact_occupied(workspace, &secret.contact_name, ledger)?
    {
        return Err(KeyManagerError::RecoveryRequired);
    }
    reject_pending_reservation(
        inventory,
        &secret.contact_name,
        &secret.contact_id,
        &secret.offer_id,
    )?;
    reject_local_private_reuse(
        workspace,
        inventory,
        ledger,
        &secret.local_public,
        secret.local_private.expose_secret(),
    )
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
            finish_committed_journal(workspace, path, &journal)
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
    finish_committed_journal(workspace, path, journal)
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
        "baseline-update" | "legacy-adopt" | "migration-import" | "exchange-complete"
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
    if !matches!(
        journal.phase()?,
        JournalPhase::Prepared | JournalPhase::LedgerCommitted | JournalPhase::ConfigCommitted
    ) || (journal.operation == "exchange-complete"
        && journal.operation_kind != "exchange-complete")
        || (journal.operation != "exchange-complete" && !journal.operation_kind.is_empty())
    {
        return Err(KeyManagerError::RecoveryRequired);
    }
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
    finish_committed_journal(workspace, path, journal)
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

    #[test]
    fn config_capability_rejects_wrong_owner_mode_and_hard_links() {
        use std::os::unix::fs::MetadataExt;

        let (_owner_temp, mut owner_paths) = fixture();
        write_config(&owner_paths, "irc_listen = \"tcp://127.0.0.1:21001\"\n");
        let uid = fs::metadata(owner_paths.darkirc_dir())
            .expect("state metadata")
            .uid();
        owner_paths.expected_uid = Some(uid.saturating_add(1));
        assert!(matches!(
            inspect_contacts(&owner_paths, None),
            Err(KeyManagerError::UnsafePath)
        ));

        let (_mode_temp, mode_paths) = fixture();
        write_config(&mode_paths, "irc_listen = \"tcp://127.0.0.1:21001\"\n");
        fs::set_permissions(
            mode_paths.darkirc_dir().join(CONFIG_FILE),
            fs::Permissions::from_mode(0o640),
        )
        .expect("weaken config mode");
        assert!(matches!(
            inspect_contacts(&mode_paths, None),
            Err(KeyManagerError::UnsafePath)
        ));

        let (_link_temp, link_paths) = fixture();
        write_config(&link_paths, "irc_listen = \"tcp://127.0.0.1:21001\"\n");
        fs::hard_link(
            link_paths.darkirc_dir().join(CONFIG_FILE),
            link_paths.darkirc_dir().join("config-copy"),
        )
        .expect("create hard link");
        assert!(matches!(
            inspect_contacts(&link_paths, None),
            Err(KeyManagerError::UnsafePath)
        ));
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
    fn concurrent_baseline_writers_serialize_without_losing_contacts() {
        let (_temp, paths) = fixture();
        let public = valid_public(45);
        let private = valid_private(46);
        write_config(
            &paths,
            &format!(
                "[contact.\"alice\"]\ndm_chacha_public = \"{public}\"\nmy_dm_chacha_secret = \"{private}\"\n"
            ),
        );
        let first_paths = paths.clone();
        let second_paths = paths.clone();
        let first = std::thread::spawn(move || {
            update_baseline(&first_paths, SCOPE, baseline(22001).as_bytes())
        });
        let second = std::thread::spawn(move || {
            update_baseline(&second_paths, SCOPE, baseline(23001).as_bytes())
        });
        first
            .join()
            .expect("first writer thread")
            .expect("first writer");
        second
            .join()
            .expect("second writer thread")
            .expect("second writer");

        let config = fs::read(paths.darkirc_dir().join(CONFIG_FILE)).expect("settled config");
        assert_contact_field(&config, "alice", "dm_chacha_public", &public);
        assert_contact_field(&config, "alice", "my_dm_chacha_secret", &private);
        assert!(parse_document(&config).is_ok());
        assert!(
            secure_directory_files(
                &Workspace::open(paths).expect("workspace"),
                TRANSACTIONS_DIR
            )
            .expect("transaction inventory")
            .is_empty()
        );
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
    fn recovery_rejects_deferred_future_stage_journal() {
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

        let journal_path = fs::read_dir(paths.darkirc_dir().join(TRANSACTIONS_DIR))
            .expect("transaction directory")
            .next()
            .expect("journal entry")
            .expect("journal path")
            .path();
        let mut journal: JournalFile =
            serde_json::from_slice(&fs::read(&journal_path).expect("read journal"))
                .expect("parse journal");
        journal.operation = "exchange-activation".to_owned();
        journal.operation_kind = "exchange-activation".to_owned();
        journal.phase = JournalPhase::ActivationStarted.as_str().to_owned();
        fs::write(
            &journal_path,
            journal_bytes(&journal).expect("render journal"),
        )
        .expect("write future-stage journal");
        let config_before = fs::read(paths.darkirc_dir().join(CONFIG_FILE)).expect("config before");
        let ledger_before = fs::read(paths.darkirc_dir().join(LEDGER_FILE)).expect("ledger before");

        let error = recover(&paths, SCOPE).expect_err("future-stage journal must fail closed");

        assert!(matches!(error, KeyManagerError::RecoveryRequired));
        assert_eq!(
            fs::read(paths.darkirc_dir().join(CONFIG_FILE)).expect("config after"),
            config_before
        );
        assert_eq!(
            fs::read(paths.darkirc_dir().join(LEDGER_FILE)).expect("ledger after"),
            ledger_before
        );
        assert!(journal_path.exists());
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

    const EXCHANGE_NOW: i64 = 10_000;

    fn exchange_attestation() -> CompatibilityAttestation {
        attestation('a')
    }

    fn exchange_request<'a>(
        attestation: &'a CompatibilityAttestation,
        contact_name: &'a str,
        offer_id: &'a str,
        contact_id: &'a str,
        public: &'a str,
        private: &'a str,
    ) -> PrepareExchange<'a> {
        PrepareExchange {
            contact_name,
            offer_id,
            contact_id,
            local_public: public,
            local_private: private,
            intended_peer_contact_id: None,
            intended_peer_fingerprint: None,
            label: None,
            previous_fingerprint: None,
            generation: 1,
            created_at: EXCHANGE_NOW,
            expires_at: EXCHANGE_NOW + DEFAULT_OFFER_TTL_SECONDS,
            now: EXCHANGE_NOW,
            attestation,
        }
    }

    #[test]
    fn stage_a_artifact_is_typed_canonical_attested_and_secret_free() {
        let (_temp, paths) = fixture();
        let attestation = exchange_attestation();
        let public = valid_public(7);
        let private = valid_private(8);
        let request = exchange_request(
            &attestation,
            "bob",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &public,
            &private,
        );

        let first = prepare_exchange(&paths, SCOPE, &request).expect("prepare");
        let replay = prepare_exchange(&paths, SCOPE, &request).expect("idempotent prepare");
        assert_eq!(first, replay);
        assert!(!first.public_artifact.contains(&private));
        assert_eq!(
            serde_json::to_vec(
                &serde_json::from_str::<PreparedExchange>(&first.public_artifact)
                    .expect("typed artifact"),
            )
            .expect("canonical artifact"),
            first.public_artifact.as_bytes()
        );
        let validated = validate_public_exchange_at(
            first.public_artifact.as_bytes(),
            ExchangeRole::InitiatorOffer,
            &attestation,
            EXCHANGE_NOW,
        )
        .expect("self validate");
        assert_eq!(validated.public_key, public);
        assert_eq!(
            validated.public_fingerprint.as_str(),
            first.public_fingerprint
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&first.public_artifact).expect("json")["binary_sha256"],
            attestation.binary_sha256()
        );
    }

    #[test]
    fn stage_a_prepare_cli_style_retry_reuses_pending_exchange() {
        let (_temp, paths) = fixture();
        let attestation = exchange_attestation();
        let first_public = valid_public(7);
        let first_private = valid_private(8);
        let first_request = exchange_request(
            &attestation,
            "bob",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &first_public,
            &first_private,
        );
        let first = prepare_exchange(&paths, SCOPE, &first_request).expect("first prepare");

        let retry_public = valid_public(9);
        let retry_private = valid_private(10);
        let mut retry_request = exchange_request(
            &attestation,
            "bob",
            "cccccccccccccccccccccccccccccccc",
            "dddddddddddddddddddddddddddddddd",
            &retry_public,
            &retry_private,
        );
        retry_request.created_at += 1;
        retry_request.now += 1;
        let retry = prepare_exchange(&paths, SCOPE, &retry_request).expect("semantic replay");

        assert_eq!(retry, first);
        let workspace = Workspace::open(paths).expect("workspace");
        assert_eq!(
            pending_inventory(&workspace, SCOPE)
                .expect("inventory")
                .entries
                .len(),
            1
        );
    }

    #[test]
    fn contact_toml_serialization_quotes_hostile_names() {
        let public = valid_public(11);
        let private = valid_private(12);
        for name in ["victim\"] #", "a.b", "[table]", "single'quote"] {
            let rendered =
                contact_toml_for_new_contact(name, &public, &private).expect("serialize contact");
            let document = parse_document(&rendered).expect("parse rendered contact");
            let contacts = document.table["contact"].as_table().expect("contact table");
            assert_eq!(contacts.len(), 1);
            assert!(contacts.contains_key(name));
        }
    }

    #[test]
    fn malformed_active_config_blocks_prepare_without_pending_mutation() {
        let (_temp, paths) = fixture();
        write_config(&paths, "contact = [invalid");
        let attestation = exchange_attestation();
        let public = valid_public(13);
        let private = valid_private(14);
        let request = exchange_request(
            &attestation,
            "bob",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &public,
            &private,
        );

        assert!(matches!(
            prepare_exchange(&paths, SCOPE, &request),
            Err(KeyManagerError::InvalidToml)
        ));
        let pending = paths.darkirc_dir().join(PENDING_DIR);
        assert_eq!(fs::read_dir(pending).expect("pending directory").count(), 0);
    }

    #[test]
    fn local_private_key_reuse_is_rejected_across_pending_exchanges() {
        let (_temp, paths) = fixture();
        let attestation = exchange_attestation();
        let first_public = valid_public(15);
        let private = valid_private(16);
        let first = exchange_request(
            &attestation,
            "alice",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &first_public,
            &private,
        );
        prepare_exchange(&paths, SCOPE, &first).expect("first prepare");

        let second_public = valid_public(17);
        let second = exchange_request(
            &attestation,
            "bob",
            "cccccccccccccccccccccccccccccccc",
            "dddddddddddddddddddddddddddddddd",
            &second_public,
            &private,
        );
        assert!(matches!(
            prepare_exchange(&paths, SCOPE, &second),
            Err(KeyManagerError::ContactConflict)
        ));
    }

    #[test]
    fn expired_pending_exchange_is_tombstoned_before_new_prepare() {
        let (_temp, paths) = fixture();
        let attestation = exchange_attestation();
        let first_public = valid_public(18);
        let first_private = valid_private(19);
        let mut first = exchange_request(
            &attestation,
            "alice",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &first_public,
            &first_private,
        );
        first.expires_at = EXCHANGE_NOW + 1;
        prepare_exchange(&paths, SCOPE, &first).expect("first prepare");

        let second_public = valid_public(20);
        let second_private = valid_private(21);
        let mut second = exchange_request(
            &attestation,
            "bob",
            "cccccccccccccccccccccccccccccccc",
            "dddddddddddddddddddddddddddddddd",
            &second_public,
            &second_private,
        );
        second.created_at = EXCHANGE_NOW + 1;
        second.expires_at = second.created_at + DEFAULT_OFFER_TTL_SECONDS;
        second.now = second.created_at;
        prepare_exchange(&paths, SCOPE, &second).expect("prepare after cleanup");

        assert!(
            !paths
                .darkirc_dir()
                .join(PENDING_DIR)
                .join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json")
                .exists()
        );
        let ledger = read_ledger(&paths, SCOPE).expect("ledger");
        assert_eq!(
            ledger.consumed_exchange_ids["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"].state,
            "Expired"
        );
    }

    #[test]
    fn stage_a_artifact_rejects_unknown_fields_expiry_and_attestation_mismatch() {
        let (_temp, paths) = fixture();
        let measured_attestation = exchange_attestation();
        let public = valid_public(7);
        let private = valid_private(8);
        let request = exchange_request(
            &measured_attestation,
            "bob",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &public,
            &private,
        );
        let result = prepare_exchange(&paths, SCOPE, &request).expect("prepare");
        let mut unknown: serde_json::Value =
            serde_json::from_str(&result.public_artifact).expect("json");
        unknown["unexpected"] = serde_json::json!(true);
        assert!(matches!(
            validate_public_exchange_at(
                &serde_json::to_vec(&unknown).expect("json"),
                ExchangeRole::InitiatorOffer,
                &measured_attestation,
                EXCHANGE_NOW,
            ),
            Err(KeyManagerError::InvalidExchangeArtifact)
        ));
        assert!(
            validate_public_exchange_at(
                result.public_artifact.as_bytes(),
                ExchangeRole::InitiatorOffer,
                &measured_attestation,
                EXCHANGE_NOW + DEFAULT_OFFER_TTL_SECONDS + MAX_CLOCK_SKEW_SECONDS + 1,
            )
            .is_err()
        );
        assert!(matches!(
            validate_public_exchange_at(
                result.public_artifact.as_bytes(),
                ExchangeRole::InitiatorOffer,
                &attestation('b'),
                EXCHANGE_NOW,
            ),
            Err(KeyManagerError::InvalidCompatibility)
        ));
    }

    struct PairFixture {
        _initiator_temp: TempDir,
        initiator: TenantPaths,
        _responder_temp: TempDir,
        responder: TenantPaths,
        attestation: CompatibilityAttestation,
        initiator_public: String,
        initiator_private: String,
        responder_public: String,
        responder_private: String,
        offer: PreparedResult,
        response: PreparedResult,
    }

    fn prepared_pair() -> PairFixture {
        let (initiator_temp, initiator) = fixture();
        let (responder_temp, responder) = fixture();
        let attestation = exchange_attestation();
        let initiator_public = valid_public(21);
        let initiator_private = valid_private(22);
        let responder_public = valid_public(23);
        let responder_private = valid_private(24);
        let responder_fingerprint = public_fingerprint(&responder_public).expect("fingerprint");
        let mut prepare = exchange_request(
            &attestation,
            "bob",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &initiator_public,
            &initiator_private,
        );
        prepare.intended_peer_contact_id = Some("dddddddddddddddddddddddddddddddd");
        prepare.intended_peer_fingerprint = Some(&responder_fingerprint);
        let offer = prepare_exchange(&initiator, SCOPE, &prepare).expect("prepare initiator");
        let initiator_fingerprint =
            PublicFingerprint::from_encoded_public_key(&initiator_public).expect("fingerprint");
        let respond = RespondExchange {
            contact_name: "alice",
            offer_id: "cccccccccccccccccccccccccccccccc",
            contact_id: "dddddddddddddddddddddddddddddddd",
            local_public: &responder_public,
            local_private: &responder_private,
            initiator_offer_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            expected_initiator_fingerprint: &initiator_fingerprint,
            label: None,
            created_at: EXCHANGE_NOW + 1,
            expires_at: EXCHANGE_NOW + DEFAULT_OFFER_TTL_SECONDS,
            now: EXCHANGE_NOW + 1,
            attestation: &attestation,
        };
        let response = respond_exchange(
            &responder,
            SCOPE,
            offer.public_artifact.as_bytes(),
            &respond,
        )
        .expect("respond");
        let retry_public = valid_public(25);
        let retry_private = valid_private(26);
        let retry_request = RespondExchange {
            contact_name: "alice",
            offer_id: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            contact_id: "ffffffffffffffffffffffffffffffff",
            local_public: &retry_public,
            local_private: &retry_private,
            initiator_offer_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            expected_initiator_fingerprint: &initiator_fingerprint,
            label: None,
            created_at: EXCHANGE_NOW + 2,
            expires_at: EXCHANGE_NOW + DEFAULT_OFFER_TTL_SECONDS,
            now: EXCHANGE_NOW + 2,
            attestation: &attestation,
        };
        let response_replay = respond_exchange(
            &responder,
            SCOPE,
            offer.public_artifact.as_bytes(),
            &retry_request,
        )
        .expect("semantic respond replay");
        assert_eq!(response, response_replay);
        assert!(!response.public_artifact.contains(&responder_private));
        PairFixture {
            _initiator_temp: initiator_temp,
            initiator,
            _responder_temp: responder_temp,
            responder,
            attestation,
            initiator_public,
            initiator_private,
            responder_public,
            responder_private,
            offer,
            response,
        }
    }

    #[test]
    fn responder_rejects_remote_generation_as_local_authority() {
        let (_initiator_temp, initiator) = fixture();
        let (_responder_temp, responder) = fixture();
        let attestation = exchange_attestation();
        let initiator_public = valid_public(27);
        let initiator_private = valid_private(28);
        let prepare = exchange_request(
            &attestation,
            "bob",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &initiator_public,
            &initiator_private,
        );
        let offer = prepare_exchange(&initiator, SCOPE, &prepare).expect("prepare");
        let mut artifact: PreparedExchange =
            serde_json::from_str(&offer.public_artifact).expect("offer artifact");
        artifact.generation = u64::MAX;
        let hostile_offer = canonical_artifact_bytes(&artifact).expect("hostile artifact");
        let responder_public = valid_public(29);
        let responder_private = valid_private(30);
        let initiator_fingerprint =
            PublicFingerprint::from_encoded_public_key(&initiator_public).expect("fingerprint");
        let request = RespondExchange {
            contact_name: "alice",
            offer_id: "cccccccccccccccccccccccccccccccc",
            contact_id: "dddddddddddddddddddddddddddddddd",
            local_public: &responder_public,
            local_private: &responder_private,
            initiator_offer_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            expected_initiator_fingerprint: &initiator_fingerprint,
            label: None,
            created_at: EXCHANGE_NOW + 1,
            expires_at: EXCHANGE_NOW + DEFAULT_OFFER_TTL_SECONDS,
            now: EXCHANGE_NOW + 1,
            attestation: &attestation,
        };

        assert!(matches!(
            respond_exchange(&responder, SCOPE, &hostile_offer, &request),
            Err(KeyManagerError::InvalidExchangeArtifact)
        ));
        assert_eq!(
            pending_inventory(&Workspace::open(responder).expect("workspace"), SCOPE)
                .expect("inventory")
                .entries
                .len(),
            0
        );
    }

    #[test]
    fn responder_rejects_offer_at_exact_expiry() {
        let (_initiator_temp, initiator) = fixture();
        let (_responder_temp, responder) = fixture();
        let attestation = exchange_attestation();
        let initiator_public = valid_public(31);
        let initiator_private = valid_private(32);
        let prepare = exchange_request(
            &attestation,
            "bob",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &initiator_public,
            &initiator_private,
        );
        let offer = prepare_exchange(&initiator, SCOPE, &prepare).expect("prepare");
        let responder_public = valid_public(33);
        let responder_private = valid_private(34);
        let initiator_fingerprint =
            PublicFingerprint::from_encoded_public_key(&initiator_public).expect("fingerprint");
        let request = RespondExchange {
            contact_name: "alice",
            offer_id: "cccccccccccccccccccccccccccccccc",
            contact_id: "dddddddddddddddddddddddddddddddd",
            local_public: &responder_public,
            local_private: &responder_private,
            initiator_offer_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            expected_initiator_fingerprint: &initiator_fingerprint,
            label: None,
            created_at: EXCHANGE_NOW + DEFAULT_OFFER_TTL_SECONDS - 1,
            expires_at: EXCHANGE_NOW + DEFAULT_OFFER_TTL_SECONDS,
            now: EXCHANGE_NOW + DEFAULT_OFFER_TTL_SECONDS,
            attestation: &attestation,
        };

        assert!(matches!(
            respond_exchange(
                &responder,
                SCOPE,
                offer.public_artifact.as_bytes(),
                &request,
            ),
            Err(KeyManagerError::InvalidExchangeArtifact)
        ));
    }

    #[test]
    fn stage_a_wrong_response_binding_mutates_nothing() {
        let pair = prepared_pair();
        let mut response: ResponderExchange =
            serde_json::from_str(&pair.response.public_artifact).expect("response");
        response.intended_peer_contact_id = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_owned();
        let request = CompleteExchange {
            exchange_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            contact_name: "bob",
            expected_peer_fingerprint: &PublicFingerprint::from_encoded_public_key(
                &pair.responder_public,
            )
            .expect("fingerprint"),
            defer_apply: true,
            now: EXCHANGE_NOW + 1,
            attestation: &pair.attestation,
        };
        assert!(
            complete_exchange(
                &pair.initiator,
                SCOPE,
                &canonical_artifact_bytes(&response).expect("artifact"),
                &request,
            )
            .is_err()
        );
        let wrong_contact = CompleteExchange {
            contact_name: "mallory",
            ..request
        };
        assert!(matches!(
            complete_exchange(
                &pair.initiator,
                SCOPE,
                pair.response.public_artifact.as_bytes(),
                &wrong_contact,
            ),
            Err(KeyManagerError::ContactConflict)
        ));
        assert!(!pair.initiator.darkirc_dir().join(CONFIG_FILE).exists());
        assert!(!pair.initiator.darkirc_dir().join(LEDGER_FILE).exists());
    }

    #[test]
    fn stage_a_both_roles_complete_with_peer_public_and_local_private() {
        let pair = prepared_pair();
        let responder_fingerprint =
            PublicFingerprint::from_encoded_public_key(&pair.responder_public)
                .expect("fingerprint");
        let initiator_complete = CompleteExchange {
            exchange_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            contact_name: "bob",
            expected_peer_fingerprint: &responder_fingerprint,
            defer_apply: true,
            now: EXCHANGE_NOW + 1,
            attestation: &pair.attestation,
        };
        let first = complete_exchange(
            &pair.initiator,
            SCOPE,
            pair.response.public_artifact.as_bytes(),
            &initiator_complete,
        )
        .expect("complete initiator");
        let initiator_workspace = Workspace::open(pair.initiator.clone()).expect("workspace");
        let mut confirmed_pending = read_pending_secret(
            &initiator_workspace,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            SCOPE,
        )
        .expect("pending exchange");
        confirmed_pending.state = ExchangeState::Confirmed.as_str().to_owned();
        replace_pending_secret(&initiator_workspace, &confirmed_pending)
            .expect("simulate crash before final pending-state update");
        recover(&pair.initiator, SCOPE).expect("recover confirmed pending exchange");
        assert_eq!(
            read_pending_secret(
                &initiator_workspace,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                SCOPE,
            )
            .expect("recovered pending exchange")
            .state,
            ExchangeState::Installed.as_str()
        );
        let mut stale_pending = read_pending_secret(
            &initiator_workspace,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            SCOPE,
        )
        .expect("pending exchange");
        stale_pending.state = ExchangeState::Prepared.as_str().to_owned();
        stale_pending.peer_public = None;
        stale_pending.peer_fingerprint = None;
        stale_pending.peer_contact_id = None;
        stale_pending.peer_artifact = None;
        stale_pending.transcript = None;
        replace_pending_secret(&initiator_workspace, &stale_pending)
            .expect("simulate crash before pending-state update");
        let committed_ledger = read_ledger(&pair.initiator, SCOPE).expect("committed ledger");
        assert!(
            installed_reservation_matches(&stale_pending, &committed_ledger)
                .expect("validate installed reservation")
        );
        let recovered_status = inspect_exchanges(&pair.initiator, SCOPE)
            .expect("inspect committed exchange with stale pending state");
        assert_eq!(recovered_status[0].state, ExchangeState::Installed.as_str());
        let retry_after_expiry = CompleteExchange {
            now: EXCHANGE_NOW + DEFAULT_OFFER_TTL_SECONDS,
            ..initiator_complete
        };
        let retry = complete_exchange(
            &pair.initiator,
            SCOPE,
            pair.response.public_artifact.as_bytes(),
            &retry_after_expiry,
        )
        .expect("idempotent complete after pending expiry");
        assert_eq!(first, retry);

        let initiator_fingerprint =
            PublicFingerprint::from_encoded_public_key(&pair.initiator_public)
                .expect("fingerprint");
        let responder_complete = CompleteExchange {
            exchange_id: "cccccccccccccccccccccccccccccccc",
            contact_name: "alice",
            expected_peer_fingerprint: &initiator_fingerprint,
            defer_apply: true,
            now: EXCHANGE_NOW + 1,
            attestation: &pair.attestation,
        };
        complete_exchange(
            &pair.responder,
            SCOPE,
            pair.offer.public_artifact.as_bytes(),
            &responder_complete,
        )
        .expect("complete responder");

        let initiator_ledger = read_ledger(&pair.initiator, SCOPE).expect("initiator ledger");
        let responder_ledger = read_ledger(&pair.responder, SCOPE).expect("responder ledger");
        for exchange_id in [
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "cccccccccccccccccccccccccccccccc",
        ] {
            assert_eq!(
                initiator_ledger.consumed_exchange_ids[exchange_id].state,
                "Installed"
            );
            assert_eq!(
                responder_ledger.consumed_exchange_ids[exchange_id].state,
                "Installed"
            );
        }

        let initiator_config =
            fs::read(pair.initiator.darkirc_dir().join(CONFIG_FILE)).expect("initiator config");
        assert_contact_field(
            &initiator_config,
            "bob",
            "dm_chacha_public",
            &pair.responder_public,
        );
        assert_contact_field(
            &initiator_config,
            "bob",
            "my_dm_chacha_secret",
            &pair.initiator_private,
        );
        let responder_config =
            fs::read(pair.responder.darkirc_dir().join(CONFIG_FILE)).expect("responder config");
        assert_contact_field(
            &responder_config,
            "alice",
            "dm_chacha_public",
            &pair.initiator_public,
        );
        assert_contact_field(
            &responder_config,
            "alice",
            "my_dm_chacha_secret",
            &pair.responder_private,
        );
    }

    #[test]
    fn recovery_finishes_exchange_without_peer_artifact_after_config_commit() {
        let pair = prepared_pair();
        let responder_fingerprint =
            PublicFingerprint::from_encoded_public_key(&pair.responder_public)
                .expect("responder fingerprint");
        let request = CompleteExchange {
            exchange_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            contact_name: "bob",
            expected_peer_fingerprint: &responder_fingerprint,
            defer_apply: true,
            now: EXCHANGE_NOW + 1,
            attestation: &pair.attestation,
        };

        assert!(matches!(
            complete_exchange_with_crash(
                &pair.initiator,
                SCOPE,
                pair.response.public_artifact.as_bytes(),
                &request,
                Some(CrashPoint::AfterConfig),
            ),
            Err(KeyManagerError::CrashInjected)
        ));
        recover(&pair.initiator, SCOPE).expect("artifact-free recovery");

        let workspace = Workspace::open(pair.initiator.clone()).expect("workspace");
        let pending = read_pending_secret(&workspace, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", SCOPE)
            .expect("recovered pending exchange");
        assert_eq!(pending.state, ExchangeState::Installed.as_str());
        assert_eq!(
            pending.peer_artifact.as_deref(),
            Some(pair.response.public_artifact.as_str())
        );
        assert!(
            secure_directory_files(&workspace, TRANSACTIONS_DIR)
                .expect("transaction directory")
                .is_empty()
        );
    }

    #[test]
    fn stage_a_cancel_is_idempotent_and_tombstones_replay() {
        let (_temp, paths) = fixture();
        let attestation = exchange_attestation();
        let public = valid_public(31);
        let private = valid_private(32);
        let request = exchange_request(
            &attestation,
            "bob",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &public,
            &private,
        );
        prepare_exchange(&paths, SCOPE, &request).expect("prepare");
        let first =
            cancel_exchange_at(&paths, SCOPE, request.offer_id, EXCHANGE_NOW).expect("cancel");
        let retry = cancel_exchange_at(&paths, SCOPE, request.offer_id, EXCHANGE_NOW)
            .expect("cancel retry");
        assert_eq!(first, retry);
        assert!(matches!(
            prepare_exchange(&paths, SCOPE, &request),
            Err(KeyManagerError::OfferConflict)
        ));
        let ledger = read_ledger(&paths, SCOPE).expect("ledger");
        assert!(ledger.consumed_exchange_ids.contains_key(request.offer_id));
        assert_eq!(
            ledger.consumed_exchange_ids[request.offer_id].local_fingerprint,
            public_fingerprint(&public).expect("local fingerprint")
        );
        let reused = exchange_request(
            &attestation,
            "carol",
            "dddddddddddddddddddddddddddddddd",
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            &public,
            &private,
        );
        assert!(matches!(
            prepare_exchange(&paths, SCOPE, &reused),
            Err(KeyManagerError::ContactConflict)
        ));
        let ledger_text =
            fs::read_to_string(paths.darkirc_dir().join(LEDGER_FILE)).expect("ledger text");
        assert!(!ledger_text.contains(request.local_private));
    }

    #[test]
    fn initiator_confirmed_cancel_tombstones_responder_offer() {
        let pair = prepared_pair();
        let workspace = Workspace::open(pair.initiator.clone()).expect("workspace");
        let mut secret = read_pending_secret(&workspace, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", SCOPE)
            .expect("initiator pending exchange");
        let local = validate_stored_public_exchange(
            secret.public_artifact.as_bytes(),
            ExchangeRole::InitiatorOffer,
        )
        .expect("local artifact");
        let peer = validate_stored_public_exchange(
            pair.response.public_artifact.as_bytes(),
            ExchangeRole::ResponderResponse,
        )
        .expect("peer artifact");
        secret.peer_public = Some(peer.public_key);
        secret.peer_fingerprint = Some(peer.public_fingerprint.as_str().to_owned());
        secret.peer_contact_id = Some(peer.sender_contact_id);
        secret.peer_artifact = Some(pair.response.public_artifact.clone());
        secret.transcript = Some(
            compute_transcript(&local.canonical, &peer.canonical).expect("exchange transcript"),
        );
        secret.state = ExchangeState::Confirmed.as_str().to_owned();
        replace_pending_secret(&workspace, &secret).expect("store confirmed exchange");

        cancel_exchange_at(
            &pair.initiator,
            SCOPE,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            EXCHANGE_NOW + 2,
        )
        .expect("cancel confirmed initiator");

        let ledger = read_ledger(&pair.initiator, SCOPE).expect("ledger");
        assert_eq!(
            ledger.consumed_exchange_ids["cccccccccccccccccccccccccccccccc"].state,
            ExchangeState::Cancelled.as_str()
        );
    }

    #[test]
    fn cancel_recovers_after_tombstone_commit_before_pending_removal() {
        let (_temp, paths) = fixture();
        let attestation = exchange_attestation();
        let public = valid_public(41);
        let private = valid_private(42);
        let request = exchange_request(
            &attestation,
            "bob",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &public,
            &private,
        );
        prepare_exchange(&paths, SCOPE, &request).expect("prepare");
        let workspace = Workspace::open(paths.clone()).expect("workspace");
        let secret = read_pending_secret(&workspace, request.offer_id, SCOPE).expect("pending");
        let mut ledger = empty_ledger(SCOPE);
        let tombstone =
            exchange_tombstone(&secret, ExchangeState::Cancelled, "").expect("cancel tombstone");
        insert_exchange_tombstone(&mut ledger, request.offer_id, &tombstone)
            .expect("insert tombstone");
        replace_file(
            &workspace,
            LEDGER_FILE,
            &ledger_bytes(&ledger).expect("ledger bytes"),
        )
        .expect("commit tombstone while retaining pending file");

        let status = cancel_exchange_at(&paths, SCOPE, request.offer_id, request.expires_at)
            .expect("retry cancellation after expiry");

        assert_eq!(status.state, ExchangeState::Cancelled.as_str());
        assert!(
            !paths
                .darkirc_dir()
                .join(PENDING_DIR)
                .join(format!("{}.json", request.offer_id))
                .exists()
        );
    }

    #[test]
    fn ledger_rejects_peer_key_bound_to_multiple_contacts() {
        let peer_fingerprint = public_fingerprint(&valid_public(51)).expect("peer fingerprint");
        let mut ledger = empty_ledger(SCOPE);
        ledger.contacts.insert(
            "alice".to_owned(),
            build_install_ledger_contact(
                &peer_fingerprint,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                1,
                &public_fingerprint(&valid_public(52)).expect("alice local fingerprint"),
            ),
        );
        ledger.contacts.insert(
            "bob".to_owned(),
            build_install_ledger_contact(
                &peer_fingerprint,
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                1,
                &public_fingerprint(&valid_public(53)).expect("bob local fingerprint"),
            ),
        );

        assert!(matches!(
            validate_ledger(&ledger),
            Err(KeyManagerError::InvalidLedger)
        ));
    }

    #[test]
    fn ledger_rejects_deferred_future_stage_state() {
        let peer_fingerprint = public_fingerprint(&valid_public(54)).expect("peer fingerprint");
        let mut ledger = empty_ledger(SCOPE);
        let mut contact = build_install_ledger_contact(
            &peer_fingerprint,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            1,
            &public_fingerprint(&valid_public(55)).expect("local fingerprint"),
        );
        contact.state = ExchangeState::LocallyActivated.as_str().to_owned();
        ledger.contacts.insert("alice".to_owned(), contact.clone());
        assert!(matches!(
            validate_ledger(&ledger),
            Err(KeyManagerError::InvalidLedger)
        ));

        contact.state = ExchangeState::Installed.as_str().to_owned();
        contact.activation_deadline = 1;
        ledger.contacts.insert("alice".to_owned(), contact);
        assert!(matches!(
            validate_ledger(&ledger),
            Err(KeyManagerError::InvalidLedger)
        ));
    }

    #[test]
    fn structured_migration_preserves_installed_contact_and_replay_tombstones() {
        let pair = prepared_pair();
        let responder_fingerprint =
            PublicFingerprint::from_encoded_public_key(&pair.responder_public)
                .expect("responder fingerprint");
        complete_exchange(
            &pair.initiator,
            SCOPE,
            pair.response.public_artifact.as_bytes(),
            &CompleteExchange {
                exchange_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                contact_name: "bob",
                expected_peer_fingerprint: &responder_fingerprint,
                defer_apply: true,
                now: EXCHANGE_NOW + 2,
                attestation: &pair.attestation,
            },
        )
        .expect("complete managed contact");
        let source_workspace = Workspace::open(pair.initiator.clone()).expect("source workspace");
        remove_pending(&source_workspace, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .expect("settle source pending state");
        export_migration(&pair.initiator, SCOPE, &pair.attestation)
            .expect("export managed contact");
        let manifest = fs::read(pair.initiator.darkirc_dir().join(MIGRATION_FILE))
            .expect("migration manifest");

        let (_target_temp, target) = fixture();
        update_baseline(&target, SCOPE, baseline(24001).as_bytes()).expect("target baseline");
        stage_migration(&target, SCOPE, &manifest, &pair.attestation).expect("stage migration");
        import_migration(&target, SCOPE, &pair.attestation).expect("import migration");

        let ledger = read_ledger(&target, SCOPE).expect("target ledger");
        assert_eq!(
            ledger.contacts["bob"].state,
            ExchangeState::Installed.as_str()
        );
        for exchange_id in [
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "cccccccccccccccccccccccccccccccc",
        ] {
            assert_eq!(
                ledger.consumed_exchange_ids[exchange_id].state,
                ExchangeState::Installed.as_str()
            );
        }
        let config = fs::read(target.darkirc_dir().join(CONFIG_FILE)).expect("target config");
        assert_contact_field(&config, "bob", "dm_chacha_public", &pair.responder_public);
    }

    #[test]
    fn pending_exchange_count_and_bytes_are_bounded() {
        let (_temp, paths) = fixture();
        let attestation = exchange_attestation();
        let public = valid_public(35);
        let private = valid_private(36);
        let request = exchange_request(
            &attestation,
            "bob",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &public,
            &private,
        );
        prepare_exchange(&paths, SCOPE, &request).expect("prepare");
        let workspace = Workspace::open(paths).expect("workspace");
        let secret = read_pending_secret(&workspace, request.offer_id, SCOPE).expect("pending");
        let count_limited = PendingInventory {
            entries: (0..MAX_PENDING_EXCHANGES)
                .map(|_| PendingEntry {
                    secret: secret.clone(),
                })
                .collect(),
            total_bytes: 0,
        };
        assert!(matches!(
            write_new_pending_secret(&workspace, &secret, &count_limited),
            Err(KeyManagerError::PendingLimit)
        ));
        let bytes_limited = PendingInventory {
            entries: Vec::new(),
            total_bytes: MAX_PENDING_TOTAL_BYTES,
        };
        assert!(matches!(
            write_new_pending_secret(&workspace, &secret, &bytes_limited),
            Err(KeyManagerError::PendingLimit)
        ));
    }

    #[test]
    fn delayed_response_may_outlive_initiator_offer() {
        let (_initiator_temp, initiator) = fixture();
        let (_responder_temp, responder) = fixture();
        let attestation = exchange_attestation();
        let initiator_public = valid_public(41);
        let initiator_private = valid_private(42);
        let mut prepare = exchange_request(
            &attestation,
            "bob",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &initiator_public,
            &initiator_private,
        );
        prepare.expires_at = EXCHANGE_NOW + 600;
        let offer = prepare_exchange(&initiator, SCOPE, &prepare).expect("prepare");

        let responder_public = valid_public(43);
        let responder_private = valid_private(44);
        let initiator_fingerprint =
            PublicFingerprint::from_encoded_public_key(&initiator_public).expect("fingerprint");
        let response = respond_exchange(
            &responder,
            SCOPE,
            offer.public_artifact.as_bytes(),
            &RespondExchange {
                contact_name: "alice",
                offer_id: "cccccccccccccccccccccccccccccccc",
                contact_id: "dddddddddddddddddddddddddddddddd",
                local_public: &responder_public,
                local_private: &responder_private,
                initiator_offer_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                expected_initiator_fingerprint: &initiator_fingerprint,
                label: None,
                created_at: EXCHANGE_NOW + 1,
                expires_at: EXCHANGE_NOW + DEFAULT_OFFER_TTL_SECONDS,
                now: EXCHANGE_NOW + 1,
                attestation: &attestation,
            },
        )
        .expect("delayed response");
        assert_eq!(
            response.expires_at,
            EXCHANGE_NOW + DEFAULT_OFFER_TTL_SECONDS
        );

        let responder_fingerprint =
            PublicFingerprint::from_encoded_public_key(&responder_public).expect("fingerprint");
        complete_exchange(
            &initiator,
            SCOPE,
            response.public_artifact.as_bytes(),
            &CompleteExchange {
                exchange_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                contact_name: "bob",
                expected_peer_fingerprint: &responder_fingerprint,
                defer_apply: true,
                now: EXCHANGE_NOW + 2,
                attestation: &attestation,
            },
        )
        .expect("complete initiator while offer is live");
        complete_exchange(
            &responder,
            SCOPE,
            offer.public_artifact.as_bytes(),
            &CompleteExchange {
                exchange_id: "cccccccccccccccccccccccccccccccc",
                contact_name: "alice",
                expected_peer_fingerprint: &initiator_fingerprint,
                defer_apply: true,
                now: EXCHANGE_NOW + 2,
                attestation: &attestation,
            },
        )
        .expect("complete responder while both artifacts are live");
    }

    #[test]
    fn recovery_promotes_valid_first_create_pending_candidate() {
        let (_temp, paths) = fixture();
        let attestation = exchange_attestation();
        let public = valid_public(47);
        let private = valid_private(48);
        let request = exchange_request(
            &attestation,
            "bob",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &public,
            &private,
        );
        prepare_exchange(&paths, SCOPE, &request).expect("prepare");
        let workspace = Workspace::open(paths.clone()).expect("workspace");
        let final_path = format!("{PENDING_DIR}/{}.json", request.offer_id);
        let candidate_path = format!("{final_path}.next");
        workspace
            .rename(&final_path, &candidate_path)
            .expect("simulate crash before pending promotion");

        assert!(matches!(
            pending_inventory(&workspace, SCOPE),
            Err(KeyManagerError::RecoveryRequired)
        ));
        recover_locked(&workspace, SCOPE).expect("promote pending candidate");

        assert!(paths.darkirc_dir().join(&final_path).exists());
        assert!(!paths.darkirc_dir().join(&candidate_path).exists());
        let inventory = pending_inventory(&workspace, SCOPE).expect("settled inventory");
        assert_eq!(inventory.entries.len(), 1);
        assert_eq!(inventory.entries[0].secret.offer_id, request.offer_id);
    }

    #[test]
    fn recovery_discards_only_scope_bound_pending_candidate() {
        let (_temp, paths) = fixture();
        let attestation = exchange_attestation();
        let public = valid_public(45);
        let private = valid_private(46);
        let request = exchange_request(
            &attestation,
            "bob",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &public,
            &private,
        );
        prepare_exchange(&paths, SCOPE, &request).expect("prepare");
        let workspace = Workspace::open(paths.clone()).expect("workspace");
        let pending = read_pending_secret(&workspace, request.offer_id, SCOPE).expect("pending");
        let candidate_path = format!("{PENDING_DIR}/{}.json.next", request.offer_id);
        workspace
            .write_new(
                &candidate_path,
                &pending_secret_bytes(&pending).expect("candidate bytes"),
            )
            .expect("stranded candidate");
        assert!(matches!(
            pending_inventory(&workspace, SCOPE),
            Err(KeyManagerError::RecoveryRequired)
        ));

        recover_locked(&workspace, SCOPE).expect("recover candidate");
        assert_eq!(
            pending_inventory(&workspace, SCOPE)
                .expect("settled inventory")
                .entries
                .len(),
            1
        );

        let mut foreign = pending;
        foreign.scope_id = "11112222333344445555666677778899".to_owned();
        workspace
            .write_new(
                &candidate_path,
                &pending_secret_bytes(&foreign).expect("foreign bytes"),
            )
            .expect("foreign candidate");
        assert!(matches!(
            recover_locked(&workspace, SCOPE),
            Err(KeyManagerError::RecoveryRequired)
        ));
        assert!(paths.darkirc_dir().join(candidate_path).exists());
    }

    #[test]
    fn stage_a_responder_cancel_tombstones_peer_offer() {
        let pair = prepared_pair();
        cancel_exchange_at(
            &pair.responder,
            SCOPE,
            "cccccccccccccccccccccccccccccccc",
            EXCHANGE_NOW + 1,
        )
        .expect("cancel response");
        let ledger = read_ledger(&pair.responder, SCOPE).expect("ledger");
        assert_eq!(
            ledger.consumed_exchange_ids["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"].state,
            "Cancelled"
        );
        assert_eq!(
            ledger.consumed_exchange_ids["cccccccccccccccccccccccccccccccc"].state,
            "Cancelled"
        );
    }
}
