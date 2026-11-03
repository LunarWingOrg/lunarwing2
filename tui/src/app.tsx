/**
 * Layout and composition (design 3.2, 3.12.1). The vibetui4 app.tsx (state, socket handling and layout in one
 * component, the C1/C5/C8 locus) is replaced by:
 *
 * - createAppRuntime(): the one place controllers are wired (store, supervisor + coalescer, session, keymap,
 *   composer handle, credential buffer, voice, copy, notifier). main.tsx calls it with the real transport,
 *   renderer clock and child processes; tests call it with fakes, so both run the same wiring.
 * - <App runtime>: reads the store and the terminal size and renders the rows of state/layout.ts, every child
 *   with an explicit height and flexShrink 0: header, transcript (one scrollbox with every turn), approval panel,
 *   notices, attachment bar, composer, footer; the info panel is an overlay.
 */
import type { CliRenderer, Clock, ScrollBoxRenderable } from "@opentui/core"
import { useBlur, useFocus, useKeyboard, usePaste, useRenderer, useSelectionHandler, useTerminalDimensions } from "@opentui/react"
import { useEffect, useState, type ReactNode } from "react"
import type { ChildEnv, SpawnFn, StateFile, UiConfig, VoiceConfig } from "./config/types.ts"
import type { DebugLog } from "./log.ts"
import { ApprovalPanel } from "./components/approval-panel.tsx"
import { AttachmentBar } from "./components/attachment-bar.tsx"
import { Composer } from "./components/composer.tsx"
import { Footer } from "./components/footer.tsx"
import { Header } from "./components/header.tsx"
import { InfoPanel, infoPanelColumns, infoPanelRows } from "./components/info-panel.tsx"
import { micLabel, noMicAvailable } from "./components/mic-widget.tsx"
import { Notices } from "./components/notices.tsx"
import { resolveTheme, type Theme } from "./components/theme.ts"
import { Transcript } from "./components/transcript.tsx"
import { createComposerHandle, type ComposerController } from "./controller/composer-handle.ts"
import { createCopyController, type CopyController } from "./controller/copy.ts"
import { bindCredentialInput, createCredentialInput, type CredentialInput } from "./controller/credential-input.ts"
import { footerHint } from "./controller/footer-hint.ts"
import { createKeyController, pttLabel, type KeyController, type KeyTargets } from "./controller/keymap.ts"
import { createNotifier, type Notifier } from "./controller/notifier.ts"
import { buildPanel, panelTitle, pttHelp, pttUnavailableHelp, type PanelEnv } from "./controller/panel-content.ts"
import { createSession, type SessionController, type SessionImages } from "./controller/session.ts"
import { createSupervisor, type Supervisor } from "./controller/supervisor.ts"
import type { ClipboardReader } from "./images/clipboard.ts"
import type { ImageExporter } from "./images/export.ts"
import { describeImageProtocol, type ImageProtocolStatus } from "./images/protocol.ts"
import { createInitialState, type AppState, type Store } from "./state/model.ts"
import type { TurnIdFactory } from "./state/ids.ts"
import { computeLayout } from "./state/layout.ts"
import { createStore, StoreProvider, useAppState } from "./state/store.ts"
import type { Backoff } from "./transport/backoff.ts"
import type { Transport } from "./transport/types.ts"
import { createVoiceController } from "./voice/manager.ts"
import type { HexSourceOptions } from "./voice/sources/hex-locate.ts"
import { createVoiceSources } from "./voice/sources/index.ts"
import type { Recorder, VoiceController, VoiceSource } from "./voice/types.ts"

// ---------------------------------------------------------------------------------------------------------
// The runtime
// ---------------------------------------------------------------------------------------------------------

export interface RuntimeConfig {
  profile: string
  tenant: string | null
  /** False for a secondary instance of the profile ("second window"). */
  primary: boolean
  ui: UiConfig
  voice: VoiceConfig
  /** /img and drag-and-drop resolve relative paths against this directory. */
  cwd: string
  home?: string
  /** For /transport: how the listener owner is checked ("uid 1010 (derpy)"), null for a remote URL. */
  listenerOwner: string | null
  /** For /transport: TokenSource.describe(), never the token. */
  tokenSource: string | null
}

export interface RuntimeDeps {
  clock: Clock
  /**
   * The wall clock (epoch ms), default Date.now. `clock` is the renderer's SystemClock (performance.now(), not
   * epoch milliseconds), so everything compared with server times or written as a date uses this instead:
   * the history resync, export file names and the state file's first-use stamps.
   */
  wallNow?: () => number
  transport: Transport
  stateFile: StateFile
  config: RuntimeConfig
  view: { width: number; height: number }
  /** The allowlisted child environment (config/child-env.ts buildChildEnv()). */
  env: ChildEnv
  /** [OpenTUI Audio, parec]; [] with --no-voice, so no audio engine exists. */
  recorders?: Recorder[]
  /** Default: createVoiceSources() of this build. */
  sources?: VoiceSource[]
  /**
   * Collaborators of the Hex sources when `sources` is omitted (voice/sources/index.ts VoiceSourceDeps.hex). Tests
   * pass hermetic ones so no real discovery file, `hex` binary or Hex token is touched; production omits it.
   */
  hex?: HexSourceOptions
  clipboard?: ClipboardReader | null
  exporter?: ImageExporter | null
  spawn?: SpawnFn
  backoff?: Backoff
  log?: DebugLog
  newTurnId?: TurnIdFactory
  /** Config notices (config/config.ts ResolvedConfig.notices), pushed at start. */
  notices?: readonly { level: "info" | "warn"; text: string; sticky: boolean }[]
  /** process.env.NO_COLOR */
  noColor?: string | undefined
  readFile?: SessionImages["readFile"]
  screenshot?: SessionImages["screenshot"]
  /** After the controllers shut down on /quit or Ctrl+C twice (main.tsx: renderer.destroy()). */
  onQuit?: () => void
  /** renderer.copyToClipboardOSC52, set by attachRenderer when omitted. */
  osc52?: (text: string) => boolean
}

export interface AppRuntime {
  readonly store: Store
  readonly clock: Clock
  readonly theme: Theme
  readonly config: RuntimeConfig
  readonly supervisor: Supervisor
  readonly session: SessionController
  readonly composer: ComposerController
  readonly credential: CredentialInput
  readonly keys: KeyController
  readonly voice: VoiceController
  readonly copy: CopyController
  readonly notifier: Notifier
  /** Terminal facts from the renderer: tmux, KKP, the image protocol, OSC 52. Returns the cleanup. */
  attachRenderer(renderer: CliRenderer): () => void
  setTranscript(box: ScrollBoxRenderable | null): void
  panelEnv(width: number, now: number): PanelEnv
  imageStatus(): ImageProtocolStatus | null
  blur(): void
  focus(): void
  start(): void
  /** /quit, Ctrl+C twice: shut down, then onQuit. */
  quit(): void
  /** Idempotent: supervisor stop (transport closed), voice and child processes disposed, exports removed. */
  shutdown(reason?: string): Promise<void>
}

export function createAppRuntime(deps: RuntimeDeps): AppRuntime {
  return new Runtime(deps)
}

class Runtime implements AppRuntime {
  readonly store: Store
  readonly clock: Clock
  readonly theme: Theme
  readonly config: RuntimeConfig
  readonly supervisor: Supervisor
  readonly session: SessionController
  readonly composer: ComposerController
  readonly credential: CredentialInput
  readonly keys: KeyController
  readonly voice: VoiceController
  readonly copy: CopyController
  readonly notifier: Notifier
  private transcript: ScrollBoxRenderable | null = null
  private renderer: CliRenderer | null = null
  private imageProtocol: ImageProtocolStatus | null = null
  private readonly unbindCredential: () => void
  private shutdownPromise: Promise<void> | null = null
  private started = false

  constructor(private readonly deps: RuntimeDeps) {
    const { clock, config } = deps
    this.clock = clock
    this.config = config
    this.theme = resolveTheme({ color: config.ui.color, noColor: deps.noColor })
    this.store = createStore(
      createInitialState({
        profile: config.profile,
        tenant: config.tenant,
        view: deps.view,
        ui: { color: this.theme.color, reduceMotion: config.ui.reduceMotion, maxTurns: config.ui.maxTurnsInMemory },
        at: clock.now(),
      }),
    )
    this.composer = createComposerHandle({ store: this.store, clock })
    this.credential = createCredentialInput()
    this.unbindCredential = bindCredentialInput(this.store, this.credential)
    this.supervisor = createSupervisor({
      transport: deps.transport,
      store: this.store,
      clock,
      ...(deps.wallNow === undefined ? {} : { wallNow: deps.wallNow }),
      ...(deps.backoff === undefined ? {} : { backoff: deps.backoff }),
      ...(deps.log === undefined ? {} : { log: deps.log }),
      onTick: (at) => this.session.tick(at),
    })
    this.voice = this.createVoice()
    this.copy = createCopyController({
      store: this.store,
      clock,
      writer: () => deps.clipboard?.hostClipboard() ?? null,
      env: deps.env,
      ...(deps.spawn === undefined ? {} : { spawn: deps.spawn }),
      osc52: (text) => deps.osc52?.(text) ?? this.renderer?.copyToClipboardOSC52(text) ?? false,
    })
    this.notifier = createNotifier({
      store: this.store,
      clock,
      env: deps.env,
      mode: config.ui.notify,
      name: config.tenant ?? config.profile,
      ...(deps.spawn === undefined ? {} : { spawn: deps.spawn }),
    })
    this.session = this.createSession()
    this.keys = createKeyController({
      store: this.store,
      clock,
      composer: this.composer,
      credential: this.credential,
      voice: this.voice,
      targets: this.keyTargets(),
      pttKeys: config.voice.pttKeys,
      panelPage: () => infoPanelRows(this.store.getState().view.height),
    })
  }

  private createVoice(): VoiceController {
    const { deps, store, clock } = this
    const sources =
      deps.sources ??
      createVoiceSources({
        store,
        clock,
        transport: () => this.supervisor.transport,
        config: deps.config.voice,
        ...(deps.hex === undefined ? {} : { hex: deps.hex }),
      })
    return createVoiceController({
      store,
      clock,
      ...(deps.wallNow === undefined ? {} : { wallNow: deps.wallNow }),
      composer: this.composer,
      recorders: deps.config.voice.enabled ? (deps.recorders ?? []) : [],
      sources,
      config: deps.config.voice,
      stateFile: deps.stateFile,
      ...(deps.log === undefined ? {} : { log: deps.log }),
    })
  }

  private createSession(): SessionController {
    const { deps } = this
    return createSession({
      store: this.store,
      clock: this.clock,
      ...(deps.wallNow === undefined ? {} : { wallNow: deps.wallNow }),
      supervisor: this.supervisor,
      composer: this.composer,
      credential: this.credential,
      voice: this.voice,
      images: {
        clipboard: deps.clipboard ?? null,
        exporter: deps.exporter ?? null,
        env: deps.env,
        ...(deps.spawn === undefined ? {} : { spawn: deps.spawn }),
        ...(deps.readFile === undefined ? {} : { readFile: deps.readFile }),
        ...(deps.screenshot === undefined ? {} : { screenshot: deps.screenshot }),
      },
      copy: this.copy,
      stateFile: deps.stateFile,
      config: { profile: deps.config.profile, tenant: deps.config.tenant, ui: deps.config.ui, cwd: deps.config.cwd, ...(deps.config.home === undefined ? {} : { home: deps.config.home }) },
      quit: () => this.quit(),
      ...(deps.newTurnId === undefined ? {} : { newTurnId: deps.newTurnId }),
      ...(deps.log === undefined ? {} : { log: deps.log }),
    })
  }

  /** The session's key targets plus the transcript scroll, which needs the scrollbox. */
  private keyTargets(): KeyTargets {
    const session = (): SessionController => this.session
    return {
      submitCredential: () => void session().submitCredential(),
      interruptAndSend: () => void session().interruptAndSend(),
      answerFocused: (decision) => session().answerFocused(decision),
      viewApproval: () => session().viewApproval(),
      nextApproval: () => session().nextApproval(),
      pasteClipboard: () => session().pasteClipboard(),
      removeLastAttachment: () => session().removeLastAttachment(),
      clearAttachments: () => session().clearAttachments(),
      historyPrev: () => session().historyPrev(),
      historyNext: () => session().historyNext(),
      credentialEscape: () => session().credentialEscape(),
      dropPaste: (text) => session().dropPaste(text),
      quit: () => this.quit(),
      scrollTranscript: (direction) => this.scrollTranscript(direction),
      notify: (level, text) => session().notify(level, text),
    }
  }

  private scrollTranscript(direction: -1 | 1): void {
    const box = this.transcript
    if (box === null || box.isDestroyed) return
    const state = this.store.getState()
    const rows = computeLayout({
      width: state.view.width,
      height: state.view.height,
      notices: state.notices.length,
      approvalPending: state.approvalQueue.length > 0,
      pendingImages: state.composer.pendingImages.length,
      composerLines: state.composer.lines,
    }).transcript
    box.scrollBy(direction * Math.max(1, rows - 2))
  }

  attachRenderer(renderer: CliRenderer): () => void {
    this.renderer = renderer
    const learn = (): void => {
      const caps = renderer.capabilities
      const at = this.clock.now()
      this.imageProtocol = describeImageProtocol(this.config.ui.imageProtocol, caps, renderer.resolution !== null)
      const tmux = caps?.multiplexer === "tmux"
      const kkp = renderer.useKittyKeyboard && caps?.kitty_keyboard === true
      this.store.dispatch({ type: "ui/patch", patch: { tmux, ...(kkp ? { kkpRelease: true } : {}) }, at })
    }
    learn()
    renderer.on("capabilities", learn)
    return () => {
      renderer.off("capabilities", learn)
    }
  }

  setTranscript(box: ScrollBoxRenderable | null): void {
    this.transcript = box
  }

  imageStatus(): ImageProtocolStatus | null {
    return this.imageProtocol
  }

  panelEnv(width: number, now: number): PanelEnv {
    const tenant = this.config.tenant
    return {
      width,
      now,
      pttKeys: this.config.voice.pttKeys,
      voice: this.voice.status(),
      devices: this.session.panelData.devices,
      threads: this.session.panelData.threads,
      imageProtocol: this.imageProtocol,
      listenerOwner: this.config.listenerOwner,
      tokenSource: this.config.tokenSource,
      visionVerified: tenant !== null && this.config.ui.visionVerifiedTenants.includes(tenant),
    }
  }

  blur(): void {
    this.store.dispatch({ type: "ui/blur", at: this.clock.now() })
    this.voice.blur()
  }

  focus(): void {
    this.store.dispatch({ type: "ui/focus", at: this.clock.now() })
    this.voice.focus()
  }

  start(): void {
    if (this.started) return
    this.started = true
    const at = this.clock.now()
    for (const notice of this.deps.notices ?? []) {
      this.store.dispatch({ type: "notice/push", notice: { level: notice.level, source: "client", text: notice.text, sticky: notice.sticky }, at })
    }
    this.supervisor.start()
  }

  quit(): void {
    void this.shutdown("quit").then(() => this.deps.onQuit?.())
  }

  shutdown(reason = "quit"): Promise<void> {
    this.shutdownPromise ??= this.runShutdown(reason)
    return this.shutdownPromise
  }

  private async runShutdown(reason: string): Promise<void> {
    this.unbindCredential()
    this.credential.clear()
    this.notifier.dispose()
    const steps: Promise<unknown>[] = [this.supervisor.stop(reason), this.voice.dispose()]
    if (this.deps.clipboard) steps.push(this.deps.clipboard.dispose())
    if (this.deps.exporter) steps.push(this.deps.exporter.dispose())
    await Promise.allSettled(steps)
  }
}

// ---------------------------------------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------------------------------------

/** States whose labels count seconds (turn timers, REC, reconnect countdown, transcription). */
function needsTimer(state: AppState): boolean {
  if (state.conn.kind === "reconnecting") return true
  if (state.voice.phase !== "idle" || state.voice.queue.length > 0) return true
  if (state.composer.credential?.escArmedAt != null) return true
  return state.inflight.some((id) => {
    const phase = state.turns[id]?.phase
    return phase === "pending" || phase === "sent" || phase === "running" || phase === "queued"
  })
}

/** Re-renders once a second while `active`, on the runtime's Clock (ManualClock in tests). */
function useClockTick(clock: Clock, active: boolean): number {
  const [, setTick] = useState(0)
  useEffect(() => {
    if (!active) return undefined
    const handle = clock.setInterval(() => setTick((tick) => tick + 1), 1_000)
    return () => clock.clearInterval(handle)
  }, [clock, active])
  return clock.now()
}

export function App(props: { runtime: AppRuntime }): ReactNode {
  return (
    <StoreProvider store={props.runtime.store}>
      <AppView runtime={props.runtime} />
    </StoreProvider>
  )
}

function AppView({ runtime }: { runtime: AppRuntime }): ReactNode {
  const state = useAppState()
  const dims = useTerminalDimensions()
  const renderer = useRenderer()
  const now = useClockTick(runtime.clock, needsTimer(state))

  useEffect(() => runtime.attachRenderer(renderer), [runtime, renderer])
  useEffect(() => {
    const sync = (): void => runtime.composer.syncLayout()
    renderer.on("frame", sync)
    return () => {
      renderer.off("frame", sync)
    }
  }, [runtime, renderer])
  useEffect(() => {
    runtime.store.dispatch({ type: "view/resize", width: dims.width, height: dims.height, at: runtime.clock.now() })
    runtime.composer.reportMetrics()
  }, [runtime, dims.width, dims.height])
  useKeyboard((key) => runtime.keys.handleKey(key), { release: true })
  usePaste((event) => runtime.keys.handlePaste(event))
  useSelectionHandler((selection) => runtime.copy.onSelection(selection))
  useBlur(() => runtime.blur())
  useFocus(() => runtime.focus())

  const { width, height } = dims
  const theme = runtime.theme
  const layout = computeLayout({
    width,
    height,
    notices: state.notices.length,
    approvalPending: state.approvalQueue.length > 0,
    pendingImages: state.composer.pendingImages.length,
    composerLines: state.composer.credential !== null ? 1 : state.composer.lines,
  })
  const ptt = pttLabel(runtime.config.voice.pttKeys)
  const voiceReport = runtime.voice.status()
  // "(no mic)": --no-voice, or no recorder or source; then nothing offers the push-to-talk key.
  const noMic = noMicAvailable(voiceReport)
  const panel = state.panel
  return (
    <box style={{ width, height, flexDirection: "column" }} onMouseUp={() => runtime.voice.micUp()}>
      <Header state={state} info={{ primary: runtime.config.primary, imageLabel: runtime.imageStatus()?.label ?? null }} width={width} now={now} theme={theme} />
      <Transcript
        state={state}
        height={layout.transcript}
        theme={theme}
        now={now}
        protocol={runtime.config.ui.imageProtocol}
        imageColumns={layout.imageColumns}
        pttHelp={noMic ? pttUnavailableHelp(voiceReport) : pttHelp(state, runtime.config.voice.pttKeys)}
        onScrollbox={(box) => runtime.setTranscript(box)}
      />
      {layout.approval > 0 && (
        <ApprovalPanel
          state={state}
          rows={layout.approval}
          width={width}
          theme={theme}
          onAnswer={(decision) => runtime.session.answerFocused(decision)}
          onView={() => runtime.session.viewApproval()}
        />
      )}
      <Notices notices={state.notices} rows={layout.notices} width={width} theme={theme} />
      <AttachmentBar
        images={state.composer.pendingImages}
        rows={layout.attachments}
        width={width}
        tenant={state.tenant}
        protocol={runtime.config.ui.imageProtocol}
        colors={{ text: theme.fg.text, dim: theme.fg.dim, border: theme.fg.border }}
      />
      <Composer
        state={state}
        rows={layout.composer}
        width={width}
        theme={theme}
        now={now}
        ptt={ptt}
        composer={runtime.composer}
        onSubmit={() => void runtime.session.submit()}
        micLabel={micLabel(state.voice, { noMic, now })}
        onMicDown={() => runtime.voice.micDown()}
        onMicUp={() => runtime.voice.micUp()}
        cursorBlink={runtime.config.ui.cursorBlink}
        focused
      />
      {layout.footer > 0 && <Footer text={footerHint(state, { now, ptt, voice: !noMic })} width={width} theme={theme} />}
      {panel !== null && (
        <InfoPanel
          kind={panel.kind}
          title={panelTitle(panel.kind)}
          lines={buildPanel(panel.kind, state, runtime.panelEnv(infoPanelColumns(width), now))}
          scroll={panel.scroll}
          viewWidth={width}
          viewHeight={height}
          theme={theme}
        />
      )}
    </box>
  )
}
