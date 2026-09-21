//! Containment validation for the directory the generic sandbox bind-mounts.
//!
//! The generic sandbox mounts exactly one host directory into the container as
//! `/workspace` ([`crate::sandbox::cli_runner::build_run_args`]). That path is
//! chosen by the *agent* — it comes straight from the shell tool's `workdir`
//! argument — so it must be validated before it is handed to the runtime.
//!
//! Without validation a contained agent can mount the tenant's configuration
//! directory and read the plaintext gateway bearer token out of
//! `env/lunarwing.env`, which grants the full tenant-scoped gateway API surface
//! (voice, memory, logs, mission webhooks, SSE) from inside the lane that is
//! supposed to be isolated.
//!
//! Two complementary controls, because "deny" alone is not enough:
//!
//! 1. **Deny** — a mount whose root is a protected directory, or lives inside
//!    one, or whose own name is a credential-bearing file. There is no
//!    legitimate reason to mount the secret directory itself.
//! 2. **Mask** — a protected directory *inside* an otherwise legitimate mount
//!    (an agent working in its repo root, which contains `env/`) is shadowed
//!    by an empty directory bind. Denying that mount instead would break
//!    ordinary `git` work at the repository root; masking keeps the workflow
//!    and removes the secret.
//!
//! Everything is validated against canonicalized paths, so `..`, symlinks and
//! prefix confusion (`/home/u/envs` vs `/home/u/env`) resolve to their real
//! location before any check runs.
//!
//! ## Where the protected paths come from
//!
//! The daemon cannot infer the layout: in multi-tenant installs the env file
//! lives at `<tenant home>/lunarwing/env/lunarwing.env` while
//! `LUNARWING_BASE_DIR` is `<tenant home>/lunarwing/state` — a sibling, not a
//! parent. The tenant's own env directory is therefore supplied explicitly via
//! `SANDBOX_PROTECTED_PATHS` (mt-admin writes it during env generation), with
//! the base-dir-derived defaults as a fallback for single-node installs.

use std::path::{Component, Path, PathBuf};

use crate::sandbox::error::SandboxError;

/// Environment variable holding extra directories the sandbox may never
/// expose. `:`-separated, like `PATH`.
pub const PROTECTED_PATHS_ENV: &str = "SANDBOX_PROTECTED_PATHS";

/// Credential-bearing file names that must never be a mount target.
///
/// Matched case-insensitively on the final path component. The `*.env` suffix
/// is handled separately (see [`is_protected_file_name`]) so a copy under an
/// unusual name is still covered.
const PROTECTED_FILE_NAMES: &[&str] = &["pg.secret", ".env"];

/// Suffix marking an environment file; always credential-bearing.
const ENV_SUFFIX: &str = ".env";

/// Resolve the protected directory set for this process.
///
/// Explicit `SANDBOX_PROTECTED_PATHS` entries win; the base-dir-derived
/// defaults (`<base>/env`, `<base>`, and `<base>/.env`'s directory) are always
/// included because they cover the single-node layout, where the env file
/// genuinely is `<base>/.env`.
pub fn resolve_protected_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::env::var(PROTECTED_PATHS_ENV)
        .ok()
        .map(|raw| {
            raw.split(':')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default();

    let base = crate::bootstrap::lunarwing_base_dir();
    paths.push(base.join("env"));
    paths.push(base.clone());
    if let Ok(canonical) = base.canonicalize() {
        paths.push(canonical.join("env"));
        paths.push(canonical);
    }

    // Canonicalize what exists so prefix checks compare resolved paths; keep
    // unresolved entries for directories that may not exist yet.
    let mut resolved: Vec<PathBuf> = paths
        .into_iter()
        .map(|p| p.canonicalize().unwrap_or(p))
        .collect();
    resolved.sort();
    resolved.dedup();
    resolved
}

/// Canonicalize `dir` and reject it unless it is safe to bind-mount.
///
/// Returns the canonical path on success so the caller mounts the resolved
/// location rather than the literal argument.
pub fn validate_mount_dir(dir: &Path, protected: &[PathBuf]) -> Result<PathBuf, SandboxError> {
    if dir.as_os_str().is_empty() {
        return Err(SandboxError::Config {
            reason: "sandbox working directory must not be empty".to_string(),
        });
    }

    if !dir.is_absolute() {
        return Err(SandboxError::MountPathDenied {
            path: dir.display().to_string(),
            reason: "working directory must be an absolute path".to_string(),
        });
    }

    // Resolve symlinks/`..` before any containment test: checking the literal
    // path would let `/home/u/env/../workspace` or a symlink into the env dir
    // walk straight through the denylist.
    let canonical = dir
        .canonicalize()
        .map_err(|e| SandboxError::MountPathDenied {
            path: dir.display().to_string(),
            reason: format!("working directory cannot be resolved: {e}"),
        })?;

    // At or inside a protected directory → refuse. Mounting an *ancestor* of a
    // protected directory stays legal (agents run `git` at repo roots); those
    // are handled by `masked_dirs_for_mount` instead.
    //
    // `starts_with` is component-wise, so `/home/u/envs` does not match
    // `/home/u/env` — prefix confusion is not a bypass.
    if let Some(protected_dir) = protected
        .iter()
        .find(|p| canonical == **p || canonical.starts_with(p.as_path()))
    {
        return Err(SandboxError::MountPathDenied {
            path: canonical.display().to_string(),
            reason: format!(
                "working directory is at or inside the protected directory {}",
                protected_dir.display()
            ),
        });
    }

    if is_protected_file_name(&canonical) {
        return Err(SandboxError::MountPathDenied {
            path: canonical.display().to_string(),
            reason: "working directory is a credential-bearing file".to_string(),
        });
    }

    Ok(canonical)
}

/// Protected directories that lie *inside* `mount_root`.
///
/// The caller shadow-mounts each of these with an empty directory, so a mount
/// of a legitimate parent (a repository root, a home directory) cannot be used
/// to read the secrets it contains.
pub fn masked_dirs_for_mount(mount_root: &Path, protected: &[PathBuf]) -> Vec<PathBuf> {
    let Ok(root) = mount_root.canonicalize() else {
        return Vec::new();
    };
    protected
        .iter()
        .filter(|p| {
            let resolved = p.canonicalize().unwrap_or_else(|_| (*p).clone());
            resolved != root && resolved.starts_with(&root)
        })
        .cloned()
        .collect()
}

/// Container path for a masked directory, given the mount root.
///
/// Returns `None` when the protected dir is not inside the mount (nothing to
/// mask) or when the relative path cannot be expressed (defensive: the caller
/// must not mount a mask outside `/workspace`).
pub fn mask_destination(mount_root: &Path, protected_dir: &Path) -> Option<String> {
    let root = mount_root.canonicalize().ok()?;
    let target = protected_dir.canonicalize().ok()?;
    let relative = target.strip_prefix(&root).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    let joined = Path::new("/workspace").join(relative);
    Some(joined.to_string_lossy().into_owned())
}

/// True when the path's final component is a credential-bearing file name.
fn is_protected_file_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lowered = name.to_ascii_lowercase();
    lowered.ends_with(ENV_SUFFIX) || PROTECTED_FILE_NAMES.contains(&lowered.as_str())
}

/// Reject mount roots whose own path contains a lexical traversal component.
///
/// Canonicalization already collapses `..`, so this is a belt-and-braces check
/// for callers that validate before the path exists on disk.
pub fn has_traversal_component(dir: &Path) -> bool {
    dir.components().any(|c| matches!(c, Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        /// Layout mirrors a real tenant: the env directory is a *sibling* of
        /// the base/state directory, not a child of it.
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "lw-mount-guard-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(root.join("lunarwing/env")).expect("env dir");
            std::fs::create_dir_all(root.join("lunarwing/state")).expect("state dir");
            std::fs::create_dir_all(root.join("lunarwing/ic/src")).expect("repo dir");
            std::fs::write(
                root.join("lunarwing/env/lunarwing.env"),
                "GATEWAY_AUTH_TOKEN=deadbeef\n",
            )
            .expect("env file");
            std::fs::write(root.join("lunarwing/ic/src/main.rs"), "fn main() {}\n")
                .expect("source");
            Self { root }
        }

        fn protected(&self) -> Vec<PathBuf> {
            vec![self.root.join("lunarwing/env")]
        }

        fn env_dir(&self) -> PathBuf {
            self.root.join("lunarwing/env")
        }

        fn env_file(&self) -> PathBuf {
            self.root.join("lunarwing/env/lunarwing.env")
        }

        fn repo_root(&self) -> PathBuf {
            self.root.join("lunarwing")
        }

        fn repo_subdir(&self) -> PathBuf {
            self.root.join("lunarwing/ic")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn rejects_relative_paths() {
        let fixture = Fixture::new();
        let err = validate_mount_dir(Path::new("workspace"), &fixture.protected())
            .expect_err("relative denied");
        assert_eq!(err.kind(), "mount_path_denied");
    }

    #[test]
    fn rejects_empty_path() {
        let err = validate_mount_dir(Path::new(""), &[]).expect_err("empty denied");
        assert_eq!(err.kind(), "config");
    }

    #[test]
    fn rejects_the_env_directory_itself() {
        let fixture = Fixture::new();
        let err = validate_mount_dir(&fixture.env_dir(), &fixture.protected())
            .expect_err("env dir denied");
        assert_eq!(err.kind(), "mount_path_denied");
    }

    #[test]
    fn rejects_the_env_file_itself() {
        let fixture = Fixture::new();
        let err = validate_mount_dir(&fixture.env_file(), &fixture.protected())
            .expect_err("env file denied");
        assert_eq!(err.kind(), "mount_path_denied");
    }

    #[test]
    fn rejects_traversal_into_protected_dir() {
        let fixture = Fixture::new();
        let sneaky = fixture.repo_root().join("ic/../env");
        let err = validate_mount_dir(&sneaky, &fixture.protected()).expect_err("traversal denied");
        assert_eq!(err.kind(), "mount_path_denied");
    }

    #[test]
    fn accepts_a_repo_subdirectory() {
        let fixture = Fixture::new();
        let resolved =
            validate_mount_dir(&fixture.repo_subdir(), &fixture.protected()).expect("subdir ok");
        assert!(resolved.ends_with("ic"));
    }

    #[test]
    fn accepts_a_repo_root_but_reports_the_dir_to_mask() {
        let fixture = Fixture::new();
        // The repo root contains env/, so mounting it is legal (agents run git
        // there) — but the env dir must be masked.
        let resolved = validate_mount_dir(&fixture.repo_root(), &fixture.protected())
            .expect("repo root allowed");
        let masked = masked_dirs_for_mount(&resolved, &fixture.protected());
        assert_eq!(masked.len(), 1, "env dir must be masked: {masked:?}");
        assert_eq!(
            mask_destination(&resolved, &masked[0]).as_deref(),
            Some("/workspace/env")
        );
    }

    #[test]
    fn mask_destination_handles_nested_protected_dirs() {
        let fixture = Fixture::new();
        // Mounting the tenant home puts env two levels down.
        let home = fixture.root.clone();
        let masked = masked_dirs_for_mount(&home, &fixture.protected());
        assert_eq!(masked.len(), 1);
        assert_eq!(
            mask_destination(&home, &masked[0]).as_deref(),
            Some("/workspace/lunarwing/env")
        );
    }

    #[test]
    fn nothing_to_mask_for_an_unrelated_directory() {
        let fixture = Fixture::new();
        let masked = masked_dirs_for_mount(&fixture.repo_subdir(), &fixture.protected());
        assert!(masked.is_empty(), "expected no masks, got {masked:?}");
    }

    #[test]
    fn fails_closed_when_directory_missing() {
        let fixture = Fixture::new();
        let missing = fixture.root.join("does-not-exist");
        let err = validate_mount_dir(&missing, &fixture.protected()).expect_err("missing denied");
        assert_eq!(err.kind(), "mount_path_denied");
    }

    #[test]
    fn protected_file_names_cover_env_suffix_and_known_secrets() {
        assert!(is_protected_file_name(Path::new("/x/y/lunarwing.env")));
        assert!(is_protected_file_name(Path::new("/x/y/pg.secret")));
        assert!(is_protected_file_name(Path::new("/x/y/anything.env")));
        assert!(is_protected_file_name(Path::new("/x/y/.env")));
        assert!(is_protected_file_name(Path::new("/x/y/LUNARWING.ENV")));
        assert!(!is_protected_file_name(Path::new("/x/y/notes.txt")));
        assert!(!is_protected_file_name(Path::new("/x/y/environment.md")));
    }

    #[test]
    fn has_traversal_component_detects_parent_dirs() {
        assert!(has_traversal_component(Path::new("/a/../b")));
        assert!(!has_traversal_component(Path::new("/a/b")));
    }
}
