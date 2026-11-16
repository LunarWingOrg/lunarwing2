import { lstatSync } from "node:fs"
import { connect } from "node:net"

import {
  deadlineDelay,
  DEFAULT_TIMEOUT_MS,
  isRecord,
  MAX_FRAME_BYTES,
  MAX_OUTPUT_BYTES,
  MAX_PROGRESS_BYTES,
  type TaskProgress,
  type TaskRequest,
  type TaskResult,
} from "./lunarwing_runtime.js"

const RUNNER_LINE_CAP = MAX_FRAME_BYTES + 65_536
const RUN_MARGIN_MS = 60_000
const CANCEL_DRAIN_MS = 15_000

export function verifyRunnerSocket(path: string): void {
  const stat = lstatSync(path)
  if (!stat.isSocket()) throw new Error("task runner path is not a socket")
  const uid = Number(process.env.LUNARDEPTH_TASK_UID ?? 10_005)
  const gid = Number(process.env.LUNARDEPTH_TASK_GID ?? 10_005)
  if (stat.uid !== uid || stat.gid !== gid) throw new Error("task runner socket has the wrong ownership")
}

export function delegateTaskToRunner(
  path: string,
  request: TaskRequest,
  onProgress: (progress: TaskProgress) => void,
  signal: AbortSignal,
): Promise<TaskResult> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(new Error("task cancelled before runner dispatch"))
      return
    }
    const socket = connect(path)
    let buffer = Buffer.alloc(0)
    let accepted = false
    let stopping = false
    let settled = false
    let deadline: ReturnType<typeof setTimeout>
    const settle = (callback: () => void) => {
      if (settled) return
      settled = true
      clearTimeout(deadline)
      signal.removeEventListener("abort", cancel)
      socket.destroy()
      callback()
    }
    const fail = (error: Error) => settle(() => reject(error))
    const cancel = () => {
      if (settled || stopping) return
      stopping = true
      clearTimeout(deadline)
      if (accepted) socket.write(`${JSON.stringify({ op: "cancel", task_id: request.task_id })}\n`)
      deadline = setTimeout(() => fail(new Error("runner did not settle cancellation")), CANCEL_DRAIN_MS)
      if (!accepted) fail(new Error("task cancelled before runner acceptance"))
    }
    const armRunDeadline = () => {
      clearTimeout(deadline)
      const timeout = request.timeout_ms ?? DEFAULT_TIMEOUT_MS
      deadline = setTimeout(cancel, deadlineDelay(timeout, RUN_MARGIN_MS))
    }
    signal.addEventListener("abort", cancel, { once: true })
    deadline = setTimeout(() => fail(new Error("task runner handshake timed out")), 15_000)
    socket.once("error", fail)
    socket.once("close", () => {
      if (!settled) fail(new Error(stopping ? "runner closed during cancellation" : "runner closed without a result"))
    })
    socket.once("connect", () => {
      try {
        verifyRunnerSocket(path)
        socket.write(`${JSON.stringify({ op: "run", request })}\n`)
      } catch (error) {
        fail(error instanceof Error ? error : new Error(String(error)))
      }
    })
    socket.on("data", (chunk: Buffer) => {
      if (settled) return
      buffer = Buffer.concat([buffer, chunk])
      let newline = buffer.indexOf(10)
      while (newline >= 0 && !settled) {
        if (newline > RUNNER_LINE_CAP) {
          fail(new Error("task runner line exceeds its bound"))
          return
        }
        const line = buffer.subarray(0, newline).toString("utf8")
        buffer = buffer.subarray(newline + 1)
        consumeRunnerLine(line, request.task_id, stopping, {
          onAccepted: () => { accepted = true; armRunDeadline() },
          onProgress,
          onResult: result => settle(() => resolve(result)),
          onError: fail,
        })
        newline = buffer.indexOf(10)
      }
      if (buffer.length > RUNNER_LINE_CAP) fail(new Error("unterminated task runner line exceeds its bound"))
    })
  })
}

interface RunnerHandlers {
  onAccepted: () => void
  onProgress: (progress: TaskProgress) => void
  onResult: (result: TaskResult) => void
  onError: (error: Error) => void
}

function consumeRunnerLine(line: string, taskId: string, stopping: boolean, handlers: RunnerHandlers): void {
  try {
    const value: unknown = JSON.parse(line)
    if (!isRecord(value)) throw new Error("runner sent a non-object line")
    if (value.op === "accepted" && value.task_id === taskId) {
      handlers.onAccepted()
      return
    }
    if (value.op === "progress") {
      if (!isRecord(value.progress) || value.progress.task_id !== taskId ||
          typeof value.progress.delta !== "string" || Buffer.byteLength(value.progress.delta, "utf8") > MAX_PROGRESS_BYTES) {
        throw new Error("runner sent malformed progress")
      }
      handlers.onProgress({ task_id: taskId, delta: value.progress.delta, done: false })
      return
    }
    if (value.op === "result") {
      const result = parseRunnerResult(value.result, taskId)
      handlers.onResult(result)
      return
    }
    if (value.ok === false) {
      if (!stopping) handlers.onError(new Error(`task runner refused the run: ${String(value.error ?? "unknown error")}`))
      return
    }
    if (value.op === "cancelled" || value.op === "pong") return
    throw new Error("runner sent an unknown operation")
  } catch (error) {
    handlers.onError(error instanceof Error ? error : new Error(String(error)))
  }
}

function parseRunnerResult(value: unknown, taskId: string): TaskResult {
  if (!isRecord(value) || value.task_id !== taskId || typeof value.status !== "string" ||
      !["success", "error", "cancelled", "timed_out"].includes(value.status) ||
      typeof value.output !== "string" || Buffer.byteLength(value.output, "utf8") > MAX_OUTPUT_BYTES ||
      (value.error !== null && typeof value.error !== "string") ||
      typeof value.duration_ms !== "number" || !Number.isSafeInteger(value.duration_ms) || value.duration_ms < 0) {
    throw new Error("runner sent a malformed result")
  }
  return {
    task_id: taskId,
    status: value.status as TaskResult["status"],
    output: value.output,
    error: value.error as string | null,
    duration_ms: value.duration_ms,
  }
}
