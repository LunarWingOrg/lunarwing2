//! PATH policy for locating the container runtime and its helpers (WI#663).
//!
//! systemd's default user-service `PATH` omits sbin, where Gentoo installs
//! pasta. mt-admin likewise renders a tenant unit whose `PATH` names only bin
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

/// Prepend the sbin directories `inherited` lacks, keeping its own order and
/// any custom runtime locations it names.
#[cfg(unix)]
pub(crate) fn container_runtime_path(inherited: &str) -> String {
    let missing = RUNTIME_SBIN_DIRECTORIES
        .into_iter()
        .filter(|directory| !inherited.split(':').any(|part| part == *directory))
        .collect::<Vec<_>>();
    let mut path = missing.join(":");
    if !path.is_empty() && !inherited.is_empty() {
        path.push(':');
    }
    path.push_str(inherited);
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
            "/usr/local/sbin:/usr/sbin:/sbin:/usr/local/bin:/usr/bin:/bin"
        );
        // Custom runtime locations survive, and no directory is repeated.
        let custom = container_runtime_path("/custom/bin:/usr/sbin");
        assert_eq!(custom, "/usr/local/sbin:/sbin:/custom/bin:/usr/sbin");
        assert_eq!(custom.matches("/usr/sbin").count(), 1);
        assert_eq!(
            container_runtime_path("/usr/local/sbin:/usr/sbin:/sbin"),
            "/usr/local/sbin:/usr/sbin:/sbin"
        );
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
    fn binary_on_path_finds_a_runtime_in_sbin() {
        // Every supported host ships `sh`; the point of the assertion is that
        // lookup succeeds through the augmented paths rather than that a
        // particular sbin directory is populated.
        assert!(binary_on_path("sh"));
        assert!(!binary_on_path("lunarwing-no-such-runtime-binary"));
    }
}
