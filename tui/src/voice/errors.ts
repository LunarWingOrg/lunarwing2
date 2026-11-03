/**
 * Voice errors (design 5.2) and the fallback table of 5.4. VoiceError itself is declared once, in voice/types.ts
 * (the scaffold), and re-exported here: a second class would break `instanceof` across modules.
 *
 * Messages are short, user-facing and built from status codes and server error codes only: never a token, a
 * URL query, a transcript or audio.
 */
import { isVoiceError, VoiceError, type VoiceErrorKind, type VoiceSourceId } from "./types.ts"

export { isVoiceError, VoiceError } from "./types.ts"
export type { VoiceErrorKind, VoiceErrorOptions } from "./types.ts"

/** What the transcription chain does after a failed attempt (5.4). */
export type FallbackStep =
  /** Try the next candidate with the same WAV. */
  | "next"
  /** Retry the same source after 1, 2 and 4 s, then the next candidate. */
  | "retry"
  /** End the chain and dispose the clip (no_speech, client bugs). */
  | "stop"
  /** End the chain at once, abort the request, dispose the clip. */
  | "cancel"

export const FALLBACK_STEPS: Readonly<Record<VoiceErrorKind, FallbackStep>> = Object.freeze({
  unavailable: "next",
  busy: "retry",
  rate_limited: "next",
  timeout: "next",
  processing_failed: "next",
  model_not_ready: "next",
  too_large: "next",
  auth: "next",
  no_speech: "stop",
  bad_request: "stop",
  unsupported_audio: "stop",
  cancelled: "cancel",
  // Not produced by sources; an unexpected failure of one source must not end the chain.
  recorder: "next",
  internal: "next",
})

/** The step for `kind`; a kind outside the table (a buggy source) moves on to the next candidate. */
export function fallbackStep(kind: VoiceErrorKind): FallbackStep {
  return Object.hasOwn(FALLBACK_STEPS, kind) ? FALLBACK_STEPS[kind] : "next"
}

/** Busy retries of the same source (R18): after 1, 2 and 4 s. */
export const BUSY_RETRY_DELAYS_MS: readonly number[] = Object.freeze([1_000, 2_000, 4_000])

const KIND_LABELS: Readonly<Record<VoiceErrorKind, string>> = Object.freeze({
  unavailable: "unavailable",
  busy: "busy",
  rate_limited: "rate limited",
  timeout: "timed out",
  no_speech: "no speech detected",
  too_large: "too large",
  unsupported_audio: "unsupported audio",
  auth: "token rejected",
  bad_request: "request rejected",
  model_not_ready: "model not ready",
  processing_failed: "processing failed",
  cancelled: "cancelled",
  recorder: "recorder failed",
  internal: "failed",
})

/** Short label for notices and the voice panel ("lunarvoice busy", "hex-app timed out"). */
export function describeVoiceErrorKind(kind: VoiceErrorKind): string {
  return Object.hasOwn(KIND_LABELS, kind) ? KIND_LABELS[kind] : "failed"
}

export function cancelledError(message = "dictation cancelled", source?: VoiceSourceId | "recorder"): VoiceError {
  return new VoiceError("cancelled", message, source === undefined ? {} : { source })
}

function isAbortError(error: unknown): boolean {
  return typeof error === "object" && error !== null && (error as { name?: unknown }).name === "AbortError"
}

/**
 * Normalizes anything a source or recorder threw. A VoiceError passes through (with `source` filled in when it
 * was missing); an AbortError becomes `cancelled`; anything else becomes `fallback` with a generic message, so
 * foreign error text (which could quote a URL or a header) never reaches the screen.
 */
export function toVoiceError(
  error: unknown,
  source?: VoiceSourceId | "recorder",
  fallback: VoiceErrorKind = "internal",
): VoiceError {
  if (isVoiceError(error)) {
    if (source === undefined || error.opts.source !== undefined) return error
    return new VoiceError(error.kind, error.message, { ...error.opts, source })
  }
  if (isAbortError(error)) return cancelledError(undefined, source)
  const name = error instanceof Error && /^[A-Za-z]{1,40}$/.test(error.name) ? ` (${error.name})` : ""
  return new VoiceError(fallback, `unexpected failure${name}`, source === undefined ? {} : { source })
}

/** The VoiceError an AbortSignal was aborted with, or `cancelled`. */
export function abortReason(signal: AbortSignal): VoiceError {
  return isVoiceError(signal.reason) ? signal.reason : cancelledError()
}
