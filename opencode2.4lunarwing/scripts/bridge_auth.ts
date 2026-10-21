/**
 * bridge_auth.ts — hub-handshake credential resolution for the opencode2
 * bridge (WI#400, WI#449 migration). The entrypoint moves the credential out
 * of the process environment into a root-only file and exports only
 * AGENT_AUTH_TOKEN_FILE. When that pointer is set, the file MUST yield a
 * token: an unreadable or empty file is a startup error, never a silent
 * fall-through to the (already unset) environment variable that would start
 * the server with auth disabled (run-170 LP-4; same contract as the kimi,
 * opencode and pebble bridges). Without the pointer the environment variable
 * is the dev-launch fallback.
 */

import { readFileSync } from "node:fs"

export function readBridgeAuthToken(
  env: NodeJS.ProcessEnv = process.env,
  readFile: (path: string) => string = (path) => readFileSync(path, "utf8"),
): string {
  const tokenFile = env.AGENT_AUTH_TOKEN_FILE
  if (tokenFile) {
    let contents: string
    try {
      contents = readFile(tokenFile)
    } catch (err) {
      throw new Error(
        `cannot read AGENT_AUTH_TOKEN_FILE ${tokenFile}: ${err instanceof Error ? err.message : String(err)} (WI#400: refusing to start without the hub credential)`,
      )
    }
    // Trim trailing CR/LF only: the credential is written without a newline,
    // but a hand-edited file may carry one.
    const token = contents.replace(/[\r\n]+$/, "")
    if (!token) {
      throw new Error(`AGENT_AUTH_TOKEN_FILE ${tokenFile} holds no token (WI#400: refusing to start without the hub credential)`)
    }
    return token
  }
  return env.AGENT_AUTH_TOKEN || ""
}
