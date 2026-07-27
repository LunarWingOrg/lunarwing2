import assert from "node:assert/strict"

import { buildTaskPrompt } from "./lunarwing_runtime.ts"

assert.equal(
  buildTaskPrompt({ task_id: "task-1", prompt: "nanocode prompt" }),
  "nanocode prompt",
)

const prompt = buildTaskPrompt({
  task_id: "task-2",
  prompt: "authoritative",
  context: {
    conversation_history: [
      { role: "system", content: "excluded" },
      { role: "assistant", content: "prior <answer> & data" },
    ],
  },
})
assert.equal(prompt.includes("excluded"), false)
assert.equal(
  prompt.includes(`[{"role":"assistant","content":"prior \\u003canswer\\u003e \\u0026 data"}]`),
  true,
)
assert.equal(prompt.endsWith("AUTHORITATIVE TASK\nauthoritative"), true)

console.log("nanocode task prompt self-check passed")
