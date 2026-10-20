/** Pending attachment strip with inline image previews. */

import type { PendingImage } from "../protocol.ts"
import type { ReactNode } from "react"
import { truncate } from "../util.ts"

const PREVIEW_WIDTH = 20
const PREVIEW_HEIGHT = 7

export function AttachmentBar(props: { images: PendingImage[] }): ReactNode | null {
  if (props.images.length === 0) {
    return null
  }
  return (
    <box
      title={`Attachments (${props.images.length}) — Ctrl+R remove last, Ctrl+X clear`}
      style={{ border: true, flexDirection: "row", gap: 1, padding: 0, height: PREVIEW_HEIGHT + 2 }}
    >
      {props.images.map((image) => (
        <box key={image.id} style={{ flexDirection: "column", width: PREVIEW_WIDTH + 2 }}>
          <image
            source={image.bytes}
            fit="cover"
            protocol="auto"
            style={{ width: PREVIEW_WIDTH, height: PREVIEW_HEIGHT }}
          />
          <text fg="#7aa2f7">{truncate(image.filename ?? image.id, PREVIEW_WIDTH + 2)}</text>
        </box>
      ))}
    </box>
  )
}
