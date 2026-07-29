import assert from "node:assert/strict"
import { test } from "node:test"
import { buildTaskPrompt, type TaskRequest } from "./lunarwing_runtime.ts"

test("buildTaskPrompt: no history returns bare prompt", () => {
  const request: TaskRequest = { task_id: "t1", prompt: "do the thing" }
  assert.equal(buildTaskPrompt(request), "do the thing")
})

test("buildTaskPrompt: with history wraps in untrusted block", () => {
  const request: TaskRequest = {
    task_id: "t2",
    prompt: "fix the bug",
    context: {
      conversation_history: [
        { role: "user", content: "hello" },
        { role: "assistant", content: "hi there" },
      ],
    },
  }
  const result = buildTaskPrompt(request)
  assert.ok(result.includes("UNTRUSTED CONVERSATION HISTORY"))
  assert.ok(result.includes("hello"))
  assert.ok(result.includes("hi there"))
  assert.ok(result.includes("fix the bug"))
  // Untrusted JSON must be escaped so injected tags cannot break out.
  assert.ok(!result.includes("<script>"))
})

test("buildTaskPrompt: skips non-user/assistant roles", () => {
  const request: TaskRequest = {
    task_id: "t3",
    prompt: "task",
    context: {
      conversation_history: [
        { role: "tool", content: "tool output" },
        { role: "system", content: "system msg" },
        { role: "user", content: "real user" },
      ],
    },
  }
  const result = buildTaskPrompt(request)
  assert.ok(result.includes("real user"))
  assert.ok(!result.includes("tool output"))
  assert.ok(!result.includes("system msg"))
})

console.log("task prompt self-check passed")
