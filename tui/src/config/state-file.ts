/**
 * Client state files (design 3.11.5, 3.11.6): `${XDG_STATE_HOME:-~/.local/state}/lunarwing-tui/` (0700) with
 * `state.json` ({"lastTenant"}) and `<profile>.json` (lastThreadId, lastReplThread, voiceAck, imageAck,
 * voiceDevice), all 0600.
 *
 * Writes are atomic: a temp file opened O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW with mode 0600, then rename().
 * The temp name carries the pid (`<file>.<pid>.tmp`) so the primary and a secondary instance of one profile
 * never collide on it; writes to one path are serialized within the process. A secondary instance (another
 * live process holds the profile lock, config/instance-lock.ts) never writes lastThreadId, lastReplThread or
 * lastTenant: those changes are dropped silently. Reads are lenient: a missing, foreign, oversized or corrupt
 * file reads as {} and invalid fields are dropped, because state is advisory.
 */
import { constants } from "node:fs"
import { chmod, mkdir, open, rename, stat, unlink } from "node:fs/promises"
import { isValidClientId } from "../state/ids.ts"
import { isVoiceSourceId, type VoiceSourceId } from "../voice/types.ts"
import { assertProfileName, ConfigError, displayValue, isValidTenantName } from "./flags.ts"
import { globalStatePath, profileStatePath } from "./paths.ts"
import { currentUid, readCheckedFile } from "./token-file.ts"
import {
  PRIMARY_ONLY_STATE_KEYS,
  type GlobalState,
  type ImageSourceKind,
  type ProfileState,
  type StateFile,
} from "./types.ts"

export const STATE_FILE_MAX_BYTES = 64 * 1024

const IMAGE_SOURCES: readonly ImageSourceKind[] = ["clipboard", "file", "drop", "shot"]
const REPL_THREAD_PATTERN = /^tui-[a-z0-9_-]{1,32}-[0-9a-f]{16}$/
const TIMESTAMP_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$/
const DEVICE_PATTERN = /^[^\u0000-\u001f\u007f-\u009f]{1,128}$/

function errorCode(error: unknown): string {
  return typeof error === "object" && error !== null && "code" in error ? String(error.code) : "I/O error"
}

/** Creates the state directory (0700) if needed; an existing one must be ours and is tightened to 0700. */
export async function ensureStateDir(stateDir: string, uid: number = currentUid()): Promise<void> {
  const label = `state directory ${displayValue(stateDir)}`
  try {
    await mkdir(stateDir, { recursive: true, mode: 0o700 })
    const stats = await stat(stateDir)
    if (!stats.isDirectory()) throw new ConfigError(`${label} is not a directory`)
    if (stats.uid !== uid) throw new ConfigError(`${label} is owned by uid ${stats.uid}, not by you (uid ${uid})`)
    if ((stats.mode & 0o077) !== 0) await chmod(stateDir, 0o700)
  } catch (error) {
    if (error instanceof ConfigError) throw error
    throw new ConfigError(`${label} cannot be created (${errorCode(error)}); set XDG_STATE_HOME to a writable directory`)
  }
}

// ---------------------------------------------------------------------------------------------------------
// Validation (applied on read and before every write)
// ---------------------------------------------------------------------------------------------------------

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function isTimestamp(value: unknown): value is string {
  return typeof value === "string" && TIMESTAMP_PATTERN.test(value) && Number.isFinite(Date.parse(value))
}

function cleanAcks<K extends string>(value: unknown, isKey: (key: string) => key is K): Partial<Record<K, string>> | undefined {
  if (!isRecord(value)) return undefined
  const out: Partial<Record<K, string>> = {}
  let any = false
  for (const [key, at] of Object.entries(value)) {
    if (isKey(key) && isTimestamp(at)) {
      out[key] = at
      any = true
    }
  }
  return any ? out : undefined
}

function isImageSource(key: string): key is ImageSourceKind {
  return (IMAGE_SOURCES as readonly string[]).includes(key)
}

function isVoiceSource(key: string): key is VoiceSourceId {
  return isVoiceSourceId(key)
}

/** Keeps only well-formed fields of a profile state. */
export function cleanProfileState(raw: unknown): ProfileState {
  if (!isRecord(raw)) return {}
  const state: ProfileState = {}
  if (isValidClientId(raw.lastThreadId)) state.lastThreadId = raw.lastThreadId
  if (typeof raw.lastReplThread === "string" && REPL_THREAD_PATTERN.test(raw.lastReplThread) && isValidClientId(raw.lastReplThread)) {
    state.lastReplThread = raw.lastReplThread
  }
  const voiceAck = cleanAcks(raw.voiceAck, isVoiceSource)
  if (voiceAck !== undefined) state.voiceAck = voiceAck
  const imageAck = cleanAcks(raw.imageAck, isImageSource)
  if (imageAck !== undefined) state.imageAck = imageAck
  if (typeof raw.voiceDevice === "string" && DEVICE_PATTERN.test(raw.voiceDevice)) state.voiceDevice = raw.voiceDevice
  return state
}

/** Keeps only well-formed fields of the global state. */
export function cleanGlobalState(raw: unknown): GlobalState {
  if (!isRecord(raw)) return {}
  return isValidTenantName(raw.lastTenant) ? { lastTenant: raw.lastTenant } : {}
}

// ---------------------------------------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------------------------------------

/** Reads a JSON state file; {} when it is missing or fails any check (state is advisory). */
async function readStateJson(path: string, uid: number): Promise<unknown> {
  try {
    const bytes = await readCheckedFile(path, {
      uid,
      maxBytes: STATE_FILE_MAX_BYTES,
      label: `state file ${displayValue(path)}`,
      // The client writes 0600; a wider mode set by hand does not make the advisory content unusable.
      forbiddenModeBits: 0,
    })
    return JSON.parse(new TextDecoder().decode(bytes))
  } catch {
    return {}
  }
}

function isAlreadyExists(error: unknown): boolean {
  return typeof error === "object" && error !== null && "code" in error && error.code === "EEXIST"
}

async function writeTemp(tmp: string, data: Uint8Array): Promise<void> {
  const flags = constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY | constants.O_NOFOLLOW
  const handle = await open(tmp, flags, 0o600)
  try {
    await handle.write(data)
    await handle.datasync()
  } finally {
    await handle.close()
  }
}

/**
 * Writes `value` as JSON to `path` atomically: `<path>.<pid>.tmp` (O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW, 0600),
 * then rename(). A leftover temp file of a crashed process with the same pid is removed once and retried.
 */
export async function writeJsonAtomic(path: string, value: unknown): Promise<void> {
  const tmp = `${path}.${process.pid}.tmp`
  const data = new TextEncoder().encode(`${JSON.stringify(value)}\n`)
  try {
    await writeTemp(tmp, data)
  } catch (error) {
    if (!isAlreadyExists(error)) throw error
    await unlink(tmp)
    await writeTemp(tmp, data)
  }
  try {
    await rename(tmp, path)
  } catch (error) {
    await unlink(tmp).catch(() => undefined)
    throw error
  }
}

/** Per-path write queue: every StateFile of this process that targets one file is serialized. */
const queues = new Map<string, Promise<unknown>>()

function serialized<T>(path: string, task: () => Promise<T>): Promise<T> {
  const previous = queues.get(path) ?? Promise.resolve()
  const run = previous.then(task, task)
  const settled = run.then(
    () => undefined,
    () => undefined,
  )
  queues.set(path, settled)
  void settled.then(() => {
    if (queues.get(path) === settled) queues.delete(path)
  })
  return run
}

function sameJson(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b)
}

/** Restores every primary-only key of `next` to its value in `current` (or its absence). */
function withoutPrimaryKeys<T extends object>(current: T, next: T): T {
  const out = new Map(Object.entries(next))
  const before = new Map(Object.entries(current))
  for (const key of PRIMARY_ONLY_STATE_KEYS) {
    if (before.has(key)) out.set(key, before.get(key))
    else out.delete(key)
  }
  return Object.fromEntries(out) as T
}

async function updateFile<T extends object>(
  path: string,
  options: { uid: number; primary: boolean; clean: (raw: unknown) => T },
  change: (current: T) => T,
): Promise<T> {
  return serialized(path, async () => {
    const current = options.clean(await readStateJson(path, options.uid))
    let next = options.clean(change(current))
    if (!options.primary) next = options.clean(withoutPrimaryKeys(current, next))
    if (!sameJson(current, next)) await writeJsonAtomic(path, next)
    return next
  })
}

/** Reads state.json without a StateFile (the tenant picker needs lastTenant before the profile is known). */
export async function readGlobalState(stateDir: string, uid: number = currentUid()): Promise<GlobalState> {
  return cleanGlobalState(await readStateJson(globalStatePath(stateDir), uid))
}

export interface StateFileOptions {
  stateDir: string
  profile: string
  /** From acquireProfileLock(); false drops changes to PRIMARY_ONLY_STATE_KEYS. */
  primary: boolean
  uid?: number
}

class JsonStateFile implements StateFile {
  readonly profile: string
  readonly primary: boolean
  readonly #profilePath: string
  readonly #globalPath: string
  readonly #uid: number

  constructor(options: StateFileOptions) {
    this.profile = assertProfileName(options.profile, "state file")
    this.primary = options.primary
    this.#uid = options.uid ?? currentUid()
    this.#profilePath = profileStatePath(options.stateDir, this.profile)
    this.#globalPath = globalStatePath(options.stateDir)
  }

  async read(): Promise<ProfileState> {
    return cleanProfileState(await readStateJson(this.#profilePath, this.#uid))
  }

  update(change: (current: ProfileState) => ProfileState): Promise<ProfileState> {
    return updateFile(this.#profilePath, { uid: this.#uid, primary: this.primary, clean: cleanProfileState }, change)
  }

  async readGlobal(): Promise<GlobalState> {
    return cleanGlobalState(await readStateJson(this.#globalPath, this.#uid))
  }

  updateGlobal(change: (current: GlobalState) => GlobalState): Promise<GlobalState> {
    return updateFile(this.#globalPath, { uid: this.#uid, primary: this.primary, clean: cleanGlobalState }, change)
  }
}

/** The StateFile of config/types.ts for one profile. The state directory must exist (ensureStateDir). */
export function openStateFile(options: StateFileOptions): StateFile {
  return new JsonStateFile(options)
}
