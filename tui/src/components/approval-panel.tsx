/**
 * The approval panel (design 3.6.2, SEC-04, SEC-08, UX-07, UX-12). Shows the focused prompt: tool, description
 * and parameters exactly as measured by state/approval-measure.ts measureApproval(prompt,
 * approvalTextWidth(width), approvalCapsForRows(rows)) — one row per measured line, never re-wrapped — so the
 * approve gate (controller/approval-reply.ts) refuses approving while anything is off screen. Invisible
 * characters arrive as \u{XXXX} escapes and get a distinct style here.
 *
 * The action row is clickable: [Alt+Y] approve, [Alt+A] always (only with allowAlways), [Alt+N] deny, and
 * [Alt+V] view all, which becomes "[+N lines hidden - Alt+V view all]" when the panel cannot show everything.
 * A REPLv3 prompt with truncated parameters can only be denied. While a typed or dictated reply waits for its
 * second Enter the title reads "Enter: approve `<tool>` - Esc: back to editing".
 */
import type { MouseEvent } from "@opentui/core"
import type { ReactNode } from "react"
import type { AppState, ApprovalDecision, ApprovalPrompt } from "../state/model.ts"
import { approvalPosition, focusedApproval } from "../state/correlate.ts"
import { approvalCapsForRows, approvalTextWidth, measureApproval, splitInvisibleEscapes } from "../state/approval-measure.ts"
import { truncatedNotice } from "../controller/approval-reply.ts"
import { truncate } from "../util.ts"
import { borderOf, fgOf, styleOf, type Theme } from "./theme.ts"

export const APPROVAL_VOICE_HINT = "say or type yes / no, then Enter twice"

/** One measured line with its escapes styled. */
export function EscapedLine(props: { line: string; theme: Theme }): ReactNode {
  const parts = splitInvisibleEscapes(props.line)
  return (
    <text selectable={false} wrapMode="none" {...fgOf(props.theme, "text")} style={{ height: 1, flexShrink: 0 }}>
      {parts.map((part, index) =>
        part.escape ? (
          <span key={index} {...styleOf(props.theme, "escape")}>
            {part.text}
          </span>
        ) : (
          part.text
        ),
      )}
    </text>
  )
}

function panelTitle(state: AppState, prompt: ApprovalPrompt): string {
  const confirm = state.composer.confirm
  if (confirm !== null && confirm.approvalId === prompt.id) {
    return `Enter: ${confirm.decision} \`${prompt.toolName}\` - Esc: back to editing`
  }
  // Arrival order, not the queue: Alt+J rotates the queue, so its index is always 1 for the focused prompt.
  const position = approvalPosition(state, prompt.id) ?? 1
  const counter = state.approvalQueue.length > 1 ? ` ${position}/${state.approvalQueue.length}` : ""
  const sending = prompt.state === "answering" ? " - sending" : prompt.state === "failed" ? " - not sent, try again" : ""
  return `APPROVAL${counter}: ${prompt.toolName}${sending}`
}

function Button(props: { label: string; theme: Theme; onPress: () => void }): ReactNode {
  const press = (event: MouseEvent): void => {
    event.preventDefault()
    props.onPress()
  }
  return (
    <box style={{ height: 1, flexShrink: 0, marginRight: BUTTON_GAP }} onMouseDown={press}>
      <text selectable={false} wrapMode="none" {...styleOf(props.theme, "approval")}>
        {props.label}
      </text>
    </box>
  )
}

export interface ApprovalPanelProps {
  state: AppState
  /** layout.approval: 8, or 3 (compact) below 14 rows. */
  rows: number
  width: number
  theme: Theme
  onAnswer: (decision: ApprovalDecision) => void
  onView: () => void
}

interface Action {
  long: string
  short: string
  /** The narrowest label (the view button only; others keep `short`). */
  tiny?: string
  /** Dropped first when even the narrowest labels do not fit (higher drops earlier); 0 is never dropped. */
  drop: number
  onPress: () => void
}

interface PlacedAction {
  label: string
  action: Action
}

const BUTTON_GAP = 2

function rowWidth(labels: readonly string[]): number {
  return labels.reduce((sum, label) => sum + label.length + BUTTON_GAP, 0)
}

/**
 * The action row for `room` columns: long labels, else short ones, else the narrowest view label with the least
 * important buttons dropped (always, then approve, then deny). The view button ("[+N: Alt+V]") always stays,
 * since it is the one key that makes a hidden prompt answerable.
 */
function placeActions(actions: readonly Action[], room: number): PlacedAction[] {
  const long = actions.map((action) => ({ label: action.long, action }))
  if (rowWidth(long.map((entry) => entry.label)) <= room) return long
  const short = actions.map((action) => ({ label: action.short, action }))
  if (rowWidth(short.map((entry) => entry.label)) <= room) return short
  let placed = actions.map((action) => ({ label: action.tiny ?? action.short, action }))
  while (rowWidth(placed.map((entry) => entry.label)) > room) {
    const victim = placed.reduce<PlacedAction | null>((worst, entry) => (entry.action.drop > (worst?.action.drop ?? 0) ? entry : worst), null)
    if (victim === null) break
    placed = placed.filter((entry) => entry !== victim)
  }
  return placed
}

function actionsFor(prompt: ApprovalPrompt, hiddenLines: number, props: ApprovalPanelProps): Action[] {
  const actions: Action[] = []
  if (!prompt.truncated) actions.push({ long: "[Alt+Y] approve", short: "[Alt+Y]", drop: 2, onPress: () => props.onAnswer("approve") })
  if (prompt.allowAlways && !prompt.truncated) {
    actions.push({ long: "[Alt+A] always", short: "[Alt+A]", drop: 3, onPress: () => props.onAnswer("always") })
  }
  actions.push({ long: "[Alt+N] deny", short: "[Alt+N]", drop: 1, onPress: () => props.onAnswer("deny") })
  actions.push(
    hiddenLines > 0
      ? {
          long: `[+${hiddenLines} lines hidden - Alt+V view all]`,
          short: `[+${hiddenLines} hidden: Alt+V]`,
          tiny: `[+${hiddenLines}: Alt+V]`,
          drop: 0,
          onPress: props.onView,
        }
      : { long: "[Alt+V] view all", short: "[Alt+V]", drop: 0, onPress: props.onView },
  )
  return actions
}

export function ApprovalPanel(props: ApprovalPanelProps): ReactNode {
  const { state, theme } = props
  const prompt = focusedApproval(state)
  if (prompt === null || props.rows <= 0) return null
  const textWidth = approvalTextWidth(props.width)
  const measure = measureApproval(prompt, textWidth, approvalCapsForRows(props.rows))
  const textRows = Math.max(0, props.rows - 3)
  const lines = [...measure.description, ...measure.parameters].slice(0, textRows)
  const placed = placeActions(actionsFor(prompt, measure.hiddenLines, props), textWidth)
  const titleWidth = Math.max(1, props.width - 4)
  return (
    <box
      border
      {...borderOf(theme, "approval")}
      title={truncate(panelTitle(state, prompt), titleWidth)}
      bottomTitle={truncate(prompt.truncated ? truncatedNotice(prompt) : APPROVAL_VOICE_HINT, titleWidth)}
      style={{ height: props.rows, flexShrink: 0, flexDirection: "column", paddingLeft: 1, paddingRight: 1 }}
    >
      {lines.map((line, index) => (
        <EscapedLine key={index} line={line} theme={theme} />
      ))}
      {textRows > lines.length && <box style={{ height: textRows - lines.length, flexShrink: 0 }} />}
      <box style={{ height: 1, flexShrink: 0, flexDirection: "row" }}>
        {placed.map((entry) => (
          <Button key={entry.action.long} label={entry.label} theme={theme} onPress={entry.action.onPress} />
        ))}
      </box>
    </box>
  )
}
