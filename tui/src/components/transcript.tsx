/**
 * Chat transcript: a scrollable history with a live streaming turn.
 *
 * The view keeps the whole conversation and sticks to the bottom as new
 * content arrives, so history is never discarded to fit the screen — scroll
 * up to read it, and sticky-scroll re-engages when you return to the bottom.
 */

import type { ReactNode } from "react"
import type { ReplStatusKind } from "../protocol.ts"
import type { ImageProtocol } from "../images.ts"
import { truncate } from "../util.ts"

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
/** Total vertical size of one image block. */
const IMAGE_BLOCK_ROWS = IMAGE_ROWS + CAPTION_ROWS

/** Preferred inline image width for a given transcript width. */
function imageWidth(transcriptWidth: number): number {
  const available = Math.max(transcriptWidth - 8, 16)
  return Math.min(available, 56)
}

/**
 * One inline image block: the rendered image plus its caption line.
 *
 * `protocol` is passed straight through to OpenTUI; `auto` negotiates Kitty
 * graphics, Sixel, or a unicode-block fallback per terminal, while an explicit
 * value bypasses that negotiation (needed behind a tmux that can forward
 * graphics).
 */
function ImageBlock(props: {
  image: TranscriptImage
  width: number
  protocol: ImageProtocol
}): ReactNode {
  const caption = props.image.caption ?? ""
  return (
    <box style={{ flexDirection: "column", height: IMAGE_BLOCK_ROWS }}>
      <image
        source={props.image.bytes}
        fit="fit"
        protocol={props.protocol}
        style={{ width: props.width, height: IMAGE_ROWS }}
      />
      {caption.length > 0 && <text fg="#565f89">{truncate(caption, props.width)}</text>}
    </box>
  )
}

/** One sealed transcript entry: its header line plus any inline images. */
function ItemBlock(props: {
  item: TranscriptItem
  width: number
  protocol: ImageProtocol
}): ReactNode {
  const item = props.item
  return (
    <box style={{ flexDirection: "column" }}>
      <text fg={COLORS[item.kind]}>{`${PREFIX[item.kind]} ▏ ${item.text}`}</text>
      {(item.images ?? []).map((image) => (
        <ImageBlock key={image.id} image={image} width={props.width} protocol={props.protocol} />
      ))}
    </box>
  )
}

/** The turn currently streaming: text, images arriving so far, and status. */
function LiveBlock(props: {
  live: LiveTurnView
  width: number
  protocol: ImageProtocol
}): ReactNode {
  const live = props.live
  const liveText = live.text.length > 0 ? live.text : "…"
  return (
    <box style={{ flexDirection: "column" }}>
      <text fg="#c0caf5">{`lunarwing ▏ ${liveText}`}</text>
      {live.images.map((image) => (
        <ImageBlock key={image.id} image={image} width={props.width} protocol={props.protocol} />
      ))}
      {live.statusKind !== "idle" && live.statusText.length > 0 && (
        <text fg="#9ece6a">{`  ⋮ ${live.statusText}`}</text>
      )}
    </box>
  )
}

/**
 * Scrollable transcript.
 *
 * `stickyScroll` keeps the view pinned to the newest content while a turn
 * streams, and releases the moment the user scrolls up so history stays
 * readable.
 */
export function Transcript(props: {
  items: TranscriptItem[]
  live: LiveTurnView | null
  height: number
  width: number
  /** Requested image backend; defaults to renderer negotiation. */
  protocol?: ImageProtocol
}): ReactNode {
  const protocol = props.protocol ?? "auto"
  const inlineWidth = imageWidth(props.width)

  return (
    <scrollbox
      stickyScroll
      stickyStart="bottom"
      scrollX={false}
      style={{ flexGrow: 1, height: props.height }}
    >
      {props.items.map((item) => (
        <ItemBlock key={item.id} item={item} width={inlineWidth} protocol={protocol} />
      ))}
      {props.live !== null && (
        <LiveBlock key="live-turn" live={props.live} width={inlineWidth} protocol={protocol} />
      )}
    </scrollbox>
  )
}
