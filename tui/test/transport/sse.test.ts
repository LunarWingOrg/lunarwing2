import { describe, expect, test } from "bun:test"
import { FrameTooLargeError } from "../../src/transport/framing.ts"
import { SSE_MAX_EVENT_BYTES, SseParser } from "../../src/transport/sse.ts"

const encoder = new TextEncoder()
const bytes = (text: string) => encoder.encode(text)

function parser(maxEventBytes = 1024): SseParser {
  return new SseParser({ maxEventBytes })
}

describe("SseParser (3.8, WHATWG event-stream rules)", () => {
  test("dispatches on a blank line, with the event name and the data", () => {
    const p = parser()
    expect(p.push(bytes('event: response\ndata: {"type":"response"}\n'))).toEqual([])
    expect(p.push(bytes("\n"))).toEqual([{ event: "response", data: '{"type":"response"}' }])
  })

  test("the event type defaults to message and resets after each dispatch", () => {
    const p = parser()
    expect(p.push(bytes("event: a\ndata: 1\n\ndata: 2\n\n"))).toEqual([
      { event: "a", data: "1" },
      { event: "message", data: "2" },
    ])
  })

  test("multi-line data is joined with \\n", () => {
    expect(parser().push(bytes("data: first\ndata: second\ndata:\ndata: fourth\n\n"))).toEqual([
      { event: "message", data: "first\nsecond\n\nfourth" },
    ])
  })

  test("field: value strips exactly one leading space; a field without a colon has an empty value", () => {
    expect(parser().push(bytes("data:  two spaces\n\ndata\n\n"))).toEqual([
      { event: "message", data: " two spaces" },
      { event: "message", data: "" },
    ])
  })

  test("comments call onActivity only", () => {
    const p = parser()
    let activity = 0
    const onActivity = () => {
      activity += 1
    }
    expect(p.push(bytes(":\n\n: keepalive\n\n"), onActivity)).toEqual([])
    expect(activity).toBe(2)
  })

  test("an event without a data line is not dispatched", () => {
    expect(parser().push(bytes("event: ping\n\n"))).toEqual([])
  })

  test("id and retry fields are ignored", () => {
    expect(parser().push(bytes("id: 7\nretry: 1000\nevent: x\ndata: y\nfoo: bar\n\n"))).toEqual([{ event: "x", data: "y" }])
  })

  test("CRLF line endings", () => {
    expect(parser().push(bytes("event: x\r\ndata: y\r\n\r\n"))).toEqual([{ event: "x", data: "y" }])
  })

  test("events split across chunks and inside multi-byte code points", () => {
    const stream = bytes('event: response\ndata: {"content":"café 😀 日本語"}\n\n')
    for (let cut = 1; cut < stream.length; cut += 1) {
      const p = parser()
      const messages = [...p.push(stream.subarray(0, cut)), ...p.push(stream.subarray(cut))]
      expect(messages).toEqual([{ event: "response", data: '{"content":"café 😀 日本語"}' }])
    }
  })

  test("one leading BOM of the stream is ignored", () => {
    expect(parser().push(bytes("\uFEFFdata: x\n\n"))).toEqual([{ event: "message", data: "x" }])
  })

  test("an oversize data line throws FrameTooLargeError", () => {
    const p = parser(32)
    expect(() => p.push(bytes(`data: ${"x".repeat(200)}\n\n`))).toThrow(FrameTooLargeError)
  })

  test("an oversize event made of several data lines throws, and the parser recovers after reset", () => {
    const p = parser(32)
    expect(() => p.push(bytes(`data: ${"x".repeat(20)}\ndata: ${"y".repeat(20)}\n`))).toThrow(FrameTooLargeError)
    p.reset()
    expect(p.push(bytes("data: ok\n\n"))).toEqual([{ event: "message", data: "ok" }])
  })

  test("the default bound accepts a multi-megabyte data line", () => {
    expect(SSE_MAX_EVENT_BYTES).toBe(24 * 1024 * 1024)
    const big = "A".repeat(3 * 1024 * 1024)
    const messages = new SseParser({ maxEventBytes: SSE_MAX_EVENT_BYTES }).push(bytes(`event: image_generated\ndata: ${big}\n\n`))
    expect(messages.length).toBe(1)
    expect(messages[0]?.data.length).toBe(big.length)
  })
})
