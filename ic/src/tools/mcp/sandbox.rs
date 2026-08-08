//! Host OS sandboxing for host-local stdio MCP servers.
//!
//! Sandboxing answers **what can this code damage if it is malicious or
//! compromised**. It complements package provenance (what code is this); it
//! does not replace it. A sandboxed stdio child still runs as the LunarWing
//! service account, but with reduced process, resource, filesystem, and
//! network reach. Every restriction is opt-in per server and activation
//! fails closed when a requested restriction is unavailable.
//!
//! Portable restrictions (all unix):
//! - `max_memory_mb`, `max_cpu_seconds`, `max_file_descriptors`,
//!   `max_processes`, `max_file_size_mb`: setrlimit caps applied to the
//!   child. A request above the daemon's current hard limit fails activation
//!   instead of silently clamping. Core dumps are disabled unless
//!   `allow_core_dumps` is set.
//! - `isolate_process_group`: the child becomes a process-group leader so
//!   shutdown kills the whole group and orphaned grandchildren do not
//!   outlive the server.
//!
//! Linux-only restrictions (activation fails closed when requested on other
//! platforms):
//! - `no_new_privileges`: `PR_SET_NO_NEW_PRIVS` blocks setuid and
//!   file-capability privilege escalation.
//! - `deny_ptrace`: a seccomp filter rejects the `ptrace`,
//!   `process_vm_readv`, and `process_vm_writev` syscalls so a compromised
//!   server cannot inspect or inject into sibling processes. Note this cannot
//!   cover same-UID `/proc/<pid>/mem` reads, which the kernel authorizes by
//!   credential checks rather than by syscall.
//! - `read_only_root` + `writable_paths`: Landlock restricts the filesystem
//!   to read/execute except beneath explicitly writable paths.
//! - `network_isolated`: the child enters a fresh user and network namespace
//!   whose only interface is a down loopback, removing all network access.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::tools::mcp::config::ConfigError;

fn is_false(value: &bool) -> bool {
    !*value
}

/// OS sandbox policy for a host-local stdio MCP server.
///
/// Absent on [`crate::tools::mcp::config::McpServerConfig`] preserves legacy
/// unsandboxed behavior. All fields default to off/zero so a present but
/// empty policy still disables core dumps; combine with the explicit options
/// for real containment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpSandboxConfig {
    /// Virtual address space cap (RLIMIT_AS) in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_memory_mb: Option<u64>,
    /// CPU time cap (RLIMIT_CPU) in seconds; the child is killed at the cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cpu_seconds: Option<u64>,
    /// Open file descriptor cap (RLIMIT_NOFILE).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_descriptors: Option<u64>,
    /// Process/thread count cap (RLIMIT_NPROC, counted per real UID).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_processes: Option<u64>,
    /// Single-file size cap (RLIMIT_FSIZE) in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_size_mb: Option<u64>,
    /// Keep core dumps enabled. Default disables them (RLIMIT_CORE = 0).
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_core_dumps: bool,
    /// Start the child as a process-group leader so shutdown kills the whole
    /// group, including grandchildren the server spawned.
    #[serde(default, skip_serializing_if = "is_false")]
    pub isolate_process_group: bool,
    /// Block setuid/file-capability privilege escalation (Linux).
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_new_privileges: bool,
    /// Reject the ptrace and process_vm_* syscalls from the child via
    /// seccomp (Linux), so a compromised server cannot inspect or inject
    /// into sibling processes. Same-UID /proc/<pid>/mem reads are a kernel
    /// credential check and cannot be filtered by seccomp.
    #[serde(default, skip_serializing_if = "is_false")]
    pub deny_ptrace: bool,
    /// Landlock read/execute-only filesystem root. Only `writable_paths`
    /// remain writable (Linux, kernel >= 5.13 with Landlock enabled).
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_only_root: bool,
    /// Absolute paths that stay writable when `read_only_root` is set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writable_paths: Vec<PathBuf>,
    /// Move the child into a fresh user+network namespace with no usable
    /// interface, removing all network access (Linux, requires unprivileged
    /// user namespaces).
    #[serde(default, skip_serializing_if = "is_false")]
    pub network_isolated: bool,
}

impl McpSandboxConfig {
    /// Structural validation. Platform enforcement happens at spawn time so
    /// configurations stay portable to load.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.writable_paths.is_empty() && !self.read_only_root {
            return Err(ConfigError::InvalidConfig {
                reason: "sandbox writable_paths require read_only_root".to_string(),
            });
        }
        for path in &self.writable_paths {
            if !path.is_absolute() {
                return Err(ConfigError::InvalidConfig {
                    reason: format!("sandbox writable path must be absolute: {}", path.display()),
                });
            }
            if path.as_os_str().as_encoded_bytes().contains(&0) {
                return Err(ConfigError::InvalidConfig {
                    reason: format!(
                        "sandbox writable path contains a NUL byte: {}",
                        path.display()
                    ),
                });
            }
        }
        Ok(())
    }

    /// Whether any enabled restriction is only enforceable on Linux.
    pub fn requires_linux(&self) -> bool {
        self.no_new_privileges || self.deny_ptrace || self.read_only_root || self.network_isolated
    }

    /// Short human-readable summary for CLI and doctor output.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(mb) = self.max_memory_mb {
            parts.push(format!("memory<={mb}MiB"));
        }
        if let Some(secs) = self.max_cpu_seconds {
            parts.push(format!("cpu<={secs}s"));
        }
        if let Some(fds) = self.max_file_descriptors {
            parts.push(format!("fds<={fds}"));
        }
        if let Some(procs) = self.max_processes {
            parts.push(format!("processes<={procs}"));
        }
        if let Some(mb) = self.max_file_size_mb {
            parts.push(format!("file-size<={mb}MiB"));
        }
        if !self.allow_core_dumps {
            parts.push("no-core-dumps".to_string());
        }
        if self.isolate_process_group {
            parts.push("process-group".to_string());
        }
        if self.no_new_privileges {
            parts.push("no-new-privileges".to_string());
        }
        if self.deny_ptrace {
            parts.push("ptrace-denied".to_string());
        }
        if self.read_only_root {
            parts.push(format!(
                "read-only-root(+{} writable)",
                self.writable_paths.len()
            ));
        }
        if self.network_isolated {
            parts.push("network-isolated".to_string());
        }
        parts.join(", ")
    }
}

/// Error returned when a sandbox policy cannot be prepared or applied.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("MCP host sandboxing is only supported on unix platforms")]
    UnsupportedPlatform,
    #[error("sandbox option '{option}' requires Linux")]
    RequiresLinux { option: String },
    #[error(
        "sandbox limit '{limit}' requests {requested} but the daemon's hard limit is {hard}; raise the daemon limit first"
    )]
    LimitExceedsHardLimit {
        limit: String,
        requested: u64,
        hard: u64,
    },
    #[error("sandbox setup failed at {step}: {reason}")]
    Setup { step: String, reason: String },
}

/// Whether shutdown should SIGKILL the child's whole process group.
pub fn kills_process_group(sandbox: Option<&McpSandboxConfig>) -> bool {
    sandbox.is_some_and(|sandbox| sandbox.isolate_process_group)
}

/// Probe for Landlock support (None when unavailable or not Linux).
#[cfg(not(target_os = "linux"))]
pub fn landlock_abi_version() -> Option<u32> {
    None
}

#[cfg(target_os = "linux")]
pub use unix_impl::landlock_abi_version;

#[cfg(unix)]
pub use unix_impl::install;

#[cfg(not(unix))]
pub use fallback_impl::install;

#[cfg(not(unix))]
mod fallback_impl {
    use super::{McpSandboxConfig, SandboxError};

    /// Sandbox installation always fails closed on non-unix platforms.
    pub fn install(
        _command: &mut tokio::process::Command,
        _sandbox: &McpSandboxConfig,
    ) -> Result<(), SandboxError> {
        Err(SandboxError::UnsupportedPlatform)
    }
}

#[cfg(unix)]
mod unix_impl {
    use std::os::unix::process::CommandExt;

    use nix::sys::resource::{Resource, getrlimit, setrlimit};

    use super::{McpSandboxConfig, SandboxError};

    /// Apply a sandbox policy to a command about to spawn.
    ///
    /// Resource limits and namespace setup run in the child between fork and
    /// exec via `pre_exec`; the process group is set through the standard
    /// command API. Preparation happens before the fork so allocation and
    /// detailed errors stay in the parent.
    pub fn install(
        command: &mut tokio::process::Command,
        sandbox: &McpSandboxConfig,
    ) -> Result<(), SandboxError> {
        if sandbox.isolate_process_group {
            command.process_group(0);
        }
        if let Some(prepared) = PreparedSandbox::prepare(sandbox)? {
            // SAFETY: `apply` performs only async-signal-safe raw syscalls on
            // data prepared before the fork; no allocation or locking happens
            // between fork and exec.
            unsafe {
                command.as_std_mut().pre_exec(move || prepared.apply());
            }
        }
        Ok(())
    }

    /// Pre-fork prepared sandbox state for one child.
    struct PreparedSandbox {
        rlimits: Vec<(Resource, u64, u64)>,
        #[cfg(target_os = "linux")]
        network_isolated: bool,
        #[cfg(target_os = "linux")]
        no_new_privileges: bool,
        #[cfg(target_os = "linux")]
        deny_ptrace: bool,
        #[cfg(target_os = "linux")]
        landlock: Option<linux::PreparedLandlock>,
    }

    impl PreparedSandbox {
        fn prepare(sandbox: &McpSandboxConfig) -> Result<Option<Self>, SandboxError> {
            let rlimits = resolve_rlimits(sandbox)?;
            #[cfg(target_os = "linux")]
            {
                let landlock = prepare_landlock(sandbox)?;
                let no_new_privileges =
                    sandbox.no_new_privileges || sandbox.deny_ptrace || landlock.is_some();
                Ok(Some(Self {
                    rlimits,
                    network_isolated: sandbox.network_isolated,
                    no_new_privileges,
                    deny_ptrace: sandbox.deny_ptrace,
                    landlock,
                }))
            }
            #[cfg(not(target_os = "linux"))]
            {
                if sandbox.requires_linux() {
                    return Err(SandboxError::RequiresLinux {
                        option: sandbox.describe(),
                    });
                }
                if rlimits.is_empty() {
                    return Ok(None);
                }
                Ok(Some(Self { rlimits }))
            }
        }

        /// Runs in the child after fork, before exec. Only async-signal-safe
        /// raw syscalls are permitted here; every allocation already happened
        /// in `prepare`. Only the errno of a failure reaches the parent, so
        /// each step returns the raw OS error.
        fn apply(&self) -> std::io::Result<()> {
            for (resource, soft, hard) in &self.rlimits {
                setrlimit(*resource, *soft, *hard)
                    .map_err(|errno| std::io::Error::from_raw_os_error(errno as i32))?;
            }
            #[cfg(target_os = "linux")]
            {
                if self.network_isolated {
                    linux::enter_isolated_netns()?;
                }
                if self.no_new_privileges {
                    linux::deny_new_privileges()?;
                }
                if let Some(landlock) = &self.landlock {
                    landlock.apply()?;
                }
                // Seccomp goes last so the filter cannot interfere with the
                // sandbox setup itself.
                if self.deny_ptrace {
                    linux::deny_process_vm_syscalls()?;
                }
            }
            Ok(())
        }
    }

    /// Translate the sandbox limits into concrete setrlimit triples, failing
    /// early when a request exceeds the daemon's current hard limit.
    fn resolve_rlimits(
        sandbox: &McpSandboxConfig,
    ) -> Result<Vec<(Resource, u64, u64)>, SandboxError> {
        const MIB: u64 = 1024 * 1024;
        let mut specs: Vec<(Resource, &'static str, u64)> = Vec::new();
        if let Some(mb) = sandbox.max_memory_mb {
            specs.push((Resource::RLIMIT_AS, "max_memory_mb", mb.saturating_mul(MIB)));
        }
        if let Some(secs) = sandbox.max_cpu_seconds {
            specs.push((Resource::RLIMIT_CPU, "max_cpu_seconds", secs));
        }
        if let Some(fds) = sandbox.max_file_descriptors {
            specs.push((Resource::RLIMIT_NOFILE, "max_file_descriptors", fds));
        }
        if let Some(procs) = sandbox.max_processes {
            specs.push((Resource::RLIMIT_NPROC, "max_processes", procs));
        }
        if let Some(mb) = sandbox.max_file_size_mb {
            specs.push((
                Resource::RLIMIT_FSIZE,
                "max_file_size_mb",
                mb.saturating_mul(MIB),
            ));
        }
        if !sandbox.allow_core_dumps {
            specs.push((Resource::RLIMIT_CORE, "allow_core_dumps", 0));
        }

        let mut resolved = Vec::with_capacity(specs.len());
        for (resource, name, requested) in specs {
            let (_soft, hard) = getrlimit(resource).map_err(|errno| SandboxError::Setup {
                step: format!("getrlimit({name})"),
                reason: errno.to_string(),
            })?;
            if requested > hard {
                return Err(SandboxError::LimitExceedsHardLimit {
                    limit: name.to_string(),
                    requested,
                    hard,
                });
            }
            resolved.push((resource, requested, requested));
        }
        Ok(resolved)
    }

    /// Probe for Landlock support (None when the kernel lacks it).
    #[cfg(target_os = "linux")]
    pub fn landlock_abi_version() -> Option<u32> {
        linux::abi_version()
    }

    #[cfg(target_os = "linux")]
    fn prepare_landlock(
        sandbox: &McpSandboxConfig,
    ) -> Result<Option<linux::PreparedLandlock>, SandboxError> {
        if !sandbox.read_only_root {
            return Ok(None);
        }
        let abi = linux::abi_version().ok_or_else(|| SandboxError::Setup {
            step: "landlock".to_string(),
            reason: "kernel does not support Landlock (requires Linux >= 5.13 with the Landlock LSM enabled)".to_string(),
        })?;
        linux::PreparedLandlock::new(abi, &sandbox.writable_paths).map(Some)
    }

    /// Linux-only enforcement primitives.
    #[cfg(target_os = "linux")]
    mod linux {
        use std::ffi::{CStr, CString};
        use std::io;
        use std::os::unix::ffi::OsStrExt;
        use std::path::PathBuf;

        use super::SandboxError;

        const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
        const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

        const ACCESS_FS_ABI1: u64 = 0x1FFF; // EXECUTE..MAKE_SYM (bits 0..=12)
        const ACCESS_FS_REFER: u64 = 1 << 13; // ABI 2
        const ACCESS_FS_TRUNCATE: u64 = 1 << 14; // ABI 3
        const ACCESS_FS_IOCTL_DEV: u64 = 1 << 15; // ABI 4
        const ACCESS_FS_READ_ONLY: u64 = (1 << 0) | (1 << 2) | (1 << 3); // EXEC|READ_FILE|READ_DIR

        #[repr(C)]
        struct LandlockRulesetAttr {
            handled_access_fs: u64,
        }

        #[repr(C)]
        struct LandlockPathBeneathAttr {
            allowed_access: u64,
            parent_fd: std::os::fd::RawFd,
        }

        /// Highest Landlock ABI the running kernel supports, if any.
        pub fn abi_version() -> Option<u32> {
            // SAFETY: null ruleset pointer with size 0 and the VERSION flag
            // only queries the supported ABI; no state is created.
            let version = unsafe {
                libc::syscall(
                    libc::SYS_landlock_create_ruleset,
                    std::ptr::null::<u8>(),
                    0usize,
                    LANDLOCK_CREATE_RULESET_VERSION,
                )
            };
            (version > 0).then_some(version as u32)
        }

        fn fs_mask_for_abi(abi: u32) -> u64 {
            let mut mask = ACCESS_FS_ABI1;
            if abi >= 2 {
                mask |= ACCESS_FS_REFER;
            }
            if abi >= 3 {
                mask |= ACCESS_FS_TRUNCATE;
            }
            if abi >= 4 {
                mask |= ACCESS_FS_IOCTL_DEV;
            }
            mask
        }

        fn last_os_error() -> io::Error {
            io::Error::last_os_error()
        }

        /// Move the child into a fresh user+network namespace whose only
        /// interface is a down loopback.
        pub(super) fn enter_isolated_netns() -> io::Result<()> {
            // Capture the identity before unshare: inside a mapless user
            // namespace the uid reads back as the overflow uid (65534).
            // SAFETY: getuid/getgid cannot fail.
            let uid = unsafe { libc::getuid() };
            let gid = unsafe { libc::getgid() };
            // SAFETY: creates namespaces for the calling (about to exec)
            // process only; no buffers are involved.
            if unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET) } != 0 {
                return Err(last_os_error());
            }
            write_proc_self(c"/proc/self/setgroups", b"deny")?;
            let mut buffer = [0_u8; 48];
            write_proc_self(c"/proc/self/uid_map", format_id_map(&mut buffer, uid))?;
            write_proc_self(c"/proc/self/gid_map", format_id_map(&mut buffer, gid))?;
            Ok(())
        }

        /// PR_SET_NO_NEW_PRIVS: block setuid and file-capability escalation.
        pub(super) fn deny_new_privileges() -> io::Result<()> {
            // SAFETY: scalar-only prctl; applies to the calling process.
            if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
                return Err(last_os_error());
            }
            Ok(())
        }

        /// Seccomp filter rejecting ptrace and process_vm_* syscalls so the
        /// child cannot inspect or inject into sibling processes. Requires
        /// no_new_privileges to already be set.
        pub(super) fn deny_process_vm_syscalls() -> io::Result<()> {
            const DENY: u32 =
                libc::SECCOMP_RET_ERRNO | (libc::EPERM as u32 & libc::SECCOMP_RET_DATA);
            const ALLOW: u32 = libc::SECCOMP_RET_ALLOW;
            const fn load_nr() -> libc::sock_filter {
                libc::sock_filter {
                    code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                    jt: 0,
                    jf: 0,
                    k: 0, // offsetof(seccomp_data, nr)
                }
            }
            const fn jump_if_eq(syscall: u32, jt: u8, jf: u8) -> libc::sock_filter {
                libc::sock_filter {
                    code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                    jt,
                    jf,
                    k: syscall,
                }
            }
            const fn ret(value: u32) -> libc::sock_filter {
                libc::sock_filter {
                    code: (libc::BPF_RET | libc::BPF_K) as u16,
                    jt: 0,
                    jf: 0,
                    k: value,
                }
            }
            let mut filter = [
                load_nr(),
                jump_if_eq(libc::SYS_ptrace as u32, 0, 1),
                ret(DENY),
                jump_if_eq(libc::SYS_process_vm_readv as u32, 0, 1),
                ret(DENY),
                jump_if_eq(libc::SYS_process_vm_writev as u32, 0, 1),
                ret(DENY),
                ret(ALLOW),
            ];
            let prog = libc::sock_fprog {
                len: filter.len() as u16,
                filter: filter.as_mut_ptr(),
            };
            // SAFETY: filter outlives the call; prog points at a valid
            // classic-BPF program of exactly `len` instructions.
            if unsafe { libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &prog) } != 0 {
                return Err(last_os_error());
            }
            Ok(())
        }

        fn write_proc_self(path: &CStr, content: &[u8]) -> io::Result<()> {
            // SAFETY: path is a static NUL-terminated C string.
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
            if fd < 0 {
                return Err(last_os_error());
            }
            let result = write_all_fd(fd, content);
            // SAFETY: closing an fd we opened; failure is harmless here.
            unsafe {
                libc::close(fd);
            }
            result
        }

        fn write_all_fd(fd: std::os::fd::RawFd, mut content: &[u8]) -> io::Result<()> {
            while !content.is_empty() {
                // SAFETY: fd is open for writing and content is a valid slice.
                let written = unsafe { libc::write(fd, content.as_ptr().cast(), content.len()) };
                if written < 0 {
                    let error = last_os_error();
                    if error.raw_os_error() == Some(libc::EINTR) {
                        continue;
                    }
                    return Err(error);
                }
                content = &content[written as usize..];
            }
            Ok(())
        }

        /// Format "<id> <id> 1\n" into a stack buffer without allocating;
        /// allocation is not async-signal-safe in the fork/exec window.
        fn format_id_map<'a>(buffer: &'a mut [u8; 48], id: libc::uid_t) -> &'a [u8] {
            let mut pos = write_u32(buffer, id);
            buffer[pos] = b' ';
            pos += 1;
            pos += write_u32(&mut buffer[pos..], id);
            buffer[pos] = b' ';
            pos += 1;
            buffer[pos] = b'1';
            buffer[pos + 1] = b'\n';
            &buffer[..pos + 2]
        }

        fn write_u32(buffer: &mut [u8], mut value: u32) -> usize {
            let mut digits = [0_u8; 10];
            let mut len = 0;
            loop {
                digits[len] = b'0' + (value % 10) as u8;
                value /= 10;
                len += 1;
                if value == 0 {
                    break;
                }
            }
            for (slot, digit) in buffer.iter_mut().zip(digits[..len].iter().rev()) {
                *slot = *digit;
            }
            len
        }

        /// Prepared Landlock policy: read/execute root plus writable subtrees.
        pub(super) struct PreparedLandlock {
            abi: u32,
            root: CString,
            writable: Vec<CString>,
        }

        impl PreparedLandlock {
            pub(super) fn new(abi: u32, writable_paths: &[PathBuf]) -> Result<Self, SandboxError> {
                let to_cstring = |path: &std::path::Path| {
                    CString::new(path.as_os_str().as_bytes()).map_err(|_| SandboxError::Setup {
                        step: "landlock".to_string(),
                        reason: format!("path contains a NUL byte: {}", path.display()),
                    })
                };
                let root = to_cstring(std::path::Path::new("/"))?;
                let mut writable = Vec::with_capacity(writable_paths.len());
                for path in writable_paths {
                    writable.push(to_cstring(path)?);
                }
                Ok(Self {
                    abi,
                    root,
                    writable,
                })
            }

            /// Apply the ruleset in the fork/exec window. Requires
            /// no_new_privileges to already be set.
            pub(super) fn apply(&self) -> io::Result<()> {
                let handled = fs_mask_for_abi(self.abi);
                let attr = LandlockRulesetAttr {
                    handled_access_fs: handled,
                };
                // SAFETY: attr points at a valid ruleset attribute struct.
                let ruleset = unsafe {
                    libc::syscall(
                        libc::SYS_landlock_create_ruleset,
                        &raw const attr,
                        std::mem::size_of::<LandlockRulesetAttr>(),
                        0,
                    )
                };
                if ruleset < 0 {
                    return Err(last_os_error());
                }
                let ruleset = ruleset as std::os::fd::RawFd;
                let result = self.populate(ruleset, handled);
                // SAFETY: closing our own ruleset fd; failure is harmless.
                unsafe {
                    libc::close(ruleset);
                }
                result
            }

            fn populate(&self, ruleset: std::os::fd::RawFd, handled: u64) -> io::Result<()> {
                add_path_rule(ruleset, &self.root, ACCESS_FS_READ_ONLY & handled)?;
                for path in &self.writable {
                    add_path_rule(ruleset, path, handled)?;
                }
                // SAFETY: ruleset is a live Landlock fd; no_new_privileges is set.
                let result = unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset, 0) };
                if result != 0 {
                    return Err(last_os_error());
                }
                Ok(())
            }
        }

        fn add_path_rule(ruleset: std::os::fd::RawFd, path: &CStr, rights: u64) -> io::Result<()> {
            // SAFETY: path is a NUL-terminated string prepared before the fork.
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
            if fd < 0 {
                return Err(last_os_error());
            }
            let attr = LandlockPathBeneathAttr {
                allowed_access: rights,
                parent_fd: fd,
            };
            // SAFETY: ruleset is a live Landlock fd and attr is a valid
            // path-beneath rule for this call.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_landlock_add_rule,
                    ruleset,
                    LANDLOCK_RULE_PATH_BENEATH,
                    &raw const attr,
                    0,
                )
            };
            // SAFETY: closing our own O_PATH fd; failure is harmless.
            unsafe {
                libc::close(fd);
            }
            if result != 0 {
                return Err(last_os_error());
            }
            Ok(())
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn fs_mask_scales_with_abi() {
                assert_eq!(fs_mask_for_abi(1), ACCESS_FS_ABI1);
                assert!(fs_mask_for_abi(2) & ACCESS_FS_REFER != 0);
                assert!(fs_mask_for_abi(3) & ACCESS_FS_TRUNCATE != 0);
                assert!(fs_mask_for_abi(4) & ACCESS_FS_IOCTL_DEV != 0);
                assert_eq!(fs_mask_for_abi(4), fs_mask_for_abi(9));
            }

            #[test]
            fn id_map_formats_without_allocation() {
                let mut buffer = [0_u8; 48];
                assert_eq!(format_id_map(&mut buffer, 1000), b"1000 1000 1\n");
                assert_eq!(format_id_map(&mut buffer, 0), b"0 0 1\n");
            }

            #[test]
            fn abi_probe_is_consistent() {
                // Either the kernel supports Landlock (ABI >= 1) or the probe
                // reports None; a half-broken probe must not pass.
                if let Some(abi) = abi_version() {
                    assert!(abi >= 1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid_and_minimal() {
        let config = McpSandboxConfig::default();
        config.validate().expect("default sandbox validates");
        assert!(!config.requires_linux());
        assert_eq!(config.describe(), "no-core-dumps");
    }

    #[test]
    fn writable_paths_require_read_only_root() {
        let config = McpSandboxConfig {
            writable_paths: vec![PathBuf::from("/tmp/data")],
            ..McpSandboxConfig::default()
        };
        let error = config.validate().expect_err("must fail");
        assert!(error.to_string().contains("read_only_root"));
    }

    #[test]
    fn writable_paths_must_be_absolute() {
        let config = McpSandboxConfig {
            read_only_root: true,
            writable_paths: vec![PathBuf::from("relative/dir")],
            ..McpSandboxConfig::default()
        };
        let error = config.validate().expect_err("must fail");
        assert!(error.to_string().contains("absolute"));
    }

    #[test]
    fn read_only_root_with_absolute_writable_paths_validates() {
        let config = McpSandboxConfig {
            read_only_root: true,
            writable_paths: vec![PathBuf::from("/var/lib/mcp")],
            ..McpSandboxConfig::default()
        };
        config.validate().expect("valid policy");
        assert!(config.requires_linux());
        assert!(config.describe().contains("read-only-root(+1 writable)"));
    }

    #[test]
    fn serde_roundtrip_skips_defaults() {
        let config = McpSandboxConfig::default();
        let json = serde_json::to_value(&config).expect("serialize");
        assert_eq!(json, serde_json::json!({}));

        let config = McpSandboxConfig {
            max_memory_mb: Some(256),
            isolate_process_group: true,
            no_new_privileges: true,
            ..McpSandboxConfig::default()
        };
        let json = serde_json::to_value(&config).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "max_memory_mb": 256,
                "isolate_process_group": true,
                "no_new_privileges": true
            })
        );
        let parsed: McpSandboxConfig = serde_json::from_value(json).expect("deserialize");
        assert_eq!(parsed, config);
    }

    #[test]
    fn kills_process_group_only_when_isolated() {
        assert!(!kills_process_group(None));
        let plain = McpSandboxConfig::default();
        assert!(!kills_process_group(Some(&plain)));
        let isolated = McpSandboxConfig {
            isolate_process_group: true,
            ..McpSandboxConfig::default()
        };
        assert!(kills_process_group(Some(&isolated)));
    }
}
