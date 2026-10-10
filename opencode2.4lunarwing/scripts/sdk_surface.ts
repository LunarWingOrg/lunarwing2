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
 *    typeof for every path below, so SDK drift fails the build, not the
 *    runtime.
 *  - scripts/opencode_task_executor_test.ts — asserts the injected mock
 *    implements EXACTLY this surface (both directions), so the mock cannot
 *    drift from the surface the build gate verified.
 *
 * When the bridge starts calling a new client method, add its path here once
 * and both gates cover it.
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

/** Resolve a dotted path ("message.list") on a nested object. */
export function resolveMethod(root: unknown, path: string): unknown {
  let current = root
  for (const segment of path.split(".")) {
    if (current === null || typeof current !== "object") return undefined
    current = (current as Record<string, unknown>)[segment]
  }
  return current
}

/** Flatten a nested method mock into sorted dotted paths ("session.create", ...). */
export function mockMethodPaths(mock: object): string[] {
  const paths: string[] = []
  for (const [group, methods] of Object.entries(mock)) {
    if (methods === null || typeof methods !== "object") continue
    for (const [name, fn] of Object.entries(methods as Record<string, unknown>)) {
      if (typeof fn === "function") paths.push(`${group}.${name}`)
    }
  }
  return paths.sort()
}
