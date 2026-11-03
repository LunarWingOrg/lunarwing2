/**
 * Send-path guard (design 3.10.2, 4.2.11). The daemon's SubmissionParser trims the content, lowercases it and
 * maps exactly "/quit", "/exit" and "/shutdown" to Submission::Quit (ic/src/agent/submission.rs), which stops a
 * daemon that predates the M2a refusal. controller/session.ts checks every send and steer, and every transport
 * asserts again in its write path, so these strings never reach the wire (pasted text, //quit and dictated
 * transcripts included).
 */
import { TransportError } from "./errors.ts"

const FORBIDDEN_CONTENT: ReadonlySet<string> = new Set(["/quit", "/exit", "/shutdown"])

/** The longest forbidden string; lowercasing never shortens a string, so longer input cannot match. */
const MAX_FORBIDDEN_LENGTH = 9

/**
 * Rust's str::trim strips Unicode White_Space, which includes U+0085 (NEL); String.prototype.trim does not, but
 * strips U+FEFF, which Rust keeps. The guard trims the union, so it refuses everything the daemon reads as a quit
 * command (and, harmlessly, "\uFEFF/quit", which the daemon would not). A loop instead of a regex keeps long
 * whitespace runs linear.
 */
function isEdgeWhitespace(code: number): boolean {
  if (code === 0x20 || (code >= 0x09 && code <= 0x0d)) return true
  if (code < 0x85) return false
  return (
    code === 0x85 ||
    code === 0xa0 ||
    code === 0x1680 ||
    (code >= 0x2000 && code <= 0x200a) ||
    code === 0x2028 ||
    code === 0x2029 ||
    code === 0x202f ||
    code === 0x205f ||
    code === 0x3000 ||
    code === 0xfeff
  )
}

function trimmedBounds(content: string): { start: number; end: number } {
  let start = 0
  let end = content.length
  while (start < end && isEdgeWhitespace(content.charCodeAt(start))) start += 1
  while (end > start && isEdgeWhitespace(content.charCodeAt(end - 1))) end -= 1
  return { start, end }
}

/** The content as the daemon's command matcher sees it: trimmed (both whitespace sets) and lowercased. */
export function normalizeCommandText(content: string): string {
  const { start, end } = trimmedBounds(content)
  return content.slice(start, end).toLowerCase()
}

/** True for /quit, /exit and /shutdown after the daemon's normalization (" /EXIT ", "/Quit"; not "/quitting"). */
export function isForbiddenContent(content: string): boolean {
  const { start, end } = trimmedBounds(content)
  if (end - start > MAX_FORBIDDEN_LENGTH) return false
  return FORBIDDEN_CONTENT.has(content.slice(start, end).toLowerCase())
}

/** Write-path assertion: a forbidden content throws TransportError{bad_request} and never reaches the wire. */
export function assertSendableContent(content: string): void {
  if (isForbiddenContent(content)) {
    throw new TransportError("bad_request", "refused: /quit, /exit and /shutdown are never sent to the daemon")
  }
}
