use std::fmt;

use serde::{Deserialize, Serialize};

use crate::client_caps::{TurnOutcome, WireLimits};

pub const PROTOCOL_LEGACY: u32 = 1;
pub const PROTOCOL_V3: u32 = 3;
pub const MAX_HANDSHAKE_BYTES: usize = 16 * 1024;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024 + 64 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub enum ClientFrame {
    Connect(ConnectFrame),
    Message(MessageFrame),
    Interrupt(InterruptFrame),
    Steer(SteerFrame),
    Approval(ApprovalFrame),
    Transcribe(TranscribeFrame),
    Disconnect(DisconnectFrame),
    Ping,
    Pong,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum ServerFrame {
    Welcome(WelcomeFrame),
    Ack(AckFrame),
    TurnStarted(TurnFrame),
    TurnFinished(TurnFinishedFrame),
    Status(StatusFrame),
    Response(ResponseFrame),
    ApprovalRequest(ApprovalRequestFrame),
    ApprovalResolved(ApprovalResolvedFrame),
    AuthRequired(AuthRequiredFrame),
    Error(ErrorFrame),
    TranscribeResult(TranscribeResultFrame),
    Notice(NoticeFrame),
    Ping,
    Pong,
}

impl ClientFrame {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Connect(_) => "Connect",
            Self::Message(_) => "Message",
            Self::Interrupt(_) => "Interrupt",
            Self::Steer(_) => "Steer",
            Self::Approval(_) => "Approval",
            Self::Transcribe(_) => "Transcribe",
            Self::Disconnect(_) => "Disconnect",
            Self::Ping => "Ping",
            Self::Pong => "Pong",
        }
    }
}

impl ServerFrame {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Welcome(_) => "Welcome",
            Self::Ack(_) => "Ack",
            Self::TurnStarted(_) => "TurnStarted",
            Self::TurnFinished(_) => "TurnFinished",
            Self::Status(_) => "Status",
            Self::Response(_) => "Response",
            Self::ApprovalRequest(_) => "ApprovalRequest",
            Self::ApprovalResolved(_) => "ApprovalResolved",
            Self::AuthRequired(_) => "AuthRequired",
            Self::Error(_) => "Error",
            Self::TranscribeResult(_) => "TranscribeResult",
            Self::Notice(_) => "Notice",
            Self::Ping => "Ping",
            Self::Pong => "Pong",
        }
    }
}

impl fmt::Debug for ClientFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind())
    }
}

impl fmt::Debug for ServerFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind())
    }
}

pub fn encode_line(frame: &ServerFrame) -> serde_json::Result<Vec<u8>> {
    encode(frame)
}

pub fn encode_client_line(frame: &ClientFrame) -> serde_json::Result<Vec<u8>> {
    encode(frame)
}

fn encode<T: Serialize>(frame: &T) -> serde_json::Result<Vec<u8>> {
    let mut line = serde_json::to_vec(frame)?;
    line.push(b'\n');
    Ok(line)
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectFrame {
    pub version: String,
    pub client_info: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}

impl fmt::Debug for ConnectFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectFrame")
            .field("version", &Redacted(self.version.len()))
            .field(
                "client_info",
                &self.client_info.as_ref().map(|value| Redacted(value.len())),
            )
            .field("protocol", &self.protocol)
            .field("features", &self.features)
            .field("thread", &self.thread)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageFrame {
    pub content: String,
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<WireAttachment>,
}

impl fmt::Debug for MessageFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MessageFrame")
            .field("content", &Redacted(self.content.len()))
            .field("session_id", &self.session_id)
            .field("msg_id", &self.msg_id)
            .field("attachments", &self.attachments)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct WireAttachment {
    pub mime_type: String,
    #[serde(default)]
    pub filename: Option<String>,
    pub data_base64: String,
}

impl fmt::Debug for WireAttachment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WireAttachment")
            .field("mime_type", &self.mime_type)
            .field("filename", &self.filename)
            .field("data_base64", &Redacted(self.data_base64.len()))
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct InterruptFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct SteerFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    pub content: String,
}

impl fmt::Debug for SteerFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SteerFrame")
            .field("msg_id", &self.msg_id)
            .field("content", &Redacted(self.content.len()))
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ApprovalFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    pub request_id: String,
    pub decision: ApprovalDecision,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Always,
    Deny,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscribeFrame {
    pub req_id: String,
    #[serde(default = "default_audio_format")]
    pub format: String,
    pub audio: String,
}

impl fmt::Debug for TranscribeFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TranscribeFrame")
            .field("req_id", &self.req_id)
            .field("format", &self.format)
            .field("audio", &Redacted(self.audio.len()))
            .finish()
    }
}

fn default_audio_format() -> String {
    "wav".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DisconnectFrame {
    pub session_id: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WelcomeFrame {
    pub protocol: u32,
    pub server_version: String,
    pub agent_name: String,
    pub engine: String,
    pub session_id: String,
    pub thread: String,
    pub features: Vec<String>,
    pub limits: WireLimits,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AckFrame {
    pub msg_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TurnFrame {
    pub msg_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TurnFinishedFrame {
    pub msg_id: String,
    pub outcome: TurnOutcome,
    #[serde(default)]
    pub dropped_status: u64,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    pub event: serde_json::Value,
}

impl fmt::Debug for StatusFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StatusFrame")
            .field("msg_id", &self.msg_id)
            .field("event", &JsonType(&self.event))
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct ResponseFrame {
    pub content: String,
    pub session_id: Option<String>,
    pub is_complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
}

impl fmt::Debug for ResponseFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponseFrame")
            .field("content", &Redacted(self.content.len()))
            .field("session_id", &self.session_id)
            .field("is_complete", &self.is_complete)
            .field("msg_id", &self.msg_id)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovalRequestFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    pub request_id: String,
    pub tool_name: String,
    pub description: String,
    pub parameters: String,
    pub allow_always: bool,
    #[serde(default)]
    pub parameters_truncated: bool,
    #[serde(default)]
    pub parameters_bytes: u64,
}

impl fmt::Debug for ApprovalRequestFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApprovalRequestFrame")
            .field("msg_id", &self.msg_id)
            .field("request_id", &self.request_id)
            .field("tool_name", &self.tool_name)
            .field("description", &self.description)
            .field("parameters", &Redacted(self.parameters.len()))
            .field("allow_always", &self.allow_always)
            .field("parameters_truncated", &self.parameters_truncated)
            .field("parameters_bytes", &self.parameters_bytes)
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ApprovalResolvedFrame {
    pub request_id: String,
    pub resolution: ApprovalResolution,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalResolution {
    Approve,
    Always,
    Deny,
    Cancelled,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AuthRequiredFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    pub extension_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ErrorFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub req_id: Option<String>,
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub fatal: bool,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscribeResultFrame {
    pub req_id: String,
    pub text: String,
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl fmt::Debug for TranscribeResultFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TranscribeResultFrame")
            .field("req_id", &self.req_id)
            .field("text", &Redacted(self.text.len()))
            .field("elapsed_ms", &self.elapsed_ms)
            .field("language", &self.language)
            .field("model", &self.model)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct NoticeFrame {
    pub kind: NoticeKind,
    pub level: NoticeLevel,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
}

impl fmt::Debug for NoticeFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NoticeFrame")
            .field("kind", &self.kind)
            .field("level", &self.level)
            .field("text", &Redacted(self.text.len()))
            .field("msg_id", &self.msg_id)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    Background,
    Broadcast,
    Shutdown,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warning,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadFrame,
    TooLarge,
    FeatureNotNegotiated,
    UnsupportedMedia,
    RateLimited,
    Busy,
    ServerBusy,
    ThreadInUse,
    NoPendingApproval,
    UnknownRequest,
    ForbiddenCommand,
    Unavailable,
    Timeout,
    NoSpeech,
    TranscribeFailed,
    IdleTimeout,
    SlowConsumer,
    Shutdown,
    Internal,
    #[serde(other)]
    Unknown,
}

pub struct Redacted(pub usize);

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<redacted {} bytes>", self.0)
    }
}

struct JsonType<'a>(&'a serde_json::Value);

impl fmt::Debug for JsonType<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            serde_json::Value::Null => "null",
            serde_json::Value::Bool(_) => "bool",
            serde_json::Value::Number(_) => "number",
            serde_json::Value::String(_) => "string",
            serde_json::Value::Array(_) => "array",
            serde_json::Value::Object(_) => "object",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1_FRAMES: &str = include_str!("../tests/fixtures/replv3/v1_frames.ndjson");
    const CLIENT_FRAMES: &str = include_str!("../tests/fixtures/replv3/client_frames.ndjson");
    const SERVER_FRAMES: &str = include_str!("../tests/fixtures/replv3/server_frames.ndjson");

    #[test]
    fn v1_frames_round_trip_byte_identical() {
        for line in V1_FRAMES.lines() {
            if let Ok(frame) = serde_json::from_str::<ClientFrame>(line) {
                assert_eq!(
                    encode_client_line(&frame).expect("client encoding"),
                    format!("{line}\n").as_bytes()
                );
            } else {
                let frame: ServerFrame = serde_json::from_str(line).expect("server frame");
                assert_eq!(
                    encode_line(&frame).expect("server encoding"),
                    format!("{line}\n").as_bytes()
                );
            }
        }
    }

    #[test]
    fn v3_frames_round_trip_semantically() {
        for line in CLIENT_FRAMES.lines() {
            let expected: serde_json::Value = serde_json::from_str(line).expect("client JSON");
            let frame: ClientFrame = serde_json::from_str(line).expect("client frame");
            assert_eq!(serde_json::to_value(frame).expect("client value"), expected);
        }
        for line in SERVER_FRAMES.lines() {
            let expected: serde_json::Value = serde_json::from_str(line).expect("server JSON");
            let frame: ServerFrame = serde_json::from_str(line).expect("server frame");
            assert_eq!(serde_json::to_value(frame).expect("server value"), expected);
        }
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let frame: ClientFrame = serde_json::from_str(
            r#"{"Message":{"content":"hello","session_id":null,"future":true}}"#,
        )
        .expect("unknown field");
        assert_eq!(
            serde_json::to_string(&frame).expect("encoding"),
            r#"{"Message":{"content":"hello","session_id":null}}"#
        );
    }

    #[test]
    fn unknown_error_code_maps_to_unknown() {
        let frame: ServerFrame = serde_json::from_str(
            r#"{"Error":{"code":"future_code","message":"x","retryable":false,"fatal":false}}"#,
        )
        .expect("unknown code");
        let ServerFrame::Error(error) = frame else {
            panic!("expected error frame");
        };
        assert_eq!(error.code, ErrorCode::Unknown);
    }

    #[test]
    fn debug_output_redacts_payloads() {
        let secret = "never-print-this-secret";
        let message = MessageFrame {
            content: secret.to_string(),
            session_id: None,
            msg_id: None,
            attachments: vec![WireAttachment {
                mime_type: "image/png".to_string(),
                filename: None,
                data_base64: secret.to_string(),
            }],
        };
        let response = ResponseFrame {
            content: secret.to_string(),
            session_id: None,
            is_complete: true,
            msg_id: None,
        };
        let notice = NoticeFrame {
            kind: NoticeKind::Background,
            level: NoticeLevel::Info,
            text: secret.to_string(),
            msg_id: None,
        };
        let output = format!(
            "{message:?} {response:?} {notice:?} {:?} {:?}",
            ClientFrame::Message(message.clone()),
            ServerFrame::Response(response.clone())
        );
        assert!(!output.contains(secret), "{output}");
        assert!(output.contains("<redacted 23 bytes>"), "{output}");
    }

    #[test]
    fn approval_request_truncation_fields_default_false() {
        let frame: ServerFrame = serde_json::from_str(
            r#"{"ApprovalRequest":{"request_id":"r","tool_name":"shell","description":"run","parameters":"{}","allow_always":false}}"#,
        )
        .expect("approval request");
        let ServerFrame::ApprovalRequest(request) = frame else {
            panic!("expected approval request");
        };
        assert!(!request.parameters_truncated);
        assert_eq!(request.parameters_bytes, 0);
    }
}
