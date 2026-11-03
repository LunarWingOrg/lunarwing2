/**
 * Hex discovery (design 5.3.2, M1b): the running app's endpoint and the `hex` binary. Hex is optional and probed;
 * nothing here records, downloads or prepares anything.
 *
 * Running app (mirrors hex:src/local_api/discovery.rs:105-123, :204-226, :260-266, plus the listener-owner check):
 * 1. `${HEX_APPLICATION_SUPPORT_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/hex}/local-api.json`
 *    (hex:src/app_paths.rs:31-43, :89-91).
 * 2. Opened without following links: a regular file owned by this uid, (mode & 0o077) === 0, at most 4 KiB.
 * 3. JSON {port, token, apiVersion, pid}: apiVersion "2", token /^hex_[0-9a-f]{64}$/, 1 <= port <= 65535, pid > 0.
 * 4. process.kill(pid, 0): ESRCH means a stale file; EPERM means another user's process, refused.
 * 5. GET http://127.0.0.1:<port>/health (300 ms) through HexClient, which first checks that this uid owns the
 *    listening socket (3.8.1, SEC-01) and only then sends the bearer token.
 * The result (endpoint or reason) is cached for 5 s. A 5 s lstat poll of the discovery file (mtime, inode, size)
 * invalidates the cache and hints the sources to re-probe (5.4, manager.ts AvailabilityHintSource).
 *
 * Binary: voice.hex.command (argv; default ["hex"] resolved with Bun.which, or $LUNARWING_TUI_HEX_BIN through
 * config.ts), then `<command> --version` (2 s) must print `hex <major>.<minor>.<patch>`; 3.3.0 or later is needed
 * for `hex service --embedded`. A working binary stays cached; a failure is re-checked after 60 s.
 */
import { lstat } from "node:fs/promises"
import { homedir } from "node:os"
import { isAbsolute, join } from "node:path"
import type { Clock, TimerHandle } from "@opentui/core"
import { buildChildEnv } from "../../config/child-env.ts"
import { isConfigError } from "../../config/flags.ts"
import { envValue, xdgDir, type EnvSource } from "../../config/paths.ts"
import { Secret } from "../../config/secret.ts"
import { MissingFileError, readCheckedFile } from "../../config/token-file.ts"
import { bunSpawn, type ChildEnv, type SpawnFn, type VoiceConfig } from "../../config/types.ts"
import { isMissingExecutable, runCapped } from "../../images/clipboard.ts"
import type { FetchLike } from "../../transport/gateway.ts"
import type { ProcReader } from "../../transport/listener-owner.ts"
import { isVoiceError } from "../errors.ts"
import { HEX_API_VERSION, HexClient, cleanHexVersion, hexBaseUrl, raceSignal, type HexHttpDeps } from "./hex-http.ts"

export const HEX_DISCOVERY_FILE = "local-api.json"
export const HEX_DISCOVERY_MAX_BYTES = 4096
/** hex:src/local_api/discovery.rs:208-214 (lowercase hex only). */
export const HEX_TOKEN_PATTERN = /^hex_[0-9a-f]{64}$/
/** `hex service --embedded` and Linux /dictations shipped in 3.3.0 (hex report). */
export const HEX_MIN_EMBEDDED_VERSION = Object.freeze({ major: 3, minor: 3, patch: 0 })
export const HEX_LOCATE_TIMING = Object.freeze({
  /** locateApp() results, endpoint or reason (5.3.2). */
  cacheMs: 5_000,
  /** lstat of the discovery file (5.4). */
  pollMs: 5_000,
  versionTimeoutMs: 2_000,
  /** A missing or unusable binary is looked up again after this long. */
  binaryRetryMs: 60_000,
})

/** Linux pids fit in 22 bits; anything beyond int32 cannot name a process. */
const MAX_PID = 0x7fff_ffff
const VERSION_LINE = /^hex (\d{1,4})\.(\d{1,4})\.(\d{1,4})([-+][0-9A-Za-z.+-]{1,40})?$/

export interface HexEndpoint {
  /** Always "http://127.0.0.1:<port>". */
  baseUrl: string
  token: Secret
  pid: number
  origin: "app" | "embedded"
  /** From GET /health (app) or the binary check (embedded), for the voice panel. */
  version?: string
}

export interface HexUnavailable {
  /** Short, user facing; never a token. */
  unavailable: string
}

export interface HexBinary {
  /** The resolved argv prefix: an absolute executable path, then the extra words of voice.hex.command. */
  command: string[]
  version: string
}

export interface HexLocator {
  /** The running app, cached 5 s. */
  locateApp(signal: AbortSignal): Promise<HexEndpoint | HexUnavailable>
  /** The `hex` binary and its version, cached. */
  binary(): Promise<HexBinary | HexUnavailable>
  /** Forgets the app location (a 401, a changed discovery file). */
  invalidate(): void
  /** Forgets the binary (it vanished between the check and a spawn). */
  forgetBinary(): void
  readonly discoveryPath: string
  /** Called after the discovery file changed (polled every 5 s while someone listens). */
  onDiscoveryChange(listener: () => void): () => void
  /** One poll now; true when the file changed. The 5 s timer calls this too. */
  checkDiscovery(): Promise<boolean>
}

/** Bun.which. */
export type WhichFn = (command: string, options?: { PATH?: string; cwd?: string }) => string | null
/** process.kill(pid, 0). */
export type PidSignaller = (pid: number, signal: number) => unknown

/** Collaborators of the Hex sources; production defaults when omitted (tests inject fakes). */
export interface HexSourceOptions {
  /** One locator for hex-app and hex-embedded; default: created from these options once per deps object. */
  locator?: HexLocator
  /** Discovery paths and the proxy guard; default process.env. */
  env?: EnvSource
  /** For `hex --version` and the embedded child; default buildChildEnv() at spawn time (TRANSCRIBE_DUMP_DIR is always removed). */
  childEnv?: ChildEnv
  spawn?: SpawnFn
  which?: WhichFn
  fetch?: FetchLike
  readProc?: ProcReader
  /** Default process.getuid(). */
  uid?: number
  kill?: PidSignaller
}

/** What a Hex source factory needs; VoiceSourceDeps (sources/index.ts) satisfies it. */
export interface HexSourceDeps {
  clock: Clock
  config: VoiceConfig
  hex?: HexSourceOptions
}

export interface ResolvedHexOptions {
  clock: Clock
  /** null on a system without getuid: every Hex source is unavailable. */
  uid: number | null
  env: EnvSource
  childEnv: () => ChildEnv
  spawn: SpawnFn
  which: WhichFn
  fetch: FetchLike | undefined
  readProc: ProcReader | undefined
  kill: PidSignaller
}

/** The child environment without TRANSCRIBE_DUMP_DIR (Hex can write audio-derived tensors there), whatever was passed. */
export function scrubHexChildEnv(base: ChildEnv): ChildEnv {
  if (!Object.hasOwn(base, "TRANSCRIBE_DUMP_DIR")) return base
  const env: Record<string, string> = { ...base }
  delete env.TRANSCRIBE_DUMP_DIR
  return Object.freeze(env)
}

export function resolveHexOptions(clock: Clock, options: HexSourceOptions = {}): ResolvedHexOptions {
  const given = options.childEnv
  return {
    clock,
    uid: options.uid ?? process.getuid?.() ?? null,
    env: options.env ?? process.env,
    childEnv: () => scrubHexChildEnv(given ?? buildChildEnv()),
    spawn: options.spawn ?? bunSpawn,
    which: options.which ?? ((command, opts) => Bun.which(command, opts)),
    fetch: options.fetch,
    readProc: options.readProc,
    kill: options.kill ?? ((pid, signal) => process.kill(pid, signal)),
  }
}

/** HexClient dependencies; null without a uid (nothing may be sent then). */
export function hexHttpDeps(resolved: ResolvedHexOptions): HexHttpDeps | null {
  if (resolved.uid === null) return null
  return {
    clock: resolved.clock,
    uid: resolved.uid,
    env: resolved.env,
    ...(resolved.fetch ? { fetch: resolved.fetch } : {}),
    ...(resolved.readProc ? { readProc: resolved.readProc } : {}),
  }
}

// ---------------------------------------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------------------------------------

/** Hex's data directory: HEX_APPLICATION_SUPPORT_DIR, else $XDG_DATA_HOME/hex (absolute only), else ~/.local/share/hex. */
export function hexSupportDir(env: EnvSource = process.env): string {
  const explicit = envValue(env, "HEX_APPLICATION_SUPPORT_DIR")
  if (explicit !== undefined) return explicit
  const data = xdgDir(env, "XDG_DATA_HOME") ?? join(envValue(env, "HOME") ?? homedir(), ".local", "share")
  return join(data, "hex")
}

export function hexDiscoveryPath(env: EnvSource = process.env): string {
  return join(hexSupportDir(env), HEX_DISCOVERY_FILE)
}

// ---------------------------------------------------------------------------------------------------------
// The discovery document
// ---------------------------------------------------------------------------------------------------------

export interface HexDiscovery {
  port: number
  pid: number
  token: Secret
}

function shownVersion(value: unknown): string {
  return cleanHexVersion(value) ?? (value === undefined ? "missing" : "unknown")
}

/** Validates {port, token, apiVersion, pid} (hex:src/local_api/discovery.rs:14-21, :204-214). */
export function parseHexDiscovery(value: unknown): HexDiscovery | HexUnavailable {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return { unavailable: `${HEX_DISCOVERY_FILE} is not a JSON object` }
  const input = value as Record<string, unknown>
  if (input.apiVersion !== HEX_API_VERSION) {
    return { unavailable: `Hex local API version ${shownVersion(input.apiVersion)} is not supported (this client speaks 2)` }
  }
  if (typeof input.token !== "string" || !HEX_TOKEN_PATTERN.test(input.token)) {
    return { unavailable: `${HEX_DISCOVERY_FILE} holds no valid Hex token` }
  }
  const { port, pid } = input
  if (typeof port !== "number" || !Number.isInteger(port) || port < 1 || port > 65_535) {
    return { unavailable: `${HEX_DISCOVERY_FILE} has an invalid port` }
  }
  if (typeof pid !== "number" || !Number.isSafeInteger(pid) || pid < 1 || pid > MAX_PID) {
    return { unavailable: `${HEX_DISCOVERY_FILE} has an invalid pid` }
  }
  return { port, pid, token: new Secret(input.token) }
}

/** Reads and validates the discovery file; the raw bytes are zero-filled after parsing. */
export async function readHexDiscovery(path: string, uid: number): Promise<HexDiscovery | HexUnavailable> {
  let bytes: Uint8Array
  try {
    bytes = await readCheckedFile(path, { uid, maxBytes: HEX_DISCOVERY_MAX_BYTES, label: HEX_DISCOVERY_FILE, forbiddenModeBits: 0o077 })
  } catch (error) {
    if (error instanceof MissingFileError) return { unavailable: "not running" }
    return { unavailable: isConfigError(error) ? error.message : `${HEX_DISCOVERY_FILE} cannot be read` }
  }
  try {
    return parseHexDiscovery(JSON.parse(new TextDecoder().decode(bytes)) as unknown)
  } catch {
    return { unavailable: `${HEX_DISCOVERY_FILE} is not valid JSON` }
  } finally {
    bytes.fill(0)
  }
}

export type PidState = "alive" | "dead" | "foreign"

/** kill(pid, 0): ESRCH is a dead pid; EPERM (or anything unexpected) a process this user does not own. */
export function pidState(pid: number, kill: PidSignaller = (p, s) => process.kill(p, s)): PidState {
  try {
    kill(pid, 0)
    return "alive"
  } catch (error) {
    const code = typeof error === "object" && error !== null ? (error as { code?: unknown }).code : undefined
    return code === "ESRCH" ? "dead" : "foreign"
  }
}

// ---------------------------------------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------------------------------------

export interface HexVersion {
  major: number
  minor: number
  patch: number
  /** "3.3.0", "3.4.0-beta.1" */
  text: string
}

/** The first line of `hex --version` (clap: "hex 3.3.0"). */
export function parseHexVersion(output: string): HexVersion | null {
  const line = output.split("\n", 1)[0]?.trim() ?? ""
  const match = VERSION_LINE.exec(line)
  if (!match) return null
  const [major, minor, patch] = [Number(match[1]), Number(match[2]), Number(match[3])]
  return { major, minor, patch, text: `${major}.${minor}.${patch}${match[4] ?? ""}` }
}

export function compareHexVersions(a: Omit<HexVersion, "text">, b: Omit<HexVersion, "text">): number {
  return a.major - b.major || a.minor - b.minor || a.patch - b.patch
}

export function supportsEmbedded(version: HexVersion): boolean {
  return compareHexVersions(version, HEX_MIN_EMBEDDED_VERSION) >= 0
}

const MIN_EMBEDDED_TEXT = `${HEX_MIN_EMBEDDED_VERSION.major}.${HEX_MIN_EMBEDDED_VERSION.minor}.${HEX_MIN_EMBEDDED_VERSION.patch}`

// ---------------------------------------------------------------------------------------------------------
// The locator
// ---------------------------------------------------------------------------------------------------------

export interface HexLocatorOptions extends HexSourceOptions {
  clock: Clock
  /** voice.hex.command. */
  command: readonly string[]
}

interface Cached<T> {
  at: number
  task: Promise<T>
  /** null while pending. */
  ok: boolean | null
}

function healthReason(error: unknown): string {
  if (!isVoiceError(error)) return "Hex did not answer /health"
  return error.opts.status === 401 ? `Hex rejected the token in ${HEX_DISCOVERY_FILE}` : error.message
}

class DefaultHexLocator implements HexLocator {
  readonly discoveryPath: string
  private readonly opts: ResolvedHexOptions
  private readonly command: readonly string[]
  private app: Cached<HexEndpoint | HexUnavailable> | null = null
  private bin: Cached<HexBinary | HexUnavailable> | null = null
  private readonly listeners = new Set<{ listener: () => void }>()
  private pollTimer: TimerHandle | null = null
  private baseline: Promise<string> | null = null
  private checking: Promise<boolean> | null = null

  constructor(options: HexLocatorOptions) {
    this.opts = resolveHexOptions(options.clock, options)
    this.command = Object.freeze([...options.command])
    this.discoveryPath = hexDiscoveryPath(this.opts.env)
  }

  locateApp(signal: AbortSignal): Promise<HexEndpoint | HexUnavailable> {
    const now = this.opts.clock.now()
    if (this.app === null || now - this.app.at >= HEX_LOCATE_TIMING.cacheMs) {
      const task = this.locate().catch((): HexUnavailable => ({ unavailable: "Hex discovery failed" }))
      this.app = { at: now, task, ok: null }
    }
    return raceSignal(this.app.task, signal)
  }

  binary(): Promise<HexBinary | HexUnavailable> {
    const now = this.opts.clock.now()
    const cached = this.bin
    if (cached !== null && (cached.ok !== false || now - cached.at < HEX_LOCATE_TIMING.binaryRetryMs)) return cached.task
    const entry: Cached<HexBinary | HexUnavailable> = { at: now, ok: null, task: Promise.resolve({ unavailable: "" }) }
    entry.task = this.resolveBinary()
      .catch((): HexUnavailable => ({ unavailable: "the hex binary cannot be checked" }))
      .then((result) => {
        entry.ok = !("unavailable" in result)
        return result
      })
    this.bin = entry
    return entry.task
  }

  invalidate(): void {
    this.app = null
  }

  forgetBinary(): void {
    this.bin = null
  }

  onDiscoveryChange(listener: () => void): () => void {
    const entry = { listener }
    this.listeners.add(entry)
    if (this.pollTimer === null) {
      this.baseline = this.signature()
      this.pollTimer = this.opts.clock.setInterval(() => void this.checkDiscovery(), HEX_LOCATE_TIMING.pollMs)
    }
    return () => {
      this.listeners.delete(entry)
      if (this.listeners.size > 0 || this.pollTimer === null) return
      this.opts.clock.clearInterval(this.pollTimer)
      this.pollTimer = null
      this.baseline = null
    }
  }

  checkDiscovery(): Promise<boolean> {
    this.checking ??= this.poll().finally(() => {
      this.checking = null
    })
    return this.checking
  }

  private async poll(): Promise<boolean> {
    const before = await (this.baseline ?? this.signature())
    const now = await this.signature()
    this.baseline = Promise.resolve(now)
    if (now === before) return false
    this.invalidate()
    for (const { listener } of [...this.listeners]) {
      try {
        listener()
      } catch {
        // a listener failure must not stop the others
      }
    }
    return true
  }

  /** Identity of the discovery file: Hex replaces it by rename (new inode) and removes it on exit. */
  private async signature(): Promise<string> {
    try {
      const stats = await lstat(this.discoveryPath)
      return `${stats.dev}:${stats.ino}:${stats.size}:${stats.mtimeMs}:${stats.mode}`
    } catch (error) {
      const code = typeof error === "object" && error !== null ? (error as { code?: unknown }).code : undefined
      return `absent:${typeof code === "string" ? code : "error"}`
    }
  }

  private async locate(): Promise<HexEndpoint | HexUnavailable> {
    const http = hexHttpDeps(this.opts)
    if (http === null || this.opts.uid === null) return { unavailable: "needs a POSIX system (no getuid)" }
    const found = await readHexDiscovery(this.discoveryPath, this.opts.uid)
    if ("unavailable" in found) return found
    const state = pidState(found.pid, this.opts.kill)
    if (state !== "alive") {
      found.token.dispose()
      if (state === "dead") return { unavailable: `not running (stale ${HEX_DISCOVERY_FILE})` }
      return { unavailable: `${HEX_DISCOVERY_FILE} names pid ${found.pid}, which is not this user's process; refused` }
    }
    const endpoint: HexEndpoint = { baseUrl: hexBaseUrl(found.port), token: found.token, pid: found.pid, origin: "app" }
    try {
      const health = await new HexClient(endpoint, http).health()
      return { ...endpoint, version: health.version }
    } catch (error) {
      found.token.dispose()
      return { unavailable: healthReason(error) }
    }
  }

  private async resolveBinary(): Promise<HexBinary | HexUnavailable> {
    const [first, ...rest] = this.command
    if (first === undefined || first === "") return { unavailable: "voice.hex.command is empty" }
    const env = this.opts.childEnv()
    let path: string | null
    if (first.includes("/")) {
      if (!isAbsolute(first)) return { unavailable: "voice.hex.command must name a command or an absolute path" }
      path = this.opts.which(first)
    } else {
      path = this.opts.which(first, { PATH: env.PATH ?? envValue(this.opts.env, "PATH") ?? "" })
    }
    if (path === null) return { unavailable: "not installed" }
    return this.checkVersion([path, ...rest], env)
  }

  private async checkVersion(command: string[], env: ChildEnv): Promise<HexBinary | HexUnavailable> {
    let run: Awaited<ReturnType<typeof runCapped>>
    try {
      run = await runCapped(this.opts.spawn, [...command, "--version"], {
        env,
        clock: this.opts.clock,
        timeoutMs: HEX_LOCATE_TIMING.versionTimeoutMs,
        maxStdoutBytes: 4096,
        maxStderrBytes: 1024,
      })
    } catch (error) {
      return { unavailable: isMissingExecutable(error) ? "not installed" : "the hex binary cannot be started" }
    }
    if (run.timedOut) return { unavailable: "`hex --version` did not answer within 2 s" }
    if (run.status !== 0) return { unavailable: `\`hex --version\` failed (exit ${run.status})` }
    const version = parseHexVersion(new TextDecoder().decode(run.stdout))
    if (version === null) return { unavailable: "`hex --version` printed no version" }
    if (!supportsEmbedded(version)) return { unavailable: `Hex ${MIN_EMBEDDED_TEXT} or later required (found ${version.text})` }
    return { command, version: version.text }
  }
}

export function createHexLocator(options: HexLocatorOptions): HexLocator {
  return new DefaultHexLocator(options)
}

const sharedLocators = new WeakMap<object, HexLocator>()

/**
 * The locator for one createVoiceSources() call: deps.hex.locator when given, else one created per deps object,
 * so hex-app and hex-embedded share the 5 s cache and the discovery poll.
 */
export function hexLocatorFor(deps: HexSourceDeps): HexLocator {
  const given = deps.hex?.locator
  if (given !== undefined) return given
  let locator = sharedLocators.get(deps)
  if (locator === undefined) {
    locator = createHexLocator({ ...deps.hex, clock: deps.clock, command: deps.config.hex.command })
    sharedLocators.set(deps, locator)
  }
  return locator
}
