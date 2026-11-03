/**
 * The per-profile instance lock (design 3.11.5, F3): `<state dir>/<profile>.lock`, mode 0600, holding the pid of
 * the primary instance.
 *
 * The lock content is written to `<lock>.<pid>.tmp` (opened O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW, 0600) and
 * published with link(2), which fails with EEXIST when the lock (or any link at its path) exists. So the lock
 * never appears empty to a concurrent starter; on a filesystem without hard links the lock is created directly
 * with O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW. An existing lock whose pid is dead (process.kill(pid, 0) throws
 * ESRCH) is replaced; a live pid (EPERM counts as live) makes this instance secondary: it never writes
 * lastThreadId, lastReplThread or lastTenant, uses a new gateway thread unless --thread was given, and the
 * header shows "second window". A symbolic link (or any non-regular file) at the lock path is refused.
 */
import { constants } from "node:fs"
import { link, lstat, open, unlink } from "node:fs/promises"
import { assertProfileName, ConfigError, displayValue } from "./flags.ts"
import { profileLockPath } from "./paths.ts"
import { currentUid, MissingFileError, OversizeFileError, readCheckedFile } from "./token-file.ts"

export interface ProfileLock {
  /** True when this process holds the lock. */
  readonly primary: boolean
  readonly path: string
  /** Removes the lock if this process still holds it. Idempotent; a no-op for a secondary instance. */
  release(): Promise<void>
}

export interface ProfileLockOptions {
  uid?: number
  /** Liveness probe; default process.kill(pid, 0) with ESRCH meaning dead. */
  isAlive?: (pid: number) => boolean
}

const EXCLUSIVE = constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY | constants.O_NOFOLLOW
const PID_PATTERN = /^\s*([1-9][0-9]{0,9})\s*$/
const ATTEMPTS = 3

function errorCode(error: unknown): string | undefined {
  return typeof error === "object" && error !== null && "code" in error ? String(error.code) : undefined
}

export function processIsAlive(pid: number): boolean {
  try {
    process.kill(pid, 0)
    return true
  } catch (error) {
    return errorCode(error) !== "ESRCH"
  }
}

async function writeExclusive(path: string, content: string): Promise<void> {
  const handle = await open(path, EXCLUSIVE, 0o600)
  try {
    await handle.write(content)
  } finally {
    await handle.close()
  }
}

/** Publishes the lock atomically. False when a lock already exists. */
async function tryCreate(lockPath: string, content: string): Promise<boolean> {
  const tmp = `${lockPath}.${process.pid}.tmp`
  try {
    await writeExclusive(tmp, content)
  } catch (error) {
    if (errorCode(error) !== "EEXIST") throw error
    await unlink(tmp) // left over by a crashed process that had this pid
    await writeExclusive(tmp, content)
  }
  try {
    await link(tmp, lockPath)
    return true
  } catch (error) {
    const code = errorCode(error)
    if (code === "EEXIST") return false
    if (code !== "EPERM" && code !== "ENOTSUP" && code !== "EOPNOTSUPP" && code !== "EXDEV") throw error
  } finally {
    await unlink(tmp).catch(() => undefined)
  }
  return createDirectly(lockPath, content)
}

async function createDirectly(lockPath: string, content: string): Promise<boolean> {
  try {
    await writeExclusive(lockPath, content)
    return true
  } catch (error) {
    if (errorCode(error) === "EEXIST") return false
    throw error
  }
}

interface ExistingLock {
  pid: number | null
  ino: number
}

/** Inspects the lock at the path; null when it vanished meanwhile. Refuses links and foreign files. */
async function inspect(lockPath: string, uid: number): Promise<ExistingLock | null> {
  const label = `instance lock ${displayValue(lockPath)}`
  let ino: number
  try {
    const stats = await lstat(lockPath)
    if (stats.isSymbolicLink()) throw new ConfigError(`${label} is a symbolic link; refusing to use it`)
    if (!stats.isFile()) throw new ConfigError(`${label} is not a regular file; refusing to use it`)
    if (stats.uid !== uid) throw new ConfigError(`${label} is owned by uid ${stats.uid}, not by you (uid ${uid})`)
    ino = stats.ino
  } catch (error) {
    if (errorCode(error) === "ENOENT") return null
    throw error
  }
  try {
    const bytes = await readCheckedFile(lockPath, { uid, maxBytes: 64, label, forbiddenModeBits: 0 })
    const match = PID_PATTERN.exec(new TextDecoder().decode(bytes))
    return { pid: match?.[1] === undefined ? null : Number(match[1]), ino }
  } catch (error) {
    if (error instanceof MissingFileError) return null
    // Not a pid this client wrote: stale, like any other unreadable content.
    if (error instanceof OversizeFileError) return { pid: null, ino }
    throw error
  }
}

/** Removes a stale lock unless it was replaced since it was inspected (another starter got there first). */
async function removeStale(lockPath: string, stale: ExistingLock): Promise<void> {
  try {
    const now = await lstat(lockPath)
    if (now.ino === stale.ino) await unlink(lockPath)
  } catch (error) {
    if (errorCode(error) !== "ENOENT") throw error
  }
}

function secondary(path: string): ProfileLock {
  return { primary: false, path, release: () => Promise.resolve() }
}

function primary(path: string, content: string, uid: number): ProfileLock {
  let released = false
  return {
    primary: true,
    path,
    async release(): Promise<void> {
      if (released) return
      released = true
      const held = await inspect(path, uid).catch(() => null)
      if (held !== null && `${held.pid}\n` === content) await unlink(path).catch(() => undefined)
    },
  }
}

async function acquire(path: string, uid: number, isAlive: (pid: number) => boolean): Promise<ProfileLock> {
  const content = `${process.pid}\n`
  for (let attempt = 0; attempt < ATTEMPTS; attempt += 1) {
    if (await tryCreate(path, content)) return primary(path, content, uid)
    const existing = await inspect(path, uid)
    if (existing === null) continue
    if (existing.pid !== null && isAlive(existing.pid)) return secondary(path)
    await removeStale(path, existing)
  }
  // Lost every race to other starters: behave as a secondary instance, which never writes primary-only state.
  return secondary(path)
}

/** Acquires `<stateDir>/<profile>.lock`. The state directory must exist (ensureStateDir). */
export async function acquireProfileLock(
  stateDir: string,
  profile: string,
  options: ProfileLockOptions = {},
): Promise<ProfileLock> {
  assertProfileName(profile, "instance lock")
  const path = profileLockPath(stateDir, profile)
  try {
    return await acquire(path, options.uid ?? currentUid(), options.isAlive ?? processIsAlive)
  } catch (error) {
    if (error instanceof ConfigError) throw error
    throw new ConfigError(`instance lock ${displayValue(path)} cannot be created (${errorCode(error) ?? "I/O error"})`)
  }
}
