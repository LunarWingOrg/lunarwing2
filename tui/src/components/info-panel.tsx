/**
 * The info panel (design 3.12.3, UX-04): an overlay box with position "absolute" and zIndex 10, top 1, left 2,
 * width view.width - 4, height view.height - 4, a border titled with the panel kind. It takes no layout rows.
 *
 * Content comes from the pure controller/panel-content.ts buildPanel(), so live panels (voice) follow the state.
 * The panel shows a window of those lines starting at AppState.panel.scroll (PageUp/PageDown, clamped here), which
 * keeps scrolling exact and testable. In the approval full view, \u{XXXX} escapes get their distinct style.
 * Esc closes the panel before anything else (controller/keymap.ts).
 */
import type { ReactNode } from "react"
import type { PanelKind } from "../state/model.ts"
import { truncate } from "../util.ts"
import { EscapedLine } from "./approval-panel.tsx"
import { borderOf, fgOf, styleOf, type Theme } from "./theme.ts"

export const INFO_PANEL_HINT = "PageUp/PageDown scroll - Esc close"

/** Inner text rows of the panel for a terminal of `height` rows (border and the hint row excluded). */
export function infoPanelRows(height: number): number {
  return Math.max(1, Math.floor(height) - 4 - 2 - 1)
}

/** Text columns inside the panel for a terminal of `width` columns. */
export function infoPanelColumns(width: number): number {
  return Math.max(10, Math.floor(width) - 4 - 2 - 2)
}

/** The first shown line: `scroll` clamped so the window never runs past the end. */
export function clampPanelScroll(scroll: number, total: number, rows: number): number {
  return Math.max(0, Math.min(Math.floor(scroll), Math.max(0, total - rows)))
}

export interface InfoPanelProps {
  kind: PanelKind
  title: string
  lines: readonly string[]
  scroll: number
  viewWidth: number
  viewHeight: number
  theme: Theme
}

export function InfoPanel(props: InfoPanelProps): ReactNode {
  const { theme } = props
  const rows = infoPanelRows(props.viewHeight)
  const start = clampPanelScroll(props.scroll, props.lines.length, rows)
  const shown = props.lines.slice(start, start + rows)
  const more = props.lines.length > rows ? ` (${start + 1}-${start + shown.length} of ${props.lines.length})` : ""
  const columns = infoPanelColumns(props.viewWidth)
  const background = theme.panelBackground
  return (
    <box
      border
      {...borderOf(theme, "accent")}
      title={truncate(`${props.title}${more}`, Math.max(1, columns))}
      backgroundColor={background}
      style={{
        position: "absolute",
        zIndex: 10,
        top: 1,
        left: 2,
        width: Math.max(12, props.viewWidth - 4),
        height: Math.max(5, props.viewHeight - 4),
        flexDirection: "column",
        paddingLeft: 1,
        paddingRight: 1,
      }}
    >
      {shown.map((line, index) =>
        props.kind === "approval" ? (
          <EscapedLine key={start + index} line={line} theme={theme} />
        ) : (
          <text key={start + index} selectable={false} wrapMode="none" {...fgOf(theme, "text")} style={{ height: 1, flexShrink: 0 }}>
            {truncate(line, columns)}
          </text>
        ),
      )}
      <box style={{ flexGrow: 1 }} />
      <text selectable={false} wrapMode="none" {...styleOf(theme, "dim")} style={{ height: 1, flexShrink: 0 }}>
        {truncate(INFO_PANEL_HINT, columns)}
      </text>
    </box>
  )
}
