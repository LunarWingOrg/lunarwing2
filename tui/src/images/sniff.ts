/**
 * Image type sniffing from magic bytes (design 6.2, 10.5; the client half of D6).
 *
 * The allowlist is PNG, JPEG, GIF (87a and 89a) and WebP: the same four types ic/src/channels/image_mime.rs
 * accepts on both servers. BMP, SVG/XML, TIFF, HEIC, AVIF, ICO and everything else are rejected. The sniffed type
 * is the only type the client sends or renders; clipboard types, file extensions and data-URL prefixes are never
 * trusted, which is why nothing here takes a declared type.
 *
 * Salvaged from v4:tui/src/images.ts sniffImageMime without the BMP arm, and GIF now needs the full
 * "GIF87a"/"GIF89a" signature instead of "GIF8".
 */
import type { ImageMime } from "../state/model.ts"

export const UNSUPPORTED_IMAGE_MESSAGE = "unsupported image type (allowed: PNG, JPEG, GIF, WebP)"

export type ImageExtension = "png" | "jpg" | "gif" | "webp"

const PNG_SIGNATURE = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a] as const
const JPEG_SIGNATURE = [0xff, 0xd8, 0xff] as const
/** "GIF87a" */
const GIF87A_SIGNATURE = [0x47, 0x49, 0x46, 0x38, 0x37, 0x61] as const
/** "GIF89a" */
const GIF89A_SIGNATURE = [0x47, 0x49, 0x46, 0x38, 0x39, 0x61] as const
/** "RIFF" at 0, then a 4-byte size, then "WEBP" at 8. */
const RIFF_SIGNATURE = [0x52, 0x49, 0x46, 0x46] as const
const WEBP_SIGNATURE = [0x57, 0x45, 0x42, 0x50] as const

function hasSignature(bytes: Uint8Array, signature: readonly number[], offset = 0): boolean {
  if (bytes.length < offset + signature.length) return false
  return signature.every((byte, index) => bytes[offset + index] === byte)
}

/** The allowlisted type of `bytes` by magic bytes; null for anything else (including empty input). */
export function sniffImageMime(bytes: Uint8Array): ImageMime | null {
  if (hasSignature(bytes, PNG_SIGNATURE)) return "image/png"
  if (hasSignature(bytes, JPEG_SIGNATURE)) return "image/jpeg"
  if (hasSignature(bytes, GIF87A_SIGNATURE) || hasSignature(bytes, GIF89A_SIGNATURE)) return "image/gif"
  if (hasSignature(bytes, RIFF_SIGNATURE) && hasSignature(bytes, WEBP_SIGNATURE, 8)) return "image/webp"
  return null
}

const EXTENSIONS: Readonly<Record<ImageMime, ImageExtension>> = Object.freeze({
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/gif": "gif",
  "image/webp": "webp",
})

const TYPE_NAMES: Readonly<Record<ImageMime, string>> = Object.freeze({
  "image/png": "PNG",
  "image/jpeg": "JPEG",
  "image/gif": "GIF",
  "image/webp": "WebP",
})

/** File extension for a sniffed type (mirrors the server's extension_for: png | jpg | gif | webp). */
export function extensionFor(mime: ImageMime): ImageExtension {
  return EXTENSIONS[mime]
}

/** "PNG", "JPEG", "GIF" or "WebP", for messages. */
export function imageTypeName(mime: ImageMime): string {
  return TYPE_NAMES[mime]
}
