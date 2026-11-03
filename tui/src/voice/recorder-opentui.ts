/**
 * The OpenTUI Audio recorder, primary on this desktop (design 5.5; ot081:audio.d.ts).
 *
 * - One engine per process, created at startup: Audio.create({ sampleRate: 16000, autoStart: false }). Capture
 *   always runs at the engine rate (default 48 000), so the rate is set here; autoStart false opens no device until
 *   a recording, which keeps engine creation off the press-to-capture path. An initialization error marks the
 *   recorder unavailable.
 * - Per recording: openCapture({ channels: 1, chunkFrames: 320, capacityFrames: 16000, signal }) gives an
 *   AudioCaptureStream whose `readable` yields Float32Array chunks of 20 ms; the first chunk read calls
 *   onFirstChunk (a PipeWire source woken from suspend takes a moment, only then is the mic live).
 * - A second openCapture while one is open throws "Audio capture ring is already in use": the recorder is a
 *   singleton and waits briefly for a previous capture to close.
 * - Stop: capture.stop() drains the ring and closes the stream, so the read loop ends. Cancel: capture.dispose().
 * - getStats().framesDropped (a bigint) becomes WavClip.droppedFrames ("audio dropped: the UI was busy").
 * - Devices: listCaptureDevices() / selectCaptureDevice(index); the requested name matches case-insensitively.
 *
 * Audio and AudioCaptureStream are EventEmitters whose "error" event throws when nobody listens, so the adapter
 * always attaches a listener; failures still reach the read loop through the stream.
 */
import { Audio, type AudioCaptureStream, type Clock } from "@opentui/core"
import { cancelledError, VoiceError } from "./errors.ts"
import {
  CaptureSession,
  matchCaptureDevice,
  readAll,
  settleWithin,
  TIMED_OUT,
  type CaptureHandle,
  type CaptureStartOptions,
  type DeviceAwareRecorder,
  type DisposableRecorder,
} from "./recorder.ts"
import type { CaptureDevice, WavClip } from "./types.ts"
import { FRAME_SAMPLES, SAMPLE_RATE } from "./wav.ts"

/** openCapture options of 5.5: mono, 20 ms chunks, a one-second ring. */
export const CAPTURE_OPTIONS = Object.freeze({ channels: 1, chunkFrames: FRAME_SAMPLES, capacityFrames: SAMPLE_RATE })
/** Audio.create options of 5.5. */
export const ENGINE_OPTIONS = Object.freeze({ sampleRate: SAMPLE_RATE, autoStart: false })

const STOP_DRAIN_MS = 2_000
const PREVIOUS_CLOSE_MS = 1_000

/** The AudioCaptureStream subset the recorder uses. */
export interface CaptureStreamLike {
  readonly readable: ReadableStream<Float32Array>
  readonly closed: Promise<void>
  stop(): void
  dispose(): void
  getStats(): { framesDropped: bigint }
}

/** The Audio subset the recorder uses; tests inject a fake. */
export interface AudioEngineLike {
  readonly sampleRate: number
  openCapture(options: { channels: number; chunkFrames: number; capacityFrames: number; signal?: AbortSignal }): Promise<CaptureStreamLike>
  listCaptureDevices(): readonly CaptureDevice[] | null
  selectCaptureDevice(index: number): boolean
  clearCaptureDeviceSelection(): void
  dispose(): void
}

function adaptCapture(capture: AudioCaptureStream): CaptureStreamLike {
  capture.on("error", () => undefined)
  return {
    readable: capture.readable,
    closed: capture.closed,
    stop: () => capture.stop(),
    dispose: () => capture.dispose(),
    getStats: () => capture.getStats(),
  }
}

/** Wraps the real engine; the error listener keeps Node's EventEmitter from throwing on "error" events. */
export function adaptOpenTuiAudio(audio: Audio): AudioEngineLike {
  audio.on("error", () => undefined)
  return {
    sampleRate: audio.sampleRate,
    openCapture: async (options) => adaptCapture(await audio.openCapture(options)),
    listCaptureDevices: () => audio.listCaptureDevices(),
    selectCaptureDevice: (index) => audio.selectCaptureDevice(index),
    clearCaptureDeviceSelection: () => audio.clearCaptureDeviceSelection(),
    dispose: () => audio.dispose(),
  }
}

export function createOpenTuiEngine(): AudioEngineLike {
  return adaptOpenTuiAudio(Audio.create({ ...ENGINE_OPTIONS }))
}

export interface OpenTuiRecorderOptions {
  clock: Clock
  /** Default: createOpenTuiEngine (the real native engine). */
  createEngine?: () => AudioEngineLike
}

function errorText(error: unknown): string {
  const message = error instanceof Error ? error.message : ""
  return message.trim() === "" ? "unknown error" : message.trim().slice(0, 160)
}

class OpenTuiRecording implements CaptureHandle {
  readonly kind = "opentui" as const
  readonly done: Promise<void>
  private readonly session: CaptureSession
  private stopping = false
  private cancelled = false

  constructor(
    private readonly capture: CaptureStreamLike,
    private readonly opts: CaptureStartOptions,
    private readonly clock: Clock,
    readonly deviceName: string | null,
    readonly deviceFallback: boolean,
  ) {
    this.session = new CaptureSession({ maxDurationMs: opts.maxDurationMs, clock, onLevel: opts.onLevel, onFirstChunk: opts.onFirstChunk })
    this.done = this.pump()
    if (opts.signal.aborted) this.cancel()
    else opts.signal.addEventListener("abort", () => this.cancel(), { once: true })
  }

  async stop(): Promise<WavClip> {
    if (this.cancelled) throw cancelledError("the recording was cancelled", "recorder")
    this.stopping = true
    this.capture.stop()
    if ((await settleWithin(this.done, STOP_DRAIN_MS, this.clock)) === TIMED_OUT) this.capture.dispose()
    return this.session.finish(Number(this.capture.getStats().framesDropped))
  }

  cancel(): void {
    if (this.cancelled) return
    this.cancelled = true
    this.capture.dispose()
    this.session.discard()
  }

  private async pump(): Promise<void> {
    try {
      await readAll(this.capture.readable, (chunk) => {
        if (!this.cancelled) this.session.appendFloat32(chunk)
        chunk.fill(0)
      })
      if (!this.stopping && !this.cancelled) this.fail("the audio capture ended unexpectedly")
    } catch (error) {
      if (!this.stopping && !this.cancelled) this.fail(`the audio capture failed: ${errorText(error)}`)
    }
  }

  private fail(message: string): void {
    this.opts.onFailure?.(new VoiceError("recorder", message, { source: "recorder" }))
  }
}

class OpenTuiRecorder implements DeviceAwareRecorder, DisposableRecorder {
  readonly kind = "opentui" as const
  private engine: AudioEngineLike | null = null
  private unavailableReason: string | null = null
  private active: OpenTuiRecording | null = null
  private activeCapture: CaptureStreamLike | null = null

  constructor(private readonly opts: OpenTuiRecorderOptions) {
    try {
      this.engine = (opts.createEngine ?? createOpenTuiEngine)()
    } catch (error) {
      this.unavailableReason = `OpenTUI audio is unavailable: ${errorText(error)}`
    }
  }

  async available(): Promise<{ ok: true } | { ok: false; reason: string }> {
    if (this.engine === null) return { ok: false, reason: this.unavailableReason ?? "OpenTUI audio is unavailable" }
    const devices = this.engine.listCaptureDevices()
    if (devices === null) return { ok: false, reason: "OpenTUI audio cannot list capture devices" }
    if (devices.length === 0) return { ok: false, reason: "no capture device" }
    return { ok: true }
  }

  async listDevices(): Promise<CaptureDevice[]> {
    return (this.engine?.listCaptureDevices() ?? []).map((d) => ({ index: d.index, name: d.name, isDefault: d.isDefault }))
  }

  async start(opts: CaptureStartOptions): Promise<CaptureHandle> {
    const engine = this.engine
    if (engine === null) throw new VoiceError("recorder", this.unavailableReason ?? "OpenTUI audio is unavailable", { source: "recorder" })
    if (opts.signal.aborted) throw cancelledError("the recording was cancelled", "recorder")
    // The previous capture may still be releasing the ring (dispose is asynchronous).
    if (this.activeCapture !== null) await settleWithin(this.activeCapture.closed, PREVIOUS_CLOSE_MS, this.opts.clock)
    const device = this.selectDevice(engine, opts.device)
    let capture: CaptureStreamLike
    try {
      capture = await engine.openCapture({ ...CAPTURE_OPTIONS, signal: opts.signal })
    } catch (error) {
      if (opts.signal.aborted) throw cancelledError("the recording was cancelled", "recorder")
      throw new VoiceError("recorder", `the microphone could not be opened: ${errorText(error)}`, { source: "recorder" })
    }
    const recording = new OpenTuiRecording(capture, opts, this.opts.clock, device.name, device.fallback)
    this.active = recording
    this.activeCapture = capture
    void recording.done.then(() => {
      if (this.active === recording) this.active = null
    })
    return recording
  }

  dispose(): void {
    this.active?.cancel()
    this.engine?.dispose()
    this.engine = null
    this.unavailableReason = "the audio engine was closed"
  }

  private selectDevice(engine: AudioEngineLike, requested: string | undefined): { name: string | null; fallback: boolean } {
    const devices = engine.listCaptureDevices() ?? []
    const match = requested === undefined ? null : matchCaptureDevice(devices, requested)
    if (match === null) {
      engine.clearCaptureDeviceSelection()
      const fallback = requested !== undefined && requested.trim() !== ""
      return { name: devices.find((d) => d.isDefault)?.name ?? null, fallback }
    }
    if (!engine.selectCaptureDevice(match.index)) {
      throw new VoiceError("recorder", `the capture device ${match.name} could not be selected`, { source: "recorder" })
    }
    return { name: match.name, fallback: false }
  }
}

export function createOpenTuiRecorder(opts: OpenTuiRecorderOptions): DeviceAwareRecorder & DisposableRecorder {
  return new OpenTuiRecorder(opts)
}
