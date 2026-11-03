/**
 * Recorder plumbing shared by voice/recorder-opentui.ts, voice/recorder-parec.ts and the test fake (design 5.5).
 *
 * The Recorder and RecorderHandle contracts live in voice/types.ts. This module adds:
 * - CaptureSession: PCM accumulation into the preallocated buffer, 20 ms levels, the clip peak, first-chunk
 *   detection and the final WAV. No temp files; every buffer is zero-filled after use.
 * - CaptureStartOptions.onFailure: how a recorder reports that a running capture ended on its own (device error,
 *   parec exit) before stop() or cancel(); RecorderStartOptions has no channel for that.
 * - Optional recorder capabilities the manager probes for: device listing (OpenTUI) and dispose (the engine).
 *
 * main.tsx builds the list in fallback order: [createOpenTuiRecorder(...), createParecRecorder(...)].
 */
import type { Clock, TimerHandle } from "@opentui/core"
import type { VoiceError } from "./errors.ts"
import { FrameSplitter, LEVEL_FLOOR_DB, measureS16 } from "./levels.ts"
import type { CaptureDevice, LevelSample, Recorder, RecorderHandle, RecorderStartOptions } from "./types.ts"
import { createWavClip, PcmBuffer, type DisposableWavClip } from "./wav.ts"

export interface CaptureStartOptions extends RecorderStartOptions {
  /**
   * The capture ended on its own before stop() or cancel(). Before the first chunk the manager tries the next
   * recorder; afterwards the recording is cancelled with the message.
   */
  onFailure?: (error: VoiceError) => void
}

/** The device a handle actually records from (5.5). */
export interface RecordingDevice {
  /** The device name when the recorder knows it; null = the default source. */
  readonly deviceName: string | null
  /** The requested device was not found, so the default source is used. */
  readonly deviceFallback: boolean
}

export type CaptureHandle = RecorderHandle & RecordingDevice

export interface DeviceAwareRecorder extends Recorder {
  listDevices(): Promise<CaptureDevice[]>
}

export interface DisposableRecorder extends Recorder {
  dispose(): void | Promise<void>
}

export function isDeviceAware(recorder: Recorder): recorder is DeviceAwareRecorder {
  return typeof (recorder as Partial<DeviceAwareRecorder>).listDevices === "function"
}

export function isDisposableRecorder(recorder: Recorder): recorder is DisposableRecorder {
  return typeof (recorder as Partial<DisposableRecorder>).dispose === "function"
}

/** Device info of a handle; plain RecorderHandles (no info) report the default source. */
export function recordingDevice(handle: RecorderHandle): RecordingDevice {
  const info = handle as Partial<RecordingDevice>
  return { deviceName: info.deviceName ?? null, deviceFallback: info.deviceFallback === true }
}

/**
 * Picks a capture device for a configured or stored name: an exact name (case-insensitive) first, then the first
 * device whose name contains the query (5.5, `voice.device` is a substring).
 */
export function matchCaptureDevice<T extends { name: string }>(devices: readonly T[], query: string): T | null {
  const needle = query.trim().toLowerCase()
  if (needle === "") return null
  return devices.find((d) => d.name.toLowerCase() === needle) ?? devices.find((d) => d.name.toLowerCase().includes(needle)) ?? null
}

export const TIMED_OUT: unique symbol = Symbol("timed out")

/** Resolves with the promise's value, or TIMED_OUT after `ms` on `clock`; never rejects (a rejection resolves too). */
export function settleWithin<T>(promise: Promise<T>, ms: number, clock: Clock): Promise<T | typeof TIMED_OUT | undefined> {
  return new Promise((resolve) => {
    let timer: TimerHandle | null = clock.setTimeout(() => {
      timer = null
      resolve(TIMED_OUT)
    }, ms)
    const done = (value: T | undefined) => {
      if (timer !== null) clock.clearTimeout(timer)
      timer = null
      resolve(value)
    }
    promise.then(done, () => done(undefined))
  })
}

/**
 * Reads a stream to its end, handing every chunk to `onChunk` (a reader loop: the DOM lib types of
 * ReadableStream have no async iterator). Rejects when the stream errors.
 */
export async function readAll<T>(stream: ReadableStream<T>, onChunk: (chunk: T) => void): Promise<void> {
  const reader = stream.getReader()
  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done) return
      onChunk(value)
    }
  } finally {
    reader.releaseLock()
  }
}

/** Calls a recorder callback without letting a listener failure stop the read loop. */
function safely<A extends unknown[]>(fn: ((...args: A) => void) | undefined, ...args: A): void {
  try {
    fn?.(...args)
  } catch {
    // The manager's callbacks never throw by design; a throwing listener must not lose audio.
  }
}

export interface CaptureSessionOptions {
  maxDurationMs: number
  clock: Clock
  onLevel: (level: LevelSample) => void
  onFirstChunk: () => void
}

/**
 * One recording's audio: the PCM buffer (capacity maxDurationMs * 16 samples), 20 ms levels for the meter and
 * the VAD, the loudest frame for the no-signal discard rule, and the WAV at the end.
 */
export class CaptureSession {
  private readonly pcm: PcmBuffer
  private readonly frames: FrameSplitter
  private peakDb = LEVEL_FLOOR_DB
  private firstChunk = false
  private closed = false

  constructor(private readonly opts: CaptureSessionOptions) {
    this.pcm = new PcmBuffer(opts.maxDurationMs)
    this.frames = new FrameSplitter((frame) => this.onFrame(frame))
  }

  get sampleCount(): number {
    return this.pcm.length
  }

  get firstChunkSeen(): boolean {
    return this.firstChunk
  }

  get isClosed(): boolean {
    return this.closed
  }

  /** OpenTUI capture chunks (Float32 in [-1, 1]). */
  appendFloat32(chunk: Float32Array): void {
    if (this.closed || chunk.length === 0) return
    this.markFirstChunk()
    this.frames.push(this.pcm.appendFloat32(chunk))
  }

  /** parec chunks after S16leDecoder. */
  appendS16(samples: Int16Array): void {
    if (this.closed || samples.length === 0) return
    this.markFirstChunk()
    this.frames.push(this.pcm.appendS16(samples))
  }

  /** Encodes the clip; the PCM and the frame scratch buffer are zero-filled. */
  finish(droppedFrames: number): DisposableWavClip {
    const partial = this.frames.partial()
    if (partial !== null) this.peakDb = Math.max(this.peakDb, measureS16(partial, this.opts.clock.now()).peakDb)
    this.closed = true
    this.frames.clear()
    const clip = createWavClip(this.pcm.view(), { peakDb: this.peakDb, droppedFrames })
    this.pcm.zero()
    return clip
  }

  /** Cancel: zero-fills everything recorded so far. */
  discard(): void {
    this.closed = true
    this.frames.clear()
    this.pcm.zero()
  }

  private markFirstChunk(): void {
    if (this.firstChunk) return
    this.firstChunk = true
    safely(this.opts.onFirstChunk)
  }

  private onFrame(frame: Int16Array): void {
    const level = measureS16(frame, this.opts.clock.now())
    if (level.peakDb > this.peakDb) this.peakDb = level.peakDb
    safely(this.opts.onLevel, level)
  }
}
