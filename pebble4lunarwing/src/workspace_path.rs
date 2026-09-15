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
}
