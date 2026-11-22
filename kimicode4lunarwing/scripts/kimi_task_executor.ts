/**
 * kimi_task_executor.ts — Executes coding tasks via the Kimi Code CLI.
 *
 * Spawns `kimi -p <prompt> --output-format stream-json` as a child process,
 * parses the JSONL event stream, and maps it to LunarWing protocol
 * task_progress / task_result callbacks.
 */

import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process"

import { chownSync, lstatSync, readFileSync, renameSync, unlinkSync, writeFileSync, type Stats } from "node:fs"

import { isAbsolute, join } from "node:path"

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
 * WI#833 point 2: `KIMI_CODE_*` names a task may set in its per-task
 * environment. Everything else with that prefix is dropped, so a new upstream
 * name is scrubbed by default rather than reaching the CLI from task input.
 * Both entries are informational image metadata; the CLI reads neither, so a
 * task cannot change lane behavior through them. The names the lane owns
 * itself (`KIMI_CODE_HOME`, `KIMI_CODE_LLM_HEADERS_TIMEOUT_MS`,
 * `KIMI_CODE_NO_AUTO_UPDATE`) are not on the list: lane policy sets or clears
 * them after merging the environments.
 */
export const TASK_SETTABLE_KIMI_CODE_ENV: readonly string[] = [
  "KIMI_CODE_VERSION",
  "KIMI_CODE_IMAGE_VERSION",
]

/**
 * `KIMI_CODE_*` names the executor handles by its own rule, so the allowlist
 * pass leaves them alone: the three scrubbed one at a time by their own
 * comments, the relay URL, and the operator-owned names reassigned below
 * (`KIMI_CODE_HOME`, `KIMI_CODE_LLM_HEADERS_TIMEOUT_MS`,
 * `KIMI_CODE_NO_AUTO_UPDATE`). Listing them keeps one warning per drop
 * instead of two, and stops an operator value being reported as a task drop.
 */
const HANDLED_KIMI_CODE_ENV = new Set([
  "KIMI_CODE_DANGEROUS_COMMAND_GUARD",
  "KIMI_CODE_PERMISSION_MODE_REMINDER",
  "KIMI_CODE_TRUST_WORKSPACE",
  "KIMI_CODE_REMOTE_CONTROL_RELAY_URL",
  "KIMI_CODE_HOME",
  "KIMI_CODE_LLM_HEADERS_TIMEOUT_MS",
  "KIMI_CODE_NO_AUTO_UPDATE",
])

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

/** The user-level MCP registration the CLI loads at session start (WI#848). */
const MCP_CONFIG_FILE = "mcp.json"

interface TrustedMcpSnapshot {
  home: string
  content: string
  mode: number
  uid: number
  gid: number
}

let trustedMcpSnapshot: TrustedMcpSnapshot | null = null

/**
 * WI#848: the CLI reads `$KIMI_CODE_HOME/mcp.json` as trusted user-level
 * config and spawns every server listed there before the first model turn.
 * Those processes never pass through the Bash tool, so the PreToolUse hook
 * cannot see them, and the file lives in the task's own home — a task can
 * rewrite it for a later session. The task-side entrypoint materializes the
 * root-owned baked registration at container start; this snapshot freezes
 * those bytes (and mode) in the runner's memory before any task runs, and
 * `restoreTrustedMcpConfig` puts them back before every CLI spawn. An
 * operator override via KIMI_MCP_SOURCE is preserved because the snapshot is
 * taken after the entrypoint wrote it.
 *
 * Capture at startup, never lazily: a lazy capture on the first task could
 * bless a file a previous runner instance left poisoned. This is a
 * same-uid restore, not a boundary — a task already running in this
 * container can still race the window between restore and the CLI's read,
 * and a nested `kimi -p` a task starts itself reads whatever the file holds
 * at that moment. The container and the hook's deny rule remain the
 * boundary; the restore removes the cross-task persistence this item is
 * about.
 */
export function captureTrustedMcpConfig(home = kimiCodeHome()): void {
  trustedMcpSnapshot = null
  if (!home) return
  const file = join(home, MCP_CONFIG_FILE)
  let stat: Stats
  try {
    stat = lstatSync(file)
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
      console.warn(
        `[kimi executor] cannot inspect the trusted user-level ${MCP_CONFIG_FILE} (${describeFsError(error)}); no per-task restore (WI#848)`,
      )
    }
    return
  }
  if (!stat.isFile()) {
    console.warn(
      `[kimi executor] the trusted user-level ${MCP_CONFIG_FILE} is not a regular file; no per-task restore (WI#848)`,
    )
    return
  }
  try {
    trustedMcpSnapshot = {
      home,
      content: readFileSync(file, "utf8"),
      mode: stat.mode & 0o777,
      uid: stat.uid,
      gid: stat.gid,
    }
  } catch (error) {
    console.warn(
      `[kimi executor] cannot read the trusted user-level ${MCP_CONFIG_FILE} (${describeFsError(error)}); no per-task restore (WI#848)`,
    )
  }
}

function describeFsError(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

/**
 * Put the captured registration back if anything changed it. A task that
 * edited the file has already finished its own session, so the next task is
 * innocent: log and repair rather than fail it. Only a failed write aborts
 * the spawn — the CLI would otherwise start from content nobody vetted.
 */
function restoreTrustedMcpConfig(taskId: string): void {
  const snapshot = trustedMcpSnapshot
  if (!snapshot) return
  const file = join(snapshot.home, MCP_CONFIG_FILE)
  try {
    const stat = lstatSync(file)
    if (stat.isFile() && readFileSync(file, "utf8") === snapshot.content) return
  } catch {
    // Missing or unreadable: fall through to the write, which reports the
    // real failure (or repairs a deleted/symlinked file).
  }
  const temporary = join(snapshot.home, `.${MCP_CONFIG_FILE}.${process.pid}.restore`)
  try {
    writeFileSync(temporary, snapshot.content, { mode: snapshot.mode })
    // The legacy single-container shape runs this executor as root while the
    // CLI runs as the task uid (WI#400). Its rename would hand the task a
    // root-owned 0600 file it cannot read, so restore the original owner
    // whenever this process is privileged enough to do it.
    if (process.getuid?.() === 0 && (snapshot.uid !== 0 || snapshot.gid !== 0)) {
      chownSync(temporary, snapshot.uid, snapshot.gid)
    }
    renameSync(temporary, file)
  } catch (error) {
    try {
      unlinkSync(temporary)
    } catch {
      // The temporary file may never have been created.
    }
    throw new Error(
      `refusing to spawn task ${taskId}: the trusted user-level ${MCP_CONFIG_FILE} could not be restored (${describeFsError(error)}, WI#848)`,
    )
  }
  console.warn(
    `[kimi executor] restored the trusted user-level ${MCP_CONFIG_FILE} before task ${taskId}; the on-disk file differed from the entrypoint's registration (WI#848)`,
  )
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

function llmHeadersTimeoutMs(): number {
  return boundedEnvInteger("KIMI_CODE_LLM_HEADERS_TIMEOUT_MS", 900_000, 1_000, 86_400_000)
}

class LlmRequestTracker {
  private index = 1
  private startedAt = Date.now()
  private awaiting = true
  private pendingTools = new Set<string>()

  snapshot(): { index: number; elapsedSeconds: number; awaiting: boolean } {
    return {
      index: this.index,
      elapsedSeconds: Math.floor((Date.now() - this.startedAt) / 1_000),
      awaiting: this.awaiting,
    }
  }

  complete(toolCallIds: readonly string[]): void {
    this.awaiting = false
    this.startedAt = Date.now()
    this.pendingTools = new Set(toolCallIds)
  }

  retry(): void {
    if (this.awaiting) return
    this.awaiting = true
    this.startedAt = Date.now()
  }

  finishTool(toolCallId: string | undefined): void {
    if (toolCallId !== undefined) this.pendingTools.delete(toolCallId)
    if (this.awaiting || this.pendingTools.size > 0) return
    this.index += 1
    this.startedAt = Date.now()
    this.awaiting = true
  }
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
  next_attempt?: number
  max_attempts?: number
  delay_ms?: number
  error_name?: string
  status_code?: number
}

function optionalNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined
}

function retryProgress(line: StreamJsonLine, requestIndex: number): string {
  const next = line.next_attempt ?? "?"
  const max = line.max_attempts ?? "?"
  const delay = line.delay_ms ?? "?"
  const error = line.error_name ? ` after ${truncateUtf8(line.error_name, 128)}` : ""
  const status = line.status_code === undefined ? "" : ` (HTTP ${line.status_code})`
  return `[worker] LLM request ${requestIndex} retry ${next}/${max} in ${delay}ms${error}${status}; task remains active.`
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
  const headersTimeoutMs = llmHeadersTimeoutMs()
  const requestTracker = new LlmRequestTracker()

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
          const current = requestTracker.snapshot()
          const phase = current.awaiting ? "response" : "tool results"
          emitProgress(
            `[worker] Still waiting for Kimi Code ${phase}; request ${current.index} elapsed ${current.elapsedSeconds}s; task remains active.`,
          )
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
    // WI#833 point 2: the 2.7.2 dist still references a remote-control relay
    // URL, and no relay decision is on record for this lane. It is stripped
    // from every source (process.env spread, task env, extra proxy env) the
    // same way the trust switch is, so no spelling of it reaches the CLI. A
    // task-supplied value is reported — the name is new enough that seeing it
    // in a run is worth knowing about.
    if (Object.hasOwn(request.context?.environment ?? {}, "KIMI_CODE_REMOTE_CONTROL_RELAY_URL")) {
      console.warn(
        `[kimi executor] dropped task-supplied env entry KIMI_CODE_REMOTE_CONTROL_RELAY_URL for task ${request.task_id}: no remote-control relay is configured for this lane (WI#833)`,
      )
    }
    delete env.KIMI_CODE_REMOTE_CONTROL_RELAY_URL
    // WI#833 point 2: the names above were scrubbed one at a time, so every
    // new KIMI_CODE_* name upstream adds reached the CLI from task input
    // unless someone remembered to strip it. Now the direction is inverted:
    // a task-supplied KIMI_CODE_* name is dropped unless it is on this
    // allowlist, and an unknown name is dropped by default. The entries are
    // informational image metadata; the CLI reads neither.
    for (const key of Object.keys(request.context?.environment ?? {})) {
      if (!key.startsWith("KIMI_CODE_") || TASK_SETTABLE_KIMI_CODE_ENV.includes(key)) continue
      if (HANDLED_KIMI_CODE_ENV.has(key)) continue
      delete env[key]
      console.warn(
        `[kimi executor] dropped task-supplied env entry ${key} for task ${request.task_id}: not on the task-settable KIMI_CODE_* allowlist (WI#833)`,
      )
    }
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
    } else {
      delete env.KIMI_CODE_HOME
    }
    // Auto-update and telemetry: forced, not defaulted (WI#833 point 3). The
    // `||` form let a task-supplied "0"/"false" survive, so a task could turn
    // both back on; the assignment is operator policy, so it wins.
    env.KIMI_CODE_NO_AUTO_UPDATE = "1"
    env.KIMI_DISABLE_TELEMETRY = "1"
    // Operator-owned request deadline: task-supplied env cannot extend it.
    env.KIMI_CODE_LLM_HEADERS_TIMEOUT_MS = String(headersTimeoutMs)

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

    // WI#848: the CLI reads the trusted user-level mcp.json at session start.
    // Put the entrypoint's registration back first, so a task that rewrote it
    // (for this or a later session) cannot plant an MCP server the Bash hook
    // never sees. Throws on a failed restore — fail closed rather than spawn.
    restoreTrustedMcpConfig(request.task_id)

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
          next_attempt: optionalNumber(value.next_attempt),
          max_attempts: optionalNumber(value.max_attempts),
          delay_ms: optionalNumber(value.delay_ms),
          error_name: typeof value.error_name === "string" ? value.error_name : undefined,
          status_code: optionalNumber(value.status_code),
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
          const toolCallIds = (parsed.tool_calls ?? []).flatMap((call) =>
            typeof call.id === "string" ? [call.id] : [])
          requestTracker.complete(toolCallIds)
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
          requestTracker.finishTool(parsed.tool_call_id)
          const toolOutput = typeof parsed.content === "string" ? parsed.content.trim() : ""
          if (toolOutput) {
            const delta = `[tool result] ${toolOutput}`
            appendOutput(delta + "\n")
            emitProgress(delta)
          }
        } else if (isMetaMessage(parsed)) {
          if (parsed.type === "turn.step.retrying") {
            requestTracker.retry()
            emitProgress(retryProgress(parsed, requestTracker.snapshot().index))
          }
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
