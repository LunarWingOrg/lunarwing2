/**
 * In-process PCM/WAV (design 5.5) and the two recorders that produce it. No microphone is opened: parec runs as
 * test/helpers/fake-spawn.ts, OpenTUI Audio as an injected fake engine; time is a ManualClock.
 */
import { describe, expect, test } from "bun:test"
import { ManualClock } from "@opentui/core/testing"
import { cancelledError, isVoiceError, VoiceError } from "../../src/voice/errors.ts"
import { createOpenTuiRecorder, CAPTURE_OPTIONS, ENGINE_OPTIONS, type AudioEngineLike, type CaptureStreamLike } from "../../src/voice/recorder-opentui.ts"
import { createParecRecorder, parecArgv, PAREC_STOP_GRACE_MS } from "../../src/voice/recorder-parec.ts"
import { CaptureSession, matchCaptureDevice, type CaptureHandle } from "../../src/voice/recorder.ts"
import type { CaptureDevice, LevelSample, WavClip } from "../../src/voice/types.ts"
import {
  createWavClip,
  encodeWav,
  floatToS16,
  maxDurationForWavBytes,
  parseWav,
  PcmBuffer,
  WAV_HEADER_BYTES,
  wavBytesForDuration,
} from "../../src/voice/wav.ts"
import { createFakeSpawn, type FakeChildProcess } from "../helpers/fake-spawn.ts"
import { flushAsync, isZeroed } from "../mocks/fake-sources.ts"
import { tone } from "../mocks/fake-recorder.ts"

function ascii(bytes: Uint8Array, offset: number, length: number): string {
  return String.fromCharCode(...bytes.subarray(offset, offset + length))
}

function s16le(samples: readonly number[]): Uint8Array {
  const out = new Uint8Array(samples.length * 2)
  const view = new DataView(out.buffer)
  samples.forEach((s, i) => view.setInt16(i * 2, s, true))
  return out
}

describe("PCM and WAV", () => {
  test("clamping and rounding (-1 → -32768, 1 → 32767)", () => {
    expect(floatToS16(-1)).toBe(-32_768)
    expect(floatToS16(1)).toBe(32_767)
    expect(floatToS16(-3.5)).toBe(-32_768)
    expect(floatToS16(7)).toBe(32_767)
    expect(floatToS16(0)).toBe(0)
    expect(floatToS16(0.5)).toBe(16_384)
    expect(floatToS16(-0.5)).toBe(-16_384)
    expect(floatToS16(Number.NaN)).toBe(0)
    const pcm = new PcmBuffer(1)
    pcm.appendFloat32(Float32Array.from([-2, -1, 1, 2, 0.25]))
    expect([...pcm.view()]).toEqual([-32_768, -32_768, 32_767, 32_767, 8_192])
  })

  test("44-byte header fields", () => {
    const wav = encodeWav(Int16Array.from([1, -2, 32_767]))
    expect(wav.length).toBe(WAV_HEADER_BYTES + 6)
    const view = new DataView(wav.buffer)
    expect(ascii(wav, 0, 4)).toBe("RIFF")
    expect(view.getUint32(4, true)).toBe(36 + 6)
    expect(ascii(wav, 8, 4)).toBe("WAVE")
    expect(ascii(wav, 12, 4)).toBe("fmt ")
    expect(view.getUint32(16, true)).toBe(16)
    expect(view.getUint16(20, true)).toBe(1)
    expect(view.getUint16(22, true)).toBe(1)
    expect(view.getUint32(24, true)).toBe(16_000)
    expect(view.getUint32(28, true)).toBe(32_000)
    expect(view.getUint16(32, true)).toBe(2)
    expect(view.getUint16(34, true)).toBe(16)
    expect(ascii(wav, 36, 4)).toBe("data")
    expect(view.getUint32(40, true)).toBe(6)
    expect([...wav.subarray(44)]).toEqual([1, 0, 0xfe, 0xff, 0xff, 0x7f])
  })

  test("encode/parse round trip", () => {
    let seed = 7
    const samples = Int16Array.from({ length: 4_000 }, () => {
      seed = (seed * 1_103_515_245 + 12_345) % 2 ** 31
      return (seed % 65_536) - 32_768
    })
    const parsed = parseWav(encodeWav(samples))
    expect(parsed).toMatchObject({ sampleRate: 16_000, channels: 1, bitsPerSample: 16, byteRate: 32_000, blockAlign: 2 })
    expect(parsed.samples).toEqual(samples)
    expect(() => parseWav(new Uint8Array(10))).toThrow(RangeError)
    const corrupt = encodeWav(samples)
    corrupt[8] = 0x58
    expect(() => parseWav(corrupt)).toThrow(RangeError)
  })

  test("the PCM buffer holds maxDurationMs * 16 samples and counts the rest", () => {
    const pcm = new PcmBuffer(10)
    expect(pcm.capacity).toBe(160)
    pcm.appendS16(new Int16Array(100).fill(5))
    const stored = pcm.appendS16(new Int16Array(100).fill(6))
    expect(stored.length).toBe(60)
    expect(pcm.full).toBe(true)
    expect(pcm.truncatedSamples).toBe(40)
    pcm.zero()
    expect(pcm.length).toBe(0)
  })

  test("a clip zero-fills its PCM at once and its WAV on dispose", () => {
    const pcm = Int16Array.from([100, -100, 2_000, 3])
    const clip = createWavClip(pcm, { peakDb: -12, droppedFrames: 2 })
    expect(isZeroed(new Uint8Array(pcm.buffer))).toBe(true)
    expect(parseWav(clip.wav).samples).toEqual(Int16Array.from([100, -100, 2_000, 3]))
    expect(clip).toMatchObject({ sampleRate: 16_000, durationMs: 0, peakDb: -12, droppedFrames: 2, disposed: false })
    clip.dispose()
    expect(clip.disposed).toBe(true)
    expect(isZeroed(clip.wav)).toBe(true)
  })

  test("duration limits from WAV sizes", () => {
    expect(maxDurationForWavBytes(wavBytesForDuration(120_000))).toBe(120_000)
    expect(maxDurationForWavBytes(18 * 1024 * 1024)).toBe(589_000)
    expect(maxDurationForWavBytes(12 * 1024 * 1024)).toBe(393_000)
    expect(maxDurationForWavBytes(44)).toBe(0)
    expect(wavBytesForDuration(1_000)).toBe(32_044)
  })
})

describe("capture session", () => {
  test("levels every 20 ms, the first chunk once, and a WAV of what was captured", () => {
    const clock = new ManualClock()
    const levels: LevelSample[] = []
    let firstChunks = 0
    const session = new CaptureSession({ maxDurationMs: 2_000, clock, onLevel: (l) => levels.push(l), onFirstChunk: () => (firstChunks += 1) })
    session.appendFloat32(new Float32Array(0))
    expect(firstChunks).toBe(0)
    const audio = tone(1_000, 0.5)
    for (let i = 0; i < audio.length; i += 100) session.appendFloat32(audio.subarray(i, i + 100))
    expect(firstChunks).toBe(1)
    expect(levels).toHaveLength(50)
    const clip = session.finish(0)
    expect(clip.durationMs).toBe(1_000)
    expect(clip.peakDb).toBeCloseTo(-6.02, 1)
    expect(parseWav(clip.wav).samples.length).toBe(16_000)
    session.appendFloat32(audio)
    expect(parseWav(clip.wav).samples.length).toBe(16_000)
  })

  test("device names match exactly first, then by case-insensitive substring", () => {
    const devices: CaptureDevice[] = [
      { index: 0, name: "Monitor of C922 Pro Stream Webcam", isDefault: false },
      { index: 1, name: "C922 Pro Stream Webcam Analog Stereo", isDefault: false },
      { index: 2, name: "c922", isDefault: true },
    ]
    expect(matchCaptureDevice(devices, "C922")?.index).toBe(2)
    expect(matchCaptureDevice(devices, "analog")?.index).toBe(1)
    expect(matchCaptureDevice(devices, "  ")).toBeNull()
    expect(matchCaptureDevice(devices, "Blue Yeti")).toBeNull()
  })
})

// ---------------------------------------------------------------------------------------------------------
// parec (fake-spawn: no real parec ever runs)
// ---------------------------------------------------------------------------------------------------------

function recorderCallbacks() {
  const levels: LevelSample[] = []
  const failures: VoiceError[] = []
  const state = { firstChunks: 0 }
  const abort = new AbortController()
  return {
    levels,
    failures,
    state,
    abort,
    options: (extra: { device?: string; maxDurationMs?: number } = {}) => ({
      maxDurationMs: extra.maxDurationMs ?? 10_000,
      ...(extra.device === undefined ? {} : { device: extra.device }),
      onLevel: (l: LevelSample) => levels.push(l),
      onFirstChunk: () => {
        state.firstChunks += 1
      },
      onFailure: (e: VoiceError) => failures.push(e),
      signal: abort.signal,
    }),
  }
}

describe("parec recorder", () => {
  const ENV = { PATH: "/usr/bin", XDG_RUNTIME_DIR: "/run/user/1000" }

  test("argv only, the allowlisted environment, odd-byte chunks to WAV; stop is SIGINT, then SIGKILL after 1 s", async () => {
    const clock = new ManualClock()
    const fake = createFakeSpawn({ clock })
    fake.on("parec", { stayAlive: true, ignoreSignals: ["SIGINT"] })
    const recorder = createParecRecorder({ clock, env: ENV, spawn: fake.spawn, which: () => "/usr/bin/parec" })
    expect(await recorder.available()).toEqual({ ok: true })
    const cb = recorderCallbacks()
    const handle = (await recorder.start(cb.options({ device: "alsa_input.usb-C922" }))) as CaptureHandle
    const call = fake.lastCall
    expect(call?.argv).toEqual([
      "parec",
      "--raw",
      "--format=s16le",
      "--rate=16000",
      "--channels=1",
      "--latency-msec=20",
      "--client-name=lunarwing-tui",
      "--stream-name=dictation",
      "--device=alsa_input.usb-C922",
    ])
    expect(call?.env).toEqual(ENV)
    expect(call?.options).toMatchObject({ stdin: "ignore", stdout: "pipe", stderr: "pipe" })
    expect(handle.deviceName).toBe("alsa_input.usb-C922")

    const child = call?.process as FakeChildProcess
    const pcm = s16le(Array.from({ length: 640 }, (_, i) => (i % 2 === 0 ? 8_000 : -8_000)))
    // Split at odd offsets: every sample must survive the carry.
    child.pushStdout(pcm.subarray(0, 333))
    child.pushStdout(pcm.subarray(333, 801))
    child.pushStdout(pcm.subarray(801))
    await flushAsync()
    expect(cb.state.firstChunks).toBe(1)
    expect(cb.levels).toHaveLength(2)

    const stopping = handle.stop()
    await flushAsync()
    expect(child.kills).toEqual(["SIGINT"])
    clock.advance(PAREC_STOP_GRACE_MS)
    const clip = await stopping
    expect(child.kills).toEqual(["SIGINT", "SIGKILL"])
    expect(parseWav(clip.wav).samples).toEqual(Int16Array.from({ length: 640 }, (_, i) => (i % 2 === 0 ? 8_000 : -8_000)))
    expect(clip.durationMs).toBe(40)
    expect(cb.failures).toEqual([])
  })

  test("a parec that exits on SIGINT is not killed", async () => {
    const clock = new ManualClock()
    const fake = createFakeSpawn({ clock })
    fake.on("parec", { stayAlive: true })
    const recorder = createParecRecorder({ clock, env: ENV, spawn: fake.spawn, which: () => "/usr/bin/parec" })
    const handle = await recorder.start(recorderCallbacks().options())
    const child = fake.lastCall?.process as FakeChildProcess
    child.pushStdout(s16le(new Array(320).fill(100)))
    const clip = await handle.stop()
    expect(child.kills).toEqual(["SIGINT"])
    expect(clip.durationMs).toBe(20)
    expect(parecArgv()).not.toContain("--device=")
    expect(parecArgv().some((arg) => arg.startsWith("--device"))).toBe(false)
  })

  test("cancel kills with SIGKILL; an exit before stop() is a failure with the stderr summary", async () => {
    const clock = new ManualClock()
    const fake = createFakeSpawn({ clock })
    fake.once("parec", { stayAlive: true })
    fake.once("parec", { stderr: ["Stream error: No such entity\n"], exitCode: 1 })
    const recorder = createParecRecorder({ clock, env: ENV, spawn: fake.spawn, which: () => "/usr/bin/parec" })
    const first = recorderCallbacks()
    const handle = await recorder.start(first.options())
    handle.cancel()
    expect((fake.calls[0]?.process as FakeChildProcess).kills).toEqual(["SIGKILL"])
    expect(await handle.stop().catch((e: unknown) => isVoiceError(e) && e.kind)).toBe("cancelled")

    const second = recorderCallbacks()
    await recorder.start(second.options())
    await flushAsync()
    expect(second.failures.map((e) => [e.kind, e.message])).toEqual([["recorder", "parec exited (code 1): Stream error: No such entity"]])
  })

  test("missing parec: available() says so and start() throws a recorder error", async () => {
    const clock = new ManualClock()
    const fake = createFakeSpawn({ clock })
    const recorder = createParecRecorder({ clock, env: ENV, spawn: fake.spawn, which: () => null })
    expect(await recorder.available()).toEqual({ ok: false, reason: "parec is not installed" })
    const error = await recorder.start(recorderCallbacks().options()).catch((e: unknown) => e)
    expect(isVoiceError(error) && [error.kind, error.message]).toEqual(["recorder", "parec is not installed"])
  })
})

// ---------------------------------------------------------------------------------------------------------
// OpenTUI Audio (an injected engine: no native audio library is loaded)
// ---------------------------------------------------------------------------------------------------------

class FakeCapture implements CaptureStreamLike {
  readonly readable: ReadableStream<Float32Array>
  readonly closed: Promise<void>
  stats = { framesDropped: 0n }
  stops = 0
  disposes = 0
  private controller: ReadableStreamDefaultController<Float32Array> | null = null
  private resolveClosed: () => void = () => undefined

  constructor(readonly options: Parameters<AudioEngineLike["openCapture"]>[0]) {
    this.readable = new ReadableStream<Float32Array>({ start: (c) => void (this.controller = c) })
    this.closed = new Promise((resolve) => (this.resolveClosed = resolve))
  }

  push(chunk: Float32Array): void {
    this.controller?.enqueue(chunk)
  }

  error(message: string): void {
    this.controller?.error(new Error(message))
    this.resolveClosed()
  }

  stop(): void {
    this.stops += 1
    this.controller?.close()
    this.resolveClosed()
  }

  dispose(): void {
    this.disposes += 1
    try {
      this.controller?.close()
    } catch {
      // already closed
    }
    this.resolveClosed()
  }

  getStats(): { framesDropped: bigint } {
    return this.stats
  }
}

class FakeEngine implements AudioEngineLike {
  readonly sampleRate = 16_000
  readonly captures: FakeCapture[] = []
  selected: number | null = null
  clears = 0
  disposed = false

  constructor(readonly devices: CaptureDevice[] | null = [{ index: 0, name: "Built-in Audio Analog Stereo", isDefault: true }]) {}

  async openCapture(options: Parameters<AudioEngineLike["openCapture"]>[0]): Promise<CaptureStreamLike> {
    const capture = new FakeCapture(options)
    this.captures.push(capture)
    return capture
  }

  listCaptureDevices(): readonly CaptureDevice[] | null {
    return this.devices
  }

  selectCaptureDevice(index: number): boolean {
    this.selected = index
    return true
  }

  clearCaptureDeviceSelection(): void {
    this.clears += 1
    this.selected = null
  }

  dispose(): void {
    this.disposed = true
  }
}

describe("OpenTUI recorder", () => {
  test("the 5.5 engine and capture options; Float32 chunks to WAV; stop drains; dropped frames are reported", async () => {
    expect(ENGINE_OPTIONS).toEqual({ sampleRate: 16_000, autoStart: false })
    expect(CAPTURE_OPTIONS).toEqual({ channels: 1, chunkFrames: 320, capacityFrames: 16_000 })
    const clock = new ManualClock()
    const engine = new FakeEngine()
    const recorder = createOpenTuiRecorder({ clock, createEngine: () => engine })
    expect(await recorder.available()).toEqual({ ok: true })
    const cb = recorderCallbacks()
    const handle = (await recorder.start(cb.options())) as CaptureHandle
    const capture = engine.captures[0] as FakeCapture
    expect(capture.options).toEqual({ channels: 1, chunkFrames: 320, capacityFrames: 16_000, signal: cb.abort.signal })
    expect(engine.clears).toBe(1)
    expect(handle.deviceName).toBe("Built-in Audio Analog Stereo")

    const chunks = [tone(20, 0.5), tone(20, 0.5), tone(20, 0.5)]
    for (const chunk of chunks) capture.push(chunk)
    await flushAsync()
    expect(cb.state.firstChunks).toBe(1)
    expect(cb.levels).toHaveLength(3)
    capture.stats = { framesDropped: 64n }
    const clip: WavClip = await handle.stop()
    expect(capture.stops).toBe(1)
    expect(clip.durationMs).toBe(60)
    expect(clip.droppedFrames).toBe(64)
    expect(chunks.every((chunk) => chunk.every((s) => s === 0))).toBe(true)
    expect(parseWav(clip.wav).samples.length).toBe(960)
  })

  test("a stored device is selected by name; an unknown one falls back to the default source", async () => {
    const clock = new ManualClock()
    const engine = new FakeEngine([
      { index: 0, name: "Built-in Audio Analog Stereo", isDefault: true },
      { index: 1, name: "C922 Pro Stream Webcam Analog Stereo", isDefault: false },
    ])
    const recorder = createOpenTuiRecorder({ clock, createEngine: () => engine })
    expect((await recorder.listDevices()).map((d) => d.name)).toEqual(["Built-in Audio Analog Stereo", "C922 Pro Stream Webcam Analog Stereo"])
    const picked = (await recorder.start(recorderCallbacks().options({ device: "c922" }))) as CaptureHandle
    expect(engine.selected).toBe(1)
    expect([picked.deviceName, picked.deviceFallback]).toEqual(["C922 Pro Stream Webcam Analog Stereo", false])
    picked.cancel()
    const fallback = (await recorder.start(recorderCallbacks().options({ device: "Blue Yeti" }))) as CaptureHandle
    expect(engine.selected).toBeNull()
    expect([fallback.deviceName, fallback.deviceFallback]).toEqual(["Built-in Audio Analog Stereo", true])
  })

  test("cancel disposes the capture and zero-fills; a stream error is reported once as a failure", async () => {
    const clock = new ManualClock()
    const engine = new FakeEngine()
    const recorder = createOpenTuiRecorder({ clock, createEngine: () => engine })
    const cancelled = recorderCallbacks()
    const handle = await recorder.start(cancelled.options())
    engine.captures[0]?.push(tone(20))
    await flushAsync()
    handle.cancel()
    await flushAsync()
    expect(engine.captures[0]?.disposes).toBe(1)
    expect(cancelled.failures).toEqual([])
    expect(await handle.stop().catch((e: unknown) => (isVoiceError(e) ? e.kind : "other"))).toBe("cancelled")

    const failing = recorderCallbacks()
    await recorder.start(failing.options())
    engine.captures[1]?.error("Audio capture stream read failed")
    await flushAsync()
    expect(failing.failures.map((e) => e.message)).toEqual(["the audio capture failed: Audio capture stream read failed"])
  })

  test("an engine that cannot be created leaves the recorder unavailable; no devices is unavailable too", async () => {
    const clock = new ManualClock()
    const broken = createOpenTuiRecorder({
      clock,
      createEngine: () => {
        throw new Error("Failed to resolve the native audio library")
      },
    })
    expect(await broken.available()).toEqual({ ok: false, reason: "OpenTUI audio is unavailable: Failed to resolve the native audio library" })
    const error = await broken.start(recorderCallbacks().options()).catch((e: unknown) => e)
    expect(isVoiceError(error) && error.kind).toBe("recorder")

    const empty = createOpenTuiRecorder({ clock, createEngine: () => new FakeEngine([]) })
    expect(await empty.available()).toEqual({ ok: false, reason: "no capture device" })
    const engine = new FakeEngine()
    const disposable = createOpenTuiRecorder({ clock, createEngine: () => engine })
    disposable.dispose()
    expect(engine.disposed).toBe(true)
  })

  test("an aborted signal cancels a start", async () => {
    const clock = new ManualClock()
    const recorder = createOpenTuiRecorder({ clock, createEngine: () => new FakeEngine() })
    const cb = recorderCallbacks()
    cb.abort.abort(cancelledError())
    const error = await recorder.start(cb.options()).catch((e: unknown) => e)
    expect(isVoiceError(error) && error.kind).toBe("cancelled")
  })
})
