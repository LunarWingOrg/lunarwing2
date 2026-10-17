//! Workspace-path confinement for daemon-supplied project directories.
//!
//! Mirrors `kimicode4lunarwing/scripts/workspace_path.ts`: a requested
//! `project_dir` is expanded against `WORKSPACE_ROOT` (`~`/`~/...` map under
//! the root, relative paths join it), canonicalized through the filesystem so
//! `..` segments and symlinks are resolved, and rejected when the result
//! escapes the root. Confinement runs before the pebble subprocess spawns, so
//! a crafted `project_dir` can never point the CLI at an arbitrary host
//! directory.

use std::io;
use std::path::{Path, PathBuf};

pub(crate) fn expand_workspace_path(path: &str, workspace_root: &Path) -> PathBuf {
    if path == "~" {
        return workspace_root.to_path_buf();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return workspace_root.join(rest);
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        workspace_root.join(candidate)
    }
}

pub(crate) fn resolve_workspace_path(path: &str, workspace_root: &Path) -> io::Result<PathBuf> {
    let canonical_root = workspace_root.canonicalize()?;
    // A daemon-supplied dir that does not exist (or is not traversable)
    // surfaces as a bare OS error like ENOENT; re-wrap it with the requested
    // path so the rejection envelope tells the operator what failed, while
    // preserving the original error kind for programmatic matching.
    let candidate = expand_workspace_path(path, &canonical_root)
        .canonicalize()
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("project_dir '{path}' could not be resolved under WORKSPACE_ROOT: {e}"),
            )
        })?;
    if candidate.strip_prefix(&canonical_root).is_ok() {
        Ok(candidate)
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("Workspace path escapes WORKSPACE_ROOT: {path}"),
        ))
    }
}

pub(crate) fn workspace_paths_overlap(left: &Path, right: &Path) -> bool {
    left.strip_prefix(right).is_ok() || right.strip_prefix(left).is_ok()
}

/// Bootstrap a cron mission directory using held directory descriptors.
/// Linux workers provide /proc/self/fd; `O_NOFOLLOW` rejects symlink components
/// before the next mkdir, including swaps by another task in the workspace.
fn create_cron_workspace(path: &str, workspace_root: &Path) -> io::Result<PathBuf> {
    use std::fs::{create_dir, OpenOptions};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    if path.is_empty()
        || path.starts_with('~')
        || path.contains('\0')
        || Path::new(path).is_absolute()
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected relative directory components",
        ));
    }
    // DS-6: fail explicitly when the descriptor-walk transport is unavailable
    // (e.g. a sandbox masking /proc) instead of a misleading mkdir ENOENT.
    if !Path::new("/proc/self/fd").exists() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "/proc/self/fd is not available (required for symlink-safe directory creation)",
        ));
    }
    let root = workspace_root.canonicalize()?;
    let open_dir = |path: &Path| {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)
    };
    let mut parent = open_dir(&root)?;
    for part in path.split('/') {
        let child = PathBuf::from(format!("/proc/self/fd/{}", parent.as_raw_fd())).join(part);
        match create_dir(&child) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(annotate_component(part, error)),
        }
        parent = open_dir(&child).map_err(|error| annotate_component(part, error))?;
    }
    let resolved = resolve_workspace_path(path, &root)?;
    // WI#375 DS-1: the descriptor walk and the re-resolved path must name
    // the same directory — a dev/ino mismatch means an in-root redirect
    // raced the walk. Reported as NotFound so the host classifies the swap
    // race as transient (retry re-walks), like a swap-race ENOENT.
    {
        use std::os::unix::fs::MetadataExt;
        let walked = parent.metadata()?;
        let landed = resolved.metadata()?;
        if walked.dev() != landed.dev() || walked.ino() != landed.ino() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("resolved path changed during bootstrap for '{path}'"),
            ));
        }
    }
    Ok(resolved)
}

/// DS-2: only validation, a non-directory component (ENOTDIR), a symlink loop
/// (ELOOP), permission denials (EACCES/EPERM) and containment failures are
/// permanent. Everything else (EMFILE/ENFILE/ENOMEM, ENOSPC/EDQUOT/EIO on the
/// first mkdir, ENOENT/EINVAL from a swap race) is transient and the host
/// supervisor may re-dispatch. `Unsupported` marks the missing-/proc preflight.
fn is_non_retryable_bootstrap(error: &io::Error) -> bool {
    use io::ErrorKind::{InvalidInput, PermissionDenied, Unsupported};
    if matches!(error.kind(), InvalidInput | PermissionDenied | Unsupported) {
        return true;
    }
    // The component annotation (WI#375 DS-7) boxes the original error, so the
    // raw errno is reachable through the payload, not on the wrapper itself.
    let raw = error.raw_os_error().or_else(|| {
        error
            .get_ref()
            .and_then(|payload| payload.downcast_ref::<AnnotatedComponent>())
            .and_then(|annotated| annotated.inner.raw_os_error())
    });
    matches!(raw, Some(code) if code == libc::ENOTDIR || code == libc::ELOOP)
}

/// Bootstrap failure payload naming the path component that failed (WI#375
/// DS-7): std `io::Error` cannot carry both a custom message and the raw
/// errno, so the original error is boxed as the payload — `kind()` is set at
/// construction and the raw errno stays reachable through `inner` for retry
/// classification.
#[derive(Debug)]
struct AnnotatedComponent {
    part: String,
    inner: io::Error,
}

impl std::fmt::Display for AnnotatedComponent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "component '{}': {}", self.part, self.inner)
    }
}

impl std::error::Error for AnnotatedComponent {}

/// Name the failing component (the raw error only names the /proc/self/fd
/// magic path) while preserving the error kind and raw errno. Mirrors the TS
/// bridges' `annotateComponent` wording.
fn annotate_component(part: &str, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        AnnotatedComponent {
            part: part.to_string(),
            inner: error,
        },
    )
}

pub(crate) fn resolve_task_workspace(
    context: &crate::protocol::TaskContext,
    workspace_root: &Path,
) -> io::Result<PathBuf> {
    let path = context.project_dir.as_deref().unwrap_or("~");
    if context
        .cron_tick
        .as_ref()
        .and_then(|tick| tick.get("event"))
        .and_then(serde_json::Value::as_str)
        == Some("cron.tick")
    {
        create_cron_workspace(path, workspace_root).map_err(|error| {
            // DS-2: the wrapper text is the host supervisor's retry signal —
            // "Invalid workspace path" abandons the tick, "Transient workspace
            // error" re-dispatches (checked before any marker substring, so a
            // wrapped "could not be resolved under WORKSPACE_ROOT" cannot
            // misclassify a transient failure).
            let wrapper = if is_non_retryable_bootstrap(&error) {
                format!("Invalid workspace path for cron tick: {error}")
            } else {
                format!("Transient workspace error for cron tick: {error}")
            };
            io::Error::new(error.kind(), wrapper)
        })
    } else {
        resolve_workspace_path(path, workspace_root)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root(tag: &str) -> PathBuf {
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("pebble-ws-{}-{unique}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp root");
        dir
    }

    #[test]
    fn expand_maps_home_and_relative_paths_under_root() {
        let root = Path::new("/workspace");

        assert_eq!(
            expand_workspace_path("~", root),
            PathBuf::from("/workspace")
        );
        assert_eq!(
            expand_workspace_path("~/src", root),
            PathBuf::from("/workspace/src")
        );
        assert_eq!(
            expand_workspace_path("pkg", root),
            PathBuf::from("/workspace/pkg")
        );
        assert_eq!(expand_workspace_path("/etc", root), PathBuf::from("/etc"));
        assert_eq!(
            expand_workspace_path("../etc", root),
            PathBuf::from("/workspace/../etc")
        );
        assert_eq!(
            expand_workspace_path("~user", root),
            PathBuf::from("/workspace/~user")
        );
    }

    #[test]
    fn resolve_accepts_root_subtree_and_symlink_aliases() {
        let root = temp_root("resolve-ok");
        let project = root.join("project");
        std::fs::create_dir_all(&project).expect("create project");
        std::os::unix::fs::symlink(&project, root.join("alias")).expect("create alias");

        assert_eq!(
            resolve_workspace_path("project", &root).expect("subdir"),
            project.canonicalize().expect("canonical project")
        );
        assert_eq!(
            resolve_workspace_path("alias", &root).expect("alias"),
            project.canonicalize().expect("canonical project")
        );
        assert_eq!(
            resolve_workspace_path("~", &root).expect("root"),
            root.canonicalize().expect("canonical root")
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_rejects_traversal_and_escaping_symlinks() {
        let root = temp_root("resolve-bad");
        let outside = temp_root("resolve-outside");
        std::os::unix::fs::symlink(&outside, root.join("escape")).expect("create escape symlink");

        for requested in ["..", "escape", outside.to_str().expect("utf-8 path")] {
            let err = resolve_workspace_path(requested, &root).expect_err("must escape");
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
            assert!(
                err.to_string()
                    .contains("Workspace path escapes WORKSPACE_ROOT"),
                "unexpected error: {err}"
            );
        }

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn resolve_wraps_missing_path_error_with_requested_path() {
        let root = temp_root("resolve-missing");

        let err = resolve_workspace_path("does-not-exist", &root).expect_err("missing dir");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert!(
            err.to_string().contains("project_dir 'does-not-exist'"),
            "error should carry the requested path: {err}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn overlap_detects_equal_and_nested_only() {
        assert!(workspace_paths_overlap(
            Path::new("/workspace/project"),
            Path::new("/workspace/project")
        ));
        assert!(workspace_paths_overlap(
            Path::new("/workspace/project"),
            Path::new("/workspace/project/src")
        ));
        assert!(workspace_paths_overlap(
            Path::new("/workspace/project/src"),
            Path::new("/workspace/project")
        ));
        assert!(!workspace_paths_overlap(
            Path::new("/workspace/project-a"),
            Path::new("/workspace/project-b")
        ));
        // Component-aware, not raw string prefix: "project-a" is not under "project".
        assert!(!workspace_paths_overlap(
            Path::new("/workspace/project"),
            Path::new("/workspace/project-a")
        ));
    }

    #[test]
    fn cron_bootstrap_error_classification() {
        // Validation, permission denials and the missing-/proc preflight are
        // permanent; so are a non-directory component and a symlink loop.
        for error in [
            io::Error::new(io::ErrorKind::InvalidInput, "bad components"),
            io::Error::new(io::ErrorKind::PermissionDenied, "denied"),
            io::Error::new(io::ErrorKind::Unsupported, "no /proc/self/fd"),
            io::Error::from_raw_os_error(libc::ENOTDIR),
            io::Error::from_raw_os_error(libc::ELOOP),
            io::Error::from_raw_os_error(libc::EACCES),
        ] {
            assert!(is_non_retryable_bootstrap(&error), "permanent: {error}");
        }
        // Transient errnos are worth a re-dispatch (DS-2).
        for error in [
            io::Error::from_raw_os_error(libc::EMFILE),
            io::Error::from_raw_os_error(libc::ENFILE),
            io::Error::from_raw_os_error(libc::ENOMEM),
            io::Error::from_raw_os_error(libc::ENOSPC),
            io::Error::from_raw_os_error(libc::EIO),
            io::Error::from_raw_os_error(libc::ENOENT),
        ] {
            assert!(!is_non_retryable_bootstrap(&error), "transient: {error}");
        }
    }

    /// DS-3: a FIFO component must fail fast with ENOTDIR (permanent) —
    /// without `O_DIRECTORY` the blocking FIFO open would hang the bridge.
    /// WI#375 DS-7: the error names the failing component, and the ENOTDIR
    /// errno must survive the annotation (still classified permanent).
    #[test]
    fn cron_bootstrap_rejects_fifo_component_fast() {
        let root = temp_root("cron-fifo");
        let fifo = root.join("missions");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("spawn mkfifo");
        assert!(status.success(), "mkfifo failed: {status}");
        let context = crate::protocol::TaskContext {
            project_dir: Some("missions/x".to_string()),
            cron_tick: Some(serde_json::json!({"event":"cron.tick"})),
            ..Default::default()
        };
        let error = resolve_task_workspace(&context, &root).expect_err("FIFO must be refused");
        assert!(
            error.to_string().contains("Invalid workspace path"),
            "FIFO component must be a permanent failure, got: {error}"
        );
        assert!(
            error.to_string().contains("component 'missions'"),
            "error names the failing component, got: {error}"
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// WI#375 DS-7: the component annotation names the failing component in
    /// the message while the boxed inner error keeps its raw errno for
    /// retry classification.
    #[test]
    fn annotated_component_preserves_errno_classification() {
        let permanent = annotate_component("missions", io::Error::from_raw_os_error(libc::ENOTDIR));
        assert!(is_non_retryable_bootstrap(&permanent));
        assert!(permanent.to_string().contains("component 'missions'"));
        let symlink_loop = annotate_component("x", io::Error::from_raw_os_error(libc::ELOOP));
        assert!(is_non_retryable_bootstrap(&symlink_loop));
        // Transient errnos stay transient through the same annotation.
        let transient = annotate_component("missions", io::Error::from_raw_os_error(libc::EMFILE));
        assert!(!is_non_retryable_bootstrap(&transient));
        let swap_race = annotate_component("x", io::Error::from_raw_os_error(libc::ENOENT));
        assert!(!is_non_retryable_bootstrap(&swap_race));
    }

    /// WI#375 DS-1: the walked descriptor and the re-resolved path must be
    /// the same directory; the happy path keeps them equal.
    #[test]
    fn cron_bootstrap_walked_fd_matches_resolved_path() {
        let root = temp_root("cron-ds1");
        let context = crate::protocol::TaskContext {
            project_dir: Some("missions/pr-digest".to_string()),
            cron_tick: Some(serde_json::json!({"event":"cron.tick"})),
            ..Default::default()
        };
        let mission = resolve_task_workspace(&context, &root).expect("bootstrap");
        assert_eq!(
            mission,
            root.join("missions/pr-digest").canonicalize().unwrap()
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// WI#375 DS-3: bounded swap race — an attacker process swaps the parent
    /// component between a real directory and a symlink to an outside
    /// directory while the cron bootstrap walks it. A successful resolve must
    /// never land outside the root, and the outside directory must stay
    /// empty.
    #[test]
    fn cron_bootstrap_swap_race_never_escapes_root() {
        let root = temp_root("cron-swap-root");
        let outside = temp_root("cron-swap-outside");
        let link = root.join("missions");
        std::fs::create_dir(&link).expect("create missions dir");
        // Attacker thread (MR !1669 review IK-5): alternate `missions`
        // between a symlink to the outside dir and a real dir as fast as
        // possible until stopped — a forked `sh` manages only a handful of
        // swaps over the whole loop, so the O_NOFOLLOW-removal mutant
        // survived; the in-process thread contests most iterations.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let attacker = {
            let (stop, link, outside) = (stop.clone(), link.clone(), outside.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::fs::remove_dir_all(&link).ok();
                    std::os::unix::fs::symlink(&outside, &link).ok();
                    std::fs::remove_file(&link).ok();
                    std::fs::create_dir(&link).ok();
                }
            })
        };
        let context = crate::protocol::TaskContext {
            project_dir: Some("missions/x".to_string()),
            cron_tick: Some(serde_json::json!({"event":"cron.tick"})),
            ..Default::default()
        };
        let real_root = root.canonicalize().expect("canonical root");
        let mut succeeded = 0u32;
        for _ in 0..2000 {
            if let Ok(resolved) = resolve_task_workspace(&context, &root) {
                assert!(
                    resolved.starts_with(&real_root),
                    "resolved path escaped the root: {}",
                    resolved.display()
                );
                succeeded += 1;
            }
            // Swap races legitimately fail (ELOOP/ENOENT/redirect mismatch);
            // the invariant is that a success never lands outside the root.
        }
        stop.store(true, Ordering::Relaxed);
        attacker.join().ok();
        assert!(
            succeeded > 0,
            "harness never completed a bootstrap — vacuous"
        );
        assert!(
            std::fs::read_dir(&outside)
                .expect("read outside dir")
                .next()
                .is_none(),
            "outside directory must stay empty"
        );
        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(outside).ok();
    }

    /// WI#375 DS-1 / MR !1669 review IK-3: an in-root redirect raced between
    /// the descriptor walk and the path re-resolve must be refused. The decoy
    /// already contains "x", so without the dev/ino compare a raced resolve
    /// returns root/decoy/x. Runs until the race window is hit once (a
    /// redirected success or the DS-1 refusal) or the time budget runs out.
    #[test]
    fn cron_bootstrap_refuses_in_root_redirect_raced_after_walk() {
        let root = temp_root("cron-ds1-race")
            .canonicalize()
            .expect("canonical root");
        let link = root.join("missions");
        let decoy = root.join("decoy");
        std::fs::create_dir(&link).expect("create missions dir");
        std::fs::create_dir_all(decoy.join("x")).expect("create decoy/x");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let attacker = {
            let (stop, link, decoy) = (stop.clone(), link.clone(), decoy.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::fs::remove_dir_all(&link).ok();
                    std::os::unix::fs::symlink(&decoy, &link).ok();
                    std::fs::remove_file(&link).ok();
                    std::fs::create_dir(&link).ok();
                }
            })
        };
        let context = crate::protocol::TaskContext {
            project_dir: Some("missions/x".to_string()),
            cron_tick: Some(serde_json::json!({"event":"cron.tick"})),
            ..Default::default()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut redirected = Vec::new();
        let mut refused = 0u32;
        while std::time::Instant::now() < deadline && redirected.is_empty() && refused == 0 {
            match resolve_task_workspace(&context, &root) {
                Ok(resolved) if resolved.starts_with(&decoy) => redirected.push(resolved),
                Ok(_) => {}
                Err(error) if error.to_string().contains("resolved path changed") => refused += 1,
                Err(_) => {}
            }
        }
        stop.store(true, Ordering::Relaxed);
        attacker.join().ok();
        std::fs::remove_dir_all(&root).ok();
        assert!(
            redirected.is_empty(),
            "in-root redirect returned: {redirected:?}"
        );
        assert!(refused > 0, "race window never hit within the time budget");
    }

    #[test]
    fn cron_bootstrap_creates_default_directory_without_following_symlinks() {
        let root = temp_root("cron");
        let outside = temp_root("cron-outside");
        let mut context = crate::protocol::TaskContext {
            project_dir: Some("missions/pr-digest".to_string()),
            ..Default::default()
        };
        assert!(resolve_task_workspace(&context, &root).is_err());
        assert!(!root.join("missions").exists());
        context.cron_tick = Some(serde_json::json!({"event":"cron.tick"}));
        let mission = resolve_task_workspace(&context, &root).unwrap();
        assert_eq!(
            mission,
            root.join("missions/pr-digest").canonicalize().unwrap()
        );
        std::fs::write(mission.join("state"), "tick one").unwrap();
        assert_eq!(resolve_task_workspace(&context, &root).unwrap(), mission);
        assert_eq!(
            std::fs::read_to_string(mission.join("state")).unwrap(),
            "tick one"
        );
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        std::os::unix::fs::symlink(root.join("missing"), root.join("dangling")).unwrap();
        for path in [
            "../bad",
            "/tmp/bad",
            "~",
            "missions/../bad",
            "escape/new",
            "dangling/new",
        ] {
            context.project_dir = Some(path.to_string());
            assert!(resolve_task_workspace(&context, &root)
                .unwrap_err()
                .to_string()
                .contains("Invalid workspace path"));
        }
        assert!(!outside.join("new").exists());
        assert!(!root.join("missing").exists());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
}
