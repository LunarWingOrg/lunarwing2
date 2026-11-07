use lunarwing_common::TurnOutcome;
use lunarwing_common::repl_wire::{ResponseFrame, ServerFrame};
use tracing::debug;

use super::registry::Registry;
use super::writer::{FrameClass, OfferError};
use crate::channels::{IncomingMessage, MessageLifecycle, OutgoingResponse, StatusUpdate};
use crate::error::ChannelError;

const MAX_APPROVAL_PARAMETER_BYTES: usize = 8 * 1024;

pub(super) fn respond(
    registry: &Registry,
    msg: &IncomingMessage,
    response: OutgoingResponse,
) -> Result<(), ChannelError> {
    let session_id = routing_session(msg);
    let Some(outbound) = registry.route(session_id) else {
        debug!(%session_id, "unix socket REPL: session not found for response (client may have disconnected)");
        return Ok(());
    };
    outbound.offer(
        &ServerFrame::Response(ResponseFrame {
            content: response.content,
            session_id: Some(session_id.to_string()),
            is_complete: true,
            msg_id: None,
        }),
        FrameClass::Control,
    )?;
    registry.mark_delivered(msg.id);
    Ok(())
}

pub(super) fn send_status(
    registry: &Registry,
    status: StatusUpdate,
    metadata: &serde_json::Value,
) -> Result<(), ChannelError> {
    let content = match status {
        StatusUpdate::ApprovalNeeded {
            request_id,
            tool_name,
            parameters,
            allow_always,
            ..
        } => Some(approval_prompt(
            &request_id,
            &tool_name,
            &parameters,
            allow_always,
        )),
        StatusUpdate::AuthRequired {
            extension_name,
            instructions,
            auth_url,
            setup_url,
        } => Some(auth_prompt(
            &extension_name,
            instructions,
            auth_url,
            setup_url,
        )),
        _ => None,
    };
    let Some(content) = content else {
        return Ok(());
    };
    let Some(session_id) = metadata
        .get("unix_session_id")
        .and_then(serde_json::Value::as_str)
    else {
        debug!("unix socket REPL: status has no session route");
        return Ok(());
    };
    let Some(outbound) = registry.route(session_id) else {
        debug!(%session_id, "unix socket REPL: session not found for status");
        return Ok(());
    };
    outbound.offer(
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

pub(super) fn message_lifecycle(
    registry: &Registry,
    msg: &IncomingMessage,
    event: MessageLifecycle,
) {
    let MessageLifecycle::Finished(outcome) = event else {
        return;
    };
    let delivered = registry.take_delivered(msg.id);
    if outcome != TurnOutcome::NoReply || delivered {
        return;
    }
    let content = if is_approval_keyword(&msg.content) {
        "Nothing is waiting for approval."
    } else {
        "(no reply)"
    };
    let session_id = routing_session(msg);
    if let Some(outbound) = registry.route(session_id) {
        let _ = outbound.offer(
            &ServerFrame::Response(ResponseFrame {
                content: content.to_string(),
                session_id: Some(session_id.to_string()),
                is_complete: true,
                msg_id: None,
            }),
            FrameClass::Control,
        );
    }
}

fn routing_session(msg: &IncomingMessage) -> &str {
    msg.metadata
        .get("unix_session_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(msg.sender_id.as_str())
}

fn approval_prompt(
    request_id: &str,
    tool_name: &str,
    parameters: &serde_json::Value,
    allow_always: bool,
) -> String {
    let parameters =
        serde_json::to_string_pretty(parameters).unwrap_or_else(|_| parameters.to_string());
    let parameters = truncate_parameters(&parameters);
    let always_line = if allow_always {
        format!(
            "\nReply with `always` or `a` to approve and auto-approve future {tool_name} requests."
        )
    } else {
        String::new()
    };
    format!(
        "Approval required for `{tool_name}`.\nRequest ID: `{request_id}`\nParameters:\n{parameters}\nReply with `yes`/`y` to approve or `no`/`n` to deny.{always_line}"
    )
}

fn truncate_parameters(parameters: &str) -> String {
    if parameters.len() <= MAX_APPROVAL_PARAMETER_BYTES {
        return parameters.to_string();
    }
    let mut boundary = MAX_APPROVAL_PARAMETER_BYTES;
    while !parameters.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let omitted = parameters.len() - boundary;
    format!(
        "{}\n[... {omitted} more bytes not shown; review the full request in the web UI before approving]",
        &parameters[..boundary]
    )
}

fn auth_prompt(
    extension_name: &str,
    instructions: Option<String>,
    auth_url: Option<String>,
    setup_url: Option<String>,
) -> String {
    let mut content = format!("Authentication required for `{extension_name}`.");
    if let Some(instructions) = instructions.filter(|value| !value.is_empty()) {
        content.push_str(&format!("\n{instructions}"));
    }
    if let Some(auth_url) = auth_url {
        content.push_str(&format!("\nAuth URL: {auth_url}"));
    }
    if let Some(setup_url) = setup_url {
        content.push_str(&format!("\nSetup URL: {setup_url}"));
    }
    content
}

fn is_approval_keyword(content: &str) -> bool {
    matches!(
        content.trim().to_ascii_lowercase().as_str(),
        "yes"
            | "y"
            | "approve"
            | "ok"
            | "/approve"
            | "/yes"
            | "/y"
            | "always"
            | "a"
            | "yes always"
            | "approve always"
            | "/always"
            | "/a"
            | "no"
            | "n"
            | "deny"
            | "reject"
            | "cancel"
            | "/deny"
            | "/no"
            | "/n"
    )
}

impl From<OfferError> for ChannelError {
    fn from(error: OfferError) -> Self {
        match error {
            OfferError::Serialize(error) => ChannelError::InvalidMessage(error.to_string()),
            OfferError::SlowConsumer => ChannelError::SendFailed {
                name: crate::channels::UNIX_SOCKET_REPL_CHANNEL.to_string(),
                reason: "slow consumer disconnected".to_string(),
            },
        }
    }
}
