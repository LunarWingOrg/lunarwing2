//! Durable cross-channel attachment store.
//!
//! Attachments are persisted into the workspace under `attachments/` so a file
//! sent on one channel stays retrievable from every other channel and from the
//! WebUI. The store is channel-independent: retrieval goes through the
//! `attachment_store` tool (or `memory_read`), never through the originating
//! channel's API.
//!
//! Content is written through the normal workspace write path, which means it
//! is prompt-injection-scanned, search-indexed, and subject to the same
//! durability guarantees as the rest of workspace memory.

use std::sync::Arc;

use crate::channels::{IncomingAttachment, IncomingMessage};
use crate::workspace::Workspace;

#[cfg(test)]
use crate::channels::AttachmentKind;

/// Workspace directory holding durable attachment documents.
pub const ATTACHMENT_DIR: &str = "attachments";

/// Maximum bytes of raw attachment data persisted inline (base64) per file.
/// Larger payloads are recorded as metadata-only entries.
const MAX_INLINE_DATA_BYTES: usize = 5 * 1024 * 1024;

/// Maximum attachment document size (metadata + inline data) in bytes.
const MAX_DOCUMENT_BYTES: usize = 6 * 1024 * 1024;

/// Maximum attachment ID length (channel-supplied, must stay bounded).
const MAX_ATTACHMENT_ID_CHARS: usize = 256;

/// Maximum filename length (channel-supplied, must stay bounded).
const MAX_FILENAME_CHARS: usize = 512;

/// Maximum extracted-text length persisted per attachment.
const MAX_EXTRACTED_TEXT_CHARS: usize = 2 * 1024 * 1024;

/// Sanitize a channel-supplied attachment ID into a safe path segment.
///
/// The ID is channel-controlled input, so it is stripped to alphanumerics,
/// `-`, `_`, and `.` and bounded before being embedded in a workspace path.
fn sanitize_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(MAX_ATTACHMENT_ID_CHARS)
        .collect();
    if cleaned.is_empty() {
        "attachment".to_string()
    } else {
        cleaned
    }
}

/// Sanitize a channel-supplied filename into a safe path segment.
fn sanitize_filename(filename: Option<&str>) -> String {
    let raw = filename.unwrap_or("unnamed");
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(MAX_FILENAME_CHARS)
        .collect();
    let trimmed = cleaned.trim_matches('.');
    if trimmed.is_empty() {
        "unnamed".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Build the workspace path for a stored attachment document.
pub fn attachment_path(channel: &str, id: &str, filename: &str) -> String {
    let channel = sanitize_id(channel);
    let id = sanitize_id(id);
    format!("{ATTACHMENT_DIR}/{channel}/{id}/{filename}.md")
}

/// Build the JSON metadata header for an attachment document.
fn metadata_json(message: &IncomingMessage, attachment: &IncomingAttachment) -> serde_json::Value {
    serde_json::json!({
        "attachment_id": attachment.id,
        "channel": message.channel,
        "sender_id": message.sender_id,
        "user_name": message.user_name,
        "received_at": message.received_at.to_rfc3339(),
        "mime_type": attachment.mime_type,
        "filename": attachment.filename,
        "size_bytes": attachment.size_bytes,
        "duration_secs": attachment.duration_secs,
        "source_url": attachment.source_url,
        "storage_key": attachment.storage_key,
    })
}

/// Render one attachment into a Markdown document body.
///
/// Returns `None` when the attachment has no content worth persisting.
fn render_attachment(message: &IncomingMessage, attachment: &IncomingAttachment) -> Option<String> {
    let metadata = metadata_json(message, attachment);
    let metadata = serde_json::to_string_pretty(&metadata).unwrap_or_else(|_| "{}".to_string());

    let mut body = String::new();
    body.push_str(&format!(
        "# {}\n\n",
        sanitize_filename(attachment.filename.as_deref())
    ));
    body.push_str("```json\n");
    body.push_str(&metadata);
    body.push_str("\n```\n\n");

    let mut has_content = false;
    if let Some(text) = &attachment.extracted_text {
        let text = text
            .chars()
            .take(MAX_EXTRACTED_TEXT_CHARS)
            .collect::<String>();
        body.push_str("## Extracted text\n\n");
        body.push_str(&text);
        body.push('\n');
        has_content = true;
    }
    if !attachment.data.is_empty() && attachment.data.len() <= MAX_INLINE_DATA_BYTES {
        let encoded =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &attachment.data);
        body.push_str("## Raw data (base64)\n\n");
        body.push_str(&encoded);
        body.push('\n');
        has_content = true;
    }

    if !has_content {
        return None;
    }

    if body.len() > MAX_DOCUMENT_BYTES {
        tracing::warn!(
            attachment_id = %attachment.id,
            channel = %message.channel,
            bytes = body.len(),
            max = MAX_DOCUMENT_BYTES,
            "attachment document exceeds size cap; persisting metadata only"
        );
        body.truncate(MAX_DOCUMENT_BYTES);
    }

    Some(body)
}

/// Persist all attachments on a message into the durable attachment store.
///
/// Failures are logged and skipped: attachment persistence must never block
/// message processing. Returns the number of attachments persisted.
pub async fn store_attachments(workspace: &Arc<Workspace>, message: &IncomingMessage) -> usize {
    let mut stored = 0usize;
    for attachment in &message.attachments {
        let Some(body) = render_attachment(message, attachment) else {
            continue;
        };
        let filename = sanitize_filename(attachment.filename.as_deref());
        let path = attachment_path(&message.channel, &attachment.id, &filename);
        match workspace.write(&path, &body).await {
            Ok(_) => {
                stored += 1;
                tracing::info!(
                    path = %path,
                    channel = %message.channel,
                    attachment_id = %attachment.id,
                    "Stored attachment in durable cross-channel attachment store"
                );
            }
            Err(e) => {
                tracing::warn!(
                    path = %path,
                    error = %e,
                    "Failed to store attachment in durable attachment store"
                );
            }
        }
    }
    stored
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_attachment(id: &str, kind: AttachmentKind) -> IncomingAttachment {
        IncomingAttachment {
            id: id.to_string(),
            kind,
            mime_type: "application/octet-stream".to_string(),
            filename: Some("report.pdf".to_string()),
            size_bytes: Some(42),
            source_url: None,
            storage_key: None,
            extracted_text: None,
            data: vec![],
            duration_secs: None,
        }
    }

    fn test_message(channel: &str, attachments: Vec<IncomingAttachment>) -> IncomingMessage {
        IncomingMessage::new(channel, "user-1", "please look at this").with_attachments(attachments)
    }

    #[test]
    fn sanitize_id_strips_path_separators_and_bounds_length() {
        assert_eq!(sanitize_id("../../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize_id("a/b\\c"), "abc");
        assert_eq!(sanitize_id(""), "attachment");
        let long = "x".repeat(1000);
        assert_eq!(sanitize_id(&long).len(), MAX_ATTACHMENT_ID_CHARS);
    }

    #[test]
    fn sanitize_filename_strips_traversal_and_bounds_length() {
        assert_eq!(sanitize_filename(Some("../../secret.txt")), "secret.txt");
        assert_eq!(sanitize_filename(Some("..")), "unnamed");
        assert_eq!(sanitize_filename(None), "unnamed");
        let long = "y".repeat(1000);
        assert_eq!(sanitize_filename(Some(&long)).len(), MAX_FILENAME_CHARS);
    }

    #[test]
    fn attachment_path_is_channel_scoped_and_sanitized() {
        let path = attachment_path("xmpp", "file-123", "report.pdf");
        assert_eq!(path, "attachments/xmpp/file-123/report.pdf.md");
        let path = attachment_path("../../etc", "../passwd", "..");
        assert_eq!(path, "attachments/etc/passwd/unnamed.md");
    }

    #[test]
    fn document_with_extracted_text_is_rendered() {
        let mut attachment = test_attachment("doc-1", AttachmentKind::Document);
        attachment.extracted_text = Some("Q3 results summary".to_string());
        let message = test_message("xmpp", vec![attachment]);

        let body = render_attachment(&message, &message.attachments[0]).unwrap();
        assert!(body.contains("# report.pdf"));
        assert!(body.contains("\"channel\": \"xmpp\""));
        assert!(body.contains("\"mime_type\": \"application/octet-stream\""));
        assert!(body.contains("Q3 results summary"));
    }

    #[test]
    fn inline_data_is_base64_encoded() {
        let mut attachment = test_attachment("img-1", AttachmentKind::Image);
        attachment.data = vec![0xFF, 0xD8, 0xFF];
        let message = test_message("signal", vec![attachment]);

        let body = render_attachment(&message, &message.attachments[0]).unwrap();
        assert!(body.contains("/wj/"));
    }

    #[test]
    fn attachment_without_content_is_skipped() {
        let attachment = test_attachment("empty-1", AttachmentKind::Document);
        let message = test_message("web", vec![attachment]);
        assert!(render_attachment(&message, &message.attachments[0]).is_none());
    }

    #[test]
    fn oversized_inline_data_is_not_embedded() {
        let mut attachment = test_attachment("big-1", AttachmentKind::Document);
        attachment.data = vec![0u8; MAX_INLINE_DATA_BYTES + 1];
        let message = test_message("xmpp", vec![attachment]);

        let body = render_attachment(&message, &message.attachments[0]).unwrap();
        assert!(!body.contains("## Raw data"));
        assert!(body.contains("\"size_bytes\": 42"));
    }

    #[test]
    fn metadata_records_channel_and_sender_for_cross_channel_provenance() {
        let mut attachment = test_attachment("doc-2", AttachmentKind::Document);
        attachment.extracted_text = Some("hello".to_string());
        let message = test_message("darkirc", vec![attachment])
            .with_sender_id("alice")
            .with_user_name("Alice");

        let body = render_attachment(&message, &message.attachments[0]).unwrap();
        assert!(body.contains("\"channel\": \"darkirc\""));
        assert!(body.contains("\"sender_id\": \"alice\""));
        assert!(body.contains("\"user_name\": \"Alice\""));
    }
}
