import assert from "node:assert/strict"

import type { OpencodeClient } from "@opencode-ai/sdk/v2"

import { executeTask } from "./opencode_task_executor.ts"

interface MockOptions {
  events?: unknown[]
  pendingEvents?: boolean
  parts?: Array<{ type: string; text?: string }>
  promptError?: Error
}

function makeSdk(options: MockOptions = {}) {
  let v2Replies = 0
  let aborts = 0
  const stream = async function* () {
    if (options.pendingEvents) {
      await new Promise(() => {})
    }
    for (const event of options.events ?? [
      { type: "session.idle", properties: { sessionID: "session-1" } },
    ]) {
      yield event
    }
  }

  const sdk = {
    session: {
      create: async () => ({ data: { id: "session-1" } }),
      prompt: async () => {
        if (options.promptError) throw options.promptError
        return {
          data: {
            info: {},
            parts: options.parts ?? [{ type: "text", text: "done" }],
          },
        }
      },
      abort: async () => {
        aborts++
        return { data: true }
      },
    },
    event: {
      subscribe: async () => ({ stream: stream() }),
    },
    permission: {
      reply: async () => ({ data: true }),
    },
    v2: {
      session: {
        permission: {
          reply: async () => {
            v2Replies++
            return { data: undefined }
          },
        },
      },
    },
  } as unknown as OpencodeClient

  return { sdk, v2ReplyCount: () => v2Replies, abortCount: () => aborts }
}

async function runTask(sdk: OpencodeClient) {
  const results: Array<{
    status: string
    output: string
    error: string | null
  }> = []

  await executeTask(
    { task_id: "task-1", prompt: "test" },
    () => {},
    (taskResult) => {
      results.push(taskResult)
    },
    undefined,
    sdk,
  )

  assert.equal(results.length, 1)
  return results[0]!
}

const canonical = await runTask(
  makeSdk({ parts: [{ type: "text", text: " canonical output " }] }).sdk,
)
assert.equal(canonical.status, "success")
assert.equal(canonical.output, "canonical output")

const empty = await runTask(makeSdk({ parts: [] }).sdk)
assert.equal(empty.status, "error")
assert.equal(empty.error, "opencode completed without assistant text or tool output")

const rejected = await runTask(makeSdk({ promptError: new Error("provider failed") }).sdk)
assert.equal(rejected.status, "error")
assert.equal(rejected.error, "provider failed")

const permission = makeSdk({
  events: [
    {
      type: "permission.v2.asked",
      properties: { id: "permission-1", sessionID: "session-1" },
    },
    { type: "session.idle", properties: { sessionID: "session-1" } },
  ],
})
const permissionResult = await runTask(permission.sdk)
assert.equal(permissionResult.status, "success")
assert.equal(permission.v2ReplyCount(), 1)

const timeout = makeSdk({ pendingEvents: true })
const timeoutResults: Array<{ status: string; error: string | null }> = []
await executeTask(
  { task_id: "task-timeout", prompt: "test", timeout_ms: 10 },
  () => {},
  (result) => timeoutResults.push(result),
  undefined,
  timeout.sdk,
)
assert.equal(timeoutResults.length, 1)
assert.equal(timeoutResults[0]!.status, "error")
assert.equal(timeoutResults[0]!.error, "Task timed out after 10ms")
assert.equal(timeout.abortCount(), 1)

console.log("opencode task executor self-check passed")
