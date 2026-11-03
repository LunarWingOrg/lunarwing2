/**
 * /shot [full] (design 6.1, UX-08): a region (or one output) captured with slurp and grim.
 *
 * - Region: ["slurp"] (60 s); exit 0 yields one line matching /^-?\d+,-?\d+ \d+x\d+$/; a non-zero exit (Esc in
 *   slurp) is a quiet cancel. Full: ["slurp", "-o"], the user clicks one output, the same validation. Picking the
 *   output is the deliberate second step for a full-screen capture.
 * - Then ["grim", "-s", "1", "-g", geometry, "-"]: grim's default scale is the greatest output scale (1.5 on this
 *   host), so a full capture of the 6400x1440 layout would come out near 9600x2160 and no PNG of it fits 5 MiB;
 *   every capture therefore pins the scale to 1. Each grim run has a 10 s timeout and a stdout cap of
 *   maxImageBytes + 1 bytes (the child is killed when exceeded). A PNG over the limit is retried once as
 *   ["grim", "-s", "1", "-t", "jpeg", "-q", "85", "-g", geometry, "-"]; still too large: "screenshot is N MiB
 *   after JPEG; select a smaller region". The result must sniff as PNG, or as JPEG when JPEG was requested.
 *
 * No shell (the brief's grim -g "$(slurp)" - is two argv calls), no clipboard (cliphist records clipboard images;
 * this is the recommended path), no temporary file. The capture is only attached; it is sent with the next Enter.
 */
import { SystemClock, type Clock } from "@opentui/core"
import { bunSpawn, type ChildEnv, type SpawnFn } from "../config/types.ts"
import { sanitizeLine } from "../sanitize.ts"
import type { ImageMime, PendingImage } from "../state/model.ts"
import { createPendingImage } from "./attach.ts"
import { isMissingExecutable, runCapped, type CappedRunResult } from "./clipboard.ts"
import { formatBytes, ImageInputError, type ImageInspector } from "./limits.ts"
import { sniffImageMime } from "./sniff.ts"

export const SLURP_TIMEOUT_MS = 60_000
export const GRIM_TIMEOUT_MS = 10_000
/** slurp's default output format "%x,%y %wx%h". */
export const SLURP_GEOMETRY = /^-?\d+,-?\d+ \d+x\d+$/
const SLURP_MAX_STDOUT_BYTES = 256
/** The JPEG retry counts up to this multiple of the limit before it is killed, so the message can give the size. */
const JPEG_COUNT_FACTOR = 8

export interface ScreenshotOptions {
  /** /shot full: pick one output with slurp -o. */
  full: boolean
  /** Limits.maxImageBytes: grim's stdout cap and the per-image limit. */
  maxBytes: number
  /** config/child-env.ts buildChildEnv(); must hold WAYLAND_DISPLAY. */
  env: ChildEnv
  clock?: Clock
  /** Aborting (Esc, quit) kills slurp or grim and resolves as cancelled. */
  signal?: AbortSignal
  spawn?: SpawnFn
}

export type ScreenshotCapture =
  | { kind: "captured"; bytes: Uint8Array; mime: "image/png" | "image/jpeg"; geometry: string; jpegFallback: boolean }
  | { kind: "cancelled" }

export function slurpArgv(full: boolean): string[] {
  return full ? ["slurp", "-o"] : ["slurp"]
}

export function grimArgv(geometry: string, jpeg: boolean): string[] {
  return jpeg
    ? ["grim", "-s", "1", "-t", "jpeg", "-q", "85", "-g", geometry, "-"]
    : ["grim", "-s", "1", "-g", geometry, "-"]
}

/** Runs slurp and grim; throws ImageInputError for every failure except a cancel. */
export async function captureScreenshot(opts: ScreenshotOptions): Promise<ScreenshotCapture> {
  if ((opts.env.WAYLAND_DISPLAY ?? "") === "") {
    throw new ImageInputError("unavailable", "/shot needs a Wayland session (WAYLAND_DISPLAY is not set)")
  }
  const run = runner(opts)
  const geometry = await selectGeometry(run, opts.full)
  if (geometry === null) return { kind: "cancelled" }
  const png = await capture(run, geometry, false, opts.maxBytes, 0)
  if (png.aborted) return { kind: "cancelled" }
  if (!png.overflow) return captured(png, geometry, "image/png")
  const jpeg = await capture(run, geometry, true, opts.maxBytes, opts.maxBytes * JPEG_COUNT_FACTOR)
  if (jpeg.aborted) return { kind: "cancelled" }
  if (jpeg.overflow) {
    const size = jpeg.overflowKilled ? `more than ${formatBytes(jpeg.stdoutBytes)}` : formatBytes(jpeg.stdoutBytes)
    throw new ImageInputError("too_large", `screenshot is ${size} after JPEG; select a smaller region`)
  }
  return captured(jpeg, geometry, "image/jpeg")
}

export interface TakeScreenshotOptions extends ScreenshotOptions {
  /** Limits.imageMimeTypes; default all four. */
  imageMimeTypes?: readonly ImageMime[]
  newId?: () => string
  inspect?: ImageInspector
}

/** captureScreenshot() validated as a PendingImage (source "shot", "shot.png" or "shot.jpg"); null when cancelled. */
export async function takeScreenshot(opts: TakeScreenshotOptions): Promise<PendingImage | null> {
  const shot = await captureScreenshot(opts)
  if (shot.kind === "cancelled") return null
  return createPendingImage(shot.bytes, {
    source: "shot",
    filename: shot.mime === "image/jpeg" ? "shot.jpg" : "shot.png",
    maxImageBytes: opts.maxBytes,
    label: "screenshot",
    ...(opts.imageMimeTypes ? { imageMimeTypes: opts.imageMimeTypes } : {}),
    ...(opts.newId ? { newId: opts.newId } : {}),
    ...(opts.inspect ? { inspect: opts.inspect } : {}),
  })
}

type Run = (
  argv: readonly string[],
  timeoutMs: number,
  maxStdoutBytes: number,
  countOverflowBytes: number,
) => Promise<CappedRunResult>

function runner(opts: ScreenshotOptions): Run {
  const spawn = opts.spawn ?? bunSpawn
  const clock = opts.clock ?? new SystemClock()
  return async (argv, timeoutMs, maxStdoutBytes, countOverflowBytes) => {
    try {
      return await runCapped(spawn, argv, {
        env: opts.env,
        clock,
        timeoutMs,
        maxStdoutBytes,
        countOverflowBytes,
        ...(opts.signal ? { signal: opts.signal } : {}),
      })
    } catch (error) {
      const tool = argv[0] ?? "the helper"
      if (isMissingExecutable(error)) throw new ImageInputError("unavailable", `/shot needs ${tool} (not installed)`)
      throw new ImageInputError("failed", `${tool} could not start`)
    }
  }
}

async function selectGeometry(run: Run, full: boolean): Promise<string | null> {
  const result = await run(slurpArgv(full), SLURP_TIMEOUT_MS, SLURP_MAX_STDOUT_BYTES, 0)
  if (result.aborted) return null
  if (result.timedOut) throw new ImageInputError("timeout", `screenshot selection timed out after ${SLURP_TIMEOUT_MS / 1000} s`)
  if (result.status !== 0) return null
  const line = new TextDecoder().decode(result.stdout).split("\n")[0]?.trim() ?? ""
  if (result.overflow || !SLURP_GEOMETRY.test(line)) {
    throw new ImageInputError("failed", "slurp returned an unexpected selection")
  }
  return line
}

async function capture(run: Run, geometry: string, jpeg: boolean, maxBytes: number, countBeyond: number): Promise<CappedRunResult> {
  const result = await run(grimArgv(geometry, jpeg), GRIM_TIMEOUT_MS, maxBytes, countBeyond)
  if (result.aborted || result.overflow) return result
  if (result.timedOut) throw new ImageInputError("timeout", `grim timed out after ${GRIM_TIMEOUT_MS / 1000} s`)
  if (result.status !== 0) throw new ImageInputError("failed", `grim failed (exit ${result.status}${firstLine(result.stderr)})`)
  if (result.stdout.byteLength === 0) throw new ImageInputError("failed", "grim returned no image")
  return result
}

function captured(result: CappedRunResult, geometry: string, mime: "image/png" | "image/jpeg"): ScreenshotCapture {
  if (sniffImageMime(result.stdout) !== mime) {
    throw new ImageInputError("failed", `grim did not return a ${mime === "image/png" ? "PNG" : "JPEG"} image`)
  }
  return { kind: "captured", bytes: result.stdout, mime, geometry, jpegFallback: mime === "image/jpeg" }
}

function firstLine(stderr: string): string {
  const line = sanitizeLine(stderr.split("\n").find((l) => l.trim().length > 0) ?? "").trim()
  if (line.length === 0) return ""
  return `: ${line.length > 120 ? `${line.slice(0, 117)}...` : line}`
}
