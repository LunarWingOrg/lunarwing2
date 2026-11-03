/**
 * The `lunarvoice` source (design 5.3.1): server STT through the active transport's transcribe() (gateway
 * POST /api/voice/transcribe today, REPLv3 Transcribe in M4), which proxies to the daemon's loopback LunarVoice
 * sidecar. No remote provider is ever used for dictation.
 *
 * - Probe: unavailable("not connected") unless the connection is ready, then ServerInfo.capabilities.transcribe.
 *   Side-effect free: it reads the store only (GET /api/voice/status is the transport's job at connect, and the
 *   gateway's voice rate-limit slot is consumed by every voice request).
 * - maxWavBytes() = limits.maxTranscribeWavBytes (18 MiB gateway, 12 MiB REPLv3).
 * - Client rate window: 60 s sliding window of its own calls, limit limits.transcribePerMinute (10); a full window
 *   (or a recent server 429) yields rate_limited with retryAfterMs without a request.
 * - An empty transcript is no_speech.
 * - Gateway errors (TransportError, mapped by transport/gateway.ts from voice.rs): 429 rate_limited; 503 "voice
 *   sidecar is unavailable" busy; 503 "voice service is not configured" unavailable, and the source stays
 *   disabled until the next connection; 504 timeout; 413 too_large; 422 processing_failed; 400 bad_request;
 *   401 auth; 500/502 and network errors unavailable. A VoiceError thrown by a transport passes through, which is
 *   how the M4 socket transport reports REPLv3 codes without a TransportErrorCode (voiceErrorKindForReplCode).
 */
import type { Clock } from "@opentui/core"
import { newTranscribeRequestId } from "../../state/ids.ts"
import type { Store } from "../../state/model.ts"
import { isTransportError, type TransportError } from "../../transport/errors.ts"
import type { ServerInfo, TranscribeResult, Transport } from "../../transport/types.ts"
import { cancelledError, isVoiceError, VoiceError, type VoiceErrorKind } from "../errors.ts"
import type { SourceAvailability, Transcript, VoiceSource, WavClip } from "../types.ts"

export const LUNARVOICE_ID = "lunarvoice" as const
export const LUNARVOICE_LABEL = "LunarVoice"
export const LUNARVOICE_PRIVACY_NOTE =
  "audio goes to the daemon's host and its loopback LunarVoice sidecar; nothing is stored (the sidecar caches results in memory for 10 min)"

export const RATE_WINDOW_MS = 60_000

export interface LunarVoiceDeps {
  /** Connection state and ServerInfo (conn, server). */
  store: Store
  clock: Clock
  /** The active transport; null before the first connection. */
  transport: () => Transport | null
}

/** Sliding window of this client's own transcription requests (5.3.1). */
export class RateWindow {
  private readonly sent: number[] = []
  private blockedUntil = 0

  constructor(
    private readonly clock: Clock,
    private readonly windowMs = RATE_WINDOW_MS,
  ) {}

  /** Counts a request now, or says how long to wait. `limit` null or below 1 means no client-side limit. */
  acquire(limit: number | null): { ok: true } | { ok: false; retryAfterMs: number } {
    const now = this.clock.now()
    while (this.sent.length > 0 && (this.sent[0] ?? now) <= now - this.windowMs) this.sent.shift()
    if (now < this.blockedUntil) return { ok: false, retryAfterMs: this.blockedUntil - now }
    if (limit !== null && limit >= 1 && this.sent.length >= limit) {
      return { ok: false, retryAfterMs: Math.max(1, (this.sent[0] ?? now) + this.windowMs - now) }
    }
    this.sent.push(now)
    return { ok: true }
  }

  /** After a server 429: nothing more until `until`. */
  block(until: number): void {
    this.blockedUntil = Math.max(this.blockedUntil, until)
  }
}

function statusSuffix(error: TransportError): string {
  return error.status === undefined ? "" : ` (HTTP ${error.status})`
}

/** Maps a gateway TransportError onto the voice error kinds of 5.3.1 with a short message (no server text). */
export function lunarVoiceErrorFromTransport(error: TransportError): VoiceError {
  const source = LUNARVOICE_ID
  const status = error.status
  const opts = { source, ...(status === undefined ? {} : { status }), code: error.code }
  const suffix = statusSuffix(error)
  switch (error.code) {
    case "rate_limited":
      return new VoiceError("rate_limited", `rate limited${suffix}`, { ...opts, retryAfterMs: error.retryAfterMs ?? RATE_WINDOW_MS })
    case "busy":
      return new VoiceError("busy", `the voice sidecar is busy${suffix}`, opts)
    case "unavailable":
      return /not configured/i.test(error.message)
        ? new VoiceError("unavailable", "LunarVoice is not configured on this daemon", opts)
        : new VoiceError("unavailable", `the voice service is unavailable${suffix}`, opts)
    case "timeout":
      return new VoiceError("timeout", `the transcription timed out${suffix}`, opts)
    case "too_large":
      return new VoiceError("too_large", `the recording is too large for the daemon${suffix}`, opts)
    case "bad_request":
      return status === 422
        ? new VoiceError("processing_failed", "the voice sidecar could not process the recording (HTTP 422)", opts)
        : new VoiceError("bad_request", `the daemon rejected the transcription request${suffix}`, opts)
    case "auth":
    case "forbidden":
    case "permission_denied":
      return new VoiceError("auth", `the gateway rejected the token${suffix}`, opts)
    case "server":
      return new VoiceError("unavailable", `the voice sidecar failed${suffix}`, opts)
    case "unreachable":
      return new VoiceError("unavailable", "the daemon is unreachable", opts)
    case "closed":
      return new VoiceError("unavailable", "not connected", opts)
    default:
      return new VoiceError("unavailable", `transcription failed (${error.code})${suffix}`, opts)
  }
}

// A Map, not an object literal: wire codes are untrusted and must not hit inherited properties ("toString").
const REPL_CODES: ReadonlyMap<string, VoiceErrorKind> = new Map<string, VoiceErrorKind>([
  ["busy", "busy"],
  ["rate_limited", "rate_limited"],
  ["too_large", "too_large"],
  ["unavailable", "unavailable"],
  ["feature_not_negotiated", "unavailable"],
  ["timeout", "timeout"],
  ["no_speech", "no_speech"],
  ["transcribe_failed", "processing_failed"],
  ["unsupported_media", "unsupported_audio"],
])

/** REPLv3 Error{req_id} codes (5.3.1), for the M4 socket transport; unknown codes are `unavailable`. */
export function voiceErrorKindForReplCode(code: string): VoiceErrorKind {
  return REPL_CODES.get(code) ?? "unavailable"
}

function mib(bytes: number): string {
  return `${Math.round((bytes / (1024 * 1024)) * 10) / 10} MiB`
}

class LunarVoiceSource implements VoiceSource {
  readonly id = LUNARVOICE_ID
  readonly label = LUNARVOICE_LABEL
  readonly privacyNote = LUNARVOICE_PRIVACY_NOTE
  private readonly window: RateWindow
  /** The connection whose daemon answered "voice service is not configured" (5.3.1: disabled for the session). */
  private disabledFor: ServerInfo | null = null

  constructor(private readonly deps: LunarVoiceDeps) {
    this.window = new RateWindow(deps.clock)
  }

  async probe(_signal: AbortSignal): Promise<SourceAvailability> {
    const checkedAt = this.deps.clock.now()
    const state = this.deps.store.getState()
    if (state.conn.kind !== "ready") return { available: false, reason: "not connected", checkedAt }
    const info = state.server
    const transport = this.deps.transport()
    if (info === null || transport === null || transport.transcribe === undefined) {
      return { available: false, reason: "no transcription on this transport", checkedAt }
    }
    if (!info.capabilities.transcribe || info.limits.maxTranscribeWavBytes <= 0) {
      const reason = info.transport === "socket" ? "no transcription on this socket daemon" : "not configured on this gateway"
      return { available: false, reason, checkedAt }
    }
    if (this.disabledFor === info) return { available: false, reason: "not configured on this daemon", checkedAt }
    const perMinute = info.limits.transcribePerMinute
    const detail = `${info.endpointLabel}, up to ${mib(info.limits.maxTranscribeWavBytes)}${perMinute ? `, ${perMinute}/min` : ""}`
    return { available: true, detail, checkedAt }
  }

  maxWavBytes(): number {
    const info = this.deps.store.getState().server
    return info?.capabilities.transcribe ? Math.max(0, info.limits.maxTranscribeWavBytes) : 0
  }

  async transcribe(clip: WavClip, signal: AbortSignal): Promise<Transcript> {
    if (signal.aborted) throw cancelledError(undefined, LUNARVOICE_ID)
    const state = this.deps.store.getState()
    const transport = this.deps.transport()
    const info = state.server
    if (state.conn.kind !== "ready" || info === null || transport?.transcribe === undefined) {
      throw new VoiceError("unavailable", "not connected", { source: LUNARVOICE_ID })
    }
    const max = info.limits.maxTranscribeWavBytes
    if (max > 0 && clip.wav.byteLength > max) {
      throw new VoiceError("too_large", `the recording is larger than ${mib(max)}`, { source: LUNARVOICE_ID })
    }
    const slot = this.window.acquire(info.limits.transcribePerMinute)
    if (!slot.ok) {
      const wait = Math.ceil(slot.retryAfterMs / 1000)
      throw new VoiceError("rate_limited", `rate limited (wait ${wait} s)`, { source: LUNARVOICE_ID, retryAfterMs: slot.retryAfterMs })
    }
    const started = this.deps.clock.now()
    let result: TranscribeResult
    try {
      result = await transport.transcribe({ requestId: newTranscribeRequestId(), wav: clip.wav, signal })
    } catch (error) {
      throw this.failure(error, signal, info)
    }
    if (signal.aborted) throw cancelledError(undefined, LUNARVOICE_ID)
    return this.transcript(result, clip, started)
  }

  async dispose(): Promise<void> {
    this.disabledFor = null
  }

  private transcript(result: TranscribeResult, clip: WavClip, started: number): Transcript {
    const text = result.text.trim()
    if (text === "") throw new VoiceError("no_speech", "no speech detected", { source: LUNARVOICE_ID })
    return {
      text,
      source: LUNARVOICE_ID,
      ...(result.model ? { model: result.model } : {}),
      ...(result.language ? { language: result.language } : {}),
      audioMs: clip.durationMs,
      elapsedMs: result.elapsedMs ?? this.deps.clock.now() - started,
    }
  }

  private failure(error: unknown, signal: AbortSignal, info: ServerInfo): VoiceError {
    if (signal.aborted) return cancelledError(undefined, LUNARVOICE_ID)
    if (isVoiceError(error)) return error.opts.source === undefined ? new VoiceError(error.kind, error.message, { ...error.opts, source: LUNARVOICE_ID }) : error
    if (!isTransportError(error)) return new VoiceError("unavailable", "the transcription request failed", { source: LUNARVOICE_ID })
    const mapped = lunarVoiceErrorFromTransport(error)
    if (mapped.kind === "rate_limited") this.window.block(this.deps.clock.now() + (mapped.opts.retryAfterMs ?? RATE_WINDOW_MS))
    if (mapped.kind === "unavailable" && /not configured/i.test(error.message)) this.disabledFor = info
    return mapped
  }
}

export function createLunarVoiceSource(deps: LunarVoiceDeps): VoiceSource {
  return new LunarVoiceSource(deps)
}
