/**
 * sdk_surface.ts — Single source of truth for the @opencode/client promise
 * surface the opencode2 bridge calls (WI#329).
 *
 * Why this module exists: several v2.0.x bump entries "verified" SDK methods
 * by diffing protocol/OpenAPI identifier strings, and the bridge shipped a
 * call to session.message.list — a path that never existed on the generated
 * promise client (session.message exposes only get; session message listing
 * is the top-level message.list). The runtime TypeError was silent because
 * the executor fell back to streamed deltas (WI#320 finding F7).
 *
 * Imported by:
 *  - scripts/sdk_surface_assert.ts — the image-build gate: constructs the
 *    real client against a dummy base URL (no server contact) and asserts
 *    typeof for every path below, plus asserts every BRIDGE_SDK_EVENTS name
 *    is still a member literal of the pinned generated V2Event union (the
 *    type event.subscribe actually yields), so SDK drift fails the build,
 *    not the runtime.
 *  - scripts/opencode_task_executor_test.ts — asserts the injected mock
 *    implements EXACTLY this surface (both directions), binds the executor's
 *    real call sites / event literals extracted from every non-test
 *    scripts/*.ts source, AND wraps the mock in recordingSdkProxy so every
 *    client leaf path the suite actually touches — however it was reached
 *    (alias, cast, bracket, optional call, destructuring, helper parameter)
 *    — must be listed here. A bridge call added without updating this file
 *    fails the suite instead of drifting past the gates the way WI#320 F7
 *    did.
 *
 * When the bridge starts calling a new client method or matching a new
 * event name, ADD its path here once and every gate covers it. Never shrink
 * a list to make an assertion pass — that reopens the silent-drift class
 * these gates exist to close (WI#369).
 */

/** Dotted method paths the bridge calls on the promise client. */
export const BRIDGE_SDK_METHODS = [
  "session.create",
  "session.prompt",
  "session.interrupt",
  "permission.reply",
  "event.subscribe",
  "message.list",
] as const

export type BridgeSdkMethod = (typeof BRIDGE_SDK_METHODS)[number]

/**
 * Plausible-looking paths that do NOT exist on the v2 promise client and
 * must never be called: session.message only exposes get, and there is no
 * session.permission group at all (both are top-level client groups). If a
 * future upstream pin really adds one, remove it here only after re-auditing
 * the bridge call sites against the pinned generated client.
 */
export const STALE_SDK_METHODS = [
  "session.message.list",
  "session.permission.reply",
] as const

/**
 * SSE event `type` literals the bridge matches on. Completion detection
 * depends on session.execution.{succeeded,failed,interrupted} / session.idle
 * and permission handling on permission.asked, so an upstream rename would
 * hang every task or stall permissions — the build gate asserts each name is
 * still a member literal of the pinned generated V2Event union (WI#329,
 * !1512 CY-2, scoped to the union itself by WI#369: a v1-only schema literal
 * no longer satisfies the gate).
 */
export const BRIDGE_SDK_EVENTS = [
  "permission.asked",
  "session.execution.failed",
  "session.execution.interrupted",
  "session.execution.succeeded",
  "session.idle",
  "session.text.delta",
  "session.tool.failed",
  "session.tool.success",
] as const

export type BridgeSdkEvent = (typeof BRIDGE_SDK_EVENTS)[number]

/** Resolve a dotted path ("message.list") on a nested object. */
export function resolveMethod(root: unknown, path: string): unknown {
  let current = root
  for (const segment of path.split(".")) {
    if (current === null || typeof current !== "object") return undefined
    current = (current as Record<string, unknown>)[segment]
  }
  return current
}

/** Flatten a nested method mock into sorted dotted paths, any depth ("session.message.get", ...). */
export function mockMethodPaths(mock: object): string[] {
  const paths: string[] = []
  const walk = (node: unknown, prefix: string) => {
    if (node === null || typeof node !== "object") return
    for (const [name, value] of Object.entries(node as Record<string, unknown>)) {
      const path = prefix ? `${prefix}.${name}` : name
      if (typeof value === "function") paths.push(path)
      else walk(value, path)
    }
  }
  walk(mock, "")
  return paths.sort()
}

/** A single- or double-quoted literal with no escape sequences. */
export interface StringSpan {
  /** Offset of the opening quote in the original source. */
  start: number
  /** Offset just past the closing quote in the original source. */
  end: number
  /** Inner text (quotes stripped). Only recorded for escape-free literals. */
  value: string
}

/**
 * Blank comments and string/template literal contents out of source text,
 * PRESERVING OFFSETS (every blanked character becomes a space except
 * newlines, so positions in the returned text still index the original).
 * Escape-free quoted literals are also returned as spans so callers can read
 * a literal back from the original source at an exact offset — that is how
 * extractEventTypes can require the compared operand to BE a string literal
 * without ever matching text inside one (WI#369 DF-4).
 *
 * Handles line/block comments, single/double quotes with escapes, and
 * template literals with ${}-nested code (which is scanned as code: a call
 * inside a template expression still counts). Regex literals are not
 * specially tokenized; none of the scanned bridge sources contain a regex
 * whose body holds a quote or a comment opener (such a regex would only
 * over-blank, which fails closed).
 */
export function blankCommentsAndStrings(source: string): {
  blanked: string
  strings: StringSpan[]
} {
  const blanked = source.split("")
  const strings: StringSpan[] = []
  const blank = (from: number, to: number) => {
    for (let k = from; k < to; k++) if (blanked[k] !== "\n") blanked[k] = " "
  }
  const n = source.length
  let i = 0

  const scanQuoted = (quote: string) => {
    const start = i
    i++ // opening quote
    let value = ""
    let clean = true
    let terminated = false
    while (i < n) {
      const c = source[i]!
      if (c === "\\") {
        clean = false
        i += 2
        continue
      }
      if (c === quote) {
        terminated = true
        i++
        break
      }
      if (c === "\n") break // unterminated (JS strings cannot span raw newlines)
      value += c
      i++
    }
    const end = Math.min(i, n)
    blank(start, end)
    if (terminated && clean) strings.push({ start, end, value })
  }

  const scanTemplate = () => {
    // Blank the template TEXT segments but leave ${}-expression code intact:
    // a call inside a template expression is a real call site (WI#369).
    let segmentStart = i // opening backtick
    i++
    while (i < n) {
      const c = source[i]!
      if (c === "\\") {
        i += 2
        continue
      }
      if (c === "`") {
        i++
        blank(segmentStart, i)
        return
      }
      if (c === "$" && source[i + 1] === "{") {
        blank(segmentStart, i)
        i += 2
        scanCode(true) // returns positioned AT the matching '}'
        i++
        segmentStart = i
        continue
      }
      i++
    }
    blank(segmentStart, Math.min(i, n))
  }

  const scanCode = (inTemplateExpression: boolean): void => {
    let braceDepth = 0
    while (i < n) {
      const c = source[i]!
      const next = source[i + 1]
      if (c === "/" && next === "/") {
        let j = i
        while (j < n && source[j] !== "\n") j++
        blank(i, j)
        i = j
        continue
      }
      if (c === "/" && next === "*") {
        const close = source.indexOf("*/", i + 2)
        const j = close === -1 ? n : close + 2
        blank(i, j)
        i = j
        continue
      }
      if (c === "'" || c === '"') {
        scanQuoted(c)
        continue
      }
      if (c === "`") {
        scanTemplate()
        continue
      }
      if (c === "{") braceDepth++
      if (c === "}") {
        if (inTemplateExpression && braceDepth === 0) return
        braceDepth--
      }
      i++
    }
  }

  scanCode(false)
  return { blanked: blanked.join(""), strings }
}

/**
 * Extract every client call path invoked through an `sdk` receiver from
 * bridge source, any depth, multi-line aware ("sdk.session\n  .interrupt("
 * binds). Comments and string/template literal contents are blanked first,
 * so a comment such as `// sdk.session.message.list(` or a log string can no
 * longer produce a misleading "call sites differ" failure (WI#369 DF-4).
 * Beyond the plain `sdk.<path>(` form, the receiver pattern also binds
 *  - optional calls:        sdk.session.summarize?.(…)
 *  - non-null assertions:   sdk.session!.create(…)
 *  - casts:                 (sdk as any).session.summarize(…)
 * Binding by NAME is inherently weaker than binding by behaviour, so aliases
 * (const s = sdk.session), destructuring, bracket access (sdk["session"]) and
 * helper parameters of another name are the recordingSdkProxy's job — the
 * self-test wraps the mock in it and asserts every leaf path actually
 * touched is in BRIDGE_SDK_METHODS (WI#369 DF-1).
 */
export function extractSdkCallPaths(source: string): string[] {
  const { blanked } = blankCommentsAndStrings(source)
  const paths = new Set<string>()
  const pattern =
    /(?:\(\s*sdk\s+as\s+[^()]*?\)|\bsdk)((?:\s*[?!]?\s*\.\s*[A-Za-z_$][\w$]*)+)\s*(?:[?!]\s*\.?)?\s*\(/g
  for (const match of blanked.matchAll(pattern)) {
    const captured = match[1]
    if (!captured) continue
    paths.add(
      captured
        .replace(/[?!\s]/g, "")
        .split(".")
        .filter((segment) => segment.length > 0)
        .join("."),
    )
  }
  return [...paths].sort()
}

/**
 * Extract every event name compared against `event.type` with ===, in either
 * operand order (`event.type === "x"` or the Yoda form `"x" === event.type`,
 * multi-line aware). The comparison is located on the comment/string-blanked
 * source and the literal is read back from the original text by offset, so
 * only a real string literal in a real comparison counts — text inside
 * comments or other strings never matches (WI#369 DF-1/DF-4).
 */
export function extractEventTypes(source: string): string[] {
  const { blanked, strings } = blankCommentsAndStrings(source)
  const names = new Set<string>()
  const WINDOW = 80 // generous reach for multi-line comparisons
  for (const span of strings) {
    const before = blanked.slice(Math.max(0, span.start - WINDOW), span.start)
    const after = blanked.slice(span.end, span.end + WINDOW)
    if (/\bevent\s*\.\s*type\s*===\s*$/.test(before) || /^\s*===\s*event\s*\.\s*type\b/.test(after)) {
      names.add(span.value)
    }
  }
  return [...names].sort()
}

/**
 * Wrap a mock client in a deep recording Proxy: every property path reached
 * through it whose value is a function OR undefined is recorded into `sink`.
 * This is the behavioural complement to extractSdkCallPaths (WI#369 DF-1):
 * aliases, casts, bracket access, destructuring, optional-call no-ops
 * (sdk.session.summarize?.() resolves undefined and silently does nothing —
 * the WI#320 F7 class) and helper parameters of any name are all recorded on
 * the paths the test run actually exercises. Intermediate group objects are
 * not recorded; symbol keys pass through untracked. Functions are returned
 * wrapped (their own property accesses stay recorded); call results are the
 * raw values.
 */
export function recordingSdkProxy<T extends object>(root: T, sink: Set<string>): T {
  const wrap = (node: unknown, prefix: string): unknown => {
    if (node === null || (typeof node !== "object" && typeof node !== "function")) return node
    return new Proxy(node as object, {
      get(target, property, receiver) {
        const value = Reflect.get(target, property, receiver)
        if (typeof property !== "string") return value
        const path = prefix ? `${prefix}.${property}` : property
        if (typeof value === "function" || value === undefined) sink.add(path)
        return wrap(value, path)
      },
    })
  }
  return wrap(root, "") as T
}

/**
 * Accessed leaf paths that are NOT declared in BRIDGE_SDK_METHODS. The
 * self-test asserts this is empty after the suite runs: anything else is a
 * bridge call the shared list does not know about (WI#320 F7 drift class).
 */
export function findUndeclaredSdkPaths(accessed: Iterable<string>): string[] {
  const declared = new Set<string>(BRIDGE_SDK_METHODS)
  return [...accessed].filter((path) => !declared.has(path)).sort()
}

/**
 * Parse the pinned generated `export type V2Event = ...` union from
 * client/src/promise/generated/types.ts into the `type: "<lit>"` literal of
 * each member definition (EventSubscribeOutput = V2Event is what
 * event.subscribe actually yields). This scopes the event gate to the union
 * itself (WI#369 DF-2): under the old corpus-wide substring check a v2-only
 * rename survived as long as the v1 compat schema still carried the literal
 * (schema/src/v1/permission.ts:61 at v2.0.24), and the bridge would have
 * hung waiting for an event that never fires. Fail-closed: any drift in the
 * generator's format (missing union, missing alias, a member with no
 * top-level literal) is reported as a problem instead of passing silently.
 */
export function parseV2EventTypes(typesSource: string): {
  literals: string[]
  problems: string[]
  /** Members whose top-level `type:` is not a plain literal (dynamic families
   * such as V2EventRpc's `${"rpc."}${string}` template type) — not gateable,
   * not drift. */
  dynamicMembers: string[]
} {
  const problems: string[] = []
  const dynamicMembers: string[] = []
  const unionStart = /export\s+type\s+V2Event\s*=/.exec(typesSource)
  if (!unionStart) {
    return {
      literals: [],
      problems: ["export type V2Event not found in the pinned generated types"],
      dynamicMembers,
    }
  }
  const rest = typesSource.slice(unionStart.index + unionStart[0].length)
  const unionEnd = /\n\s*\n/.exec(rest)
  const unionBody = unionEnd ? rest.slice(0, unionEnd.index) : rest
  const members = [...unionBody.matchAll(/\|\s*([A-Za-z_$][\w$]*)/g)].map((match) => match[1]!)
  if (members.length === 0) {
    problems.push("the V2Event union has no members — generator format drift?")
  }
  if (!/export\s+type\s+EventSubscribeOutput\s*=\s*V2Event\b/.test(typesSource)) {
    problems.push("EventSubscribeOutput is no longer aliased to V2Event — the subscribe union drifted")
  }
  const literals: string[] = []
  for (const member of members) {
    const definition = new RegExp(
      `export\\s+type\\s+${member}\\s*=\\s*\\{([\\s\\S]*?)\\n\\}`,
    ).exec(typesSource)
    if (!definition) {
      problems.push(`V2Event member ${member} has no object definition in the generated types`)
      continue
    }
    const typeField = /^  type: (.+)$/m.exec(definition[1]!)
    if (!typeField) {
      problems.push(`V2Event member ${member} has no top-level type field — generator format drift?`)
      continue
    }
    const literal = /^"([^"]+)"$/.exec(typeField[1]!.trim())
    if (!literal) {
      dynamicMembers.push(member)
      continue
    }
    literals.push(literal[1]!)
  }
  return { literals: [...new Set(literals)].sort(), problems, dynamicMembers: dynamicMembers.sort() }
}

/**
 * Assert the real client surface: every BRIDGE_SDK_METHODS path resolves to
 * a function and every STALE_SDK_METHODS path is absent. Returns the list of
 * failures (empty = pass); the caller decides how to report them.
 */
export function checkClientSurface(client: unknown): string[] {
  const failures: string[] = []
  for (const path of BRIDGE_SDK_METHODS) {
    if (typeof resolveMethod(client, path) !== "function") {
      failures.push(
        `pinned @opencode/client is missing client.${path}() — the bridge calls it (WI#329)`,
      )
    }
  }
  for (const path of STALE_SDK_METHODS) {
    if (resolveMethod(client, path) !== undefined) {
      failures.push(
        `client.${path} exists on the pinned client — remove it from STALE_SDK_METHODS only after re-auditing the bridge call sites (WI#329)`,
      )
    }
  }
  return failures
}

/**
 * Assert every BRIDGE_SDK_EVENTS name is a member literal of the pinned
 * V2Event union (parseV2EventTypes). Returns the list of failures
 * (empty = pass).
 */
export function checkBridgeEventsAgainstUnion(typesSource: string): string[] {
  const { literals, problems } = parseV2EventTypes(typesSource)
  const failures = [...problems]
  const union = new Set(literals)
  for (const name of BRIDGE_SDK_EVENTS) {
    if (!union.has(name)) {
      failures.push(
        `event "${name}" is not a member literal of the pinned V2Event union — ` +
          `the bridge matches on event.type, so an upstream rename would hang ` +
          `every task or stall permissions at runtime (WI#329, WI#369)`,
      )
    }
  }
  return failures
}
