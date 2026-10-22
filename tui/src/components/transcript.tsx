/** Chat transcript rendering (tail view) with live streaming turn. */

import { Fragment, type ReactNode } from "react"
import type { ReplStatusKind } from "../protocol.ts"
import type { ImageProtocol } from "../images.ts"
import { truncate, wrapText } from "../util.ts"

export interface TranscriptImage {
  id: string
  bytes: Uint8Array
  caption?: string
}

export interface TranscriptItem {
  id: string
  kind: "user" | "queued" | "assistant" | "info" | "error"
  text: string
  images?: TranscriptImage[]
}

export interface LiveTurnView {
  text: string
  statusKind: ReplStatusKind | "idle"
  statusText: string
  images: TranscriptImage[]
}

const COLORS: Record<TranscriptItem["kind"], string> = {
  user: "#7dcfff",
  queued: "#e0af68",
  assistant: "#c0caf5",
  info: "#565f89",
  error: "#f7768e",
}

const PREFIX: Record<TranscriptItem["kind"], string> = {
  user: "you",
  queued: "you (queued)",
  assistant: "lunarwing",
  info: "·",
  error: "!",
}

/** Cell rows given to the image itself. */
const IMAGE_ROWS = 12
/** Caption line under the image. */
const CAPTION_ROWS = 1
/** Total vertical cost of one image block, used for the tail-view budget. */
const IMAGE_BLOCK_ROWS = IMAGE_ROWS + CAPTION_ROWS

/** Preferred inline image width for a given transcript width. */
function imageWidth(transcriptWidth: number): number {
  const available = Math.max(transcriptWidth - 8, 16)
  return Math.min(available, 56)
}

interface Block {
  key: string
  text: string
  height: number
  render: () => ReactNode
}

/**
 * One inline image block: the rendered image plus its caption line.
 *
 * `protocol` is passed straight through to OpenTUI; `auto` negotiates Kitty
 * graphics, Sixel, or a unicode-block fallback per terminal, while an explicit
 * value bypasses that negotiation (needed behind a tmux that can forward
 * graphics).
 */
function imageBlock(
  image: TranscriptImage,
  key: string,
  width: number,
  protocol: ImageProtocol,
): Block {
  const caption = image.caption ?? ""
  return {
    key,
    // A non-empty caption makes the budget charge its wrapped height, but the
    // image rows dominate, so the cost stays IMAGE_BLOCK_ROWS either way.
    text: caption,
    height: IMAGE_BLOCK_ROWS,
    render: () => (
      <box key={image.id} style={{ flexDirection: "column", height: IMAGE_BLOCK_ROWS }}>
        <image
          source={image.bytes}
          fit="fit"
          protocol={protocol}
          style={{ width, height: IMAGE_ROWS }}
        />
        {caption.length > 0 && <text fg="#565f89">{truncate(caption, width)}</text>}
      </box>
    ),
  }
}

/** Build renderable blocks from the transcript plus the live turn. */
function buildBlocks(
  items: TranscriptItem[],
  live: LiveTurnView | null,
  width: number,
  protocol: ImageProtocol,
): Block[] {
  const blocks: Block[] = []
  const inlineWidth = imageWidth(width)

  for (const item of items) {
    blocks.push({
      key: `h-${item.id}`,
      text: `${PREFIX[item.kind]} ▏ ${item.text}`,
      height: 1,
      render: () => (
        <text fg={COLORS[item.kind]}>{`${PREFIX[item.kind]} ▏ ${item.text}`}</text>
      ),
    })
    for (const image of item.images ?? []) {
      blocks.push(imageBlock(image, `i-${image.id}`, inlineWidth, protocol))
    }
  }

  if (live !== null) {
    const liveText = live.text.length > 0 ? live.text : "…"
    blocks.push({
      key: "live-turn",
      text: `lunarwing ▏ ${liveText}`,
      height: 1,
      render: () => <text fg="#c0caf5">{`lunarwing ▏ ${liveText}`}</text>,
    })
    // Images generated during the live turn render as they arrive, in the same
    // shape they keep once the turn seals into a transcript item.
    for (const image of live.images) {
      blocks.push(imageBlock(image, `li-${image.id}`, inlineWidth, protocol))
    }
    if (live.statusKind !== "idle" && live.statusText.length > 0) {
      blocks.push({
        key: "live-status",
        text: `  ⋮ ${live.statusText}`,
        height: 1,
        render: () => <text fg="#9ece6a">{`  ⋮ ${live.statusText}`}</text>,
      })
    }
  }

  return blocks
}

/** Tail-view transcript: keeps the most recent blocks that fit. */
export function Transcript(props: {
  items: TranscriptItem[]
  live: LiveTurnView | null
  height: number
  width: number
  /** Requested image backend; defaults to renderer negotiation. */
  protocol?: ImageProtocol
}): ReactNode {
  const blocks = buildBlocks(props.items, props.live, props.width, props.protocol ?? "auto")
  const innerWidth = Math.max(props.width - 4, 12)
  const budget = Math.max(props.height - 2, 1)

  const kept: Block[] = []
  let used = 0
  for (let i = blocks.length - 1; i >= 0; i--) {
    const block = blocks[i]
    const cost = block.text.length === 0
      ? block.height
      : Math.max(wrapText(block.text, innerWidth).length, block.height)
    if (used + cost > budget && kept.length > 0) {
      break
    }
    used += cost
    kept.unshift(block)
  }

  return (
    <box style={{ flexDirection: "column", flexGrow: 1, padding: 0 }}>
      {kept.map((block) => (
        <Fragment key={block.key}>{block.render()}</Fragment>
      ))}
    </box>
  )
}
