use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigureRequest {
    pub jid: String,
    pub password: String,
    #[serde(default)]
    pub dm_policy: String,
    #[serde(default)]
    pub allow_from: Vec<String>,
    #[serde(default)]
    pub allow_rooms: Vec<String>,
    #[serde(default)]
    pub encrypted_rooms: Vec<String>,
    #[serde(default)]
    pub device_id: u32,
    pub omemo_store_dir: Option<String>,
    #[serde(default = "default_true")]
    pub allow_plaintext_fallback: bool,
    #[serde(default)]
    pub max_messages_per_hour: u32,
    pub resource: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigureResponse {
    pub configured: bool,
    pub running: bool,
    pub jid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeMessage {
    pub message_id: String,
    /// Conversation/storage target retained for backward compatibility.
    pub user_id: String,
    /// Actual channel actor. Older bridge payloads omit this field.
    #[serde(default)]
    pub sender_id: Option<String>,
    pub user_name: Option<String>,
    pub content: String,
    pub thread_id: Option<String>,
    pub metadata_json: String,
    #[serde(default)]
    pub attachments: Vec<BridgeAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MessagesQuery {
    /// Return only messages with a cursor greater than this value.
    pub cursor: Option<u64>,
    /// Page size for this fetch. Omitted means the bridge's default page size;
    /// the bridge clamps it to a ceiling that keeps every page inside a
    /// channel's per-callback emit budget (WI#850).
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MessagesResponse {
    /// Cursor of the last message in this page, or the request cursor when the
    /// page is empty. Persist this verbatim: it advances only past the messages
    /// actually returned, so a backlog that did not fit in one page is fetched
    /// on the next poll instead of being skipped (WI#850).
    ///
    /// The bridge's cursor sequence is per-process. A request cursor above the
    /// bridge's current cursor means the bridge restarted; the response then
    /// carries the bridge's current cursor so the poller resynchronizes instead
    /// of waiting behind a position that can never be reached.
    #[serde(default)]
    pub cursor: u64,
    #[serde(default)]
    pub messages: Vec<BridgeMessage>,
}

/// Request body for `POST /v1/messages/drain` (WI#854).
///
/// Select exactly one discard mode for unacknowledged inbound messages.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DrainRequest {
    /// Explicitly discard all pending messages.
    pub all: bool,
    /// Drain only messages that have waited at least this many seconds.
    pub older_than_secs: Option<u64>,
}

impl<'de> Deserialize<'de> for DrainRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            #[serde(default)]
            all: bool,
            older_than_secs: Option<u64>,
        }

        struct ObjectOnly;
        impl<'de> serde::de::Visitor<'de> for ObjectOnly {
            type Value = DrainRequest;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object with named drain options")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                let Fields {
                    all,
                    older_than_secs,
                } = Fields::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(DrainRequest {
                    all,
                    older_than_secs,
                })
            }
        }

        // A derived struct alone also accepts positional arrays. Full discard
        // must name the `all` field explicitly.
        deserializer.deserialize_map(ObjectOnly)
    }
}

/// Result of a drain: how many queued messages were discarded and what is
/// left for the poller. Counts are also logged by the bridge, so a drain is
/// never a silent discard (WI#854).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DrainResponse {
    #[serde(default)]
    pub drained: usize,
    #[serde(default)]
    pub remaining: usize,
    /// Age of the oldest message still queued, if any.
    pub oldest_queued_age_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutboundRateLimitRequest {
    pub max_messages_per_hour: Option<u32>,
    #[serde(default)]
    pub reset_counter: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutboundRateLimitResponse {
    pub configured: bool,
    pub running: bool,
    pub configured_max_messages_per_hour: u32,
    pub active_max_messages_per_hour: u32,
    pub outbound_messages_last_hour: usize,
    pub reset_counter_applied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendRequest {
    pub target: String,
    pub content: String,
    pub metadata_json: String,
    /// Files to upload via XEP-0363 and deliver as out-of-band URLs.
    #[serde(default)]
    pub attachments: Vec<BridgeAttachment>,
}

/// A file attachment forwarded from the WASM channel to the bridge for
/// XEP-0363 HTTP upload. Bytes are base64-encoded for JSON transport.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeAttachment {
    pub filename: String,
    pub mime_type: String,
    /// Base64-encoded (standard alphabet) file bytes.
    pub data_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BridgeStatusResponse {
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub current_cursor: u64,
    #[serde(default)]
    pub queued_messages: usize,
    /// Number of retained entries not yet acknowledged by the poller.
    #[serde(default)]
    pub pending_messages: usize,
    /// Age of the oldest pending message, or `None` if there are none.
    #[serde(default)]
    pub oldest_queued_age_secs: Option<u64>,
    pub jid: Option<String>,
    #[serde(default)]
    pub configured_rooms: Vec<String>,
    #[serde(default)]
    pub rooms_with_presence: Vec<String>,
    #[serde(default)]
    pub configured_max_messages_per_hour: u32,
    #[serde(default)]
    pub active_max_messages_per_hour: u32,
    #[serde(default)]
    pub outbound_messages_last_hour: usize,
    #[serde(default)]
    pub outbound_rate_limit_overridden: bool,
    #[serde(default)]
    pub omemo_enabled: bool,
    pub device_id: Option<u32>,
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub bundle_published: bool,
    #[serde(default)]
    pub prekeys_available: usize,
    pub migration_state: Option<String>,
    pub last_omemo_error: Option<String>,
    #[serde(default)]
    pub encrypted_rooms_total: usize,
    #[serde(default)]
    pub encrypted_rooms_ready: usize,
    pub last_room_error: Option<String>,
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_message_without_attachments_deserializes() {
        let json = r#"{"message_id":"1","user_id":"a@b","content":"hi","metadata_json":"{}"}"#;
        let msg: BridgeMessage = serde_json::from_str(json).expect("deserializes");
        assert_eq!(msg.message_id, "1");
        assert!(msg.sender_id.is_none());
        assert!(msg.attachments.is_empty());
    }

    #[test]
    fn bridge_message_with_attachments_round_trips() {
        let msg = BridgeMessage {
            message_id: "1".to_string(),
            user_id: "a@b".to_string(),
            sender_id: Some("room@conference.example/alice".to_string()),
            user_name: None,
            content: "see file".to_string(),
            thread_id: None,
            metadata_json: "{}".to_string(),
            attachments: vec![BridgeAttachment {
                filename: "photo.jpg".to_string(),
                mime_type: "image/jpeg".to_string(),
                data_base64: "AQID".to_string(),
            }],
        };
        let json = serde_json::to_string(&msg).expect("serializes");
        let decoded: BridgeMessage = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded.attachments.len(), 1);
        assert_eq!(
            decoded.sender_id.as_deref(),
            Some("room@conference.example/alice")
        );
        assert_eq!(decoded.attachments[0].filename, "photo.jpg");
    }

    #[test]
    fn send_request_without_attachments_deserializes() {
        // Older clients omit the attachments field entirely.
        let json = r#"{"target":"a@b","content":"hi","metadata_json":"{}"}"#;
        let request: SendRequest = serde_json::from_str(json).expect("deserializes");
        assert_eq!(request.target, "a@b");
        assert!(request.attachments.is_empty());
    }

    #[test]
    fn send_request_with_attachments_round_trips() {
        let request = SendRequest {
            target: "a@b".to_string(),
            content: "hi".to_string(),
            metadata_json: "{}".to_string(),
            attachments: vec![BridgeAttachment {
                filename: "f.png".to_string(),
                mime_type: "image/png".to_string(),
                data_base64: "AQID".to_string(),
            }],
        };
        let json = serde_json::to_string(&request).expect("serializes");
        let decoded: SendRequest = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded, request);
        assert_eq!(decoded.attachments[0].data_base64, "AQID");
    }

    #[test]
    fn drain_request_rejects_unknown_fields() {
        assert!(serde_json::from_str::<DrainRequest>(r#"{"older_than":3600}"#).is_err());
        assert!(serde_json::from_str::<DrainRequest>("[true,null]").is_err());
    }

    #[test]
    fn drain_response_round_trips() {
        let response = DrainResponse {
            drained: 3,
            remaining: 1,
            oldest_queued_age_secs: Some(42),
        };
        let json = serde_json::to_string(&response).expect("serializes");
        let decoded: DrainResponse = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded, response);
    }

    #[test]
    fn status_without_queue_age_deserializes() {
        // A bridge that predates WI#854 omits the field entirely.
        let json = r#"{"configured":true,"queued_messages":0}"#;
        let status: BridgeStatusResponse = serde_json::from_str(json).expect("deserializes");
        assert_eq!(status.oldest_queued_age_secs, None);
    }
}
