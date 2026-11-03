/**
 * The VoiceController (design 5.4, 5.4.1, 5.5, 5.7 and section 7).
 *
 * - Push-to-talk: every key, mouse, focus and recorder event, plus a 50 ms Clock tick while not idle, runs
 *   through pttReduce (voice/ptt.ts); this module executes the effects.
 * - Start never waits for a probe: the cached availability decides whether a recording may start, then the first
 *   recorder that starts wins (OpenTUI Audio, else parec). "opening mic..." lasts until the first chunk.
 * - Finished clips enter a queue of at most 3 (one transcribing, two waiting) and a new recording may start at
 *   once. The worker transcribes them one at a time in order and inserts each transcript at the composer cursor.
 * - Per clip, the candidates are tried in order with the same WAV and a 180 s budget; the error kind decides the
 *   next step (voice/errors.ts FALLBACK_STEPS); busy is retried after 1, 2 and 4 s.
 * - A clip no source could transcribe is retained in memory (never in the store or on disk) for Alt+R, until
 *   success, Esc, a new recording, quit or 10 min.
 * - Sources are probed in the background (1 s budget each): at creation, when the connection changes, every 30 s,
 *   on /voice, and when a source hints (AvailabilityHintSource, the M1b Hex discovery watch).
 *
 * The store sees only VoiceUiState metadata (voice/state, throttled voice/level) and notices; audio never enters
 * it. Dictation is refused in credential mode, and a recording running when credential mode starts is cancelled.
 *
 * VoiceUiState.queue item semantics (for the status line, 7.4): state "transcribing" = a request is in flight to
 * `source` since `startedAt`, `attempt` = busy retries so far; state "waiting" with a `source` and attempt > 0 = a
 * busy retry of `source` fires at `startedAt` ("lunarvoice busy - retry 2/3 in 2 s"); state "waiting" with
 * source null = queued behind the head.
 */
import type { Clock, TimerHandle } from "@opentui/core"
import { VOICE_MAX_DURATION_CAP_MS, type ProfileState, type StateFile, type VoiceConfig } from "../config/types.ts"
import type { ComposerHandle } from "../controller/types.ts"
import type { DebugLog } from "../log.ts"
import { sanitizeLine } from "../sanitize.ts"
import type { NoticeLevel, Store } from "../state/model.ts"
import {
  abortReason,
  BUSY_RETRY_DELAYS_MS,
  cancelledError,
  describeVoiceErrorKind,
  fallbackStep,
  toVoiceError,
  VoiceError,
} from "./errors.ts"
import { LevelMeterModel } from "./levels.ts"
import { createPttState, pttConfigFromVoice, pttReduce, type PttCancelReason, type PttConfig, type PttEffect, type PttEvent, type PttState } from "./ptt.ts"
import {
  isDeviceAware,
  isDisposableRecorder,
  matchCaptureDevice,
  recordingDevice,
  type CaptureStartOptions,
  type RecordingDevice,
} from "./recorder.ts"
import {
  isVoiceSourceId,
  VOICE_SOURCE_IDS,
  type CaptureDevice,
  type LevelSample,
  type Recorder,
  type RecorderHandle,
  type RetainedClipInfo,
  type SourceAvailability,
  type Transcript,
  type VoiceAttempt,
  type VoiceController,
  type VoiceErrorKind,
  type VoiceQueueItem,
  type VoiceSource,
  type VoiceSourceId,
  type VoiceSourceStatus,
  type VoiceStatusReport,
  type VoiceUiState,
  type WavClip,
} from "./types.ts"
import { EnergyVad, localDiscardReason, NO_SIGNAL_DB, type LocalDiscardReason } from "./vad.ts"
import { maxDurationForWavBytes } from "./wav.ts"

export type { VoiceController, VoiceStatusReport } from "./types.ts"

export const VOICE_TIMING = Object.freeze({
  /** pttReduce tick while not idle. */
  tickMs: 50,
  probeBudgetMs: 1_000,
  probeIntervalMs: 30_000,
  /** Availability older than this is re-probed before a clip is sent. */
  probeStaleMs: 30_000,
  clipBudgetMs: 180_000,
  retainMs: 10 * 60_000,
  /** No first chunk this long after a recorder started: the next recorder is tried. */
  firstChunkTimeoutMs: 5_000,
  /** voice/level throttle: 15 Hz, 4 Hz with reduce_motion. */
  levelIntervalMs: 1_000 / 15,
  reducedLevelIntervalMs: 250,
})

/** One transcribing and two waiting (5.4). */
export const MAX_QUEUED_CLIPS = 3

export const HEX_MODEL_HINT = "Hex has no verified model: run `hex model install parakeet_unified_en --language en`"

/** Notice ids: a newer notice of the same kind replaces the older one. */
export const VOICE_NOTICE_IDS = Object.freeze({
  result: "voice-result",
  ptt: "voice-ptt",
  unavailable: "voice-unavailable",
  auth: "voice-auth",
  modelHint: "voice-model-hint",
  dropped: "voice-dropped",
  device: "voice-device",
  config: "voice-config",
})

const SOURCE_LABELS: Readonly<Record<VoiceSourceId, string>> = Object.freeze({
  lunarvoice: "LunarVoice",
  "hex-app": "Hex (running app)",
  "hex-embedded": "Hex (embedded service)",
})

export interface VoiceControllerDeps {
  store: Store
  clock: Clock
  /** The wall clock (epoch ms) for the voiceAck stamps; default Date.now (the Clock is not epoch in production). */
  wallNow?: () => number
  composer: ComposerHandle
  /** Fallback order, normally [OpenTUI Audio, parec]; empty with --no-voice. */
  recorders: Recorder[]
  sources: VoiceSource[]
  config: VoiceConfig
  stateFile: StateFile
  /** Metadata-only debug log (log.ts): sources, outcomes, durations and sizes; never transcripts or audio. */
  log?: DebugLog
}

/**
 * Optional source capability: the source calls `listener` when a re-probe is worthwhile (M1b: the Hex discovery
 * file's mtime changed). Returns the unsubscribe function.
 */
export interface AvailabilityHintSource extends VoiceSource {
  onAvailabilityHint(listener: () => void): () => void
}

function hasAvailabilityHint(source: VoiceSource): source is AvailabilityHintSource {
  return typeof (source as Partial<AvailabilityHintSource>).onAvailabilityHint === "function"
}

// ---------------------------------------------------------------------------------------------------------
// Shared context and async helpers
// ---------------------------------------------------------------------------------------------------------

interface Ctx {
  readonly store: Store
  readonly clock: Clock
  readonly log: DebugLog | null
  alive(): boolean
  now(): number
  patch(patch: Partial<VoiceUiState>): void
  notice(level: NoticeLevel, text: string, opts?: { id?: string; sticky?: boolean }): void
  dismiss(id: string): void
}

function createCtx(store: Store, clock: Clock, log: DebugLog | null, alive: () => boolean): Ctx {
  return {
    store,
    clock,
    log,
    alive,
    now: () => clock.now(),
    patch: (patch) => {
      if (alive()) store.dispatch({ type: "voice/state", patch, at: clock.now() })
    },
    notice: (level, text, opts = {}) => {
      if (!alive()) return
      const notice = { level, source: "voice" as const, text, ...(opts.id ? { id: opts.id } : {}), ...(opts.sticky === undefined ? {} : { sticky: opts.sticky }) }
      store.dispatch({ type: "notice/push", notice, at: clock.now() })
    },
    dismiss: (id) => {
      if (alive()) store.dispatch({ type: "notice/dismiss", id, at: clock.now() })
    },
  }
}

/** Resolves after `ms` on `clock`; rejects with the abort reason as soon as `signal` aborts. */
function sleep(clock: Clock, ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(abortReason(signal))
      return
    }
    const onAbort = () => {
      clock.clearTimeout(timer)
      reject(abortReason(signal))
    }
    const timer = clock.setTimeout(() => {
      signal.removeEventListener("abort", onAbort)
      resolve()
    }, ms)
    signal.addEventListener("abort", onAbort, { once: true })
  })
}

/** Settles with `promise`, or rejects with the abort reason once `signal` aborts (a late result is dropped). */
function raceAbort<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  return new Promise((resolve, reject) => {
    const onAbort = () => reject(abortReason(signal))
    if (signal.aborted) onAbort()
    else signal.addEventListener("abort", onAbort, { once: true })
    promise.then(
      (value) => {
        signal.removeEventListener("abort", onAbort)
        resolve(value)
      },
      (error: unknown) => {
        signal.removeEventListener("abort", onAbort)
        reject(error)
      },
    )
  })
}

function wholeSeconds(ms: number): string {
  return `${Math.max(1, Math.round(ms / 1000))} s`
}

function tenthSeconds(ms: number): string {
  return `${(Math.max(0, ms) / 1000).toFixed(1)} s`
}

function cleanText(text: string, max: number): string {
  const line = sanitizeLine(text).trim()
  return line.length > max ? `${line.slice(0, max - 1)}…` : line
}

function normalizeOrder(ids: readonly unknown[]): VoiceSourceId[] {
  const order: VoiceSourceId[] = []
  for (const id of ids) if (isVoiceSourceId(id) && !order.includes(id)) order.push(id)
  return order
}

// ---------------------------------------------------------------------------------------------------------
// Availability: background probes of sources and recorders (5.4)
// ---------------------------------------------------------------------------------------------------------

type RecorderCheck = { ok: true } | { ok: false; reason: string }

class Availability {
  private readonly cache = new Map<VoiceSourceId, SourceAvailability>()
  private readonly inflight = new Map<VoiceSourceId, Promise<SourceAvailability>>()
  private readonly recorderState = new Map<Recorder, RecorderCheck>()
  private readonly recorderChecks = new Set<Recorder>()

  constructor(
    private readonly ctx: Ctx,
    private readonly sources: readonly VoiceSource[],
    private readonly recorders: readonly Recorder[],
  ) {}

  get(id: VoiceSourceId): SourceAvailability | undefined {
    return this.cache.get(id)
  }

  probeAll(): void {
    for (const source of this.sources) void this.probe(source)
  }

  /** One probe at a time per source, 1 s budget; a probe that throws or overruns counts as unavailable. */
  probe(source: VoiceSource): Promise<SourceAvailability> {
    const running = this.inflight.get(source.id)
    if (running) return running
    const task = this.runProbe(source).then((result) => {
      this.inflight.delete(source.id)
      this.store(source.id, result)
      return result
    })
    this.inflight.set(source.id, task)
    return task
  }

  /** The cached availability when younger than 30 s, else a fresh probe (awaited, abortable). */
  async fresh(source: VoiceSource, signal: AbortSignal): Promise<SourceAvailability> {
    const cached = this.cache.get(source.id)
    if (cached !== undefined && this.ctx.now() - cached.checkedAt <= VOICE_TIMING.probeStaleMs) return cached
    return raceAbort(this.probe(source), signal)
  }

  /** After an `unavailable` attempt: skipped until the next probe (at most 30 s later). */
  markUnavailable(source: VoiceSource, reason: string): void {
    this.store(source.id, { available: false, reason, checkedAt: this.ctx.now() })
  }

  /** Reasons per source when every one of them is known to be unavailable, else null (unknown counts as maybe). */
  unavailableReasons(sources: readonly VoiceSource[]): string | null {
    const parts: string[] = []
    for (const source of sources) {
      const known = this.cache.get(source.id)
      if (known === undefined || known.available) return null
      parts.push(`${source.id} ${known.reason ?? "unavailable"}`)
    }
    return parts.join("; ")
  }

  checkRecorders(): void {
    for (const recorder of this.recorders) {
      if (this.recorderChecks.has(recorder)) continue
      this.recorderChecks.add(recorder)
      void Promise.resolve()
        .then(() => recorder.available())
        .then(
          (result): RecorderCheck => (result.ok ? { ok: true } : { ok: false, reason: cleanText(result.reason, 120) }),
          (): RecorderCheck => ({ ok: false, reason: "the recorder check failed" }),
        )
        .then((result) => {
          this.recorderChecks.delete(recorder)
          this.recorderState.set(recorder, result)
        })
    }
  }

  recorderKnownBad(recorder: Recorder): boolean {
    return this.recorderState.get(recorder)?.ok === false
  }

  /** Reasons when no recorder can work, else null. */
  recorderReasons(): string | null {
    if (this.recorders.length === 0) return "none configured"
    const parts: string[] = []
    for (const recorder of this.recorders) {
      const state = this.recorderState.get(recorder)
      if (state === undefined || state.ok) return null
      parts.push(`${recorder.kind}: ${state.reason}`)
    }
    return parts.join("; ")
  }

  recorderStatus(): VoiceStatusReport["recorder"] {
    const working = this.recorders.find((r) => this.recorderState.get(r)?.ok === true)
    if (working !== undefined) return { kind: working.kind }
    if (this.recorders.some((r) => !this.recorderState.has(r))) return { kind: null, reason: "not checked yet" }
    return { kind: null, reason: this.recorderReasons() ?? "unavailable" }
  }

  private async runProbe(source: VoiceSource): Promise<SourceAvailability> {
    const controller = new AbortController()
    const unavailable = (reason: string): SourceAvailability => ({ available: false, reason, checkedAt: this.ctx.now() })
    return new Promise<SourceAvailability>((resolve) => {
      const timer = this.ctx.clock.setTimeout(() => {
        controller.abort()
        resolve(unavailable("no answer within 1 s"))
      }, VOICE_TIMING.probeBudgetMs)
      Promise.resolve()
        .then(() => source.probe(controller.signal))
        .then(
          (result) => {
            this.ctx.clock.clearTimeout(timer)
            resolve(this.clean(result))
          },
          () => {
            this.ctx.clock.clearTimeout(timer)
            resolve(unavailable("the probe failed"))
          },
        )
    })
  }

  /** Sources are trusted code, but reasons can quote a server; checkedAt is always this clock's time. */
  private clean(result: SourceAvailability): SourceAvailability {
    const reason = typeof result.reason === "string" ? cleanText(result.reason, 120) : ""
    const detail = typeof result.detail === "string" ? cleanText(result.detail, 160) : ""
    return {
      available: result.available === true,
      ...(reason ? { reason } : {}),
      ...(detail ? { detail } : {}),
      checkedAt: this.ctx.now(),
    }
  }

  private store(id: VoiceSourceId, result: SourceAvailability): void {
    this.cache.set(id, result)
    this.ctx.patch({ availability: Object.fromEntries(this.cache) })
  }
}

// ---------------------------------------------------------------------------------------------------------
// Transcriber: the clip queue, the fallback chain and the retained clip (5.4, 5.4.1)
// ---------------------------------------------------------------------------------------------------------

interface QueueEntry {
  readonly clip: WavClip
  item: VoiceQueueItem
  abort: AbortController | null
  cancelled: boolean
}

interface Failure {
  source: VoiceSourceId
  error: VoiceError
  retries: number
}

interface ChainState {
  readonly failures: Failure[]
  /** One "<source> <outcome>" part per candidate, in order, for the end-of-chain notice. */
  readonly steps: string[]
  retryAt: number | null
  /** Sources with a limit at or below this already refused the clip as too large. */
  tooLargeAbove: number
  /** The candidate being tried (named when the 180 s budget runs out during its attempt). */
  current: VoiceSourceId | null
}

type ChainResult =
  | { kind: "ok"; transcript: Transcript; failures: readonly Failure[] }
  | { kind: "stopped"; error: VoiceError }
  | { kind: "cancelled" }
  | { kind: "exhausted"; chain: ChainState }

type AttemptResult = { ok: true; transcript: Transcript } | { ok: false; error: VoiceError; retries: number }

interface TranscriberHost {
  candidates(): VoiceSource[]
  readonly availability: Availability
  readonly composer: ComposerHandle
  acknowledge(source: VoiceSource): void
}

interface RetainedClip {
  readonly clip: WavClip
  readonly info: RetainedClipInfo
  readonly timer: TimerHandle
}

function describeFailure(failure: Failure): string {
  const { error, retries } = failure
  if (error.kind === "busy" && retries > 0) return `busy (${retries} ${retries === 1 ? "retry" : "retries"})`
  if (error.kind === "rate_limited" && error.opts.retryAfterMs !== undefined) return `rate limited (wait ${wholeSeconds(error.opts.retryAfterMs)})`
  return describeVoiceErrorKind(error.kind)
}

class Transcriber {
  private queue: QueueEntry[] = []
  private retained: RetainedClip | null = null
  private attempts: VoiceAttempt[] = []
  private working = false
  private disposed = false
  private seq = 0
  private modelHintShown = false

  constructor(
    private readonly ctx: Ctx,
    private readonly host: TranscriberHost,
  ) {}

  get size(): number {
    return this.queue.length
  }

  retainedInfo(): RetainedClipInfo | null {
    return this.retained?.info ?? null
  }

  lastAttempts(): readonly VoiceAttempt[] {
    return this.attempts
  }

  enqueue(clip: WavClip): void {
    if (this.disposed) {
      clip.dispose()
      return
    }
    this.seq += 1
    const item: VoiceQueueItem = { id: `clip-${this.seq}`, durationMs: clip.durationMs, state: "waiting", source: null, startedAt: null, attempt: 0 }
    this.queue.push({ clip, item, abort: null, cancelled: false })
    this.publishQueue()
    void this.pump()
  }

  /** Esc while idle (3.10.4): aborts the in-flight request and zero-fills every queued clip. */
  cancelAll(notice = true): boolean {
    if (this.queue.length === 0) return false
    const entries = this.queue
    this.queue = []
    for (const entry of entries) {
      entry.cancelled = true
      entry.abort?.abort(cancelledError("transcription cancelled"))
      entry.clip.dispose()
    }
    this.publishQueue()
    if (notice) this.ctx.notice("info", "transcription cancelled", { id: VOICE_NOTICE_IDS.ptt })
    return true
  }

  /** Zero-fills and drops the retained clip; `why` picks the notice (null: silent). */
  dropRetained(why: "escape" | "new_recording" | "expired" | null): boolean {
    const retained = this.retained
    if (retained === null) return false
    this.retained = null
    this.ctx.clock.clearTimeout(retained.timer)
    retained.clip.dispose()
    this.ctx.patch({ retained: null })
    const text =
      why === "escape" ? "failed recording discarded" : why === "new_recording" ? "previous failed recording discarded" : why === "expired" ? "failed recording discarded after 10 min" : null
    if (text !== null) this.ctx.notice("info", text, { id: VOICE_NOTICE_IDS.result })
    return true
  }

  /** Alt+R and /voice retry (5.4.1); `reserved` counts a recording in progress against the queue limit. */
  retry(reserved: number): boolean {
    const retained = this.retained
    if (retained === null) return false
    const now = this.ctx.now()
    if (retained.info.retryAt !== null && now < retained.info.retryAt) {
      this.ctx.notice("info", `wait ${wholeSeconds(retained.info.retryAt - now)} before retrying (rate limited)`, { id: VOICE_NOTICE_IDS.result })
      return true
    }
    if (this.queue.length + reserved >= MAX_QUEUED_CLIPS) {
      this.ctx.notice("warn", "transcription queue full; wait or press Esc", { id: VOICE_NOTICE_IDS.unavailable })
      return true
    }
    this.retained = null
    this.ctx.clock.clearTimeout(retained.timer)
    this.ctx.patch({ retained: null })
    // The sticky "Alt+R retry" notice goes with the clip; the retry's own result replaces nothing stale.
    this.ctx.dismiss(VOICE_NOTICE_IDS.result)
    this.enqueue(retained.clip)
    return true
  }

  dispose(): void {
    this.disposed = true
    this.cancelAll(false)
    this.dropRetained(null)
  }

  private publishQueue(): void {
    this.ctx.patch({ queue: this.queue.map((entry) => entry.item) })
  }

  private setItem(entry: QueueEntry, patch: Partial<VoiceQueueItem>): void {
    entry.item = { ...entry.item, ...patch }
    if (!entry.cancelled) this.publishQueue()
  }

  private async pump(): Promise<void> {
    if (this.working) return
    this.working = true
    try {
      for (let entry = this.queue[0]; entry !== undefined && !this.disposed; entry = this.queue[0]) {
        const result = await this.runChain(entry)
        this.queue = this.queue.filter((queued) => queued !== entry)
        this.publishQueue()
        this.complete(entry, result)
      }
    } finally {
      this.working = false
    }
  }

  private async runChain(entry: QueueEntry): Promise<ChainResult> {
    const abort = new AbortController()
    entry.abort = abort
    this.attempts = []
    const budget = this.ctx.clock.setTimeout(
      () => abort.abort(new VoiceError("timeout", "the 180 s transcription budget is used up", { code: "budget" })),
      VOICE_TIMING.clipBudgetMs,
    )
    const chain: ChainState = { failures: [], steps: [], retryAt: null, tooLargeAbove: -1, current: null }
    try {
      for (const source of this.host.candidates()) {
        const outcome = await this.trySource(entry, source, abort.signal, chain)
        if (outcome !== null) return outcome
      }
      return { kind: "exhausted", chain }
    } catch (error) {
      const reason = toVoiceError(error)
      if (reason.kind === "cancelled" || entry.cancelled) return { kind: "cancelled" }
      const who = chain.current === null ? "" : `${chain.current} `
      chain.steps.push(reason.opts.code === "budget" ? `${who}timed out (the 180 s budget ran out)` : `${who}failed unexpectedly`)
      return { kind: "exhausted", chain }
    } finally {
      this.ctx.clock.clearTimeout(budget)
      entry.abort = null
    }
  }

  /** null: go on with the next candidate. */
  private async trySource(entry: QueueEntry, source: VoiceSource, signal: AbortSignal, chain: ChainState): Promise<ChainResult | null> {
    chain.current = source.id
    const availability = await this.host.availability.fresh(source, signal)
    if (!availability.available) {
      chain.current = null
      chain.steps.push(`${source.id} ${availability.reason ?? "unavailable"}`)
      return null
    }
    const limit = source.maxWavBytes()
    if (limit < entry.clip.wav.length || limit <= chain.tooLargeAbove) {
      chain.current = null
      chain.steps.push(`${source.id} recording too large`)
      return null
    }
    const attempt = await this.attemptSource(entry, source, signal)
    chain.current = null
    if (attempt.ok) return { kind: "ok", transcript: attempt.transcript, failures: chain.failures }
    const failure: Failure = { source: source.id, error: attempt.error, retries: attempt.retries }
    chain.failures.push(failure)
    chain.steps.push(`${source.id} ${describeFailure(failure)}`)
    const step = fallbackStep(attempt.error.kind)
    if (step === "stop") return { kind: "stopped", error: attempt.error }
    if (step === "cancel") return { kind: "cancelled" }
    this.afterFailure(source, attempt.error, limit, chain)
    return null
  }

  private async attemptSource(entry: QueueEntry, source: VoiceSource, signal: AbortSignal): Promise<AttemptResult> {
    this.host.acknowledge(source)
    for (let retries = 0; ; retries += 1) {
      const started = this.ctx.now()
      this.setItem(entry, { state: "transcribing", source: source.id, startedAt: started, attempt: retries })
      try {
        const transcript = await raceAbort(
          Promise.resolve().then(() => source.transcribe(entry.clip, signal)),
          signal,
        )
        this.record(source.id, "ok", started, entry.clip)
        return { ok: true, transcript }
      } catch (raw) {
        if (signal.aborted) throw abortReason(signal)
        const error = toVoiceError(raw, source.id)
        this.record(source.id, error.kind, started, entry.clip)
        const delay = BUSY_RETRY_DELAYS_MS[retries]
        if (error.kind !== "busy" || delay === undefined) return { ok: false, error, retries }
        this.setItem(entry, { state: "waiting", source: source.id, startedAt: this.ctx.now() + delay, attempt: retries + 1 })
        await sleep(this.ctx.clock, delay, signal)
      }
    }
  }

  private afterFailure(source: VoiceSource, error: VoiceError, limit: number, chain: ChainState): void {
    switch (error.kind) {
      case "unavailable":
        this.host.availability.markUnavailable(source, error.message)
        return
      case "auth":
        this.ctx.notice("warn", `${source.id}: ${error.message}; dictation uses the next source until the token is fixed`, {
          id: VOICE_NOTICE_IDS.auth,
          sticky: true,
        })
        return
      case "model_not_ready":
        if (!this.modelHintShown) this.ctx.notice("info", HEX_MODEL_HINT, { id: VOICE_NOTICE_IDS.modelHint })
        this.modelHintShown = true
        return
      case "too_large":
        chain.tooLargeAbove = Math.max(chain.tooLargeAbove, limit)
        return
      case "rate_limited":
        chain.retryAt = Math.max(chain.retryAt ?? 0, this.ctx.now() + (error.opts.retryAfterMs ?? 60_000))
        return
      default:
        return
    }
  }

  private record(source: VoiceSourceId, outcome: VoiceErrorKind | "ok", started: number, clip: WavClip): void {
    const ms = this.ctx.now() - started
    this.attempts = [...this.attempts, { source, outcome, ms }]
    this.ctx.patch({ lastAttempts: this.attempts })
    this.ctx.log?.event("voice.attempt", { source, outcome, ms, bytes: clip.wav.length })
  }

  private complete(entry: QueueEntry, result: ChainResult): void {
    if (entry.cancelled || result.kind === "cancelled" || this.disposed) {
      entry.clip.dispose()
      return
    }
    try {
      if (result.kind === "ok") this.succeed(entry, result.transcript, result.failures)
      else if (result.kind === "stopped") this.stop(entry, result.error)
      else this.retain(entry.clip, result.chain)
    } catch {
      // A composer that throws must not stop the queue; the clip is never kept after an unexpected failure.
      entry.clip.dispose()
    }
  }

  private succeed(entry: QueueEntry, transcript: Transcript, failures: readonly Failure[]): void {
    const text = sanitizeLine(transcript.text).trim()
    if (text === "") {
      this.stop(entry, new VoiceError("no_speech", "no speech detected", { source: transcript.source }))
      return
    }
    const durationMs = entry.clip.durationMs
    entry.clip.dispose()
    this.host.composer.insertText(text, { separate: true })
    this.ctx.patch({ lastError: null })
    if (failures.length === 0) {
      this.ctx.notice("info", `transcribed ${wholeSeconds(durationMs)} in ${tenthSeconds(transcript.elapsedMs)} (${transcript.source})`, {
        id: VOICE_NOTICE_IDS.result,
      })
      return
    }
    const reasons = failures.map((f) => `${f.source}: ${describeVoiceErrorKind(f.error.kind)}`).join("; ")
    this.ctx.notice("warn", `transcribed by ${transcript.source} (${reasons})`, { id: VOICE_NOTICE_IDS.result })
  }

  private stop(entry: QueueEntry, error: VoiceError): void {
    entry.clip.dispose()
    this.ctx.patch({ lastError: { kind: error.kind, message: error.message } })
    if (error.kind === "no_speech") {
      this.ctx.notice("info", "no speech detected", { id: VOICE_NOTICE_IDS.result })
      return
    }
    const source = error.opts.source ?? "the source"
    this.ctx.notice("error", `voice: ${source} refused the recording (${error.kind}): ${error.message}`, { id: VOICE_NOTICE_IDS.result })
  }

  /**
   * End of chain: one warn naming every attempt, and the clip is kept for Alt+R (5.4.1). The keys lead (a notice
   * row is cut to the terminal width) and the notice stays while the clip is kept (10 min), instead of expiring
   * after 12 s; a retry, Esc, a new recording or the expiry replaces it. /voice repeats it.
   */
  private retain(clip: WavClip, chain: ChainState): void {
    this.dropRetained(null)
    const first = chain.failures[0]?.error
    const now = this.ctx.now()
    const info: RetainedClipInfo = { durationMs: clip.durationMs, reason: first?.kind ?? "unavailable", until: now + VOICE_TIMING.retainMs, retryAt: chain.retryAt }
    const timer = this.ctx.clock.setTimeout(() => this.dropRetained("expired"), VOICE_TIMING.retainMs)
    this.retained = { clip, info, timer }
    this.ctx.patch({ retained: info, lastError: { kind: info.reason, message: first?.message ?? "no voice source was available" } })
    const parts = chain.steps.length > 0 ? chain.steps.join("; ") : "no voice source"
    this.ctx.notice("warn", `voice failed (Alt+R retry, Esc discard): ${parts}`, { id: VOICE_NOTICE_IDS.result, sticky: true })
  }
}

// ---------------------------------------------------------------------------------------------------------
// The controller: push-to-talk, recording, public API
// ---------------------------------------------------------------------------------------------------------

interface Recording {
  readonly abort: AbortController
  readonly meter: LevelMeterModel
  readonly vad: EnergyVad
  readonly maxDurationMs: number
  /** Index of the recorder being tried; a callback from an older attempt is ignored. */
  index: number
  handle: RecorderHandle | null
  firstChunk: boolean
  signalSeen: boolean
  pendingFinish: boolean
  stopping: boolean
  cancelled: boolean
  readonly failures: string[]
  firstChunkTimer: TimerHandle | null
}

type PublishedPtt = Pick<VoiceUiState, "phase" | "mode" | "trigger" | "startedAt">

class VoiceManager implements VoiceController {
  private readonly ctx: Ctx
  private readonly availability: Availability
  private readonly transcriber: Transcriber
  private readonly sourceById = new Map<VoiceSourceId, VoiceSource>()
  private readonly unsubscribe: () => void
  private readonly hintUnsubscribers: (() => void)[] = []
  private readonly probeTimer: TimerHandle
  private readonly profileReady: Promise<void>
  private order: VoiceSourceId[]
  private pinned: VoiceSourceId | null = null
  private ptt: PttState = createPttState()
  private cfg: PttConfig
  private readonly events: PttEvent[] = []
  private draining = false
  private recording: Recording | null = null
  private tick: TimerHandle | null = null
  private published: PublishedPtt = { phase: "idle", mode: null, trigger: null, startedAt: null }
  private storedDevice: string | null = null
  private acks: Partial<Record<VoiceSourceId, string>> = {}
  private readonly ackShown = new Set<VoiceSourceId>()
  private lastConn: string
  private lastServer: unknown
  private inCredential = false
  /** Next voice/level dispatch; scheduled against a running deadline so 20 ms frames average the full rate. */
  private nextLevelAt = Number.NEGATIVE_INFINITY
  private disposed = false

  constructor(private readonly deps: VoiceControllerDeps) {
    this.ctx = createCtx(deps.store, deps.clock, deps.log ?? null, () => !this.disposed)
    for (const source of deps.sources) if (!this.sourceById.has(source.id)) this.sourceById.set(source.id, source)
    this.order = normalizeOrder(deps.config.order)
    this.availability = new Availability(this.ctx, [...this.sourceById.values()], deps.recorders)
    this.cfg = pttConfigFromVoice(deps.config, this.maxDurationMs())
    this.transcriber = new Transcriber(this.ctx, {
      candidates: () => this.candidates(),
      availability: this.availability,
      composer: deps.composer,
      acknowledge: (source) => this.acknowledge(source),
    })
    const state = deps.store.getState()
    this.lastConn = state.conn.kind
    this.lastServer = state.server
    this.inCredential = state.composer.credential !== null
    this.unsubscribe = deps.store.subscribe(() => this.onStore())
    this.probeTimer = deps.clock.setInterval(() => this.probeNow(), VOICE_TIMING.probeIntervalMs)
    for (const source of this.sourceById.values()) {
      if (hasAvailabilityHint(source)) this.hintUnsubscribers.push(source.onAvailabilityHint(() => this.probeOne(source)))
    }
    this.profileReady = this.loadProfileState()
    this.probeNow()
  }

  // ---- input (keymap, mic widget, renderer focus) ----

  pttKey(e: { eventType: "press" | "release"; repeated: boolean }): void {
    const at = this.ctx.now()
    this.feed(e.eventType === "release" ? { type: "key_release", at } : { type: "key_press", repeated: e.repeated, at })
  }

  micDown(): void {
    this.feed({ type: "mouse_down", at: this.ctx.now() })
  }

  micUp(): void {
    this.feed({ type: "mouse_up", at: this.ctx.now() })
  }

  escape(): boolean {
    if (this.disposed) return false
    if (this.ptt.phase !== "idle") {
      this.feed({ type: "escape", at: this.ctx.now() })
      return true
    }
    if (this.transcriber.cancelAll()) return true
    return this.transcriber.dropRetained("escape")
  }

  retry(): boolean {
    if (this.disposed) return false
    return this.transcriber.retry(this.ptt.phase === "idle" ? 0 : 1)
  }

  blur(): void {
    this.feed({ type: "blur", at: this.ctx.now() })
  }

  focus(): void {
    // Focus-in changes nothing: a recording that survived the blur (toggle) keeps running.
  }

  // ---- commands (/voice ...) ----

  probeNow(): void {
    if (this.disposed || !this.deps.config.enabled) return
    this.availability.probeAll()
    this.availability.checkRecorders()
  }

  status(): VoiceStatusReport {
    return {
      enabled: this.deps.config.enabled,
      order: [...this.order],
      pinned: this.pinned,
      sources: VOICE_SOURCE_IDS.map((id) => this.sourceStatus(id)),
      recorder: this.availability.recorderStatus(),
      device: this.deviceStatus(),
      retained: this.transcriber.retainedInfo(),
      lastAttempts: this.transcriber.lastAttempts(),
      maxDurationMs: this.maxDurationMs(),
    }
  }

  use(id: VoiceSourceId | "auto"): void {
    if (id === "auto") {
      this.pinned = null
      return
    }
    const source = this.sourceById.get(id)
    if (source === undefined) {
      this.ctx.notice("warn", `${id} is not in this build`, { id: VOICE_NOTICE_IDS.config })
      return
    }
    this.pinned = id
    this.probeOne(source)
  }

  setOrder(ids: VoiceSourceId[]): void {
    const order = normalizeOrder(ids)
    if (order.length === 0) {
      this.ctx.notice("warn", "the voice order needs at least one source id", { id: VOICE_NOTICE_IDS.config })
      return
    }
    this.order = order
  }

  async listDevices(): Promise<CaptureDevice[]> {
    for (const recorder of this.deps.recorders) {
      if (!isDeviceAware(recorder) || this.availability.recorderKnownBad(recorder)) continue
      try {
        return await recorder.listDevices()
      } catch {
        // Try the next recorder that can list devices.
      }
    }
    return []
  }

  /** Persists the first matching device as voiceDevice; an empty name restores [voice] device or the default. */
  async selectDevice(name: string): Promise<boolean> {
    if (this.disposed) return false
    if (name.trim() === "") {
      this.storedDevice = null
      await this.saveDevice((state) => {
        const { voiceDevice: _dropped, ...rest } = state
        return rest
      })
      return true
    }
    const match = matchCaptureDevice(await this.listDevices(), name)
    if (match === null || this.disposed) return false
    this.storedDevice = match.name
    await this.saveDevice((state) => ({ ...state, voiceDevice: match.name }))
    return true
  }

  async dispose(): Promise<void> {
    if (this.disposed) return
    this.disposed = true
    this.unsubscribe()
    for (const off of this.hintUnsubscribers.splice(0)) off()
    this.deps.clock.clearInterval(this.probeTimer)
    if (this.tick !== null) this.deps.clock.clearInterval(this.tick)
    this.tick = null
    const rec = this.recording
    this.recording = null
    if (rec !== null) this.abandon(rec)
    this.transcriber.dispose()
    await Promise.allSettled(this.deps.sources.map((source) => source.dispose()))
    await Promise.allSettled(this.deps.recorders.filter(isDisposableRecorder).map(async (recorder) => recorder.dispose()))
  }

  // ---- the push-to-talk machine ----

  private feed(event: PttEvent): void {
    if (this.disposed) return
    this.events.push(event)
    if (this.draining) return
    this.draining = true
    try {
      for (let next = this.events.shift(); next !== undefined; next = this.events.shift()) this.step(next)
    } finally {
      this.draining = false
    }
  }

  private step(event: PttEvent): void {
    const result = pttReduce(this.ptt, event, this.cfg)
    this.ptt = result.state
    this.syncTick()
    this.publishPtt()
    for (const effect of result.effects) this.run(effect)
  }

  private run(effect: PttEffect): void {
    switch (effect.type) {
      case "start_capture":
        this.startCapture()
        return
      case "finish_capture":
        this.finishCapture()
        return
      case "cancel_capture":
        this.cancelCapture(effect.reason)
        return
      case "notice":
        this.ctx.notice(effect.level, effect.text, { id: VOICE_NOTICE_IDS.ptt })
        return
    }
  }

  private syncTick(): void {
    if (this.ptt.phase === "idle") {
      if (this.tick !== null) this.deps.clock.clearInterval(this.tick)
      this.tick = null
      return
    }
    this.tick ??= this.deps.clock.setInterval(() => this.onTick(), VOICE_TIMING.tickMs)
  }

  private onTick(): void {
    const at = this.ctx.now()
    this.feed({ type: "tick", at })
    const s = this.ptt
    const rec = this.recording
    const silent = rec !== null && !rec.signalSeen && s.phase === "recording" && s.mode === "toggle"
    if (silent && s.startedAt !== null && at - s.startedAt >= this.cfg.noSignalTimeoutMs) this.feed({ type: "no_signal_timeout", at })
  }

  private publishPtt(): void {
    const { phase, mode, trigger, startedAt } = this.ptt
    const last = this.published
    if (last.phase === phase && last.mode === mode && last.trigger === trigger && last.startedAt === startedAt) return
    this.published = { phase, mode, trigger, startedAt }
    this.ctx.patch({ phase, mode, trigger, startedAt })
  }

  // ---- recording ----

  /** Why a recording may not start now (5.4), or null. Only cached facts: a key press never waits for a probe. */
  private refusal(): string | null {
    if (!this.deps.config.enabled) return "voice is off (--no-voice)"
    if (this.deps.store.getState().composer.credential !== null) return "dictation is off while a credential is requested"
    const candidates = this.candidates()
    if (candidates.length === 0) return "voice unavailable: no voice source in this build"
    const unavailable = this.availability.unavailableReasons(candidates)
    if (unavailable !== null) return `voice unavailable: ${unavailable}`
    if (this.transcriber.size >= MAX_QUEUED_CLIPS) return "transcription queue full; wait or press Esc"
    const recorders = this.availability.recorderReasons()
    if (recorders !== null) return `voice unavailable: no recorder (${recorders})`
    return null
  }

  private startCapture(): void {
    const refusal = this.refusal()
    if (refusal !== null) {
      this.ctx.notice("warn", refusal, { id: VOICE_NOTICE_IDS.unavailable })
      this.feed({ type: "capture_failed", at: this.ctx.now() })
      return
    }
    this.transcriber.dropRetained("new_recording")
    const maxDurationMs = this.maxDurationMs()
    this.cfg = pttConfigFromVoice(this.deps.config, maxDurationMs)
    const rec: Recording = {
      abort: new AbortController(),
      meter: new LevelMeterModel(),
      vad: new EnergyVad({ silenceStopMs: this.deps.config.silenceStopMs }),
      maxDurationMs,
      index: -1,
      handle: null,
      firstChunk: false,
      signalSeen: false,
      pendingFinish: false,
      stopping: false,
      cancelled: false,
      failures: [],
      firstChunkTimer: null,
    }
    this.recording = rec
    this.nextLevelAt = Number.NEGATIVE_INFINITY
    this.ctx.patch({ captureLive: false, recorder: null })
    void this.openRecorder(rec, 0)
  }

  private isCurrent(rec: Recording): boolean {
    return this.recording === rec && !rec.cancelled && !this.disposed
  }

  /** Tries the recorders from `from` on; the first that starts records this clip. */
  private async openRecorder(rec: Recording, from: number): Promise<void> {
    const recorders = this.deps.recorders
    for (let index = from; index < recorders.length; index += 1) {
      const recorder = recorders[index]
      if (recorder === undefined || this.availability.recorderKnownBad(recorder)) continue
      if (!this.isCurrent(rec)) return
      rec.index = index
      this.armFirstChunkTimer(rec, index)
      try {
        const handle = await recorder.start(this.startOptions(rec, index))
        if (!this.isCurrent(rec) || rec.index !== index) {
          handle.cancel()
          return
        }
        rec.handle = handle
        this.ctx.patch({ recorder: recorder.kind })
        this.noteDevice(recordingDevice(handle))
        if (rec.pendingFinish) void this.stopRecording(rec)
        return
      } catch (error) {
        if (!this.isCurrent(rec) || rec.index !== index) return
        rec.failures.push(`${recorder.kind}: ${toVoiceError(error, "recorder", "recorder").message}`)
      }
    }
    if (this.isCurrent(rec)) this.failRecording(rec, `voice unavailable: ${rec.failures.join("; ") || "no recorder"}`)
  }

  private startOptions(rec: Recording, index: number): CaptureStartOptions {
    const device = this.deviceName()
    return {
      maxDurationMs: rec.maxDurationMs,
      ...(device === null ? {} : { device }),
      onLevel: (level) => {
        if (rec.index === index) this.onLevel(rec, level)
      },
      onFirstChunk: () => {
        if (rec.index === index) this.onFirstChunk(rec)
      },
      onFailure: (error) => this.recorderFailed(rec, index, error.message),
      // parec gave up the requested source after start() returned: the same notice as a fallback at start.
      onDeviceFallback: (device) => {
        if (this.isCurrent(rec) && rec.index === index) this.noteDevice(device)
      },
      signal: rec.abort.signal,
    }
  }

  /** A recorder failed on its own: before the first chunk the next recorder is tried, afterwards the recording ends. */
  private recorderFailed(rec: Recording, index: number, message: string): void {
    if (!this.isCurrent(rec) || rec.index !== index || rec.stopping) return
    if (rec.firstChunk) {
      this.failRecording(rec, `recording failed: ${message}`)
      return
    }
    rec.failures.push(`${this.deps.recorders[index]?.kind ?? "recorder"}: ${message}`)
    const handle = rec.handle
    rec.handle = null
    rec.index = index + 1
    handle?.cancel()
    void this.openRecorder(rec, index + 1)
  }

  private failRecording(_rec: Recording, text: string): void {
    this.ctx.notice("warn", text, { id: VOICE_NOTICE_IDS.unavailable })
    this.feed({ type: "capture_failed", at: this.ctx.now() })
  }

  private armFirstChunkTimer(rec: Recording, index: number): void {
    this.clearFirstChunkTimer(rec)
    rec.firstChunkTimer = this.deps.clock.setTimeout(() => {
      rec.firstChunkTimer = null
      this.recorderFailed(rec, index, "no audio arrived within 5 s")
    }, VOICE_TIMING.firstChunkTimeoutMs)
  }

  private clearFirstChunkTimer(rec: Recording): void {
    if (rec.firstChunkTimer !== null) this.deps.clock.clearTimeout(rec.firstChunkTimer)
    rec.firstChunkTimer = null
  }

  private onFirstChunk(rec: Recording): void {
    if (!this.isCurrent(rec) || rec.firstChunk) return
    rec.firstChunk = true
    this.clearFirstChunkTimer(rec)
    this.ctx.patch({ captureLive: true })
    this.feed({ type: "capture_started", at: this.ctx.now() })
  }

  private onLevel(rec: Recording, level: LevelSample): void {
    if (!this.isCurrent(rec)) return
    if (level.peakDb > NO_SIGNAL_DB) rec.signalSeen = true
    const meter = rec.meter.update(level)
    if (rec.vad.push(level.rmsDb) === "silence") this.feed({ type: "silence", at: level.at })
    const interval = this.deps.store.getState().ui.reduceMotion ? VOICE_TIMING.reducedLevelIntervalMs : VOICE_TIMING.levelIntervalMs
    if (level.at < this.nextLevelAt) return
    this.nextLevelAt = level.at - this.nextLevelAt >= interval ? level.at + interval : this.nextLevelAt + interval
    this.deps.store.dispatch({ type: "voice/level", level: meter, at: level.at })
  }

  private finishCapture(): void {
    const rec = this.recording
    if (rec === null) {
      this.feed({ type: "finished", at: this.ctx.now() })
      return
    }
    if (rec.handle === null) rec.pendingFinish = true
    else void this.stopRecording(rec)
  }

  private async stopRecording(rec: Recording): Promise<void> {
    const handle = rec.handle
    if (handle === null || rec.stopping) return
    rec.stopping = true
    this.clearFirstChunkTimer(rec)
    let clip: WavClip | null = null
    let failure = "no audio"
    try {
      clip = await handle.stop()
    } catch (error) {
      failure = toVoiceError(error, "recorder", "recorder").message
    }
    if (!this.isCurrent(rec)) {
      clip?.dispose()
      return
    }
    this.recording = null
    this.endRecordingUi()
    if (clip === null) this.ctx.notice("warn", `recording failed: ${failure}`, { id: VOICE_NOTICE_IDS.unavailable })
    else this.acceptClip(clip, handle)
    this.feed({ type: "finished", at: this.ctx.now() })
  }

  private acceptClip(clip: WavClip, handle: RecorderHandle): void {
    this.ctx.log?.event("voice.capture", { recorder: handle.kind, ms: clip.durationMs, dropped: clip.droppedFrames })
    const reason = localDiscardReason(clip)
    if (reason !== null) {
      clip.dispose()
      this.discardNotice(reason, handle)
      return
    }
    if (clip.droppedFrames > 0) this.ctx.notice("warn", "audio dropped: the UI was busy", { id: VOICE_NOTICE_IDS.dropped })
    this.transcriber.enqueue(clip)
  }

  private discardNotice(reason: LocalDiscardReason, handle: RecorderHandle | null): void {
    if (reason === "too_short") {
      this.ctx.notice("info", "too short to transcribe (under 0.3 s)", { id: VOICE_NOTICE_IDS.ptt })
      return
    }
    this.ctx.notice("warn", `no signal from ${this.deviceLabel(handle)} - /voice device`, { id: VOICE_NOTICE_IDS.ptt })
  }

  private cancelCapture(reason: PttCancelReason): void {
    const rec = this.recording
    this.recording = null
    if (rec !== null) {
      this.abandon(rec)
      this.endRecordingUi()
    }
    if (reason === "too_short") this.discardNotice("too_short", null)
    if (reason === "no_signal") this.discardNotice("no_signal", rec?.handle ?? null)
  }

  /** Cancels a recording's capture; whatever was recorded is zero-filled by the recorder. */
  private abandon(rec: Recording): void {
    rec.cancelled = true
    this.clearFirstChunkTimer(rec)
    rec.abort.abort(cancelledError())
    rec.handle?.cancel()
  }

  private endRecordingUi(): void {
    const at = this.ctx.now()
    if (!this.ctx.alive()) return
    this.deps.store.dispatchMany([
      { type: "voice/level", level: null, at },
      { type: "voice/state", patch: { captureLive: false, recorder: null }, at },
    ])
  }

  private noteDevice(device: RecordingDevice): void {
    if (!device.deviceFallback) return
    const requested = this.deviceName() ?? "the configured device"
    this.ctx.notice("warn", `capture device "${requested}" not found; using the default source - /voice device`, { id: VOICE_NOTICE_IDS.device })
  }

  // ---- sources, devices, limits ----

  private candidates(): VoiceSource[] {
    const ids = this.pinned === null ? this.order : [this.pinned]
    return ids.flatMap((id) => {
      const source = this.sourceById.get(id)
      return source === undefined ? [] : [source]
    })
  }

  private probeOne(source: VoiceSource): void {
    if (!this.disposed && this.deps.config.enabled) void this.availability.probe(source)
  }

  /**
   * The effective cap of one recording (5.5): voice.max_duration_s, the 290 s hard cap, and the largest WAV a
   * source that may be available accepts: floor((maxWavBytes - 44) / 32000) s.
   */
  private maxDurationMs(): number {
    let cap = Math.min(this.deps.config.maxDurationMs, VOICE_MAX_DURATION_CAP_MS)
    let largest = 0
    for (const source of this.candidates()) {
      if (this.availability.get(source.id)?.available === false) continue
      largest = Math.max(largest, source.maxWavBytes())
    }
    if (largest > 0) cap = Math.min(cap, maxDurationForWavBytes(largest))
    return Math.max(1_000, cap)
  }

  private deviceName(): string | null {
    return this.storedDevice ?? (this.deps.config.device?.trim() || null)
  }

  /** The device a recording used: its own name, "the default source" after a fallback, else the requested one. */
  private deviceLabel(handle: RecorderHandle | null): string {
    const used = handle === null ? null : recordingDevice(handle)
    if (used?.deviceName) return used.deviceName
    if (used?.deviceFallback === true) return "the default source"
    return this.deviceName() ?? "the default source"
  }

  private deviceStatus(): VoiceStatusReport["device"] {
    if (this.storedDevice !== null) return { name: this.storedDevice, origin: "state" }
    const configured = this.deps.config.device?.trim()
    return configured ? { name: configured, origin: "config" } : { name: null, origin: "default" }
  }

  private sourceStatus(id: VoiceSourceId): VoiceSourceStatus {
    const source = this.sourceById.get(id)
    if (source === undefined) {
      return { id, label: SOURCE_LABELS[id], privacyNote: "", inBuild: false, availability: null, maxWavBytes: null }
    }
    return {
      id,
      label: source.label,
      privacyNote: source.privacyNote,
      inBuild: true,
      availability: this.availability.get(id) ?? null,
      maxWavBytes: source.maxWavBytes(),
    }
  }

  // ---- profile state: device choice and first-use privacy notices (5.5, 5.6) ----

  private async saveDevice(change: (state: ProfileState) => ProfileState): Promise<void> {
    try {
      await this.deps.stateFile.update(change)
    } catch {
      this.ctx.notice("warn", "the device choice could not be saved; it applies to this session", { id: VOICE_NOTICE_IDS.device })
    }
  }

  private async loadProfileState(): Promise<void> {
    try {
      const state = await this.deps.stateFile.read()
      if (this.disposed) return
      const device = typeof state.voiceDevice === "string" ? state.voiceDevice.trim() : ""
      if (device !== "" && this.storedDevice === null) this.storedDevice = device
      this.acks = { ...state.voiceAck, ...this.acks }
    } catch {
      // An unreadable state file means defaults: no stored device, privacy notes shown again.
    }
  }

  /** First use of a source shows its privacy note once and records voiceAck in the profile state file. */
  private acknowledge(source: VoiceSource): void {
    void this.profileReady.then(() => {
      if (this.disposed || this.acks[source.id] !== undefined || this.ackShown.has(source.id)) return
      this.ackShown.add(source.id)
      this.ctx.notice("info", `${source.label}: ${source.privacyNote}`, { id: `voice-privacy-${source.id}` })
      const stamp = new Date((this.deps.wallNow ?? Date.now)()).toISOString()
      this.acks = { ...this.acks, [source.id]: stamp }
      this.deps.stateFile.update((state) => ({ ...state, voiceAck: { ...state.voiceAck, [source.id]: stamp } })).catch(() => undefined)
    })
  }

  // ---- store changes ----

  private onStore(): void {
    if (this.disposed) return
    const state = this.deps.store.getState()
    if (state.conn.kind !== this.lastConn || state.server !== this.lastServer) {
      this.lastConn = state.conn.kind
      this.lastServer = state.server
      queueMicrotask(() => this.probeNow())
    }
    const credential = state.composer.credential !== null
    if (credential && !this.inCredential) queueMicrotask(() => this.stopForCredential())
    this.inCredential = credential
  }

  private stopForCredential(): void {
    if (this.disposed || this.ptt.phase === "idle") return
    this.feed({ type: "escape", at: this.ctx.now() })
    this.ctx.notice("info", "dictation stopped: a credential is requested", { id: VOICE_NOTICE_IDS.ptt })
  }
}

export function createVoiceController(deps: VoiceControllerDeps): VoiceController {
  return new VoiceManager(deps)
}
