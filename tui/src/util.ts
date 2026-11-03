/**
 * Small text utilities for terminal layout (salvaged from v4:tui/src/util.ts, design 3.2).
 *
 * Changes from vibetui4: widths are display cells, not UTF-16 code units, so wide and combining characters wrap
 * correctly; a word wider than the line is split instead of cut, so wrapping never hides text; truncate() never
 * splits a grapheme cluster (or a surrogate pair). decodeDataUrl moved to images/data-url.ts.
 *
 * Widths follow the engine that draws the text: OpenTUI measures with its native library under the renderer's
 * width method (capabilities.unicode: "wcwidth", "unicode" or "unicode-wide"), and that differs from
 * Bun.stringWidth for some code points (U+4DC0-U+4DFF hexagrams and U+2630 trigrams are 2 cells there and 1 in
 * Bun; U+0600-U+0605 are 1 there and 0 in Bun) and for sequences (VS16 emoji). A grapheme counts with the widest
 * of Bun.stringWidth and the native measure under every width method, so a line measured here never needs more
 * columns on screen, whatever method the terminal negotiated.
 */
import { measureEditText, type WidthMethod } from "@opentui/core"

const WIDTH_METHODS: readonly WidthMethod[] = ["wcwidth", "unicode", "unicode-wide"]
const PRINTABLE_ASCII = /^[\x20-\x7e]*$/
const GRAPHEMES = new Intl.Segmenter(undefined, { granularity: "grapheme" })
const WIDTH_CACHE_LIMIT = 4_096
const widthCache = new Map<string, number>()

/** OpenTUI's own width of `text` (no line breaks) under its widest width method; never less than Bun's. */
export function engineWidth(text: string): number {
  let width = Bun.stringWidth(text)
  for (const widthMethod of WIDTH_METHODS) {
    try {
      width = Math.max(width, measureEditText(text, { widthMethod }))
    } catch {
      // Without the native library assume the widest case: two cells per code point.
      width = Math.max(width, 2 * [...text].length)
    }
  }
  return width
}

/** The grapheme clusters of `text` (Intl.Segmenter). */
export function graphemesOf(text: string): string[] {
  return Array.from(GRAPHEMES.segment(text), (part) => part.segment)
}

/** Display cells of one grapheme cluster as the renderer may draw it (cached). */
export function graphemeWidth(grapheme: string): number {
  if (grapheme.length === 1 && PRINTABLE_ASCII.test(grapheme)) return 1
  const cached = widthCache.get(grapheme)
  if (cached !== undefined) return cached
  const width = engineWidth(grapheme)
  if (widthCache.size >= WIDTH_CACHE_LIMIT) widthCache.clear()
  widthCache.set(grapheme, width)
  return width
}

/** True when every character is printable ASCII (one cell each under every width method). */
export function isPrintableAscii(text: string): boolean {
  return PRINTABLE_ASCII.test(text)
}

/** Display cells of `text`: the sum of its grapheme widths (wide characters 2, combining marks 0). */
export function cellWidth(text: string): number {
  if (PRINTABLE_ASCII.test(text)) return text.length
  let width = 0
  for (const part of GRAPHEMES.segment(text)) width += graphemeWidth(part.segment)
  return width
}

/** Word-wraps plain text (no escape sequences) to `width` cells; paragraphs split on "\n", empty ones kept. */
export function wrapText(text: string, width: number): string[] {
  const max = Math.max(Math.floor(width), 8)
  const lines: string[] = []
  for (const paragraph of text.replace(/\t/g, "    ").split("\n")) {
    // A paragraph that fits is kept as it is (indentation and inner spacing included).
    if (cellWidth(paragraph) <= max) {
      lines.push(paragraph)
      continue
    }
    wrapParagraph(paragraph, max, lines)
  }
  return lines
}

function wrapParagraph(paragraph: string, max: number, lines: string[]): void {
  let current = ""
  for (const word of paragraph.split(/ +/)) {
    const candidate = current.length === 0 ? word : `${current} ${word}`
    if (cellWidth(candidate) <= max) {
      current = candidate
      continue
    }
    if (current.length > 0) lines.push(current)
    current = ""
    for (const piece of splitWord(word, max)) {
      if (current.length > 0) lines.push(current)
      current = piece
    }
  }
  if (current.length > 0) lines.push(current)
}

/** Splits a word wider than `max` cells into pieces of at most `max` cells (grapheme boundaries). */
function splitWord(word: string, max: number): string[] {
  if (cellWidth(word) <= max) return [word]
  const pieces: string[] = []
  let piece = ""
  let used = 0
  for (const char of graphemesOf(word)) {
    const w = graphemeWidth(char)
    if (used + w > max && piece.length > 0) {
      pieces.push(piece)
      piece = ""
      used = 0
    }
    piece += char
    used += w
  }
  if (piece.length > 0) pieces.push(piece)
  return pieces
}

/** Cuts `text` to at most `max` cells, ending with "…" when it was cut. */
export function truncate(text: string, max: number): string {
  const limit = Math.floor(max)
  if (limit <= 0) return ""
  if (cellWidth(text) <= limit) return text
  let out = ""
  let used = 0
  for (const char of graphemesOf(text)) {
    const w = graphemeWidth(char)
    if (used + w > limit - 1) break
    out += char
    used += w
  }
  return `${out}…`
}

/** "0:07", "1:05", "12:00" for a recording or turn duration. */
export function formatClock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000))
  const minutes = Math.floor(total / 60)
  const seconds = total % 60
  return `${minutes}:${String(seconds).padStart(2, "0")}`
}

/** "12 s", "2 min 5 s" for elapsed times in turn labels. */
export function formatSeconds(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000))
  if (total < 60) return `${total} s`
  const minutes = Math.floor(total / 60)
  const seconds = total % 60
  return seconds === 0 ? `${minutes} min` : `${minutes} min ${seconds} s`
}

/** UTF-8 bytes of `text`. */
export function utf8Bytes(text: string): number {
  return Buffer.byteLength(text, "utf8")
}

/** Pads or cuts `text` to exactly `cells` display cells. */
export function fitCellsExact(text: string, cells: number): string {
  const cut = truncate(text, cells)
  const pad = Math.max(0, cells - cellWidth(cut))
  return cut + " ".repeat(pad)
}
