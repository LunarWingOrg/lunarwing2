/**
 * The parec fallback recorder (design 5.5), used when OpenTUI Audio is unavailable. One async child per
 * recording, argv only (never a shell), the allowlisted child environment (config/child-env.ts):
 *
 *   parec --raw --format=s16le --rate=16000 --channels=1 --latency-msec=20 --client-name=lunarwing-tui
 *         --stream-name=dictation [--device=<name>]
 *
 * parec writes raw PCM unbuffered; chunks are converted with an odd-byte carry (voice/levels.ts S16leDecoder) and
 * the first stdout chunk calls onFirstChunk. Stop: SIGINT, wait 1 s, SIGKILL, read the rest. Cancel: SIGKILL.
 * stderr goes to a 4 KiB ring and is only summarized. pw-record is not used (it needs --raw and stdbuf -o0).
 *
 * The configured or stored device (`[voice] device`, `/voice device`) is an OpenTUI device name or a substring of
 * one ("C922"), not a PulseAudio source name, so --device gets it only when it names a source: an exact entry of
 * `pactl list short sources` (asked only for names that do not look like one), or a name shaped like a source
 * (`alsa_input.usb-...`, `<sink>.monitor`, `@DEFAULT_SOURCE@`). Otherwise parec records the default source and the
 * handle reports a device fallback, as the OpenTUI recorder does when no device matches. When parec exits before
 * the first chunk with --device set, it is started once more without it and reports the fallback through
 * onDeviceFallback (start() has returned by then).
 */
import type { Clock } from "@opentui/core"
import { bunSpawn, type ChildEnv, type ChildProcess, type SpawnFn } from "../config/types.ts"
import { sanitizeLine } from "../sanitize.ts"
import { cancelledError, VoiceError } from "./errors.ts"
import { S16leDecoder } from "./levels.ts"
import { CaptureSession, readAll, settleWithin, TIMED_OUT, type CaptureHandle, type CaptureStartOptions } from "./recorder.ts"
import type { Recorder, WavClip } from "./types.ts"

export const PAREC_STOP_GRACE_MS = 1_000
/** `pactl list short sources` gets this long before it is killed (an unresponsive sound server). */
export const PACTL_TIMEOUT_MS = 1_500
const STDERR_RING_BYTES = 4 * 1024
const PACTL_MAX_OUTPUT_BYTES = 64 * 1024
const SUMMARY_CHARS = 160

/** The parec argv of 5.5 (the command can be overridden for tests and odd installs). */
export function parecArgv(device?: string, command = "parec"): string[] {
  const deviceArg = device !== undefined && device.trim() !== "" ? [`--device=${device.trim()}`] : []
  return [
    command,
    "--raw",
    "--format=s16le",
    "--rate=16000",
    "--channels=1",
    "--latency-msec=20",
    "--client-name=lunarwing-tui",
    "--stream-name=dictation",
    ...deviceArg,
  ]
}

const PULSE_SPECIAL_SOURCES: ReadonlySet<string> = new Set(["@DEFAULT_SOURCE@", "@DEFAULT_MONITOR@"])
/** One token of dot-separated parts: `alsa_input.usb-046d_C922-02.analog-stereo`, `bluez_input.00:1B:66:AA:BB:CC.0`. */
const PULSE_SOURCE_NAME = /^[A-Za-z0-9_][A-Za-z0-9_:+-]*(?:\.[A-Za-z0-9_:+-]+)+$/

/**
 * True for PulseAudio / PipeWire source names: the device-backed sources are `<api>_input.<device>[.<profile>]`,
 * monitors `<sink>.monitor`, plus the special `@DEFAULT_SOURCE@` and `@DEFAULT_MONITOR@`. An OpenTUI device name
 * (spaces) or a `[voice] device` substring ("C922", no dot) is not.
 */
export function looksLikePulseSource(name: string): boolean {
  return PULSE_SPECIAL_SOURCES.has(name) || PULSE_SOURCE_NAME.test(name)
}

/** The names of `pactl list short sources` (index, name, driver, sample spec, state; tab-separated). */
export function parsePactlSources(text: string): string[] {
  return text.split("\n").flatMap((line) => {
    const name = line.split("\t")[1]?.trim()
    return name === undefined || name === "" ? [] : [name]
  })
}

/** PulseAudio source names, for an exact --device match; [] when they cannot be listed. */
export type PulseSourceLister = (signal: AbortSignal) => Promise<readonly string[]>

export interface ParecRecorderOptions {
  clock: Clock
  /** buildChildEnv() from config/child-env.ts. */
  env: ChildEnv
  spawn?: SpawnFn
  /** Executable lookup for available(); default Bun.which with the child PATH. */
  which?: (command: string) => string | null
  command?: string
  /** Default: `pactl list short sources` through `spawn`, with the same environment. */
  listSources?: PulseSourceLister
}

/** Keeps the last `capacity` bytes of a stream. */
class ByteRing {
  private bytes = new Uint8Array(0)

  constructor(private readonly capacity: number) {}

  push(chunk: Uint8Array): void {
    const joined = new Uint8Array(Math.min(this.capacity, this.bytes.length + chunk.length))
    const tail = chunk.subarray(Math.max(0, chunk.length - joined.length))
    const keep = joined.length - tail.length
    joined.set(this.bytes.subarray(this.bytes.length - keep), 0)
    joined.set(tail, keep)
    this.bytes = joined
  }

  /** The last non-empty line, sanitized and shortened. */
  summary(): string {
    const lines = new TextDecoder().decode(this.bytes).split("\n").map((line) => sanitizeLine(line).trim())
    const last = lines.filter((line) => line !== "").at(-1) ?? ""
    return last.length > SUMMARY_CHARS ? `${last.slice(0, SUMMARY_CHARS - 1)}…` : last
  }
}

async function drain(stream: ReadableStream<Uint8Array> | undefined, onChunk: (chunk: Uint8Array) => void): Promise<void> {
  if (stream === undefined) return
  try {
    await readAll(stream, onChunk)
  } catch {
    // The child was killed mid-read; whatever arrived is kept.
  }
}

function spawnFailure(error: unknown): VoiceError {
  const code = typeof error === "object" && error !== null ? (error as { code?: unknown }).code : undefined
  const detail = code === "ENOENT" ? "parec is not installed" : code === "EACCES" ? "parec is not executable" : "parec could not be started"
  return new VoiceError("recorder", detail, { source: "recorder" })
}

/** `pactl list short sources` (argv only, the child environment, killed after PACTL_TIMEOUT_MS); [] on any failure. */
async function listPulseSources(spawn: SpawnFn, env: ChildEnv, signal: AbortSignal): Promise<string[]> {
  let child: ChildProcess
  try {
    child = spawn(["pactl", "list", "short", "sources"], {
      env,
      stdin: "ignore",
      stdout: "pipe",
      stderr: "ignore",
      signal,
      timeout: PACTL_TIMEOUT_MS,
      killSignal: "SIGKILL",
    })
  } catch {
    return []
  }
  const chunks: Uint8Array[] = []
  let size = 0
  await drain(child.stdout, (chunk) => {
    if (size >= PACTL_MAX_OUTPUT_BYTES) return
    chunks.push(chunk.slice(0, PACTL_MAX_OUTPUT_BYTES - size))
    size += chunk.length
  })
  if ((await child.exited) !== 0) return []
  return parsePactlSources(new TextDecoder().decode(Buffer.concat(chunks)))
}

/** Starts parec on a source name, or on the default source (null). */
type ParecLauncher = (device: string | null) => ChildProcess

class ParecRecording implements CaptureHandle {
  readonly kind = "parec" as const
  /** The source parec records from; null: the default source. */
  deviceName: string | null
  /** A device was requested, but the default source records. */
  deviceFallback: boolean
  private readonly session: CaptureSession
  private readonly decoder = new S16leDecoder()
  private child: ChildProcess
  private stderr = new ByteRing(STDERR_RING_BYTES)
  private stdoutDone: Promise<void>
  private retried = false
  private stopping = false
  private cancelled = false

  constructor(
    private readonly launch: ParecLauncher,
    requested: string | null,
    device: string | null,
    private readonly opts: CaptureStartOptions,
    private readonly clock: Clock,
  ) {
    this.deviceName = device
    this.deviceFallback = requested !== null && device === null
    this.session = new CaptureSession({ maxDurationMs: opts.maxDurationMs, clock, onLevel: opts.onLevel, onFirstChunk: opts.onFirstChunk })
    this.child = launch(device)
    this.stdoutDone = this.watch(this.child)
    if (opts.signal.aborted) this.cancel()
    else opts.signal.addEventListener("abort", () => this.cancel(), { once: true })
  }

  async stop(): Promise<WavClip> {
    if (this.cancelled) throw cancelledError("the recording was cancelled", "recorder")
    this.stopping = true
    this.child.kill("SIGINT")
    if ((await settleWithin(this.child.exited, PAREC_STOP_GRACE_MS, this.clock)) === TIMED_OUT) {
      this.child.kill("SIGKILL")
      await this.child.exited
    }
    await this.stdoutDone
    this.decoder.reset()
    return this.session.finish(0)
  }

  cancel(): void {
    if (this.cancelled) return
    this.cancelled = true
    this.child.kill("SIGKILL")
    this.decoder.reset()
    this.session.discard()
  }

  /** Reads one child's stdout and stderr and handles its exit; returns the end of its stdout. */
  private watch(child: ChildProcess): Promise<void> {
    const stderr = this.stderr
    const stdoutDone = drain(child.stdout, (chunk) => this.onPcm(chunk))
    void drain(child.stderr, (chunk) => stderr.push(chunk))
    void child.exited.then((code) => this.onExit(child, stdoutDone, code))
    return stdoutDone
  }

  private onPcm(chunk: Uint8Array): void {
    const samples = this.decoder.push(chunk)
    chunk.fill(0)
    if (!this.cancelled) this.session.appendS16(samples)
    samples.fill(0)
  }

  private async onExit(child: ChildProcess, stdoutDone: Promise<void>, code: number): Promise<void> {
    if (this.stopping || this.cancelled || child !== this.child) return
    await stdoutDone
    if (this.stopping || this.cancelled || child !== this.child) return
    if (!this.session.firstChunkSeen && this.deviceName !== null && !this.retried) {
      this.retryOnDefaultSource()
      return
    }
    const summary = this.stderr.summary()
    const message = `parec exited (code ${code})${summary ? `: ${summary}` : ""}`
    this.opts.onFailure?.(new VoiceError("recorder", message, { source: "recorder" }))
  }

  /** The named source failed before any audio arrived: once more on the default source, reported as a fallback. */
  private retryOnDefaultSource(): void {
    this.retried = true
    this.decoder.reset()
    let child: ChildProcess
    try {
      child = this.launch(null)
    } catch (error) {
      this.opts.onFailure?.(spawnFailure(error))
      return
    }
    this.child = child
    this.stderr = new ByteRing(STDERR_RING_BYTES)
    this.stdoutDone = this.watch(child)
    this.deviceName = null
    this.deviceFallback = true
    this.opts.onDeviceFallback?.({ deviceName: null, deviceFallback: true })
  }
}

class ParecRecorder implements Recorder {
  readonly kind = "parec" as const
  private readonly spawn: SpawnFn
  private readonly command: string

  constructor(private readonly opts: ParecRecorderOptions) {
    this.spawn = opts.spawn ?? bunSpawn
    this.command = opts.command ?? "parec"
  }

  async available(): Promise<{ ok: true } | { ok: false; reason: string }> {
    const which = this.opts.which ?? ((command: string) => Bun.which(command, { PATH: this.opts.env.PATH ?? "" }))
    return which(this.command) === null ? { ok: false, reason: "parec is not installed" } : { ok: true }
  }

  async start(opts: CaptureStartOptions): Promise<CaptureHandle> {
    if (opts.signal.aborted) throw cancelledError("the recording was cancelled", "recorder")
    const requested = opts.device?.trim() || null
    const device = requested === null ? null : await this.sourceFor(requested, opts.signal)
    if (opts.signal.aborted) throw cancelledError("the recording was cancelled", "recorder")
    const launch: ParecLauncher = (name) =>
      this.spawn(parecArgv(name ?? undefined, this.command), {
        env: this.opts.env,
        stdin: "ignore",
        stdout: "pipe",
        stderr: "pipe",
      })
    try {
      return new ParecRecording(launch, requested, device, opts, this.opts.clock)
    } catch (error) {
      throw spawnFailure(error)
    }
  }

  /** The source name to pass as --device, or null for the default source. */
  private async sourceFor(name: string, signal: AbortSignal): Promise<string | null> {
    if (looksLikePulseSource(name)) return name
    const list = this.opts.listSources ?? ((abort: AbortSignal) => listPulseSources(this.spawn, this.opts.env, abort))
    const sources = await list(signal).catch((): readonly string[] => [])
    return sources.includes(name) ? name : null
  }
}

export function createParecRecorder(opts: ParecRecorderOptions): Recorder {
  return new ParecRecorder(opts)
}
