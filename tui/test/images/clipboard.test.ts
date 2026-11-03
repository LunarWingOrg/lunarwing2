/**
 * Ctrl+V (design 6.1, C7, 11.3.2): host clipboard image; the limit-exceeded message; host failure falls back to
 * wl-paste (injected spawn); wl-paste output over the cap kills the child; text/uri-list yields files; no
 * spawnSync anywhere under src/ (source grep). Nothing touches the real clipboard: the host is a
 * FakeHostClipboard and every child a FakeSpawn script.
 */
import { describe, expect, test } from "bun:test"
import { readFile } from "node:fs/promises"
import { join } from "node:path"
import { ManualClock } from "@opentui/core/testing"
import { buildChildEnv } from "../../src/config/child-env.ts"
import type { SpawnFn } from "../../src/config/types.ts"
import {
  CLIPBOARD_IMAGE_TYPES,
  createClipboardReader,
  pickClipboardType,
  runCapped,
  type ClipboardReaderOptions,
} from "../../src/images/clipboard.ts"
import { createFakeSpawn, type FakeSpawn } from "../helpers/fake-spawn.ts"
import { FakeHostClipboard, readResult } from "../mocks/fake-clipboard.ts"

const MIB = 1024 * 1024
const PNG = new Uint8Array(
  Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR4nGM8IWfDwMDAxMDAwMDAAAAOTgEmR2hJIgAAAABJRU5ErkJggg==", "base64"),
)
const WAYLAND_ENV = buildChildEnv({
  PATH: "/usr/bin:/bin",
  HOME: "/home/sun",
  WAYLAND_DISPLAY: "wayland-1",
  XDG_RUNTIME_DIR: "/run/user/1000",
  LUNARWING_GATEWAY_TOKEN: "do-not-leak-this-token-0123456789",
  HTTP_PROXY: "http://proxy.invalid:3128",
})
const X11_ENV = buildChildEnv({ PATH: "/usr/bin:/bin", DISPLAY: ":0" })
const NO_SESSION_ENV = buildChildEnv({ PATH: "/usr/bin:/bin" })

interface Rig {
  host: FakeHostClipboard
  fake: FakeSpawn
  clock: ManualClock
  options: ClipboardReaderOptions
}

function rig(overrides: Partial<ClipboardReaderOptions> = {}): Rig {
  const clock = new ManualClock()
  const host = new FakeHostClipboard()
  const fake = createFakeSpawn({ clock })
  const options: ClipboardReaderOptions = {
    maxImageBytes: 5 * MIB,
    env: WAYLAND_ENV,
    clock,
    spawn: fake.spawn,
    createHost: host.factory,
    ...overrides,
  }
  return { host, fake, clock, options }
}

const signal = (): AbortSignal => new AbortController().signal
const failed = { status: "failed", error: new Error("no data-control protocol") } as const

describe("host clipboard (OpenTUI)", () => {
  test("host clipboard image", async () => {
    const { host, fake, options } = rig()
    host.holdImage(PNG, "image/png")
    const reader = createClipboardReader(options)
    const paste = await reader.read(signal())
    expect(paste).toEqual({ kind: "image", bytes: PNG, declaredMime: "image/png", via: "host" })
    expect(host.created).toEqual([{ timeoutMs: 1500, maxReadBytes: 5 * MIB, maxImagePixels: 40_000_000 }])
    expect(host.reads[0]?.preferredTypes).toEqual([...CLIPBOARD_IMAGE_TYPES])
    expect(host.reads[0]?.selection).toBe("clipboard")
    expect(fake.calls).toHaveLength(0)
    // Created once, shared with copy (3.12.4).
    await reader.read(signal())
    expect(reader.hostClipboard()).toBe(host)
    expect(host.created).toHaveLength(1)
    await reader.dispose()
    expect(host.disposed).toBe(true)
  })

  test("limit-exceeded message", async () => {
    const { host, options } = rig()
    host.onRead("image/png", { status: "limit-exceeded" })
    const reader = createClipboardReader(options)
    expect(await reader.read(signal())).toEqual({ kind: "error", reason: "clipboard image larger than 5 MiB" })
  })

  test("a lower current limit applies to an existing host", async () => {
    let limit = 5 * MIB
    const { host, options } = rig({ maxImageBytes: () => limit })
    host.holdImage(PNG)
    const reader = createClipboardReader(options)
    expect((await reader.read(signal())).kind).toBe("image")
    limit = PNG.length - 1
    expect(await reader.read(signal())).toEqual({ kind: "error", reason: `clipboard image larger than ${PNG.length - 1} B` })
  })

  test("text/uri-list yields files", async () => {
    const { host, options } = rig()
    host.holdUriList("file:///home/sun/Pictures/cat%20one.png", "file:///home/sun/Pictures/dog.jpg")
    const paste = await createClipboardReader(options).read(signal())
    expect(paste).toEqual({
      kind: "files",
      paths: ["/home/sun/Pictures/cat one.png", "/home/sun/Pictures/dog.jpg"],
      text: "/home/sun/Pictures/cat one.png\n/home/sun/Pictures/dog.jpg",
    })
    expect(host.readTypes()).toEqual(["image/png", "text/uri-list"])
  })

  test("text is sanitized but keeps its line breaks; nothing is empty", async () => {
    const { host, options } = rig()
    host.holdText("line one\r\n\u001b]0;evil title\u0007line two‮")
    const reader = createClipboardReader(options)
    expect(await reader.read(signal())).toEqual({ kind: "text", text: "line one\nline two" })
    expect(host.readTypes()).toEqual(["image/png", "text/uri-list", "text/plain"])
    expect(await createClipboardReader(rig().options).read(signal())).toEqual({ kind: "empty" })
  })

  test("an aborted read is cancelled", async () => {
    const { options } = rig()
    const controller = new AbortController()
    controller.abort()
    expect(await createClipboardReader(options).read(controller.signal)).toEqual({ kind: "cancelled" })
  })
})

describe("wl-paste and xclip fallback", () => {
  test("host failure falls back to wl-paste (injected spawn)", async () => {
    const { host, fake, options } = rig()
    host.onAnyRead(failed)
    fake.once(["wl-paste", "--list-types"], { stdout: ["text/plain;charset=utf-8\nimage/jpeg\nimage/png\n"] })
    fake.once(["wl-paste", "--no-newline", "--type", "image/png"], { stdout: [PNG.slice(0, 30), PNG.slice(30)] })
    const paste = await createClipboardReader(options).read(signal())
    expect(paste).toEqual({ kind: "image", bytes: PNG, declaredMime: "image/png", via: "wl-paste" })
    expect(fake.calls.map((call) => call.argv)).toEqual([
      ["wl-paste", "--list-types"],
      ["wl-paste", "--no-newline", "--type", "image/png"],
    ])
    for (const call of fake.calls) {
      expect(call.env).toEqual(WAYLAND_ENV)
      expect(call.env.LUNARWING_GATEWAY_TOKEN).toBeUndefined()
      expect(call.env.HTTP_PROXY).toBeUndefined()
      expect(call.options.stdin).toBe("ignore")
    }
  })

  test("a host timeout falls back too; a forced wl-paste backend never creates the host", async () => {
    const timedOut = rig()
    timedOut.host.onAnyRead({ status: "timed-out" })
    timedOut.fake.once(["wl-paste", "--list-types"], { stdout: ["text/plain\n"] })
    timedOut.fake.once(["wl-paste", "--no-newline", "--type", "text/plain"], { stdout: ["pasted"] })
    expect(await createClipboardReader(timedOut.options).read(signal())).toEqual({ kind: "text", text: "pasted" })

    const forced = rig({ backend: "wl-paste" })
    forced.fake.once(["wl-paste", "--list-types"], { stdout: ["text/uri-list\n"] })
    forced.fake.once(["wl-paste", "--no-newline", "--type", "text/uri-list"], { stdout: ["file:///tmp/a.png\n"] })
    expect(await createClipboardReader(forced.options).read(signal())).toMatchObject({ kind: "files", paths: ["/tmp/a.png"] })
    expect(forced.host.created).toHaveLength(0)
  })

  test("wl-paste output over the cap kills the child", async () => {
    const { host, fake, options } = rig({ maxImageBytes: 1000 })
    host.onAnyRead(failed)
    fake.once(["wl-paste", "--list-types"], { stdout: ["image/png\n"] })
    fake.once(["wl-paste", "--no-newline", "--type", "image/png"], {
      stdout: [new Uint8Array(600), new Uint8Array(600)],
      stayAlive: true,
    })
    const paste = await createClipboardReader(options).read(signal())
    expect(paste).toEqual({ kind: "error", reason: "clipboard image larger than 1000 B" })
    const child = fake.calls[1]?.process
    expect(child?.kills).toContain("SIGKILL")
    expect(fake.running()).toHaveLength(0)
  })

  test("an empty clipboard and a missing wl-paste", async () => {
    const empty = rig({ backend: "wl-paste" })
    empty.fake.once(["wl-paste", "--list-types"], { stderr: ["Nothing is copied\n"], exitCode: 1 })
    expect(await createClipboardReader(empty.options).read(signal())).toEqual({ kind: "empty" })

    const missing = rig()
    missing.host.onAnyRead(failed)
    const paste = await createClipboardReader(missing.options).read(signal())
    expect(paste.kind).toBe("error")
    expect(paste.kind === "error" ? paste.reason : "").toBe(
      "clipboard unavailable: host clipboard failed (no data-control protocol); wl-paste is not installed; use /img <path> or /shot",
    )
  })

  test("wl-paste is killed after its timeout", async () => {
    const { host, fake, clock, options } = rig()
    host.onAnyRead(failed)
    fake.once(["wl-paste", "--list-types"], { stayAlive: true })
    const pending = createClipboardReader(options).read(signal())
    for (let i = 0; i < 100 && fake.calls.length === 0; i += 1) await Promise.resolve()
    expect(fake.calls).toHaveLength(1)
    clock.advance(1499)
    expect(fake.running()).toHaveLength(1)
    clock.advance(1)
    const paste = await pending
    expect(paste.kind === "error" ? paste.reason : paste.kind).toContain("wl-paste timed out after 1.5 s")
    expect(fake.calls[0]?.process?.kills).toEqual(["SIGKILL"])
  })

  test("xclip on X11, and no session at all", async () => {
    const x11 = rig({ env: X11_ENV })
    x11.host.onAnyRead(failed)
    x11.fake.once(["xclip", "-selection", "clipboard", "-t", "TARGETS", "-o"], { stdout: ["TARGETS\nUTF8_STRING\nSTRING\n"] })
    x11.fake.once(["xclip", "-selection", "clipboard", "-t", "UTF8_STRING", "-o"], { stdout: ["from x11"] })
    expect(await createClipboardReader(x11.options).read(signal())).toEqual({ kind: "text", text: "from x11" })

    const none = rig({ env: NO_SESSION_ENV })
    none.host.onAnyRead(failed)
    const paste = await createClipboardReader(none.options).read(signal())
    expect(paste.kind === "error" ? paste.reason : "").toContain("no Wayland or X11 session")
    expect(none.fake.calls).toHaveLength(0)
  })

  test("a host-only backend never spawns", async () => {
    const { host, fake, options } = rig({ backend: "host" })
    host.onAnyRead(failed)
    expect(await createClipboardReader(options).read(signal())).toEqual({
      kind: "error",
      reason: "clipboard read failed (no data-control protocol)",
    })
    expect(fake.calls).toHaveLength(0)
  })

  test("type preference: images first, then text/uri-list, then text/plain", () => {
    expect(pickClipboardType(["text/plain", "image/gif", "image/webp"])).toEqual({ kind: "image", type: "image/webp" })
    expect(pickClipboardType(["text/plain", "text/uri-list"])).toEqual({ kind: "files", type: "text/uri-list" })
    expect(pickClipboardType(["text/plain", "TEXT/PLAIN;CHARSET=UTF-8"])).toEqual({ kind: "text", type: "TEXT/PLAIN;CHARSET=UTF-8" })
    expect(pickClipboardType(["image/bmp", "application/x-thing"])).toBeNull()
  })
})

describe("runCapped", () => {
  test("counts overflow without keeping it, then kills", async () => {
    const clock = new ManualClock()
    const fake = createFakeSpawn({ clock })
    fake.on("grim", { stdout: [new Uint8Array(10), new Uint8Array(10), new Uint8Array(10)], stayAlive: true })
    const result = await runCapped(fake.spawn, ["grim", "-"], { env: WAYLAND_ENV, clock, timeoutMs: 1000, maxStdoutBytes: 12, countOverflowBytes: 15 })
    expect(result.stdout.byteLength).toBe(12)
    expect(result.stdoutBytes).toBe(30)
    expect(result.overflow).toBe(true)
    expect(result.overflowKilled).toBe(true)
    expect(result.status).toBe(137)
  })

  test("a timeout ends the run even when a grandchild keeps the pipes open", async () => {
    const clock = new ManualClock()
    let resolveExit: (code: number) => void = () => undefined
    const exited = new Promise<number>((resolve) => {
      resolveExit = resolve
    })
    const kills: unknown[] = []
    // Pipes that never close: the killed child's descendant still holds them.
    const open = (): ReadableStream<Uint8Array> => new ReadableStream<Uint8Array>({ start: () => undefined })
    const spawn: SpawnFn = () => ({
      pid: 1,
      stdin: undefined,
      stdout: open(),
      stderr: open(),
      exited,
      exitCode: null,
      signalCode: null,
      killed: false,
      kill: (signal) => {
        kills.push(signal)
        resolveExit(137)
      },
    })
    const pending = runCapped(spawn, ["wl-paste", "--list-types"], { env: WAYLAND_ENV, clock, timeoutMs: 1500, maxStdoutBytes: 64 })
    clock.advance(1500)
    const result = await pending
    expect(result).toMatchObject({ timedOut: true, status: 137, stdoutBytes: 0, aborted: false })
    expect(kills).toEqual(["SIGKILL"])
  })

  test("readResult helper builds host answers", () => {
    expect(readResult("x", "text/plain")).toEqual({ status: "read", representation: { mimeType: "text/plain", bytes: new TextEncoder().encode("x") } })
  })
})

describe("C7: no synchronous child processes", () => {
  test("no spawnSync anywhere under src/ (source grep)", async () => {
    const srcDir = join(import.meta.dir, "..", "..", "src")
    const offenders: string[] = []
    for await (const file of new Bun.Glob("**/*.{ts,tsx,js,mjs}").scan({ cwd: srcDir })) {
      const source = await readFile(join(srcDir, file), "utf8")
      // Comments may name the forbidden calls (as this rule's own documentation does); code may not.
      const code = source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:])\/\/.*$/gm, "$1")
      code.split("\n").forEach((line, index) => {
        if (/\b(?:spawnSync|execSync|execFileSync)\b/.test(line)) offenders.push(`src/${file}:${index + 1}: ${line.trim()}`)
      })
    }
    expect(offenders).toEqual([])
  })
})
