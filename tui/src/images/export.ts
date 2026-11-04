/**
 * /image open [n] and /image save [n] [path] (design 6.4, UX-14, 10.5): full-size views of generated images,
 * which are block thumbnails of at most 56x12 cells inside tmux.
 *
 * - open writes the validated bytes of the newest (or n-th newest) generated image to
 *   $XDG_RUNTIME_DIR/wingtip/img-<uuid>.<ext>. The directory is created 0700 and checked with lstat (a real
 *   directory, owned by this uid, no group or other bits); the file is opened O_CREAT | O_EXCL | O_NOFOLLOW |
 *   O_WRONLY with mode 0600. Then ["xdg-open", file] is spawned (argv only, the child environment). Exported files
 *   are removed by dispose() on quit.
 * - save writes a 0600 file the same way (O_EXCL: an existing file or link is never replaced), by default
 *   ./lunarwing-<tenant>-<YYYYmmdd-HHMMSS>-<n>.<ext>.
 * Both refuse an image whose bytes were evicted ("image no longer in memory"; the newest 20 keep their bytes).
 */
import { constants } from "node:fs"
import { lstat, mkdir, open, rm, rmdir, stat, type FileHandle } from "node:fs/promises"
import { homedir } from "node:os"
import { join, resolve } from "node:path"
import { SystemClock, type Clock } from "@opentui/core"
import { displayValue } from "../config/flags.ts"
import { displayPath, expandHome } from "../config/paths.ts"
import { bunSpawn, type ChildEnv, type SpawnFn } from "../config/types.ts"
import type { AppState, ImageMime, TranscriptImage } from "../state/model.ts"
import { ImageInputError } from "./limits.ts"
import { extensionFor, sniffImageMime } from "./sniff.ts"

export const EXPORT_FILE_MODE = 0o600
export const EXPORT_DIR_MODE = 0o700
export const EVICTED_IMAGE_MESSAGE = "image no longer in memory"
/** How long open() waits for xdg-open to report a failure; a viewer that keeps running counts as success. */
export const OPENER_WAIT_MS = 1_500
const MAX_IMAGE_INDEX = 999
const EXCLUSIVE_WRITE = constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW | constants.O_WRONLY

/** The parts of a TranscriptImage an export needs. */
export type ExportableImage = Pick<TranscriptImage, "mime" | "bytes">

// ---------------------------------------------------------------------------------------------------------
// Picking the image and parsing the command
// ---------------------------------------------------------------------------------------------------------

/** Generated images (TurnRecord.images, under replies), newest first. */
export function generatedImages(state: Pick<AppState, "order" | "turns">): TranscriptImage[] {
  const out: TranscriptImage[] = []
  for (let i = state.order.length - 1; i >= 0; i -= 1) {
    const id = state.order[i]
    const turn = id === undefined ? undefined : state.turns[id]
    if (turn === undefined) continue
    for (let j = turn.images.length - 1; j >= 0; j -= 1) {
      const image = turn.images[j]
      if (image !== undefined) out.push(image)
    }
  }
  return out
}

/** The n-th newest generated image (n = 1 is the newest); throws ImageInputError("no_image"). */
export function pickGeneratedImage(state: Pick<AppState, "order" | "turns">, n = 1): TranscriptImage {
  const images = generatedImages(state)
  const image = images[n - 1]
  if (image !== undefined) return image
  if (images.length === 0) throw new ImageInputError("no_image", "no generated image in this session")
  const count = images.length === 1 ? "is 1 generated image" : `are ${images.length} generated images`
  throw new ImageInputError("no_image", `there ${count}`)
}

export type ImageCommand =
  | { action: "open"; n: number }
  | { action: "save"; n: number; path: string | null }
  | { action: "error"; message: string }

export const IMAGE_COMMAND_USAGE = "usage: /image open [n] | /image save [n] [path]"

/** The arguments after "/image": "open [n]" or "save [n] [path]" (the path may contain spaces or be quoted). */
export function parseImageCommand(args: string): ImageCommand {
  const match = /^(\S+)\s*([\s\S]*)$/.exec(args.trim())
  const action = match?.[1]?.toLowerCase()
  const rest = match?.[2]?.trim() ?? ""
  if (action !== "open" && action !== "save") return { action: "error", message: IMAGE_COMMAND_USAGE }
  const indexed = /^(\d+)(?:\s+([\s\S]+))?$/.exec(rest)
  const n = indexed?.[1] === undefined ? 1 : Number(indexed[1])
  const path = indexed ? (indexed[2]?.trim() ?? "") : rest
  if (!Number.isInteger(n) || n < 1 || n > MAX_IMAGE_INDEX) return { action: "error", message: IMAGE_COMMAND_USAGE }
  if (action === "open") return path.length === 0 ? { action, n } : { action: "error", message: IMAGE_COMMAND_USAGE }
  return { action, n, path: path.length === 0 ? null : stripQuotePair(path) }
}

function stripQuotePair(text: string): string {
  const first = text.charAt(0)
  if (text.length >= 2 && (first === "'" || first === '"') && text.endsWith(first)) return text.slice(1, -1)
  return text
}

function pad(value: number, width = 2): string {
  return String(value).padStart(width, "0")
}

/** lunarwing-<label>-<YYYYmmdd-HHMMSS>-<n>.<ext> in local time; the label is the tenant (or the profile). */
export function defaultSaveName(label: string | null, at: Date, n: number, mime: ImageMime): string {
  const safe = (label ?? "").toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "") || "local"
  const day = `${at.getFullYear()}${pad(at.getMonth() + 1)}${pad(at.getDate())}`
  const time = `${pad(at.getHours())}${pad(at.getMinutes())}${pad(at.getSeconds())}`
  const stamp = `${day}-${time}`
  return `lunarwing-${safe}-${stamp}-${n}.${extensionFor(mime)}`
}

// ---------------------------------------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------------------------------------

function errnoCode(error: unknown): string | undefined {
  return typeof error === "object" && error !== null && "code" in error ? String(error.code) : undefined
}

/** Creates `path` O_CREAT | O_EXCL | O_NOFOLLOW | O_WRONLY 0600 and writes `bytes`; never replaces anything. */
export async function writeExclusive(path: string, bytes: Uint8Array, shown = path): Promise<void> {
  let handle: FileHandle
  try {
    handle = await open(path, EXCLUSIVE_WRITE, EXPORT_FILE_MODE)
  } catch (error) {
    throw writeError(error, shown)
  }
  try {
    await handle.writeFile(bytes)
  } catch (error) {
    await handle.close().catch(() => undefined)
    await rm(path, { force: true }).catch(() => undefined)
    throw writeError(error, shown)
  }
  await handle.close()
}

function writeError(error: unknown, shown: string): ImageInputError {
  switch (errnoCode(error)) {
    case "EEXIST":
    case "ELOOP":
      return new ImageInputError("exists", `${shown} already exists; choose another name`)
    case "ENOENT":
    case "ENOTDIR":
      return new ImageInputError("not_found", `cannot write ${shown}: the directory does not exist`)
    case "EACCES":
    case "EPERM":
    case "EROFS":
      return new ImageInputError("failed", `cannot write ${shown}: permission denied`)
    case "EISDIR":
      return new ImageInputError("exists", `${shown} is a directory`)
    default:
      return new ImageInputError("failed", `cannot write ${shown} (${errnoCode(error) ?? "I/O error"})`)
  }
}

/** mkdir 0700 if missing, then lstat: a directory (not a link) owned by `uid` with no group or other bits. */
export async function ensurePrivateDir(dir: string, uid: number): Promise<void> {
  try {
    await mkdir(dir, { mode: EXPORT_DIR_MODE })
  } catch (error) {
    if (errnoCode(error) !== "EEXIST") {
      throw new ImageInputError("unavailable", `cannot create ${displayValue(dir)} (${errnoCode(error) ?? "I/O error"})`)
    }
  }
  const info = await lstat(dir)
  if (!info.isDirectory()) throw new ImageInputError("unsafe_dir", `refusing to use ${displayValue(dir)}: not a directory`)
  if (info.uid !== uid) throw new ImageInputError("unsafe_dir", `refusing to use ${displayValue(dir)}: owned by uid ${info.uid}`)
  if ((info.mode & 0o077) !== 0) {
    const mode = (info.mode & 0o777).toString(8).padStart(3, "0")
    throw new ImageInputError("unsafe_dir", `refusing to use ${displayValue(dir)}: mode 0${mode} (chmod 700 it)`)
  }
}

function validatedBytes(image: ExportableImage): { bytes: Uint8Array; mime: ImageMime } {
  if (image.bytes === null) throw new ImageInputError("evicted", EVICTED_IMAGE_MESSAGE)
  const mime = sniffImageMime(image.bytes)
  if (mime === null || mime !== image.mime) throw new ImageInputError("failed", "the image bytes do not match their type")
  return { bytes: image.bytes, mime }
}

// ---------------------------------------------------------------------------------------------------------
// The exporter
// ---------------------------------------------------------------------------------------------------------

export interface ImageExporterOptions {
  /** config/paths.ts resolveRuntimeDir(env): $XDG_RUNTIME_DIR/wingtip, or null without one. */
  runtimeDir: string | null
  /** config/child-env.ts buildChildEnv(), for xdg-open. */
  env: ChildEnv
  spawn?: SpawnFn
  clock?: Clock
  /** Default process.getuid(). */
  uid?: number
  /** For relative save paths; default process.cwd(). */
  cwd?: string
  /** For "~/" and for showing paths; default os.homedir(). */
  home?: string
  /** UUID for img-<uuid>; default crypto.randomUUID. */
  newId?: () => string
}

export interface SaveRequest {
  /** The user's path (relative to cwd, "~/" expanded); a directory gets the default name; null: the default. */
  path: string | null
  /** The n of the command, part of the default name. */
  n: number
  /** Tenant or profile, part of the default name. */
  label: string | null
  at: Date
}

export interface ImageExporter {
  /** Writes the image to the runtime directory and hands it to xdg-open; resolves with the file path. */
  open(image: ExportableImage): Promise<string>
  /** Writes the image to a new 0600 file; resolves with the path written. */
  save(image: ExportableImage, request: SaveRequest): Promise<string>
  /** Files open() wrote that dispose() will remove. */
  exported(): readonly string[]
  /** Removes the exported files (on quit). */
  dispose(): Promise<void>
}

export function createImageExporter(opts: ImageExporterOptions): ImageExporter {
  return new FileImageExporter(opts)
}

class FileImageExporter implements ImageExporter {
  readonly #opts: ImageExporterOptions
  readonly #files: string[] = []
  #disposed = false

  constructor(opts: ImageExporterOptions) {
    this.#opts = opts
  }

  #home(): string {
    return this.#opts.home ?? homedir()
  }

  #show(path: string): string {
    return displayValue(displayPath(path, this.#home()))
  }

  async open(image: ExportableImage): Promise<string> {
    if (this.#disposed) throw new ImageInputError("failed", "the image exporter was closed")
    const { bytes, mime } = validatedBytes(image)
    const dir = this.#opts.runtimeDir
    if (dir === null) {
      throw new ImageInputError("unavailable", "/image open needs XDG_RUNTIME_DIR for a private copy; use /image save")
    }
    await ensurePrivateDir(dir, this.#opts.uid ?? process.getuid?.() ?? -1)
    const file = join(dir, `img-${(this.#opts.newId ?? crypto.randomUUID.bind(crypto))()}.${extensionFor(mime)}`)
    await writeExclusive(file, bytes, this.#show(file))
    this.#files.push(file)
    await this.#launch(file)
    return file
  }

  async #launch(file: string): Promise<void> {
    const shown = this.#show(file)
    let status: number | null
    try {
      const child = (this.#opts.spawn ?? bunSpawn)(["xdg-open", file], {
        env: this.#opts.env,
        stdin: "ignore",
        stdout: "ignore",
        stderr: "ignore",
      })
      status = await settleWithin(child.exited, this.#opts.clock ?? new SystemClock(), OPENER_WAIT_MS)
    } catch (error) {
      const code = errnoCode(error)
      const why = code === "ENOENT" || code === "EACCES" ? "xdg-open is not installed" : "xdg-open could not start"
      throw new ImageInputError("unavailable", `${why}; the image is at ${shown} until you quit`)
    }
    if (status !== null && status !== 0) {
      throw new ImageInputError("failed", `xdg-open failed (exit ${status}); the image is at ${shown} until you quit`)
    }
  }

  async save(image: ExportableImage, request: SaveRequest): Promise<string> {
    const { bytes, mime } = validatedBytes(image)
    const cwd = this.#opts.cwd ?? process.cwd()
    const name = defaultSaveName(request.label, request.at, request.n, mime)
    let target = request.path === null ? join(cwd, name) : resolve(cwd, expandHome(request.path, this.#home()))
    if (request.path !== null && (request.path.endsWith("/") || (await isDirectory(target)))) target = join(target, name)
    await writeExclusive(target, bytes, this.#show(target))
    return target
  }

  exported(): readonly string[] {
    return [...this.#files]
  }

  async dispose(): Promise<void> {
    this.#disposed = true
    const files = this.#files.splice(0)
    await Promise.all(files.map((file) => rm(file, { force: true }).catch(() => undefined)))
    // Succeeds only when no other instance still has exports there.
    if (files.length > 0 && this.#opts.runtimeDir !== null) await rmdir(this.#opts.runtimeDir).catch(() => undefined)
  }
}

async function isDirectory(path: string): Promise<boolean> {
  try {
    return (await stat(path)).isDirectory()
  } catch {
    return false
  }
}

/** The promise's value if it settles within `ms`, else null (the opener keeps running). */
async function settleWithin(promise: Promise<number>, clock: Clock, ms: number): Promise<number | null> {
  let timer: ReturnType<Clock["setTimeout"]> | undefined
  const timeout = new Promise<null>((resolveTimeout) => {
    timer = clock.setTimeout(() => resolveTimeout(null), ms)
  })
  try {
    return await Promise.race([promise, timeout])
  } finally {
    if (timer !== undefined) clock.clearTimeout(timer)
  }
}
