/**
 * Ctrl+V (design 6.1, C7): the OpenTUI host clipboard first, then an async wl-paste (Wayland) or xclip (X11)
 * fallback. Replaces v4:tui/src/images.ts readClipboard, whose chain of synchronous child processes could freeze
 * the UI for 3 x 3 s.
 *
 * 1. Host clipboard, created once on first use: createHostClipboard({timeoutMs: 1500, maxReadBytes:
 *    maxImageBytes}) (ot081:lib/clipboard.d.ts:90-109). Reads prefer ["image/png","image/webp","image/jpeg",
 *    "image/gif"] (bare MIME essences; OpenTUI throws on parameters), then "text/uri-list" (files copied in a file
 *    manager), then "text/plain". "limit-exceeded" is an error; "empty"/"unsupported" moves to the next type;
 *    "failed"/"timed-out" moves to step 2.
 * 2. Only when the host clipboard failed: `wl-paste --list-types` (1.5 s) when WAYLAND_DISPLAY is set, else
 *    `xclip -selection clipboard -t TARGETS -o` when DISPLAY is set and xclip is installed. The first offered of
 *    the four image types, text/uri-list, text/plain* is then read with `wl-paste --no-newline --type <type>`
 *    (or xclip -t <type> -o): 3 s, stdout capped at maxImageBytes + 1 bytes, the child killed when exceeded.
 *    xsel is not used (text only).
 *
 * Every child is an async SpawnFn call with an argv array and the allowlisted child environment; nothing here
 * runs a shell. The clipboard is read only when read() is called, i.e. on an explicit Ctrl+V. Bytes are returned
 * unvalidated with the declared type for the debug log: the caller validates them (attach.ts createPendingImage),
 * and only the sniffed type is ever used.
 */
import {
  createHostClipboard,
  type ClipboardReadResult,
  type Clock,
  type HostClipboardOptions,
  type HostClipboardService,
} from "@opentui/core"
import { bunSpawn, type ChildEnv, type SpawnFn } from "../config/types.ts"
import { sanitizeLine, sanitizeText } from "../sanitize.ts"
import { formatBytes, MAX_IMAGE_PIXELS } from "./limits.ts"
import { parseUriList } from "./paste-paths.ts"

/** Host clipboard read preference for images (6.1). */
export const CLIPBOARD_IMAGE_TYPES = ["image/png", "image/webp", "image/jpeg", "image/gif"] as const
export const HOST_CLIPBOARD_TIMEOUT_MS = 1_500
export const LIST_TYPES_TIMEOUT_MS = 1_500
export const PASTE_TIMEOUT_MS = 3_000
const MAX_TYPE_LIST_BYTES = 64 * 1024
const MAX_REASON_CHARS = 120

export type ClipboardVia = "host" | "wl-paste" | "xclip"

export type ClipboardPaste =
  /** Unvalidated bytes; `declaredMime` is what the clipboard claimed and is never trusted. */
  | { kind: "image"; bytes: Uint8Array; declaredMime: string; via: ClipboardVia }
  /** Local files copied in a file manager; `text` is the paths one per line, for insertion when attaching fails. */
  | { kind: "files"; paths: string[]; text: string }
  /** Sanitized text (escape sequences and control characters removed, line feeds kept). */
  | { kind: "text"; text: string }
  | { kind: "empty" }
  /** The signal was aborted. */
  | { kind: "cancelled" }
  /** One line, for a notice. */
  | { kind: "error"; reason: string }

/** "auto": host, then the fallback; "host": host only; "wl-paste": skip the host (forced fallback, design 3.1 risks). */
export type ClipboardBackend = "auto" | "host" | "wl-paste"

/** What controller/copy.ts needs to write text (3.12.4). */
export type ClipboardWriter = Pick<HostClipboardService, "writeText">

export interface ClipboardReader {
  read(signal: AbortSignal): Promise<ClipboardPaste>
  /**
   * The host clipboard instance (created on first use), shared with copy (3.12.4) so only one native clipboard
   * service exists. Throws when OpenTUI cannot create it or after dispose().
   */
  hostClipboard(): HostClipboardService
  dispose(): Promise<void>
}

export interface ClipboardReaderOptions {
  /** The current per-image limit; a function is read on every paste so limits from a reconnect apply. */
  maxImageBytes: number | (() => number)
  /** From config/child-env.ts buildChildEnv(); also decides the fallback (WAYLAND_DISPLAY, DISPLAY). */
  env: ChildEnv
  clock: Clock
  backend?: ClipboardBackend
  /** Default bunSpawn. */
  spawn?: SpawnFn
  /** Default OpenTUI createHostClipboard; tests inject test/mocks/fake-clipboard.ts. */
  createHost?: (options: HostClipboardOptions) => HostClipboardService
}

export function createClipboardReader(opts: ClipboardReaderOptions): ClipboardReader {
  return new LayeredClipboardReader(opts)
}

type HostOutcome = { kind: "done"; paste: ClipboardPaste } | { kind: "fallback"; reason: string }

interface FallbackTool {
  readonly name: "wl-paste" | "xclip"
  readonly listArgv: readonly string[]
  readArgv(type: string): readonly string[]
  /** stderr of an empty clipboard. */
  readonly empty: RegExp
}

const WL_PASTE: FallbackTool = {
  name: "wl-paste",
  listArgv: ["wl-paste", "--list-types"],
  readArgv: (type) => ["wl-paste", "--no-newline", "--type", type],
  empty: /nothing is copied|no selection|no suitable type/i,
}

const XCLIP: FallbackTool = {
  name: "xclip",
  listArgv: ["xclip", "-selection", "clipboard", "-t", "TARGETS", "-o"],
  readArgv: (type) => ["xclip", "-selection", "clipboard", "-t", type, "-o"],
  empty: /not available|no owner/i,
}

const utf8 = new TextDecoder("utf-8")

class LayeredClipboardReader implements ClipboardReader {
  readonly #opts: ClipboardReaderOptions
  #host: HostClipboardService | null = null
  #hostCap = 0
  #disposed = false

  constructor(opts: ClipboardReaderOptions) {
    this.#opts = opts
  }

  hostClipboard(): HostClipboardService {
    if (this.#disposed) throw new Error("the clipboard reader was disposed")
    if (this.#host === null) {
      const cap = this.#cap()
      const create = this.#opts.createHost ?? createHostClipboard
      // maxImagePixels bounds the host's own image conversions to the client's pixel cap.
      this.#host = create({ timeoutMs: HOST_CLIPBOARD_TIMEOUT_MS, maxReadBytes: cap, maxImagePixels: MAX_IMAGE_PIXELS })
      this.#hostCap = cap
    }
    return this.#host
  }

  async read(signal: AbortSignal): Promise<ClipboardPaste> {
    if (this.#disposed) return { kind: "error", reason: "the clipboard reader was disposed" }
    if (signal.aborted) return { kind: "cancelled" }
    const backend = this.#opts.backend ?? "auto"
    let hostFailure: string | null = null
    if (backend !== "wl-paste") {
      const outcome = await this.#readHost(signal)
      if (outcome.kind === "done") return outcome.paste
      if (backend === "host") return { kind: "error", reason: `clipboard read failed (${outcome.reason})` }
      hostFailure = outcome.reason
    }
    return this.#readFallback(signal, hostFailure)
  }

  async dispose(): Promise<void> {
    this.#disposed = true
    const host = this.#host
    this.#host = null
    await host?.dispose()
  }

  #cap(): number {
    const value = typeof this.#opts.maxImageBytes === "function" ? this.#opts.maxImageBytes() : this.#opts.maxImageBytes
    return Math.max(1, Math.floor(value))
  }

  // ---- host clipboard ----

  async #readHost(signal: AbortSignal): Promise<HostOutcome> {
    let host: HostClipboardService
    try {
      host = this.hostClipboard()
    } catch (error) {
      return { kind: "fallback", reason: `host clipboard unavailable: ${reasonText(error)}` }
    }
    // The host was created with the limit of its first use; a lower current limit still applies.
    const cap = Math.min(this.#cap(), this.#hostCap)
    const image = await hostRead(host, CLIPBOARD_IMAGE_TYPES, signal)
    if (image.status === "read") {
      const { bytes, mimeType } = image.representation
      if (bytes.byteLength > cap) return done(limitError("image", cap))
      return done({ kind: "image", bytes: exactBytes(bytes), declaredMime: mimeType, via: "host" })
    }
    const imageOutcome = settleStatus(image, "image", cap)
    if (imageOutcome !== null) return imageOutcome

    const files = await hostRead(host, ["text/uri-list"], signal)
    if (files.status === "read") {
      const paste = filesPaste(files.representation.bytes)
      if (paste !== null) return done(paste)
    } else {
      const filesOutcome = settleStatus(files, "content", cap)
      if (filesOutcome !== null) return filesOutcome
    }

    const text = await hostRead(host, ["text/plain"], signal)
    if (text.status === "read") return done(textPaste(text.representation.bytes))
    return settleStatus(text, "text", cap) ?? done({ kind: "empty" })
  }

  // ---- wl-paste / xclip ----

  async #readFallback(signal: AbortSignal, hostFailure: string | null): Promise<ClipboardPaste> {
    const tool = fallbackTool(this.#opts.env)
    if (tool === null) {
      return { kind: "error", reason: unavailable(hostFailure, "no Wayland or X11 session for a clipboard tool") }
    }
    const listing = await this.#run(tool, tool.listArgv, LIST_TYPES_TIMEOUT_MS, MAX_TYPE_LIST_BYTES, signal)
    if (listing.kind === "paste") return listing.paste
    if (listing.kind === "error") return { kind: "error", reason: unavailable(hostFailure, listing.reason) }
    const choice = pickClipboardType(parseTypeList(listing.stdout))
    if (choice === null) {
      if (listing.stdout.byteLength === 0) return { kind: "empty" }
      return { kind: "error", reason: "the clipboard holds no image, file list or text" }
    }
    const cap = this.#cap()
    const content = await this.#run(tool, tool.readArgv(choice.type), PASTE_TIMEOUT_MS, cap, signal)
    if (content.kind === "paste") return content.paste
    if (content.kind === "error") return { kind: "error", reason: content.reason }
    return contentPaste(choice, content, tool.name, cap)
  }

  async #run(
    tool: FallbackTool,
    argv: readonly string[],
    timeoutMs: number,
    maxStdoutBytes: number,
    signal: AbortSignal,
  ): Promise<ToolRun> {
    let result: CappedRunResult
    try {
      result = await runCapped(this.#opts.spawn ?? bunSpawn, argv, {
        env: this.#opts.env,
        clock: this.#opts.clock,
        timeoutMs,
        maxStdoutBytes,
        signal,
      })
    } catch (error) {
      const reason = isMissingExecutable(error)
        ? `${tool.name} is not installed`
        : `${tool.name} could not start (${reasonText(error)})`
      return { kind: "error", reason }
    }
    if (result.aborted) return { kind: "paste", paste: { kind: "cancelled" } }
    if (result.timedOut) return { kind: "error", reason: `${tool.name} timed out after ${timeoutMs / 1000} s` }
    if (result.overflow) return { kind: "ok", stdout: result.stdout, overflow: true }
    if (result.status !== 0) {
      if (tool.empty.test(result.stderr)) return { kind: "paste", paste: { kind: "empty" } }
      return { kind: "error", reason: `${tool.name} failed (exit ${result.status}${stderrSuffix(result.stderr)})` }
    }
    return { kind: "ok", stdout: result.stdout, overflow: false }
  }
}

type ToolRun =
  | { kind: "ok"; stdout: Uint8Array; overflow: boolean }
  | { kind: "paste"; paste: ClipboardPaste }
  | { kind: "error"; reason: string }

function done(paste: ClipboardPaste): HostOutcome {
  return { kind: "done", paste }
}

/** The paste for what wl-paste or xclip printed for the chosen type. */
function contentPaste(
  choice: ClipboardTypeChoice,
  content: { stdout: Uint8Array; overflow: boolean },
  via: ClipboardVia,
  cap: number,
): ClipboardPaste {
  if (content.overflow) return limitError(choice.kind === "image" ? "image" : choice.kind === "files" ? "content" : "text", cap)
  if (choice.kind === "files") return filesPaste(content.stdout) ?? textPaste(content.stdout)
  if (choice.kind === "text") return textPaste(content.stdout)
  if (content.stdout.byteLength === 0) return { kind: "empty" }
  return { kind: "image", bytes: content.stdout, declaredMime: choice.type, via }
}

/** Non-"read" statuses: null continues with the next type (empty, unsupported). */
function settleStatus(result: ClipboardReadResult, what: "image" | "content" | "text", cap: number): HostOutcome | null {
  switch (result.status) {
    case "limit-exceeded":
      return done(limitError(what, cap))
    case "cancelled":
      return done({ kind: "cancelled" })
    case "failed":
      return { kind: "fallback", reason: reasonText(result.error) }
    case "timed-out":
      return { kind: "fallback", reason: `timed out after ${HOST_CLIPBOARD_TIMEOUT_MS / 1000} s` }
    default:
      return null
  }
}

async function hostRead(
  host: HostClipboardService,
  preferredTypes: readonly [string, ...string[]],
  signal: AbortSignal,
): Promise<ClipboardReadResult> {
  try {
    return await host.read({ preferredTypes, selection: "clipboard", signal })
  } catch (error) {
    return { status: "failed", error: error instanceof Error ? error : new Error(String(error)) }
  }
}

/** The bytes as an exact-size array, never a view into a larger buffer (whose spare bytes `.buffer` would expose). */
function exactBytes(bytes: Uint8Array): Uint8Array {
  return bytes.byteOffset === 0 && bytes.buffer.byteLength === bytes.byteLength ? bytes : bytes.slice()
}

function limitError(what: "image" | "content" | "text", cap: number): ClipboardPaste {
  return { kind: "error", reason: `clipboard ${what} larger than ${formatBytes(cap)}` }
}

function filesPaste(bytes: Uint8Array): ClipboardPaste | null {
  const paths = parseUriList(utf8.decode(bytes))
  if (paths.length === 0) return null
  return { kind: "files", paths, text: sanitizeText(paths.join("\n"), { multiline: true }) }
}

function textPaste(bytes: Uint8Array): ClipboardPaste {
  const text = sanitizeText(utf8.decode(bytes), { multiline: true })
  return text.trim().length === 0 ? { kind: "empty" } : { kind: "text", text }
}

function fallbackTool(env: ChildEnv): FallbackTool | null {
  if ((env.WAYLAND_DISPLAY ?? "") !== "") return WL_PASTE
  if ((env.DISPLAY ?? "") !== "") return XCLIP
  return null
}

function unavailable(hostFailure: string | null, why: string): string {
  const host = hostFailure === null ? "" : `host clipboard failed (${hostFailure}); `
  return `clipboard unavailable: ${host}${why}; use /img <path> or /shot`
}

/** Offered types, one per line; anything that is not a plain token (spaces, controls, over 255 bytes) is dropped. */
export function parseTypeList(stdout: Uint8Array): string[] {
  return utf8
    .decode(stdout)
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => /^[\x21-\x7e]{1,255}$/.test(line))
}

export type ClipboardTypeChoice = { kind: "image" | "files" | "text"; type: string }

/** The first offered of the four image types (in preference order), then text/uri-list, then text/plain*. */
export function pickClipboardType(offered: readonly string[]): ClipboardTypeChoice | null {
  const lower = offered.map((type) => type.toLowerCase())
  const exact = (wanted: string): string | undefined => offered[lower.indexOf(wanted)]
  for (const mime of CLIPBOARD_IMAGE_TYPES) {
    const type = exact(mime)
    if (type !== undefined) return { kind: "image", type }
  }
  const uriList = exact("text/uri-list")
  if (uriList !== undefined) return { kind: "files", type: uriList }
  const text =
    exact("text/plain;charset=utf-8") ?? offered[lower.findIndex((type) => type.startsWith("text/plain"))] ?? exact("utf8_string")
  return text === undefined ? null : { kind: "text", type: text }
}

function reasonText(error: unknown): string {
  const text = sanitizeLine(error instanceof Error ? error.message : String(error)).trim()
  return text.length > MAX_REASON_CHARS ? `${text.slice(0, MAX_REASON_CHARS - 3)}...` : text || "unknown error"
}

function stderrSuffix(stderr: string): string {
  const line = sanitizeLine(stderr.split("\n").find((l) => l.trim().length > 0) ?? "").trim()
  if (line.length === 0) return ""
  return `: ${line.length > MAX_REASON_CHARS ? `${line.slice(0, MAX_REASON_CHARS - 3)}...` : line}`
}

// ---------------------------------------------------------------------------------------------------------
// Capped child runs (also used by screenshot.ts)
// ---------------------------------------------------------------------------------------------------------

export interface CappedRunOptions {
  env: ChildEnv
  clock: Clock
  /** Then the child is killed (SIGKILL) and timedOut is set. */
  timeoutMs: number
  /** Stdout bytes kept; one more byte sets overflow and kills the child. */
  maxStdoutBytes: number
  /**
   * Bytes beyond maxStdoutBytes that are counted, not kept, before the child is killed (grim's JPEG retry reports
   * the real size). Default 0.
   */
  countOverflowBytes?: number
  /** Kept stderr bytes; the rest is drained and dropped. Default 4096. */
  maxStderrBytes?: number
  /** Aborting kills the child and sets aborted. */
  signal?: AbortSignal
}

export interface CappedRunResult {
  /** The exit status: the code, or 128 + n after signal n (Bun semantics). */
  status: number
  /** At most maxStdoutBytes bytes. */
  stdout: Uint8Array
  /** Every stdout byte seen, the counted overflow included; a lower bound when overflowKilled. */
  stdoutBytes: number
  overflow: boolean
  /** The child was killed after countOverflowBytes more bytes. */
  overflowKilled: boolean
  stderr: string
  timedOut: boolean
  aborted: boolean
}

/** ENOENT or EACCES from spawn(): the executable is missing or not runnable. */
export function isMissingExecutable(error: unknown): boolean {
  const code = typeof error === "object" && error !== null && "code" in error ? String(error.code) : ""
  return code === "ENOENT" || code === "EACCES"
}

/**
 * Runs argv (stdin ignored) with a timeout and a stdout cap, without a shell. Throws only when spawn() throws
 * (missing executable); every other outcome is in the result.
 */
export async function runCapped(spawn: SpawnFn, argv: readonly string[], opts: CappedRunOptions): Promise<CappedRunResult> {
  const empty = new Uint8Array(0)
  if (opts.signal?.aborted) {
    return {
      status: -1,
      stdout: empty,
      stdoutBytes: 0,
      overflow: false,
      overflowKilled: false,
      stderr: "",
      timedOut: false,
      aborted: true,
    }
  }
  const child = spawn(argv, { env: opts.env, stdin: "ignore", stdout: "pipe", stderr: "pipe" })
  let timedOut = false
  let aborted = false
  const stop = new AbortController()
  const kill = (): void => {
    try {
      child.kill("SIGKILL")
    } catch {
      // already gone
    }
    stop.abort()
  }
  const timer = opts.clock.setTimeout(() => {
    timedOut = true
    kill()
  }, opts.timeoutMs)
  const onAbort = (): void => {
    aborted = true
    kill()
  }
  opts.signal?.addEventListener("abort", onAbort, { once: true })
  try {
    const [out, err] = await Promise.all([
      collect(child.stdout, opts.maxStdoutBytes, opts.countOverflowBytes ?? 0, kill, stop.signal),
      collect(child.stderr, opts.maxStderrBytes ?? 4096, Number.POSITIVE_INFINITY, () => undefined, stop.signal),
    ])
    const status = await child.exited
    return {
      status,
      stdout: out.bytes,
      stdoutBytes: out.seen,
      overflow: out.seen > opts.maxStdoutBytes,
      overflowKilled: out.limitHit,
      stderr: utf8.decode(err.bytes),
      timedOut,
      aborted,
    }
  } finally {
    opts.clock.clearTimeout(timer)
    opts.signal?.removeEventListener("abort", onAbort)
  }
}

interface Collected {
  /** The first `keep` bytes. */
  bytes: Uint8Array
  /** Every byte read. */
  seen: number
  /** More than keep + countBeyond bytes arrived and onLimit ran. */
  limitHit: boolean
}

/**
 * Reads a pipe, keeping the first `keep` bytes and counting the rest; past keep + countBeyond bytes it calls
 * onLimit and stops. `stop` cancels the read: a killed child's pipe can stay open in a grandchild.
 */
async function collect(
  stream: ReadableStream<Uint8Array> | undefined,
  keep: number,
  countBeyond: number,
  onLimit: () => void,
  stop: AbortSignal,
): Promise<Collected> {
  if (stream === undefined) return { bytes: new Uint8Array(0), seen: 0, limitHit: false }
  const reader = stream.getReader()
  const cancel = (): void => {
    reader.cancel().catch(() => undefined)
  }
  stop.addEventListener("abort", cancel, { once: true })
  const chunks: Uint8Array[] = []
  let kept = 0
  let seen = 0
  let limitHit = false
  try {
    for (;;) {
      const { done: finished, value } = await reader.read()
      if (finished) break
      seen += value.byteLength
      const room = keep - kept
      if (room > 0) {
        // A copy: the kept bytes must not depend on whether the runtime reuses its read buffers.
        const part = value.slice(0, Math.min(room, value.byteLength))
        chunks.push(part)
        kept += part.byteLength
      }
      if (seen > keep + countBeyond) {
        limitHit = true
        onLimit()
        break
      }
    }
  } catch {
    // the stream errored or was cancelled: keep what arrived
  } finally {
    stop.removeEventListener("abort", cancel)
    reader.cancel().catch(() => undefined)
  }
  return { bytes: joinChunks(chunks, kept), seen, limitHit }
}

function joinChunks(chunks: readonly Uint8Array[], total: number): Uint8Array {
  const out = new Uint8Array(total)
  let offset = 0
  for (const chunk of chunks) {
    out.set(chunk, offset)
    offset += chunk.byteLength
  }
  return out
}
