import assert from "node:assert/strict"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

import {
  extractAssistantText,
  opencodeErrorMessage,
  withServeLogDetail,
} from "./opencode_result.ts"

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

const GENERIC = "Unexpected server error. Check server logs for details."
const temp = mkdtempSync(join(tmpdir(), "serve-log-test-"))
try {
  const logPath = join(temp, "opencode-serve.log")
  const crash =
    "SystemPrompt.environment — TypeError: undefined is not an object " +
    "(evaluating 'a.name')"

  // Non-generic errors already name their cause: never touched, log or not.
  writeFileSync(logPath, `${crash}\n`)
  assert.equal(await withServeLogDetail("provider timed out", logPath), "provider timed out")

  // The generic wrapper with no readable log stays as-is.
  assert.equal(await withServeLogDetail(GENERIC, join(temp, "missing.log")), GENERIC)

  // The generic wrapper gains the serve log tail with the real root cause.
  const enriched = await withServeLogDetail(GENERIC, logPath)
  assert.ok(enriched.startsWith(GENERIC))
  assert.ok(enriched.includes("opencode serve log tail:"))
  assert.ok(enriched.includes(crash))

  // Blank lines are dropped from the tail.
  writeFileSync(logPath, `first\n\n\nsecond\n`)
  const compact = await withServeLogDetail(GENERIC, logPath)
  assert.ok(compact.endsWith("first\nsecond"))

  // The tail is bounded: only the last lines survive, long lines truncate,
  // and a mid-line byte-window cut never emits a partial first line.
  const longLine = "x".repeat(600)
  const body = Array.from({ length: 60 }, (_, i) => `line-${i}-${longLine}`).join("\n")
  writeFileSync(logPath, body)
  const bounded = await withServeLogDetail(GENERIC, logPath)
  const tailLines = bounded.split("opencode serve log tail:\n")[1].split("\n")
  assert.ok(tailLines.length <= 25, `expected <=25 tail lines, got ${tailLines.length}`)
  assert.ok(tailLines.every((line) => line.length <= 401))
  assert.ok(tailLines.every((line) => /^line-\d+-/.test(line)), "partial first line leaked")
  assert.ok(tailLines.at(-1)?.startsWith("line-59-"))
  assert.ok(!bounded.includes("line-0-"))
} finally {
  rmSync(temp, { recursive: true, force: true })
}

console.log("opencode result self-check passed")
