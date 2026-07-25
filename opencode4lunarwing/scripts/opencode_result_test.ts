import assert from "node:assert/strict"

import { extractAssistantText, opencodeErrorMessage } from "./opencode_result.ts"

assert.equal(
  extractAssistantText([
    { type: "tool" },
    { type: "text", text: " first " },
    { type: "text", text: "second" },
  ]),
  "first\nsecond",
)
assert.equal(extractAssistantText([{ type: "text", text: "  " }]), "")
assert.equal(extractAssistantText(undefined), "")
assert.equal(opencodeErrorMessage({ data: { message: "provider failed" } }), "provider failed")
assert.equal(opencodeErrorMessage(new Error("request failed")), "request failed")

console.log("opencode result self-check passed")
