import assert from "node:assert/strict"
import { execFileSync } from "node:child_process"
import { appendFileSync, mkdtempSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs"
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
  // Real formatter shape (run-92 FJ-1): opencode's logging.ts emits
  // ["level", output.level] and effect's formatStructured uppercases it, so
  // a real 500 logs `level=ERROR`. Values with whitespace/quotes are
  // JSON-quoted by the logger.
  return (
    `timestamp=2026-10-12T00:00:00.000Z level=ERROR run=run01 message=failed ${tag} ` +
    `error=TypeError: undefined is not an object (evaluating 'a.name')${extra}`
  )
}

function infoLine(tag: string): string {
  return `timestamp=2026-10-12T00:00:00.000Z level=INFO run=run01 message=session.updated ${tag}`
}

function tailOf(enriched: string): string[] {
  return enriched.split("opencode serve log tail:\n")[1]?.split("\n") ?? []
}

const LONE_SURROGATE = /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?:[^\uD800-\uDBFF]|^)[\uDC00-\uDFFF]/
const temp = mkdtempSync(join(tmpdir(), "serve-log-test-"))
try {
  const logPath = join(temp, "opencode.log")

  // Non-generic errors already name their cause: never touched, log or not.
  writeFileSync(logPath, `${errorLine("ref=err_01d00001")}\n`)
  assert.equal(await withServeLogDetail("provider timed out", { logPath }), "provider timed out")

  // The generic wrapper with no readable log stays as-is.
  assert.equal(
    await withServeLogDetail(GENERIC(), { logPath: join(temp, "missing.log"), ref: "err_1a2b3c4d" }),
    GENERIC(),
  )

  // FJ-5: without a ref the enrichment is fail-closed — concurrent tasks of
  // other users share this serve log, and uncorrelated ERROR lines must
  // never attach to this job's record.
  writeFileSync(logPath, `${errorLine("ref=err_3a3a3a3a")}\n`)
  assert.equal(await withServeLogDetail(GENERIC(), { logPath }), GENERIC())

  // FJ-5: the ref match is the exact space-delimited token — a longer ref
  // sharing the same prefix must not satisfy a substring test.
  writeFileSync(logPath, `${errorLine("ref=err_444444444")}\n`)
  assert.equal(await withServeLogDetail(GENERIC(), { logPath, ref: "err_44444444" }), GENERIC())

  // Error-level filter: INFO lines never attach.
  writeFileSync(logPath, `${infoLine("ref=err_1a2b3c4d")}\n${errorLine("ref=err_1a2b3c4d")}\n`)
  const enriched = await withServeLogDetail(GENERIC(), { logPath, ref: "err_1a2b3c4d" })
  assert.ok(enriched.startsWith(GENERIC()))
  assert.ok(enriched.includes("opencode serve log tail:"))
  assert.ok(enriched.includes("TypeError: undefined is not an object"))
  assert.ok(!enriched.includes("session.updated"), "info-level line leaked into the tail")

  // FJ-1 regression pin: `level=Error` (the pre-FJ-1 fixture format) is not
  // what the real formatter writes — it must never attach. A line in the
  // real `level=ERROR` format attaches (next test).
  const wrongLevel = errorLine("ref=err_5a1e0001").replace("level=ERROR", "level=Error")
  assert.ok(wrongLevel.includes("level=Error"))
  writeFileSync(logPath, `${wrongLevel}\n`)
  assert.equal(await withServeLogDetail(GENERIC(), { logPath, ref: "err_5a1e0001" }), GENERIC())

  // FJ-1: a line produced by the real formatter shape (uppercased level
  // token, JSON-quoted error/cause values) attaches end to end.
  const realFormatterLine =
    'timestamp=2026-10-13T00:00:00.000Z level=ERROR run=run4242 message=failed ref=err_9e1c0de5 ' +
    'error="TypeError: undefined is not an object (evaluating \'a.name\')" ' +
    'cause="Error: prompt assembly crashed"'
  writeFileSync(logPath, `${realFormatterLine}\n`)
  const realFormat = await withServeLogDetail(GENERIC(), { logPath, ref: "err_9e1c0de5" })
  assert.ok(realFormat.includes("ref=err_9e1c0de5"), "real-formatter line did not attach")
  assert.ok(realFormat.includes("prompt assembly crashed"))

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

  // FJ-2: serve's Effect file logger flushes on a 1 s batch window — the
  // cause line can land well after the failing HTTP response. The retry
  // budget must still find it (the old 5×100 ms budget missed a 700 ms
  // flush about 60% of the time; this test fails at 5 retries).
  {
    const latePath = join(temp, "late.log")
    writeFileSync(latePath, "")
    const lateRef = "err_1a7ef00d"
    const timer = setTimeout(() => {
      appendFileSync(latePath, `${errorLine(`ref=${lateRef}`)}\n`)
    }, 700)
    try {
      const late = await withServeLogDetail(GENERIC(), { logPath: latePath, ref: lateRef })
      assert.ok(late.includes(`ref=${lateRef}`), "late-flushed crash line missed by the retry budget")
    } finally {
      clearTimeout(timer)
    }
  }

  // Per-task offset: only lines appended after the recorded size attach.
  // FU-1: use the same ref so only the offset excludes the pre-task line.
  writeFileSync(logPath, `${errorLine("ref=err_01d0c0de phase=PRETASK")}\n`)
  const startOffset = serveLogStartSizeOf(logPath)
  appendFileSync(logPath, `${errorLine("ref=err_01d0c0de phase=INTASK")}\n`)
  const scoped = await withServeLogDetail(GENERIC(), { logPath, startOffset, ref: "err_01d0c0de" })
  assert.ok(scoped.includes("phase=INTASK"))
  assert.ok(!scoped.includes("phase=PRETASK"), "pre-task line leaked past the start offset")

  // Line-count bound: 40 short error lines (same request ref) all inside the
  // byte window — exactly the last 25 may attach.
  writeFileSync(
    logPath,
    Array.from({ length: 40 }, (_, i) => errorLine("ref=err_c0ffee01", ` idx=${String(i).padStart(4, "0")}`)).join("\n") + "\n",
  )
  const countBounded = tailOf(await withServeLogDetail(GENERIC(), { logPath, ref: "err_c0ffee01" }))
  assert.equal(countBounded.length, 25, `expected exactly 25 tail lines, got ${countBounded.length}`)
  assert.ok(countBounded.at(-1)?.includes("idx=0039"))
  assert.ok(countBounded[0].includes("idx=0015"))
  assert.ok(!countBounded.some((line) => line.includes("idx=0014")))

  // Byte-window bound: 10 error lines of ~1.2 KiB (>8 KiB total, <25 lines)
  // — only the last window may attach, and the window's partial first line
  // never survives.
  const filler = `x`.repeat(1100)
  writeFileSync(
    logPath,
    Array.from({ length: 10 }, (_, i) =>
      errorLine("ref=err_b166d00d", ` idx=${String(i).padStart(4, "0")} ${filler}`),
    ).join("\n") + "\n",
  )
  const byteBounded = tailOf(await withServeLogDetail(GENERIC(), { logPath, ref: "err_b166d00d" }))
  assert.ok(byteBounded.some((line) => line.includes("idx=0009")))
  assert.ok(!byteBounded.some((line) => line.includes("idx=0000")), "byte window over-read")
  assert.ok(!byteBounded.some((line) => line.includes("idx=0002")), "byte window over-read")
  for (const line of byteBounded) {
    assert.match(line, /^timestamp=/)
  }

  // Per-line cap: 400 code points, surrogate-safe (a lone surrogate would
  // make the hub's serde_json reject the whole task_result frame). The
  // astral character sits exactly at code point 399, so a UTF-16-unit
  // implementation would split it.
  const capPrefix = "timestamp=2026-10-12T00:00:00.000Z level=ERROR run=run01 ref=err_5eedc0de message=failed "
  const capLineText = `${capPrefix}${"x".repeat(399 - Array.from(capPrefix).length)}😀${"y".repeat(600)}`
  writeFileSync(logPath, `${capLineText}\n`)
  const capped = tailOf(await withServeLogDetail(GENERIC(), { logPath, ref: "err_5eedc0de" }))[0]
  assert.ok(capped.endsWith("😀…"), "cap did not split cleanly at a code point")
  assert.ok(!LONE_SURROGATE.test(capped), "lone surrogate in capped line")
  assert.ok(Array.from(capped).length <= 401)
  for (const line of tailOf(await withServeLogDetail(GENERIC(), { logPath, ref: "err_5eedc0de" }))) {
    assert.ok(!LONE_SURROGATE.test(line), "lone surrogate leaked into the tail")
  }

  // Secret scrub: known secret values never survive into the tail; short
  // values (<8 chars) are left alone so common words are not mangled (pins
  // MIN_SCRUB_SECRET_LENGTH — a 0 threshold would mangle them).
  writeFileSync(logPath, `${errorLine("ref=err_5ec0e7a1", " token=sk-live-supersecret99 mode=on sub=sh0rt1")}\n`)
  const scrubbed = await withServeLogDetail(GENERIC(), {
    logPath,
    ref: "err_5ec0e7a1",
    secrets: ["sk-live-supersecret99", "short", "sh0rt1"],
  })
  assert.ok(!scrubbed.includes("sk-live-supersecret99"), "secret survived the scrub")
  assert.ok(scrubbed.includes("[redacted]"))
  assert.ok(scrubbed.includes("mode=on"))
  assert.ok(scrubbed.includes("sub=sh0rt1"), "short value was mangled by the scrub")

  // FJ-6: the logger JSON-quotes values containing whitespace/quotes, so a
  // secret with those characters appears escaped in the log — the escaped
  // rendering must be scrubbed too.
  {
    const quoted = `sk-live-pa"ss\\wo rd`
    const escaped = JSON.stringify(quoted).slice(1, -1)
    writeFileSync(logPath, `${errorLine("ref=err_6a1e0001", ` token="${escaped}"`)}\n`)
    const escapedScrub = await withServeLogDetail(GENERIC(), {
      logPath,
      ref: "err_6a1e0001",
      secrets: [quoted],
    })
    assert.ok(!escapedScrub.includes(escaped), "JSON-escaped secret survived the scrub")
    assert.ok(!escapedScrub.includes(quoted), "raw secret survived next to its escaped form")
    assert.ok(escapedScrub.includes("[redacted]"))
  }

  // FJ-6: longest first — a secret containing a shorter secret must not
  // leave its suffix behind after the shorter one is replaced.
  {
    const inner = "innersecret"
    const outer = `sk-live-${inner}wrap`
    writeFileSync(logPath, `${errorLine("ref=err_6a1e0002", ` token=${outer}`)}\n`)
    const ordered = await withServeLogDetail(GENERIC(), {
      logPath,
      ref: "err_6a1e0002",
      secrets: [inner, outer],
    })
    assert.ok(!ordered.includes("wrap"), "shortest-first replacement left the longer secret's suffix")
    assert.ok(!ordered.includes(inner) && !ordered.includes(outer))
  }

  // FJ-6: no count cap — every secret in the list is scrubbed (a cap of 1
  // would let the rest through).
  {
    const many = Array.from({ length: 5 }, (_, i) => `sk-live-batch${i}secret`)
    writeFileSync(
      logPath,
      `${errorLine("ref=err_6a1e0003", ` token=${many.join(" ")}`)}\n`,
    )
    const uncapped = await withServeLogDetail(GENERIC(), {
      logPath,
      ref: "err_6a1e0003",
      secrets: many,
    })
    for (const secret of many) {
      assert.ok(!uncapped.includes(secret), `secret ${secret.slice(-8)} survived a capped scrub`)
    }
  }

  // Planted FIFO: rejected via fstat before any read — no hang, no attach.
  const fifoPath = join(temp, "planted-fifo")
  execFileSync("mkfifo", [fifoPath])
  assert.equal(await withServeLogDetail(GENERIC(), { logPath: fifoPath, ref: "err_7a1e0001" }), GENERIC())

  // Planted symlink: O_NOFOLLOW rejects it — even to a harmless regular file.
  const realFile = join(temp, "real.log")
  writeFileSync(realFile, `${errorLine("ref=err_7a1e0002")}\n`)
  const linkPath = join(temp, "symlink.log")
  symlinkSync(realFile, linkPath)
  assert.equal(await withServeLogDetail(GENERIC(), { logPath: linkPath, ref: "err_7a1e0002" }), GENERIC())
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
    let bunAvailable = true
    try {
      out = execFileSync("bun", ["-e", script], {
        env: { ...process.env, OPENCODE_SERVE_LOG: modLog },
        encoding: "utf8",
      })
    } catch (error) {
      if ((error as { code?: string }).code !== "ENOENT") throw error
      bunAvailable = false
    }
    if (!bunAvailable) {
      console.log("module-load pin skipped (bun not on PATH)")
    } else {
      const parsed = JSON.parse(out) as { first: boolean; second: boolean }
      assert.equal(parsed.first, true, "enrichment did not use the module-load path")
      assert.equal(
        parsed.second,
        true,
        "a post-load env mutation retargeted or disabled enrichment",
      )
    }
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
