/**
 * The voice controller's recording path and transcription queue (design 5.4, 5.5, 5.7; cases of 7.5): the real
 * reducer and store, a fake recorder (no microphone) and scripted sources; time is a ManualClock.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { ManualClock } from "@opentui/core/testing"
import { DEFAULT_VOICE_CONFIG, type ProfileState, type VoiceConfig } from "../../src/config/types.ts"
import type { Action } from "../../src/state/actions.ts"
import { createInitialState, type Store } from "../../src/state/model.ts"
import { createStore } from "../../src/state/store.ts"
import { VoiceError } from "../../src/voice/errors.ts"
import { createVoiceController, VOICE_TIMING } from "../../src/voice/manager.ts"
import { createParecRecorder } from "../../src/voice/recorder-parec.ts"
import type { Recorder, VoiceController, VoiceUiState } from "../../src/voice/types.ts"
import { createFakeSpawn, type FakeChildProcess, type FakeSpawn } from "../helpers/fake-spawn.ts"
import { FakeRecorder, type FakeRecorderOptions } from "../mocks/fake-recorder.ts"
import { ALL_VOICE_ACKS, FakeComposer, FakeSource, FakeStateFile, flushAsync, isZeroed } from "../mocks/fake-sources.ts"

interface Rig {
  clock: ManualClock
  store: Store
  actions: Action[]
  composer: FakeComposer
  stateFile: FakeStateFile
  recorder: FakeRecorder
  sources: FakeSource[]
  voice: VoiceController
}

interface RigOptions {
  sources?: (clock: ManualClock) => FakeSource[]
  config?: Partial<VoiceConfig>
  state?: ProfileState
  recorder?: Partial<FakeRecorderOptions>
  extraRecorders?: (clock: ManualClock) => Recorder[]
  /** The wall clock; default the ManualClock itself. */
  wallNow?: (clock: ManualClock) => () => number
}

const cleanups: (() => Promise<void>)[] = []

afterEach(async () => {
  for (const cleanup of cleanups.splice(0)) await cleanup()
})

function rig(opts: RigOptions = {}): Rig {
  const clock = new ManualClock()
  const base = createStore(createInitialState({ profile: "test", tenant: null, view: { width: 80, height: 24 }, at: 0 }))
  const actions: Action[] = []
  const store: Store = {
    getState: base.getState,
    subscribe: base.subscribe,
    dispatch: (action) => {
      actions.push(action)
      base.dispatch(action)
    },
    dispatchMany: (list) => {
      actions.push(...list)
      base.dispatchMany(list)
    },
  }
  const composer = new FakeComposer()
  const stateFile = new FakeStateFile({ voiceAck: { ...ALL_VOICE_ACKS }, ...opts.state })
  const recorder = new FakeRecorder({ clock, ...opts.recorder })
  const sources = opts.sources?.(clock) ?? [new FakeSource({ id: "lunarvoice", clock })]
  const config: VoiceConfig = { ...DEFAULT_VOICE_CONFIG, ...opts.config }
  const recorders: Recorder[] = [recorder, ...(opts.extraRecorders?.(clock) ?? [])]
  // By default the ManualClock doubles as the wall clock of the voiceAck stamps.
  const wallNow = opts.wallNow?.(clock) ?? (() => clock.now())
  const voice = createVoiceController({ store, clock, wallNow, composer, recorders, sources, config, stateFile })
  cleanups.push(() => voice.dispose())
  return { clock, store, actions, composer, stateFile, recorder, sources, voice }
}

const press = (r: Rig) => r.voice.pttKey({ eventType: "press", repeated: false })
const voice = (r: Rig): VoiceUiState => r.store.getState().voice
const notices = (r: Rig): string[] => r.store.getState().notices.map((n) => n.text)
const source = (r: Rig, index = 0): FakeSource => {
  const found = r.sources[index]
  if (found === undefined) throw new Error(`no source ${index}`)
  return found
}

/** Toggle dictation: tap, `ms` of speech, tap. The clip is queued (and its chain started) when this resolves. */
async function dictate(r: Rig, ms = 1_000): Promise<void> {
  press(r)
  await flushAsync()
  r.recorder.play(ms)
  press(r)
  await flushAsync()
}

describe("start", () => {
  test("press starts capture without awaiting probes", async () => {
    const r = rig({ sources: (clock) => [new FakeSource({ id: "lunarvoice", clock, probeDelayMs: "never" })] })
    press(r)
    expect(r.recorder.startCalls).toHaveLength(1)
    expect(voice(r)).toMatchObject({ phase: "arming", mode: "provisional", trigger: "key", captureLive: false })
    await flushAsync()
    // The probe started with the controller and is still pending.
    expect(source(r).probes).toBe(1)
    expect(voice(r).availability.lunarvoice).toBeUndefined()
    r.recorder.play(100)
    expect(voice(r)).toMatchObject({ phase: "recording", captureLive: true, recorder: "opentui" })
    expect(voice(r).startedAt).toBe(0)
  })

  test("offline: PTT start says voice unavailable and opens no recorder", async () => {
    const r = rig({ sources: (clock) => [new FakeSource({ id: "lunarvoice", clock, available: false, reason: "not connected" })] })
    await flushAsync()
    press(r)
    expect(r.recorder.startCalls).toHaveLength(0)
    expect(notices(r)).toContain("voice unavailable: lunarvoice not connected")
    expect(voice(r).phase).toBe("idle")
  })

  test("dictation is refused in credential mode and with --no-voice", async () => {
    const r = rig()
    await flushAsync()
    r.store.dispatch({ type: "evt", event: { kind: "turn.auth_required", turnId: null, extensionName: "github" }, at: 0 })
    press(r)
    expect(r.recorder.startCalls).toHaveLength(0)
    expect(notices(r)).toContain("dictation is off while a credential is requested")

    const off = rig({ config: { enabled: false } })
    press(off)
    expect(off.recorder.startCalls).toHaveLength(0)
    expect(notices(off)).toContain("voice is off (--no-voice)")
    expect(source(off).probes).toBe(0)
  })

  test("a recording running when credential mode starts is cancelled", async () => {
    const r = rig()
    await flushAsync()
    press(r)
    await flushAsync()
    r.recorder.play(500)
    r.store.dispatch({ type: "evt", event: { kind: "turn.auth_required", turnId: null, extensionName: "github" }, at: r.clock.now() })
    await flushAsync()
    expect(voice(r).phase).toBe("idle")
    expect(r.recorder.cancels).toBe(1)
    expect(notices(r)).toContain("dictation stopped: a credential is requested")
  })

  test("a recorder that fails to start falls back to the next recorder", async () => {
    const r = rig({
      recorder: { startError: "the microphone could not be opened: busy" },
      extraRecorders: (clock) => [new FakeRecorder({ clock, kind: "parec" })],
    })
    await flushAsync()
    press(r)
    await flushAsync()
    expect(r.recorder.startCalls).toHaveLength(1)
    expect(voice(r).recorder).toBe("parec")
  })
})

describe("queue", () => {
  test("a second dictation during transcription inserts in order", async () => {
    const r = rig({ sources: (clock) => [new FakeSource({ id: "lunarvoice", clock, replies: [{ manual: true }, { manual: true }] })] })
    await flushAsync()
    await dictate(r)
    const lv = source(r)
    expect(lv.calls).toHaveLength(1)
    expect(voice(r).queue.map((q) => [q.state, q.source])).toEqual([["transcribing", "lunarvoice"]])
    await dictate(r)
    expect(voice(r).queue.map((q) => q.state)).toEqual(["transcribing", "waiting"])
    expect(lv.calls).toHaveLength(1)

    lv.pending[0]?.resolve("first")
    await flushAsync()
    expect(r.composer.text).toBe("first")
    expect(lv.calls).toHaveLength(2)
    lv.pending[1]?.resolve("second")
    await flushAsync()
    expect(r.composer.text).toBe("first second")
    expect(r.composer.inserts).toEqual([
      { text: "first", separate: true },
      { text: "second", separate: true },
    ])
    expect(voice(r).queue).toEqual([])
    expect(lv.calls.every((call) => isZeroed(call.clip.wav))).toBe(true)
    expect(notices(r)).toContain("transcribed 1 s in 0.0 s (lunarvoice)")
  })

  test("a fourth clip is refused with queue full", async () => {
    const r = rig({ sources: (clock) => [new FakeSource({ id: "lunarvoice", clock, defaultReply: { manual: true } })] })
    await flushAsync()
    await dictate(r)
    await dictate(r)
    await dictate(r)
    expect(voice(r).queue.map((q) => q.state)).toEqual(["transcribing", "waiting", "waiting"])
    press(r)
    expect(r.recorder.startCalls).toHaveLength(3)
    expect(notices(r)).toContain("transcription queue full; wait or press Esc")
    expect(voice(r).phase).toBe("idle")
  })

  test("Esc while idle cancels the queue, then drops the retained clip", async () => {
    const r = rig({ sources: (clock) => [new FakeSource({ id: "lunarvoice", clock, defaultReply: { manual: true } })] })
    await flushAsync()
    await dictate(r)
    await dictate(r)
    const lv = source(r)
    lv.pending[0]?.reject(new VoiceError("timeout", "scripted timeout", { source: "lunarvoice" }))
    await flushAsync()
    expect(voice(r).retained).toMatchObject({ reason: "timeout", retryAt: null })
    expect(voice(r).queue.map((q) => q.state)).toEqual(["transcribing"])
    const inFlight = lv.calls[1]
    if (inFlight === undefined) throw new Error("the second clip was not sent")

    expect(r.voice.escape()).toBe(true)
    expect(inFlight.signal.aborted).toBe(true)
    expect(isZeroed(inFlight.clip.wav)).toBe(true)
    expect(voice(r).queue).toEqual([])
    expect(voice(r).retained).not.toBeNull()

    expect(r.voice.escape()).toBe(true)
    expect(voice(r).retained).toBeNull()
    expect(isZeroed(lv.calls[0]?.clip.wav ?? new Uint8Array(1).fill(1))).toBe(true)
    expect(r.voice.escape()).toBe(false)
    await flushAsync()
    expect(r.composer.text).toBe("")
  })

  test("Esc during a recording cancels it and nothing is queued", async () => {
    const r = rig()
    await flushAsync()
    press(r)
    await flushAsync()
    r.recorder.play(600)
    expect(r.voice.escape()).toBe(true)
    expect(voice(r)).toMatchObject({ phase: "idle", captureLive: false, recorder: null, level: null })
    expect(r.recorder.cancels).toBe(1)
    expect(notices(r)).toContain("dictation cancelled")
    await flushAsync()
    expect(source(r).calls).toHaveLength(0)
  })
})

describe("local discard and levels", () => {
  test("a clip without signal is discarded with the device name", async () => {
    const r = rig()
    await flushAsync()
    press(r)
    await flushAsync()
    r.recorder.playQuiet(1_000, 0.001)
    press(r)
    await flushAsync()
    expect(source(r).calls).toHaveLength(0)
    expect(notices(r)).toContain("no signal from Built-in Audio Analog Stereo - /voice device")
    expect(r.recorder.recordings[0]?.clip?.wav.every((b) => b === 0)).toBe(true)
  })

  test("a silent toggle recording is cancelled after 8 s", async () => {
    const r = rig()
    await flushAsync()
    press(r)
    await flushAsync()
    r.recorder.playQuiet(8_200)
    expect(voice(r).phase).toBe("idle")
    expect(r.recorder.cancels).toBe(1)
    expect(notices(r)).toContain("no signal from Built-in Audio Analog Stereo - /voice device")
  })

  test("a click under 300 ms is too short", async () => {
    const r = rig()
    await flushAsync()
    r.voice.micDown()
    await flushAsync()
    r.recorder.play(100)
    r.voice.micUp()
    r.recorder.play(100)
    r.voice.micDown()
    await flushAsync()
    expect(voice(r).phase).toBe("idle")
    expect(notices(r)).toContain("too short to transcribe (under 0.3 s)")
    expect(source(r).calls).toHaveLength(0)
  })

  test("levels are dispatched throttled (15 Hz) and cleared when the recording ends", async () => {
    const r = rig()
    await flushAsync()
    press(r)
    await flushAsync()
    r.recorder.play(1_000)
    const levels = r.actions.filter((a) => a.type === "voice/level" && a.level !== null)
    expect(levels).toHaveLength(15)
    expect(voice(r).level?.rmsDb).toBeCloseTo(-13.5, 0)
    press(r)
    await flushAsync()
    expect(voice(r).level).toBeNull()
  })

  test("dropped frames raise one warning and the clip is still sent", async () => {
    const r = rig({ recorder: { droppedFrames: 320 } })
    await flushAsync()
    await dictate(r)
    expect(notices(r)).toContain("audio dropped: the UI was busy")
    expect(source(r).calls).toHaveLength(1)
  })
})

describe("devices, privacy and status", () => {
  test("/voice device persists voiceDevice and the next recording uses it", async () => {
    const r = rig({
      recorder: {
        devices: [
          { index: 0, name: "Built-in Audio Analog Stereo", isDefault: true },
          { index: 1, name: "C922 Pro Stream Webcam Analog Stereo", isDefault: false },
        ],
      },
    })
    await flushAsync()
    expect((await r.voice.listDevices()).map((d) => d.index)).toEqual([0, 1])
    expect(await r.voice.selectDevice("blue yeti")).toBe(false)
    expect(await r.voice.selectDevice("c922")).toBe(true)
    expect(r.stateFile.state.voiceDevice).toBe("C922 Pro Stream Webcam Analog Stereo")
    expect(r.voice.status().device).toEqual({ name: "C922 Pro Stream Webcam Analog Stereo", origin: "state" })
    press(r)
    expect(r.recorder.startCalls[0]?.device).toBe("C922 Pro Stream Webcam Analog Stereo")
    expect(r.voice.escape()).toBe(true)

    // An empty name restores the default source.
    expect(await r.voice.selectDevice("")).toBe(true)
    expect(r.stateFile.state.voiceDevice).toBeUndefined()
    expect(r.voice.status().device).toEqual({ name: null, origin: "default" })
    press(r)
    expect(r.recorder.startCalls[1]?.device).toBeUndefined()
  })

  test("parec: a configured source it cannot open is retried on the default source, with the fallback notice", async () => {
    let fake: FakeSpawn | null = null
    const r = rig({
      config: { device: "alsa_input.usb-gone-00.analog-stereo" },
      recorder: { startError: "OpenTUI audio is unavailable" },
      extraRecorders: (clock) => {
        fake = createFakeSpawn({ clock })
        fake.once("parec", { stderr: ["Stream error: No such entity\n"], exitCode: 1 })
        fake.once("parec", { stayAlive: true })
        return [createParecRecorder({ clock, env: { PATH: "/usr/bin" }, spawn: fake.spawn, which: () => "/usr/bin/parec" })]
      },
    })
    await flushAsync()
    press(r)
    await flushAsync()
    const spawned = (fake as FakeSpawn | null)?.calls ?? []
    expect(spawned.map((call) => call.argv.some((arg) => arg.startsWith("--device=")))).toEqual([true, false])
    expect(notices(r)).toContain('capture device "alsa_input.usb-gone-00.analog-stereo" not found; using the default source - /voice device')
    expect(notices(r).some((text) => text.startsWith("voice unavailable"))).toBe(false)
    const child = spawned[1]?.process as FakeChildProcess
    // half a second of digital silence: the clip is discarded, naming the source that really recorded it
    child.pushStdout(new Uint8Array(16_000))
    await flushAsync()
    expect(voice(r)).toMatchObject({ phase: "recording", captureLive: true, recorder: "parec" })
    // past the key-repeat window the second tap stops the toggle recording
    r.clock.advance(1_000)
    await flushAsync()
    press(r)
    await flushAsync()
    await flushAsync()
    expect(notices(r)).toContain("no signal from the default source - /voice device")
  })

  test("parec: an OpenTUI device name is never passed to --device; the default source records with the notice", async () => {
    let fake: FakeSpawn | null = null
    const r = rig({
      state: { voiceDevice: "C922 Pro Stream Webcam Analog Stereo" },
      recorder: { startError: "OpenTUI audio is unavailable" },
      extraRecorders: (clock) => {
        fake = createFakeSpawn({ clock })
        fake.on("parec", { stayAlive: true })
        const listSources = async () => ["alsa_input.usb-046d_C922_Pro_Stream_Webcam-02.analog-stereo"]
        return [createParecRecorder({ clock, env: { PATH: "/usr/bin" }, spawn: fake.spawn, which: () => "/usr/bin/parec", listSources })]
      },
    })
    await flushAsync()
    press(r)
    await flushAsync()
    const spawned = (fake as FakeSpawn | null)?.calls ?? []
    expect(spawned).toHaveLength(1)
    expect(spawned[0]?.argv.some((arg) => arg.startsWith("--device"))).toBe(false)
    expect(notices(r)).toContain('capture device "C922 Pro Stream Webcam Analog Stereo" not found; using the default source - /voice device')
  })

  test("a stored device wins over [voice] device", async () => {
    const r = rig({ config: { device: "Built-in" }, state: { voiceDevice: "C922" } })
    await flushAsync()
    expect(r.voice.status().device).toEqual({ name: "C922", origin: "state" })
    const plain = rig({ config: { device: "Built-in" } })
    await flushAsync()
    expect(plain.voice.status().device).toEqual({ name: "Built-in", origin: "config" })
  })

  test("the first use of a source shows its privacy note once and records voiceAck", async () => {
    const r = rig({ state: { voiceAck: {} } })
    await flushAsync()
    await dictate(r)
    await dictate(r)
    const privacy = r.actions.filter((a) => a.type === "notice/push" && a.notice.id === "voice-privacy-lunarvoice")
    expect(privacy).toHaveLength(1)
    expect(notices(r)).toContain("fake lunarvoice: privacy note of lunarvoice")
    expect(r.stateFile.state.voiceAck?.lunarvoice).toBe(new Date(r.clock.now() - 1_000).toISOString())
  })

  test("the voiceAck stamp is wall-clock time even when the Clock counts from process start", async () => {
    // Production's Clock is performance.now(); new Date(clock.now()) wrote 1970 stamps into the state file.
    const r = rig({ state: { voiceAck: {} }, wallNow: (clock) => () => Date.UTC(2026, 10, 3, 9, 0, 0) + clock.now() })
    await flushAsync()
    await dictate(r)
    const stamp = Date.parse(r.stateFile.state.voiceAck?.lunarvoice ?? "")
    expect(stamp - Date.UTC(2026, 10, 3, 9, 0, 0)).toBeGreaterThanOrEqual(0)
    expect(stamp - Date.UTC(2026, 10, 3, 9, 0, 0)).toBeLessThanOrEqual(r.clock.now())
  })

  test("status lists every source id, the build, the order and the duration cap", async () => {
    const r = rig({ config: { maxDurationMs: 600_000 } })
    await flushAsync()
    const status = r.voice.status()
    expect(status.sources.map((s) => [s.id, s.inBuild])).toEqual([
      ["lunarvoice", true],
      ["hex-app", false],
      ["hex-embedded", false],
    ])
    expect(status.sources[0]?.availability).toMatchObject({ available: true })
    expect(status.recorder).toEqual({ kind: "opentui" })
    expect(status.maxDurationMs).toBe(290_000)
    expect(status.order).toEqual(["lunarvoice", "hex-app", "hex-embedded"])
    r.voice.use("hex-app")
    expect(notices(r)).toContain("hex-app is not in this build")
    expect(r.voice.status().pinned).toBeNull()
    r.voice.use("lunarvoice")
    expect(r.voice.status().pinned).toBe("lunarvoice")
    r.voice.use("auto")
    expect(r.voice.status().pinned).toBeNull()
  })

  test("probes run again when the connection changes and every 30 s", async () => {
    const r = rig()
    await flushAsync()
    expect(source(r).probes).toBe(1)
    r.store.dispatch({ type: "conn/offline", reason: "test", fatal: false, at: 0 })
    await flushAsync()
    expect(source(r).probes).toBe(2)
    r.clock.advance(VOICE_TIMING.probeIntervalMs)
    await flushAsync()
    expect(source(r).probes).toBe(3)
  })

  test("dispose cancels the recording and disposes sources and the queue", async () => {
    const r = rig({ sources: (clock) => [new FakeSource({ id: "lunarvoice", clock, defaultReply: { manual: true } })] })
    await flushAsync()
    await dictate(r)
    press(r)
    await flushAsync()
    r.recorder.play(200)
    await r.voice.dispose()
    expect(source(r).disposed).toBe(true)
    expect(source(r).calls[0]?.signal.aborted).toBe(true)
    expect(isZeroed(source(r).calls[0]?.clip.wav ?? new Uint8Array(1).fill(1))).toBe(true)
    expect(r.recorder.cancels).toBe(1)
    expect(r.clock.pendingTimerCount).toBe(0)
  })
})
