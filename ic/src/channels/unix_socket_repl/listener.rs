use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use super::conn::serve_connection;
use super::ConnCtx;

const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_ACCEPT_BACKOFF: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub(super) enum ReplSocketError {
    #[error("{action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("insecure REPL socket parent {path}: owner uid {owner}, mode {mode:04o}; expected a directory owned by the daemon uid with no group/other write bits")]
    InsecureParent {
        path: PathBuf,
        owner: u32,
        mode: u32,
    },
    #[error("another LunarWing REPL listener is already serving {0}")]
    AlreadyServing(PathBuf),
    #[error("REPL socket {path} is owned by foreign uid {uid}; refusing to probe or unlink it")]
    ForeignOwner { path: PathBuf, uid: u32 },
    #[error("REPL socket path {0} is a symlink or non-socket file; refusing to replace it")]
    NotASocket(PathBuf),
}

impl ReplSocketError {
    fn io(action: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            action,
            path: path.to_path_buf(),
            source,
        }
    }

    pub(super) fn is_not_serving(&self) -> bool {
        matches!(
            self,
            Self::AlreadyServing(_)
                | Self::ForeignOwner { .. }
                | Self::NotASocket(_)
                | Self::InsecureParent { .. }
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SocketIdentity {
    dev: u64,
    ino: u64,
}

impl SocketIdentity {
    fn of(path: &Path) -> Result<Self, ReplSocketError> {
        use std::os::unix::fs::MetadataExt;

        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| ReplSocketError::io("failed to inspect REPL socket", path, error))?;
        Ok(Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }

    pub(super) fn matches(self, path: &Path) -> bool {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};

        std::fs::symlink_metadata(path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.dev() == self.dev
                && metadata.ino() == self.ino
        })
    }
}

pub(super) struct BoundSocket {
    pub(super) listener: UnixListener,
    pub(super) identity: SocketIdentity,
    pub(super) lock: Arc<std::fs::File>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Existing {
    OwnedSocket,
    Live,
    Stale,
    Foreign { uid: u32 },
    NotSocket,
}

pub(super) async fn bind_socket(path: &Path) -> Result<BoundSocket, ReplSocketError> {
    bind_socket_with_uid(path, nix::unistd::geteuid().as_raw()).await
}

async fn bind_socket_with_uid(path: &Path, expected_uid: u32) -> Result<BoundSocket, ReplSocketError> {
    prepare_parent_dir(path, expected_uid)?;
    let lock = acquire_path_lock(path)?;
    let listener = match UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            match probe_existing(path, expected_uid).await? {
                Existing::Live => return Err(ReplSocketError::AlreadyServing(path.to_path_buf())),
                Existing::Foreign { uid } => {
                    return Err(ReplSocketError::ForeignOwner {
                        path: path.to_path_buf(),
                        uid,
                    });
                }
                Existing::NotSocket => {
                    return Err(ReplSocketError::NotASocket(path.to_path_buf()));
                }
                Existing::Stale => {
                    remove_stale(path)?;
                    UnixListener::bind(path)
                        .map_err(|error| ReplSocketError::io("failed to rebind REPL socket", path, error))?
                }
                Existing::OwnedSocket => {
                    return Err(ReplSocketError::io(
                        "REPL socket probe returned an invalid state",
                        path,
                        std::io::Error::other("invalid owned-socket probe state"),
                    ));
                }
            }
        }
        Err(error) => return Err(ReplSocketError::io("failed to bind REPL socket", path, error)),
    };
    set_mode_0600(path)?;
    let identity = SocketIdentity::of(path)?;
    Ok(BoundSocket {
        listener,
        identity,
        lock: Arc::new(lock),
    })
}

fn prepare_parent_dir(path: &Path, expected_uid: u32) -> Result<(), ReplSocketError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    match std::fs::symlink_metadata(parent) {
        Ok(metadata) => validate_parent(parent, &metadata, expected_uid),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder
                .create(parent)
                .map_err(|error| ReplSocketError::io("failed to create REPL socket parent", parent, error))?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| ReplSocketError::io("failed to chmod REPL socket parent", parent, error))?;
            let metadata = std::fs::symlink_metadata(parent)
                .map_err(|error| ReplSocketError::io("failed to inspect REPL socket parent", parent, error))?;
            validate_parent(parent, &metadata, expected_uid)
        }
        Err(error) => Err(ReplSocketError::io(
            "failed to inspect REPL socket parent",
            parent,
            error,
        )),
    }
}

fn validate_parent(
    parent: &Path,
    metadata: &std::fs::Metadata,
    expected_uid: u32,
) -> Result<(), ReplSocketError> {
    use std::os::unix::fs::MetadataExt;

    if metadata.is_dir() && metadata.uid() == expected_uid && metadata.mode() & 0o022 == 0 {
        return Ok(());
    }
    Err(ReplSocketError::InsecureParent {
        path: parent.to_path_buf(),
        owner: metadata.uid(),
        mode: metadata.mode() & 0o7777,
    })
}

fn set_mode_0600(path: &Path) -> Result<(), ReplSocketError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| ReplSocketError::io("failed to chmod REPL socket", path, error))
}

fn acquire_path_lock(path: &Path) -> Result<std::fs::File, ReplSocketError> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&lock_path)
        .map_err(|error| ReplSocketError::io("failed to open REPL socket lock", &lock_path, error))?;
    lock.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| ReplSocketError::io("failed to chmod REPL socket lock", &lock_path, error))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => {
            Err(ReplSocketError::AlreadyServing(path.to_path_buf()))
        }
        Err(error) => Err(ReplSocketError::io(
            "failed to lock REPL socket path",
            &lock_path,
            std::io::Error::other(error),
        )),
    }
}

pub(super) fn classify_existing(metadata: &std::fs::Metadata, expected_uid: u32) -> Existing {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let file_type = metadata.file_type();
    if file_type.is_symlink() || !file_type.is_socket() {
        return Existing::NotSocket;
    }
    let uid = metadata.uid();
    if uid != expected_uid {
        return Existing::Foreign { uid };
    }
    Existing::OwnedSocket
}

async fn probe_existing(path: &Path, expected_uid: u32) -> Result<Existing, ReplSocketError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Existing::Stale),
        Err(error) => {
            return Err(ReplSocketError::io(
                "failed to inspect existing REPL socket",
                path,
                error,
            ));
        }
    };
    match classify_existing(&metadata, expected_uid) {
        Existing::OwnedSocket => match tokio::time::timeout(PROBE_TIMEOUT, UnixStream::connect(path)).await {
            Ok(Ok(_stream)) | Err(_) => Ok(Existing::Live),
            Ok(Err(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                Ok(Existing::Stale)
            }
            Ok(Err(error)) => Err(ReplSocketError::io(
                "failed to probe existing REPL socket",
                path,
                error,
            )),
        },
        Existing::Foreign { uid } => Ok(Existing::Foreign { uid }),
        Existing::NotSocket => Ok(Existing::NotSocket),
        Existing::Live | Existing::Stale => Err(ReplSocketError::io(
            "REPL socket probe returned an invalid state",
            path,
            std::io::Error::other("invalid pre-probed socket state"),
        )),
    }
}

fn remove_stale(path: &Path) -> Result<(), ReplSocketError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ReplSocketError::io(
            "failed to remove stale REPL socket",
            path,
            error,
        )),
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum PeerRejected {
    #[error("failed to read peer credentials: {0}")]
    Credentials(std::io::Error),
    #[error("peer uid {uid} (pid {pid:?}) is not the daemon uid")]
    WrongUid { uid: u32, pid: Option<i32> },
}

pub(super) fn peer_allowed(stream: &UnixStream, expected_uid: u32) -> Result<(), PeerRejected> {
    let credentials = stream.peer_cred().map_err(PeerRejected::Credentials)?;
    if credentials.uid() != expected_uid {
        return Err(PeerRejected::WrongUid {
            uid: credentials.uid(),
            pid: credentials.pid(),
        });
    }
    Ok(())
}

struct AcceptBackoff(Duration);

impl AcceptBackoff {
    fn new() -> Self {
        Self(Duration::from_millis(10))
    }

    fn reset(&mut self) {
        self.0 = Duration::from_millis(10);
    }

    fn next(&mut self) -> Duration {
        let current = self.0;
        self.0 = self.0.mul_f64(2.0).min(MAX_ACCEPT_BACKOFF);
        current
    }
}

pub(super) async fn accept_loop(
    listener: UnixListener,
    expected_uid: u32,
    pending_handshakes: Arc<Semaphore>,
    ctx: ConnCtx,
    cancel: CancellationToken,
) {
    let mut backoff = AcceptBackoff::new();
    loop {
        let accepted = tokio::select! {
            () = cancel.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        let (stream, _address) = match accepted {
            Ok(accepted) => {
                backoff.reset();
                accepted
            }
            Err(error) => {
                error!(%error, "unix socket REPL: accept error");
                let delay = backoff.next();
                tokio::select! {
                    () = cancel.cancelled() => break,
                    () = tokio::time::sleep(delay) => {}
                }
                continue;
            }
        };
        if let Err(error) = peer_allowed(&stream, expected_uid) {
            warn!(%error, "unix socket REPL: rejected peer");
            drop(stream);
            continue;
        }
        let Ok(permit) = Arc::clone(&pending_handshakes).try_acquire_owned() else {
            warn!("unix socket REPL: pending handshake limit reached");
            drop(stream);
            continue;
        };
        spawn_connection(stream, permit, ctx.clone());
    }
}

fn spawn_connection(stream: UnixStream, permit: OwnedSemaphorePermit, ctx: ConnCtx) {
    let (read, write) = stream.into_split();
    tokio::spawn(async move {
        serve_connection(read, write, ctx, permit).await;
    });
}
