/**
 * The composer (design 3.10.5, 3.12.1, 7.4, UX-01, UX-03): a bordered row of
 * [mic widget, 10 columns][VU meter, 12 columns, only while recording][textarea, 1-6 rows].
 *
 * The textarea is OpenTUI's <textarea> (word wrap; Enter submits, Ctrl+J / Alt+Enter / Shift+Enter add a line,
 * Ctrl+_ undoes: controller/composer-handle.ts COMPOSER_KEY_BINDINGS), uncontrolled and driven through the
 * ComposerHandle. Its height is clamp(lines, 1, 6) text rows from the layout; longer text scrolls inside it.
 *
 * The border title carries exactly one voice status line while voice is active ("opening mic...", "REC 0:07 -
 * F9 stop - Esc cancel", "REC 0:07 - release to stop - Esc cancel", "transcribing 0:03 - lunarvoice - Esc cancel",
 * "lunarvoice busy - retry 2/3 in 2 s", "2 clips waiting", or the no-signal hint), and the byte counter
 * "N KiB / 98 KiB" above 80 % of the content limit.
 *
 * Credential mode (3.6.4): the textarea is hidden and blurred (its draft stays) and the box shows only
 * `credential for <extension>: **** (N chars)`; the secret is never rendered.
 */
import type { TextareaRenderable } from "@opentui/core"
import type { ReactNode } from "react"
import { COMPOSER_KEY_BINDINGS, type ComposerController } from "../controller/composer-handle.ts"
import { maskedCredentialLine } from "../controller/credential-input.ts"
import type { AppState } from "../state/model.ts"
import { formatBytes } from "../images/limits.ts"
import { cellWidth, formatClock, truncate } from "../util.ts"
import { MIC_WIDGET_WIDTH, MicWidget } from "./mic-widget.tsx"
import { borderOf, styleOf, textareaColors, type Theme } from "./theme.ts"
import { VuMeter } from "./vu-meter.tsx"

/** Content size above which the byte counter shows (3.10.5). */
export const BYTE_COUNTER_FRACTION = 0.8
const DEFAULT_MAX_CONTENT_BYTES = 100_000
const BUSY_RETRIES = 3

/** The one voice status line (7.4), or null while voice is idle. */
export function voiceStatusLine(state: AppState, now: number, ptt: string): string | null {
  const voice = state.voice
  if (voice.phase === "arming") return "opening mic..."
  if (voice.phase === "recording") {
    if (voice.level?.noSignal === true) return "no signal from the microphone - /voice device"
    const rec = `REC ${formatClock(now - (voice.startedAt ?? now))}`
    if (voice.mode === "toggle") return `${rec} - ${ptt} stop - Esc cancel`
    if (voice.mode === "provisional") return `${rec} - Esc cancel`
    return `${rec} - release to stop - Esc cancel`
  }
  const head = voice.queue[0]
  if (head === undefined) return voice.phase === "finishing" ? "transcribing - Esc cancel" : null
  if (head.state === "transcribing") {
    const since = head.startedAt ?? now
    return `transcribing ${formatClock(now - since)} - ${head.source ?? "voice"} - Esc cancel`
  }
  if (head.source !== null && head.attempt > 0) {
    const wait = Math.max(0, Math.ceil(((head.startedAt ?? now) - now) / 1000))
    return `${head.source} busy - retry ${head.attempt}/${BUSY_RETRIES} in ${wait} s`
  }
  return voice.queue.length === 1 ? "1 clip waiting" : `${voice.queue.length} clips waiting`
}

/** "88 KiB / 98 KiB" above 80 % of the content limit, else null (the units of the transport panel). */
export function byteCounter(state: AppState): string | null {
  const max = state.server?.limits.maxContentBytes ?? DEFAULT_MAX_CONTENT_BYTES
  const bytes = state.composer.bytes
  if (bytes <= max * BYTE_COUNTER_FRACTION) return null
  return `${formatBytes(bytes)} / ${formatBytes(max)}`
}

export function composerTitle(state: AppState, now: number, ptt: string): string {
  const credential = state.composer.credential
  if (credential !== null) return `credential for ${credential.extensionName} - Enter sends it, Esc Esc cancels`
  return [voiceStatusLine(state, now, ptt), byteCounter(state)].filter((part): part is string => part !== null).join(" - ")
}

/** The masked credential line within `room` cells: the character count stays visible, the name is cut first. */
export function credentialLine(extensionName: string, chars: number, room: number): string {
  const full = maskedCredentialLine(extensionName, chars)
  if (cellWidth(full) <= room) return full
  const tail = chars === 0 ? "(0 chars)" : `**** (${chars} chars)`
  const nameRoom = room - cellWidth(tail) - 2
  return nameRoom >= 4 ? `${truncate(extensionName, nameRoom)}: ${tail}` : truncate(tail, Math.max(1, room))
}

export interface ComposerProps {
  state: AppState
  /** Layout rows: text rows plus the 2-row border. */
  rows: number
  width: number
  theme: Theme
  now: number
  /** Label of the first push-to-talk key. */
  ptt: string
  composer: ComposerController
  onSubmit: () => void
  micLabel: string
  onMicDown: () => void
  onMicUp: () => void
  cursorBlink: boolean
  /** False while an overlay (tenant picker) owns the keyboard. */
  focused: boolean
}

export function Composer(props: ComposerProps): ReactNode {
  const { state, theme } = props
  const credential = state.composer.credential
  const textRows = Math.max(1, props.rows - 2)
  const recording = state.voice.phase === "recording"
  const title = truncate(composerTitle(state, props.now, props.ptt), Math.max(1, props.width - 4))
  const attach = (renderable: TextareaRenderable | null): void => props.composer.attach(renderable)
  return (
    <box
      border
      {...borderOf(theme, credential === null ? "border" : "warn")}
      {...(title.length > 0 ? { title } : {})}
      style={{ height: props.rows, flexShrink: 0, flexDirection: "row" }}
    >
      <MicWidget label={props.micLabel} theme={theme} recording={recording} onDown={props.onMicDown} onUp={props.onMicUp} />
      {recording && <VuMeter level={state.voice.level} theme={theme} />}
      {credential !== null && (
        <text selectable={false} wrapMode="none" {...styleOf(theme, "warn")} style={{ height: 1, flexGrow: 1, flexShrink: 1, flexBasis: 0, minWidth: 0 }}>
          {credentialLine(credential.extensionName, credential.chars, props.width - 2 - MIC_WIDGET_WIDTH)}
        </text>
      )}
      <textarea
        ref={attach}
        // `visible` before `focused`: OpenTUI's visible setter blurs a focused renderable.
        visible={credential === null}
        focused={props.focused && credential === null}
        wrapMode="word"
        keyBindings={COMPOSER_KEY_BINDINGS}
        onSubmit={props.onSubmit}
        onContentChange={() => props.composer.reportMetrics()}
        cursorStyle={{ style: "block", blinking: props.cursorBlink }}
        placeholder="message - Enter send, Ctrl+J new line, F1 help"
        placeholderColor={theme.placeholder}
        {...textareaColors(theme)}
        // Width comes from the row, never from the text: the textarea measures its text wrapped at the whole row,
        // and OpenTUI turns flexShrink off whenever a numeric height is set (every time the composer grows), so a
        // content-sized basis overflowed the slot after the mic (and the VU meter) and the end of every line was
        // drawn off-screen (UX-01). Basis 0 plus flexGrow fills exactly the room that is left.
        style={{ height: textRows, flexGrow: 1, flexShrink: 1, flexBasis: 0, minWidth: 0 }}
      />
    </box>
  )
}
