/**
 * Magic-byte allowlist (design 6.2, 11.3.2): PNG, JPEG, GIF87a, GIF89a and WebP are accepted; BMP, SVG, TIFF,
 * HEIC and empty input are rejected; a declared type never changes the answer.
 */
import { describe, expect, test } from "bun:test"
import {
  ANIMATED_WEBP_MESSAGE,
  extensionFor,
  imageTypeName,
  isAnimatedWebp,
  sniffImageMime,
  UNSUPPORTED_IMAGE_MESSAGE,
} from "../../src/images/sniff.ts"
import { IMAGE_MIME_TYPES } from "../../src/state/model.ts"

const b64 = (text: string): Uint8Array => new Uint8Array(Buffer.from(text, "base64"))
const bytes = (...values: number[]): Uint8Array => new Uint8Array(values)
const ascii = (text: string): Uint8Array => new TextEncoder().encode(text)

/** Real 2x2 or 1x1 images (PIL) so the signatures are the ones encoders write. */
const PNG = b64("iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR4nGM8IWfDwMDAxMDAwMDAAAAOTgEmR2hJIgAAAABJRU5ErkJggg==")
const GIF87A = b64("R0lGODdhAgACAIEAAMgePAAAAAAAAAAAACwAAAAAAgACAAAIBgABCAQQEAA7")
const GIF89A = b64("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7")
const WEBP = b64("UklGRh4AAABXRUJQVlA4TBEAAAAvAUAAAAdQjyKXp/+BiOh/AAA=")
const JPEG_HEAD = bytes(0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46)
/** PIL 11.3: a 3-frame lossless animated WebP (VP8X flags 0x02, then ANIM and ANMF chunks). */
const ANIMATED_WEBP = b64(
  "UklGRrQAAABXRUJQVlA4WAoAAAACAAAAPwAALwAAQU5JTQYAAAAAAAAAAABBTk1GKAAAAAAAAAAAAD8AAC8AAGQAAAJWUDhMDwAAAC8/wAsABxD9j/4HIqL/AQBBTk1GKAAAAAAAAAAAAD8AAC8AAGQAAABWUDhMDwAAAC8/wAsAB9D/iP4HIqL/AQBBTk1GKAAAAAAAAAAAAD8AAC8AAGQAAABWUDhMDwAAAC8/wAsABxDR//4HIqL/AQA=",
)
/** PIL 11.3: a still lossy WebP with alpha (VP8X flags 0x10: alpha only, then ALPH and VP8). */
const ALPHA_WEBP = b64(
  "UklGRn4AAABXRUJQVlA4WAoAAAAQAAAAPwAALwAAQUxQSBAAAAABB1DAiAgACeH/ejGi/6kfVlA4IEgAAADwAwCdASpAADAAPm02mEkkIyKhIqgAgA2JZwDU9oB+AAAVGmMcPgzAAP7wm0P/8guWF1yNf/8gP+QH/ID/+PfFDYqwyAAAAAA=",
)

/** RIFF/WEBP with a VP8X chunk of `flags` (a 1x1 canvas) and nothing else. */
function vp8x(flags: number): Uint8Array {
  return bytes(...ascii("RIFF"), 22, 0, 0, 0, ...ascii("WEBPVP8X"), 10, 0, 0, 0, flags, 0, 0, 0, 0, 0, 0, 0, 0, 0)
}

describe("sniffImageMime", () => {
  test("PNG, JPEG, GIF87a, GIF89a and WebP are accepted", () => {
    expect(sniffImageMime(PNG)).toBe("image/png")
    expect(sniffImageMime(JPEG_HEAD)).toBe("image/jpeg")
    expect(sniffImageMime(bytes(0xff, 0xd8, 0xff))).toBe("image/jpeg")
    expect(sniffImageMime(GIF87A)).toBe("image/gif")
    expect(sniffImageMime(GIF89A)).toBe("image/gif")
    expect(sniffImageMime(WEBP)).toBe("image/webp")
  })

  test("BMP, SVG, TIFF, HEIC and empty input are rejected", () => {
    expect(sniffImageMime(b64("Qk06AAAAAAAAADYAAAAoAAAAAQAAAAEAAAABABgAAAAAAAQAAAATCwAAEwsAAAAAAAAAAAAA////AA=="))).toBeNull()
    expect(sniffImageMime(ascii('<svg xmlns="http://www.w3.org/2000/svg"/>'))).toBeNull()
    expect(sniffImageMime(ascii('<?xml version="1.0"?><svg/>'))).toBeNull()
    expect(sniffImageMime(bytes(0x49, 0x49, 0x2a, 0x00, 0x08, 0x00, 0x00, 0x00))).toBeNull() // TIFF little-endian
    expect(sniffImageMime(bytes(0x4d, 0x4d, 0x00, 0x2a, 0x00, 0x00, 0x00, 0x08))).toBeNull() // TIFF big-endian
    expect(sniffImageMime(bytes(0x00, 0x00, 0x00, 0x18, ...ascii("ftypheic"), 0, 0, 0, 0))).toBeNull()
    expect(sniffImageMime(bytes(0x00, 0x00, 0x00, 0x1c, ...ascii("ftypavif"), 0, 0, 0, 0))).toBeNull()
    expect(sniffImageMime(bytes(0x00, 0x00, 0x01, 0x00, 0x01, 0x00))).toBeNull() // ICO
    expect(sniffImageMime(new Uint8Array())).toBeNull()
  })

  test("truncated or near-miss signatures are rejected", () => {
    expect(sniffImageMime(PNG.slice(0, 7))).toBeNull()
    expect(sniffImageMime(bytes(0xff, 0xd8))).toBeNull()
    expect(sniffImageMime(ascii("GIF8"))).toBeNull()
    expect(sniffImageMime(ascii("GIF88a"))).toBeNull()
    expect(sniffImageMime(ascii("RIFF\x10\x00\x00\x00WAVEfmt "))).toBeNull() // a WAV is RIFF too
    expect(sniffImageMime(ascii("RIFF\x10\x00\x00\x00WEB"))).toBeNull()
  })

  test("declared MIME ignored: only the bytes decide", () => {
    // What a clipboard, a file name or a data-URL prefix claims never enters the decision.
    const claims: { declared: string; data: Uint8Array; expected: string | null }[] = [
      { declared: "image/jpeg", data: PNG, expected: "image/png" },
      { declared: "image/png", data: ascii("<svg onload=alert(1)/>"), expected: null },
      { declared: "image/gif", data: WEBP, expected: "image/webp" },
      { declared: "image/bmp", data: GIF89A, expected: "image/gif" },
    ]
    for (const claim of claims) expect(sniffImageMime(claim.data)).toBe(claim.expected as never)
  })

  test("animated WebP: a VP8X chunk with the animation flag; still WebP, alpha-only VP8X and other types are not", () => {
    expect(sniffImageMime(ANIMATED_WEBP)).toBe("image/webp")
    expect(isAnimatedWebp(ANIMATED_WEBP)).toBe(true)
    expect(isAnimatedWebp(vp8x(0x02))).toBe(true)
    expect(isAnimatedWebp(vp8x(0x02 | 0x10 | 0x20))).toBe(true) // animation plus alpha and ICC
    expect(isAnimatedWebp(WEBP)).toBe(false) // VP8L, simple format
    expect(isAnimatedWebp(ALPHA_WEBP)).toBe(false)
    expect(isAnimatedWebp(vp8x(0x10 | 0x08 | 0x04 | 0x01))).toBe(false) // every flag but animation
    expect(isAnimatedWebp(vp8x(0x02).slice(0, 20))).toBe(false) // truncated before the flags byte
    expect(isAnimatedWebp(PNG)).toBe(false)
    expect(isAnimatedWebp(ascii("RIFF\x16\x00\x00\x00WAVEVP8X\x0a\x00\x00\x00\x02"))).toBe(false)
    expect(ANIMATED_WEBP_MESSAGE).toBe("animated WebP is not supported; convert it to PNG or GIF")
  })

  test("extensions and names cover exactly the client allowlist", () => {
    expect(IMAGE_MIME_TYPES.map(extensionFor)).toEqual(["png", "jpg", "gif", "webp"])
    expect(IMAGE_MIME_TYPES.map(imageTypeName)).toEqual(["PNG", "JPEG", "GIF", "WebP"])
    expect(UNSUPPORTED_IMAGE_MESSAGE).toBe("unsupported image type (allowed: PNG, JPEG, GIF, WebP)")
  })
})
