import assert from "node:assert/strict"
import { test } from "node:test"
import {
  buildTaskPrompt,
  parseEnvelope,
  truncateUtf8,
  utf8ByteLength,
  validateTaskRequest,
  type TaskRequest,
} from "./lunarwing_runtime.js"

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

test("runtime JSON validation rejects arrays, nulls, and oversized UTF-8 fields", () => {
  assert.equal(parseEnvelope('{"type":"ping","payload":[]}'), null)
  assert.equal(parseEnvelope('{"type":"ping","payload":null}'), null)
  const limits = {
    maxTaskIdBytes: 32,
    maxPromptBytes: 3,
    maxEnvironmentBytes: 5,
    maxTimeoutMs: 60_000,
  }
  assert.match(validateTaskRequest({ task_id: "t", prompt: "😀" }, limits).error || "", /exceeds/)
  assert.match(validateTaskRequest({ task_id: "t", prompt: "ok", context: { environment: [] } }, limits).error || "", /JSON object/)
  assert.match(validateTaskRequest({ task_id: "t", prompt: "ok", context: { environment: { A: 4 } } }, limits).error || "", /strings/)
  assert.match(validateTaskRequest({ task_id: "t", prompt: "ok", timeout_ms: 60_001 }, limits).error || "", /60000/)
})

test("truncateUtf8 preserves complete code points within byte cap", () => {
  const value = truncateUtf8("a😀b", 4)
  assert.equal(value, "a")
  assert.ok(utf8ByteLength(value) <= 4)
  assert.equal(truncateUtf8("a😀b", 5), "a😀")
})

console.log("task prompt self-check passed")
