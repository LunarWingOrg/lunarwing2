/**
 * Local slash commands and the send-path guards (design 3.10.1-3.10.3, R17, 11.3.2), through the real session
 * controller and a FakeTransport that records every request.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { parseSlash, unescapeSlash, parseVoiceIds } from "../../src/controller/commands.ts"
import { FakeTransport, turnId } from "../mocks/fake-transport.ts"
import { renderApp, startRuntime, type AppHarness, type RuntimeHarness } from "../helpers/render.tsx"

let harness: RuntimeHarness | null = null
let app: AppHarness | null = null

afterEach(async () => {
  await harness?.dispose()
  await app?.dispose()
  harness = null
  app = null
})

function requests(transport: FakeTransport): string[] {
  return transport.log.filter((method) => method !== "connect" && method !== "resync" && method !== "close")
}

function notices(h: RuntimeHarness | AppHarness): string[] {
  return h.store.getState().notices.map((notice) => notice.text)
}

const approval = {
  id: "req-1",
  threadId: null,
  toolName: "shell",
  description: "Run a command",
  parameters: '{"command":"ls"}',
  allowAlways: true,
  truncated: false,
  source: "live" as const,
}

describe("parseSlash", () => {
  test("first token lower-cased, rest trimmed; // is not a command", () => {
    expect(parseSlash("  /VOICE use  hex-app ")).toEqual({ name: "voice", args: "use  hex-app", raw: "/VOICE use  hex-app" })
    expect(parseSlash("//help")).toBeNull()
    expect(parseSlash("hello /quit")).toBeNull()
    expect(unescapeSlash(" //help me ")).toBe("/help me")
    expect(parseVoiceIds("lunarvoice, hex-app")).toEqual(["lunarvoice", "hex-app"])
    expect(parseVoiceIds("lunarvoice nope")).toBeNull()
  })
})

describe("never sent", () => {
  test("/quit and /exit quit without sending anything", async () => {
    const transport = new FakeTransport()
    harness = await startRuntime({ transport })
    await harness.run("/quit")
    expect(harness.quits).toBe(1)
    expect(requests(transport)).toEqual([])
    const other = new FakeTransport()
    await harness.dispose()
    harness = await startRuntime({ transport: other })
    await harness.run("/EXIT now")
    expect(harness.quits).toBe(1)
    expect(requests(other)).toEqual([])
  })

  test("/shutdown is refused and never sent", async () => {
    const transport = new FakeTransport()
    harness = await startRuntime({ transport })
    await harness.run("/shutdown")
    expect(requests(transport)).toEqual([])
    expect(notices(harness).join(" ")).toContain("stop-tenant")
  })

  test("//quit and pasted /quit text never reach the wire", async () => {
    const transport = new FakeTransport()
    harness = await startRuntime({ transport })
    await harness.run("//quit")
    await harness.run("  //Exit ")
    expect(requests(transport)).toEqual([])
    expect(notices(harness).join(" ")).toContain("never sent")
  })

  test("/restart is refused; //restart needs a second Enter within 5 s", async () => {
    const transport = new FakeTransport()
    harness = await startRuntime({ transport })
    await harness.run("/restart")
    expect(requests(transport)).toEqual([])
    expect(notices(harness).join(" ")).toContain("restart-tenant derpy")

    await harness.run("//restart")
    expect(transport.sentTexts).toEqual([])
    await harness.advance(6_000)
    await harness.enter()
    expect(transport.sentTexts).toEqual([])
    await harness.enter()
    expect(transport.sentTexts).toEqual(["/restart"])
  })
})

describe("forwarded", () => {
  test("//help forwards /help", async () => {
    const transport = new FakeTransport()
    harness = await startRuntime({ transport })
    await harness.run("//help")
    expect(transport.sentTexts).toEqual(["/help"])
    expect(harness.store.getState().panel).toBeNull()
  })

  test("/help opens the help panel locally", async () => {
    const transport = new FakeTransport()
    harness = await startRuntime({ transport })
    await harness.run("/help")
    expect(requests(transport)).toEqual([])
    expect(harness.store.getState().panel?.kind).toBe("help")
  })

  test("an unknown /cmd is forwarded verbatim", async () => {
    const transport = new FakeTransport()
    harness = await startRuntime({ transport })
    await harness.run("/compact")
    await harness.run("/model Kimi-K3")
    expect(transport.sentTexts).toEqual(["/compact", "/model Kimi-K3"])
  })

  test("/clear and /new are recorded as controls", async () => {
    const transport = new FakeTransport()
    harness = await startRuntime({ transport })
    await harness.run("/clear")
    await harness.run("/new")
    const state = harness.store.getState()
    const kinds = state.order.map((id) => state.turns[id]?.control?.kind)
    expect(kinds).toEqual(["clear", "new"])
    expect(transport.sentTexts).toEqual(["/clear", "/new"])
  })

  test("/interrupt and /steer get their own control records", async () => {
    const transport = new FakeTransport()
    harness = await startRuntime({ transport })
    await harness.run("/stop")
    await harness.run("/steer use staging")
    expect(transport.callsOf("interrupt")).toHaveLength(1)
    expect(transport.callsOf("steer").map((call) => call.req.text)).toEqual(["use staging"])
    await harness.run("/steer")
    expect(transport.callsOf("steer")).toHaveLength(1)
    expect(harness.runtime.composer.getText()).toBe("/steer")
  })
})

describe("approval keywords", () => {
  test("/approve is structured on the gateway", async () => {
    const transport = new FakeTransport()
    harness = await startRuntime({ transport })
    await harness.emit({ kind: "turn.approval", turnId: turnId("t-0"), approval })
    await harness.advance(1_000)
    await harness.run("/approve")
    expect(transport.callsOf("approve").map((call) => [call.req.approvalId, call.req.decision])).toEqual([["req-1", "approve"]])
    expect(transport.sentTexts).toEqual([])
  })

  test("/approve sends `yes` as text where the daemon has no approval prompts (legacy socket)", async () => {
    const transport = new FakeTransport({ capabilities: { approvals: false } })
    harness = await startRuntime({ transport })
    await harness.run("/approve")
    await harness.run("/deny")
    expect(transport.sentTexts).toEqual(["yes", "no"])
    expect(transport.callsOf("approve")).toHaveLength(0)
  })

  test("the keyword guard needs a second Enter when nothing is pending", async () => {
    const transport = new FakeTransport()
    harness = await startRuntime({ transport })
    await harness.run("ok")
    expect(transport.sentTexts).toEqual([])
    expect(notices(harness).join(" ")).toContain("approval keyword")
    await harness.enter()
    expect(transport.sentTexts).toEqual(["ok"])
  })
})

describe("credential mode", () => {
  test("Ctrl+S in credential mode never sends a message or an interrupt", async () => {
    const transport = new FakeTransport()
    app = await renderApp({ transport })
    await app.type("draft text")
    await app.emit({ kind: "turn.auth_required", turnId: null, extensionName: "github" })
    await app.type("ghp_secret")
    await app.press("s", { ctrl: true })
    expect(transport.callsOf("send")).toHaveLength(0)
    expect(transport.callsOf("interrupt")).toHaveLength(0)
    expect(transport.callsOf("authToken").map((call) => [call.req.extensionName, call.tokenLength])).toEqual([["github", 10]])
    expect(app.runtime.composer.getText()).toBe("draft text")
  })
})
