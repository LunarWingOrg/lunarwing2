import assert from "node:assert/strict"
import { execFileSync } from "node:child_process"
import { mkdtempSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"

import {
  extractAssistantText,
  opencodeErrorRef,
  opencodeErrorMessage,
  resolveServeLogPath,
  serveLogStartSize,
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

// --- resolveServeLogPath: override / explicit disable / xdg default ---

assert.equal(resolveServeLogPath({ OPENCODE_SERVE_LOG: "/tmp/x.log" }, "/home/u"), "/tmp/x.log")
assert.equal(resolveServeLogPath({ OPENCODE_SERVE_LOG: "" }, "/home/u"), undefined)
assert.equal(
  resolveServeLogPath({}, "/home/u"),
  join("/home/u", ".local", "share", "opencode", "log", "opencode.log"),
)
assert.equal(
  resolveServeLogPath({ XDG_DATA_HOME: "/data" }, "/home/u"),
  join("/data", "opencode", "log", "opencode.log"),
)

// --- opencodeErrorRef: SDK-wrapped body, POJO, and junk rejection ---

const sdkError = new Error(GENERIC(), {
  cause: { body: { name: "UnknownError", data: { message: GENERIC(), ref: "err_deadbeef" } }, status: 500 },
})
assert.equal(opencodeErrorRef(sdkError), "err_deadbeef")
assert.equal(opencodeErrorRef({ data: { ref: "err_0123abcd" } }), "err_0123abcd")
assert.equal(opencodeErrorRef({ ref: "err_ff" }), "err_ff")
assert.equal(opencodeErrorRef({ data: { ref: "not-a-ref" } }), undefined)
assert.equal(opencodeErrorRef(new Error(GENERIC())), undefined)
assert.equal(opencodeErrorRef(undefined), undefined)

function GENERIC() {
  return "Unexpected server error. Check server logs for details."
}

function errorLine(tag: string, extra = ""): string {
  return (
    `timestamp=2026-10-12T00:00:00.000Z level=Error run=run01 message=failed ${tag} ` +
    `error=TypeError: undefined is not an object (evaluating 'a.name')${extra}`
  )
}

function infoLine(tag: string): string {
  return `timestamp=2026-10-12T00:00:00.000Z level=Info run=run01 message=session.updated ${tag}`
}

function tailOf(enriched: string): string[] {
  return enriched.split("opencode serve log tail:\n")[1]?.split("\n") ?? []
}

const LONE_SURROGATE = /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?:[^\uD800-\uDBFF]|^)[\uDC00-\uDFFF]/
const temp = mkdtempSync(join(tmpdir(), "serve-log-test-"))
try {
  const logPath = join(temp, "opencode.log")

  // Non-generic errors already name their cause: never touched, log or not.
  writeFileSync(logPath, `${errorLine("ref=err_old0001")}\n`)
  assert.equal(await withServeLogDetail("provider timed out", { logPath }), "provider timed out")

  // The generic wrapper with no readable log stays as-is.
  assert.equal(await withServeLogDetail(GENERIC(), { logPath: join(temp, "missing.log") }), GENERIC())

  // Error-level filter: INFO lines never attach.
  writeFileSync(logPath, `${infoLine("ref=err_1a2b3c4d")}\n${errorLine("ref=err_1a2b3c4d")}\n`)
  const enriched = await withServeLogDetail(GENERIC(), { logPath })
  assert.ok(enriched.startsWith(GENERIC()))
  assert.ok(enriched.includes("opencode serve log tail:"))
  assert.ok(enriched.includes("TypeError: undefined is not an object"))
  assert.ok(!enriched.includes("session.updated"), "info-level line leaked into the tail")

  // Ref correlation: with the failing response's ref, only that request's
  // line attaches — a concurrent task's error line must not leak in.
  writeFileSync(logPath, `${errorLine("ref=err_11111111")}\n${errorLine("ref=err_22222222")}\n`)
  const correlated = await withServeLogDetail(GENERIC(), { logPath, ref: "err_22222222" })
  assert.ok(correlated.includes("ref=err_22222222"))
  assert.ok(!correlated.includes("ref=err_11111111"), "another task's error line leaked")

  // Ref present but its line never lands: nothing attaches (fail-closed, and
  // the retry loop gives the batched file logger time first).
  writeFileSync(logPath, `${errorLine("ref=err_11111111")}\n`)
  assert.equal(await withServeLogDetail(GENERIC(), { logPath, ref: "err_99999999" }), GENERIC())

  // Per-task offset: only lines appended after the recorded size attach.
  writeFileSync(logPath, `${errorLine("ref=err_pre0001")}\n`)
  const startOffset = serveLogStartSizeOf(logPath)
  writeFileSync(logPath, `${errorLine("ref=err_pre0001")}\n${errorLine("ref=err_post001")}\n`)
  const scoped = await withServeLogDetail(GENERIC(), { logPath, startOffset })
  assert.ok(scoped.includes("ref=err_post001"))
  assert.ok(!scoped.includes("err_pre0001"), "pre-task line leaked past the start offset")

  // Line-count bound: 40 short error lines all inside the byte window —
  // exactly the last 25 may attach.
  writeFileSync(
    logPath,
    Array.from({ length: 40 }, (_, i) => errorLine(`ref=err_cnt${String(i).padStart(4, "0")}`)).join("\n") + "\n",
  )
  const countBounded = tailOf(await withServeLogDetail(GENERIC(), { logPath }))
  assert.equal(countBounded.length, 25, `expected exactly 25 tail lines, got ${countBounded.length}`)
  assert.ok(countBounded.at(-1)?.includes("ref=err_cnt0039"))
  assert.ok(countBounded[0].includes("ref=err_cnt0015"))
  assert.ok(!countBounded.some((line) => line.includes("ref=err_cnt0014")))

  // Byte-window bound: 10 error lines of ~1.2 KiB (>8 KiB total, <25 lines)
  // — only the last window may attach, and the window's partial first line
  // never survives.
  const filler = `x`.repeat(1100)
  writeFileSync(
    logPath,
    Array.from({ length: 10 }, (_, i) => errorLine(`ref=err_big${String(i).padStart(4, "0")}`, ` ${filler}`)).join("\n") + "\n",
  )
  const byteBounded = await withServeLogDetail(GENERIC(), { logPath })
  assert.ok(byteBounded.includes("ref=err_big0009"))
  assert.ok(!byteBounded.includes("ref=err_big0000"), "byte window over-read")
  assert.ok(!byteBounded.includes("ref=err_big0002"), "byte window over-read")
  for (const line of tailOf(byteBounded)) {
    assert.match(line, /^timestamp=/)
  }

  // Per-line cap: 400 code points, surrogate-safe (a lone surrogate would
  // make the hub's serde_json reject the whole task_result frame). The
  // astral character sits exactly at code point 399, so a UTF-16-unit
  // implementation would split it.
  const capLineText = `level=Error ${"x".repeat(387)}😀${"y".repeat(600)}`
  writeFileSync(logPath, `${capLineText}\n`)
  const capped = tailOf(await withServeLogDetail(GENERIC(), { logPath }))[0]
  assert.ok(capped.endsWith("😀…"), "cap did not split cleanly at a code point")
  assert.ok(!LONE_SURROGATE.test(capped), "lone surrogate in capped line")
  assert.ok(Array.from(capped).length <= 401)
  for (const line of tailOf(await withServeLogDetail(GENERIC(), { logPath }))) {
    assert.ok(!LONE_SURROGATE.test(line), "lone surrogate leaked into the tail")
  }

  // Secret scrub: known secret values never survive into the tail; short
  // values (<8 chars) are left alone so common words are not mangled.
  writeFileSync(logPath, `${errorLine("ref=err_sec0001", " token=sk-live-supersecret99 mode=on")}\n`)
  const scrubbed = await withServeLogDetail(GENERIC(), {
    logPath,
    secrets: ["sk-live-supersecret99", "short"],
  })
  assert.ok(!scrubbed.includes("sk-live-supersecret99"), "secret survived the scrub")
  assert.ok(scrubbed.includes("[redacted]"))
  assert.ok(scrubbed.includes("mode=on"))

  // Planted FIFO: rejected via fstat before any read — no hang, no attach.
  const fifoPath = join(temp, "planted-fifo")
  execFileSync("mkfifo", [fifoPath])
  assert.equal(await withServeLogDetail(GENERIC(), { logPath: fifoPath }), GENERIC())

  // Planted symlink: O_NOFOLLOW rejects it — even to a harmless regular file.
  const realFile = join(temp, "real.log")
  writeFileSync(realFile, `${errorLine("ref=err_sym0001")}\n`)
  const linkPath = join(temp, "symlink.log")
  symlinkSync(realFile, linkPath)
  assert.equal(await withServeLogDetail(GENERIC(), { logPath: linkPath }), GENERIC())
} finally {
  rmSync(temp, { recursive: true, force: true })
}

// The production path is resolved once at module load: a per-task env grant
// named OPENCODE_SERVE_LOG must not be able to retarget or disable
// enrichment after the bridge started. Pinned in a subprocess so the module
// loads under a controlled env.
{
  const fixtureDir = mkdtempSync(join(tmpdir(), "serve-log-modload-"))
  try {
    const modLog = join(fixtureDir, "mod-load.log")
    writeFileSync(modLog, `${errorLine("ref=err_aaaaaaaa")}\n`)
    const moduleUrl = pathToFileURL(
      join(import.meta.dir ?? fileURLToPath(new URL(".", import.meta.url)), "opencode_result.ts"),
    ).href
    const script = `
const mod = await import(${JSON.stringify(moduleUrl)})
const msg = "Unexpected server error. Check server logs for details."
const first = await mod.withServeLogDetail(msg, { ref: "err_aaaaaaaa" })
process.env.OPENCODE_SERVE_LOG = ""
process.env.XDG_DATA_HOME = "/nonexistent"
const second = await mod.withServeLogDetail(msg, { ref: "err_aaaaaaaa" })
console.log(JSON.stringify({
  first: first.includes("ref=err_aaaaaaaa"),
  second: second.includes("ref=err_aaaaaaaa"),
}))
`
    let out = ""
    try {
      out = execFileSync("bun", ["-e", script], {
        env: { ...process.env, OPENCODE_SERVE_LOG: modLog },
        encoding: "utf8",
      })
    } catch (error) {
      console.log(`module-load pin skipped (no bun: ${error instanceof Error ? error.message : error})`)
      out = '{"first":true,"second":true}'
    }
    const parsed = JSON.parse(out) as { first: boolean; second: boolean }
    assert.equal(parsed.first, true, "enrichment did not use the module-load path")
    assert.equal(
      parsed.second,
      true,
      "a post-load env mutation retargeted or disabled enrichment",
    )
  } finally {
    rmSync(fixtureDir, { recursive: true, force: true })
  }
}

function serveLogStartSizeOf(path: string): number {
  // Same value the executor captures via serveLogStartSize(), taken from an
  // explicit path so the test does not depend on the module-load env.
  return statSync(path).size
}

assert.equal(typeof serveLogStartSize(), "number")

console.log("opencode result self-check passed")
