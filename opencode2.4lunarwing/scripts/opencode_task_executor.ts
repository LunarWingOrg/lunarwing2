/**
 * opencode_task_executor.ts — Executes coding tasks via OpenCode 2.
 *
 * Uses the local opencode headless server (HTTP/SSE) through the v2 client
 * SDK (@opencode/client/promise) to create sessions, send prompts, and stream
 * events back to the caller.
 *
 * v2 differences from the v1 worker (see docs/plans/
 * OPENCODE2_EXTERNAL_WORKER_PLAN.md §4):
 *  - Client construction is OpenCode.make({ baseUrl, headers }) — no
 *    per-request `directory` option, so the work dir is selected by running
 *    the bridge under WORKSPACE_ROOT (the daemon bind-mounts the project dir
 *    at /workspace).
 *  - Permissions are {action, resource, effect} rulesets; the default agent
 *    allows "*"/"*" so the session-level deny rules below are what matters.
 *  - Prompt completion is observed via session.execution.{succeeded,failed,
 *    interrupted} / session.idle events (v1 used session.status idle).
 *  - Cancellation calls session.interrupt (v1 used session.abort).
 */

import { OpenCode, type OpenCodeClient } from "@opencode/client/promise"
import {
  buildTaskPrompt,
  type TaskRequest,
  type TaskProgress,
  type TaskResult,
  DEFAULT_TIMEOUT_MS,
} from "./lunarwing_runtime"
import { extractAssistantText, opencodeErrorMessage } from "./opencode_result"
import { resolveWorkspacePath } from "./workspace_path"

const OPENCODE_HOST = process.env.OPENCODE_SERVE_HOST || "127.0.0.1"
const OPENCODE_PORT = process.env.OPENCODE_SERVE_PORT || "4096"
const OPENCODE_BASE_URL = `http://${OPENCODE_HOST}:${OPENCODE_PORT}`
const WORKSPACE_ROOT = process.env.WORKSPACE_ROOT || "/workspace"
const RETRY_PROMPT =
  "The previous model request failed transiently. Continue this task from the current " +
  "workspace state. Do not repeat completed work. Inspect existing changes and proceed " +
  "from the next incomplete step."

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

function createSdk(): OpenCodeClient {
  const password = getPassword()
  const headers: Record<string, string> = {}
  if (password) {
    const username = process.env.OPENCODE_SERVER_USERNAME ?? "opencode"
    headers["Authorization"] = `Basic ${Buffer.from(`${username}:${password}`).toString("base64")}`
  }

  return OpenCode.make({
    baseUrl: OPENCODE_BASE_URL,
    headers: Object.keys(headers).length > 0 ? headers : undefined,
  })
}

export type ProgressCallback = (progress: TaskProgress) => void
export type ResultCallback = (result: TaskResult) => void

// v2 permission ruleset: allow the sandboxed tool surface, deny the
// interactive ones the worker cannot answer (mirrors the v1 worker policy).
// Evaluation is findLast over [agent rules, ...session rules], so the deny
// rules below must come AFTER the allow-* entries to win (WI#357: workers
// must never self-expose). The shell tool wildcard-matches each parsed
// command's full text against resource; the leading * covers absolute-path
// and sudo-prefixed invocations of the binary. This ruleset and the
// in-image wrapper are NAIVE-USE guards, not a boundary: both miss
// whitespace/quote-obfuscated command text, and the real binary stays
// directly executable by the agent uid — the boundary against a
// compromised agent is the egress allowlist (WI#357 control 1, deferred).
function sessionPermissions(): Array<{ action: string; resource: string; effect: "allow" | "deny" }> {
  return [
    { action: "edit", resource: "*", effect: "allow" },
    { action: "write", resource: "*", effect: "allow" },
    { action: "read", resource: "*", effect: "allow" },
    { action: "shell", resource: "*", effect: "allow" },
    { action: "glob", resource: "*", effect: "allow" },
    { action: "grep", resource: "*", effect: "allow" },
    { action: "patch", resource: "*", effect: "allow" },
    { action: "webfetch", resource: "*", effect: "allow" },
    { action: "question", resource: "*", effect: "deny" },
    { action: "subagent", resource: "*", effect: "deny" },
    { action: "shell", resource: "*opencode service *", effect: "deny" },
    { action: "shell", resource: "*opencode pair *", effect: "deny" },
    { action: "shell", resource: "*opencode serve *", effect: "deny" },
  ]
}

interface V2EventLike {
  type: string
  data?: Record<string, unknown>
}

function eventSessionID(event: V2EventLike): string | undefined {
  const data = event.data
  if (!data || typeof data !== "object") return undefined
  const value = (data as { sessionID?: unknown }).sessionID
  return typeof value === "string" ? value : undefined
}

function isPromptSettled(event: V2EventLike): boolean {
  return (
    event.type === "session.execution.succeeded" ||
    event.type === "session.execution.failed" ||
    event.type === "session.execution.interrupted" ||
    event.type === "session.idle"
  )
}

function eventErrorMessage(event: V2EventLike): string {
  const error = (event.data as { error?: unknown } | undefined)?.error
  return error ? opencodeErrorMessage(error) : ""
}

export async function executeTask(
  request: TaskRequest,
  onProgress: ProgressCallback,
  onResult: ResultCallback,
  abortSignal?: AbortSignal,
  sdkOverride?: OpenCodeClient,
): Promise<void> {
  const startTime = Date.now()
  const timeoutMs = request.timeout_ms ?? DEFAULT_TIMEOUT_MS
  const maxProviderRetries = providerMaxRetries()
  const retryDelayMs = providerRetryDelayMs()
  const heartbeatMs = progressHeartbeatMs()
  const abortTimeoutMs = sessionAbortTimeoutMs()

  const injectedKeys: string[] = []
  const savedValues: Record<string, string | undefined> = {}
  if (request.context?.environment) {
    for (const [key, value] of Object.entries(request.context.environment)) {
      injectedKeys.push(key)
      savedValues[key] = process.env[key]
      process.env[key] = value
    }
  }

  const sdk = sdkOverride ?? createSdk()
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

  const interruptSession = async () => {
    if (!sessionID) return
    const controller = new AbortController()
    let timer: ReturnType<typeof setTimeout> | undefined
    const outcome = await Promise.race([
      sdk.session
        .interrupt({ sessionID }, { signal: controller.signal })
        .then(
          () => ({ kind: "done" as const }),
          (error: unknown) => ({ kind: "error" as const, error }),
        ),
      new Promise<{ kind: "timeout" }>((resolve) => {
        timer = setTimeout(() => {
          controller.abort("session interrupt timed out")
          resolve({ kind: "timeout" })
        }, abortTimeoutMs)
      }),
    ])
    if (timer) clearTimeout(timer)
    if (outcome.kind === "error") {
      console.error("[opencode_task_executor] failed to interrupt stopped session:", outcome.error)
    } else if (outcome.kind === "timeout") {
      console.error(
        `[opencode_task_executor] session interrupt exceeded ${abortTimeoutMs}ms for task ${request.task_id}`,
      )
    }
  }

  try {
    const sessionResult = await waitForStop(
      sdk.session.create(
        {
          title: `lunarwing-opencode2-${request.task_id}`,
          permissions: sessionPermissions(),
        },
        { signal: taskController.signal },
      ),
    )

    sessionID = sessionResult.id
    if (!sessionID) {
      throw new Error("Failed to create opencode session")
    }

    const eventIterator = sdk.event
      .subscribe({ signal: taskController.signal })
      [Symbol.asyncIterator]()
    closeEvents = () => void eventIterator.return?.()
    let executionError = ""

    for (let attempt = 0; ; attempt++) {
      let sessionErrorMsg = ""
      const promptText = attempt === 0 ? buildTaskPrompt(request) : RETRY_PROMPT
      // Attach rejection handling immediately so a provider/API error cannot
      // become an unhandled rejection while the SSE loop is still draining.
      const promptPromise = sdk.session
        .prompt(
          {
            sessionID,
            text: promptText,
          },
          { signal: taskController.signal },
        )
        .then(
          (result) => ({ result }),
          (error: unknown) => ({ error }),
        )
      const promptFailure = promptPromise.then((outcome) => {
        if (!("error" in outcome)) return new Promise<never>(() => {})
        return { kind: "prompt_error" as const }
      })
      executionError = ""

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
        const event = next.result.value as V2EventLike

        if (event.type === "permission.asked") {
          if (eventSessionID(event) !== sessionID) continue
          // Reject anything not granted by the session allow-policy above.
          deniedPermissions++
          const requestID = (event.data as { id?: unknown } | undefined)?.id
          if (typeof requestID === "string") {
            await waitForStop(
              sdk.permission.reply(
                {
                  sessionID,
                  requestID,
                  decision: "reject",
                },
                { signal: taskController.signal },
              ),
            )
          }
          continue
        }

        if (event.type === "session.text.delta") {
          if (eventSessionID(event) !== sessionID) continue
          const delta = (event.data as { delta?: unknown } | undefined)?.delta
          if (typeof delta === "string" && delta) {
            output += delta
            emitProgress(delta)
          }
          continue
        }

        if (event.type === "session.tool.success" || event.type === "session.tool.failed") {
          if (eventSessionID(event) !== sessionID) continue
          const toolID = (event.data as { id?: unknown } | undefined)?.id
          const toolName =
            typeof toolID === "string" && toolID.includes("/") ? toolID.split("/").pop() : toolID
          if (event.type === "session.tool.failed") {
            const delta = `[${toolName ?? "tool"}] ERROR: ${eventErrorMessage(event)}`
            output += delta + "\n"
            emitProgress(delta)
          } else {
            const delta = `[${toolName ?? "tool"}] completed`
            output += delta + "\n"
            emitProgress(delta)
          }
          continue
        }

        if (event.type === "session.execution.failed") {
          if (eventSessionID(event) !== sessionID) continue
          executionError = eventErrorMessage(event)
        }

        if (isPromptSettled(event)) {
          if (eventSessionID(event) !== sessionID) continue
          break
        }
      }

      if (stopReason) throw new TaskStoppedError(stopReason)

      const promptOutcome = await waitForStop(promptPromise)
      const promptError =
        ("error" in promptOutcome ? opencodeErrorMessage(promptOutcome.error) : "") ||
        executionError ||
        sessionErrorMsg

      if (!promptError) {
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

    const canonicalOutput = await fetchAssistantText(sdk, sessionID)
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
          `configuration likely needs to match the opencode v2 SDK — smoke-verify).`,
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
      await interruptSession()
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

// After execution settles, pull the session's assistant messages for the
// canonical output. v2 keeps final assistant text in session messages rather
// than the prompt response (the prompt API returns an inbox receipt).
//
// The v2 promise client lists session messages at the TOP level —
// client.message.list(input) calls GET /api/session/:sessionID/message and
// returns a paginated { data, cursor } envelope. There is NO
// session.message.list (session.message only has get); the previous call path
// threw TypeError at runtime (WI#320 finding F7). Permission replies likewise
// live at client.permission.reply, not session.permission.reply.
//
// Paging follows the server contract (packages/server/src/handlers/message.ts):
// `order` applies to the FIRST page only — a request carrying both `cursor`
// and `order` is rejected with 400 InvalidCursorError — and `cursor.next` is
// returned for every non-empty page, so a short page is the end signal. The
// first page asks for newest-first, so the MESSAGE_LIST_MAX_PAGES bound keeps
// the newest assistant messages (the final answer), and the collected pages
// are reversed back into chronological order. A failure on a later page keeps
// the pages already fetched.
const MESSAGE_LIST_PAGE_SIZE = 200 // upstream maximum per page
const MESSAGE_LIST_MAX_PAGES = 5

async function fetchAssistantText(sdk: OpenCodeClient, sessionID: string): Promise<string> {
  const newestFirst: Array<{ type?: unknown; content?: unknown }> = []
  let cursor: string | undefined
  try {
    for (let page = 0; page < MESSAGE_LIST_MAX_PAGES; page++) {
      const response = await sdk.message.list(
        cursor === undefined
          ? { sessionID, type: "assistant", limit: MESSAGE_LIST_PAGE_SIZE, order: "desc" }
          : { sessionID, type: "assistant", limit: MESSAGE_LIST_PAGE_SIZE, cursor },
        { signal: AbortSignal.timeout(10_000) },
      )
      if (!response || !Array.isArray(response.data)) {
        console.warn("[opencode_task_executor] message.list returned an unexpected response shape")
        break
      }
      newestFirst.push(...response.data)
      if (response.data.length < MESSAGE_LIST_PAGE_SIZE) break
      const next = response.cursor?.next
      if (typeof next !== "string" || next.length === 0) break
      cursor = next
    }
  } catch (err) {
    console.warn("[opencode_task_executor] failed to list session messages:", err)
  }
  return newestFirst
    .reverse()
    .filter((message) => message?.type === "assistant")
    .flatMap((message) => (Array.isArray(message.content) ? message.content : []))
    .filter(
      (part) =>
        part?.type === "text" &&
        typeof (part as { text?: unknown }).text === "string" &&
        ((part as { text: string }).text ?? "").trim().length > 0,
    )
    .map((part) => ((part as { text: string }).text ?? "").trim())
    .join("\n")
}

export function resolveWorkDir(path: string): string {
  return resolveWorkspacePath(path, WORKSPACE_ROOT)
}
