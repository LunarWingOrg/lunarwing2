import { describe, expect, test } from "bun:test"
import { FrameTooLargeError, LineFramer } from "../../src/transport/framing.ts"

const encoder = new TextEncoder()

function bytesOf(text: string): Uint8Array {
  return encoder.encode(text)
}

/** Feeds `bytes` split at `cut`, returning every line produced. */
function feedSplit(framer: LineFramer, bytes: Uint8Array, cut: number): string[] {
  return [...framer.push(bytes.subarray(0, cut)), ...framer.push(bytes.subarray(cut))]
}

describe("LineFramer (3.9, C4, C6)", () => {
  const samples = ["café 😀", "日本語", "👩‍👩‍👧‍👦 family", "mixed é 😀 日本 👨‍💻"]

  for (const sample of samples) {
    test(`UTF-8 split at every byte offset survives: ${sample}`, () => {
      const bytes = bytesOf(`${sample}\n`)
      for (let cut = 0; cut <= bytes.length; cut += 1) {
        const framer = new LineFramer(1024)
        expect(feedSplit(framer, bytes, cut)).toEqual([sample])
      }
    })
  }

  test("a 4-byte code point fed one byte at a time decodes intact", () => {
    const framer = new LineFramer(64)
    const bytes = bytesOf("😀\n")
    const lines: string[] = []
    for (const byte of bytes) lines.push(...framer.push(Uint8Array.of(byte)))
    expect(lines).toEqual(["😀"])
  })

  test("several lines per chunk", () => {
    const framer = new LineFramer(64)
    expect(framer.push(bytesOf("one\ntwo\nthree\npart"))).toEqual(["one", "two", "three"])
    expect(framer.bufferedBytes).toBe(4)
    expect(framer.push(bytesOf("ial\n"))).toEqual(["partial"])
  })

  test("one line across many chunks", () => {
    const framer = new LineFramer(1024)
    const pieces = ["al", "pha ", "be", "ta ", "gam", "ma"]
    for (const piece of pieces) expect(framer.push(bytesOf(piece))).toEqual([])
    expect(framer.push(bytesOf("\n"))).toEqual(["alpha beta gamma"])
  })

  test("CRLF is stripped, including a CR at the end of a chunk", () => {
    const framer = new LineFramer(64)
    expect(framer.push(bytesOf("a\r\nb\r"))).toEqual(["a"])
    expect(framer.push(bytesOf("\nc\n"))).toEqual(["b", "c"])
  })

  test("empty lines are returned as empty strings", () => {
    expect(new LineFramer(8).push(bytesOf("\n\nx\n"))).toEqual(["", "", "x"])
  })

  test("content is kept verbatim, a leading BOM included", () => {
    expect(new LineFramer(16).push(bytesOf("\uFEFFx\n"))).toEqual(["\uFEFFx"])
  })

  test("an oversize line throws FrameTooLargeError and resets", () => {
    const framer = new LineFramer(8)
    expect(framer.push(bytesOf("12345678\n"))).toEqual(["12345678"])
    expect(() => framer.push(bytesOf("123456789"))).toThrow(FrameTooLargeError)
    expect(framer.bufferedBytes).toBe(0)
    expect(framer.push(bytesOf("ok\n"))).toEqual(["ok"])
  })

  test("an oversize line split across chunks throws once the bound is crossed", () => {
    const framer = new LineFramer(10)
    expect(framer.push(bytesOf("123456"))).toEqual([])
    expect(() => framer.push(bytesOf("78901"))).toThrow(FrameTooLargeError)
    expect(framer.push(bytesOf("fresh\n"))).toEqual(["fresh"])
  })

  test("an 8 MiB line in 64 KiB chunks is linear: every byte scanned and decoded exactly once", () => {
    const size = 8 * 1024 * 1024
    const chunk = 64 * 1024
    const framer = new LineFramer(size + 16)
    const block = new Uint8Array(chunk).fill(0x61)
    for (let offset = 0; offset < size; offset += chunk) expect(framer.push(block)).toEqual([])
    const lines = framer.push(Uint8Array.of(0x0a))
    expect(lines.length).toBe(1)
    expect(lines[0]?.length).toBe(size)
    expect(framer.stats).toEqual({ scannedBytes: size + 1, decodedBytes: size })
  })

  test("rejects a non-positive bound", () => {
    expect(() => new LineFramer(0)).toThrow(RangeError)
  })
})
