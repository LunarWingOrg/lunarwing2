/**
 * Keys (design 3.10.4): one global useKeyboard(handler, { release: true }) plus the global usePaste handler.
 * KEYMAP is data; the help panel and the footer are generated from it.
 *
 * Global handlers run before the focused renderable (ot081:chunk-bun-ved2tpxm.js:289-330), and every handled key
 * calls preventDefault(), so F9 and NUL (Ctrl+Space inside tmux) never reach the composer. Keys the handler does
 * not claim go to the focused textarea with the bindings of controller/composer-handle.ts (Enter submits through
 * the textarea's onSubmit).
 *
 * In credential mode (3.6.4) every key and paste is consumed and fed to the masked buffer; only Enter / Ctrl+S
 * (submit), Esc (arm, then cancel), Backspace, Ctrl+C, F1 and PageUp/PageDown keep a meaning.
 *
 * Legacy encodings: Alt+] arrives as ESC ], the start of an OSC string, which OpenTUI's input parser drops after its
 * 20 ms timeout (verified on 0.8.1), so "next approval" also has Alt+J. Only Alt+<letter or digit> combinations
 * are reported in the legacy encoding.
 */
import { decodePasteBytes, type KeyEvent, type PasteEvent } from "@opentui/core"
import type { Clock } from "@opentui/core"
import type { ComposerHandle } from "./types.ts"
import type { CredentialInput } from "./credential-input.ts"
import type { ApprovalDecision, Store } from "../state/model.ts"
import type { VoiceController } from "../voice/types.ts"

export type KeyAction =
  | "submit"
  | "newline"
  | "interrupt_send"
  | "ptt"
  | "help"
  | "escape"
  | "paste"
  | "remove_attachment"
  | "clear_attachments"
  | "approve"
  | "always"
  | "deny"
  | "view_approval"
  | "next_approval"
  | "voice_retry"
  | "history"
  | "undo"
  | "redo"
  | "scroll"
  | "cancel_or_quit"

export interface KeymapEntry {
  readonly keys: readonly string[]
  readonly action: KeyAction
  /** When the binding applies, in words (help panel). */
  readonly when: string
  /** What it does (help panel). */
  readonly help: string
}

/** The 3.10.4 table. "F9" and "Ctrl+Space" stand for the configured push-to-talk keys. */
export const KEYMAP: readonly KeymapEntry[] = Object.freeze([
  { keys: ["Enter"], action: "submit", when: "always", help: "send (queued while a turn runs); confirm a guarded send or a typed approval; send the credential" },
  { keys: ["Ctrl+J", "Alt+Enter", "Shift+Enter"], action: "newline", when: "composer", help: "new line in the message (Shift+Enter needs the kitty keyboard protocol)" },
  { keys: ["Ctrl+S", "Ctrl+Enter"], action: "interrupt_send", when: "always", help: "interrupt the running turn, then send; with an empty message interrupt only" },
  { keys: ["F9", "Ctrl+Space"], action: "ptt", when: "voice", help: "push-to-talk (tap to start and stop, or hold)" },
  { keys: ["F1"], action: "help", when: "always", help: "this help" },
  { keys: ["Esc"], action: "escape", when: "always", help: "close the panel; cancel a confirmation; cancel dictation; dismiss the newest notice" },
  { keys: ["Ctrl+V"], action: "paste", when: "composer", help: "paste from the clipboard (image, copied files or text)" },
  { keys: ["Ctrl+R"], action: "remove_attachment", when: "images attached", help: "remove the last attachment" },
  { keys: ["Ctrl+X"], action: "clear_attachments", when: "images attached", help: "remove every attachment" },
  { keys: ["Alt+Y"], action: "approve", when: "approval pending", help: "approve the focused tool call" },
  { keys: ["Alt+A"], action: "always", when: "approval pending", help: "approve and always allow this tool (otherwise Alt+A is line start)" },
  { keys: ["Alt+N"], action: "deny", when: "approval pending", help: "deny the focused tool call" },
  { keys: ["Alt+V"], action: "view_approval", when: "approval pending", help: "view the full approval text" },
  { keys: ["Alt+]", "Alt+J"], action: "next_approval", when: "several approvals", help: "focus the next pending approval" },
  { keys: ["Alt+R"], action: "voice_retry", when: "voice clip kept", help: "retry the kept voice clip" },
  { keys: ["Up", "Down"], action: "history", when: "first / last line", help: "previous / next sent message or cleared draft" },
  { keys: ["Ctrl+_"], action: "undo", when: "composer", help: "undo (what Ctrl+- sends, also inside tmux)" },
  { keys: ["Ctrl+."], action: "redo", when: "composer", help: "redo" },
  { keys: ["PageUp", "PageDown"], action: "scroll", when: "always", help: "scroll the panel or the transcript; the end follows new text again" },
  { keys: ["Ctrl+C"], action: "cancel_or_quit", when: "always", help: "cancel dictation; else clear the message (Ctrl+_ restores it); else press twice to quit" },
])

// ---------------------------------------------------------------------------------------------------------
// Key specs ("f9", "ctrl+space") and labels
// ---------------------------------------------------------------------------------------------------------

export interface KeySpec {
  name: string
  ctrl: boolean
  meta: boolean
  shift: boolean
  super: boolean
  hyper: boolean
}

/** "ctrl+space" -> { name: "space", ctrl: true, ... }; "alt" and "meta" are the same modifier in OpenTUI. */
export function parseKeySpec(spec: string): KeySpec {
  const parts = spec.toLowerCase().split("+").filter((part) => part.length > 0)
  const name = parts.pop() ?? ""
  const mods = new Set(parts)
  return {
    name,
    ctrl: mods.has("ctrl"),
    meta: mods.has("alt") || mods.has("meta"),
    shift: mods.has("shift"),
    super: mods.has("super"),
    hyper: mods.has("hyper"),
  }
}

export function keyMatchesSpec(key: Pick<KeyEvent, "name" | "ctrl" | "meta" | "shift"> & { super?: boolean; hyper?: boolean }, spec: KeySpec): boolean {
  return (
    key.name === spec.name &&
    key.ctrl === spec.ctrl &&
    key.meta === spec.meta &&
    key.shift === spec.shift &&
    (key.super ?? false) === spec.super &&
    (key.hyper ?? false) === spec.hyper
  )
}

/** "ctrl+space" -> "Ctrl+Space", "f9" -> "F9". */
export function keyLabel(spec: string): string {
  return spec
    .split("+")
    .map((part) => {
      if (part === "alt" || part === "meta") return "Alt"
      if (/^f\d{1,2}$/.test(part)) return part.toUpperCase()
      return part.length === 0 ? part : part[0]?.toUpperCase() + part.slice(1)
    })
    .join("+")
}

/** The label of the first push-to-talk key ("F9"), for the footer and the status line. */
export function pttLabel(pttKeys: readonly string[]): string {
  const first = pttKeys[0]
  return first === undefined ? "F9" : keyLabel(first)
}

// ---------------------------------------------------------------------------------------------------------
// The global handlers
// ---------------------------------------------------------------------------------------------------------

/** What the key handler drives; implemented by controller/session.ts and the app runtime. */
export interface KeyTargets {
  submitCredential(): void
  interruptAndSend(): void
  answerFocused(decision: ApprovalDecision): void
  viewApproval(): void
  nextApproval(): void
  pasteClipboard(): void
  removeLastAttachment(): void
  clearAttachments(): void
  historyPrev(): boolean
  historyNext(): boolean
  /** Esc in credential mode: arm, then cancel within 2 s. */
  credentialEscape(): void
  /** Drag-and-drop: the paste was a list of image paths (images/paste-paths.ts); it attaches asynchronously. */
  dropPaste(text: string): boolean
  /** Ctrl+C on an empty composer twice within 2 s. */
  quit(): void
  /** PageUp (-1) / PageDown (+1) of the transcript. */
  scrollTranscript(direction: -1 | 1): void
  notify(level: "info" | "warn", text: string): void
}

export interface KeyControllerDeps {
  store: Store
  clock: Pick<Clock, "now">
  composer: ComposerHandle
  credential: CredentialInput
  voice: VoiceController | null
  targets: KeyTargets
  /** Normalized config voice.pttKeys ("f9", "ctrl+space"). */
  pttKeys: readonly string[]
  /** Info panel page size (rows). */
  panelPage?: () => number
}

export interface KeyController {
  handleKey(key: KeyEvent): void
  handlePaste(event: PasteEvent): void
}

/** The textarea's own rule (ot081:index.bun.js:8530): a sequence with a control character is not text. */
function isPrintableSequence(sequence: string): boolean {
  return sequence.length > 0 && !/[\u0000-\u001f\u007f-\u009f]/u.test(sequence)
}

/** Ctrl+C on an empty composer must be pressed again within this window to quit. */
export const QUIT_WINDOW_MS = 2_000

export function createKeyController(deps: KeyControllerDeps): KeyController {
  return new GlobalKeys(deps)
}

class GlobalKeys implements KeyController {
  private readonly ptt: readonly KeySpec[]
  /** The name of the last PTT key pressed ("space", "f9"): only its release or repeat may lose a modifier. */
  private pttPressed: string | null = null
  private quitArmedAt: number | null = null

  constructor(private readonly deps: KeyControllerDeps) {
    this.ptt = deps.pttKeys.map(parseKeySpec)
  }

  handleKey(key: KeyEvent): void {
    if (key.eventType === "release") {
      this.handleRelease(key)
      return
    }
    const state = this.deps.store.getState()
    if (state.composer.credential !== null) {
      key.preventDefault()
      this.credentialKey(key)
      return
    }
    if (this.isPtt(key)) {
      key.preventDefault()
      this.pttPressed = key.name
      this.deps.voice?.pttKey({ eventType: "press", repeated: key.repeated === true || key.eventType === "repeat" })
      return
    }
    if (this.isHeldPttKey(key, "hold-key")) {
      // kitty sends the held key's repeats as plain text once its modifier is lifted (Ctrl+Space -> " "): they
      // are repeats of the hold, never text for the composer.
      key.preventDefault()
      this.deps.voice?.pttKey({ eventType: "press", repeated: true })
      return
    }
    if (this.claim(key)) key.preventDefault()
  }

  handlePaste(event: PasteEvent): void {
    const state = this.deps.store.getState()
    if (state.composer.credential !== null) {
      event.preventDefault()
      const pasted = decodePasteBytes(event.bytes).replace(/[\r\n]+$/u, "")
      if (!this.deps.credential.push(pasted)) this.deps.targets.notify("warn", "the credential is too long")
      this.reportCredentialChars()
      return
    }
    const text = decodePasteBytes(event.bytes)
    if (this.deps.targets.dropPaste(text)) event.preventDefault()
  }

  private handleRelease(key: KeyEvent): void {
    // With KKP a hold of Ctrl+Space ends with a release that has no ctrl when Ctrl is lifted first
    // (ESC[32;1:3u): while a key-triggered capture runs, the PTT key's name alone identifies its release.
    if (!this.isPtt(key) && !this.isHeldPttKey(key, "any")) return
    key.preventDefault()
    const state = this.deps.store.getState()
    if (!state.ui.kkpRelease) this.deps.store.dispatch({ type: "ui/patch", patch: { kkpRelease: true }, at: this.deps.clock.now() })
    if (state.composer.credential !== null) return
    this.deps.voice?.pttKey({ eventType: "release", repeated: false })
  }

  private isPtt(key: KeyEvent): boolean {
    return this.ptt.some((spec) => keyMatchesSpec(key, spec))
  }

  /**
   * The PTT key that started the running capture, by name with its modifiers ignored: "hold-key" only during a
   * KKP hold (repeats), "any" for releases. Another key's release (F9 started it, a space was typed) never counts.
   */
  private isHeldPttKey(key: KeyEvent, mode: "hold-key" | "any"): boolean {
    const voice = this.deps.store.getState().voice
    if (voice.trigger !== "key" || voice.phase === "idle" || key.name !== this.pttPressed) return false
    return mode === "any" || voice.mode === "hold-key"
  }

  /** Returns true when the key was handled here. */
  private claim(key: KeyEvent): boolean {
    const plain = !key.ctrl && !key.meta && !key.shift
    if (key.name === "f1" && plain) return this.openHelp()
    if (key.name === "escape" && !key.ctrl) return this.escape()
    if (key.ctrl && !key.meta) return this.ctrlKey(key)
    if (key.meta && !key.ctrl && !key.shift) return this.altKey(key)
    if (key.name === "pageup" || key.name === "pagedown") return this.page(key.name === "pageup" ? -1 : 1)
    if (key.name === "up" && plain) return this.deps.composer.visualRow() === 0 && this.deps.targets.historyPrev()
    if (key.name === "down" && plain) {
      const last = this.deps.composer.visualRow() >= this.deps.composer.visualLines() - 1
      return last && this.deps.targets.historyNext()
    }
    return false
  }

  private ctrlKey(key: KeyEvent): boolean {
    const { targets } = this.deps
    switch (key.name) {
      case "c":
        this.cancelOrQuit()
        return true
      case "s":
      case "return":
        targets.interruptAndSend()
        return true
      case "v":
        targets.pasteClipboard()
        return true
      case "r":
        targets.removeLastAttachment()
        return true
      case "x":
        targets.clearAttachments()
        return true
      default:
        return false
    }
  }

  /** Alt+Y/A/N/V/]/J only while a prompt is pending (otherwise Alt+A stays the textarea's line start). */
  private altKey(key: KeyEvent): boolean {
    const { targets, store } = this.deps
    if (key.name === "r") {
      this.deps.voice?.retry()
      return true
    }
    if (store.getState().approvalQueue.length === 0) return false
    switch (key.name) {
      case "y":
        targets.answerFocused("approve")
        return true
      case "a":
        targets.answerFocused("always")
        return true
      case "n":
        targets.answerFocused("deny")
        return true
      case "v":
        targets.viewApproval()
        return true
      case "]":
      case "j":
        targets.nextApproval()
        return true
      default:
        return false
    }
  }

  private openHelp(): boolean {
    this.deps.store.dispatch({ type: "panel/open", kind: "help", at: this.deps.clock.now() })
    return true
  }

  private page(direction: -1 | 1): boolean {
    const { store, clock } = this.deps
    if (store.getState().panel !== null) {
      const rows = Math.max(1, (this.deps.panelPage?.() ?? 10) - 2)
      store.dispatch({ type: "panel/scroll", delta: direction * rows, at: clock.now() })
    } else {
      this.deps.targets.scrollTranscript(direction)
    }
    return true
  }

  /** The first that applies: panel, confirmation or guard, dictation, newest notice (3.10.4). */
  private escape(): boolean {
    const { store, clock, voice } = this.deps
    const state = store.getState()
    const at = clock.now()
    if (state.panel !== null) {
      store.dispatch({ type: "panel/close", at })
      return true
    }
    if (state.composer.confirm !== null || state.composer.guard !== null) {
      store.dispatchMany([
        { type: "approval/confirm_cleared", at },
        { type: "composer/guard_clear", at },
      ])
      return true
    }
    if (voice?.escape() === true) return true
    if (state.notices.length > 0) {
      store.dispatch({ type: "notice/dismiss", at })
      return true
    }
    return false
  }

  /** Cancel dictation; else clear a non-empty composer (undoable, kept in history); else press twice to quit. */
  private cancelOrQuit(): void {
    const { store, clock, voice, composer, targets } = this.deps
    const state = store.getState()
    const at = clock.now()
    if (voice !== null && (state.voice.phase !== "idle" || state.voice.queue.length > 0)) {
      voice.escape()
      return
    }
    const text = composer.getText()
    if (text.length > 0) {
      store.dispatch({ type: "composer/history_push", text, at })
      composer.clear()
      this.quitArmedAt = null
      return
    }
    if (this.quitArmedAt !== null && at - this.quitArmedAt < QUIT_WINDOW_MS) {
      targets.quit()
      return
    }
    this.quitArmedAt = at
    targets.notify("info", "press Ctrl+C again to quit")
  }

  private credentialKey(key: KeyEvent): void {
    const { targets, credential } = this.deps
    if (key.name === "return" || key.name === "kpenter" || (key.ctrl && key.name === "s")) {
      targets.submitCredential()
      return
    }
    if (key.name === "escape") {
      if (this.deps.store.getState().panel !== null) this.escape()
      else targets.credentialEscape()
      return
    }
    if (key.name === "f1") {
      this.openHelp()
      return
    }
    if (key.name === "pageup" || key.name === "pagedown") {
      this.page(key.name === "pageup" ? -1 : 1)
      return
    }
    if (key.name === "backspace") credential.backspace()
    else if (key.ctrl && (key.name === "c" || key.name === "u")) credential.clear()
    else if (!key.ctrl && !key.meta && key.name === "space") credential.push(" ")
    else if (!key.ctrl && !key.meta && isPrintableSequence(key.sequence)) credential.push(key.sequence)
    this.disarmCredentialEsc()
    this.reportCredentialChars()
  }

  private disarmCredentialEsc(): void {
    const credential = this.deps.store.getState().composer.credential
    if (credential?.escArmedAt !== null && credential !== null) {
      this.deps.store.dispatch({ type: "composer/credential_esc_armed", armed: false, at: this.deps.clock.now() })
    }
  }

  private reportCredentialChars(): void {
    const { store, clock, credential } = this.deps
    const current = store.getState().composer.credential
    if (current !== null && current.chars !== credential.length) {
      store.dispatch({ type: "composer/credential_chars", chars: credential.length, at: clock.now() })
    }
  }
}
