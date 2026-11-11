import { createHash, randomUUID, timingSafeEqual } from "node:crypto"
import { lstatSync, readFileSync, renameSync, writeFileSync } from "node:fs"

export const SUBPROTOCOL = "lunarwing-agent-v1"
export const WORKER_VERSION = "lunardepth-worker-1.0.0"
export const MAX_FRAME_BYTES = 16 * 1024 * 1024
export const MAX_OUTPUT_BYTES = 15 * 1024 * 1024
export const MAX_PROGRESS_BYTES = 64 * 1024
export const MAX_HISTORY_MESSAGES = 12
export const MAX_HISTORY_MESSAGE_UNITS = 2_000
export const MAX_HISTORY_UNITS = 12_000
export const DEFAULT_TIMEOUT_MS = 3_600_000
export const MAX_TIMEOUT_MS = 2_147_483_647

export interface Envelope {
  id: string
  type: string
  timestamp: string
  payload: Record<string, unknown>
}

export interface ConversationMessage {
  role: "user" | "assistant"
  content: string
}

export interface TaskContext {
  project_dir?: string
  conversation_history?: ConversationMessage[]
}

export interface TaskRequest {
  task_id: string
  prompt: string
  context?: TaskContext
  timeout_ms?: number
}

export interface TaskProgress {
  task_id: string
  delta: string
  done: boolean
}

export type TaskStatus = "success" | "error" | "cancelled" | "timed_out"

export interface TaskResult {
  task_id: string
  status: TaskStatus
  output: string
  error: string | null
  duration_ms: number
}

export interface WsState {
  timestamp: string
  ready: boolean
  role: string
  listening: boolean
  bind_host: string
  port: number
  path: string
  connections: number
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value)
}

export function createEnvelope(type: string, payload: Record<string, unknown>): Envelope {
  return { id: randomUUID(), type, timestamp: new Date().toISOString(), payload }
}

export function parseEnvelope(text: string): Envelope {
  const value: unknown = JSON.parse(text)
  if (!isRecord(value) || typeof value.id !== "string" || value.id.length === 0 ||
      typeof value.type !== "string" || value.type.length === 0 ||
      typeof value.timestamp !== "string" || value.timestamp.length === 0 ||
      !isRecord(value.payload)) {
    throw new Error("message must be a LunarWing envelope")
  }
  return { id: value.id, type: value.type, timestamp: value.timestamp, payload: value.payload }
}

export function bearerTokenMatches(header: string | undefined, expected: string): boolean {
  const match = header === undefined ? null : /^Bearer ([^\s]+)$/.exec(header)
  const token = match?.[1]
  if (token === undefined || expected.length === 0) return false
  const supplied = createHash("sha256").update(token, "utf8").digest()
  const wanted = createHash("sha256").update(expected, "utf8").digest()
  return timingSafeEqual(supplied, wanted)
}

export function readCredentialFile(path: string): string {
  if (path.length === 0) throw new Error("credential file path is required")
  const stat = lstatSync(path)
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error("credential path must be a regular non-symlink file")
  if ((stat.mode & 0o777) !== 0o400) throw new Error("credential file mode must be 0400")
  const uid = process.geteuid?.()
  if (uid !== undefined && stat.uid !== uid) throw new Error("credential file must belong to the current role")
  const value = readFileSync(path, "utf8")
  if (value.length < 32 || value.length > 4096 || /[\x00-\x20\x7f]/.test(value)) {
    throw new Error("credential file has an invalid value")
  }
  return value
}

export function validateTaskRequest(payload: unknown): TaskRequest {
  if (!isRecord(payload)) throw new Error("task payload must be an object")
  const allowed = new Set(["task_id", "prompt", "context", "timeout_ms"])
  for (const key of Object.keys(payload)) {
    if (!allowed.has(key)) throw new Error(`unsupported task field: ${key}`)
  }
  if (typeof payload.task_id !== "string" || payload.task_id.length === 0 ||
      Buffer.byteLength(payload.task_id, "utf8") > 256 || /[\x00-\x1f\x7f]/.test(payload.task_id)) {
    throw new Error("task_id must be non-empty, control-free, and at most 256 UTF-8 bytes")
  }
  if (typeof payload.prompt !== "string" || payload.prompt.trim().length === 0 ||
      Buffer.byteLength(payload.prompt, "utf8") > MAX_FRAME_BYTES) {
    throw new Error("prompt must be non-empty and fit the 16 MiB frame limit")
  }
  const timeout = payload.timeout_ms ?? DEFAULT_TIMEOUT_MS
  if (typeof timeout !== "number" || !Number.isSafeInteger(timeout) || timeout < 1 || timeout > MAX_TIMEOUT_MS) {
    throw new Error(`timeout_ms must be an integer from 1 through ${MAX_TIMEOUT_MS}`)
  }
  const context = validateTaskContext(payload.context)
  return {
    task_id: payload.task_id,
    prompt: payload.prompt,
    timeout_ms: timeout,
    ...context === undefined ? {} : { context },
  }
}

function validateTaskContext(value: unknown): TaskContext | undefined {
  if (value === undefined) return undefined
  if (!isRecord(value)) throw new Error("context must be an object")
  const allowed = new Set(["project_dir", "conversation_history"])
  for (const key of Object.keys(value)) {
    if (!allowed.has(key)) throw new Error(`unsupported task context field: ${key}`)
  }
  if (value.project_dir !== undefined && typeof value.project_dir !== "string") {
    throw new Error("context.project_dir must be a string")
  }
  const history = validateHistory(value.conversation_history)
  return {
    ...typeof value.project_dir === "string" ? { project_dir: value.project_dir } : {},
    ...history === undefined ? {} : { conversation_history: history },
  }
}

function validateHistory(value: unknown): ConversationMessage[] | undefined {
  if (value === undefined) return undefined
  if (!Array.isArray(value) || value.length > MAX_HISTORY_MESSAGES) {
    throw new Error(`conversation_history must be an array of at most ${MAX_HISTORY_MESSAGES} messages`)
  }
  let total = 0
  const history = value.map((entry): ConversationMessage => {
    if (!isRecord(entry) || (entry.role !== "user" && entry.role !== "assistant") ||
        typeof entry.content !== "string" || Object.keys(entry).some(key => key !== "role" && key !== "content")) {
      throw new Error("conversation history entries require only role user/assistant and string content")
    }
    if (entry.content.length > MAX_HISTORY_MESSAGE_UNITS) {
      throw new Error(`conversation history content exceeds ${MAX_HISTORY_MESSAGE_UNITS} UTF-16 code units`)
    }
    total += entry.content.length
    return { role: entry.role, content: entry.content }
  })
  if (total > MAX_HISTORY_UNITS) throw new Error(`conversation history exceeds ${MAX_HISTORY_UNITS} UTF-16 code units`)
  return history
}

export function resultPayload(result: TaskResult): Record<string, unknown> {
  return {
    task_id: result.task_id,
    status: result.status,
    output: result.output,
    error: result.error,
    duration_ms: result.duration_ms,
  }
}

export function errorResult(taskId: string, error: unknown, status: TaskStatus = "error"): TaskResult {
  return {
    task_id: taskId,
    status,
    output: "",
    error: error instanceof Error ? error.message.slice(0, 4096) : String(error).slice(0, 4096),
    duration_ms: 0,
  }
}

export function serializeBounded(envelope: Envelope): string {
  const text = JSON.stringify(envelope)
  if (Buffer.byteLength(text, "utf8") > MAX_FRAME_BYTES) {
    throw new Error("outgoing envelope exceeds the 16 MiB frame limit")
  }
  return text
}

export function writeWsState(path: string, state: WsState): void {
  const temporary = `${path}.${process.pid}.tmp`
  writeFileSync(temporary, JSON.stringify(state), { mode: 0o600 })
  renameSync(temporary, path)
}
