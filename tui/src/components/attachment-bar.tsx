/**
 * Pending attachments (design 6.4, 3.12.1, 5.6; kept from v4:tui/src/components/attachment-bar.tsx).
 *
 * With the 9 rows state/layout.ts gives it (16 or more terminal rows): a bordered strip of previews, each a
 * 20x6-cell <image fit="cover"> over a caption like "shot.png 412 KiB 1280x720"; the top border names the count and
 * the keys, the bottom border says where sent images end up ("stored in <tenant>'s workspace", DEC-3 default:
 * keep and disclose). With 1 row: one line, "[2 images] Ctrl+R remove last, Ctrl+X clear - stored in ...".
 * Previews that do not fit the width are left out, newest kept, and the title says so.
 *
 * The <image> source is always the validated bytes (images/protocol.ts imageRenderSource), never a path. All text
 * is chrome: selectable={false}, so a drag selects transcript text only (3.12.4).
 */
import type { ReactNode } from "react"
import { imageStorageCaption } from "../images/attach.ts"
import { formatBytes } from "../images/limits.ts"
import { imageRenderSource, type ImageProtocol } from "../images/protocol.ts"
import { extensionFor } from "../images/sniff.ts"
import { sanitizeLine } from "../sanitize.ts"
import type { PendingImage } from "../state/model.ts"
import type { ThemeColor } from "./theme.ts"

export const PREVIEW_WIDTH = 20
/** 9 layout rows = 2 border rows + a 6-row preview + its caption row. */
export const PREVIEW_HEIGHT = 6
/** Wide enough for "shot.png 412 KiB 1280x720". */
export const TILE_WIDTH = 26
const TILE_GAP = 1
const BORDER_COLUMNS = 2
const BORDER_ROWS = 2
const KEYS_HINT = "Ctrl+R remove last, Ctrl+X clear"

export interface AttachmentBarColors {
  /** Captions and the compact line. */
  text?: ThemeColor
  /** Size and dimension details. */
  dim?: ThemeColor
  border?: ThemeColor
}

export interface AttachmentBarProps {
  images: readonly PendingImage[]
  /** Rows from state/layout.ts: 0 hides the bar, 1 is the one-line summary, 9 shows previews. */
  rows: number
  /** Columns available to the bar (the terminal width). */
  width: number
  /** For the storage caption; null outside a tenant ("the daemon's workspace"). */
  tenant: string | null
  /** The requested backend (config ui.imageProtocol); default "auto". */
  protocol?: ImageProtocol
  /** The theme's colours (the monochrome theme passes the terminal's default foreground, never OpenTUI's white). */
  colors?: AttachmentBarColors
}

/** Display cells of `text` (wide characters count 2, combining marks 0). */
function cells(text: string): number {
  return Bun.stringWidth(text)
}

/** `text` cut to at most `max` cells, with "…" when cut. */
export function fitCells(text: string, max: number): string {
  if (max <= 0) return ""
  if (cells(text) <= max) return text
  let out = ""
  let used = 0
  for (const char of text) {
    const width = cells(char)
    if (used + width > max - 1) break
    out += char
    used += width
  }
  return `${out}…`
}

function displayName(image: PendingImage): string {
  const name = image.filename === null ? "" : sanitizeLine(image.filename).trim()
  return name.length > 0 ? name : `${image.source}.${extensionFor(image.mime)}`
}

/** "shot.png 412 KiB 1280x720", the name shortened first so size and dimensions stay visible. */
export function attachmentCaption(image: PendingImage, maxCells: number = TILE_WIDTH): string {
  const details = `${formatBytes(image.bytes.byteLength)} ${image.width}x${image.height}`
  const room = maxCells - cells(details) - 1
  if (room < 4) return fitCells(`${displayName(image)} ${details}`, maxCells)
  return `${fitCells(displayName(image), room)} ${details}`
}

/** The one-line form: "[2 images] Ctrl+R remove last, Ctrl+X clear - stored in derpy's workspace". */
export function attachmentSummary(count: number, tenant: string | null, maxCells = Number.POSITIVE_INFINITY): string {
  const noun = count === 1 ? "image" : "images"
  return fitCells(`[${count} ${noun}] ${KEYS_HINT} - ${imageStorageCaption(tenant)}`, maxCells)
}

/** How many preview tiles fit in `width` columns (at least one). */
export function previewSlots(width: number): number {
  const inner = Math.floor(width) - BORDER_COLUMNS
  return Math.max(1, Math.floor((inner + TILE_GAP) / (TILE_WIDTH + TILE_GAP)))
}

function previewTitle(total: number, shown: number, maxCells: number): string {
  const count = shown < total ? `${total}, newest ${shown} shown` : `${total}`
  return fitCells(`Attachments (${count}) - ${KEYS_HINT}`, maxCells)
}

function colorProps(color: ThemeColor | undefined): { fg?: ThemeColor } {
  return color === undefined ? {} : { fg: color }
}

export function AttachmentBar(props: AttachmentBarProps): ReactNode {
  const { images, rows, width } = props
  if (images.length === 0 || rows <= 0) return null
  const colors = props.colors ?? {}
  if (rows < 4) {
    return (
      <box style={{ height: 1, flexShrink: 0 }}>
        <text selectable={false} {...colorProps(colors.text)}>
          {attachmentSummary(images.length, props.tenant, Math.max(1, width))}
        </text>
      </box>
    )
  }
  const previewHeight = Math.min(PREVIEW_HEIGHT, rows - BORDER_ROWS - 1)
  const visible = images.slice(-previewSlots(width))
  const titleCells = Math.max(1, width - 4)
  return (
    <box
      border
      title={previewTitle(images.length, visible.length, titleCells)}
      bottomTitle={fitCells(imageStorageCaption(props.tenant), titleCells)}
      {...(colors.border === undefined ? {} : { borderColor: colors.border })}
      style={{ height: rows, flexDirection: "row", gap: TILE_GAP, flexShrink: 0, overflow: "hidden" }}
    >
      {visible.map((image) => (
        <box key={image.id} style={{ flexDirection: "column", width: TILE_WIDTH, flexShrink: 0 }}>
          <image
            source={imageRenderSource(image.bytes)}
            fit="cover"
            protocol={props.protocol ?? "auto"}
            style={{ width: PREVIEW_WIDTH, height: previewHeight, flexShrink: 0 }}
          />
          <text selectable={false} {...colorProps(colors.dim ?? colors.text)} style={{ height: 1, flexShrink: 0 }}>
            {attachmentCaption(image, TILE_WIDTH)}
          </text>
        </box>
      ))}
    </box>
  )
}
