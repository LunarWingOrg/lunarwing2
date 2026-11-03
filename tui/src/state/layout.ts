/**
 * Row layout of the screen (design 3.12.1, area-client 2.13). Computed, not left to flex growth, so it is
 * deterministic on resize and testable (WI#458 item 6, C8). app.tsx gives every child the height below with
 * flexShrink 0; the info panel and the tenant picker are overlays and take no rows.
 *
 * Rules: header 1. Composer clamp(composerLines, 1, 6) text rows plus a 2-row border, at most 3 text rows below
 * 16 terminal rows. Footer 1, dropped below 10 rows. Approval 8 rows while a prompt is pending, 3 (compact) below
 * 14 rows. Notices min(count, 2), at most 1 below 14 rows. Attachments while images are pending: 9 rows (previews)
 * from 16 rows, else 1 (one-line summary). The transcript gets the rest, at least 3 rows. When the rows still do
 * not fit: drop the footer, compact the attachments, then the approval, cut notices to 1, then shrink the
 * composer to 1 text row. Beyond the design's list, a terminal that is still too short also hides the notices and
 * the attachment line before the transcript would drop under 3 rows.
 */
import type { AppState } from "./model.ts"

export interface LayoutInput {
  /** From useTerminalDimensions(). */
  width: number
  height: number
  /** Active notices; at most two are shown. */
  notices: number
  approvalPending: boolean
  pendingImages: number
  /** Visual lines of the composer text (composer/metrics). */
  composerLines: number
}

export interface Layout {
  header: 1
  /** At least 3. */
  transcript: number
  /** 3 = compact one-prompt summary. */
  approval: 0 | 3 | 8
  notices: 0 | 1 | 2
  /** 1 = one-line summary, 9 = previews (7 rows plus a border). */
  attachments: 0 | 1 | 9
  /** Text rows plus the 2-row border: 3 to 8. */
  composer: number
  footer: 0 | 1
  /** Width of generated-image previews in the transcript: min(56, width - 8), at least 16. */
  imageColumns: number
}

export const MIN_TRANSCRIPT_ROWS = 3
export const MAX_COMPOSER_TEXT_ROWS = 6
/** Below this height the composer keeps at most 3 text rows and the attachment bar collapses to one line. */
export const COMPACT_COMPOSER_BELOW = 16
export const COMPACT_ATTACHMENTS_BELOW = 16
export const COMPACT_APPROVAL_BELOW = 14
export const ONE_NOTICE_BELOW = 14
export const NO_FOOTER_BELOW = 10
const COMPOSER_BORDER_ROWS = 2

type Rows = Omit<Layout, "transcript" | "imageColumns">

export function computeLayout(input: LayoutInput): Layout {
  const width = Math.max(1, Math.floor(input.width))
  const height = Math.max(1, Math.floor(input.height))
  const rows = fitRows(height, preferredRows(height, input))
  return {
    ...rows,
    transcript: Math.max(MIN_TRANSCRIPT_ROWS, height - sumRows(rows)),
    imageColumns: Math.max(16, Math.min(56, width - 8)),
  }
}

/** The layout for the current store state (credential mode shows a one-line masked composer). */
export function layoutForState(state: AppState): Layout {
  return computeLayout({
    width: state.view.width,
    height: state.view.height,
    notices: state.notices.length,
    approvalPending: state.approvalQueue.length > 0,
    pendingImages: state.composer.pendingImages.length,
    composerLines: state.composer.credential !== null ? 1 : state.composer.lines,
  })
}

function preferredRows(height: number, input: LayoutInput): Rows {
  const lines = clamp(Number.isFinite(input.composerLines) ? Math.floor(input.composerLines) : 1, 1, MAX_COMPOSER_TEXT_ROWS)
  const textRows = height < COMPACT_COMPOSER_BELOW ? Math.min(lines, 3) : lines
  const noticeCap = height < ONE_NOTICE_BELOW ? 1 : 2
  return {
    header: 1,
    approval: input.approvalPending ? (height < COMPACT_APPROVAL_BELOW ? 3 : 8) : 0,
    notices: Math.min(Math.max(0, Math.floor(input.notices)), noticeCap) as 0 | 1 | 2,
    attachments: input.pendingImages > 0 ? (height < COMPACT_ATTACHMENTS_BELOW ? 1 : 9) : 0,
    composer: textRows + COMPOSER_BORDER_ROWS,
    footer: height < NO_FOOTER_BELOW ? 0 : 1,
  }
}

/** Applies the reductions in order until the transcript keeps MIN_TRANSCRIPT_ROWS (or nothing is left to cut). */
function fitRows(height: number, preferred: Rows): Rows {
  const steps: readonly ((rows: Rows) => Rows)[] = [
    (rows) => ({ ...rows, footer: 0 }),
    // "compact approvals and attachments": the previews go first, the prompt keeps its rows as long as it can
    (rows) => ({ ...rows, attachments: rows.attachments === 9 ? 1 : rows.attachments }),
    (rows) => ({ ...rows, approval: rows.approval === 8 ? 3 : rows.approval }),
    (rows) => ({ ...rows, notices: rows.notices === 2 ? 1 : rows.notices }),
    (rows) => ({ ...rows, composer: 1 + COMPOSER_BORDER_ROWS }),
    (rows) => ({ ...rows, notices: 0 }),
    (rows) => ({ ...rows, attachments: 0 }),
  ]
  let rows = preferred
  for (const step of steps) {
    if (fits(height, rows)) return rows
    rows = step(rows)
  }
  return rows
}

function fits(height: number, rows: Rows): boolean {
  return sumRows(rows) + MIN_TRANSCRIPT_ROWS <= height
}

function sumRows(rows: Rows): number {
  return rows.header + rows.approval + rows.notices + rows.attachments + rows.composer + rows.footer
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value))
}
