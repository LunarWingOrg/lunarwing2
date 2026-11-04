/**
 * Image gestures through the app (design 6.1, 6.4, 5.6 SEC-09, 6.5 UX-05): Ctrl+V attaches a clipboard image or
 * inserts clipboard text; a drop of image paths attaches them (a failed one inserts the original text); /shot
 * attaches the screenshot; Ctrl+R and Ctrl+X remove attachments; Enter sends the sniffed images; /image save goes
 * to the exporter. The first image from each source shows the storage notice once per profile.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { ManualClock } from "@opentui/core/testing"
import { createPendingImage, imageStorageNotice } from "../../src/images/attach.ts"
import type { ExportableImage, ImageExporter, SaveRequest } from "../../src/images/export.ts"
import { ImageInputError } from "../../src/images/limits.ts"
import type { PendingImage } from "../../src/state/model.ts"
import { renderApp, type AppHarness } from "../helpers/render.tsx"
import { FakeClipboardReader } from "../mocks/fake-clipboard.ts"
import { FakeTransport, turnId } from "../mocks/fake-transport.ts"
import { MemoryStateFile, TINY_PNG } from "../mocks/mock-gateway.ts"

let app: AppHarness | null = null

afterEach(async () => {
  await app?.dispose()
  app = null
})

function pending(source: PendingImage["source"], filename: string | null): PendingImage {
  return createPendingImage(TINY_PNG, { source, filename, maxImageBytes: 5 * 1024 * 1024 })
}

function notices(harness: AppHarness): string[] {
  return harness.store.getState().notices.map((notice) => notice.text)
}

class RecordingExporter implements ImageExporter {
  readonly saves: { image: ExportableImage; request: SaveRequest }[] = []
  async open(): Promise<string> {
    return "/run/user/1000/wingtip/img-1.png"
  }
  async save(image: ExportableImage, request: SaveRequest): Promise<string> {
    this.saves.push({ image, request })
    return "./lunarwing-derpy-1.png"
  }
  exported(): readonly string[] {
    return []
  }
  async dispose(): Promise<void> {}
}

describe("image input", () => {
  test("Ctrl+V attaches a clipboard image; the storage notice shows once; Ctrl+R and Ctrl+X remove", async () => {
    const clipboard = new FakeClipboardReader()
    const image = { kind: "image" as const, bytes: TINY_PNG, declaredMime: "image/gif", via: "host" as const }
    clipboard.push(image, image, image)
    app = await renderApp({ clipboard, stateFile: new MemoryStateFile(), width: 100, height: 30 })
    await app.press("v", { ctrl: true })
    expect(app.store.getState().composer.pendingImages.map((img) => [img.mime, img.source])).toEqual([["image/png", "clipboard"]])
    expect(notices(app)).toContain(imageStorageNotice("derpy"))
    await app.press("v", { ctrl: true })
    await app.press("v", { ctrl: true })
    expect(app.store.getState().composer.pendingImages).toHaveLength(3)
    expect(notices(app).filter((text) => text === imageStorageNotice("derpy"))).toHaveLength(1)
    const frame = await app.render()
    expect(frame).toContain("Attachments (3)")
    expect(frame).toContain("stored in derpy's workspace")

    await app.press("r", { ctrl: true })
    expect(app.store.getState().composer.pendingImages).toHaveLength(2)
    await app.press("x", { ctrl: true })
    expect(app.store.getState().composer.pendingImages).toHaveLength(0)
  })

  test("Ctrl+V with text inserts it at the cursor; an empty clipboard says so", async () => {
    const clipboard = new FakeClipboardReader()
    clipboard.push({ kind: "text", text: "pasted words" })
    app = await renderApp({ clipboard })
    await app.type("see: ")
    await app.press("v", { ctrl: true })
    expect(app.runtime.composer.getText()).toBe("see: pasted words")
    await app.press("v", { ctrl: true })
    expect(notices(app)).toContain("the clipboard is empty")
  })

  test("a drop of image paths attaches them; a failed one inserts the original text", async () => {
    const read = async (path: string, opts: { source?: PendingImage["source"] }): Promise<PendingImage> => {
      if (path.includes("broken")) throw new ImageInputError("unsupported", "broken.png: unsupported image type")
      return pending(opts.source ?? "file", path)
    }
    app = await renderApp({ readFile: read })
    await app.paste("/home/sun/Pictures/cat.png")
    expect(app.store.getState().composer.pendingImages.map((img) => img.source)).toEqual(["drop"])
    expect(app.runtime.composer.getText()).toBe("")

    await app.paste("/home/sun/Pictures/broken.png")
    expect(notices(app)).toContain("broken.png: unsupported image type")
    expect(app.runtime.composer.getText()).toBe("/home/sun/Pictures/broken.png")
    // Prose is never taken for a drop.
    await app.paste(" and some words")
    expect(app.runtime.composer.getText()).toBe("/home/sun/Pictures/broken.png and some words")
  })

  test("a kitty drop of a file whose name has an apostrophe attaches it; too many paths say why they were pasted", async () => {
    const reads: string[] = []
    const read = async (path: string, opts: { source?: PendingImage["source"] }): Promise<PendingImage> => {
      reads.push(path)
      return pending(opts.source ?? "file", path)
    }
    app = await renderApp({ readFile: read })
    await app.paste("/home/sun/Pictures/Chris's shot.png")
    expect(reads).toEqual(["/home/sun/Pictures/Chris's shot.png"])
    expect(app.store.getState().composer.pendingImages.map((img) => img.source)).toEqual(["drop"])
    expect(app.runtime.composer.getText()).toBe("")

    const six = Array.from({ length: 6 }, (_, index) => `/home/sun/Pictures/${index}.png`).join("\n")
    await app.paste(six)
    expect(reads).toHaveLength(1)
    expect(notices(app)).toContain("not attached: a drop takes up to 5 absolute image paths; the text was pasted")
  })

  test("/shot attaches the screenshot; /img reads a file", async () => {
    const shots: boolean[] = []
    app = await renderApp({
      screenshot: async (opts) => {
        shots.push(opts.full)
        return pending("shot", "screenshot.png")
      },
      readFile: async (_path, opts) => pending(opts.source ?? "file", "cat.png"),
    })
    await app.type("/shot full")
    await app.enter()
    await app.type("/img ~/cat.png")
    await app.enter()
    expect(shots).toEqual([true])
    expect(app.store.getState().composer.pendingImages.map((img) => img.source)).toEqual(["shot", "file"])
    expect(app.runtime.composer.getText()).toBe("")
  })

  test("Enter sends the pending images with their sniffed type, then the bar is empty", async () => {
    const transport = new FakeTransport()
    const clipboard = new FakeClipboardReader()
    clipboard.push({ kind: "image", bytes: TINY_PNG, declaredMime: "image/jpeg", via: "wl-paste" })
    app = await renderApp({ transport, clipboard })
    await app.press("v", { ctrl: true })
    await app.type("what is this")
    await app.enter()
    const sent = transport.callsOf("send")[0]?.req
    expect(sent?.text).toBe("what is this")
    expect(sent?.images.map((img) => [img.mime, img.bytes.byteLength])).toEqual([["image/png", TINY_PNG.byteLength]])
    expect(app.store.getState().composer.pendingImages).toHaveLength(0)
    expect(notices(app)).toContain("images go to the configured model; vision is not verified for derpy")
  })

  test("with a performance.now() Clock, export names and first-use stamps still use the wall clock", async () => {
    // Production's Clock (SystemClock) counts from process start: new Date(clock.now()) would be 1970.
    const clock = new ManualClock()
    clock.setTime(183)
    const wall = Date.UTC(2026, 10, 3, 9, 30, 5)
    const exporter = new RecordingExporter()
    const clipboard = new FakeClipboardReader()
    clipboard.push({ kind: "image" as const, bytes: TINY_PNG, declaredMime: "image/png", via: "host" as const })
    const stateFile = new MemoryStateFile()
    app = await renderApp({ clock, wallNow: () => wall, exporter, clipboard, stateFile })
    await app.press("v", { ctrl: true })
    expect((await stateFile.read()).imageAck).toEqual({ clipboard: "2026-11-03T09:30:05.000Z" })
    await app.emit({ kind: "turn.image", turnId: turnId("bg-1"), image: { mime: "image/png", bytes: TINY_PNG } })
    await app.type("/image save")
    await app.enter()
    expect(exporter.saves.map((save) => save.request.at.toISOString())).toEqual(["2026-11-03T09:30:05.000Z"])
  })

  test("/image save hands the newest generated image to the exporter", async () => {
    const exporter = new RecordingExporter()
    app = await renderApp({ exporter })
    await app.emit({ kind: "turn.image", turnId: turnId("bg-1"), image: { mime: "image/png", bytes: TINY_PNG } })
    await app.type("/image save")
    await app.enter()
    expect(exporter.saves.map((save) => [save.request.label, save.request.n, save.request.path])).toEqual([["derpy", 1, null]])
    expect(notices(app)).toContain("saved ./lunarwing-derpy-1.png")
  })
})
