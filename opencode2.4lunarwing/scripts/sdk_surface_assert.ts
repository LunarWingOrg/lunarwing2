/**
 * sdk_surface_assert.ts — image-build gate for the bridge's SDK surface
 * (WI#329). Runs inside the Docker build against the real, pinned
 * @opencode/client: constructs the promise client against a dummy base URL
 * (pure construction — no server contact) and asserts typeof "function" for
 * every path in BRIDGE_SDK_METHODS, plus the absence of STALE_SDK_METHODS.
 * Any drift exits non-zero and fails the image build instead of dying at
 * runtime the way WI#320 F7 did.
 */

import { BRIDGE_SDK_METHODS, STALE_SDK_METHODS, resolveMethod } from "./sdk_surface.ts"

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
if (failed) process.exit(1)

console.log(
  `opencode2 SDK surface OK: ${BRIDGE_SDK_METHODS.length} bridge method paths present, ` +
    `${STALE_SDK_METHODS.length} known-stale paths absent`,
)
