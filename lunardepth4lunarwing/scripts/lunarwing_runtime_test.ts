import assert from "node:assert/strict"
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"

import {
  bearerTokenMatches,
  boundedCloseReason,
  createEnvelope,
  deadlineDelay,
  MAX_HISTORY_MESSAGES,
  MAX_TIMER_DELAY_MS,
  MAX_TIMEOUT_MS,
  parseEnvelope,
  readCredentialFile,
  serializeBounded,
  validateTaskRequest,
} from "./lunarwing_runtime.js"

test("accepts a bounded task with the daemon's serialized context", () => {
  const request = validateTaskRequest({
    task_id: "job-1",
    prompt: "inspect the workspace",
    timeout_ms: 1000,
    context: {
      project_dir: "/workspace/project",
      conversation_history: [{ role: "user", content: "earlier" }],
      environment: {},
      user_id: "user-1",
      metadata: { origin: "daemon-test" },
    },
  })
  assert.equal(request.task_id, "job-1")
  assert.equal(request.context?.conversation_history?.length, 1)
  assert.equal(Object.keys(request.context ?? {}).includes("environment"), false)
})

test("rejects grants, cron, malformed context, and unknown fields", () => {
  const taskFields: Array<[string, unknown]> = [["credentials", {}], ["credential_grants", []], ["metadata", {}]]
  for (const [key, value] of taskFields) {
    assert.throws(() => validateTaskRequest({ task_id: "job", prompt: "x", [key]: value }), /unsupported task field/)
  }
  assert.throws(
    () => validateTaskRequest({ task_id: "job", prompt: "x", context: { environment: { KEY: "value" } } }),
    /environment and credential grants are unsupported/,
  )
  assert.throws(
    () => validateTaskRequest({ task_id: "job", prompt: "x", context: { cron_tick: null } }),
    /cron dispatch is unsupported/,
  )
  assert.throws(
    () => validateTaskRequest({ task_id: "job", prompt: "x", context: { metadata: { invalid: 1 } } }),
    /string-to-string object/,
  )
  assert.throws(
    () => validateTaskRequest({ task_id: "job", prompt: "x", context: { unknown: true } }),
    /unsupported task context field/,
  )
})

test("enforces history count and Unicode code-point limits", () => {
  assert.throws(
    () => validateTaskRequest({
      task_id: "job",
      prompt: "x",
      context: { conversation_history: Array.from({ length: MAX_HISTORY_MESSAGES + 1 }, () => ({ role: "user", content: "x" })) },
    }),
    /at most 12 messages/,
  )
  assert.equal(validateTaskRequest({
    task_id: "job",
    prompt: "x",
    context: { conversation_history: [{ role: "user", content: "😀".repeat(1001) }] },
  }).context?.conversation_history?.[0]?.content.length, 2002)
  assert.throws(
    () => validateTaskRequest({
      task_id: "job",
      prompt: "x",
      context: { conversation_history: [{ role: "user", content: "😀".repeat(2001) }] },
    }),
    /Unicode code points/,
  )
})

test("bounds close reasons and deadline timers", () => {
  const reason = boundedCloseReason(new Error("😀".repeat(100)))
  assert.ok(Buffer.byteLength(reason, "utf8") <= 123)
  assert.equal(deadlineDelay(MAX_TIMEOUT_MS, 60_000), MAX_TIMER_DELAY_MS)
})

test("round-trips envelopes and compares bearer digests", () => {
  const envelope = createEnvelope("ping", {})
  assert.equal(parseEnvelope(JSON.stringify(envelope)).type, "ping")
  assert.equal(bearerTokenMatches("Bearer secret", "secret"), true)
  assert.equal(bearerTokenMatches("Bearer wrong", "secret"), false)
  assert.equal(bearerTokenMatches(undefined, "secret"), false)
  assert.throws(() => parseEnvelope('{"type":"ping","payload":{}}'), /envelope/)
  assert.equal(serializeBounded(envelope), JSON.stringify(envelope))
})

test("reads only current-role regular 0400 credential files", () => {
  const directory = mkdtempSync(join(tmpdir(), "lunardepth-runtime-"))
  const path = join(directory, "token")
  try {
    writeFileSync(path, "a".repeat(64), { mode: 0o600 })
    assert.throws(() => readCredentialFile(path), /0400/)
    chmodSync(path, 0o400)
    assert.equal(readCredentialFile(path), "a".repeat(64))
  } finally {
    chmodSync(path, 0o600)
    rmSync(directory, { recursive: true, force: true })
  }
})
