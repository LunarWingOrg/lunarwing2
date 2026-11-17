//! PATH policy for locating the container runtime and its helpers (WI#663).
//!
//! systemd's default user-service `PATH` omits sbin, where Gentoo installs
//! pasta, and a tenant unit rendered before this work item names only bin
//! directories. Rootless Podman resolves pasta by name, so a runtime started
//! with that `PATH` fails every run with "could not find pasta, the network
//! namespace can't be configured".
//!
//! The fix must cover the whole runtime lifecycle, not just container startup:
//! a runtime installed only under sbin is also invisible to the `which` probe
//! that selects it, and to the version/rootless probes that classify it. This
//! module owns that single policy so detection, execution, and the reaper all
//! see the same directories.
//!
//! The policy is Unix-only: Windows has no sbin convention and separates
//! `PATH` with `;`, so the mutators are no-ops there and detection keeps the
//! inherited environment.

/// Runtime `PATH` used when the daemon has none at all.
#[cfg(unix)]
pub(crate) const DEFAULT_CONTAINER_RUNTIME_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

#[cfg(unix)]
const RUNTIME_SBIN_DIRECTORIES: [&str; 3] = ["/usr/local/sbin", "/usr/sbin", "/sbin"];

/// Append the sbin directories `inherited` lacks, keeping its own order and
/// any custom runtime locations it names.
///
/// They go last so that every name the daemon already resolves keeps resolving
/// to the same file: on hosts where `/usr/sbin` is a link to `/usr/bin`, sbin
/// in front would put `/usr/bin` ahead of `/usr/local/bin`. A helper that
/// exists only under sbin is found either way.
#[cfg(unix)]
pub(crate) fn container_runtime_path(inherited: &str) -> String {
    let mut path = inherited.to_string();
    for directory in RUNTIME_SBIN_DIRECTORIES {
        if inherited.split(':').any(|part| part == directory) {
            continue;
        }
        if !path.is_empty() {
            path.push(':');
        }
        path.push_str(directory);
    }
    path
}

/// The daemon's `PATH` with the sbin directories restored.
#[cfg(unix)]
pub(crate) fn search_path() -> String {
    let inherited =
        std::env::var("PATH").unwrap_or_else(|_| DEFAULT_CONTAINER_RUNTIME_PATH.to_string());
    container_runtime_path(&inherited)
}

/// Set the runtime search `PATH` on a workload command.
///
/// Call this before `prepare_service`: service mode serialises only the
/// variables the caller set into the unit, and unsets every other variable of
/// the user manager, so an unset `PATH` would otherwise reach the unit as no
/// `PATH` at all.
#[cfg(unix)]
pub(crate) fn apply(cmd: &mut tokio::process::Command) {
    cmd.env("PATH", search_path());
}

#[cfg(not(unix))]
pub(crate) fn apply(_cmd: &mut tokio::process::Command) {}

/// Same as [`apply`] for a command that is spawned directly.
#[cfg(unix)]
pub(crate) fn apply_std(cmd: &mut std::process::Command) {
    cmd.env("PATH", search_path());
}

/// Locate `bin` through [`search_path`] rather than the daemon's raw `PATH`,
/// so a runtime installed only in an sbin directory is still selected.
pub(crate) fn binary_on_path(bin: &str) -> bool {
    lookup_command(bin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn lookup_command(bin: &str) -> std::process::Command {
    #[cfg(unix)]
    let program = "which";
    #[cfg(windows)]
    let program = "where";
    let mut cmd = std::process::Command::new(program);
    cmd.arg(bin);
    // `where` takes a Windows `PATH` (`;`-separated), so only Unix gets the
    // sbin-augmented search path.
    #[cfg(unix)]
    apply_std(&mut cmd);
    cmd
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;

    #[cfg(unix)]
    #[test]
    fn container_runtime_path_restores_missing_sbin_directories() {
        assert_eq!(
            container_runtime_path("/usr/local/bin:/usr/bin:/bin"),
            "/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"
        );
        // Custom runtime locations survive, and no directory is repeated.
        let custom = container_runtime_path("/custom/bin:/usr/sbin");
        assert_eq!(custom, "/custom/bin:/usr/sbin:/usr/local/sbin:/sbin");
        assert_eq!(custom.matches("/usr/sbin").count(), 1);
        assert_eq!(
            container_runtime_path("/usr/local/sbin:/usr/sbin:/sbin"),
            "/usr/local/sbin:/usr/sbin:/sbin"
        );
        assert_eq!(
            container_runtime_path(""),
            "/usr/local/sbin:/usr/sbin:/sbin"
        );
        // The tenant unit's own `PATH` names all three already: nothing is
        // added, so the runtime commands get the unit's order unchanged.
        let unit =
            "/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin:/home/tenant/.cargo/bin";
        assert_eq!(container_runtime_path(unit), unit);
    }

    /// !2522 AFD-1: on a host where `/usr/sbin` is a link to `/usr/bin`, sbin
    /// in front lets `/usr/bin` shadow `/usr/local/bin`. The inherited entries
    /// therefore stay in front, in their own order, whatever is added.
    #[cfg(unix)]
    #[test]
    fn container_runtime_path_keeps_inherited_entries_first() {
        for inherited in [
            "/usr/local/bin:/usr/bin:/bin",
            "/usr/local/bin:/usr/bin:/bin:/home/tenant/.cargo/bin",
            "/opt/runtime/bin:/usr/sbin:/usr/local/bin",
            "/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin",
        ] {
            let path = container_runtime_path(inherited);
            assert!(
                path == inherited || path.starts_with(&format!("{inherited}:")),
                "{path} does not keep the inherited entries {inherited} in front"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn runtime_lookup_uses_the_sbin_search_path() {
        let lookup = lookup_command("podman");
        let path = lookup
            .get_envs()
            .find_map(|(key, value)| {
                if key == "PATH" {
                    value.map(|value| value.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
            .expect("lookup PATH");
        for directory in RUNTIME_SBIN_DIRECTORIES {
            assert!(
                path.split(':').any(|part| part == directory),
                "{directory} missing from lookup PATH {path}"
            );
        }

        let mut runtime = tokio::process::Command::new("podman");
        apply(&mut runtime);
        let path = runtime
            .as_std()
            .get_envs()
            .find_map(|(key, value)| {
                if key == "PATH" {
                    value.map(|value| value.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
            .expect("runtime PATH");
        assert_eq!(path, search_path());
    }

    #[cfg(unix)]
    #[test]
    fn binary_on_path_tells_present_from_absent() {
        // Every supported host ships `sh`. This shows only that the lookup
        // runs and tells present from absent; the sbin directories on its
        // `PATH` are pinned by `runtime_lookup_uses_the_sbin_search_path`.
        assert!(binary_on_path("sh"));
        assert!(!binary_on_path("lunarwing-no-such-runtime-binary"));
    }
}
