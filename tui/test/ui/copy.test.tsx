/**
 * Copy (design 3.12.4, UX-11, 11.3.4): a drag over transcript text writes it through the shared host clipboard;
 * chrome is not selectable; /copy copies the newest reply; an unsupported host clipboard falls back to wl-copy
 * with the text on stdin (FakeSpawn, never the real binary).
 */
import { afterEach, describe, expect, test } from "bun:test"
import { createFakeSpawn } from "../helpers/fake-spawn.ts"
import { locate } from "../helpers/frames.ts"
import { renderApp, type AppHarness } from "../helpers/render.tsx"
import { FakeClipboardReader, FakeHostClipboard } from "../mocks/fake-clipboard.ts"
import { FakeTransport, turnId } from "../mocks/fake-transport.ts"

let app: AppHarness | null = null

afterEach(async () => {
  await app?.dispose()
  app = null
})

async function withReply(host: FakeHostClipboard, opts: { spawn?: ReturnType<typeof createFakeSpawn>; wayland?: boolean } = {}): Promise<AppHarness> {
  const transport = new FakeTransport()
  const harness = await renderApp({
    transport,
    clipboard: new FakeClipboardReader(host),
    width: 80,
    height: 20,
    ...(opts.spawn === undefined ? {} : { spawn: opts.spawn.spawn }),
    env: opts.wayland === true ? { PATH: "/usr/bin", WAYLAND_DISPLAY: "wayland-1" } : { PATH: "/usr/bin" },
  })
  await harness.type("question")
  await harness.enter()
  const id = turnId("t-1")
  await harness.emit({ kind: "turn.final", turnId: id, text: "copy this exact answer" }, { kind: "turn.finished", turnId: id, outcome: "replied" })
  app = harness
  return harness
}

describe("copy", () => {
  test("a drag over transcript text writes it to the host clipboard", async () => {
    const host = new FakeHostClipboard()
    const harness = await withReply(host)
    const frame = await harness.render()
    const at = locate(frame, "copy this exact answer")
    expect(at).not.toBeNull()
    const x = at?.x ?? 0
    const y = at?.y ?? 0
    await harness.setup.mockMouse.drag(x, y, x + "copy this exact answer".length - 1, y)
    await harness.settle()
    expect(host.writes.map((write) => [write.text, write.selection])).toEqual([["copy this exact answer", "clipboard"]])
    expect(harness.store.getState().notices.at(-1)?.text).toBe("copied 22 characters (cliphist records copies)")
  })

  test("chrome is not selectable: a drag over the header or footer copies nothing", async () => {
    const host = new FakeHostClipboard()
    const harness = await withReply(host)
    const frame = await harness.render()
    await harness.setup.mockMouse.drag(0, 0, 30, 0)
    const footer = frame.split("\n").length - 2
    await harness.setup.mockMouse.drag(0, footer, 30, footer)
    await harness.settle()
    expect(host.writes).toEqual([])
  })

  test("/copy copies the newest reply", async () => {
    const host = new FakeHostClipboard()
    const harness = await withReply(host)
    await harness.type("/copy")
    await harness.enter()
    expect(host.writes.map((write) => write.text)).toEqual(["copy this exact answer"])
    await harness.type("/copy 2")
    await harness.enter()
    expect(harness.store.getState().notices.at(-1)?.text).toBe("there is no reply number 2")
  })

  test("an unsupported host clipboard falls back to wl-copy with the text on stdin", async () => {
    const host = new FakeHostClipboard()
    host.writeResult = { status: "unsupported" }
    const fake = createFakeSpawn()
    fake.on("wl-copy", { exitCode: 0 })
    const harness = await withReply(host, { spawn: fake, wayland: true })
    await harness.type("/copy")
    await harness.enter()
    const calls = fake.callsFor("wl-copy")
    expect(calls.map((call) => call.argv)).toEqual([["wl-copy"]])
    expect(calls[0]?.process?.stdinText()).toBe("copy this exact answer")
    expect(calls[0]?.env).toEqual({ PATH: "/usr/bin", WAYLAND_DISPLAY: "wayland-1" })
    expect(harness.store.getState().notices.at(-1)?.text).toContain("via wl-copy")
  })
})
