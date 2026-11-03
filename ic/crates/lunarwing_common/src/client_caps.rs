//! Wire vocabulary shared by the gateway chat API and REPLv3 (TUI design 4.1).
//!
//! Both TUI transports use one correlation-id rule, one set of feature names,
//! one limits document and one set of turn outcome names, so the client needs a
//! single decoder. The TypeScript mirror is `tui/src/transport/types.ts`.

use serde::{Deserialize, Serialize};

/// Max length of every client-chosen id: gateway `client_turn_id`, REPLv3 `msg_id`,
/// `req_id` and the REPLv3 thread token.
pub const CLIENT_ID_MAX_LEN: usize = 64;

/// `^[A-Za-z0-9_-]{1,64}$`. Ids are opaque: never parsed, never logged beyond the id itself.
///
/// An invalid id is rejected by the server (gateway 400, REPLv3 `Error{bad_frame}`),
/// never rewritten.
pub fn is_valid_client_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= CLIENT_ID_MAX_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Progress events (`status` objects in the gateway SSE shape) are delivered.
pub const FEATURE_STATUS: &str = "status";
/// Image attachments are accepted with user messages.
pub const FEATURE_ATTACHMENTS: &str = "attachments";
/// Server-side speech-to-text is available.
pub const FEATURE_TRANSCRIBE: &str = "transcribe";
/// Reply text streams as `stream_chunk` events.
pub const FEATURE_STREAM: &str = "stream";
/// The daemon loads `state/gateway-client-tokens.json` and authenticates the client
/// tokens `mt-admin tui-access` registers there (M2b). `tui-access` reads the
/// capabilities document with the master token before minting a client token and
/// refuses, writing nothing, unless the running daemon advertises this feature.
pub const FEATURE_CLIENT_TOKENS: &str = "client_tokens";
/// Canonical order used whenever a server lists features.
pub const FEATURES_CANONICAL: [&str; 5] = [
    FEATURE_STATUS,
    FEATURE_ATTACHMENTS,
    FEATURE_TRANSCRIBE,
    FEATURE_STREAM,
    FEATURE_CLIENT_TOKENS,
];

/// How the agent finished handling one inbound message (TUI design 8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    /// A non-empty reply was handed to outbound delivery before this event.
    Replied,
    /// Handling ended without a reply (suppressed, consumed control, stale approval, hook rejection).
    NoReply,
    /// The turn stopped at a gate (approval, credential, external callback); a continuation follows the decision.
    Paused,
    /// A running turn was stopped by `/interrupt`.
    Interrupted,
    /// The turn outlived its wait budget and continues in the background; exactly one more
    /// `Finished` follows when it ends. Not terminal.
    Detached,
    /// The handler returned an error, panicked, or the engine reported failure.
    Failed,
    /// The agent loop's soft timeout fired; the timeout notice was sent.
    TimedOut,
    /// The message never ran: removed from the queue by an interrupt, rejected by a full queue, or shutdown.
    Cancelled,
    /// Forward compatibility for readers; producers never emit it.
    #[serde(other)]
    Unknown,
}

impl TurnOutcome {
    /// The wire name, identical to the serde representation (`"no_reply"`, `"timed_out"`, ...).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Replied => "replied",
            Self::NoReply => "no_reply",
            Self::Paused => "paused",
            Self::Interrupted => "interrupted",
            Self::Detached => "detached",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for TurnOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Limits advertised by `GET /api/chat/capabilities` and REPLv3 `Welcome.limits`.
///
/// The per-transport values are listed in TUI design 4.1; each server fills them
/// from the constants it enforces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireLimits {
    /// UTF-8 bytes of the message text.
    pub max_content_bytes: u64,
    /// Images per message.
    pub max_attachments: u32,
    /// Decoded bytes per image.
    pub max_attachment_bytes: u64,
    /// Decoded bytes of all images of one message.
    pub max_total_attachment_bytes: u64,
    /// Accepted image MIME types (`image/png`, `image/jpeg`, `image/gif`, `image/webp`).
    pub attachment_mime_types: Vec<String>,
    /// Base64 bytes of one transcription request; 0 when transcription is not available.
    pub max_transcribe_b64_bytes: u64,
    /// Accepted audio formats for transcription; empty when transcription is not available.
    pub transcribe_formats: Vec<String>,
    /// Chat messages per minute.
    pub messages_per_minute: u32,
    /// Transcriptions per minute.
    pub transcribes_per_minute: u32,
    /// Gateway only: the JSON request body limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_request_bytes: Option<u64>,
    /// REPLv3 only: the line (frame) limit, including the newline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_frame_bytes: Option<u64>,
    /// REPLv3 only: messages in flight per connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_in_flight: Option<u32>,
    /// REPLv3 only: idle timeout of a connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout_secs: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_OUTCOMES: [(TurnOutcome, &str); 9] = [
        (TurnOutcome::Replied, "replied"),
        (TurnOutcome::NoReply, "no_reply"),
        (TurnOutcome::Paused, "paused"),
        (TurnOutcome::Interrupted, "interrupted"),
        (TurnOutcome::Detached, "detached"),
        (TurnOutcome::Failed, "failed"),
        (TurnOutcome::TimedOut, "timed_out"),
        (TurnOutcome::Cancelled, "cancelled"),
        (TurnOutcome::Unknown, "unknown"),
    ];

    fn gateway_like_limits() -> WireLimits {
        WireLimits {
            max_content_bytes: 100_000,
            max_attachments: 5,
            max_attachment_bytes: 5 * 1024 * 1024,
            max_total_attachment_bytes: 7 * 1024 * 1024,
            attachment_mime_types: vec![
                "image/png".to_string(),
                "image/jpeg".to_string(),
                "image/gif".to_string(),
                "image/webp".to_string(),
            ],
            max_transcribe_b64_bytes: 0,
            transcribe_formats: Vec::new(),
            messages_per_minute: 30,
            transcribes_per_minute: 10,
            max_request_bytes: Some(10 * 1024 * 1024),
            max_frame_bytes: None,
            max_in_flight: None,
            idle_timeout_secs: None,
        }
    }

    #[test]
    fn client_id_accepts_uuid_and_short_ids() {
        let long = "a".repeat(CLIENT_ID_MAX_LEN);
        for id in [
            "4f9c2a1e-7b3d-4c58-9e21-0a6b8d3f5c47",
            "t-4f9c2a1e-7b3d-4c58-9e21-0a6b8d3f5c47",
            "s-4f9c2a1e7b3d4c589e210a6b8d3f5c47",
            "c-0007",
            "t-0003",
            "a",
            "A_b-9",
            long.as_str(),
        ] {
            assert!(is_valid_client_id(id), "{id} should be valid");
        }
    }

    #[test]
    fn client_id_rejects_dot_colon_space_and_65_chars() {
        let too_long = "a".repeat(CLIENT_ID_MAX_LEN + 1);
        for id in [
            "",
            "a.b",
            "a:b",
            "a b",
            " a",
            "a\n",
            "a/b",
            "a+b",
            "caf\u{e9}",
            "\u{0661}",
            too_long.as_str(),
        ] {
            assert!(!is_valid_client_id(id), "{id:?} should be rejected");
        }
    }

    #[test]
    fn turn_outcome_serializes_snake_case() {
        for (outcome, wire) in ALL_OUTCOMES {
            let json = serde_json::to_string(&outcome).expect("serialize outcome");
            assert_eq!(json, format!("\"{wire}\""));
            assert_eq!(outcome.as_str(), wire);
            assert_eq!(outcome.to_string(), wire);
            let back: TurnOutcome = serde_json::from_str(&json).expect("deserialize outcome");
            assert_eq!(back, outcome);
        }
    }

    #[test]
    fn unknown_outcome_deserializes_to_unknown() {
        for wire in [
            "\"exploded\"",
            "\"awaiting_approval\"",
            "\"Replied\"",
            "\"\"",
        ] {
            let outcome: TurnOutcome = serde_json::from_str(wire).expect("deserialize unknown");
            assert_eq!(outcome, TurnOutcome::Unknown, "{wire}");
        }
        let event: serde_json::Value =
            serde_json::json!({"type": "turn_finished", "outcome": "future_value"});
        let outcome: TurnOutcome =
            serde_json::from_value(event["outcome"].clone()).expect("deserialize from value");
        assert_eq!(outcome, TurnOutcome::Unknown);
    }

    #[test]
    fn wire_limits_omit_transport_specific_none_fields() {
        let gateway = gateway_like_limits();
        let value = serde_json::to_value(&gateway).expect("serialize limits");
        let object = value.as_object().expect("limits are an object");
        assert_eq!(object["max_request_bytes"], 10 * 1024 * 1024);
        for absent in ["max_frame_bytes", "max_in_flight", "idle_timeout_secs"] {
            assert!(!object.contains_key(absent), "{absent} must be omitted");
        }
        assert_eq!(object["max_transcribe_b64_bytes"], 0);
        assert_eq!(object["transcribe_formats"], serde_json::json!([]));

        let socket = WireLimits {
            max_request_bytes: None,
            max_frame_bytes: Some(16_842_752),
            max_in_flight: Some(8),
            idle_timeout_secs: Some(180),
            ..gateway.clone()
        };
        let value = serde_json::to_value(&socket).expect("serialize socket limits");
        assert!(value.get("max_request_bytes").is_none());
        assert_eq!(value["max_frame_bytes"], 16_842_752);
        assert_eq!(value["max_in_flight"], 8);
        assert_eq!(value["idle_timeout_secs"], 180);

        // Absent optional fields read back as None; the round trip is lossless.
        let back: WireLimits = serde_json::from_value(serde_json::to_value(&gateway).expect("ser"))
            .expect("deserialize gateway limits");
        assert_eq!(back, gateway);
    }

    #[test]
    fn features_are_listed_in_canonical_order() {
        assert_eq!(
            FEATURES_CANONICAL,
            [
                "status",
                "attachments",
                "transcribe",
                "stream",
                "client_tokens"
            ]
        );
    }
}
