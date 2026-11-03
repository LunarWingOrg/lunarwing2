/**
 * Notices (design 3.6.3): at most `rows` (layout: 0, 1 or 2) of the newest notices, one line each, with a level
 * word ("info:", "warn:", "error:") so the level never depends on colour. Info expires after 6 s, warn after
 * 12 s, errors stay until Esc or a newer error (state/correlate.ts pushNotice).
 */
import type { ReactNode } from "react"
import type { Notice } from "../state/model.ts"
import { truncate } from "../util.ts"
import { styleOf, type Theme, type ThemeRole } from "./theme.ts"

const ROLE: Readonly<Record<Notice["level"], ThemeRole>> = { info: "info", warn: "warn", error: "error" }

export function noticeLine(notice: Notice): string {
  return `${notice.level}: ${notice.text}`
}

export function Notices(props: { notices: readonly Notice[]; rows: number; width: number; theme: Theme }): ReactNode {
  if (props.rows <= 0 || props.notices.length === 0) return null
  const shown = props.notices.slice(-props.rows)
  return (
    <box style={{ height: props.rows, flexShrink: 0, flexDirection: "column" }}>
      {shown.map((notice) => (
        <text key={notice.id} selectable={false} wrapMode="none" {...styleOf(props.theme, ROLE[notice.level])} style={{ height: 1, flexShrink: 0 }}>
          {truncate(noticeLine(notice), Math.max(1, props.width))}
        </text>
      ))}
    </box>
  )
}
