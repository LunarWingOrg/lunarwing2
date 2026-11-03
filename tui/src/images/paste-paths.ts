/**
 * Drag-and-drop detection for bracketed pastes (design 6.1) and local file URIs (shared with /img and the
 * clipboard's text/uri-list).
 *
 * kitty 0.44 delivers a drop as unquoted, newline-joined local paths through bracketed paste, which tmux forwards;
 * WezTerm escapes spaces with backslashes; VTE single-quotes paths and separates several with spaces. The global
 * usePaste handler (T1.7) decodes the paste with decodePasteBytes and calls parseDroppedPaths() synchronously:
 * a non-null result means "this is a drop", so it calls event.preventDefault() and attaches the paths
 * asynchronously with readImageFile(); null means an ordinary paste for the composer.
 *
 * Everything here is pure: no file system access, so the decision is synchronous and cheap.
 */

/** Pastes larger than this are never treated as drops. */
export const DROP_MAX_BYTES = 64 * 1024

const IMAGE_EXTENSION = /\.(?:png|jpe?g|gif|webp)$/i
/** C0 controls and DEL never occur in a dropped path; their presence means the paste is not a drop. */
const CONTROL = /[\u0000-\u001f\u007f]/
const LINE_BREAK = /\r\n|\r|\n/

/** True when the path ends in .png, .jpg, .jpeg, .gif or .webp (case-insensitive). The bytes decide the type. */
export function hasImageExtension(path: string): boolean {
  return IMAGE_EXTENSION.test(path)
}

/**
 * file:///abs/path and file://localhost/abs/path (scheme and host case-insensitive), percent-decoded, to an
 * absolute path; null for other hosts, other schemes, a query or fragment, bad escapes, NUL or control characters.
 * The path is not normalized, so ".." keeps the kernel's meaning.
 */
export function fileUriToPath(uri: string): string | null {
  const match = /^file:\/\/(localhost)?(\/[^?#]*)$/i.exec(uri.trim())
  const encoded = match?.[2]
  if (encoded === undefined) return null
  let path: string
  try {
    path = decodeURIComponent(encoded)
  } catch {
    return null
  }
  return CONTROL.test(path) ? null : path
}

/** RFC 2483 text/uri-list: local file paths in order; comments ("#") and non-file URIs are skipped. */
export function parseUriList(text: string): string[] {
  const paths: string[] = []
  for (const raw of text.split(LINE_BREAK)) {
    const line = raw.trim()
    if (line.length === 0 || line.startsWith("#")) continue
    const path = fileUriToPath(line)
    if (path !== null) paths.push(path)
  }
  return paths
}

export interface DropOptions {
  /** More paths than this is not a drop (pass the server's maxImages). */
  maxPaths: number
}

/**
 * The image paths of a drag-and-drop paste, or null when the paste is not a drop: larger than 64 KiB, no
 * non-empty line, more than `maxPaths` paths, or any entry that is not an absolute path or local file URI ending
 * in an image extension. Each line is one kitty path (spaces allowed) unless it uses quotes or backslashes as
 * shell quoting, in which case it is split like a shell word list (WezTerm, VTE). kitty sends file names as they
 * are, so a quote inside a word ("Chris's shot.png", a"b.png), a backslash before a letter or digit, or quotes
 * that do not balance keep the line as one literal path.
 */
export function parseDroppedPaths(text: string, opts: DropOptions): string[] | null {
  if (!Number.isInteger(opts.maxPaths) || opts.maxPaths < 1) return null
  if (text.length > DROP_MAX_BYTES || Buffer.byteLength(text, "utf8") > DROP_MAX_BYTES) return null
  const lines = text
    .split(LINE_BREAK)
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
  if (lines.length === 0 || lines.length > opts.maxPaths) return null
  const paths: string[] = []
  for (const line of lines) {
    const entries = lineEntries(line)
    if (entries === null) return null
    for (const entry of entries) {
      const path = droppedPath(entry)
      if (path === null) return null
      paths.push(path)
    }
    if (paths.length > opts.maxPaths) return null
  }
  return paths
}

function lineEntries(line: string): string[] | null {
  if (/['"\\]/.test(line)) {
    if (isLiteralPath(line)) return [line]
    const words = splitShellWords(line)
    if (words !== null) return words
    // Unbalanced quotes are not shell quoting: kitty's unquoted path, taken as it is.
    return droppedPath(line) === null ? null : [line]
  }
  // kitty sends one unquoted path per line, spaces included. A line of several absolute image paths separated by
  // spaces cannot be one path (a file name never contains "/"), so it is split.
  const words = line.split(/\s+/)
  if (words.length > 1 && words.every((word) => droppedPath(word) !== null)) return words
  return [line]
}

/**
 * A kitty line whose quote or backslash cannot be shell quoting: quoting terminals start a quoted word with the
 * quote (VTE '...') and escape only special characters (WezTerm "\ ", "\'"), so a quote right after a word
 * character or a backslash before a letter or digit is part of the file name.
 */
function isLiteralPath(line: string): boolean {
  if (!line.startsWith("/") || droppedPath(line) === null) return false
  return /[^\s\\'"]['"]/u.test(line) || /\\[A-Za-z0-9]/u.test(line)
}

/** A paste refused as a drop that still reads like dropped image files (absolute paths or file URIs, image names). */
export function looksLikeDrop(text: string): boolean {
  if (text.length > DROP_MAX_BYTES) return false
  const lines = text
    .split(LINE_BREAK)
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
  return lines.length > 0 && lines.every((line) => /^(?:['"]?\/|file:)/iu.test(line) && /\.(?:png|jpe?g|gif|webp)['"]?$/iu.test(line))
}

function droppedPath(entry: string): string | null {
  const path = /^file:/i.test(entry) ? fileUriToPath(entry) : entry
  if (path === null || !path.startsWith("/") || CONTROL.test(path) || !hasImageExtension(path)) return null
  return path
}

/**
 * Shell-style word splitting for one line: whitespace separates words; '...' is literal; "..." honours \" \\ \$ \`;
 * a backslash outside quotes escapes the next character. Null when a quote is unbalanced or the line ends in a
 * lone backslash.
 */
export function splitShellWords(line: string): string[] | null {
  const words: string[] = []
  let word = ""
  let inWord = false
  let quote: "'" | '"' | null = null
  for (let i = 0; i < line.length; i += 1) {
    const c = line.charAt(i)
    if (quote === "'") {
      if (c === "'") quote = null
      else word += c
      continue
    }
    if (quote === '"') {
      if (c === '"') quote = null
      else if (c === "\\" && i + 1 < line.length && '"\\$`'.includes(line.charAt(i + 1))) word += line.charAt(++i)
      else word += c
      continue
    }
    if (/\s/.test(c)) {
      if (inWord) words.push(word)
      word = ""
      inWord = false
      continue
    }
    inWord = true
    if (c === "'" || c === '"') quote = c
    else if (c === "\\") {
      if (i + 1 >= line.length) return null
      word += line.charAt(++i)
    } else word += c
  }
  if (quote !== null) return null
  if (inWord) words.push(word)
  return words
}
