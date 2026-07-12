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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JournalPhase {
    Prepared,
    LedgerCommitted,
    ConfigCommitted,
}

impl JournalPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::LedgerCommitted => "ledger_committed",
            Self::ConfigCommitted => "config_committed",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "prepared" => Ok(Self::Prepared),
            "ledger_committed" => Ok(Self::LedgerCommitted),
            "config_committed" => Ok(Self::ConfigCommitted),
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
    for relative in [KEY_EXCHANGE_DIR, CANDIDATES_DIR, TRANSACTIONS_DIR] {
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
                | "Installed"
                | "LocallyActivated"
                | "PeerVerified"
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
    }
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
        "baseline-update" | "legacy-adopt" | "migration-import"
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
}
