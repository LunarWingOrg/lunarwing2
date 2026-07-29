import assert from "node:assert/strict"
import { test } from "node:test"
import { extractAssistantText, kimiErrorMessage } from "./kimi_result.ts"

test("extractAssistantText returns the last non-empty assistant message", () => {
  const messages = [
    { role: "assistant", content: "thinking..." },
    { role: "tool", content: "result" },
    { role: "assistant", content: "final answer" },
  ]
  assert.equal(extractAssistantText(messages as never), "final answer")
})

test("extractAssistantText falls back to all assistant text when no single non-empty", () => {
  const messages = [{ role: "assistant", content: "partial" }]
  assert.equal(extractAssistantText(messages as never), "partial")
})

test("extractAssistantText returns empty for no assistant messages", () => {
  const messages = [{ role: "tool", content: "only tool" }]
  assert.equal(extractAssistantText(messages as never), "")
})

test("kimiErrorMessage extracts message from Error", () => {
  assert.equal(kimiErrorMessage(new Error("boom")), "boom")
})

test("kimiErrorMessage extracts message from object", () => {
  assert.equal(kimiErrorMessage({ message: "fail" }), "fail")
})

test("kimiErrorMessage returns generic for null", () => {
  assert.equal(kimiErrorMessage(null), "Unknown kimi error")
})

console.log("kimi result self-check passed")
