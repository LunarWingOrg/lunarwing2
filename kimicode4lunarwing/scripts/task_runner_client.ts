/**
 * task_runner_client.ts — WI#454 bridge-side client for the task runner.
 *
 * The bridge container's half of the two-container split: when
 * LUNARWING_TASK_RUNNER_SOCKET is set, the bridge delegates each task to the
 * task-runner daemon (running as the task uid — a different identity that
 * never sees the hub-handshake credential) over the shared unix socket,
 * instead of spawning the CLI locally through setpriv. In the two-container
 * shape the socket crosses the shared workspace volume; rehearsed in the
 * current single-container image the entrypoint starts the runner as the
 * task uid before the root bridge, so the credential-free boundary already
 * exists end to end.
 *
 * Wire contract mirrors task_runner.ts: one JSON object per line, `run`
 * streams {"op":"progress"} lines and exactly one terminal
 * {"op":"result"} before the runner half-closes the connection.
 */

import { connect } from "node:net"
import { basename } from "node:path"

import { isRecord, type TaskProgress, type TaskRequest, type TaskResult } from "./lunarwing_runtime.js"

/** Socket path the bridge delegates through; empty = local execution. */
export function bridgeRunnerSocketPath(): string {
  return process.env.LUNARWING_TASK_RUNNER_SOCKET || ""
}

interface RunnerLine {
  op?: string
  ok?: boolean
  progress?: unknown
  result?: unknown
  error?: unknown
  uid?: unknown
}

/**
 * Delegates one task to the runner. Resolves with the terminal result
 * (progress lines flow through onProgress as they arrive). Rejects on
 * connection failure, an unexpected line shape, or a runner error reply —
 * the bridge maps every rejection to an error task_result for the hub.
 */
export function delegateTaskToRunner(
  socketPath: string,
  request: TaskRequest,
  extraEnv: Record<string, string> | undefined,
  onProgress: (progress: TaskProgress) => void,
  abortSignal?: AbortSignal,
): Promise<TaskResult> {
  return new Promise<TaskResult>((resolve, reject) => {
    if (abortSignal?.aborted) {
      reject(new Error("task cancelled before the runner connection opened"))
      return
    }
    const sock = connect(socketPath)
    let buffer = Buffer.alloc(0)
    let settled = false
    const finish = (fn: () => void) => {
      if (settled) return
      settled = true
      clearTimeout(deadline)
      abortSignal?.removeEventListener("abort", onAbort)
      sock.destroy()
      fn()
    }
    // The task's own timeout bounds the run; this deadline only bounds the
    // CONNECTION (a wedged socket that never answers). The executor's
    // watchdogs own the run itself once it starts.
    const deadline = setTimeout(() => {
      finish(() => reject(new Error("task runner connection timed out")))
    }, runnerHandshakeTimeoutMs())
    const onAbort = () => {
      // The runner owns the spawned CLI once the run starts; cancelling the
      // local side drops the socket, and the runner's close handler lets the
      // executor's own escalation reap the child.
      sock.end()
      finish(() => reject(new Error("task cancelled while the runner was busy")))
    }
    abortSignal?.addEventListener("abort", onAbort, { once: true })
    sock.once("error", (err) => {
      finish(() => reject(err))
    })
    sock.on("data", (chunk: Buffer) => {
      buffer = Buffer.concat([buffer, chunk])
      let newline = buffer.indexOf(0x0a)
      while (newline >= 0) {
        const line = buffer.subarray(0, newline).toString("utf8").trim()
        buffer = buffer.subarray(newline + 1)
        if (line.length > 0) {
          const consumed = consumeLine(line, onProgress, (result) => {
            finish(() => resolve(result))
          }, (err) => {
            finish(() => reject(err))
          })
          if (consumed === "terminal") return
        }
        newline = buffer.indexOf(0x0a)
      }
    })
    sock.once("connect", () => {
      sock.write(
        JSON.stringify({ op: "run", request, extra_env: extraEnv ?? {} }) + "\n",
      )
    })
  })
}

type ConsumeOutcome = "progress" | "terminal" | "ignored"

function consumeLine(
  line: string,
  onProgress: (progress: TaskProgress) => void,
  onResult: (result: TaskResult) => void,
  onError: (error: Error) => void,
): ConsumeOutcome {
  let value: unknown
  try {
    value = JSON.parse(line)
  } catch {
    onError(new Error("task runner sent a non-JSON line"))
    return "terminal"
  }
  if (!isRecord(value)) {
    onError(new Error("task runner sent a non-object line"))
    return "terminal"
  }
  const runnerLine = value as RunnerLine
  if (runnerLine.op === "progress") {
    if (isRecord(runnerLine.progress)
      && typeof (runnerLine.progress as Record<string, unknown>).task_id === "string"
      && typeof (runnerLine.progress as Record<string, unknown>).delta === "string") {
      onProgress(runnerLine.progress as unknown as TaskProgress)
      return "progress"
    }
    onError(new Error("task runner sent a malformed progress line"))
    return "terminal"
  }
  if (runnerLine.op === "result") {
    const result = runnerLine.result
    if (!isRecord(result)
      || typeof (result as Record<string, unknown>).task_id !== "string"
      || typeof (result as Record<string, unknown>).status !== "string") {
      onError(new Error("task runner sent a malformed result line"))
      return "terminal"
    }
    onResult(result as unknown as TaskResult)
    return "terminal"
  }
  if (runnerLine.ok === false) {
    onError(new Error(`task runner refused the run: ${String(runnerLine.error ?? "unknown error")}`))
    return "terminal"
  }
  // Unknown informational lines are ignored (forward compatibility).
  return "ignored"
}

function runnerHandshakeTimeoutMs(): number {
  const raw = Number.parseInt(process.env.LUNARWING_TASK_RUNNER_TIMEOUT_MS ?? "", 10)
  if (!Number.isFinite(raw)) return 15_000
  return Math.min(600_000, Math.max(1_000, raw))
}

/** Cancels a run delegated to the runner (best-effort IPC op). */
export function cancelRunnerTask(socketPath: string, taskId: string): void {
  if (!socketPath) return
  try {
    const sock = connect(socketPath)
    sock.once("connect", () => {
      sock.write(JSON.stringify({ op: "cancel", task_id: taskId }) + "\n")
      // Fire-and-forget: the abort already dropped the delegation socket;
      // this op only accelerates the runner's own reaping.
      sock.end()
    })
    sock.once("error", () => sock.destroy())
  } catch {
    // The abort path must never throw into the bridge.
  }
}

/** Test hook: file name uniqueness guard for concurrent scratch sockets. */
export function runnerSocketLabel(socketPath: string): string {
  return basename(socketPath)
}
