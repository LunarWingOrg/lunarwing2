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
  MAX_HISTORY_MESSAGE_UNITS,
  MAX_HISTORY_MESSAGES,
  MAX_HISTORY_UNITS,
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

test("enforces the harness's history caps in the harness's unit", () => {
  // 50 messages, 10,000 and 50,000 UTF-16 code units: the caps of the LunarDepth
  // harness, which the lane overlay configures. Every history a daemon sends at
  // its default limits (12 messages, 2,000 and 12,000 code points) fits.
  assert.equal(MAX_HISTORY_MESSAGES, 50)
  assert.equal(MAX_HISTORY_MESSAGE_UNITS, 10_000)
  assert.equal(MAX_HISTORY_UNITS, 50_000)
  const request = (history: { role: string; content: string }[]) => validateTaskRequest({
    task_id: "job",
    prompt: "x",
    context: { conversation_history: history },
  })
  const repeatMessage = (count: number, content: string) => Array.from({ length: count }, () => ({ role: "user", content }))

  assert.throws(() => request(repeatMessage(MAX_HISTORY_MESSAGES + 1, "x")), /at most 50 messages/)
  assert.equal(request(repeatMessage(MAX_HISTORY_MESSAGES, "x")).context?.conversation_history?.length, MAX_HISTORY_MESSAGES)

  // The daemon's default worst case: 12 messages that share 12,000 code points,
  // all outside the Basic Multilingual Plane, so two units each.
  assert.equal(request(repeatMessage(12, "😀".repeat(1_000))).context?.conversation_history?.length, 12)
  // One message at the daemon's default per-message bound, again all two-unit characters.
  assert.equal(request([{ role: "user", content: "😀".repeat(2_000) }]).context?.conversation_history?.[0]?.content.length, 4_000)
  // More than the daemon's defaults, within the caps.
  assert.equal(request(repeatMessage(30, "y".repeat(1_500))).context?.conversation_history?.length, 30)

  // Units, not code points: 5,000 two-unit characters are exactly the per-message cap.
  assert.equal(request([{ role: "user", content: "😀".repeat(5_000) }]).context?.conversation_history?.length, 1)
  assert.throws(() => request([{ role: "user", content: "😀".repeat(5_001) }]), /exceeds 10000 UTF-16 code units/)
  assert.throws(() => request([{ role: "user", content: "x".repeat(MAX_HISTORY_MESSAGE_UNITS + 1) }]), /exceeds 10000 UTF-16 code units/)

  const fullMessage = "z".repeat(MAX_HISTORY_MESSAGE_UNITS)
  assert.equal(request(repeatMessage(MAX_HISTORY_UNITS / MAX_HISTORY_MESSAGE_UNITS, fullMessage)).context?.conversation_history?.length, 5)
  assert.throws(
    () => request([...repeatMessage(MAX_HISTORY_UNITS / MAX_HISTORY_MESSAGE_UNITS, fullMessage), { role: "assistant", content: "z" }]),
    /exceeds 50000 UTF-16 code units/,
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
