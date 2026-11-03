/**
 * Typed and dictated approval replies (design 3.10.3, 3.6.2, SEC-04, SEC-08, UX-07, 11.3.2): the pure matcher and
 * gate, and the confirmation flow through the real session controller with a FakeTransport.
 */
import { afterEach, describe, expect, test } from "bun:test"
import {
  approvalGate,
  isDaemonApprovalKeyword,
  matchApprovalReply,
  normalizeApprovalReply,
} from "../../src/controller/approval-reply.ts"
import type { ApprovalPrompt } from "../../src/state/model.ts"
import type { WireApproval } from "../../src/transport/types.ts"
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

const wire: WireApproval = {
  id: "req-1",
  threadId: null,
  toolName: "shell",
  description: "Run a command",
  parameters: '{"command":"ls"}',
  allowAlways: true,
  truncated: false,
  source: "live",
}

function prompt(patch: Partial<ApprovalPrompt> = {}): ApprovalPrompt {
  return { ...wire, turnId: null, state: "pending", createdAt: 1_000, focusedAt: 1_000, viewedFull: false, ...patch }
}

function lastNotice(h: RuntimeHarness | AppHarness): string {
  return h.store.getState().notices.at(-1)?.text ?? ""
}

async function withPrompt(transport = new FakeTransport()): Promise<RuntimeHarness> {
  const h = await startRuntime({ transport })
  await h.emit({ kind: "turn.approval", turnId: turnId("t-0"), approval: wire })
  await h.advance(1_000)
  return h
}

describe("matchApprovalReply", () => {
  test("normalizes dictation punctuation, case and width", () => {
    expect(matchApprovalReply("Yes.")).toBe("approve")
    expect(matchApprovalReply("  OKAY!  ")).toBe("approve")
    expect(matchApprovalReply("No!")).toBe("deny")
    expect(matchApprovalReply("“yes”")).toBeNull()
    expect(matchApprovalReply("\"yes\"")).toBe("approve")
    expect(matchApprovalReply("ＹＥＳ")).toBe("approve")
    expect(matchApprovalReply("yes   always")).toBe("always")
    expect(matchApprovalReply("a")).toBe("always")
    expect(matchApprovalReply("Yes, but use staging.")).toBeNull()
    expect(normalizeApprovalReply(" Yes, please. ")).toBe("yes, please")
  })

  test("the daemon's own rule is exact: 'ok' is a keyword, 'Yes.' is not", () => {
    expect(isDaemonApprovalKeyword(" OK ")).toBe(true)
    expect(isDaemonApprovalKeyword("/y")).toBe(true)
    expect(isDaemonApprovalKeyword("Yes.")).toBe(false)
    expect(isDaemonApprovalKeyword("okay")).toBe(false)
  })
})

describe("approvalGate", () => {
  test("freshness: focused for at least 1 s", () => {
    expect(approvalGate(prompt(), { startedAt: null }, 80, 1_500)).toBe("too_new")
    expect(approvalGate(prompt(), { startedAt: null }, 80, 2_000)).toBe("ok")
    expect(approvalGate(prompt({ focusedAt: null }), { startedAt: null }, 80, 9_000)).toBe("too_new")
  })

  test("typed text must start after the prompt arrived", () => {
    // The prompt (createdAt 1000) must exist before the first keystroke of the current text.
    expect(approvalGate(prompt(), { startedAt: 1_100 }, 80, 5_000, { via: "text" })).toBe("ok")
    expect(approvalGate(prompt(), { startedAt: 900 }, 80, 5_000, { via: "text" })).toBe("typed_before_prompt")
    expect(approvalGate(prompt(), { startedAt: 1_000 }, 80, 5_000, { via: "text" })).toBe("typed_before_prompt")
    expect(approvalGate(prompt(), { startedAt: 900 }, 80, 5_000, { via: "key" })).toBe("ok")
  })

  test("hidden content blocks approve until the full view, never deny", () => {
    const long = prompt({ parameters: Array.from({ length: 30 }, (_, i) => `line ${i}`).join("\n") })
    expect(approvalGate(long, { startedAt: null }, 80, 5_000)).toBe("hidden_unviewed")
    expect(approvalGate(long, { startedAt: null }, 80, 5_000, { decision: "deny" })).toBe("ok")
    expect(approvalGate({ ...long, viewedFull: true }, { startedAt: null }, 80, 5_000)).toBe("ok")
  })

  test("a truncated prompt can only be denied", () => {
    expect(approvalGate(prompt({ truncated: true }), { startedAt: null }, 80, 5_000)).toBe("truncated")
    expect(approvalGate(prompt({ truncated: true }), { startedAt: null }, 80, 5_000, { decision: "deny" })).toBe("ok")
  })
})

describe("typed and dictated replies", () => {
  test("'Yes.' with a pending prompt asks for confirmation and sends nothing", async () => {
    const transport = new FakeTransport()
    harness = await withPrompt(transport)
    await harness.run("Yes.")
    expect(transport.sentTexts).toEqual([])
    expect(transport.callsOf("approve")).toHaveLength(0)
    expect(harness.store.getState().composer.confirm).toMatchObject({ approvalId: "req-1", decision: "approve" })
  })

  test("a second Enter approves the focused prompt", async () => {
    const transport = new FakeTransport()
    harness = await withPrompt(transport)
    await harness.run("Yes.")
    await harness.enter()
    expect(transport.callsOf("approve").map((call) => call.req.decision)).toEqual(["approve"])
    expect(transport.sentTexts).toEqual([])
    expect(harness.runtime.composer.getText()).toBe("")
  })

  test("'No!' denies after confirmation", async () => {
    const transport = new FakeTransport()
    harness = await withPrompt(transport)
    await harness.run("No!")
    await harness.enter()
    expect(transport.callsOf("approve").map((call) => call.req.decision)).toEqual(["deny"])
  })

  test("'Okay.' approves after confirmation", async () => {
    const transport = new FakeTransport()
    harness = await withPrompt(transport)
    await harness.run("Okay.")
    await harness.enter()
    expect(transport.callsOf("approve").map((call) => call.req.decision)).toEqual(["approve"])
  })

  test("an edit cancels the confirmation", async () => {
    const transport = new FakeTransport()
    harness = await withPrompt(transport)
    await harness.run("Yes.")
    harness.runtime.composer.insertText(" wait")
    await harness.settle()
    expect(harness.store.getState().composer.confirm).toBeNull()
  })

  test("'Yes, but use staging.' is sent as text", async () => {
    const transport = new FakeTransport()
    harness = await withPrompt(transport)
    await harness.run("Yes, but use staging.")
    expect(transport.sentTexts).toEqual(["Yes, but use staging."])
    expect(transport.callsOf("approve")).toHaveLength(0)
  })

  test("bare 'a' with a pending prompt approves nothing and sends nothing", async () => {
    const transport = new FakeTransport()
    harness = await withPrompt(transport)
    await harness.run("a")
    await harness.enter()
    expect(transport.sentTexts).toEqual([])
    expect(transport.callsOf("approve")).toHaveLength(0)
    expect(lastNotice(harness)).toContain("Alt+A")
  })

  test("a prompt arriving while typing /approve is not approved", async () => {
    const transport = new FakeTransport()
    harness = await startRuntime({ transport })
    harness.runtime.composer.replaceText("/appro")
    await harness.settle()
    await harness.advance(100)
    await harness.emit({ kind: "turn.approval", turnId: turnId("t-0"), approval: wire })
    await harness.advance(1_500)
    harness.runtime.composer.insertText("ve")
    await harness.enter()
    expect(transport.callsOf("approve")).toHaveLength(0)
    expect(lastNotice(harness)).toContain("approval just arrived")
    // The text stays, so pressing Enter again after a fresh start works.
    expect(harness.runtime.composer.getText()).toBe("/approve")
  })

  test("no normalisation when nothing is pending: 'Yes.' is sent at once", async () => {
    const transport = new FakeTransport()
    harness = await startRuntime({ transport })
    await harness.run("Yes.")
    expect(transport.sentTexts).toEqual(["Yes."])
  })
})

describe("keys", () => {
  test("a prompt focused for under 1 s ignores Alt+Y", async () => {
    const transport = new FakeTransport()
    app = await renderApp({ transport })
    await app.emit({ kind: "turn.approval", turnId: turnId("t-0"), approval: wire })
    await app.advance(500)
    await app.press("y", { meta: true })
    expect(transport.callsOf("approve")).toHaveLength(0)
    expect(lastNotice(app)).toContain("approval just arrived")
    await app.advance(600)
    await app.press("y", { meta: true })
    expect(transport.callsOf("approve")).toHaveLength(1)
  })
})
