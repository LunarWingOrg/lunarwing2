/**
 * opencode_task_executor.ts — Executes coding tasks via the opencode SDK.
 *
 * Uses the local opencode headless server (HTTP/SSE) to create sessions,
 * send prompts, and stream events back to the caller.
 */

import { createOpencodeClient, type OpencodeClient } from "@opencode-ai/sdk/v2/client"
import {
  buildTaskPrompt,
  type FollowupQueue,
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
// Appended to a follow-up prompt carrying done=true (WI#328): the injected
// turn is the wrap-up turn — the worker must not start new lines of work
// after it, and the queue is closed so nothing can extend the run further.
const WRAP_UP_SUFFIX =
  "[control] This is the final instruction for this task: no further follow-up prompts will be " +
  "accepted. Answer it against the current session state, then consider the task complete — do " +
  "not start new lines of work."

type StopReason = "timeout" | "cancelled"

/** Settled outcome of a `session.prompt` call: the SDK result, or the error. */
type PromptOutcome =
  | { result: Awaited<ReturnType<OpencodeClient["session"]["prompt"]>> }
  | { error: unknown }

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

function emptyOutputError(deniedPermissions: number): string {
  if (deniedPermissions > 0) {
    return `Task performed no actions: ${deniedPermissions} permission request(s) ` +
      "were denied by the worker permission policy (the session permission " +
      "configuration likely needs to match the opencode SDK — smoke-verify)."
  }
  return "opencode completed without assistant text or tool output"
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
  followups?: FollowupQueue,
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
  // Per-turn canonical assistant text (MR !1525 DI-3): every turn's prompt
  // response contributes to the final result, so a short follow-up turn
  // (e.g. "status?") can never replace the main deliverable. Declared
  // function-scope so the catch path can still report banked turns (MR !1537
  // review DW-3).
  const canonicalTurns: string[] = []
  // [control] notes owed to the final output (MR !1552 ER-2): accepted-but-
  // unrun follow-ups are accounted for on BOTH the success and failure paths.
  const controlNotes: string[] = []
  // Seq of the follow-up prompt whose turn is currently running, undefined
  // while the MAIN turn (or its retries) is in flight. Once any turn has
  // succeeded, a failure/timeout in a follow-up turn must not change the
  // task's terminal status (DW-3).
  let currentFollowupSeq: number | undefined
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

    let nextPrompt = buildTaskPrompt(request)
    // Completed tool output of the CURRENT turn, accumulated across its retry
    // attempts (MR !1552 ES-1): turnStreamed resets per attempt, so a tool-only
    // turn whose first attempt produced output and then failed retryably would
    // otherwise bank nothing when the retry succeeds silently. Only completed
    // tools count — never tool errors, partial text, or retry diagnostics — so
    // a failing turn cannot manufacture success.
    let turnToolOutput = ""
    for (let attempt = 0; ; attempt++) {
      let sessionErrorMsg = ""
      const promptText = nextPrompt
      // Streamed text/tool output for THIS turn attempt (MR !1537 DW-7): a
      // turn whose assistant text is empty but whose tools produced output
      // still banks that output, so a follow-up can't erase a tool-only main
      // turn. Reset per attempt — failed attempts don't bank partial text.
      let turnStreamed = ""
      // DW-10: a stale idle event (published by a previous turn, possibly
      // under both event names) must not end this turn before the session
      // was observed busy or the prompt call settled.
      let turnBusy = false
      let turnSettled = false
      // Attach rejection handling immediately so a provider/API error cannot
      // become an unhandled rejection while the SSE loop is still draining.
      // The explicit PromptOutcome annotation keeps tsc's inference out of a
      // self-referential loop through the SDK's conditional RequestResult.
      const promptPromise: Promise<PromptOutcome> = sdk.session
        .prompt(
          {
            sessionID,
            parts: [{ type: "text", text: promptText }],
          },
          { throwOnError: true, signal: taskController.signal },
        )
        .then(
          (result): PromptOutcome => ({ result }),
          (error: unknown): PromptOutcome => ({ error }),
        )
      const promptFailure = promptPromise.then((outcome: PromptOutcome) => {
        if (!("error" in outcome)) return new Promise<never>(() => {})
        return { kind: "prompt_error" as const }
      })
      void promptPromise.then(() => {
        turnSettled = true
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
          turnBusy = true

          if (part.type === "text" && part.time?.end) {
            const text = part.text?.trim()
            if (text) {
              output += text + "\n"
              turnStreamed += text + "\n"
              emitProgress(text)
            }
          }

          if (part.type === "tool" && part.state?.status === "completed") {
            const toolOutput = part.state.output?.trim()
            const toolName = part.tool || "unknown"
            const delta = `[${toolName}] ${toolOutput || "completed"}`
            output += delta + "\n"
            turnStreamed += delta + "\n"
            turnToolOutput += delta + "\n"
            emitProgress(delta)
          }

          if (part.type === "tool" && part.state?.status === "error") {
            const toolName = part.tool || "unknown"
            const delta = `[${toolName}] ERROR: ${part.state.error}`
            output += delta + "\n"
            turnStreamed += delta + "\n"
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

        if (event.type === "session.status" && event.properties.sessionID === sessionID) {
          if (event.properties.status?.type === "idle") {
            // DW-10: an idle event only ends the turn once the turn was seen
            // busy or the prompt call settled — a stale idle replayed from a
            // previous turn (some builds publish both `session.status{idle}`
            // and `session.idle`) must not cut this turn short.
            if (turnBusy || turnSettled) break
            continue
          }
          turnBusy = true
        }

        if (event.type === "session.idle" && event.properties.sessionID === sessionID) {
          if (turnBusy || turnSettled) break
          continue
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
        // Turn finished cleanly; bank its canonical text (DI-3) — or, when
        // the assistant produced no text, this turn's streamed output (DW-7),
        // so a tool-only main turn survives a later "status?" follow-up.
        const turnText = extractAssistantText(promptData?.parts) || turnStreamed.trim() || turnToolOutput.trim()
        if (turnText) canonicalTurns.push(turnText)
        // A queued follow-up prompt (WI#328 runtime control channel)
        // continues the SAME session instead of resolving the task: the
        // daemon pushed it while the worker was busy and it is injected only
        // here, at a turn boundary — never mid-call. The framing header
        // attributes the instruction to the orchestrator so follow-up text
        // can't masquerade as a worker-generated `[control]` note (DW-13).
        // `closed` seals admission, not consumption: the bridge sets it when
        // accepting done=true, including while earlier accepted turns wait.
        const followup = followups?.prompts.shift()
        if (followup) {
          currentFollowupSeq = followup.seq
          emitProgress(
            `[control] Follow-up prompt received (seq=${followup.seq}${followup.done ? ", wrap-up requested" : ""})`,
          )
          if (followup.done) {
            // done=true: this injected turn is the wrap-up turn. The bridge
            // seals admission when it ACCEPTS a done prompt, so anything still
            // queued behind it is unreachable through the shipped bridge —
            // account for it explicitly instead of running it after the FINAL
            // turn told the model no further prompts would come (ER-2).
            if (followups) followups.closed = true
            for (const pending of followups?.prompts.splice(0) ?? []) {
              const note = `[control] follow-up seq=${pending.seq} not run: wrap-up turn already requested`
              controlNotes.push(note)
              emitProgress(note)
            }
            nextPrompt =
              `FOLLOW-UP INSTRUCTION (seq=${followup.seq}, FINAL) from the orchestrator:\n` +
              `${followup.prompt}\n\n${WRAP_UP_SUFFIX}`
          } else {
            nextPrompt =
              `FOLLOW-UP INSTRUCTION (seq=${followup.seq}) from the orchestrator:\n` +
              followup.prompt
          }
          turnToolOutput = "" // the follow-up starts a fresh turn
          attempt = -1 // each follow-up turn gets its own transient-retry budget
          continue
        }
        // Finishing: close the queue BEFORE resolving so a prompt landing
        // after this drain is refused (accepted=false) rather than queued
        // into a buffer nothing will ever read (DI-4).
        if (followups) followups.closed = true
        break
      }

      if (!isRetryableProviderError(promptError) || attempt >= maxProviderRetries) {
        // Share the terminal policy with permission/SSE failures. Control
        // notes are not completed output and cannot manufacture success.
        throw new Error(promptError)
      }

      const retryNumber = attempt + 1
      const delta =
        `[worker] Transient provider failure; retrying same OpenCode session ` +
        `(${retryNumber}/${maxProviderRetries}).`
      output += `ERROR: ${promptError}\n${delta}\n`
      emitProgress(delta)
      console.warn(`[opencode_task_executor] task ${request.task_id}: ${delta} ${promptError}`)

      nextPrompt = RETRY_PROMPT

      const delayResult = await Promise.race([
        new Promise<undefined>((resolve) => setTimeout(resolve, retryDelayMs)),
        stopPromise,
      ])
      if (delayResult) {
        throw new TaskStoppedError(delayResult)
      }
    }

    const canonicalOutput = canonicalTurns.join("\n\n")
    const durationMs = Date.now() - startTime

    if (!canonicalOutput) {
      // Only a cleanly finished turn can bank completed output. Retry
      // diagnostics, failed-attempt output, and control notes must not
      // manufacture success after an empty retry (FG-1/EZ-1).
      onResult({
        task_id: request.task_id,
        status: "error",
        output: [output.trim(), ...controlNotes].filter(Boolean).join("\n\n"),
        error: emptyOutputError(deniedPermissions),
        duration_ms: durationMs,
      })
      return
    }

    const resultOutput = [canonicalOutput, ...controlNotes].filter(Boolean).join("\n\n")

    onResult({
      task_id: request.task_id,
      status: "success",
      output: resultOutput,
      error: null,
      duration_ms: durationMs,
    })
  } catch (err) {
    // DW-5: no path out of a task leaves the control queue open — a prompt
    // landing during teardown (including the abortSession wait below) must be
    // refused (accepted=false) instead of acked and silently dropped.
    if (followups) followups.closed = true
    const stopped = stopReason ?? (err instanceof TaskStoppedError ? err.reason : undefined)
    const errorMsg = opencodeErrorMessage(err)
    const banked = canonicalTurns.join("\n\n")
    if (currentFollowupSeq !== undefined) {
      const outcome = stopped === "timeout" ? "timed out before completing"
        : stopped === "cancelled" ? "cancelled before completing" : `failed: ${errorMsg}`
      controlNotes.push(`[control] follow-up seq=${currentFollowupSeq} ${outcome}`)
    }
    // Accepted does not promise execution: explicitly account for every turn
    // still queued when any error/cancellation/timeout terminates this task.
    for (const pending of followups?.prompts.splice(0) ?? []) {
      controlNotes.push(`[control] follow-up seq=${pending.seq} not run: task ended before this turn`)
    }
    const resultOutput = [banked || output.trim(), ...controlNotes].filter(Boolean).join("\n\n")
    const preserveSuccess = currentFollowupSeq !== undefined && canonicalTurns.length > 0
    if (stopped) {
      closeEvents?.()
      await abortSession()
      // Only actual completed output preserves success through a follow-up
      // timeout. Explicit cancellation always remains cancelled.
      const timedOut = stopped === "timeout" && !preserveSuccess
      onResult({
        task_id: request.task_id,
        status: stopped === "cancelled" ? "cancelled" : timedOut ? "timed_out" : "success",
        output: resultOutput,
        error: timedOut ? `Task timed out after ${timeoutMs}ms` : null,
        duration_ms: Date.now() - startTime,
      })
      return
    }
    onResult({
      task_id: request.task_id,
      status: preserveSuccess ? "success" : "error",
      output: resultOutput,
      error: preserveSuccess ? null : currentFollowupSeq !== undefined
        ? `${emptyOutputError(deniedPermissions)}; follow-up failed: ${errorMsg}` : errorMsg,
      duration_ms: Date.now() - startTime,
    })
  } finally {
    // DW-5: belt and braces — every exit path leaves the queue closed.
    if (followups) followups.closed = true
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
