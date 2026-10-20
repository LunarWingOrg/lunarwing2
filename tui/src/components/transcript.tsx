/** Chat transcript rendering (tail view) with live streaming turn. */

import type { ReactNode } from "react"
import type { ReplStatusKind } from "../protocol.ts"
import { wrapText } from "../util.ts"

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

interface Block {
  key: string
  text: string
  height: number
  render: () => ReactNode
}

/** Build renderable blocks from the transcript plus the live turn. */
function buildBlocks(items: TranscriptItem[], live: LiveTurnView | null): Block[] {
  const blocks: Block[] = []

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
      blocks.push({
        key: `i-${image.id}`,
        text: "",
        height: 8,
        render: () => (
          <box key={image.id} style={{ height: 8 }}>
            <image
              source={image.bytes}
              fit="cover"
              protocol="auto"
              style={{ width: 24, height: 7 }}
            />
          </box>
        ),
      })
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
}): ReactNode {
  const blocks = buildBlocks(props.items, props.live)
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
      {kept.map((block) => block.render())}
    </box>
  )
}
