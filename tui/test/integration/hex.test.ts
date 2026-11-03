/**
 * Hex sources end to end (design 5.3.3; cases of 11.3.3) against test/mocks/mock-hex.ts on 127.0.0.1:0.
 *
 * - hex-app: the real discovery file in a temp HEX_APPLICATION_SUPPORT_DIR, real loopback HTTP, an injected
 *   /proc/net/tcp reader.
 * - hex-embedded: the child is a FakeSpawn script printing the mock's ready line (ManualClock drives the 15 s ready
 *   timeout, the 10 min idle TTL and the 6 s / 10 s close sequence); one test runs the standalone mock as a real
 *   child process (the "fake hex script", never a Hex binary) to prove the stdin lease ends it.
 * - Fallback through the real VoiceController with a scripted second source.
 * No real Hex, microphone, model download or non-loopback peer is involved.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { join } from "node:path"
import { SystemClock } from "@opentui/core"
import { ManualClock } from "@opentui/core/testing"
import { buildChildEnv } from "../../src/config/child-env.ts"
import { DEFAULT_VOICE_CONFIG, type VoiceConfig } from "../../src/config/types.ts"
import { createInitialState } from "../../src/state/model.ts"
import { createStore } from "../../src/state/store.ts"
import { isVoiceError, type VoiceError, type VoiceErrorKind } from "../../src/voice/errors.ts"
import { createVoiceController } from "../../src/voice/manager.ts"
import { createHexAppSource, type HexAppSource } from "../../src/voice/sources/hex-app.ts"
import { createHexEmbeddedSource, HEX_APP_RUNNING_REASON, HEX_EMBEDDED_TIMING, type HexEmbeddedSource } from "../../src/voice/sources/hex-embedded.ts"
import { HEX_TIMEOUTS } from "../../src/voice/sources/hex-http.ts"
import { createHexLocator, pidState, type HexSourceOptions } from "../../src/voice/sources/hex-locate.ts"
import { createVoiceSources } from "../../src/voice/sources/index.ts"
import type { WavClip } from "../../src/voice/types.ts"
import { createWavClip } from "../../src/voice/wav.ts"
import { createFakeSpawn, type FakeChildProcess, type FakeProcessScript, type FakeSpawn } from "../helpers/fake-spawn.ts"
import { makeTempDir } from "../helpers/tmp.ts"
import { FakeRecorder } from "../mocks/fake-recorder.ts"
import { ALL_VOICE_ACKS, FakeComposer, FakeSource, FakeStateFile, flushAsync } from "../mocks/fake-sources.ts"
import { procReaderFor } from "../mocks/mock-gateway.ts"
import { MOCK_HEX_PORT_FILE, MockHex, portFileProcReader, randomHexToken } from "../mocks/mock-hex.ts"

const uid = process.getuid?.() ?? 0
const never = new AbortController().signal
const MOCK_HEX_SCRIPT = join(import.meta.dir, "..", "mocks", "mock-hex.ts")
const EMBEDDED_PID = 52_017

const cleanups: (() => Promise<void> | void)[] = []

afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
})

async function startHex(opts: Parameters<typeof MockHex.start>[0] = {}): Promise<MockHex> {
  const mock = await MockHex.start(opts)
  cleanups.push(() => mock.stop())
  return mock
}

/** One second of a quiet tone as an in-memory clip. */
function clip(ms = 1_000): WavClip {
  const pcm = new Int16Array(ms * 16)
  for (let i = 0; i < pcm.length; i += 1) pcm[i] = Math.round(Math.sin(i / 6) * 6_000)
  return createWavClip(pcm, { peakDb: -15, droppedFrames: 0 })
}

/** Waits (by yielding to the event loop, never by sleeping for a fixed time) until real loopback I/O made `condition` true. */
async function eventually(condition: () => boolean, what: string): Promise<void> {
  for (let i = 0; i < 4_000; i += 1) {
    if (condition()) return
    await new Promise<void>((resolve) => setTimeout(resolve, 0))
  }
  throw new Error(`timed out waiting for ${what}`)
}

async function rejection(promise: Promise<unknown>): Promise<VoiceError> {
  try {
    await promise
  } catch (error) {
    if (isVoiceError(error)) return error
    throw error
  }
  throw new Error("expected a VoiceError")
}

function voiceConfig(model = ""): VoiceConfig {
  return { ...DEFAULT_VOICE_CONFIG, hex: { command: ["hex"], model } }
}

// ---------------------------------------------------------------------------------------------------------
// hex-app
// ---------------------------------------------------------------------------------------------------------

interface AppRig {
  mock: MockHex
  clock: ManualClock
  source: HexAppSource
}

async function appRig(opts: { model?: string; hex?: Partial<HexSourceOptions>; mock?: MockHex } = {}): Promise<AppRig> {
  const mock = opts.mock ?? (await startHex())
  const clock = new ManualClock()
  const hex: HexSourceOptions = {
    env: { HEX_APPLICATION_SUPPORT_DIR: mock.supportDir, HOME: "/nonexistent" },
    uid,
    readProc: procReaderFor(mock.port, uid),
    which: () => null,
    ...opts.hex,
  }
  const source = createHexAppSource({ clock, config: voiceConfig(opts.model), hex })
  cleanups.push(() => source.dispose())
  return { mock, clock, source }
}

describe("hex-app", () => {
  test("hex-app posts audio/wav with Content-Length, model, language and the bearer token", async () => {
    const { mock, source } = await appRig()
    const wav = clip()
    const transcript = await source.transcribe(wav, never)
    expect(transcript).toMatchObject({ text: "check the ports registry", source: "hex-app", model: "parakeet_unified_en", language: "en", audioMs: 1_000 })
    const posts = mock.requestsTo("/transcriptions", "POST")
    expect(posts).toHaveLength(1)
    const post = posts[0]
    expect(post?.headers["content-type"]).toBe("audio/wav")
    expect(post?.headers["content-length"]).toBe(String(wav.wav.length))
    expect(post?.headers["transfer-encoding"]).toBeUndefined()
    expect(post?.headers.authorization).toBe(`Bearer ${mock.token}`)
    expect(post?.headers.host).toBe(`127.0.0.1:${mock.port}`)
    expect(post?.query).toEqual({ model: "parakeet_unified_en", language: "en" })
    expect(post?.bodyBytes).toBe(wav.wav.length)
    expect(mock.requests.every((r) => r.authorized)).toBe(true)
  })

  test("the model comes from a verified entry of GET /models and no prepare call is made", async () => {
    const mock = await startHex({
      models: [
        { id: "parakeet_v3", name: "v3", installed: true, verified: false, downloadBytes: 1, languages: ["en"], supportsLanguageDetection: false },
        { id: "gigaam_v3", name: "ru", installed: true, verified: true, downloadBytes: 1, languages: ["ru"], supportsLanguageDetection: false },
        { id: "whisper_large_v3_turbo", name: "whisper", installed: true, verified: true, downloadBytes: 1, languages: ["*", "auto"], supportsLanguageDetection: true },
      ],
    })
    const { source } = await appRig({ mock })
    expect(await source.probe(never)).toMatchObject({ available: true, detail: `Hex 3.3.0 on 127.0.0.1:${mock.port} (pid ${process.pid}), model whisper_large_v3_turbo` })
    await source.transcribe(clip(), never)
    expect(mock.requestsTo("/transcriptions")[0]?.query.model).toBe("whisper_large_v3_turbo")
    expect(mock.requestsTo("/models")[0]?.query).toEqual({ language: "en" })
    expect(mock.requests.filter((r) => r.path.startsWith("/models/"))).toHaveLength(0)
    expect(mock.requests.every((r) => r.method === "GET" || r.path === "/transcriptions")).toBe(true)
    expect(mock.requestsTo("/capabilities")).toHaveLength(0)
  })

  test("no verified model: unavailable with the install command, nothing prepared or downloaded", async () => {
    const mock = await startHex({
      models: [{ id: "parakeet_unified_en", name: "unified", installed: false, verified: false, downloadBytes: 731_000_000, languages: ["en"], supportsLanguageDetection: false }],
    })
    const { source } = await appRig({ mock })
    const reason = "no verified Hex model: run `hex model install parakeet_unified_en --language en`"
    expect(await source.probe(never)).toMatchObject({ available: false, reason })
    const error = await rejection(source.transcribe(clip(), never))
    expect([error.kind, error.message]).toEqual(["unavailable", reason])
    expect(mock.requestsTo("/transcriptions")).toHaveLength(0)
    expect(mock.requests.filter((r) => r.method === "POST")).toHaveLength(0)
  })

  test("voice.hex.model is used; an uninstalled one is model_not_ready, an unknown one keeps the clip (unavailable)", async () => {
    const mock = await startHex()
    const { source } = await appRig({ mock, model: "parakeet_unified_en" })
    await source.transcribe(clip(), never)
    expect(mock.requestsTo("/transcriptions")[0]?.query.model).toBe("parakeet_unified_en")

    const missing = await appRig({ mock, model: "parakeet_v3" })
    const notReady = await rejection(missing.source.transcribe(clip(), never))
    expect([notReady.kind, notReady.message]).toEqual(["model_not_ready", "the Hex model parakeet_v3 is not installed: run `hex model install parakeet_v3 --language en`"])

    const unknown = await appRig({ mock, model: "no_such_model" })
    const error = await rejection(unknown.source.transcribe(clip(), never))
    expect([error.kind, error.message]).toEqual(["unavailable", "Hex does not offer the model no_such_model (voice.hex.model)"])
    expect(mock.requestsTo("/transcriptions")).toHaveLength(1)
  })

  test("Hex error answers map onto the 5.3.3 kinds; an empty transcript is no_speech", async () => {
    const { mock, source } = await appRig()
    const cases: [number, string, VoiceErrorKind][] = [
      [409, "model-not-ready", "model_not_ready"],
      [429, "queue-full", "busy"],
      [413, "resource-exhausted", "too_large"],
      [415, "unsupported-audio", "unsupported_audio"],
      [400, "invalid-audio", "bad_request"],
      [400, "unknown-model", "bad_request"],
      [400, "unsupported-model", "bad_request"],
      [400, "invalid-request", "bad_request"],
      [408, "upload-timeout", "timeout"],
      [500, "transcription-failed", "processing_failed"],
      [500, "model-load-failed", "processing_failed"],
      [503, "service-gone", "unavailable"],
    ]
    for (const [status, code, kind] of cases) {
      mock.override("POST /transcriptions", () => Response.json({ code }, { status }), 1)
      const error = await rejection(source.transcribe(clip(), never))
      expect({ kind: error.kind, status: error.opts.status, code: error.opts.code, source: error.opts.source }).toEqual({ kind, status, code, source: "hex-app" })
      expect(error.message).not.toContain(mock.token)
    }
    for (const transcript of ["", "   \n"]) {
      mock.transcript = transcript
      const error = await rejection(source.transcribe(clip(), never))
      expect(error.kind).toBe("no_speech")
    }
  })

  test("queue-full falls back", async () => {
    const mock = await startHex()
    mock.queueFull = true
    const clock = new ManualClock()
    const store = createStore(createInitialState({ profile: "test", tenant: null, view: { width: 80, height: 24 }, at: 0 }))
    const notices: string[] = []
    store.subscribe(() => {
      for (const n of store.getState().notices) if (!notices.includes(`${n.level}: ${n.text}`)) notices.push(`${n.level}: ${n.text}`)
    })
    const config: VoiceConfig = { ...voiceConfig(), order: ["hex-app", "hex-embedded"] }
    const hexApp = createHexAppSource({
      clock,
      config,
      hex: { env: { HEX_APPLICATION_SUPPORT_DIR: mock.supportDir, HOME: "/nonexistent" }, uid, readProc: procReaderFor(mock.port, uid) },
    })
    const fallback = new FakeSource({ id: "hex-embedded", clock, replies: [{ text: "from the next source" }] })
    const composer = new FakeComposer()
    const recorder = new FakeRecorder({ clock })
    const voice = createVoiceController({
      store,
      clock,
      composer,
      recorders: [recorder],
      sources: [hexApp, fallback],
      config,
      stateFile: new FakeStateFile({ voiceAck: { ...ALL_VOICE_ACKS } }),
    })
    cleanups.push(() => voice.dispose())
    await eventually(() => store.getState().voice.availability["hex-app"]?.available === true, "the hex-app probe")

    voice.pttKey({ eventType: "press", repeated: false })
    await flushAsync()
    recorder.play(1_000)
    voice.pttKey({ eventType: "press", repeated: false })
    await flushAsync()
    for (const [attempt, delay] of [[1, 1_000], [2, 2_000], [3, 4_000]] as const) {
      await eventually(() => {
        const head = store.getState().voice.queue[0]
        return head?.state === "waiting" && head.source === "hex-app" && head.attempt === attempt
      }, `busy retry ${attempt}`)
      clock.advance(delay)
    }
    await eventually(() => composer.text !== "", "the fallback transcript")
    expect(composer.text).toBe("from the next source")
    expect(mock.requestsTo("/transcriptions")).toHaveLength(4)
    expect(fallback.calls).toHaveLength(1)
    expect(notices).toContain("warn: transcribed by hex-embedded (hex-app: busy)")
    expect(store.getState().voice.lastAttempts.map((a) => [a.source, a.outcome])).toEqual([
      ["hex-app", "busy"],
      ["hex-app", "busy"],
      ["hex-app", "busy"],
      ["hex-app", "busy"],
      ["hex-embedded", "ok"],
    ])
  })

  test("stale discovery makes hex-app unavailable", async () => {
    const mock = await startHex()
    await mock.writeDiscovery({ pid: 2_147_483_647 })
    const { source } = await appRig({ mock })
    expect(await source.probe(never)).toMatchObject({ available: false, reason: "not running (stale local-api.json)" })
    const error = await rejection(source.transcribe(clip(), never))
    expect([error.kind, error.message]).toEqual(["unavailable", "not running (stale local-api.json)"])
    expect(mock.requests).toHaveLength(0)
  })

  test("a 401 invalidates the discovery cache and re-locates once", async () => {
    const { mock, source } = await appRig()
    expect((await source.probe(never)).available).toBe(true)
    const oldToken = mock.token
    // Hex restarted with a new token and published it; the source still holds the cached endpoint.
    mock.token = randomHexToken()
    await mock.writeDiscovery()
    const transcript = await source.transcribe(clip(), never)
    expect(transcript.text).toBe("check the ports registry")
    const posts = mock.requestsTo("/transcriptions")
    expect(posts.map((p) => [p.authorized, p.headers.authorization === `Bearer ${oldToken}`])).toEqual([
      [false, true],
      [true, false],
    ])
    expect(mock.requestsTo("/health")).toHaveLength(2)
  })

  test("a 401 without a new discovery file leaves hex-app unavailable", async () => {
    const { mock, source } = await appRig()
    expect((await source.probe(never)).available).toBe(true)
    mock.token = randomHexToken()
    const error = await rejection(source.transcribe(clip(), never))
    expect([error.kind, error.message]).toEqual(["unavailable", "Hex rejected the token in local-api.json"])
    expect(mock.requestsTo("/transcriptions")).toHaveLength(1)
    expect(await source.probe(never)).toMatchObject({ available: false, reason: "Hex rejected the token in local-api.json" })
  })

  test("a listener owned by another uid never receives the token or the audio", async () => {
    const mock = await startHex()
    const { source } = await appRig({ mock, hex: { readProc: procReaderFor(mock.port, uid + 3) } })
    const reason = `port ${mock.port} is held by uid ${uid + 3}, expected ${uid} (this user)`
    expect(await source.probe(never)).toMatchObject({ available: false, reason })
    expect((await rejection(source.transcribe(clip(), never))).message).toBe(reason)
    expect(mock.requests).toHaveLength(0)
  })

  test("an active proxy variable refuses every Hex request (fail closed)", async () => {
    const mock = await startHex()
    const { source } = await appRig({ mock, hex: { env: { HEX_APPLICATION_SUPPORT_DIR: mock.supportDir, HOME: "/x", HTTP_PROXY: "http://127.0.0.1:9" } } })
    expect(await source.probe(never)).toMatchObject({ available: false, reason: "HTTP_PROXY is set: Hex requests would go through a proxy; refused" })
    expect(mock.requests).toHaveLength(0)
  })

  test("the client gives up after 90 s and Esc aborts a request at once", async () => {
    const { mock, clock, source } = await appRig()
    let release: () => void = () => undefined
    const held = new Promise<Response>((resolve) => {
      release = () => resolve(Response.json({ transcript: "late", durationMs: 1 }))
    })
    cleanups.push(() => release())
    mock.override("POST /transcriptions", () => held)
    const slow = source.transcribe(clip(), never)
    await eventually(() => mock.requestsTo("/transcriptions").length === 1, "the upload")
    clock.advance(HEX_TIMEOUTS.transcribeMs)
    const timeout = await rejection(slow)
    expect([timeout.kind, timeout.message]).toEqual(["timeout", "Hex did not answer POST /transcriptions within 90 s"])

    const esc = new AbortController()
    const pending = source.transcribe(clip(), esc.signal)
    await eventually(() => mock.requestsTo("/transcriptions").length === 2, "the second upload")
    esc.abort()
    expect((await rejection(pending)).kind).toBe("cancelled")
  })

  test("discovery changes hint a re-probe; dispose unsubscribes", async () => {
    const mock = await startHex()
    const clock = new ManualClock()
    const hex: HexSourceOptions = { env: { HEX_APPLICATION_SUPPORT_DIR: mock.supportDir, HOME: "/x" }, uid, readProc: procReaderFor(mock.port, uid) }
    const locator = createHexLocator({ clock, command: ["hex"], ...hex })
    const source = createHexAppSource({ clock, config: voiceConfig(), hex: { ...hex, locator } })
    let hints = 0
    source.onAvailabilityHint(() => {
      hints += 1
    })
    expect(clock.pendingTimerCount).toBe(1)
    await locator.checkDiscovery()
    await mock.removeDiscovery()
    await locator.checkDiscovery()
    expect(hints).toBe(1)
    await source.dispose()
    expect(clock.pendingTimerCount).toBe(0)

    const disabled = createHexAppSource({ clock, config: { ...voiceConfig(), enabled: false }, hex: { ...hex, locator } })
    disabled.onAvailabilityHint(() => undefined)
    expect(clock.pendingTimerCount).toBe(0)
  })
})

// ---------------------------------------------------------------------------------------------------------
// hex-embedded (FakeSpawn child, ManualClock)
// ---------------------------------------------------------------------------------------------------------

interface EmbeddedRig {
  mock: MockHex
  clock: ManualClock
  fake: FakeSpawn
  source: HexEmbeddedSource
  /** Resolves with the n-th `service --embedded` child. */
  spawned(n?: number): Promise<FakeChildProcess>
}

interface EmbeddedRigOptions {
  /** Overrides of the embedded child's script (default: the mock's ready line, alive until stdin ends). */
  child?: Partial<FakeProcessScript> | ((mock: MockHex) => Partial<FakeProcessScript>)
  /** The LISTEN row /proc/net/tcp shows: default the mock's port, owned by this uid. */
  listener?: { port?: number; uid?: number }
  /** A Hex app (MockHex in app mode) whose support dir the locator reads. */
  app?: MockHex
}

async function embeddedRig(opts: EmbeddedRigOptions = {}): Promise<EmbeddedRig> {
  const mock = await startHex({ mode: "embedded" })
  const clock = new ManualClock()
  const fake = createFakeSpawn({ clock })
  const children: FakeChildProcess[] = []
  const waiters: { n: number; resolve: (child: FakeChildProcess) => void }[] = []
  fake.on((argv) => argv.at(-1) === "--version", { stdout: ["hex 3.3.0\n"] })
  const overrides = typeof opts.child === "function" ? opts.child(mock) : (opts.child ?? {})
  fake.on((argv) => argv.includes("--embedded"), {
    pid: EMBEDDED_PID,
    stdout: [mock.readyLine(EMBEDDED_PID)],
    stayAlive: true,
    exitOnStdinEnd: true,
    ...overrides,
  })
  const spawn: typeof fake.spawn = (argv, options) => {
    const child = fake.spawn(argv, options)
    if (argv.includes("--embedded")) {
      children.push(child as FakeChildProcess)
      for (const w of waiters.splice(0)) {
        if (children.length >= w.n) w.resolve(children[w.n - 1] as FakeChildProcess)
        else waiters.push(w)
      }
    }
    return child
  }
  const supportDir = opts.app?.supportDir ?? mock.supportDir
  const source = createHexEmbeddedSource({
    clock,
    config: voiceConfig(),
    hex: {
      env: { HEX_APPLICATION_SUPPORT_DIR: supportDir, HOME: "/nonexistent" },
      childEnv: { PATH: "/usr/bin", HOME: "/home/test", HEX_APPLICATION_SUPPORT_DIR: supportDir, TRANSCRIBE_DUMP_DIR: "/tmp/tensors" },
      uid,
      spawn,
      which: (cmd) => (cmd === "hex" ? "/usr/bin/hex" : null),
      readProc: procReaderFor(opts.listener?.port ?? opts.app?.port ?? mock.port, opts.listener?.uid ?? uid),
    },
  })
  cleanups.push(async () => {
    fake.killAll()
    await source.dispose()
  })
  const spawned = (n = 1): Promise<FakeChildProcess> => {
    const ready = children[n - 1]
    if (ready !== undefined) return Promise.resolve(ready)
    return new Promise((resolve) => waiters.push({ n, resolve }))
  }
  return { mock, clock, fake, source, spawned }
}

describe("hex-embedded", () => {
  test("embedded child: ready line parsed, transcription sent with its token, child kept for the idle TTL", async () => {
    const rig = await embeddedRig()
    const wav = clip()
    const transcript = await rig.source.transcribe(wav, never)
    expect(transcript).toMatchObject({ text: "check the ports registry", source: "hex-embedded", model: "parakeet_unified_en" })
    const call = rig.fake.callsFor((argv) => argv.includes("--embedded"))
    expect(call).toHaveLength(1)
    expect(call[0]?.argv).toEqual(["/usr/bin/hex", "service", "--embedded"])
    expect(call[0]?.options).toMatchObject({ stdin: "pipe", stdout: "pipe", stderr: "pipe" })
    expect(call[0]?.env).toEqual({ PATH: "/usr/bin", HOME: "/home/test", HEX_APPLICATION_SUPPORT_DIR: rig.mock.supportDir })
    expect(rig.fake.calls.every((c) => !("TRANSCRIBE_DUMP_DIR" in c.env))).toBe(true)
    const post = rig.mock.requestsTo("/transcriptions")[0]
    expect(post?.headers.authorization).toBe(`Bearer ${rig.mock.token}`)
    expect(post?.headers["content-length"]).toBe(String(wav.wav.length))
    const child = await rig.spawned()
    expect(child.running).toBe(true)
    expect(child.stdinEnded).toBe(false)
    expect(child.stdinBytes()).toHaveLength(0)
    expect(rig.source.childPid).toBe(EMBEDDED_PID)
    expect(await rig.source.probe(never)).toMatchObject({ available: true, detail: `Hex 3.3.0, embedded service pid ${EMBEDDED_PID}, model parakeet_unified_en` })
    // A second dictation reuses the child; embedded mode only ever uses /models and /transcriptions.
    await rig.source.transcribe(clip(), never)
    expect(rig.fake.callsFor((argv) => argv.includes("--embedded"))).toHaveLength(1)
    expect([...new Set(rig.mock.requests.map((r) => `${r.method} ${r.path}`))].sort()).toEqual(["GET /models", "POST /transcriptions"])
  })

  test("the embedded service is started lazily: probes never spawn it", async () => {
    const rig = await embeddedRig()
    expect(await rig.source.probe(never)).toMatchObject({ available: true, detail: "Hex 3.3.0; the embedded service starts at the first transcription" })
    await rig.source.probe(never)
    expect(rig.fake.callsFor((argv) => argv.includes("--embedded"))).toHaveLength(0)
    expect(rig.source.childPid).toBeNull()
  })

  test("a non-loopback URL in the ready line is rejected and the child killed", async () => {
    const urls = (port: number) => [
      `http://192.168.1.20:${port}`,
      `http://localhost:${port}`,
      `https://127.0.0.1:${port}`,
      `http://user:pass@127.0.0.1:${port}`,
      `http://127.0.0.1:${port}/api`,
      `http://127.0.0.1:${port}?x=1`,
    ]
    for (let i = 0; i < 6; i += 1) {
      const rig = await embeddedRig({ child: (mock) => ({ stdout: [mock.readyLine(EMBEDDED_PID, { url: urls(mock.port)[i] })] }) })
      const error = await rejection(rig.source.transcribe(clip(), never))
      expect([error.kind, error.message]).toEqual(["unavailable", "the embedded Hex service sent an invalid ready line (URL is not http://127.0.0.1:<port>)"])
      const child = await rig.spawned()
      expect(child.kills).toContain("SIGKILL")
      expect(child.stdinEnded).toBe(true)
      expect(child.running).toBe(false)
      expect(rig.mock.requests).toHaveLength(0)
    }
  })

  test("a ready line naming a port nothing listens on is rejected and the child killed", async () => {
    const rig = await embeddedRig({ listener: { port: 1 } })
    const error = await rejection(rig.source.transcribe(clip(), never))
    expect([error.kind, error.message]).toEqual(["unavailable", `nothing listens on 127.0.0.1:${rig.mock.port}`])
    expect((await rig.spawned()).kills).toContain("SIGKILL")
    expect(rig.mock.requests).toHaveLength(0)
  })

  test("foreign listener owner rejected: the child is killed before its token is used", async () => {
    const rig = await embeddedRig({ listener: { uid: uid + 9 } })
    const error = await rejection(rig.source.transcribe(clip(), never))
    expect([error.kind, error.message]).toEqual(["unavailable", `port ${rig.mock.port} is held by uid ${uid + 9}, expected ${uid} (this user)`])
    const child = await rig.spawned()
    expect(child.kills).toContain("SIGKILL")
    expect(child.stdinEnded).toBe(true)
    expect(rig.mock.requests).toHaveLength(0)
  })

  test("a ready line with another pid, a bad token, another API version or over 16 KiB is refused", async () => {
    const cases: [(mock: MockHex) => Partial<FakeProcessScript>, string][] = [
      [(m) => ({ stdout: [m.readyLine(EMBEDDED_PID + 1)] }), `the embedded Hex service reported pid ${EMBEDDED_PID + 1}, not its own pid ${EMBEDDED_PID}`],
      [(m) => ({ stdout: [m.readyLine(EMBEDDED_PID, { token: "hex_short" })] }), "the embedded Hex service sent an invalid ready line (invalid token)"],
      [(m) => ({ stdout: [m.readyLine(EMBEDDED_PID, { apiVersion: "3" })] }), "the embedded Hex service sent an invalid ready line (local API version is not 2)"],
      [(m) => ({ stdout: [m.readyLine(EMBEDDED_PID, { type: "starting" })] }), "the embedded Hex service sent an invalid ready line (not a ready line)"],
      [() => ({ stdout: ["Hex is starting...\n"] }), "the embedded Hex service sent an invalid ready line (not JSON)"],
      [() => ({ stdout: ["x".repeat(10_000), "y".repeat(10_000)] }), "the embedded Hex service sent a ready line over 16 KiB"],
      [() => ({ stdout: [], stayAlive: false, exitCode: 1 }), "the embedded Hex service exited before it was ready"],
    ]
    for (const [child, reason] of cases) {
      const rig = await embeddedRig({ child })
      const error = await rejection(rig.source.transcribe(clip(), never))
      expect(error.message.startsWith(reason)).toBe(true)
      expect((await rig.spawned()).running).toBe(false)
      expect(rig.mock.requests).toHaveLength(0)
      expect(error.message).not.toContain(rig.mock.token)
    }
  })

  test("no ready line within 15 s kills the child", async () => {
    const rig = await embeddedRig({ child: { stdout: [] } })
    const pending = rejection(rig.source.transcribe(clip(), never))
    const child = await rig.spawned()
    rig.clock.advance(HEX_EMBEDDED_TIMING.readyTimeoutMs - 1)
    await flushAsync()
    expect(child.running).toBe(true)
    rig.clock.advance(1)
    const error = await pending
    expect([error.kind, error.message]).toEqual(["unavailable", "the embedded Hex service did not start within 15 s"])
    expect(child.kills).toEqual(["SIGKILL"])
    expect(child.running).toBe(false)
  })

  test("stdin lease closed on dispose, child exits", async () => {
    const rig = await embeddedRig()
    await rig.source.transcribe(clip(), never)
    const child = await rig.spawned()
    await rig.source.dispose()
    expect(child.stdinEnded).toBe(true)
    expect(child.running).toBe(false)
    expect(child.kills).toEqual([])
    expect(rig.clock.pendingTimerCount).toBe(0)
  })

  test("a child that ignores the lease gets SIGTERM after 6 s and SIGKILL after 10 s", async () => {
    const rig = await embeddedRig({ child: { exitOnStdinEnd: false, ignoreSignals: ["SIGTERM"] } })
    await rig.source.transcribe(clip(), never)
    const child = await rig.spawned()
    let disposed = false
    const disposing = rig.source.dispose().then(() => {
      disposed = true
    })
    await flushAsync()
    expect(child.stdinEnded).toBe(true)
    rig.clock.advance(HEX_EMBEDDED_TIMING.termAfterMs - 1)
    expect(child.kills).toEqual([])
    rig.clock.advance(1)
    expect(child.kills).toEqual(["SIGTERM"])
    expect(child.running).toBe(true)
    rig.clock.advance(HEX_EMBEDDED_TIMING.killAfterMs - HEX_EMBEDDED_TIMING.termAfterMs)
    expect(child.kills).toEqual(["SIGTERM", "SIGKILL"])
    await disposing
    expect(disposed).toBe(true)
    expect(child.running).toBe(false)
  })

  test("after 10 min idle the lease is closed; the next transcription starts a new child", async () => {
    const rig = await embeddedRig()
    await rig.source.transcribe(clip(), never)
    const first = await rig.spawned()
    rig.clock.advance(HEX_EMBEDDED_TIMING.idleTtlMs - 1)
    expect(first.stdinEnded).toBe(false)
    rig.clock.advance(1)
    await flushAsync()
    expect(first.stdinEnded).toBe(true)
    expect(first.running).toBe(false)
    expect(rig.source.childPid).toBeNull()
    await rig.source.transcribe(clip(), never)
    const second = await rig.spawned(2)
    expect(second.running).toBe(true)
    expect(rig.fake.callsFor((argv) => argv.includes("--embedded"))).toHaveLength(2)
  })

  test("the Hex app running makes hex-embedded unavailable and nothing is spawned", async () => {
    const app = await startHex()
    const rig = await embeddedRig({ app })
    expect(await rig.source.probe(never)).toMatchObject({ available: false, reason: HEX_APP_RUNNING_REASON })
    const error = await rejection(rig.source.transcribe(clip(), never))
    expect([error.kind, error.message]).toEqual(["unavailable", HEX_APP_RUNNING_REASON])
    expect(rig.fake.callsFor((argv) => argv.includes("--embedded"))).toHaveLength(0)
  })

  test("a 401 from the embedded child replaces it once", async () => {
    const rig = await embeddedRig()
    rig.mock.override("POST /transcriptions", () => new Response(null, { status: 401 }), 1)
    const transcript = await rig.source.transcribe(clip(), never)
    expect(transcript.text).toBe("check the ports registry")
    const first = await rig.spawned(1)
    const second = await rig.spawned(2)
    expect(first.kills).toContain("SIGKILL")
    expect(second.running).toBe(true)
  })

  test("a missing binary is not installed and nothing is spawned", async () => {
    const clock = new ManualClock()
    const fake = createFakeSpawn({ clock })
    const dir = await makeTempDir()
    const source = createHexEmbeddedSource({ clock, config: voiceConfig(), hex: { env: { HEX_APPLICATION_SUPPORT_DIR: dir, HOME: "/x" }, uid, spawn: fake.spawn, which: () => null } })
    cleanups.push(() => source.dispose())
    expect(await source.probe(never)).toMatchObject({ available: false, reason: "not installed" })
    expect((await rejection(source.transcribe(clip(), never))).message).toBe("not installed")
    expect(fake.calls).toHaveLength(0)
  })
})

// ---------------------------------------------------------------------------------------------------------
// Registration and the real child
// ---------------------------------------------------------------------------------------------------------

describe("registration", () => {
  test("with Hex absent, the voice panel lists hex-app and hex-embedded as unavailable with their reasons", async () => {
    const dir = await makeTempDir()
    const clock = new ManualClock()
    const fake = createFakeSpawn({ clock })
    const store = createStore(createInitialState({ profile: "test", tenant: null, view: { width: 80, height: 24 }, at: 0 }))
    const sources = createVoiceSources({
      store,
      clock,
      transport: () => null,
      config: DEFAULT_VOICE_CONFIG,
      hex: { env: { HEX_APPLICATION_SUPPORT_DIR: dir, HOME: "/nonexistent" }, uid, spawn: fake.spawn, which: () => null },
    })
    expect(sources.map((s) => s.id)).toEqual(["lunarvoice", "hex-app", "hex-embedded"])
    const voice = createVoiceController({
      store,
      clock,
      composer: new FakeComposer(),
      recorders: [new FakeRecorder({ clock })],
      sources,
      config: DEFAULT_VOICE_CONFIG,
      stateFile: new FakeStateFile({}),
    })
    cleanups.push(() => voice.dispose())
    await eventually(() => Object.keys(store.getState().voice.availability).length === 3, "the first probes")
    const rows = voice.status().sources.map((s) => [s.id, s.inBuild, s.availability?.available, s.availability?.reason])
    expect(rows).toEqual([
      ["lunarvoice", true, false, "not connected"],
      ["hex-app", true, false, "not running"],
      ["hex-embedded", true, false, "not installed"],
    ])
    expect(fake.calls).toHaveLength(0)
    expect(voice.status().sources.map((s) => s.label)).toEqual(["LunarVoice", "Hex (running app)", "Hex (embedded service)"])
  })

  test("embedded child (the fake hex script as a real process): ready line, transcription, lease closed on dispose, child exits", async () => {
    const dir = await makeTempDir()
    const source = createHexEmbeddedSource({
      clock: new SystemClock(),
      config: { ...DEFAULT_VOICE_CONFIG, hex: { command: [process.execPath, MOCK_HEX_SCRIPT], model: "" } },
      hex: {
        env: { HEX_APPLICATION_SUPPORT_DIR: dir, HOME: process.env.HOME ?? "/nonexistent" },
        childEnv: buildChildEnv({ PATH: process.env.PATH, HOME: process.env.HOME }, { HEX_APPLICATION_SUPPORT_DIR: dir }),
        uid,
        readProc: portFileProcReader(join(dir, MOCK_HEX_PORT_FILE), uid),
      },
    })
    cleanups.push(() => source.dispose())
    const transcript = await source.transcribe(clip(), never)
    expect(transcript.text).toBe("check the ports registry")
    const pid = source.childPid
    if (pid === null) throw new Error("no embedded child")
    expect(pidState(pid)).toBe("alive")
    await source.dispose()
    expect(pidState(pid)).toBe("dead")
  }, 20_000)
})
