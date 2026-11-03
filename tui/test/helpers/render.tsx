/**
 * renderApp(): the real <App> on OpenTUI's testRender with the real runtime wiring (app.tsx createAppRuntime)
 * and fakes for everything outside the process: FakeTransport (or a real GatewayTransport against the mock
 * gateway), fake recorders and sources, a fake clipboard, FakeSpawn. One ManualClock drives the renderer, the
 * supervisor, the coalescer, the voice controller and every timeout, so nothing sleeps (design 11.3.4, 11.3.5).
 *
 * Act hygiene (C12): testRender mounts inside act(); afterwards IS_REACT_ACT_ENVIRONMENT is set to false, because
 * the store changes from I/O and timers rather than from React events. settle() then waits for React's commit
 * (a macrotask) and for every async operation the session started; render() also paints one frame.
 */
import { ManualClock } from "@opentui/core/testing"
import type { TestRendererSetup } from "@opentui/core/testing"
import { testRender } from "@opentui/react/test-utils"
import { App, createAppRuntime, type AppRuntime, type RuntimeConfig } from "../../src/app.tsx"
import { DEFAULT_UI_CONFIG, DEFAULT_VOICE_CONFIG, type ChildEnv, type SpawnFn, type StateFile, type UiConfig, type VoiceConfig } from "../../src/config/types.ts"
import type { ClipboardReader } from "../../src/images/clipboard.ts"
import type { ImageExporter } from "../../src/images/export.ts"
import type { SessionImages } from "../../src/controller/session.ts"
import { createTurnIdSequence } from "../../src/state/ids.ts"
import { COALESCE_INTERVAL_MS } from "../../src/state/coalesce.ts"
import type { Store } from "../../src/state/model.ts"
import type { Backoff } from "../../src/transport/backoff.ts"
import type { Transport, TransportEvent } from "../../src/transport/types.ts"
import type { Recorder, VoiceSource } from "../../src/voice/types.ts"
import type { HexSourceOptions } from "../../src/voice/sources/hex-locate.ts"
import type { ProfileState, TokenKind } from "../../src/config/types.ts"
import { GatewayTransport } from "../../src/transport/gateway.ts"
import type { ProcReader } from "../../src/transport/listener-owner.ts"
import { FakeTransport } from "../mocks/fake-transport.ts"
import { FakeRecorder } from "../mocks/fake-recorder.ts"
import { FakeSource } from "../mocks/fake-sources.ts"
import { fixtureToken, MemoryStateFile, MemoryTokenSource, MockGateway, procReaderFor, type MockGatewayOptions } from "../mocks/mock-gateway.ts"
import { createFakeSpawn } from "./fake-spawn.ts"
import { makeTempDir } from "./tmp.ts"

/** A fixed, non-zero start time (2026-11-03T00:00:00Z). */
export const TEST_START_MS = Date.UTC(2026, 10, 3)

/**
 * A working microphone for renderApp: a fake recorder and an available fake LunarVoice on one ManualClock.
 * Without them the mic reads "(no mic)" and nothing offers the push-to-talk key (footer, welcome).
 */
export function withMic(): Pick<RenderAppOptions, "clock" | "recorders" | "sources"> {
  const clock = new ManualClock()
  clock.setTime(TEST_START_MS)
  return { clock, recorders: [new FakeRecorder({ clock })], sources: [new FakeSource({ id: "lunarvoice", clock })] }
}

export interface RenderAppOptions {
  width?: number
  height?: number
  kittyKeyboard?: boolean
  clock?: ManualClock
  /**
   * The runtime's wall clock (epoch ms). Default: the ManualClock itself, which tests start at an epoch time, so
   * both clocks agree. Production's Clock is performance.now() and its wall clock Date.now (src/main.tsx).
   */
  wallNow?: () => number
  /** Default: a FakeTransport in current mode. */
  transport?: Transport
  ui?: Partial<UiConfig>
  voice?: Partial<VoiceConfig>
  tenant?: string | null
  profile?: string
  primary?: boolean
  recorders?: Recorder[]
  /** Default: no voice sources (the mic shows "(no mic)"). */
  sources?: VoiceSource[]
  /** Use this build's real sources (voice/sources/index.ts: LunarVoice over the transport) instead of `sources`. */
  buildSources?: boolean
  clipboard?: ClipboardReader | null
  exporter?: ImageExporter | null
  spawn?: SpawnFn
  env?: ChildEnv
  noColor?: string
  stateFile?: StateFile
  backoff?: Backoff
  readFile?: SessionImages["readFile"]
  screenshot?: SessionImages["screenshot"]
  /** Start the supervisor (connect) right away; default true. */
  connect?: boolean
  cwd?: string
}

export interface AppHarness {
  readonly setup: TestRendererSetup
  readonly clock: ManualClock
  readonly runtime: AppRuntime
  readonly store: Store
  /** The transport given, or the default FakeTransport. */
  readonly transport: Transport
  /** onQuit calls (/quit, Ctrl+C twice). */
  readonly quits: number
  readonly env: ChildEnv
  settle(rounds?: number): Promise<void>
  /** settle() and one painted frame. */
  render(): Promise<string>
  frame(): string
  /** Advances the ManualClock in steps of at most `step` ms, settling in between. */
  advance(ms: number, step?: number): Promise<void>
  /** Emits on a FakeTransport, then lets the coalescer flush. */
  emit(...events: TransportEvent[]): Promise<void>
  type(text: string): Promise<void>
  press(key: string, modifiers?: { shift?: boolean; ctrl?: boolean; meta?: boolean }): Promise<void>
  enter(): Promise<void>
  /** Esc (and the 20 ms parser timeout that tells a lone ESC from an escape sequence). */
  escape(): Promise<void>
  paste(text: string): Promise<void>
  dispose(): Promise<void>
}

const IS_ACT = "IS_REACT_ACT_ENVIRONMENT"

function setActEnvironment(value: boolean): void {
  Reflect.set(globalThis, IS_ACT, value)
}

/**
 * Destroys a bare testRender() setup without act() noise: OpenTUI's root unmounts itself on "destroy" outside
 * act(), so the act environment is switched off first.
 */
export async function destroySetup(setup: TestRendererSetup): Promise<void> {
  setActEnvironment(false)
  setup.renderer.destroy()
  await setup.renderer.closed
}

function macrotask(): Promise<void> {
  return new Promise((resolve) => setImmediate(resolve))
}

/**
 * The voice-source part of the runtime deps. With buildSources the real sources of this build are created, and
 * the Hex ones get hermetic collaborators: an empty support directory (no local-api.json, so the host's Hex app
 * and its token are never seen), no `hex` on PATH and a spawn that refuses everything (FakeSpawn's ENOENT).
 */
async function voiceSourceDeps(opts: RenderAppOptions): Promise<{ sources: VoiceSource[] } | { hex: HexSourceOptions }> {
  if (opts.buildSources !== true) return { sources: opts.sources ?? [] }
  const supportDir = await makeTempDir("lw-tui-nohex-")
  return { hex: { env: { HEX_APPLICATION_SUPPORT_DIR: supportDir }, which: () => null, spawn: opts.spawn ?? createFakeSpawn().spawn } }
}

function runtimeConfig(opts: RenderAppOptions): RuntimeConfig {
  return {
    profile: opts.profile ?? "test",
    tenant: opts.tenant === undefined ? "derpy" : opts.tenant,
    primary: opts.primary ?? true,
    ui: { ...DEFAULT_UI_CONFIG, ...opts.ui },
    voice: { ...DEFAULT_VOICE_CONFIG, ...opts.voice },
    cwd: opts.cwd ?? process.cwd(),
    listenerOwner: "uid 1000 (test)",
    tokenSource: "tokens/test.token",
  }
}

export async function renderApp(opts: RenderAppOptions = {}): Promise<AppHarness> {
  const clock = opts.clock ?? new ManualClock()
  if (clock.now() === 0) clock.setTime(TEST_START_MS)
  const width = opts.width ?? 80
  const height = opts.height ?? 24
  const transport = opts.transport ?? new FakeTransport()
  const env: ChildEnv = opts.env ?? { PATH: "/usr/bin" }
  let quits = 0
  const runtime = createAppRuntime({
    clock,
    wallNow: opts.wallNow ?? (() => clock.now()),
    transport,
    stateFile: opts.stateFile ?? new MemoryStateFile(),
    config: runtimeConfig(opts),
    view: { width, height },
    env,
    recorders: opts.recorders ?? [],
    ...(await voiceSourceDeps(opts)),
    clipboard: opts.clipboard ?? null,
    exporter: opts.exporter ?? null,
    ...(opts.spawn === undefined ? {} : { spawn: opts.spawn }),
    ...(opts.backoff === undefined ? {} : { backoff: opts.backoff }),
    ...(opts.readFile === undefined ? {} : { readFile: opts.readFile }),
    ...(opts.screenshot === undefined ? {} : { screenshot: opts.screenshot }),
    noColor: opts.noColor,
    newTurnId: createTurnIdSequence("t"),
    onQuit: () => {
      quits += 1
    },
  })
  // The production renderer settings that matter for input (src/main.tsx rendererConfig, design 3.1).
  const setup = await testRender(<App runtime={runtime} />, {
    width,
    height,
    clock,
    kittyKeyboard: opts.kittyKeyboard ?? false,
    exitOnCtrlC: false,
    useMouse: true,
    enableMouseMovement: false,
  })
  setActEnvironment(false)

  const settle = async (rounds = 3): Promise<void> => {
    for (let round = 0; round < rounds; round += 1) {
      await macrotask()
      await runtime.session.idle()
    }
  }
  /** Two passes: a frame can report layout-dependent facts (the composer's wrapped line count) to the store. */
  const render = async (): Promise<string> => {
    await settle()
    await setup.renderOnce()
    await settle()
    await setup.renderOnce()
    return setup.captureCharFrame()
  }
  const advance = async (ms: number, step = 500): Promise<void> => {
    let left = ms
    while (left > 0) {
      const delta = Math.min(step, left)
      clock.advance(delta)
      left -= delta
      await settle(1)
    }
    await settle()
  }
  const harness: AppHarness = {
    setup,
    clock,
    runtime,
    store: runtime.store,
    transport,
    get quits() {
      return quits
    },
    env,
    settle,
    render,
    frame: () => setup.captureCharFrame(),
    advance,
    emit: async (...events) => {
      if (transport instanceof FakeTransport) transport.emit(...events)
      await settle()
      clock.advance(COALESCE_INTERVAL_MS)
      await settle()
    },
    type: async (text) => {
      await setup.mockInput.typeText(text)
      await settle()
    },
    press: async (key, modifiers) => {
      setup.mockInput.pressKey(key, modifiers)
      await settle()
    },
    enter: async () => {
      setup.mockInput.pressEnter()
      await settle()
    },
    escape: async () => {
      setup.mockInput.pressEscape()
      clock.advance(25)
      await settle()
    },
    paste: async (text) => {
      await setup.mockInput.pasteBracketedText(text)
      await settle()
    },
    dispose: async () => {
      await runtime.shutdown("test")
      // OpenTUI's root unmounts itself on "destroy" outside act(); with the act environment off React stays quiet.
      setup.renderer.destroy()
      await setup.renderer.closed
    },
  }
  if (opts.connect !== false) runtime.start()
  await settle()
  return harness
}

// ---------------------------------------------------------------------------------------------------------
// Controllers without a renderer (commands, approval replies, integration tests)
// ---------------------------------------------------------------------------------------------------------

export interface RuntimeHarness {
  readonly clock: ManualClock
  readonly runtime: AppRuntime
  readonly store: Store
  readonly transport: Transport
  readonly quits: number
  settle(rounds?: number): Promise<void>
  advance(ms: number, step?: number): Promise<void>
  /** Emits on a FakeTransport, then flushes the coalescer. */
  emit(...events: TransportEvent[]): Promise<void>
  /** Puts `text` in the composer and presses Enter (session.submit). */
  run(text: string): Promise<void>
  /** Presses Enter on whatever the composer holds. */
  enter(): Promise<void>
  dispose(): Promise<void>
}

/**
 * The real runtime with a detached composer (controller/composer-handle.ts keeps the text without a textarea).
 * The supervisor is started, so a FakeTransport is "ready" after settle().
 */
export async function startRuntime(opts: RenderAppOptions = {}): Promise<RuntimeHarness> {
  const clock = opts.clock ?? new ManualClock()
  if (clock.now() === 0) clock.setTime(TEST_START_MS)
  const transport = opts.transport ?? new FakeTransport()
  let quits = 0
  const runtime = createAppRuntime({
    clock,
    wallNow: opts.wallNow ?? (() => clock.now()),
    transport,
    stateFile: opts.stateFile ?? new MemoryStateFile(),
    config: runtimeConfig(opts),
    view: { width: opts.width ?? 80, height: opts.height ?? 24 },
    env: opts.env ?? { PATH: "/usr/bin" },
    recorders: opts.recorders ?? [],
    ...(await voiceSourceDeps(opts)),
    clipboard: opts.clipboard ?? null,
    exporter: opts.exporter ?? null,
    ...(opts.spawn === undefined ? {} : { spawn: opts.spawn }),
    ...(opts.backoff === undefined ? {} : { backoff: opts.backoff }),
    ...(opts.readFile === undefined ? {} : { readFile: opts.readFile }),
    ...(opts.screenshot === undefined ? {} : { screenshot: opts.screenshot }),
    newTurnId: createTurnIdSequence("t"),
    onQuit: () => {
      quits += 1
    },
  })
  const settle = async (rounds = 3): Promise<void> => {
    for (let round = 0; round < rounds; round += 1) {
      await macrotask()
      await runtime.session.idle()
    }
  }
  const advance = async (ms: number, step = 500): Promise<void> => {
    let left = ms
    while (left > 0) {
      const delta = Math.min(step, left)
      clock.advance(delta)
      left -= delta
      await settle(1)
    }
    await settle()
  }
  const harness: RuntimeHarness = {
    clock,
    runtime,
    store: runtime.store,
    transport,
    get quits() {
      return quits
    },
    settle,
    advance,
    emit: async (...events) => {
      if (transport instanceof FakeTransport) transport.emit(...events)
      await settle()
      runtime.supervisor.flush()
      await settle()
    },
    run: async (text) => {
      await runtime.session.runText(text)
      await settle()
    },
    enter: async () => {
      await runtime.session.submit()
      await settle()
    },
    dispose: async () => {
      await runtime.shutdown("test")
    },
  }
  if (opts.connect !== false) runtime.start()
  await settle()
  return harness
}

// ---------------------------------------------------------------------------------------------------------
// The real GatewayTransport against the mock gateway (integration tests)
// ---------------------------------------------------------------------------------------------------------

export const GATEWAY_TEST_TOKEN = fixtureToken("test")

export interface GatewayRigOptions {
  mode: "legacy" | "current"
  mock?: Partial<MockGatewayOptions>
  /** The token the client presents (default: the one the mock accepts). */
  clientToken?: string
  tokenKind?: TokenKind
  /** /proc/net/tcp as the listener-owner check reads it; default: the mock's port owned by this uid. */
  readProc?: ProcReader
  backoff?: Backoff
  /** Start of the ManualClock (default TEST_START_MS); a value near 0 behaves like production's performance.now(). */
  clockStart?: number
  /** The runtime's wall clock; default the ManualClock (see RenderAppOptions.wallNow). */
  wallNow?: () => number
  /** Runs on the mock before the runtime connects (threads, gates that exist before the TUI starts). */
  setup?: (mock: MockGateway) => void
  /** Initial profile state (lastThreadId for thread `last`). */
  state?: ProfileState
}

export interface GatewayRig extends RuntimeHarness {
  readonly mock: MockGateway
  readonly gateway: GatewayTransport
  readonly token: MemoryTokenSource
  /** Polls real I/O (loopback HTTP) until `check` holds; flushes the coalescer in between (its timer is manual). */
  until(check: () => boolean, what: string, timeoutMs?: number): Promise<void>
}

export const TEST_UID = process.getuid?.() ?? 1000

export async function startGatewayRuntime(opts: GatewayRigOptions): Promise<GatewayRig> {
  const mock = await MockGateway.start({ mode: opts.mode, token: GATEWAY_TEST_TOKEN, ...opts.mock })
  opts.setup?.(mock)
  const clock = new ManualClock()
  clock.setTime(opts.clockStart ?? TEST_START_MS)
  const token = new MemoryTokenSource(opts.clientToken ?? GATEWAY_TEST_TOKEN, opts.tokenKind ?? "client", "tenants/derpy.json")
  const stateFile = new MemoryStateFile(opts.state ?? {})
  const gateway = new GatewayTransport({
    baseUrl: mock.url,
    token,
    thread: { kind: "last" },
    clock,
    timezone: "UTC",
    stateFile,
    expectedListener: { uid: TEST_UID, label: "test" },
    readProc: opts.readProc ?? procReaderFor(mock.port, TEST_UID),
    tenant: "derpy",
    env: {},
  })
  let base: RuntimeHarness
  try {
    base = await startRuntime({
      clock,
      transport: gateway,
      stateFile,
      ...(opts.backoff === undefined ? {} : { backoff: opts.backoff }),
      ...(opts.wallNow === undefined ? {} : { wallNow: opts.wallNow }),
    })
  } catch (error) {
    await mock.stop()
    throw error
  }
  const until = async (check: () => boolean, what: string, timeoutMs = 4_000): Promise<void> => {
    const deadline = Date.now() + timeoutMs
    while (!check()) {
      if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`)
      await new Promise((resolve) => setTimeout(resolve, 2))
      base.runtime.supervisor.flush()
      await base.runtime.session.idle()
    }
  }
  return {
    ...base,
    get quits() {
      return base.quits
    },
    mock,
    gateway,
    token,
    until,
    dispose: async () => {
      await base.dispose()
      await mock.stop()
    },
  }
}
