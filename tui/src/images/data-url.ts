/**
 * Generated-image data URLs (design 6.4, 10.5): decodeDataUrl moved from v4:tui/src/util.ts and hardened.
 *
 * Only data:image/(png|jpeg|gif|webp);base64, URLs with at most 16 MiB of strict, padded base64 are decoded, and
 * the decoded bytes must sniff (sniff.ts) as the declared type. Anything else (SVG, non-base64, an oversize or
 * mismatched payload) is rejected; vibetui4 decoded whatever followed the first comma. The result is the only
 * thing ever handed to an <image> element; a server `path` stays a caption.
 */
import type { ImageMime } from "../state/model.ts"
import { sniffImageMime } from "./sniff.ts"

/** Base64 characters of the payload (16 MiB, design 6.4). */
export const MAX_DATA_URL_BASE64_BYTES = 16 * 1024 * 1024

export interface DecodedImage {
  mime: ImageMime
  bytes: Uint8Array
}

export type DataUrlRejection =
  /** Not a data: URL, or no comma. */
  | "not_data_url"
  /** A media type outside image/png, image/jpeg, image/gif, image/webp (e.g. image/svg+xml). */
  | "unsupported_type"
  /** Not ;base64, or not strict padded standard base64. */
  | "not_base64"
  | "too_large"
  | "empty"
  /** The bytes do not sniff as the declared type. */
  | "mismatch"

export type DataUrlResult = { ok: true; image: DecodedImage } | { ok: false; reason: DataUrlRejection }

export interface DataUrlOptions {
  /** Default MAX_DATA_URL_BASE64_BYTES. */
  maxBase64Bytes?: number
}

const ALLOWED_TYPES: ReadonlySet<string> = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"])
/** The media type is never longer than this; the comma must appear within it. */
const MAX_HEADER_CHARS = 64
const BASE64_BODY = /^[A-Za-z0-9+/]*={0,2}$/

export function parseImageDataUrl(url: string, opts: DataUrlOptions = {}): DataUrlResult {
  if (!url.startsWith("data:")) return { ok: false, reason: "not_data_url" }
  const comma = url.indexOf(",", 5)
  if (comma === -1 || comma > 5 + MAX_HEADER_CHARS) return { ok: false, reason: "not_data_url" }
  const header = url.slice(5, comma)
  if (!header.endsWith(";base64")) {
    return { ok: false, reason: ALLOWED_TYPES.has(header.split(";")[0] ?? "") ? "not_base64" : "unsupported_type" }
  }
  const declared = header.slice(0, -";base64".length)
  if (!ALLOWED_TYPES.has(declared)) return { ok: false, reason: "unsupported_type" }
  const body = url.length - comma - 1
  if (body === 0) return { ok: false, reason: "empty" }
  if (body > (opts.maxBase64Bytes ?? MAX_DATA_URL_BASE64_BYTES)) return { ok: false, reason: "too_large" }
  const base64 = url.slice(comma + 1)
  if (base64.length % 4 !== 0 || !BASE64_BODY.test(base64)) return { ok: false, reason: "not_base64" }
  const bytes = new Uint8Array(Buffer.from(base64, "base64"))
  if (bytes.byteLength === 0) return { ok: false, reason: "empty" }
  const sniffed = sniffImageMime(bytes)
  if (sniffed === null || sniffed !== declared) return { ok: false, reason: "mismatch" }
  return { ok: true, image: { mime: sniffed, bytes } }
}

/** The validated image of a data URL, or null (the transport turns null into a warning notice). */
export function decodeDataUrl(url: string, opts: DataUrlOptions = {}): DecodedImage | null {
  const result = parseImageDataUrl(url, opts)
  return result.ok ? result.image : null
}
