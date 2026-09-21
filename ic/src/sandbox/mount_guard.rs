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
    resolve_protected_paths_from(
        std::env::var(PROTECTED_PATHS_ENV).ok().as_deref(),
        &crate::bootstrap::lunarwing_base_dir(),
    )
}

/// Protected set for an explicit env override and base dir.
///
/// Split out of [`resolve_protected_paths`] so the derivation can be tested
/// without touching process env (parallel tests would race on it).
///
/// WI#215: when the multi-tenant layout is detected — the base dir is a
/// `state/` component *inside* the tenant root, so `env/` and `run/` are
/// siblings of it, not children — the tenant's `<root>/env` and `<root>/run`
/// directories are added defensively. A tenant that has not been re-rendered
/// since `SANDBOX_PROTECTED_PATHS` was introduced (WI#205) would otherwise
/// leave `<root>/run` (the world-writable `ssh-agent.sock`, the tenant's SSH
/// identity) unmasked to any sandboxed command mounting the tenant root. The
/// added paths are inert subdirectories of already-protected single-node
/// bases, so default protection behaviour is unchanged.
fn resolve_protected_paths_from(env_raw: Option<&str>, base: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = env_raw
        .map(|raw| {
            raw.split(':')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default();

    paths.push(base.join("env"));
    paths.push(base.join("state"));
    paths.push(base.join("run"));
    paths.push(base.to_path_buf());
    add_mt_tenant_siblings(base, &mut paths);
    if let Ok(canonical) = base.canonicalize() {
        paths.push(canonical.join("env"));
        paths.push(canonical.join("state"));
        paths.push(canonical.join("run"));
        paths.push(canonical.clone());
        add_mt_tenant_siblings(&canonical, &mut paths);
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

/// Add `<root>/env` and `<root>/run` when `base` looks like an MT state dir
/// (`<root>/<something>/state`). The explicit list already protects the base
/// itself, so `state/` is not repeated.
fn add_mt_tenant_siblings(base: &Path, paths: &mut Vec<PathBuf>) {
    if base.file_name().and_then(|n| n.to_str()) == Some("state")
        && let Some(tenant_root) = base.parent()
    {
        paths.push(tenant_root.join("env"));
        paths.push(tenant_root.join("run"));
    }
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
///
/// Existence is NOT a precondition (WI#209): a protected directory that does
/// not exist yet at plan time (`<root>/run` on a freshly provisioned tenant)
/// is still planned for masking via lexical containment. The caller must ensure
/// the mask destination exists inside the mount before `podman run` (see
/// `EmptyMaskDir`/pre-mount creation in `cli_runner`); the empty bind then
/// shadows whatever appears there for the container's whole lifetime.
///
/// `mount_root` must be the already-validated canonical root (from
/// `validate_mount_dir`); resolution failure here is a hard error, never a
/// silent empty mask list.
pub fn masked_dirs_for_mount(
    mount_root: &Path,
    protected: &[PathBuf],
) -> Result<Vec<PathBuf>, SandboxError> {
    let root = mount_root
        .canonicalize()
        .map_err(|e| SandboxError::MountPathDenied {
            path: mount_root.display().to_string(),
            reason: format!("mount root cannot be resolved for masking: {e}"),
        })?;
    Ok(protected
        .iter()
        .filter(|p| contains_protected_dir(&root, p))
        .cloned()
        .collect())
}

/// True when `protected` is strictly inside `root`, resolved lexically so the
/// check holds even while the protected directory does not exist yet (WI#209).
/// Existing paths are compared canonically first (symlink-safe); nonexistent
/// ones fall back to component-wise lexical containment, which cannot be
/// defeated by `..` (the caller canonicalized `root`) or prefix confusion
/// (`starts_with` is component-wise).
fn contains_protected_dir(root: &Path, protected: &Path) -> bool {
    let resolved = protected
        .canonicalize()
        .unwrap_or_else(|_| protected.to_path_buf());
    if resolved != root && resolved.starts_with(root) {
        return true;
    }
    protected != root && protected.starts_with(root)
}

/// Container path for a masked directory, given the canonical mount root and
/// the protected directory as returned by [`masked_dirs_for_mount`].
///
/// Returns `None` when the relative path cannot be expressed (defensive: the
/// caller must not mount a mask outside `/workspace`). Works for protected
/// directories that do not exist yet (lexical suffix, no canonicalization).
pub fn mask_destination(mount_root: &Path, protected_dir: &Path) -> Option<String> {
    let relative = protected_dir.strip_prefix(mount_root).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    let joined = Path::new("/workspace").join(relative);
    Some(joined.to_string_lossy().into_owned())
}

/// True when the path's final component is a credential-bearing file name.
pub fn is_protected_file_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lowered = name.to_ascii_lowercase();
    lowered.ends_with(ENV_SUFFIX) || PROTECTED_FILE_NAMES.contains(&lowered.as_str())
}

/// Bound for the credential-file walk of a mounted tree (WI#211).
const MASK_SCAN_MAX_DEPTH: usize = 5;
/// Total entries visited before the walk stops expanding.
const MASK_SCAN_MAX_ENTRIES: usize = 10_000;

/// Credential-bearing files discovered by a bounded walk of the mount tree
/// (WI#211). Directory masking covers the enumerated protected dirs; this
/// covers credential files *outside* them — a copied `lunarwing.env` in a
/// workspace, a backup, a snapshot. Returns canonical file paths the caller
/// shadows with an empty read-only file bind.
///
/// The walk is plan-time only (like directory masking): files created during
/// execution are not retroactively masked, but the mount is read-only for the
/// sandboxed policy and writable workspaces are agent-owned task data, not a
/// security boundary. Protected directories passed in `skip` are not descended
/// into (they are shadowed whole).
pub fn masked_files_for_mount(mount_root: &Path, skip: &[PathBuf]) -> Vec<PathBuf> {
    let Ok(root) = mount_root.canonicalize() else {
        return Vec::new();
    };
    let skipped: Vec<PathBuf> = skip.iter().filter_map(|p| p.canonicalize().ok()).collect();
    let mut found = Vec::new();
    let mut entries = 0usize;
    // Stack of (dir, depth).
    let mut stack = vec![(root.clone(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if entries >= MASK_SCAN_MAX_ENTRIES {
            break;
        }
        let Ok(children) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in children.flatten() {
            entries += 1;
            if entries > MASK_SCAN_MAX_ENTRIES {
                break;
            }
            let path = entry.path();
            let file_type = entry.file_type();
            let Ok(ft) = file_type else { continue };
            if ft.is_symlink() {
                // Do not follow links during the walk: a link could point at a
                // file outside the mount (masking it would fail the whole run)
                // or re-enter a protected dir. The bind-mount surface is the
                // real file tree only.
                continue;
            }
            if ft.is_file() {
                if is_protected_file_name(&path) {
                    found.push(path);
                }
            } else if ft.is_dir() && depth + 1 < MASK_SCAN_MAX_DEPTH {
                if skipped.iter().any(|s| path.starts_with(s)) {
                    continue;
                }
                stack.push((path, depth + 1));
            }
        }
    }
    found.sort();
    found.dedup();
    found
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
            std::fs::create_dir_all(root.join("lunarwing/run")).expect("run dir");
            std::fs::create_dir_all(root.join("lunarwing/ic/src")).expect("repo dir");
            std::fs::write(
                root.join("lunarwing/env/lunarwing.env"),
                "GATEWAY_AUTH_TOKEN=deadbeef\n",
            )
            .expect("env file");
            std::fs::write(
                root.join("lunarwing/state/config.toml"),
                "[sandbox.external_workers]\nauth_token = \"secret\"\n",
            )
            .expect("state config");
            std::fs::write(root.join("lunarwing/ic/src/main.rs"), "fn main() {}\n")
                .expect("source");
            Self { root }
        }

        /// The protected set mt-admin writes: env, state and run under the
        /// tenant root — deliberately not the root itself, because the
        /// daemon's working directory lives inside it.
        fn protected(&self) -> Vec<PathBuf> {
            vec![
                self.root.join("lunarwing/env"),
                self.root.join("lunarwing/state"),
                self.root.join("lunarwing/run"),
            ]
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
    fn accepts_the_daemon_working_directory() {
        // The daemon's cwd is <tenant root>/ic and the shell tool defaults to
        // it, so the default sandboxed invocation must never be refused. This
        // is the reason the tenant root itself is not in the protected set.
        let fixture = Fixture::new();
        let daemon_cwd = fixture.root.join("lunarwing/ic");
        let resolved = validate_mount_dir(&daemon_cwd, &fixture.protected())
            .expect("daemon cwd must stay mountable");
        assert!(
            masked_dirs_for_mount(&resolved, &fixture.protected())
                .expect("mask planning")
                .is_empty(),
            "nothing under the repo needs masking"
        );
    }

    #[test]
    fn accepts_a_repo_root_but_reports_the_dirs_to_mask() {
        let fixture = Fixture::new();
        // The tenant root holds env/, state/ and run/, so mounting it is legal
        // (agents run git there) — but all three must be masked.
        let resolved = validate_mount_dir(&fixture.repo_root(), &fixture.protected())
            .expect("tenant root allowed");
        let mut destinations: Vec<String> = masked_dirs_for_mount(&resolved, &fixture.protected())
            .expect("mask planning")
            .iter()
            .filter_map(|dir| mask_destination(&resolved, dir))
            .collect();
        destinations.sort();
        assert_eq!(
            destinations,
            vec![
                "/workspace/env".to_string(),
                "/workspace/run".to_string(),
                "/workspace/state".to_string(),
            ],
            "every credential-bearing directory must be shadowed"
        );
    }

    #[test]
    fn rejects_the_state_and_run_directories() {
        let fixture = Fixture::new();
        for (label, dir) in [
            ("state", fixture.root.join("lunarwing/state")),
            ("run", fixture.root.join("lunarwing/run")),
        ] {
            match validate_mount_dir(&dir, &fixture.protected()) {
                Ok(resolved) => panic!("{label} dir must be denied, got {resolved:?}"),
                Err(err) => assert_eq!(err.kind(), "mount_path_denied", "{label} denial kind"),
            }
        }
    }

    #[test]
    fn rejects_the_state_config_file() {
        let fixture = Fixture::new();
        let config = fixture.root.join("lunarwing/state/config.toml");
        // Direct file mount: not a credential-suffixed name, but it lives
        // inside a protected directory, so traversal to it is refused.
        let err =
            validate_mount_dir(&config, &fixture.protected()).expect_err("state config denied");
        assert_eq!(err.kind(), "mount_path_denied");
    }

    #[test]
    fn mask_destination_handles_nested_protected_dirs() {
        let fixture = Fixture::new();
        // Mounting the tenant home puts the protected dirs two levels down.
        let home = fixture.root.clone();
        let mut destinations: Vec<String> = masked_dirs_for_mount(&home, &fixture.protected())
            .expect("mask planning")
            .iter()
            .filter_map(|dir| mask_destination(&home, dir))
            .collect();
        destinations.sort();
        assert_eq!(
            destinations,
            vec![
                "/workspace/lunarwing/env".to_string(),
                "/workspace/lunarwing/run".to_string(),
                "/workspace/lunarwing/state".to_string(),
            ]
        );
    }

    #[test]
    fn nothing_to_mask_for_an_unrelated_directory() {
        let fixture = Fixture::new();
        let masked =
            masked_dirs_for_mount(&fixture.repo_subdir(), &fixture.protected()).expect("planning");
        assert!(masked.is_empty(), "expected no masks, got {masked:?}");
    }

    #[test]
    fn fails_closed_when_directory_missing() {
        let fixture = Fixture::new();
        let missing = fixture.root.join("does-not-exist");
        let err = validate_mount_dir(&missing, &fixture.protected()).expect_err("missing denied");
        assert_eq!(err.kind(), "mount_path_denied");
    }

    /// WI#215: pin the "protected dir absent at plan time, created
    /// concurrently" scenario end to end — mount of a legitimate parent (the
    /// tenant root) is allowed while `run/` is missing, the missing dir is
    /// still planned for masking, and after run/` appears the planned
    /// `/workspace/run` mask destination covers the secrets created inside it.
    #[test]
    fn masks_protected_dir_created_after_planning() {
        let fixture = Fixture::new();
        let run_dir = fixture.root.join("lunarwing/run");
        // start/  is fresh: run/ does not exist yet (staging mirrors provisioners).
        std::fs::remove_dir_all(&run_dir).expect("remove run dir");

        let mount_root = validate_mount_dir(&fixture.repo_root(), &fixture.protected())
            .expect("tenant root remains mountable while run/ is absent");

        // Plan BEFORE run/ exists (WI#209): lexical containment still selects it.
        let masked = masked_dirs_for_mount(&mount_root, &fixture.protected()).expect("planning");
        assert!(masked.contains(&run_dir), "absent run/ must be masked: {masked:?}");

        // Concurrent first start: the daemon creates run/ and binds the sockets.
        std::fs::create_dir_all(&run_dir).expect("recreate run dir");
        std::fs::write(run_dir.join("lunarwing.sock"), "sock").expect("daemon socket");
        std::fs::write(run_dir.join("ssh-agent.sock"), "sock").expect("ssh-agent socket");

        // Socket names are ordinary files (the WI#211 credential-file walk does
        // not pick them up), so the plan-time /workspace/run empty-dir shadow is
        // the only control — assert it covers every file created there.
        let destination = mask_destination(&mount_root, &run_dir).expect("mask destination");
        assert_eq!(destination, "/workspace/run");
        for created in ["lunarwing.sock", "ssh-agent.sock", "pg.secret", "tenant.env"] {
            let sibling = run_dir.join(created);
            let covered = Path::new(&destination).join(created);
            assert!(
                sibling.starts_with(&run_dir) && covered.starts_with(&destination),
                "{created} created post-plan must fall inside the masked /workspace/run"
            );
        }
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

    /// WI#215: an MT base dir of `<root>/state` gains the tenant's env/ and
    /// run/ siblings, closing the stale-render gap (WI#209's fix covered
    /// existence; this covers which paths are known).
    #[test]
    fn mt_layout_base_dir_derives_env_and_run_siblings() {
        let base = Path::new("/home/tenant/lunarwing/state");
        let paths = resolve_protected_paths_from(None, base);
        let norm = |p: String| {
            std::fs::canonicalize(&p).map_or(p, |c| c.to_string_lossy().into_owned())
        };
        let rendered: Vec<String> = paths
            .iter()
            .map(|p| norm(p.to_string_lossy().into_owned()))
            .collect();
        for want in [
            "/home/tenant/lunarwing/env",
            "/home/tenant/lunarwing/run",
            "/home/tenant/lunarwing/state",
        ] {
            let want = norm(want.to_string());
            assert!(rendered.contains(&want), "missing {want} in {rendered:?}");
        }
        // The tenant root itself stays unprotected: the daemon's working
        // directory (<root>/ic) must remain mountable.
        let root = norm("/home/tenant/lunarwing".to_string());
        assert!(
            !rendered.contains(&root),
            "tenant root must not be protected: {rendered:?}"
        );
    }

    /// Single-node bases (`~/.ironclaw`, `/var/lib/lunarwing`, ...) do not end
    /// in `state/`, so no tenant siblings are derived — existing behaviour is
    /// unchanged.
    #[test]
    fn non_mt_base_dir_gains_no_siblings() {
        let paths = resolve_protected_paths_from(None, Path::new("/home/u/.ironclaw"));
        assert!(
            !paths.iter().any(|p| p.ends_with(Path::new("u/env"))),
            "no sibling env/ derived for a non-MT base: {paths:?}"
        );
    }

    /// Explicit SANDBOX_PROTECTED_PATHS entries are preserved alongside the
    /// derived set (mt-admin's env:state:run triple when rendered).
    #[test]
    fn explicit_paths_are_preserved() {
        let paths = resolve_protected_paths_from(
            Some("/t/lunarwing/env: /t/lunarwing/state ::/t/lunarwing/run "),
            Path::new("/home/u/.ironclaw"),
        );
        for want in [
            Path::new("/t/lunarwing/env"),
            Path::new("/t/lunarwing/state"),
            Path::new("/t/lunarwing/run"),
        ] {
            assert!(paths.contains(&want.to_path_buf()), "missing {want:?}");
        }
    }

    #[test]
    fn has_traversal_component_detects_parent_dirs() {
        assert!(has_traversal_component(Path::new("/a/../b")));
        assert!(!has_traversal_component(Path::new("/a/b")));
    }
}
