/**
 * Entry point: resolve the socket path, boot the OpenTUI renderer, and
 * mount the app.
 */

import { createCliRenderer } from "@opentui/core"
import { createRoot } from "@opentui/react"
import { defaultSocketPath } from "./repl-client.ts"
import { IMAGE_PROTOCOLS, parseImageProtocol, type ImageProtocol } from "./images.ts"
import { App } from "./app.tsx"

function parseSocketArg(args: string[]): string | undefined {
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--socket" && i + 1 < args.length) {
      return args[i + 1]
    }
  }
  return undefined
}

/**
 * Image protocol selection: `--image-protocol <name>` wins, then
 * `$LUNARWING_TUI_IMAGE_PROTOCOL`, then `auto` (renderer negotiates).
 *
 * Forcing a protocol matters inside tmux, where OpenTUI otherwise downgrades
 * every image to unicode blocks regardless of what the outer terminal can do.
 */
function parseImageProtocolChoice(args: string[]): ImageProtocol {
  for (let i = 0; i < args.length; i++) {
    if (args[i] !== "--image-protocol" || i + 1 >= args.length) {
      continue
    }
    const raw = args[i + 1]
    const parsed = parseImageProtocol(raw.trim())
    if (parsed === null) {
      warnUnknownProtocol(`--image-protocol "${raw}"`)
    } else if (parsed !== "auto") {
      noteForcedProtocol(parsed, "--image-protocol")
    }
    return parsed ?? "auto"
  }

  const fromEnv = process.env.LUNARWING_TUI_IMAGE_PROTOCOL
  if (fromEnv === undefined) {
    return "auto"
  }
  const parsed = parseImageProtocol(fromEnv.trim())
  if (parsed === null) {
    warnUnknownProtocol(`LUNARWING_TUI_IMAGE_PROTOCOL="${fromEnv}"`)
    return "auto"
  }
  if (parsed !== "auto") {
    noteForcedProtocol(parsed, "LUNARWING_TUI_IMAGE_PROTOCOL")
  }
  return parsed
}

function warnUnknownProtocol(source: string): void {
  process.stderr.write(
    `lunarwing tui: ignoring unknown ${source} (expected one of ${IMAGE_PROTOCOLS.join(", ")})\n`,
  )
}

function noteForcedProtocol(protocol: ImageProtocol, source: string): void {
  process.stderr.write(`lunarwing tui: image protocol forced to "${protocol}" (${source})\n`)
}

const cliArgs = process.argv.slice(2)
const socketPath = parseSocketArg(cliArgs) ?? defaultSocketPath()
const imageProtocol = parseImageProtocolChoice(cliArgs)

const renderer = await createCliRenderer({
  exitOnCtrlC: false,
  // Kitty keyboard protocol (progressive enhancement): lets us distinguish
  // Ctrl+Enter from plain Enter. Terminals without support fall back to the
  // Ctrl+S interrupt key.
  useKittyKeyboard: {},
})
const root = createRoot(renderer)
root.render(<App socketPath={socketPath} agentName="LunarWing" imageProtocol={imageProtocol} />)
