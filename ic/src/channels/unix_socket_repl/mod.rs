//! Same-UID Unix-socket REPL channel.
//!
//! Protocol 1 remains newline-delimited JSON with its original response bytes.
//! The listener owns an explicit 0600 socket and a start lock; accepted peers
//! must have the daemon's effective UID. Rootless keep-id containers can appear
//! as that UID, so sandbox mount masking is a separate required boundary.

mod conn;
mod listener;
mod outbound;
mod registry;
mod writer;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use lunarwing_common::repl_wire::{MAX_FRAME_BYTES, MAX_HANDSHAKE_BYTES};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use self::listener::{SocketIdentity, accept_loop, bind_socket};
use self::registry::Registry;
use crate::channels::{
    Channel, IncomingMessage, MessageLifecycle, MessageStream, OutgoingResponse, StatusUpdate,
};
use crate::error::ChannelError;

#[derive(Clone, Copy, Debug)]
struct ReplLimits {
    max_handshake_bytes: usize,
    handshake_timeout: Duration,
    max_frame_bytes: usize,
    pending_handshakes: usize,
    outbound_queue_frames: usize,
    outbound_queue_bytes: usize,
    status_shed_bytes: usize,
    write_timeout: Duration,
}

impl Default for ReplLimits {
    fn default() -> Self {
        Self {
            max_handshake_bytes: MAX_HANDSHAKE_BYTES,
            handshake_timeout: Duration::from_secs(10),
            max_frame_bytes: MAX_FRAME_BYTES,
            pending_handshakes: 8,
            outbound_queue_frames: 1024,
            outbound_queue_bytes: 32 * 1024 * 1024,
            status_shed_bytes: 2 * 1024 * 1024,
            write_timeout: Duration::from_secs(15),
        }
    }
}

#[derive(Clone)]
struct ConnCtx {
    registry: Arc<Registry>,
    message_tx: mpsc::Sender<IncomingMessage>,
    agent_name: String,
    owner_id: String,
    max_connections: usize,
    limits: ReplLimits,
}

#[derive(Clone)]
enum ListenerState {
    NotStarted,
    Serving {
        identity: SocketIdentity,
        _lock: Arc<std::fs::File>,
        cancel: CancellationToken,
    },
    NotServing {
        reason: String,
    },
}

pub struct UnixSocketReplChannel {
    socket_path: PathBuf,
    max_connections: usize,
    agent_name: String,
    owner_id: String,
    expected_uid: u32,
    limits: ReplLimits,
    registry: Arc<Registry>,
    listener_state: Mutex<ListenerState>,
    health_warned: AtomicBool,
}

impl UnixSocketReplChannel {
    pub fn new(
        socket_path: impl Into<PathBuf>,
        max_connections: usize,
        agent_name: impl Into<String>,
        owner_id: impl Into<String>,
    ) -> Self {
        Self {
            socket_path: socket_path.into(),
            max_connections,
            agent_name: agent_name.into(),
            owner_id: owner_id.into(),
            expected_uid: nix::unistd::geteuid().as_raw(),
            limits: ReplLimits::default(),
            registry: Arc::new(Registry::default()),
            listener_state: Mutex::new(ListenerState::NotStarted),
            health_warned: AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    fn with_limits(mut self, limits: ReplLimits) -> Self {
        self.limits = limits;
        self
    }

    #[cfg(test)]
    fn with_expected_uid(mut self, expected_uid: u32) -> Self {
        self.expected_uid = expected_uid;
        self
    }

    fn state(&self) -> MutexGuard<'_, ListenerState> {
        self.listener_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn startup_error(error: listener::ReplSocketError) -> ChannelError {
        ChannelError::StartupFailed {
            name: crate::channels::UNIX_SOCKET_REPL_CHANNEL.to_string(),
            reason: error.to_string(),
        }
    }

    fn not_serving(&self, reason: String) -> MessageStream {
        error!(path = ?self.socket_path, %reason, "unix socket REPL is not serving");
        *self.state() = ListenerState::NotServing { reason };
        self.health_warned.store(false, Ordering::Release);
        Box::pin(futures::stream::pending())
    }

    fn warn_health_once(&self, reason: &str) {
        if !self.health_warned.swap(true, Ordering::AcqRel) {
            warn!(path = ?self.socket_path, %reason, "unix socket REPL health is degraded without a restartable failure");
        }
    }
}

#[async_trait]
impl Channel for UnixSocketReplChannel {
    fn name(&self) -> &str {
        crate::channels::UNIX_SOCKET_REPL_CHANNEL
    }

    async fn start(&self) -> Result<MessageStream, ChannelError> {
        let bound = match bind_socket(&self.socket_path).await {
            Ok(bound) => bound,
            Err(error) if error.is_not_serving() => {
                return Ok(self.not_serving(error.to_string()));
            }
            Err(error) => return Err(Self::startup_error(error)),
        };
        let (message_tx, message_rx) = mpsc::channel(64);
        let cancel = CancellationToken::new();
        let ctx = ConnCtx {
            registry: Arc::clone(&self.registry),
            message_tx,
            agent_name: self.agent_name.clone(),
            owner_id: self.owner_id.clone(),
            max_connections: self.max_connections,
            limits: self.limits,
        };
        *self.state() = ListenerState::Serving {
            identity: bound.identity,
            _lock: bound.lock,
            cancel: cancel.clone(),
        };
        self.health_warned.store(false, Ordering::Release);
        let pending_handshakes = Arc::new(Semaphore::new(self.limits.pending_handshakes));
        tokio::spawn(accept_loop(
            bound.listener,
            self.expected_uid,
            pending_handshakes,
            ctx,
            cancel,
        ));
        info!(path = ?self.socket_path, "unix socket REPL server listening");
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(
            message_rx,
        )))
    }

    async fn respond(
        &self,
        msg: &IncomingMessage,
        response: OutgoingResponse,
    ) -> Result<(), ChannelError> {
        outbound::respond(&self.registry, msg, response)
    }

    async fn send_status(
        &self,
        status: StatusUpdate,
        metadata: &serde_json::Value,
    ) -> Result<(), ChannelError> {
        outbound::send_status(&self.registry, status, metadata)
    }

    async fn message_lifecycle(&self, msg: &IncomingMessage, event: MessageLifecycle) {
        outbound::message_lifecycle(&self.registry, msg, event);
    }

    async fn health_check(&self) -> Result<(), ChannelError> {
        match self.state().clone() {
            ListenerState::Serving { identity, .. } => {
                if !identity.matches(&self.socket_path) {
                    let current = SocketIdentity::of(&self.socket_path).ok();
                    self.warn_health_once(&format!(
                        "socket inode changed or disappeared (bound {identity:?}, current {current:?})"
                    ));
                }
                Ok(())
            }
            ListenerState::NotServing { reason } => {
                self.warn_health_once(&reason);
                Ok(())
            }
            ListenerState::NotStarted => Err(ChannelError::StartupFailed {
                name: crate::channels::UNIX_SOCKET_REPL_CHANNEL.to_string(),
                reason: "channel has not been started".to_string(),
            }),
        }
    }

    async fn shutdown(&self) -> Result<(), ChannelError> {
        let state = std::mem::replace(&mut *self.state(), ListenerState::NotStarted);
        if let ListenerState::Serving {
            identity, cancel, ..
        } = state
        {
            cancel.cancel();
            self.registry.close_all();
            if identity.matches(&self.socket_path) {
                match std::fs::remove_file(&self.socket_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(ChannelError::StartupFailed {
                            name: crate::channels::UNIX_SOCKET_REPL_CHANNEL.to_string(),
                            reason: format!("failed to remove socket on shutdown: {error}"),
                        });
                    }
                }
            } else {
                warn!(path = ?self.socket_path, ?identity, "unix socket REPL shutdown left a replaced or foreign path untouched");
            }
        }
        info!("unix socket REPL server stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests;
