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
 */
import type { Clock } from "@opentui/core"
import { bunSpawn, type ChildEnv, type ChildProcess, type SpawnFn } from "../config/types.ts"
import { sanitizeLine } from "../sanitize.ts"
import { cancelledError, VoiceError } from "./errors.ts"
import { S16leDecoder } from "./levels.ts"
import { CaptureSession, readAll, settleWithin, TIMED_OUT, type CaptureHandle, type CaptureStartOptions } from "./recorder.ts"
import type { Recorder, WavClip } from "./types.ts"

export const PAREC_STOP_GRACE_MS = 1_000
const STDERR_RING_BYTES = 4 * 1024
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

export interface ParecRecorderOptions {
  clock: Clock
  /** buildChildEnv() from config/child-env.ts. */
  env: ChildEnv
  spawn?: SpawnFn
  /** Executable lookup for available(); default Bun.which with the child PATH. */
  which?: (command: string) => string | null
  command?: string
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

class ParecRecording implements CaptureHandle {
  readonly kind = "parec" as const
  readonly deviceFallback = false
  readonly deviceName: string | null
  private readonly session: CaptureSession
  private readonly decoder = new S16leDecoder()
  private readonly stderr = new ByteRing(STDERR_RING_BYTES)
  private readonly stdoutDone: Promise<void>
  private stopping = false
  private cancelled = false

  constructor(
    private readonly child: ChildProcess,
    private readonly opts: CaptureStartOptions,
    private readonly clock: Clock,
  ) {
    this.deviceName = opts.device?.trim() || null
    this.session = new CaptureSession({ maxDurationMs: opts.maxDurationMs, clock, onLevel: opts.onLevel, onFirstChunk: opts.onFirstChunk })
    this.stdoutDone = drain(child.stdout, (chunk) => this.onPcm(chunk))
    void drain(child.stderr, (chunk) => this.stderr.push(chunk))
    void child.exited.then((code) => this.onExit(code))
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

  private onPcm(chunk: Uint8Array): void {
    const samples = this.decoder.push(chunk)
    chunk.fill(0)
    if (!this.cancelled) this.session.appendS16(samples)
    samples.fill(0)
  }

  private async onExit(code: number): Promise<void> {
    if (this.stopping || this.cancelled) return
    await this.stdoutDone
    if (this.stopping || this.cancelled) return
    const summary = this.stderr.summary()
    const message = `parec exited (code ${code})${summary ? `: ${summary}` : ""}`
    this.opts.onFailure?.(new VoiceError("recorder", message, { source: "recorder" }))
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
    let child: ChildProcess
    try {
      child = this.spawn(parecArgv(opts.device, this.command), {
        env: this.opts.env,
        stdin: "ignore",
        stdout: "pipe",
        stderr: "pipe",
      })
    } catch (error) {
      throw spawnFailure(error)
    }
    return new ParecRecording(child, opts, this.opts.clock)
  }
}

export function createParecRecorder(opts: ParecRecorderOptions): Recorder {
  return new ParecRecorder(opts)
}
