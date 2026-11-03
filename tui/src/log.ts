/**
 * The opt-in debug log (design 3.11.6): JSON lines of event types, sizes, durations and status codes, written to
 * a 0600 file only with --debug-log or LUNARWING_TUI_DEBUG_LOG (config.ts resolves the path; the default is
 * <state dir>/debug.log). Never message text, transcripts, audio, image bytes, headers or tokens.
 *
 * The API enforces "metadata only": a field is a number, a boolean, null, or a short identifier-like tag
 * ("turn.delta", "rate_limited", "/api/chat/send", "lunarvoice"). A string with spaces or other free-text
 * characters, or with a run of 16 or more characters that could be a token, is written as "[redacted]"; any
 * other value (an object, a Secret) is dropped. The app never uses console.log (OpenTUI owns the terminal), and
 * a write failure silently turns the log off rather than disturbing the UI.
 */
import { constants } from "node:fs"
import { open, type FileHandle } from "node:fs/promises"
import { ConfigError, displayValue } from "./config/flags.ts"

export type DebugValue = number | boolean | null | string
export type DebugFields = Readonly<Record<string, DebugValue | undefined>>

export interface DebugLog {
  readonly enabled: boolean
  readonly path: string | null
  /** Queues one line; never throws and never blocks. */
  event(type: string, fields?: DebugFields): void
  /** Writes what is queued and closes the file; later events are ignored. */
  close(): Promise<void>
}

export const REDACTED_TAG = "[redacted]"
/** The log stops (with a final "log.limit" line) after this many bytes per session. */
export const DEBUG_LOG_MAX_BYTES = 8 * 1024 * 1024
const MAX_QUEUED_LINES = 2000
const MAX_FIELDS = 24

const EVENT_TYPE = /^[a-z][a-z0-9_.:-]{0,63}$/
const FIELD_NAME = /^[a-z][a-zA-Z0-9_]{0,31}$/
const TAG = /^[A-Za-z0-9_.:/@+-]{0,64}$/
const TAG_SEPARATORS = /[_.:/@+-]/
const SECRET_RUN = 16

/** A tag as written: unchanged when it is identifier-like and holds no token-length run, else "[redacted]". */
export function safeTag(value: string): string {
  if (!TAG.test(value)) return REDACTED_TAG
  return value.split(TAG_SEPARATORS).some((part) => part.length >= SECRET_RUN) ? REDACTED_TAG : value
}

function safeValue(value: unknown): DebugValue | undefined {
  if (value === null || typeof value === "boolean") return value
  if (typeof value === "number") return Number.isFinite(value) ? value : null
  if (typeof value === "string") return safeTag(value)
  return undefined
}

/** Builds one log line (exported for tests): {"t":<ms>,"type":"<type>",...fields}. */
export function formatDebugLine(at: number, type: string, fields: DebugFields = {}): string {
  const line: Record<string, DebugValue> = { t: at, type: EVENT_TYPE.test(type) ? type : "invalid" }
  let count = 0
  for (const [name, value] of Object.entries(fields)) {
    if (count >= MAX_FIELDS) break
    if (!FIELD_NAME.test(name) || name === "t" || name === "type") continue
    const safe = safeValue(value)
    if (safe === undefined) continue
    line[name] = safe
    count += 1
  }
  return `${JSON.stringify(line)}\n`
}

export const NULL_DEBUG_LOG: DebugLog = Object.freeze({
  enabled: false,
  path: null,
  event: () => undefined,
  close: () => Promise.resolve(),
})

export interface DebugLogOptions {
  now?: () => number
  uid?: number
  maxBytes?: number
}

class FileDebugLog implements DebugLog {
  readonly enabled = true
  #queue: string[] = []
  #dropped = 0
  #bytes = 0
  #flushing: Promise<void> | null = null
  #open = true
  #closed = false
  #limited = false

  constructor(
    readonly path: string,
    private readonly handle: FileHandle,
    private readonly now: () => number,
    private readonly maxBytes: number,
  ) {}

  event(type: string, fields?: DebugFields): void {
    if (!this.#open || this.#closed || this.#limited) return
    if (this.#queue.length >= MAX_QUEUED_LINES) {
      this.#dropped += 1
      return
    }
    let line = formatDebugLine(this.now(), type, fields)
    if (this.#bytes + line.length > this.maxBytes) {
      this.#limited = true
      line = formatDebugLine(this.now(), "log.limit", { maxBytes: this.maxBytes })
    }
    this.#bytes += line.length
    this.#queue.push(line)
    this.#schedule()
  }

  #schedule(): void {
    this.#flushing ??= this.#flush().finally(() => {
      this.#flushing = null
      if (this.#queue.length > 0 && this.#open) this.#schedule()
    })
  }

  async #flush(): Promise<void> {
    const lines = this.#queue.splice(0)
    if (this.#dropped > 0) {
      lines.push(formatDebugLine(this.now(), "log.dropped", { lines: this.#dropped }))
      this.#dropped = 0
    }
    try {
      await this.handle.write(lines.join(""))
    } catch {
      this.#open = false
      this.#queue = []
    }
  }

  async close(): Promise<void> {
    if (this.#closed) return
    this.#closed = true
    while (this.#flushing !== null) await this.#flushing
    if (this.#queue.length > 0 && this.#open) await this.#flush()
    this.#open = false
    await this.handle.close().catch(() => undefined)
  }
}

/**
 * Opens (appends to) the debug log at `path`, or returns NULL_DEBUG_LOG for null. The file is created 0600 with
 * O_NOFOLLOW (a link is refused); an existing file must be a regular file owned by the user and is tightened to
 * 0600. Throws ConfigError (exit 2) naming the path and the problem.
 */
export async function openDebugLog(path: string | null, options: DebugLogOptions = {}): Promise<DebugLog> {
  if (path === null) return NULL_DEBUG_LOG
  const label = `debug log ${displayValue(path)}`
  const flags = constants.O_WRONLY | constants.O_CREAT | constants.O_APPEND | constants.O_NOFOLLOW | constants.O_NONBLOCK
  const handle = await open(path, flags, 0o600).catch((error: unknown) => {
    const code = typeof error === "object" && error !== null && "code" in error ? String(error.code) : "I/O error"
    throw new ConfigError(`${label} cannot be opened (${code === "ELOOP" ? "it is a symbolic link" : code})`)
  })
  try {
    const stats = await handle.stat()
    const uid = options.uid ?? process.getuid?.()
    if (!stats.isFile()) throw new ConfigError(`${label} is not a regular file`)
    if (uid !== undefined && stats.uid !== uid) throw new ConfigError(`${label} is owned by uid ${stats.uid}, not by you`)
    if ((stats.mode & 0o077) !== 0) await handle.chmod(0o600)
  } catch (error) {
    await handle.close().catch(() => undefined)
    throw error
  }
  const log = new FileDebugLog(path, handle, options.now ?? Date.now, options.maxBytes ?? DEBUG_LOG_MAX_BYTES)
  log.event("log.open", { pid: process.pid })
  return log
}
