/**
 * Token files and the environment token (design 3.11.3), plus the checked file reader shared with
 * tenant-access.ts and the config.toml loader.
 *
 * A token file is opened O_RDONLY | O_NOFOLLOW (and O_NONBLOCK, so a FIFO planted at the path cannot block the
 * start-up); fstat must show a regular file owned by the current uid with (mode & 0o077) === 0 and at most
 * 4096 bytes; the trimmed content must match ^[\x21-\x7e]{16,512}$. Failures name the file and the failed check,
 * never the content. LUNARWING_GATEWAY_TOKEN is read once and then blanked in the environment (assigned "",
 * which is what Bun honours, 3.11.4). Tokens are never taken from argv (flags.ts).
 */
import { constants, type Stats } from "node:fs"
import { lstat, open, stat, type FileHandle } from "node:fs/promises"
import { basename, dirname, join } from "node:path"
import { ConfigError, displayValue } from "./flags.ts"
import { displayPath } from "./paths.ts"
import { blankEnv, type MutableEnv } from "./proxy-env.ts"
import { Secret } from "./secret.ts"
import type { TokenKind, TokenSource } from "./types.ts"

export const TOKEN_FILE_MAX_BYTES = 4096
/** Printable ASCII without spaces, 16 to 512 characters (3.11.3). */
export const TOKEN_PATTERN = /^[\x21-\x7e]{16,512}$/
export const GATEWAY_TOKEN_ENV = "LUNARWING_GATEWAY_TOKEN"
export const GATEWAY_TOKEN_FILE_ENV = "LUNARWING_GATEWAY_TOKEN_FILE"

export function isValidTokenText(text: string): boolean {
  return TOKEN_PATTERN.test(text)
}

/** The current uid; the client is POSIX-only. */
export function currentUid(): number {
  const uid = process.getuid?.()
  if (uid === undefined) throw new ConfigError("lunarwing-tui needs a POSIX system (no process.getuid)")
  return uid
}

// ---------------------------------------------------------------------------------------------------------
// Checked file reads
// ---------------------------------------------------------------------------------------------------------

export interface FileCheck {
  /** Required owner (process.getuid()). */
  uid: number
  maxBytes: number
  /** How the file is named in messages, e.g. "token file ~/.config/lunarwing-tui/tokens/devgw.token". */
  label: string
  /** 0o077 for secrets (no group/other access at all), 0o022 for config.toml (not writable by others). */
  forbiddenModeBits: number
  /** Default false: a symbolic link is refused (O_NOFOLLOW). config.toml may be a dotfile-manager link. */
  followSymlinks?: boolean
}

/** The file does not exist; callers with an optional file (default config.toml, default token file) catch it. */
export class MissingFileError extends ConfigError {}

/** The file is larger than the check allows. */
export class OversizeFileError extends ConfigError {}

function octal(mode: number): string {
  return `0${(mode & 0o777).toString(8).padStart(3, "0")}`
}

function modeAdvice(forbidden: number): string {
  return forbidden === 0o077 ? "group and others must have no access (chmod 600)" : "it must not be writable by group or others (chmod go-w)"
}

function errorCode(error: unknown): string | undefined {
  return typeof error === "object" && error !== null && "code" in error ? String(error.code) : undefined
}

function fsFailure(error: unknown, label: string): ConfigError {
  const code = errorCode(error)
  if (code === "ENOENT" || code === "ENOTDIR") return new MissingFileError(`${label} does not exist`)
  if (code === "ELOOP") return new ConfigError(`${label} is a symbolic link; refusing to follow it`)
  if (code === "EACCES" || code === "EPERM") return new ConfigError(`${label} is not readable (permission denied)`)
  return new ConfigError(`${label} cannot be read (${code ?? "I/O error"})`)
}

function checkStats(stats: Stats, check: FileCheck): void {
  if (stats.isSymbolicLink()) throw new ConfigError(`${check.label} is a symbolic link; refusing to follow it`)
  if (!stats.isFile()) throw new ConfigError(`${check.label} is not a regular file`)
  if (stats.uid !== check.uid) {
    throw new ConfigError(`${check.label} is owned by uid ${stats.uid}, not by you (uid ${check.uid})`)
  }
  if ((stats.mode & check.forbiddenModeBits) !== 0) {
    throw new ConfigError(`${check.label} has mode ${octal(stats.mode)}: ${modeAdvice(check.forbiddenModeBits)}`)
  }
  if (stats.size > check.maxBytes) throw new OversizeFileError(`${check.label} is larger than ${check.maxBytes} bytes`)
}

/** Reads at most maxBytes + 1 bytes, so a file whose size lied (procfs, a growing file) is still bounded. */
async function readBounded(handle: FileHandle, check: FileCheck): Promise<Uint8Array> {
  const buffer = new Uint8Array(check.maxBytes + 1)
  let total = 0
  while (total < buffer.length) {
    const { bytesRead } = await handle.read(buffer, total, buffer.length - total, total)
    if (bytesRead === 0) break
    total += bytesRead
  }
  if (total > check.maxBytes) {
    buffer.fill(0)
    throw new OversizeFileError(`${check.label} is larger than ${check.maxBytes} bytes`)
  }
  const content = buffer.slice(0, total)
  buffer.fill(0)
  return content
}

/**
 * lstat (or stat with followSymlinks) and check before opening, open without following links and without
 * blocking, then fstat the descriptor and check again (same inode, owner, mode and size), then a bounded read.
 * Throws ConfigError (MissingFileError when absent) naming the file and the failed check, never the content.
 */
export async function readCheckedFile(path: string, check: FileCheck): Promise<Uint8Array> {
  const follow = check.followSymlinks === true
  let before: Stats
  try {
    before = follow ? await stat(path) : await lstat(path)
  } catch (error) {
    throw fsFailure(error, check.label)
  }
  checkStats(before, check)
  const flags = constants.O_RDONLY | constants.O_NONBLOCK | (follow ? 0 : constants.O_NOFOLLOW)
  let handle: FileHandle
  try {
    handle = await open(path, flags)
  } catch (error) {
    throw fsFailure(error, check.label)
  }
  try {
    const opened = await handle.stat()
    if (opened.ino !== before.ino || opened.dev !== before.dev) {
      throw new ConfigError(`${check.label} was replaced while it was being opened; try again`)
    }
    checkStats(opened, check)
    return await readBounded(handle, check)
  } finally {
    await handle.close()
  }
}

// ---------------------------------------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------------------------------------

export interface TokenFileOptions {
  uid?: number
  /** For "~" in messages. */
  home: string
}

const TOKEN_CONTENT_ADVICE = "it must hold one token: 16 to 512 printable ASCII characters without spaces"

// A long name of token characters without a file extension is more likely a token pasted where a path belongs.
const TOKEN_LIKE_NAME = /^[A-Za-z0-9._~+=-]{24,}$/
const FILE_EXTENSION = /\.[a-z]{1,5}$/

function looksLikeToken(name: string): boolean {
  return TOKEN_LIKE_NAME.test(name) && !FILE_EXTENSION.test(name)
}

/** Reads and validates a token file. The error never contains the content (nor a name that looks like a token). */
export async function readTokenFile(path: string, options: TokenFileOptions): Promise<Secret> {
  const tokenLike = looksLikeToken(basename(path))
  const shown = tokenLike ? join(displayPath(dirname(path), options.home), "<name withheld>") : displayPath(path, options.home)
  const label = `token file ${displayValue(shown)}`
  let bytes: Uint8Array
  try {
    bytes = await readCheckedFile(path, { uid: options.uid ?? currentUid(), maxBytes: TOKEN_FILE_MAX_BYTES, label, forbiddenModeBits: 0o077 })
  } catch (error) {
    if (error instanceof MissingFileError && tokenLike) {
      throw new MissingFileError(`${label} does not exist (the name looks like a token: pass the path of a 0600 file, never the token itself)`)
    }
    throw error
  }
  try {
    const text = new TextDecoder().decode(bytes).trim()
    if (!isValidTokenText(text)) throw new ConfigError(`${label} is not a valid token file: ${TOKEN_CONTENT_ADVICE}`)
    return new Secret(text)
  } finally {
    bytes.fill(0)
  }
}

/** Warns when the token's directory is reachable by group or others ("token directories should be 0700"). */
export async function tokenDirectoryWarning(path: string, home: string): Promise<string | null> {
  try {
    const dir = dirname(path)
    const stats = await stat(dir)
    if ((stats.mode & 0o077) === 0) return null
    return `token directory ${displayValue(displayPath(dir, home))} has mode ${octal(stats.mode)}; chmod 700 is recommended`
  } catch {
    return null
  }
}

/** A token file source: reload() re-reads the file once (401 rotation, 3.8). */
class FileTokenSource implements TokenSource {
  #secret: Secret
  #reloading: Promise<boolean> | null = null

  constructor(
    readonly path: string,
    secret: Secret,
    private readonly options: TokenFileOptions,
  ) {
    this.#secret = secret
  }

  current(): Secret {
    return this.#secret
  }

  reload(): Promise<boolean> {
    this.#reloading ??= this.#reread().finally(() => {
      this.#reloading = null
    })
    return this.#reloading
  }

  async #reread(): Promise<boolean> {
    let next: Secret
    try {
      next = await readTokenFile(this.path, this.options)
    } catch {
      return false
    }
    if (next.equals(this.#secret)) {
      next.dispose()
      return false
    }
    this.#secret = next
    return true
  }

  describe(): string {
    return `token file ${displayValue(displayPath(this.path, this.options.home))}`
  }

  kind(): TokenKind {
    return "file"
  }
}

/** Opens a token file source; throws ConfigError when the file fails a check. */
export async function openTokenFileSource(path: string, options: TokenFileOptions): Promise<TokenSource> {
  return new FileTokenSource(path, await readTokenFile(path, options), options)
}

/** A token with no file behind it (LUNARWING_GATEWAY_TOKEN): reload() has nothing to re-read. */
class StaticTokenSource implements TokenSource {
  readonly #secret: Secret

  constructor(
    secret: Secret,
    private readonly origin: string,
  ) {
    this.#secret = secret
  }

  current(): Secret {
    return this.#secret
  }

  reload(): Promise<boolean> {
    return Promise.resolve(false)
  }

  describe(): string {
    return this.origin
  }

  kind(): TokenKind {
    return "file"
  }
}

/**
 * Takes LUNARWING_GATEWAY_TOKEN out of `env`: returns it (untrimmed, unvalidated) and blanks the variable,
 * whether or not the token is used later. Null when unset or empty.
 */
export function takeEnvToken(env: MutableEnv = process.env, name: string = GATEWAY_TOKEN_ENV): Secret | null {
  const value = env[name]
  blankEnv(env, [name])
  return value === undefined || value === "" ? null : new Secret(value)
}

/** Validates a token taken from the environment and wraps it as a TokenSource. */
export function envTokenSource(secret: Secret, name: string = GATEWAY_TOKEN_ENV): TokenSource {
  const text = secret.expose().trim()
  if (!isValidTokenText(text)) throw new ConfigError(`${name} is not a valid token: ${TOKEN_CONTENT_ADVICE}`)
  return new StaticTokenSource(text === secret.expose() ? secret : new Secret(text), name)
}
