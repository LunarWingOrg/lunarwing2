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

import {
  activePromptQueues,
  activeTasks,
  handleCancel,
  handleTaskPrompt,
} from "./lunarwing_bridge.ts"
import { MAX_CONTROL_QUEUE_PER_TASK, type FollowupQueue } from "./lunarwing_runtime.ts"

interface SentFrame {
  type: string
  payload: { task_id?: string; seq?: number; accepted?: boolean }
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
  cleanup(taskId)
}

console.log("lunarwing bridge self-check passed")
