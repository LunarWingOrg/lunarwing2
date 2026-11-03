/**
 * Reconnect backoff (design 3.6.5): 500 ms base, factor 2, capped at 15 s, +/-20 % jitter, and back to the first
 * delay once a connection has stayed ready for 30 s. The clock and the random source are injected so tests are
 * deterministic (ManualClock, a fixed random).
 *
 * controller/supervisor.ts: `backoff.ready()` on conn/ready; on loss `await clock delay backoff.next()`. A
 * connection that drops within 30 s keeps growing the delay, so an accept-then-drop server cannot cause a tight
 * reconnect loop. Server hints (429: 60 s, 503 "Too many connections": 30 s) arrive as
 * TransportError.retryAfterMs; the supervisor waits max(backoff.next(), retryAfterMs).
 */

export interface Backoff {
  /** Delay in ms before the next attempt; advances the attempt count (after the 30 s ready reset, if due). */
  next(): number
  /** Back to the first delay (e.g. /transport reconnect). */
  reset(): void
  /** The connection became ready now (clock time); a later next() resets once it lasted resetAfterMs. */
  ready(): void
  /** Delays handed out since the last reset. */
  readonly attempt: number
}

export interface BackoffOptions {
  baseMs?: number
  factor?: number
  maxMs?: number
  /** Fraction: 0.2 = +/-20 %. */
  jitter?: number
  /** Ready time after which the sequence starts over. */
  resetAfterMs?: number
  /** Uniform in [0, 1); default Math.random. */
  random?: () => number
  /** Default Date.now. Pass the renderer's Clock (ManualClock in tests). */
  clock?: { now(): number }
}

export const BACKOFF_DEFAULTS = Object.freeze({
  baseMs: 500,
  factor: 2,
  maxMs: 15_000,
  jitter: 0.2,
  resetAfterMs: 30_000,
})

/** The un-jittered delay of attempt `n` (0-based). */
export function nominalBackoffDelay(
  attempt: number,
  opts: Pick<BackoffOptions, "baseMs" | "factor" | "maxMs"> = {},
): number {
  const base = opts.baseMs ?? BACKOFF_DEFAULTS.baseMs
  const factor = opts.factor ?? BACKOFF_DEFAULTS.factor
  const max = opts.maxMs ?? BACKOFF_DEFAULTS.maxMs
  return Math.min(max, base * factor ** Math.max(0, attempt))
}

export function exponentialBackoff(opts: BackoffOptions = {}): Backoff {
  const jitter = Math.max(0, opts.jitter ?? BACKOFF_DEFAULTS.jitter)
  const resetAfterMs = opts.resetAfterMs ?? BACKOFF_DEFAULTS.resetAfterMs
  const random = opts.random ?? Math.random
  const clock = opts.clock ?? { now: () => Date.now() }
  let attempt = 0
  let readySince: number | null = null

  return {
    next(): number {
      if (readySince !== null && clock.now() - readySince >= resetAfterMs) attempt = 0
      readySince = null
      const nominal = nominalBackoffDelay(attempt, opts)
      attempt += 1
      const r = Math.min(1, Math.max(0, random()))
      return Math.max(0, Math.round(nominal * (1 + jitter * (2 * r - 1))))
    },
    reset(): void {
      attempt = 0
      readySince = null
    },
    ready(): void {
      readySince = clock.now()
    },
    get attempt(): number {
      return attempt
    },
  }
}
