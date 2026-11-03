import { describe, expect, test } from "bun:test"
import { ManualClock } from "@opentui/core/testing"
import { BACKOFF_DEFAULTS, exponentialBackoff, nominalBackoffDelay } from "../../src/transport/backoff.ts"

/** Deterministic uniform values in [0, 1) (mulberry32). */
function seededRandom(seed: number): () => number {
  let state = seed >>> 0
  return () => {
    state = (state + 0x6d2b79f5) >>> 0
    let t = state
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

describe("exponentialBackoff (3.6.5)", () => {
  test("500 ms doubling capped at 15 s", () => {
    const backoff = exponentialBackoff({ random: () => 0.5, clock: new ManualClock() })
    const delays = Array.from({ length: 9 }, () => backoff.next())
    expect(delays).toEqual([500, 1_000, 2_000, 4_000, 8_000, 15_000, 15_000, 15_000, 15_000])
    expect(backoff.attempt).toBe(9)
  })

  test("defaults match the design", () => {
    expect(BACKOFF_DEFAULTS).toEqual({ baseMs: 500, factor: 2, maxMs: 15_000, jitter: 0.2, resetAfterMs: 30_000 })
    expect(nominalBackoffDelay(0)).toBe(500)
    expect(nominalBackoffDelay(5)).toBe(15_000)
  })

  test("jitter stays within +/-20 % of the nominal delay", () => {
    const random = seededRandom(1234)
    for (let run = 0; run < 50; run += 1) {
      const backoff = exponentialBackoff({ random, clock: new ManualClock() })
      for (let attempt = 0; attempt < 8; attempt += 1) {
        const nominal = nominalBackoffDelay(attempt)
        const delay = backoff.next()
        expect(delay).toBeGreaterThanOrEqual(Math.floor(nominal * 0.8))
        expect(delay).toBeLessThanOrEqual(Math.ceil(nominal * 1.2))
      }
    }
  })

  test("the extremes of the random source give exactly -20 % and +20 %", () => {
    expect(exponentialBackoff({ random: () => 0 }).next()).toBe(400)
    expect(exponentialBackoff({ random: () => 1 }).next()).toBe(600)
  })

  test("resets after 30 s ready", () => {
    const clock = new ManualClock()
    const backoff = exponentialBackoff({ random: () => 0.5, clock })
    backoff.next()
    backoff.next()
    backoff.next()
    backoff.ready()
    clock.advance(30_000)
    expect(backoff.next()).toBe(500)
    expect(backoff.next()).toBe(1_000)
  })

  test("a connection that drops before 30 s keeps growing the delay", () => {
    const clock = new ManualClock()
    const backoff = exponentialBackoff({ random: () => 0.5, clock })
    expect(backoff.next()).toBe(500)
    backoff.ready()
    clock.advance(29_999)
    expect(backoff.next()).toBe(1_000)
    backoff.ready()
    clock.advance(10_000)
    expect(backoff.next()).toBe(2_000)
  })

  test("reset() starts over", () => {
    const backoff = exponentialBackoff({ random: () => 0.5 })
    backoff.next()
    backoff.next()
    backoff.reset()
    expect(backoff.attempt).toBe(0)
    expect(backoff.next()).toBe(500)
  })
})
