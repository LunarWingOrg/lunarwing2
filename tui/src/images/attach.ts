/**
 * /img <path> and /attach <path> (design 6.1), the step every source (clipboard, file, drop, shot) uses to turn
 * validated bytes into a PendingImage, and the storage disclosure (5.6, SEC-09, DEC-3).
 *
 * readImageFile() replaces v4:tui/src/images.ts attachFile (readFileSync on an unchecked path): it is async and
 * bounded. stat must report a regular file within the limit (a FIFO, device or directory could block or never
 * end); the file is opened O_NONBLOCK, so a FIFO swapped in after the stat cannot block either, and the open
 * descriptor is checked again (same inode, regular, within the limit); the read stops at maxImageBytes + 1 bytes
 * because sizes can lie (procfs). Symbolic links are followed: the user named the file.
 *
 * Images are stored in the tenant workspace with no expiry once sent (5.6). DEC-3 is open; this client keeps the
 * default "keep and disclose": the first image from each source shows imageStorageNotice() once per profile
 * (acknowledgeImageSource, the imageAck key of the profile state) and the attachment bar always shows
 * imageStorageCaption().
 */
import { constants, type Stats } from "node:fs"
import { open, stat, type FileHandle } from "node:fs/promises"
import { homedir } from "node:os"
import { basename, resolve } from "node:path"
import { displayValue } from "../config/flags.ts"
import { displayPath, expandHome } from "../config/paths.ts"
import type { ImageSourceKind, StateFile } from "../config/types.ts"
import { sanitizeLine } from "../sanitize.ts"
import { IMAGE_MIME_TYPES, type ImageMime, type PendingImage } from "../state/model.ts"
import { checkImageBytes, formatBytes, ImageInputError, OVERSIZE_ADVICE, type ImageInspector } from "./limits.ts"
import { fileUriToPath } from "./paste-paths.ts"

/** File names sent with an image (REPLv3 `filename`) and shown in captions. */
export const MAX_FILENAME_CHARS = 128
const READ_CHUNK_BYTES = 256 * 1024

/** "img-" + UUIDv4; unique across sources, so composer/attach never drops an image as a duplicate. */
export function newImageId(): string {
  return `img-${crypto.randomUUID()}`
}

export interface PendingImageOptions {
  source: ImageSourceKind
  filename: string | null
  maxImageBytes: number
  /** The server's accepted types (Limits.imageMimeTypes); default all four. */
  imageMimeTypes?: readonly ImageMime[]
  newId?: () => string
  inspect?: ImageInspector
  /** How the image is named in messages; default the file name, else "<source> image". */
  label?: string
}

/** Validates (limits.ts checkImageBytes) and wraps bytes; the sniffed type is the only type kept. */
export function createPendingImage(bytes: Uint8Array, opts: PendingImageOptions): PendingImage {
  const filename = opts.filename === null ? null : imageFileName(opts.filename)
  const label = opts.label ?? filename ?? `${opts.source} image`
  const checked = checkImageBytes(
    bytes,
    { maxImageBytes: opts.maxImageBytes, imageMimeTypes: opts.imageMimeTypes ?? IMAGE_MIME_TYPES },
    { label, ...(opts.inspect ? { inspect: opts.inspect } : {}) },
  )
  return {
    id: (opts.newId ?? newImageId)(),
    mime: checked.mime,
    bytes,
    filename,
    width: checked.width,
    height: checked.height,
    source: opts.source,
  }
}

/** The basename without control or bidi characters, at most 128 characters; null when nothing is left. */
export function imageFileName(path: string): string | null {
  const name = sanitizeLine(basename(path)).trim()
  if (name.length === 0 || name === "." || name === "..") return null
  const chars = Array.from(name)
  return chars.length <= MAX_FILENAME_CHARS ? name : chars.slice(0, MAX_FILENAME_CHARS).join("")
}

/** Trim; strip one pair of surrounding quotes; file:///… and file://localhost/…; "~/"; relative to `cwd`. */
export function resolveImagePath(input: string, opts: { cwd: string; home: string }): string {
  const text = stripQuotePair(input.trim())
  if (text.length === 0) throw new ImageInputError("bad_path", "no file given: /img <path>")
  if (/^file:/i.test(text)) {
    const path = fileUriToPath(text)
    if (path === null) {
      throw new ImageInputError("bad_path", "only local file URIs are supported (file:///path or file://localhost/path)")
    }
    return path
  }
  return resolve(opts.cwd, expandHome(text, opts.home))
}

function stripQuotePair(text: string): string {
  const first = text.charAt(0)
  if (text.length >= 2 && (first === "'" || first === '"') && text.endsWith(first)) return text.slice(1, -1).trim()
  return text
}

export interface ReadImageFileOptions {
  maxImageBytes: number
  /** Relative paths resolve against this directory. */
  cwd: string
  /** For "~/"; default os.homedir(). */
  home?: string
  imageMimeTypes?: readonly ImageMime[]
  /** Default "file"; drag-and-drop passes "drop", files copied in a file manager pass "clipboard". */
  source?: ImageSourceKind
  newId?: () => string
  inspect?: ImageInspector
}

/** Reads, bounds and validates an image file; throws ImageInputError with a one-line reason. */
export async function readImageFile(path: string, opts: ReadImageFileOptions): Promise<PendingImage> {
  const home = opts.home ?? homedir()
  const resolved = resolveImagePath(path, { cwd: opts.cwd, home })
  const shown = displayValue(displayPath(resolved, home))
  const filename = imageFileName(resolved)
  const label = filename === null ? shown : displayValue(filename)
  const bytes = await readBoundedImage(resolved, { shown, label, maxBytes: opts.maxImageBytes })
  return createPendingImage(bytes, {
    source: opts.source ?? "file",
    filename,
    maxImageBytes: opts.maxImageBytes,
    label,
    ...(opts.imageMimeTypes ? { imageMimeTypes: opts.imageMimeTypes } : {}),
    ...(opts.newId ? { newId: opts.newId } : {}),
    ...(opts.inspect ? { inspect: opts.inspect } : {}),
  })
}

interface ReadTarget {
  /** The path as shown in messages. */
  shown: string
  /** The file name as shown in size messages. */
  label: string
  maxBytes: number
}

async function readBoundedImage(path: string, target: ReadTarget): Promise<Uint8Array> {
  let before: Stats
  try {
    before = await stat(path)
  } catch (error) {
    throw fileError(error, target.shown)
  }
  checkFileStats(before, target)
  let handle: FileHandle
  try {
    handle = await open(path, constants.O_RDONLY | constants.O_NONBLOCK | (constants.O_NOCTTY ?? 0))
  } catch (error) {
    throw fileError(error, target.shown)
  }
  try {
    const opened = await handle.stat()
    if (opened.dev !== before.dev || opened.ino !== before.ino) {
      throw new ImageInputError("unreadable", `${target.shown} changed while it was being opened; try again`)
    }
    checkFileStats(opened, target)
    return await readAtMost(handle, target, opened.size)
  } catch (error) {
    throw error instanceof ImageInputError ? error : fileError(error, target.shown)
  } finally {
    await handle.close()
  }
}

function checkFileStats(stats: Stats, target: ReadTarget): void {
  if (stats.isDirectory()) throw new ImageInputError("not_a_file", `${target.shown} is a directory`)
  if (!stats.isFile()) throw new ImageInputError("not_a_file", `${target.shown} is not a regular file`)
  if (stats.size > target.maxBytes) {
    throw new ImageInputError(
      "too_large",
      `${target.label} is ${formatBytes(stats.size)}; the limit is ${formatBytes(target.maxBytes)}: ${OVERSIZE_ADVICE}`,
    )
  }
}

/** Sequential reads of at most maxBytes + 1 bytes in total, whatever the size the descriptor reported. */
async function readAtMost(handle: FileHandle, target: ReadTarget, reportedSize: number): Promise<Uint8Array> {
  const cap = target.maxBytes + 1
  const chunks: Uint8Array[] = []
  let total = 0
  let chunkSize = Math.min(cap, Math.max(reportedSize + 1, 4096), READ_CHUNK_BYTES)
  while (total < cap) {
    const chunk = new Uint8Array(Math.min(chunkSize, cap - total))
    const { bytesRead } = await handle.read(chunk, 0, chunk.length, null)
    if (bytesRead === 0) break
    chunks.push(bytesRead === chunk.length ? chunk : chunk.subarray(0, bytesRead))
    total += bytesRead
    chunkSize = READ_CHUNK_BYTES
  }
  if (total > target.maxBytes) {
    throw new ImageInputError(
      "too_large",
      `${target.label} is larger than ${formatBytes(target.maxBytes)}, the limit: ${OVERSIZE_ADVICE}`,
    )
  }
  return concatChunks(chunks, total)
}

/** An exact-size array: never a view into a larger read buffer, whose spare bytes `.buffer` would expose. */
function concatChunks(chunks: readonly Uint8Array[], total: number): Uint8Array {
  const only = chunks.length === 1 ? chunks[0] : undefined
  if (only !== undefined && only.byteOffset === 0 && only.buffer.byteLength === total) return only
  const out = new Uint8Array(total)
  let offset = 0
  for (const chunk of chunks) {
    out.set(chunk, offset)
    offset += chunk.byteLength
  }
  return out
}

function errnoCode(error: unknown): string | undefined {
  return typeof error === "object" && error !== null && "code" in error ? String(error.code) : undefined
}

function fileError(error: unknown, shown: string): ImageInputError {
  switch (errnoCode(error)) {
    case "ENOENT":
    case "ENOTDIR":
      return new ImageInputError("not_found", `no such file: ${shown}`)
    case "EACCES":
    case "EPERM":
      return new ImageInputError("unreadable", `cannot read ${shown}: permission denied`)
    case "ELOOP":
      return new ImageInputError("unreadable", `cannot read ${shown}: too many symbolic links`)
    case "EISDIR":
      return new ImageInputError("not_a_file", `${shown} is a directory`)
    default:
      return new ImageInputError("unreadable", `cannot read ${shown} (${errnoCode(error) ?? "I/O error"})`)
  }
}

// ---------------------------------------------------------------------------------------------------------
// Storage disclosure (5.6, SEC-09, DEC-3)
// ---------------------------------------------------------------------------------------------------------

function workspaceOwner(tenant: string | null): string {
  return tenant === null || tenant.length === 0 ? "the daemon's" : `${sanitizeLine(tenant)}'s`
}

/** The one-line notice for the first image from each source, once per profile. */
export function imageStorageNotice(tenant: string | null): string {
  return `images you send are stored in ${workspaceOwner(tenant)} workspace (no expiry) and are readable from every channel`
}

/** The attachment bar's caption ends with this. */
export function imageStorageCaption(tenant: string | null): string {
  return `stored in ${workspaceOwner(tenant)} workspace`
}

/**
 * Records the first use of an image source in the profile state (imageAck). True when this was the first use, so
 * the caller shows imageStorageNotice(); false when it was acknowledged before. If the state file cannot be
 * written the notice is shown (true) and may show again next time.
 */
export async function acknowledgeImageSource(
  stateFile: Pick<StateFile, "read" | "update">,
  source: ImageSourceKind,
  at: number,
): Promise<boolean> {
  try {
    if ((await stateFile.read()).imageAck?.[source] !== undefined) return false
    let first = false
    await stateFile.update((state) => {
      if (state.imageAck?.[source] !== undefined) return state
      first = true
      return { ...state, imageAck: { ...state.imageAck, [source]: new Date(at).toISOString() } }
    })
    return first
  } catch {
    return true
  }
}
