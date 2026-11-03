/**
 * Energy VAD and the signal-based discard rule (design 5.5, UX-02): the VAD only serves the opt-in toggle
 * auto-stop; whether a clip is sent depends on its length and loudest frame alone.
 */
import { describe, expect, test } from "bun:test"
import { ManualClock } from "@opentui/core/testing"
import { createPttState, DEFAULT_PTT_CONFIG, pttReduce, type PttConfig, type PttEffect, type PttState } from "../../src/voice/ptt.ts"
import { CaptureSession } from "../../src/voice/recorder.ts"
import { EnergyVad, localDiscardReason, percentile, VAD_TUNING } from "../../src/voice/vad.ts"
import { quiet, tone } from "../mocks/fake-recorder.ts"

/** Feeds `count` frames of `rmsDb`; returns the events in order. */
function feed(vad: EnergyVad, rmsDb: number | ((i: number) => number), count: number): string[] {
  const events: string[] = []
  for (let i = 0; i < count; i += 1) {
    const event = vad.push(typeof rmsDb === "number" ? rmsDb : rmsDb(i))
    if (event !== null) events.push(event)
  }
  return events
}

function clipOf(samples: Float32Array) {
  const session = new CaptureSession({ maxDurationMs: 10_000, clock: new ManualClock(), onLevel: () => undefined, onFirstChunk: () => undefined })
  session.appendFloat32(samples)
  return session.finish(0)
}

describe("local discard (signal, never the VAD)", () => {
  test("speech from the first frame is still sent", () => {
    const vad = new EnergyVad({ silenceStopMs: 3_500 })
    // Talking from the very first frame: the floor estimate is clamped to -35 dBFS, speech still stands out.
    const events = feed(vad, -18, 15)
    expect(events).toEqual(["speech"])
    expect(vad.floorDb).toBe(VAD_TUNING.floorMaxDb)
    const clip = clipOf(tone(1_000, 0.2))
    expect(localDiscardReason(clip)).toBeNull()
  })

  test("a muted input (peak < -50 dBFS) is discarded as no signal", () => {
    const muted = clipOf(tone(2_000, 0.002))
    expect(muted.peakDb).toBeLessThan(-50)
    expect(localDiscardReason(muted)).toBe("no_signal")
    expect(localDiscardReason(clipOf(quiet(2_000)))).toBe("no_signal")
    // A quiet but real signal is sent; the server decides about speech.
    expect(localDiscardReason(clipOf(tone(2_000, 0.005)))).toBeNull()
    expect(localDiscardReason({ durationMs: 280, peakDb: -10 })).toBe("too_short")
    expect(localDiscardReason({ durationMs: 300, peakDb: -10 })).toBeNull()
  })
})

describe("energy VAD", () => {
  test("3500 ms silence after speech emits silence only with auto-stop on", () => {
    const vad = new EnergyVad({ silenceStopMs: 3_500 })
    expect(feed(vad, -75, 15)).toEqual([])
    expect(feed(vad, -20, 25)).toEqual(["speech"])
    // 3480 ms of non-speech: not yet.
    expect(feed(vad, -74, 174)).toEqual([])
    expect(feed(vad, -74, 1)).toEqual(["silence"])
    expect(feed(vad, -74, 500)).toEqual([])

    // The PTT machine only acts on `silence` in toggle mode with auto_stop_on_silence.
    const toggle = (cfg: PttConfig): PttEffect[] => {
      let state: PttState = createPttState()
      const effects: PttEffect[] = []
      const send = (e: Parameters<typeof pttReduce>[1]) => {
        const r = pttReduce(state, e, cfg)
        state = r.state
        effects.push(...r.effects)
      }
      send({ type: "key_press", repeated: false, at: 0 })
      send({ type: "capture_started", at: 20 })
      send({ type: "tick", at: 1_000 })
      send({ type: "silence", at: 4_000 })
      return effects
    }
    expect(toggle(DEFAULT_PTT_CONFIG).map((e) => e.type)).toEqual(["start_capture"])
    expect(toggle({ ...DEFAULT_PTT_CONFIG, autoStopOnSilence: true })).toContainEqual({ type: "finish_capture", reason: "silence" })
  })

  test("a short pause after speech does not count as silence", () => {
    const vad = new EnergyVad({ silenceStopMs: 3_500 })
    feed(vad, -70, 15)
    feed(vad, -20, 10)
    expect(feed(vad, -70, 100)).toEqual([])
    expect(feed(vad, -22, 3)).toEqual([])
    expect(feed(vad, -70, 174)).toEqual([])
    expect(feed(vad, -70, 1)).toEqual(["silence"])
  })

  test("floor adapts to hum", () => {
    const vad = new EnergyVad({ silenceStopMs: 3_500 })
    feed(vad, -72, 15)
    expect(vad.floorDb).toBeCloseTo(-72, 6)
    // A hum that creeps up by 0.05 dB per frame (5 dB/s) to -45 dBFS: the floor follows, no false speech.
    const ramp = feed(vad, (i) => Math.min(-45, -72 + i * 0.05), 1_000)
    expect(ramp).toEqual([])
    expect(vad.floorDb).toBeGreaterThan(-50)
    expect(vad.speechStarted).toBe(false)
    // Speech above the hum is still found.
    expect(feed(vad, -25, 10)).toEqual(["speech"])
    // Without adaptation the same hum would have been "speech" against the initial floor (-72 + 12).
    expect(-45).toBeGreaterThan(-72 + VAD_TUNING.speechMarginDb)
  })

  test("nearest-rank percentile", () => {
    expect(percentile([], 10)).toBe(0)
    expect(percentile([5], 10)).toBe(5)
    const values = Array.from({ length: 15 }, (_, i) => -80 + i)
    expect(percentile(values, 10)).toBe(-79)
    expect(percentile(values, 100)).toBe(-66)
  })
})
