/**
 * task_runner_client.ts — WI#454 bridge-side client for the task runner.
 *
 * The bridge container's half of the two-container split: when
 * LUNARWING_TASK_RUNNER_SOCKET is set, the bridge delegates each task to the
 * task-runner daemon (running as the task uid — a different identity that
 * never sees the hub-handshake credential) over a unix socket, instead of
 * spawning the CLI locally through setpriv. Rehearsed in the current
 * single-container image, the root launcher binds that socket in a root-only
 * directory and hands the runner only the listening descriptor (NL-7), so
 * the root bridge is the only party that can connect to it.
 *
 * Wire contract mirrors task_runner.ts: one JSON object per line, `run`
 * streams {"op":"accepted"}, {"op":"spawned"}, {"op":"progress"} lines and
 * exactly one terminal {"op":"result"} before the runner half-closes the
 * connection.
 */

import { connect } from "node:net"
import { lstatSync, readdirSync, readFileSync } from "node:fs"
import { basename, dirname } from "node:path"

import {
  isRecord,
  runnerLineCapBytes,
  DEFAULT_TIMEOUT_MS,
  type TaskProgress,
  type TaskRequest,
  type TaskResult,
} from "./lunarwing_runtime.js"

/**
 * Margin added to the task's own timeout when bounding a delegated run from
 * the bridge side (!1861 NB-13): enough for the runner to write its terminal
 * result line after the executor fires the task timeout, without the bridge
 * giving up on a run the runner is still finishing.
 */
export const TASK_RUN_MARGIN_MS = 60_000

/**
 * NL-3 (!1873 run 200): after the bridge cancels a run (hub cancel, hub
 * disconnect, run deadline) it waits this long past the executor's kill
 * grace for the runner's terminal result before reaping the CLI itself.
 */
export const CANCEL_DRAIN_MARGIN_MS = 5_000

/** How long the bridge waits for a SIGKILLed CLI process group to vanish. */
export const REAP_TIMEOUT_MS = 5_000

/** Socket path the bridge delegates through; empty = local execution. */
export function bridgeRunnerSocketPath(): string {
  return process.env.LUNARWING_TASK_RUNNER_SOCKET || ""
}

/**
 * Checks the runner socket before the bridge connects to it.
 *
 * NL-7 (!1873 run 200): a ROOT bridge (the image) only accepts a root-owned
 * socket in a root-owned directory that group and others cannot write — the
 * shape the launcher's task_runner_bind.py creates. The task uid can then
 * neither stand in for the runner (the old task-owned directory let any
 * task replace the socket, with no race needed) nor swap the path between
 * this check and the connect (the NC-10 TOCTOU). An unprivileged bridge
 * (host-side dev runs, tests) keeps the NB-16 shape: the path must be a
 * socket, owned by the task identity when KIMI_TASK_UID/KIMI_TASK_GID are
 * pinned.
 */
export function verifyRunnerSocket(socketPath: string): void {
  const st = lstatSync(socketPath)
  // S_IFSOCK = 0140000
  if ((st.mode & 0o170000) !== 0o140000) {
    throw new Error(`task runner path ${socketPath} is not a socket (refused, NB-16)`)
  }
  if (process.geteuid?.() === 0) {
    if (st.uid !== 0) {
      throw new Error(`task runner socket owned by uid ${st.uid}, expected root (refused, NL-7)`)
    }
    const dir = lstatSync(dirname(socketPath))
    if (!dir.isDirectory() || dir.uid !== 0 || (dir.mode & 0o022) !== 0) {
      throw new Error(
        `task runner socket directory ${dirname(socketPath)} must be a root-owned directory that group and others cannot write (refused, NL-7)`,
      )
    }
    return
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
  task_id?: unknown
  pgid?: unknown
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
 *
 * NL-3 (!1873 run 200): the bridge frees the task's workspace lock the
 * moment this promise settles, so it settles only once the CLI is gone:
 * - a cancel (abort signal) or an expired run deadline sends {"op":"cancel"}
 *   on THIS connection and waits, bounded, for the run's own terminal
 *   result, which the runner writes after the executor's SIGTERM→SIGKILL
 *   escalation has stopped the CLI (local execution settles at the same
 *   point, and the hub now sees `cancelled` in both modes);
 * - a run that ends without a terminal result (the runner died or dropped
 *   the connection, the drain timed out, a protocol error) first reaps the
 *   CLI's process group, reported by the runner's {"op":"spawned"} frame.
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
    // NL-8: one cap per line, in both directions, following the hub's
    // WS_MAX_PAYLOAD_BYTES (the runner serializes its lines within it).
    const lineCap = runnerLineCapBytes()
    const sock = connect(socketPath)
    let buffer = Buffer.alloc(0)
    let settled = false
    let connected = false
    let stopping: string | null = null
    let spawnedPgid: number | undefined
    let deadline: ReturnType<typeof setTimeout> | undefined
    const clearDeadline = () => {
      if (deadline === undefined) return
      clearTimeout(deadline)
      deadline = undefined
    }
    const settle = () => {
      settled = true
      clearDeadline()
      abortSignal?.removeEventListener("abort", onAbort)
      sock.destroy()
    }
    const finishWithResult = (result: TaskResult) => {
      if (settled) return
      settle()
      resolve(result)
    }
    // Every settlement WITHOUT a terminal result goes through here: the
    // detached CLI may still be running, so its process group is reaped
    // before the rejection lets the bridge free the workspace lock.
    const finishWithError = (error: Error) => {
      if (settled) return
      settle()
      const pgid = spawnedPgid
      if (pgid === undefined) {
        reject(error)
        return
      }
      void reapTaskProcessGroup(pgid).then(
        (outcome) => {
          if (outcome === "refused" || outcome === "timeout" || outcome === "unsupported") {
            console.warn(`[runner-client] task ${request.task_id}: CLI process group ${pgid} not reaped (${outcome})`)
          }
          reject(error)
        },
        () => reject(error),
      )
    }
    const drainMs = () => killGraceMs() + CANCEL_DRAIN_MARGIN_MS
    // NL-3: stop the run through the runner and wait for its terminal result.
    const stopRun = (reason: string) => {
      if (settled || stopping !== null) return
      stopping = reason
      clearDeadline()
      if (!connected) {
        // Nothing was sent yet, so nothing runs.
        finishWithError(new Error(reason))
        return
      }
      try {
        sock.write(JSON.stringify({ op: "cancel", task_id: request.task_id }) + "\n")
      } catch {
        // A dead connection surfaces through close/error below.
      }
      deadline = setTimeout(() => {
        finishWithError(new Error(`${reason} (no terminal result from the runner within ${drainMs()}ms)`))
      }, drainMs())
    }
    // The connection deadline bounds only the HANDSHAKE (a wedged socket
    // that never answers). It is cleared once the runner accepts the run —
    // review !1861 run 195 NB-13: a 15 s default that bounds the whole run
    // failed every real task, because kimi tasks routinely run for minutes.
    // After the runner accepts, the run is bounded by the task's own
    // timeout plus a margin.
    deadline = setTimeout(() => {
      finishWithError(new Error("task runner connection timed out"))
    }, runnerHandshakeTimeoutMs())
    const runDeadlineMs = () => {
      const taskTimeout = typeof request.timeout_ms === "number" && Number.isSafeInteger(request.timeout_ms)
        ? request.timeout_ms
        : DEFAULT_TIMEOUT_MS
      return taskTimeout + TASK_RUN_MARGIN_MS
    }
    const onAccepted = () => {
      if (stopping !== null) return
      clearDeadline()
      deadline = setTimeout(() => {
        stopRun(`task runner run exceeded ${runDeadlineMs()}ms (task timeout + margin)`)
      }, runDeadlineMs())
    }
    const onAbort = () => stopRun("task cancelled while the runner was busy")
    abortSignal?.addEventListener("abort", onAbort, { once: true })
    sock.once("error", (err) => {
      finishWithError(err)
    })
    // NC-8: a runner that accepts and then dies (or is killed by any task
    // since it runs as the task uid) closes the socket cleanly — without a
    // terminal line, the client would wait the full run deadline (task
    // timeout + 60 s, potentially hours) before rejecting. Settle at once
    // (after the NL-3 reap) when the connection ends without a result; the
    // normal path has already settled by now (the result line resolves
    // before end).
    const onClosedEarly = () => {
      finishWithError(new Error(stopping ?? "runner connection closed without a result"))
    }
    sock.once("close", onClosedEarly)
    sock.once("end", onClosedEarly)
    sock.on("data", (chunk: Buffer) => {
      if (settled) return
      buffer = Buffer.concat([buffer, chunk])
      // NB-15: the runner is the untrusted side of this socket. Each line is
      // capped (NL-2: per line, not per receive buffer, so a valid result
      // queued behind progress lines is not refused), and so is an
      // unterminated remainder, so a peer that never sends a newline cannot
      // grow the root bridge's memory without bound.
      let newline = buffer.indexOf(0x0a)
      while (newline >= 0) {
        if (newline > lineCap) {
          finishWithError(new Error(`task runner line exceeds ${lineCap} bytes`))
          return
        }
        const line = buffer.subarray(0, newline).toString("utf8").trim()
        buffer = buffer.subarray(newline + 1)
        if (line.length > 0) {
          const consumed = consumeLine(line, request.task_id, stopping !== null, {
            onProgress,
            onResult: finishWithResult,
            onError: finishWithError,
            onAccepted,
            onSpawned: (pgid) => {
              spawnedPgid = pgid
            },
          })
          if (consumed === "terminal") return
        }
        newline = buffer.indexOf(0x0a)
      }
      if (buffer.length > lineCap) {
        finishWithError(new Error(`task runner line exceeds ${lineCap} bytes without a newline`))
      }
    })
    sock.once("connect", () => {
      connected = true
      sock.write(
        JSON.stringify({ op: "run", request, extra_env: extraEnv ?? {} }) + "\n",
      )
    })
  })
}

type ConsumeOutcome = "progress" | "terminal" | "ignored"

interface LineHandlers {
  onProgress: (progress: TaskProgress) => void
  onResult: (result: TaskResult) => void
  onError: (error: Error) => void
  onAccepted: () => void
  onSpawned: (pgid: number) => void
}

function consumeLine(
  line: string,
  expectedTaskId: string,
  stopping: boolean,
  handlers: LineHandlers,
): ConsumeOutcome {
  let value: unknown
  try {
    value = JSON.parse(line)
  } catch {
    handlers.onError(new Error("task runner sent a non-JSON line"))
    return "terminal"
  }
  if (!isRecord(value)) {
    handlers.onError(new Error("task runner sent a non-object line"))
    return "terminal"
  }
  const runnerLine = value as RunnerLine
  if (runnerLine.op === "accepted") {
    // The runner took the run: the connection deadline's job is done, and
    // the run deadline (task timeout + margin) takes over (NB-13).
    handlers.onAccepted()
    return "ignored"
  }
  if (runnerLine.op === "spawned") {
    // NL-3: the CLI's process group, kept for the reap. A foreign task_id
    // or an implausible group id is ignored; the reap re-checks every
    // member's uid before it signals anything (reapTaskProcessGroup).
    if (runnerLine.task_id === expectedTaskId
      && typeof runnerLine.pgid === "number"
      && Number.isSafeInteger(runnerLine.pgid)
      && runnerLine.pgid >= 2) {
      handlers.onSpawned(runnerLine.pgid)
    }
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
      handlers.onProgress(runnerLine.progress as unknown as TaskProgress)
      return "progress"
    }
    handlers.onError(new Error("task runner sent a malformed progress line"))
    return "terminal"
  }
  if (runnerLine.op === "result") {
    const result = runnerLine.result
    if (!isRecord(result)
      || typeof (result as Record<string, unknown>).task_id !== "string"
      || typeof (result as Record<string, unknown>).status !== "string") {
      handlers.onError(new Error("task runner sent a malformed result line"))
      return "terminal"
    }
    // NB-15: frames are trusted only for the run this client delegated.
    // Progress and results carrying a foreign task_id are dropped (counted
    // as ignored), so a stand-in runner cannot forge another task's result.
    // Statuses are validated against the protocol's set. Output size is
    // bounded by the line cap here and re-bounded by the bridge's hub
    // envelope limit when it is forwarded (NL-2: the old fixed 1 MiB output
    // refusal is gone — it failed results the runner had already fitted).
    const res = result as Record<string, unknown>
    if (res.task_id !== expectedTaskId) {
      return "ignored"
    }
    // NC-6: use the protocol's status names (lunarwing_runtime.ts TaskResult.status),
    // not the colloquial "timeout" — the executor emits "timed_out" (WI#263).
    const allowedStatuses = new Set(["success", "error", "cancelled", "timed_out"])
    if (typeof res.status === "string" && !allowedStatuses.has(res.status)) {
      handlers.onError(new Error(`task runner sent an unknown result status ${JSON.stringify(res.status).slice(0, 64)}`))
      return "terminal"
    }
    const output = res.output
    if (output !== undefined && typeof output !== "string") {
      handlers.onError(new Error("task runner sent a non-string result output"))
      return "terminal"
    }
    handlers.onResult(result as unknown as TaskResult)
    return "terminal"
  }
  if (runnerLine.ok === false) {
    // While a cancel is draining, the runner may answer the cancel itself
    // with "no run in flight" (the run ended first); its result line, or
    // the close, settles the run.
    if (stopping) return "ignored"
    handlers.onError(new Error(`task runner refused the run: ${String(runnerLine.error ?? "unknown error")}`))
    return "terminal"
  }
  // Unknown informational lines (the cancel ack among them) are ignored
  // (forward compatibility).
  return "ignored"
}

function runnerHandshakeTimeoutMs(): number {
  const raw = Number.parseInt(process.env.LUNARWING_TASK_RUNNER_TIMEOUT_MS ?? "", 10)
  if (!Number.isFinite(raw)) return 15_000
  return Math.min(600_000, Math.max(1_000, raw))
}

/** The executor's kill grace (KIMI_KILL_GRACE_MS, same bounds and default). */
function killGraceMs(): number {
  const raw = Number.parseInt(process.env.KIMI_KILL_GRACE_MS ?? "", 10)
  if (!Number.isFinite(raw)) return 7_000
  return Math.min(30_000, Math.max(100, raw))
}

export interface ProcessGroupMember {
  pid: number
  uid: number
}

/**
 * Live (non-zombie) members of process group `pgid`, read from /proc, with
 * each member's real uid. Returns null when /proc cannot be read.
 */
export function processGroupMembers(pgid: number, procRoot = "/proc"): ProcessGroupMember[] | null {
  let entries: string[]
  try {
    entries = readdirSync(procRoot)
  } catch {
    return null
  }
  const members: ProcessGroupMember[] = []
  for (const entry of entries) {
    if (!/^[0-9]+$/.test(entry)) continue
    let stat: string
    let status: string
    try {
      stat = readFileSync(`${procRoot}/${entry}/stat`, "utf8")
      status = readFileSync(`${procRoot}/${entry}/status`, "utf8")
    } catch {
      continue
    }
    // comm (field 2) may hold spaces and parentheses: parse after the last ")".
    const fields = stat.slice(stat.lastIndexOf(")") + 2).split(" ")
    const state = fields[0]
    if (Number(fields[2]) !== pgid) continue
    if (state === "Z" || state === "X" || state === "x") continue
    const uidMatch = /^Uid:\s+([0-9]+)/m.exec(status)
    members.push({ pid: Number(entry), uid: uidMatch ? Number(uidMatch[1]) : -1 })
  }
  return members
}

export type ReapOutcome = "gone" | "killed" | "refused" | "timeout" | "unsupported"

/**
 * NL-3 (!1873 run 200): SIGKILL a delegated run's leftover CLI process
 * group and wait, bounded, until no live member is left.
 *
 * The group id comes from the runner, the untrusted side of the socket, and
 * this runs in the root bridge, so the group is signalled only when every
 * live member belongs to the task uid (KIMI_TASK_UID; the bridge's own uid
 * when no identity is pinned, i.e. host runs). The CLI is a session leader
 * (spawned detached) under no_new_privs, so nothing more privileged can join
 * its group afterwards. Processes that left the group on purpose (setsid)
 * survive, as they do in local execution.
 */
export async function reapTaskProcessGroup(
  pgid: number,
  options: { timeoutMs?: number; allowedUid?: number; procRoot?: string } = {},
): Promise<ReapOutcome> {
  if (!Number.isSafeInteger(pgid) || pgid < 2 || pgid === process.pid) return "refused"
  const allowedUid = options.allowedUid ?? reapAllowedUid()
  const timeoutMs = options.timeoutMs ?? REAP_TIMEOUT_MS
  let members = processGroupMembers(pgid, options.procRoot)
  if (members === null) return "unsupported"
  if (members.length === 0) return "gone"
  const deadline = Date.now() + timeoutMs
  for (;;) {
    if (members.some((member) => member.uid !== allowedUid)) {
      console.warn(`[runner-client] refusing to signal process group ${pgid}: a member is not owned by uid ${allowedUid}`)
      return "refused"
    }
    try {
      process.kill(-pgid, "SIGKILL")
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code !== "ESRCH") {
        console.warn(`[runner-client] SIGKILL of process group ${pgid} failed:`, err)
      }
    }
    await new Promise((resolve) => setTimeout(resolve, 100))
    members = processGroupMembers(pgid, options.procRoot) ?? []
    if (members.length === 0) return "killed"
    if (Date.now() >= deadline) return "timeout"
  }
}

function reapAllowedUid(): number {
  const raw = process.env.KIMI_TASK_UID
  if (raw && /^[1-9][0-9]*$/.test(raw)) return Number(raw)
  return process.geteuid?.() ?? -1
}

/** Test hook: file name uniqueness guard for concurrent scratch sockets. */
export function runnerSocketLabel(socketPath: string): string {
  return basename(socketPath)
}
