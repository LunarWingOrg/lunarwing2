/**
 * The header row (design 3.12.1, 3.12.2): tenant or profile, connection state, engine and correlation mode,
 * image protocol, "second window" and the other-thread counter. One row, selectable={false}, cut to the width.
 * Every state is a word (OFFLINE, legacy, second window), never colour alone.
 */
import type { ReactNode } from "react"
import type { AppState } from "../state/model.ts"
import { formatSeconds, truncate } from "../util.ts"
import { styleOf, type Theme } from "./theme.ts"

export interface HeaderInfo {
  /** False for a secondary instance of the profile (3.11.5). */
  primary: boolean
  /** Header label of the image protocol (images/protocol.ts describeImageProtocol().label), or null. */
  imageLabel: string | null
}

/** "ready", "connecting (2)", "reconnecting in 3 s", "OFFLINE". */
export function connectionLabel(state: Pick<AppState, "conn">, now: number): string {
  const conn = state.conn
  switch (conn.kind) {
    case "connecting":
      return conn.attempt > 1 ? `connecting (${conn.attempt})` : "connecting"
    case "ready":
      return "ready"
    case "reconnecting":
      return `reconnecting in ${formatSeconds(Math.max(0, conn.retryAt - now))}`
    case "offline":
      return conn.reason === "quit" ? "OFFLINE (quit)" : "OFFLINE"
  }
}

/** The header text without styling (tests, NO_COLOR). */
export function headerText(state: AppState, info: HeaderInfo, now: number): string {
  const parts = [`LunarWing ${state.tenant ?? state.profile}`, connectionLabel(state, now)]
  const server = state.server
  if (server !== null) {
    parts.push(`engine ${server.engine}`)
    parts.push(server.capabilities.correlation ? "exact" : "legacy daemon")
  }
  if (info.imageLabel !== null) parts.push(`images ${info.imageLabel}`)
  if (!info.primary) parts.push("second window")
  if (state.otherThreadsUnread > 0) parts.push(`+${state.otherThreadsUnread} in other threads`)
  return parts.join(" | ")
}

export function Header(props: { state: AppState; info: HeaderInfo; width: number; now: number; theme: Theme }): ReactNode {
  const { state, theme } = props
  const offline = state.conn.kind === "offline"
  const text = truncate(headerText(state, props.info, props.now), Math.max(1, props.width))
  return (
    <box style={{ height: 1, flexShrink: 0 }}>
      <text selectable={false} wrapMode="none" {...styleOf(theme, offline ? "error" : "title")}>
        {text}
      </text>
    </box>
  )
}
