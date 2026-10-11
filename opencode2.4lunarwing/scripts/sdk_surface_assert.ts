/**
 * sdk_surface_assert.ts — image-build gate for the bridge's SDK surface
 * (WI#329). Runs inside the Docker build against the real, pinned
 * @opencode/client: constructs the promise client against a dummy base URL
 * (pure construction — no server contact) and asserts typeof "function" for
 * every path in BRIDGE_SDK_METHODS, plus the absence of STALE_SDK_METHODS.
 * It also asserts every BRIDGE_SDK_EVENTS name still exists as a literal in
 * the pinned @opencode client/schema/protocol source shipped in the image:
 * completion detection and permission handling match on those event.type
 * strings, so an upstream rename would otherwise hang every task or stall
 * permissions at runtime (!1512 CY-2). Any drift exits non-zero and fails
 * the image build instead of dying at runtime the way WI#320 F7 did.
 *
 * SDK_SURFACE_SOURCE_DIRS (colon-separated) overrides the scanned source
 * directories for host-side verification against a pinned checkout; the
 * image build derives them from the resolved @opencode/client package.
 */

import { createRequire } from "node:module"
import { readdirSync, readFileSync } from "node:fs"
import { dirname, join } from "node:path"

import {
  BRIDGE_SDK_EVENTS,
  BRIDGE_SDK_METHODS,
  STALE_SDK_METHODS,
  resolveMethod,
} from "./sdk_surface.ts"

const promiseClient = await import("@opencode/client/promise").catch((error: unknown) => {
  console.error(
    "FATAL: cannot import @opencode/client/promise —",
    error instanceof Error ? error.message : error,
  )
  process.exit(1)
})

if (typeof promiseClient.OpenCode?.make !== "function") {
  console.error("FATAL: @opencode/client/promise resolved but is missing OpenCode.make")
  process.exit(1)
}

const client = promiseClient.OpenCode.make({ baseUrl: "http://127.0.0.1:4096" })

let failed = false
for (const path of BRIDGE_SDK_METHODS) {
  if (typeof resolveMethod(client, path) !== "function") {
    console.error(
      `FATAL: pinned @opencode/client is missing client.${path}() — the bridge calls it (WI#329)`,
    )
    failed = true
  }
}
for (const path of STALE_SDK_METHODS) {
  if (resolveMethod(client, path) !== undefined) {
    console.error(
      `FATAL: client.${path} exists on the pinned client — remove it from STALE_SDK_METHODS only after re-auditing the bridge call sites (WI#329)`,
    )
    failed = true
  }
}

/**
 * The pinned @opencode source directories shipped in the image whose .ts
 * text is scanned for the bridge's event literals. Derived from the
 * resolved promise entry (<pkgroot>/src/promise/index.ts) so a relocated
 * client package still resolves.
 */
function sdkSourceDirs(): string[] {
  const override = process.env.SDK_SURFACE_SOURCE_DIRS
  if (override) return override.split(":").filter((dir) => dir.length > 0)
  const promiseEntry = createRequire(import.meta.url).resolve("@opencode/client/promise")
  const clientSrc = dirname(dirname(promiseEntry))
  const opencodeScope = dirname(dirname(clientSrc))
  return [clientSrc, join(opencodeScope, "schema/src"), join(opencodeScope, "protocol/src")]
}

function collectTsSource(dir: string): string {
  const chunks: string[] = []
  const walk = (current: string) => {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const path = join(current, entry.name)
      if (entry.isDirectory()) walk(path)
      else if (entry.isFile() && entry.name.endsWith(".ts")) chunks.push(readFileSync(path, "utf8"))
    }
  }
  walk(dir)
  return chunks.join("\n")
}

let pinnedSource = ""
try {
  pinnedSource = sdkSourceDirs().map(collectTsSource).join("\n")
} catch (error) {
  console.error(
    "FATAL: cannot read the pinned @opencode source tree for the event gate —",
    error instanceof Error ? error.message : error,
  )
  failed = true
}
for (const name of BRIDGE_SDK_EVENTS) {
  if (!pinnedSource.includes(`"${name}"`)) {
    console.error(
      `FATAL: event "${name}" is not a literal in the pinned @opencode source — the bridge matches on event.type (WI#329)`,
    )
    failed = true
  }
}
if (failed) process.exit(1)

console.log(
  `opencode2 SDK surface OK: ${BRIDGE_SDK_METHODS.length} bridge method paths present, ` +
    `${STALE_SDK_METHODS.length} known-stale paths absent, ` +
    `${BRIDGE_SDK_EVENTS.length} event names present in the pinned source`,
)
