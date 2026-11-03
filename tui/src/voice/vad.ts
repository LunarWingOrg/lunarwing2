/**
 * Energy VAD for the opt-in toggle auto-stop, and the signal-based local discard rule (design 5.5, UX-02).
 *
 * The VAD never decides whether a clip is sent: a clip is discarded locally only when it is under 300 ms ("too
 * short") or when its loudest frame stayed below -50 dBFS ("no signal"); everything else goes to a source, and an
 * empty server transcript means no_speech. The VAD only emits `silence` for `auto_stop_on_silence`.
 */
import { FRAME_MS } from "./wav.ts"

/** Frames above this level count as signal; a clip that never reaches it is discarded as "no signal". */
export const NO_SIGNAL_DB = -50
export const MIN_CLIP_MS = 300

export const VAD_TUNING = Object.freeze({
  calibrationFrames: 15,
  floorPercentile: 10,
  floorMinDb: -80,
  floorMaxDb: -35,
  /** The floor adapts only to frames below floor + this. */
  adaptMarginDb: 6,
  adaptWeight: 0.05,
  /** A frame is speech above max(floor + this, NO_SIGNAL_DB). */
  speechMarginDb: 12,
  window: 10,
  speechFrames: 5,
})

export type VadEvent = "speech" | "silence"

export interface VadOptions {
  /** Non-speech after speech before `silence` (voice.silence_stop_ms, default 3500). */
  silenceStopMs: number
  frameMs?: number
}

/** Nearest-rank percentile; 0 for an empty list. */
export function percentile(values: readonly number[], p: number): number {
  if (values.length === 0) return 0
  const sorted = [...values].sort((a, b) => a - b)
  const rank = Math.ceil((p / 100) * sorted.length)
  return sorted[Math.min(sorted.length - 1, Math.max(0, rank - 1))] ?? 0
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value))
}

/**
 * Noise floor = 10th-percentile RMS of the first 15 frames, clamped to [-80, -35] dBFS, then
 * floor = 0.95*floor + 0.05*rms while rms < floor + 6. Speech when rms > max(floor + 12, -50); speech has started
 * after 5 speech frames among the last 10; after that, `silenceStopMs` of continuous non-speech emits `silence`
 * once. Frames before the floor is known are classified when it is (speech from the first frame is not lost).
 */
export class EnergyVad {
  private readonly frameMs: number
  private calibration: number[] = []
  private floor: number | null = null
  private readonly recent: boolean[] = []
  private started = false
  private quietMs = 0
  private silenceSent = false

  constructor(private readonly opts: VadOptions) {
    this.frameMs = opts.frameMs ?? FRAME_MS
  }

  get floorDb(): number | null {
    return this.floor
  }

  get speechStarted(): boolean {
    return this.started
  }

  /** One 20 ms frame; returns the event it caused, if any. */
  push(rmsDb: number): VadEvent | null {
    if (this.floor !== null) return this.classify(rmsDb)
    this.calibration.push(rmsDb)
    if (this.calibration.length < VAD_TUNING.calibrationFrames) return null
    this.floor = clamp(percentile(this.calibration, VAD_TUNING.floorPercentile), VAD_TUNING.floorMinDb, VAD_TUNING.floorMaxDb)
    const frames = this.calibration
    this.calibration = []
    let event: VadEvent | null = null
    for (const frame of frames) event = this.classify(frame) ?? event
    return event
  }

  private classify(rmsDb: number): VadEvent | null {
    const floor = this.floor ?? VAD_TUNING.floorMaxDb
    const speech = rmsDb > Math.max(floor + VAD_TUNING.speechMarginDb, NO_SIGNAL_DB)
    if (rmsDb < floor + VAD_TUNING.adaptMarginDb) this.floor = (1 - VAD_TUNING.adaptWeight) * floor + VAD_TUNING.adaptWeight * rmsDb
    this.recent.push(speech)
    if (this.recent.length > VAD_TUNING.window) this.recent.shift()
    if (!this.started) {
      if (this.recent.filter(Boolean).length < VAD_TUNING.speechFrames) return null
      this.started = true
      this.quietMs = 0
      return "speech"
    }
    if (speech) {
      this.quietMs = 0
      return null
    }
    this.quietMs += this.frameMs
    if (this.silenceSent || this.quietMs < this.opts.silenceStopMs) return null
    this.silenceSent = true
    return "silence"
  }
}

export type LocalDiscardReason = "too_short" | "no_signal"

/** The local discard rule (5.5): signal and length only, never the VAD. null means "send it". */
export function localDiscardReason(
  clip: { durationMs: number; peakDb: number },
  opts: { minClipMs?: number; noSignalDb?: number } = {},
): LocalDiscardReason | null {
  if (clip.durationMs < (opts.minClipMs ?? MIN_CLIP_MS)) return "too_short"
  if (clip.peakDb < (opts.noSignalDb ?? NO_SIGNAL_DB)) return "no_signal"
  return null
}
