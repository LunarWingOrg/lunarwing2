//! Multi-channel input system.
//!
//! Channels receive messages from external sources (CLI, HTTP, etc.)
//! and convert them to a unified message format for the agent to process.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                         ChannelManager                              │
//! │                                                                     │
//! │   ┌──────────────┐   ┌─────────────┐   ┌─────────────┐             │
//! │   │ ReplChannel  │   │ HttpChannel │   │ WasmChannel │   ...       │
//! │   └──────┬───────┘   └──────┬──────┘   └──────┬──────┘             │
//! │          │                 │                 │                      │
//! │          └─────────────────┴─────────────────┘                      │
//! │                            │                                        │
//! │                   select_all (futures)                              │
//! │                            │                                        │
//! │                            ▼                                        │
//! │                     MessageStream                                   │
//! └─────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # WASM Channels
//!
//! WASM channels allow dynamic loading of channel implementations at runtime.
//! See the [`wasm`] module for details.

pub mod budget;
mod channel;
pub mod coalesce;
mod http;
pub(crate) mod image_mime;
pub mod lifecycle;
mod manager;
mod repl;
mod signal;
#[cfg(unix)]
mod unix_socket_repl;
pub mod wasm;
pub mod web;
mod webhook_server;
mod xmpp;

/// Channel name of the web gateway ([`GatewayChannel`]).
pub const GATEWAY_CHANNEL: &str = "gateway";
/// Channel name of the Unix-socket REPL. Not cfg-gated, so callers can compare
/// channel names on every platform.
pub const UNIX_SOCKET_REPL_CHANNEL: &str = "unix_socket_repl";

pub use channel::{
    AttachmentKind, Channel, ChannelSecretUpdater, IncomingAttachment, IncomingMessage,
    MessageStream, OutgoingResponse, StatusUpdate, ToolDecision, routing_target_from_metadata,
};
pub use http::{HttpChannel, HttpChannelState};
pub use lifecycle::{MessageLifecycle, TurnOutcome};
pub use manager::ChannelManager;
pub use repl::ReplChannel;
pub use signal::SignalChannel;
#[cfg(unix)]
pub use unix_socket_repl::UnixSocketReplChannel;
pub use web::GatewayChannel;
pub use webhook_server::{WebhookServer, WebhookServerConfig};
pub use xmpp::{OutboundAttachment, XmppChannel};
