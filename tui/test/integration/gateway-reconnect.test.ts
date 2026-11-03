/**
 * Supervisor and reconnects against the mock gateway (design 3.6.5, 11.3.3, WI#458 item 5, SEC-01): a dropped
 * stream reconnects and resyncs without re-sending; a daemon restart mid-turn (uptime_secs starts again) seals the
 * dead turn "lost (daemon restarted)" while a plain drop keeps it open; the 45 s watchdog forces a reconnect; 503 "Too many
 * connections" waits 30 s; a 401 on the event stream goes offline without a retry loop; a listener owned by
 * another uid during a reconnect goes offline with the uid message. Every case runs against the mock gateway in
 * both modes: "legacy" (today's tenant daemons, the M1a target) and "current" (the M2a contract). Backoff
 * jitter is fixed; time is manual.
 */
import { afterEach, describe, expect, test } from "bun:test"
import type { TurnId } from "../../src/state/model.ts"
import { exponentialBackoff } from "../../src/transport/backoff.ts"
import type { ProcReader } from "../../src/transport/listener-owner.ts"
import { startGatewayRuntime, TEST_UID, type GatewayRig } from "../helpers/render.tsx"
import { echoReply, fixtureToken, procNetTcpLine, type MockEvent, type MockSendContext } from "../mocks/mock-gateway.ts"

let rig: GatewayRig | null = null

afterEach(async () => {
  await rig?.dispose()
  rig = null
})

const T1 = "t-1" as TurnId
const TCP_HEADER = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode"

/** /proc/net/tcp with the mock's port owned by whatever `owner()` says now. */
function switchableProc(port: () => number, owner: () => number): ProcReader {
  return async (path) => (path === "/proc/net/tcp" ? `${TCP_HEADER}\n${procNetTcpLine(0, "00000000", port(), owner())}\n` : `${TCP_HEADER}\n`)
}

type Mode = Parameters<typeof startGatewayRuntime>[0]["mode"]

async function start(mode: Mode, opts: Partial<Parameters<typeof startGatewayRuntime>[0]> = {}): Promise<GatewayRig> {
  let clockNow = (): number => 0
  const backoff = exponentialBackoff({ random: () => 0.5, clock: { now: () => clockNow() } })
  const r = await startGatewayRuntime({ mode, backoff, ...opts })
  clockNow = () => r.clock.now()
  rig = r
  return r
}

function eventStreams(r: GatewayRig): number {
  return r.mock.requestsTo("GET /api/chat/events").length
}

describe.each(["legacy", "current"] as const)("reconnects (%s gateway)", (mode) => {
  test("a dropped stream: reconnecting, ready again, history resynced, nothing re-sent", async () => {
    const r = await start(mode)
    await r.until(() => r.store.getState().conn.kind === "ready", "ready")
    await r.run("hello")
    await r.until(() => r.store.getState().turns[T1]?.phase === "done", "first turn")
    const histories = r.mock.requestsTo("GET /api/chat/history").length

    r.mock.dropStreams()
    await r.until(() => r.store.getState().conn.kind === "reconnecting", "reconnecting")
    const conn = r.store.getState().conn
    expect(conn.kind === "reconnecting" ? conn.retryAt - r.clock.now() : -1).toBe(500)
    await r.advance(600)
    await r.until(() => r.store.getState().conn.kind === "ready", "ready again")
    expect(eventStreams(r)).toBe(2)
    expect(r.mock.requestsTo("GET /api/chat/history").length).toBeGreaterThan(histories)
    expect(r.mock.requestsTo("POST /api/chat/send")).toHaveLength(1)
    expect(r.store.getState().turns[T1]?.phase).toBe("done")
  })

  test("a daemon restart mid-turn: the dead turn is lost (daemon restarted) and the next message gets its own reply", async () => {
    // The first message runs and never finishes: the daemon goes down mid-turn.
    const replies = (ctx: MockSendContext): MockEvent[] =>
      ctx.content === "migrate the database"
        ? [
            ...(ctx.mode === "current" ? [{ type: "turn_started", thread_id: ctx.threadId }] : []),
            { type: "thinking", message: "Processing...", thread_id: ctx.threadId },
            { type: "tool_started", name: "shell", thread_id: ctx.threadId },
          ]
        : echoReply(ctx)
    const r = await start(mode, { mock: { replies } })
    await r.until(() => r.store.getState().conn.kind === "ready", "ready")
    await r.run("migrate the database")
    await r.until(() => r.store.getState().turns[T1]?.phase === "running", "running")
    await r.advance(30_000, 5_000)

    // upgrade-tenant, OOM or crash: streams drop, the daemon starts again, history shows the turn Failed
    r.mock.restart()
    await r.until(() => r.store.getState().conn.kind === "reconnecting", "reconnecting")
    await r.advance(600)
    await r.until(() => r.store.getState().conn.kind === "ready" && r.store.getState().turns[T1]?.phase === "lost", "lost after the resync")
    expect(r.mock.requestsTo("GET /api/gateway/status")).toHaveLength(2)
    expect(r.store.getState().turns[T1]?.notes).toEqual(["lost (daemon restarted)"])
    expect(r.store.getState().notices.map((notice) => notice.text)).toContain("the daemon restarted: 1 unfinished turn was lost")

    await r.run("what happened?")
    await r.until(() => r.store.getState().turns["t-2" as TurnId]?.phase === "done", "the next reply")
    expect(r.store.getState().turns["t-2" as TurnId]?.finalText).toBe("echo: what happened?")
    expect(r.store.getState().turns[T1]).toMatchObject({ phase: "lost", finalText: null })
  })

  test("a dropped stream without a restart keeps the turn open (the daemon is the same process)", async () => {
    const replies = (ctx: MockSendContext): MockEvent[] =>
      ctx.content === "run the build"
        ? [
            ...(ctx.mode === "current" ? [{ type: "turn_started", thread_id: ctx.threadId }] : []),
            { type: "thinking", message: "Processing...", thread_id: ctx.threadId },
          ]
        : echoReply(ctx)
    const r = await start(mode, { mock: { replies } })
    await r.until(() => r.store.getState().conn.kind === "ready", "ready")
    await r.run("run the build")
    await r.until(() => r.store.getState().turns[T1]?.phase === "running", "running")
    await r.advance(30_000, 5_000)
    r.mock.dropStreams()
    await r.until(() => r.store.getState().conn.kind === "reconnecting", "reconnecting")
    await r.advance(600)
    await r.until(() => r.store.getState().conn.kind === "ready", "ready again")
    await r.settle()
    expect(r.store.getState().turns[T1]?.phase).toBe("running")
    expect(r.store.getState().notices.map((notice) => notice.text).some((text) => text.includes("restarted"))).toBe(false)
  })

  test("the watchdog: 45 s without a byte forces a reconnect", async () => {
    const r = await start(mode)
    await r.until(() => r.store.getState().conn.kind === "ready", "ready")
    await r.advance(44_000)
    expect(r.store.getState().conn.kind).toBe("ready")
    await r.advance(1_500)
    await r.until(() => r.store.getState().conn.kind !== "ready" || eventStreams(r) === 2, "watchdog fired")
    await r.advance(600)
    await r.until(() => eventStreams(r) === 2 && r.store.getState().conn.kind === "ready", "reconnected")
  })

  test("503 Too many connections retries after 30 s", async () => {
    const r = await start(mode)
    await r.until(() => r.store.getState().conn.kind === "ready", "ready")
    r.mock.override("GET /api/chat/events", () => new Response("Too many connections", { status: 503 }), 1)
    r.mock.dropStreams()
    await r.until(() => r.store.getState().conn.kind === "reconnecting", "reconnecting")
    await r.advance(600)
    await r.until(() => eventStreams(r) === 2, "second stream attempt")
    await r.until(() => {
      const conn = r.store.getState().conn
      return conn.kind === "reconnecting" && conn.retryAt - r.clock.now() >= 30_000
    }, "30 s wait")
    await r.advance(29_000)
    expect(eventStreams(r)).toBe(2)
    await r.advance(1_500)
    await r.until(() => eventStreams(r) === 3 && r.store.getState().conn.kind === "ready", "ready after 30 s")
  })

  test("a 401 on the event stream goes offline without a retry loop", async () => {
    const r = await start(mode, { clientToken: fixtureToken("wrong") })
    await r.until(() => r.store.getState().conn.kind === "offline", "offline")
    // The access file was re-read once; an unchanged token is not retried (transport/gateway.ts refreshToken).
    expect(r.token.reloads).toBe(1)
    const streams = eventStreams(r)
    expect(streams).toBe(1)
    await r.advance(60_000, 1_000)
    expect(eventStreams(r)).toBe(streams)
    expect(r.store.getState().conn.kind).toBe("offline")
  })

  test("a listener owned by another uid during a reconnect goes offline with the uid message", async () => {
    let owner = TEST_UID
    let port = 0
    const r = await start(mode, { readProc: switchableProc(() => port, () => owner) })
    port = r.mock.port
    r.runtime.supervisor.reconnectNow()
    await r.until(() => r.store.getState().conn.kind === "ready", "ready")
    const authorized = r.mock.requests.filter((rec) => rec.headers.authorization !== undefined).length

    owner = TEST_UID + 4242
    r.mock.dropStreams()
    await r.until(() => r.store.getState().conn.kind === "reconnecting", "reconnecting")
    await r.advance(600)
    await r.until(() => r.store.getState().conn.kind === "offline", "offline")
    const conn = r.store.getState().conn
    expect(conn.kind === "offline" ? conn.reason : "").toContain(`is held by uid ${TEST_UID + 4242}, expected ${TEST_UID} (test)`)
    expect(conn.kind === "offline" && conn.fatal).toBe(true)
    // The foreign listener never saw another token-bearing request.
    expect(r.mock.requests.filter((rec) => rec.headers.authorization !== undefined)).toHaveLength(authorized)
  })
})
