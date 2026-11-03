/**
 * Frame helpers for UI tests (design 11.3.1): a captured char frame as rows, and where a text sits in it.
 */

/** The frame's rows with trailing spaces removed (a captured frame ends with a newline). */
export function rows(frame: string): string[] {
  const body = frame.endsWith("\n") ? frame.slice(0, -1) : frame
  return body.split("\n").map((row) => row.replace(/\s+$/u, ""))
}

/** The first position of `text` in the frame (x in UTF-16 units of the row), or null. */
export function locate(frame: string, text: string): { x: number; y: number } | null {
  const lines = frame.split("\n")
  for (let y = 0; y < lines.length; y += 1) {
    const x = (lines[y] ?? "").indexOf(text)
    if (x >= 0) return { x, y }
  }
  return null
}

/** The row index of the first row containing `text`, or -1. */
export function rowOf(frame: string, text: string): number {
  return locate(frame, text)?.y ?? -1
}

/** The last non-empty row index. */
export function lastContentRow(frame: string): number {
  const all = rows(frame)
  for (let index = all.length - 1; index >= 0; index -= 1) if ((all[index] ?? "").length > 0) return index
  return -1
}
