/**
 * lunarwing_bridge_test.ts — self-test for the WI#328 control-channel bridge
 * handlers (MR !1537 review DW-9). Covers the refusal paths the executor
 * tests cannot see: closed-queue refusal (J), post-done refusal (B),
 * cross-socket injection (G), frame validation (DW-11), and seq dedupe
 * (DW-13).
 *
 * Run: bun scripts/lunarwing_bridge_test.ts
 */

import assert from "node:assert/strict"
import type { OpencodeClient } from "@opencode-ai/sdk/v2/client"

import {
  activePromptQueues,
  activeTasks,
  handleCancel,
  handleMessage,
  handleTaskRequest,
  handleTaskPrompt,
  sendReady,
} from "./lunarwing_bridge.ts"
import { createEnvelope, MAX_CONTROL_QUEUE_PER_TASK, type FollowupQueue } from "./lunarwing_runtime.ts"

interface SentFrame {
  type: string
  payload: {
    task_id?: string; seq?: number; accepted?: boolean
    capabilities?: string[]; status?: string; output?: string
  }
}

function fakeSocket(): { socket: { send(data: string): void }; frames: SentFrame[] } {
  const frames: SentFrame[] = []
  return {
    socket: {
      send(data: string) {
        frames.push(JSON.parse(data) as SentFrame)
      },
    },
    frames,
  }
}

function lastAck(frames: SentFrame[]): { task_id?: string; seq?: number; accepted?: boolean } {
  const acks = frames.filter((f) => f.type === "task_prompt_ack")
  assert.ok(acks.length > 0, "expected at least one task_prompt_ack")
  return acks[acks.length - 1]!.payload
}

let taskCounter = 0
/** Register a live task on a socket with an open prompt queue. */
function registerTask(socket: { send(data: string): void }, closed = false) {
  const taskId = `task-${++taskCounter}`
  const controller = new AbortController()
  activeTasks.set(taskId, { controller, socket, workspace: `/workspace/${taskId}` })
  const queue: FollowupQueue = { prompts: [], closed, seenSeqs: new Set() }
  activePromptQueues.set(taskId, queue)
  return { taskId, controller, queue }
}

function cleanup(taskId: string): void {
  activeTasks.delete(taskId)
  activePromptQueues.delete(taskId)
}

// ── Happy path: a well-formed prompt is queued and acked accepted ────────
{
  const a = fakeSocket()
  const { taskId, queue } = registerTask(a.socket)
  handleTaskPrompt({ task_id: taskId, seq: 1, prompt: "status?", done: false }, a.socket)
  assert.equal(lastAck(a.frames).accepted, true)
  assert.equal(queue.prompts.length, 1)
  assert.equal(queue.prompts[0]!.prompt, "status?")
  cleanup(taskId)
}

// ── Scenario B (DW-4): done=true closes the queue at ACCEPT time ─────────
{
  const a = fakeSocket()
  const { taskId, queue } = registerTask(a.socket)
  handleTaskPrompt({ task_id: taskId, seq: 1, prompt: "wrap up", done: true }, a.socket)
  assert.equal(lastAck(a.frames).accepted, true)
  assert.equal(queue.closed, true, "done must close the queue when accepted, not when consumed")

  // A prompt sent after done is refused, never queued-and-dropped.
  handleTaskPrompt({ task_id: taskId, seq: 2, prompt: "one more", done: false }, a.socket)
  const ack = lastAck(a.frames)
  assert.equal(ack.accepted, false)
  assert.equal(ack.seq, 2)
  assert.equal(queue.prompts.length, 1, "post-done prompt must not be queued")
  cleanup(taskId)
}

// ── Scenario J (DI-4): a closed queue (final drain done) refuses ─────────
{
  const a = fakeSocket()
  const { taskId, queue } = registerTask(a.socket, true)
  handleTaskPrompt({ task_id: taskId, seq: 7, prompt: "late", done: false }, a.socket)
  assert.equal(lastAck(a.frames).accepted, false)
  assert.equal(queue.prompts.length, 0)
  cleanup(taskId)
}

// ── Scenario G (DW-8): a foreign socket cannot feed another task's queue ──
{
  const a = fakeSocket()
  const b = fakeSocket()
  const { taskId, queue } = registerTask(a.socket)
  handleTaskPrompt({ task_id: taskId, seq: 1, prompt: "injected by B", done: false }, b.socket)
  const ack = lastAck(b.frames)
  assert.equal(ack.accepted, false, "foreign socket must be refused")
  assert.equal(queue.prompts.length, 0, "foreign prompt must never be queued")
  assert.equal(a.frames.length, 0, "the owning socket is not notified either")
  cleanup(taskId)
}

// ── DW-8: cancel is bound to the owning socket too ───────────────────────
{
  const a = fakeSocket()
  const b = fakeSocket()
  const { taskId, controller } = registerTask(a.socket)
  handleCancel({ task_id: taskId }, b.socket)
  assert.equal(controller.signal.aborted, false, "foreign cancel must not abort the task")
  handleCancel({ task_id: taskId }, a.socket)
  assert.equal(controller.signal.aborted, true, "owning socket cancels its own task")
  cleanup(taskId)
}

// ── DW-11: frame validation ──────────────────────────────────────────────
{
  const a = fakeSocket()
  const { taskId, queue } = registerTask(a.socket)
  const refuse = (payload: unknown, label: string) => {
    const before = a.frames.length
    handleTaskPrompt(payload, a.socket)
    assert.equal(queue.prompts.length, 0, `${label}: must not queue`)
    assert.equal(a.frames.length, before + 1, `${label}: must ack the refusal`)
    assert.equal(lastAck(a.frames).accepted, false, `${label}: must be refused`)
  }
  // seq must be a positive safe integer
  refuse({ task_id: taskId, seq: "7", prompt: "x", done: false }, "string seq")
  refuse({ task_id: taskId, seq: 1.5, prompt: "x", done: false }, "fractional seq")
  refuse({ task_id: taskId, seq: -1, prompt: "x", done: false }, "negative seq")
  refuse({ task_id: taskId, seq: 0, prompt: "x", done: false }, "zero seq")
  // prompt must be non-empty, NUL-free, and within the 16 KiB bound
  refuse({ task_id: taskId, seq: 1, prompt: "   ", done: false }, "blank prompt")
  refuse(
    { task_id: taskId, seq: 1, prompt: `has${String.fromCharCode(0)}nul`, done: false },
    "NUL prompt",
  )
  refuse({ task_id: taskId, seq: 1, prompt: "x".repeat(16 * 1024 + 1), done: false }, "oversized")
  refuse({ task_id: taskId, seq: 1, prompt: "x", done: "yes" }, "non-boolean done")
  assert.deepEqual(lastAck(a.frames), { task_id: taskId, seq: 1, accepted: false },
    "malformed instructions retain valid correlation fields")
  refuse(null, "null payload")
  refuse("task_prompt", "string payload")
  // unknown task
  handleTaskPrompt({ task_id: "no-such-task", seq: 1, prompt: "x", done: false }, a.socket)
  assert.equal(lastAck(a.frames).accepted, false, "unknown task refused")

  // Per-task queue bound: MAX_CONTROL_QUEUE_PER_TASK queued prompts, then refuse.
  for (let seq = 1; seq <= MAX_CONTROL_QUEUE_PER_TASK; seq++) {
    handleTaskPrompt({ task_id: taskId, seq, prompt: `prompt ${seq}`, done: false }, a.socket)
  }
  assert.equal(queue.prompts.length, MAX_CONTROL_QUEUE_PER_TASK)
  handleTaskPrompt(
    { task_id: taskId, seq: MAX_CONTROL_QUEUE_PER_TASK + 1, prompt: "flood", done: false },
    a.socket,
  )
  assert.equal(lastAck(a.frames).accepted, false, "queue flood refused at the bound")
  assert.equal(queue.prompts.length, MAX_CONTROL_QUEUE_PER_TASK)
  cleanup(taskId)
}

// ── DW-13: seq dedupe — a redelivered seq is acked but not queued twice ──
{
  const a = fakeSocket()
  const { taskId, queue } = registerTask(a.socket)
  const frame = { task_id: taskId, seq: 4, prompt: "status?", done: false }
  handleTaskPrompt(frame, a.socket)
  handleTaskPrompt(frame, a.socket)
  const acks = a.frames.filter((f) => f.type === "task_prompt_ack")
  assert.equal(acks.length, 2)
  assert.ok(acks.every((f) => f.payload.accepted === true), "redelivery is acked accepted")
  assert.equal(queue.prompts.length, 1, "duplicate seq is not enqueued twice")
  queue.closed = true
  handleTaskPrompt(frame, a.socket)
  assert.equal(lastAck(a.frames).accepted, false, "closed admission also refuses redelivery")
  assert.equal(queue.prompts.length, 1)
  cleanup(taskId)
}

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((done) => { resolve = done })
  return { promise, resolve }
}

async function bounded<T>(promise: Promise<T>): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error("bridge fixture timed out")), 2_000)
      }),
    ])
  } finally {
    clearTimeout(timer)
  }
}

// The SDK boundary is stubbed; queue creation, dispatch, acceptance, execution,
// result frames and finally cleanup all use the real bridge/executor wiring.
function controlledSdk() {
  const turns = Array.from({ length: 3 }, () => ({
    started: deferred<{ sessionID: string; parts: Array<{ text: string }> }>(),
    completed: deferred<string>(),
  }))
  const abortStarted = deferred<void>()
  const abortCompleted = deferred<void>()
  let calls = 0
  const sdk = {
    session: {
      create: async () => ({ data: { id: "bridge-session" } }),
      prompt: async (args: { sessionID: string; parts: Array<{ text: string }> }) => {
        const turn = turns[calls++]!
        turn.started.resolve(args)
        const text = await turn.completed.promise
        return { data: { info: {}, parts: [{ type: "text", text }] } }
      },
      abort: async () => {
        abortStarted.resolve()
        await abortCompleted.promise
        return { data: true }
      },
    },
    event: {
      subscribe: async () => ({
        stream: (async function* () {
          for (const turn of turns) {
            await turn.completed.promise
            yield { type: "session.idle", properties: { sessionID: "bridge-session" } }
          }
        })(),
      }),
    },
  } as unknown as OpencodeClient
  return { sdk, turns, abortStarted, abortCompleted, calls: () => calls }
}

{
  const a = fakeSocket()
  sendReady(a.socket)
  assert.equal(a.frames[0]!.type, "ready")
  assert.ok(a.frames[0]!.payload.capabilities?.includes("task_prompt"))
}

// !1552 EK-1 A/B/C: final alone, normal+final queued during main, and final
// arriving during a follow-up. Later frames are refused, accepted work drains.
for (const scenario of ["final-only", "queued-final", "mid-followup-final"] as const) {
  const a = fakeSocket()
  const mock = controlledSdk()
  const taskId = `integration-${scenario}`
  const execution = handleTaskRequest({ task_id: taskId, prompt: "build", timeout_ms: 5_000 }, a.socket, mock.sdk)
  await bounded(mock.turns[0]!.started.promise)
  const queue = activePromptQueues.get(taskId)!
  const sendPrompt = (seq: number, done: boolean) => handleMessage(JSON.stringify(createEnvelope(
    "task_prompt", { task_id: taskId, seq, prompt: done ? "wrap up" : "status?", done },
  )), a.socket)

  if (scenario !== "final-only") await sendPrompt(1, false)
  if (scenario === "mid-followup-final") {
    mock.turns[0]!.completed.resolve("MAIN DELIVERABLE")
    await bounded(mock.turns[1]!.started.promise)
  }
  const finalSeq = scenario === "final-only" ? 1 : 2
  await sendPrompt(finalSeq, true)
  assert.equal(lastAck(a.frames).accepted, true)
  assert.equal(queue.closed, true)
  await sendPrompt(finalSeq + 1, false)
  assert.equal(lastAck(a.frames).accepted, false)
  assert.equal(mock.calls(), scenario === "mid-followup-final" ? 2 : 1, "no mid-call injection")

  mock.turns[0]!.completed.resolve("MAIN DELIVERABLE")
  for (let turn = 1; turn <= finalSeq; turn++) {
    const args = await bounded(mock.turns[turn]!.started.promise)
    assert.equal(args.sessionID, "bridge-session")
    const text = args.parts[0]!.text
    assert.ok(text.startsWith(`FOLLOW-UP INSTRUCTION (seq=${turn}${turn === finalSeq ? ", FINAL" : ""}) from the orchestrator:\n`))
    assert.equal(text.includes("final instruction for this task"), turn === finalSeq)
    mock.turns[turn]!.completed.resolve(`ANSWER ${turn}`)
  }
  await bounded(execution)
  assert.equal(mock.calls(), finalSeq + 1)
  assert.equal(queue.prompts.length, 0)
  assert.equal(activePromptQueues.has(taskId), false, "finally deletes the queue")
  assert.equal(activeTasks.has(taskId), false)
  const results = a.frames.filter((frame) => frame.type === "task_result")
  assert.equal(results.length, 1)
  assert.equal(results[0]!.payload.status, "success")
  assert.equal(results[0]!.payload.output,
    ["MAIN DELIVERABLE", ...Array.from({ length: finalSeq }, (_, i) => `ANSWER ${i + 1}`)].join("\n\n"))
}

// !1552 EK-5: route cancel through handleMessage during a follow-up, then
// submit a prompt while session.abort is still pending. Admission must close
// BEFORE that await; cancellation must retain canonical (not SSE) output.
{
  const a = fakeSocket()
  const foreign = fakeSocket()
  const mock = controlledSdk()
  const taskId = "integration-cancel"
  const execution = handleTaskRequest({ task_id: taskId, prompt: "build", timeout_ms: 5_000 }, a.socket, mock.sdk)
  await bounded(mock.turns[0]!.started.promise)
  await handleMessage(JSON.stringify(createEnvelope("task_prompt", {
    task_id: taskId, seq: 1, prompt: "status?", done: false,
  })), a.socket)
  mock.turns[0]!.completed.resolve("MAIN DELIVERABLE")
  await bounded(mock.turns[1]!.started.promise)
  const controller = activeTasks.get(taskId)!.controller
  const cancel = JSON.stringify(createEnvelope("cancel", { task_id: taskId }))
  await handleMessage(cancel, foreign.socket)
  assert.equal(controller.signal.aborted, false)
  await handleMessage(cancel, a.socket)
  await bounded(mock.abortStarted.promise)
  await handleMessage(JSON.stringify(createEnvelope("task_prompt", {
    task_id: taskId, seq: 2, prompt: "too late", done: false,
  })), a.socket)
  assert.equal(lastAck(a.frames).accepted, false, "refuse during abort wait")
  mock.abortCompleted.resolve()
  mock.turns[1]!.completed.resolve("late response")
  await bounded(execution)
  const result = a.frames.find((frame) => frame.type === "task_result")!
  assert.equal(result.payload.status, "cancelled")
  assert.match(result.payload.output!, /^MAIN DELIVERABLE\n\n\[control\] follow-up seq=1 cancelled/)
  assert.equal(activePromptQueues.has(taskId), false)
  assert.equal(activeTasks.has(taskId), false)
}

console.log("lunarwing bridge self-check passed")
