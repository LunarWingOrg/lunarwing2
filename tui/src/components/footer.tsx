/**
 * The footer (design 3.12.3, UX-04): one row with the keys that apply now (controller/footer-hint.ts),
 * selectable={false}, cut to the width. Dropped below 10 terminal rows (state/layout.ts).
 */
import type { ReactNode } from "react"
import { truncate } from "../util.ts"
import { styleOf, type Theme } from "./theme.ts"

export function Footer(props: { text: string; width: number; theme: Theme }): ReactNode {
  return (
    <box style={{ height: 1, flexShrink: 0 }}>
      <text selectable={false} wrapMode="none" {...styleOf(props.theme, "dim")}>
        {truncate(props.text, Math.max(1, props.width))}
      </text>
    </box>
  )
}
