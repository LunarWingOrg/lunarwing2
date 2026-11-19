//! Per-execution Unix transport to the host's loopback HTTP proxy.
//!
//! Enforced containers retain `--network none`; legacy Podman containers keep
//! their normal advisory network. Only this socket's directory is mounted for
//! proxy transport; neither the container runtime socket nor the host proxy's
//! TCP listener is exposed to the container.
//!
//! # Socket placement (!1044 review finding 1)
//!
//! The socket path must be resolvable by **both** the daemon process and the
//! container runtime's mount namespace. The daemon may run under a systemd
//! unit with `PrivateTmp=true`, and on multi-tenant hosts the workload
//! spawner runs the container CLI from a manager-forked transient service in
//! a *different* mount namespace. A socket created under the daemon's private
//! `/tmp` would then be invisible at `podman/docker create` time ("statfs
//! ...: no such file or directory"). `$XDG_RUNTIME_DIR` (`/run/user/<uid>`)
//! is shared per-user and never private-tmp'd, so it is the preferred parent.
//! The process temp dir remains the fallback for contexts where the runtime
//! dir does not exist or is not ours.
//!
//! # SELinux (WI#641)
//!
//! A container's `connect()` to the relay passes two SELinux checks: `write`
//! on the socket inode (`sock_file`, which the runtime's `:z` relabel covers)
//! and `connectto` on the **listening socket's own context**, which comes from
//! the task that created it. The measured denial (WI#641 note 19289) is the
//! second: `container_t` to the daemon's `unconfined_t`. Relabeling the inode
//! cannot change it.
//!
//! The listener is therefore created under the context a container's own
//! sockets carry ([`RELAY_SOCKET_CONTEXTS`]), so the container's connect is the
//! same-type check stock policy already allows. Before any container starts,
//! [`probe_labeled_connection`] replays that connect from a socket created
//! under a container-shaped source context, under the host's own policy
//! (levels included), and a refusal falls back to the previous unlabeled
//! socket. A host without selinuxfs starts no label thread and binds exactly
//! as before. The sandbox is never made unconfined and no policy is changed.
//!
//! What the label opens: at the SELinux layer, on the targeted policy, any
//! process of type `container_t`, at any level, passes the socket check for
//! this listener. Before the label, no container did on an enforcing host.
//! Other containers are kept out by the same two facts as on a host without
//! SELinux: the private `0700` parent directory, and that only this run
//! mounts `channel`.

use std::path::{Path, PathBuf};

use crate::sandbox::error::{Result, SandboxError};

pub(crate) struct ProxyRelay {
    _directory: tempfile::TempDir,
    channel: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl ProxyRelay {
    #[cfg(target_os = "linux")]
    pub async fn start(port: u16) -> Result<Self> {
        let (directory, channel, listener) =
            relay_setup().map_err(|e| SandboxError::ProxyError {
                reason: format!("cannot create isolated proxy transport: {e}"),
            })?;
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept(), if connections.len() < 32 => {
                        let Ok((mut stream, _)) = accepted else { break };
                        connections.spawn(async move {
                            let _ = tokio::time::timeout(std::time::Duration::from_secs(120), async {
                                let mut proxy = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await?;
                                tokio::io::copy_bidirectional(&mut stream, &mut proxy).await
                            }).await;
                        });
                    }
                }
            }
            // Task abort aborts every open connection, including CONNECT.
        });
        Ok(Self {
            _directory: directory,
            channel,
            task,
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub async fn start(_port: u16) -> Result<Self> {
        Err(SandboxError::Config {
            reason: "the proxy relay requires a local Linux container runtime".into(),
        })
    }

    pub fn path(&self) -> &Path {
        // The parent directory is retained until the execution supervisor ends.
        &self.channel
    }
}

/// Add path context to an I/O failure; a bare errno cannot say which of the
/// four host-side nodes failed.
#[cfg(target_os = "linux")]
fn at_path(context: &str, path: &Path, error: std::io::Error) -> std::io::Error {
    std::io::Error::new(
        error.kind(),
        format!("{context} {}: {error}", path.display()),
    )
}

// ---------------------------------------------------------------------------
// SELinux listener labeling (WI#641)
// ---------------------------------------------------------------------------

/// Contexts a sandbox container's own sockets carry, in the order tried: the
/// stock container type at the base level, then at the full category range.
///
/// On the targeted policy only the type decides `connectto`, so the first
/// candidate is the one expected to be used. The second is kept for a policy
/// that also compares the two levels. That such a policy accepts it was not
/// verified against any policy source; [`probe_labeled_connection`] decides it
/// on each host. Neither grants more than a container already has for its own
/// sockets.
#[cfg(target_os = "linux")]
const RELAY_SOCKET_CONTEXTS: [&str; 2] = [
    "system_u:system_r:container_t:s0",
    "system_u:system_r:container_t:s0:c0.c1023",
];

/// Source contexts the self-test connects from: a runtime-assigned category
/// pair, then the base level. The self-test therefore exercises the
/// container's own `connectto` (source socket label against listener label,
/// under whatever the host's policy requires of the two levels) before any
/// container can fail on it.
#[cfg(target_os = "linux")]
const SIMULATED_CLIENT_CONTEXTS: [&str; 2] = [
    "system_u:system_r:container_t:s0:c533,c991",
    "system_u:system_r:container_t:s0",
];

/// Per-task SELinux create context of the calling thread.
///
/// `/proc/self/attr/sockcreate` is deliberately not tried as a second node:
/// `/proc/self` names the thread-group leader, and the kernel lets a task
/// write only its own attributes. From the label thread it always answers
/// `EACCES`, which would replace this node's real error in the report.
#[cfg(target_os = "linux")]
const SOCKCREATE_NODE: &str = "/proc/thread-self/attr/sockcreate";

#[cfg(target_os = "linux")]
const SELINUX_ENFORCE: &str = "/sys/fs/selinux/enforce";

/// The two host nodes that decide whether a labeled listener is possible: the
/// selinuxfs `enforce` value and this task's socket create context.
///
/// Injectable so the labeling path itself — candidate order, the cleanup after
/// a refused candidate, and the unlabeled fallback — runs in tests on a host
/// without SELinux (the test-only `at` constructor names ordinary files
/// instead). The daemon always uses [`LabelNodes::defaults`].
#[cfg(target_os = "linux")]
#[derive(Clone)]
struct LabelNodes {
    enforce: PathBuf,
    sockcreate: PathBuf,
}

#[cfg(target_os = "linux")]
impl LabelNodes {
    fn defaults() -> Self {
        Self {
            enforce: PathBuf::from(SELINUX_ENFORCE),
            sockcreate: PathBuf::from(SOCKCREATE_NODE),
        }
    }

    /// Another pair of nodes, so the labeling path can be tested on a host
    /// without SELinux.
    #[cfg(test)]
    fn at(enforce: impl Into<PathBuf>, sockcreate: impl Into<PathBuf>) -> Self {
        Self {
            enforce: enforce.into(),
            sockcreate: sockcreate.into(),
        }
    }
}

/// Whether SELinux is enabled on this host (`0` permissive, `1` enforcing).
/// A host without selinuxfs — AppArmor, or none — keeps the old socket.
#[cfg(target_os = "linux")]
fn selinux_enabled(nodes: &LabelNodes) -> bool {
    selinux_enabled_at(&nodes.enforce)
}

/// [`selinux_enabled`] for a given enforce node.
#[cfg(target_os = "linux")]
fn selinux_enabled_at(enforce: &Path) -> bool {
    std::fs::read_to_string(enforce)
        .map(|value| matches!(value.trim(), "0" | "1"))
        .unwrap_or(false)
}

/// Pin the calling thread's next socket to `context`. The error is the
/// kernel's own answer for this thread's attribute.
///
/// The attribute belongs to the task, not the process, so no other thread in
/// the daemon can pick it up; [`with_socket_context`] clears it as soon as the
/// socket exists.
#[cfg(target_os = "linux")]
fn set_socket_create_context(nodes: &LabelNodes, context: &str) -> std::io::Result<()> {
    std::fs::write(&nodes.sockcreate, context)
}

/// Restore the default create context on the calling thread. A newline is the
/// kernel's "no context specified" form; an empty write may never reach the
/// attribute handler.
#[cfg(target_os = "linux")]
fn clear_socket_create_context(nodes: &LabelNodes) {
    let _ = std::fs::write(&nodes.sockcreate, "\n");
}

/// Run `create` with this thread's socket create context pinned to `context`,
/// clearing the attribute afterwards even when creation fails.
#[cfg(target_os = "linux")]
fn with_socket_context<T>(
    nodes: &LabelNodes,
    context: &str,
    create: impl FnOnce() -> std::io::Result<T>,
) -> std::io::Result<T> {
    set_socket_create_context(nodes, context)?;
    let created = create();
    clear_socket_create_context(nodes);
    created
}

/// Bind the relay socket and make it connectable by an unmapped container uid.
#[cfg(target_os = "linux")]
fn bind_relay_socket(socket: &Path) -> std::io::Result<std::os::unix::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;

    let listener = std::os::unix::net::UnixListener::bind(socket)
        .map_err(|error| at_path("cannot bind relay socket", socket, error))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o666))
        .map_err(|error| at_path("cannot set relay socket mode", socket, error))?;
    Ok(listener)
}

/// Prove one candidate listener answers the connect a sandbox container will
/// make: `connectto` from a socket created under `client_context`, `accept`,
/// and one byte each way. Every step is an SELinux check against the
/// listener's own context, so a refusal here means a labeled socket would
/// break the relay instead of fixing it.
#[cfg(target_os = "linux")]
fn probe_labeled_connection(
    nodes: &LabelNodes,
    socket: &Path,
    listener: &std::os::unix::net::UnixListener,
    client_context: &str,
) -> std::io::Result<()> {
    use std::io::{Read, Write};

    let timeout = std::time::Duration::from_secs(2);
    let mut client = with_socket_context(nodes, client_context, || {
        std::os::unix::net::UnixStream::connect(socket)
    })?;
    client.set_read_timeout(Some(timeout))?;
    client.set_write_timeout(Some(timeout))?;
    let (mut server, _) = listener.accept()?;
    server.set_read_timeout(Some(timeout))?;
    server.set_write_timeout(Some(timeout))?;
    let mut byte = [0u8; 1];
    client.write_all(b"!")?;
    server.read_exact(&mut byte)?;
    server.write_all(&byte)?;
    client.read_exact(&mut byte)?;
    Ok(())
}

/// Bind the relay listener: labeled for containers where SELinux is enabled
/// and a label is usable, unlabeled otherwise.
///
/// SELinux is checked here, on the calling thread, so a host without
/// selinuxfs starts no thread and binds exactly as it did before WI#641.
/// Every refusal of the labeled attempt, a label thread that cannot be
/// started included, ends at the same unlabeled bind.
///
/// Returns the listener and the context it was created under, or `None` when
/// the default context was kept.
#[cfg(target_os = "linux")]
fn bind_relay_listener(
    socket: &Path,
) -> std::io::Result<(tokio::net::UnixListener, Option<&'static str>)> {
    bind_relay_listener_at(&LabelNodes::defaults(), socket)
}

/// [`bind_relay_listener`] against an explicit pair of label nodes.
///
/// The decisions — whether labeling is attempted at all, which candidate wins,
/// and where every refusal ends — are the same for any pair; the injectable
/// pair is what lets a host without SELinux exercise them.
#[cfg(target_os = "linux")]
fn bind_relay_listener_at(
    nodes: &LabelNodes,
    socket: &Path,
) -> std::io::Result<(tokio::net::UnixListener, Option<&'static str>)> {
    if selinux_enabled(nodes)
        && let Some((listener, context)) = bind_labeled_on_own_thread(nodes, socket)
    {
        return Ok((to_tokio(listener)?, Some(context)));
    }
    Ok((to_tokio(bind_relay_socket(socket)?)?, None))
}

/// Run [`bind_labeled`] on a dedicated thread, so the per-task create context
/// can never leak onto a runtime worker.
///
/// A thread that cannot be started, or that panics, is one more refusal: it
/// is reported like the others and the caller keeps the unlabeled socket.
#[cfg(target_os = "linux")]
fn bind_labeled_on_own_thread(
    nodes: &LabelNodes,
    socket: &Path,
) -> Option<(std::os::unix::net::UnixListener, &'static str)> {
    let owned_socket = socket.to_path_buf();
    let owned_nodes = nodes.clone();
    let outcome = match std::thread::Builder::new()
        .name("relay-label".into())
        .spawn(move || bind_labeled(&owned_nodes, &owned_socket))
    {
        Ok(handle) => handle.join().unwrap_or_else(|_| {
            // The thread may have bound a candidate before it died.
            let _ = std::fs::remove_file(socket);
            Err(vec!["the relay label thread panicked".to_string()])
        }),
        Err(error) => Err(vec![format!(
            "cannot start the relay label thread: {error}"
        )]),
    };
    match outcome {
        Ok(labeled) => Some(labeled),
        Err(reasons) => {
            report_label_refusal(&reasons);
            None
        }
    }
}

/// Try each candidate context on the calling thread (the dedicated label
/// thread), keeping the first that survives a container-shaped connect.
///
/// `Err` holds one reason per refused candidate, in the order tried, each
/// with the kernel's own error. A refused candidate leaves no socket behind.
#[cfg(target_os = "linux")]
fn bind_labeled(
    nodes: &LabelNodes,
    socket: &Path,
) -> std::result::Result<(std::os::unix::net::UnixListener, &'static str), Vec<String>> {
    let mut refused = Vec::with_capacity(RELAY_SOCKET_CONTEXTS.len());
    for context in RELAY_SOCKET_CONTEXTS {
        if let Err(error) = set_socket_create_context(nodes, context) {
            refused.push(format!("{context}: cannot set sockcreate: {error}"));
            continue;
        }
        let bound = bind_relay_socket(socket);
        clear_socket_create_context(nodes);
        // A refusal to create under this context is the same fallback as an
        // unwritable attribute; the unlabeled attempt reports real errors.
        let listener = match bound {
            Ok(listener) => listener,
            Err(error) => {
                let _ = std::fs::remove_file(socket);
                refused.push(format!("{context}: labeled bind refused: {error}"));
                continue;
            }
        };
        match SIMULATED_CLIENT_CONTEXTS
            .iter()
            .try_for_each(|client| probe_labeled_connection(nodes, socket, &listener, client))
        {
            Ok(()) => return Ok((listener, context)),
            Err(error) => {
                refused.push(format!(
                    "{context}: container-shaped connect refused: {error}"
                ));
                drop(listener);
                let _ = std::fs::remove_file(socket);
            }
        }
    }
    Err(refused)
}

/// One line for the log: every refused candidate with its reason.
#[cfg(target_os = "linux")]
fn label_refusal_summary(reasons: &[String]) -> String {
    reasons.join("; ")
}

/// True for the first caller only.
#[cfg(target_os = "linux")]
fn first_refusal(reported: &std::sync::atomic::AtomicBool) -> bool {
    !reported.swap(true, std::sync::atomic::Ordering::Relaxed)
}

/// Report why no container label is usable: as a warning once per process,
/// at debug level afterwards.
///
/// `relay_setup` runs for every sandbox execution, and the labeled attempt is
/// repeated each time so that a corrected policy takes effect without a
/// daemon restart. Only the warning is limited, because a host that refuses
/// every label would otherwise log it on every shell call. An enforcing host
/// whose policy denies a step still records that denial in its audit log on
/// every call.
#[cfg(target_os = "linux")]
fn report_label_refusal(reasons: &[String]) {
    static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    let reasons = label_refusal_summary(reasons);
    if first_refusal(&REPORTED) {
        tracing::warn!(
            reasons = %reasons,
            "no container label is usable for the relay socket; keeping the unlabeled socket \
             (warned once per process; later refusals are logged at debug level)"
        );
    } else {
        tracing::debug!(
            reasons = %reasons,
            "no container label is usable for the relay socket; keeping the unlabeled socket"
        );
    }
}

#[cfg(target_os = "linux")]
fn to_tokio(
    listener: std::os::unix::net::UnixListener,
) -> std::io::Result<tokio::net::UnixListener> {
    listener.set_nonblocking(true)?;
    tokio::net::UnixListener::from_std(listener)
}

/// Build the isolated relay socket directory: a unique private parent dir, a
/// world-traversable `channel` child (only it is bind-mounted, so a rootless
/// keep-id user and a Docker-spawned UID can both traverse it), and a
/// world-connectable socket inside it.
#[cfg(target_os = "linux")]
fn relay_setup() -> std::io::Result<(tempfile::TempDir, PathBuf, tokio::net::UnixListener)> {
    use std::os::unix::fs::PermissionsExt;

    let base = relay_parent_dir()?;
    let directory = tempfile::Builder::new()
        .prefix("lw-proxy-")
        .tempdir_in(&base)
        .map_err(|error| at_path("cannot create relay directory in", &base, error))?;
    // `tempfile` creates at 0777 & ~umask; only the `channel` child is meant to
    // be reachable by another uid, so the parent stays private.
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
        .map_err(|error| at_path("cannot set relay directory mode", directory.path(), error))?;
    let channel = directory.path().join("channel");
    std::fs::create_dir(&channel)
        .map_err(|error| at_path("cannot create relay channel", &channel, error))?;
    std::fs::set_permissions(&channel, std::fs::Permissions::from_mode(0o711))
        .map_err(|error| at_path("cannot set relay channel mode", &channel, error))?;
    let socket = channel.join("proxy.sock");
    let (listener, label) = bind_relay_listener(&socket)?;
    // A host connect would queue an upstream client without testing the
    // container's mount, UID mapping or security context; that is what
    // `probe_labeled_connection` replays instead, when a label was applied.
    tracing::debug!(
        socket = %socket.display(),
        channel_mode = %format!("{:04o}", mode_of(&channel)),
        socket_mode = %format!("{:04o}", mode_of(&socket)),
        label = label.unwrap_or("daemon"),
        "proxy relay ready"
    );
    Ok((directory, channel, listener))
}

#[cfg(target_os = "linux")]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o7777)
        .unwrap_or(0)
}

/// Preferred parent for relay sockets: `$XDG_RUNTIME_DIR` (shared per-user,
/// never private-tmp'd) when it exists and belongs to our euid. `/run/user/<uid>`
/// is shared across the user manager's mount namespaces — daemon,
/// transient-service workloads, and rootless podman state — so relay sockets
/// remain resolvable even when the daemon runs with `PrivateTmp=true` and the
/// container CLI runs in a different mount namespace (manager-forked transient
/// services). Falls back to the process temp dir otherwise.
#[cfg(target_os = "linux")]
pub(crate) fn relay_parent_dir() -> std::io::Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;

    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        let dir = PathBuf::from(&xdg);
        if dir.is_absolute()
            && let Ok(meta) = std::fs::metadata(&dir)
            && meta.is_dir()
            && meta.uid() == nix::unistd::geteuid().as_raw()
        {
            return Ok(dir);
        }
    }
    Ok(std::env::temp_dir())
}

impl Drop for ProxyRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn relay_drop_removes_socket_and_closes_live_connections() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = ProxyRelay::start(listener.local_addr().unwrap().port())
            .await
            .unwrap();
        let path = relay.path().join("proxy.sock");
        let mut client = tokio::net::UnixStream::connect(&path).await.unwrap();
        let (mut upstream, _) = listener.accept().await.unwrap();
        upstream.write_all(b"hello").await.unwrap();
        let mut bytes = [0; 5];
        client.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"hello");
        let local = relay.path().join("proxy.sock");
        drop(relay);
        assert!(!local.exists());
        let read = tokio::time::timeout(std::time::Duration::from_secs(2), client.read(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read, 0, "relay cancellation must close active streams");
    }

    /// WI#641: the container connects as an unmapped UID, so the channel must
    /// stay traversable and the socket connectable under DAC regardless of
    /// its owner, without broadening access to the private host parent.
    #[tokio::test]
    async fn relay_channel_is_world_traversable_and_socket_world_connectable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = ProxyRelay::start(listener.local_addr().unwrap().port())
            .await
            .unwrap();
        let channel = relay.path();
        let socket = channel.join("proxy.sock");
        assert_eq!(
            std::fs::metadata(channel).unwrap().permissions().mode() & 0o777,
            0o711
        );
        assert_eq!(
            std::fs::metadata(&socket).unwrap().permissions().mode() & 0o7777,
            0o666
        );
        // The private parent keeps the socket reachable for this user only;
        // the channel itself is the sole mounted node.
        let parent = channel.parent().unwrap();
        assert_eq!(
            std::fs::metadata(parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    /// WI#641: only a readable enforce node holding `0` or `1` means SELinux
    /// is enabled. Anything else must keep the label thread from starting.
    #[test]
    fn selinux_is_enabled_only_by_an_enforce_node_holding_0_or_1() {
        let directory = tempfile::tempdir().unwrap();
        let node = directory.path().join("enforce");
        assert!(!selinux_enabled_at(&node), "a missing node is not SELinux");
        for (content, expected) in [("1", true), ("0\n", true), ("", false), ("2", false)] {
            std::fs::write(&node, content).unwrap();
            assert_eq!(selinux_enabled_at(&node), expected, "enforce={content:?}");
        }
    }

    /// WI#641: a refused labeled attempt names every candidate with its own
    /// reason, in the order tried, and leaves no socket behind. The outcome
    /// itself depends on the kernel and its policy, so both are accepted.
    #[test]
    fn labeled_bind_reports_one_reason_per_refused_candidate() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("proxy.sock");
        match bind_labeled(&LabelNodes::defaults(), &socket) {
            Ok((_listener, context)) => {
                assert!(RELAY_SOCKET_CONTEXTS.contains(&context));
                assert!(socket.exists());
            }
            Err(reasons) => {
                assert_eq!(reasons.len(), RELAY_SOCKET_CONTEXTS.len());
                for (reason, context) in reasons.iter().zip(RELAY_SOCKET_CONTEXTS) {
                    assert!(reason.starts_with(&format!("{context}: ")), "{reason}");
                }
                assert!(
                    !socket.exists(),
                    "a refused candidate must not leave a socket"
                );
                let summary = label_refusal_summary(&reasons);
                assert!(
                    reasons
                        .iter()
                        .all(|reason| summary.contains(reason.as_str()))
                );
            }
        }
    }

    /// WI#641 (AGK-1): the choices of the labeling path itself, on a host
    /// without SELinux. The first candidate must be the one bound and
    /// returned, the create context must be cleared afterwards, and the
    /// socket must stay world-connectable.
    #[tokio::test]
    async fn labeled_path_binds_under_the_first_candidate_and_clears_the_attribute() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("proxy.sock");
        let attribute = directory.path().join("sockcreate");
        std::fs::write(&attribute, "").unwrap();
        let nodes = LabelNodes::at(directory.path().join("enforce"), &attribute);
        std::fs::write(&nodes.enforce, "1").unwrap();

        let (listener, label) = bind_relay_listener_at(&nodes, &socket).unwrap();
        assert_eq!(
            label,
            Some(RELAY_SOCKET_CONTEXTS[0]),
            "the first candidate is the one tried first"
        );
        assert_eq!(
            std::fs::read_to_string(&attribute).unwrap(),
            "\n",
            "a successful labeled bind must clear the create context"
        );
        assert_eq!(
            std::fs::metadata(&socket).unwrap().permissions().mode() & 0o7777,
            0o666
        );
        let mut client = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (_accepted, _) = listener.accept().await.unwrap();
        client.write_all(b"x").await.unwrap();
    }

    /// WI#641 (AGK-1): the fallback is decided by the node values, not by the
    /// host: an unwritable attribute refuses every candidate, each with its
    /// own reason, and the old unlabeled socket is bound in their place.
    #[tokio::test]
    async fn a_refused_label_falls_back_to_the_unlabeled_socket() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("proxy.sock");
        // A node under a missing directory: every write fails, so both
        // candidates refuse before any socket is attempted.
        let nodes = LabelNodes::at(
            directory.path().join("enforce"),
            directory.path().join("missing").join("sockcreate"),
        );
        std::fs::write(&nodes.enforce, "1").unwrap();

        let reasons = bind_labeled(&nodes, &socket).unwrap_err();
        assert_eq!(reasons.len(), RELAY_SOCKET_CONTEXTS.len());
        for (reason, context) in reasons.iter().zip(RELAY_SOCKET_CONTEXTS) {
            assert!(reason.starts_with(&format!("{context}: ")), "{reason}");
            assert!(reason.contains("cannot set sockcreate"), "{reason}");
        }
        assert!(
            !socket.exists(),
            "a refused candidate must not leave a socket"
        );

        let (listener, label) = bind_relay_listener_at(&nodes, &socket).unwrap();
        assert_eq!(label, None, "a refused label must keep the daemon socket");
        assert_eq!(
            std::fs::metadata(&socket).unwrap().permissions().mode() & 0o7777,
            0o666
        );
        let mut client = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (_accepted, _) = listener.accept().await.unwrap();
        client.write_all(b"x").await.unwrap();
    }

    /// WI#641 (AGK-1): a label the kernel accepts but that cannot own a socket
    /// is refused with the kernel's error, and the obstacle is not replaced.
    #[test]
    fn a_refused_labeled_bind_reports_the_kernel_error() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("proxy.sock");
        // bind() on an existing directory fails for root and unprivileged
        // users alike, so this is not a permissions-dependent fixture.
        std::fs::create_dir(&socket).unwrap();
        let attribute = directory.path().join("sockcreate");
        let nodes = LabelNodes::at(directory.path().join("enforce"), &attribute);
        std::fs::write(&nodes.enforce, "1").unwrap();

        let reasons = bind_labeled(&nodes, &socket).unwrap_err();
        assert_eq!(reasons.len(), RELAY_SOCKET_CONTEXTS.len());
        for (reason, context) in reasons.iter().zip(RELAY_SOCKET_CONTEXTS) {
            assert!(reason.starts_with(&format!("{context}: ")), "{reason}");
            assert!(reason.contains("labeled bind refused"), "{reason}");
        }
        assert!(socket.is_dir(), "cleanup must not replace the obstacle");
    }

    /// WI#641 (AGK-1): a host without selinuxfs — AppArmor, or none — binds
    /// exactly as before the labeling work and never touches the create
    /// context. A permissive `0` still counts as SELinux and is labeled, so a
    /// host that later switches to enforcing cannot regress.
    #[tokio::test]
    async fn a_host_without_selinuxfs_skips_labeling_entirely() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("proxy.sock");
        let attribute = directory.path().join("sockcreate");
        let nodes = LabelNodes::at(directory.path().join("enforce"), &attribute);

        let (_listener, label) = bind_relay_listener_at(&nodes, &socket).unwrap();
        assert_eq!(label, None, "a host without SELinux needs no label");
        assert!(
            !attribute.exists(),
            "no label attempt may start without selinuxfs"
        );
    }

    /// WI#641: the refusal warning is raised once; later refusals are not.
    #[test]
    fn label_refusal_is_announced_once() {
        let reported = std::sync::atomic::AtomicBool::new(false);
        assert!(first_refusal(&reported));
        assert!(!first_refusal(&reported));
        assert!(!first_refusal(&reported));
    }

    /// WI#641: the labeled path must keep the socket connectable at the same
    /// world-connectable mode; without SELinux the pre-WI#641 socket is kept.
    #[tokio::test]
    async fn relay_listener_binds_under_the_container_label_or_falls_back() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("proxy.sock");
        let (listener, label) = bind_relay_listener(&socket).unwrap();
        assert_eq!(
            std::fs::metadata(&socket).unwrap().permissions().mode() & 0o7777,
            0o666
        );
        if !selinux_enabled(&LabelNodes::defaults()) {
            assert_eq!(label, None, "a host without SELinux keeps the daemon label");
        }
        // A plain client still reaches the listener after either path, so the
        // fallback cannot leave a half-built socket behind.
        let mut client = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (_accepted, _) = listener.accept().await.unwrap();
        client.write_all(b"x").await.unwrap();
    }
}
