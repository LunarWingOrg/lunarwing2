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
// ("Unexpected server error. Check server logs for details.") that names no
// cause. The real error (e.g. the v1.18.25 SystemPrompt.environment TypeError
// behind WI#320 F4 / WI#374) only exists in the serve process's own log, which
// the entrypoint captures to OPENCODE_SERVE_LOG. Attach a bounded tail of that
// log so the job error reaching the hub already contains the root cause.
const GENERIC_SERVER_ERROR = /unexpected server error/i
const DEFAULT_SERVE_LOG = "/var/log/lunarwing/opencode-serve.log"
const TAIL_BYTES = 8192
const TAIL_LINES = 25
const LINE_LIMIT = 400

async function serveLogTail(path: string): Promise<string> {
  try {
    const file = Bun.file(path)
    const start = Math.max(0, file.size - TAIL_BYTES)
    let lines = (await file.slice(start).text()).split("\n")
    if (start > 0) lines = lines.slice(1) // drop the partial first line
    lines = lines.filter((line) => line.trim().length > 0).slice(-TAIL_LINES)
    return lines
      .map((line) => (line.length > LINE_LIMIT ? `${line.slice(0, LINE_LIMIT)}…` : line))
      .join("\n")
  } catch {
    return ""
  }
}

export async function withServeLogDetail(message: string, logPath?: string): Promise<string> {
  if (!GENERIC_SERVER_ERROR.test(message)) return message
  const tail = await serveLogTail(logPath || process.env.OPENCODE_SERVE_LOG || DEFAULT_SERVE_LOG)
  if (!tail) return message
  return `${message}\nopencode serve log tail:\n${tail}`
}
