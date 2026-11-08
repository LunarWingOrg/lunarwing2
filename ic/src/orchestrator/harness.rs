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
/// Built from the job's resolved secret values; scrubs selected encodings
/// of a value from job events and completion output before they are persisted
/// or broadcast, and reused by the promotion pipeline to reject staged files
/// that embed a mounted secret (CW-10).
///
/// Examples of covered variants: raw text; lines of a multi-line value;
/// JSON-escaped and URL-encoded forms; lowercase/uppercase hex and base64 in
/// the standard and URL-safe alphabets (padded and unpadded); and the
/// stream-stable base64 cores for a value encoded at byte offsets 1 and 2
/// inside a larger blob (the `Authorization: Basic` shape). Encoded forms are
/// matched against the text with line breaks folded out (both real newlines
/// and the JSON-escaped `\n`/`\r` two-character forms), so ANY CLI wrap width
/// is covered — the `base64` 76-column default, `openssl base64` /
/// `base64 -w 64`, `xxd -p` at 60 columns, etc. (EB-3). The raw value is
/// fold-matched too, so `fold -w N` of an unencoded token is caught (EE-8).
/// Every folded and direct match is collected before replacement and merged
/// in one sweep, so a second value sharing a substring cannot leave its
/// non-overlapping tail behind (EE-9). Multi-line values are also armed one
/// line at a time, except lines that carry no key material — armor delimiters
/// and lines with no alphanumeric byte (DZ-5). For secrets whose encoded form
/// is at most [`WRAP_LINE_VARIANT_CHAR_CAP`] chars, individual wrap lines at
/// the canonical widths are armed as well (a tool returning one line at a
/// time); larger and truncated encodings use aligned chunks in the same
/// per-job folded automaton (EB-4/EE-2).
///
/// NOT covered (documented gaps): offset/grouped dump formats such as plain
/// `xxd`, `od`, or `hexdump -C`, which interleave offsets and ASCII columns
/// and never contain a contiguous encoded span; base32 and other alphabets;
/// `rev` and similar reorderings.
///
/// Redaction is BEST-EFFORT, not a security boundary: a sub-agent that
/// deliberately slices a value (e.g. `cut -c1-32` over several calls, or an
/// encoding not covered above) can smuggle partials past any string matcher.
/// The real control is the `Always` approval tier on harness secret mounts —
/// redaction limits accidental exposure of whole values.
///
/// Retained variants are held as [`SecretString`] and zeroize on drop (CV-10);
/// automaton tables, temporary strings, and serialized copies do not all
/// zeroize. `Debug` deliberately prints no variant material.
#[derive(Clone, Default)]
pub struct SecretValueRedactor {
    /// Variants matched directly against the text: the raw value, per-line
    /// splits of multi-line values, JSON-escaped and URL-encoded forms, and
    /// capped per-width wrap lines.
    direct_variants: Vec<SecretString>,
    /// Variants matched against the text with line breaks folded out:
    /// contiguous ASCII encodings (hex lower/upper, base64 standard and
    /// URL-safe plus offset cores), aligned partial chunks, and the raw value
    /// itself (EE-8).
    folded_variants: Vec<SecretString>,
    direct_matcher: Option<aho_corasick::AhoCorasick>,
    folded_matcher: Option<aho_corasick::AhoCorasick>,
    /// Longest variant length in bytes; bounds the truncation-cut scan.
    max_variant_bytes: usize,
}

impl std::fmt::Debug for SecretValueRedactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretValueRedactor")
            .field(
                "armed_variants",
                &(self.direct_variants.len() + self.folded_variants.len()),
            )
            .finish()
    }
}

/// Column width of the default (wrapped) output of the `base64` CLI.
const BASE64_CLI_WRAP_COLS: usize = 76;

/// Column width of plain hex output from `xxd -p` (30 input bytes).
const HEX_CLI_WRAP_COLS: usize = 60;

/// Stable base64 quantum-aligned chunk used to catch partial encoded output.
const BASE64_ALIGNED_CHUNK_CHARS: usize = 48;
/// Stable byte-aligned chunk used to catch partial hexadecimal output.
const HEX_ALIGNED_CHUNK_CHARS: usize = 32;

/// Cap on the encoded length for which individual wrap lines are armed as
/// direct variants (EB-4). Larger and truncated encodings are covered by
/// aligned chunks in the width-independent fold pass.
const WRAP_LINE_VARIANT_CHAR_CAP: usize = 4096;

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

fn push_json_string_variant(
    variants: &mut Vec<SecretString>,
    seen: &mut std::collections::HashSet<u64>,
    raw: &str,
) {
    if let Ok(json_quoted) = serde_json::to_string(raw) {
        let escaped = json_quoted
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(json_quoted.as_str())
            .to_string();
        push_variant(variants, seen, escaped);
    }
}

/// Arm individual wrap lines at one canonical width as direct variants, so a
/// tool returning a single wrapped line at a time is still caught.
fn push_wrap_line_variants(
    variants: &mut Vec<SecretString>,
    seen: &mut std::collections::HashSet<u64>,
    encoded: &str,
    width: usize,
) {
    if encoded.len() <= width || encoded.len() > WRAP_LINE_VARIANT_CHAR_CAP {
        return;
    }
    // All callers pass ASCII encodings, so the column boundaries are UTF-8 safe.
    for chunk in encoded.as_bytes().chunks(width) {
        if let Ok(line) = std::str::from_utf8(chunk) {
            push_variant(variants, seen, line.to_string());
        }
    }
}

/// Arm fixed aligned chunks so truncated prints and non-canonical wrap widths
/// still match without a per-line/per-chunk search loop (EE-2).
fn push_aligned_chunk_variants(
    variants: &mut Vec<SecretString>,
    seen: &mut std::collections::HashSet<u64>,
    encoded: &str,
    width: usize,
) {
    for chunk in encoded.as_bytes().chunks(width) {
        if let Ok(chunk) = std::str::from_utf8(chunk) {
            push_variant(variants, seen, chunk.to_string());
        }
    }
}

/// Insert a base64 encoding in padded and unpadded form, plus the
/// stream-stable core with the final quantum dropped (the last 4 chars can
/// mix with suffix bytes when the value sits inside a larger encoded blob).
/// Dedup is per-list (EE-3): an exactly-80-char encoding has a core equal to
/// its 76-column line piece, and sharing one set would drop the core from
/// the fold list — unwrapping the leak the line piece exists to stop.
fn push_base64_variants(
    direct: &mut Vec<SecretString>,
    folded: &mut Vec<SecretString>,
    seen_direct: &mut std::collections::HashSet<u64>,
    seen_folded: &mut std::collections::HashSet<u64>,
    encoded: String,
) {
    push_wrap_line_variants(direct, seen_direct, &encoded, BASE64_CLI_WRAP_COLS);
    push_aligned_chunk_variants(folded, seen_folded, &encoded, BASE64_ALIGNED_CHUNK_CHARS);
    if encoded.len() >= MIN_REDACTED_SECRET_BYTES + 4 {
        let core = encoded[..encoded.len() - 4].to_string();
        push_variant(folded, seen_folded, core);
    }
    let unpadded = encoded.trim_end_matches('=').to_string();
    if unpadded != encoded {
        push_variant(folded, seen_folded, unpadded);
    }
    push_variant(folded, seen_folded, encoded);
}

/// True when a raw-value line carries no distinctive key material: an RFC 7468
/// armor delimiter (`-----BEGIN …-----` / `-----END …-----`) or a line with no
/// ASCII alphanumeric byte. Arming such lines as per-line variants over-redacts
/// whitespace-only and structural lines in unrelated files, which corrupts the
/// sub-agent's view and causes promotion rejections (DZ-5).
fn is_low_entropy_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with("-----BEGIN ") || trimmed.starts_with("-----END ") {
        return true;
    }
    !line.bytes().any(|b| b.is_ascii_alphanumeric())
}

/// Selected encodings of one secret value (DG-2), split into variants matched
/// directly against the text and variants matched with line breaks folded out
/// (EB-3/EB-4/EE-8). The two lists dedup independently (EE-3): wrap-line
/// pieces and folded cores can coincide (58-62 byte values), and a core
/// missing from the fold list is exactly the leak fold matching closes.
fn secret_variants(raw: &str) -> (Vec<SecretString>, Vec<SecretString>) {
    use base64::Engine;
    use secrecy::ExposeSecret;
    let mut direct = Vec::new();
    let mut folded = Vec::new();
    let mut seen_direct = std::collections::HashSet::new();
    let mut seen_folded = std::collections::HashSet::new();

    push_variant(&mut direct, &mut seen_direct, raw.to_string());
    // EE-8: the raw value is fold-matched too, so `fold -w N` or any tool that
    // inserts line breaks into an unencoded token is caught. The needle is
    // folded with the same rules as the text, so a multi-line value matches
    // its break-free concatenation as well.
    push_variant(&mut folded, &mut seen_folded, fold_line_breaks(raw).text);
    // Multi-line values (PEM blocks, service-account JSON) also leak one line
    // at a time. Lines with no distinctive key material are not armed on
    // their own (DZ-5).
    for line in raw.lines() {
        if is_low_entropy_line(line) {
            continue;
        }
        push_variant(&mut direct, &mut seen_direct, line.to_string());
        push_json_string_variant(&mut direct, &mut seen_direct, line);
        let trimmed = line.trim();
        if trimmed != line {
            push_variant(&mut direct, &mut seen_direct, trimmed.to_string());
            push_json_string_variant(&mut direct, &mut seen_direct, trimmed);
        }
    }
    push_json_string_variant(&mut direct, &mut seen_direct, raw);
    push_variant(
        &mut direct,
        &mut seen_direct,
        urlencoding::encode(raw).into_owned(),
    );
    // Plain hex, lowercase and `xxd -p -u` uppercase.
    let mut hex = String::with_capacity(raw.len() * 2);
    for byte in raw.as_bytes() {
        hex.push_str(&format!("{byte:02x}"));
    }
    for encoded in [hex.to_ascii_uppercase(), hex] {
        push_wrap_line_variants(&mut direct, &mut seen_direct, &encoded, HEX_CLI_WRAP_COLS);
        push_aligned_chunk_variants(
            &mut folded,
            &mut seen_folded,
            &encoded,
            HEX_ALIGNED_CHUNK_CHARS,
        );
        push_variant(&mut folded, &mut seen_folded, encoded);
    }

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
            &mut direct,
            &mut folded,
            &mut seen_direct,
            &mut seen_folded,
            base64::engine::general_purpose::STANDARD.encode(slice),
        );
        if start == 0 {
            push_base64_variants(
                &mut direct,
                &mut folded,
                &mut seen_direct,
                &mut seen_folded,
                base64::engine::general_purpose::URL_SAFE.encode(slice),
            );
        }
    }

    // Longest first so overlapping values redact fully.
    direct.sort_by_key(|v| std::cmp::Reverse(v.expose_secret().len()));
    folded.sort_by_key(|v| std::cmp::Reverse(v.expose_secret().len()));
    (direct, folded)
}

/// Text with every line break removed so wrapped encodings match however the
/// CLI wrapped them (EB-3). Both real newlines and the JSON-escaped `\n`/`\r`
/// two-character forms are folded; `map` translates folded byte offsets back
/// to the original byte offset of each folded char.
struct FoldedText {
    text: String,
    map: Vec<usize>,
}

fn fold_line_breaks(text: &str) -> FoldedText {
    let mut folded = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        match ch {
            '\n' | '\r' => {}
            '\\' => {
                if matches!(chars.peek(), Some(&(_, 'n' | 'r'))) {
                    chars.next();
                } else {
                    push_mapped(&mut folded, &mut map, idx, ch);
                }
            }
            _ => push_mapped(&mut folded, &mut map, idx, ch),
        }
    }
    FoldedText { text: folded, map }
}

fn push_mapped(folded: &mut String, map: &mut Vec<usize>, orig_idx: usize, ch: char) {
    // One map entry per folded BYTE, each holding the ORIGINAL start offset of
    // the char it belongs to. Variants may contain non-ASCII text (the raw
    // value is fold-matched, EE-8), so a match end can land on a continuation
    // entry; callers resolve the original end with `char_end_at` instead of
    // adding one.
    for _ in 0..ch.len_utf8() {
        map.push(orig_idx);
    }
    folded.push(ch);
}

/// Original-text end offset of the char starting at `start`, which must be a
/// char boundary. The folded map records char START offsets only, and a folded
/// match can end on a multi-byte char (EE-8), so `map[end - 1] + 1` can land
/// inside that char and panic the replacement sweep (TP-1).
fn char_end_at(text: &str, start: usize) -> usize {
    text[start..]
        .chars()
        .next()
        .map_or(text.len(), |ch| start + ch.len_utf8())
}

/// Replace every range in `ranges` with the marker in one sweep. Overlapping
/// ranges merge (the cursor advances to the furthest end) so the
/// non-overlapping tail of a second value sharing a substring cannot survive
/// the redaction (EE-9). Breaks inside a matched span are swallowed; breaks
/// before or after it survive.
fn replace_ranges(text: &str, mut ranges: Vec<(usize, usize)>) -> String {
    ranges.sort_unstable_by_key(|&(start, end)| (start, std::cmp::Reverse(end)));
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for (start, end) in ranges {
        if start < cursor {
            cursor = cursor.max(end);
            continue;
        }
        out.push_str(&text[cursor..start]);
        out.push_str("[redacted-secret]");
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

fn build_variant_matcher(variants: &[SecretString]) -> Option<aho_corasick::AhoCorasick> {
    use secrecy::ExposeSecret;
    let short: Vec<&[u8]> = variants
        .iter()
        .filter(|variant| variant.expose_secret().len() <= WRAP_LINE_VARIANT_CHAR_CAP)
        .map(|variant| variant.expose_secret().as_bytes())
        .collect();
    if short.is_empty() {
        return None;
    }
    match aho_corasick::AhoCorasick::new(short) {
        Ok(matcher) => Some(matcher),
        Err(error) => {
            tracing::warn!(
                %error,
                "Failed to build harness secret matcher; using sequential fallback"
            );
            None
        }
    }
}

/// Compact overlapping automaton matches while their end offsets are ordered.
/// A later long match may swallow several earlier ranges, so merge backwards
/// instead of storing every occurrence for the final sort.
fn collect_automaton_ranges(
    matches: impl IntoIterator<Item = (usize, usize)>,
) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for (mut start, mut end) in matches {
        while let Some(last) = ranges.last() {
            if start > last.1 {
                break;
            }
            start = start.min(last.0);
            end = end.max(last.1);
            ranges.pop();
        }
        ranges.push((start, end));
    }
    ranges
}

fn sequential_match_ranges<'a>(
    variants: impl IntoIterator<Item = &'a SecretString>,
    text: &str,
) -> Vec<(usize, usize)> {
    use secrecy::ExposeSecret;
    let mut ranges = Vec::new();
    for variant in variants {
        let needle = variant.expose_secret();
        let mut from = 0usize;
        while needle.len() <= text.len().saturating_sub(from) {
            let Some(pos) = text[from..].find(needle) else {
                break;
            };
            let start = from + pos;
            ranges.push((start, start + needle.len()));
            from = start + needle.len();
        }
    }
    ranges
}

fn long_sequential_match_ranges(variants: &[SecretString], text: &str) -> Vec<(usize, usize)> {
    use secrecy::ExposeSecret;
    sequential_match_ranges(
        variants
            .iter()
            .filter(|variant| variant.expose_secret().len() > WRAP_LINE_VARIANT_CHAR_CAP),
        text,
    )
}

fn variants_contain(variants: &[SecretString], text: &str) -> bool {
    use secrecy::ExposeSecret;
    variants
        .iter()
        .any(|variant| text.contains(variant.expose_secret()))
}

fn long_variants_contain(variants: &[SecretString], text: &str) -> bool {
    use secrecy::ExposeSecret;
    variants
        .iter()
        .filter(|variant| variant.expose_secret().len() > WRAP_LINE_VARIANT_CHAR_CAP)
        .any(|variant| text.contains(variant.expose_secret()))
}

const MAX_STRUCTURED_JSON_DEPTH: usize = 4;
const MAX_STRUCTURED_JSON_FIELDS: usize = 256;

fn collect_structured_json_fields(
    raw: &str,
    depth: usize,
    seen: &mut std::collections::HashSet<u64>,
    fields: &mut Vec<String>,
) {
    use std::hash::{Hash, Hasher};
    if depth >= MAX_STRUCTURED_JSON_DEPTH || fields.len() >= MAX_STRUCTURED_JSON_FIELDS {
        return;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return;
    };
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        if fields.len() >= MAX_STRUCTURED_JSON_FIELDS {
            return;
        }
        match value {
            serde_json::Value::String(field) => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                field.hash(&mut hasher);
                if seen.insert(hasher.finish()) {
                    collect_structured_json_fields(&field, depth + 1, seen, fields);
                    if fields.len() < MAX_STRUCTURED_JSON_FIELDS {
                        fields.push(field);
                    }
                }
            }
            serde_json::Value::Array(items) => pending.extend(items),
            serde_json::Value::Object(map) => pending.extend(map.into_values()),
            _ => {}
        }
    }
}

impl SecretValueRedactor {
    pub fn new(values: &[SecretString]) -> Self {
        use secrecy::ExposeSecret;
        let mut redactor = Self::default();
        for value in values {
            redactor.add_raw(value.expose_secret());
        }
        redactor.rebuild_matchers();
        redactor
    }

    /// Add one more value to an existing redactor (e.g. the worker bearer
    /// token, as defense in depth against accidental output — DG-7).
    pub fn add_value(&mut self, value: &str) {
        self.add_raw(value);
        self.rebuild_matchers();
    }

    fn extend_variants(&mut self, raw: &str) {
        if raw.len() < MIN_REDACTED_SECRET_BYTES {
            return;
        }
        let (direct, folded) = secret_variants(raw);
        self.direct_variants.extend(direct);
        self.folded_variants.extend(folded);
    }

    fn add_raw(&mut self, raw: &str) {
        use secrecy::ExposeSecret;
        self.extend_variants(raw);
        let mut fields = Vec::new();
        collect_structured_json_fields(raw, 0, &mut std::collections::HashSet::new(), &mut fields);
        for field in fields {
            if field != raw {
                self.extend_variants(&field);
            }
        }
        self.direct_variants
            .sort_by_key(|v| std::cmp::Reverse(v.expose_secret().len()));
        self.folded_variants
            .sort_by_key(|v| std::cmp::Reverse(v.expose_secret().len()));
        self.max_variant_bytes = self
            .direct_variants
            .iter()
            .chain(self.folded_variants.iter())
            .map(|v| v.expose_secret().len())
            .max()
            .unwrap_or(0);
    }

    fn rebuild_matchers(&mut self) {
        self.direct_matcher = build_variant_matcher(&self.direct_variants);
        self.folded_matcher = build_variant_matcher(&self.folded_variants);
    }

    pub fn is_armed(&self) -> bool {
        !self.direct_variants.is_empty() || !self.folded_variants.is_empty()
    }

    /// True when `text` contains any mounted secret value in a known encoding.
    pub fn contains_secret(&self, text: &str) -> bool {
        let direct_hit = self.direct_matcher.as_ref().map_or_else(
            || variants_contain(&self.direct_variants, text),
            |matcher| matcher.is_match(text) || long_variants_contain(&self.direct_variants, text),
        );
        if direct_hit || self.folded_variants.is_empty() {
            return direct_hit;
        }
        let folded = text
            .bytes()
            .any(|b| matches!(b, b'\n' | b'\r' | b'\\'))
            .then(|| fold_line_breaks(text));
        let haystack = folded.as_ref().map_or(text, |f| f.text.as_str());
        self.folded_matcher.as_ref().map_or_else(
            || variants_contain(&self.folded_variants, haystack),
            |matcher| {
                matcher.is_match(haystack) || long_variants_contain(&self.folded_variants, haystack)
            },
        )
    }

    pub fn scrub(&self, text: &str) -> String {
        let ranges = self.match_ranges(text);
        if ranges.is_empty() {
            return text.to_string();
        }
        replace_ranges(text, ranges)
    }

    /// Original-text byte ranges of every folded and direct variant match.
    /// Both passes are collected BEFORE any replacement, so a match that
    /// starts inside another match merges with it instead of being evaluated
    /// against already-replaced text — in either direction (EE-9).
    fn match_ranges(&self, text: &str) -> Vec<(usize, usize)> {
        let mut ranges = self.folded_match_ranges(text);
        if let Some(matcher) = &self.direct_matcher {
            ranges.extend(collect_automaton_ranges(
                matcher
                    .find_overlapping_iter(text)
                    .map(|found| (found.start(), found.end())),
            ));
            ranges.extend(long_sequential_match_ranges(&self.direct_variants, text));
        } else {
            ranges.extend(sequential_match_ranges(self.direct_variants.iter(), text));
        }
        ranges
    }

    /// Ranges of every folded-variant match, mapped back to original offsets.
    /// Line breaks are folded out for matching (EB-3/EB-4); when the text has
    /// none to fold, the text is searched as-is with an identity mapping.
    fn folded_match_ranges(&self, text: &str) -> Vec<(usize, usize)> {
        if self.folded_variants.is_empty() {
            return Vec::new();
        }
        let folded = text
            .bytes()
            .any(|b| matches!(b, b'\n' | b'\r' | b'\\'))
            .then(|| fold_line_breaks(text));
        let haystack = folded.as_ref().map_or(text, |f| f.text.as_str());
        let mut long_matches = long_sequential_match_ranges(&self.folded_variants, haystack);
        long_matches.sort_unstable_by_key(|&(start, end)| (end, start));
        let long_matches = collect_automaton_ranges(long_matches);
        if long_matches.is_empty() {
            return self.overlapping_folded_match_ranges(
                &folded,
                text,
                haystack,
                0,
                haystack.len(),
            );
        }

        let mut ranges = Vec::new();
        let mut cursor = 0usize;
        for (start, end) in long_matches {
            if cursor < start {
                let from = cursor.saturating_sub(WRAP_LINE_VARIANT_CHAR_CAP);
                let to = (start + WRAP_LINE_VARIANT_CHAR_CAP).min(haystack.len());
                ranges.extend(
                    self.overlapping_folded_match_ranges(&folded, text, haystack, from, to),
                );
            }
            ranges.push(Self::original_range(&folded, text, start, end));
            cursor = end;
        }
        if cursor < haystack.len() {
            let from = cursor.saturating_sub(WRAP_LINE_VARIANT_CHAR_CAP);
            ranges.extend(self.overlapping_folded_match_ranges(
                &folded,
                text,
                haystack,
                from,
                haystack.len(),
            ));
        }
        ranges
    }

    fn overlapping_folded_match_ranges(
        &self,
        folded: &Option<FoldedText>,
        text: &str,
        haystack: &str,
        start: usize,
        end: usize,
    ) -> Vec<(usize, usize)> {
        let start = crate::util::floor_char_boundary(haystack, start);
        let mut end = end.min(haystack.len());
        while end < haystack.len() && !haystack.is_char_boundary(end) {
            end += 1;
        }
        let slice = &haystack[start..end];
        if let Some(matcher) = &self.folded_matcher {
            return collect_automaton_ranges(
                matcher
                    .find_overlapping_iter(slice)
                    .map(|found| (start + found.start(), start + found.end())),
            )
            .into_iter()
            .map(|(start, end)| Self::original_range(folded, text, start, end))
            .collect();
        }
        use secrecy::ExposeSecret;
        sequential_match_ranges(
            self.folded_variants
                .iter()
                .filter(|variant| variant.expose_secret().len() <= WRAP_LINE_VARIANT_CHAR_CAP),
            slice,
        )
        .into_iter()
        .map(|(found_start, found_end)| {
            Self::original_range(folded, text, start + found_start, start + found_end)
        })
        .collect()
    }

    fn original_range(
        folded: &Option<FoldedText>,
        text: &str,
        start: usize,
        end: usize,
    ) -> (usize, usize) {
        match folded {
            Some(f) => (f.map[start], char_end_at(text, f.map[end - 1])),
            None => (start, end),
        }
    }

    /// Scrub before this helper's truncation (DG-2). A variant prefix (≥
    /// [`MIN_REDACTED_SECRET_BYTES`] bytes) at this cut is redacted as well.
    /// This does not recover fragments at earlier, internal tool-output cuts.
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
        for variant in self
            .direct_variants
            .iter()
            .chain(self.folded_variants.iter())
        {
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
    /// True when promotion did not run or stopped before every accepted write
    /// completed; `reason` says why. Records completed before a deadline remain.
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

/// Scrub every mounted secret value out of a completed promotion summary
/// (DZ-6). Accepted promotions already pass the content and now the path scan,
/// but a structured leak detector gap, a hostile reason string, or a value
/// that slipped past a best-effort scan must not survive in the audit event or
/// the `wait=true` result surfaced to the parent. Redacts record/rejection
/// paths and reasons, plus the skip reason.
pub fn scrub_promotion_summary(summary: &mut PromotionSummary, redactor: &SecretValueRedactor) {
    if !redactor.is_armed() {
        return;
    }
    for record in &mut summary.records {
        record.relative_path = redactor.scrub(&record.relative_path);
        if let Some(backup) = &mut record.backup_path {
            *backup = redactor.scrub(backup);
        }
    }
    for rejection in &mut summary.rejections {
        rejection.relative_path = redactor.scrub(&rejection.relative_path);
        rejection.reason = redactor.scrub(&rejection.reason);
    }
    if let Some(reason) = &mut summary.skip_reason {
        *reason = redactor.scrub(reason);
    }
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
            Err(reason) => {
                // DZ-6: rejection paths are container-controlled strings that
                // land verbatim in the `harness_write_back` audit event and
                // the `wait=true` result. Scrub the recorded path against the
                // job's own mounted values at the source — a file NAMED after
                // a secret must not re-leak it through its rejection record.
                let display = match redactor {
                    Some(r) => r.scrub(&relative),
                    None => relative,
                };
                plan.reject(display, reason);
            }
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
    // DZ-6: staged PATHS are as much an exfiltration channel as content — a
    // job can name a file after a mounted secret value, and that name lands
    // in the `harness_write_back` audit event and the `wait=true` result.
    // Reject before any read so the value never reaches the write set.
    if let Some(redactor) = input.redactor
        && redactor.contains_secret(&relative)
    {
        return Err("staged path embeds a mounted worker secret value".to_string());
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
            (
                "projects/dirty.md",
                "copied: harness-promotion-fixture-value",
            ),
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
    fn promotion_rejects_staged_paths_embedding_mounted_secrets() {
        // DZ-6: a staged file NAMED after a mounted secret value must be
        // rejected before the name reaches the write set — it would otherwise
        // land verbatim in the `harness_write_back` audit event and the
        // `wait=true` result. The rejection record must not re-leak the name.
        let secret = "harness-staged-path-fixture-value";
        let tmp = staging_with(&[
            ("projects/clean.md", "fine"),
            (format!("projects/{secret}.md").as_str(), "content"),
        ]);
        let redactor = SecretValueRedactor::new(&[SecretString::from(secret.to_string())]);
        let plan =
            plan_promotion(tmp.path(), &WriteBackPolicy::default(), Some(&redactor)).unwrap(); // safety: test
        assert_eq!(plan.accepted.len(), 1, "clean file only: {plan:?}");
        assert_eq!(plan.accepted[0].relative_path, "projects/clean.md");
        assert!(
            plan.rejections
                .iter()
                .any(|r| r.reason == "staged path embeds a mounted worker secret value"),
            "the secret-named staged file must be rejected by the path scan, got {plan:?}"
        );
        // The secret value must never appear in the recorded rejection.
        for rejection in &plan.rejections {
            assert!(
                !rejection.relative_path.contains(secret),
                "rejection path leaked the secret: {}",
                rejection.relative_path
            );
            assert!(
                !rejection.reason.contains(secret),
                "rejection reason leaked the secret: {}",
                rejection.reason
            );
        }
    }

    #[test]
    fn scrub_promotion_summary_redacts_leaked_paths_and_reasons() {
        // DZ-6 backstop: even if a value slips past the path/content scans
        // (best-effort redaction is not a boundary), the audit event and
        // `wait=true` result must not persist or surface it. The stored
        // summary is scrubbed in place before it is persisted or attached to
        // the handle.
        let secret = "harness-summary-scrub-fixture-value";
        let redactor = SecretValueRedactor::new(&[SecretString::from(secret.to_string())]);
        let mut summary = PromotionSummary {
            records: vec![PromotionRecord {
                relative_path: format!("projects/{secret}.md"),
                sha256: "deadbeef".to_string(),
                previous_sha256: None,
                backup_path: Some(format!("backups/harness/x/{secret}.md")),
                size_bytes: 4,
                job_id: Uuid::new_v4(),
                owner_id: "owner".to_string(),
                promoted_at: Utc::now(),
            }],
            rejections: vec![PromotionRejection {
                relative_path: format!("projects/{secret}-other.md"),
                reason: format!("rejected because {secret}"),
            }],
            dropped_rejections: 0,
            skipped: true,
            skip_reason: Some(format!("skipped over {secret}")),
        };
        scrub_promotion_summary(&mut summary, &redactor);
        let serialized = serde_json::to_string(&summary).unwrap(); // safety: test
        assert!(
            !serialized.contains(secret),
            "scrubbed summary still carries the secret: {serialized}"
        );
        assert_eq!(summary.records[0].owner_id, "owner");
        assert!(serialized.contains("[redacted-secret]"));
    }

    #[test]
    fn scrub_promotion_summary_is_a_noop_for_unarmed_redactors() {
        let mut summary = PromotionSummary {
            skipped: true,
            skip_reason: Some("plain reason".to_string()),
            ..Default::default()
        };
        let redactor = SecretValueRedactor::new(&[]);
        scrub_promotion_summary(&mut summary, &redactor);
        assert_eq!(summary.skip_reason.as_deref(), Some("plain reason"));
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
        assert!(redactor.direct_matcher.is_some());
        assert!(redactor.folded_matcher.is_some());

        let b64 = base64::engine::general_purpose::STANDARD.encode(value);
        let url = urlencoding::encode(value).into_owned();
        for variant in [value.to_string(), b64.clone(), url] {
            let scrubbed = redactor.scrub(&format!("prefix {variant} suffix"));
            assert!(!scrubbed.contains(&variant), "variant must be scrubbed");
            assert!(scrubbed.contains("[redacted-secret]"));
        }
        assert!(redactor.contains_secret(value));
        // An encoded variant in fold-free text must trip the fast path too
        // (CW-10 promotion scanning relies on contains_secret for encodings).
        assert!(redactor.contains_secret(&format!("noise {b64} noise")));
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
    fn redactor_scrubs_decoded_structured_json_fields() {
        let private_key = "-----BEGIN PRIVATE KEY-----\nfixture-extracted-private-key-material-0123456789\n-----END PRIVATE KEY-----";
        let nested = serde_json::to_string(&serde_json::json!({
            "api_token": "nested-extracted-token-0123456789"
        }))
        .unwrap(); // safety: test
        let mounted = serde_json::to_string(&serde_json::json!({
            "private_key": private_key,
            "nested": nested,
        }))
        .unwrap(); // safety: test
        let redactor = SecretValueRedactor::new(&[SecretString::from(mounted)]);

        for decoded in [
            private_key,
            "fixture-extracted-private-key-material-0123456789",
            "nested-extracted-token-0123456789",
        ] {
            let scrubbed = redactor.scrub(&format!("jq output:\n{decoded}"));
            assert!(
                !scrubbed.contains(decoded),
                "decoded structured field leaked: {decoded}"
            );
            assert!(scrubbed.contains("[redacted-secret]"));
        }
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
        let value = "harness-fixture-value-with-enough-length-to-wrap-nicely-at-least-61-bytes";
        assert!(value.len() >= 61);
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
        assert!(wrapped.contains('\n'));
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
    fn redactor_scrubs_wrapped_cli_output_in_serialized_shell_results() {
        use base64::Engine;

        let value = "harness-fixture-with-at-least-61-bytes-for-real-shell-output-wrapping";
        assert!(value.len() >= 61);
        let redactor = SecretValueRedactor::new(&[SecretString::from(value.to_string())]);
        let hex: String = value.bytes().map(|byte| format!("{byte:02x}")).collect();
        for (encoded, width) in [
            (base64::engine::general_purpose::STANDARD.encode(value), 76),
            (hex.to_ascii_uppercase(), 60),
            (hex, 60),
        ] {
            let lines = encoded
                .as_bytes()
                .chunks(width)
                .map(|line| std::str::from_utf8(line).unwrap())
                .collect::<Vec<_>>();
            assert!(lines.len() > 1);
            let output = format!("{}\n", lines.join("\n"));
            // ShellTool result -> execute_tool_simple -> worker boundary.
            let result = serde_json::json!({
                "output": output, "exit_code": 0, "sandboxed": false, "truncated": false,
                "success": true, "warning": null,
            });
            let serialized = serde_json::to_string_pretty(&result).unwrap();
            assert!(serialized.contains("\\n"));
            let scrubbed = redactor.scrub(&serialized);
            let cleaned: serde_json::Value = serde_json::from_str(&scrubbed).unwrap();
            assert_eq!(cleaned["output"], "[redacted-secret]\n");
            assert_eq!(cleaned["exit_code"], 0);
            assert_eq!(redactor.scrub(&output), "[redacted-secret]\n");
            for line in lines {
                if line.len() >= MIN_REDACTED_SECRET_BYTES {
                    assert_eq!(redactor.scrub(line), "[redacted-secret]");
                }
            }
        }
    }

    #[test]
    fn redactor_fold_matching_covers_any_wrap_width() {
        // EB-3: `openssl base64` (== `base64 -w 64`) and other non-76-column
        // widths must scrub whole via fold matching, both as raw CLI output
        // and inside a JSON-serialized tool result (escaped newlines).
        use base64::Engine;
        let value = "harness-fixture-with-at-least-61-bytes-for-real-shell-output-wrapping";
        let redactor = SecretValueRedactor::new(&[SecretString::from(value.to_string())]);
        let b64 = base64::engine::general_purpose::STANDARD.encode(value);
        let hex: String = value.bytes().map(|b| format!("{b:02x}")).collect();
        for (encoded, width) in [(b64.clone(), 64usize), (b64, 30), (hex, 48)] {
            let wrapped = encoded
                .as_bytes()
                .chunks(width)
                .map(|c| std::str::from_utf8(c).unwrap()) // safety: test
                .collect::<Vec<_>>()
                .join("\n");
            let scrubbed = redactor.scrub(&format!("prefix\n{wrapped}\nsuffix"));
            assert!(
                !scrubbed.contains(&encoded),
                "{width}-col wrap must fold-scrub"
            );
            for line in wrapped.lines() {
                assert!(
                    !scrubbed.contains(line),
                    "no {width}-col line may survive: {line}"
                );
            }
            let result = serde_json::json!({ "output": wrapped });
            let serialized = serde_json::to_string_pretty(&result).unwrap(); // safety: test
            let scrubbed = redactor.scrub(&serialized);
            let cleaned: serde_json::Value = serde_json::from_str(&scrubbed).unwrap(); // safety: test
            assert_eq!(cleaned["output"], "[redacted-secret]");
        }
    }

    #[test]
    fn redactor_fold_matching_scrubs_large_secrets_with_aligned_chunks() {
        use base64::Engine;
        let value = "V".repeat(MAX_SECRET_BYTES);
        let redactor = SecretValueRedactor::new([SecretString::from(value.clone())].as_slice());
        assert!(redactor.folded_matcher.is_some());
        let b64 = base64::engine::general_purpose::STANDARD.encode(&value);
        for width in [64usize, 76] {
            let wrapped = b64
                .as_bytes()
                .chunks(width)
                .map(|c| std::str::from_utf8(c).unwrap()) // safety: test
                .collect::<Vec<_>>()
                .join("\n");
            let scrubbed = redactor.scrub(&wrapped);
            assert!(
                !scrubbed.contains(&b64[..64]),
                "{width}-col wrap of a large secret must fold-scrub"
            );
        }

        let varying: String = (0..5000u32)
            .map(|i| char::from_digit(i % 10, 10).unwrap())
            .collect(); // safety: test
        let redactor = SecretValueRedactor::new([SecretString::from(varying.clone())].as_slice());
        assert!(redactor.direct_variants.len() < 256);
        assert!(redactor.folded_variants.len() < 20_000);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&varying);
        assert!(b64.len() > WRAP_LINE_VARIANT_CHAR_CAP);
        let first_line = std::str::from_utf8(&b64.as_bytes()[..76]).unwrap(); // safety: test
        assert!(redactor.contains_secret(first_line));

        let partial = b64.as_bytes()[..320]
            .chunks(64)
            .map(|c| std::str::from_utf8(c).unwrap()) // safety: test
            .collect::<Vec<_>>()
            .join("\n");
        let scrubbed = redactor.scrub(&partial);
        let folded_scrubbed = fold_line_breaks(&scrubbed).text;
        for chunk in b64.as_bytes()[..288].chunks(BASE64_ALIGNED_CHUNK_CHARS) {
            let chunk = std::str::from_utf8(chunk).unwrap(); // safety: test
            assert!(!folded_scrubbed.contains(chunk), "aligned chunk leaked");
        }
    }

    #[test]
    fn redactor_fold_matching_handles_multibyte_text_and_crlf() {
        // Folded-offset bookkeeping must stay correct when the text contains
        // multi-byte UTF-8 and CRLF/JSON-escaped-\r line breaks.
        use base64::Engine;
        let value = "harness-fixture-value-with-enough-length-to-wrap-nicely-at-least-61-bytes";
        let redactor = SecretValueRedactor::new([SecretString::from(value.to_string())].as_slice());
        let b64 = base64::engine::general_purpose::STANDARD.encode(value);
        let wrapped = b64
            .as_bytes()
            .chunks(40)
            .map(|c| std::str::from_utf8(c).unwrap()) // safety: test
            .collect::<Vec<_>>()
            .join("\r\n");
        let text = format!("émoji 🎠 prefix\r\n{wrapped}\r\n后缀");
        let scrubbed = redactor.scrub(&text);
        // Non-vacuous: the wrapped blob itself must be gone — the contiguous
        // encoding never appears in this text, so `!contains(&b64)` alone
        // would pass even if the folded pass redacted nothing.
        assert!(scrubbed.contains("[redacted-secret]"));
        assert!(!scrubbed.contains(&b64[..40]));
        for line in wrapped.lines() {
            assert!(!scrubbed.contains(line), "no wrapped line may survive");
        }
        assert!(!scrubbed.contains(&b64));
        assert!(scrubbed.contains("émoji 🎠 prefix"));
        assert!(scrubbed.contains("后缀"));
        assert!(redactor.contains_secret(&text));
        assert!(!redactor.contains_secret("émoji 🎠 only"));
    }

    #[test]
    fn redactor_fold_matching_covers_eighty_char_encoding_cores() {
        // EE-3 regression: an exactly-80-char base64 encoding (raw values of
        // 58-62 bytes at offset 0/1/2) has a stream-stable core equal to its
        // own 76-column wrap line. Sharing one dedup set between the direct
        // and folded lists dropped the core from the fold list, so a wrap
        // width other than 76 leaked the whole stable region.
        use base64::Engine;
        for raw_len in 58usize..=62 {
            let value: String = "fake-ee3-fixture-".chars().cycle().take(raw_len).collect();
            assert_eq!(value.len(), raw_len);
            let redactor = SecretValueRedactor::new([SecretString::from(value.clone())].as_slice());
            // Offset 0 with a suffix (`base64 -w64` of S+":"), and the value
            // embedded at byte offsets 1 and 2 inside a larger encoded blob.
            // The offset-1/2 blobs carry a trailing byte (`Z`) so the blob's
            // final quantum mixes suffix bytes and only the stream-stable
            // CORE of raw[2..]/raw[1..] can match — without it the padded
            // full encoding matches even when the core was deduplicated out
            // of the fold list (the old EE-3 shape), and the test passes
            // vacuously for those cells.
            let blobs = [
                base64::engine::general_purpose::STANDARD.encode(format!("{value}:")),
                base64::engine::general_purpose::URL_SAFE.encode(format!("{value}ZZ")),
                base64::engine::general_purpose::STANDARD.encode(format!("x{value}Z")),
                base64::engine::general_purpose::STANDARD.encode(format!("xy{value}Z")),
            ];
            for blob in blobs {
                let wrapped = blob
                    .as_bytes()
                    .chunks(64)
                    .map(|c| std::str::from_utf8(c).unwrap()) // safety: test
                    .collect::<Vec<_>>()
                    .join("\n");
                let scrubbed = redactor.scrub(&wrapped);
                // The stream-stable region (everything past the blob's first
                // quantum) must not survive contiguously.
                assert!(
                    !scrubbed.contains(&blob[16..64]),
                    "raw len {raw_len}: stable region leaked: {}",
                    &blob[16..64]
                );
            }
        }
    }

    #[test]
    fn redactor_folded_sweep_merges_overlapping_matches() {
        // EE-9: two mounted values whose encodings share a substring — a
        // match starting inside an already-redacted span must be MERGED (its
        // tail swallowed), not dropped whole, or the non-overlapping tail of
        // the second value survives.
        let shared = "shared-fixture-segment-0123456789";
        let left = format!("left-{shared}");
        let right = format!("{shared}-right-tail-fixture-bytes");
        let redactor = SecretValueRedactor::new(
            [
                SecretString::from(left.clone()),
                SecretString::from(right.clone()),
            ]
            .as_slice(),
        );
        // hex of the joined form contains hex(left) and hex(right) overlapped.
        let joined = format!("{left}-right-tail-fixture-bytes");
        let hex_joined: String = joined.bytes().map(|b| format!("{b:02x}")).collect();
        let hex_right: String = right.bytes().map(|b| format!("{b:02x}")).collect();
        // Force the folded path with an unrelated newline.
        let text = format!("header\n{hex_joined}");
        let scrubbed = redactor.scrub(&text);
        let tail = &hex_right[hex_right.len() - 32..];
        assert!(!scrubbed.contains(tail), "overlapping tail leaked: {tail}");
        assert!(!scrubbed.contains(&hex_joined[..64]));
    }

    #[test]
    fn redactor_fast_path_merges_overlapping_encodings() {
        // EE-9: fold-free text takes the identity-mapping path, which used to
        // replace variant by variant. A second value whose encoding overlaps
        // the first one loses its head to that replacement and never matches
        // again, so its non-overlapping head survived. Matches are now
        // collected first and merged, so the whole overlap is redacted.
        let shared = "shared-fixture-segment-0123456789";
        let head = "left-pad-fixture-padding-with-extra-bytes";
        let left = format!("{head}-{shared}");
        let right = format!("{shared}-right-tail-fixture-bytes");
        let redactor = SecretValueRedactor::new(
            [
                SecretString::from(left.clone()),
                SecretString::from(right.clone()),
            ]
            .as_slice(),
        );
        let hex_left: String = left.bytes().map(|b| format!("{b:02x}")).collect();
        let hex_right: String = right.bytes().map(|b| format!("{b:02x}")).collect();
        // Longest-first: hex(left) is replaced first and swallows the start of
        // hex(right), which begins inside it.
        assert!(hex_left.len() > hex_right.len(), "left must sort first");
        // hex(left + "-right-tail-fixture-bytes") contains both encodings,
        // hex_right starting inside hex_left; no line break, no backslash.
        let hex_text: String = format!("{left}-right-tail-fixture-bytes")
            .bytes()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(hex_text.contains(&hex_left) && hex_text.contains(&hex_right));
        let scrubbed = redactor.scrub(&hex_text);
        // Replacing hex_left first leaves the hex of right's non-overlapping
        // tail behind, so that tail is what leaks under the sequential sweep.
        let leaked_tail = &hex_right[hex_right.len() - 48..];
        assert!(
            !scrubbed.contains(leaked_tail),
            "non-overlapping tail of the shorter encoding leaked: {leaked_tail}"
        );
        assert_eq!(scrubbed, "[redacted-secret]");
    }

    #[test]
    fn redactor_merges_direct_and_folded_matches_across_passes() {
        // EE-9: a direct (raw) match that starts before, and extends into, a
        // folded (encoded) match used to be tested against the text AFTER the
        // folded pass replaced it, so its head survived. Both passes now
        // contribute ranges and the sweep merges them.
        use base64::Engine;
        let encoded_blob = "harness-fixture-ee9-encoded-tail-material-bytes";
        let b64 = base64::engine::general_purpose::STANDARD.encode(encoded_blob);
        assert!(b64.len() > 32);
        let prefix = "leak-prefix-fixture-material";
        let overlapping = format!("{prefix}{}", &b64[..20]);
        let redactor = SecretValueRedactor::new(
            [
                SecretString::from(encoded_blob.to_string()),
                SecretString::from(overlapping),
            ]
            .as_slice(),
        );
        // The raw variant matches contiguously at the head; the line break at
        // column 32 forces the folded pass over the encoding behind it.
        let wrapped = format!("{}\n{}", &b64[..32], &b64[32..]);
        let text = format!("{prefix}{wrapped}");
        let scrubbed = redactor.scrub(&text);
        assert!(
            !scrubbed.contains(prefix),
            "head of the overlapping raw value leaked: {scrubbed}"
        );
        assert_eq!(scrubbed, "[redacted-secret]");
    }

    #[test]
    fn redactor_fold_matches_reflowed_raw_value() {
        // EE-8: `fold -w N` (or any tool that re-wraps output) inserts line
        // breaks into an UNENCODED token. No per-line raw pieces are armed, so
        // only fold-matching the raw value catches it.
        let value = "harness-fixture-ee8-raw-value-with-no-encoding";
        assert!(value.len() >= MIN_REDACTED_SECRET_BYTES);
        let redactor = SecretValueRedactor::new([SecretString::from(value.to_string())].as_slice());
        let wrapped = value
            .as_bytes()
            .chunks(16)
            .map(|c| std::str::from_utf8(c).unwrap()) // safety: test
            .collect::<Vec<_>>()
            .join("\n");
        assert!(wrapped.lines().count() > 1);
        let text = format!("prefix\n{wrapped}\nsuffix");
        assert!(redactor.contains_secret(&text));
        let scrubbed = redactor.scrub(&text);
        assert!(!scrubbed.contains(value));
        for line in wrapped.lines() {
            assert!(!scrubbed.contains(line), "reflowed piece leaked: {line}");
        }
        assert!(scrubbed.contains("prefix"));
        assert!(scrubbed.contains("suffix"));
    }

    #[test]
    fn redactor_folded_raw_value_with_multibyte_tail_does_not_panic() {
        // TP-1: the folded map stores char START offsets. Once the raw value
        // is fold-matched (EE-8), a value ending in a multi-byte char puts a
        // match end inside that char; deriving the original end as
        // `map[end - 1] + 1` panics on the replacement slice.
        let value = "härness-fixture-pässwörd-€";
        assert!(value.ends_with('€'));
        let redactor = SecretValueRedactor::new([SecretString::from(value.to_string())].as_slice());
        for separator in ["\n", "\r\n"] {
            // The line break makes this the folded slow path.
            let text = format!("prefix{separator}{value}{separator}suffix");
            let scrubbed = redactor.scrub(&text);
            assert!(scrubbed.contains("[redacted-secret]"));
            assert!(!scrubbed.contains(value));
            assert!(!scrubbed.contains("pässwörd"));
            assert!(scrubbed.contains("prefix"));
            assert!(scrubbed.contains("suffix"));
        }
    }

    #[test]
    fn redactor_long_multibyte_raw_value_snaps_search_window_boundaries() {
        // WU-1: the short-variant search windows around a long raw match must
        // not slice through a multi-byte character at either computed margin.
        let value = "€".repeat(1734);
        assert!(value.len() > WRAP_LINE_VARIANT_CHAR_CAP);
        let redactor = SecretValueRedactor::new([SecretString::from(value.clone())].as_slice());
        let text = format!("prefix\n{value}\nsuffix");
        let scrubbed = redactor.scrub(&text);
        assert_eq!(scrubbed, "prefix\n[redacted-secret]\nsuffix");
    }

    #[test]
    fn redactor_dz5_skips_armor_and_punctuation_lines_when_arming() {
        // DZ-5: arming armor delimiters and punctuation-only lines redacted
        // unrelated files the sub-agent read. Asserted on the DIRECT variant
        // list, because the folded raw value legitimately contains the whole
        // multi-line text (so `contains_secret` on one of its lines is not a
        // statement about per-line arming).
        use secrecy::ExposeSecret;
        let value = "-----BEGIN FIXTURE-----\nslug-fixture-key-material\n:::---:::---:::----\n-----END FIXTURE-----";
        let redactor = SecretValueRedactor::new([SecretString::from(value.to_string())].as_slice());
        let armed_line = |line: &str| {
            redactor
                .direct_variants
                .iter()
                .any(|v| v.expose_secret() == line)
        };
        assert!(armed_line("slug-fixture-key-material"));
        for line in [
            "-----BEGIN FIXTURE-----",
            "-----END FIXTURE-----",
            ":::---:::---:::----",
        ] {
            assert!(
                !armed_line(line),
                "low-entropy line must not be armed on its own: {line}"
            );
        }
        // The value as a whole is still redacted, and its key material with it.
        assert_eq!(redactor.scrub(value), "[redacted-secret]");
        assert!(redactor.contains_secret("slug-fixture-key-material"));
    }

    #[test]
    fn redactor_compact_json_with_escaped_breaks_still_folds() {
        // Compact JSON carries line breaks ONLY as two-character `\n`/`\r`
        // escapes. The folded path must trigger on the backslash byte and
        // fold both escape forms, or wrapped encodings inside compactly
        // serialized tool results leak whole.
        use base64::Engine;
        let value = "harness-fixture-with-at-least-61-bytes-for-real-shell-output-wrapping";
        let redactor = SecretValueRedactor::new([SecretString::from(value.to_string())].as_slice());
        let b64 = base64::engine::general_purpose::STANDARD.encode(value);
        for separator in ["\n", "\r\n"] {
            let wrapped = b64
                .as_bytes()
                .chunks(64)
                .map(|c| std::str::from_utf8(c).unwrap()) // safety: test
                .collect::<Vec<_>>()
                .join(separator);
            // Compact serialization: no real newlines, only escaped ones.
            let serialized =
                serde_json::to_string(&serde_json::json!({ "output": wrapped })).unwrap(); // safety: test
            assert!(!serialized.contains('\n'));
            assert!(serialized.contains("\\n"));
            let scrubbed = redactor.scrub(&serialized);
            let cleaned: serde_json::Value = serde_json::from_str(&scrubbed).unwrap(); // safety: test
            assert_eq!(cleaned["output"], "[redacted-secret]");
        }
    }

    #[test]
    fn redactor_contains_secret_folds_compact_json_escapes() {
        // CW-10 promotion scanning relies on `contains_secret` catching
        // encoded variants. The fixture is 45 bytes, so its base64 is one
        // 60-char line: too short for direct wrap pieces (armed only past 76
        // columns), meaning ONLY the folded path can match once the encoding
        // is wrapped at 32 columns inside compact JSON. Kills two surviving
        // mutations at once: dropping the backslash byte from the fast-path
        // check (compact JSON has no real line breaks to trigger folding),
        // and not folding the escaped `\r` pair (the CRLF case).
        use base64::Engine;
        let value = "harness-fixture-short-enough-for-one-b64-line";
        assert_eq!(value.len(), 45);
        let redactor = SecretValueRedactor::new([SecretString::from(value.to_string())].as_slice());
        let b64 = base64::engine::general_purpose::STANDARD.encode(value);
        assert_eq!(b64.len(), 60);
        for separator in ["\n", "\r\n"] {
            let wrapped = b64
                .as_bytes()
                .chunks(32)
                .map(|c| std::str::from_utf8(c).unwrap()) // safety: test
                .collect::<Vec<_>>()
                .join(separator);
            let serialized =
                serde_json::to_string(&serde_json::json!({ "output": wrapped })).unwrap(); // safety: test
            assert!(!serialized.contains('\n'));
            assert!(
                redactor.contains_secret(&serialized),
                "wrapped base64 inside compact JSON must be detected ({separator:?} separator)"
            );
        }
    }

    #[test]
    fn redactor_new_scrubs_overlapping_values_longest_first() {
        let password = "harness-fixture-password";
        let url = format!("postgres://fixture:{password}@fixture.invalid/fixture");
        for values in [
            vec![
                SecretString::from(password.to_string()),
                SecretString::from(url.clone()),
            ],
            vec![
                SecretString::from(url.clone()),
                SecretString::from(password.to_string()),
            ],
        ] {
            let redactor = SecretValueRedactor::new(&values);
            assert_eq!(redactor.scrub(&url), "[redacted-secret]");
            assert_eq!(redactor.scrub(password), "[redacted-secret]");
        }
    }

    #[test]
    fn redactor_scrubs_each_line_of_multiline_values() {
        let value = "-----BEGIN FIXTURE-----\nline-one-fixture-material\nline-two-fixture-material\n-----END FIXTURE-----";
        let redactor = SecretValueRedactor::new(&[SecretString::from(value.to_string())]);
        for line in ["line-one-fixture-material", "line-two-fixture-material"] {
            let scrubbed = redactor.scrub(&format!("leaked {line} here"));
            assert!(!scrubbed.contains(line), "line must be scrubbed");
        }
    }

    #[test]
    fn redactor_preserves_json_when_a_secret_line_ends_in_backslash() {
        let line = "json-line-with-terminal-backslash\\";
        let value = format!("prefix-line-material\n{line}\nsuffix-line-material");
        let redactor = SecretValueRedactor::new([SecretString::from(value)].as_slice());
        let serialized = serde_json::to_string(&serde_json::json!({ "output": line })).unwrap(); // safety: test
        let scrubbed = redactor.scrub(&serialized);
        let cleaned: serde_json::Value = serde_json::from_str(&scrubbed).unwrap(); // safety: test
        assert_eq!(cleaned["output"], "[redacted-secret]");
        assert_eq!(redactor.scrub(line), "[redacted-secret]");
    }

    #[test]
    fn redactor_aligned_hex_chunks_cover_truncated_noncanonical_output() {
        let value: String = (0..5000u32)
            .map(|i| char::from_digit(i % 10, 10).unwrap())
            .collect(); // safety: test
        let redactor = SecretValueRedactor::new([SecretString::from(value.clone())].as_slice());
        let hex: String = value.bytes().map(|byte| format!("{byte:02x}")).collect();
        let partial = hex.as_bytes()[..160]
            .chunks(48)
            .map(|c| std::str::from_utf8(c).unwrap()) // safety: test
            .collect::<Vec<_>>()
            .join("\n");
        let scrubbed = redactor.scrub(&partial);
        let folded_scrubbed = fold_line_breaks(&scrubbed).text;
        for chunk in hex.as_bytes()[..128].chunks(HEX_ALIGNED_CHUNK_CHARS) {
            let chunk = std::str::from_utf8(chunk).unwrap(); // safety: test
            assert!(!folded_scrubbed.contains(chunk), "aligned hex chunk leaked");
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
