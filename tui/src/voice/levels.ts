/**
 * Levels and VU-meter data (design 5.5): per 20 ms frame rmsDb = 20*log10(rms) and peakDb = 20*log10(peak) with a
 * -90 dBFS floor; the meter model adds instant attack, about 25 dB/s release, a 1 s peak hold, the clip marker at
 * -1 dBFS or above and "no signal" after 1.5 s below -70 dBFS. components/vu-meter.tsx draws the LevelMeter this
 * module produces (-60..0 dBFS in 12 cells); the manager dispatches it throttled (15 Hz, 4 Hz with reduce_motion).
 */
import type { LevelMeter, LevelSample } from "./types.ts"
import { FRAME_SAMPLES } from "./wav.ts"

export const LEVEL_FLOOR_DB = -90

export function linearToDb(value: number): number {
  if (!(value > 0)) return LEVEL_FLOOR_DB
  return Math.max(LEVEL_FLOOR_DB, 20 * Math.log10(value))
}

/** rmsDb and peakDb of s16 samples (full scale 32768). */
export function measureS16(samples: Int16Array, at: number): LevelSample {
  if (samples.length === 0) return { rmsDb: LEVEL_FLOOR_DB, peakDb: LEVEL_FLOOR_DB, at }
  let sum = 0
  let peak = 0
  for (let i = 0; i < samples.length; i += 1) {
    const value = (samples[i] ?? 0) / 32_768
    sum += value * value
    const magnitude = Math.abs(value)
    if (magnitude > peak) peak = magnitude
  }
  return { rmsDb: linearToDb(Math.sqrt(sum / samples.length)), peakDb: linearToDb(peak), at }
}

/**
 * Raw s16le bytes (parec stdout) to samples. Pipe chunks can split a sample: an odd trailing byte is carried
 * into the next chunk. Little-endian regardless of the host, through a DataView.
 */
export class S16leDecoder {
  private carry: number | null = null

  /** Bytes held for the next chunk (0 or 1). */
  get pending(): number {
    return this.carry === null ? 0 : 1
  }

  push(chunk: Uint8Array): Int16Array {
    if (chunk.length === 0) return new Int16Array(0)
    const total = chunk.length + this.pending
    const out = new Int16Array(total >> 1)
    let offset = 0
    let index = 0
    if (this.carry !== null) {
      const low = this.carry
      const high = chunk[0] ?? 0
      out[index] = ((high << 8) | low) << 16 >> 16
      index += 1
      offset = 1
      this.carry = null
    }
    const view = new DataView(chunk.buffer, chunk.byteOffset, chunk.byteLength)
    for (; offset + 1 < chunk.length; offset += 2) {
      out[index] = view.getInt16(offset, true)
      index += 1
    }
    if (offset < chunk.length) this.carry = chunk[offset] ?? 0
    return out
  }

  reset(): void {
    this.carry = null
  }
}

/**
 * Cuts a stream of sample chunks of any size into 20 ms frames. The frame scratch buffer holds audio, so it is
 * zero-filled by clear().
 */
export class FrameSplitter {
  private readonly frame = new Int16Array(FRAME_SAMPLES)
  private fill = 0

  constructor(private readonly onFrame: (frame: Int16Array) => void) {}

  push(samples: Int16Array): void {
    let offset = 0
    while (offset < samples.length) {
      const take = Math.min(FRAME_SAMPLES - this.fill, samples.length - offset)
      this.frame.set(samples.subarray(offset, offset + take), this.fill)
      this.fill += take
      offset += take
      if (this.fill === FRAME_SAMPLES) {
        this.onFrame(this.frame)
        this.fill = 0
      }
    }
  }

  /** The incomplete last frame (a view), or null when the stream ended on a frame boundary. */
  partial(): Int16Array | null {
    return this.fill === 0 ? null : this.frame.subarray(0, this.fill)
  }

  clear(): void {
    this.frame.fill(0)
    this.fill = 0
  }
}

export interface LevelMeterOptions {
  releaseDbPerSecond: number
  peakHoldMs: number
  /** A frame peak at or above this marks clipping. */
  clipDb: number
  noSignalDb: number
  noSignalMs: number
}

export const DEFAULT_LEVEL_METER_OPTIONS: Readonly<LevelMeterOptions> = Object.freeze({
  releaseDbPerSecond: 25,
  peakHoldMs: 1_000,
  clipDb: -1,
  noSignalDb: -70,
  noSignalMs: 1_500,
})

/** Meter dynamics over the 20 ms LevelSamples of one recording. */
export class LevelMeterModel {
  private readonly opts: LevelMeterOptions
  private rmsDb = LEVEL_FLOOR_DB
  private peakDb = LEVEL_FLOOR_DB
  private heldDb = LEVEL_FLOOR_DB
  private heldAt = Number.NEGATIVE_INFINITY
  private lastAt: number | null = null
  private quietSince: number | null = null
  private clipAt = Number.NEGATIVE_INFINITY

  constructor(options: Partial<LevelMeterOptions> = {}) {
    this.opts = { ...DEFAULT_LEVEL_METER_OPTIONS, ...options }
  }

  update(sample: LevelSample): LevelMeter {
    const elapsed = this.lastAt === null ? 0 : Math.max(0, sample.at - this.lastAt)
    this.lastAt = sample.at
    const release = (this.opts.releaseDbPerSecond * elapsed) / 1_000
    this.rmsDb = Math.max(sample.rmsDb, this.rmsDb - release, LEVEL_FLOOR_DB)
    this.peakDb = Math.max(sample.peakDb, this.peakDb - release, LEVEL_FLOOR_DB)
    if (sample.peakDb >= this.heldValue(sample.at)) {
      this.heldDb = sample.peakDb
      this.heldAt = sample.at
    }
    if (sample.peakDb >= this.opts.clipDb) this.clipAt = sample.at
    if (sample.rmsDb < this.opts.noSignalDb) this.quietSince ??= sample.at
    else this.quietSince = null
    return this.read(sample.at)
  }

  /** The meter at `at` (the held peak keeps decaying between samples). */
  read(at: number): LevelMeter {
    return {
      rmsDb: this.rmsDb,
      peakDb: this.peakDb,
      heldPeakDb: Math.max(this.heldValue(at), this.peakDb),
      clipping: at - this.clipAt <= this.opts.peakHoldMs,
      noSignal: this.quietSince !== null && at - this.quietSince >= this.opts.noSignalMs,
    }
  }

  private heldValue(at: number): number {
    const sinceHold = at - this.heldAt - this.opts.peakHoldMs
    if (sinceHold <= 0) return this.heldDb
    return Math.max(LEVEL_FLOOR_DB, this.heldDb - (this.opts.releaseDbPerSecond * sinceHold) / 1_000)
  }
}
