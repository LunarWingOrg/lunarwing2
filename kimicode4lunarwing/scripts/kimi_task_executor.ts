/**
 * kimi_task_executor.ts — Executes coding tasks via the Kimi Code CLI.
 *
 * Spawns `kimi -p <prompt> --output-format stream-json` as a child process,
 * parses the JSONL event stream, and maps it to LunarWing protocol
 * task_progress / task_result callbacks.
 */

import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process"

import { isAbsolute } from "node:path"

import { dropLoaderEnv, resolveFromBridgePath, taskIdentity, taskSetprivArgv } from "./task_identity.js"
import {
  buildTaskPrompt,
  isRecord,
  truncateUtf8,
  truncateUtf8Tail,
  utf8ByteLength,
  type TaskRequest,
  type TaskProgress,
  type TaskResult,
  DEFAULT_TIMEOUT_MS,
} from "./lunarwing_runtime.js"
import { extractAssistantText, kimiErrorMessage, type KimiMessage } from "./kimi_result.js"
import { resolveWorkspacePath } from "./workspace_path.js"

// Read env lazily: binding these at module load freezes the values at import
// time, which breaks per-task overrides (and tests that set env after import).
function kimiBin(): string {
  return process.env.KIMI_BIN || "kimi"
}
// WI#463 (amended by NB-11, run-195 review): with the identity pinned (the
// container bridge, running as root), a bare `kimi` in the setpriv argv is
// resolved by the TASK-side PATH the executor is about to hand the child —
// a resolution the task uid can plant a shim into even though setpriv execs
// it after the drop. Resolve the bare name through the BRIDGE's own PATH
// first (root-ownership-checked), the same treatment setpriv gets (WI#400
// LD-11). An absolute KIMI_BIN — every operator/test override — is passed
// through unchanged, preserving the host-side dev shape where no `kimi` is
// installed on PATH at all. On the pinned path a failed bridge lookup FAILS
// the task (fail-closed): falling back to the bare name would send setpriv
// down the child PATH — the very lookup this function exists to avoid (and,
// after the NB-10 root-first reordering, one a prior task could have seeded).
function taskKimiBin(enforceRootOwnership?: boolean): string {
  const bin = kimiBin()
  if (isAbsolute(bin)) return bin
  return resolveFromBridgePath(bin, process.env.PATH, enforceRootOwnership)
}

/**
 * WI#462 (NB-10, run-195 review): the task child's PATH carries the
 * ROOT-OWNED directories first and the task uid's toolchain directories
 * after, so whatever one task leaves in its writable directories (a planted
 * `node`, a rewritten `python`) never shadows the image's copy for a LATER
 * task's processes. One policy for both execution shapes: the setpriv drop
 * (identity pinned) and the delegated task runner (NL-1, !1873 run 200).
 */
export const TASK_TOOLCHAIN_DIRS: readonly string[] = [
  "/home/kimi/.cargo/bin",
  "/usr/local/cargo/bin",
  "/usr/local/go/bin",
  "/home/kimi/go/bin",
]

export function taskChildPath(rootPath: string | undefined): string {
  return `${rootPath ?? ""}:${TASK_TOOLCHAIN_DIRS.join(":")}`
}

/** Per-call execution options (WI#454 delegated runner). */
export interface ExecuteTaskOptions {
  /**
   * NL-1 (!1873 run 200): set by the delegated task runner. The runner IS
   * the task uid, so the identity branch (setpriv drop, WI#462 PATH order,
   * WI#463 kimi lookup) never runs for it; this flag applies the same PATH
   * and kimi-resolution policy without the drop.
   */
  delegatedRunner?: boolean
  /**
   * NL-3 (!1873 run 200): called with the CLI's pid right after the spawn.
   * The CLI is spawned detached, so the pid is also its process-group id —
   * the handle the bridge needs to reap a run whose runner died.
   */
  onSpawn?: (pid: number) => void
}
function kimiCodeHome(): string {
  return process.env.KIMI_CODE_HOME || ""
}
function workspaceRoot(): string {
  return process.env.WORKSPACE_ROOT || "/workspace"
}

function boundedEnvInteger(name: string, fallback: number, min: number, max: number): number {
  const parsed = Number.parseInt(process.env[name] ?? "", 10)
  if (!Number.isFinite(parsed)) return fallback
  return Math.min(max, Math.max(min, parsed))
}

function progressHeartbeatMs(): number {
  return boundedEnvInteger("KIMI_PROGRESS_HEARTBEAT_MS", 120_000, 0, 300_000)
}

function killGraceMs(): number {
  return boundedEnvInteger("KIMI_KILL_GRACE_MS", 7_000, 100, 30_000)
}

function maxOutputBytes(): number {
  return boundedEnvInteger("KIMI_MAX_OUTPUT_BYTES", 1_048_576, 1_024, 16_777_216)
}

function maxStdoutLineBytes(): number {
  return boundedEnvInteger("KIMI_MAX_STDOUT_LINE_BYTES", 1_048_576, 1_024, 16_777_216)
}

function maxProgressBytes(): number {
  return boundedEnvInteger("KIMI_MAX_PROGRESS_BYTES", 65_536, 256, 1_048_576)
}

/**
 * How long the CLI may stay completely silent (no stream-json output) before
 * the task is treated as wedged (WI#263). The observed failure was a hang at
 * the final report-composition step: investigation events flowed normally,
 * then the CLI emitted nothing at all while the task sat at the 4h deadline.
 * 0 disables the watchdog.
 */
function silenceTimeoutMs(taskTimeoutMs: number): number {
  // Cap the silence budget at the remaining task deadline so the watchdog can
  // never outlive the task's own timeout.
  const budget = boundedEnvInteger("KIMI_SILENCE_TIMEOUT_MS", 900_000, 0, 86_400_000)
  if (budget === 0) return 0
  return Math.min(budget, Math.max(1, taskTimeoutMs))
}

type StopReason = "timeout" | "cancelled" | "silence"

class TaskStoppedError extends Error {
  constructor(readonly reason: StopReason) {
    super(`Task ${reason}`)
  }
}

export type ProgressCallback = (progress: TaskProgress) => void
export type ResultCallback = (result: TaskResult) => void

interface StreamJsonLine {
  role?: string
  content?: string
  tool_calls?: KimiMessage["tool_calls"]
  tool_call_id?: string
  type?: string
}

function isAssistantMessage(line: StreamJsonLine): boolean {
  return line.role === "assistant"
}

function isToolMessage(line: StreamJsonLine): boolean {
  return line.role === "tool"
}

function isMetaMessage(line: StreamJsonLine): boolean {
  return line.role === "meta"
}

function isKnownRole(role: string | undefined): boolean {
  return role === undefined || role === "assistant" || role === "tool" || role === "meta"
}

/**
 * Collects stream-json lines into a final assistant-text buffer.
 * Mirrors the opencode worker's extractAssistantText: only the last
 * assistant message with non-empty content is treated as the canonical
 * result, so intermediate tool-call-only assistant messages don't drown
 * out the real answer.
 */
function collectOutput(lines: KimiMessage[]): string {
  const assistantMessages = lines.filter((m) => m?.role === "assistant")
  for (let i = assistantMessages.length - 1; i >= 0; i--) {
    const content = assistantMessages[i]?.content
    if (typeof content === "string" && content.trim().length > 0) {
      return content.trim()
    }
  }
  // No assistant text — fall back to tool outputs.
  return extractAssistantText(lines).trim()
}

export async function executeTask(
  request: TaskRequest,
  onProgress: ProgressCallback,
  onResult: ResultCallback,
  abortSignal?: AbortSignal,
  resolvedWorkDir?: string,
  // WI#324: bridge-injected bindings (tool-proxy socket, task id). Applied
  // AFTER request.context.environment so task-supplied env can never shadow
  // the proxy binding for the MCP shim.
  extraEnv?: Record<string, string>,
  options?: ExecuteTaskOptions,
): Promise<void> {
  const startTime = Date.now()
  const timeoutMs = request.timeout_ms ?? DEFAULT_TIMEOUT_MS
  const heartbeatMs = progressHeartbeatMs()
  const killGrace = killGraceMs()
  const outputCap = maxOutputBytes()
  const stdoutLineCap = maxStdoutLineBytes()
  const progressCap = maxProgressBytes()
  let output = ""
  let outputBytes = 0
  const collectedMessages: KimiMessage[] = []
  let collectedBytes = 0
  let stopReason: StopReason | undefined
  let proc: ChildProcessWithoutNullStreams | undefined
  let procClosed = false
  let killEscalation: NodeJS.Timeout | undefined
  let stderrBuffer = ""
  let lastProgressAt = Date.now()
  // WI#263: tracks the last time the CLI emitted any stream-json line. The
  // progress heartbeat keeps the socket warm, but only real CLI output proves
  // the CLI is alive — a wedged CLI streams nothing while the heartbeat
  // claims "still waiting".
  let lastStreamAt = Date.now()
  const silenceBudget = silenceTimeoutMs(timeoutMs)

  // Bounds on in-memory accumulation for long/verbose runs (default task
  // timeout is 4h). Progress is streamed live, so truncating loses nothing
  // operationally; the canonical result only needs the LAST non-empty
  // assistant message, so old messages can be dropped safely.
  const STDERR_CAP = 65_536
  const MESSAGES_CAP = 1_000
  const appendOutput = (text: string) => {
    const remaining = outputCap - outputBytes
    if (remaining <= 0) return
    const appended = truncateUtf8(text, remaining)
    output += appended
    outputBytes += utf8ByteLength(appended)
  }
  const appendStderr = (text: string) => {
    stderrBuffer = truncateUtf8Tail(stderrBuffer + text, STDERR_CAP)
  }

  const emitProgress = (delta: string) => {
    lastProgressAt = Date.now()
    onProgress({ task_id: request.task_id, delta: truncateUtf8(delta, progressCap), done: false })
  }

  const stoppedResult = (reason: StopReason): TaskResult => ({
    task_id: request.task_id,
    status: reason === "cancelled" ? "cancelled" : "timed_out",
    output: truncateUtf8(output.trim(), outputCap),
    error:
      reason === "timeout"
        ? `Task timed out after ${timeoutMs}ms`
        : reason === "silence"
          ? `kimi produced no stream output for ${silenceBudget}ms (wedged after ${Date.now() - startTime}ms of runtime); partial output preserved`
          : null,
    duration_ms: Date.now() - startTime,
  })

  if (abortSignal?.aborted) {
    onResult(stoppedResult("cancelled"))
    return
  }

  const progressHeartbeat =
    heartbeatMs > 0
      ? setInterval(() => {
          if (Date.now() - lastProgressAt < heartbeatMs) return
          emitProgress("[worker] Still waiting for Kimi Code response; task remains active.")
        }, heartbeatMs)
      : undefined

  const timeout = setTimeout(() => {
    if (stopReason) return
    stopReason = "timeout"
    killProc()
  }, timeoutMs)

  // WI#263: silence watchdog. A CLI that emits nothing for the silence budget
  // is wedged (the observed signature: full investigation, then silence at
  // report composition). Kill it and resolve as timed_out carrying whatever
  // partial output was streamed, so the investigation is not lost and the
  // slot is freed instead of lingering to the 4h deadline.
  const silenceWatchdog =
    silenceBudget > 0
      ? setInterval(() => {
          if (stopReason) return
          if (Date.now() - lastStreamAt < silenceBudget) return
          stopReason = "silence"
          console.error(
            `[kimi executor] no stream output for ${silenceBudget}ms (task ${request.task_id}); treating as wedged`,
          )
          killProc()
        }, Math.min(silenceBudget, 60_000))
      : undefined

  const handleAbort = () => {
    if (stopReason) return
    stopReason = "cancelled"
    killProc()
  }
  abortSignal?.addEventListener("abort", handleAbort, { once: true })
  if (abortSignal?.aborted) handleAbort()

  function killProc() {
    if (!proc || procClosed) return
    signalProcessGroup("SIGTERM")
    if (killEscalation) return
    killEscalation = setTimeout(() => {
      signalProcessGroup("SIGKILL")
    }, killGrace)
  }

  function signalProcessGroup(signal: NodeJS.Signals): void {
    if (!proc) return
    try {
      if (process.platform !== "win32" && proc.pid) process.kill(-proc.pid, signal)
      else proc.kill(signal)
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ESRCH") return
      try {
        proc.kill(signal)
      } catch (fallbackError) {
        if ((fallbackError as NodeJS.ErrnoException).code !== "ESRCH") {
          console.error(`[kimi executor] failed to send ${signal}:`, fallbackError)
        }
      }
    }
  }

  function processGroupExists(): boolean {
    if (!proc?.pid || process.platform === "win32") return false
    try {
      process.kill(-proc.pid, 0)
      return true
    } catch (error) {
      return (error as NodeJS.ErrnoException).code !== "ESRCH"
    }
  }

  try {
    const workDir = resolvedWorkDir ?? resolveWorkDir(request.context?.project_dir || workspaceRoot())
    const promptText = buildTaskPrompt(request)
    // No --yolo/--auto flags: this kimi-code version rejects them in
    // combination with -p (print mode), which forces the auto permission
    // policy on its own. Dangerous-command enforcement comes from the baked
    // PreToolUse hook (WI#206), not a permission-mode flag.
    const args = ["-p", promptText, "--output-format", "stream-json"]
    // Per-task env is assembled, never injected into the bridge's shared
    // process.env — concurrent tasks in different workspaces must not see
    // each other's variables (no mutation means nothing to leak or restore).
    const env: NodeJS.ProcessEnv = { ...process.env, ...request.context?.environment }
    // WI#324: the bridge's tool-proxy binding overrides task-supplied env.
    if (extraEnv) Object.assign(env, extraEnv)
    // WI#206: a task (or a prompt-injected command inside it) must not be able
    // to lower its own approval policy. These are stripped per task even
    // though the entrypoint also unsets them — request.context.environment is
    // attacker-controllable task input, not operator config.
    delete env.KIMI_CODE_DANGEROUS_COMMAND_GUARD
    delete env.KIMI_CODE_PERMISSION_MODE_REMINDER
    // WI#411: kimi-code 2.2.0 added KIMI_CODE_TRUST_WORKSPACE. A truthy value
    // ("1", "true", "yes", "on") makes the CLI treat the workspace as trusted
    // and spawn every MCP server listed in the repo's .mcp.json or
    // .kimi-code/mcp.json at startup — before any model turn and outside the
    // WI#206 PreToolUse hook. Task input must never be able to grant that.
    delete env.KIMI_CODE_TRUST_WORKSPACE
    // WI#373: AGENT_AUTH_TOKEN is the bridge's hub-handshake credential. The
    // task process runs untrusted job code, so it must never inherit the
    // token. Deleted after every merge (process.env spread, task env, extra
    // proxy env) so nothing can re-inject it; the bridge keeps its own copy.
    if (Object.hasOwn(request.context?.environment ?? {}, "AGENT_AUTH_TOKEN")) {
      // FY-5: a credential grant reusing the bridge's handshake key is
      // dropped — say so instead of letting the value vanish silently.
      console.warn(
        `[kimi executor] dropped task-supplied env entry AGENT_AUTH_TOKEN for task ${request.task_id}: reserved for the bridge handshake (WI#373)`,
      )
    }
    delete env.AGENT_AUTH_TOKEN
    // WI#400: the credential now lives in a root-only file the bridge reads;
    // the pointer is stripped alongside the token itself.
    if (Object.hasOwn(request.context?.environment ?? {}, "AGENT_AUTH_TOKEN_FILE")) {
      console.warn(
        `[kimi executor] dropped task-supplied env entry AGENT_AUTH_TOKEN_FILE for task ${request.task_id}: reserved for the bridge handshake (WI#400)`,
      )
    }
    delete env.AGENT_AUTH_TOKEN_FILE
    const kimiHome = kimiCodeHome()
    if (kimiHome) {
      env.KIMI_CODE_HOME = kimiHome
    }
    // Auto-update and telemetry: disable to keep CI/headless runs deterministic.
    env.KIMI_CODE_NO_AUTO_UPDATE = env.KIMI_CODE_NO_AUTO_UPDATE || "1"
    env.KIMI_DISABLE_TELEMETRY = env.KIMI_DISABLE_TELEMETRY || "1"

    // WI#400: the bridge runs as container root; task code runs under the
    // pinned task uid (separate from the bridge) via a setpriv wrapper, so
    // the bridge's and PID 1's environ are EACCES to it. Root's HOME/GOPATH
    // are unreadable to the task uid and retargeted at the task home.
    const identity = taskIdentity()
    let spawnBin = kimiBin()
    let spawnArgs = args
    if (identity) {
      if (env.HOME === "/root") env.HOME = "/home/kimi"
      if (env.GOPATH === "/root/go") env.GOPATH = "/home/kimi/go"
      // WI#454/ME-1: the image PATH carries only root-owned directories (the
      // bridge runs as root and resolves through it), so the task uid's
      // toolchain directories are handed to the child here, at the drop.
      // WI#462 (NB-10, run-195 review): the toolchain order is ROOT-OWNED
      // dirs FIRST, task-uid-writable dirs after — the same ME-1 posture as
      // the bridge PATH. The old task-first order was backwards: whatever
      // one task left in its writable directories (a planted `node`, a
      // rewritten `python`) is then used by LATER tasks' processes in the
      // same container, including the kimi CLI's own interpreter lookup
      // (its npm entry point starts with an env-node line). Task-owned
      // names root does not ship (cargo/go toolchain dirs) still resolve
      // from the task directories, so the toolchain keeps working —
      // ordering only decides which copy wins when BOTH have the name.
      // The bridge itself never resolves through this PATH (setpriv and
      // the kimi binary are resolved from the bridge PATH, below and via
      // taskKimiBin).
      env.PATH = taskChildPath(env.PATH)
      // WI#400 LD-11/LP-6: setpriv comes from the bridge's OWN PATH (a bare
      // name would be looked up through the child env), and the loader
      // variables a task could supply are dropped before the still-root
      // setpriv honours them.
      spawnBin = resolveFromBridgePath("setpriv")
      for (const key of dropLoaderEnv(env)) {
        console.warn(
          `[kimi executor] dropped task env entry ${key} for task ${request.task_id}: loader variables never reach the privilege drop (WI#400 LD-11)`
        )
      }
      spawnArgs = [...taskSetprivArgv(identity), taskKimiBin(), ...args]
    } else if (options?.delegatedRunner) {
      // NL-1 (!1873 run 200): the runner IS the task uid, so the branch
      // above never ran for it, and a bare `kimi` resolved through the
      // runner's inherited PATH, whose first entry was the task-owned
      // ~/.cargo/bin: one task could plant a `kimi` (or a `node` for the
      // CLI's env-node line) that every later task then ran. The same
      // policy applies here without the drop: the CLI's PATH is the
      // runner's own root-owned launcher PATH followed by the task
      // toolchain dirs (WI#462 order), and a bare kimi resolves only to a
      // root-owned file in a root-owned, non-group/world-writable directory
      // of the RUNNER's PATH, failing the task when there is none (WI#463).
      env.PATH = taskChildPath(env.PATH)
      // The runner's listening-socket handoff (NL-7) is not task input.
      delete env.LUNARWING_TASK_RUNNER_LISTEN_FD
      spawnBin = taskKimiBin(true)
    }

    proc = spawn(spawnBin, spawnArgs, {
      cwd: workDir,
      env,
      stdio: ["pipe", "pipe", "pipe"],
      detached: process.platform !== "win32",
    })
    if (proc.pid !== undefined && options?.onSpawn) {
      try {
        options.onSpawn(proc.pid)
      } catch (error) {
        console.error("[kimi executor] onSpawn hook failed:", error)
      }
    }
    if (stopReason) killProc()

    await new Promise<void>((resolveTask, rejectTask) => {
      let stdoutBuffer = Buffer.alloc(0)
      let streamError: Error | undefined

      proc!.stdout.on("data", (chunk: Buffer) => {
        if (streamError) return
        // Any raw stdout byte is proof of life for the silence watchdog,
        // even when the line is not yet parseable.
        lastStreamAt = Date.now()
        stdoutBuffer = Buffer.concat([stdoutBuffer, chunk])
        let newlineIndex = stdoutBuffer.indexOf(0x0a)
        while (newlineIndex >= 0) {
          if (newlineIndex > stdoutLineCap) {
            streamError = new Error(`kimi stdout line exceeds ${stdoutLineCap} bytes`)
            killProc()
            return
          }
          const line = stdoutBuffer.subarray(0, newlineIndex).toString("utf8").trim()
          stdoutBuffer = stdoutBuffer.subarray(newlineIndex + 1)
          if (line.length > 0) processStreamLine(line)
          newlineIndex = stdoutBuffer.indexOf(0x0a)
        }
        if (stdoutBuffer.length > stdoutLineCap) {
          streamError = new Error(`kimi stdout line exceeds ${stdoutLineCap} bytes`)
          killProc()
        }
      })

      proc!.stderr.on("data", (chunk: Buffer) => {
        appendStderr(chunk.toString("utf-8"))
      })

      proc!.on("error", (err) => {
        if (stopReason) return
        rejectTask(err)
      })

      proc!.on("close", (code) => {
        procClosed = true
        if (killEscalation && !processGroupExists()) clearTimeout(killEscalation)
        if (!streamError && stdoutBuffer.length > 0) {
          if (stdoutBuffer.length > stdoutLineCap) {
            streamError = new Error(`kimi stdout line exceeds ${stdoutLineCap} bytes`)
          } else {
            const line = stdoutBuffer.toString("utf8").trim()
            if (line.length > 0) processStreamLine(line)
          }
        }
        if (stopReason) {
          rejectTask(new TaskStoppedError(stopReason))
          return
        }
        if (streamError) {
          rejectTask(streamError)
          return
        }
        if (code !== null && code !== 0) {
          // Non-zero exit without an explicit stop reason: treat as error.
          const errMsg = stderrBuffer.trim() || `kimi exited with code ${code}`
          rejectTask(new Error(errMsg))
          return
        }
        resolveTask()
      })

      function processStreamLine(line: string): void {
        let value: unknown
        try {
          value = JSON.parse(line)
        } catch {
          return
        }
        if (!isRecord(value)) return
        const parsed: StreamJsonLine = {
          role: typeof value.role === "string" ? value.role : undefined,
          content: typeof value.content === "string" ? value.content : undefined,
          type: typeof value.type === "string" ? value.type : undefined,
          tool_call_id: typeof value.tool_call_id === "string" ? value.tool_call_id : undefined,
          tool_calls: Array.isArray(value.tool_calls)
            ? value.tool_calls.filter((call): call is NonNullable<KimiMessage["tool_calls"]>[number] =>
                isRecord(call) && typeof call.id === "string" && typeof call.type === "string" &&
                isRecord(call.function) && typeof call.function.name === "string" &&
                typeof call.function.arguments === "string")
            : undefined,
        }

        // Collect for final output extraction. Bounded: only the tail is
        // needed to find the last non-empty assistant message.
        const content = typeof parsed.content === "string"
          ? truncateUtf8(parsed.content, outputCap)
          : undefined
        if (isAssistantMessage(parsed)) {
          const msg: KimiMessage = { role: "assistant", content }
          collectedMessages.push(msg)
          collectedBytes += content ? utf8ByteLength(content) : 0
          while (collectedMessages.length > MESSAGES_CAP || collectedBytes > outputCap) {
            const removed = collectedMessages.shift()
            if (removed?.content) collectedBytes -= utf8ByteLength(removed.content)
          }
        }

        if (isAssistantMessage(parsed)) {
          const text = typeof parsed.content === "string" ? parsed.content.trim() : ""
          if (text) {
            appendOutput(text + "\n")
            emitProgress(text)
          }
          // Surface tool-call activity so the agent sees live progress.
          if (parsed.tool_calls && parsed.tool_calls.length > 0) {
            for (const tc of parsed.tool_calls) {
              const delta = `[${tc.function?.name || "tool"}] ${tc.function?.arguments || ""}`
              emitProgress(delta)
            }
          }
        } else if (isToolMessage(parsed)) {
          const toolOutput = typeof parsed.content === "string" ? parsed.content.trim() : ""
          if (toolOutput) {
            const delta = `[tool result] ${toolOutput}`
            appendOutput(delta + "\n")
            emitProgress(delta)
          }
        } else if (isMetaMessage(parsed)) {
          // Meta messages (resume hints, retry notices, version) go to stderr only.
          if (typeof parsed.type === "string") {
            console.error(`[kimi meta] ${truncateUtf8(parsed.type, progressCap)}`)
          }
        } else if (!isKnownRole(parsed.role)) {
          // Roles outside {assistant, tool, meta} (a new reasoning role, system/
          // user echoes) are dropped from output — say so instead of failing
          // later with the generic "completed without assistant text" error.
          console.warn(
            `[kimi stream] unrecognized role ${JSON.stringify(truncateUtf8(String(parsed.role), 64))}: line dropped`,
          )
        }
      }
    })

    // Process exited cleanly — assemble the result.
    const canonicalOutput = collectOutput(collectedMessages)
    const resultOutput = truncateUtf8(canonicalOutput || output.trim(), outputCap)
    const durationMs = Date.now() - startTime

    if (!resultOutput) {
      onResult({
        task_id: request.task_id,
        status: "error",
        output: "",
        error:
          "kimi completed without assistant text or tool output " +
          "(check stderr for dropped stream-json roles or parse failures)",
        duration_ms: durationMs,
      })
      return
    }

    onResult({
      task_id: request.task_id,
      status: "success",
      output: resultOutput,
      error: null,
      duration_ms: durationMs,
    })
  } catch (err) {
    const durationMs = Date.now() - startTime
    const stopped = stopReason ?? (err instanceof TaskStoppedError ? err.reason : undefined)
    if (stopped) {
      onResult(stoppedResult(stopped))
      return
    }
    const errorMsg = truncateUtf8(kimiErrorMessage(err), STDERR_CAP)
    onResult({
      task_id: request.task_id,
      status: "error",
      output: truncateUtf8(output.trim(), outputCap),
      error: errorMsg,
      duration_ms: durationMs,
    })
  } finally {
    clearTimeout(timeout)
    if (progressHeartbeat) clearInterval(progressHeartbeat)
    if (silenceWatchdog) clearInterval(silenceWatchdog)
    abortSignal?.removeEventListener("abort", handleAbort)
    if (proc && !procClosed) signalProcessGroup("SIGKILL")
  }
}

function resolveWorkDir(path: string): string {
  return resolveWorkspacePath(path, workspaceRoot())
}
