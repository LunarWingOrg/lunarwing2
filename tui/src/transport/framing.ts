/**
 * Byte-level line framing shared by the SSE parser (transport/sse.ts) and the M4 socket transport (design 3.9,
 * 3.13 C4 and C6).
 *
 * - Splits on the byte 0x0A only. 0x0A never occurs inside a multi-byte UTF-8 sequence, so a code point split
 *   across chunks stays in the streaming TextDecoder until its last byte arrives (C4).
 * - Linear: every byte is scanned once and decoded once; a long line is collected as decoded parts and joined
 *   once at its end, never re-concatenated per chunk (C6).
 * - Bounded: a line longer than `maxLineBytes` throws FrameTooLargeError before it is decoded, and the framer
 *   resets so the caller can tear the connection down.
 * - Content is kept verbatim (ignoreBOM: true); a stream-level BOM is the caller's business (sse.ts strips one).
 */

export class FrameTooLargeError extends Error {
  override readonly name = "FrameTooLargeError"

  constructor(readonly maxBytes: number) {
    super(`line longer than ${maxBytes} bytes`)
  }
}

export function isFrameTooLargeError(value: unknown): value is FrameTooLargeError {
  return value instanceof FrameTooLargeError
}

const LF = 0x0a

export interface LineFramerStats {
  /** Bytes inspected by the newline scan (each input byte exactly once). */
  readonly scannedBytes: number
  /** Bytes handed to the decoder (each line byte exactly once; newlines excluded). */
  readonly decodedBytes: number
}

export class LineFramer {
  private decoder = new TextDecoder("utf-8", { ignoreBOM: true })
  private parts: string[] = []
  private pendingBytes = 0
  private scanned = 0
  private decoded = 0

  /** `maxLineBytes`: the most bytes a line may have before its "\n" (a trailing "\r" counts). */
  constructor(private readonly maxLineBytes: number) {
    if (!Number.isSafeInteger(maxLineBytes) || maxLineBytes < 1) {
      throw new RangeError("maxLineBytes must be a positive integer")
    }
  }

  /** Complete lines without "\n" or "\r\n". Throws FrameTooLargeError (and resets) on an oversize line. */
  push(chunk: Uint8Array): string[] {
    const lines: string[] = []
    let start = 0
    while (start < chunk.length) {
      const newline = chunk.indexOf(LF, start)
      const end = newline === -1 ? chunk.length : newline
      this.scanned += (newline === -1 ? chunk.length : newline + 1) - start
      this.append(chunk.subarray(start, end))
      if (newline === -1) break
      lines.push(this.finishLine())
      start = newline + 1
    }
    return lines
  }

  /** Drops a partial line and any decoder state (after an error or a reconnect). */
  reset(): void {
    this.decoder = new TextDecoder("utf-8", { ignoreBOM: true })
    this.parts = []
    this.pendingBytes = 0
  }

  /** Bytes of the current incomplete line. */
  get bufferedBytes(): number {
    return this.pendingBytes
  }

  /** Work counters; framing.test.ts pins linearity (C6) with them. */
  get stats(): LineFramerStats {
    return { scannedBytes: this.scanned, decodedBytes: this.decoded }
  }

  private append(segment: Uint8Array): void {
    if (segment.length === 0) return
    if (this.pendingBytes + segment.length > this.maxLineBytes) {
      this.reset()
      throw new FrameTooLargeError(this.maxLineBytes)
    }
    this.pendingBytes += segment.length
    this.decoded += segment.length
    this.parts.push(this.decoder.decode(segment, { stream: true }))
  }

  private finishLine(): string {
    const tail = this.decoder.decode()
    if (tail.length > 0) this.parts.push(tail)
    const line = this.parts.length === 1 ? (this.parts[0] ?? "") : this.parts.join("")
    this.parts = []
    this.pendingBytes = 0
    return line.endsWith("\r") ? line.slice(0, -1) : line
  }
}
