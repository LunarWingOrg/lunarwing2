/**
 * Terminal-safe text (design 3.12.5, 10.1 "Terminal").
 *
 * Every server string (replies, activity, approval text, notices, captions) and every transcript passes through
 * sanitizeText() before it reaches the state: escape sequences are removed with OpenTUI's own
 * stripAnsiSequences (ot081:lib/paste.d.ts:7, CSI, OSC, DCS, SOS/PM/APC and 8-bit C1 introducers), then the
 * remaining control characters: U+0000-U+0008, U+000B-U+001F (CR included), U+007F-U+009F and the bidi
 * embedding, override and isolate controls U+202A-U+202E and U+2066-U+2069. TAB survives; LF survives only in
 * multiline mode.
 *
 * Because every ESC and C1 character is removed, no escape sequence can survive or be reassembled, even when a
 * sequence was split across two streamed chunks (the fragment then shows as plain text).
 * Invisible format characters (zero-width, tags) are left alone here; the approval panel escapes them (3.6.2).
 */
import { stripAnsiSequences } from "@opentui/core"

export interface SanitizeOptions {
  /** Keep line feeds (replies, transcripts). Default false: one line (activity, notices, captions). */
  multiline?: boolean
}

// C0 except TAB and LF, DEL and C1, then the bidi controls.
const CONTROL_CHARACTERS = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/g
const LINE_FEEDS = /\n+/g
const ESC = 0x1b
const BEL = 0x07
const ST = 0x9c
const REMOVED_TO_END = "[…escape sequence removed]"

function stringSequenceEnd(text: string, start: number): number {
  for (let i = start; i < text.length; i += 1) {
    const code = text.charCodeAt(i)
    if (code === BEL || code === ST) return i + 1
    if (code === ESC && text.charCodeAt(i + 1) === 0x5c) return i + 2
  }
  return -1
}

/** True when OSC, DCS, SOS, PM or APC consumes everything after its introducer. */
function hasUnterminatedStringSequence(text: string): boolean {
  for (let i = 0; i < text.length; i += 1) {
    const first = text.charCodeAt(i)
    const second = text.charCodeAt(i + 1)
    const c1 = first === 0x90 || first === 0x98 || (first >= 0x9d && first <= 0x9f)
    const escaped =
      first === ESC && (second === 0x50 || second === 0x58 || second === 0x5d || second === 0x5e || second === 0x5f)
    if (!c1 && !escaped) continue
    const end = stringSequenceEnd(text, i + (escaped ? 2 : 1))
    if (end < 0) return true
    i = end - 1
  }
  return false
}

export function sanitizeText(text: string, options: SanitizeOptions = {}): string {
  const stripped = stripAnsiSequences(text).replace(CONTROL_CHARACTERS, "")
  const visible = options.multiline === true ? stripped : stripped.replace(LINE_FEEDS, " ")
  if (!hasUnterminatedStringSequence(text) || visible === text) return visible
  const separator = visible === "" || /\s$/.test(visible) ? "" : " "
  return `${visible}${separator}${REMOVED_TO_END}`
}

/** sanitizeText(text) in single-line mode. */
export function sanitizeLine(text: string): string {
  return sanitizeText(text, { multiline: false })
}

// Everything sanitizeText removes, CR and LF included; LF is put back in multiline mode.
const VISIBLE_ESCAPES = /[\u0000-\u0008\u000a-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/g

/**
 * Terminal-safe like sanitizeText, but hides nothing: every control and bidi character becomes a visible
 * "\u{1b}"-style escape instead of being removed. Stripping removes an escape sequence together with its
 * payload (an OSC title or DCS string). sanitizeText appends a visible marker when an unterminated sequence
 * consumes the rest of the text, but sanitizeVisible keeps the payload itself visible. Use this where the user must see everything: approval tool names,
 * descriptions and parameters (3.6.2, SEC-04).
 */
export function sanitizeVisible(text: string, options: SanitizeOptions = {}): string {
  const multiline = options.multiline === true
  return text.replace(VISIBLE_ESCAPES, (c) => (multiline && c === "\n" ? c : `\\u{${c.charCodeAt(0).toString(16)}}`))
}
