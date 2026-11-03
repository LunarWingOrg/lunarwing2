/**
 * Generated-image data URLs (design 6.4, 10.5, 11.3.2): the four types are accepted; image/svg+xml and non-base64
 * are rejected; an oversize payload is rejected; a magic-byte mismatch is rejected.
 */
import { describe, expect, test } from "bun:test"
import { decodeDataUrl, MAX_DATA_URL_BASE64_BYTES, parseImageDataUrl } from "../../src/images/data-url.ts"

const PNG_B64 = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR4nGM8IWfDwMDAxMDAwMDAAAAOTgEmR2hJIgAAAABJRU5ErkJggg=="
const GIF_B64 = "R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7"
const WEBP_B64 = "UklGRh4AAABXRUJQVlA4TBEAAAAvAUAAAAdQjyKXp/+BiOh/AAA="
/** A JPEG signature is all the decoder looks at (no imageInfo here). */
const JPEG_B64 = Buffer.from([0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01]).toString("base64")

describe("decodeDataUrl", () => {
  test("four types accepted, the bytes returned as decoded", () => {
    for (const [mime, data] of [
      ["image/png", PNG_B64],
      ["image/jpeg", JPEG_B64],
      ["image/gif", GIF_B64],
      ["image/webp", WEBP_B64],
    ] as const) {
      const image = decodeDataUrl(`data:${mime};base64,${data}`)
      expect(image?.mime).toBe(mime)
      expect(image ? Buffer.from(image.bytes).toString("base64") : null).toBe(data)
    }
  })

  test("image/svg+xml and other types are rejected", () => {
    const svg = Buffer.from('<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"/>').toString("base64")
    expect(parseImageDataUrl(`data:image/svg+xml;base64,${svg}`)).toEqual({ ok: false, reason: "unsupported_type" })
    expect(parseImageDataUrl(`data:image/bmp;base64,${PNG_B64}`)).toEqual({ ok: false, reason: "unsupported_type" })
    expect(parseImageDataUrl(`data:text/html;base64,${PNG_B64}`)).toEqual({ ok: false, reason: "unsupported_type" })
    expect(parseImageDataUrl(`data:IMAGE/PNG;base64,${PNG_B64}`)).toEqual({ ok: false, reason: "unsupported_type" })
    expect(decodeDataUrl(`data:image/svg+xml;base64,${svg}`)).toBeNull()
  })

  test("non-base64 rejected", () => {
    expect(parseImageDataUrl("data:image/png,%89PNG%0D%0A")).toEqual({ ok: false, reason: "not_base64" })
    expect(parseImageDataUrl(`data:image/png;base64,${PNG_B64.slice(0, -2)}`)).toEqual({ ok: false, reason: "not_base64" })
    expect(parseImageDataUrl(`data:image/png;base64,${PNG_B64.slice(0, 8)} ${PNG_B64.slice(8, 15)}`)).toEqual({
      ok: false,
      reason: "not_base64",
    })
    // The URL-safe alphabet is not what the servers send.
    expect(parseImageDataUrl("data:image/png;base64,iVBORw0K-_8=")).toEqual({ ok: false, reason: "not_base64" })
    expect(parseImageDataUrl("data:image/png;base64,")).toEqual({ ok: false, reason: "empty" })
  })

  test("non-data URLs are rejected, so a server path or URL is never decoded", () => {
    expect(parseImageDataUrl("images/cat.png")).toEqual({ ok: false, reason: "not_data_url" })
    expect(parseImageDataUrl("https://example.invalid/cat.png")).toEqual({ ok: false, reason: "not_data_url" })
    expect(parseImageDataUrl("file:///etc/passwd")).toEqual({ ok: false, reason: "not_data_url" })
    expect(parseImageDataUrl("data:image/png;base64")).toEqual({ ok: false, reason: "not_data_url" })
  })

  test("oversize rejected before decoding", () => {
    expect(MAX_DATA_URL_BASE64_BYTES).toBe(16 * 1024 * 1024)
    const padded = PNG_B64 + "A".repeat(MAX_DATA_URL_BASE64_BYTES - PNG_B64.length + 4)
    expect(parseImageDataUrl(`data:image/png;base64,${padded}`)).toEqual({ ok: false, reason: "too_large" })
    expect(parseImageDataUrl(`data:image/png;base64,${PNG_B64}`, { maxBase64Bytes: PNG_B64.length - 4 })).toEqual({
      ok: false,
      reason: "too_large",
    })
    expect(parseImageDataUrl(`data:image/png;base64,${PNG_B64}`, { maxBase64Bytes: PNG_B64.length }).ok).toBe(true)
  })

  test("magic-byte mismatch rejected", () => {
    expect(parseImageDataUrl(`data:image/jpeg;base64,${PNG_B64}`)).toEqual({ ok: false, reason: "mismatch" })
    expect(parseImageDataUrl(`data:image/png;base64,${GIF_B64}`)).toEqual({ ok: false, reason: "mismatch" })
    const bmp = "Qk06AAAAAAAAADYAAAAoAAAAAQAAAAEAAAABABgAAAAAAAQAAAATCwAAEwsAAAAAAAAAAAAA////AA=="
    expect(parseImageDataUrl(`data:image/png;base64,${bmp}`)).toEqual({ ok: false, reason: "mismatch" })
  })
})
