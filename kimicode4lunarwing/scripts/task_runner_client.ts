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
import { lstatSync } from "node:fs"
import { basename } from "node:path"

import { isRecord, DEFAULT_TIMEOUT_MS, type TaskProgress, type TaskRequest, type TaskResult } from "./lunarwing_runtime.js"

/**
 * Margin added to the task's own timeout when bounding a delegated run from
 * the bridge side (!1861 NB-13): enough for the runner to write its terminal
 * result line after the executor fires the task timeout, without the bridge
 * giving up on a run the runner is still finishing.
 */
export const TASK_RUN_MARGIN_MS = 60_000

/** Socket path the bridge delegates through; empty = local execution. */
export function bridgeRunnerSocketPath(): string {
  return process.env.LUNARWING_TASK_RUNNER_SOCKET || ""
}

/**
 * NB-16 (!1861 run 195): the runner socket directory is handed to the task
 * uid 0700, so the bridge (root) would otherwise connect through whatever
 * sits at the path — a symlink to a live socket is followed. Verify the path
 * is a socket owned by the task identity before connecting; if the task-uid
 * env pins are absent (host-side dev runs), the liveness/type check still
 * applies but ownership cannot be asserted.
 */
export function verifyRunnerSocket(socketPath: string): void {
  const st = lstatSync(socketPath)
  // S_IFSOCK = 0140000
  if ((st.mode & 0o170000) !== 0o140000) {
    throw new Error(`task runner path ${socketPath} is not a socket (refused, NB-16)`)
  }
  const uid = process.env.KIMI_TASK_UID
  const gid = process.env.KIMI_TASK_GID
  if (uid) {
    if (st.uid !== Number(uid)) {
      throw new Error(`task runner socket owned by uid ${st.uid}, expected task uid ${uid} (refused, NB-16)`)
    }
    if (gid && st.gid !== Number(gid)) {
      throw new Error(`task runner socket owned by gid ${st.gid}, expected task gid ${gid} (refused, NB-16)`)
    }
  }
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
    let deadline: ReturnType<typeof setTimeout> | undefined
    const finish = (fn: () => void) => {
      if (settled) return
      settled = true
      if (deadline !== undefined) clearTimeout(deadline)
      abortSignal?.removeEventListener("abort", onAbort)
      sock.destroy()
      fn()
    }
    const clearConnectionDeadline = () => {
      if (deadline === undefined) return
      clearTimeout(deadline)
      deadline = undefined
    }
    // The connection deadline bounds only the HANDSHAKE (a wedged socket
    // that never answers). It is cleared once the runner accepts the run —
    // review !1861 run 195 NB-13: a 15 s default that bounds the whole run
    // failed every real task, because kimi tasks routinely run for minutes.
    // After the runner accepts, the run is bounded by the task's own
    // timeout plus a margin (the runner's close handler aborts the run on
    // disconnect, so a vanished bridge cannot leave an orphaned CLI editing
    // the workspace while the bridge reports an error task_result).
    deadline = setTimeout(() => {
      finish(() => reject(new Error("task runner connection timed out")))
    }, runnerHandshakeTimeoutMs())
    const runDeadlineMs = () => {
      const taskTimeout = typeof request.timeout_ms === "number" && Number.isSafeInteger(request.timeout_ms)
        ? request.timeout_ms
        : DEFAULT_TIMEOUT_MS
      return taskTimeout + TASK_RUN_MARGIN_MS
    }
    const onAccepted = () => {
      clearConnectionDeadline()
      deadline = setTimeout(() => {
        finish(() => reject(new Error(`task runner run exceeded ${runDeadlineMs()}ms (task timeout + margin)`)))
      }, runDeadlineMs())
    }
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
    const maxLineBytes = 1_048_576
    sock.on("data", (chunk: Buffer) => {
      buffer = Buffer.concat([buffer, chunk])
      // NB-15: the runner is the untrusted side of this socket. Cap the
      // receive buffer so a peer that never sends a newline cannot grow the
      // root bridge's memory without bound (the runner side already caps
      // request lines at 1 MiB; this is the mirror on the way back).
      if (buffer.length > maxLineBytes) {
        finish(() => reject(new Error(`task runner line exceeds ${maxLineBytes} bytes without a newline`)))
        return
      }
      let newline = buffer.indexOf(0x0a)
      while (newline >= 0) {
        const line = buffer.subarray(0, newline).toString("utf8").trim()
        buffer = buffer.subarray(newline + 1)
        if (line.length > 0) {
          const consumed = consumeLine(line, request.task_id, onProgress, (result) => {
            finish(() => resolve(result))
          }, (err) => {
            finish(() => reject(err))
          }, onAccepted)
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
  expectedTaskId: string,
  onProgress: (progress: TaskProgress) => void,
  onResult: (result: TaskResult) => void,
  onError: (error: Error) => void,
  onAccepted: () => void,
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
  if (runnerLine.op === "accepted") {
    // The runner took the run: the connection deadline's job is done, and
    // the run deadline (task timeout + margin) takes over (NB-13).
    onAccepted()
    return "ignored"
  }
  if (runnerLine.op === "progress") {
    if (isRecord(runnerLine.progress)
      && typeof (runnerLine.progress as Record<string, unknown>).task_id === "string"
      && typeof (runnerLine.progress as Record<string, unknown>).delta === "string") {
      // NB-15: progress frames for a foreign task_id are dropped, exactly
      // like results — the runner reports only the run it was handed.
      if ((runnerLine.progress as Record<string, unknown>).task_id !== expectedTaskId) {
        return "ignored"
      }
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
    // NB-15: the root bridge trusts the runner only for the run IT delegated.
    // A runner frame claiming another task's id is dropped or overwritten;
    // statuses are validated against the protocol's set; output is bounded
    // by the bridge's existing envelope limit (WS_MAX_PAYLOAD_BYTES, 1 MiB).
    const res = result as Record<string, unknown>
    // NB-15: frames are trusted only for the run this client delegated.
    // Progress and results carrying a foreign task_id are dropped (counted
    // as ignored), so a stand-in runner cannot forge another task's result.
    if (res.task_id !== expectedTaskId) {
      return "ignored"
    }
    const allowedStatuses = new Set(["success", "error", "timeout", "cancelled"])
    if (typeof res.status === "string" && !allowedStatuses.has(res.status)) {
      onError(new Error(`task runner sent an unknown result status ${JSON.stringify(res.status).slice(0, 64)}`))
      return "terminal"
    }
    const output = res.output
    if (output !== undefined && typeof output !== "string") {
      onError(new Error("task runner sent a non-string result output"))
      return "terminal"
    }
    if (typeof output === "string" && Buffer.byteLength(output, "utf8") > 1_048_576) {
      onError(new Error("task runner result output exceeds the 1 MiB envelope limit"))
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
