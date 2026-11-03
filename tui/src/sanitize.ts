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

export function sanitizeText(text: string, options: SanitizeOptions = {}): string {
  const stripped = stripAnsiSequences(text).replace(CONTROL_CHARACTERS, "")
  return options.multiline === true ? stripped : stripped.replace(LINE_FEEDS, " ")
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
 * payload (an OSC title, a DCS string, and an unterminated one swallows the rest of the text, as a terminal
 * would), which is a place to hide text. Use this where the user must see everything: approval tool names,
 * descriptions and parameters (3.6.2, SEC-04).
 */
export function sanitizeVisible(text: string, options: SanitizeOptions = {}): string {
  const multiline = options.multiline === true
  return text.replace(VISIBLE_ESCAPES, (c) => (multiline && c === "\n" ? c : `\\u{${c.charCodeAt(0).toString(16)}}`))
}
