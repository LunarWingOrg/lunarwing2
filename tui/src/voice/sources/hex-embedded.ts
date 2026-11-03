/**
 * The `hex-embedded` source (design 5.3.3, M1b): when no Hex app runs, the TUI starts the hidden Linux
 * subcommand `hex service --embedded` (hex:src/linux.rs:64-71, :123-125) as its own child and POSTs the WAV it
 * recorded to that child's /transcriptions. Hex never records here and nothing is ever downloaded.
 *
 * Child lifecycle (hex:src/local_api.rs:273-343):
 * - Spawned lazily at the first transcription, only through the injected SpawnFn, as argv
 *   [...voice.hex.command, "service", "--embedded"] (never a shell), with the allowlisted child environment and
 *   TRANSCRIBE_DUMP_DIR removed (it can write audio-derived tensors).
 * - The first stdout line, within 15 s and at most 16 KiB, must be
 *   {"type":"ready","url":"http://127.0.0.1:<port>","token":"hex_<64 hex>","apiVersion":"2","pid":<child pid>}
 *   and this uid must own the listening socket (3.8.1); anything else kills the child.
 * - stdin is the lifetime lease: kept open and never written. Closing it makes Hex exit within about 5 s, so a TUI
 *   that dies, even by SIGKILL, never leaves the child behind. stdout and stderr are drained into 8 KiB rings that
 *   are never shown.
 * - Kept for a 10 min idle TTL after the last transcription; disposed on quit: close stdin, SIGTERM after 6 s,
 *   SIGKILL after 10 s.
 * - Embedded mode writes no discovery file and takes no global lock; only /models and /transcriptions are used.
 *
 * Availability: unavailable while the Hex app runs (hex-app transcribes then, and a second model copy would only
 * cost memory), when the binary is missing or older than 3.3.0, or when the running child has no ready model.
 */
import type { Clock, TimerHandle } from "@opentui/core"
import { Secret } from "../../config/secret.ts"
import type { ChildEnv, ChildProcess, SpawnFn, VoiceConfig } from "../../config/types.ts"
import { isMissingExecutable } from "../../images/clipboard.ts"
import { isTransportError } from "../../transport/errors.ts"
import { isFrameTooLargeError, LineFramer } from "../../transport/framing.ts"
import { assertListenerOwner, readProcNet, type ProcReader } from "../../transport/listener-owner.ts"
import { cancelledError, isVoiceError, toVoiceError, VoiceError } from "../errors.ts"
import type { SourceAvailability, Transcript, VoiceSource, WavClip } from "../types.ts"
import { HexModelCache } from "./hex-app.ts"
import {
  HEX_API_VERSION,
  HEX_LANGUAGE,
  HEX_MAX_WAV_BYTES,
  HexClient,
  hexBaseUrl,
  hexPortOf,
  isHexConnectionFailure,
  pickHexModel,
  raceSignal,
  type HexHttpDeps,
} from "./hex-http.ts"
import {
  HEX_TOKEN_PATTERN,
  hexHttpDeps,
  hexLocatorFor,
  resolveHexOptions,
  type HexEndpoint,
  type HexLocator,
  type HexSourceDeps,
  type HexUnavailable,
  type ResolvedHexOptions,
} from "./hex-locate.ts"

export const HEX_EMBEDDED_ID = "hex-embedded" as const
export const HEX_EMBEDDED_LABEL = "Hex (embedded service)"
export const HEX_EMBEDDED_PRIVACY_NOTE =
  "audio goes to a private `hex service --embedded` child on this machine (127.0.0.1 only); it keeps no audio, history or transcript (Hex writes its process.log)"
export const HEX_APP_RUNNING_REASON = "the Hex app is running (hex-app transcribes)"

export const HEX_EMBEDDED_TIMING = Object.freeze({
  readyTimeoutMs: 15_000,
  idleTtlMs: 10 * 60_000,
  /** After stdin closes (the lease). */
  termAfterMs: 6_000,
  killAfterMs: 10_000,
  /** dispose() resolves by then even if the child never reports its exit. */
  giveUpAfterMs: 12_000,
})
export const HEX_READY_LINE_MAX_BYTES = 16 * 1024
export const HEX_OUTPUT_RING_BYTES = 8 * 1024

const LF = 0x0a
const MAX_PID = 0x7fff_ffff

// ---------------------------------------------------------------------------------------------------------
// The ready line
// ---------------------------------------------------------------------------------------------------------

export interface HexReadyLine {
  port: number
  pid: number
  token: Secret
}

/** Validates the embedded ready line (hex:src/local_api.rs:61-70, :273-281). */
export function parseHexReadyLine(line: string): HexReadyLine | HexUnavailable {
  let value: unknown
  try {
    value = JSON.parse(line) as unknown
  } catch {
    return { unavailable: "not JSON" }
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) return { unavailable: "not a JSON object" }
  const input = value as Record<string, unknown>
  if (input.type !== "ready") return { unavailable: "not a ready line" }
  if (input.apiVersion !== HEX_API_VERSION) return { unavailable: "local API version is not 2" }
  const port = typeof input.url === "string" ? hexPortOf(input.url) : null
  if (port === null) return { unavailable: "URL is not http://127.0.0.1:<port>" }
  if (typeof input.token !== "string" || !HEX_TOKEN_PATTERN.test(input.token)) return { unavailable: "invalid token" }
  const pid = input.pid
  if (typeof pid !== "number" || !Number.isSafeInteger(pid) || pid < 1 || pid > MAX_PID) return { unavailable: "invalid pid" }
  return { port, pid, token: new Secret(input.token) }
}

// ---------------------------------------------------------------------------------------------------------
// Output rings
// ---------------------------------------------------------------------------------------------------------

/** Keeps the last `capacity` bytes of a pipe (diagnostics only; never rendered). */
export class ByteRing {
  private readonly buffer: Uint8Array
  private used = 0
  private total = 0

  constructor(readonly capacity: number) {
    this.buffer = new Uint8Array(capacity)
  }

  push(chunk: Uint8Array): void {
    this.total += chunk.byteLength
    if (chunk.byteLength >= this.capacity) {
      this.buffer.set(chunk.subarray(chunk.byteLength - this.capacity))
      this.used = this.capacity
      return
    }
    const overflow = this.used + chunk.byteLength - this.capacity
    if (overflow > 0) {
      this.buffer.copyWithin(0, overflow, this.used)
      this.used -= overflow
    }
    this.buffer.set(chunk, this.used)
    this.used += chunk.byteLength
  }

  /** Bytes kept (at most capacity). */
  get length(): number {
    return this.used
  }

  /** Every byte pushed. */
  get seen(): number {
    return this.total
  }

  bytes(): Uint8Array {
    return this.buffer.slice(0, this.used)
  }

  clear(): void {
    this.buffer.fill(0)
    this.used = 0
  }
}

async function drainInto(reader: ReadableStreamDefaultReader<Uint8Array>, ring: ByteRing): Promise<void> {
  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done) return
      ring.push(value)
    }
  } catch {
    // the pipe closed with the child
  }
}

// ---------------------------------------------------------------------------------------------------------
// The child
// ---------------------------------------------------------------------------------------------------------

export interface HexEmbeddedChildOptions {
  clock: Clock
  spawn: SpawnFn
  env: ChildEnv
  /** Expected owner of the child's listening socket. */
  uid: number
  readProc?: ProcReader
}

/** One `hex service --embedded` child: ready line, lease, drained output and the close sequence. */
export class HexEmbeddedChild {
  readonly pid: number
  /** Resolves with the endpoint once the ready line is valid; rejects with VoiceError("unavailable"). */
  readonly ready: Promise<HexEndpoint>
  readonly stdout = new ByteRing(HEX_OUTPUT_RING_BYTES)
  readonly stderr = new ByteRing(HEX_OUTPUT_RING_BYTES)
  private readonly exit: Promise<number>
  private exitStatus: number | null = null
  private endpointValue: HexEndpoint | null = null
  private refused = false
  private closing: Promise<void> | null = null
  private readonly timers = new Set<TimerHandle>()
  private readonly exitListeners = new Set<(status: number) => void>()

  /** Throws when spawn() throws (missing or non-executable binary). */
  static start(argv: readonly string[], opts: HexEmbeddedChildOptions): HexEmbeddedChild {
    const proc = opts.spawn(argv, { env: opts.env, stdin: "pipe", stdout: "pipe", stderr: "pipe" })
    return new HexEmbeddedChild(proc, opts)
  }

  private constructor(
    private readonly proc: ChildProcess,
    private readonly opts: HexEmbeddedChildOptions,
  ) {
    this.pid = proc.pid
    this.exit = proc.exited.then(
      (status) => this.handleExit(status),
      () => this.handleExit(-1),
    )
    if (proc.stderr) void drainInto(proc.stderr.getReader(), this.stderr)
    this.ready = this.awaitReady()
    // The source observes `ready`; this keeps an unobserved failure from becoming an unhandled rejection.
    this.ready.catch(() => undefined)
  }

  /** Spawned and not exited (it may still be starting). */
  get running(): boolean {
    return this.exitStatus === null && !this.refused
  }

  /** The validated endpoint, or null while starting or after a refused ready line. */
  get endpoint(): HexEndpoint | null {
    return this.running ? this.endpointValue : null
  }

  get exitCode(): number | null {
    return this.exitStatus
  }

  /** Runs `listener` once the child has exited (at once if it already has). */
  onExit(listener: (status: number) => void): void {
    if (this.exitStatus !== null) listener(this.exitStatus)
    else this.exitListeners.add(listener)
  }

  /**
   * Ends the lease (closes stdin), then SIGTERM after 6 s and SIGKILL after 10 s if the child is still there.
   * Resolves when it exited, or after 12 s at the latest.
   */
  close(): Promise<void> {
    this.closing ??= new Promise<void>((resolve) => {
      if (this.exitStatus !== null) {
        resolve()
        return
      }
      this.endLease()
      this.after(HEX_EMBEDDED_TIMING.termAfterMs, () => this.signal("SIGTERM"))
      this.after(HEX_EMBEDDED_TIMING.killAfterMs, () => this.signal("SIGKILL"))
      const giveUp = this.opts.clock.setTimeout(resolve, HEX_EMBEDDED_TIMING.giveUpAfterMs)
      void this.exit.then(() => {
        this.opts.clock.clearTimeout(giveUp)
        resolve()
      })
    })
    return this.closing
  }

  /** Immediate SIGKILL (a refused ready line, a broken child). */
  kill(): void {
    this.refused = true
    this.endLease()
    this.signal("SIGKILL")
  }

  private handleExit(status: number): number {
    this.exitStatus = status
    for (const timer of this.timers) this.opts.clock.clearTimeout(timer)
    this.timers.clear()
    for (const listener of [...this.exitListeners]) {
      try {
        listener(status)
      } catch {
        // listeners are the source's bookkeeping; one failure must not skip the others
      }
    }
    this.exitListeners.clear()
    return status
  }

  private after(ms: number, run: () => void): void {
    const timer = this.opts.clock.setTimeout(() => {
      this.timers.delete(timer)
      run()
    }, ms)
    this.timers.add(timer)
  }

  private signal(name: "SIGTERM" | "SIGKILL"): void {
    if (this.exitStatus !== null) return
    try {
      this.proc.kill(name)
    } catch {
      // already gone
    }
  }

  private endLease(): void {
    try {
      void Promise.resolve(this.proc.stdin?.end()).catch(() => undefined)
    } catch {
      // the pipe is already closed
    }
  }

  private awaitReady(): Promise<HexEndpoint> {
    return new Promise<HexEndpoint>((resolve, reject) => {
      let settled = false
      const fail = (reason: string): void => {
        if (settled) return
        settled = true
        this.opts.clock.clearTimeout(timer)
        this.kill()
        reject(new VoiceError("unavailable", reason, { source: HEX_EMBEDDED_ID }))
      }
      const timer = this.opts.clock.setTimeout(
        () => fail(`the embedded Hex service did not start within ${HEX_EMBEDDED_TIMING.readyTimeoutMs / 1000} s`),
        HEX_EMBEDDED_TIMING.readyTimeoutMs,
      )
      this.readFirstLine()
        .then((line) => this.validate(line))
        .then(
          (endpoint) => {
            if (settled) {
              endpoint.token.dispose()
              return
            }
            if (this.exitStatus !== null) {
              fail("the embedded Hex service exited right after it started")
              return
            }
            settled = true
            this.opts.clock.clearTimeout(timer)
            this.endpointValue = endpoint
            resolve(endpoint)
          },
          (error: unknown) => fail(isVoiceError(error) ? error.message : "the embedded Hex service failed to start"),
        )
    })
  }

  private async readFirstLine(): Promise<string> {
    const stdout = this.proc.stdout
    if (stdout === undefined) throw new VoiceError("unavailable", "the embedded Hex service has no stdout")
    const reader = stdout.getReader()
    const framer = new LineFramer(HEX_READY_LINE_MAX_BYTES)
    try {
      for (;;) {
        const { done, value } = await reader.read()
        if (done) {
          const status = this.exitStatus === null ? "" : ` (exit ${this.exitStatus})`
          throw new VoiceError("unavailable", `the embedded Hex service exited before it was ready${status}`)
        }
        const newline = value.indexOf(LF)
        const lines = framer.push(newline === -1 ? value : value.subarray(0, newline + 1))
        if (lines.length > 0) {
          this.stdout.push(value.subarray(newline + 1))
          return lines[0] ?? ""
        }
      }
    } catch (error) {
      if (isFrameTooLargeError(error)) {
        throw new VoiceError("unavailable", `the embedded Hex service sent a ready line over ${HEX_READY_LINE_MAX_BYTES / 1024} KiB`)
      }
      throw error
    } finally {
      // Whatever follows is drained so the child never blocks on a full pipe.
      void drainInto(reader, this.stdout)
    }
  }

  private async validate(line: string): Promise<HexEndpoint> {
    const parsed = parseHexReadyLine(line)
    if ("unavailable" in parsed) {
      throw new VoiceError("unavailable", `the embedded Hex service sent an invalid ready line (${parsed.unavailable})`)
    }
    if (parsed.pid !== this.pid) {
      parsed.token.dispose()
      throw new VoiceError("unavailable", `the embedded Hex service reported pid ${parsed.pid}, not its own pid ${this.pid}`)
    }
    const baseUrl = hexBaseUrl(parsed.port)
    try {
      await assertListenerOwner(new URL(baseUrl), { uid: this.opts.uid, label: "this user" }, this.opts.readProc ?? readProcNet)
    } catch (error) {
      parsed.token.dispose()
      throw new VoiceError("unavailable", isTransportError(error) ? error.message : "cannot verify the owner of the embedded Hex port")
    }
    return { baseUrl, token: parsed.token, pid: parsed.pid, origin: "embedded" }
  }
}

// ---------------------------------------------------------------------------------------------------------
// The source
// ---------------------------------------------------------------------------------------------------------

export interface HexEmbeddedSource extends VoiceSource {
  readonly id: typeof HEX_EMBEDDED_ID
  /** AvailabilityHintSource: the Hex app starting or stopping changes this source's availability. */
  onAvailabilityHint(listener: () => void): () => void
  /** The running child's pid, for the voice panel; null when none runs. */
  readonly childPid: number | null
}

class DefaultHexEmbeddedSource implements HexEmbeddedSource {
  readonly id = HEX_EMBEDDED_ID
  readonly label = HEX_EMBEDDED_LABEL
  readonly privacyNote = HEX_EMBEDDED_PRIVACY_NOTE
  private readonly locator: HexLocator
  private readonly opts: ResolvedHexOptions
  private readonly http: HexHttpDeps | null
  private readonly models: HexModelCache
  private readonly hints = new Set<() => void>()
  private readonly closing = new Set<Promise<void>>()
  private child: HexEmbeddedChild | null = null
  private inflight = 0
  private idleTimer: TimerHandle | null = null
  private disposed = false

  constructor(
    private readonly clock: Clock,
    private readonly config: VoiceConfig,
    deps: HexSourceDeps,
  ) {
    this.locator = hexLocatorFor(deps)
    this.opts = resolveHexOptions(clock, deps.hex)
    this.http = hexHttpDeps(this.opts)
    this.models = new HexModelCache(clock)
  }

  get childPid(): number | null {
    return this.child?.running ? this.child.pid : null
  }

  async probe(signal: AbortSignal): Promise<SourceAvailability> {
    const unavailable = (reason: string): SourceAvailability => ({ available: false, reason, checkedAt: this.clock.now() })
    if (this.disposed) return unavailable("disposed")
    if (this.http === null) return unavailable("needs a POSIX system (no getuid)")
    try {
      const app = await raceSignal(this.locator.locateApp(signal), signal)
      if (!("unavailable" in app)) {
        this.retireIdle()
        return unavailable(HEX_APP_RUNNING_REASON)
      }
      const bin = await raceSignal(this.locator.binary(), signal)
      if ("unavailable" in bin) return unavailable(bin.unavailable)
      const child = this.child
      const endpoint = child?.endpoint ?? null
      if (child === null || endpoint === null) {
        return { available: true, detail: `Hex ${bin.version}; the embedded service starts at the first transcription`, checkedAt: this.clock.now() }
      }
      const choice = pickHexModel(await this.models.get(new HexClient(endpoint, this.http, HEX_EMBEDDED_ID), endpoint, signal), this.config.hex.model)
      if (!choice.ok) return unavailable(choice.reason)
      return { available: true, detail: `Hex ${bin.version}, embedded service pid ${child.pid}, model ${choice.model.id}`, checkedAt: this.clock.now() }
    } catch (error) {
      return unavailable(isVoiceError(error) ? error.message : "the probe failed")
    }
  }

  maxWavBytes(): number {
    return HEX_MAX_WAV_BYTES
  }

  async transcribe(clip: WavClip, signal: AbortSignal): Promise<Transcript> {
    if (signal.aborted) throw cancelledError(undefined, HEX_EMBEDDED_ID)
    if (this.disposed) throw new VoiceError("unavailable", "disposed", { source: HEX_EMBEDDED_ID })
    const started = this.clock.now()
    this.inflight += 1
    this.clearIdle()
    try {
      return await this.attempt(clip, signal, started, true)
    } catch (error) {
      if (signal.aborted) throw cancelledError(undefined, HEX_EMBEDDED_ID)
      throw toVoiceError(error, HEX_EMBEDDED_ID, "unavailable")
    } finally {
      this.inflight -= 1
      this.armIdle()
    }
  }

  onAvailabilityHint(listener: () => void): () => void {
    if (!this.config.enabled || this.disposed) return () => undefined
    const off = this.locator.onDiscoveryChange(listener)
    this.hints.add(off)
    return () => {
      if (this.hints.delete(off)) off()
    }
  }

  async dispose(): Promise<void> {
    this.disposed = true
    for (const off of [...this.hints]) off()
    this.hints.clear()
    this.clearIdle()
    if (this.child !== null) this.retire(this.child)
    await Promise.allSettled([...this.closing])
  }

  private async attempt(clip: WavClip, signal: AbortSignal, started: number, mayRespawn: boolean): Promise<Transcript> {
    if (this.http === null) throw new VoiceError("unavailable", "needs a POSIX system (no getuid)", { source: HEX_EMBEDDED_ID })
    const app = await raceSignal(this.locator.locateApp(signal), signal)
    if (!("unavailable" in app)) throw new VoiceError("unavailable", HEX_APP_RUNNING_REASON, { source: HEX_EMBEDDED_ID })
    const endpoint = await raceSignal(this.ensureChild(), signal)
    const child = this.child
    const client = new HexClient(endpoint, this.http, HEX_EMBEDDED_ID)
    try {
      const choice = pickHexModel(await this.models.get(client, endpoint, signal), this.config.hex.model)
      if (!choice.ok) throw new VoiceError(choice.kind, choice.reason, { source: HEX_EMBEDDED_ID })
      const result = await client.transcribe({ wav: clip.wav, model: choice.model.id, language: HEX_LANGUAGE }, signal)
      const text = result.transcript.trim()
      if (text === "") throw new VoiceError("no_speech", "no speech detected", { source: HEX_EMBEDDED_ID })
      return { text, source: HEX_EMBEDDED_ID, model: choice.model.id, language: HEX_LANGUAGE, audioMs: clip.durationMs, elapsedMs: this.clock.now() - started }
    } catch (error) {
      if (!isVoiceError(error) || signal.aborted) throw error
      const rejectedToken = error.opts.status === 401
      if ((rejectedToken || isHexConnectionFailure(error)) && child !== null && this.child === child) {
        // A child whose port refuses its own token or does not answer is broken: replace it.
        child.kill()
        this.child = null
        this.models.clear()
      }
      if (!rejectedToken) throw error
      if (mayRespawn) return this.attempt(clip, signal, started, false)
      throw new VoiceError("unavailable", "the embedded Hex service rejected its token (HTTP 401)", { source: HEX_EMBEDDED_ID, status: 401 })
    }
  }

  /** The running child's endpoint, or a new child (spawned lazily, shared by concurrent callers). */
  private async ensureChild(): Promise<HexEndpoint> {
    const current = this.child
    if (current?.running) return current.ready
    const bin = await this.locator.binary()
    if ("unavailable" in bin) throw new VoiceError("unavailable", bin.unavailable, { source: HEX_EMBEDDED_ID })
    if (this.disposed) throw new VoiceError("unavailable", "disposed", { source: HEX_EMBEDDED_ID })
    const raced = this.child
    if (raced?.running) return raced.ready
    if (this.opts.uid === null) throw new VoiceError("unavailable", "needs a POSIX system (no getuid)", { source: HEX_EMBEDDED_ID })
    let child: HexEmbeddedChild
    try {
      child = HexEmbeddedChild.start([...bin.command, "service", "--embedded"], {
        clock: this.clock,
        spawn: this.opts.spawn,
        env: this.opts.childEnv(),
        uid: this.opts.uid,
        ...(this.opts.readProc ? { readProc: this.opts.readProc } : {}),
      })
    } catch (error) {
      this.locator.forgetBinary()
      throw new VoiceError("unavailable", isMissingExecutable(error) ? "not installed" : "the hex binary cannot be started", { source: HEX_EMBEDDED_ID })
    }
    this.child = child
    child.onExit(() => {
      if (this.child !== child) return
      this.child = null
      this.models.clear()
    })
    child.ready.catch(() => {
      if (this.child === child) this.child = null
    })
    return child.ready
  }

  private clearIdle(): void {
    if (this.idleTimer === null) return
    this.clock.clearTimeout(this.idleTimer)
    this.idleTimer = null
  }

  /** The idle TTL runs from the end of the last transcription. */
  private armIdle(): void {
    if (this.inflight > 0 || this.child === null || this.disposed) return
    this.clearIdle()
    this.idleTimer = this.clock.setTimeout(() => {
      this.idleTimer = null
      this.retireIdle()
    }, HEX_EMBEDDED_TIMING.idleTtlMs)
  }

  private retireIdle(): void {
    if (this.inflight === 0 && this.child !== null) this.retire(this.child)
  }

  private retire(child: HexEmbeddedChild): void {
    if (this.child === child) this.child = null
    this.clearIdle()
    this.models.clear()
    const closing = child.close()
    this.closing.add(closing)
    void closing.finally(() => this.closing.delete(closing))
  }
}

export function createHexEmbeddedSource(deps: HexSourceDeps): HexEmbeddedSource {
  return new DefaultHexEmbeddedSource(deps.clock, deps.config, deps)
}
