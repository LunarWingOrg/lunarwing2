import assert from "node:assert/strict"
import { test } from "node:test"

import {
  bearerTokenMatches,
  createEnvelope,
  serializeEnvelopeWithinLimit,
  validateTaskRequest,
} from "./lunarwing_runtime.js"

const limits = {
  maxTaskIdBytes: 32,
  maxPromptBytes: 1024,
  maxEnvironmentBytes: 1024,
  maxTimeoutMs: 60_000,
}

test("bearerTokenMatches: requires exact Bearer token", () => {
  assert.equal(bearerTokenMatches(undefined, "secret"), false)
  assert.equal(bearerTokenMatches("Bearer wrong", "secret"), false)
  assert.equal(bearerTokenMatches("Bearer secret", "secret"), true)
  assert.equal(bearerTokenMatches("Basic secret", "secret"), false)
})

test("validateTaskRequest: bounds task id and timeout", () => {
  assert.match(
    validateTaskRequest({ task_id: "bad\nlog", prompt: "x" }, limits).error || "",
    /control characters/,
  )
  assert.match(
    validateTaskRequest({ task_id: "ok", prompt: "x", timeout_ms: 60_001 }, limits).error || "",
    /between 1 and 60000/,
  )
  assert.ok(validateTaskRequest({ task_id: "ok", prompt: "x", timeout_ms: 60_000 }, limits).request)
})

test("serializeEnvelopeWithinLimit: caps escaped payload after JSON encoding", () => {
  const encoded = serializeEnvelopeWithinLimit(
    createEnvelope("task_result", {
      task_id: "bounded",
      status: "success",
      output: "\\\"\u0000".repeat(10_000),
      error: null,
      duration_ms: 1,
    }),
    1024,
  )
  assert.ok(Buffer.byteLength(encoded, "utf8") <= 1024)
  assert.equal(JSON.parse(encoded).payload.task_id, "bounded")
})
