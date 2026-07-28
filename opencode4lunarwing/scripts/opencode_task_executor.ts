/**
 * opencode_task_executor.ts — Executes coding tasks via the opencode SDK.
 *
 * Uses the local opencode headless server (HTTP/SSE) to create sessions,
 * send prompts, and stream events back to the caller.
 */

import { createOpencodeClient, type OpencodeClient } from "@opencode-ai/sdk/v2"
import {
  buildTaskPrompt,
  type TaskRequest,
  type TaskProgress,
  type TaskResult,
  DEFAULT_TIMEOUT_MS,
} from "./lunarwing_runtime"
import { extractAssistantText, opencodeErrorMessage } from "./opencode_result"
import { expandWorkspacePath } from "./workspace_path"

const OPENCODE_HOST = process.env.OPENCODE_SERVE_HOST || "127.0.0.1"
const OPENCODE_PORT = process.env.OPENCODE_SERVE_PORT || "4096"
const OPENCODE_BASE_URL = `http://${OPENCODE_HOST}:${OPENCODE_PORT}`
const WORKSPACE_ROOT = process.env.WORKSPACE_ROOT || "/workspace"
const RETRY_PROMPT =
  "The previous model request failed transiently. Continue this task from the current session and " +
  "workspace state. Do not repeat completed work. Inspect existing changes and proceed from " +
  "the next incomplete step."

type StopReason = "timeout" | "cancelled"

class TaskStoppedError extends Error {
  constructor(readonly reason: StopReason) {
    super(`Task ${reason}`)
  }
}

function getPassword(): string | undefined {
  return process.env.OPENCODE_SERVER_PASSWORD
}

function boundedEnvInteger(name: string, fallback: number, min: number, max: number): number {
  const parsed = Number.parseInt(process.env[name] ?? "", 10)
  if (!Number.isFinite(parsed)) return fallback
  return Math.min(max, Math.max(min, parsed))
}

function providerMaxRetries(): number {
  return boundedEnvInteger("OPENCODE_PROVIDER_MAX_RETRIES", 2, 0, 5)
}

function providerRetryDelayMs(): number {
  return boundedEnvInteger("OPENCODE_PROVIDER_RETRY_DELAY_MS", 5_000, 0, 60_000)
}

function progressHeartbeatMs(): number {
  return boundedEnvInteger("OPENCODE_PROGRESS_HEARTBEAT_MS", 60_000, 0, 300_000)
}

function sessionAbortTimeoutMs(): number {
  return boundedEnvInteger("OPENCODE_SESSION_ABORT_TIMEOUT_MS", 5_000, 100, 30_000)
}

function isRetryableProviderError(message: string): boolean {
  return (
    /streaming\.total_ms/i.test(message) ||
    /\bprovider\b.*\btimed?\s*out\b/i.test(message) ||
    /\bupstream\b.*\btimed?\s*out\b/i.test(message) ||
    /\bno provider available\b/i.test(message)
  )
}

function createSdk(directory?: string): OpencodeClient {
  const password = getPassword()
  const headers: Record<string, string> = {}
  if (password) {
    const username = process.env.OPENCODE_SERVER_USERNAME ?? "opencode"
    headers["Authorization"] = `Basic ${Buffer.from(`${username}:${password}`).toString("base64")}`
  }

  return createOpencodeClient({
    baseUrl: OPENCODE_BASE_URL,
    directory: directory || WORKSPACE_ROOT,
    headers: Object.keys(headers).length > 0 ? headers : undefined,
  })
}

export type ProgressCallback = (progress: TaskProgress) => void
export type ResultCallback = (result: TaskResult) => void

export async function executeTask(
  request: TaskRequest,
  onProgress: ProgressCallback,
  onResult: ResultCallback,
  abortSignal?: AbortSignal,
  sdkOverride?: OpencodeClient,
): Promise<void> {
  const startTime = Date.now()
  const timeoutMs = request.timeout_ms ?? DEFAULT_TIMEOUT_MS
  const maxProviderRetries = providerMaxRetries()
  const retryDelayMs = providerRetryDelayMs()
  const heartbeatMs = progressHeartbeatMs()
  const abortTimeoutMs = sessionAbortTimeoutMs()
  const workDir = request.context?.project_dir
    ? resolveWorkDir(request.context.project_dir)
    : WORKSPACE_ROOT

  const injectedKeys: string[] = []
  const savedValues: Record<string, string | undefined> = {}
  if (request.context?.environment) {
    for (const [key, value] of Object.entries(request.context.environment)) {
      injectedKeys.push(key)
      savedValues[key] = process.env[key]
      process.env[key] = value
    }
  }

  const sdk = sdkOverride ?? createSdk(workDir)
  let sessionID: string | undefined
  let closeEvents: (() => void) | undefined
  let output = ""
  let deniedPermissions = 0
  let lastProgressAt = Date.now()
  const emitProgress = (delta: string) => {
    lastProgressAt = Date.now()
    onProgress({ task_id: request.task_id, delta, done: false })
  }
  const progressHeartbeat =
    heartbeatMs > 0
      ? setInterval(() => {
          if (Date.now() - lastProgressAt < heartbeatMs) return
          emitProgress("[worker] Still waiting for OpenCode/model response; task remains active.")
        }, heartbeatMs)
      : undefined

  let stopReason: StopReason | undefined
  let resolveStop: (reason: StopReason) => void = () => {}
  const stopPromise = new Promise<StopReason>((resolve) => {
    resolveStop = resolve
  })
  const taskController = new AbortController()
  const requestStop = (reason: StopReason) => {
    if (stopReason) return
    stopReason = reason
    resolveStop(reason)
    taskController.abort(reason)
  }
  const timeout = setTimeout(() => requestStop("timeout"), timeoutMs)
  const handleAbort = () => requestStop("cancelled")
  abortSignal?.addEventListener("abort", handleAbort, { once: true })
  if (abortSignal?.aborted) handleAbort()

  const waitForStop = async <T>(operation: Promise<T>): Promise<T> => {
    const outcome = await Promise.race([
      operation.then(
        (value) => ({ kind: "value" as const, value }),
        (error: unknown) => ({ kind: "error" as const, error }),
      ),
      stopPromise.then((reason) => ({ kind: "stop" as const, reason })),
    ])
    if (outcome.kind === "stop") throw new TaskStoppedError(outcome.reason)
    if (outcome.kind === "error") throw outcome.error
    return outcome.value
  }

  const abortSession = async () => {
    if (!sessionID) return
    const controller = new AbortController()
    let timer: ReturnType<typeof setTimeout> | undefined
    const outcome = await Promise.race([
      sdk.session
        .abort(
          { sessionID },
          { throwOnError: true, signal: controller.signal },
        )
        .then(
          () => ({ kind: "done" as const }),
          (error: unknown) => ({ kind: "error" as const, error }),
        ),
      new Promise<{ kind: "timeout" }>((resolve) => {
        timer = setTimeout(() => {
          controller.abort("session abort timed out")
          resolve({ kind: "timeout" })
        }, abortTimeoutMs)
      }),
    ])
    if (timer) clearTimeout(timer)
    if (outcome.kind === "error") {
      console.error("[opencode_task_executor] failed to abort stopped session:", outcome.error)
    } else if (outcome.kind === "timeout") {
      console.error(
        `[opencode_task_executor] session abort exceeded ${abortTimeoutMs}ms for task ${request.task_id}`,
      )
    }
  }

  try {
    const sessionResult = await waitForStop(
      sdk.session.create(
        {
          title: `lunarwing-opencode-${request.task_id}`,
          permission: [
            { permission: "edit", action: "allow", pattern: "*" },
            { permission: "bash", action: "allow", pattern: "*" },
            { permission: "read", action: "allow", pattern: "*" },
            { permission: "write", action: "allow", pattern: "*" },
            { permission: "question", action: "deny", pattern: "*" },
            { permission: "plan_enter", action: "deny", pattern: "*" },
            { permission: "plan_exit", action: "deny", pattern: "*" },
          ],
        },
        { throwOnError: true, signal: taskController.signal },
      ),
    )

    sessionID = sessionResult.data?.id
    if (!sessionID) {
      throw new Error("Failed to create opencode session")
    }

    const events = await waitForStop(
      sdk.event.subscribe(undefined, { signal: taskController.signal }),
    )
    const eventIterator = events.stream[Symbol.asyncIterator]()
    closeEvents = () => void eventIterator.return?.()
    let finalPromptData: Awaited<ReturnType<typeof sdk.session.prompt>>["data"] | undefined

    for (let attempt = 0; ; attempt++) {
      let sessionErrorMsg = ""
      const promptText = attempt === 0 ? buildTaskPrompt(request) : RETRY_PROMPT
      // Attach rejection handling immediately so a provider/API error cannot
      // become an unhandled rejection while the SSE loop is still draining.
      const promptPromise = sdk.session
        .prompt(
          {
            sessionID,
            parts: [{ type: "text", text: promptText }],
          },
          { throwOnError: true, signal: taskController.signal },
        )
        .then(
          (result) => ({ result }),
          (error: unknown) => ({ error }),
        )
      const promptFailure = promptPromise.then((outcome) => {
        if (!("error" in outcome)) return new Promise<never>(() => {})
        return { kind: "prompt_error" as const }
      })

      while (!stopReason) {
        const next = await Promise.race([
          stopPromise.then((reason) => ({ kind: "stop" as const, reason })),
          promptFailure,
          eventIterator.next().then((result) => ({ kind: "event" as const, result })),
        ])
        if (next.kind === "stop") {
          stopReason = next.reason
          break
        }
        if (next.kind === "prompt_error") break
        if (next.result.done) break
        const event = next.result.value

        if (event.type === "message.part.updated") {
          const part = event.properties.part
          if (part.sessionID !== sessionID) continue

          if (part.type === "text" && part.time?.end) {
            const text = part.text?.trim()
            if (text) {
              output += text + "\n"
              emitProgress(text)
            }
          }

          if (part.type === "tool" && part.state?.status === "completed") {
            const toolOutput = part.state.output?.trim()
            const toolName = part.tool || "unknown"
            const delta = `[${toolName}] ${toolOutput || "completed"}`
            output += delta + "\n"
            emitProgress(delta)
          }

          if (part.type === "tool" && part.state?.status === "error") {
            const toolName = part.tool || "unknown"
            const delta = `[${toolName}] ERROR: ${part.state.error}`
            output += delta + "\n"
            emitProgress(delta)
          }
        }

        if (event.type === "session.error") {
          if (event.properties.sessionID !== sessionID) continue
          if (!sessionErrorMsg) sessionErrorMsg = opencodeErrorMessage(event.properties.error)
        }

        if (event.type === "permission.asked") {
          if (event.properties.sessionID !== sessionID) continue
          // Reject anything not granted by the session allow-policy above.
          deniedPermissions++
          await waitForStop(
            sdk.permission.reply(
              {
                requestID: event.properties.id,
                reply: "reject",
              },
              { throwOnError: true, signal: taskController.signal },
            ),
          )
        }

        if (event.type === "permission.v2.asked") {
          if (event.properties.sessionID !== sessionID) continue
          deniedPermissions++
          await waitForStop(
            sdk.v2.session.permission.reply(
              {
                sessionID,
                requestID: event.properties.id,
                reply: "reject",
              },
              { throwOnError: true, signal: taskController.signal },
            ),
          )
        }

        if (
          (event.type === "session.status" &&
            event.properties.sessionID === sessionID &&
            event.properties.status?.type === "idle") ||
          (event.type === "session.idle" && event.properties.sessionID === sessionID)
        ) {
          break
        }
      }

      if (stopReason) throw new TaskStoppedError(stopReason)

      const promptOutcome = await waitForStop(promptPromise)
      const promptData = "result" in promptOutcome ? promptOutcome.result.data : undefined
      const promptError =
        ("error" in promptOutcome ? opencodeErrorMessage(promptOutcome.error) : "") ||
        (promptData?.info.error ? opencodeErrorMessage(promptData.info.error) : "") ||
        sessionErrorMsg

      if (!promptError) {
        finalPromptData = promptData
        break
      }

      if (!isRetryableProviderError(promptError) || attempt >= maxProviderRetries) {
        throw new Error(promptError)
      }

      const retryNumber = attempt + 1
      const delta =
        `[worker] Transient provider failure; retrying same OpenCode session ` +
        `(${retryNumber}/${maxProviderRetries}).`
      output += `ERROR: ${promptError}\n${delta}\n`
      emitProgress(delta)
      console.warn(`[opencode_task_executor] task ${request.task_id}: ${delta} ${promptError}`)

      const delayResult = await Promise.race([
        new Promise<undefined>((resolve) => setTimeout(resolve, retryDelayMs)),
        stopPromise,
      ])
      if (delayResult) {
        throw new TaskStoppedError(delayResult)
      }
    }

    const canonicalOutput = extractAssistantText(finalPromptData?.parts)
    const resultOutput = canonicalOutput || output.trim()
    const durationMs = Date.now() - startTime

    // If every permission was denied and the task produced no output, it did
    // nothing — report that instead of a silent "success".
    if (deniedPermissions > 0 && !resultOutput) {
      onResult({
        task_id: request.task_id,
        status: "error",
        output: resultOutput,
        error:
          `Task performed no actions: ${deniedPermissions} permission request(s) ` +
          `were denied by the worker permission policy (the session permission ` +
          `configuration likely needs to match the opencode SDK — smoke-verify).`,
        duration_ms: durationMs,
      })
      return
    }

    if (!resultOutput) {
      onResult({
        task_id: request.task_id,
        status: "error",
        output: "",
        error: "opencode completed without assistant text or tool output",
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
      closeEvents?.()
      await abortSession()
      onResult({
        task_id: request.task_id,
        status: stopped === "timeout" ? "timed_out" : "cancelled",
        output: output.trim(),
        error: stopped === "timeout" ? `Task timed out after ${timeoutMs}ms` : null,
        duration_ms: Date.now() - startTime,
      })
      return
    }
    const errorMsg = opencodeErrorMessage(err)
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
    closeEvents?.()
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
  return expandWorkspacePath(path, WORKSPACE_ROOT)
}
