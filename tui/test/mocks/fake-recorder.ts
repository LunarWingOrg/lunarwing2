/**
 * A Recorder that replays scripted Float32 chunks instead of opening a microphone (design 11.3.1). It records
 * through the real CaptureSession (src/voice/recorder.ts), so clips are real WAVs with real levels, peaks and a
 * real dispose(); only the audio device is fake. Time comes from the test's ManualClock:
 *
 *   const recorder = new FakeRecorder({ clock })
 *   voice.pttKey({ eventType: "press", repeated: false })   // start() is called at once
 *   await flushAsync()                                       // start() resolved
 *   recorder.play(1_000)                                     // 1 s of tone in 20 ms chunks, clock advanced per chunk
 *   recorder.active?.fail("device unplugged")                // a capture that ends on its own
 */
import type { Clock } from "@opentui/core"
import { cancelledError, VoiceError } from "../../src/voice/errors.ts"
import { CaptureSession, matchCaptureDevice, type CaptureHandle, type CaptureStartOptions, type DeviceAwareRecorder } from "../../src/voice/recorder.ts"
import type { CaptureDevice, RecorderKind, WavClip } from "../../src/voice/types.ts"
import { FRAME_MS, FRAME_SAMPLES, SAMPLE_RATE } from "../../src/voice/wav.ts"

/** ManualClock, or any Clock the fake can move forward while it plays audio. */
export interface AdvanceableClock extends Clock {
  advance(ms: number): void
}

/** `ms` of a sine at `amplitude` (0.3 is about -13.5 dBFS RMS: speech-like). */
export function tone(ms: number, amplitude = 0.3, hz = 220): Float32Array {
  const samples = new Float32Array(Math.round((ms * SAMPLE_RATE) / 1000))
  for (let i = 0; i < samples.length; i += 1) samples[i] = amplitude * Math.sin((2 * Math.PI * hz * i) / SAMPLE_RATE)
  return samples
}

/** `ms` at a constant tiny level (a muted source reads about -70 to -90 dBFS). */
export function quiet(ms: number, amplitude = 0): Float32Array {
  return new Float32Array(Math.round((ms * SAMPLE_RATE) / 1000)).fill(amplitude)
}

export interface FakeRecorderOptions {
  clock: AdvanceableClock
  kind?: RecorderKind
  available?: { ok: true } | { ok: false; reason: string }
  /** start() rejects with VoiceError("recorder", startError). */
  startError?: string
  /** start() resolves after this many clock ms (default: a microtask). */
  startDelayMs?: number
  /** stop() resolves after this many clock ms (default: a microtask). */
  stopDelayMs?: number
  devices?: CaptureDevice[]
  /** Reported as WavClip.droppedFrames. */
  droppedFrames?: number
}

export class FakeRecording implements CaptureHandle {
  readonly kind: RecorderKind
  readonly deviceName: string | null
  readonly deviceFallback: boolean
  stopped = false
  cancelled = false
  clip: WavClip | null = null
  private readonly session: CaptureSession

  constructor(
    readonly options: CaptureStartOptions,
    private readonly owner: FakeRecorder,
    private readonly clock: AdvanceableClock,
  ) {
    this.kind = owner.kind
    // The same rule as the OpenTUI recorder: exact name first, then a case-insensitive substring.
    const match = options.device === undefined ? null : matchCaptureDevice(owner.devices, options.device)
    this.deviceName = match?.name ?? owner.devices.find((d) => d.isDefault)?.name ?? null
    this.deviceFallback = options.device !== undefined && match === null
    this.session = new CaptureSession({ maxDurationMs: options.maxDurationMs, clock, onLevel: options.onLevel, onFirstChunk: options.onFirstChunk })
    options.signal.addEventListener("abort", () => this.cancel(), { once: true })
  }

  get live(): boolean {
    return !this.stopped && !this.cancelled
  }

  /** Delivers samples now (the first call fires onFirstChunk). Ignored once stopped or cancelled. */
  push(samples: Float32Array): void {
    if (this.live) this.session.appendFloat32(samples)
  }

  /** The capture ends on its own (device error). */
  fail(message = "device unplugged"): void {
    if (!this.live) return
    this.cancelled = true
    this.session.discard()
    this.options.onFailure?.(new VoiceError("recorder", message, { source: "recorder" }))
  }

  async stop(): Promise<WavClip> {
    if (this.cancelled) throw cancelledError("the recording was cancelled", "recorder")
    this.stopped = true
    this.owner.stops += 1
    const delay = this.owner.opts.stopDelayMs ?? 0
    if (delay > 0) await new Promise<void>((resolve) => this.clock.setTimeout(resolve, delay))
    if (this.cancelled) throw cancelledError("the recording was cancelled", "recorder")
    this.clip = this.session.finish(this.owner.opts.droppedFrames ?? 0)
    return this.clip
  }

  cancel(): void {
    if (this.cancelled) return
    this.cancelled = true
    this.owner.cancels += 1
    this.session.discard()
  }
}

export class FakeRecorder implements DeviceAwareRecorder {
  readonly kind: RecorderKind
  readonly devices: CaptureDevice[]
  /** Every start() call, also the refused ones. */
  readonly startCalls: CaptureStartOptions[] = []
  readonly recordings: FakeRecording[] = []
  stops = 0
  cancels = 0

  constructor(readonly opts: FakeRecorderOptions) {
    this.kind = opts.kind ?? "opentui"
    this.devices = opts.devices ?? [{ index: 0, name: "Built-in Audio Analog Stereo", isDefault: true }]
  }

  /** The newest recording that is still capturing. */
  get active(): FakeRecording | null {
    const last = this.recordings.at(-1)
    return last?.live ? last : null
  }

  async available(): Promise<{ ok: true } | { ok: false; reason: string }> {
    return this.opts.available ?? { ok: true }
  }

  async listDevices(): Promise<CaptureDevice[]> {
    return this.devices.map((d) => ({ ...d }))
  }

  async start(options: CaptureStartOptions): Promise<CaptureHandle> {
    this.startCalls.push(options)
    const delay = this.opts.startDelayMs ?? 0
    if (delay > 0) await new Promise<void>((resolve) => this.opts.clock.setTimeout(resolve, delay))
    if (this.opts.startError !== undefined) throw new VoiceError("recorder", this.opts.startError, { source: "recorder" })
    if (options.signal.aborted) throw cancelledError("the recording was cancelled", "recorder")
    const recording = new FakeRecording(options, this, this.opts.clock)
    this.recordings.push(recording)
    return recording
  }

  /**
   * Plays `samples` into the active recording in 20 ms chunks, advancing the clock 20 ms after each chunk (so the
   * controller's 50 ms tick runs as in real time). Stops early when the recording ends.
   */
  playSamples(samples: Float32Array): void {
    for (let offset = 0; offset < samples.length; offset += FRAME_SAMPLES) {
      const recording = this.active
      if (recording === null) return
      recording.push(samples.subarray(offset, offset + FRAME_SAMPLES))
      this.opts.clock.advance(FRAME_MS)
    }
  }

  /** `ms` of speech-like tone. */
  play(ms: number, amplitude = 0.3): void {
    this.playSamples(tone(ms, amplitude))
  }

  /** `ms` of (near) silence. */
  playQuiet(ms: number, amplitude = 0): void {
    this.playSamples(quiet(ms, amplitude))
  }
}
