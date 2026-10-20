import { describe, expect, test } from "bun:test"
import { attachFile, makePendingImage, sniffImageMime } from "../src/images.ts"
import { decodeDataUrl, wrapText } from "../src/util.ts"

describe("sniffImageMime", () => {
  test("detects png, jpeg, gif, webp, bmp", () => {
    expect(sniffImageMime(new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))).toBe("image/png")
    expect(sniffImageMime(new Uint8Array([0xff, 0xd8, 0xff, 0xe0]))).toBe("image/jpeg")
    expect(sniffImageMime(new Uint8Array([0x47, 0x49, 0x46, 0x38, 0x39, 0x61]))).toBe("image/gif")
    expect(
      sniffImageMime(new Uint8Array([0x52, 0x49, 0x46, 0x46, 0x00, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50])),
    ).toBe("image/webp")
    expect(sniffImageMime(new Uint8Array([0x42, 0x4d, 0x00]))).toBe("image/bmp")
  })

  test("rejects non-image bytes", () => {
    expect(sniffImageMime(new Uint8Array([0x68, 0x65, 0x6c, 0x6c, 0x6f]))).toBeNull()
    expect(sniffImageMime(new Uint8Array())).toBeNull()
  })

  test("makePendingImage wraps known images only", () => {
    const image = makePendingImage(new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), "a.png")
    expect(image?.mime_type).toBe("image/png")
    expect(image?.filename).toBe("a.png")
    expect(makePendingImage(new Uint8Array([1, 2, 3]), "a.txt")).toBeNull()
  })
})

describe("attachFile", () => {
  test("reads an image from disk", () => {
    // 1x1 transparent PNG.
    const png = Buffer.from(
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==",
      "base64",
    )
    const path = `/tmp/opencode/lw-tui-attach-${process.pid}.png`
    Bun.write(path, png)
    const image = attachFile(path)
    expect(image?.mime_type).toBe("image/png")
    expect(image?.bytes.length).toBe(png.length)
  })

  test("returns null for missing files", () => {
    expect(attachFile("/nonexistent/image.png")).toBeNull()
  })
})

describe("util", () => {
  test("wrapText wraps to width", () => {
    const lines = wrapText("the quick brown fox jumps over the lazy dog", 16)
    for (const line of lines) {
      expect(line.length).toBeLessThanOrEqual(16)
    }
    expect(lines.join(" ")).toContain("quick brown")
  })

  test("decodeDataUrl roundtrip", () => {
    const bytes = decodeDataUrl(`data:image/png;base64,${Buffer.from([0x89, 0x50, 0x4e, 0x47]).toString("base64")}`)
    expect(bytes ? Array.from(bytes) : []).toEqual([0x89, 0x50, 0x4e, 0x47])
    expect(decodeDataUrl("not-a-data-url")).toBeNull()
  })
})
