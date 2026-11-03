/**
 * A daemon restart in the middle of a turn (design 3.5 rule 13; review finding client-correctness-6). The gateway
 * reports when the daemon process started (ServerInfo.daemonStart, from GET /api/gateway/status uptime_secs); a
 * reconnect that sees a later start marks a restart, and the resync after it seals what the old process was still
 * handling "lost (daemon restarted)" once history had its chance to show finished turns. Durable V2 history shows
 * such a turn Failed without a reply, which on its own also means "still running", so without a restart nothing
 * changes. Pure reducer; time is explicit.
 */
import { describe, expect, test } from "bun:test"
import type { Action } from "../../src/state/actions.ts"
import { daemonRestarted, NOTES, RESTART_SLACK_MS } from "../../src/state/correlate.ts"
import { createTurnIdSequence } from "../../src/state/ids.ts"
import { createInitialState, type AppState, type TurnId, type TurnRecord } from "../../src/state/model.ts"
import { reduce } from "../../src/state/reducer.ts"
import { turnStatusLabel } from "../../src/components/turn-view.tsx"
import * as wire from "../../src/transport/gateway-wire.ts"
import type { DaemonStart, HistoryTurn, ServerInfo, TransportEvent, WireApproval } from "../../src/transport/types.ts"

type WithoutAt<A> = A extends unknown ? Omit<A, "at"> : never

const THREAD = "5f0e4a52-6a3e-4a8f-9a1e-2b9d6c1e7a10"
/** Production-like Clock values (performance.now(): milliseconds since the TUI started). */
const START = 120_000
/** Epoch time of START: history times are wall-clock server times, converted with wallAt. */
const WALL_START = Date.UTC(2026, 10, 3, 9, 0, 0)

function info(mode: "legacy" | "current", daemonStart: DaemonStart | null): ServerInfo {
  const capabilities =
    mode === "current"
      ? wire.capabilitiesFromChat(
          { api_version: 1, server_version: "2.1.9", engine: "v2", features: ["status", "attachments", "stream"], limits: {} as never, voice: { provider: null, stt: false, tts: false }, credential: "master" },
          null,
        )
      : wire.legacyGatewayCapabilities(null)
  return {
    transport: "gateway",
    protocol: mode === "current" ? "gateway-chat" : "gateway-legacy",
    protocolVersion: mode === "current" ? 1 : null,
    serverVersion: null,
    agentName: null,
    engine: mode === "current" ? "v2" : "unknown",
    sessionId: null,
    threadId: THREAD,
    endpointLabel: "http://127.0.0.1:10000",
    credential: "master",
    capabilities,
    limits: wire.legacyGatewayLimits(null),
    warnings: [],
    daemonStart,
  }
}

/** The start a probe answered at `answeredAt` with `uptimeSecs` reports (gateway.ts probeDaemonStart, no RTT). */
function startAt(answeredAt: number, uptimeSecs: number): DaemonStart {
  return { earliest: answeredAt - (uptimeSecs + 1) * 1000, latest: answeredAt - uptimeSecs * 1000, measuredAt: answeredAt }
}

class Rig {
  state: AppState
  now = START
  private readonly next = createTurnIdSequence("t")

  constructor(
    readonly mode: "legacy" | "current",
    /** The daemon's age at the first connect. */
    uptimeSecs = 3_600,
  ) {
    this.state = createInitialState({ profile: "p", tenant: null, view: { width: 100, height: 40 }, at: this.now })
    this.dispatch({ type: "conn/ready", info: info(mode, startAt(this.now, uptimeSecs)) })
  }

  dispatch(action: WithoutAt<Action>): AppState {
    this.state = reduce(this.state, { ...action, at: this.now } as Action)
    return this.state
  }

  wall(clockMs = this.now): number {
    return WALL_START + (clockMs - START)
  }

  send(text: string): TurnId {
    const turnId = this.next()
    this.dispatch({ type: "turn/created", turnId, text, images: [] })
    this.dispatch({ type: "turn/sent", turnId })
    return turnId
  }

  evt(event: TransportEvent): AppState {
    return this.dispatch({ type: "evt", event })
  }

  /** One raw gateway SSE object through the real mapper, as the mode's connection maps it. */
  sse(raw: Record<string, unknown>, turnId?: TurnId): AppState {
    const corr = turnId === undefined ? {} : { client_turn_id: turnId, message_id: crypto.randomUUID() }
    const ctx = { correlated: this.mode === "current", resolveTurnId: (id: string | undefined) => (id === undefined ? null : (id as TurnId)) }
    for (const event of wire.decodeGatewayEvent({ thread_id: THREAD, ...raw, ...corr }, ctx)) this.evt(event)
    return this.state
  }

  /** The stream drops, `downMs` pass, and the gateway answers again with a daemon `uptimeSecs` old (null: unknown). */
  reconnect(downMs: number, uptimeSecs: number | null): void {
    this.dispatch({ type: "conn/lost", reason: "the gateway closed the event stream", attempt: 1, retryAt: this.now + 500, resumable: true })
    this.now += downMs
    this.dispatch({ type: "conn/ready", info: info(this.mode, uptimeSecs === null ? null : startAt(this.now, uptimeSecs)) })
  }

  resync(turns: HistoryTurn[], approvals: WireApproval[] = []): void {
    this.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, turns, approvals }, wallAt: this.wall() })
  }

  history(userInput: string, response: string | null, state: string, startedClock: number): HistoryTurn {
    const startedAt = new Date(this.wall(startedClock)).toISOString()
    return { userInput, response, state, startedAt, completedAt: response === null ? null : startedAt }
  }

  turn(id: TurnId): TurnRecord {
    const record = this.state.turns[id]
    if (record === undefined) throw new Error(`no record ${id}`)
    return record
  }
}

describe("daemonRestarted", () => {
  test("a start later than the known one, beyond the slack and the drift, is a restart; unknown says nothing", () => {
    const known = startAt(START, 3_600)
    expect(daemonRestarted(known, startAt(START + 60_000, 3_660))).toBe(false) // the same process a minute later
    expect(daemonRestarted(known, startAt(START + 60_000, 3_659))).toBe(false) // rounding of the whole seconds
    expect(daemonRestarted(known, startAt(START + 60_000, 4))).toBe(true)
    // a daemon that had just started when the TUI connected, restarted a few seconds later
    expect(daemonRestarted(startAt(START, 1), startAt(START + 9_000, 2))).toBe(true)
    expect(daemonRestarted(startAt(START, 1), startAt(START + 1_000 + RESTART_SLACK_MS, 1))).toBe(false)
    // a day apart, the two hosts' clocks may have drifted by seconds: 1 ms per second is allowed
    const day = 86_400_000
    expect(daemonRestarted(known, startAt(START + day, 3_600 + 86_400 - 30))).toBe(false)
    expect(daemonRestarted(known, startAt(START + day, 3_600 + 86_400 - 200))).toBe(true)
    // an earlier start (the client's clock stood still while its host slept) is never a restart
    expect(daemonRestarted(known, startAt(START + 60_000, 7_200))).toBe(false)
    expect(daemonRestarted(null, startAt(START, 1))).toBe(false)
    expect(daemonRestarted(known, null)).toBe(false)
    expect(daemonRestarted(known, undefined)).toBe(false)
  })
})

describe.each(["legacy", "current"] as const)("a daemon restart mid-turn (%s gateway)", (mode) => {
  test("the turn in flight is sealed lost and never takes the next turn's reply", () => {
    const r = new Rig(mode)
    const a = r.send("migrate the database")
    const started = r.now
    r.sse({ type: "thinking", message: "Processing..." }, mode === "current" ? a : undefined)
    r.sse({ type: "tool_started", name: "shell" }, mode === "current" ? a : undefined)
    // upgrade-tenant, an OOM or a crash: the stream drops, the daemon comes back 8 s later
    r.reconnect(8_000, 2)
    expect(r.turn(a).phase).toBe("running") // the resync decides
    // durable history shows the dead turn Failed without a reply (ic/src/channels/web/util.rs)
    r.resync([r.history("migrate the database", null, "Failed", started)])
    expect(r.turn(a)).toMatchObject({ phase: "lost", notes: [NOTES.daemonRestarted] })
    expect(turnStatusLabel(r.turn(a), { now: r.now, queuePosition: null, streaming: true })).toBe("lost (daemon restarted)")
    expect(r.state.inflight).toEqual([])
    expect(r.state.notices.map((notice) => notice.text)).toContain("the daemon restarted: 1 unfinished turn was lost")

    r.now += 5_000
    const b = r.send("what happened?")
    r.sse({ type: "turn_started" }, mode === "current" ? b : undefined)
    r.sse({ type: "thinking", message: "Processing..." }, mode === "current" ? b : undefined)
    r.sse({ type: "response", content: "The daemon restarted while the migration was running." }, mode === "current" ? b : undefined)
    if (mode === "current") r.evt({ kind: "turn.finished", turnId: b, outcome: "replied" })
    expect(r.turn(b)).toMatchObject({ phase: "done", finalText: "The daemon restarted while the migration was running." })
    expect(r.turn(a)).toMatchObject({ phase: "lost", finalText: null })
  })

  test("without a restart a young reply-less Failed turn stays open: on V2 it may still be running", () => {
    for (const uptime of [3_600 + 8, null]) {
      const r = new Rig(mode)
      const a = r.send("run the build")
      const started = r.now
      r.sse({ type: "thinking", message: "Processing..." }, mode === "current" ? a : undefined)
      // the same daemon (8 s older), or a probe that could not tell
      r.reconnect(8_000, uptime)
      r.resync([r.history("run the build", null, "Failed", started)])
      expect(r.turn(a).phase).toBe("running")
      expect(r.state.notices.map((notice) => notice.text).some((text) => text.includes("restarted"))).toBe(false)
    }
  })

  test("a turn the old daemon finished before it went down is recovered from history, not lost", () => {
    const r = new Rig(mode)
    const a = r.send("check the disk")
    const started = r.now
    r.reconnect(8_000, 2)
    r.resync([r.history("check the disk", "The disk is 41% full.", "Completed", started)])
    expect(r.turn(a)).toMatchObject({ phase: "done", finalText: "The disk is 41% full.", recovered: true })
    expect(r.state.notices.at(-1)).toMatchObject({ level: "info", text: "the daemon restarted" })
  })

  test("a turn sent to the new daemon before the restart was seen (a probe failed in between) is not touched", () => {
    const r = new Rig(mode)
    const old = r.send("before the restart")
    r.reconnect(8_000, null) // restarted, but this connect could not tell
    r.now += 2_000
    const fresh = r.send("after the restart")
    r.reconnect(3_000, 5) // the new daemon is 5 s old now
    r.resync([r.history("after the restart", null, "Processing", r.now - 3_000)])
    expect(r.turn(old).phase).toBe("lost")
    expect(r.turn(fresh).phase).not.toBe("lost")
  })
})

describe("what a restart keeps", () => {
  const prompt = (id: string): WireApproval => ({
    id,
    threadId: THREAD,
    toolName: "shell",
    description: "Run a shell command",
    parameters: "{}",
    allowAlways: false,
    truncated: false,
    source: "pending_gates",
  })

  test("a V2 turn paused at a gate the restarted daemon still lists stays paused; one whose gate is gone is lost", () => {
    const r = new Rig("current")
    const kept = r.send("rotate the logs")
    r.evt({ kind: "turn.approval", turnId: kept, threadId: THREAD, approval: { ...prompt("6c0f2a8e-1f9b-4d3e-a1c2-7b5e9d0f4a21"), source: "live" } })
    r.evt({ kind: "turn.finished", turnId: kept, outcome: "paused" })
    const gone = r.send("delete the cache")
    r.evt({ kind: "turn.approval", turnId: gone, threadId: THREAD, approval: { ...prompt("0d1e2f3a-4b5c-4d6e-8f70-8192a3b4c5d6"), source: "live" } })
    r.evt({ kind: "turn.finished", turnId: gone, outcome: "paused" })
    expect([r.turn(kept).phase, r.turn(gone).phase]).toEqual(["awaiting_approval", "awaiting_approval"])

    r.reconnect(8_000, 2)
    // V2 restores persisted gates: pending-gates still lists the first one
    r.resync([], [prompt("6c0f2a8e-1f9b-4d3e-a1c2-7b5e9d0f4a21")])
    expect(r.turn(kept).phase).toBe("awaiting_approval")
    expect(r.state.approvalQueue).toEqual(["6c0f2a8e-1f9b-4d3e-a1c2-7b5e9d0f4a21"])
    expect(r.turn(gone)).toMatchObject({ phase: "lost", notes: [NOTES.daemonRestarted] })
    expect(r.state.approvals["0d1e2f3a-4b5c-4d6e-8f70-8192a3b4c5d6"]?.state).toBe("expired")
  })

  test("history records seeded open, credential mode without its gate, and a waiting legacy turn are all ended", () => {
    const r = new Rig("legacy")
    // the TUI started while the old daemon was still running a turn (a young reply-less Failed seed)
    r.resync([r.history("run the backup", null, "Failed", r.now - 20_000)])
    const seeded = r.state.order.map((id) => r.state.turns[id]).find((record) => record?.origin === "history")
    expect(seeded?.phase).toBe("running")
    // a V1 credential request (no gate id: it lives in the old process's memory only)
    r.evt({ kind: "turn.auth_required", turnId: null, extensionName: "github" })
    expect(r.state.composer.credential).not.toBeNull()
    const waiting = r.send("ok")
    r.now += 20_000
    r.dispatch({ type: "notice/tick" })
    expect(r.turn(waiting).notes).toEqual([NOTES.noReplyYet])

    r.reconnect(8_000, 2)
    r.resync([r.history("run the backup", null, "Failed", r.now - 50_000)])
    expect(seeded && r.state.turns[seeded.id]?.phase).toBe("lost")
    expect(r.turn(waiting)).toMatchObject({ phase: "lost", notes: [NOTES.daemonRestarted] })
    expect(r.state.composer.credential).toBeNull()
    expect(r.state.inflight).toEqual([])
    expect(r.state.daemonRestart).toBeNull()

    // a later resync of the same (new) daemon changes nothing
    const before = r.state
    r.resync([])
    expect(r.state.turns).toBe(before.turns)
  })
})
