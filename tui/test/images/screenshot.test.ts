/**
 * /shot (design 6.1, UX-08, 11.3.2): region uses slurp then grim -s 1 -g; full uses slurp -o and grim -s 1; an
 * oversize PNG is retried once as JPEG q85; the JPEG result attaches as image/jpeg; still too large reports the
 * size. slurp and grim are FakeSpawn scripts: no real capture ever runs.
 */
import { describe, expect, test } from "bun:test"
import { ManualClock } from "@opentui/core/testing"
import { buildChildEnv } from "../../src/config/child-env.ts"
import { ImageInputError } from "../../src/images/limits.ts"
import { captureScreenshot, grimArgv, slurpArgv, takeScreenshot, type TakeScreenshotOptions } from "../../src/images/screenshot.ts"
import { createFakeSpawn, type FakeSpawn } from "../helpers/fake-spawn.ts"

const MIB = 1024 * 1024
const b64 = (text: string): Uint8Array => new Uint8Array(Buffer.from(text, "base64"))
const PNG = b64("iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR4nGM8IWfDwMDAxMDAwMDAAAAOTgEmR2hJIgAAAABJRU5ErkJggg==")
const JPEG = b64(
  "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAUDBAQEAwUEBAQFBQUGBwwIBwcHBw8LCwkMEQ8SEhEPERETFhwXExQaFRERGCEYGh0dHx8fExciJCIeJBweHx7/2wBDAQUFBQcGBw4ICA4eFBEUHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh7/wAARCAACAAIDASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwDzmiiivJP6FP/Z",
)
const ENV = buildChildEnv({ PATH: "/usr/bin:/bin", WAYLAND_DISPLAY: "wayland-1", LUNARWING_GATEWAY_TOKEN: "do-not-leak-0123456789abcdef" })

/** n MiB of output in 1 MiB chunks. */
const mibChunks = (n: number): Uint8Array[] => Array.from({ length: n }, () => new Uint8Array(MIB))

function setup(overrides: Partial<TakeScreenshotOptions> = {}): { fake: FakeSpawn; options: TakeScreenshotOptions } {
  const clock = new ManualClock()
  const fake = createFakeSpawn({ clock })
  return { fake, options: { full: false, maxBytes: 5 * MIB, env: ENV, clock, spawn: fake.spawn, ...overrides } }
}

async function rejection(promise: Promise<unknown>): Promise<ImageInputError> {
  try {
    await promise
  } catch (error) {
    if (error instanceof ImageInputError) return error
    throw error
  }
  throw new Error("expected an ImageInputError")
}

describe("/shot", () => {
  test("region uses slurp then grim -s 1 -g", async () => {
    const { fake, options } = setup()
    fake.once(["slurp"], { stdout: ["10,20 300x200\n"] })
    fake.once(["grim"], { stdout: [PNG] })
    const image = await takeScreenshot({ ...options, newId: () => "img-shot" })
    expect(image).toMatchObject({ id: "img-shot", mime: "image/png", filename: "shot.png", source: "shot", width: 2, height: 2 })
    expect(fake.calls.map((call) => call.argv)).toEqual([["slurp"], ["grim", "-s", "1", "-g", "10,20 300x200", "-"]])
    for (const call of fake.calls) {
      expect(call.options.stdin).toBe("ignore")
      expect(call.env).toEqual(ENV)
      expect(call.env.LUNARWING_GATEWAY_TOKEN).toBeUndefined()
    }
  })

  test("full uses slurp -o and grim -s 1", async () => {
    const { fake, options } = setup({ full: true })
    fake.once(["slurp", "-o"], { stdout: ["-1920,0 6400x1440\n"] })
    fake.once(["grim"], { stdout: [PNG] })
    const shot = await captureScreenshot(options)
    expect(shot).toMatchObject({ kind: "captured", mime: "image/png", geometry: "-1920,0 6400x1440", jpegFallback: false })
    expect(fake.calls.map((call) => call.argv)).toEqual([["slurp", "-o"], ["grim", "-s", "1", "-g", "-1920,0 6400x1440", "-"]])
    expect(slurpArgv(true)).toEqual(["slurp", "-o"])
    expect(grimArgv("0,0 1x1", true)).toEqual(["grim", "-s", "1", "-t", "jpeg", "-q", "85", "-g", "0,0 1x1", "-"])
  })

  test("an oversize PNG is retried once as JPEG q85", async () => {
    const { fake, options } = setup()
    fake.once(["slurp"], { stdout: ["0,0 9600x2160\n"] })
    fake.once(["grim", "-s", "1", "-g"], { stdout: mibChunks(6), stayAlive: true })
    fake.once(["grim", "-s", "1", "-t", "jpeg"], { stdout: [JPEG] })
    const shot = await captureScreenshot(options)
    expect(shot).toMatchObject({ kind: "captured", mime: "image/jpeg", jpegFallback: true })
    expect(fake.calls.map((call) => call.argv.slice(0, 7))).toEqual([
      ["slurp"],
      ["grim", "-s", "1", "-g", "0,0 9600x2160", "-"],
      ["grim", "-s", "1", "-t", "jpeg", "-q", "85"],
    ])
    // The PNG run was killed at the cap rather than read to the end.
    expect(fake.calls[1]?.process?.kills).toEqual(["SIGKILL"])
    expect(fake.running()).toHaveLength(0)
  })

  test("the JPEG result attaches as image/jpeg", async () => {
    const { fake, options } = setup()
    fake.once(["slurp"], { stdout: ["0,0 9600x2160\n"] })
    fake.once(["grim", "-s", "1", "-g"], { stdout: mibChunks(6), stayAlive: true })
    fake.once(["grim", "-s", "1", "-t", "jpeg"], { stdout: [JPEG] })
    const image = await takeScreenshot(options)
    expect(image).toMatchObject({ mime: "image/jpeg", filename: "shot.jpg", source: "shot", width: 2, height: 2 })
    expect(image?.bytes.byteLength).toBe(JPEG.byteLength)
  })

  test("still too large reports the size", async () => {
    const { fake, options } = setup()
    fake.once(["slurp"], { stdout: ["0,0 9600x2160\n"] })
    fake.once(["grim", "-s", "1", "-g"], { stdout: mibChunks(7), stayAlive: true })
    fake.once(["grim", "-s", "1", "-t", "jpeg"], { stdout: mibChunks(6) })
    const error = await rejection(captureScreenshot(options))
    expect(error.kind).toBe("too_large")
    expect(error.message).toBe("screenshot is 6 MiB after JPEG; select a smaller region")
    expect(fake.calls).toHaveLength(3)
  })

  test("Esc in slurp is a quiet cancel; nothing is captured", async () => {
    const { fake, options } = setup()
    fake.once(["slurp"], { stderr: ["selection cancelled\n"], exitCode: 1 })
    expect(await takeScreenshot(options)).toBeNull()
    expect(fake.calls).toHaveLength(1)
  })

  test("aborting kills slurp and cancels", async () => {
    const controller = new AbortController()
    const { fake, options } = setup({ signal: controller.signal })
    fake.once(["slurp"], { stayAlive: true })
    const pending = captureScreenshot(options)
    for (let i = 0; i < 100 && fake.calls.length === 0; i += 1) await Promise.resolve()
    controller.abort()
    expect(await pending).toEqual({ kind: "cancelled" })
    expect(fake.calls[0]?.process?.kills).toEqual(["SIGKILL"])
  })

  test("refusals: no Wayland, missing tools, bad selections, wrong output type", async () => {
    const noWayland = setup({ env: buildChildEnv({ PATH: "/usr/bin" }) })
    expect((await rejection(captureScreenshot(noWayland.options))).message).toBe("/shot needs a Wayland session (WAYLAND_DISPLAY is not set)")
    expect(noWayland.fake.calls).toHaveLength(0)

    const noSlurp = setup()
    expect((await rejection(captureScreenshot(noSlurp.options))).message).toBe("/shot needs slurp (not installed)")

    const noGrim = setup()
    noGrim.fake.once(["slurp"], { stdout: ["1,2 3x4\n"] })
    expect((await rejection(captureScreenshot(noGrim.options))).message).toBe("/shot needs grim (not installed)")

    const garbage = setup()
    garbage.fake.once(["slurp"], { stdout: ["1,2 3x4; rm -rf ~\n"] })
    expect((await rejection(captureScreenshot(garbage.options))).message).toBe("slurp returned an unexpected selection")
    expect(garbage.fake.calls).toHaveLength(1)

    const notPng = setup()
    notPng.fake.once(["slurp"], { stdout: ["1,2 3x4\n"] })
    notPng.fake.once(["grim"], { stdout: [JPEG] })
    expect((await rejection(captureScreenshot(notPng.options))).message).toBe("grim did not return a PNG image")

    const failing = setup()
    failing.fake.once(["slurp"], { stdout: ["1,2 3x4\n"] })
    failing.fake.once(["grim"], { stderr: ["compositor doesn't support wlr-screencopy\n"], exitCode: 1 })
    expect((await rejection(captureScreenshot(failing.options))).message).toBe(
      "grim failed (exit 1: compositor doesn't support wlr-screencopy)",
    )
  })
})
