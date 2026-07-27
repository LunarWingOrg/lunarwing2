/**
 * opencode_task_executor.ts — Executes coding tasks via the opencode SDK.
 *
 * Uses the local opencode headless server (HTTP/SSE) to create sessions,
 * send prompts, and stream events back to the caller.
 */

import { createOpencodeClient, type OpencodeClient } from "@opencode-ai/sdk/v2"
import {
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

function getPassword(): string | undefined {
  return process.env.OPENCODE_SERVER_PASSWORD
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

function buildPrompt(request: TaskRequest): string {
  const parts: string[] = []
  const ctx = request.context

  if (ctx?.conversation_history && ctx.conversation_history.length > 0) {
    parts.push("--- Conversation history ---")
    for (const msg of ctx.conversation_history) {
      parts.push(`${msg.role}: ${msg.content}`)
    }
    parts.push("--- End conversation history ---\n")
  }

  if (ctx?.metadata && Object.keys(ctx.metadata).length > 0) {
    parts.push("--- Task metadata ---")
    for (const [key, value] of Object.entries(ctx.metadata)) {
      parts.push(`${key}: ${value}`)
    }
    parts.push("--- End task metadata ---\n")
  }

  parts.push(request.prompt)
  return parts.join("\n")
}

export async function executeTask(
  request: TaskRequest,
  onProgress: ProgressCallback,
  onResult: ResultCallback,
  abortSignal?: AbortSignal,
  sdkOverride?: OpencodeClient,
): Promise<void> {
  const startTime = Date.now()
  const timeoutMs = request.timeout_ms ?? DEFAULT_TIMEOUT_MS
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
  let output = ""
  let sawSessionError = false
  let sessionErrorMsg = ""
  let deniedPermissions = 0

  type StopReason = "timeout" | "cancelled"
  let resolveStop: (reason: StopReason) => void = () => {}
  const stopPromise = new Promise<StopReason>((resolve) => {
    resolveStop = resolve
  })
  const timeout = setTimeout(() => resolveStop("timeout"), timeoutMs)
  const handleAbort = () => resolveStop("cancelled")
  abortSignal?.addEventListener("abort", handleAbort, { once: true })
  if (abortSignal?.aborted) handleAbort()

  try {
    const sessionResult = await sdk.session.create(
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
      { throwOnError: true },
    )

    sessionID = sessionResult.data?.id
    if (!sessionID) {
      throw new Error("Failed to create opencode session")
    }

    const events = await sdk.event.subscribe()

    // Attach rejection handling immediately so a provider/API error cannot
    // become an unhandled rejection while the SSE loop is still draining.
    const promptText = buildPrompt(request)
  const promptPromise = sdk.session
      .prompt(
        {
          sessionID,
          parts: [{ type: "text", text: promptText }],
        },
        { throwOnError: true },
      )
      .then(
        (result) => ({ result }),
        (error: unknown) => ({ error }),
      )

    const eventIterator = events.stream[Symbol.asyncIterator]()
    let stopReason: StopReason | undefined

    while (!stopReason) {
      const next = await Promise.race([
        stopPromise.then((reason) => ({ kind: "stop" as const, reason })),
        eventIterator.next().then((result) => ({ kind: "event" as const, result })),
      ])
      if (next.kind === "stop") {
        stopReason = next.reason
        break
      }
      if (next.result.done) break
      const event = next.result.value

      if (event.type === "message.part.updated") {
        const part = event.properties.part
        if (part.sessionID !== sessionID) continue

        if (part.type === "text" && part.time?.end) {
          const text = part.text?.trim()
          if (text) {
            output += text + "\n"
            onProgress({
              task_id: request.task_id,
              delta: text,
              done: false,
            })
          }
        }

        if (part.type === "tool" && part.state?.status === "completed") {
          const toolOutput = part.state.output?.trim()
          const toolName = part.tool || "unknown"
          const delta = `[${toolName}] ${toolOutput || "completed"}`
          output += delta + "\n"
          onProgress({
            task_id: request.task_id,
            delta,
            done: false,
          })
        }

        if (part.type === "tool" && part.state?.status === "error") {
          const toolName = part.tool || "unknown"
          const delta = `[${toolName}] ERROR: ${part.state.error}`
          output += delta + "\n"
          onProgress({
            task_id: request.task_id,
            delta,
            done: false,
          })
        }
      }

      if (event.type === "session.error") {
        if (event.properties.sessionID !== sessionID) continue
        const err = event.properties.error
        const errMsg = opencodeErrorMessage(err)
        output += `ERROR: ${errMsg}\n`
        sawSessionError = true
        if (!sessionErrorMsg) sessionErrorMsg = errMsg
      }

      if (event.type === "permission.asked") {
        if (event.properties.sessionID !== sessionID) continue
        // This rejects every runtime permission prompt — a safety net for
        // anything NOT already granted by the session.create allow-policy above.
        // If that policy's shape is wrong (unverified vs the real opencode SDK —
        // see DEFERRED-2026-07-02-OPENCODE-EXTERNAL-WORKER.md), every action
        // falls through to here and is denied, so the task does nothing. Count
        // denials so we fail loudly below instead of reporting a false success.
        deniedPermissions++
        await sdk.permission.reply(
          {
            requestID: event.properties.id,
            reply: "reject",
          },
          { throwOnError: true },
        )
      }

      if (event.type === "permission.v2.asked") {
        if (event.properties.sessionID !== sessionID) continue
        deniedPermissions++
        await sdk.v2.session.permission.reply(
          {
            sessionID,
            requestID: event.properties.id,
            reply: "reject",
          },
          { throwOnError: true },
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

    if (stopReason) {
      try {
        await sdk.session.abort({ sessionID }, { throwOnError: true })
      } catch (error) {
        console.error("[opencode_task_executor] failed to abort stopped session:", error)
      }
      // Do not await return(): a silent SSE iterator may still be blocked in next().
      void eventIterator.return?.()

      const durationMs = Date.now() - startTime
      onResult({
        task_id: request.task_id,
        status: stopReason === "timeout" ? "error" : "cancelled",
        output: output.trim(),
        error: stopReason === "timeout" ? `Task timed out after ${timeoutMs}ms` : null,
        duration_ms: durationMs,
      })
      return
    }

    const promptOutcome = await promptPromise
    if ("error" in promptOutcome) throw promptOutcome.error

    const promptData = promptOutcome.result.data
    const assistantError = promptData?.info.error
    if (assistantError) {
      throw new Error(opencodeErrorMessage(assistantError))
    }

    const canonicalOutput = extractAssistantText(promptData?.parts)
    const resultOutput = canonicalOutput || output.trim()
    const durationMs = Date.now() - startTime

    // Honest result: a session error must not be reported as success.
    if (sawSessionError) {
      onResult({
        task_id: request.task_id,
        status: "error",
        output: output.trim(),
        error: sessionErrorMsg || "opencode session reported an error",
        duration_ms: durationMs,
      })
      return
    }

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
    abortSignal?.removeEventListener("abort", handleAbort)
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
