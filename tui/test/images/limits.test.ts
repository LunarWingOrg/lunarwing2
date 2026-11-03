/**
 * Image validation and limits (design 6.2, 3.7, 3.8, 11.3.2): per-image, count and total limits; the gateway body
 * budget includes base64 expansion and worst-case JSON escaping; decodability and the pixel caps.
 */
import { describe, expect, test } from "bun:test"
import {
  base64Length,
  checkAttachLimits,
  checkImageBytes,
  checkSendLimits,
  estimateRequestBytes,
  FALLBACK_IMAGE_LIMITS,
  formatBytes,
  GATEWAY_BODY_HEADROOM_BYTES,
  GATEWAY_BODY_OVERHEAD_BYTES,
  imageInputRefusal,
  imageLimitsFor,
  ImageInputError,
  MAX_IMAGE_PIXELS,
  requestBudgetBytes,
  type SendCheckInput,
} from "../../src/images/limits.ts"
import { IMAGE_MIME_TYPES, type ImageMime } from "../../src/state/model.ts"
import type { Capabilities, Limits, ServerInfo } from "../../src/transport/types.ts"

const KIB = 1024
const MIB = 1024 * 1024
const b64 = (text: string): Uint8Array => new Uint8Array(Buffer.from(text, "base64"))

const PNG = b64("iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR4nGM8IWfDwMDAxMDAwMDAAAAOTgEmR2hJIgAAAABJRU5ErkJggg==")
const JPEG = b64(
  "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAUDBAQEAwUEBAQFBQUGBwwIBwcHBw8LCwkMEQ8SEhEPERETFhwXExQaFRERGCEYGh0dHx8fExciJCIeJBweHx7/2wBDAQUFBQcGBw4ICA4eFBEUHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh7/wAARCAACAAIDASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwDzmiiivJP6FP/Z",
)
const GIF = b64("R0lGODdhAgACAIEAAMgePAAAAAAAAAAAACwAAAAAAgACAAAIBgABCAQQEAA7")
const WEBP = b64("UklGRh4AAABXRUJQVlA4TBEAAAAvAUAAAAdQjyKXp/+BiOh/AAA=")
const PNG_SIGNATURE = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]

const ALL = { maxImageBytes: 5 * MIB, imageMimeTypes: IMAGE_MIME_TYPES }

/** `size` bytes that sniff as PNG (the send check sniffs; it does not decode). */
function pngSized(size: number): Uint8Array {
  const out = new Uint8Array(size)
  out.set(PNG_SIGNATURE)
  return out
}

function chunk(type: string, data: Uint8Array): Uint8Array {
  const out = new Uint8Array(12 + data.length)
  const view = new DataView(out.buffer)
  view.setUint32(0, data.length)
  out.set(new TextEncoder().encode(type), 4)
  out.set(data, 8)
  view.setUint32(8 + data.length, Bun.hash.crc32(out.subarray(4, 8 + data.length)) >>> 0)
  return out
}

/** A PNG whose IHDR claims width x height: a decode bomb in a few dozen bytes. */
function pngClaiming(width: number, height: number): Uint8Array {
  const ihdr = new Uint8Array(13)
  const view = new DataView(ihdr.buffer)
  view.setUint32(0, width)
  view.setUint32(4, height)
  ihdr[8] = 8 // bit depth
  ihdr[9] = 6 // RGBA
  const parts = [new Uint8Array(PNG_SIGNATURE), chunk("IHDR", ihdr), chunk("IDAT", new Uint8Array([0x78, 0x9c, 0x03, 0x00])), chunk("IEND", new Uint8Array())]
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0))
  let offset = 0
  for (const part of parts) {
    out.set(part, offset)
    offset += part.length
  }
  return out
}

function rejection(run: () => unknown): ImageInputError {
  try {
    run()
  } catch (error) {
    if (error instanceof ImageInputError) return error
    throw error
  }
  throw new Error("expected an ImageInputError")
}

const GATEWAY_LIMITS: SendCheckInput["limits"] = {
  maxImages: 5,
  maxImageBytes: 5 * MIB,
  maxTotalImageBytes: 7 * MIB,
  imageMimeTypes: IMAGE_MIME_TYPES,
  maxRequestBytes: 10 * MIB,
}

describe("checkImageBytes", () => {
  test("the four types pass with their dimensions; the sniffed type is returned", () => {
    expect(checkImageBytes(PNG, ALL)).toEqual({ mime: "image/png", width: 2, height: 2 })
    expect(checkImageBytes(JPEG, ALL)).toEqual({ mime: "image/jpeg", width: 2, height: 2 })
    expect(checkImageBytes(GIF, ALL)).toEqual({ mime: "image/gif", width: 2, height: 2 })
    expect(checkImageBytes(WEBP, ALL)).toEqual({ mime: "image/webp", width: 2, height: 2 })
  })

  test("per-image limit: an oversize image is rejected with the advice", () => {
    const error = rejection(() => checkImageBytes(PNG, { maxImageBytes: PNG.length - 1, imageMimeTypes: IMAGE_MIME_TYPES }, { label: "shot.png" }))
    expect(error.kind).toBe("too_large")
    expect(error.message).toBe(`shot.png is ${PNG.length} B; the limit is ${PNG.length - 1} B: select a smaller region (/shot) or compress it`)
    expect(checkImageBytes(PNG, { maxImageBytes: PNG.length, imageMimeTypes: IMAGE_MIME_TYPES }).mime).toBe("image/png")
  })

  test("empty, unsupported and server-refused types", () => {
    expect(rejection(() => checkImageBytes(new Uint8Array(), ALL)).kind).toBe("empty")
    const svg = rejection(() => checkImageBytes(new TextEncoder().encode("<svg/>"), ALL))
    expect(svg.kind).toBe("unsupported")
    expect(svg.message).toBe("image: unsupported image type (allowed: PNG, JPEG, GIF, WebP)")
    const refused = rejection(() => checkImageBytes(JPEG, { maxImageBytes: 5 * MIB, imageMimeTypes: ["image/png", "image/webp"] }))
    expect(refused.kind).toBe("unsupported")
    expect(refused.message).toBe("image: this daemon does not accept JPEG images (allowed: PNG, WebP)")
  })

  test("decodability: a valid signature with malformed data is rejected", () => {
    const broken = PNG.slice(0, 40)
    const error = rejection(() => checkImageBytes(broken, ALL, { label: "a.png" }))
    expect(error.kind).toBe("undecodable")
    expect(error.message).toBe("a.png cannot be decoded (malformed image data)")
  })

  test("a decode bomb is stopped by OpenTUI's own dimension limit before any pixel is decoded", () => {
    const bomb = pngClaiming(60_000, 60_000)
    expect(bomb.length).toBeLessThan(100)
    const error = rejection(() => checkImageBytes(bomb, ALL))
    expect(error.kind).toBe("too_many_pixels")
    expect(error.message).toBe("image has too many pixels to decode: select a smaller region (/shot) or compress it")
  })

  test("the client's 40-megapixel cap and the minimum of 1x1", () => {
    expect(MAX_IMAGE_PIXELS).toBe(40_000_000)
    const huge = rejection(() => checkImageBytes(PNG, ALL, { inspect: () => ({ width: 8000, height: 6000 }) }))
    expect(huge.kind).toBe("too_many_pixels")
    expect(huge.message).toBe("image is 8000x6000 (48 MP); the limit is 40 MP: select a smaller region (/shot) or compress it")
    expect(checkImageBytes(PNG, ALL, { inspect: () => ({ width: 8000, height: 5000 }) }).width).toBe(8000)
    expect(rejection(() => checkImageBytes(PNG, ALL, { inspect: () => ({ width: 0, height: 10 }) })).kind).toBe("undecodable")
  })
})

describe("attach-time limits", () => {
  const limits = { maxImages: 2, maxTotalImageBytes: 1000 }

  test("count limit", () => {
    const pending = [{ bytes: pngSized(10) }, { bytes: pngSized(10) }]
    const error = rejection(() => checkAttachLimits(pending, { bytes: pngSized(10) }, limits))
    expect(error.kind).toBe("too_many")
    expect(error.message).toBe("at most 2 images per message (Ctrl+R removes the last)")
    expect(() => checkAttachLimits(pending.slice(1), { bytes: pngSized(10) }, limits)).not.toThrow()
  })

  test("total limit", () => {
    expect(() => checkAttachLimits([{ bytes: pngSized(600) }], { bytes: pngSized(400) }, limits)).not.toThrow()
    const error = rejection(() => checkAttachLimits([{ bytes: pngSized(600) }], { bytes: pngSized(401) }, limits))
    expect(error.kind).toBe("total_too_large")
    expect(error.message).toBe("the images would be 1001 B in total; the limit is 1000 B per message")
  })
})

describe("send-time limits", () => {
  const image = (bytes: Uint8Array, mime: ImageMime = "image/png") => ({ mime, bytes })

  test("count, per-image and total limits against the current server limits", () => {
    const six = Array.from({ length: 6 }, () => image(pngSized(16)))
    expect(rejection(() => checkSendLimits({ images: six, contentBytes: 0, limits: GATEWAY_LIMITS })).kind).toBe("too_many")
    const big = rejection(() => checkSendLimits({ images: [image(pngSized(5 * MIB + 1))], contentBytes: 0, limits: GATEWAY_LIMITS }))
    expect(big.kind).toBe("too_large")
    expect(big.message).toContain("image 1 is 5 MiB; the limit is 5 MiB")
    const total = rejection(() =>
      checkSendLimits({ images: [image(pngSized(4 * MIB)), image(pngSized(4 * MIB))], contentBytes: 0, limits: GATEWAY_LIMITS }),
    )
    expect(total.kind).toBe("total_too_large")
    expect(total.message).toBe("the images are 8 MiB in total; the limit is 7 MiB per message")
  })

  test("the type is re-sniffed: a mismatched or refused type fails", () => {
    expect(rejection(() => checkSendLimits({ images: [image(pngSized(16), "image/jpeg")], contentBytes: 0, limits: GATEWAY_LIMITS })).kind).toBe(
      "unsupported",
    )
    const pngOnly = { ...GATEWAY_LIMITS, imageMimeTypes: ["image/png"] as const }
    expect(rejection(() => checkSendLimits({ images: [image(GIF, "image/gif")], contentBytes: 0, limits: pngOnly })).message).toBe(
      "image 1: this daemon does not accept GIF images (allowed: PNG)",
    )
  })

  test("gateway body budget includes base64 expansion", () => {
    expect(base64Length(3000)).toBe(4000)
    expect(base64Length(3001)).toBe(4004)
    // 3001 raw bytes fit a 6050-byte budget, their 4004 base64 characters plus 2 KiB of JSON do not.
    const limits = { ...GATEWAY_LIMITS, maxRequestBytes: GATEWAY_BODY_HEADROOM_BYTES + GATEWAY_BODY_OVERHEAD_BYTES + 4002 }
    expect(requestBudgetBytes(limits)).toBe(GATEWAY_BODY_OVERHEAD_BYTES + 4002)
    expect(() => checkSendLimits({ images: [image(pngSized(3000))], contentBytes: 0, limits })).not.toThrow()
    const error = rejection(() => checkSendLimits({ images: [image(pngSized(3001))], contentBytes: 0, limits }))
    expect(error.kind).toBe("request_too_large")
    expect(error.message).toContain("exceed the gateway's")
  })

  test("gateway body budget includes worst-case JSON escaping of the text", () => {
    expect(estimateRequestBytes([], 1000)).toBe(GATEWAY_BODY_OVERHEAD_BYTES + 6000)
    const limits = { ...GATEWAY_LIMITS, maxRequestBytes: GATEWAY_BODY_HEADROOM_BYTES + GATEWAY_BODY_OVERHEAD_BYTES + 6000 }
    expect(() => checkSendLimits({ images: [], contentBytes: 1000, limits })).not.toThrow()
    expect(rejection(() => checkSendLimits({ images: [], contentBytes: 1001, limits })).kind).toBe("request_too_large")
    // Today's limits: two 3.5 MiB images plus 98 KiB of text are inside the 10 MiB body limit.
    const halves = [image(pngSized(3.5 * MIB)), image(pngSized(3.5 * MIB))]
    expect(() => checkSendLimits({ images: halves, contentBytes: 98 * KIB, limits: GATEWAY_LIMITS })).not.toThrow()
  })

  test("socket frames also count the filename", () => {
    const named = [{ mime: "image/png" as const, bytes: pngSized(300), filename: "\u0001".repeat(10) }]
    expect(estimateRequestBytes(named, 0, "socket") - estimateRequestBytes(named, 0, "gateway")).toBe(96 + 60)
    expect(requestBudgetBytes({ maxRequestBytes: 1000 }, "socket")).toBe(999)
  })
})

describe("limits from ServerInfo", () => {
  function server(images: boolean, protocol: ServerInfo["protocol"], limits: Partial<Limits> = {}): ServerInfo {
    const capabilities: Capabilities = {
      streaming: false,
      activity: false,
      images,
      generatedImages: false,
      approvals: false,
      transcribe: false,
      correlation: false,
      lifecycle: false,
      interrupt: "text",
      steer: "text",
      threads: false,
      history: false,
      backgroundMessages: false,
      resume: false,
    }
    return {
      transport: protocol === "repl-legacy" || protocol === "replv3" ? "socket" : "gateway",
      protocol,
      protocolVersion: null,
      serverVersion: null,
      agentName: null,
      engine: "unknown",
      sessionId: null,
      threadId: null,
      endpointLabel: "http://127.0.0.1:1/",
      credential: "unknown",
      capabilities,
      limits: {
        maxContentBytes: 100_000,
        maxImages: 3,
        maxImageBytes: 2 * MIB,
        maxTotalImageBytes: 4 * MIB,
        maxRequestBytes: 10 * MIB,
        imageMimeTypes: ["image/png"],
        maxTranscribeWavBytes: 0,
        transcribePerMinute: null,
        chatPerMinute: null,
        maxInFlight: 8,
        ...limits,
      },
      warnings: [],
    }
  }

  test("the server's limits win; the gateway defaults apply only before the first ServerInfo", () => {
    expect(imageLimitsFor(null)).toEqual(FALLBACK_IMAGE_LIMITS)
    expect(FALLBACK_IMAGE_LIMITS).toEqual({ maxImages: 5, maxImageBytes: 5 * MIB, maxTotalImageBytes: 7 * MIB, imageMimeTypes: IMAGE_MIME_TYPES })
    expect(imageLimitsFor(server(true, "gateway-chat")).maxImages).toBe(3)
  })

  test("image gestures are refused where the daemon takes no images", () => {
    expect(imageInputRefusal(null)).toBeNull()
    expect(imageInputRefusal(server(true, "gateway-chat"))).toBeNull()
    expect(imageInputRefusal(server(false, "repl-legacy"))).toBe("this daemon does not accept images (legacy REPL)")
    expect(imageInputRefusal(server(false, "gateway-chat"))).toBe("this daemon does not accept images")
  })
})

describe("formatBytes", () => {
  test("bytes, KiB and MiB with one decimal", () => {
    expect(formatBytes(512)).toBe("512 B")
    expect(formatBytes(412 * KIB)).toBe("412 KiB")
    expect(formatBytes(5 * MIB)).toBe("5 MiB")
    expect(formatBytes(7.25 * MIB)).toBe("7.3 MiB")
    expect(formatBytes(-1)).toBe("? B")
  })
})
