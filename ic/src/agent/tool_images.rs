//! Tool-result images in the v1 chat loop.
//!
//! The images of one batch of tool calls ride on a single user-role message
//! appended after that batch's tool results: no backend the daemon talks to
//! accepts images inside a tool-role message (OpenAI Chat Completions allows
//! text only there, and TensorZero stringifies it). The message lives only in
//! the turn's in-memory context: nothing here is recorded on the session
//! turn or persisted, so images never reach history or the database.
//!
//! A batch shows at most [`MAX_TOOL_IMAGES_PER_STEP`] images and
//! [`MAX_TOOL_IMAGE_BYTES_PER_REQUEST`] bytes; the rest are replaced by a note
//! on their tool's text. Before each model call only the newest
//! [`MAX_RETAINED_TOOL_IMAGE_STEPS`] image messages, within the same byte
//! budget, keep their images; older ones keep their text and say the images
//! were dropped. Engine V2 applies the same limits.

use crate::llm::{ChatMessage, ContentPart, ImageUrl};
use crate::tools::ToolImage;
use crate::tools::images::{
    MAX_TOOL_IMAGE_BYTES_PER_REQUEST, MAX_TOOL_IMAGES_PER_STEP, tool_images_message_text,
};

pub(crate) use crate::tools::images::{MAX_RETAINED_TOOL_IMAGE_STEPS, is_tool_images_message};

const DROPPED_SUFFIX: &str =
    " (These images are no longer attached. Take a new screenshot if you need to see the screen.)";

/// Images returned by one tool call of a batch.
pub(crate) struct ToolCallImages {
    pub tool_name: String,
    pub call_id: String,
    pub images: Vec<ToolImage>,
}

/// Running image budget for one batch of tool calls.
#[derive(Debug, Default)]
pub(crate) struct BatchImageBudget {
    images: usize,
    bytes: usize,
}

impl BatchImageBudget {
    /// Keep the images that still fit, in order; returns them and how many
    /// were left out.
    pub(crate) fn admit(&mut self, images: Vec<ToolImage>) -> (Vec<ToolImage>, usize) {
        let mut kept = Vec::new();
        let mut omitted = 0;
        for image in images {
            if self.images < MAX_TOOL_IMAGES_PER_STEP
                && self.bytes + image.len() <= MAX_TOOL_IMAGE_BYTES_PER_REQUEST
            {
                self.images += 1;
                self.bytes += image.len();
                kept.push(image);
            } else {
                omitted += 1;
            }
        }
        (kept, omitted)
    }
}

/// Build the user-role message carrying a batch's images, in call order.
/// Returns `None` when the batch returned no images.
pub(crate) fn tool_images_message(batch: Vec<ToolCallImages>) -> Option<ChatMessage> {
    let batch: Vec<_> = batch
        .into_iter()
        .filter(|call| !call.images.is_empty())
        .collect();
    if batch.is_empty() {
        return None;
    }
    let text = tool_images_message_text(batch.iter().map(|call| {
        (
            call.tool_name.as_str(),
            call.call_id.as_str(),
            call.images.len(),
        )
    }));
    let parts = batch
        .iter()
        .flat_map(|call| call.images.iter())
        .map(|image| ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: image.data_url(),
                detail: None,
            },
        })
        .collect();
    Some(ChatMessage::user_with_parts(text, parts))
}

/// Keep images only on the newest `keep_steps` tool-images messages whose
/// images fit [`MAX_TOOL_IMAGE_BYTES_PER_REQUEST`] together. Older ones lose
/// their image parts and say so. User attachments are not touched.
pub(crate) fn retain_recent_tool_images(messages: &mut [ChatMessage], keep_steps: usize) {
    let mut kept_steps = 0usize;
    let mut kept_bytes = 0usize;
    for message in messages.iter_mut().rev() {
        if !is_tool_images_message(message) || message.content_parts.is_empty() {
            continue;
        }
        let bytes = decoded_size(&message.content_parts);
        if kept_steps < keep_steps && kept_bytes + bytes <= MAX_TOOL_IMAGE_BYTES_PER_REQUEST {
            kept_steps += 1;
            kept_bytes += bytes;
            continue;
        }
        message.content_parts.clear();
        if !message.content.ends_with(DROPPED_SUFFIX) {
            message.content.push_str(DROPPED_SUFFIX);
        }
    }
}

/// Approximate decoded size of `data:` URL image parts.
fn decoded_size(parts: &[ContentPart]) -> usize {
    parts
        .iter()
        .map(|part| match part {
            ContentPart::ImageUrl { image_url } => {
                let payload = image_url
                    .url
                    .split_once(',')
                    .map_or(image_url.url.as_str(), |(_, data)| data);
                payload.len() / 4 * 3
            }
            ContentPart::Text { .. } => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Role;
    use crate::tools::images::{TEST_PNG, test_png_of_size};

    fn png() -> ToolImage {
        ToolImage::from_bytes(TEST_PNG.to_vec()).expect("png")
    }

    fn call(name: &str, call_id: &str, images: Vec<ToolImage>) -> ToolCallImages {
        ToolCallImages {
            tool_name: name.to_string(),
            call_id: call_id.to_string(),
            images,
        }
    }

    #[test]
    fn builds_one_message_with_every_image_of_the_batch_in_call_order() {
        let message = tool_images_message(vec![
            call("desktop_screenshot", "c1", vec![png()]),
            call("echo", "c2", Vec::new()),
            call("camera", "c3", vec![png(), png()]),
        ])
        .expect("message");

        assert_eq!(message.role, Role::User);
        assert!(is_tool_images_message(&message));
        assert!(message.content.contains(
            "1 image from `desktop_screenshot` (call c1), 2 images from `camera` (call c3)"
        ));
        assert_eq!(message.content_parts.len(), 3);
        assert!(matches!(
            &message.content_parts[0],
            ContentPart::ImageUrl { image_url } if image_url.url.starts_with("data:image/png;base64,")
        ));
        assert!(tool_images_message(vec![call("echo", "c4", Vec::new())]).is_none());
    }

    #[test]
    fn batch_budget_caps_images_and_bytes_in_call_order() {
        let mut budget = BatchImageBudget::default();
        let (kept, omitted) =
            budget.admit((0..MAX_TOOL_IMAGES_PER_STEP + 2).map(|_| png()).collect());
        assert_eq!(kept.len(), MAX_TOOL_IMAGES_PER_STEP);
        assert_eq!(omitted, 2);

        let mut budget = BatchImageBudget::default();
        let big = || ToolImage::from_bytes(test_png_of_size(5 * 1024 * 1024)).expect("big png");
        let (kept, omitted) = budget.admit(vec![big()]);
        assert_eq!((kept.len(), omitted), (1, 0));
        let (kept, omitted) = budget.admit(vec![big(), png()]);
        assert_eq!(
            (kept.len(), omitted),
            (1, 1),
            "the second big image does not fit, the small one does"
        );
    }

    #[test]
    fn only_the_newest_image_messages_keep_their_images() {
        let image_message = || {
            tool_images_message(vec![call("desktop_screenshot", "c", vec![png()])])
                .expect("message")
        };
        let user_attachment = ChatMessage::user_with_parts(
            "look at this",
            vec![ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/png;base64,AAAA".to_string(),
                    detail: None,
                },
            }],
        );
        let mut messages = vec![
            user_attachment,
            image_message(),
            ChatMessage::assistant("clicking"),
            image_message(),
            image_message(),
            image_message(),
        ];

        retain_recent_tool_images(&mut messages, 3);
        retain_recent_tool_images(&mut messages, 3);

        assert_eq!(
            messages[0].content_parts.len(),
            1,
            "user attachments are kept"
        );
        assert!(messages[1].content_parts.is_empty());
        assert!(messages[1].content.ends_with(DROPPED_SUFFIX));
        assert_eq!(
            messages[1].content.matches(DROPPED_SUFFIX).count(),
            1,
            "idempotent"
        );
        for message in &messages[3..] {
            assert_eq!(message.content_parts.len(), 1);
        }
    }

    #[test]
    fn retention_also_respects_the_byte_budget() {
        // Regression guard: retention used to count messages only, so large
        // batches could resend far more than Engine V2's per-request budget.
        let big = || ToolImage::from_bytes(test_png_of_size(3 * 1024 * 1024)).expect("big png");
        let mut messages: Vec<ChatMessage> = (0..3)
            .map(|step| {
                tool_images_message(vec![call(
                    "desktop_screenshot",
                    &format!("c{step}"),
                    vec![big()],
                )])
                .expect("message")
            })
            .collect();

        retain_recent_tool_images(&mut messages, 3);

        let kept: Vec<bool> = messages
            .iter()
            .map(|message| !message.content_parts.is_empty())
            .collect();
        assert_eq!(kept, vec![false, true, true], "3 x 3 MiB exceeds 8 MiB");
    }
}
