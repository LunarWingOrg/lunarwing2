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
 *    is present as a literal in the pinned client/schema source, so SDK
 *    drift fails the build, not the runtime.
 *  - scripts/opencode_task_executor_test.ts — asserts the injected mock
 *    implements EXACTLY this surface (both directions) AND that the
 *    executor source's real `sdk.*(...)` call sites and
 *    `event.type === "..."` literals match these lists, so a bridge call
 *    added without updating this file fails the suite instead of drifting
 *    past the gates the way WI#320 F7 did.
 *
 * When the bridge starts calling a new client method or matching a new
 * event name, add its path here once and every gate covers it.
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
 * still a literal in the pinned client/schema source (WI#329, !1512 CY-2).
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

/**
 * Extract every `sdk.<seg>.<seg>...(…)` call path from bridge source, any
 * depth, multi-line aware ("sdk.session\n  .interrupt(" binds). This is what
 * ties BRIDGE_SDK_METHODS to the executor's real call sites: the self-test
 * asserts set equality, so an unlisted bridge call fails the suite (WI#329,
 * !1512 CY-1).
 */
export function extractSdkCallPaths(source: string): string[] {
  const paths = new Set<string>()
  const pattern = /\bsdk((?:\s*\.\s*[A-Za-z_$][\w$]*)+)\s*\(/g
  for (const match of source.matchAll(pattern)) {
    const captured = match[1]
    if (!captured) continue
    paths.add(
      captured
        .split(".")
        .map((segment) => segment.trim())
        .filter((segment) => segment.length > 0)
        .join("."),
    )
  }
  return [...paths].sort()
}

/**
 * Extract every `event.type === "<name>"` literal from bridge source. Ties
 * BRIDGE_SDK_EVENTS to the executor's real event matches the same way
 * extractSdkCallPaths ties the method list (!1512 CY-2).
 */
export function extractEventTypes(source: string): string[] {
  const names = new Set<string>()
  const pattern = /\bevent\s*\.\s*type\s*===\s*"([^"]+)"/g
  for (const match of source.matchAll(pattern)) {
    const captured = match[1]
    if (captured) names.add(captured)
  }
  return [...names].sort()
}
