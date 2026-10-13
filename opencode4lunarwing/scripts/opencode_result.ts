import { closeSync, constants as fsConstants, fstatSync, openSync, readSync, statSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"

interface PromptPartLike {
  type: string
  text?: string
}

export function extractAssistantText(parts: PromptPartLike[] | undefined): string {
  if (!parts) return ""

  return parts
    .filter((part) => part.type === "text" && typeof part.text === "string")
    .map((part) => part.text?.trim() ?? "")
    .filter(Boolean)
    .join("\n")
}

export function opencodeErrorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === "string") return error
  if (!error || typeof error !== "object") return String(error)

  const value = error as {
    name?: unknown
    message?: unknown
    data?: { message?: unknown }
  }
  if (typeof value.data?.message === "string") return value.data.message
  if (typeof value.message === "string") return value.message
  if (typeof value.name === "string") return value.name
  return "Unknown opencode error"
}

// opencode serve answers internal failures with a bare HTTP 500 wrapper
// ("Unexpected server error. Check server logs for details." — WI#320 F4 /
// WI#333 / WI#374) that names no cause. Verified at the pinned v1.18.25
// (OPENCODE_REF cb7d8b2f5e44876ef98b661dc10590c915af3a9f): the server's
// error middleware logs the real cause with Effect.logError("failed",
// { ref, error, cause }) through its structured file logger to
// $XDG_DATA_HOME|~/.local/share/opencode/log/opencode.log — stdout/stderr
// only carry it when OPENCODE_PRINT_LOGS=1, which serve is not started
// with. The failing response body carries the same unique `ref`
// ("err_<8 hex>"), so the enrichment below reads a bounded, per-task,
// error-level window of that structured log and correlates it by ref.

const GENERIC_SERVER_ERROR = /unexpected server error/i
const TAIL_BYTES = 8192
const TAIL_LINES = 25
const LINE_LIMIT = 400
// The Effect file logger batches writes on a 1 s window ("Do not set
// batchWindow to 0", upstream logging.ts), and the cause line can land just
// after the failing HTTP response — the retry budget must cover that flush
// window with margin. Only the generic-500 path ever reaches this loop.
const READ_RETRIES = 16
const READ_RETRY_DELAY_MS = 100
const REF_PATTERN = /^err_[0-9a-f]{1,64}$/
// The real formatter's level token: opencode's logger emits
// ["level", output.level] and effect's formatStructured uppercases it
// (logLevel.toUpperCase()), so a real 500 logs `level=ERROR` — never
// `level=Error` (verified at the pinned cb7d8b2f, run-92 FJ-1).
const ERROR_LEVEL_TOKEN = /(?:^|\s)level=ERROR(?:\s|$)/
const REDACTED = "[redacted]"
const MIN_SCRUB_SECRET_LENGTH = 8

export interface ServeLogOptions {
  ref?: string
  startOffset?: number
  secrets?: string[]
  // Test seam only: production callers never set it — the path is the
  // module-load constant so per-task env injection cannot retarget it.
  logPath?: string
}

export function resolveServeLogPath(
  env: Record<string, string | undefined>,
  home: string,
): string | undefined {
  const override = env.OPENCODE_SERVE_LOG
  if (override !== undefined) return override === "" ? undefined : override
  const dataHome =
    env.XDG_DATA_HOME && env.XDG_DATA_HOME.trim() !== ""
      ? env.XDG_DATA_HOME
      : join(home, ".local", "share")
  return join(dataHome, "opencode", "log", "opencode.log")
}

// Resolved once at module load: the executor injects per-task credential
// grants into process.env, so a grant named OPENCODE_SERVE_LOG must not be
// able to retarget or disable enrichment mid-task.
const SERVE_LOG_PATH = resolveServeLogPath(process.env, homedir())

export function opencodeErrorRef(error: unknown): string | undefined {
  if (!error || typeof error !== "object") return undefined
  const value = error as {
    cause?: { body?: { data?: { ref?: unknown } } }
    data?: { ref?: unknown }
    ref?: unknown
  }
  const candidates = [value.cause?.body?.data?.ref, value.data?.ref, value.ref]
  for (const candidate of candidates) {
    if (typeof candidate === "string" && REF_PATTERN.test(candidate)) return candidate
  }
  return undefined
}

interface LogWindow {
  text: string
  start: number
}

function readServeLogWindow(path: string, startOffset: number): LogWindow {
  // O_NOFOLLOW: a replaced log path must not lead the bridge to another file
  // (e.g. a symlink to /proc/<pid>/environ or /dev/zero planted by workspace
  // code running as the same uid). O_NONBLOCK: opening a planted FIFO must
  // not wedge the task — non-regular files are rejected via fstat before any
  // read. The whole window is bounded by TAIL_BYTES.
  const flags = fsConstants.O_RDONLY | (fsConstants.O_NOFOLLOW ?? 0) | (fsConstants.O_NONBLOCK ?? 0)
  let fd: number | undefined
  try {
    fd = openSync(path, flags)
    const stat = fstatSync(fd)
    if (!stat.isFile()) return { text: "", start: 0 }
    const start = Math.max(
      Math.min(Math.max(startOffset, 0), stat.size),
      stat.size - TAIL_BYTES,
    )
    const length = stat.size - start
    if (length <= 0) return { text: "", start: 0 }
    // Read one leading byte: at a line boundary it is the "\n" ending the
    // previous line, so the caller's drop-first-element rule keeps every
    // complete line; mid-line it makes the first element a partial line,
    // which the same rule discards.
    const readStart = start > 0 ? start - 1 : start
    const readLength = stat.size - readStart
    const buffer = Buffer.allocUnsafe(readLength)
    let read = 0
    while (read < readLength) {
      const count = readSync(fd, buffer, read, readLength - read, readStart + read)
      if (count <= 0) break
      read += count
    }
    return { text: buffer.subarray(0, read).toString("utf8"), start: readStart }
  } catch {
    return { text: "", start: 0 }
  } finally {
    if (fd !== undefined) closeSync(fd)
  }
}

export function serveLogStartSize(): number {
  if (SERVE_LOG_PATH === undefined) return 0
  try {
    return statSync(SERVE_LOG_PATH).size
  } catch {
    return 0
  }
}

// Cap by code points, never UTF-16 code units: splitting an astral character
// would emit a lone surrogate, and the hub's serde_json then rejects the
// whole task_result frame and the job runs to the worker timeout.
function capLine(line: string): string {
  if (Array.from(line).length <= LINE_LIMIT) return line
  return `${Array.from(line).slice(0, LINE_LIMIT).join("")}…`
}

function scrubSecrets(line: string, secrets: string[]): string {
  let scrubbed = line
  // Longest first: a secret containing a shorter secret must not leave its
  // suffix behind after the shorter one is replaced. There is no count cap —
  // a cap would silently let later values through.
  const eligible = secrets
    .filter((secret) => secret.length >= MIN_SCRUB_SECRET_LENGTH)
    .sort((a, b) => b.length - a.length)
  for (const secret of eligible) {
    // The structured logger JSON-quotes any value containing whitespace,
    // '"' or a backslash, so the escaped rendering must be scrubbed too.
    const forms = [secret]
    const escaped = JSON.stringify(secret).slice(1, -1)
    if (escaped !== secret) forms.push(escaped)
    for (const form of forms) {
      if (scrubbed.includes(form)) scrubbed = scrubbed.split(form).join(REDACTED)
    }
  }
  return scrubbed
}

function selectTailLines(window: LogWindow, ref: string | undefined, secrets: string[]): string[] {
  let lines = window.text.split("\n")
  if (window.start > 0) lines = lines.slice(1) // drop the partial first line
  lines = lines.filter((line) => ERROR_LEVEL_TOKEN.test(line))
  // With the response's ref, attach only that request's line — one serve
  // process serves every task and user of the tenant, so uncorrelated error
  // lines could leak another task's failure into this job's record. Match
  // the exact space-delimited token so a longer ref sharing the same prefix
  // cannot satisfy a substring test.
  if (ref !== undefined) {
    const token = `ref=${ref}`
    lines = lines.filter((line) => line.split(" ").includes(token))
  }
  return lines
    .map((line) => capLine(scrubSecrets(line, secrets)))
    .filter((line) => line.trim().length > 0)
    .slice(-TAIL_LINES)
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

export async function withServeLogDetail(
  message: string,
  options: ServeLogOptions = {},
): Promise<string> {
  if (!GENERIC_SERVER_ERROR.test(message)) return message
  const path = options.logPath ?? SERVE_LOG_PATH
  if (path === undefined) return message
  const ref = typeof options.ref === "string" ? options.ref : undefined
  // Fail-closed without a ref: every ERROR line since task start would
  // otherwise be eligible, and concurrent tasks of other users share this
  // serve log (their session.error/info.error messages carry no ref). The
  // pinned middleware always sets a ref on these 500s, so nothing real is
  // lost (run-92 FJ-5).
  if (ref === undefined) return message
  const secrets = options.secrets ?? []
  let lines: string[] = []
  for (let attempt = 0; attempt < READ_RETRIES; attempt++) {
    if (attempt > 0) await delay(READ_RETRY_DELAY_MS)
    lines = selectTailLines(readServeLogWindow(path, options.startOffset ?? 0), ref, secrets)
    if (lines.length > 0) break
    // Nothing matched yet — the Effect file logger batches writes, so the
    // cause line may land just after the failing HTTP response; look again.
  }
  if (lines.length === 0) return message
  return `${message}\nopencode serve log tail:\n${lines.join("\n")}`
}
