//! Harness job capabilities (WI#326).
//!
//! The conventional LunarWing harness runs as a Docker worker job with three
//! optional capability layers on top of the plain `worker` mode:
//!
//! 1. **tmpfs secret mounts** — worker-scoped secrets resolved host-side and
//!    pulled by the worker over its authenticated per-job channel, then
//!    written as files under `/run/secrets/<NAME>` on an in-memory tmpfs
//!    (Docker-secrets pattern). Values never enter the container env, the job
//!    context, logs, or job events.
//! 2. **Read-only file/memory injection** — selected tenant workspace
//!    documents are snapshotted into a per-job directory mounted read-only at
//!    `/harness/injected`. Every injected file is recorded in a
//!    `PROVENANCE.json` manifest (normalized source path, SHA-256, size,
//!    timestamp). Identity documents require explicit opt-in.
//! 3. **Staged write-back** — a separate writable staging dir mounted at
//!    `/harness/workspace_out`. On job completion the daemon promotes staged
//!    writes into tenant workspace memory only after path-allowlist, size,
//!    count, and secret-scan checks pass. The allowlist is operator-fixed to
//!    `projects/**`; the caller may only narrow it. System-prompt files,
//!    identity documents, daily logs, lorebooks, `context/`, `backups/`,
//!    and engine-internal paths are NEVER writable (WI#322).
//!
//! Boundary rule: the injection mount is one-way (read-only from the job's
//! perspective); the only way out is the promotion pipeline.
//!
//! Path discipline (MR !1510 review): every workspace-relative path is
//! normalized with [`normalize_harness_path`], which requires
//! normalized == raw, so whitespace/casing tricks cannot smuggle a denied
//! path past the checks (`Workspace::write` trims; we check what we write).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::bootstrap::lunarwing_base_dir;

/// Container path of the tmpfs secrets mount (Docker-secrets pattern).
pub const HARNESS_SECRETS_CONTAINER_DIR: &str = "/run/secrets";
/// Container path of the read-only injection mount.
pub const HARNESS_INJECTION_CONTAINER_DIR: &str = "/harness/injected";
/// Container path of the writable write-back staging mount.
pub const HARNESS_WRITE_BACK_CONTAINER_DIR: &str = "/harness/workspace_out";
/// Provenance manifest file name inside the injection mount.
pub const INJECTION_MANIFEST_NAME: &str = "PROVENANCE.json";

/// Maximum worker-scoped secret files per job.
pub const MAX_SECRET_FILES: usize = 16;
/// Maximum bytes for a single secret file.
pub const MAX_SECRET_BYTES: usize = 64 * 1024;
/// Maximum injected files per job.
pub const MAX_INJECTIONS: usize = 32;
/// Maximum bytes for a single injected file.
pub const MAX_INJECTION_FILE_BYTES: u64 = 512 * 1024;
/// Maximum total bytes across all injected files.
pub const MAX_INJECTION_TOTAL_BYTES: u64 = 4 * 1024 * 1024;

/// Operator-fixed write-back roots (CW-1): staged files may only ever be
/// promoted under `projects/`. Callers may narrow this set (e.g. to
/// `projects/demo/`) but never widen or replace it.
pub const WRITE_BACK_ALLOWED_ROOTS: &[&str] = &["projects"];

const DEFAULT_WRITE_BACK_MAX_FILES: usize = 64;
const DEFAULT_WRITE_BACK_MAX_FILE_BYTES: u64 = 256 * 1024;
const DEFAULT_WRITE_BACK_MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024;

/// Server-side ceilings (CW-6): caller-supplied limits may only go LOWER.
pub const WRITE_BACK_MAX_FILES_CEILING: usize = 256;
pub const WRITE_BACK_MAX_FILE_BYTES_CEILING: u64 = 1024 * 1024;
pub const WRITE_BACK_MAX_TOTAL_BYTES_CEILING: u64 = 8 * 1024 * 1024;

const MAX_STAGING_DEPTH: usize = 8;
/// Cap on directory entries visited during the staging walk (CW-6).
const MAX_STAGING_WALK_ENTRIES: usize = 4096;
/// Cap on recorded rejection entries so a hostile staging dir cannot bloat
/// the audit row unboundedly (CW-6/CW-13).
pub const MAX_PROMOTION_REJECTIONS: usize = 128;
/// Maximum accepted length of a workspace-relative harness path.
const MAX_HARNESS_PATH_CHARS: usize = 512;
/// Secret values shorter than this are not value-redacted from job events:
/// replacing a short token would mangle legitimate output. Such values still
/// never enter env/logs by construction (tmpfs files only).
const MIN_REDACTED_SECRET_BYTES: usize = 8;

/// Errors raised by harness capability validation and staging I/O.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("invalid harness secret mount: {0}")]
    InvalidSecretMount(String),
    #[error("invalid harness injection: {0}")]
    InvalidInjection(String),
    #[error("invalid harness write-back policy: {0}")]
    InvalidPolicy(String),
    #[error("harness write-back rejected: {0}")]
    PromotionRejected(String),
    #[error("harness I/O failed: {0}")]
    Io(String),
}

impl HarnessError {
    fn io(context: impl Into<String>, error: std::io::Error) -> Self {
        Self::Io(format!("{}: {}", context.into(), error))
    }
}

/// SHA-256 hex digest used by injection and promotion provenance records.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Basenames of the workspace identity documents (CW-14: one consistent set,
/// shared with `workspace::IDENTITY_PATHS`).
pub fn identity_file_names() -> Vec<&'static str> {
    crate::workspace::IDENTITY_PATHS
        .iter()
        .filter_map(|p| Path::new(p).file_name())
        .filter_map(|n| n.to_str())
        .collect()
}

/// True when the file name carries tenant identity (any depth).
pub fn is_identity_file_name(file_name: &str) -> bool {
    identity_file_names().contains(&file_name)
}

/// True when the normalized workspace-relative path names an identity
/// document, either exactly (the workspace identity set) or by basename at
/// any depth (conservative: `docs/USER.md` also requires the opt-in).
pub fn is_identity_path(path: &str) -> bool {
    if crate::workspace::is_identity_path(path) {
        return true;
    }
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_identity_file_name)
}

/// Normalize a workspace-relative path and require normalized == raw.
///
/// `Workspace::write` trims and resolves `.`/`..` before writing, so checking
/// the raw string but writing the normalized one let `" SOUL.md"` or
/// `" engine/..."` bypass identity/engine guards (CW-2/CW-5). Here a path is
/// accepted only when it is already in canonical form: no leading/trailing
/// or doubled separators, no `.`/`..` segments, no absolute prefix, no
/// backslashes, and no segment with leading/trailing whitespace or control
/// characters.
pub fn normalize_harness_path(raw: &str) -> Result<String, HarnessError> {
    if raw.is_empty() || raw.len() > MAX_HARNESS_PATH_CHARS {
        return Err(HarnessError::InvalidPolicy(format!(
            "path must be 1..={MAX_HARNESS_PATH_CHARS} characters"
        )));
    }
    if raw.starts_with('/') || raw.starts_with('\\') || raw.contains('\\') {
        return Err(HarnessError::InvalidPolicy(format!(
            "path '{raw}' must be workspace-relative with '/' separators"
        )));
    }
    for segment in raw.split('/') {
        if segment.is_empty() {
            return Err(HarnessError::InvalidPolicy(format!(
                "path '{raw}' contains an empty segment"
            )));
        }
        if segment != segment.trim()
            || segment.chars().any(|c| c.is_control())
            || segment.chars().any(|c| c.is_whitespace() && !c.is_ascii())
        {
            return Err(HarnessError::InvalidPolicy(format!(
                "path '{raw}' has a segment with leading/trailing whitespace or control characters"
            )));
        }
    }
    let normalized = crate::workspace::normalize_path(raw);
    if normalized != raw {
        return Err(HarnessError::InvalidPolicy(format!(
            "path '{raw}' is not in canonical form (normalizes to '{normalized}')"
        )));
    }
    Ok(normalized)
}

/// Hard-deny check for write-back promotion (CW-1). Applied AFTER
/// normalization, regardless of the allowed roots: system-prompt files,
/// identity documents, daily logs, lorebooks, `context/`, `backups/`,
/// quarantined logs, and engine-internal state are never writable through
/// the harness. Returns the rejection reason when denied.
pub fn write_back_deny_reason(normalized: &str) -> Option<&'static str> {
    if is_identity_path(normalized) {
        return Some("identity documents are never writable through write-back");
    }
    if crate::workspace::is_system_prompt_file(normalized) {
        return Some("system-prompt files are never writable through write-back");
    }
    if normalized == "context"
        || normalized.starts_with("context/")
        || normalized == "daily"
        || normalized.starts_with("daily/")
        || normalized == "lorebooks"
        || normalized.starts_with("lorebooks/")
    {
        return Some("context/, daily/ and lorebooks/ are never writable through write-back");
    }
    if crate::workspace::is_backup_path(normalized)
        || crate::workspace::is_quarantined_daily_log(normalized)
    {
        return Some("backup and quarantined paths are reserved for operator recovery");
    }
    if crate::workspace::is_engine_runtime_path(normalized)
        || crate::workspace::is_engine_internal_path(normalized)
    {
        return Some("engine-managed paths are never writable through write-back (WI#322)");
    }
    None
}

/// Injection-side deny check (CW-5): shadow backups, quarantined daily logs,
/// and engine runtime/internal state are never injected — they carry
/// quarantined prompt-injection evidence or engine-private state. Identity
/// documents are handled separately via the explicit opt-in.
fn injection_deny_reason(normalized: &str) -> Option<&'static str> {
    if crate::workspace::is_backup_path(normalized) {
        return Some("shadow backup files (backups/) are reserved and cannot be injected");
    }
    if crate::workspace::is_quarantined_daily_log(normalized) {
        return Some("quarantined daily logs are reserved for operator recovery");
    }
    if crate::workspace::is_engine_runtime_path(normalized)
        || crate::workspace::is_engine_internal_path(normalized)
    {
        return Some("engine-managed paths cannot be injected");
    }
    None
}

/// Env var names that could hijack process behavior; harness secret file
/// names share the env-name surface, so the same denylist applies (CV-4).
/// Canonical list lives in `tools::builtin::job`.
pub fn validate_secret_file_name(name: &str) -> Result<(), HarnessError> {
    if name.is_empty() {
        return Err(HarnessError::InvalidSecretMount(
            "file name cannot be empty".to_string(),
        ));
    }
    let valid = name
        .bytes()
        .enumerate()
        .all(|(i, b)| matches!(b, b'A'..=b'Z' | b'_') || (i > 0 && b.is_ascii_digit()));
    if !valid {
        return Err(HarnessError::InvalidSecretMount(format!(
            "file name '{name}' must match [A-Z_][A-Z0-9_]*"
        )));
    }
    if crate::tools::builtin::job::is_dangerous_env_var_name(name) {
        return Err(HarnessError::InvalidSecretMount(format!(
            "file name '{name}' is on the env-var denylist (could hijack process behavior)"
        )));
    }
    if name.starts_with("LUNARWING_") || name.starts_with("IRONCLAW_") {
        return Err(HarnessError::InvalidSecretMount(format!(
            "file name '{name}' uses a reserved runtime prefix"
        )));
    }
    Ok(())
}

/// A worker-scoped secret requested for tmpfs file mounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFileMount {
    /// Name of the secret in the tenant secrets store.
    pub secret_name: String,
    /// File name under `/run/secrets/`.
    pub file_name: String,
}

impl SecretFileMount {
    pub fn new(secret_name: impl Into<String>, file_name: impl Into<String>) -> Self {
        Self {
            secret_name: secret_name.into(),
            file_name: file_name.into(),
        }
    }

    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.secret_name.trim().is_empty() {
            return Err(HarnessError::InvalidSecretMount(
                "secret name cannot be empty".to_string(),
            ));
        }
        validate_secret_file_name(&self.file_name)
    }
}

/// A secret value resolved host-side, held until the worker pulls it over its
/// authenticated channel. The value is never logged or serialized.
pub struct ResolvedSecretFile {
    pub file_name: String,
    value: SecretString,
}

impl ResolvedSecretFile {
    pub fn new(file_name: impl Into<String>, value: SecretString) -> Self {
        Self {
            file_name: file_name.into(),
            value,
        }
    }

    /// Clone the secret value (secrecy zeroizes each copy on drop).
    pub fn value_clone(&self) -> SecretString {
        self.value.clone()
    }

    /// Consume into the mounted file name and the secret value.
    pub fn into_parts(self) -> (String, SecretString) {
        (self.file_name, self.value)
    }
}

impl std::fmt::Debug for ResolvedSecretFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedSecretFile")
            .field("file_name", &self.file_name)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// One requested workspace injection (read-only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectionSpec {
    /// Workspace-relative source path (e.g. `MEMORY.md`, `projects/x/notes.md`).
    pub source: String,
    /// File name inside the injection mount. Defaults to the source file name.
    pub target_name: Option<String>,
}

impl InjectionSpec {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            target_name: None,
        }
    }

    pub fn with_target(mut self, target_name: impl Into<String>) -> Self {
        self.target_name = Some(target_name.into());
        self
    }

    /// Normalized source path (CW-5): canonical form required, engine/backup/
    /// quarantined paths denied. Provenance records this normalized path.
    pub fn normalized_source(&self) -> Result<String, HarnessError> {
        let normalized = normalize_harness_path(&self.source)
            .map_err(|e| HarnessError::InvalidInjection(e.to_string()))?;
        if let Some(reason) = injection_deny_reason(&normalized) {
            return Err(HarnessError::InvalidInjection(format!(
                "'{normalized}': {reason}"
            )));
        }
        Ok(normalized)
    }

    /// File name the injected document lands under inside the mount.
    pub fn resolved_target_name(&self) -> Result<String, HarnessError> {
        let name = match &self.target_name {
            Some(name) => name.clone(),
            None => Path::new(&self.source)
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
                .ok_or_else(|| {
                    HarnessError::InvalidInjection(format!(
                        "cannot derive a file name from source '{}'",
                        self.source
                    ))
                })?,
        };
        validate_injection_target_name(&name)?;
        Ok(name)
    }

    pub fn validate(&self) -> Result<(), HarnessError> {
        self.normalized_source()?;
        self.resolved_target_name()?;
        Ok(())
    }
}

fn validate_injection_target_name(name: &str) -> Result<(), HarnessError> {
    let valid = !name.is_empty()
        && name.len() <= 255
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !name.starts_with('.');
    if !valid {
        return Err(HarnessError::InvalidInjection(format!(
            "target name '{name}' must be a plain file name ([A-Za-z0-9._-], no leading dot)"
        )));
    }
    Ok(())
}

/// Write-back policy for the staged promotion pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteBackPolicy {
    /// Workspace-relative roots staged files may promote into. These may only
    /// NARROW the operator-fixed [`WRITE_BACK_ALLOWED_ROOTS`] set (CW-1).
    pub allowed_roots: Vec<String>,
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for WriteBackPolicy {
    fn default() -> Self {
        Self {
            allowed_roots: WRITE_BACK_ALLOWED_ROOTS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            max_files: DEFAULT_WRITE_BACK_MAX_FILES,
            max_file_bytes: DEFAULT_WRITE_BACK_MAX_FILE_BYTES,
            max_total_bytes: DEFAULT_WRITE_BACK_MAX_TOTAL_BYTES,
        }
    }
}

impl WriteBackPolicy {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.allowed_roots.is_empty() {
            return Err(HarnessError::InvalidPolicy(
                "at least one allowed root is required".to_string(),
            ));
        }
        for root in &self.allowed_roots {
            let normalized = normalize_harness_path(root)
                .map_err(|e| HarnessError::InvalidPolicy(e.to_string()))?;
            let narrows_fixed_set = WRITE_BACK_ALLOWED_ROOTS
                .iter()
                .any(|fixed| normalized == *fixed || normalized.starts_with(&format!("{fixed}/")));
            if !narrows_fixed_set {
                return Err(HarnessError::InvalidPolicy(format!(
                    "allowed root '{root}' is outside the operator-fixed write-back roots ({})",
                    WRITE_BACK_ALLOWED_ROOTS.join(", ")
                )));
            }
        }
        if self.max_files == 0 || self.max_files > WRITE_BACK_MAX_FILES_CEILING {
            return Err(HarnessError::InvalidPolicy(format!(
                "max_files must be 1..={WRITE_BACK_MAX_FILES_CEILING}"
            )));
        }
        if self.max_file_bytes == 0 || self.max_file_bytes > WRITE_BACK_MAX_FILE_BYTES_CEILING {
            return Err(HarnessError::InvalidPolicy(format!(
                "max_file_bytes must be 1..={WRITE_BACK_MAX_FILE_BYTES_CEILING}"
            )));
        }
        if self.max_total_bytes == 0 || self.max_total_bytes > WRITE_BACK_MAX_TOTAL_BYTES_CEILING {
            return Err(HarnessError::InvalidPolicy(format!(
                "max_total_bytes must be 1..={WRITE_BACK_MAX_TOTAL_BYTES_CEILING}"
            )));
        }
        Ok(())
    }

    /// True when a normalized staging-relative path falls under an allowed
    /// root. The hard-deny set ([`write_back_deny_reason`]) always wins.
    /// DH-9: a staged file named exactly like a root ("projects") is NOT
    /// under it — promotion targets are files below a root directory.
    pub fn root_allows(&self, normalized_path: &str) -> bool {
        self.allowed_roots
            .iter()
            .any(|root| normalized_path.starts_with(&format!("{root}/")))
    }
}

/// Full harness capability request for one job. All layers are optional but
/// at least one must be present for harness mode to make sense.
#[derive(Debug, Clone, Default)]
pub struct HarnessJobOptions {
    pub worker_secrets: Vec<SecretFileMount>,
    pub injections: Vec<InjectionSpec>,
    /// Identity documents are excluded unless this is explicitly true.
    pub allow_identity_injection: bool,
    /// Write-back staging is opt-in; `None` means no writable mount.
    pub write_back: Option<WriteBackPolicy>,
}

impl HarnessJobOptions {
    pub fn is_empty(&self) -> bool {
        self.worker_secrets.is_empty() && self.injections.is_empty() && self.write_back.is_none()
    }

    pub fn validate(&self) -> Result<(), HarnessError> {
        self.validate_secret_mounts()?;
        self.validate_injections()?;
        if let Some(policy) = &self.write_back {
            policy.validate()?;
        }
        Ok(())
    }

    fn validate_secret_mounts(&self) -> Result<(), HarnessError> {
        if self.worker_secrets.len() > MAX_SECRET_FILES {
            return Err(HarnessError::InvalidSecretMount(format!(
                "too many secret files ({} > {MAX_SECRET_FILES})",
                self.worker_secrets.len()
            )));
        }
        let mut names = std::collections::HashSet::new();
        let mut files = std::collections::HashSet::new();
        for mount in &self.worker_secrets {
            mount.validate()?;
            // CV-10: duplicate mounts would make delivery ambiguous.
            if !names.insert(mount.secret_name.as_str()) {
                return Err(HarnessError::InvalidSecretMount(format!(
                    "duplicate secret name '{}'",
                    mount.secret_name
                )));
            }
            if !files.insert(mount.file_name.as_str()) {
                return Err(HarnessError::InvalidSecretMount(format!(
                    "duplicate secret file name '{}'",
                    mount.file_name
                )));
            }
        }
        Ok(())
    }

    fn validate_injections(&self) -> Result<(), HarnessError> {
        if self.injections.len() > MAX_INJECTIONS {
            return Err(HarnessError::InvalidInjection(format!(
                "too many injections ({} > {MAX_INJECTIONS})",
                self.injections.len()
            )));
        }
        let mut targets = std::collections::HashSet::new();
        for spec in &self.injections {
            spec.validate()?;
            let normalized = spec.normalized_source()?;
            if !self.allow_identity_injection && is_identity_path(&normalized) {
                return Err(HarnessError::InvalidInjection(format!(
                    "identity document '{normalized}' requires allow_identity_injection=true"
                )));
            }
            let target = spec.resolved_target_name()?;
            if target == INJECTION_MANIFEST_NAME || !targets.insert(target.clone()) {
                return Err(HarnessError::InvalidInjection(format!(
                    "duplicate or reserved injection target name '{target}'"
                )));
            }
        }
        Ok(())
    }
}

/// Provenance record for one injected file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InjectedFileProvenance {
    /// Normalized workspace-relative source path actually read (CW-5).
    pub source_path: String,
    pub target_name: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub injected_at: DateTime<Utc>,
}

/// Manifest written as `PROVENANCE.json` into the injection mount.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InjectionManifest {
    pub job_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub files: Vec<InjectedFileProvenance>,
}

/// A resolved injection: the document content read from the workspace plus
/// the validated target name.
pub struct InjectionEntry {
    /// Normalized source path (post `normalize_harness_path`).
    pub source_path: String,
    pub target_name: String,
    pub content: String,
}

/// Write injected files (mode 0444) plus the provenance manifest into `dir`.
///
/// Files are created with `O_CREAT|O_EXCL|O_NOFOLLOW` and chmodded through
/// the open fd so a hostile symlink planted at the target cannot redirect the
/// write onto a host file (CW-3f).
pub fn write_injection_dir(
    dir: &Path,
    job_id: Uuid,
    entries: &[InjectionEntry],
) -> Result<InjectionManifest, HarnessError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| HarnessError::io(format!("create {}", dir.display()), e))?;

    let mut total_bytes = 0u64;
    let mut provenance = Vec::with_capacity(entries.len());
    for entry in entries {
        let bytes = entry.content.as_bytes();
        let size = bytes.len() as u64;
        if size > MAX_INJECTION_FILE_BYTES {
            return Err(HarnessError::InvalidInjection(format!(
                "injected file '{}' exceeds {MAX_INJECTION_FILE_BYTES} bytes",
                entry.source_path
            )));
        }
        total_bytes += size;
        if total_bytes > MAX_INJECTION_TOTAL_BYTES {
            return Err(HarnessError::InvalidInjection(format!(
                "injected files exceed {MAX_INJECTION_TOTAL_BYTES} total bytes"
            )));
        }
        validate_injection_target_name(&entry.target_name)?;
        write_file_no_follow(&dir.join(&entry.target_name), bytes, 0o444)?;
        provenance.push(InjectedFileProvenance {
            source_path: entry.source_path.clone(),
            target_name: entry.target_name.clone(),
            sha256: sha256_hex(bytes),
            size_bytes: size,
            injected_at: Utc::now(),
        });
    }

    let manifest = InjectionManifest {
        job_id,
        created_at: Utc::now(),
        files: provenance,
    };
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| HarnessError::Io(format!("serialize manifest: {e}")))?;
    write_file_no_follow(
        &dir.join(INJECTION_MANIFEST_NAME),
        manifest_json.as_bytes(),
        0o444,
    )?;
    Ok(manifest)
}

/// Create `path` with `content`, refusing to follow or replace anything
/// already there, and set `mode` via the open file descriptor.
#[cfg(unix)]
fn write_file_no_follow(path: &Path, content: &[u8], mode: u32) -> Result<(), HarnessError> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| HarnessError::io(format!("create {}", path.display()), e))?;
    let mut file = file;
    use std::io::Write;
    file.write_all(content)
        .map_err(|e| HarnessError::io(format!("write {}", path.display()), e))?;
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(mode))
        .map_err(|e| HarnessError::io(format!("chmod {}", path.display()), e))
}

#[cfg(not(unix))]
fn write_file_no_follow(path: &Path, content: &[u8], _mode: u32) -> Result<(), HarnessError> {
    std::fs::write(path, content)
        .map_err(|e| HarnessError::io(format!("write {}", path.display()), e))?;
    let mut perms = std::fs::metadata(path)
        .map_err(|e| HarnessError::io(format!("stat {}", path.display()), e))?
        .permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(path, perms)
        .map_err(|e| HarnessError::io(format!("chmod {}", path.display()), e))
}

/// Base directory for per-job harness state: `~/.lunarwing/harness/`.
///
/// Deliberately OUTSIDE `~/.lunarwing/projects/` (CW-3): the projects base is
/// bind-mountable by any job with an explicit `project_dir`, so harness
/// injection snapshots and write-back staging must not live under it. The
/// base is created mode 0700.
pub fn harness_base_dir() -> PathBuf {
    lunarwing_base_dir().join("harness")
}

/// Create the harness base dir with owner-only permissions.
pub fn ensure_harness_base_dir() -> Result<PathBuf, HarnessError> {
    let base = harness_base_dir();
    std::fs::create_dir_all(&base)
        .map_err(|e| HarnessError::io(format!("create {}", base.display()), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| HarnessError::io(format!("chmod {}", base.display()), e))?;
    }
    Ok(base)
}

/// Per-job harness host directory: `~/.lunarwing/harness/<job_id>`.
pub fn harness_job_dir(job_id: Uuid) -> PathBuf {
    harness_base_dir().join(job_id.to_string())
}

pub fn injection_dir(job_id: Uuid) -> PathBuf {
    harness_job_dir(job_id).join("injected")
}

pub fn staging_dir(job_id: Uuid) -> PathBuf {
    harness_job_dir(job_id).join("workspace_out")
}

/// Best-effort removal of a job's harness host directory.
pub fn cleanup_harness_job_dir(job_id: Uuid) {
    let dir = harness_job_dir(job_id);
    if dir.exists()
        && let Err(e) = std::fs::remove_dir_all(&dir)
    {
        tracing::warn!(job_id = %job_id, "Failed to remove harness dir {}: {}", dir.display(), e);
    }
}

/// Remove harness job dirs that do not belong to a live container (CW-8 boot
/// sweep). `live_job_ids` carries the currently running labeled containers;
/// dirs younger than `grace` are kept so an in-flight create never loses its
/// staging dir mid-dispatch. Returns the number of dirs removed.
pub fn sweep_stale_harness_dirs(
    live_job_ids: &std::collections::HashSet<Uuid>,
    grace: std::time::Duration,
) -> usize {
    let Ok(base) = ensure_harness_base_dir() else {
        return 0;
    };
    sweep_harness_dirs_in(&base, live_job_ids, grace)
}

fn sweep_harness_dirs_in(
    base: &Path,
    live_job_ids: &std::collections::HashSet<Uuid>,
    grace: std::time::Duration,
) -> usize {
    let Ok(entries) = std::fs::read_dir(base) else {
        return 0;
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(job_id) = name.to_str().unwrap_or("").parse::<Uuid>() else {
            continue; // foreign entry — leave it alone
        };
        if live_job_ids.contains(&job_id) {
            continue;
        }
        let young = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|mtime| {
                now.duration_since(mtime)
                    .map(|age| age < grace)
                    .unwrap_or(false)
            })
            .unwrap_or(true); // unreadable mtime: keep, fail safe
        if young {
            continue;
        }
        match std::fs::remove_dir_all(entry.path()) {
            Ok(()) => {
                removed += 1;
                tracing::info!(job_id = %job_id, "Swept orphaned harness dir");
            }
            Err(e) => {
                tracing::warn!(job_id = %job_id, "Failed to sweep harness dir: {}", e);
            }
        }
    }
    removed
}

/// Value-level redactor for worker-mounted secrets (CV-7, DG-2).
///
/// Built from the job's resolved secret values; scrubs every known encoding
/// of a value from job events and completion output before they are persisted
/// or broadcast, and reused by the promotion pipeline to reject staged files
/// that embed a mounted secret (CW-10).
///
/// Variant coverage per value: the raw text; each line of a multi-line value
/// (PEM / service-account JSON); the JSON-escaped form; the URL-encoded form;
/// lowercase hex; base64 in the standard and URL-safe alphabets (padded and
/// unpadded); the 76-column wrapped form the `base64` CLI emits by default;
/// and the stream-stable base64 cores for a value encoded at byte offsets 1
/// and 2 inside a larger blob (the `Authorization: Basic` shape).
///
/// Redaction is BEST-EFFORT, not a security boundary: a sub-agent that
/// deliberately slices a value (e.g. `cut -c1-32` over several calls, or an
/// encoding not covered above) can smuggle partials past any string matcher.
/// The real control is the `Always` approval tier on harness secret mounts —
/// redaction limits accidental exposure of whole values.
///
/// Variants are held as [`SecretString`] so they zeroize on drop (CV-10);
/// `Debug` deliberately prints no variant material.
#[derive(Clone, Default)]
pub struct SecretValueRedactor {
    variants: Vec<SecretString>,
    /// Longest variant length in bytes; bounds the truncation-cut scan.
    max_variant_bytes: usize,
}

impl std::fmt::Debug for SecretValueRedactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretValueRedactor")
            .field("armed_variants", &self.variants.len())
            .finish()
    }
}

/// Column width of the default (wrapped) output of the `base64` CLI.
const BASE64_CLI_WRAP_COLS: usize = 76;

/// Insert a variant when it is long enough and not already present. Dedup
/// hashes only — plaintext copies live solely in `variants`.
fn push_variant(
    variants: &mut Vec<SecretString>,
    seen: &mut std::collections::HashSet<u64>,
    value: String,
) {
    use std::hash::{Hash, Hasher};
    if value.len() < MIN_REDACTED_SECRET_BYTES {
        return;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    if seen.insert(hasher.finish()) {
        variants.push(SecretString::from(value));
    }
}

/// Insert a base64 encoding in padded, unpadded, and (when long enough to
/// wrap) 76-column wrapped form, plus the stream-stable core with the final
/// quantum dropped (the last 4 chars can mix with suffix bytes when the value
/// sits inside a larger encoded blob).
fn push_base64_variants(
    variants: &mut Vec<SecretString>,
    seen: &mut std::collections::HashSet<u64>,
    encoded: String,
) {
    if encoded.len() > BASE64_CLI_WRAP_COLS {
        let wrapped = encoded
            .as_bytes()
            .chunks(BASE64_CLI_WRAP_COLS)
            .map(|chunk| std::str::from_utf8(chunk).unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        push_variant(variants, seen, wrapped);
    }
    if encoded.len() >= MIN_REDACTED_SECRET_BYTES + 4 {
        let core = encoded[..encoded.len() - 4].to_string();
        push_variant(variants, seen, core);
    }
    let unpadded = encoded.trim_end_matches('=').to_string();
    if unpadded != encoded {
        push_variant(variants, seen, unpadded);
    }
    push_variant(variants, seen, encoded);
}

/// Every known encoding of one secret value (DG-2).
fn secret_variants(raw: &str) -> Vec<SecretString> {
    use base64::Engine;
    use secrecy::ExposeSecret;
    let mut variants = Vec::new();
    let mut seen = std::collections::HashSet::new();

    push_variant(&mut variants, &mut seen, raw.to_string());
    // Multi-line values (PEM keys, service-account JSON) also leak one line
    // at a time.
    for line in raw.lines() {
        push_variant(&mut variants, &mut seen, line.to_string());
        let trimmed = line.trim();
        if trimmed != line {
            push_variant(&mut variants, &mut seen, trimmed.to_string());
        }
    }
    // The form the value takes inside a JSON string literal.
    if let Ok(json_quoted) = serde_json::to_string(raw) {
        let escaped = json_quoted
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(json_quoted.as_str())
            .to_string();
        push_variant(&mut variants, &mut seen, escaped);
    }
    push_variant(
        &mut variants,
        &mut seen,
        urlencoding::encode(raw).into_owned(),
    );
    // `xxd -p` / hex dumps.
    let mut hex = String::with_capacity(raw.len() * 2);
    for byte in raw.as_bytes() {
        hex.push_str(&format!("{byte:02x}"));
    }
    push_variant(&mut variants, &mut seen, hex);

    // Base64: standard + URL-safe alphabets, and the stream-stable cores for
    // a value encoded inside a larger blob at byte offset 0, 1, or 2. Offset
    // o leaves b64(raw[a..]) stable from its second quantum on, with
    // a = (3 - o) % 3 — so offsets 1 and 2 need the raw[2..] / raw[1..]
    // encodings (their final quantum is dropped inside push_base64_variants).
    let bytes = raw.as_bytes();
    for start in [0usize, 1, 2] {
        if start >= bytes.len() {
            continue;
        }
        let slice = &bytes[start..];
        push_base64_variants(
            &mut variants,
            &mut seen,
            base64::engine::general_purpose::STANDARD.encode(slice),
        );
        if start == 0 {
            push_base64_variants(
                &mut variants,
                &mut seen,
                base64::engine::general_purpose::URL_SAFE.encode(slice),
            );
        }
    }

    // Longest first so overlapping values redact fully.
    variants.sort_by_key(|v| std::cmp::Reverse(v.expose_secret().len()));
    variants
}

impl SecretValueRedactor {
    pub fn new(values: &[SecretString]) -> Self {
        use secrecy::ExposeSecret;
        let mut variants = Vec::new();
        for value in values {
            let raw = value.expose_secret();
            if raw.len() < MIN_REDACTED_SECRET_BYTES {
                continue;
            }
            variants.extend(secret_variants(raw));
        }
        let max_variant_bytes = variants
            .iter()
            .map(|v| v.expose_secret().len())
            .max()
            .unwrap_or(0);
        Self {
            variants,
            max_variant_bytes,
        }
    }

    /// Add one more value to an existing redactor (e.g. the worker bearer
    /// token, which is readable by job children via /proc/1/environ — DG-7).
    pub fn add_value(&mut self, value: &str) {
        use secrecy::ExposeSecret;
        if value.len() < MIN_REDACTED_SECRET_BYTES {
            return;
        }
        self.variants.extend(secret_variants(value));
        self.variants
            .sort_by_key(|v| std::cmp::Reverse(v.expose_secret().len()));
        self.max_variant_bytes = self
            .variants
            .iter()
            .map(|v| v.expose_secret().len())
            .max()
            .unwrap_or(0);
    }

    pub fn is_armed(&self) -> bool {
        !self.variants.is_empty()
    }

    /// True when `text` contains any mounted secret value in a known encoding.
    pub fn contains_secret(&self, text: &str) -> bool {
        use secrecy::ExposeSecret;
        self.variants
            .iter()
            .any(|v| text.contains(v.expose_secret()))
    }

    pub fn scrub(&self, text: &str) -> String {
        use secrecy::ExposeSecret;
        let mut out = text.to_string();
        for variant in &self.variants {
            let variant = variant.expose_secret();
            if out.contains(variant) {
                out = out.replace(variant, "[redacted-secret]");
            }
        }
        out
    }

    /// Scrub first, THEN truncate (DG-2): the daemon never sees text the
    /// redactor has not scanned. When the cut lands on a value that was
    /// already truncated upstream, the trailing partial prefix (≥
    /// [`MIN_REDACTED_SECRET_BYTES`] bytes) is redacted as well.
    pub fn scrub_and_truncate(&self, text: &str, max: usize) -> String {
        let scrubbed = self.scrub(text);
        if scrubbed.len() <= max {
            return scrubbed;
        }
        let end = crate::util::floor_char_boundary(&scrubbed, max);
        let mut cut = scrubbed[..end].to_string();
        self.redact_partial_tail(&mut cut);
        cut.push_str("...");
        cut
    }

    /// Replace the longest suffix of `text` that is a proper prefix (≥
    /// [`MIN_REDACTED_SECRET_BYTES`] bytes) of any variant.
    fn redact_partial_tail(&self, text: &mut String) {
        use secrecy::ExposeSecret;
        if self.max_variant_bytes < MIN_REDACTED_SECRET_BYTES + 1 {
            return;
        }
        let tail_bytes = text.as_bytes();
        let mut best = 0usize;
        for variant in &self.variants {
            let variant = variant.expose_secret().as_bytes();
            let upper = (variant.len() - 1).min(tail_bytes.len());
            let mut k = upper;
            while k >= MIN_REDACTED_SECRET_BYTES {
                if tail_bytes.ends_with(&variant[..k]) {
                    best = best.max(k);
                    break;
                }
                k -= 1;
            }
        }
        if best > 0 {
            let keep = crate::util::floor_char_boundary(text, text.len() - best);
            text.truncate(keep);
            text.push_str("[redacted-secret]");
        }
    }

    /// Recursively scrub every string in a JSON value in place, including
    /// object keys (DG-2: a secret used as a JSON key must not survive).
    pub fn scrub_json(&self, value: &mut serde_json::Value) {
        if !self.is_armed() {
            return;
        }
        match value {
            serde_json::Value::String(s) => {
                *s = self.scrub(s);
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    self.scrub_json(item);
                }
            }
            serde_json::Value::Object(map) => {
                let entries: Vec<(String, serde_json::Value)> = std::mem::take(map)
                    .into_iter()
                    .map(|(key, mut v)| {
                        self.scrub_json(&mut v);
                        (self.scrub(&key), v)
                    })
                    .collect();
                *map = entries.into_iter().collect();
            }
            _ => {}
        }
    }
}

/// One staged file that passed all promotion checks.
#[derive(Debug, Clone)]
pub struct StagedWrite {
    /// Normalized workspace-relative target path (forward slashes).
    pub relative_path: String,
    pub content: String,
    pub sha256: String,
    pub size_bytes: u64,
}

/// A staged file rejected by the promotion pipeline, with the reason.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PromotionRejection {
    pub relative_path: String,
    pub reason: String,
}

/// Result of scanning the staging dir before any write reaches the workspace.
#[derive(Debug, Default)]
pub struct PromotionPlan {
    pub accepted: Vec<StagedWrite>,
    pub rejections: Vec<PromotionRejection>,
    /// Rejections dropped past [`MAX_PROMOTION_REJECTIONS`] (CW-6/CW-13).
    pub dropped_rejections: usize,
}

impl PromotionPlan {
    fn reject(&mut self, relative_path: String, reason: String) {
        if self.rejections.len() >= MAX_PROMOTION_REJECTIONS {
            self.dropped_rejections += 1;
            return;
        }
        self.rejections.push(PromotionRejection {
            relative_path: sanitize_display_path(&relative_path),
            reason,
        });
    }
}

/// Audit record for one promoted file (WI#326 provenance trail).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PromotionRecord {
    pub relative_path: String,
    pub sha256: String,
    /// SHA-256 of the document this write replaced, when one existed.
    pub previous_sha256: Option<String>,
    /// Workspace path holding a full snapshot of the replaced content
    /// (`backups/harness/<job_id>/<path>`) — the actual rollback target
    /// (DH-5: a bare hash is not a rollback). None for new files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
    pub size_bytes: u64,
    pub job_id: Uuid,
    pub owner_id: String,
    pub promoted_at: DateTime<Utc>,
}

/// Outcome of a completed promotion run, stored on the job handle.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PromotionSummary {
    pub records: Vec<PromotionRecord>,
    pub rejections: Vec<PromotionRejection>,
    pub dropped_rejections: usize,
    /// True when promotion never ran (failed job, container teardown failure,
    /// missing resolver); `reason` says why.
    pub skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
}

/// Trim a container-controlled path to a bounded, control-char-free string
/// before it is returned to the parent agent or persisted (CW-13).
fn sanitize_display_path(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .take(MAX_HARNESS_PATH_CHARS)
        .collect()
}

/// Scan the staging dir and partition files into promotable writes and
/// rejections. Enforces normalized path discipline, the hard-deny set, the
/// (narrowed) root allowlist, size/count limits, UTF-8, the leak scan, and
/// the mounted-secret scan.
pub fn plan_promotion(
    staging: &Path,
    policy: &WriteBackPolicy,
    redactor: Option<&SecretValueRedactor>,
) -> Result<PromotionPlan, HarnessError> {
    policy.validate()?;
    let mut files = Vec::new();
    // DH-6: the walk runs over the CANONICAL staging dir, so every relative
    // path below must strip the canonical prefix — stripping the raw prefix
    // rejects everything when the harness base path contains a symlink.
    let canonical_staging = if staging.is_dir() {
        let canonical = staging
            .canonicalize()
            .map_err(|e| HarnessError::io(format!("canonicalize {}", staging.display()), e))?;
        let mut visited = 0usize;
        collect_staged_files(&canonical, 0, &mut visited, &mut files)?;
        canonical
    } else {
        staging.to_path_buf()
    };
    files.sort();

    let mut plan = PromotionPlan::default();
    let mut total_bytes = 0u64;
    let detector = crate::safety::LeakDetector::new();

    for (path, lstat_size) in files {
        if plan.accepted.len() >= policy.max_files {
            plan.reject(
                relative_lossy(&canonical_staging, &path),
                format!("write-back file count limit ({}) reached", policy.max_files),
            );
            continue;
        }
        let relative = match strict_relative(&canonical_staging, &path) {
            Some(rel) => rel,
            None => {
                plan.reject(
                    relative_lossy(&canonical_staging, &path),
                    "non-UTF-8 path cannot be promoted".to_string(),
                );
                continue;
            }
        };
        match screen_staged_file(ScreenInput {
            path: &path,
            relative: &relative,
            lstat_size,
            canonical_staging: &canonical_staging,
            policy,
            detector: &detector,
            redactor,
            total_bytes: &mut total_bytes,
        }) {
            Ok(write) => plan.accepted.push(write),
            Err(reason) => plan.reject(relative, reason),
        }
    }
    Ok(plan)
}

/// lstat-walk the staging dir (no symlink following), skipping dot-names.
/// Carries the lstat size so oversized files can be rejected before any read
/// (CW-6). Entry count is capped so a hostile dir cannot OOM the walk.
fn collect_staged_files(
    dir: &Path,
    depth: usize,
    visited: &mut usize,
    out: &mut Vec<(PathBuf, u64)>,
) -> Result<(), HarnessError> {
    if depth > MAX_STAGING_DEPTH {
        return Err(HarnessError::PromotionRejected(format!(
            "staging nesting exceeds {MAX_STAGING_DEPTH} levels under {}",
            dir.display()
        )));
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|e| HarnessError::io(format!("read {}", dir.display()), e))?;
    for entry in entries {
        let entry = entry.map_err(|e| HarnessError::io("read dir entry", e))?;
        *visited += 1;
        if *visited > MAX_STAGING_WALK_ENTRIES {
            return Err(HarnessError::PromotionRejected(format!(
                "staging entry count exceeds {MAX_STAGING_WALK_ENTRIES}"
            )));
        }
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        // DirEntry::metadata is lstat: symlinks are not followed here.
        let metadata = entry
            .metadata()
            .map_err(|e| HarnessError::io(format!("stat {}", path.display()), e))?;
        if metadata.is_dir() {
            collect_staged_files(&path, depth + 1, visited, out)?;
        } else if metadata.is_file() {
            out.push((path, metadata.len()));
        }
        // Symlinks and other special files are ignored entirely.
    }
    Ok(())
}

/// Strict staging-relative path: forward-slash joined, UTF-8 required.
fn strict_relative(base: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(base).ok()?;
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

fn relative_lossy(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

struct ScreenInput<'a> {
    path: &'a Path,
    relative: &'a str,
    lstat_size: u64,
    canonical_staging: &'a Path,
    policy: &'a WriteBackPolicy,
    detector: &'a crate::safety::LeakDetector,
    redactor: Option<&'a SecretValueRedactor>,
    total_bytes: &'a mut u64,
}

fn screen_staged_file(input: ScreenInput<'_>) -> Result<StagedWrite, String> {
    let relative =
        normalize_harness_path(input.relative).map_err(|e| format!("invalid staged path: {e}"))?;
    if let Some(reason) = write_back_deny_reason(&relative) {
        return Err(reason.to_string());
    }
    if !input.policy.root_allows(&relative) {
        return Err(format!(
            "path is outside the allowed write-back roots ({})",
            input.policy.allowed_roots.join(", ")
        ));
    }
    // CW-3: re-verify containment right before reading — the walk used lstat,
    // but a hostile job could swap an ancestor for a symlink afterwards.
    let parent = input
        .path
        .parent()
        .ok_or_else(|| "staged path has no parent".to_string())?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|e| format!("canonicalize parent failed: {e}"))?;
    if !canonical_parent.starts_with(input.canonical_staging) {
        return Err("staged path escapes the staging dir".to_string());
    }
    if input.lstat_size > input.policy.max_file_bytes {
        return Err(format!(
            "file exceeds the {} byte per-file limit",
            input.policy.max_file_bytes
        ));
    }

    let bytes = read_staged_file(input.path, input.policy.max_file_bytes)?;
    let size = bytes.len() as u64;
    *input.total_bytes += size;
    if *input.total_bytes > input.policy.max_total_bytes {
        return Err(format!(
            "write-back total exceeds the {} byte limit",
            input.policy.max_total_bytes
        ));
    }
    let content = String::from_utf8(bytes).map_err(|_| "not valid UTF-8".to_string())?;
    let scan = input.detector.scan(&content);
    if !scan.is_clean() {
        return Err("secret pattern detected by leak scan".to_string());
    }
    if let Some(redactor) = input.redactor
        && redactor.contains_secret(&content)
    {
        return Err("content embeds a mounted worker secret value".to_string());
    }
    Ok(StagedWrite {
        relative_path: relative,
        sha256: sha256_hex(content.as_bytes()),
        size_bytes: size,
        content,
    })
}

/// Bounded, symlink-safe read of one staged file (CW-3d, CW-6):
/// `O_NOFOLLOW|O_NONBLOCK` open (a dangling symlink or fifo fails instead of
/// being followed or blocking), fstat confirms a regular file within the size
/// limit, and the read itself is capped at `max_bytes + 1`.
#[cfg(unix)]
fn read_staged_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::OpenOptionsExt;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("open failed: {e}"))?;
    let metadata = file.metadata().map_err(|e| format!("fstat failed: {e}"))?;
    if !metadata.is_file() {
        return Err("not a regular file".to_string());
    }
    if metadata.size() > max_bytes {
        return Err(format!("file exceeds the {max_bytes} byte per-file limit"));
    }
    let mut limited = file.take(max_bytes + 1);
    let mut bytes = Vec::new();
    limited
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read failed: {e}"))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!("file exceeds the {max_bytes} byte per-file limit"));
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn read_staged_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| format!("open failed: {e}"))?;
    let metadata = file.metadata().map_err(|e| format!("stat failed: {e}"))?;
    if !metadata.is_file() {
        return Err("not a regular file".to_string());
    }
    let mut limited = file.take(max_bytes + 1);
    let mut bytes = Vec::new();
    limited
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read failed: {e}"))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!("file exceeds the {max_bytes} byte per-file limit"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_file_names_must_be_env_safe() {
        assert!(validate_secret_file_name("GITHUB_TOKEN").is_ok());
        assert!(validate_secret_file_name("_PRIVATE").is_ok());
        assert!(validate_secret_file_name("").is_err());
        assert!(validate_secret_file_name("lower").is_err());
        assert!(validate_secret_file_name("1STARTS_WITH_DIGIT").is_err());
        assert!(validate_secret_file_name("HAS-DASH").is_err());
        assert!(validate_secret_file_name("HAS/slash").is_err());
    }

    #[test]
    fn secret_file_names_reject_env_denylist_and_reserved_prefixes() {
        // CV-4: LD_PRELOAD/PATH-style names would echo values into job events
        // via loader/shell behavior.
        for denied in ["LD_PRELOAD", "PATH", "LD_LIBRARY_PATH", "BASH_ENV", "HOME"] {
            assert!(
                validate_secret_file_name(denied).is_err(),
                "{denied} must be rejected"
            );
        }
        for reserved in ["LUNARWING_TOKEN", "IRONCLAW_JOB_ID"] {
            assert!(
                validate_secret_file_name(reserved).is_err(),
                "{reserved} must be rejected"
            );
        }
    }

    #[test]
    fn identity_files_detected_by_name_at_any_depth() {
        // CW-14: the full workspace identity set, not just SOUL/USER/IDENTITY.
        for name in [
            "SOUL.md",
            "USER.md",
            "IDENTITY.md",
            "AGENTS.md",
            "TOOLS.md",
            "BOOTSTRAP.md",
        ] {
            assert!(is_identity_path(name), "{name} at root");
            assert!(is_identity_path(&format!("docs/{name}")), "{name} nested");
        }
        assert!(!is_identity_path("projects/x/SOUL.md.txt"));
        assert!(!is_identity_path("MEMORY.md"));
    }

    #[test]
    fn harness_paths_reject_noncanonical_forms() {
        // CW-2/CW-5: paths are checked AFTER normalization and must already
        // be canonical (normalized == raw).
        for bad in [
            " SOUL.md",
            "SOUL.md\t",
            "IDENTITY.md ",
            "\u{3000}USER.md",
            " engine",
            "\u{a0}backups",
            "SOUL.md\n",
            "a//b",
            "./MEMORY.md",
            "projects/../MEMORY.md",
            "/absolute.md",
            "back\\slash.md",
            "projects/\u{7}bell.md",
        ] {
            assert!(
                normalize_harness_path(bad).is_err(),
                "'{bad}' must be rejected"
            );
        }
        assert_eq!(
            normalize_harness_path("projects/demo/notes.md").unwrap(),
            "projects/demo/notes.md"
        );
        assert_eq!(normalize_harness_path("MEMORY.md").unwrap(), "MEMORY.md");
    }

    #[test]
    fn injection_specs_validate_and_resolve_targets() {
        let spec = InjectionSpec::new("projects/demo/notes.md");
        assert_eq!(spec.resolved_target_name().unwrap(), "notes.md");

        let renamed = InjectionSpec::new("MEMORY.md").with_target("memory_snapshot.md");
        assert_eq!(
            renamed.resolved_target_name().unwrap(),
            "memory_snapshot.md"
        );

        assert!(InjectionSpec::new("../escape").validate().is_err());
        assert!(InjectionSpec::new("/absolute").validate().is_err());
        assert!(InjectionSpec::new("a//b").validate().is_err());
        assert!(
            InjectionSpec::new("ok.md")
                .with_target("bad/name")
                .validate()
                .is_err()
        );
    }

    #[test]
    fn injection_denies_engine_backup_and_quarantined_sources() {
        // CW-5: these stay denied even with the identity opt-in set.
        for source in [
            "backups/SOUL.md.bak",
            "engine/.runtime/state.json",
            "engine/projects/p/missions/m/mission.json",
            "daily/2026-10-10.poisoned-1.md",
        ] {
            let options = HarnessJobOptions {
                injections: vec![InjectionSpec::new(source)],
                allow_identity_injection: true,
                ..Default::default()
            };
            assert!(
                options.validate().is_err(),
                "'{source}' must never be injectable"
            );
        }
    }

    #[test]
    fn options_reject_identity_injection_without_opt_in() {
        let options = HarnessJobOptions {
            injections: vec![InjectionSpec::new("SOUL.md")],
            ..Default::default()
        };
        assert!(options.validate().is_err());

        let opted_in = HarnessJobOptions {
            allow_identity_injection: true,
            ..options
        };
        assert!(opted_in.validate().is_ok());

        // Whitespace-padded identity sources are rejected before the opt-in
        // check, so they cannot smuggle identity content past it (CW-5).
        let padded = HarnessJobOptions {
            injections: vec![InjectionSpec::new(" SOUL.md")],
            allow_identity_injection: true,
            ..Default::default()
        };
        assert!(padded.validate().is_err());
    }

    #[test]
    fn options_reject_duplicate_targets_and_reserved_manifest_name() {
        let options = HarnessJobOptions {
            injections: vec![
                InjectionSpec::new("a/notes.md"),
                InjectionSpec::new("b/notes.md"),
            ],
            ..Default::default()
        };
        assert!(options.validate().is_err());

        let reserved = HarnessJobOptions {
            injections: vec![InjectionSpec::new("a/x.md").with_target(INJECTION_MANIFEST_NAME)],
            ..Default::default()
        };
        assert!(reserved.validate().is_err());
    }

    #[test]
    fn options_reject_duplicate_secret_names_and_file_names() {
        // CV-10: duplicate mounts make delivery ambiguous.
        let dup_file = HarnessJobOptions {
            worker_secrets: vec![
                SecretFileMount::new("secret_a", "API_KEY"),
                SecretFileMount::new("secret_b", "API_KEY"),
            ],
            ..Default::default()
        };
        assert!(dup_file.validate().is_err());

        let dup_name = HarnessJobOptions {
            worker_secrets: vec![
                SecretFileMount::new("secret_a", "KEY_A"),
                SecretFileMount::new("secret_a", "KEY_B"),
            ],
            ..Default::default()
        };
        assert!(dup_name.validate().is_err());
    }

    #[test]
    fn write_back_policy_defaults_and_validation() {
        let policy = WriteBackPolicy::default();
        assert!(policy.validate().is_ok());
        assert!(policy.root_allows("projects/x/result.md"));
        assert!(!policy.root_allows("MEMORY.md"));
    }

    #[test]
    fn write_back_roots_may_only_narrow_the_fixed_set() {
        // CW-1: the LLM may narrow `projects/` but never widen or replace it.
        for bad_root in [
            "AGENTS.md",
            "HEARTBEAT.md",
            "MEMORY.md",
            "../escape/",
            "/absolute",
            "engine",
            "backups",
            "daily",
            "context",
            "lorebooks",
            "projects/..",
            " projects",
            "projects ",
        ] {
            let policy = WriteBackPolicy {
                allowed_roots: vec![bad_root.to_string()],
                ..Default::default()
            };
            assert!(policy.validate().is_err(), "root '{bad_root}' must fail");
        }
        let narrowed = WriteBackPolicy {
            allowed_roots: vec!["projects/demo".to_string()],
            ..Default::default()
        };
        assert!(narrowed.validate().is_ok());
        assert!(narrowed.root_allows("projects/demo/out.md"));
        assert!(!narrowed.root_allows("projects/other/out.md"));
    }

    #[test]
    fn write_back_limits_have_server_side_ceilings() {
        // CW-6: the caller may lower limits but never raise them past ceilings.
        let too_many_files = WriteBackPolicy {
            max_files: WRITE_BACK_MAX_FILES_CEILING + 1,
            ..Default::default()
        };
        assert!(too_many_files.validate().is_err());
        let too_big_file = WriteBackPolicy {
            max_file_bytes: WRITE_BACK_MAX_FILE_BYTES_CEILING + 1,
            ..Default::default()
        };
        assert!(too_big_file.validate().is_err());
        let too_big_total = WriteBackPolicy {
            max_total_bytes: WRITE_BACK_MAX_TOTAL_BYTES_CEILING + 1,
            ..Default::default()
        };
        assert!(too_big_total.validate().is_err());
        let zero = WriteBackPolicy {
            max_files: 0,
            ..Default::default()
        };
        assert!(zero.validate().is_err());
        let lowered = WriteBackPolicy {
            max_files: 1,
            max_file_bytes: 1024,
            max_total_bytes: 2048,
            ..Default::default()
        };
        assert!(lowered.validate().is_ok());
    }

    #[test]
    fn injection_dir_writes_files_readonly_with_manifest() {
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let dir = tmp.path().join("injected");
        let entries = vec![InjectionEntry {
            source_path: "MEMORY.md".to_string(),
            target_name: "memory.md".to_string(),
            content: "# Memory".to_string(),
        }];
        let job_id = Uuid::new_v4();
        let manifest = write_injection_dir(&dir, job_id, &entries).unwrap(); // safety: test

        assert_eq!(manifest.job_id, job_id);
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].source_path, "MEMORY.md");
        assert_eq!(manifest.files[0].sha256, sha256_hex(b"# Memory"));

        let written_path = dir.join("memory.md");
        let written = std::fs::read_to_string(&written_path).unwrap(); // safety: test
        assert_eq!(written, "# Memory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&written_path)
                .unwrap() // safety: test
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o444, "injected files must be read-only");
        }
        let manifest_on_disk: InjectionManifest = serde_json::from_str(
            &std::fs::read_to_string(dir.join(INJECTION_MANIFEST_NAME)).unwrap(), // safety: test
        )
        .unwrap(); // safety: test
        assert_eq!(manifest_on_disk.files.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn injection_write_refuses_preexisting_target() {
        // CW-3f: create_new + O_NOFOLLOW means a planted file or symlink at
        // the target aborts the injection instead of being clobbered.
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let dir = tmp.path().join("injected");
        std::fs::create_dir_all(&dir).unwrap(); // safety: test
        let outside = tmp.path().join("host.md");
        std::fs::write(&outside, "host").unwrap(); // safety: test
        std::os::unix::fs::symlink(&outside, dir.join("link.md")).unwrap(); // safety: test
        let entries = vec![InjectionEntry {
            source_path: "MEMORY.md".to_string(),
            target_name: "link.md".to_string(),
            content: "injected".to_string(),
        }];
        let result = write_injection_dir(&dir, Uuid::new_v4(), &entries);
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(), // safety: test
            "host",
            "symlink target must not be clobbered"
        );
    }

    fn staging_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        for (rel, content) in files {
            let path = tmp.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap(); // safety: test
            std::fs::write(path, content).unwrap(); // safety: test
        }
        tmp
    }

    #[test]
    fn root_allows_only_files_below_a_root() {
        // DH-9: a staged file named exactly like the root is not "under" it.
        let policy = WriteBackPolicy::default();
        assert!(!policy.root_allows("projects"));
        assert!(policy.root_allows("projects/out.md"));
        assert!(policy.root_allows("projects/demo/out.md"));
        assert!(!policy.root_allows("projectsx/out.md"));

        let tmp = staging_with(&[("projects", "a file literally named projects")]);
        let plan = plan_promotion(tmp.path(), &WriteBackPolicy::default(), None).unwrap(); // safety: test
        assert!(plan.accepted.is_empty());
        assert!(
            plan.rejections
                .iter()
                .any(|r| r.relative_path == "projects")
        );
    }

    #[cfg(unix)]
    #[test]
    fn promotion_works_through_a_symlinked_base_path() {
        // DH-6: when the harness base path contains a symlink, canonical and
        // raw prefixes differ — promotion must still work (strip canonical).
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let real = tmp.path().join("real");
        let staging = real.join("workspace_out");
        std::fs::create_dir_all(staging.join("projects")).unwrap(); // safety: test
        std::fs::write(staging.join("projects/out.md"), "result").unwrap(); // safety: test
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap(); // safety: test
        let via_link = link.join("workspace_out");
        let plan = plan_promotion(&via_link, &WriteBackPolicy::default(), None).unwrap(); // safety: test
        assert_eq!(plan.accepted.len(), 1);
        assert_eq!(plan.accepted[0].relative_path, "projects/out.md");
    }

    #[cfg(unix)]
    #[test]
    fn read_staged_file_rejects_symlinks_and_fifos_fast() {
        // DH-8: exercise the read-path defense DIRECTLY (the lstat walk
        // filters these before reads, so walking tests cannot regress it).
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let target = tmp.path().join("target.md");
        std::fs::write(&target, "secret-ish").unwrap(); // safety: test
        let link = tmp.path().join("link.md");
        std::os::unix::fs::symlink(&target, &link).unwrap(); // safety: test
        // Without O_NOFOLLOW this read would SUCCEED.
        assert!(read_staged_file(&link, 1024).is_err());

        let fifo = tmp.path().join("pipe.md");
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap(); // safety: test
        let created = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) }; // safety: test fifo creation
        assert_eq!(created, 0);
        // Without O_NONBLOCK the open would block forever — bound the wait so
        // a regression FAILS instead of hanging the suite.
        let (tx, rx) = std::sync::mpsc::channel();
        let fifo_clone = fifo.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read_staged_file(&fifo_clone, 1024));
        });
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("fifo read must fail fast, not block"); // safety: test
        assert!(result.is_err());
    }

    #[test]
    fn promotion_caps_rejection_list_and_walk_entries() {
        // DH-8: the 128-rejection cap and the 4096-entry walk cap.
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let dir = tmp.path().join("projects");
        std::fs::create_dir_all(&dir).unwrap(); // safety: test
        for i in 0..(MAX_PROMOTION_REJECTIONS + 2) {
            // Non-UTF-8 content: every file is rejected.
            std::fs::write(dir.join(format!("f{i}.md")), [0xff, 0xfe]).unwrap(); // safety: test
        }
        let plan = plan_promotion(tmp.path(), &WriteBackPolicy::default(), None).unwrap(); // safety: test
        assert_eq!(plan.rejections.len(), MAX_PROMOTION_REJECTIONS);
        assert_eq!(plan.dropped_rejections, 2);

        let wide = tempfile::tempdir().unwrap(); // safety: test
        let dir = wide.path().join("projects");
        std::fs::create_dir_all(&dir).unwrap(); // safety: test
        for i in 0..(MAX_STAGING_WALK_ENTRIES + 1) {
            std::fs::write(dir.join(format!("f{i:05}.md")), "x").unwrap(); // safety: test
        }
        let result = plan_promotion(wide.path(), &WriteBackPolicy::default(), None);
        assert!(result.is_err(), "walk entry cap must fail the plan");
    }

    #[test]
    fn promotion_accepts_allowed_and_rejects_disallowed_paths() {
        let tmp = staging_with(&[
            ("projects/demo/out.md", "result"),
            ("MEMORY.md", "must not promote"),
            ("SOUL.md", "identity"),
            ("engine/projects/x/missions/m/mission.json", "{}"),
            ("random.txt", "outside roots"),
        ]);
        let plan = plan_promotion(tmp.path(), &WriteBackPolicy::default(), None).unwrap(); // safety: test

        assert_eq!(plan.accepted.len(), 1);
        assert_eq!(plan.accepted[0].relative_path, "projects/demo/out.md");
        assert_eq!(plan.accepted[0].sha256, sha256_hex(b"result"));

        let rejected: Vec<&str> = plan
            .rejections
            .iter()
            .map(|r| r.relative_path.as_str())
            .collect();
        assert!(rejected.contains(&"MEMORY.md"));
        assert!(rejected.contains(&"SOUL.md"));
        assert!(rejected.contains(&"engine/projects/x/missions/m/mission.json"));
        assert!(rejected.contains(&"random.txt"));
    }

    #[test]
    fn promotion_hard_denies_identity_and_reserved_paths_under_allowed_roots() {
        // Non-vacuous guard coverage (MR !1510 review): these live UNDER the
        // allowed root, so only the hard-deny checks can stop them.
        let tmp = staging_with(&[
            ("projects/demo/SOUL.md", "identity by basename"),
            ("projects/demo/USER.md", "identity by basename"),
            ("projects/demo/AGENTS.md", "identity by basename"),
        ]);
        let plan = plan_promotion(tmp.path(), &WriteBackPolicy::default(), None).unwrap(); // safety: test
        assert!(plan.accepted.is_empty());
        assert_eq!(plan.rejections.len(), 3);
        assert!(
            plan.rejections
                .iter()
                .all(|r| r.reason.contains("identity"))
        );
    }

    #[test]
    fn promotion_hard_denies_prompt_daily_lorebook_context_backup_paths() {
        // CW-1: with any roots, these are never writable. Root check would
        // also reject them, so deny the full set explicitly via the deny fn.
        for denied in [
            "MEMORY.md",
            "HEARTBEAT.md",
            "AGENTS.md",
            "BOOTSTRAP.md",
            "context/assistant-directives.md",
            "context/profile.json",
            "lorebooks/core.json",
            "daily/2026-10-10.md",
            "daily/2026-10-10.poisoned-1.md",
            "backups/MEMORY.md.bak",
            "engine/projects/p/missions/m/mission.json",
        ] {
            assert!(
                write_back_deny_reason(denied).is_some(),
                "'{denied}' must be hard-denied"
            );
        }
    }

    #[test]
    fn promotion_rejects_whitespace_aliased_staged_names() {
        // CW-2: a staged name whose segments carry leading/trailing
        // whitespace normalizes onto a denied target; it must be rejected on
        // the raw path, before any workspace write can trim it.
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let dir = tmp.path().join("projects");
        std::fs::create_dir_all(&dir).unwrap(); // safety: test
        std::fs::write(dir.join(" SOUL.md"), "aliased identity").unwrap(); // safety: test
        let plan = plan_promotion(tmp.path(), &WriteBackPolicy::default(), None).unwrap(); // safety: test
        assert!(plan.accepted.is_empty());
        assert_eq!(plan.rejections.len(), 1);
        assert!(plan.rejections[0].reason.contains("invalid staged path"));
    }

    #[test]
    fn promotion_rejects_secret_content_and_enforces_limits() {
        let tmp = staging_with(&[
            ("projects/a.md", "plain"),
            ("projects/leak.md", "aws key AKIAIOSFODNN7EXAMPLE here"),
        ]);
        let plan = plan_promotion(tmp.path(), &WriteBackPolicy::default(), None).unwrap(); // safety: test
        assert_eq!(plan.accepted.len(), 1);
        assert_eq!(plan.accepted[0].relative_path, "projects/a.md");
        assert!(
            plan.rejections
                .iter()
                .any(|r| r.relative_path == "projects/leak.md" && r.reason.contains("leak scan"))
        );

        let tmp2 = staging_with(&[("projects/big.md", &"x".repeat(300 * 1024))]);
        let plan2 = plan_promotion(tmp2.path(), &WriteBackPolicy::default(), None).unwrap(); // safety: test
        assert!(plan2.accepted.is_empty());
        assert!(plan2.rejections[0].reason.contains("per-file limit"));
    }

    #[cfg(unix)]
    #[test]
    fn promotion_rejects_oversized_sparse_file_before_reading() {
        // CW-6: the lstat size pre-check rejects oversized files before any
        // content is read into daemon memory.
        use std::io::Seek;
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let dir = tmp.path().join("projects");
        std::fs::create_dir_all(&dir).unwrap(); // safety: test
        let mut file = std::fs::File::create(dir.join("sparse.md")).unwrap(); // safety: test
        file.seek(std::io::SeekFrom::Start(512 * 1024)).unwrap(); // safety: test
        file.write_all(b"end").unwrap(); // safety: test
        drop(file);
        let plan = plan_promotion(tmp.path(), &WriteBackPolicy::default(), None).unwrap(); // safety: test
        assert!(plan.accepted.is_empty());
        assert!(plan.rejections[0].reason.contains("per-file limit"));
    }

    #[cfg(unix)]
    #[test]
    fn promotion_never_follows_symlinks() {
        // CW-3d: symlinks are invisible to the walk and unopenable to the
        // reader; a symlink pointing outside staging must not be promoted.
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let outside = tmp.path().join("outside.md");
        std::fs::write(&outside, "host secret-free content").unwrap(); // safety: test
        let staging = tmp.path().join("staging");
        let dir = staging.join("projects");
        std::fs::create_dir_all(&dir).unwrap(); // safety: test
        std::os::unix::fs::symlink(&outside, dir.join("link.md")).unwrap(); // safety: test
        std::os::unix::fs::symlink(tmp.path(), staging.join("projects_link")).unwrap(); // safety: test
        let plan = plan_promotion(&staging, &WriteBackPolicy::default(), None).unwrap(); // safety: test
        assert!(plan.accepted.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn promotion_rejects_fifos_without_hanging() {
        // CW-3d: a fifo must fail fast (O_NONBLOCK), never block the daemon.
        use std::os::unix::ffi::OsStrExt;
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let dir = tmp.path().join("projects");
        std::fs::create_dir_all(&dir).unwrap(); // safety: test
        let fifo = dir.join("pipe.md");
        let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap(); // safety: test
        let created = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) }; // safety: test fifo creation
        assert_eq!(created, 0);
        let plan = plan_promotion(tmp.path(), &WriteBackPolicy::default(), None).unwrap(); // safety: test
        assert!(plan.accepted.is_empty());
    }

    #[test]
    fn promotion_rejects_staged_content_embedding_mounted_secrets() {
        // CW-10: the job's own mounted secret values are scanned in addition
        // to pattern-based leak detection.
        let tmp = staging_with(&[
            ("projects/clean.md", "no secrets here"),
            ("projects/dirty.md", "copied: harness-promotion-fixture-value"),
        ]);
        let values = vec![SecretString::from(
            "harness-promotion-fixture-value".to_string(),
        )];
        let redactor = SecretValueRedactor::new(&values);
        let plan =
            plan_promotion(tmp.path(), &WriteBackPolicy::default(), Some(&redactor)).unwrap(); // safety: test
        assert_eq!(plan.accepted.len(), 1);
        assert_eq!(plan.accepted[0].relative_path, "projects/clean.md");
        assert!(
            plan.rejections
                .iter()
                .any(|r| r.relative_path == "projects/dirty.md"
                    && r.reason.contains("worker secret"))
        );
    }

    #[test]
    fn promotion_ignores_hidden_and_missing_staging() {
        let tmp = staging_with(&[(".hidden", "ignored"), ("projects/ok.md", "fine")]);
        let plan = plan_promotion(tmp.path(), &WriteBackPolicy::default(), None).unwrap(); // safety: test
        assert_eq!(plan.accepted.len(), 1);

        let missing = tempfile::tempdir().unwrap(); // safety: test
        let plan = plan_promotion(
            &missing.path().join("does-not-exist"),
            &WriteBackPolicy::default(),
            None,
        )
        .unwrap(); // safety: test
        assert!(plan.accepted.is_empty());
        assert!(plan.rejections.is_empty());
    }

    #[test]
    fn promotion_rejects_non_utf8() {
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let dir = tmp.path().join("projects");
        std::fs::create_dir_all(&dir).unwrap(); // safety: test
        std::fs::write(dir.join("bin.md"), [0xff, 0xfe, 0xfd]).unwrap(); // safety: test
        let plan = plan_promotion(tmp.path(), &WriteBackPolicy::default(), None).unwrap(); // safety: test
        assert!(plan.accepted.is_empty());
        assert_eq!(plan.rejections[0].reason, "not valid UTF-8");
    }

    #[test]
    fn promotion_caps_walk_depth_and_rejection_list() {
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let mut deep = tmp.path().to_path_buf();
        for level in 0..10 {
            deep = deep.join(format!("d{level}"));
        }
        std::fs::create_dir_all(&deep).unwrap(); // safety: test
        std::fs::write(deep.join("leaf.md"), "too deep").unwrap(); // safety: test
        let result = plan_promotion(tmp.path(), &WriteBackPolicy::default(), None);
        assert!(result.is_err(), "depth cap must fail the plan");
    }

    #[test]
    fn redactor_scrubs_exact_base64_and_url_encoded_values() {
        use base64::Engine;
        let value = "harness-redactor-fixture-value";
        let values = vec![SecretString::from(value.to_string())];
        let redactor = SecretValueRedactor::new(&values);
        assert!(redactor.is_armed());

        let b64 = base64::engine::general_purpose::STANDARD.encode(value);
        let url = urlencoding::encode(value).into_owned();
        for variant in [value.to_string(), b64, url] {
            let scrubbed = redactor.scrub(&format!("prefix {variant} suffix"));
            assert!(!scrubbed.contains(&variant), "variant must be scrubbed");
            assert!(scrubbed.contains("[redacted-secret]"));
        }
        assert!(redactor.contains_secret(value));
        assert!(!redactor.contains_secret("unrelated content"));
    }

    #[test]
    fn redactor_skips_short_values() {
        let values = vec![SecretString::from("short".to_string())];
        let redactor = SecretValueRedactor::new(&values);
        assert!(!redactor.is_armed());
        assert_eq!(redactor.scrub("a short story"), "a short story");
    }

    #[test]
    fn redactor_scrubs_json_recursively() {
        let values = vec![SecretString::from("tok_1234567890abcdef".to_string())];
        let redactor = SecretValueRedactor::new(&values);
        let mut event = serde_json::json!({
            "output": "leaked tok_1234567890abcdef here",
            "nested": ["tok_1234567890abcdef"],
            "count": 3,
        });
        redactor.scrub_json(&mut event);
        let text = event.to_string();
        assert!(!text.contains("tok_1234567890abcdef"));
        assert_eq!(event["count"], 3);
    }

    #[test]
    fn redactor_scrubs_json_object_keys() {
        // DG-2: a secret used as a JSON KEY must not survive scrubbing.
        let values = vec![SecretString::from("tok_1234567890abcdef".to_string())];
        let redactor = SecretValueRedactor::new(&values);
        let mut event = serde_json::json!({ "tok_1234567890abcdef": "value" });
        redactor.scrub_json(&mut event);
        let text = event.to_string();
        assert!(!text.contains("tok_1234567890abcdef"));
        assert!(text.contains("[redacted-secret]"));
    }

    #[test]
    fn redactor_scrubs_wrapped_offset_urlsafe_hex_and_json_escaped() {
        // DG-2: the encodings a job can produce from a mounted secret.
        use base64::Engine;
        // 60 bytes: long enough for the 76-column base64 wrap to kick in.
        let value = "harness-fixture-value-with-enough-length-to-wrap-nicely-0";
        let values = vec![SecretString::from(value.to_string())];
        let redactor = SecretValueRedactor::new(&values);

        // base64 CLI default output wraps at 76 columns.
        let b64 = base64::engine::general_purpose::STANDARD.encode(value);
        let wrapped = b64
            .as_bytes()
            .chunks(76)
            .map(|c| std::str::from_utf8(c).unwrap()) // safety: test
            .collect::<Vec<_>>()
            .join("\n");
        // Offset base64: `curl -u user:$(cat T)` encodes "user:<value>".
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("user:{value}"));
        // URL-safe base64 and hex (`xxd -p`).
        let urlsafe = base64::engine::general_purpose::URL_SAFE.encode(value);
        let hex: String = value.bytes().map(|b| format!("{b:02x}")).collect();
        // JSON-escaped (value contains a quote as it appears in tool_use.input).
        let quoted_value = "harness-fixture-\"quoted\"-value";
        let quoted_redactor =
            SecretValueRedactor::new(&[SecretString::from(quoted_value.to_string())]);
        let json_escaped = serde_json::to_string(quoted_value).unwrap(); // safety: test
        let json_escaped = json_escaped
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap() // safety: test
            .to_string();

        for variant in [wrapped, basic, urlsafe, hex] {
            let scrubbed = redactor.scrub(&format!("prefix {variant} suffix"));
            assert!(
                !scrubbed.contains(&variant),
                "variant must be scrubbed: {variant}"
            );
        }
        let scrubbed = quoted_redactor.scrub(&format!("prefix {json_escaped} suffix"));
        assert!(!scrubbed.contains(&json_escaped));
    }

    #[test]
    fn redactor_scrubs_each_line_of_multiline_values() {
        let value = "-----BEGIN FIXTURE-----\nline-one-fixture-material\nline-two-fixture-material\n-----END FIXTURE-----";
        let redactor = SecretValueRedactor::new(&[SecretString::from(value.to_string())]);
        for line in [
            "line-one-fixture-material",
            "line-two-fixture-material",
        ] {
            let scrubbed = redactor.scrub(&format!("leaked {line} here"));
            assert!(!scrubbed.contains(line), "line must be scrubbed");
        }
    }

    #[test]
    fn scrub_and_truncate_redacts_before_and_at_the_cut() {
        // DG-2: scrubbing runs BEFORE truncation, so a value that straddles
        // the cut is already gone; a value truncated by an upstream step is
        // caught by the trailing partial-prefix redaction.
        let value = "harness-fixture-truncation-value";
        let redactor = SecretValueRedactor::new(&[SecretString::from(value.to_string())]);

        // Value straddling the cut: scrubbed out before truncation (the cut
        // may slice the marker itself — what matters is the value is gone).
        let text = format!("{}{}", "x".repeat(100), value);
        let out = redactor.scrub_and_truncate(&text, 110);
        assert!(!out.contains(value));
        assert!(!out.contains(&value[..20]));
        // Marker fully inside the limit survives whole.
        let text = format!("{}{}{}", "x".repeat(50), value, "y".repeat(100));
        let out = redactor.scrub_and_truncate(&text, 80);
        assert!(out.contains("[redacted-secret]"));

        // Value already truncated upstream (partial prefix at the cut). The
        // text must exceed the limit so the cut slices the partial value.
        let partial = &value[..20];
        let text = format!("{}{}", "y".repeat(80), partial);
        let out = redactor.scrub_and_truncate(&text, 95);
        assert!(!out.contains(partial));
        assert!(out.ends_with("[redacted-secret]..."));

        // Short tails (< MIN_REDACTED_SECRET_BYTES) are left alone.
        let tiny = &value[..6];
        let text = format!("{}{}", "z".repeat(80), tiny);
        let out = redactor.scrub_and_truncate(&text, 100);
        assert!(out.contains(tiny));

        // Under the limit: returned unchanged apart from scrubbing.
        let out = redactor.scrub_and_truncate("clean", 100);
        assert_eq!(out, "clean");
    }

    #[test]
    fn harness_dirs_live_outside_projects_base() {
        // CW-3: harness state must not sit under the bind-mountable projects
        // base, or any job with an explicit project_dir could mount it RW.
        let job_id = Uuid::new_v4();
        let projects_base = lunarwing_base_dir().join("projects");
        assert!(!harness_job_dir(job_id).starts_with(&projects_base));
        assert!(harness_job_dir(job_id).starts_with(harness_base_dir()));
        assert!(injection_dir(job_id).ends_with("injected"));
        assert!(staging_dir(job_id).ends_with("workspace_out"));
    }

    #[test]
    fn sweep_removes_only_dirs_for_non_live_jobs() {
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let live = Uuid::new_v4();
        let stale_a = Uuid::new_v4();
        let stale_b = Uuid::new_v4();
        for id in [live, stale_a, stale_b] {
            std::fs::create_dir_all(tmp.path().join(id.to_string())).unwrap(); // safety: test
        }

        // Zero grace: every non-live dir is old enough to sweep.
        let live_set = std::collections::HashSet::from([live]);
        let removed = sweep_harness_dirs_in(tmp.path(), &live_set, std::time::Duration::ZERO);
        assert_eq!(removed, 2);
        assert!(tmp.path().join(live.to_string()).exists());
        assert!(!tmp.path().join(stale_a.to_string()).exists());
        assert!(!tmp.path().join(stale_b.to_string()).exists());

        // Huge grace: freshly created dirs are all kept.
        for id in [stale_a, stale_b] {
            std::fs::create_dir_all(tmp.path().join(id.to_string())).unwrap(); // safety: test
        }
        let removed = sweep_harness_dirs_in(
            tmp.path(),
            &std::collections::HashSet::new(),
            std::time::Duration::from_secs(86_400),
        );
        assert_eq!(removed, 0);
        assert!(tmp.path().join(stale_a.to_string()).exists());
    }
}
