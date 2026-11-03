/**
 * Controller-facing contracts (design 3.10.5). Declared by the scaffold task so the session controller, the
 * voice controller and the keymap compile before controller/composer-handle.ts exists (F6).
 */

/**
 * The composer textarea as controllers see it. Implemented by controller/composer-handle.ts over OpenTUI's
 * TextareaRenderable (ot081:renderables/EditBufferRenderable.d.ts); components never expose the renderable.
 */
export interface ComposerHandle {
  /** The whole text (TextareaRenderable.plainText). */
  getText(): string
  /**
   * Inserts at the cursor. With `separate: true` (dictation, 3.10.5) a space is inserted first when the
   * character before the cursor is not whitespace.
   */
  insertText(text: string, opts?: { separate?: boolean }): void
  /** Replaces the whole text as one undo step (EditBufferRenderable.replaceText). */
  replaceText(text: string): void
  /** replaceText(""): undoable, so Ctrl+_ restores a cleared draft. */
  clear(): void
  focus(): void
  blur(): void
  /** 0-based visual row of the cursor (visualCursor.visualRow); Up on row 0 recalls history. */
  visualRow(): number
  /** Visual (wrapped) line count (virtualLineCount); Down on the last row recalls history. */
  visualLines(): number
  /** Dispatches composer/metrics with the visual line count and the UTF-8 byte length. */
  reportMetrics(): void
}
