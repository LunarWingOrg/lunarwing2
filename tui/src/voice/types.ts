/**
 * Voice contracts (design 5.2, 5.7, 7.2). Declared by the scaffold task so state/model.ts, the reducer and the
 * controllers compile before voice/manager.ts and voice/ptt.ts exist (F6). Implementations import these types;
 * they do not redeclare them (voice/errors.ts re-exports VoiceError rather than defining a second class).
 */

export type VoiceSourceId = "lunarvoice" | "hex-app" | "hex-embedded"

/** Every source id in the default fallback order (5.4). Hex ids are skipped in builds without them (M1a). */
export const VOICE_SOURCE_IDS: readonly VoiceSourceId[] = ["lunarvoice", "hex-app", "hex-embedded"]

export function isVoiceSourceId(value: unknown): value is VoiceSourceId {
  return typeof value === "string" && (VOICE_SOURCE_IDS as readonly string[]).includes(value)
}

/** One 20 ms analysis frame (voice/levels.ts): -90 dBFS floor. */
export interface LevelSample {
  rmsDb: number
  peakDb: number
  at: number
}

/** A recorded clip, only ever in memory (5.6). */
export interface WavClip {
  /** 44-byte RIFF/PCM header followed by s16le mono samples. */
  readonly wav: Uint8Array
  readonly sampleRate: 16000
  readonly durationMs: number
  /** Loudest 20 ms frame of the whole clip (5.5): below -50 dBFS the clip is discarded as "no signal". */
  readonly peakDb: number
  readonly droppedFrames: number
  /** Zero-fills the WAV and PCM buffers; the clip is unusable afterwards. */
  dispose(): void
}

export interface Transcript {
  text: string
  source: VoiceSourceId
  model?: string
  language?: string
  audioMs?: number
  elapsedMs: number
}

export interface SourceAvailability {
  available: boolean
  /** Short and user facing, e.g. "not connected", "Hex not running". */
  reason?: string
  /** Model, endpoint or rate window. Never a token. */
  detail?: string
  checkedAt: number
}

/** A transcription backend fed with a WAV the TUI recorded (every source in this release, 5.1). */
export interface VoiceSource {
  readonly id: VoiceSourceId
  readonly label: string
  /** Shown on first use and in the privacy panel (5.6). */
  readonly privacyNote: string
  /** Side-effect free (no model downloads, no recording); the manager caches the result (5.4). */
  probe(signal: AbortSignal): Promise<SourceAvailability>
  /** Largest WAV (bytes, header included) this source accepts; 0 when it accepts none. */
  maxWavBytes(): number
  /** Rejects with VoiceError; an empty transcript is VoiceError("no_speech"). */
  transcribe(clip: WavClip, signal: AbortSignal): Promise<Transcript>
  dispose(): Promise<void>
}

export type VoiceErrorKind =
  | "unavailable"
  | "busy"
  | "rate_limited"
  | "timeout"
  | "no_speech"
  | "too_large"
  | "unsupported_audio"
  | "auth"
  | "bad_request"
  | "model_not_ready"
  | "processing_failed"
  | "cancelled"
  | "recorder"
  | "internal"

export interface VoiceErrorOptions {
  source?: VoiceSourceId | "recorder"
  status?: number
  code?: string
  retryAfterMs?: number
}

/** Messages are built from status codes and server error codes only; never a token, URL query or audio. */
export class VoiceError extends Error {
  override readonly name = "VoiceError"

  constructor(
    readonly kind: VoiceErrorKind,
    message: string,
    readonly opts: VoiceErrorOptions = {},
  ) {
    super(message)
  }
}

export function isVoiceError(value: unknown): value is VoiceError {
  return value instanceof VoiceError
}

export type RecorderKind = "opentui" | "parec"

export interface RecorderStartOptions {
  maxDurationMs: number
  /** Case-insensitive device name (5.5); undefined = the default source. */
  device?: string
  /** Called once per 20 ms chunk. */
  onLevel: (level: LevelSample) => void
  /** The first captured chunk arrived: the mic is live (emits the PTT event capture_started). */
  onFirstChunk: () => void
  signal: AbortSignal
}

export interface Recorder {
  readonly kind: RecorderKind
  available(): Promise<{ ok: true } | { ok: false; reason: string }>
  start(opts: RecorderStartOptions): Promise<RecorderHandle>
}

export interface RecorderHandle {
  readonly kind: RecorderKind
  /** Drains the capture and encodes the clip. */
  stop(): Promise<WavClip>
  /** Discards the capture immediately; zero-fills what was recorded. */
  cancel(): void
}

// Push-to-talk phase and mode live here (not in voice/ptt.ts) so the store compiles first (F6, 7.2).
export type PttPhase = "idle" | "arming" | "recording" | "finishing"
export type PttMode = "provisional" | "toggle" | "hold-key" | "hold-inferred" | "hold-mouse"

export interface VoiceQueueItem {
  id: string
  durationMs: number
  state: "waiting" | "transcribing"
  source: VoiceSourceId | null
  startedAt: number | null
  attempt: number
}

/** VU meter state computed by voice/levels.ts and dispatched (throttled) with voice/level. */
export interface LevelMeter {
  rmsDb: number
  peakDb: number
  /** Peak held for 1 s, then decaying. */
  heldPeakDb: number
  /** A frame reached -1 dBFS or more. */
  clipping: boolean
  /** 1.5 s below -70 dBFS ("no signal from <device>"). */
  noSignal: boolean
}

export interface VoiceAttempt {
  source: VoiceSourceId
  outcome: VoiceErrorKind | "ok"
  ms: number
}

/** Metadata of the one retained failed clip (5.4.1); the clip itself stays in the controller, never in the store. */
export interface RetainedClipInfo {
  durationMs: number
  reason: VoiceErrorKind
  /** The clip is dropped at this time (10 min after the failure). */
  until: number
  /** rate_limited only: Alt+R before this time shows "wait N s". */
  retryAt: number | null
}

export interface VoiceUiState {
  phase: PttPhase
  mode: PttMode | null
  trigger: "key" | "mouse" | null
  recorder: RecorderKind | null
  startedAt: number | null
  /** The first chunk arrived ("opening mic..." until then). */
  captureLive: boolean
  level: LevelMeter | null
  /** At most 3: one transcribing, two waiting (5.4). */
  queue: readonly VoiceQueueItem[]
  retained: RetainedClipInfo | null
  availability: Partial<Record<VoiceSourceId, SourceAvailability>>
  lastAttempts: readonly VoiceAttempt[]
  lastError: { kind: VoiceErrorKind; message: string } | null
}

export function createInitialVoiceUiState(): VoiceUiState {
  return {
    phase: "idle",
    mode: null,
    trigger: null,
    recorder: null,
    startedAt: null,
    captureLive: false,
    level: null,
    queue: [],
    retained: null,
    availability: {},
    lastAttempts: [],
    lastError: null,
  }
}

export interface CaptureDevice {
  index: number
  name: string
  isDefault: boolean
}

/** One row of the voice panel (3.12.3, 5.4). */
export interface VoiceSourceStatus {
  id: VoiceSourceId
  label: string
  privacyNote: string
  /** False for ids without an implementation in this build ("not in this build", 5.4). */
  inBuild: boolean
  /** Cached probe result; null before the first probe. */
  availability: SourceAvailability | null
  /** maxWavBytes() of an in-build source, else null. */
  maxWavBytes: number | null
}

/** Returned by VoiceController.status() for /voice and the voice panel. */
export interface VoiceStatusReport {
  /** False with --no-voice. */
  enabled: boolean
  order: readonly VoiceSourceId[]
  /** Set by /voice use <id>; null in automatic order. */
  pinned: VoiceSourceId | null
  sources: readonly VoiceSourceStatus[]
  recorder: { kind: RecorderKind | null; reason?: string }
  device: { name: string | null; origin: "state" | "config" | "default" }
  retained: RetainedClipInfo | null
  lastAttempts: readonly VoiceAttempt[]
  /** Effective cap: min(voice.max_duration_s, 290 s, the largest maxWavBytes) (5.5). */
  maxDurationMs: number
}

/**
 * Implemented by voice/manager.ts createVoiceController (5.7). Declared here so the keymap, the mic widget and the
 * panels compile before the manager exists; manager.ts should re-export this type instead of redeclaring it.
 */
export interface VoiceController {
  pttKey(e: { eventType: "press" | "release"; repeated: boolean }): void
  micDown(): void
  micUp(): void
  /** True if it cancelled the recording, the transcription queue, or dropped the retained clip (Esc consumed). */
  escape(): boolean
  /** Alt+R and /voice retry (5.4.1); false when there is nothing to retry. */
  retry(): boolean
  blur(): void
  focus(): void
  /** /voice and reconnects: probe every source now. */
  probeNow(): void
  status(): VoiceStatusReport
  use(id: VoiceSourceId | "auto"): void
  setOrder(ids: VoiceSourceId[]): void
  listDevices(): Promise<CaptureDevice[]>
  /** Persists voiceDevice in the profile state file; false when no device matches. */
  selectDevice(name: string): Promise<boolean>
  dispose(): Promise<void>
}
