//! Server-side image validation shared by the gateway and REPLv3 (TUI design 6.2).
//!
//! Only PNG, JPEG, GIF and WebP are accepted, identified by their magic bytes;
//! the declared MIME type is never trusted. Count and base64 lengths are
//! checked before anything is decoded, so an oversize request is rejected
//! without allocating its decoded form.
//!
//! Image positions in rejections and default filenames are 1-based, as users
//! count them and as the model-visible attachment block numbers them.

use base64::Engine;
use uuid::Uuid;

use crate::channels::{AttachmentKind, IncomingAttachment};

/// Image types accepted from clients, in the order servers advertise them.
pub(crate) const IMAGE_MIME_ALLOWLIST: [&str; 4] =
    ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Max characters of a client-supplied filename after sanitization (D12).
pub(crate) const MAX_IMAGE_FILENAME_CHARS: usize = 128;

const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const JPEG_MAGIC: &[u8] = &[0xFF, 0xD8, 0xFF];
const EXTENSIONS: [(&str, &str); 4] = [
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
];

/// Identifies PNG, JPEG, GIF and WebP from their magic bytes; `None` for anything else.
pub(crate) fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(PNG_MAGIC) {
        Some("image/png")
    } else if bytes.starts_with(JPEG_MAGIC) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        Some("image/webp")
    } else {
        None
    }
}

/// File extension for an allowlisted image type (`png`, `jpg`, `gif`, `webp`).
///
/// Matching ignores ASCII case and MIME parameters; any other type yields `bin`.
pub(crate) fn extension_for(mime: &str) -> &'static str {
    let base = mime.split_once(';').map_or(mime, |(base, _)| base).trim();
    EXTENSIONS
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(base))
        .map_or("bin", |&(_, extension)| extension)
}

/// Count and size limits for the images of one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ImageLimits {
    /// Images per message.
    pub max_count: usize,
    /// Decoded bytes per image.
    pub max_bytes: usize,
    /// Decoded bytes of all images of one message.
    pub max_total_bytes: usize,
}

/// Gateway limits: the 10 MiB JSON body caps decoded images near 7.5 MiB (R27).
pub(crate) const GATEWAY_IMAGE_LIMITS: ImageLimits = ImageLimits {
    max_count: 5,
    max_bytes: 5 * 1024 * 1024,
    max_total_bytes: 7 * 1024 * 1024,
};

/// REPLv3 limits.
// The socket channel (M4) is the consumer; until it is wired only the tests use it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const SOCKET_IMAGE_LIMITS: ImageLimits = ImageLimits {
    max_count: 5,
    max_bytes: 5 * 1024 * 1024,
    max_total_bytes: 10 * 1024 * 1024,
};

/// One image as received on the wire.
#[derive(Clone, Copy)]
pub(crate) struct ImageInput<'a> {
    /// The client's declared MIME type; must start with `image/`, otherwise unused.
    pub declared_mime: &'a str,
    /// Standard base64 (with padding), no `data:` prefix.
    pub data_b64: &'a str,
    /// The client's filename, if the transport carries one.
    pub filename: Option<&'a str>,
}

impl std::fmt::Debug for ImageInput<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the payload or client strings, only their sizes.
        f.debug_struct("ImageInput")
            .field("declared_mime_len", &self.declared_mime.len())
            .field("data_b64_len", &self.data_b64.len())
            .field("has_filename", &self.filename.is_some())
            .finish()
    }
}

/// Why `decode_images` refused a message's images. `index` is the 1-based position.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ImageRejection {
    #[error("{count} images (max {max})")]
    TooMany { count: usize, max: usize },
    #[error("image {index} exceeds {max} bytes")]
    TooLarge { index: usize, max: usize },
    #[error("images exceed {max} bytes in total")]
    TotalTooLarge { max: usize },
    #[error("image {index}: invalid base64")]
    BadBase64 { index: usize },
    #[error("image {index}: unsupported image type (allowed: png, jpeg, gif, webp)")]
    Unsupported { index: usize },
    #[error("image {index} is empty")]
    Empty { index: usize },
}

impl ImageRejection {
    /// The 1-based position of the offending image, when one image is at fault.
    // For the REPLv3 error frames (M4); the gateway maps each variant itself.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn index(&self) -> Option<usize> {
        match self {
            Self::TooLarge { index, .. }
            | Self::BadBase64 { index }
            | Self::Unsupported { index }
            | Self::Empty { index } => Some(*index),
            Self::TooMany { .. } | Self::TotalTooLarge { .. } => None,
        }
    }

    /// True for the size rejections (`TooLarge`, `TotalTooLarge`).
    pub(crate) fn is_size_limit(&self) -> bool {
        matches!(self, Self::TooLarge { .. } | Self::TotalTooLarge { .. })
    }
}

/// Validates and decodes the images of one message.
///
/// Count and base64 lengths are checked before any decoding (D4); content is sniffed and the
/// sniffed type replaces the declared one (D6); ids are `<id_prefix>-<uuid v4>` (D7);
/// filenames are sanitized to <= 128 chars without control characters or path separators (D12).
pub(crate) fn decode_images(
    inputs: &[ImageInput<'_>],
    limits: ImageLimits,
    id_prefix: &str,
) -> Result<Vec<IncomingAttachment>, ImageRejection> {
    if inputs.len() > limits.max_count {
        return Err(ImageRejection::TooMany {
            count: inputs.len(),
            max: limits.max_count,
        });
    }
    let total_b64 = inputs.iter().fold(0usize, |sum, input| {
        sum.saturating_add(input.data_b64.len())
    });
    if total_b64 > base64_len_bound(limits.max_total_bytes) {
        return Err(ImageRejection::TotalTooLarge {
            max: limits.max_total_bytes,
        });
    }
    let mut attachments = Vec::with_capacity(inputs.len());
    let mut total_bytes = 0usize;
    for (position, input) in inputs.iter().enumerate() {
        let index = position.saturating_add(1);
        let (mime, data) = decode_one(input, index, limits, &mut total_bytes)?;
        attachments.push(build_attachment(input, index, mime, data, id_prefix));
    }
    Ok(attachments)
}

/// Checks one image in the order of TUI design 6.2 and returns its sniffed type and bytes.
fn decode_one(
    input: &ImageInput<'_>,
    index: usize,
    limits: ImageLimits,
    total_bytes: &mut usize,
) -> Result<(&'static str, Vec<u8>), ImageRejection> {
    if input.data_b64.len() > base64_len_bound(limits.max_bytes) {
        return Err(ImageRejection::TooLarge {
            index,
            max: limits.max_bytes,
        });
    }
    if !declares_image(input.declared_mime) {
        return Err(ImageRejection::Unsupported { index });
    }
    let data = base64::engine::general_purpose::STANDARD
        .decode(input.data_b64.as_bytes())
        .map_err(|_| ImageRejection::BadBase64 { index })?;
    if data.is_empty() {
        return Err(ImageRejection::Empty { index });
    }
    if data.len() > limits.max_bytes {
        return Err(ImageRejection::TooLarge {
            index,
            max: limits.max_bytes,
        });
    }
    *total_bytes = total_bytes.saturating_add(data.len());
    if *total_bytes > limits.max_total_bytes {
        return Err(ImageRejection::TotalTooLarge {
            max: limits.max_total_bytes,
        });
    }
    let mime = sniff_image_mime(&data)
        .filter(|mime| IMAGE_MIME_ALLOWLIST.contains(mime))
        .ok_or(ImageRejection::Unsupported { index })?;
    if !declared_base(input.declared_mime).eq_ignore_ascii_case(mime) {
        // Only the position and the sniffed type: client strings never reach the logs.
        tracing::debug!(
            index,
            sniffed = mime,
            "declared image type differs from the content; using the sniffed type"
        );
    }
    Ok((mime, data))
}

fn build_attachment(
    input: &ImageInput<'_>,
    index: usize,
    mime: &'static str,
    data: Vec<u8>,
    id_prefix: &str,
) -> IncomingAttachment {
    let filename = sanitize_filename(input.filename)
        .unwrap_or_else(|| format!("image-{index}.{}", extension_for(mime)));
    IncomingAttachment {
        id: format!("{id_prefix}-{}", Uuid::new_v4()),
        kind: AttachmentKind::Image,
        mime_type: mime.to_string(),
        filename: Some(filename),
        size_bytes: Some(data.len() as u64),
        source_url: None,
        storage_key: None,
        extracted_text: None,
        data,
        duration_secs: None,
    }
}

/// Base64 length of `decoded` bytes with padding: `4 * ceil(decoded / 3)`.
fn base64_len_bound(decoded: usize) -> usize {
    decoded.div_ceil(3).saturating_mul(4)
}

/// `image/` prefix, ASCII case-insensitive.
fn declares_image(declared: &str) -> bool {
    declared
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/"))
}

/// The declared type without parameters (`image/png; x=y` -> `image/png`).
fn declared_base(declared: &str) -> &str {
    declared
        .split_once(';')
        .map_or(declared, |(base, _)| base)
        .trim()
}

/// Last path component, without control or invisible formatting characters, trimmed and
/// cut to [`MAX_IMAGE_FILENAME_CHARS`]. `None` when nothing usable is left.
fn sanitize_filename(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let base = raw.rsplit_once(['/', '\\']).map_or(raw, |(_, name)| name);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !is_invisible_format_char(*c))
        .skip_while(|c| c.is_whitespace())
        .take(MAX_IMAGE_FILENAME_CHARS)
        .collect();
    let name = cleaned.trim_end();
    if name.is_empty() || name.chars().all(|c| c == '.') {
        None
    } else {
        Some(name.to_string())
    }
}

/// Zero-width and bidirectional formatting characters, which can disguise a filename.
fn is_invisible_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: usize = 1024 * 1024;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// PNG magic followed by filler, `len` bytes in total.
    fn png(len: usize) -> Vec<u8> {
        let mut bytes = PNG_MAGIC.to_vec();
        bytes.resize(len.max(PNG_MAGIC.len()), 0x42);
        bytes
    }

    fn webp() -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&[0x24, 0, 0, 0]);
        bytes.extend_from_slice(b"WEBPVP8 ");
        bytes
    }

    fn input<'a>(declared_mime: &'a str, data_b64: &'a str) -> ImageInput<'a> {
        ImageInput {
            declared_mime,
            data_b64,
            filename: None,
        }
    }

    fn named<'a>(data_b64: &'a str, filename: &'a str) -> ImageInput<'a> {
        ImageInput {
            declared_mime: "image/png",
            data_b64,
            filename: Some(filename),
        }
    }

    #[test]
    fn sniffs_png_jpeg_gif_webp_only() {
        assert_eq!(sniff_image_mime(&png(16)), Some("image/png"));
        assert_eq!(
            sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10]),
            Some("image/jpeg")
        );
        assert_eq!(sniff_image_mime(b"GIF87a\x01\x00"), Some("image/gif"));
        assert_eq!(sniff_image_mime(b"GIF89a\x01\x00"), Some("image/gif"));
        assert_eq!(sniff_image_mime(&webp()), Some("image/webp"));
        for mime in IMAGE_MIME_ALLOWLIST {
            assert_ne!(extension_for(mime), "bin", "{mime}");
        }
        assert_eq!(extension_for("image/png"), "png");
        assert_eq!(extension_for("IMAGE/JPEG; q=1"), "jpg");
        assert_eq!(extension_for("image/gif"), "gif");
        assert_eq!(extension_for("image/webp"), "webp");
        assert_eq!(extension_for("image/svg+xml"), "bin");
    }

    #[test]
    fn rejects_svg_bmp_tiff_and_short_input() {
        let rejected: [&[u8]; 16] = [
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>",
            b"<?xml version=\"1.0\"?><svg/>",
            b"BM\x36\x00\x00\x00\x00\x00",
            b"II*\x00\x08\x00\x00\x00",
            b"MM\x00*\x00\x00\x00\x08",
            b"\x00\x00\x00\x18ftypheic\x00\x00",
            b"\x00\x00\x00\x1cftypavif\x00\x00",
            b"\x00\x00\x01\x00\x01\x00",
            b"RIFF\x24\x00\x00\x00WAVEfmt ",
            b"GIF90a",
            b"",
            b"\x89PNG\r\n\x1a",
            b"\xFF\xD8",
            b"GIF8",
            b"RIFF\x24\x00\x00\x00WEB",
            b"%PDF-1.7",
        ];
        for bytes in rejected {
            assert_eq!(sniff_image_mime(bytes), None, "{bytes:?}");
        }
    }

    #[test]
    fn decode_checks_base64_length_before_decoding() {
        // 20 MiB of a character that is never valid base64: only a length check can
        // reject it with a size error, so no decoding happened.
        let huge = "!".repeat(20 * MIB);
        let generous_total = ImageLimits {
            max_total_bytes: 64 * MIB,
            ..GATEWAY_IMAGE_LIMITS
        };
        assert_eq!(
            decode_images(&[input("image/png", &huge)], generous_total, "web-image")
                .expect_err("oversize"),
            ImageRejection::TooLarge {
                index: 1,
                max: 5 * MIB
            }
        );
        // With the gateway limits the message-level check fires first.
        let rejection = decode_images(&[input("image/png", &huge)], GATEWAY_IMAGE_LIMITS, "w")
            .expect_err("oversize");
        assert_eq!(rejection, ImageRejection::TotalTooLarge { max: 7 * MIB });
        assert!(rejection.is_size_limit());
        // The per-image length check precedes the declared-type check as well.
        let small = b64(&png(32));
        let rejection = decode_images(
            &[input("image/png", &small), input("text/plain", &huge)],
            generous_total,
            "w",
        )
        .expect_err("oversize second image");
        assert_eq!(
            rejection,
            ImageRejection::TooLarge {
                index: 2,
                max: 5 * MIB
            }
        );
        assert_eq!(rejection.index(), Some(2));
        // The same characters within the limits are a base64 error.
        assert_eq!(
            decode_images(&[input("image/png", "!!!!")], GATEWAY_IMAGE_LIMITS, "w")
                .expect_err("bad base64"),
            ImageRejection::BadBase64 { index: 1 }
        );
    }

    #[test]
    fn decode_uses_sniffed_mime_not_declared() {
        let png_b64 = b64(&png(64));
        let webp_b64 = b64(&webp());
        let images = decode_images(
            &[
                input("image/jpeg", &png_b64),
                input("IMAGE/PNG", &webp_b64),
                input("image/png; charset=binary", &png_b64),
            ],
            GATEWAY_IMAGE_LIMITS,
            "web-image",
        )
        .expect("valid images");
        assert_eq!(images.len(), 3);
        assert_eq!(images[0].mime_type, "image/png");
        assert_eq!(images[0].filename.as_deref(), Some("image-1.png"));
        assert_eq!(images[0].kind, AttachmentKind::Image);
        assert_eq!(images[0].size_bytes, Some(64));
        assert_eq!(images[0].data, png(64));
        assert_eq!(images[1].mime_type, "image/webp");
        assert_eq!(images[1].filename.as_deref(), Some("image-2.webp"));
        assert_eq!(images[2].mime_type, "image/png");

        // A non-image declaration is refused even for valid image bytes.
        assert_eq!(
            decode_images(
                &[input("application/octet-stream", &png_b64)],
                GATEWAY_IMAGE_LIMITS,
                "w"
            )
            .expect_err("non-image declaration"),
            ImageRejection::Unsupported { index: 1 }
        );
        // An image declaration cannot smuggle other content.
        let svg = b64(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>");
        let rejection = decode_images(
            &[input("image/png", &png_b64), input("image/svg+xml", &svg)],
            GATEWAY_IMAGE_LIMITS,
            "w",
        )
        .expect_err("svg");
        assert_eq!(rejection, ImageRejection::Unsupported { index: 2 });
        assert!(!rejection.is_size_limit());
        assert_eq!(
            decode_images(&[input("image/png", "")], GATEWAY_IMAGE_LIMITS, "w").expect_err("empty"),
            ImageRejection::Empty { index: 1 }
        );
    }

    #[test]
    fn decode_rejects_more_than_max_count() {
        let png_b64 = b64(&png(16));
        let six = [input("image/png", &png_b64); 6];
        assert_eq!(
            decode_images(&six, GATEWAY_IMAGE_LIMITS, "w").expect_err("six images"),
            ImageRejection::TooMany { count: 6, max: 5 }
        );
        // The count is checked before anything else, even invalid data.
        let invalid = [input("text/plain", "!!!!"); 6];
        let rejection = decode_images(&invalid, SOCKET_IMAGE_LIMITS, "w").expect_err("count");
        assert_eq!(rejection, ImageRejection::TooMany { count: 6, max: 5 });
        assert_eq!(rejection.index(), None);
        let five = &six[..5]; // safety: slices an array of ImageInput, not a string
        assert_eq!(
            decode_images(five, GATEWAY_IMAGE_LIMITS, "w")
                .expect("five images")
                .len(),
            5
        );
        assert!(
            decode_images(&[], GATEWAY_IMAGE_LIMITS, "w")
                .expect("no images")
                .is_empty()
        );
    }

    #[test]
    fn decode_enforces_total_bytes() {
        // Message-level base64 bound: 4 * ceil(150 / 3) = 200 < 2 * 136.
        let limits = ImageLimits {
            max_count: 5,
            max_bytes: 100,
            max_total_bytes: 150,
        };
        let hundred = b64(&png(100));
        assert_eq!(
            decode_images(
                &[input("image/png", &hundred), input("image/png", &hundred)],
                limits,
                "w"
            )
            .expect_err("total"),
            ImageRejection::TotalTooLarge { max: 150 }
        );

        // Base64 total 100 + 104 = 204 = 4 * ceil(151 / 3) passes the length bound, but the
        // decoded total 75 + 77 = 152 exceeds 151.
        let a = b64(&png(75));
        let b = b64(&png(77));
        assert_eq!(a.len() + b.len(), 204);
        let tight = ImageLimits {
            max_total_bytes: 151,
            ..limits
        };
        assert_eq!(
            decode_images(
                &[input("image/png", &a), input("image/png", &b)],
                tight,
                "w"
            )
            .expect_err("decoded total"),
            ImageRejection::TotalTooLarge { max: 151 }
        );
        let exact = ImageLimits {
            max_total_bytes: 152,
            ..limits
        };
        let images = decode_images(
            &[input("image/png", &a), input("image/png", &b)],
            exact,
            "w",
        )
        .expect("exactly at the limit");
        assert_eq!(images.len(), 2);

        // Per-image decoded size: 101 bytes fit the base64 bound (136) but not max_bytes.
        let over = b64(&png(101));
        assert_eq!(
            decode_images(&[input("image/png", &over)], limits, "w").expect_err("per image"),
            ImageRejection::TooLarge { index: 1, max: 100 }
        );
    }

    #[test]
    fn decode_ids_are_prefixed_unique_uuids() {
        let png_b64 = b64(&png(16));
        let inputs = [input("image/png", &png_b64); 3];
        let first = decode_images(&inputs, GATEWAY_IMAGE_LIMITS, "web-image").expect("first");
        let second = decode_images(&inputs, SOCKET_IMAGE_LIMITS, "repl-image").expect("second");

        let mut seen = std::collections::HashSet::new();
        for (attachments, prefix) in [(&first, "web-image-"), (&second, "repl-image-")] {
            for attachment in attachments.iter() {
                let uuid = attachment
                    .id
                    .strip_prefix(prefix)
                    .unwrap_or_else(|| panic!("{} lacks {prefix}", attachment.id));
                let parsed = Uuid::parse_str(uuid).expect("uuid suffix");
                assert_eq!(parsed.get_version(), Some(uuid::Version::Random));
                assert_eq!(parsed.to_string(), uuid, "hyphenated lower-case form");
                assert!(seen.insert(attachment.id.clone()), "duplicate id");
            }
        }
        assert_eq!(seen.len(), 6);
    }

    #[test]
    fn decode_sanitizes_and_truncates_filenames() {
        let png_b64 = b64(&png(16));
        let long = format!("{}.png", "x".repeat(300));
        let wide = "\u{e9}".repeat(200);
        let cases: [(&str, &str); 8] = [
            ("shot.png", "shot.png"),
            ("../../etc/passwd.png", "passwd.png"),
            ("C:\\Users\\me\\Pictures\\shot.png", "shot.png"),
            ("a\u{0}b\nc\t\u{1b}[31md.png", "abc[31md.png"),
            ("evil\u{202E}gnp.exe", "evilgnp.exe"),
            ("  spaced name.png  ", "spaced name.png"),
            ("\u{feff}caf\u{e9}.gif", "caf\u{e9}.gif"),
            ("\u{200b}  \u{2066}lead.png", "lead.png"),
        ];
        for (raw, expected) in cases {
            let images = decode_images(&[named(&png_b64, raw)], GATEWAY_IMAGE_LIMITS, "w")
                .expect("valid image");
            assert_eq!(images[0].filename.as_deref(), Some(expected), "{raw:?}");
        }

        let images = decode_images(
            &[named(&png_b64, &long), named(&png_b64, &wide)],
            GATEWAY_IMAGE_LIMITS,
            "w",
        )
        .expect("valid images");
        for image in &images {
            let name = image.filename.as_deref().expect("filename");
            assert_eq!(name.chars().count(), MAX_IMAGE_FILENAME_CHARS);
        }
        assert!(
            images[0]
                .filename
                .as_deref()
                .is_some_and(|name| name.bytes().all(|b| b == b'x'))
        );

        // Nothing usable left: the default name with the sniffed extension.
        for raw in ["", "   ", "/", "dir/", "..", ".", "\u{0}\u{7f}", "\u{200b}"] {
            let images = decode_images(
                &[input("image/png", &png_b64), named(&png_b64, raw)],
                GATEWAY_IMAGE_LIMITS,
                "w",
            )
            .expect("valid images");
            assert_eq!(images[0].filename.as_deref(), Some("image-1.png"));
            assert_eq!(
                images[1].filename.as_deref(),
                Some("image-2.png"),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn image_input_debug_omits_payload() {
        let debug = format!("{:?}", named("aGVsbG8=", "secret-name.png"));
        assert!(!debug.contains("aGVsbG8="), "{debug}");
        assert!(!debug.contains("secret-name"), "{debug}");
        assert!(debug.contains("data_b64_len: 8"), "{debug}");
    }
}
