/**
 * guard_config.ts — per-task re-assertion of the WI#206 PreToolUse guard hook.
 *
 * WI#847: the effective CLI config ($KIMI_CODE_HOME/config.toml) is written by
 * entrypoint-task-setup.sh AS THE TASK UID, inside the task's own home — so the
 * guard registry is owned by the same untrusted identity the hook polices. A
 * task can rewrite that file (`echo '' >`, `sed -i`, a heredoc, a python
 * open().write(), truncate) and drop the `[[hooks]]` entry; the setup script
 * only runs once per container boot, so every LATER task in the container
 * would start with no PreToolUse guard and nothing failing loudly.
 *
 * The executor therefore re-asserts the hook immediately before every spawn:
 * a rewrite cannot outlive the task that made it, and a config that cannot be
 * repaired fails the task instead of spawning it unguarded.
 *
 * The presence check is a small TOML structure scan rather than a regex over
 * the raw text: a line that merely *looks* like the hook entry inside a
 * comment or a multi-line string must not count, because the CLI would not
 * register it either.
 */
import { lstatSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"

/** The command the baked config and entrypoint-task-setup.sh register. */
export const GUARD_HOOK_COMMAND = "node /app/hooks/headless_command_guard.mjs"

const GUARD_HOOK_KEYS = new Set(["event", "matcher", "command", "timeout"])

/**
 * Canonical `[[hooks]]` entry, byte-identical to the baked config's block
 * (config/config.toml) and to entrypoint-task-setup.sh's hook_block().
 */
export function guardHookBlock(): string {
  return (
    "\n[[hooks]]\n" +
    'event = "PreToolUse"\n' +
    'matcher = "Bash"\n' +
    `command = "${GUARD_HOOK_COMMAND}"\n` +
    "timeout = 10\n"
  )
}

type ScanState = "none" | "ml-basic" | "ml-literal"

interface ScannedLine {
  /** The line with string bodies and comments blanked to spaces. */
  structure: string
  /** Multi-line string state carried into the next line. */
  state: ScanState
}

function blanked(count: number): string {
  return " ".repeat(Math.max(0, count))
}

/** Index of the next unescaped `needle` in `text`, or -1. */
function findUnescaped(text: string, needle: string): number {
  let from = 0
  for (;;) {
    const index = text.indexOf(needle, from)
    if (index < 0) return -1
    let backslashes = 0
    for (let i = index - 1; i >= 0 && text[i] === "\\"; i--) backslashes += 1
    if (backslashes % 2 === 0) return index
    from = index + 1
  }
}

/** Index of the closing quote of a single-line string starting at 0, or -1. */
function findLineStringEnd(text: string, quote: string): number {
  for (let i = 1; i < text.length; i++) {
    const char = text[i]
    if (quote === '"' && char === "\\") {
      i += 1
      continue
    }
    if (char === quote) return i
  }
  return -1
}

/**
 * Blank out string bodies and comments on one line, keeping everything the
 * TOML structure exposes (headers, keys, `=`, bare values). Multi-line string
 * state is threaded across lines so a `[[hooks]]` inside `"""…"""` is blanked.
 */
function scanLine(line: string, state: ScanState): ScannedLine {
  let current = state
  let index = 0
  let structure = ""
  while (index < line.length) {
    const rest = line.slice(index)
    if (current === "ml-basic" || current === "ml-literal") {
      const terminator = current === "ml-basic" ? '"""' : "'''"
      const close = current === "ml-basic" ? findUnescaped(rest, terminator) : rest.indexOf(terminator)
      if (close < 0) {
        structure += blanked(rest.length)
        index = line.length
        continue
      }
      structure += blanked(close + terminator.length)
      index += close + terminator.length
      current = "none"
      continue
    }
    if (rest.startsWith('"""') || rest.startsWith("'''")) {
      const terminator = rest.startsWith('"""') ? '"""' : "'''"
      const after = rest.slice(terminator.length)
      const close = terminator === '"""' ? findUnescaped(after, terminator) : after.indexOf(terminator)
      if (close < 0) {
        structure += blanked(rest.length)
        index = line.length
        current = terminator === '"""' ? "ml-basic" : "ml-literal"
        continue
      }
      const consumed = terminator.length + close + terminator.length
      structure += blanked(consumed)
      index += consumed
      continue
    }
    const char = rest[0]
    if (char === "#") {
      structure += blanked(rest.length)
      index = line.length
      continue
    }
    if (char === '"' || char === "'") {
      const close = findLineStringEnd(rest, char)
      if (close < 0) {
        structure += blanked(rest.length)
        index = line.length
        continue
      }
      structure += blanked(close + 1)
      index += close + 1
      continue
    }
    structure += char
    index += 1
  }
  return { structure, state: current }
}

const SIMPLE_VALUE =
  /^\s*([A-Za-z0-9_-]+)\s*=\s*(?:"([^"\\]*)"|'([^']*)'|(-?\d+)|(true|false))\s*(?:#.*)?$/

/** Parse `key = "string"` / `key = 10` / `key = true`; anything else is not simple. */
function parseSimpleValue(line: string): { key: string; value: string } | undefined {
  const match = SIMPLE_VALUE.exec(line)
  if (!match) return undefined
  const value = match[2] ?? match[3] ?? match[4] ?? match[5]
  if (value === undefined) return undefined
  return { key: match[1] as string, value }
}

/**
 * True when `config` registers the guard hook in a top-level `[[hooks]]`
 * array-of-tables element. Conservative by design: an entry this scan cannot
 * read as a plain string (escapes, quoted header, sub-table) reports false and
 * the caller appends the canonical block, so a false negative is harmless and
 * a false positive is what must not happen.
 */
export function hasGuardHook(config: string): boolean {
  let state: ScanState = "none"
  let values: Map<string, string> | undefined
  let clean = true
  const elements: Array<{ values: Map<string, string>; clean: boolean }> = []
  const closeElement = () => {
    if (!values) return
    elements.push({ values, clean })
    values = undefined
    clean = true
  }
  for (const line of config.split("\n")) {
    const entering = state
    const scanned = scanLine(line, state)
    state = scanned.state
    const structure = scanned.structure.trim()
    if (structure.startsWith("[")) {
      // TOML table headers are absolute paths, so any header at line start
      // closes the current hooks element — including [[hooks]] itself.
      closeElement()
      if (structure === "[[hooks]]") values = new Map()
      continue
    }
    if (!values) continue
    if (entering !== "none") {
      // The line begins inside a multi-line string opened in this element.
      clean = false
      continue
    }
    if (structure.length === 0) continue // blank or comment-only
    const pair = parseSimpleValue(line)
    if (pair === undefined || values.has(pair.key)) {
      clean = false
      continue
    }
    values.set(pair.key, pair.value)
  }
  closeElement()
  return elements.some((element) => {
    const entries = element.values
    if (!element.clean || entries.size < 3 || entries.size > 4) return false
    for (const key of entries.keys()) {
      if (!GUARD_HOOK_KEYS.has(key)) return false
    }
    return (
      entries.get("event") === "PreToolUse" &&
      entries.get("matcher") === "Bash" &&
      entries.get("command") === GUARD_HOOK_COMMAND &&
      (entries.get("timeout") === undefined || entries.get("timeout") === "10")
    )
  })
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

/**
 * Ensure `$KIMI_CODE_HOME/config.toml` registers the guard hook, appending the
 * canonical block when the task-side copy lost it. Runs before the spawn, as
 * whichever identity owns the file (container root in the single-container
 * bridge, the task uid in the delegated task runner); the in-place write keeps
 * the file's ownership and mode so the CLI can still read it.
 *
 * `enforce` (managed container shapes) turns a missing config into a task
 * failure; host-side dev runs have no managed config and skip it.
 */
export function ensureGuardHook(home: string | undefined, taskId: string, enforce: boolean): void {
  if (!home) {
    if (enforce) {
      throw new Error(
        `KIMI_CODE_HOME is unset: cannot verify the PreToolUse guard hook before task ${taskId} (WI#847)`,
      )
    }
    return
  }
  const path = join(home, "config.toml")
  let stats: ReturnType<typeof lstatSync>
  try {
    stats = lstatSync(path)
  } catch (error) {
    const missing = (error as NodeJS.ErrnoException).code === "ENOENT"
    if (missing && !enforce) return
    throw new Error(
      `cannot verify the PreToolUse guard hook: ${path} is unavailable (${errorMessage(error)}); refusing to spawn task ${taskId} unguarded (WI#847)`,
    )
  }
  if (!stats.isFile()) {
    throw new Error(
      `cannot verify the PreToolUse guard hook: ${path} is not a regular file; refusing to spawn task ${taskId} unguarded (WI#847)`,
    )
  }
  let config: string
  try {
    config = readFileSync(path, "utf8")
  } catch (error) {
    throw new Error(
      `cannot verify the PreToolUse guard hook: ${path} is unreadable (${errorMessage(error)}); refusing to spawn task ${taskId} unguarded (WI#847)`,
    )
  }
  if (hasGuardHook(config)) return
  const repaired = config + guardHookBlock()
  if (!hasGuardHook(repaired)) {
    throw new Error(
      `cannot re-assert the PreToolUse guard hook in ${path}: the file ends inside a string or table, so no appended [[hooks]] entry would register; refusing to spawn task ${taskId} unguarded (WI#847)`,
    )
  }
  try {
    writeFileSync(path, repaired, "utf8")
  } catch (error) {
    throw new Error(
      `cannot re-assert the PreToolUse guard hook in ${path} (${errorMessage(error)}); refusing to spawn task ${taskId} unguarded (WI#847)`,
    )
  }
  console.warn(
    `[kimi executor] re-asserted the PreToolUse guard hook in ${path} for task ${taskId}: a previous task had rewritten the config without it (WI#847)`,
  )
}
