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
    use std::fs::{OpenOptions, create_dir};
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
            Err(error) => return Err(error),
        }
        parent = open_dir(&child)?;
    }
    resolve_workspace_path(path, &root)
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
    matches!(error.raw_os_error(), Some(code) if code == libc::ENOTDIR || code == libc::ELOOP)
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
