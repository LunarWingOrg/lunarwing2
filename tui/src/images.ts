/**
 * Multimodal input helpers: clipboard image paste, file attachment, and
 * MIME sniffing by magic bytes (no external dependencies).
 */

import { spawnSync } from "node:child_process"
import { readFileSync } from "node:fs"
import type { PendingImage } from "./protocol.ts"

let imageCounter = 0

function nextImageId(): string {
  imageCounter += 1
  return `img-${Date.now().toString(36)}-${imageCounter}`
}

/** Sniff the MIME type from magic bytes; null when not a known image type. */
export function sniffImageMime(bytes: Uint8Array): string | null {
  const startsWith = (...prefix: number[]) =>
    prefix.every((byte, index) => bytes[index] === byte)

  if (startsWith(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a)) return "image/png"
  if (startsWith(0xff, 0xd8, 0xff)) return "image/jpeg"
  if (startsWith(0x47, 0x49, 0x46, 0x38)) return "image/gif"
  if (
    startsWith(0x52, 0x49, 0x46, 0x46) &&
    bytes[8] === 0x57 &&
    bytes[9] === 0x45 &&
    bytes[10] === 0x42 &&
    bytes[11] === 0x50
  ) {
    return "image/webp"
  }
  if (startsWith(0x42, 0x4d)) return "image/bmp"
  return null
}

/** Wrap raw bytes as a pending image; returns null when not an image. */
export function makePendingImage(
  bytes: Uint8Array,
  filename: string | null,
): PendingImage | null {
  const mime = sniffImageMime(bytes)
  if (mime === null) {
    return null
  }
  return { id: nextImageId(), mime_type: mime, filename, bytes }
}

/** Read an image file from disk as a pending image. */
export function attachFile(path: string): PendingImage | null {
  const clean = path.trim().replace(/^["']|["']$/g, "")
  let bytes: Uint8Array
  try {
    bytes = new Uint8Array(readFileSync(clean))
  } catch {
    return null
  }
  return makePendingImage(bytes, clean.split("/").pop() ?? clean)
}

interface ClipboardCommand {
  command: string
  args: string[]
}

/** Platform clipboard read commands, in preference order. */
function clipboardCommands(): ClipboardCommand[] {
  const commands: ClipboardCommand[] = []
  if (process.platform === "darwin") {
    commands.push({ command: "pngpaste", args: [] })
  } else if (process.platform === "win32") {
    commands.push({
      command: "powershell",
      args: [
        "-NoProfile",
        "-Command",
        "Add-Type -AssemblyName System.Windows.Forms;$clip=[Windows.Forms.Clipboard]::GetImage();if($clip){$ms=New-Object IO.MemoryStream;$clip.Save($ms,[Drawing.Imaging.ImageFormat]::Png);$ms.ToArray()}",
      ],
    })
  } else {
    // Linux: Wayland first, then X11.
    commands.push({ command: "wl-paste", args: ["--type", "image/png"] })
    commands.push({
      command: "xclip",
      args: ["-selection", "clipboard", "-t", "image/png", "-o"],
    })
    commands.push({
      command: "xsel",
      args: ["--clipboard", "--output"],
    })
  }
  return commands
}

export type ClipboardRead =
  | { kind: "image"; image: PendingImage }
  | { kind: "text"; text: string }
  | { kind: "unavailable"; reason: string }

/**
 * Read the system clipboard. Image content wins; otherwise text is returned
 * so the caller can insert it into the input line.
 */
export function readClipboard(): ClipboardRead {
  for (const { command, args } of clipboardCommands()) {
    const result = spawnSync(command, args, {
      timeout: 3_000,
      maxBuffer: 32 * 1024 * 1024,
    })
    if (result.error !== undefined) {
      continue // tool not installed
    }
    if (result.status !== 0) {
      continue // tool present, clipboard empty or not an image
    }
    const stdout = result.stdout
    if (stdout.length === 0) {
      continue
    }
    const image = makePendingImage(new Uint8Array(stdout), null)
    if (image !== null) {
      return { kind: "image", image }
    }
    const text = stdout.toString("utf8").trim()
    if (text.length > 0) {
      return { kind: "text", text }
    }
  }
  return {
    kind: "unavailable",
    reason:
      "no clipboard tool found (install wl-paste/xclip, or pngpaste on macOS); use /img <path> instead",
  }
}
