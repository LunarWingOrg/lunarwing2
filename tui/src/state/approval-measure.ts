/**
 * What the approval panel can show of a prompt (design 3.6.2, SEC-04, UX-12). Pure: the panel renders exactly
 * the lines returned here and the controller gates approve actions on the same result, so "hidden" always means
 * "not on screen". Approve (Alt+Y, Alt+A, /approve, /always, a confirmed typed or dictated reply) is refused until
 * the full view was opened when anything is hidden; deny is always allowed.
 *
 * Wrapping (the util.ts wrapText idea, made safe for review): words wrap at spaces, a word wider than the line
 * is split instead of cut, indentation at the start of a paragraph is kept, a tab is 4 spaces. Widths are
 * conservative and follow the renderer, not Bun.stringWidth: each grapheme cluster counts with the widest of
 * Bun.stringWidth and OpenTUI's native measure under every width method (util.ts graphemeWidth), and every line
 * that is not plain ASCII is measured again as a whole with the native measure and split further if the engine
 * draws it wider. So a rendered line never needs more columns than the panel has and cannot be clipped (both the
 * panel and the full view draw one measured line per row with wrapMode "none"). Invisible and control characters
 * become visible \u{XXXX} escapes (3.6.2): the design list U+00AD, U+180E, U+200B-U+200F, U+2060-U+2064, U+FEFF,
 * U+E0000-U+E007F, plus C0/C1 and bidi controls (normally already removed by sanitize.ts) and the blank-looking
 * U+034F, U+061C, U+115F, U+1160, U+2028, U+2029, U+2065, U+3164, U+FFA0.
 */
import type { ApprovalPrompt } from "./model.ts"
import type { WireApproval } from "../transport/types.ts"
import { utf8Length } from "./turn-text.ts"
import { engineWidth, graphemesOf, graphemeWidth, isPrintableAscii } from "../util.ts"

/** How many wrapped lines the panel shows: per field, and both fields together. */
export interface ApprovalCaps {
  descriptionLines: number
  parameterLines: number
  totalLines: number
}

export interface ApprovalMeasure {
  /** Shown description lines, each at most `width` columns. */
  description: string[]
  /** Shown parameter lines, each at most `width` columns. */
  parameters: string[]
  /** Wrapped lines of description and parameters that the panel does not show. */
  hiddenLines: number
  /** UTF-8 bytes of the original text behind the hidden lines. */
  hiddenBytes: number
}

/** The 3.6.2 limits: up to 6 description and 8 parameter lines. Used when a panel has room for them. */
export const APPROVAL_FULL_CAPS: Readonly<ApprovalCaps> = { descriptionLines: 6, parameterLines: 8, totalLines: 14 }

/** Border (2 rows) plus the action row ([Alt+Y] approve ... with the +N hidden marker). */
const PANEL_CHROME_ROWS = 3
/** Border (2 columns) plus one column of padding on each side. */
export const APPROVAL_PANEL_CHROME_COLUMNS = 4
export const MIN_WRAP_WIDTH = 10
const TAB_WIDTH = 4

/**
 * Caps for a panel of `rows` terminal rows (state/layout.ts `approval`). The 8-row panel has 5 text rows:
 * description up to 2 lines, parameters the rest. The compact 3-row panel has no text rows, so everything counts
 * as hidden and approving needs the full view.
 */
export function approvalCapsForRows(rows: number): ApprovalCaps {
  const text = Math.max(0, Math.floor(rows) - PANEL_CHROME_ROWS)
  if (text === 0) return { descriptionLines: 0, parameterLines: 0, totalLines: 0 }
  return {
    descriptionLines: Math.min(APPROVAL_FULL_CAPS.descriptionLines, Math.max(1, Math.ceil((text * 2) / 5))),
    parameterLines: Math.min(APPROVAL_FULL_CAPS.parameterLines, text),
    totalLines: text,
  }
}

/** Caps of the regular 8-row approval panel (layout `approval: 8`): the default of measureApproval. */
export const APPROVAL_PANEL_CAPS: Readonly<ApprovalCaps> = approvalCapsForRows(8)

/** Text columns inside the approval panel for a terminal of `viewWidth` columns. */
export function approvalTextWidth(viewWidth: number): number {
  return Math.max(MIN_WRAP_WIDTH, Math.floor(viewWidth) - APPROVAL_PANEL_CHROME_COLUMNS)
}

/** Measures per prompt object and width; prompts in the store are immutable, so a hit is reused while the text matches. */
const measureCache = new WeakMap<object, Map<string, ApprovalMeasure>>()
const textOf = new WeakMap<ApprovalMeasure, { description: string; parameters: string }>()

export function measureApproval(
  prompt: WireApproval,
  width: number,
  caps: Readonly<ApprovalCaps> = APPROVAL_PANEL_CAPS,
): ApprovalMeasure {
  const key = `${width}:${caps.descriptionLines}:${caps.parameterLines}:${caps.totalLines}`
  const cached = measureCache.get(prompt)?.get(key)
  if (cached !== undefined && sameText(cached, prompt)) return cached
  const result = computeMeasure(prompt, width, caps)
  const perPrompt = measureCache.get(prompt) ?? new Map<string, ApprovalMeasure>()
  perPrompt.set(key, result)
  measureCache.set(prompt, perPrompt)
  textOf.set(result, { description: prompt.description, parameters: prompt.parameters })
  return result
}

/** The full view (Alt+V): every line, wrapped and escaped the same way. */
export function fullApprovalLines(prompt: WireApproval, width: number): { description: string[]; parameters: string[] } {
  const wrap = effectiveWidth(width)
  return {
    description: wrapApprovalText(prompt.description, wrap).map((line) => line.text),
    parameters: wrapApprovalText(prompt.parameters, wrap).map((line) => line.text),
  }
}

/**
 * The SEC-04 gate: true while approving must wait for the full view. Once the full view was opened for this
 * request the panel's limits no longer matter, so nothing is measured.
 */
export function approvalNeedsFullView(
  prompt: ApprovalPrompt,
  width: number,
  caps: Readonly<ApprovalCaps> = APPROVAL_PANEL_CAPS,
): boolean {
  if (prompt.viewedFull) return false
  return measureApproval(prompt, width, caps).hiddenLines > 0
}

/** Matches one escape of this module (\u{200B}) or of sanitize.ts sanitizeVisible (\u{1b}); no `g` flag. */
export const INVISIBLE_ESCAPE_PATTERN = /\\u\{[0-9A-Fa-f]{1,6}\}/

/** Splits a measured line into plain text and escapes, so the panel can give the escapes a distinct style. */
export function splitInvisibleEscapes(line: string): { text: string; escape: boolean }[] {
  const parts: { text: string; escape: boolean }[] = []
  let last = 0
  for (const match of line.matchAll(/\\u\{[0-9A-Fa-f]{1,6}\}/g)) {
    if (match.index > last) parts.push({ text: line.slice(last, match.index), escape: false })
    parts.push({ text: match[0], escape: true })
    last = match.index + match[0].length
  }
  if (last < line.length || parts.length === 0) parts.push({ text: line.slice(last), escape: false })
  return parts
}

/** Replaces invisible and control characters with a visible \u{XXXX} escape. */
export function escapeInvisible(text: string): string {
  let out = ""
  for (const char of text) {
    const code = char.codePointAt(0) ?? 0
    out += isInvisible(code) ? escapeCodePoint(code) : char
  }
  return out
}

export function isInvisible(code: number): boolean {
  return (
    (code < 0x20 && code !== 0x09 && code !== 0x0a) ||
    (code >= 0x7f && code <= 0x9f) ||
    code === 0x00ad ||
    code === 0x034f ||
    code === 0x061c ||
    code === 0x115f ||
    code === 0x1160 ||
    code === 0x180e ||
    (code >= 0x200b && code <= 0x200f) ||
    (code >= 0x2028 && code <= 0x202e) ||
    (code >= 0x2060 && code <= 0x2069) ||
    code === 0x3164 ||
    code === 0xfeff ||
    code === 0xffa0 ||
    (code >= 0xe0000 && code <= 0xe007f)
  )
}

// ---------------------------------------------------------------------------------------------------------

/** A cached measure is reused only while the prompt object still carries the same text. */
function sameText(measure: ApprovalMeasure, prompt: WireApproval): boolean {
  const text = textOf.get(measure)
  return text !== undefined && text.description === prompt.description && text.parameters === prompt.parameters
}

function computeMeasure(prompt: WireApproval, width: number, caps: Readonly<ApprovalCaps>): ApprovalMeasure {
  const wrap = effectiveWidth(width)
  const description = wrapApprovalText(prompt.description, wrap)
  const parameters = wrapApprovalText(prompt.parameters, wrap)
  const total = Math.max(0, caps.totalLines)
  const shownDescription = Math.min(description.length, Math.max(0, caps.descriptionLines), total)
  const shownParameters = Math.min(parameters.length, Math.max(0, caps.parameterLines), total - shownDescription)
  const hidden = [...description.slice(shownDescription), ...parameters.slice(shownParameters)]
  return {
    description: description.slice(0, shownDescription).map((line) => line.text),
    parameters: parameters.slice(0, shownParameters).map((line) => line.text),
    hiddenLines: hidden.length,
    hiddenBytes: hidden.reduce((sum, line) => sum + line.bytes, 0),
  }
}

function effectiveWidth(width: number): number {
  return Number.isFinite(width) ? Math.max(MIN_WRAP_WIDTH, Math.floor(width)) : MIN_WRAP_WIDTH
}

function escapeCodePoint(code: number): string {
  return `\\u{${code.toString(16).toUpperCase().padStart(4, "0")}}`
}

/** One wrapped line and the UTF-8 bytes of the original text it stands for (separators included). */
interface WrappedLine {
  text: string
  bytes: number
}

/** An atomic piece of display text; `ascii` pieces are printable ASCII runs that may be split anywhere. */
interface Piece {
  text: string
  width: number
  bytes: number
  ascii: boolean
}

interface Token {
  space: boolean
  pieces: Piece[]
  width: number
  bytes: number
}

/** Wraps `text` (trailing whitespace ignored; empty text has no lines) at `width` columns. */
export function wrapApprovalText(text: string, width: number): WrappedLine[] {
  const body = text.trimEnd()
  if (body.length === 0) return []
  const paragraphs = body.split("\n")
  const lines: WrappedLine[] = []
  paragraphs.forEach((paragraph, index) => {
    const wrapped = wrapParagraph(paragraph, width)
    const last = wrapped[wrapped.length - 1]
    if (last !== undefined && index < paragraphs.length - 1) last.bytes += 1 // the "\n"
    lines.push(...wrapped)
  })
  return lines
}

/** A piece as placed on a line, kept so a line the engine draws wider can be split again. */
interface Part {
  text: string
  bytes: number
  /** A printable ASCII run that may be split anywhere. */
  ascii: boolean
}

class LineBuilder {
  readonly lines: WrappedLine[] = []
  private parts: Part[] = []
  private text = ""
  private columns = 0
  private bytes = 0
  private content = false

  constructor(private readonly width: number) {}

  get room(): number {
    return this.width - this.columns
  }

  get hasContent(): boolean {
    return this.content
  }

  add(piece: Piece, isSpace: boolean): void {
    this.parts.push({ text: piece.text, bytes: piece.bytes, ascii: piece.ascii })
    this.text += piece.text
    this.columns += piece.width
    this.bytes += piece.bytes
    if (!isSpace) this.content = true
  }

  /** Bytes consumed at a line break (the spaces the break replaces) belong to the line that ends. */
  consume(bytes: number): void {
    this.parts.push({ text: "", bytes, ascii: true })
    this.bytes += bytes
  }

  push(): void {
    const text = this.text.trimEnd()
    if (fitsOnScreen(text, this.width)) this.lines.push({ text, bytes: this.bytes })
    else this.lines.push(...splitByEngine(this.parts, this.width))
    this.parts = []
    this.text = ""
    this.columns = 0
    this.bytes = 0
    this.content = false
  }

  finish(): WrappedLine[] {
    if (this.text.length > 0 || this.bytes > 0 || this.lines.length === 0) this.push()
    return this.lines
  }
}

/** True when the renderer draws `text` within `width` columns, whatever its width method (ASCII is exact). */
function fitsOnScreen(text: string, width: number): boolean {
  return isPrintableAscii(text) || engineWidth(text) <= width
}

/**
 * Defence in depth for a line the engine draws wider than the sum of its graphemes (a sequence the two
 * segmentations see differently): split it again, measuring every prefix with the native measure itself.
 * ASCII runs are split per character; every other piece stays whole.
 */
function splitByEngine(parts: readonly Part[], width: number): WrappedLine[] {
  const lines: WrappedLine[] = []
  let text = ""
  let bytes = 0
  const place = (piece: string, pieceBytes: number): void => {
    if (text.trim().length > 0 && !fitsOnScreen(`${text}${piece}`.trimEnd(), width)) {
      lines.push({ text: text.trimEnd(), bytes })
      text = ""
      bytes = 0
    }
    // A continuation line never starts with the spaces of the break.
    if (text.length > 0 || piece.trim().length > 0) text += piece
    bytes += pieceBytes
  }
  for (const part of parts) {
    // A word run has one byte per character; a tab's four spaces stand for one byte and stay whole.
    if (part.ascii && part.text.length > 1 && part.bytes === part.text.length) for (const char of part.text) place(char, 1)
    else place(part.text, part.bytes)
  }
  if (text.length > 0 || bytes > 0 || lines.length === 0) lines.push({ text: text.trimEnd(), bytes })
  return lines
}

function wrapParagraph(paragraph: string, width: number): WrappedLine[] {
  const line = new LineBuilder(width)
  for (const token of tokenize(paragraph)) {
    if (token.width <= line.room) {
      for (const piece of token.pieces) line.add(piece, token.space)
    } else if (token.space) {
      if (line.hasContent) {
        line.consume(token.bytes)
        line.push()
      } else {
        placeSplit(line, token, width) // indentation wider than the line
      }
    } else {
      if (line.hasContent) line.push()
      placeSplit(line, token, width)
    }
  }
  return line.finish()
}

/** Places a token that does not fit, splitting ASCII runs and moving atomic pieces to the next line. */
function placeSplit(line: LineBuilder, token: Token, width: number): void {
  for (const piece of token.pieces) {
    let rest = piece
    while (rest.width > line.room) {
      if (rest.ascii && line.room > 0) {
        const head = line.room
        line.add({ text: rest.text.slice(0, head), width: head, bytes: head, ascii: true }, token.space)
        rest = { text: rest.text.slice(head), width: rest.width - head, bytes: rest.bytes - head, ascii: true }
      }
      if (line.room < width) line.push()
      else break // a single piece wider than the whole line (not possible at MIN_WRAP_WIDTH)
    }
    line.add(rest, token.space)
  }
}

function tokenize(paragraph: string): Token[] {
  const tokens: Token[] = []
  let current: Token | null = null
  for (const piece of pieces(paragraph)) {
    const space = piece.text.trim().length === 0 && piece.ascii
    const lastPiece = current?.pieces[current.pieces.length - 1]
    if (piece.width === 0 && current !== null && !current.space && lastPiece !== undefined && !lastPiece.ascii) {
      // A zero-width piece (a mark the segmenter left on its own) stays with the piece before it, measured again.
      const before = lastPiece.width
      lastPiece.text += piece.text
      lastPiece.bytes += piece.bytes
      lastPiece.width = Math.max(before, engineWidth(lastPiece.text))
      current.width += lastPiece.width - before
      current.bytes += piece.bytes
      continue
    }
    if (current === null || current.space !== space) {
      current = { space, pieces: [], width: 0, bytes: 0 }
      tokens.push(current)
    }
    current.pieces.push(piece)
    current.width += piece.width
    current.bytes += piece.bytes
  }
  return tokens
}

/**
 * The pieces of one paragraph, by grapheme cluster: printable ASCII runs (splittable), spaces and tabs, and
 * atomic pieces for everything else, so a cluster such as "1️⃣" or "❤️" is measured (and drawn) as one unit.
 */
function* pieces(paragraph: string): Generator<Piece> {
  if (PLAIN_ASCII_PARAGRAPH.test(paragraph)) {
    yield* asciiPieces(paragraph)
    return
  }
  let run = ""
  for (const grapheme of graphemesOf(paragraph)) {
    if (grapheme.length === 1 && isPrintableAsciiWordChar(grapheme.charCodeAt(0))) {
      run += grapheme
      continue
    }
    if (run.length > 0) {
      yield { text: run, width: run.length, bytes: run.length, ascii: true }
      run = ""
    }
    if (grapheme === " " || grapheme === "\t") {
      const text = grapheme === "\t" ? " ".repeat(TAB_WIDTH) : " "
      yield { text, width: text.length, bytes: 1, ascii: true }
      continue
    }
    yield* clusterPieces(grapheme)
  }
  if (run.length > 0) yield { text: run, width: run.length, bytes: run.length, ascii: true }
}

/** One grapheme cluster: invisible code points become escapes; the visible runs between them stay atomic. */
function* clusterPieces(cluster: string): Generator<Piece> {
  let visible = ""
  for (const char of cluster) {
    const code = char.codePointAt(0) ?? 0
    if (!isInvisible(code)) {
      visible += char
      continue
    }
    if (visible.length > 0) {
      yield { text: visible, width: graphemeWidth(visible), bytes: utf8Length(visible), ascii: false }
      visible = ""
    }
    const text = escapeCodePoint(code)
    yield { text, width: text.length, bytes: utf8Length(char), ascii: false }
  }
  if (visible.length > 0) yield { text: visible, width: graphemeWidth(visible), bytes: utf8Length(visible), ascii: false }
}

/** Printable ASCII, spaces and tabs only: one cell per character, no clusters (the common case, kept fast). */
const PLAIN_ASCII_PARAGRAPH = /^[\x20-\x7e\t]*$/

function* asciiPieces(paragraph: string): Generator<Piece> {
  let index = 0
  while (index < paragraph.length) {
    const code = paragraph.charCodeAt(index)
    if (code === 0x20 || code === 0x09) {
      const text = code === 0x09 ? " ".repeat(TAB_WIDTH) : " "
      yield { text, width: text.length, bytes: 1, ascii: true }
      index += 1
      continue
    }
    let end = index + 1
    while (end < paragraph.length && isPrintableAsciiWordChar(paragraph.charCodeAt(end))) end += 1
    const text = paragraph.slice(index, end)
    yield { text, width: text.length, bytes: text.length, ascii: true }
    index = end
  }
}

function isPrintableAsciiWordChar(code: number): boolean {
  return code > 0x20 && code < 0x7f
}
