/**
 * task_runner.ts — WI#454 task-runner daemon (the task-container component).
 *
 * The maintainer's no-root policy (run-184 follow-up, note #10954) splits each
 * worker lane into two non-root containers: a bridge container that holds the
 * hub-handshake credential, and a task container that runs all task code under
 * a different uid. They share the workspace volume and talk over a unix
 * socket. This module is the task-container half: a long-lived daemon,
 * started as the task uid (never root), that receives task requests over the
 * socket and runs them locally. Because the daemon IS the task uid, no
 * setpriv privilege drop is needed — the boundary is the uid the container
 * itself runs under, which is the shape the two-container split needs.
 *
 * IPC protocol (one JSON object per line, mirroring the tool-proxy relay):
 *   {"op": "run", "request": <TaskRequest>, "extra_env": {...}}
 *     → any number of {"op": "progress", "progress": <TaskProgress>}
 *     → exactly one terminal {"op": "result", "result": <TaskResult>}
 *     → {"op": "cancel", "task_id": "..."} (best effort; SIGTERM/SIGKILL
 *       escalation is owned by the executor once a run starts)
 *   {"op": "ping"} → {"ok": true, "op": "pong", "uid": <runner uid>}
 *   Every failure shape is {"ok": false, "error": "<reason>"}.
 *
 * Bounds: a request line larger than the hub's task_request budget can never
 * be valid, so the line is refused and the connection dropped.
 */

import { createServer, type Server, type Socket } from "node:net"
import { lstatSync, mkdirSync, rmSync, unlinkSync, writeFileSync } from "node:fs"
import { dirname } from "node:path"

import { executeTask } from "./kimi_task_executor.js"
import { resolveTaskWorkspace } from "./workspace_path.js"
import { isRecord, type TaskProgress, type TaskRequest, type TaskResult } from "./lunarwing_runtime.js"

// Keep in sync with the bridge's WS_MAX_PAYLOAD_BYTES default (1 MiB), which
// bounds a task_request envelope on the hub socket; a runner line that big
// can never be a valid request.
export const RUNNER_REQUEST_LINE_CAP_BYTES = 1_048_576

/** Socket path the daemon listens on. Configured by the entrypoint/launcher. */
export function taskRunnerSocketPath(): string {
  return process.env.LUNARWING_TASK_RUNNER_SOCKET || ""
}

/** Directory creation guard mirroring the tool-proxy relay's WI#400 LP-5
 * posture: a symlink at the directory path is refused, never followed. */
export function refuseSymlinkedRunnerDir(dir: string): void {
  let st
  try {
    st = lstatSync(dir)
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === "ENOENT") return
    throw err
  }
  if (st.isSymbolicLink()) throw new Error(`runner dir ${dir} is a symlink (refused, WI#400 LP-5)`)
  if (!st.isDirectory()) throw new Error(`runner dir ${dir} is not a directory (refused, WI#400 LP-5)`)
}

interface ActiveRun {
  taskId: string
  controller: AbortController
  conn: Socket
}

/**
 * The daemon. One runner per container; the bridge connects, sends `run`
 * requests, and reads progress/result lines back on the same connection.
 */
export class TaskRunner {
  private readonly server: Server
  private readonly runs = new Map<string, ActiveRun>()
  private socketPath: string | null = null
  private closed = false

  constructor(private readonly workspaceRoot: string = process.env.WORKSPACE_ROOT || "/workspace") {
    this.server = createServer((conn) => this.acceptConnection(conn))
  }

  /** Listen on the socket path. Returns the path, or null on failure (the
   * caller decides whether a runner-less worker is fatal). */
  async listen(socketPath: string): Promise<string | null> {
    if (this.closed) return null
    try {
      refuseSymlinkedRunnerDir(dirname(socketPath))
      mkdirSync(dirname(socketPath), { recursive: true, mode: 0o700 })
      refuseSymlinkedRunnerDir(dirname(socketPath))
      // A stale socket from a crashed runner would make listen() fail with
      // EADDRINUSE; remove it (the path is owned by this daemon's uid).
      rmSync(socketPath, { force: true })
    } catch (err) {
      console.error("[task-runner] socket dir setup failed:", err instanceof Error ? err.message : err)
      return null
    }
    const started = await new Promise<boolean>((resolve) => {
      this.server.once("error", (err) => {
        console.error("[task-runner] listen failed:", err.message)
        resolve(false)
      })
      this.server.listen(socketPath, () => resolve(true))
    })
    if (!started) return null
    this.socketPath = socketPath
    return socketPath
  }

  /** Stop accepting connections, cancel every active run, remove the socket. */
  close(): void {
    if (this.closed) return
    this.closed = true
    for (const run of this.runs.values()) run.controller.abort()
    this.runs.clear()
    this.server.close()
    if (this.socketPath) {
      try {
        unlinkSync(this.socketPath)
      } catch {
        // Best effort: a vanished directory cannot be re-listened anyway.
      }
    }
  }

  /** Number of runs in flight (test introspection). */
  get activeRunCount(): number {
    return this.runs.size
  }

  private acceptConnection(conn: Socket): void {
    conn.on("error", () => conn.destroy())
    let buffer = Buffer.alloc(0)
    conn.on("data", (chunk: Buffer) => {
      buffer = Buffer.concat([buffer, chunk])
      let newline = buffer.indexOf(0x0a)
      while (newline >= 0) {
        if (newline > RUNNER_REQUEST_LINE_CAP_BYTES) {
          this.writeLine(conn, { ok: false, error: `request line exceeds ${RUNNER_REQUEST_LINE_CAP_BYTES} bytes` })
          conn.destroy()
          return
        }
        const line = buffer.subarray(0, newline).toString("utf8").trim()
        buffer = buffer.subarray(newline + 1)
        if (line.length > 0) void this.handleLine(conn, line)
        newline = buffer.indexOf(0x0a)
      }
      if (buffer.length > RUNNER_REQUEST_LINE_CAP_BYTES) {
        this.writeLine(conn, { ok: false, error: `request line exceeds ${RUNNER_REQUEST_LINE_CAP_BYTES} bytes` })
        conn.destroy()
      }
    })
    conn.on("close", () => {
      // Runs started on THIS connection are aborted in handleRun's own
      // close listener (NB-13), which matches by connection identity. This
      // handler stays for connections that only ever carried cancel/ping
      // traffic — nothing to do for those.
    })
  }

  private writeLine(conn: Socket, message: Record<string, unknown>): void {
    if (conn.destroyed) return
    try {
      conn.write(JSON.stringify(message) + "\n")
    } catch {
      // Peer vanished; nothing useful to do.
    }
  }

  private async handleLine(conn: Socket, line: string): Promise<void> {
    let request: unknown
    try {
      request = JSON.parse(line)
    } catch {
      this.writeLine(conn, { ok: false, error: "request is not valid JSON" })
      return
    }
    if (!isRecord(request) || typeof request.op !== "string") {
      this.writeLine(conn, { ok: false, error: "request must be an object with an op field" })
      return
    }
    if (this.closed) {
      this.writeLine(conn, { ok: false, error: "runner has shut down" })
      return
    }
    switch (request.op) {
      case "ping":
        this.writeLine(conn, {
          ok: true,
          op: "pong",
          uid: typeof process.getuid === "function" ? process.getuid() : null,
        })
        return
      case "cancel":
        this.handleCancel(conn, request)
        return
      case "run":
        await this.handleRun(conn, request)
        return
      default:
        this.writeLine(conn, { ok: false, error: `unknown op ${JSON.stringify(request.op).slice(0, 64)}` })
    }
  }

  private handleCancel(conn: Socket, request: Record<string, unknown>): void {
    const taskId = request.task_id
    if (typeof taskId !== "string" || taskId.length === 0) {
      this.writeLine(conn, { ok: false, error: "cancel requires a non-empty task_id" })
      return
    }
    const run = this.runs.get(taskId)
    if (!run) {
      this.writeLine(conn, { ok: false, error: `no run in flight for task ${taskId}` })
      return
    }
    run.controller.abort()
    this.writeLine(conn, { ok: true, op: "cancelled", task_id: taskId })
  }

  private async handleRun(conn: Socket, request: Record<string, unknown>): Promise<void> {
    if (!isRecord(request.request)) {
      this.writeLine(conn, { ok: false, error: "run requires a request object" })
      return
    }
    const taskRequest = request.request as unknown as TaskRequest
    const taskId = typeof taskRequest.task_id === "string" ? taskRequest.task_id : ""
    if (!taskId) {
      this.writeLine(conn, { ok: false, error: "run requires a task_id" })
      return
    }
    if (this.runs.has(taskId)) {
      this.writeLine(conn, { ok: false, error: `task ${taskId} is already in flight` })
      return
    }
    const extraEnv = isRecord(request.extra_env)
      ? Object.fromEntries(
          Object.entries(request.extra_env).filter(
            (entry): entry is [string, string] => typeof entry[1] === "string",
          ),
        )
      : undefined
    const controller = new AbortController()
    this.runs.set(taskId, { taskId, controller, conn })
    // NB-13: ack the acceptance before the run starts, so the bridge's
    // connection deadline clears the moment the runner takes the job and
    // the RUN deadline (task timeout + margin) takes over from there.
    this.writeLine(conn, { ok: true, op: "accepted", task_id: taskId })
    // NB-13: the runner's connection is the bridge's only heartbeat for this
    // run. A vanished bridge means progress has nowhere to go AND nothing
    // will ever collect the result — the client's close-path comment already
    // assumes the run dies with the connection. Abort it; the executor's
    // own escalation reaps the child CLI.
    conn.on("close", () => {
      const run = this.runs.get(taskId)
      if (run && run.conn === conn) {
        run.controller.abort()
        this.runs.delete(taskId)
      }
    })
    try {
      await executeTask(
        taskRequest,
        (progress: TaskProgress) => this.writeLine(conn, { op: "progress", progress }),
        (result: TaskResult) => {
          this.writeLine(conn, { op: "result", result })
          conn.end()
        },
        controller.signal,
        // NB-14: executeTask's workdir argument is resolved by resolveTaskWorkspace
        // from the request's own context (project_dir / cron_tick), under the
        // same WORKSPACE_ROOT the bridge resolved against — so a delegated task
        // runs in ITS project directory, not the workspace root.
        resolveTaskWorkspace(taskRequest.context, this.workspaceRoot),
        extraEnv,
      )
    } catch (err) {
      // executeTask's contract resolves (never rejects) — this is the
      // belt-and-suspenders shape so the bridge never waits forever.
      this.writeLine(conn, {
        op: "result",
        result: {
          task_id: taskId,
          status: "error",
          output: "",
          error: err instanceof Error ? err.message : String(err),
          duration_ms: 0,
        },
      })
      conn.end()
    } finally {
      this.runs.delete(taskId)
    }
  }
}

/** CLI entrypoint: `node task_runner.js <socket-path>` (env-driven too). */
export async function main(argv: string[] = process.argv.slice(2)): Promise<number> {
  const socketPath = argv[0] ?? taskRunnerSocketPath()
  if (!socketPath) {
    console.error("[task-runner] socket path required (argv or LUNARWING_TASK_RUNNER_SOCKET)")
    return 1
  }
  if (typeof process.getuid === "function" && process.getuid() === 0) {
    // The runner must never be root: it spawns task code, and a root runner
    // is exactly the WI#400 hazard this daemon exists to remove. The only
    // sanctioned launcher drops to the task uid before exec'ing it.
    console.error("[task-runner] refusing to run as root (WI#454: task code must run unprivileged)")
    return 1
  }
  const runner = new TaskRunner()
  const started = await runner.listen(socketPath)
  if (!started) {
    console.error(`[task-runner] could not listen on ${socketPath}`)
    return 1
  }
  console.log(`[task-runner] listening on ${started}`)
  // NB-17: publish runner liveness for the health server's /ready check.
  // The heartbeat interval is short relative to any watchdog, and the file
  // lives next to the socket (task-uid owned, 0700 dir).
  const stateFile = process.env.LUNARWING_TASK_RUNNER_STATE_FILE
    || `${dirname(socketPath)}/task-runner-state.json`
  const writeState = () => {
    try {
      writeFileSync(stateFile, JSON.stringify({
        ready: true,
        socket: socketPath,
        pid: process.pid,
        heartbeat_at: new Date().toISOString(),
      }) + "\n", { mode: 0o600 })
    } catch (err) {
      console.error("[task-runner] state write failed:", err instanceof Error ? err.message : err)
    }
  }
  writeState()
  const heartbeat = setInterval(writeState, 10_000)
  const shutdown = () => {
    clearInterval(heartbeat)
    try { rmSync(stateFile, { force: true }) } catch { /* state is best-effort */ }
    runner.close()
    process.exit(0)
  }
  process.on("SIGTERM", shutdown)
  process.on("SIGINT", shutdown)
  await new Promise(() => {})
  return 0
}

if (process.argv[1] && (process.argv[1].endsWith("task_runner.js") || process.argv[1].endsWith("task_runner.ts"))) {
  void main()
}
