/**
 * Entry point: resolve the socket path, boot the OpenTUI renderer, and
 * mount the app.
 */

import { createCliRenderer } from "@opentui/core"
import { createRoot } from "@opentui/react"
import { defaultSocketPath } from "./repl-client.ts"
import { App } from "./app.tsx"

function parseSocketArg(args: string[]): string | undefined {
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--socket" && i + 1 < args.length) {
      return args[i + 1]
    }
  }
  return undefined
}

const socketPath = parseSocketArg(process.argv.slice(2)) ?? defaultSocketPath()

const renderer = await createCliRenderer({
  exitOnCtrlC: false,
  // Kitty keyboard protocol (progressive enhancement): lets us distinguish
  // Ctrl+Enter from plain Enter. Terminals without support fall back to the
  // Ctrl+S interrupt key.
  useKittyKeyboard: {},
})
const root = createRoot(renderer)
root.render(<App socketPath={socketPath} agentName="LunarWing" />)
