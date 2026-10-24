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
 *     → {"ok": true, "op": "accepted", "task_id": "..."}
 *     → {"op": "spawned", "task_id": "...", "pgid": <CLI process group>}
 *     → any number of {"op": "progress", "progress": <TaskProgress>}
 *     → exactly one terminal {"op": "result", "result": <TaskResult>}
 *   {"op": "cancel", "task_id": "..."} → {"ok": true, "op": "cancelled"}
 *     (the run's own terminal result follows once the executor's
 *     SIGTERM/SIGKILL escalation has stopped the CLI; the bridge sends this
 *     on the run's own connection and waits for that result, NL-3)
 *   {"op": "ping"} → {"ok": true, "op": "pong", "uid": <runner uid>}
 *   Every failure shape is {"ok": false, "error": "<reason>"}.
 *
 * Bounds (NL-2/NL-8, !1873 run 200): every line, in both directions, is
 * capped at runnerLineCapBytes() — the hub's WS_MAX_PAYLOAD_BYTES plus
 * framing slack, read from the same env as the bridge. A longer request
 * line can never be valid, so it is refused and the connection dropped.
 * Progress and result lines are serialized within the cap by truncating
 * their delta/output/error with the hub envelope's own rule, so a large
 * result arrives truncated, never as an over-cap line the client rejects.
 *
 * Listening (NL-7): in the image the root launcher binds the socket in a
 * root-only directory (task_runner_bind.py) and the runner inherits just the
 * listening descriptor (LUNARWING_TASK_RUNNER_LISTEN_FD); the path form
 * (`node task_runner.js <socket-path>`) stays for host-side runs and tests.
 */

import { createServer, type Server, type Socket } from "node:net"
import { lstatSync, mkdirSync, rmSync, unlinkSync, writeFileSync } from "node:fs"
import { dirname } from "node:path"

import { executeTask } from "./kimi_task_executor.js"
import { resolveTaskWorkspace } from "./workspace_path.js"
import {
  isRecord,
  runnerLineCapBytes,
  serializeFrameWithinLimit,
  type TaskProgress,
  type TaskRequest,
  type TaskResult,
} from "./lunarwing_runtime.js"

/** Env var carrying the inherited listening descriptor (NL-7). */
export const RUNNER_LISTEN_FD_ENV = "LUNARWING_TASK_RUNNER_LISTEN_FD"

/**
 * Hub-handshake credential names the runner must never see (WI#373/WI#400).
 * The launcher unsets both before exec'ing the runner; the list below turns
 * that convention into an enforced contract (WI#454 — the runner is the
 * environment store for task code, so it must not itself hold the credential
 * the bridge uses to authenticate to the hub).
 */
export const RESERVED_CREDENTIAL_KEYS = ["AGENT_AUTH_TOKEN", "AGENT_AUTH_TOKEN_FILE"] as const

/**
 * Reserved credential names present (non-empty) in `env`. Empty means the
 * launcher's credential handoff did its job.
 */
export function presentCredentialKeys(env: NodeJS.ProcessEnv = process.env): string[] {
  return RESERVED_CREDENTIAL_KEYS.filter((key) => (env[key] ?? "") !== "")
}

/** Everything `main` needs to decide whether the daemon may come up. */
export interface StartupState {
  listenFd: number | null
  socketPath: string
  env: NodeJS.ProcessEnv
  /** `process.getuid()` when the platform has it, else null. */
  uid: number | null
}

/**
 * The daemon's startup gates, in order, as a pure function: returns the
 * refusal message, or null when the runner may listen. Kept separate from
 * `main` so every gate is pinned in both uid shapes — a test running as root
 * cannot exercise the credential gate through `main` because the root gate
 * answers first.
 */
export function startupRefusal(state: StartupState): string | null {
  const { listenFd, socketPath, env, uid } = state
  if (listenFd === null && !socketPath) {
    return "socket path required (argv or LUNARWING_TASK_RUNNER_SOCKET), or an inherited LUNARWING_TASK_RUNNER_LISTEN_FD"
  }
  // WI#454: the runner is the environment store for task code, so the bridge's
  // hub-handshake credential must not reach it. The launcher unsets both names
  // before the exec; enforce that here rather than trust the convention — a
  // runner that inherited the token would hand it to every task it spawns.
  // This is a configuration error on any uid, so it is checked before the
  // unit gate and reports consistently whatever the runner runs as.
  const leaked = presentCredentialKeys(env)
  if (leaked.length > 0) {
    return (
      `refusing to start with the hub credential in its environment (${leaked.join(", ")}); ` +
      "the launcher must unset it before exec (WI#454)"
    )
  }
  if (uid === 0) {
    // The runner must never be root: it spawns task code, and a root runner
    // is exactly the WI#400 hazard this daemon exists to remove. The only
    // sanctioned launcher drops to the task uid before exec'ing it.
    return "refusing to run as root (WI#454: task code must run unprivileged)"
  }
  return null
}

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
  // NL-8: one cap for both directions, following the hub's envelope cap.
  private readonly lineCap = runnerLineCapBytes()

  constructor(private readonly workspaceRoot: string = process.env.WORKSPACE_ROOT || "/workspace") {
    this.server = createServer((conn) => this.acceptConnection(conn))
  }

  /** Line cap in bytes for both directions (test introspection). */
  get lineCapBytes(): number {
    return this.lineCap
  }

  /**
   * NL-7: listen on an inherited, already-bound and listening descriptor (the
   * root launcher's task_runner_bind.py). The runner never owns the socket
   * path, so close() leaves it to the launcher. Node marks inherited
   * descriptors close-on-exec at startup, so the CLI and its tools never
   * inherit the listener.
   */
  async listenFd(fd: number): Promise<boolean> {
    if (this.closed) return false
    return await new Promise<boolean>((resolve) => {
      this.server.once("error", (err) => {
        console.error("[task-runner] listen on inherited fd failed:", err.message)
        resolve(false)
      })
      this.server.listen({ fd }, () => resolve(true))
    })
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
        if (newline > this.lineCap) {
          this.writeLine(conn, { ok: false, error: `request line exceeds ${this.lineCap} bytes` })
          conn.destroy()
          return
        }
        const line = buffer.subarray(0, newline).toString("utf8").trim()
        buffer = buffer.subarray(newline + 1)
        if (line.length > 0) void this.handleLine(conn, line)
        newline = buffer.indexOf(0x0a)
      }
      if (buffer.length > this.lineCap) {
        this.writeLine(conn, { ok: false, error: `request line exceeds ${this.lineCap} bytes` })
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
      conn.write(this.serializeLine(message) + "\n")
    } catch {
      // Peer vanished; nothing useful to do.
    }
  }

  /**
   * NL-2 (!1873 run 200): the executor caps OUTPUT bytes, not the line, and
   * JSON escaping can grow a 1 MiB output well past the client's line cap
   * (a timed_out result with 1 MiB of newline-separated output was a
   * 1.57 MB line, rejected outright). Progress and result lines are
   * serialized within the cap with the hub envelope's truncation rule.
   */
  private serializeLine(message: Record<string, unknown>): string {
    const key = message.op === "result" ? "result" : message.op === "progress" ? "progress" : null
    if (key === null) return JSON.stringify(message)
    try {
      return serializeFrameWithinLimit(message, key, this.lineCap, "task runner line")
    } catch {
      // Only oversized non-truncatable fields (an enormous task_id) land
      // here; the terminal frame must still be a valid line.
      const inner = isRecord(message[key]) ? (message[key] as Record<string, unknown>) : {}
      const taskId = typeof inner.task_id === "string" ? inner.task_id.slice(0, 256) : ""
      if (key === "progress") return JSON.stringify({ op: "progress", progress: { task_id: taskId, delta: "", done: false } })
      return JSON.stringify({
        op: "result",
        result: {
          task_id: taskId,
          status: "error",
          output: "",
          error: `task result exceeds the ${this.lineCap}-byte runner line cap`,
          duration_ms: typeof inner.duration_ms === "number" ? inner.duration_ms : 0,
        },
      })
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
        {
          // NL-1: the WI#462 PATH order and the root-owned kimi lookup.
          delegatedRunner: true,
          // NL-3: the bridge needs the CLI's process group to reap a run
          // whose runner dies before the terminal result.
          onSpawn: (pid) => this.writeLine(conn, { op: "spawned", task_id: taskId, pgid: pid }),
        },
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

/** Parses LUNARWING_TASK_RUNNER_LISTEN_FD; null when unset, throws when malformed. */
export function inheritedListenFd(raw: string | undefined = process.env[RUNNER_LISTEN_FD_ENV]): number | null {
  if (raw === undefined || raw === "") return null
  if (!/^[0-9]+$/.test(raw) || Number(raw) < 3 || !Number.isSafeInteger(Number(raw))) {
    throw new Error(`${RUNNER_LISTEN_FD_ENV} must be a descriptor number >= 3 (got ${JSON.stringify(raw).slice(0, 32)})`)
  }
  return Number(raw)
}

/**
 * CLI entrypoint. In the image: `node task_runner.js` with the listening
 * descriptor inherited from the root launcher (NL-7). Host runs and tests:
 * `node task_runner.js <socket-path>` (or LUNARWING_TASK_RUNNER_SOCKET).
 */
export async function main(argv: string[] = process.argv.slice(2)): Promise<number> {
  let listenFd: number | null
  try {
    listenFd = inheritedListenFd()
  } catch (err) {
    console.error("[task-runner]", err instanceof Error ? err.message : err)
    return 1
  }
  const socketPath = listenFd === null ? (argv[0] ?? taskRunnerSocketPath()) : ""
  const refusal = startupRefusal({
    listenFd,
    socketPath,
    env: process.env,
    uid: typeof process.getuid === "function" ? process.getuid() : null,
  })
  if (refusal !== null) {
    console.error(`[task-runner] ${refusal}`)
    return 1
  }
  const runner = new TaskRunner()
  if (listenFd !== null) {
    if (!(await runner.listenFd(listenFd))) {
      console.error(`[task-runner] could not listen on inherited fd ${listenFd}`)
      return 1
    }
    console.log(`[task-runner] listening on inherited fd ${listenFd}`)
  } else {
    const started = await runner.listen(socketPath)
    if (!started) {
      console.error(`[task-runner] could not listen on ${socketPath}`)
      return 1
    }
    console.log(`[task-runner] listening on ${started}`)
  }
  // NB-17: publish runner liveness for the health server's /ready check.
  // The heartbeat interval is short relative to any watchdog. The file lives
  // in the task-uid-owned runner directory; in the inherited-fd shape the
  // socket directory is root-only, so the launcher always exports the path.
  // NL-8: the state carries liveness only — /ready is served off-container,
  // so the socket path and pid are not published.
  const stateFile = process.env.LUNARWING_TASK_RUNNER_STATE_FILE
    || (socketPath ? `${dirname(socketPath)}/task-runner-state.json` : "")
  const writeState = () => {
    if (!stateFile) return
    try {
      writeFileSync(stateFile, JSON.stringify({
        ready: true,
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
    if (stateFile) {
      try { rmSync(stateFile, { force: true }) } catch { /* state is best-effort */ }
    }
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
