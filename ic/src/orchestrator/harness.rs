//! Harness job capabilities (WI#326).
//!
//! The conventional LunarWing harness runs as a Docker worker job with three
//! optional capability layers on top of the plain `worker` mode:
//!
//! 1. **tmpfs secret mounts** — worker-scoped secrets resolved host-side and
//!    delivered as files under `/run/secrets/<NAME>` on an in-memory tmpfs
//!    (Docker-secrets pattern). Values never enter the container env, the job
//!    context, logs, or job events.
//! 2. **Read-only file/memory injection** — selected tenant workspace
//!    documents are snapshotted into a per-job directory mounted read-only at
//!    `/harness/injected`. Every injected file is recorded in a
//!    `PROVENANCE.json` manifest (source path, SHA-256, size, timestamp).
//!    Identity files (`SOUL.md`, `USER.md`, `IDENTITY.md`) require explicit
//!    opt-in.
//! 3. **Staged write-back** — a separate writable staging dir mounted at
//!    `/harness/workspace_out`. On job completion the daemon promotes staged
//!    writes into tenant workspace memory only after path-allowlist, size,
//!    count, and secret-scan checks pass. Identity files, engine runtime
//!    state, and engine-internal paths are NEVER writable (WI#322).
//!
//! Boundary rule: the injection mount is one-way (read-only from the job's
//! perspective); the only way out is the promotion pipeline.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
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

const DEFAULT_WRITE_BACK_MAX_FILES: usize = 64;
const DEFAULT_WRITE_BACK_MAX_FILE_BYTES: u64 = 256 * 1024;
const DEFAULT_WRITE_BACK_MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024;
const MAX_STAGING_DEPTH: usize = 8;

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

/// File names that carry tenant identity and are never injected implicitly
/// and never writable through write-back promotion.
pub fn is_identity_file_name(file_name: &str) -> bool {
    matches!(file_name, "SOUL.md" | "USER.md" | "IDENTITY.md")
}

/// True when the workspace-relative path names an identity document.
pub fn is_identity_path(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_identity_file_name)
}

/// Validate a secret file name. File names double as env var names when the
/// worker exports them for child processes, so they must be env-var-safe.
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
    Ok(())
}

/// A worker-scoped secret requested for tmpfs file mounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFileMount {
    /// Name of the secret in the tenant secrets store.
    pub secret_name: String,
    /// File name under `/run/secrets/` (also the env var name the worker
    /// exports for child processes).
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

/// A secret value resolved host-side, ready to be archived into the tmpfs
/// mount. The value is never logged or serialized.
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

    pub fn value_bytes(&self) -> Vec<u8> {
        self.value.expose_secret().as_bytes().to_vec()
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
        validate_workspace_relative_path(&self.source)?;
        self.resolved_target_name()?;
        Ok(())
    }
}

/// Reject absolute paths, parent traversal, and empty segments.
fn validate_workspace_relative_path(path: &str) -> Result<(), HarnessError> {
    if path.trim().is_empty() {
        return Err(HarnessError::InvalidInjection(
            "source path cannot be empty".to_string(),
        ));
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(HarnessError::InvalidInjection(format!(
            "source path '{path}' must be workspace-relative"
        )));
    }
    for segment in path.split(['/', '\\']) {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(HarnessError::InvalidInjection(format!(
                "source path '{path}' contains an invalid segment"
            )));
        }
    }
    Ok(())
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
    /// Workspace-relative roots staged files may promote into
    /// (e.g. `["projects/"]`). Matched against the path relative to the
    /// staging dir.
    pub allowed_roots: Vec<String>,
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for WriteBackPolicy {
    fn default() -> Self {
        Self {
            allowed_roots: vec!["projects/".to_string()],
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
            if root.trim().is_empty() || root.starts_with('/') || root.contains("..") {
                return Err(HarnessError::InvalidPolicy(format!(
                    "allowed root '{root}' must be a relative path without '..'"
                )));
            }
        }
        if self.max_files == 0 || self.max_file_bytes == 0 || self.max_total_bytes == 0 {
            return Err(HarnessError::InvalidPolicy(
                "write-back limits must be non-zero".to_string(),
            ));
        }
        Ok(())
    }

    /// True when a staging-relative path falls under an allowed root.
    pub fn root_allows(&self, relative_path: &str) -> bool {
        self.allowed_roots.iter().any(|root| {
            let root = root.trim_end_matches('/');
            relative_path == root || relative_path.starts_with(&format!("{root}/"))
        })
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
        if self.worker_secrets.len() > MAX_SECRET_FILES {
            return Err(HarnessError::InvalidSecretMount(format!(
                "too many secret files ({} > {MAX_SECRET_FILES})",
                self.worker_secrets.len()
            )));
        }
        for mount in &self.worker_secrets {
            mount.validate()?;
        }
        if self.injections.len() > MAX_INJECTIONS {
            return Err(HarnessError::InvalidInjection(format!(
                "too many injections ({} > {MAX_INJECTIONS})",
                self.injections.len()
            )));
        }
        let mut targets = std::collections::HashSet::new();
        for spec in &self.injections {
            spec.validate()?;
            if !self.allow_identity_injection && is_identity_path(&spec.source) {
                return Err(HarnessError::InvalidInjection(format!(
                    "identity document '{}' requires allow_identity_injection=true",
                    spec.source
                )));
            }
            let target = spec.resolved_target_name()?;
            if target == INJECTION_MANIFEST_NAME || !targets.insert(target.clone()) {
                return Err(HarnessError::InvalidInjection(format!(
                    "duplicate or reserved injection target name '{target}'"
                )));
            }
        }
        if let Some(policy) = &self.write_back {
            policy.validate()?;
        }
        Ok(())
    }
}

/// Provenance record for one injected file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InjectedFileProvenance {
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
    pub source_path: String,
    pub target_name: String,
    pub content: String,
}

/// Write injected files (mode 0444) plus the provenance manifest into `dir`.
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
        let target = dir.join(&entry.target_name);
        std::fs::write(&target, bytes)
            .map_err(|e| HarnessError::io(format!("write {}", target.display()), e))?;
        set_readonly(&target)?;
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
    let manifest_path = dir.join(INJECTION_MANIFEST_NAME);
    std::fs::write(&manifest_path, manifest_json)
        .map_err(|e| HarnessError::io(format!("write {}", manifest_path.display()), e))?;
    set_readonly(&manifest_path)?;
    Ok(manifest)
}

#[cfg(unix)]
fn set_readonly(path: &Path) -> Result<(), HarnessError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444))
        .map_err(|e| HarnessError::io(format!("chmod {}", path.display()), e))
}

#[cfg(not(unix))]
fn set_readonly(path: &Path) -> Result<(), HarnessError> {
    let mut perms = std::fs::metadata(path)
        .map_err(|e| HarnessError::io(format!("stat {}", path.display()), e))?
        .permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(path, perms)
        .map_err(|e| HarnessError::io(format!("chmod {}", path.display()), e))
}

/// Build a tar archive of secret files for `upload_to_container` into the
/// tmpfs mount. Files are mode 0440 owned by the container worker uid.
pub fn build_secrets_tar(files: &[ResolvedSecretFile]) -> Result<Vec<u8>, HarnessError> {
    let mut builder = tar::Builder::new(Vec::new());
    for file in files {
        validate_secret_file_name(&file.file_name)?;
        let bytes = file.value_bytes();
        if bytes.len() > MAX_SECRET_BYTES {
            return Err(HarnessError::InvalidSecretMount(format!(
                "secret file '{}' exceeds {MAX_SECRET_BYTES} bytes",
                file.file_name
            )));
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o440);
        header.set_uid(1000);
        header.set_gid(1000);
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append_data(&mut header, &file.file_name, bytes.as_slice())
            .map_err(|e| HarnessError::io(format!("tar {}", file.file_name), e))?;
    }
    builder
        .into_inner()
        .map_err(|e| HarnessError::io("finish secrets tar", e))
}

/// Per-job harness host directory: `~/.lunarwing/projects/.harness/<job_id>`.
///
/// Lives under the projects base so the standard bind-mount validation
/// applies, but outside any single job workspace.
pub fn harness_job_dir(job_id: Uuid) -> PathBuf {
    lunarwing_base_dir()
        .join("projects")
        .join(".harness")
        .join(job_id.to_string())
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

/// One staged file that passed all promotion checks.
#[derive(Debug, Clone)]
pub struct StagedWrite {
    /// Workspace-relative target path (forward slashes).
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
}

/// Audit record for one promoted file (WI#326 provenance trail).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PromotionRecord {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub job_id: Uuid,
    pub promoted_at: DateTime<Utc>,
}

/// Outcome of a completed promotion run, stored on the job handle.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PromotionSummary {
    pub records: Vec<PromotionRecord>,
    pub rejections: Vec<PromotionRejection>,
}

/// Scan the staging dir and partition files into promotable writes and
/// rejections. Enforces the path allowlist, identity/engine/backup
/// exclusions, size and count limits, UTF-8, and the leak scan.
pub fn plan_promotion(
    staging: &Path,
    policy: &WriteBackPolicy,
) -> Result<PromotionPlan, HarnessError> {
    policy.validate()?;
    let mut files = Vec::new();
    if staging.is_dir() {
        collect_staged_files(staging, staging, 0, &mut files)?;
    }
    files.sort();

    let mut plan = PromotionPlan::default();
    let mut total_bytes = 0u64;
    let detector = crate::safety::LeakDetector::new();

    for path in files {
        if plan.accepted.len() >= policy.max_files {
            plan.rejections.push(PromotionRejection {
                relative_path: relative_display(staging, &path),
                reason: format!("write-back file count limit ({}) reached", policy.max_files),
            });
            continue;
        }
        let relative = relative_display(staging, &path);
        match screen_staged_file(&path, &relative, policy, &detector, &mut total_bytes) {
            Ok(write) => plan.accepted.push(write),
            Err(reason) => plan.rejections.push(PromotionRejection {
                relative_path: relative,
                reason,
            }),
        }
    }
    Ok(plan)
}

fn collect_staged_files(
    base: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<PathBuf>,
) -> Result<(), HarnessError> {
    if depth > MAX_STAGING_DEPTH {
        return Err(HarnessError::PromotionRejected(format!(
            "staging nesting exceeds {MAX_STAGING_DEPTH} levels under {}",
            base.display()
        )));
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|e| HarnessError::io(format!("read {}", dir.display()), e))?;
    for entry in entries {
        let entry = entry.map_err(|e| HarnessError::io("read dir entry", e))?;
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|e| HarnessError::io(format!("stat {}", path.display()), e))?;
        if metadata.is_dir() {
            collect_staged_files(base, &path, depth + 1, out)?;
        } else if metadata.is_file() {
            out.push(path);
        }
        // Symlinks and other special files are ignored entirely.
    }
    Ok(())
}

fn relative_display(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn screen_staged_file(
    path: &Path,
    relative: &str,
    policy: &WriteBackPolicy,
    detector: &crate::safety::LeakDetector,
    total_bytes: &mut u64,
) -> Result<StagedWrite, String> {
    if relative.split('/').any(|seg| seg.is_empty() || seg == "..") {
        return Err("path contains an invalid segment".to_string());
    }
    if is_identity_path(relative) {
        return Err("identity documents are never writable through write-back".to_string());
    }
    if crate::workspace::is_engine_runtime_path(relative)
        || crate::workspace::is_engine_internal_path(relative)
    {
        return Err("engine-managed paths are never writable through write-back".to_string());
    }
    if crate::workspace::is_backup_path(relative) {
        return Err("backup paths are not writable through write-back".to_string());
    }
    if !policy.root_allows(relative) {
        return Err(format!(
            "path is outside the allowed write-back roots ({})",
            policy.allowed_roots.join(", ")
        ));
    }

    let bytes = std::fs::read(path).map_err(|e| format!("read failed: {e}"))?;
    let size = bytes.len() as u64;
    if size > policy.max_file_bytes {
        return Err(format!(
            "file exceeds the {} byte per-file limit",
            policy.max_file_bytes
        ));
    }
    *total_bytes += size;
    if *total_bytes > policy.max_total_bytes {
        return Err(format!(
            "write-back total exceeds the {} byte limit",
            policy.max_total_bytes
        ));
    }
    let content = String::from_utf8(bytes).map_err(|_| "not valid UTF-8".to_string())?;
    let scan = detector.scan(&content);
    if !scan.is_clean() {
        return Err("secret pattern detected by leak scan".to_string());
    }
    Ok(StagedWrite {
        relative_path: relative.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        size_bytes: size,
        content,
    })
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
    fn identity_files_detected_by_name_at_any_depth() {
        assert!(is_identity_path("SOUL.md"));
        assert!(is_identity_path("docs/USER.md"));
        assert!(!is_identity_path("projects/x/SOUL.md.txt"));
        assert!(!is_identity_path("MEMORY.md"));
    }

    #[test]
    fn injection_specs_validate_and_resolve_targets() {
        let spec = InjectionSpec::new("projects/demo/notes.md");
        assert_eq!(spec.resolved_target_name().unwrap(), "notes.md");

        let renamed = InjectionSpec::new("MEMORY.md").with_target("memory_snapshot.md");
        assert_eq!(renamed.resolved_target_name().unwrap(), "memory_snapshot.md");

        assert!(InjectionSpec::new("../escape").validate().is_err());
        assert!(InjectionSpec::new("/absolute").validate().is_err());
        assert!(InjectionSpec::new("a//b").validate().is_err());
        assert!(InjectionSpec::new("ok.md").with_target("bad/name").validate().is_err());
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
    fn write_back_policy_defaults_and_validation() {
        let policy = WriteBackPolicy::default();
        assert!(policy.validate().is_ok());
        assert!(policy.root_allows("projects/x/result.md"));
        assert!(!policy.root_allows("MEMORY.md"));

        let mut bad = policy.clone();
        bad.allowed_roots = vec!["../escape/".to_string()];
        assert!(bad.validate().is_err());

        let mut empty = policy.clone();
        empty.allowed_roots = vec![];
        assert!(empty.validate().is_err());
    }

    #[test]
    fn secrets_tar_contains_files_with_restrictive_modes() {
        let files = vec![ResolvedSecretFile::new(
            "API_KEY",
            SecretString::from("s3cret".to_string()),
        )];
        let tar_bytes = build_secrets_tar(&files).unwrap();
        let mut archive = tar::Archive::new(tar_bytes.as_slice());
        let entries: Vec<_> = archive.entries().unwrap().collect();
        assert_eq!(entries.len(), 1);
        let entry = entries.into_iter().next().unwrap().unwrap();
        assert_eq!(entry.path().unwrap().to_string_lossy(), "API_KEY");
        assert_eq!(entry.header().mode().unwrap(), 0o440);
        assert_eq!(entry.header().size().unwrap(), 6);
    }

    #[test]
    fn secrets_tar_rejects_oversized_values() {
        let files = vec![ResolvedSecretFile::new(
            "BIG",
            SecretString::from("x".repeat(MAX_SECRET_BYTES + 1)),
        )];
        assert!(build_secrets_tar(&files).is_err());
    }

    #[test]
    fn injection_dir_writes_files_readonly_with_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("injected");
        let entries = vec![InjectionEntry {
            source_path: "MEMORY.md".to_string(),
            target_name: "memory.md".to_string(),
            content: "# Memory".to_string(),
        }];
        let job_id = Uuid::new_v4();
        let manifest = write_injection_dir(&dir, job_id, &entries).unwrap();

        assert_eq!(manifest.job_id, job_id);
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].source_path, "MEMORY.md");
        assert_eq!(manifest.files[0].sha256, sha256_hex(b"# Memory"));

        let written = std::fs::read_to_string(dir.join("memory.md")).unwrap();
        assert_eq!(written, "# Memory");
        let manifest_on_disk: InjectionManifest = serde_json::from_str(
            &std::fs::read_to_string(dir.join(INJECTION_MANIFEST_NAME)).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest_on_disk.files.len(), 1);
    }

    fn staging_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for (rel, content) in files {
            let path = tmp.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        tmp
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
        let plan = plan_promotion(tmp.path(), &WriteBackPolicy::default()).unwrap();

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
    fn promotion_rejects_secret_content_and_enforces_limits() {
        let tmp = staging_with(&[
            ("projects/a.md", "plain"),
            ("projects/leak.md", "aws key AKIAIOSFODNN7EXAMPLE here"),
        ]);
        let plan = plan_promotion(tmp.path(), &WriteBackPolicy::default()).unwrap();
        assert_eq!(plan.accepted.len(), 1);
        assert_eq!(plan.accepted[0].relative_path, "projects/a.md");
        assert!(
            plan.rejections
                .iter()
                .any(|r| r.relative_path == "projects/leak.md"
                    && r.reason.contains("leak scan"))
        );

        let tmp2 = staging_with(&[("projects/big.md", &"x".repeat(300 * 1024))]);
        let plan2 = plan_promotion(tmp2.path(), &WriteBackPolicy::default()).unwrap();
        assert!(plan2.accepted.is_empty());
        assert!(plan2.rejections[0].reason.contains("per-file limit"));
    }

    #[test]
    fn promotion_ignores_hidden_and_missing_staging() {
        let tmp = staging_with(&[(".hidden", "ignored"), ("projects/ok.md", "fine")]);
        let plan = plan_promotion(tmp.path(), &WriteBackPolicy::default()).unwrap();
        assert_eq!(plan.accepted.len(), 1);

        let missing = tempfile::tempdir().unwrap();
        let plan = plan_promotion(
            &missing.path().join("does-not-exist"),
            &WriteBackPolicy::default(),
        )
        .unwrap();
        assert!(plan.accepted.is_empty());
        assert!(plan.rejections.is_empty());
    }

    #[test]
    fn promotion_rejects_non_utf8() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("projects");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bin.md"), [0xff, 0xfe, 0xfd]).unwrap();
        let plan = plan_promotion(tmp.path(), &WriteBackPolicy::default()).unwrap();
        assert!(plan.accepted.is_empty());
        assert_eq!(plan.rejections[0].reason, "not valid UTF-8");
    }

    #[test]
    fn harness_dirs_live_under_projects_base() {
        let job_id = Uuid::new_v4();
        let base = lunarwing_base_dir().join("projects");
        assert!(harness_job_dir(job_id).starts_with(base.join(".harness")));
        assert!(injection_dir(job_id).ends_with("injected"));
        assert!(staging_dir(job_id).ends_with("workspace_out"));
    }
}
