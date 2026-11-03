/**
 * Scripted clipboards (design 11.3.1); nothing here touches the real clipboard.
 *
 * - FakeHostClipboard implements OpenTUI's HostClipboardService: the backend of images/clipboard.ts (inject it
 *   with `createHost: fake.factory`) and the writer of controller/copy.ts (3.12.4; it records writeText calls).
 *   Reads are scripted by the first preferred type: "image/png" answers the image read, "text/uri-list" the file
 *   read and "text/plain" the text read; anything unscripted is { status: "empty" }.
 * - FakeClipboardReader implements images/clipboard.ts ClipboardReader for UI tests of Ctrl+V: it plays queued
 *   ClipboardPaste values and records the signals it was given.
 *
 *   const host = new FakeHostClipboard().holdImage(png)            // Ctrl+V finds a PNG
 *   const reader = createClipboardReader({ ..., createHost: host.factory })
 *   const ui = new FakeClipboardReader().push({ kind: "text", text: "hello" })
 */
import type {
  ClipboardReadOptions,
  ClipboardReadResult,
  ClipboardSelection,
  HostClipboardClearResult,
  HostClipboardOperationOptions,
  HostClipboardOptions,
  HostClipboardService,
  HostClipboardWriteResult,
} from "@opentui/core"
import type { ClipboardPaste, ClipboardReader } from "../../src/images/clipboard.ts"

export type FakeReadScript =
  | ClipboardReadResult
  | ((options: ClipboardReadOptions) => ClipboardReadResult | Promise<ClipboardReadResult>)

export interface FakeClipboardWrite {
  text: string
  selection: ClipboardSelection | undefined
}

const encoder = new TextEncoder()

/** A "read" result carrying `bytes` declared as `mimeType`. */
export function readResult(bytes: Uint8Array | string, mimeType: string): ClipboardReadResult {
  return { status: "read", representation: { mimeType, bytes: typeof bytes === "string" ? encoder.encode(bytes) : bytes } }
}

export class FakeHostClipboard implements HostClipboardService {
  readonly maxWriteBytes: number
  /** Every read, in order. */
  readonly reads: ClipboardReadOptions[] = []
  /** Every writeText, in order. */
  readonly writes: FakeClipboardWrite[] = []
  clears = 0
  disposed = false
  /** Options of every factory() call: the reader must create the host once. */
  readonly created: HostClipboardOptions[] = []
  /** Answer for writeText (default "written"). */
  writeResult: HostClipboardWriteResult = { status: "written" }
  readonly #scripts = new Map<string, FakeReadScript>()
  #all: FakeReadScript | null = null

  constructor(opts: { maxWriteBytes?: number } = {}) {
    this.maxWriteBytes = opts.maxWriteBytes ?? 8 * 1024 * 1024
  }

  /** For ClipboardReaderOptions.createHost; records the options and returns this fake. */
  readonly factory = (options: HostClipboardOptions): HostClipboardService => {
    this.created.push(options)
    return this
  }

  /** Answers reads whose first preferred type is `type`. */
  onRead(type: string, script: FakeReadScript): this {
    this.#scripts.set(type.toLowerCase(), script)
    return this
  }

  /** Answers every read (e.g. { status: "failed", error } or { status: "timed-out" }). */
  onAnyRead(script: FakeReadScript): this {
    this.#all = script
    return this
  }

  /** The clipboard holds an image (answers the image read with `mimeType`, default image/png). */
  holdImage(bytes: Uint8Array, mimeType = "image/png"): this {
    return this.onRead("image/png", readResult(bytes, mimeType))
  }

  holdUriList(...uris: string[]): this {
    return this.onRead("text/uri-list", readResult(uris.map((uri) => `${uri}\r\n`).join(""), "text/uri-list"))
  }

  holdText(text: string): this {
    return this.onRead("text/plain", readResult(text, "text/plain"))
  }

  /** The first preferred type of every read so far. */
  readTypes(): string[] {
    return this.reads.map((read) => read.preferredTypes[0])
  }

  async read(options: ClipboardReadOptions): Promise<ClipboardReadResult> {
    if (this.disposed) throw new Error("Host clipboard service is disposed")
    this.reads.push(options)
    if (options.signal?.aborted) return { status: "cancelled" }
    const script = this.#all ?? this.#scripts.get(options.preferredTypes[0].toLowerCase()) ?? { status: "empty" }
    return typeof script === "function" ? await script(options) : script
  }

  async writeText(text: string, options: HostClipboardOperationOptions = {}): Promise<HostClipboardWriteResult> {
    if (this.disposed) throw new Error("Host clipboard service is disposed")
    this.writes.push({ text, selection: options.selection })
    return this.writeResult
  }

  async clear(_options: HostClipboardOperationOptions = {}): Promise<HostClipboardClearResult> {
    this.clears += 1
    return { status: "cleared" }
  }

  async dispose(): Promise<void> {
    this.disposed = true
  }
}

/** A scripted ClipboardReader: queued pastes in order, then `otherwise` (default { kind: "empty" }). */
export class FakeClipboardReader implements ClipboardReader {
  readonly host: FakeHostClipboard
  /** The signal of every read(). */
  readonly signals: AbortSignal[] = []
  disposed = false
  #queue: (ClipboardPaste | (() => Promise<ClipboardPaste>))[] = []
  #otherwise: ClipboardPaste = { kind: "empty" }

  constructor(host: FakeHostClipboard = new FakeHostClipboard()) {
    this.host = host
  }

  /** Queues pastes; a function is awaited when its turn comes (e.g. to hold a read open). */
  push(...pastes: (ClipboardPaste | (() => Promise<ClipboardPaste>))[]): this {
    this.#queue.push(...pastes)
    return this
  }

  otherwise(paste: ClipboardPaste): this {
    this.#otherwise = paste
    return this
  }

  get reads(): number {
    return this.signals.length
  }

  async read(signal: AbortSignal): Promise<ClipboardPaste> {
    this.signals.push(signal)
    if (signal.aborted) return { kind: "cancelled" }
    const next = this.#queue.shift() ?? this.#otherwise
    return typeof next === "function" ? await next() : next
  }

  hostClipboard(): HostClipboardService {
    return this.host
  }

  async dispose(): Promise<void> {
    this.disposed = true
    await this.host.dispose()
  }
}
