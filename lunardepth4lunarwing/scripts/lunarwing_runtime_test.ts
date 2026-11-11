import assert from "node:assert/strict"
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"

import {
  bearerTokenMatches,
  createEnvelope,
  MAX_HISTORY_MESSAGES,
  parseEnvelope,
  readCredentialFile,
  serializeBounded,
  validateTaskRequest,
} from "./lunarwing_runtime.js"

test("accepts a bounded task with supported context", () => {
  const request = validateTaskRequest({
    task_id: "job-1",
    prompt: "inspect the workspace",
    timeout_ms: 1000,
    context: {
      project_dir: "/workspace/project",
      conversation_history: [{ role: "user", content: "earlier" }],
    },
  })
  assert.equal(request.task_id, "job-1")
  assert.equal(request.context?.conversation_history?.length, 1)
})

test("rejects grants, environment, metadata, cron, and unknown fields", () => {
  for (const [key, value] of [["credentials", {}], ["credential_grants", []], ["metadata", {}]]) {
    assert.throws(() => validateTaskRequest({ task_id: "job", prompt: "x", [key]: value }), /unsupported task field/)
  }
  for (const [key, value] of [["environment", {}], ["metadata", {}], ["cron_tick", null], ["user_id", "u"]]) {
    assert.throws(
      () => validateTaskRequest({ task_id: "job", prompt: "x", context: { [key]: value } }),
      /unsupported task context field/,
    )
  }
})

test("enforces history count and UTF-16 code-unit limits", () => {
  assert.throws(
    () => validateTaskRequest({
      task_id: "job",
      prompt: "x",
      context: { conversation_history: Array.from({ length: MAX_HISTORY_MESSAGES + 1 }, () => ({ role: "user", content: "x" })) },
    }),
    /at most 12 messages/,
  )
  assert.throws(
    () => validateTaskRequest({
      task_id: "job",
      prompt: "x",
      context: { conversation_history: [{ role: "user", content: "😀".repeat(1001) }] },
    }),
    /UTF-16 code units/,
  )
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
