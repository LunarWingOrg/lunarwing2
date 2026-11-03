/**
 * One transcript record (design 3.3, 3.5, 3.12.1, 6.4; the ImageBlock / ItemBlock ideas of
 * v4:tui/src/components/transcript.tsx). The live turn renders here too, inside the scrollbox (fixes C5).
 *
 * A user turn shows its input (sent images below it), the reply text (the canonical final, else the streamed
 * deltas: state/turn-text.ts visibleText), the last three activity lines, generated images, then one status word
 * line ("queued #2", "waiting for reply, 12 s", "interrupted", "failed: ..."; background_running reads "still
 * running; the result will arrive here"), notes and steer notes. Control records are one dim line. Every state is
 * a word, never colour alone.
 *
 * Images: the source handed to <image> is always validated bytes (images/protocol.ts imageRenderSource); a server
 * path is caption text only. Generated images are at most 56 x 12 cells with fit="fit"; evicted ones (only the
 * newest 20 keep bytes) become a placeholder line.
 */
import { memo, type ReactNode } from "react"
import type { ImageProtocol } from "../images/protocol.ts"
import { imageRenderSource } from "../images/protocol.ts"
import { formatBytes } from "../images/limits.ts"
import type { TranscriptImage, TurnRecord } from "../state/model.ts"
import { NOTES } from "../state/correlate.ts"
import { isSealedPhase, visibleText } from "../state/turn-text.ts"
import type { Tri } from "../transport/types.ts"
import { cellWidth, formatSeconds, truncate } from "../util.ts"
import { fgOf, styleOf, type Theme } from "./theme.ts"

export const GENERATED_IMAGE_ROWS = 12
export const INPUT_IMAGE_ROWS = 6
export const INPUT_IMAGE_COLUMNS = 20
const ACTIVITY_SHOWN = 3
const PREFIX_USER = "you"
const PREFIX_ASSISTANT = "lunarwing"

export interface TurnStatusInput {
  now: number
  queuePosition: number | null
  streaming: Tri
}

/** The status word line of a user turn, or null when there is nothing to say (done). */
export function turnStatusLabel(record: TurnRecord, input: TurnStatusInput): string | null {
  const elapsed = formatSeconds(input.now - (record.sentAt ?? record.createdAt))
  switch (record.phase) {
    case "pending":
      return "sending..."
    case "sent":
      return `waiting for reply, ${elapsed}`
    case "queued":
      return input.queuePosition === null ? "queued" : `queued #${input.queuePosition}`
    case "running":
      return visibleText(record).length > 0 && input.streaming === true ? `replying, ${elapsed}` : `working, ${elapsed}`
    case "awaiting_approval":
      return "APPROVAL pending - answer in the panel"
    case "awaiting_input":
      return "waiting for input"
    case "background_running":
      return NOTES.detached
    case "done":
      return null
    case "interrupted":
      return "interrupted"
    case "failed":
      return `failed: ${firstLine(record.error?.message ?? "error")}`
    case "lost":
      return "lost"
    case "unanswered":
      return "unanswered"
  }
}

function firstLine(text: string): string {
  const newline = text.indexOf("\n")
  return newline < 0 ? text : `${text.slice(0, newline)} ...`
}

/** The caption under an image: the server's path or file name as text, else type and size. */
export function imageCaption(image: TranscriptImage): string {
  const size = `${image.mime.replace("image/", "")} ${formatBytes(image.byteLength)}`
  const dims = image.width !== undefined && image.height !== undefined ? ` ${image.width}x${image.height}` : ""
  return image.caption === undefined ? `${size}${dims}` : `${image.caption} (${size}${dims})`
}

export function ImageBlock(props: { image: TranscriptImage; columns: number; rows: number; protocol: ImageProtocol; theme: Theme }): ReactNode {
  const { image, theme } = props
  const source = imageRenderSource(image.bytes)
  const caption = truncate(imageCaption(image), Math.max(8, props.columns))
  if (source === undefined) {
    return (
      <text selectable={false} wrapMode="none" {...styleOf(theme, "dim")} style={{ height: 1, flexShrink: 0 }}>
        {`  [image no longer in memory: ${caption}]`}
      </text>
    )
  }
  return (
    <box style={{ flexDirection: "column", height: props.rows + 1, flexShrink: 0 }}>
      <image source={source} fit="fit" protocol={props.protocol} style={{ width: props.columns, height: props.rows, flexShrink: 0 }} />
      <text selectable={false} wrapMode="none" {...styleOf(theme, "dim")} style={{ height: 1, flexShrink: 0 }}>
        {caption}
      </text>
    </box>
  )
}

/**
 * A speaker line: the gutter ("you ▏ ", "lunarwing (from history) ▏ ") is a fixed column and the text wraps in
 * the column right of it, so embedded newlines and soft wraps continue under the text, never at column 0. The
 * gutter is chrome (not selectable), so a drag copies the text only.
 */
function Line(props: { prefix: string; text: string; role: "user" | "assistant"; theme: Theme; tag?: string }): ReactNode {
  const rest = `${props.tag === undefined ? "" : ` (${props.tag})`} ▏ `
  return (
    <box style={{ flexDirection: "row", flexShrink: 0 }}>
      <text selectable={false} wrapMode="none" {...fgOf(props.theme, "text")} style={{ width: cellWidth(`${props.prefix}${rest}`), flexShrink: 0 }}>
        <span {...styleOf(props.theme, props.role)}>{props.prefix}</span>
        {rest}
      </text>
      <text wrapMode="word" {...fgOf(props.theme, "text")} style={{ flexGrow: 1, flexShrink: 1, flexBasis: 0, minWidth: 0 }}>
        {props.text}
      </text>
    </box>
  )
}

/** A dim line; its leading spaces become a left padding, so wrapped lines keep the indentation. */
function Dim(props: { text: string; theme: Theme; role?: "dim" | "warn" | "error" | "approval" }): ReactNode {
  const indent = props.text.length - props.text.replace(/^ +/u, "").length
  const text = (
    <text wrapMode="word" {...styleOf(props.theme, props.role ?? "dim")}>
      {indent === 0 ? props.text : props.text.slice(indent)}
    </text>
  )
  return indent === 0 ? text : <box style={{ paddingLeft: indent, flexShrink: 0 }}>{text}</box>
}

export interface TurnViewProps {
  record: TurnRecord
  theme: Theme
  /** Only open turns use it (elapsed timers). */
  now: number
  queuePosition: number | null
  streaming: Tri
  protocol: ImageProtocol
  imageColumns: number
  /** Instructions and URLs of a credential request this turn owns (shown as text, never opened). */
  credentialHint: string | null
}

function ControlView(props: { record: TurnRecord; theme: Theme }): ReactNode {
  const { record } = props
  const control = record.control
  if (control === undefined) return null
  const answer =
    control.ack ?? (record.phase === "failed" ? `not sent: ${record.error?.message ?? "error"}` : record.phase === "done" ? "done" : "...")
  return <Dim theme={props.theme} text={`  ${control.text} -> ${answer}`} />
}

function inputText(record: TurnRecord): string {
  if (record.credential !== undefined) return `[credential sent to ${record.credential.extensionName}]`
  const text = record.input?.text ?? ""
  const images = record.input?.images.length ?? 0
  if (text.trim().length > 0) return text
  return images === 1 ? "[1 image]" : `[${images} images]`
}

/** Progress kinds that only matter while a turn runs; a sealed turn keeps its tool, skill and job lines. */
const TRANSIENT_ACTIVITY: ReadonlySet<string> = new Set(["thinking", "status", "reasoning"])

function shownActivity(record: TurnRecord): TurnRecord["activity"] {
  const entries = isSealedPhase(record.phase) ? record.activity.filter((entry) => !TRANSIENT_ACTIVITY.has(entry.kind)) : record.activity
  return entries.slice(-ACTIVITY_SHOWN)
}

function ReplyParts(props: TurnViewProps): ReactNode {
  const { record, theme } = props
  const text = visibleText(record)
  const activity = shownActivity(record)
  const tag = record.origin === "background" ? "background" : record.recovered === true ? "from history" : undefined
  return (
    <>
      {text.length > 0 && <Line prefix={PREFIX_ASSISTANT} text={text} role="assistant" theme={theme} {...(tag === undefined ? {} : { tag })} />}
      {activity.map((entry, index) => (
        <Dim key={`a${index}`} theme={theme} text={`  : ${entry.text}${entry.ok === false ? " (failed)" : ""}`} />
      ))}
      {record.images.map((image) => (
        <ImageBlock key={image.id} image={image} columns={props.imageColumns} rows={GENERATED_IMAGE_ROWS} protocol={props.protocol} theme={theme} />
      ))}
    </>
  )
}

function StatusParts(props: TurnViewProps): ReactNode {
  const { record, theme } = props
  const status = record.origin === "user" ? turnStatusLabel(record, props) : null
  const statusRole = record.phase === "failed" ? "error" : record.phase === "awaiting_approval" ? "approval" : "dim"
  return (
    <>
      {status !== null && <Dim theme={theme} role={statusRole} text={`  ${status}`} />}
      {props.credentialHint !== null && <Dim theme={theme} role="warn" text={`  ${props.credentialHint}`} />}
      {record.steerNotes.map((note, index) => (
        <Dim key={`s${index}`} theme={theme} text={`  steer: ${note.text}${note.ack === undefined ? " (sending)" : ` - ${note.ack}`}`} />
      ))}
      {record.notes.map((note, index) => (
        <Dim key={`n${index}`} theme={theme} role={note === NOTES.imageNotDelivered ? "warn" : "dim"} text={`  - ${note}`} />
      ))}
      {record.phase === "failed" && record.error !== undefined && record.error.message.includes("\n") && (
        <Dim theme={theme} role="error" text={record.error.message} />
      )}
    </>
  )
}

function TurnViewImpl(props: TurnViewProps): ReactNode {
  const { record, theme } = props
  if (record.origin === "control") return <ControlView record={record} theme={theme} />
  const hasInput = record.input !== null && (record.origin === "user" || record.origin === "history")
  return (
    <box style={{ flexDirection: "column", flexShrink: 0, marginBottom: 1 }}>
      {hasInput && <Line prefix={PREFIX_USER} text={inputText(record)} role="user" theme={theme} />}
      {hasInput &&
        (record.input?.images ?? []).map((image) => (
          <ImageBlock key={image.id} image={image} columns={INPUT_IMAGE_COLUMNS} rows={INPUT_IMAGE_ROWS} protocol={props.protocol} theme={theme} />
        ))}
      <ReplyParts {...props} />
      <StatusParts {...props} />
    </box>
  )
}

/** Records are immutable, so a record that did not change re-renders only when its timer or position changes. */
export const TurnView = memo(TurnViewImpl, (before, after) => {
  if (before.record !== after.record || before.theme !== after.theme) return false
  if (before.queuePosition !== after.queuePosition || before.streaming !== after.streaming) return false
  if (before.protocol !== after.protocol || before.imageColumns !== after.imageColumns) return false
  if (before.credentialHint !== after.credentialHint) return false
  const timed = after.record.phase === "sent" || after.record.phase === "running"
  return !timed || Math.floor(before.now / 1000) === Math.floor(after.now / 1000)
})
