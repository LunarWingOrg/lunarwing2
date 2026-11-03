/**
 * Levels and VU-meter dynamics (design 5.5): dBFS per 20 ms frame with a -90 floor, the s16le odd-byte carry,
 * instant attack and about 25 dB/s release, 1 s peak hold, the clip marker and "no signal" after 1.5 s below -70.
 */
import { describe, expect, test } from "bun:test"
import { FrameSplitter, LEVEL_FLOOR_DB, LevelMeterModel, linearToDb, measureS16, S16leDecoder } from "../../src/voice/levels.ts"
import type { LevelSample } from "../../src/voice/types.ts"
import { FRAME_SAMPLES } from "../../src/voice/wav.ts"

function sine(samples: number, amplitude: number, hz = 1_000): Int16Array {
  return Int16Array.from({ length: samples }, (_, i) => Math.round(amplitude * 32_767 * Math.sin((2 * Math.PI * hz * i) / 16_000)))
}

function frame(rmsDb: number, peakDb: number, at: number): LevelSample {
  return { rmsDb, peakDb, at }
}

describe("dBFS", () => {
  test("dBFS of full-scale sine and silence", () => {
    const full = measureS16(sine(FRAME_SAMPLES, 1), 0)
    expect(full.peakDb).toBeCloseTo(0, 2)
    expect(full.rmsDb).toBeCloseTo(-3.01, 1)
    const half = measureS16(sine(FRAME_SAMPLES, 0.5), 0)
    expect(half.peakDb).toBeCloseTo(-6.02, 1)
    expect(measureS16(new Int16Array(FRAME_SAMPLES), 0)).toEqual({ rmsDb: LEVEL_FLOOR_DB, peakDb: LEVEL_FLOOR_DB, at: 0 })
    expect(measureS16(new Int16Array(0), 5)).toEqual({ rmsDb: -90, peakDb: -90, at: 5 })
    // One LSB is about -90.3 dBFS: clamped to the floor.
    expect(measureS16(Int16Array.from([1, -1]), 0).peakDb).toBe(-90)
    expect(linearToDb(0)).toBe(-90)
    expect(linearToDb(Number.NaN)).toBe(-90)
    expect(linearToDb(0.1)).toBeCloseTo(-20, 5)
  })

  test("odd-byte carry", () => {
    const samples = [0, 1, -1, 32_767, -32_768, 12_345, -23_456, 256, -256]
    const bytes = new Uint8Array(samples.length * 2)
    const view = new DataView(bytes.buffer)
    samples.forEach((s, i) => view.setInt16(i * 2, s, true))
    for (let split = 0; split <= bytes.length; split += 1) {
      for (let second = split; second <= bytes.length; second += 1) {
        const decoder = new S16leDecoder()
        const parts = [bytes.subarray(0, split), bytes.subarray(split, second), bytes.subarray(second)]
        const out = parts.flatMap((part) => [...decoder.push(part)])
        expect(out).toEqual(samples)
        expect(decoder.pending).toBe(0)
      }
    }
    const decoder = new S16leDecoder()
    expect([...decoder.push(Uint8Array.from([0x34]))]).toEqual([])
    expect(decoder.pending).toBe(1)
    expect([...decoder.push(Uint8Array.from([0x12, 0xff]))]).toEqual([0x1234])
    expect(decoder.pending).toBe(1)
    decoder.reset()
    expect(decoder.pending).toBe(0)
  })

  test("chunks of any size are cut into 20 ms frames", () => {
    const frames: number[] = []
    const splitter = new FrameSplitter((f) => frames.push(f.length))
    splitter.push(new Int16Array(100))
    splitter.push(new Int16Array(700))
    expect(frames).toEqual([320, 320])
    expect(splitter.partial()?.length).toBe(160)
    splitter.push(new Int16Array(160).fill(9))
    expect(frames).toEqual([320, 320, 320])
    expect(splitter.partial()).toBeNull()
    splitter.push(new Int16Array(5).fill(7))
    splitter.clear()
    expect(splitter.partial()).toBeNull()
  })
})

describe("VU meter model", () => {
  test("instant attack and about 25 dB/s release", () => {
    const meter = new LevelMeterModel()
    expect(meter.update(frame(-20, -10, 0)).rmsDb).toBe(-20)
    expect(meter.update(frame(-60, -50, 400)).rmsDb).toBeCloseTo(-30, 5)
    expect(meter.update(frame(-60, -50, 800)).rmsDb).toBeCloseTo(-40, 5)
    expect(meter.update(frame(-15, -5, 820)).rmsDb).toBe(-15)
  })

  test("peak hold decays after 1 s", () => {
    const meter = new LevelMeterModel()
    meter.update(frame(-20, -6, 0))
    for (let at = 20; at <= 900; at += 20) meter.update(frame(-60, -50, at))
    expect(meter.read(900).heldPeakDb).toBe(-6)
    expect(meter.read(1_000).heldPeakDb).toBe(-6)
    expect(meter.update(frame(-60, -50, 1_400)).heldPeakDb).toBeCloseTo(-16, 5)
    // Decays down to the current peak, never below it.
    expect(meter.update(frame(-60, -50, 4_000)).heldPeakDb).toBe(-50)
    // A new, louder peak takes the hold again.
    expect(meter.update(frame(-10, -3, 4_020)).heldPeakDb).toBe(-3)
  })

  test("clip marker at -1 dBFS or above, shown for the hold time", () => {
    const meter = new LevelMeterModel()
    expect(meter.update(frame(-10, -1.5, 0)).clipping).toBe(false)
    expect(meter.update(frame(-8, -1, 20)).clipping).toBe(true)
    expect(meter.update(frame(-30, -20, 900)).clipping).toBe(true)
    expect(meter.update(frame(-30, -20, 1_100)).clipping).toBe(false)
  })

  test("no-signal after 1.5 s below -70 dBFS", () => {
    const meter = new LevelMeterModel()
    let last = meter.update(frame(-85, -80, 0))
    for (let at = 20; at < 1_500; at += 20) last = meter.update(frame(-85, -80, at))
    expect(last.noSignal).toBe(false)
    expect(meter.update(frame(-85, -80, 1_500)).noSignal).toBe(true)
    expect(meter.update(frame(-40, -30, 1_520)).noSignal).toBe(false)
    expect(meter.update(frame(-80, -75, 1_540)).noSignal).toBe(false)
    expect(meter.update(frame(-80, -75, 3_040)).noSignal).toBe(true)
  })
})
