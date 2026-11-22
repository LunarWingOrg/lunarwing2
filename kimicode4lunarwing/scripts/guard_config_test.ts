import assert from "node:assert/strict"
import { test } from "node:test"
import { chmodSync, closeSync, ftruncateSync, mkdtempSync, openSync, readFileSync, statSync, symlinkSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { ensureGuardHook, guardHookBlock, hasGuardHook } from "./guard_config.js"

const bakedConfig = readFileSync(
  fileURLToPath(new URL("../config/config.toml", import.meta.url)),
  "utf8",
)

/** The baked config with the WI#206 guard entry removed — the measured
 * task-authored rewrite shape (WI#847). */
function strippedConfig(): string {
  return bakedConfig.replace(/\n\[\[hooks\]\][\s\S]*?\ntimeout = 10\n/, "\n")
}

function tempHome(config: string | undefined): { home: string; path: string } {
  const home = mkdtempSync(join(tmpdir(), "kimi-guard-"))
  const path = join(home, "config.toml")
  if (config !== undefined) writeFileSync(path, config)
  return { home, path }
}

test("hasGuardHook: the baked config registers the guard hook", () => {
  assert.equal(hasGuardHook(bakedConfig), true)
})

test("hasGuardHook: the stripped config (measured task rewrite) does not", () => {
  const stripped = strippedConfig()
  assert.ok(!stripped.includes("headless_command_guard.mjs"), "fixture lost its hook block")
  assert.equal(hasGuardHook(stripped), false)
})

test("hasGuardHook: a canonical block appended at EOF registers", () => {
  assert.equal(hasGuardHook(strippedConfig() + guardHookBlock()), true)
})

test("hasGuardHook: an entry inside a multi-line basic string does not count", () => {
  const decoy =
    'note = """\n[[hooks]]\nevent = "PreToolUse"\nmatcher = "Bash"\n' +
    'command = "node /app/hooks/headless_command_guard.mjs"\ntimeout = 10\n"""\n'
  assert.equal(hasGuardHook(strippedConfig() + decoy), false)
})

test("hasGuardHook: an entry inside a multi-line literal string does not count", () => {
  const decoy =
    "note = '''\n[[hooks]]\nevent = \"PreToolUse\"\nmatcher = \"Bash\"\n" +
    'command = "node /app/hooks/headless_command_guard.mjs"\ntimeout = 10\n\'\'\'\n'
  assert.equal(hasGuardHook(strippedConfig() + decoy), false)
})

test("hasGuardHook: a commented-out entry does not count", () => {
  const decoy =
    '# [[hooks]]\n# event = "PreToolUse"\n# matcher = "Bash"\n' +
    '# command = "node /app/hooks/headless_command_guard.mjs"\n# timeout = 10\n'
  assert.equal(hasGuardHook(strippedConfig() + decoy), false)
})

test("hasGuardHook: an entry that cannot disable the guard is not accepted", () => {
  const disabled =
    '\n[[hooks]]\nevent = "PreToolUse"\nmatcher = "Bash"\n' +
    'command = "node /app/hooks/headless_command_guard.mjs"\ntimeout = 10\nenabled = false\n'
  assert.equal(hasGuardHook(strippedConfig() + disabled), false)
  const zeroTimeout =
    '\n[[hooks]]\nevent = "PreToolUse"\nmatcher = "Bash"\n' +
    'command = "node /app/hooks/headless_command_guard.mjs"\ntimeout = 0\n'
  assert.equal(hasGuardHook(strippedConfig() + zeroTimeout), false)
  const wrongCommand =
    '\n[[hooks]]\nevent = "PreToolUse"\nmatcher = "Bash"\ncommand = "true"\ntimeout = 10\n'
  assert.equal(hasGuardHook(strippedConfig() + wrongCommand), false)
})

test("hasGuardHook: a rival Bash hook is not vouched for, a non-Bash hook is ignored", () => {
  const rival =
    '\n[[hooks]]\nevent = "PreToolUse"\nmatcher = "Bash"\ncommand = "true"\ntimeout = 10\n'
  assert.equal(hasGuardHook(bakedConfig + rival), false)
  const otherEvent =
    '\n[[hooks]]\nevent = "PostToolUse"\nmatcher = "Bash"\ncommand = "true"\ntimeout = 10\n'
  assert.equal(hasGuardHook(bakedConfig + otherEvent), true)
})

test("ensureGuardHook: repairs a task-stripped config in place and keeps mode", () => {
  const { home, path } = tempHome(strippedConfig())
  chmodSync(path, 0o600)
  const warnings: string[] = []
  const originalWarn = console.warn
  console.warn = (...args: unknown[]) => { warnings.push(args.map(String).join(" ")) }
  try {
    ensureGuardHook(home, "task-strip", false)
  } finally {
    console.warn = originalWarn
  }
  assert.equal(hasGuardHook(readFileSync(path, "utf8")), true)
  assert.equal(statSync(path).mode & 0o777, 0o600, "the CLI's read mode must survive the repair")
  assert.ok(warnings.some((line) => line.includes("re-asserted")), "the repair must be logged")
  // The operator's other settings are untouched.
  const repaired = readFileSync(path, "utf8")
  assert.match(repaired, /default_model = "tensorzero\/FrontierCODEK"/)
  assert.match(repaired, /dangerous_command_guard = true/)
})

test("ensureGuardHook: an intact config is left byte-identical", () => {
  const { home, path } = tempHome(bakedConfig)
  const before = readFileSync(path, "utf8")
  ensureGuardHook(home, "task-intact", false)
  assert.equal(readFileSync(path, "utf8"), before)
})

test("ensureGuardHook: a missing config fails closed only when enforced", () => {
  const { home } = tempHome(undefined)
  ensureGuardHook(home, "task-dev", false)
  assert.throws(() => ensureGuardHook(home, "task-managed", true), /unavailable.*WI#847/)
  assert.throws(() => ensureGuardHook(undefined, "task-managed", true), /KIMI_CODE_HOME is unset/)
})

test("ensureGuardHook: a symlinked config is refused", () => {
  const { home } = tempHome(undefined)
  const target = join(home, "task-owned.toml")
  writeFileSync(target, strippedConfig())
  symlinkSync(target, join(home, "config.toml"))
  assert.throws(() => ensureGuardHook(home, "task-link", false), /not a regular file/)
})

test("ensureGuardHook: an oversized task-owned config is refused, not loaded", () => {
  // Sparse file: big size, no disk cost. The bridge process is shared with
  // every other task in the container.
  const { home, path } = tempHome(undefined)
  const fd = openSync(path, "w")
  try {
    ftruncateSync(fd, 2 * 1024 * 1024)
  } finally {
    closeSync(fd)
  }
  assert.throws(() => ensureGuardHook(home, "task-huge", false), /over the 1048576-byte limit/)
})

test("ensureGuardHook: an unrepairable tail fails the task instead of spawning unguarded", () => {
  // An unterminated multi-line string swallows anything appended after it, so
  // the repair cannot register a hook and must not be attempted.
  const { home, path } = tempHome(strippedConfig() + 'broken = """\n')
  assert.throws(() => ensureGuardHook(home, "task-tail", false), /ends inside a string or table/)
  assert.equal(readFileSync(path, "utf8").endsWith('broken = """\n'), true, "the file must not be rewritten")
})
