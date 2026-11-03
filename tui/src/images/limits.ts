/**
 * Image validation and limits (design 6.2, 3.7, 3.8, 10.5).
 *
 * Every image the client attaches passes checkImageBytes(): non-empty, within the per-image byte limit, sniffed
 * as PNG, JPEG, GIF or WebP (sniff.ts) and accepted by the server (Limits.imageMimeTypes), not an animated WebP,
 * decodable by OpenTUI's imageInfo() (ot081:image.d.ts:84) with width and height >= 1 and at most 40 megapixels.
 * The pixel cap is the client's decode-bomb guard; OpenTUI 0.8.1 is stricter still: imageInfo() throws ImageError
 * "dimension-limit" above 25,000,000 pixels or 16384 pixels per side (measured with probe images), and that is
 * reported the same way. Animated WebP is refused by name before decoding: imageInfo() throws "unsupported-feature"
 * for it, which would read like a corrupt file (animated GIF decodes fine).
 *
 * Limits come from ServerInfo.limits and are checked when attaching (checkAttachLimits) and again at send time
 * (checkSendLimits), because a reconnect can change them. There is no client-side downscaling: OpenTUI exposes no
 * encoder, so an oversize image is rejected with "select a smaller region (/shot) or compress it".
 */
import { ImageError, imageInfo } from "@opentui/core"
import { IMAGE_MIME_TYPES, type ImageMime, type PendingImage } from "../state/model.ts"
import type { Limits, ServerInfo, TransportKind } from "../transport/types.ts"
import { ANIMATED_WEBP_MESSAGE, imageTypeName, isAnimatedWebp, sniffImageMime, UNSUPPORTED_IMAGE_MESSAGE } from "./sniff.ts"

const KIB = 1024
const MIB = 1024 * 1024

/** width * height cap (10.5). */
export const MAX_IMAGE_PIXELS = 40_000_000
export const OVERSIZE_ADVICE = "select a smaller region (/shot) or compress it"
/** Gateway body budget (3.8): sum(4*ceil(n/3)) + 6*contentBytes + 2 KiB <= maxRequestBytes - 64 KiB. */
export const GATEWAY_BODY_HEADROOM_BYTES = 64 * KIB
export const GATEWAY_BODY_OVERHEAD_BYTES = 2 * KIB
/** Worst-case JSON escaping: a control byte becomes "\u00XX". */
export const JSON_ESCAPE_FACTOR = 6
/** REPLv3 attachment keys and quotes around the base64 (M4); the filename is added with JSON_ESCAPE_FACTOR. */
const SOCKET_ATTACHMENT_OVERHEAD_BYTES = 96

export type ImageInputErrorKind =
  /** Not PNG, JPEG, GIF or WebP, or a type this server does not accept. */
  | "unsupported"
  | "empty"
  /** One image over the per-image byte limit. */
  | "too_large"
  /** More images than the server accepts per message. */
  | "too_many"
  /** Images over the per-message total. */
  | "total_too_large"
  /** The message would exceed the gateway body or socket frame limit. */
  | "request_too_large"
  | "undecodable"
  /** Over the decoder's or the client's pixel cap. */
  | "too_many_pixels"
  /** An empty path or a URI that is not a local file. */
  | "bad_path"
  | "not_found"
  | "not_a_file"
  | "unreadable"
  /** A helper (wl-paste, grim, slurp, xdg-open), the Wayland session or the runtime directory is missing. */
  | "unavailable"
  | "timeout"
  /** A helper failed or a write failed. */
  | "failed"
  /** /image: the bytes were dropped from memory (only the newest 20 images keep them). */
  | "evicted"
  /** /image: there is no such generated image. */
  | "no_image"
  /** /image save: the target exists (O_EXCL). */
  | "exists"
  /** The export directory has the wrong owner, type or mode. */
  | "unsafe_dir"

/** A user-facing image failure; `message` is one line, safe to show as a notice, and never holds image data. */
export class ImageInputError extends Error {
  readonly kind: ImageInputErrorKind

  constructor(kind: ImageInputErrorKind, message: string) {
    super(message)
    this.name = "ImageInputError"
    this.kind = kind
  }
}

export function isImageInputError(value: unknown): value is ImageInputError {
  return value instanceof ImageInputError
}

/** The image part of Limits. */
export type ImageLimits = Pick<Limits, "maxImages" | "maxImageBytes" | "maxTotalImageBytes" | "imageMimeTypes">

/**
 * The gateway's values (3.7, legacy column), for attaching before the first ServerInfo arrives. Sending always
 * re-checks against the server's limits.
 */
export const FALLBACK_IMAGE_LIMITS: Readonly<ImageLimits> = Object.freeze({
  maxImages: 5,
  maxImageBytes: 5 * MIB,
  maxTotalImageBytes: 7 * MIB,
  imageMimeTypes: IMAGE_MIME_TYPES,
})

export function imageLimitsFor(server: ServerInfo | null): ImageLimits {
  return server?.limits ?? FALLBACK_IMAGE_LIMITS
}

/** Why image gestures are refused on this server (3.7 degradation table), or null when images are accepted. */
export function imageInputRefusal(server: ServerInfo | null): string | null {
  if (server === null || server.capabilities.images) return null
  const legacy = server.protocol === "repl-legacy" ? " (legacy REPL)" : ""
  return `this daemon does not accept images${legacy}`
}

// ---------------------------------------------------------------------------------------------------------
// One image
// ---------------------------------------------------------------------------------------------------------

export interface ImageDimensions {
  width: number
  height: number
}

/** Reads the dimensions or throws (OpenTUI's ImageError for malformed or oversize data). */
export type ImageInspector = (bytes: Uint8Array) => ImageDimensions

/** OpenTUI's imageInfo(): header-level for PNG/GIF/WebP; it runs the native decoder for JPEG. */
export const inspectImage: ImageInspector = (bytes) => {
  const info = imageInfo(bytes)
  return { width: info.width, height: info.height }
}

export interface CheckedImage {
  /** Sniffed. */
  mime: ImageMime
  width: number
  height: number
}

export interface CheckImageOptions {
  /** How the image is named in messages, e.g. a file name; default "image". */
  label?: string
  /** Injected in tests; default inspectImage. */
  inspect?: ImageInspector
}

function decoderFailure(error: unknown, label: string): ImageInputError {
  if (error instanceof ImageError) {
    if (error.code === "dimension-limit" || error.code === "memory-limit" || error.code === "out-of-memory") {
      return new ImageInputError("too_many_pixels", `${label} has too many pixels to decode: ${OVERSIZE_ADVICE}`)
    }
    const reason = error.code === "malformed-data" ? "malformed image data" : `decoder: ${error.code}`
    return new ImageInputError("undecodable", `${label} cannot be decoded (${reason})`)
  }
  return new ImageInputError("undecodable", `${label} cannot be decoded`)
}

function megapixels(pixels: number): string {
  return `${Math.round(pixels / 100_000) / 10} MP`
}

/** Sniff, server allowlist, per-image size, decodability and the pixel cap; throws ImageInputError. */
export function checkImageBytes(
  bytes: Uint8Array,
  limits: Pick<ImageLimits, "maxImageBytes" | "imageMimeTypes">,
  options: CheckImageOptions = {},
): CheckedImage {
  const label = options.label ?? "image"
  if (bytes.byteLength === 0) throw new ImageInputError("empty", `${label} is empty`)
  if (bytes.byteLength > limits.maxImageBytes) {
    throw new ImageInputError(
      "too_large",
      `${label} is ${formatBytes(bytes.byteLength)}; the limit is ${formatBytes(limits.maxImageBytes)}: ${OVERSIZE_ADVICE}`,
    )
  }
  const mime = sniffImageMime(bytes)
  if (mime === null) throw new ImageInputError("unsupported", `${label}: ${UNSUPPORTED_IMAGE_MESSAGE}`)
  if (!limits.imageMimeTypes.includes(mime)) throw notAccepted(label, mime, limits.imageMimeTypes)
  if (isAnimatedWebp(bytes)) throw new ImageInputError("unsupported", `${label}: ${ANIMATED_WEBP_MESSAGE}`)
  let dimensions: ImageDimensions
  try {
    dimensions = (options.inspect ?? inspectImage)(bytes)
  } catch (error) {
    throw decoderFailure(error, label)
  }
  const { width, height } = dimensions
  if (!Number.isInteger(width) || !Number.isInteger(height) || width < 1 || height < 1) {
    throw new ImageInputError("undecodable", `${label} has no pixels`)
  }
  if (width * height > MAX_IMAGE_PIXELS) {
    throw new ImageInputError(
      "too_many_pixels",
      `${label} is ${width}x${height} (${megapixels(width * height)}); ` +
        `the limit is ${megapixels(MAX_IMAGE_PIXELS)}: ${OVERSIZE_ADVICE}`,
    )
  }
  return { mime, width, height }
}

function notAccepted(label: string, mime: ImageMime, accepted: readonly ImageMime[]): ImageInputError {
  const allowed = accepted.length === 0 ? "none" : accepted.map(imageTypeName).join(", ")
  const message = `${label}: this daemon does not accept ${imageTypeName(mime)} images (allowed: ${allowed})`
  return new ImageInputError("unsupported", message)
}

// ---------------------------------------------------------------------------------------------------------
// Count and totals
// ---------------------------------------------------------------------------------------------------------

/** At attach time: one more image must keep the count and the per-message total within the limits. */
export function checkAttachLimits(
  pending: readonly Pick<PendingImage, "bytes">[],
  next: Pick<PendingImage, "bytes">,
  limits: Pick<ImageLimits, "maxImages" | "maxTotalImageBytes">,
): void {
  if (pending.length + 1 > limits.maxImages) {
    throw new ImageInputError("too_many", `at most ${limits.maxImages} images per message (Ctrl+R removes the last)`)
  }
  const total = pending.reduce((sum, image) => sum + image.bytes.byteLength, next.bytes.byteLength)
  if (total > limits.maxTotalImageBytes) {
    throw new ImageInputError(
      "total_too_large",
      `the images would be ${formatBytes(total)} in total; the limit is ${formatBytes(limits.maxTotalImageBytes)} per message`,
    )
  }
}

export interface SendImage {
  mime: ImageMime
  bytes: Uint8Array
  filename?: string | null
}

export interface SendCheckInput {
  images: readonly SendImage[]
  /** UTF-8 bytes of the message text. */
  contentBytes: number
  limits: Pick<Limits, "maxImages" | "maxImageBytes" | "maxTotalImageBytes" | "imageMimeTypes" | "maxRequestBytes">
  /** Default "gateway". */
  transport?: TransportKind
}

/** At send time, against the current server limits: count, type, per-image size, total and the request budget. */
export function checkSendLimits(input: SendCheckInput): void {
  const { images, limits } = input
  if (images.length > limits.maxImages) {
    throw new ImageInputError(
      "too_many",
      `at most ${limits.maxImages} images per message; remove ${images.length - limits.maxImages} (Ctrl+R)`,
    )
  }
  let total = 0
  images.forEach((image, index) => {
    const label = `image ${index + 1}`
    const sniffed = sniffImageMime(image.bytes)
    if (sniffed === null || sniffed !== image.mime) throw new ImageInputError("unsupported", `${label}: ${UNSUPPORTED_IMAGE_MESSAGE}`)
    if (!limits.imageMimeTypes.includes(sniffed)) throw notAccepted(label, sniffed, limits.imageMimeTypes)
    if (image.bytes.byteLength > limits.maxImageBytes) {
      throw new ImageInputError(
        "too_large",
        `${label} is ${formatBytes(image.bytes.byteLength)}; the limit is ${formatBytes(limits.maxImageBytes)}: ${OVERSIZE_ADVICE}`,
      )
    }
    total += image.bytes.byteLength
  })
  if (total > limits.maxTotalImageBytes) {
    throw new ImageInputError(
      "total_too_large",
      `the images are ${formatBytes(total)} in total; the limit is ${formatBytes(limits.maxTotalImageBytes)} per message`,
    )
  }
  const transport = input.transport ?? "gateway"
  if (estimateRequestBytes(images, input.contentBytes, transport) > requestBudgetBytes(limits, transport)) {
    const what = transport === "gateway" ? "gateway's" : "socket's"
    throw new ImageInputError(
      "request_too_large",
      `the message and its images exceed the ${what} ${formatBytes(limits.maxRequestBytes)} request limit`,
    )
  }
}

/** Length of the padded base64 encoding of `bytes` bytes. */
export function base64Length(bytes: number): number {
  return 4 * Math.ceil(bytes / 3)
}

/**
 * Upper bound of the request: base64 of every image, the text with worst-case JSON escaping and a fixed 2 KiB for
 * the JSON structure and the other fields (3.8). REPLv3 frames also carry each filename.
 */
export function estimateRequestBytes(
  images: readonly Pick<SendImage, "bytes" | "filename">[],
  contentBytes: number,
  transport: TransportKind = "gateway",
): number {
  let total = GATEWAY_BODY_OVERHEAD_BYTES + JSON_ESCAPE_FACTOR * Math.max(0, contentBytes)
  for (const image of images) {
    total += base64Length(image.bytes.byteLength)
    if (transport === "socket") {
      total += SOCKET_ATTACHMENT_OVERHEAD_BYTES + JSON_ESCAPE_FACTOR * Buffer.byteLength(image.filename ?? "", "utf8")
    }
  }
  return total
}

/** The gateway keeps 64 KiB of headroom below its body limit; a socket frame limit counts the trailing "\n". */
export function requestBudgetBytes(limits: Pick<Limits, "maxRequestBytes">, transport: TransportKind = "gateway"): number {
  return transport === "gateway" ? limits.maxRequestBytes - GATEWAY_BODY_HEADROOM_BYTES : limits.maxRequestBytes - 1
}

// ---------------------------------------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------------------------------------

/** "512 B", "412 KiB", "5 MiB", "7.2 MiB". */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "? B"
  if (bytes < KIB) return `${Math.round(bytes)} B`
  if (bytes < MIB) return `${Math.min(1023, Math.max(1, Math.round(bytes / KIB)))} KiB`
  const tenths = Math.round((bytes / MIB) * 10) / 10
  return `${Number.isInteger(tenths) ? tenths.toFixed(0) : tenths.toFixed(1)} MiB`
}
