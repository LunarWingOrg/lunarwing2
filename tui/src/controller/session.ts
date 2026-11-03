/**
 * The session controller (design 3.6, 3.10): every user intent that reaches the daemon goes through here.
 *
 * - Enter (submit): a pending typed-approval confirmation is applied; a guarded send (approval keyword with
 *   nothing pending, //restart) goes out on the second Enter; local slash commands run (controller/commands.ts);
 *   "//<text>" and other text are sent through the guards.
 * - Guards on every send and steer (3.10.2, 3.10.3): /quit, /exit and /shutdown never reach the wire; while a
 *   prompt is pending a bare keyword (normalized, "Yes." included) is never sent but asks for a confirming Enter;
 *   with nothing pending a daemon keyword ("ok") needs a second Enter.
 * - Records exist before the request is written (turn/created, control/created) and the coalescer is flushed
 *   first, so events overtaking the gateway's 202 still attach. Nothing is ever re-sent automatically; a failed
 *   send restores the draft into an empty composer or moves it to history (3.10.5).
 * - Approvals (3.6.2): keys, clicks, commands and typed replies act only on the prompt that is on screen (the
 *   panel's focused prompt, or the full view's own prompt while it is pending; never one an info panel covers),
 *   after it was on screen for 1 s, and approving waits for the full view when text is hidden; deny is always
 *   allowed.
 * - Credential mode (3.6.4): the masked buffer goes to POST /api/chat/auth-token (gateway) or, on the socket,
 *   as the next message; Esc Esc cancels through auth-cancel; on V1 or unknown engines the mode ends after 290 s,
 *   unless a V2 authentication gate (requestId) backs it: that one ends with its gate_resolved.
 * - Images (section 6): Ctrl+V, drag-and-drop, /img, /attach, /shot, /image, with the first-use storage notice
 *   and the vision notice.
 */
import type { Clock } from "@opentui/core"
import { homedir } from "node:os"
import type { ChildEnv, SpawnFn, StateFile, UiConfig } from "../config/types.ts"
import type { DebugLog } from "../log.ts"
import { acknowledgeImageSource, createPendingImage, imageStorageNotice, readImageFile, type ReadImageFileOptions } from "../images/attach.ts"
import type { ClipboardReader } from "../images/clipboard.ts"
import { parseImageCommand, pickGeneratedImage, type ImageExporter } from "../images/export.ts"
import { checkAttachLimits, checkSendLimits, formatBytes, imageInputRefusal, imageLimitsFor, isImageInputError } from "../images/limits.ts"
import { looksLikeDrop, parseDroppedPaths } from "../images/paste-paths.ts"
import { takeScreenshot, type TakeScreenshotOptions } from "../images/screenshot.ts"
import type { ApprovalDecision, ApprovalPrompt, AppState, ControlKind, PanelKind, PendingImage, Store, ThreadId, TurnId } from "../state/model.ts"
import { canStartTurn, displayedApproval, focusedApproval, localInFlightCap } from "../state/correlate.ts"
import { approvalCapsForRows } from "../state/approval-measure.ts"
import { layoutForState } from "../state/layout.ts"
import { newTurnId, type TurnIdFactory } from "../state/ids.ts"
import { isTransportError, TransportError } from "../transport/errors.ts"
import { isForbiddenContent, normalizeCommandText } from "../transport/guards.ts"
import type { OutgoingImage, ThreadSpec, Transport } from "../transport/types.ts"
import type { VoiceController } from "../voice/types.ts"
import {
  ALWAYS_NEEDS_KEY,
  APPROVAL_PENDING_HINT,
  approvalGate,
  approvalNotShown,
  gateRefusal,
  isDaemonApprovalKeyword,
  keywordGuardNotice,
  legacyKeyword,
  matchApprovalReply,
} from "./approval-reply.ts"
import { parseSlash, runSlash, unescapeSlash, type CommandContext, type CommandOutcome, type ControlCommandKind } from "./commands.ts"
import type { CopyController } from "./copy.ts"
import type { CredentialInput } from "./credential-input.ts"
import type { KeyTargets } from "./keymap.ts"
import type { ThreadRow } from "./panel-content.ts"
import type { Supervisor } from "./supervisor.ts"
import type { ComposerHandle } from "./types.ts"
import { utf8Bytes } from "../util.ts"

/** A guarded send (keyword with nothing pending) waits this long for its second Enter (3.10.3). */
export const KEYWORD_GUARD_MS = 10_000
/** //restart waits this long for its second Enter (R17). */
export const RESTART_GUARD_MS = 5_000
/** V1 and unknown engines: credential mode ends below the daemon's 300 s TTL (3.6.4). */
export const CREDENTIAL_TTL_MS = 290_000
/** Daemons without progress events (legacy socket): a per-turn stall notice after this long (3.6.2, 3.7). */
export const STALL_NOTICE_MS = 120_000
export const STALL_NOTICE =
  "no reply for 120 s: the daemon may be waiting for an approval (/approve, /deny) or still working (Ctrl+S stops it)"

/** Optional transport capability: the gateway's GET /api/chat/threads (not part of the Transport interface yet). */
export interface ThreadLister {
  listThreads(): Promise<ThreadRow[]>
}

function hasThreadLister(transport: Transport): transport is Transport & ThreadLister {
  return typeof (transport as Partial<ThreadLister>).listThreads === "function"
}

export interface SessionImages {
  clipboard: ClipboardReader | null
  exporter: ImageExporter | null
  env: ChildEnv
  spawn?: SpawnFn
  /** Default images/attach.ts readImageFile. */
  readFile?: (path: string, opts: ReadImageFileOptions) => Promise<PendingImage>
  /** Default images/screenshot.ts takeScreenshot. */
  screenshot?: (opts: TakeScreenshotOptions) => Promise<PendingImage | null>
}

export interface SessionConfig {
  profile: string
  tenant: string | null
  ui: Pick<UiConfig, "visionVerifiedTenants">
  cwd: string
  home?: string
}

export type SessionSupervisor = Pick<Supervisor, "transport" | "flush" | "reconnectNow" | "selectThread" | "reportFatal" | "resync">

export interface SessionDeps {
  store: Store
  clock: Clock
  /** The wall clock (epoch ms) for export names and first-use stamps; default Date.now (the Clock is not epoch). */
  wallNow?: () => number
  supervisor: SessionSupervisor
  composer: ComposerHandle
  credential: CredentialInput
  voice: VoiceController | null
  images: SessionImages
  copy: CopyController | null
  stateFile: StateFile
  config: SessionConfig
  /** Quit the TUI: supervisor stop, transport close, voice and children disposed, renderer destroyed. */
  quit: () => void
  newTurnId?: TurnIdFactory
  log?: DebugLog
}

/** Data the devices and threads panels show, fetched when the panel opens. */
export interface PanelData {
  devices: { index: number; name: string; isDefault: boolean }[] | null
  threads: ThreadRow[] | null
}

export interface SessionController extends KeyTargets {
  /** Enter (the textarea's onSubmit). */
  submit(): Promise<void>
  /** Ctrl+S / Ctrl+Enter. */
  interruptAndSend(): Promise<void>
  submitCredential(): Promise<void>
  /** Runs one composer text as if typed and submitted (tests, dictation never uses it). */
  runText(text: string): Promise<void>
  /** Session timers on the supervisor tick (credential TTL). */
  tick(at: number): void
  readonly panelData: PanelData
  /** Resolves once every async operation started by an intent has settled (tests). */
  idle(): Promise<void>
}

export function createSession(deps: SessionDeps): SessionController {
  return new Session(deps)
}

function asError(error: unknown): TransportError {
  if (isTransportError(error)) return error
  return new TransportError("protocol", error instanceof Error ? error.message : "request failed")
}

function toOutgoing(image: PendingImage): OutgoingImage {
  return { mime: image.mime, bytes: image.bytes, filename: image.filename }
}

class Session implements SessionController {
  readonly panelData: PanelData = { devices: null, threads: null }
  private readonly pending = new Set<Promise<unknown>>()
  private readonly newId: TurnIdFactory
  private historyIndex: number | null = null
  private historyDraft = ""
  private historyText: string | null = null
  private visionNoticeShown = false
  /** Turns that already got the stall notice. */
  private readonly stalled = new Set<TurnId>()
  private readonly commands: CommandContext

  constructor(private readonly deps: SessionDeps) {
    this.newId = deps.newTurnId ?? newTurnId
    this.commands = this.commandContext()
  }

  // ---- small helpers ----

  private get state(): AppState {
    return this.deps.store.getState()
  }

  private get transport(): Transport {
    return this.deps.supervisor.transport
  }

  private now(): number {
    return this.deps.clock.now()
  }

  /** Epoch milliseconds: what a date shown to the user or written to a file needs. */
  private wallNow(): number {
    return (this.deps.wallNow ?? Date.now)()
  }

  notify(level: "info" | "warn" | "error", text: string, source: "client" | "image" | "transport" | "auth" | "voice" = "client"): void {
    this.deps.store.dispatch({ type: "notice/push", notice: { level, source, text }, at: this.now() })
  }

  /** Tracks an async operation so idle() can wait for it; failures become notices, never unhandled rejections. */
  private track<T>(work: Promise<T>): Promise<T | undefined> {
    const tracked = work.catch((error: unknown) => {
      this.notify("error", error instanceof Error ? error.message : "operation failed")
      return undefined
    })
    this.pending.add(tracked)
    void tracked.finally(() => this.pending.delete(tracked))
    return tracked
  }

  async idle(): Promise<void> {
    while (this.pending.size > 0) await Promise.all([...this.pending])
  }

  private connected(): boolean {
    if (this.state.conn.kind === "ready") return true
    this.notify("warn", "not connected: the message stays in the composer (/transport shows why)")
    return false
  }

  /** Layout caps of the approval panel, so key and command gates measure what the panel shows. */
  private approvalCaps(state: AppState) {
    const rows = layoutForState(state).approval
    return approvalCapsForRows(rows === 0 ? 8 : rows)
  }

  // ---- Enter ----

  submit(): Promise<void> {
    return this.track(this.submitNow()).then(() => undefined)
  }

  runText(text: string): Promise<void> {
    this.deps.composer.replaceText(text)
    return this.submit()
  }

  private async submitNow(): Promise<void> {
    const state = this.state
    if (state.composer.credential !== null) return this.submitCredentialNow()
    if (state.composer.confirm !== null) return this.applyConfirmation()
    const text = this.deps.composer.getText()
    const guard = state.composer.guard
    const confirmed = guard !== null && guard.until > this.now() && guard.text === text
    if (guard !== null) this.deps.store.dispatch({ type: "composer/guard_clear", at: this.now() })
    await this.route(text, confirmed)
  }

  private async route(text: string, confirmed: boolean): Promise<void> {
    const trimmed = text.trim()
    if (trimmed === "") {
      if (this.state.composer.pendingImages.length > 0) await this.sendChat("", text, confirmed)
      return
    }
    const forwarded = unescapeSlash(trimmed)
    if (forwarded !== null) {
      await this.sendChat(forwarded, text, confirmed)
      return
    }
    const command = parseSlash(trimmed)
    if (command === null) {
      await this.sendChat(text, text, confirmed)
      return
    }
    this.deps.store.dispatch({ type: "composer/history_push", text: trimmed, at: this.now() })
    const outcome = await runSlash(command, this.commandsFor(text, confirmed))
    // A sent message cleared the composer already; local commands clear here unless the user typed meanwhile.
    if (outcome === "done" && this.deps.composer.getText() === text) this.deps.composer.clear()
  }

  // ---- the guarded send path ----

  /**
   * Sends `content` (what goes on the wire; `draft` is the composer text it came from) after the guards. Returns
   * without sending, the draft kept, when a guard needs another Enter or refuses.
   */
  private async sendChat(content: string, draft: string, confirmed: boolean): Promise<CommandOutcome> {
    if (isForbiddenContent(content)) {
      this.notify("warn", "not sent: /quit, /exit and /shutdown are never sent to the daemon")
      return "keep"
    }
    if (!confirmed && normalizeCommandText(content) === "/restart") {
      this.setGuard(draft, "restart", RESTART_GUARD_MS, "press Enter again within 5 s to send /restart to the daemon")
      return "keep"
    }
    if (this.keywordIntercepted(content, draft, confirmed)) return "keep"
    return this.sendMessage(content)
  }

  /** 3.10.3: true when the text was an approval keyword and nothing may be sent now. */
  private keywordIntercepted(content: string, draft: string, confirmed: boolean): boolean {
    const state = this.state
    const pending = state.server?.capabilities.approvals === false ? null : focusedApproval(state)
    if (pending !== null) {
      const decision = matchApprovalReply(content)
      if (decision === null) return false
      // A bare keyword is never sent while a prompt is pending; it answers only the prompt that is on screen.
      const prompt = this.answerable(state)
      if (prompt !== null) this.requestConfirmation(prompt, decision)
      return true
    }
    if (confirmed || !isDaemonApprovalKeyword(content)) return false
    this.setGuard(draft, "approval_keyword", KEYWORD_GUARD_MS, keywordGuardNotice(content))
    return true
  }

  private requestConfirmation(prompt: ApprovalPrompt, decision: ApprovalDecision): void {
    if (decision === "always") {
      this.notify("warn", ALWAYS_NEEDS_KEY)
      return
    }
    const state = this.state
    const gate = approvalGate(prompt, state.composer, state.view.width, this.now(), {
      decision,
      via: "text",
      caps: this.approvalCaps(state),
    })
    if (gate !== "ok") {
      this.notify("warn", `${APPROVAL_PENDING_HINT} - ${gateRefusal(gate, prompt)}`)
      return
    }
    this.deps.store.dispatch({ type: "approval/confirm_requested", approvalId: prompt.id, decision, at: this.now() })
  }

  /** The second Enter of a typed or dictated reply. */
  private async applyConfirmation(): Promise<void> {
    const state = this.state
    const confirm = state.composer.confirm
    if (confirm === null) return
    const prompt = state.approvals[confirm.approvalId]
    const shown = displayedApproval(state)
    if (prompt === undefined || shown?.id !== prompt.id || prompt.state !== "pending") {
      this.deps.store.dispatch({ type: "approval/confirm_cleared", at: this.now() })
      this.notify("warn", "the approval changed; check the panel and answer again")
      return
    }
    this.deps.composer.clear()
    await this.answer(prompt, confirm.decision)
  }

  private setGuard(text: string, reason: "approval_keyword" | "restart", ms: number, notice: string): void {
    const at = this.now()
    this.deps.store.dispatch({ type: "composer/guard", text, reason, until: at + ms, at })
    this.notify("warn", notice)
  }

  /** Every send of chat text: connection, in-flight cap, size and image checks, then turn/created and send(). */
  private async sendMessage(text: string): Promise<CommandOutcome> {
    if (!this.connected()) return "keep"
    const state = this.state
    if (!canStartTurn(state)) {
      this.notify("warn", `${localInFlightCap(state)} messages are in flight; wait for one to finish`)
      return "keep"
    }
    const images = [...state.composer.pendingImages]
    if (!this.checkOutgoing(state, text, images)) return "keep"
    const turnId = this.newId()
    const at = this.now()
    this.deps.supervisor.flush()
    this.deps.store.dispatch({ type: "turn/created", turnId, text, images, at })
    if (text.trim() !== "") this.deps.store.dispatch({ type: "composer/history_push", text, at })
    this.resetHistoryCursor()
    this.deps.composer.clear()
    if (images.length > 0) this.visionNotice()
    this.deps.log?.event("turn.send", { bytes: utf8Bytes(text), images: images.length })
    try {
      const result = await this.transport.send({ turnId, text, images: images.map(toOutgoing) })
      this.deps.store.dispatch({ type: "turn/sent", turnId, ...(result.serverMessageId === undefined ? {} : { serverMessageId: result.serverMessageId }), at: this.now() })
    } catch (error) {
      this.sendFailed(turnId, asError(error))
    }
    return "done"
  }

  private checkOutgoing(state: AppState, text: string, images: readonly PendingImage[]): boolean {
    const server = state.server
    const limits = server?.limits
    const bytes = utf8Bytes(text)
    if (limits !== undefined && bytes > limits.maxContentBytes) {
      this.notify("warn", `the message is ${formatBytes(bytes)}; the limit is ${formatBytes(limits.maxContentBytes)}`)
      return false
    }
    if (images.length === 0) return true
    const refusal = imageInputRefusal(server)
    if (refusal !== null) {
      this.notify("warn", `${refusal}: remove the attachments (Ctrl+X) to send the text`, "image")
      return false
    }
    if (limits === undefined) return true
    try {
      checkSendLimits({ images, contentBytes: bytes, limits, transport: server?.transport ?? "gateway" })
      return true
    } catch (error) {
      this.notify("warn", isImageInputError(error) ? error.message : "the images cannot be sent", "image")
      return false
    }
  }

  private sendFailed(turnId: TurnId, error: TransportError): void {
    const composerEmpty = this.deps.composer.getText().trim() === "" && this.state.composer.pendingImages.length === 0
    this.deps.store.dispatch({ type: "turn/send_failed", turnId, code: error.code, message: error.message, restore: composerEmpty, at: this.now() })
    const restore = this.state.composer.restore
    if (restore !== null) {
      this.deps.composer.replaceText(restore.text)
      this.deps.store.dispatch({ type: "composer/restore_consumed", at: this.now() })
    }
    if (error.fatal) this.deps.supervisor.reportFatal(error)
  }

  private visionNotice(): void {
    const tenant = this.deps.config.tenant
    if (this.visionNoticeShown) return
    this.visionNoticeShown = true
    if (tenant !== null && this.deps.config.ui.visionVerifiedTenants.includes(tenant)) return
    this.notify("info", `images go to the configured model; vision is not verified for ${tenant ?? this.deps.config.profile}`, "image")
  }

  // ---- controls (3.6.1) ----

  interruptAndSend(): Promise<void> {
    return this.track(this.interruptAndSendNow()).then(() => undefined)
  }

  private async interruptAndSendNow(): Promise<void> {
    if (this.state.composer.credential !== null) return this.submitCredentialNow()
    if (!this.connected()) return
    const text = this.deps.composer.getText()
    const empty = text.trim() === "" && this.state.composer.pendingImages.length === 0
    await this.sendControl("interrupt", "/interrupt")
    if (!empty) await this.submitNow()
  }

  private async sendControl(kind: ControlKind, text: string): Promise<CommandOutcome> {
    if (!this.connected()) return "keep"
    const turnId = this.newId()
    this.deps.supervisor.flush()
    this.deps.store.dispatch({ type: "control/created", turnId, kind, text, at: this.now() })
    try {
      if (kind === "interrupt") await this.transport.interrupt({ requestId: turnId })
      else if (kind === "steer") await this.transport.steer({ requestId: turnId, text: text.replace(/^\/steer\s+/iu, "") })
      else await this.transport.send({ turnId, text, images: [] })
    } catch (error) {
      const failure = asError(error)
      this.deps.store.dispatch({ type: "control/send_failed", turnId, code: failure.code, message: failure.message, at: this.now() })
      if (failure.fatal) this.deps.supervisor.reportFatal(failure)
    }
    return "done"
  }

  // ---- approvals (3.6.2) ----

  /**
   * The prompt on screen, the only one a key, click or command answers (state/correlate.ts displayedApproval).
   * While a prompt is pending but none is on screen (an info panel covers the approval panel, or the full view
   * shows a prompt that is settled) the input is refused with a notice.
   */
  private answerable(state: AppState): ApprovalPrompt | null {
    const prompt = displayedApproval(state)
    if (prompt === null && state.approvalQueue.length > 0) this.notify("warn", approvalNotShown(state.panel))
    return prompt
  }

  answerFocused(decision: ApprovalDecision): void {
    const state = this.state
    const prompt = this.answerable(state)
    if (prompt === null) return
    if (decision === "always" && !prompt.allowAlways) {
      this.notify("warn", "this tool cannot be allowed permanently; Alt+Y approves once")
      return
    }
    const gate = approvalGate(prompt, state.composer, state.view.width, this.now(), { decision, via: "key", caps: this.approvalCaps(state) })
    if (gate !== "ok") {
      this.notify("warn", gateRefusal(gate, prompt))
      return
    }
    // Alt+Y / Alt+N also apply a pending typed confirmation: the reply text is consumed (3.10.3).
    if (state.composer.confirm?.approvalId === prompt.id) this.deps.composer.clear()
    void this.track(this.answer(prompt, decision))
  }

  private async answerByCommand(decision: ApprovalDecision): Promise<CommandOutcome> {
    const state = this.state
    if (state.server !== null && !state.server.capabilities.approvals) return this.sendMessage(legacyKeyword(decision))
    if (state.approvalQueue.length === 0) {
      this.notify("info", "no approval is pending")
      return "done"
    }
    const prompt = this.answerable(state)
    if (prompt === null) return "keep"
    if (decision === "always" && !prompt.allowAlways) {
      this.notify("warn", "this tool cannot be allowed permanently; /approve approves once")
      return "keep"
    }
    const gate = approvalGate(prompt, state.composer, state.view.width, this.now(), { decision, via: "text", caps: this.approvalCaps(state) })
    if (gate !== "ok") {
      this.notify("warn", gateRefusal(gate, prompt))
      return "keep"
    }
    await this.answer(prompt, decision)
    return "done"
  }

  private async answer(prompt: ApprovalPrompt, decision: ApprovalDecision): Promise<void> {
    const { store } = this.deps
    store.dispatch({ type: "approval/answering", approvalId: prompt.id, decision, at: this.now() })
    try {
      await this.transport.approve({ approvalId: prompt.id, decision, threadId: prompt.threadId, turnId: prompt.turnId })
      store.dispatch({ type: "approval/answered", approvalId: prompt.id, ok: true, expired: false, at: this.now() })
    } catch (error) {
      const failure = asError(error)
      const expired = failure.code === "not_found"
      store.dispatch({ type: "approval/answered", approvalId: prompt.id, ok: false, expired, message: failure.message, at: this.now() })
      if (failure.fatal) this.deps.supervisor.reportFatal(failure)
    }
  }

  viewApproval(): void {
    const prompt = focusedApproval(this.state)
    if (prompt === null) {
      this.notify("info", "no approval is pending")
      return
    }
    this.deps.store.dispatch({ type: "panel/open", kind: "approval", approvalId: prompt.id, at: this.now() })
  }

  nextApproval(): void {
    this.deps.store.dispatch({ type: "approval/focus_next", at: this.now() })
  }

  // ---- credential mode (3.6.4) ----

  submitCredential(): Promise<void> {
    return this.track(this.submitCredentialNow()).then(() => undefined)
  }

  private async submitCredentialNow(): Promise<void> {
    const credential = this.state.composer.credential
    if (credential === null) return
    if (this.deps.credential.length === 0) {
      this.notify("warn", `type the credential for ${credential.extensionName} first (Esc Esc cancels)`, "auth")
      return
    }
    const secret = this.deps.credential.take()
    this.deps.store.dispatch({ type: "composer/credential_chars", chars: 0, at: this.now() })
    const authToken = this.transport.authToken?.bind(this.transport)
    try {
      if (authToken !== undefined) {
        await authToken({
          extensionName: credential.extensionName,
          token: secret,
          turnId: credential.turnId,
          ...(credential.requestId === undefined ? {} : { requestId: credential.requestId }),
          ...(credential.threadId === null ? {} : { threadId: credential.threadId }),
        })
      } else {
        await this.sendCredentialAsMessage(credential.extensionName, secret.expose())
      }
      this.deps.store.dispatch({ type: "composer/credential_done", outcome: "sent", at: this.now() })
      this.notify("info", `credential sent to ${credential.extensionName}`, "auth")
    } catch (error) {
      this.credentialFailed(asError(error))
    } finally {
      secret.dispose()
    }
  }

  /** Socket transports: the daemon's auth intercept consumes the next message; its text is never stored. */
  private async sendCredentialAsMessage(extensionName: string, text: string): Promise<void> {
    const turnId = this.newId()
    this.deps.supervisor.flush()
    this.deps.store.dispatch({ type: "turn/created", turnId, text: "", images: [], credential: { extensionName }, at: this.now() })
    await this.transport.send({ turnId, text, images: [] })
    this.deps.store.dispatch({ type: "turn/sent", turnId, at: this.now() })
  }

  private credentialFailed(failure: TransportError): void {
    const gone = failure.code === "forbidden" || failure.code === "not_found"
    if (gone) {
      this.deps.store.dispatch({ type: "composer/credential_done", outcome: "ended", at: this.now() })
      return
    }
    this.notify("error", `credential not accepted: ${failure.message} - type it again or press Esc twice`, "auth")
    if (failure.fatal) this.deps.supervisor.reportFatal(failure)
  }

  credentialEscape(): void {
    const credential = this.state.composer.credential
    if (credential === null) return
    const at = this.now()
    if (credential.escArmedAt === null || at - credential.escArmedAt >= 2_000) {
      this.deps.store.dispatch({ type: "composer/credential_esc_armed", armed: true, at })
      return
    }
    void this.track(this.cancelCredential())
  }

  private async cancelCredential(): Promise<void> {
    const credential = this.state.composer.credential
    if (credential === null) return
    this.deps.credential.clear()
    const authCancel = this.transport.authCancel?.bind(this.transport)
    try {
      if (authCancel !== undefined) {
        await authCancel({
          extensionName: credential.extensionName,
          turnId: credential.turnId,
          ...(credential.requestId === undefined ? {} : { requestId: credential.requestId }),
          ...(credential.threadId === null ? {} : { threadId: credential.threadId }),
        })
      } else {
        await this.transport.interrupt({ requestId: this.newId() })
      }
      this.deps.store.dispatch({ type: "composer/credential_done", outcome: "cancelled", at: this.now() })
    } catch (error) {
      const failure = asError(error)
      if (failure.code === "not_found" || failure.code === "forbidden") {
        this.deps.store.dispatch({ type: "composer/credential_done", outcome: "ended", at: this.now() })
        return
      }
      this.deps.store.dispatch({ type: "composer/credential_esc_armed", armed: false, at: this.now() })
      this.notify("error", `cancelling the credential failed: ${failure.message}`, "auth")
    }
  }

  tick(at: number): void {
    const state = this.state
    this.stallNotices(state, at)
    const credential = state.composer.credential
    if (credential === null) return
    const engine = state.server?.engine ?? "unknown"
    // A request id means a V2 authentication gate (a legacy gateway reports engine "unknown"): it waits up to
    // 30 min and ends with gate_resolved, so ending the mode at 290 s would let the next message become the secret.
    const gated = credential.requestId !== undefined
    if (engine !== "v2" && !gated && at - credential.startedAt >= CREDENTIAL_TTL_MS) {
      this.deps.credential.clear()
      this.deps.store.dispatch({ type: "composer/credential_done", outcome: "ended", at })
    }
  }

  /** Without progress events silence says nothing, so after 120 s each open turn gets one notice. */
  private stallNotices(state: AppState, at: number): void {
    if (state.server === null || state.server.capabilities.activity) return
    for (const id of state.inflight) {
      const record = state.turns[id]
      if (record === undefined || this.stalled.has(id)) continue
      const since = record.lastEventAt ?? record.sentAt
      if (since === undefined || at - since < STALL_NOTICE_MS) continue
      this.stalled.add(id)
      this.notify("warn", STALL_NOTICE, "transport")
    }
  }

  // ---- images (section 6) ----

  pasteClipboard(): void {
    void this.track(this.pasteClipboardNow())
  }

  private async pasteClipboardNow(): Promise<void> {
    const reader = this.deps.images.clipboard
    if (reader === null) {
      this.notify("warn", "the clipboard is not available here", "image")
      return
    }
    const paste = await reader.read(new AbortController().signal)
    switch (paste.kind) {
      case "image":
        await this.attachBytes(paste.bytes)
        return
      case "files":
        await this.attachFiles(paste.paths, "clipboard", paste.text)
        return
      case "text":
        this.deps.composer.insertText(paste.text)
        return
      case "empty":
        this.notify("info", "the clipboard is empty", "image")
        return
      case "cancelled":
        return
      case "error":
        this.notify("warn", paste.reason, "image")
        return
    }
  }

  private imagesRefused(): boolean {
    const refusal = imageInputRefusal(this.state.server)
    if (refusal === null) return false
    this.notify("warn", refusal, "image")
    return true
  }

  private async attachBytes(bytes: Uint8Array): Promise<void> {
    if (this.imagesRefused()) return
    const limits = imageLimitsFor(this.state.server)
    try {
      const image = createPendingImage(bytes, {
        source: "clipboard",
        filename: null,
        maxImageBytes: limits.maxImageBytes,
        imageMimeTypes: limits.imageMimeTypes,
      })
      await this.attachPending(image)
    } catch (error) {
      this.imageFailed(error)
    }
  }

  /** composer/attach after the per-message limits, then the first-use storage notice (5.6, SEC-09). */
  private async attachPending(image: PendingImage): Promise<void> {
    const limits = imageLimitsFor(this.state.server)
    checkAttachLimits(this.state.composer.pendingImages, image, limits)
    this.deps.store.dispatch({ type: "composer/attach", image, at: this.now() })
    if (await acknowledgeImageSource(this.deps.stateFile, image.source, this.wallNow())) {
      this.notify("info", imageStorageNotice(this.deps.config.tenant), "image")
    }
  }

  private imageFailed(error: unknown): void {
    this.notify("warn", isImageInputError(error) ? error.message : "the image could not be attached", "image")
  }

  private fileOptions(source: PendingImage["source"]): ReadImageFileOptions {
    const limits = imageLimitsFor(this.state.server)
    return {
      maxImageBytes: limits.maxImageBytes,
      cwd: this.deps.config.cwd,
      home: this.deps.config.home ?? homedir(),
      imageMimeTypes: limits.imageMimeTypes,
      source,
    }
  }

  /** Attaches every path; when one fails, the notice explains why and `fallbackText` is inserted (6.1). */
  private async attachFiles(paths: readonly string[], source: PendingImage["source"], fallbackText: string): Promise<void> {
    if (this.imagesRefused()) {
      this.deps.composer.insertText(fallbackText)
      return
    }
    const read = this.deps.images.readFile ?? readImageFile
    let failed = false
    for (const path of paths) {
      try {
        await this.attachPending(await read(path, this.fileOptions(source)))
      } catch (error) {
        failed = true
        this.imageFailed(error)
      }
    }
    if (failed) this.deps.composer.insertText(fallbackText)
  }

  dropPaste(text: string): boolean {
    const limits = imageLimitsFor(this.state.server)
    const paths = parseDroppedPaths(text, { maxPaths: limits.maxImages })
    if (paths === null) {
      // Pasted as text, but it reads like dropped image files: say so instead of attaching nothing silently (6.1).
      if (looksLikeDrop(text)) {
        this.notify("warn", `not attached: a drop takes up to ${limits.maxImages} absolute image paths; the text was pasted`, "image")
      }
      return false
    }
    void this.track(this.attachFiles(paths, "drop", text))
    return true
  }

  removeLastAttachment(): void {
    this.deps.store.dispatch({ type: "composer/remove_last", at: this.now() })
  }

  clearAttachments(): void {
    this.deps.store.dispatch({ type: "composer/clear_attachments", at: this.now() })
  }

  private async attachFile(path: string): Promise<void> {
    if (this.imagesRefused()) return
    const read = this.deps.images.readFile ?? readImageFile
    try {
      await this.attachPending(await read(path, this.fileOptions("file")))
    } catch (error) {
      this.imageFailed(error)
    }
  }

  private async screenshot(full: boolean): Promise<void> {
    if (this.imagesRefused()) return
    const limits = imageLimitsFor(this.state.server)
    const take = this.deps.images.screenshot ?? takeScreenshot
    try {
      const image = await take({
        full,
        maxBytes: limits.maxImageBytes,
        env: this.deps.images.env,
        clock: this.deps.clock,
        imageMimeTypes: limits.imageMimeTypes,
        ...(this.deps.images.spawn === undefined ? {} : { spawn: this.deps.images.spawn }),
      })
      if (image !== null) await this.attachPending(image)
    } catch (error) {
      this.imageFailed(error)
    }
  }

  private async imageCommand(args: string): Promise<void> {
    const command = parseImageCommand(args)
    if (command.action === "error") {
      this.notify("warn", command.message, "image")
      return
    }
    const exporter = this.deps.images.exporter
    if (exporter === null) {
      this.notify("warn", "image export is not available", "image")
      return
    }
    try {
      const image = pickGeneratedImage(this.state, command.n)
      if (command.action === "open") {
        const path = await exporter.open(image)
        this.notify("info", `opened ${path}`, "image")
      } else {
        const at = new Date(this.wallNow())
        const label = this.deps.config.tenant ?? this.deps.config.profile
        const path = await exporter.save(image, { path: command.path, n: command.n, label, at })
        this.notify("info", `saved ${path}`, "image")
      }
    } catch (error) {
      this.imageFailed(error)
    }
  }

  // ---- history (3.10.5) ----

  historyPrev(): boolean {
    const history = this.state.composer.history
    if (history.length === 0) return false
    this.syncHistoryCursor()
    if (this.historyIndex === null) {
      this.historyDraft = this.deps.composer.getText()
      this.historyIndex = history.length
    }
    if (this.historyIndex === 0) return true
    this.historyIndex -= 1
    this.showHistory(history[this.historyIndex] ?? "")
    return true
  }

  historyNext(): boolean {
    this.syncHistoryCursor()
    if (this.historyIndex === null) return false
    const history = this.state.composer.history
    this.historyIndex += 1
    if (this.historyIndex >= history.length) {
      const draft = this.historyDraft
      this.resetHistoryCursor()
      this.showHistory(draft)
      this.historyText = null
      return true
    }
    this.showHistory(history[this.historyIndex] ?? "")
    return true
  }

  private showHistory(text: string): void {
    this.historyText = text
    this.deps.composer.replaceText(text)
  }

  /** An edit since the last recall ends the navigation. */
  private syncHistoryCursor(): void {
    if (this.historyIndex !== null && this.deps.composer.getText() !== this.historyText) this.resetHistoryCursor()
  }

  private resetHistoryCursor(): void {
    this.historyIndex = null
    this.historyDraft = ""
    this.historyText = null
  }

  // ---- other key targets ----

  quit(): void {
    this.deps.quit()
  }

  scrollTranscript(_direction: -1 | 1): void {
    // The app runtime replaces this with the scrollbox; without a UI there is nothing to scroll.
  }

  // ---- threads ----

  private async thread(args: string): Promise<void> {
    const state = this.state
    const sub = args.trim()
    if (state.server !== null && !state.server.capabilities.threads) {
      await this.sendMessage(sub === "" ? "/thread" : `/thread ${sub}`)
      return
    }
    if (sub === "" || sub.toLowerCase() === "list") {
      await this.loadThreads()
      this.openPanel("threads")
      return
    }
    if (sub.toLowerCase() === "new") return this.switchThread({ kind: "new" })
    if (sub.toLowerCase() === "resume") {
      this.notify("info", "/thread resume rejoins a socket thread; this gateway keeps the thread across reconnects")
      return
    }
    const id = await this.resolveThreadPrefix(sub)
    if (id !== null) await this.switchThread({ kind: "id", id })
  }

  private async loadThreads(): Promise<ThreadRow[] | null> {
    const transport = this.transport
    if (!hasThreadLister(transport)) {
      this.panelData.threads = null
      return null
    }
    try {
      this.panelData.threads = await transport.listThreads()
    } catch (error) {
      this.panelData.threads = null
      this.notify("warn", `listing threads failed: ${asError(error).message}`, "transport")
    }
    return this.panelData.threads
  }

  private async resolveThreadPrefix(prefix: string): Promise<ThreadId | null> {
    const full = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu
    if (full.test(prefix)) return prefix.toLowerCase()
    const threads = await this.loadThreads()
    if (threads === null) {
      this.notify("warn", "this gateway cannot list threads here; use the full thread id")
      return null
    }
    const matches = threads.filter((thread) => thread.id.toLowerCase().startsWith(prefix.toLowerCase()))
    if (matches.length === 1) return matches[0]?.id ?? null
    this.notify("warn", matches.length === 0 ? `no thread starts with ${prefix}` : `${matches.length} threads start with ${prefix}; type more of the id`)
    return null
  }

  private async switchThread(spec: ThreadSpec): Promise<void> {
    if (!this.connected()) return
    try {
      const id = await this.deps.supervisor.selectThread(spec)
      this.notify("info", `thread ${id.slice(0, 8)}`)
    } catch (error) {
      this.notify("warn", `switching threads failed: ${asError(error).message}`, "transport")
    }
  }

  private openPanel(kind: PanelKind): void {
    this.deps.store.dispatch({ type: "panel/open", kind, at: this.now() })
  }

  private async showDevices(): Promise<void> {
    const voice = this.deps.voice
    try {
      this.panelData.devices = voice === null ? null : await voice.listDevices()
    } catch {
      this.panelData.devices = null
    }
    this.openPanel("devices")
  }

  // ---- the command context ----

  /** The command context for one submitted text (forwards keep its draft and second-Enter state). */
  private commandsFor(draft: string, confirmed: boolean): CommandContext {
    return { ...this.commands, forward: (text) => this.sendChat(text, draft, confirmed) }
  }

  private commandContext(): CommandContext {
    return {
      tenant: this.deps.config.tenant,
      voice: this.deps.voice,
      notice: (level, text) => this.notify(level, text),
      openPanel: (kind) => this.openPanel(kind),
      quit: () => this.quit(),
      forward: (text) => this.sendChat(text, this.deps.composer.getText(), false),
      control: (kind: ControlCommandKind, text) => this.sendControl(kind, text),
      answerByCommand: (decision) => this.answerByCommand(decision),
      viewApproval: () => this.viewApproval(),
      attachFile: (path) => this.attachFile(path),
      screenshot: (full) => this.screenshot(full),
      imageCommand: (args) => this.imageCommand(args),
      copyReply: async (n) => {
        await this.deps.copy?.copyReply(n)
      },
      reconnect: () => {
        this.notify("info", "reconnecting")
        this.deps.supervisor.reconnectNow()
      },
      thread: (args) => this.thread(args),
      showDevices: () => this.showDevices(),
    }
  }
}
