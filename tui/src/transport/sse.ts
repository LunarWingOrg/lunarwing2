/**
 * Bounded Server-Sent Events parser (design 3.8), built on LineFramer so UTF-8 split across chunks decodes intact.
 *
 * WHATWG event-stream rules: a blank line dispatches; a line starting with ":" is a comment (keepalive) and only
 * calls onActivity; "field: value" strips one leading space; several data lines join with "\n"; "event" sets the
 * type (default "message"); "id", "retry" and unknown fields are ignored; lines end in "\n" or "\r\n"; one leading
 * BOM of the stream is ignored; an event with no data line is not dispatched.
 *
 * The gateway sends one `event:` and one `data:` line per event (ic/src/channels/web/sse.rs) and an empty comment
 * every 15 s; `image_generated` data lines carry whole data URLs, hence the 24 MiB default bound.
 */
import { FrameTooLargeError, LineFramer } from "./framing.ts"

/** 24 MiB: a multi-megabyte image_generated data URL fits (design 3.8, 4.2.5). */
export const SSE_MAX_EVENT_BYTES = 24 * 1024 * 1024

/** Room for the field name and separator on top of the data bound. */
const LINE_SLACK_BYTES = 64

const BOM = "\uFEFF"

export interface SseMessage {
  event: string
  data: string
}

export interface SseParserOptions {
  /** UTF-8 bytes of one event's data (all data lines together). */
  maxEventBytes: number
}

export class SseParser {
  private readonly framer: LineFramer
  private readonly maxEventBytes: number
  private eventType = ""
  private data: string[] = []
  private dataBytes = 0
  private atStreamStart = true

  constructor(opts: SseParserOptions) {
    this.maxEventBytes = opts.maxEventBytes
    this.framer = new LineFramer(opts.maxEventBytes + LINE_SLACK_BYTES)
  }

  /** Feeds raw bytes; returns the events completed by them. Throws FrameTooLargeError on an oversize event. */
  push(chunk: Uint8Array, onActivity?: () => void): SseMessage[] {
    const messages: SseMessage[] = []
    let lines: string[]
    try {
      lines = this.framer.push(chunk)
    } catch (error) {
      this.reset()
      throw error
    }
    for (const raw of lines) {
      const line = this.stripStreamBom(raw)
      if (line === "") {
        const message = this.dispatch()
        if (message) messages.push(message)
      } else if (line.startsWith(":")) {
        onActivity?.()
      } else {
        this.field(line)
      }
    }
    return messages
  }

  /** Drops partial lines and the event being collected. */
  reset(): void {
    this.framer.reset()
    this.eventType = ""
    this.data = []
    this.dataBytes = 0
  }

  private stripStreamBom(line: string): string {
    if (!this.atStreamStart) return line
    this.atStreamStart = false
    return line.startsWith(BOM) ? line.slice(BOM.length) : line
  }

  private field(line: string): void {
    const colon = line.indexOf(":")
    const name = colon === -1 ? line : line.slice(0, colon)
    let value = colon === -1 ? "" : line.slice(colon + 1)
    if (value.startsWith(" ")) value = value.slice(1)
    if (name === "event") {
      this.eventType = value
    } else if (name === "data") {
      this.addData(value)
    }
  }

  private addData(value: string): void {
    // "\n" between data lines counts towards the bound too.
    this.dataBytes += Buffer.byteLength(value, "utf8") + (this.data.length > 0 ? 1 : 0)
    if (this.dataBytes > this.maxEventBytes) {
      this.reset()
      throw new FrameTooLargeError(this.maxEventBytes)
    }
    this.data.push(value)
  }

  private dispatch(): SseMessage | null {
    if (this.data.length === 0) {
      this.eventType = ""
      return null
    }
    const message = { event: this.eventType === "" ? "message" : this.eventType, data: this.data.join("\n") }
    this.eventType = ""
    this.data = []
    this.dataBytes = 0
    return message
  }
}
