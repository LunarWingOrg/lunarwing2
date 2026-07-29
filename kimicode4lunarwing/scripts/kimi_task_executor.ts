/**
 * kimi_task_executor.ts — Executes coding tasks via the Kimi Code CLI.
 *
 * Spawns `kimi -p <prompt> --output-format stream-json` as a child process,
 * parses the JSONL event stream, and maps it to LunarWing protocol
 * task_progress / task_result callbacks.
 */

import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process"
import { resolve } from "node:path"
import { EventEmitter } from "node:events"

import {
  buildTaskPrompt,
  type TaskRequest,
  type TaskProgress,
  type TaskResult,
  DEFAULT_TIMEOUT_MS,
} from "./lunarwing_runtime.js"
import { extractAssistantText, kimiErrorMessage, type KimiMessage } from "./kimi_result.js"
import { expandWorkspacePath } from "./workspace_path.js"

// Read env lazily: binding these at module load freezes the values at import
// time, which breaks per-task overrides (and tests that set env after import).
function kimiBin(): string {
  return process.env.KIMI_BIN || "kimi"
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
  return boundedEnvInteger("KIMI_PROGRESS_HEARTBEAT_MS", 60_000, 0, 300_000)
}

function killGraceMs(): number {
  return boundedEnvInteger("KIMI_KILL_GRACE_MS", 5_000, 100, 30_000)
}

type StopReason = "timeout" | "cancelled"

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
): Promise<void> {
  const startTime = Date.now()
  const timeoutMs = request.timeout_ms ?? DEFAULT_TIMEOUT_MS
  const heartbeatMs = progressHeartbeatMs()
  const killGrace = killGraceMs()
  const workDir = request.context?.project_dir
    ? resolveWorkDir(request.context.project_dir)
    : workspaceRoot()

  const injectedKeys: string[] = []
  const savedValues: Record<string, string | undefined> = {}
  if (request.context?.environment) {
    for (const [key, value] of Object.entries(request.context.environment)) {
      injectedKeys.push(key)
      savedValues[key] = process.env[key]
      process.env[key] = value
    }
  }

  let output = ""
  const collectedMessages: KimiMessage[] = []
  let stopReason: StopReason | undefined
  let proc: ChildProcessWithoutNullStreams | undefined
  let stderrBuffer = ""
  let lastProgressAt = Date.now()

  const emitProgress = (delta: string) => {
    lastProgressAt = Date.now()
    onProgress({ task_id: request.task_id, delta, done: false })
  }

  const progressHeartbeat =
    heartbeatMs > 0
      ? setInterval(() => {
          if (Date.now() - lastProgressAt < heartbeatMs) return
          emitProgress("[worker] Still waiting for Kimi Code response; task remains active.")
        }, heartbeatMs)
      : undefined

  const timeout = setTimeout(() => {
    stopReason = "timeout"
    killProc()
  }, timeoutMs)

  const handleAbort = () => {
    if (stopReason) return
    stopReason = "cancelled"
    killProc()
  }
  abortSignal?.addEventListener("abort", handleAbort, { once: true })
  if (abortSignal?.aborted) handleAbort()

  function killProc() {
    if (!proc || proc.killed) return
    // SIGTERM first for graceful shutdown (kimi drains stdio + closes sessions).
    proc.kill("SIGTERM")
    // Force-kill after a grace period so a wedged process can't hang the task.
    setTimeout(() => {
      if (proc && !proc.killed) {
        proc.kill("SIGKILL")
      }
    }, killGrace)
  }

  try {
    const promptText = buildTaskPrompt(request)
    const args = ["-p", promptText, "--output-format", "stream-json", "--yolo"]
    const env: NodeJS.ProcessEnv = { ...process.env }
    const kimiHome = kimiCodeHome()
    if (kimiHome) {
      env.KIMI_CODE_HOME = kimiHome
    }
    // Auto-update and telemetry: disable to keep CI/headless runs deterministic.
    env.KIMI_CODE_NO_AUTO_UPDATE = env.KIMI_CODE_NO_AUTO_UPDATE || "1"
    env.KIMI_DISABLE_TELEMETRY = env.KIMI_DISABLE_TELEMETRY || "1"

    proc = spawn(kimiBin(), args, {
      cwd: workDir,
      env,
      stdio: ["pipe", "pipe", "pipe"],
    })

    await new Promise<void>((resolveTask, rejectTask) => {
      let stdoutBuffer = ""

      proc!.stdout.on("data", (chunk: Buffer) => {
        stdoutBuffer += chunk.toString("utf-8")
        let newlineIndex: number
        while ((newlineIndex = stdoutBuffer.indexOf("\n")) >= 0) {
          const line = stdoutBuffer.slice(0, newlineIndex).trim()
          stdoutBuffer = stdoutBuffer.slice(newlineIndex + 1)
          if (line.length === 0) continue
          processStreamLine(line)
        }
      })

      proc!.stderr.on("data", (chunk: Buffer) => {
        stderrBuffer += chunk.toString("utf-8")
      })

      proc!.on("error", (err) => {
        if (stopReason) return
        rejectTask(err)
      })

      proc!.on("close", (code) => {
        if (stopReason) {
          rejectTask(new TaskStoppedError(stopReason))
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
        let parsed: StreamJsonLine
        try {
          parsed = JSON.parse(line)
        } catch {
          return
        }

        // Collect for final output extraction.
        const msg: KimiMessage = {
          role: parsed.role ?? "",
          content: typeof parsed.content === "string" ? parsed.content : undefined,
          tool_calls: parsed.tool_calls,
          tool_call_id: parsed.tool_call_id,
        }
        collectedMessages.push(msg)

        if (isAssistantMessage(parsed)) {
          const text = typeof parsed.content === "string" ? parsed.content.trim() : ""
          if (text) {
            output += text + "\n"
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
            output += delta + "\n"
            emitProgress(delta)
          }
        } else if (isMetaMessage(parsed)) {
          // Meta messages (resume hints, retry notices, version) go to stderr only.
          if (typeof parsed.type === "string") {
            console.error(`[kimi meta] ${parsed.type}`)
          }
        }
      }
    })

    // Process exited cleanly — assemble the result.
    const canonicalOutput = collectOutput(collectedMessages)
    const resultOutput = canonicalOutput || output.trim()
    const durationMs = Date.now() - startTime

    if (!resultOutput) {
      onResult({
        task_id: request.task_id,
        status: "error",
        output: "",
        error: "kimi completed without assistant text or tool output",
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
      onResult({
        task_id: request.task_id,
        status: stopped === "timeout" ? "timed_out" : "cancelled",
        output: output.trim(),
        error: stopped === "timeout" ? `Task timed out after ${timeoutMs}ms` : null,
        duration_ms: Date.now() - startTime,
      })
      return
    }
    const errorMsg = kimiErrorMessage(err)
    onResult({
      task_id: request.task_id,
      status: "error",
      output: output.trim(),
      error: errorMsg,
      duration_ms: durationMs,
    })
  } finally {
    clearTimeout(timeout)
    if (progressHeartbeat) clearInterval(progressHeartbeat)
    abortSignal?.removeEventListener("abort", handleAbort)
    if (proc && !proc.killed) {
      proc.kill("SIGKILL")
    }
    for (const key of injectedKeys) {
      if (savedValues[key] === undefined) {
        delete process.env[key]
      } else {
        process.env[key] = savedValues[key]
      }
    }
  }
}

function resolveWorkDir(path: string): string {
  return resolve(expandWorkspacePath(path, workspaceRoot()))
}
