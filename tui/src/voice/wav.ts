/**
 * In-process PCM and WAV (design 5.5, 5.6). Audio is 16 kHz mono signed 16-bit little-endian and exists only
 * in memory: a preallocated PCM buffer per recording, then one WAV byte array per clip. Nothing is written to
 * disk (AudioRecorder.recordToFile is not used), and every buffer is zero-filled once it is no longer needed.
 */
import type { WavClip } from "./types.ts"

export const SAMPLE_RATE = 16_000
export const SAMPLES_PER_MS = 16
export const WAV_HEADER_BYTES = 44
/** 16 000 samples per second, 2 bytes each. */
export const WAV_BYTES_PER_SECOND = 32_000
/** One 20 ms analysis frame (levels, VAD) at 16 kHz. */
export const FRAME_SAMPLES = 320
export const FRAME_MS = 20

/** Clamps to [-1, 1] and scales asymmetrically, so -1 maps to -32768 and 1 to 32767 (5.5). NaN maps to 0. */
export function floatToS16(sample: number): number {
  if (Number.isNaN(sample)) return 0
  const clamped = sample < -1 ? -1 : sample > 1 ? 1 : sample
  return Math.round(clamped < 0 ? clamped * 32_768 : clamped * 32_767)
}

/**
 * The preallocated PCM buffer of one recording: `Int16Array(maxDurationMs * 16)`. Samples beyond the capacity
 * are not stored (the push-to-talk machine stops the recording at the same duration); they are only counted.
 */
export class PcmBuffer {
  private readonly samples: Int16Array
  private used = 0
  private truncated = 0

  constructor(maxDurationMs: number) {
    const ms = Number.isFinite(maxDurationMs) ? Math.max(0, Math.floor(maxDurationMs)) : 0
    this.samples = new Int16Array(ms * SAMPLES_PER_MS)
  }

  get length(): number {
    return this.used
  }

  get capacity(): number {
    return this.samples.length
  }

  get full(): boolean {
    return this.used >= this.samples.length
  }

  /** Samples that arrived after the buffer was full. */
  get truncatedSamples(): number {
    return this.truncated
  }

  /** Converts and stores Float32 samples; returns a view of the samples actually stored by this call. */
  appendFloat32(chunk: Float32Array): Int16Array {
    const start = this.used
    const room = Math.min(chunk.length, this.samples.length - start)
    for (let i = 0; i < room; i += 1) this.samples[start + i] = floatToS16(chunk[i] ?? 0)
    this.used += room
    this.truncated += chunk.length - room
    return this.samples.subarray(start, this.used)
  }

  /** Stores s16 samples; returns a view of the samples actually stored by this call. */
  appendS16(chunk: Int16Array): Int16Array {
    const start = this.used
    const room = Math.min(chunk.length, this.samples.length - start)
    this.samples.set(chunk.subarray(0, room), start)
    this.used += room
    this.truncated += chunk.length - room
    return this.samples.subarray(start, this.used)
  }

  /** The recorded samples (a view, not a copy). */
  view(): Int16Array {
    return this.samples.subarray(0, this.used)
  }

  zero(): void {
    this.samples.fill(0)
    this.used = 0
  }
}

function writeAscii(view: DataView, offset: number, text: string): void {
  for (let i = 0; i < text.length; i += 1) view.setUint8(offset + i, text.charCodeAt(i))
}

/**
 * The canonical 44-byte RIFF/PCM header (RIFF, 36 + N, WAVE, "fmt ", 16, PCM 1, 1 channel, `sampleRate`,
 * byte rate, block align 2, 16 bits, data, N) followed by the samples, little-endian through a DataView.
 */
export function encodeWav(pcm: Int16Array, sampleRate: number = SAMPLE_RATE): Uint8Array {
  const dataBytes = pcm.length * 2
  const out = new Uint8Array(WAV_HEADER_BYTES + dataBytes)
  const view = new DataView(out.buffer, out.byteOffset, out.byteLength)
  writeAscii(view, 0, "RIFF")
  view.setUint32(4, 36 + dataBytes, true)
  writeAscii(view, 8, "WAVE")
  writeAscii(view, 12, "fmt ")
  view.setUint32(16, 16, true)
  view.setUint16(20, 1, true)
  view.setUint16(22, 1, true)
  view.setUint32(24, sampleRate, true)
  view.setUint32(28, sampleRate * 2, true)
  view.setUint16(32, 2, true)
  view.setUint16(34, 16, true)
  writeAscii(view, 36, "data")
  view.setUint32(40, dataBytes, true)
  for (let i = 0; i < pcm.length; i += 1) view.setInt16(WAV_HEADER_BYTES + i * 2, pcm[i] ?? 0, true)
  return out
}

export interface ParsedWav {
  sampleRate: number
  channels: number
  bitsPerSample: number
  byteRate: number
  blockAlign: number
  samples: Int16Array
}

function readAscii(view: DataView, offset: number, length: number): string {
  let text = ""
  for (let i = 0; i < length; i += 1) text += String.fromCharCode(view.getUint8(offset + i))
  return text
}

/** Parses the canonical layout encodeWav writes (round-trip checks); anything else throws RangeError. */
export function parseWav(bytes: Uint8Array): ParsedWav {
  if (bytes.length < WAV_HEADER_BYTES) throw new RangeError("not a WAV file: shorter than its header")
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  const layout = [readAscii(view, 0, 4), readAscii(view, 8, 4), readAscii(view, 12, 4), readAscii(view, 36, 4)]
  if (layout.join(",") !== "RIFF,WAVE,fmt ,data") throw new RangeError("not a canonical PCM WAV file")
  if (view.getUint32(16, true) !== 16 || view.getUint16(20, true) !== 1) throw new RangeError("not 16-byte PCM format")
  const dataBytes = view.getUint32(40, true)
  if (view.getUint32(4, true) !== 36 + dataBytes || WAV_HEADER_BYTES + dataBytes > bytes.length || dataBytes % 2 !== 0) {
    throw new RangeError("WAV sizes do not match the data")
  }
  const samples = new Int16Array(dataBytes / 2)
  for (let i = 0; i < samples.length; i += 1) samples[i] = view.getInt16(WAV_HEADER_BYTES + i * 2, true)
  return {
    sampleRate: view.getUint32(24, true),
    channels: view.getUint16(22, true),
    bitsPerSample: view.getUint16(34, true),
    byteRate: view.getUint32(28, true),
    blockAlign: view.getUint16(32, true),
    samples,
  }
}

export interface ClipStats {
  /** Loudest 20 ms frame of the clip, dBFS (5.5). */
  peakDb: number
  /** Frames the capture ring dropped while the UI was busy. */
  droppedFrames: number
}

/** A WavClip whose dispose() state tests and the manager can read. */
export interface DisposableWavClip extends WavClip {
  readonly disposed: boolean
}

/**
 * Encodes `pcm` into a clip. The PCM samples are zero-filled right after encoding (only the WAV is kept), and
 * dispose() zero-fills the WAV and the PCM view again, so a disposed clip holds no audio.
 */
export function createWavClip(pcm: Int16Array, stats: ClipStats): DisposableWavClip {
  const wav = encodeWav(pcm)
  const durationMs = Math.round(pcm.length / SAMPLES_PER_MS)
  pcm.fill(0)
  let disposed = false
  return {
    wav,
    sampleRate: SAMPLE_RATE,
    durationMs,
    peakDb: stats.peakDb,
    droppedFrames: Math.max(0, Math.floor(stats.droppedFrames)),
    get disposed() {
      return disposed
    },
    dispose() {
      wav.fill(0)
      pcm.fill(0)
      disposed = true
    },
  }
}

/** The longest recording a source accepts: floor((maxWavBytes - 44) / 32000) seconds (5.5), in ms. */
export function maxDurationForWavBytes(maxWavBytes: number): number {
  if (!Number.isFinite(maxWavBytes) || maxWavBytes <= WAV_HEADER_BYTES) return 0
  return Math.floor((maxWavBytes - WAV_HEADER_BYTES) / WAV_BYTES_PER_SECOND) * 1000
}

/** WAV size of a recording of `durationMs`. */
export function wavBytesForDuration(durationMs: number): number {
  return WAV_HEADER_BYTES + Math.max(0, Math.floor(durationMs)) * SAMPLES_PER_MS * 2
}
