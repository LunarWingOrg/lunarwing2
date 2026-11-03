/**
 * Scripted VoiceSources (design 11.3.1) and the small doubles the voice controller tests need around them: a
 * composer, a profile state file and an async flush. Replies are consumed in order:
 *
 *   const hex = new FakeSource({ id: "hex-app", clock })
 *   hex.reply({ error: "busy" }, { text: "check the ports registry" })
 *   hex.reply({ manual: true })              // settle later: hex.pending[0]?.resolve("text")
 *   hex.reply({ hang: true })                // only an abort ends it
 *   hex.setAvailable(false, "not running")   // probe result
 *
 * Every call keeps the WavClip it was given (the very object the manager passed), and a copy of its bytes taken
 * at call time, so a test can check "the same WAV" and "zero-filled after Esc".
 */
import type { Clock } from "@opentui/core"
import type { GlobalState, ProfileState, StateFile } from "../../src/config/types.ts"
import type { ComposerHandle } from "../../src/controller/types.ts"
import { cancelledError, VoiceError, type VoiceErrorKind } from "../../src/voice/errors.ts"
import type { SourceAvailability, Transcript, VoiceSource, VoiceSourceId, WavClip } from "../../src/voice/types.ts"

export type FakeReply =
  | { text: string; delayMs?: number; model?: string; elapsedMs?: number }
  | { error: VoiceErrorKind; message?: string; retryAfterMs?: number; status?: number; delayMs?: number }
  /** Settles only when the request is aborted. */
  | { hang: true }
  /** Settled by the test through `pending`. */
  | { manual: true }

export interface FakeCall {
  readonly clip: WavClip
  /** The WAV object the manager passed (identity shows "the same WAV"). */
  readonly wav: Uint8Array
  /** Copy of the bytes at call time. */
  readonly bytes: Uint8Array
  readonly at: number
  readonly signal: AbortSignal
}

export interface PendingCall {
  readonly call: FakeCall
  resolve(text: string): void
  reject(error: VoiceError): void
}

export interface FakeSourceOptions {
  id: VoiceSourceId
  clock: Clock
  label?: string
  privacyNote?: string
  /** Default 18 MiB. */
  maxWavBytes?: number
  available?: boolean
  reason?: string
  /** "never": the probe never settles (the manager's 1 s budget ends it). Default: a microtask. */
  probeDelayMs?: number | "never"
  replies?: FakeReply[]
  /** Used once `replies` is empty (default { text: "hello" }). */
  defaultReply?: FakeReply
}

export class FakeSource implements VoiceSource {
  readonly id: VoiceSourceId
  readonly label: string
  readonly privacyNote: string
  readonly calls: FakeCall[] = []
  readonly pending: PendingCall[] = []
  probes = 0
  disposed = false
  limit: number
  private available: boolean
  private reason: string
  private readonly replies: FakeReply[]

  constructor(private readonly opts: FakeSourceOptions) {
    this.id = opts.id
    this.label = opts.label ?? `fake ${opts.id}`
    this.privacyNote = opts.privacyNote ?? `privacy note of ${opts.id}`
    this.limit = opts.maxWavBytes ?? 18 * 1024 * 1024
    this.available = opts.available ?? true
    this.reason = opts.reason ?? "not running"
    this.replies = [...(opts.replies ?? [])]
  }

  reply(...replies: FakeReply[]): this {
    this.replies.push(...replies)
    return this
  }

  setAvailable(available: boolean, reason = "not running"): this {
    this.available = available
    this.reason = reason
    return this
  }

  async probe(_signal: AbortSignal): Promise<SourceAvailability> {
    this.probes += 1
    const delay = this.opts.probeDelayMs ?? 0
    if (delay === "never") return new Promise<SourceAvailability>(() => undefined)
    if (delay > 0) await new Promise<void>((resolve) => this.opts.clock.setTimeout(resolve, delay))
    const checkedAt = this.opts.clock.now()
    return this.available ? { available: true, detail: "fake", checkedAt } : { available: false, reason: this.reason, checkedAt }
  }

  maxWavBytes(): number {
    return this.limit
  }

  transcribe(clip: WavClip, signal: AbortSignal): Promise<Transcript> {
    const call: FakeCall = { clip, wav: clip.wav, bytes: clip.wav.slice(), at: this.opts.clock.now(), signal }
    this.calls.push(call)
    const reply = this.replies.shift() ?? this.opts.defaultReply ?? { text: "hello" }
    return new Promise<Transcript>((resolve, reject) => {
      const onAbort = () => reject(cancelledError(undefined, this.id))
      if (signal.aborted) {
        onAbort()
        return
      }
      signal.addEventListener("abort", onAbort, { once: true })
      this.play(reply, call, resolve, reject)
    })
  }

  async dispose(): Promise<void> {
    this.disposed = true
  }

  private play(reply: FakeReply, call: FakeCall, resolve: (t: Transcript) => void, reject: (e: VoiceError) => void): void {
    if ("hang" in reply) return
    const transcript = (text: string, extra: { model?: string; elapsedMs?: number } = {}): Transcript => ({
      text,
      source: this.id,
      ...(extra.model ? { model: extra.model } : {}),
      audioMs: call.clip.durationMs,
      elapsedMs: extra.elapsedMs ?? this.opts.clock.now() - call.at,
    })
    if ("manual" in reply) {
      this.pending.push({ call, resolve: (text) => resolve(transcript(text)), reject })
      return
    }
    const settle = () => {
      if ("text" in reply) {
        if (reply.text.trim() === "") reject(new VoiceError("no_speech", "no speech detected", { source: this.id }))
        else resolve(transcript(reply.text, reply))
        return
      }
      const opts = {
        source: this.id,
        ...(reply.retryAfterMs === undefined ? {} : { retryAfterMs: reply.retryAfterMs }),
        ...(reply.status === undefined ? {} : { status: reply.status }),
      }
      reject(new VoiceError(reply.error, reply.message ?? `scripted ${reply.error}`, opts))
    }
    const delay = reply.delayMs ?? 0
    if (delay > 0) this.opts.clock.setTimeout(settle, delay)
    else queueMicrotask(settle)
  }
}

/** A ComposerHandle over a plain string with the cursor at the end (dictation appends). */
export class FakeComposer implements ComposerHandle {
  text = ""
  readonly inserts: { text: string; separate: boolean }[] = []
  focused = true
  metrics = 0

  getText(): string {
    return this.text
  }

  insertText(text: string, opts: { separate?: boolean } = {}): void {
    const separate = opts.separate === true
    this.inserts.push({ text, separate })
    const needsSpace = separate && this.text.length > 0 && !/\s$/.test(this.text)
    this.text += `${needsSpace ? " " : ""}${text}`
  }

  replaceText(text: string): void {
    this.text = text
  }

  clear(): void {
    this.text = ""
  }

  focus(): void {
    this.focused = true
  }

  blur(): void {
    this.focused = false
  }

  visualRow(): number {
    return 0
  }

  visualLines(): number {
    return Math.max(1, this.text.split("\n").length)
  }

  reportMetrics(): void {
    this.metrics += 1
  }
}

/** In-memory profile state (primary instance). */
export class FakeStateFile implements StateFile {
  readonly profile = "test"
  readonly primary = true
  writes = 0
  state: ProfileState
  private global: GlobalState = {}

  constructor(initial: ProfileState = {}) {
    this.state = structuredClone(initial)
  }

  async read(): Promise<ProfileState> {
    return structuredClone(this.state)
  }

  async update(change: (current: ProfileState) => ProfileState): Promise<ProfileState> {
    this.state = change(structuredClone(this.state))
    this.writes += 1
    return structuredClone(this.state)
  }

  async readGlobal(): Promise<GlobalState> {
    return { ...this.global }
  }

  async updateGlobal(change: (current: GlobalState) => GlobalState): Promise<GlobalState> {
    this.global = change({ ...this.global })
    return { ...this.global }
  }
}

/** Every voice source acknowledged, so first-use privacy notices stay out of a test's way. */
export const ALL_VOICE_ACKS: ProfileState["voiceAck"] = Object.freeze({
  lunarvoice: "2026-11-03T00:00:00.000Z",
  "hex-app": "2026-11-03T00:00:00.000Z",
  "hex-embedded": "2026-11-03T00:00:00.000Z",
})

/** Lets every queued promise continuation run (no real time passes; timers belong to the ManualClock). */
export async function flushAsync(rounds = 4): Promise<void> {
  for (let i = 0; i < rounds; i += 1) await new Promise<void>((resolve) => setImmediate(resolve))
}

/** True when every byte is 0 (a disposed clip). */
export function isZeroed(bytes: Uint8Array): boolean {
  return bytes.every((b) => b === 0)
}
