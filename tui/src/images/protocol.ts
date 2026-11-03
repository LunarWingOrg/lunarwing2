/**
 * Terminal image protocol (design 6.4; ImageProtocol salvaged from v4:tui/src/images.ts).
 *
 * --image-protocol auto|kitty|sixel|blocks and LUNARWING_TUI_IMAGE_PROTOCOL (parsed by config/flags.ts and
 * config/config.ts) pick the backend; "auto" lets OpenTUI negotiate. The header shows
 * resolveImageRenderProtocol(requested, renderer.capabilities, renderer.resolution !== null)
 * (ot081:renderables/Image.d.ts:16). Inside tmux OpenTUI returns blocks unless a protocol is forced, and forcing
 * kitty inside tmux misplaces images (OpenTUI #1690), which /transport says.
 *
 * imageRenderSource() is the one way image bytes reach an <image> element: ImageRenderable.source also accepts
 * string | URL (ot081:image.d.ts:8), which would make OpenTUI read a file or fetch a URL, so a server-provided
 * path must never become a source.
 */
import { resolveImageRenderProtocol, type ImageRenderProtocol, type TerminalCapabilities } from "@opentui/core"
import type { ImageProtocolChoice } from "../config/types.ts"

/** The same union as OpenTUI's ImageRenderProtocol and config's ImageProtocolChoice. */
export type ImageProtocol = ImageProtocolChoice
export type ResolvedImageProtocol = Exclude<ImageRenderProtocol, "auto">

export const IMAGE_PROTOCOLS: readonly ImageProtocol[] = ["auto", "kitty", "sixel", "blocks"]

/** Exact names only; null when it is not one of them (no guessing, as in vibetui4). */
export function parseImageProtocol(value: string): ImageProtocol | null {
  return IMAGE_PROTOCOLS.find((candidate) => candidate === value) ?? null
}

export function resolveImageProtocol(
  requested: ImageProtocol,
  capabilities: TerminalCapabilities | null,
  hasResolution: boolean,
): ResolvedImageProtocol {
  return resolveImageRenderProtocol(requested, capabilities, hasResolution)
}

export interface ImageProtocolStatus {
  requested: ImageProtocol
  effective: ResolvedImageProtocol
  /** A protocol other than auto was requested. */
  forced: boolean
  tmux: boolean
  /** For the header, e.g. "kitty", "blocks (tmux)", "kitty (forced)". */
  label: string
  /** For /transport. */
  notes: string[]
}

export function describeImageProtocol(
  requested: ImageProtocol,
  capabilities: TerminalCapabilities | null,
  hasResolution: boolean,
): ImageProtocolStatus {
  const effective = resolveImageProtocol(requested, capabilities, hasResolution)
  const tmux = capabilities?.multiplexer === "tmux"
  const forced = requested !== "auto"
  const notes: string[] = []
  if (tmux && forced && effective !== "blocks") {
    notes.push(`${effective} is forced inside tmux: images may be misplaced (OpenTUI #1690)`)
  } else if (tmux) {
    notes.push("inside tmux images are block thumbnails; /image open shows the full size")
  }
  if (requested === "sixel" && effective === "blocks") notes.push("sixel needs the terminal's pixel size; using blocks")
  const reasons = [forced ? "forced" : null, tmux ? "tmux" : null].filter((r): r is string => r !== null)
  const label = reasons.length === 0 ? effective : `${effective} (${reasons.join(", ")})`
  return { requested, effective, forced, tmux, label, notes }
}

/**
 * The value for <image source>: the validated bytes or nothing. A string or URL (a server `path`, a file name) is
 * never returned, so OpenTUI cannot be made to read a file or fetch a URL.
 */
export function imageRenderSource(bytes: unknown): Uint8Array | undefined {
  return bytes instanceof Uint8Array && bytes.byteLength > 0 ? bytes : undefined
}
