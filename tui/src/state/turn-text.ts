/**
 * The text of one turn (design 3.2, 3.3). Replaces vibetui4's single mutable TurnBuffer
 * (v4:tui/src/turn-buffer.ts) with immutable per-turn values and keeps its three invariants:
 *
 * 1. An empty final is a close, not a wipe: the streamed deltas stay the turn text.
 * 2. A non-empty final is canonical: it replaces the deltas on screen. Several finals for one turn
 *    (REPLv3 may send more than one Response per msg_id) are joined with a blank line.
 * 3. A sealed text never reopens: late deltas are rejected. A later final is kept as late text (3.5 rule 3)
 *    unless it is empty or repeats the last final, which changes nothing.
 *
 * Deltas are capped at STATE_BOUNDS.maxDeltaBytesPerTurn UTF-8 bytes (3.12.5); the excess is dropped and
 * reported as "cut", and the final text replaces the deltas anyway.
 */
import { STATE_BOUNDS, type TurnPhase, type TurnRecord } from "./model.ts"

export interface TurnText {
  readonly deltas: string
  readonly finalText: string | null
  readonly sealed: boolean
}

/** Phases after which a turn never changes again except for late text (3.5 rule 3). */
export const SEALED_PHASES: ReadonlySet<TurnPhase> = new Set<TurnPhase>([
  "done",
  "interrupted",
  "failed",
  "lost",
  "unanswered",
])

export function isSealedPhase(phase: TurnPhase): boolean {
  return SEALED_PHASES.has(phase)
}

/** Joins the finals of one turn. */
export const FINAL_SEPARATOR = "\n\n"

/** A new turn starts empty and open. */
export function createTurnText(): TurnText {
  return { deltas: "", finalText: null, sealed: false }
}

export function turnTextOf(record: Pick<TurnRecord, "deltas" | "finalText" | "phase">): TurnText {
  return { deltas: record.deltas, finalText: record.finalText, sealed: isSealedPhase(record.phase) }
}

/** What the transcript shows: the canonical final when there is one, else the streamed deltas. */
export function visibleText(text: Pick<TurnText, "deltas" | "finalText">): string {
  return text.finalText !== null && text.finalText.length > 0 ? text.finalText : text.deltas
}

/**
 * appended: the whole chunk was added. cut: the byte cap was reached, so part or all of the chunk was dropped.
 * rejected: there is no open turn (none created yet, or sealed); nothing changed.
 */
export type DeltaStatus = "appended" | "cut" | "rejected"

export interface DeltaResult {
  readonly text: TurnText | undefined
  readonly status: DeltaStatus
}

export function appendDelta(
  text: TurnText | undefined,
  chunk: string,
  maxBytes: number = STATE_BOUNDS.maxDeltaBytesPerTurn,
): DeltaResult {
  if (text === undefined || text.sealed) return { text, status: "rejected" }
  if (chunk.length === 0) return { text, status: "appended" }
  // A UTF-16 code unit never encodes to more than 3 UTF-8 bytes, so short texts skip the exact count.
  if ((text.deltas.length + chunk.length) * 3 <= maxBytes) {
    return { text: { ...text, deltas: text.deltas + chunk }, status: "appended" }
  }
  const room = maxBytes - utf8Length(text.deltas)
  const chunkBytes = utf8Length(chunk)
  if (chunkBytes <= room) return { text: { ...text, deltas: text.deltas + chunk }, status: "appended" }
  const kept = room > 0 ? cutToUtf8Bytes(chunk, room) : ""
  return { text: kept.length > 0 ? { ...text, deltas: text.deltas + kept } : text, status: "cut" }
}

export interface FinalResult {
  readonly text: TurnText | undefined
  /** False when nothing changed: no turn, an empty final, or a repeat on a sealed turn. */
  readonly changed: boolean
  /** The final was added after the turn was sealed (rule 3: kept as late text). */
  readonly late: boolean
}

export function applyFinal(text: TurnText | undefined, final: string): FinalResult {
  if (text === undefined) return { text, changed: false, late: false }
  if (final.length === 0) return { text, changed: false, late: false }
  if (text.sealed && lastFinalSegment(text.finalText) === final) return { text, changed: false, late: false }
  const finalText = text.finalText === null ? final : text.finalText + FINAL_SEPARATOR + final
  return { text: { ...text, finalText }, changed: true, late: text.sealed }
}

/** Seals the text; an already sealed text is returned unchanged. */
export function sealTurnText(text: TurnText): TurnText {
  return text.sealed ? text : { ...text, sealed: true }
}

function lastFinalSegment(finalText: string | null): string | null {
  if (finalText === null) return null
  const at = finalText.lastIndexOf(FINAL_SEPARATOR)
  return at < 0 ? finalText : finalText.slice(at + FINAL_SEPARATOR.length)
}

/** UTF-8 byte length without allocating an encoded copy. */
export function utf8Length(text: string): number {
  return Buffer.byteLength(text, "utf8")
}

/** The longest prefix of `text` that fits in `maxBytes` UTF-8 bytes, cut on a code point boundary. */
export function cutToUtf8Bytes(text: string, maxBytes: number): string {
  let bytes = 0
  let end = 0
  while (end < text.length) {
    const code = text.codePointAt(end) ?? 0
    const size = code < 0x80 ? 1 : code < 0x800 ? 2 : code < 0x10000 ? 3 : 4
    if (bytes + size > maxBytes) break
    bytes += size
    end += code > 0xffff ? 2 : 1
  }
  return text.slice(0, end)
}
