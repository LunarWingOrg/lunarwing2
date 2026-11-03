/**
 * The transcript (design 3.12.1, C5, C8): one <scrollbox stickyScroll stickyStart="bottom" scrollX={false}> with
 * an explicit height from state/layout.ts, holding every TurnView, the live one included, so multi-line live text
 * is fully visible and scrolls like the rest. Manual scroll-up is kept while content arrives; PageDown to the end
 * (or the mouse wheel) re-engages following. An empty thread shows a welcome block: tenant, transport and engine,
 * "exact" or "legacy daemon", the push-to-talk keys for this environment and "F1 help".
 */
import type { ScrollBoxRenderable } from "@opentui/core"
import type { ReactNode } from "react"
import type { ImageProtocol } from "../images/protocol.ts"
import type { AppState, TurnRecord } from "../state/model.ts"
import { queuePosition } from "../state/correlate.ts"
import { fgOf, styleOf, type Theme } from "./theme.ts"
import { TurnView } from "./turn-view.tsx"

export interface TranscriptProps {
  state: AppState
  height: number
  theme: Theme
  now: number
  protocol: ImageProtocol
  imageColumns: number
  /** The push-to-talk sentence for this environment (controller/panel-content.ts pttHelp). */
  pttHelp: string
  /** The scrollbox, for PageUp/PageDown. */
  onScrollbox?: (box: ScrollBoxRenderable | null) => void
}

export function welcomeLines(state: AppState, pttHelp: string): string[] {
  const server = state.server
  const where =
    server === null
      ? state.conn.kind === "offline"
        ? `offline: ${state.conn.reason}`
        : "connecting..."
      : `${server.transport} ${server.endpointLabel}, engine ${server.engine}, ${server.capabilities.correlation ? "exact" : "legacy daemon"}`
  return [`LunarWing ${state.tenant ?? state.profile}`, where, `push-to-talk: ${pttHelp}`, "F1 help"]
}

/** The credential request's instructions and URLs (shown as text, never opened), under its owning turn. */
export function credentialHint(state: AppState): string | null {
  const credential = state.composer.credential
  if (credential === null) return null
  const parts = [`credential needed for ${credential.extensionName}`]
  if (credential.instructions !== undefined && credential.instructions.trim() !== "") parts.push(credential.instructions.trim())
  if (credential.authUrl !== undefined) parts.push(`auth: ${credential.authUrl}`)
  if (credential.setupUrl !== undefined) parts.push(`setup: ${credential.setupUrl}`)
  return parts.join(" - ")
}

function Welcome(props: { state: AppState; pttHelp: string; theme: Theme }): ReactNode {
  const lines = welcomeLines(props.state, props.pttHelp)
  return (
    <box style={{ flexDirection: "column", flexShrink: 0 }}>
      {lines.map((line, index) => (
        <text key={index} selectable={false} wrapMode="word" {...(index === 0 ? styleOf(props.theme, "title") : fgOf(props.theme, "dim"))}>
          {line}
        </text>
      ))}
    </box>
  )
}

export function Transcript(props: TranscriptProps): ReactNode {
  const { state, theme } = props
  const streaming = state.server?.capabilities.streaming ?? "unknown"
  const hint = credentialHint(state)
  const owner = state.composer.credential?.turnId ?? null
  const records = state.order.map((id) => state.turns[id]).filter((record): record is TurnRecord => record !== undefined)
  return (
    <scrollbox
      ref={props.onScrollbox}
      stickyScroll
      stickyStart="bottom"
      scrollX={false}
      focusable={false}
      // Short content sits at the bottom, right above the composer, like a terminal (3.12.1).
      contentOptions={{ minHeight: "100%", justifyContent: "flex-end" }}
      verticalScrollbarOptions={{ trackOptions: { foregroundColor: theme.scrollbar.thumb, backgroundColor: theme.scrollbar.track } }}
      style={{ height: props.height, flexShrink: 0 }}
    >
      {records.length === 0 && <Welcome state={state} pttHelp={props.pttHelp} theme={theme} />}
      {records.map((record) => (
        <TurnView
          key={record.id}
          record={record}
          theme={theme}
          now={props.now}
          queuePosition={record.phase === "queued" ? queuePosition(state, record.id) : null}
          streaming={streaming}
          protocol={props.protocol}
          imageColumns={props.imageColumns}
          credentialHint={owner === record.id ? hint : null}
        />
      ))}
      {hint !== null && (owner === null || state.turns[owner] === undefined) && (
        <text selectable={false} wrapMode="word" {...styleOf(theme, "warn")}>
          {hint}
        </text>
      )}
    </scrollbox>
  )
}
