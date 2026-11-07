#[cfg(test)]
use std::time::Duration;

use lunarwing_common::repl_wire::{
    ClientFrame, ConnectFrame, ResponseFrame, ServerFrame, encode_line,
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::registry::Session;
use super::writer::{FrameClass, OfferError, Outbound};
use super::{ConnCtx, ReplLimits};

const BUFFER_SHRINK_THRESHOLD: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum FrameRead {
    Line,
    Eof,
    TooLong,
}

#[derive(Debug, thiserror::Error)]
enum HandshakeError {
    #[error("handshake timed out")]
    Timeout,
    #[error("handshake read failed: {0}")]
    Read(std::io::Error),
    #[error("handshake exceeds {0} bytes")]
    TooLong(usize),
    #[error("handshake JSON parse failed: {0}")]
    Parse(serde_json::Error),
    #[error("expected Connect, got {0}")]
    NotConnect(&'static str),
    #[error("client disconnected before handshake")]
    Eof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    Close,
}

pub(super) async fn serve_connection(
    read: OwnedReadHalf,
    write: OwnedWriteHalf,
    ctx: ConnCtx,
    permit: OwnedSemaphorePermit,
) {
    let session_id = uuid::Uuid::new_v4().simple().to_string();
    let mut reader = BufReader::new(read);
    let mut buffer = Vec::with_capacity(4096);
    let connect = match read_handshake(&mut reader, &mut buffer, &ctx.limits).await {
        Ok(connect) => connect,
        Err(HandshakeError::Eof) => return,
        Err(error) => {
            warn!(%error, "unix socket REPL: handshake rejected");
            return;
        }
    };
    info!(
        version = %sanitize_handshake_field(&connect.version),
        client_info = %connect.client_info.as_deref().map(sanitize_handshake_field).unwrap_or_default(),
        %session_id,
        "unix socket REPL client connected"
    );

    let Some(slot) = ctx
        .registry
        .try_reserve(ctx.max_connections, session_id.clone())
    else {
        warn!(max_connections = ctx.max_connections, "unix socket REPL: max connections reached");
        write_busy(write, &session_id, ctx.max_connections, ctx.limits).await;
        return;
    };
    drop(permit);

    let kill = CancellationToken::new();
    let (outbound, rx) = Outbound::channel(ctx.limits, kill.clone());
    outbound.spawn_writer(write, rx);
    let session = slot.open_session(outbound, kill.clone());
    if let Err(error) = offer_legacy(
        &session,
        &session_id,
        format!("Connected to {} REPL. Session: {session_id}", ctx.agent_name),
    ) {
        debug!(%error, %session_id, "unix socket REPL: failed to queue welcome");
        ctx.registry.remove(&session_id);
        return;
    }

    read_loop(&mut reader, &mut buffer, &session_id, &ctx, &session).await;
    ctx.registry.remove(&session_id);
    drop(session);
    info!(%session_id, "unix socket REPL session ended");
}

async fn read_handshake(
    reader: &mut BufReader<OwnedReadHalf>,
    buffer: &mut Vec<u8>,
    limits: &ReplLimits,
) -> Result<ConnectFrame, HandshakeError> {
    let read = tokio::time::timeout(
        limits.handshake_timeout,
        read_frame_line(reader, buffer, limits.max_handshake_bytes),
    )
    .await
    .map_err(|_| HandshakeError::Timeout)?
    .map_err(HandshakeError::Read)?;
    match read {
        FrameRead::Eof => Err(HandshakeError::Eof),
        FrameRead::TooLong => Err(HandshakeError::TooLong(limits.max_handshake_bytes)),
        FrameRead::Line => {
            let frame = serde_json::from_slice::<ClientFrame>(buffer).map_err(HandshakeError::Parse)?;
            match frame {
                ClientFrame::Connect(connect) => Ok(connect),
                other => Err(HandshakeError::NotConnect(other.kind())),
            }
        }
    }
}

pub(super) async fn read_frame_line<R>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    max: usize,
) -> Result<FrameRead, std::io::Error>
where
    R: AsyncBufRead + Unpin + ?Sized,
{
    loop {
        buffer.clear();
        let read = (&mut *reader)
            .take(max.saturating_add(1) as u64)
            .read_until(b'\n', buffer)
            .await?;
        if read == 0 {
            return Ok(FrameRead::Eof);
        }
        if buffer.len() > max {
            buffer.clear();
            buffer.shrink_to(BUFFER_SHRINK_THRESHOLD);
            return Ok(FrameRead::TooLong);
        }
        if buffer.last() == Some(&b'\n') {
            buffer.pop();
        }
        if buffer.last() == Some(&b'\r') {
            buffer.pop();
        }
        if buffer.is_empty() {
            continue;
        }
        if buffer.capacity() > BUFFER_SHRINK_THRESHOLD {
            buffer.shrink_to(BUFFER_SHRINK_THRESHOLD);
        }
        return Ok(FrameRead::Line);
    }
}

async fn read_loop(
    reader: &mut BufReader<OwnedReadHalf>,
    buffer: &mut Vec<u8>,
    session_id: &str,
    ctx: &ConnCtx,
    session: &Session,
) {
    loop {
        let read = tokio::select! {
            () = session.kill.cancelled() => break,
            read = read_frame_line(reader, buffer, ctx.limits.max_frame_bytes) => read,
        };
        match read {
            Ok(FrameRead::Eof) => {
                debug!(%session_id, "unix socket REPL client disconnected (EOF)");
                break;
            }
            Ok(FrameRead::TooLong) => {
                warn!(max_bytes = ctx.limits.max_frame_bytes, %session_id, "unix socket REPL: frame too large");
                let _ = offer_legacy(
                    session,
                    session_id,
                    format!("Error: frame exceeds {} bytes", ctx.limits.max_frame_bytes),
                );
                break;
            }
            Ok(FrameRead::Line) => match serde_json::from_slice::<ClientFrame>(buffer) {
                Ok(frame) => {
                    if dispatch(frame, session_id, ctx, session).await == Flow::Close {
                        break;
                    }
                }
                Err(error) => {
                    warn!(%error, %session_id, "unix socket REPL: message parse error");
                }
            },
            Err(error) => {
                warn!(%error, %session_id, "unix socket REPL: read error");
                break;
            }
        }
    }
}

async fn dispatch(frame: ClientFrame, session_id: &str, ctx: &ConnCtx, session: &Session) -> Flow {
    match frame {
        ClientFrame::Message(message) if !message.attachments.is_empty() => {
            let _ = offer_legacy(
                session,
                session_id,
                "Error: attachments require REPLv3 (protocol 3)".to_string(),
            );
            Flow::Continue
        }
        ClientFrame::Message(message) => {
            let incoming = crate::channels::IncomingMessage::new(
                crate::channels::UNIX_SOCKET_REPL_CHANNEL,
                &ctx.owner_id,
                message.content,
            )
            .with_sender_id(session_id.to_string())
            .with_metadata(serde_json::json!({"unix_session_id": session_id}));
            if ctx.message_tx.send(incoming).await.is_err() {
                Flow::Close
            } else {
                Flow::Continue
            }
        }
        ClientFrame::Ping => {
            let _ = session.outbound.offer(&ServerFrame::Pong, FrameClass::Control);
            Flow::Continue
        }
        ClientFrame::Pong => Flow::Continue,
        ClientFrame::Disconnect(_) => {
            debug!(%session_id, "unix socket REPL client requested disconnect");
            Flow::Close
        }
        ClientFrame::Connect(_) => {
            warn!(kind = "Connect", %session_id, "unix socket REPL: unexpected frame");
            Flow::Continue
        }
        other => {
            let content = format!("Error: {} requires REPLv3 (protocol 3)", other.kind());
            let _ = offer_legacy(session, session_id, content);
            Flow::Continue
        }
    }
}

fn offer_legacy(
    session: &Session,
    session_id: &str,
    content: String,
) -> Result<(), OfferError> {
    session.outbound.offer(
        &ServerFrame::Response(ResponseFrame {
            content,
            session_id: Some(session_id.to_string()),
            is_complete: true,
            msg_id: None,
        }),
        FrameClass::Control,
    )?;
    Ok(())
}

async fn write_busy(
    mut write: OwnedWriteHalf,
    session_id: &str,
    max_connections: usize,
    limits: ReplLimits,
) {
    let frame = ServerFrame::Response(ResponseFrame {
        content: format!("Server busy: maximum connections ({max_connections}) reached."),
        session_id: Some(session_id.to_string()),
        is_complete: true,
        msg_id: None,
    });
    if let Ok(bytes) = encode_line(&frame) {
        let _ = tokio::time::timeout(limits.write_timeout, write.write_all(&bytes)).await;
    }
}

fn sanitize_handshake_field(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(64)
        .collect()
}

#[cfg(test)]
pub(super) fn handshake_timeout(limits: &ReplLimits) -> Duration {
    limits.handshake_timeout
}
