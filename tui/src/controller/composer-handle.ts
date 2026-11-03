/**
 * The composer textarea as controllers see it (design 3.10.5, UX-01): the ComposerHandle of controller/types.ts
 * over OpenTUI's TextareaRenderable (ot081:renderables/Textarea.d.ts, EditBufferRenderable.d.ts).
 *
 * OpenTUI merges COMPOSER_KEY_BINDINGS over defaultTextareaKeyBindings keyed by name + modifiers, so they replace
 * the defaults return -> newline and kpenter -> newline (ot081:index.bun.js:8540-8605; mergeKeyBindings). Enter
 * submits; Ctrl+J (LF), Alt+Enter (ESC CR, also inside tmux) and Shift+Enter (KKP only) insert a newline.
 *
 * Undo and redo work in both key encodings. In the legacy encoding (tmux, terminals without the kitty keyboard
 * protocol) Ctrl+_, Ctrl+/ and Ctrl+Shift+- send 0x1F, parsed as ctrl+"_", and Ctrl+Y sends 0x19; kitty sends
 * Ctrl+- as "-" and Ctrl+. as "." there, so the OpenTUI defaults ctrl+"-" (undo) and ctrl+"." (redo) are KKP only.
 * Under KKP the same physical keys arrive by name: ctrl+"/", ctrl+shift+"-" and ctrl+"y" are bound for them too.
 * Ctrl+Y is free: OpenTUI binds nothing to it and the composer has no kill ring to yank from.
 *
 * reportMetrics() (from onContentChange) dispatches the visual line count and UTF-8 size, marks the start and end
 * of a draft (composer/started, composer/cleared: used by the approval freshness rules, 3.6.2) and cancels a
 * pending approval confirmation or send guard when the text changed (3.10.3).
 */
import type { KeyBinding, TextareaRenderable } from "@opentui/core"
import type { ComposerHandle } from "./types.ts"
import type { Store } from "../state/model.ts"
import { utf8Bytes } from "../util.ts"

export const COMPOSER_KEY_BINDINGS: KeyBinding[] = [
  { name: "return", action: "submit" },
  { name: "kpenter", action: "submit" },
  { name: "return", shift: true, action: "newline" },
  { name: "return", meta: true, action: "newline" },
  { name: "linefeed", action: "newline" },
  // 0x1F: Ctrl+_, Ctrl+/ and Ctrl+Shift+- in the legacy encoding (also inside tmux).
  { name: "_", ctrl: true, action: "undo" },
  // The same keys under KKP.
  { name: "/", ctrl: true, action: "undo" },
  { name: "-", ctrl: true, shift: true, action: "undo" },
  // 0x19 in the legacy encoding, ctrl+"y" under KKP: the redo key that tmux delivers (Ctrl+. is KKP only).
  { name: "y", ctrl: true, action: "redo" },
]

/** Text rows of the composer before it scrolls inside the textarea. */
export const COMPOSER_MAX_ROWS = 6

export interface ComposerController extends ComposerHandle {
  /** The mounted textarea (ref callback); null on unmount. Text inserted while detached is applied on attach. */
  attach(textarea: TextareaRenderable | null): void
  readonly attached: boolean
  /**
   * After each rendered frame: the wrapped line count depends on the textarea's laid-out width, which is known
   * only after layout (first frame, resize), so a changed count is reported then (cheap: no text copy).
   */
  syncLayout(): void
}

export interface ComposerHandleDeps {
  store: Pick<Store, "getState" | "dispatch">
  clock: { now(): number }
}

export function createComposerHandle(deps: ComposerHandleDeps): ComposerController {
  return new TextareaComposer(deps)
}

class TextareaComposer implements ComposerController {
  private textarea: TextareaRenderable | null = null
  /** Text while no textarea is mounted (tests without UI, or before the first frame). */
  private detachedText = ""

  constructor(private readonly deps: ComposerHandleDeps) {}

  get attached(): boolean {
    return this.textarea !== null && !this.textarea.isDestroyed
  }

  attach(textarea: TextareaRenderable | null): void {
    if (textarea === this.textarea) return
    this.textarea = textarea
    if (textarea !== null && this.detachedText.length > 0) {
      const pending = this.detachedText
      this.detachedText = ""
      textarea.insertText(pending)
      this.reportMetrics()
    }
  }

  getText(): string {
    return this.live()?.plainText ?? this.detachedText
  }

  insertText(text: string, opts: { separate?: boolean } = {}): void {
    if (text.length === 0) return
    const textarea = this.live()
    const before = textarea === null ? this.detachedText.slice(-1) : charBeforeCursor(textarea)
    const insert = opts.separate === true && before !== "" && !/\s/u.test(before) ? ` ${text}` : text
    if (textarea === null) this.detachedText += insert
    else textarea.insertText(insert)
    this.reportMetrics()
  }

  replaceText(text: string): void {
    const textarea = this.live()
    if (textarea === null) this.detachedText = text
    else {
      textarea.replaceText(text)
      textarea.gotoBufferEnd()
    }
    this.reportMetrics()
  }

  clear(): void {
    this.replaceText("")
  }

  focus(): void {
    this.live()?.focus()
  }

  blur(): void {
    this.live()?.blur()
  }

  visualRow(): number {
    const textarea = this.live()
    if (textarea === null) return 0
    // visualCursor.visualRow is viewport-relative (ot081:zig.d.ts:982); scrollY makes it document-relative.
    return Math.max(0, textarea.visualCursor.visualRow + Math.max(0, Math.floor(textarea.scrollY)))
  }

  visualLines(): number {
    const textarea = this.live()
    if (textarea === null) return Math.max(1, this.detachedText.split("\n").length)
    // virtualLineCount counts only the lines inside the viewport on 0.8.1; the composer needs the document total.
    return Math.max(1, textarea.editorView.getTotalVirtualLineCount())
  }

  reportMetrics(): void {
    const { store, clock } = this.deps
    const text = this.getText()
    const at = clock.now()
    const composer = store.getState().composer
    const lines = this.visualLines()
    const bytes = utf8Bytes(text)
    const edited = text !== this.lastText
    this.lastText = text
    if (lines !== composer.lines || bytes !== composer.bytes) store.dispatch({ type: "composer/metrics", lines, bytes, at })
    if (text.length > 0 && composer.startedAt === null) store.dispatch({ type: "composer/started", at })
    if (text.length === 0 && composer.startedAt !== null) store.dispatch({ type: "composer/cleared", at })
    if (composer.guard !== null && composer.guard.text !== text) store.dispatch({ type: "composer/guard_clear", at })
    // Any edit cancels a pending approval confirmation (3.10.3); a resize re-reports the same text.
    if (edited && composer.confirm !== null) store.dispatch({ type: "approval/confirm_cleared", at })
  }

  /** The text of the previous report, to tell edits from re-measurements. */
  private lastText = ""

  syncLayout(): void {
    if (this.live() === null) return
    const lines = this.visualLines()
    const composer = this.deps.store.getState().composer
    if (lines !== composer.lines) {
      this.deps.store.dispatch({ type: "composer/metrics", lines, bytes: composer.bytes, at: this.deps.clock.now() })
    }
  }

  private live(): TextareaRenderable | null {
    const textarea = this.textarea
    return textarea !== null && !textarea.isDestroyed ? textarea : null
  }
}

function charBeforeCursor(textarea: TextareaRenderable): string {
  const offset = textarea.cursorOffset
  if (offset <= 0) return ""
  return textarea.getTextRange(offset - 1, offset).slice(-1)
}
