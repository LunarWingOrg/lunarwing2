/**
 * The per-clip fallback chain, the retained clip and the LunarVoice source (design 5.3.1, 5.4, 5.4.1; cases of
 * 11.3.2). LunarVoice is the real source over a scripted Transport (TransportErrors exactly as
 * transport/gateway.ts produces them from voice.rs answers); the other candidates are scripted sources with the
 * Hex ids. Time is a ManualClock; no network and no microphone.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { ManualClock } from "@opentui/core/testing"
import { DEFAULT_VOICE_CONFIG, type ProfileState, type VoiceConfig } from "../../src/config/types.ts"
import type { Action } from "../../src/state/actions.ts"
import { createInitialState, type Store } from "../../src/state/model.ts"
import { createStore } from "../../src/state/store.ts"
import { TransportError } from "../../src/transport/errors.ts"
import type {
  Capabilities,
  Limits,
  SendResult,
  ServerInfo,
  TranscribeRequest,
  TranscribeResult,
  Transport,
  TransportEvent,
} from "../../src/transport/types.ts"
import { BUSY_RETRY_DELAYS_MS, FALLBACK_STEPS, isVoiceError } from "../../src/voice/errors.ts"
import { createVoiceController, HEX_MODEL_HINT, VOICE_TIMING } from "../../src/voice/manager.ts"
import { createVoiceSources } from "../../src/voice/sources/index.ts"
import { createLunarVoiceSource, lunarVoiceErrorFromTransport, RateWindow, voiceErrorKindForReplCode } from "../../src/voice/sources/lunarvoice.ts"
import type { VoiceController, VoiceSource, VoiceUiState, WavClip } from "../../src/voice/types.ts"
import { createWavClip } from "../../src/voice/wav.ts"
import { createFakeSpawn } from "../helpers/fake-spawn.ts"
import { makeTempDir } from "../helpers/tmp.ts"
import { FakeRecorder } from "../mocks/fake-recorder.ts"
import { ALL_VOICE_ACKS, FakeComposer, FakeSource, FakeStateFile, flushAsync, isZeroed } from "../mocks/fake-sources.ts"

const MIB = 1024 * 1024

// ---------------------------------------------------------------------------------------------------------
// A Transport whose transcribe() follows a script (the rest is never called by the voice code)
// ---------------------------------------------------------------------------------------------------------

type ScriptStep = TranscribeResult | TransportError | "hang"

class ScriptedTransport implements Transport {
  readonly kind = "gateway" as const
  readonly requests: TranscribeRequest[] = []
  /** Copies of the WAV bytes at call time (the clip is zero-filled once it is done). */
  readonly bytes: Uint8Array[] = []
  private readonly script: ScriptStep[] = []

  reply(...steps: ScriptStep[]): this {
    this.script.push(...steps)
    return this
  }

  transcribe(req: TranscribeRequest): Promise<TranscribeResult> {
    this.requests.push(req)
    this.bytes.push(req.wav.slice())
    const step = this.script.shift() ?? { text: "hello from lunarvoice", language: "en", model: "parakeet" }
    if (step === "hang") {
      return new Promise((_, reject) => req.signal.addEventListener("abort", () => reject(req.signal.reason), { once: true }))
    }
    return step instanceof TransportError ? Promise.reject(step) : Promise.resolve(step)
  }

  async connect(): Promise<ServerInfo> {
    throw new Error("not used by voice tests")
  }

  events(): AsyncIterable<TransportEvent> {
    return { [Symbol.asyncIterator]: () => ({ next: async () => ({ done: true as const, value: undefined }) }) }
  }

  async send(): Promise<SendResult> {
    throw new Error("not used by voice tests")
  }

  async interrupt(): Promise<void> {}
  async steer(): Promise<void> {}
  async approve(): Promise<void> {}
  async close(): Promise<void> {}
}

function serverInfo(limits: Partial<Limits> = {}, capabilities: Partial<Capabilities> = {}): ServerInfo {
  return {
    transport: "gateway",
    protocol: "gateway-chat",
    protocolVersion: 1,
    serverVersion: "test",
    agentName: null,
    engine: "v2",
    sessionId: null,
    threadId: null,
    endpointLabel: "http://127.0.0.1:3000",
    credential: "client",
    capabilities: {
      streaming: true,
      activity: true,
      images: true,
      generatedImages: true,
      approvals: true,
      transcribe: true,
      correlation: true,
      lifecycle: true,
      interrupt: "text",
      steer: "text",
      threads: true,
      history: true,
      backgroundMessages: true,
      resume: true,
      ...capabilities,
    },
    limits: {
      maxContentBytes: 100_000,
      maxImages: 5,
      maxImageBytes: 5 * MIB,
      maxTotalImageBytes: 7 * MIB,
      maxRequestBytes: 10 * MIB,
      imageMimeTypes: ["image/png"],
      maxTranscribeWavBytes: 18 * MIB,
      transcribePerMinute: 10,
      chatPerMinute: 30,
      maxInFlight: 8,
      ...limits,
    },
    warnings: [],
  }
}

const busy = () => new TransportError("busy", "POST /api/voice/transcribe: the voice sidecar is busy (HTTP 503): voice sidecar is unavailable", { status: 503 })

// ---------------------------------------------------------------------------------------------------------
// Rig
// ---------------------------------------------------------------------------------------------------------

interface Rig {
  clock: ManualClock
  store: Store
  actions: Action[]
  composer: FakeComposer
  recorder: FakeRecorder
  transport: ScriptedTransport
  hex: Map<string, FakeSource>
  voice: VoiceController
}

interface RigOptions {
  order?: VoiceConfig["order"]
  limits?: Partial<Limits>
  /** The Hex-id candidates (scripted). */
  hex?: (clock: ManualClock) => FakeSource[]
  /** Replace the real LunarVoice by a scripted source. */
  lunarvoice?: (clock: ManualClock) => FakeSource
  state?: ProfileState
  connected?: boolean
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
  if (opts.connected !== false) store.dispatch({ type: "conn/ready", info: serverInfo(opts.limits), at: 0 })
  const transport = new ScriptedTransport()
  const lunarvoice: VoiceSource = opts.lunarvoice?.(clock) ?? createLunarVoiceSource({ store, clock, transport: () => transport })
  const hexSources = opts.hex?.(clock) ?? []
  const composer = new FakeComposer()
  const recorder = new FakeRecorder({ clock })
  const config: VoiceConfig = { ...DEFAULT_VOICE_CONFIG, order: opts.order ?? ["lunarvoice", "hex-app", "hex-embedded"] }
  const voice = createVoiceController({
    store,
    clock,
    composer,
    recorders: [recorder],
    sources: [lunarvoice, ...hexSources],
    config,
    stateFile: new FakeStateFile({ voiceAck: { ...ALL_VOICE_ACKS }, ...opts.state }),
  })
  cleanups.push(() => voice.dispose())
  return { clock, store, actions, composer, recorder, transport, hex: new Map(hexSources.map((s) => [s.id, s])), voice }
}

const press = (r: Rig) => r.voice.pttKey({ eventType: "press", repeated: false })
const voiceState = (r: Rig): VoiceUiState => r.store.getState().voice
const resultNotices = (r: Rig) =>
  r.actions.flatMap((a) => (a.type === "notice/push" && a.notice.id === "voice-result" ? [`${a.notice.level}: ${a.notice.text}`] : []))
const noticeTexts = (r: Rig) => r.store.getState().notices.map((n) => n.text)

function hexSource(r: Rig, id: string): FakeSource {
  const found = r.hex.get(id)
  if (found === undefined) throw new Error(`no fake ${id}`)
  return found
}

/** Toggle dictation of `ms` of speech; resolves once the clip is queued and its chain has started. */
async function dictate(r: Rig, ms = 1_000): Promise<void> {
  press(r)
  await flushAsync()
  r.recorder.play(ms)
  press(r)
  await flushAsync()
}

/**
 * Advances the clock in steps, letting the chain's continuations run between them. Steps stay well below the 1 s
 * probe budget: one big ManualClock jump would fire a probe's budget timer before the probe's own microtasks.
 */
async function advance(r: Rig, ms: number, step = 500): Promise<void> {
  for (let done = 0; done < ms; done += step) {
    r.clock.advance(Math.min(step, ms - done))
    await flushAsync()
  }
}

// ---------------------------------------------------------------------------------------------------------

describe("busy", () => {
  test("busy LunarVoice is retried after 1, 2, 4 s and then falls back to the next source with the same WAV", async () => {
    const r = rig({ hex: (clock) => [new FakeSource({ id: "hex-app", clock, replies: [{ text: "check the ports registry" }] })] })
    r.transport.reply(busy(), busy(), busy(), busy())
    await flushAsync()
    await dictate(r)
    expect(r.transport.requests).toHaveLength(1)
    const sent = r.clock.now()
    expect(voiceState(r).queue[0]).toMatchObject({ state: "waiting", source: "lunarvoice", attempt: 1, startedAt: sent + 1_000 })

    const times: number[] = [sent]
    for (const delay of BUSY_RETRY_DELAYS_MS) {
      r.clock.advance(delay - 1)
      await flushAsync()
      expect(r.transport.requests).toHaveLength(times.length)
      r.clock.advance(1)
      await flushAsync()
      times.push(r.clock.now())
    }
    expect(r.transport.requests).toHaveLength(4)
    expect(times.slice(1).map((t, i) => t - (times[i] ?? 0))).toEqual([1_000, 2_000, 4_000])
    const wavs = r.transport.requests.map((req) => req.wav)
    expect(wavs.every((wav) => wav === wavs[0])).toBe(true)
    const hex = hexSource(r, "hex-app")
    expect(hex.calls).toHaveLength(1)
    expect(hex.calls[0]?.wav).toBe(wavs[0] as Uint8Array)
    expect(r.composer.text).toBe("check the ports registry")
    expect(resultNotices(r)).toEqual(["warn: transcribed by hex-app (lunarvoice: busy)"])
    expect(voiceState(r).lastAttempts.map((a) => [a.source, a.outcome])).toEqual([
      ["lunarvoice", "busy"],
      ["lunarvoice", "busy"],
      ["lunarvoice", "busy"],
      ["lunarvoice", "busy"],
      ["hex-app", "ok"],
    ])
  })

  test("busy exhausted on the last source keeps the clip and Alt+R resends the same WAV", async () => {
    const r = rig({ order: ["lunarvoice"] })
    r.transport.reply(busy(), busy(), busy(), busy())
    await flushAsync()
    await dictate(r)
    await advance(r, 7_000)
    expect(r.transport.requests).toHaveLength(4)
    expect(voiceState(r).retained).toMatchObject({ reason: "busy", durationMs: 1_000, retryAt: null })
    expect(resultNotices(r)).toEqual(["warn: voice failed (Alt+R retry, Esc discard): lunarvoice busy (3 retries)"])
    expect(r.voice.retry()).toBe(true)
    await flushAsync()
    expect(r.transport.requests).toHaveLength(5)
    expect(r.transport.requests[4]?.wav).toBe(r.transport.requests[0]?.wav as Uint8Array)
    expect(r.transport.bytes[4]).toEqual(r.transport.bytes[0] as Uint8Array)
    expect(isZeroed(r.transport.bytes[4] ?? new Uint8Array(0))).toBe(false)
    expect(r.composer.text).toBe("hello from lunarvoice")
    expect(voiceState(r).retained).toBeNull()
    expect(r.voice.retry()).toBe(false)
  })
})

describe("rate limits", () => {
  test("429 keeps the clip and the retry waits for the window", async () => {
    const r = rig({ order: ["lunarvoice"] })
    r.transport.reply(
      new TransportError("rate_limited", "POST /api/voice/transcribe: rate limited (HTTP 429): too many voice requests", { status: 429, retryAfterMs: 60_000 }),
    )
    await flushAsync()
    await dictate(r)
    const failedAt = r.clock.now()
    expect(voiceState(r).retained).toMatchObject({ reason: "rate_limited", retryAt: failedAt + 60_000 })
    expect(resultNotices(r)).toEqual(["warn: voice failed (Alt+R retry, Esc discard): lunarvoice rate limited (wait 60 s)"])

    r.clock.advance(20_000)
    expect(r.voice.retry()).toBe(true)
    await flushAsync()
    expect(r.transport.requests).toHaveLength(1)
    expect(noticeTexts(r)).toContain("wait 40 s before retrying (rate limited)")

    await advance(r, 40_000)
    expect(r.voice.retry()).toBe(true)
    await flushAsync()
    expect(r.transport.requests).toHaveLength(2)
    expect(r.composer.text).toBe("hello from lunarvoice")
  })

  test("a full local window skips LunarVoice without a request", async () => {
    const r = rig({
      limits: { transcribePerMinute: 2 },
      hex: (clock) => [new FakeSource({ id: "hex-app", clock, replies: [{ text: "third" }] })],
    })
    r.transport.reply({ text: "first" }, { text: "second" })
    await flushAsync()
    await dictate(r)
    await dictate(r)
    await dictate(r)
    expect(r.transport.requests).toHaveLength(2)
    expect(hexSource(r, "hex-app").calls).toHaveLength(1)
    expect(r.composer.text).toBe("first second third")
    expect(resultNotices(r).at(-1)).toBe("warn: transcribed by hex-app (lunarvoice: rate limited)")
    expect(voiceState(r).lastAttempts).toEqual([
      { source: "lunarvoice", outcome: "rate_limited", ms: 0 },
      { source: "hex-app", outcome: "ok", ms: 0 },
    ])
  })

  test("the client rate window is a 60 s sliding window and a 429 blocks it", () => {
    const clock = new ManualClock()
    const window = new RateWindow(clock)
    expect(window.acquire(2)).toEqual({ ok: true })
    clock.advance(10_000)
    expect(window.acquire(2)).toEqual({ ok: true })
    expect(window.acquire(2)).toEqual({ ok: false, retryAfterMs: 50_000 })
    clock.advance(50_000)
    expect(window.acquire(2)).toEqual({ ok: true })
    expect(window.acquire(null)).toEqual({ ok: true })
    window.block(clock.now() + 5_000)
    expect(window.acquire(null)).toEqual({ ok: false, retryAfterMs: 5_000 })
  })
})

describe("fallback by error kind", () => {
  test("timeout falls back", async () => {
    const r = rig({ hex: (clock) => [new FakeSource({ id: "hex-app", clock, replies: [{ text: "from hex" }] })] })
    r.transport.reply(new TransportError("timeout", "POST /api/voice/transcribe failed (HTTP 504)", { status: 504 }))
    await flushAsync()
    await dictate(r)
    expect(r.composer.text).toBe("from hex")
    expect(resultNotices(r)).toEqual(["warn: transcribed by hex-app (lunarvoice: timed out)"])
  })

  test("no_speech surfaces without fallback and disposes the clip", async () => {
    const r = rig({ hex: (clock) => [new FakeSource({ id: "hex-app", clock })] })
    r.transport.reply({ text: "   " })
    await flushAsync()
    await dictate(r)
    expect(hexSource(r, "hex-app").calls).toHaveLength(0)
    expect(resultNotices(r)).toEqual(["info: no speech detected"])
    expect(isZeroed(r.transport.requests[0]?.wav ?? new Uint8Array(1).fill(1))).toBe(true)
    expect(voiceState(r).retained).toBeNull()
    expect(r.composer.text).toBe("")
  })

  test("bad_request stops the chain with the code and disposes the clip", async () => {
    const r = rig({ hex: (clock) => [new FakeSource({ id: "hex-app", clock })] })
    r.transport.reply(new TransportError("bad_request", "POST /api/voice/transcribe failed (HTTP 400): audio must be valid base64", { status: 400 }))
    await flushAsync()
    await dictate(r)
    expect(hexSource(r, "hex-app").calls).toHaveLength(0)
    expect(resultNotices(r)).toEqual(["error: voice: lunarvoice refused the recording (bad_request): the daemon rejected the transcription request (HTTP 400)"])
    expect(voiceState(r).retained).toBeNull()
  })

  test("auth falls back and raises a sticky warning", async () => {
    const r = rig({ hex: (clock) => [new FakeSource({ id: "hex-app", clock, replies: [{ text: "from hex" }] })] })
    r.transport.reply(new TransportError("auth", "the gateway rejected the token (HTTP 401 on POST /api/voice/transcribe)", { status: 401, fatal: true }))
    await flushAsync()
    await dictate(r)
    expect(r.composer.text).toBe("from hex")
    const auth = r.store.getState().notices.find((n) => n.id === "voice-auth")
    expect(auth).toMatchObject({ level: "warn", sticky: true, expiresAt: null })
    expect(auth?.text).toBe("lunarvoice: the gateway rejected the token (HTTP 401); dictation uses the next source until the token is fixed")
  })

  test("model_not_ready falls back with a one-time hint", async () => {
    const r = rig({
      order: ["hex-app", "hex-embedded"],
      hex: (clock) => [
        new FakeSource({ id: "hex-app", clock, defaultReply: { error: "model_not_ready", message: "no verified model" } }),
        new FakeSource({ id: "hex-embedded", clock, defaultReply: { text: "embedded" } }),
      ],
    })
    await flushAsync()
    await dictate(r)
    await dictate(r)
    expect(r.composer.text).toBe("embedded embedded")
    const hints = r.actions.filter((a) => a.type === "notice/push" && a.notice.text === HEX_MODEL_HINT)
    expect(hints).toHaveLength(1)
    expect(resultNotices(r).at(-1)).toBe("warn: transcribed by hex-embedded (hex-app: model not ready)")
  })

  test("too_large skips smaller-limit sources", async () => {
    const r = rig({
      limits: { maxTranscribeWavBytes: 20_000 },
      hex: (clock) => [
        new FakeSource({ id: "hex-app", clock, maxWavBytes: 2 * MIB, replies: [{ error: "too_large", message: "resource-exhausted" }] }),
        new FakeSource({ id: "hex-embedded", clock, maxWavBytes: 1 * MIB }),
      ],
    })
    await flushAsync()
    await dictate(r)
    // A 1 s clip is 32 044 bytes: over LunarVoice's 20 000, so it is skipped without a request; hex-app refuses it
    // as too large, so hex-embedded with a smaller limit is not tried either.
    expect(r.transport.requests).toHaveLength(0)
    expect(hexSource(r, "hex-app").calls).toHaveLength(1)
    expect(hexSource(r, "hex-embedded").calls).toHaveLength(0)
    expect(resultNotices(r)).toEqual([
      "warn: voice failed (Alt+R retry, Esc discard): lunarvoice recording too large; hex-app too large; hex-embedded recording too large",
    ])
  })

  test("fallback success raises the warn naming source and reason", async () => {
    const r = rig({ hex: (clock) => [new FakeSource({ id: "hex-app", clock, replies: [{ text: "ok" }] })] })
    r.transport.reply(new TransportError("bad_request", "POST /api/voice/transcribe failed (HTTP 422)", { status: 422 }))
    await flushAsync()
    await dictate(r)
    expect(resultNotices(r)).toEqual(["warn: transcribed by hex-app (lunarvoice: processing failed)"])
  })

  test("success from the first candidate is an info with duration, time and source", async () => {
    const r = rig()
    r.transport.reply({ text: "turn on the hallway lights", elapsedMs: 1_200 })
    await flushAsync()
    await dictate(r, 14_000)
    expect(resultNotices(r)).toEqual(["info: transcribed 14 s in 1.2 s (lunarvoice)"])
    expect(r.composer.text).toBe("turn on the hallway lights")
  })

  test("end of chain raises one notice listing every attempt", async () => {
    const r = rig({
      hex: (clock) => [
        new FakeSource({ id: "hex-app", clock, available: false, reason: "not running" }),
        new FakeSource({ id: "hex-embedded", clock, replies: [{ error: "processing_failed" }] }),
      ],
    })
    r.transport.reply(busy(), busy(), busy(), busy())
    await flushAsync()
    await dictate(r)
    await advance(r, 7_000)
    expect(resultNotices(r)).toEqual([
      "warn: voice failed (Alt+R retry, Esc discard): lunarvoice busy (3 retries); hex-app not running; hex-embedded processing failed",
    ])
    expect(voiceState(r).retained?.reason).toBe("busy")
    expect(voiceState(r).lastError).toMatchObject({ kind: "busy" })
  })
})

describe("cancel and the retained clip", () => {
  test("Esc aborts the request and zero-fills the clip", async () => {
    const r = rig()
    r.transport.reply("hang")
    await flushAsync()
    await dictate(r)
    const request = r.transport.requests[0]
    expect(request?.signal.aborted).toBe(false)
    expect(isZeroed(request?.wav ?? new Uint8Array(0))).toBe(false)
    expect(r.voice.escape()).toBe(true)
    await flushAsync()
    expect(request?.signal.aborted).toBe(true)
    expect(isZeroed(request?.wav ?? new Uint8Array(1).fill(1))).toBe(true)
    expect(voiceState(r).queue).toEqual([])
    expect(voiceState(r).retained).toBeNull()
    expect(r.composer.text).toBe("")
  })

  test("Esc zero-fills the retained clip", async () => {
    const r = rig({ order: ["lunarvoice"] })
    r.transport.reply(new TransportError("server", "POST /api/voice/transcribe failed (HTTP 502)", { status: 502 }))
    await flushAsync()
    await dictate(r)
    expect(voiceState(r).retained).toMatchObject({ reason: "unavailable" })
    const wav = r.transport.requests[0]?.wav ?? new Uint8Array(1).fill(1)
    expect(isZeroed(wav)).toBe(false)
    expect(r.voice.escape()).toBe(true)
    expect(isZeroed(wav)).toBe(true)
    expect(voiceState(r).retained).toBeNull()
    expect(noticeTexts(r)).toContain("failed recording discarded")
  })

  test("the retained clip expires after 10 min", async () => {
    const r = rig({ order: ["lunarvoice"] })
    r.transport.reply(new TransportError("unreachable", "cannot reach the gateway at http://127.0.0.1:3000"))
    await flushAsync()
    await dictate(r)
    const wav = r.transport.requests[0]?.wav ?? new Uint8Array(1).fill(1)
    expect(voiceState(r).retained).not.toBeNull()
    r.clock.advance(VOICE_TIMING.retainMs - 1)
    expect(voiceState(r).retained).not.toBeNull()
    r.clock.advance(1)
    expect(voiceState(r).retained).toBeNull()
    expect(isZeroed(wav)).toBe(true)
  })

  test("a new recording drops the retained clip", async () => {
    const r = rig({ order: ["lunarvoice"] })
    r.transport.reply(new TransportError("timeout", "POST /api/voice/transcribe timed out after 150 s"))
    await flushAsync()
    await dictate(r)
    const wav = r.transport.requests[0]?.wav ?? new Uint8Array(1).fill(1)
    expect(voiceState(r).retained).not.toBeNull()
    press(r)
    expect(voiceState(r).retained).toBeNull()
    expect(isZeroed(wav)).toBe(true)
  })

  test("the 180 s budget ends the chain", async () => {
    const r = rig({ order: ["lunarvoice", "hex-app"], hex: (clock) => [new FakeSource({ id: "hex-app", clock, replies: [{ hang: true }] })] })
    r.transport.reply(new TransportError("timeout", "POST /api/voice/transcribe timed out after 150 s"))
    await flushAsync()
    await dictate(r)
    const hex = hexSource(r, "hex-app")
    expect(hex.calls).toHaveLength(1)
    await advance(r, VOICE_TIMING.clipBudgetMs - 1_000)
    expect(voiceState(r).retained).toBeNull()
    await advance(r, 2_000)
    expect(hex.calls[0]?.signal.aborted).toBe(true)
    expect(resultNotices(r)).toEqual([
      "warn: voice failed (Alt+R retry, Esc discard): lunarvoice timed out; hex-app timed out (the 180 s budget ran out)",
    ])
    expect(voiceState(r).retained).not.toBeNull()
  })
})

describe("LunarVoice source", () => {
  test("probe: not connected, not configured, available with limits", async () => {
    const clock = new ManualClock()
    const store = createStore(createInitialState({ profile: "test", tenant: null, view: { width: 80, height: 24 }, at: 0 }))
    const transport = new ScriptedTransport()
    const source = createLunarVoiceSource({ store, clock, transport: () => transport })
    const signal = new AbortController().signal
    expect(await source.probe(signal)).toEqual({ available: false, reason: "not connected", checkedAt: 0 })
    expect(source.maxWavBytes()).toBe(0)
    store.dispatch({ type: "conn/ready", info: serverInfo({}, { transcribe: false }), at: 0 })
    expect(await source.probe(signal)).toMatchObject({ available: false, reason: "not configured on this gateway" })
    store.dispatch({ type: "conn/ready", info: serverInfo(), at: 0 })
    expect(await source.probe(signal)).toMatchObject({ available: true, detail: "http://127.0.0.1:3000, up to 18 MiB, 10/min" })
    expect(source.maxWavBytes()).toBe(18 * MIB)
    expect(source.privacyNote).toContain("nothing is stored")
  })

  test("503 'not configured' disables the source until the next connection", async () => {
    const clock = new ManualClock()
    const store = createStore(createInitialState({ profile: "test", tenant: null, view: { width: 80, height: 24 }, at: 0 }))
    store.dispatch({ type: "conn/ready", info: serverInfo(), at: 0 })
    const transport = new ScriptedTransport()
    transport.reply(new TransportError("unavailable", "POST /api/voice/transcribe: unavailable (HTTP 503): voice service is not configured", { status: 503 }))
    const source = createLunarVoiceSource({ store, clock, transport: () => transport })
    const clip = createWavClip(new Int16Array(16_000), { peakDb: -10, droppedFrames: 0 })
    const error = await source.transcribe(clip, new AbortController().signal).catch((e: unknown) => e)
    expect(isVoiceError(error) && [error.kind, error.message]).toEqual(["unavailable", "LunarVoice is not configured on this daemon"])
    const signal = new AbortController().signal
    expect(await source.probe(signal)).toMatchObject({ available: false, reason: "not configured on this daemon" })
    store.dispatch({ type: "conn/ready", info: serverInfo(), at: 0 })
    expect(await source.probe(signal)).toMatchObject({ available: true })
  })

  test("transcribe sends the WAV with a request id, maps the result and reports an abort as cancelled", async () => {
    const clock = new ManualClock()
    const store = createStore(createInitialState({ profile: "test", tenant: null, view: { width: 80, height: 24 }, at: 0 }))
    store.dispatch({ type: "conn/ready", info: serverInfo(), at: 0 })
    const transport = new ScriptedTransport().reply({ text: "  hello  ", language: "en", model: "parakeet", elapsedMs: 900 }, "hang")
    const source = createLunarVoiceSource({ store, clock, transport: () => transport })
    const clip: WavClip = createWavClip(new Int16Array(16_000), { peakDb: -10, droppedFrames: 0 })
    const transcript = await source.transcribe(clip, new AbortController().signal)
    expect(transcript).toEqual({ text: "hello", source: "lunarvoice", model: "parakeet", language: "en", audioMs: 1_000, elapsedMs: 900 })
    expect(transport.requests[0]?.requestId).toMatch(/^t-[0-9a-f-]{36}$/)
    expect(transport.requests[0]?.wav).toBe(clip.wav)

    const abort = new AbortController()
    const pending = source.transcribe(clip, abort.signal).catch((e: unknown) => e)
    abort.abort()
    const error = await pending
    expect(isVoiceError(error) && error.kind).toBe("cancelled")

    const big = createWavClip(new Int16Array(10 * 16_000), { peakDb: -10, droppedFrames: 0 })
    store.dispatch({ type: "conn/ready", info: serverInfo({ maxTranscribeWavBytes: 100_000 }), at: 0 })
    const tooLarge = await source.transcribe(big, new AbortController().signal).catch((e: unknown) => e)
    expect(isVoiceError(tooLarge) && tooLarge.kind).toBe("too_large")
    expect(transport.requests).toHaveLength(2)
  })

  test("gateway errors map onto the voice error kinds of 5.3.1", () => {
    const cases: [TransportError, string][] = [
      [new TransportError("rate_limited", "x", { status: 429, retryAfterMs: 60_000 }), "rate_limited"],
      [new TransportError("busy", "x: voice sidecar is unavailable", { status: 503 }), "busy"],
      [new TransportError("unavailable", "x: voice service is not configured", { status: 503 }), "unavailable"],
      [new TransportError("timeout", "x", { status: 504 }), "timeout"],
      [new TransportError("too_large", "x", { status: 413 }), "too_large"],
      [new TransportError("bad_request", "x", { status: 422 }), "processing_failed"],
      [new TransportError("bad_request", "x", { status: 400 }), "bad_request"],
      [new TransportError("auth", "x", { status: 401 }), "auth"],
      [new TransportError("forbidden", "x", { status: 403 }), "auth"],
      [new TransportError("server", "x", { status: 500 }), "unavailable"],
      [new TransportError("server", "x", { status: 502 }), "unavailable"],
      [new TransportError("unreachable", "x"), "unavailable"],
      [new TransportError("closed", "x"), "unavailable"],
      [new TransportError("protocol", "x"), "unavailable"],
    ]
    for (const [error, kind] of cases) expect([error.status, lunarVoiceErrorFromTransport(error).kind]).toEqual([error.status, kind as never])
    const limited = lunarVoiceErrorFromTransport(new TransportError("rate_limited", "x", { status: 429 }))
    expect(limited.opts).toMatchObject({ source: "lunarvoice", status: 429, retryAfterMs: 60_000 })
    // No server text in the message.
    expect(lunarVoiceErrorFromTransport(new TransportError("busy", "secret detail", { status: 503 })).message).toBe("the voice sidecar is busy (HTTP 503)")
  })

  test("REPLv3 error codes map as in 5.3.1", () => {
    expect(
      ["busy", "rate_limited", "too_large", "unavailable", "feature_not_negotiated", "timeout", "no_speech", "transcribe_failed", "unsupported_media", "toString"].map(
        voiceErrorKindForReplCode,
      ),
    ).toEqual(["busy", "rate_limited", "too_large", "unavailable", "unavailable", "timeout", "no_speech", "processing_failed", "unsupported_audio", "unavailable"])
  })

  test("the fallback table of 5.4", () => {
    expect(FALLBACK_STEPS).toMatchObject({
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
    })
  })

  test("createVoiceSources registers lunarvoice, hex-app and hex-embedded in the default order (M1b)", async () => {
    const clock = new ManualClock()
    const store = createStore(createInitialState({ profile: "test", tenant: null, view: { width: 80, height: 24 }, at: 0 }))
    // Hermetic Hex collaborators: no real discovery file, no `hex` on PATH, and a spawn that refuses everything.
    const fake = createFakeSpawn({ clock })
    const hex = { env: { HEX_APPLICATION_SUPPORT_DIR: await makeTempDir() }, which: () => null, spawn: fake.spawn }
    const sources = createVoiceSources({ store, clock, transport: () => null, config: DEFAULT_VOICE_CONFIG, hex })
    expect(sources.map((s) => s.id)).toEqual(["lunarvoice", "hex-app", "hex-embedded"])
    expect(sources.map((s) => s.id)).toEqual([...DEFAULT_VOICE_CONFIG.order])
    await Promise.all(sources.map((s) => s.dispose()))
    expect(fake.calls).toEqual([])
  })
})
