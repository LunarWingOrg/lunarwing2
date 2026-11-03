/**
 * Session against the mock gateway (design 11.3.3): the real GatewayTransport (loopback HTTP + SSE on an
 * ephemeral port), the real reducer and controllers (app.tsx createAppRuntime), a ManualClock for every timeout
 * and the listener-owner check reading injected /proc tables. Nothing touches a tenant.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { headerText } from "../../src/components/header.tsx"
import { buildPanel } from "../../src/controller/panel-content.ts"
import type { ThreadLister } from "../../src/controller/session.ts"
import { NOTES } from "../../src/state/correlate.ts"
import type { TurnId } from "../../src/state/model.ts"
import { startGatewayRuntime, type GatewayRig } from "../helpers/render.tsx"
import { echoReply, fixtureToken, type MockEvent, type MockSendContext } from "../mocks/mock-gateway.ts"

let rig: GatewayRig | null = null

afterEach(async () => {
  await rig?.dispose()
  rig = null
})

const T1 = "t-1" as TurnId
const T2 = "t-2" as TurnId

async function ready(opts: Parameters<typeof startGatewayRuntime>[0]): Promise<GatewayRig> {
  const r = await startGatewayRuntime(opts)
  rig = r
  await r.until(() => r.store.getState().conn.kind === "ready", "conn ready")
  return r
}

function routes(r: GatewayRig): string[] {
  return r.mock.requests.map((rec) => `${rec.method} ${rec.path}`)
}

function phase(r: GatewayRig, id: TurnId): string | undefined {
  return r.store.getState().turns[id]?.phase
}

/** A reply script that pauses the turn at an authentication gate for `github`. */
function authGateReply(ctx: MockSendContext): MockEvent[] {
  const request_id = crypto.randomUUID()
  const thread_id = ctx.threadId
  ctx.gateway.addAuthGate({ request_id, thread_id, extension_name: "github", instructions: "Paste a personal access token", corr: ctx.corr })
  return [
    { type: "turn_started", thread_id },
    {
      type: "gate_required",
      request_id,
      gate_name: "authentication",
      tool_name: "github",
      description: "",
      parameters: "{}",
      thread_id,
      resume_kind: { Authentication: { credential_name: "github", instructions: "Paste a personal access token", auth_url: null } },
    },
    { type: "auth_required", extension_name: "github", instructions: "Paste a personal access token", thread_id },
    { type: "turn_finished", thread_id, outcome: "paused" },
  ]
}

describe("current gateway", () => {
  test("events keyed by client_turn_id attach even when they precede the 202", async () => {
    const r = await ready({ mode: "current", mock: { emitBeforeAccept: true } })
    await r.run("hello")
    await r.until(() => phase(r, T1) === "done", "turn sealed")
    const turn = r.store.getState().turns[T1]
    expect(turn?.finalText).toBe("echo: hello")
    const send = r.mock.requestsTo("POST /api/chat/send")[0]
    expect((send?.body as { client_turn_id?: string }).client_turn_id).toBe(T1)
  })

  test("turn_finished{paused} keeps the approval open; the approval POST carries the owning turn's id", async () => {
    const r = await ready({ mode: "current" })
    await r.run("!approval")
    await r.until(() => r.store.getState().approvalQueue.length === 1, "approval prompt")
    await r.until(() => phase(r, T1) === "awaiting_approval", "paused turn")
    await r.advance(1_000)
    r.runtime.session.answerFocused("approve")
    await r.until(() => r.mock.requestsTo("POST /api/chat/approval").length === 1, "approval POST")
    const body = r.mock.requestsTo("POST /api/chat/approval")[0]?.body as Record<string, unknown>
    expect(body.client_turn_id).toBe(T1)
    expect(body.action).toBe("approve")
    await r.until(() => phase(r, T1) === "done", "continuation on the owning turn")
    expect(r.store.getState().turns[T1]?.finalText).toBe("hello")
    expect(r.store.getState().approvalQueue).toEqual([])
  })

  test("auth_required then Enter calls auth-token, never send; the continuation seals the owning turn", async () => {
    const r = await ready({ mode: "current", mock: { replies: authGateReply } })
    await r.run("connect github")
    await r.until(() => r.store.getState().composer.credential !== null, "credential mode")
    expect(r.store.getState().composer.credential?.turnId).toBe(T1)
    r.runtime.credential.push("ghp_0123456789abcdefghij")
    await r.enter()
    await r.until(() => r.mock.requestsTo("POST /api/chat/auth-token").length === 1, "auth-token POST")
    const body = r.mock.requestsTo("POST /api/chat/auth-token")[0]?.body as Record<string, unknown>
    expect(body.extension_name).toBe("github")
    expect(body.client_turn_id).toBe(T1)
    expect(r.mock.requestsTo("POST /api/chat/send")).toHaveLength(1)
    await r.until(() => phase(r, T1) === "done", "owning turn sealed")
    expect(r.store.getState().turns[T1]?.finalText).toBe("github is connected.")
    expect(JSON.stringify(r.store.getState())).not.toContain("ghp_0123456789")
  })

  test("Esc Esc calls auth-cancel and the 2xx seals the owning turn", async () => {
    const r = await ready({ mode: "current", mock: { replies: authGateReply } })
    await r.run("connect github")
    await r.until(() => r.store.getState().composer.credential !== null, "credential mode")
    r.runtime.session.credentialEscape()
    r.runtime.session.credentialEscape()
    await r.until(() => r.mock.requestsTo("POST /api/chat/auth-cancel").length === 1, "auth-cancel POST")
    await r.until(() => phase(r, T1) === "interrupted", "owning turn sealed")
    expect(r.store.getState().turns[T1]?.notes).toContain(NOTES.credentialCancelled)
    expect(r.store.getState().composer.credential).toBeNull()
    expect(r.mock.requestsTo("POST /api/chat/auth-token")).toHaveLength(0)
  })

  test("a response beginning with 'Error: ' renders a failed turn", async () => {
    const replies = (ctx: MockSendContext): MockEvent[] => [
      { type: "turn_started", thread_id: ctx.threadId },
      { type: "response", content: "Error: the tool crashed", thread_id: ctx.threadId },
      { type: "turn_finished", thread_id: ctx.threadId, outcome: "failed" },
    ]
    const r = await ready({ mode: "current", mock: { replies } })
    await r.run("run it")
    await r.until(() => phase(r, T1) === "failed", "failed turn")
    const turn = r.store.getState().turns[T1]
    expect(turn?.error?.message).toBe("the tool crashed")
    expect(turn?.finalText).toBeNull()
  })

  test("a second 401 goes offline with the hint for the token kind", async () => {
    const r = await startGatewayRuntime({ mode: "current", clientToken: fixtureToken("wrong"), tokenKind: "client" })
    rig = r
    await r.until(() => r.store.getState().conn.kind === "offline", "offline")
    const conn = r.store.getState().conn
    expect(conn.kind === "offline" && conn.fatal).toBe(true)
    expect(conn.kind === "offline" ? conn.reason : "").toContain("tui-access derpy --for-user $USER --use-master-token")
    expect(r.token.reloads).toBe(1)
  })

  test("403 shows the scope error", async () => {
    const r = await startGatewayRuntime({ mode: "current" })
    rig = r
    r.mock.override("GET /api/chat/events", () => new Response("Forbidden", { status: 403 }))
    r.runtime.supervisor.reconnectNow()
    await r.until(() => r.store.getState().conn.kind === "offline", "offline")
    const conn = r.store.getState().conn
    expect(conn.kind === "offline" ? conn.reason : "").toContain("client tokens reach only chat, voice and status routes")
  })
})

describe("legacy gateway", () => {
  test("the legacy badge, and two queued sends each get their own response (FIFO)", async () => {
    const r = await ready({ mode: "legacy", mock: { engine: "v1" } })
    expect(headerText(r.store.getState(), { primary: true, imageLabel: null }, r.clock.now())).toContain("legacy daemon")
    await r.run("one")
    await r.run("two")
    await r.until(() => phase(r, T1) === "done" && phase(r, T2) === "done", "both turns sealed")
    expect(r.store.getState().turns[T1]?.finalText).toBe("echo: one")
    expect(r.store.getState().turns[T2]?.finalText).toBe("echo: two")
  })

  test("status Done without a response triggers a resync after the grace", async () => {
    const replies = (ctx: MockSendContext): MockEvent[] => {
      // The reply reached history, but its SSE event was lost.
      const turn = ctx.gateway.threads.get(ctx.threadId)?.turns.at(-1)
      if (turn !== undefined) {
        turn.response = "recovered answer"
        turn.state = "Completed"
        turn.completed_at = new Date().toISOString()
      }
      return [{ type: "thinking", message: "Processing...", thread_id: ctx.threadId }, { type: "status", message: "Done", thread_id: ctx.threadId }]
    }
    const r = await ready({ mode: "legacy", mock: { engine: "v1", replies } })
    await r.run("question")
    await r.until(() => r.store.getState().turns[T1]?.activity.some((entry) => entry.kind === "status") === true, "status Done")
    const before = r.mock.requestsTo("GET /api/chat/history").length
    await r.advance(9_000)
    expect(r.mock.requestsTo("GET /api/chat/history")).toHaveLength(before)
    await r.advance(1_500)
    await r.until(() => phase(r, T1) === "done", "recovered from history")
    expect(r.mock.requestsTo("GET /api/chat/history").length).toBeGreaterThan(before)
    expect(r.store.getState().turns[T1]?.recovered).toBe(true)
    expect(r.store.getState().turns[T1]?.finalText).toBe("recovered answer")
  })

  test("with a performance.now() Clock the settle-grace resync never seals a turn with an older turn's reply", async () => {
    // Production's Clock (SystemClock) counts from process start; the wall clock gives history times their offset.
    const replies = (ctx: MockSendContext): MockEvent[] =>
      ctx.content === "continue"
        ? [{ type: "thinking", message: "Processing...", thread_id: ctx.threadId }]
        : [
            { type: "thinking", message: "Processing...", thread_id: ctx.threadId },
            { type: "status", message: "Done", thread_id: ctx.threadId },
            { type: "response", content: "Step 4 done.", thread_id: ctx.threadId },
          ]
    const r = await ready({ mode: "legacy", mock: { engine: "v1", replies }, clockStart: 75_000, wallNow: () => Date.now() })
    const threadId = r.gateway.currentThreadId ?? ""
    const yesterday = new Date(Date.now() - 86_400_000)
    r.mock.threads.get(threadId)?.turns.unshift({
      user_input: "continue",
      response: "Step 3 done.",
      state: "Completed",
      started_at: yesterday.toISOString(),
      completed_at: new Date(yesterday.getTime() + 5_000).toISOString(),
    })
    await r.run("do step 4")
    await r.until(() => phase(r, T1) === "done", "first turn done (status Done arms the grace)")
    await r.advance(3_000)
    await r.run("continue")
    await r.until(() => r.store.getState().turns[T2]?.activity.length === 1, "second turn running")
    const before = r.mock.requestsTo("GET /api/chat/history").length
    await r.advance(8_000)
    await r.until(() => r.mock.requestsTo("GET /api/chat/history").length > before, "settle-grace resync")
    await r.settle()
    expect(phase(r, T2)).toBe("running")
    expect(r.store.getState().turns[T2]?.finalText).toBeNull()
    expect(r.store.getState().turns[T2]?.recovered).toBeUndefined()
  })

  test("a pending authentication gate restores credential mode on connect, so the next message is never chat", async () => {
    // The live gate_required / auth_required were missed (the TUI restarted); pending-gates still lists the gate.
    const threadId = "3a1f0c9e-5b7d-4e2a-9c8b-1d2e3f4a5b6c"
    const requestId = "7f6e5d4c-3b2a-4190-8f7e-6d5c4b3a2910"
    const r = await ready({
      mode: "legacy",
      state: { lastThreadId: threadId },
      setup: (mock) => {
        mock.addThread([], threadId)
        mock.addAuthGate({ request_id: requestId, thread_id: threadId, extension_name: "github", instructions: "Paste a personal access token" })
      },
    })
    await r.until(() => r.store.getState().composer.credential !== null, "credential mode from pending-gates")
    expect(r.store.getState().composer.credential).toMatchObject({ extensionName: "github", requestId, threadId })
    await r.run("what's the status?")
    expect(r.mock.requestsTo("POST /api/chat/send")).toHaveLength(0)

    // still credential mode after the 290 s V1 limit: a V2 gate (it has a request id) waits up to 30 min
    await r.advance(300_000, 10_000)
    expect(r.store.getState().composer.credential?.requestId).toBe(requestId)

    r.runtime.credential.push("ghp_0123456789abcdefghij")
    await r.enter()
    await r.until(() => r.mock.requestsTo("POST /api/chat/auth-token").length === 1, "auth-token POST")
    const body = r.mock.requestsTo("POST /api/chat/auth-token")[0]?.body as Record<string, unknown>
    expect(body).toMatchObject({ extension_name: "github", request_id: requestId, thread_id: threadId })
    expect(r.mock.requestsTo("POST /api/chat/send")).toHaveLength(0)
  })

  test("a stream gap resync restores credential mode for a gate whose live events were lost", async () => {
    const r = await ready({ mode: "legacy" })
    const threadId = r.gateway.currentThreadId ?? ""
    const requestId = crypto.randomUUID()
    r.mock.addAuthGate({ request_id: requestId, thread_id: threadId, extension_name: "linear", instructions: "API key" })
    r.mock.emit({ type: "stream_gap", missed_events: 12 })
    await r.until(() => r.store.getState().composer.credential !== null, "credential mode after the resync")
    expect(r.store.getState().composer.credential).toMatchObject({ extensionName: "linear", requestId, threadId })
  })

  test("a bare keyword the daemon consumes: 'no reply yet' after 15 s, unanswered after 10 min", async () => {
    const r = await ready({ mode: "legacy", mock: { engine: "v1", replies: () => [] } })
    await r.run("ok")
    expect(r.mock.requestsTo("POST /api/chat/send")).toHaveLength(0)
    await r.enter()
    await r.until(() => phase(r, T1) === "sent", "sent")
    await r.advance(14_000)
    expect(r.store.getState().turns[T1]).toMatchObject({ phase: "sent", notes: [] })
    await r.advance(2_000)
    expect(r.store.getState().turns[T1]).toMatchObject({ phase: "sent", notes: [NOTES.noReplyYet] })
    await r.advance(600_000, 10_000)
    expect(r.store.getState().turns[T1]).toMatchObject({ phase: "unanswered", notes: [NOTES.unanswered] })
  })

  test("Ctrl+S cancels a turn that still waits: the daemon says 'Nothing to interrupt.' and the turn is unanswered", async () => {
    const replies = (ctx: MockSendContext): MockEvent[] => (ctx.content === "ok" ? [] : echoReply(ctx))
    const r = await ready({ mode: "legacy", mock: { engine: "v1", replies } })
    await r.run("ok")
    await r.enter()
    await r.advance(20_000)
    expect(r.store.getState().turns[T1]?.notes).toEqual([NOTES.noReplyYet])
    // Ctrl+S with an empty composer: interrupt only
    r.runtime.session.interruptAndSend()
    await r.until(() => phase(r, T1) === "unanswered", "cancelled")
    expect(r.store.getState().turns[T1]?.notes).toEqual([NOTES.nothingToInterrupt])
    expect(r.mock.requestsTo("POST /api/chat/send").map((rec) => (rec.body as { content: string }).content)).toEqual(["ok", "/interrupt"])
    // the next message is first in line and takes its own reply
    await r.run("status please")
    await r.until(() => r.store.getState().turns["t-3" as TurnId]?.phase === "done", "next reply")
    expect(r.store.getState().turns["t-3" as TurnId]?.finalText).toBe("echo: status please")
  })

  test("a message the daemon's scheduler held back for 20 s still gets its own reply; the next one gets its own", async () => {
    // Another channel's exclusive turn runs on the daemon: the message waits in the MessageScheduler without a word.
    const held: MockSendContext[] = []
    const replies = (ctx: MockSendContext): MockEvent[] => {
      if (ctx.content !== "check the backup status") return echoReply(ctx)
      held.push(ctx)
      return []
    }
    const r = await ready({ mode: "legacy", mock: { engine: "v2", replies } })
    await r.run("check the backup status")
    await r.until(() => phase(r, T1) === "sent", "sent")
    await r.advance(20_000, 1_000)
    expect(r.store.getState().turns[T1]).toMatchObject({ phase: "sent", notes: [NOTES.noReplyYet] })
    // the held message finally runs: its reply lands on it, not on a later turn
    const ctx = held[0] as MockSendContext
    for (const event of echoReply({ ...ctx, content: "Backups: all green." })) r.mock.emit(event)
    await r.until(() => phase(r, T1) === "done", "late reply")
    expect(r.store.getState().turns[T1]).toMatchObject({ finalText: "echo: Backups: all green.", notes: [] })
    await r.run("also list disk usage")
    await r.until(() => phase(r, T2) === "done", "second reply")
    expect(r.store.getState().turns[T2]?.finalText).toBe("echo: also list disk usage")
  })

  test("other-thread events only bump the counter", async () => {
    const r = await ready({ mode: "legacy" })
    const records = r.store.getState().order.length
    r.mock.emit({ type: "response", content: "from elsewhere", thread_id: crypto.randomUUID() })
    await r.until(() => r.store.getState().otherThreadsUnread === 1, "counter")
    expect(r.store.getState().order).toHaveLength(records)
  })

  test("/thread list shows the gateway's threads (titles sanitized) and /thread <prefix> switches to one", async () => {
    const r = await ready({ mode: "legacy" })
    // Compile-time: the real transport provides the session's optional thread-listing capability.
    const lister: ThreadLister = r.gateway
    const other = r.mock.addThread([], "0f1e2d3c-4b5a-4968-8776-655443322110")
    other.title = "deploy \u001b]0;pwned\u0007notes\u202e"
    r.mock.addThread([], "not-a-uuid")
    const rows = await lister.listThreads()
    expect(rows.find((row) => row.id === other.id)).toMatchObject({ title: "deploy notes", assistant: false, turnCount: 0 })
    expect(rows.some((row) => row.id === "not-a-uuid")).toBe(false)
    expect(rows[0]?.assistant).toBe(true)

    await r.run("/thread list")
    const state = r.store.getState()
    expect(state.panel?.kind).toBe("threads")
    const lines = buildPanel("threads", state, r.runtime.panelEnv(100, r.clock.now())).join("\n")
    expect(lines).toContain(`${other.id.slice(0, 8)} deploy notes`)
    expect(lines).not.toContain("\u001b")
    expect(lines).not.toContain("pwned")

    await r.run(`/thread ${other.id.slice(0, 6)}`)
    await r.until(() => r.store.getState().threadId === other.id, "thread switch")
    expect(routes(r).filter((route) => route === "GET /api/chat/threads").length).toBeGreaterThanOrEqual(2)
  })

  test("send 429 restores the draft", async () => {
    const r = await ready({ mode: "legacy" })
    r.mock.override("POST /api/chat/send", () => new Response("Too Many Requests", { status: 429 }), 1)
    await r.run("my precious draft")
    await r.until(() => phase(r, T1) === "failed", "failed send")
    expect(r.runtime.composer.getText()).toBe("my precious draft")
    expect(r.store.getState().notices.map((notice) => notice.text).join(" ")).toContain("send failed")
    expect(routes(r).filter((route) => route === "POST /api/chat/send")).toHaveLength(1)
  })
})
