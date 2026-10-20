/** Small text utilities for terminal layout. */

/** Word-wrap text to a display width (ANSI-free plain text). */
export function wrapText(text: string, width: number): string[] {
  const max = Math.max(width, 8)
  const lines: string[] = []
  for (const paragraph of text.split("\n")) {
    if (paragraph.length === 0) {
      lines.push("")
      continue
    }
    let current = ""
    for (const word of paragraph.split(/[ \t]+/)) {
      const candidate = current.length === 0 ? word : `${current} ${word}`
      if (candidate.length <= max) {
        current = candidate
      } else {
        if (current.length > 0) {
          lines.push(current)
        }
        current = word.length > max ? word.slice(0, max) : word
      }
    }
    if (current.length > 0) {
      lines.push(current)
    }
  }
  return lines
}

/** Decode a base64 data URL (`data:image/png;base64,…`) to bytes. */
export function decodeDataUrl(dataUrl: string): Uint8Array | null {
  const comma = dataUrl.indexOf(",")
  if (comma === -1) {
    return null
  }
  try {
    const base64 = dataUrl.slice(comma + 1)
    return new Uint8Array(Buffer.from(base64, "base64"))
  } catch {
    return null
  }
}

/** Truncate a string with an ellipsis. */
export function truncate(text: string, max: number): string {
  if (text.length <= max) {
    return text
  }
  return `${text.slice(0, Math.max(max - 1, 1))}…`
}
