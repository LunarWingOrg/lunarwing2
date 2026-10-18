/**
 * sdk_surface_assert.ts — image-build gate for the bridge's SDK surface
 * (WI#329, hardened per WI#369). Runs inside the Docker build against the
 * real, pinned @opencode/client: constructs the promise client against a
 * dummy base URL (pure construction — no server contact) and asserts typeof
 * "function" for every path in BRIDGE_SDK_METHODS, plus the absence of
 * STALE_SDK_METHODS. It also asserts every BRIDGE_SDK_EVENTS name is a
 * member literal of the pinned generated `export type V2Event` union
 * (EventSubscribeOutput = V2Event is what event.subscribe yields): the
 * bridge's completion detection and permission handling match on those
 * event.type strings, so an upstream rename would otherwise hang every task
 * or stall permissions at runtime — and a v1 compat schema literal that
 * survives a v2 rename no longer counts (that was the corpus-wide substring
 * blind spot, WI#369 DF-2). Any drift exits non-zero and fails the image
 * build instead of dying at runtime the way WI#320 F7 did.
 *
 * Environment overrides (host-side verification and the committed negative
 * test; the image build sets neither):
 *  - SDK_SURFACE_CLIENT_MODULE: module specifier imported instead of
 *    "@opencode/client/promise" (a fixture client).
 *  - SDK_SURFACE_TYPES_FILE: path to the generated types.ts parsed for the
 *    V2Event union, instead of deriving it from the resolved client package.
 */

import { createRequire } from "node:module"
import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"

import {
  BRIDGE_SDK_EVENTS,
  BRIDGE_SDK_METHODS,
  STALE_SDK_METHODS,
  checkBridgeEventsAgainstUnion,
  checkClientSurface,
} from "./sdk_surface.ts"

const clientModule = process.env.SDK_SURFACE_CLIENT_MODULE || "@opencode/client/promise"

const promiseClient = await import(clientModule).catch((error: unknown) => {
  console.error(`FATAL: cannot import ${clientModule} —`, error instanceof Error ? error.message : error)
  process.exit(1)
})

if (typeof promiseClient.OpenCode?.make !== "function") {
  console.error(`FATAL: ${clientModule} resolved but is missing OpenCode.make`)
  process.exit(1)
}

const client = promiseClient.OpenCode.make({ baseUrl: "http://127.0.0.1:4096" })

let failed = false
for (const failure of checkClientSurface(client)) {
  console.error(`FATAL: ${failure}`)
  failed = true
}

/**
 * The pinned generated types.ts whose V2Event union the event gate parses.
 * Default: derived from the resolved promise entry
 * (<pkgroot>/src/promise/index.ts → <pkgroot>/src/promise/generated/types.ts)
 * so a relocated client package still resolves. The file ships in the image
 * at /app/node_modules/@opencode/client/src/promise/generated/types.ts.
 */
function typesFile(): string {
  const override = process.env.SDK_SURFACE_TYPES_FILE
  if (override) return override
  const promiseEntry = createRequire(import.meta.url).resolve(clientModule)
  return join(dirname(dirname(promiseEntry)), "promise", "generated", "types.ts")
}

let typesSource = ""
try {
  typesSource = readFileSync(typesFile(), "utf8")
} catch (error) {
  console.error(
    "FATAL: cannot read the pinned generated types for the event gate —",
    error instanceof Error ? error.message : error,
  )
  failed = true
}
if (typesSource) {
  for (const failure of checkBridgeEventsAgainstUnion(typesSource)) {
    console.error(`FATAL: ${failure}`)
    failed = true
  }
}
if (failed) process.exit(1)

console.log(
  `opencode2 SDK surface OK: ${BRIDGE_SDK_METHODS.length} bridge method paths present, ` +
    `${STALE_SDK_METHODS.length} known-stale paths absent, ` +
    `${BRIDGE_SDK_EVENTS.length} event names present in the pinned V2Event union`,
)
