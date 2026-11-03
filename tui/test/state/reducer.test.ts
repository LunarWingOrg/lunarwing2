/**
 * state/reducer.ts + state/correlate.ts (design 3.3, 3.5 including rule 3b, 3.6, 11.3.2) and the C1/C2 pins of
 * 3.13, plus the store and the event coalescer that feed the reducer. Time is explicit: every action carries
 * `at` from the harness clock, so no test sleeps.
 */
import { describe, expect, spyOn, test } from "bun:test"
import { ManualClock } from "@opentui/core/testing"
import type { Action } from "../../src/state/actions.ts"
import { createTurnIdSequence } from "../../src/state/ids.ts"
import {
  STATE_BOUNDS,
  createInitialState,
  type AppState,
  type PendingImage,
  type TurnId,
  type TurnRecord,
} from "../../src/state/model.ts"
import { reduce } from "../../src/state/reducer.ts"
import {
  NOTES,
  activeUserTurnCount,
  canStartTurn,
  focusedApproval,
  localInFlightCap,
  queuePosition,
} from "../../src/state/correlate.ts"
import { visibleText } from "../../src/state/turn-text.ts"
import { createStore } from "../../src/state/store.ts"
import { COALESCE_INTERVAL_MS, createCoalescer } from "../../src/state/coalesce.ts"
import { decodeGatewayEvent } from "../../src/transport/gateway-wire.ts"
import type {
  Capabilities,
  HistoryTurn,
  ServerInfo,
  TransportEvent,
  WireApproval,
} from "../../src/transport/types.ts"

// ---------------------------------------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------------------------------------

type WithoutAt<A> = A extends unknown ? Omit<A, "at"> : never
type ActionInput = WithoutAt<Action>

type Mode = "current" | "legacy" | "replv3" | "legacy-socket"

const THREAD = "5f0e4a52-6a3e-4a8f-9a1e-2b9d6c1e7a10"
const BASE = Date.UTC(2026, 10, 3, 9, 0, 0)

function capabilities(mode: Mode): Capabilities {
  const exact = mode === "current" || mode === "replv3"
  const socketLegacy = mode === "legacy-socket"
  return {
    streaming: mode === "legacy" ? "unknown" : !socketLegacy,
    activity: !socketLegacy,
    images: !socketLegacy,
    generatedImages: mode === "legacy" ? "unknown" : !socketLegacy,
    approvals: !socketLegacy,
    transcribe: false,
    correlation: exact,
    lifecycle: exact,
    interrupt: mode === "replv3" ? "native" : "text",
    steer: mode === "replv3" ? "native" : "text",
    threads: !socketLegacy,
    history: mode === "current" || mode === "legacy",
    backgroundMessages: !socketLegacy,
    resume: !socketLegacy,
  }
}

function serverInfo(mode: Mode, opts: { engine?: ServerInfo["engine"]; maxInFlight?: number } = {}): ServerInfo {
  return {
    transport: mode === "current" || mode === "legacy" ? "gateway" : "socket",
    protocol: mode === "current" ? "gateway-chat" : mode === "legacy" ? "gateway-legacy" : mode === "replv3" ? "replv3" : "repl-legacy",
    protocolVersion: null,
    serverVersion: null,
    agentName: null,
    engine: opts.engine ?? (mode === "legacy-socket" ? "v1" : "unknown"),
    sessionId: null,
    threadId: THREAD,
    endpointLabel: "http://127.0.0.1:10000",
    credential: "master",
    capabilities: capabilities(mode),
    limits: {
      maxContentBytes: 100_000,
      maxImages: 5,
      maxImageBytes: 5 * 1024 * 1024,
      maxTotalImageBytes: 7 * 1024 * 1024,
      maxRequestBytes: 10 * 1024 * 1024,
      imageMimeTypes: ["image/png", "image/jpeg", "image/gif", "image/webp"],
      maxTranscribeWavBytes: 0,
      transcribePerMinute: null,
      chatPerMinute: 30,
      maxInFlight: opts.maxInFlight ?? 8,
    },
    warnings: [],
  }
}

class Harness {
  state: AppState
  now = BASE
  private readonly nextId = createTurnIdSequence("t")

  /** `start`: the first Clock reading (default BASE, epoch-like); production's Clock is performance.now(). */
  constructor(mode: Mode, opts: { engine?: ServerInfo["engine"]; maxInFlight?: number; start?: number } = {}) {
    this.now = opts.start ?? BASE
    this.state = createInitialState({ profile: "test", tenant: null, view: { width: 80, height: 24 }, at: this.now })
    this.dispatch({ type: "conn/ready", info: serverInfo(mode, opts) })
  }

  dispatch(action: ActionInput): AppState {
    this.state = reduce(this.state, { ...action, at: this.now } as Action)
    return this.state
  }

  evt(event: TransportEvent): AppState {
    return this.dispatch({ type: "evt", event })
  }

  advance(ms: number): void {
    this.now += ms
  }

  /** turn/created then turn/sent, as controller/session.ts does around Transport.send(). */
  send(text: string, images: readonly PendingImage[] = []): TurnId {
    const turnId = this.create(text, images)
    this.dispatch({ type: "turn/sent", turnId })
    return turnId
  }

  create(text: string, images: readonly PendingImage[] = []): TurnId {
    const turnId = this.nextId()
    this.dispatch({ type: "turn/created", turnId, text, images })
    return turnId
  }

  control(kind: "interrupt" | "steer" | "clear" | "new", text: string): TurnId {
    const turnId = this.nextId()
    this.dispatch({ type: "control/created", turnId, kind, text })
    return turnId
  }

  /** A credential turn as the socket path creates it; `leakedText` stands for a controller that passed the secret. */
  credentialTurn(extensionName: string, leakedText: string): TurnId {
    const turnId = this.nextId()
    this.dispatch({ type: "turn/created", turnId, text: leakedText, images: [], credential: { extensionName } })
    return turnId
  }

  /** One raw legacy gateway SSE object through the real mapper (gateway-wire.ts, uncorrelated). */
  sse(raw: Record<string, unknown>): AppState {
    for (const event of decodeGatewayEvent({ thread_id: THREAD, ...raw }, { correlated: false, resolveTurnId: () => null })) this.evt(event)
    return this.state
  }

  turn(id: TurnId): TurnRecord {
    const record = this.state.turns[id]
    if (record === undefined) throw new Error(`no record ${id}`)
    return record
  }

  records(): TurnRecord[] {
    return this.state.order.flatMap((id) => {
      const record = this.state.turns[id]
      return record === undefined ? [] : [record]
    })
  }
}

function approval(id: string, overrides: Partial<WireApproval> = {}): WireApproval {
  return {
    id,
    threadId: THREAD,
    toolName: "shell",
    description: "Run a shell command",
    parameters: '{\n  "command": "ls"\n}',
    allowAlways: true,
    truncated: false,
    source: "live",
    ...overrides,
  }
}

function image(id: string, bytes = 64): PendingImage {
  return { id, mime: "image/png", bytes: new Uint8Array(bytes).fill(7), filename: `${id}.png`, width: 4, height: 4, source: "file" }
}

function iso(ms: number): string {
  return new Date(ms).toISOString()
}

function historyTurn(userInput: string, response: string | null, state: string, startedAt: number): HistoryTurn {
  return { userInput, response, state, startedAt: iso(startedAt), completedAt: response === null ? null : iso(startedAt + 1000) }
}

// ---------------------------------------------------------------------------------------------------------
// Correlation and sealing (3.5)
// ---------------------------------------------------------------------------------------------------------

describe("correlated transports (current gateway, REPLv3)", () => {
  test("queued turn receives its own final", () => {
    const h = new Harness("current")
    const a = h.send("first question")
    const b = h.send("second question")
    expect(h.turn(a).phase).toBe("sent")
    expect(h.turn(b).phase).toBe("queued")
    expect(queuePosition(h.state, b)).toBe(1)

    h.evt({ kind: "turn.started", turnId: a, threadId: THREAD })
    h.evt({ kind: "turn.delta", turnId: a, threadId: THREAD, text: "thinking about A" })
    // the queued turn's reply arrives (correlated) while A is still streaming: it must land on B, not on A
    h.evt({ kind: "turn.final", turnId: b, threadId: THREAD, text: "reply for B" })
    expect(h.turn(b).finalText).toBe("reply for B")
    expect(h.turn(a).finalText).toBeNull()
    expect(h.turn(a).deltas).toBe("thinking about A")

    h.evt({ kind: "turn.final", turnId: a, threadId: THREAD, text: "reply for A" })
    h.evt({ kind: "turn.finished", turnId: a, outcome: "replied" })
    h.evt({ kind: "turn.finished", turnId: b, outcome: "replied" })
    expect(h.turn(a)).toMatchObject({ phase: "done", finalText: "reply for A" })
    expect(h.turn(b)).toMatchObject({ phase: "done", finalText: "reply for B" })
    expect(h.state.inflight).toEqual([])
  })

  test("response then turn_finished seals once on both transports", () => {
    for (const mode of ["current", "replv3"] as const) {
      const h = new Harness(mode)
      const a = h.send("hello")
      if (mode === "replv3") h.evt({ kind: "turn.accepted", turnId: a })
      h.evt({ kind: "turn.started", turnId: a })
      h.evt({ kind: "turn.final", turnId: a, text: "hi there" })
      expect(h.turn(a).phase).toBe("running") // lifecycle: the final alone does not seal
      h.advance(10)
      h.evt({ kind: "turn.finished", turnId: a, outcome: "replied" })
      const sealed = h.turn(a)
      expect(sealed).toMatchObject({ phase: "done", finalText: "hi there", endedAt: h.now })
      expect(sealed.finish?.outcome).toBe("replied")
      expect(h.state.inflight).toEqual([])

      h.advance(10)
      const before = h.state
      h.evt({ kind: "turn.finished", turnId: a, outcome: "failed" }) // a second finish is ignored
      h.evt({ kind: "turn.final", turnId: a, text: "hi there" }) // a repeated final changes nothing
      h.evt({ kind: "turn.delta", turnId: a, text: "zombie" }) // late deltas are dropped
      expect(h.state.turns[a]).toBe(before.turns[a])
      expect(h.turn(a).notes).toEqual([])
    }
  })

  test("turn_finished before response still shows the late response", () => {
    const h = new Harness("current")
    const a = h.send("question")
    h.evt({ kind: "turn.started", turnId: a })
    h.evt({ kind: "turn.delta", turnId: a, text: "partial" })
    h.evt({ kind: "turn.finished", turnId: a, outcome: "replied" })
    expect(h.turn(a).phase).toBe("done")
    expect(visibleText(h.turn(a))).toBe("partial")

    const b = h.send("next")
    h.evt({ kind: "turn.final", turnId: a, text: "the full answer" })
    expect(h.turn(a)).toMatchObject({ phase: "done", finalText: "the full answer" })
    expect(h.turn(a).notes).toContain(NOTES.late)
    expect(visibleText(h.turn(a))).toBe("the full answer")
    expect(h.turn(b).finalText).toBeNull()
  })

  test("two finals for one id are joined", () => {
    const h = new Harness("replv3")
    const a = h.send("two parts please")
    h.evt({ kind: "turn.started", turnId: a })
    h.evt({ kind: "turn.final", turnId: a, text: "part one" })
    h.evt({ kind: "turn.final", turnId: a, text: "part two" })
    h.evt({ kind: "turn.finished", turnId: a, outcome: "replied" })
    expect(h.turn(a).finalText).toBe("part one\n\npart two")
    expect(h.turn(a).notes).toEqual([])
  })

  test("paused keeps the turn open and turn.started resumes it", () => {
    const h = new Harness("current")
    const a = h.send("delete the temp files")
    h.evt({ kind: "turn.started", turnId: a, threadId: THREAD })
    h.evt({ kind: "turn.approval", turnId: a, threadId: THREAD, approval: approval("req-1") })
    h.evt({ kind: "turn.finished", turnId: a, outcome: "paused" })
    expect(h.turn(a).phase).toBe("awaiting_approval")
    expect(h.state.inflight).toEqual([a])
    expect(focusedApproval(h.state)?.id).toBe("req-1")

    // progress while the prompt is still pending does not resume the turn
    h.evt({ kind: "turn.activity", turnId: a, activity: { kind: "status", text: "Awaiting approval" } })
    expect(h.turn(a).phase).toBe("awaiting_approval")

    h.dispatch({ type: "approval/answering", approvalId: "req-1", decision: "approve" })
    h.dispatch({ type: "approval/answered", approvalId: "req-1", ok: true, expired: false })
    expect(h.state.approvals["req-1"]).toMatchObject({ state: "resolved", decision: "approve" })
    expect(h.state.approvalQueue).toEqual([])
    h.evt({ kind: "turn.started", turnId: a, threadId: THREAD })
    expect(h.turn(a).phase).toBe("running")
    expect(h.turn(a).finish).toBeUndefined()
    h.evt({ kind: "turn.final", turnId: a, text: "Deleted 3 files." })
    h.evt({ kind: "turn.finished", turnId: a, outcome: "replied" })
    expect(h.turn(a)).toMatchObject({ phase: "done", finalText: "Deleted 3 files.", approvalIds: ["req-1"] })
  })

  test("no_reply with a pending approval stays awaiting_approval", () => {
    const h = new Harness("current")
    const a = h.send("do it")
    h.evt({ kind: "turn.approval", turnId: a, approval: approval("req-1") })
    h.evt({ kind: "turn.finished", turnId: a, outcome: "no_reply" })
    expect(h.turn(a).phase).toBe("awaiting_approval")
    expect(h.state.inflight).toContain(a)

    const b = h.send("ok")
    h.evt({ kind: "turn.finished", turnId: b, outcome: "no_reply" })
    expect(h.turn(b).phase).toBe("done")
    expect(h.turn(b).notes).toContain(NOTES.noReply)
  })

  test("detached shows background_running and the late finish seals", () => {
    const h = new Harness("current")
    const a = h.send("long job")
    h.evt({ kind: "turn.started", turnId: a })
    h.evt({ kind: "turn.delta", turnId: a, text: "working" })
    h.evt({ kind: "turn.finished", turnId: a, outcome: "detached" })
    expect(h.turn(a).phase).toBe("background_running")
    expect(h.state.inflight).toContain(a)
    expect(activeUserTurnCount(h.state)).toBe(0)

    h.advance(60_000)
    h.evt({ kind: "turn.final", turnId: a, text: "job finished" })
    expect(h.turn(a).phase).toBe("running")
    h.evt({ kind: "turn.finished", turnId: a, outcome: "replied" })
    expect(h.turn(a)).toMatchObject({ phase: "done", finalText: "job finished" })
    expect(h.state.inflight).toEqual([])
  })

  test("interrupted keeps deltas", () => {
    const h = new Harness("current")
    const a = h.send("write an essay")
    h.evt({ kind: "turn.started", turnId: a })
    h.evt({ kind: "turn.delta", turnId: a, text: "Once upon a time" })
    h.evt({ kind: "turn.finished", turnId: a, outcome: "interrupted" })
    expect(h.turn(a)).toMatchObject({ phase: "interrupted", deltas: "Once upon a time" })
    expect(visibleText(h.turn(a))).toBe("Once upon a time")
  })

  test("cancelled shows cancelled before it started", () => {
    const h = new Harness("current")
    h.send("first")
    const b = h.send("queued behind")
    h.evt({ kind: "turn.finished", turnId: b, outcome: "cancelled" })
    expect(h.turn(b).phase).toBe("interrupted")
    expect(h.turn(b).notes).toContain(NOTES.cancelled)
  })

  test("failed and timed_out take the message of a preceding error or final", () => {
    const h = new Harness("current")
    const a = h.send("x")
    h.evt({ kind: "turn.final", turnId: a, text: "The agent hit its time limit." })
    h.evt({ kind: "turn.finished", turnId: a, outcome: "timed_out" })
    expect(h.turn(a)).toMatchObject({ phase: "failed", error: { code: "timed_out", message: "The agent hit its time limit." } })
  })
})

describe("rule 3b: paused turns whose gate is discarded", () => {
  function pausedAtApproval(h: Harness, requestId: string): TurnId {
    const turnId = h.send("needs approval")
    h.evt({ kind: "turn.started", turnId, threadId: THREAD })
    h.evt({ kind: "turn.approval", turnId, threadId: THREAD, approval: approval(requestId) })
    h.evt({ kind: "turn.finished", turnId, outcome: "paused" })
    expect(h.turn(turnId).phase).toBe("awaiting_approval")
    return turnId
  }

  test("interrupt while awaiting approval seals the paused turn", () => {
    const h = new Harness("current")
    const a = pausedAtApproval(h, "req-1")
    const stop = h.control("interrupt", "/interrupt")
    h.evt({ kind: "turn.started", turnId: stop })
    h.evt({ kind: "turn.final", turnId: stop, text: "Interrupted." })
    h.evt({ kind: "turn.finished", turnId: stop, outcome: "replied" })
    expect(h.turn(stop)).toMatchObject({ phase: "done", control: { kind: "interrupt", ack: "Interrupted." } })
    expect(h.turn(a).phase).toBe("interrupted")
    expect(h.turn(a).notes).toContain("interrupted while waiting for approval")
    expect(h.state.approvals["req-1"]?.state).toBe("expired")
    expect(h.state.approvalQueue).toEqual([])
    expect(h.state.inflight).toEqual([])

    // "Nothing to interrupt." leaves a paused turn alone
    const b = pausedAtApproval(h, "req-2")
    const idle = h.control("interrupt", "/interrupt")
    h.evt({ kind: "turn.final", turnId: idle, text: "Nothing to interrupt." })
    expect(h.turn(b).phase).toBe("awaiting_approval")
    expect(h.state.approvalQueue).toEqual(["req-2"])

    // /clear forwarded as a control has the same effect
    const clear = h.control("clear", "/clear")
    h.evt({ kind: "turn.final", turnId: clear, text: "Conversation cleared." })
    expect(h.turn(b).phase).toBe("interrupted")
    expect(h.turn(b).notes).toContain("conversation cleared while waiting for approval")
  })

  test("gate_resolved thread_terminated seals", () => {
    const h = new Harness("current")
    const a = pausedAtApproval(h, "req-1")
    h.evt({ kind: "turn.approval_resolved", approvalId: "req-1", resolution: "terminated" })
    expect(h.turn(a).phase).toBe("interrupted")
    expect(h.turn(a).notes).toContain(NOTES.threadTerminated)
    expect(h.state.approvals["req-1"]?.state).toBe("resolved")
    expect(h.state.approvalQueue).toEqual([])
  })

  test("gate_resolved expired seals with a note", () => {
    const h = new Harness("current")
    const a = pausedAtApproval(h, "req-1")
    h.evt({ kind: "turn.approval_resolved", approvalId: "req-1", resolution: "expired" })
    expect(h.turn(a)).toMatchObject({ phase: "interrupted", deltas: "" })
    expect(h.turn(a).notes).toContain(NOTES.approvalExpired)
    expect(h.state.approvals["req-1"]?.state).toBe("expired")

    // approve, deny, ... resume the turn instead
    const b = pausedAtApproval(h, "req-2")
    h.evt({ kind: "turn.approval_resolved", approvalId: "req-2", resolution: "deny" })
    expect(h.turn(b).phase).toBe("running")
    expect(h.state.approvals["req-2"]).toMatchObject({ state: "resolved", decision: "deny" })
  })

  test("auth-cancel 2xx seals the owning turn", () => {
    const h = new Harness("current")
    const a = h.send("connect my github")
    h.evt({ kind: "turn.started", turnId: a })
    h.evt({ kind: "turn.auth_required", turnId: a, extensionName: "github", requestId: "gate-9" })
    h.evt({ kind: "turn.finished", turnId: a, outcome: "paused" })
    expect(h.turn(a).phase).toBe("awaiting_input")
    h.dispatch({ type: "composer/credential_chars", chars: 4 })
    h.dispatch({ type: "composer/credential_esc_armed", armed: true })
    h.dispatch({ type: "composer/credential_done", outcome: "cancelled" })
    expect(h.state.composer.credential).toBeNull()
    expect(h.turn(a).phase).toBe("interrupted")
    expect(h.turn(a).notes).toContain(NOTES.credentialCancelled)

    // credential mode ending without sending seals the same way and warns
    const b = h.send("connect slack")
    h.evt({ kind: "turn.auth_required", turnId: b, extensionName: "slack" })
    h.evt({ kind: "turn.finished", turnId: b, outcome: "paused" })
    h.dispatch({ type: "composer/credential_done", outcome: "ended" })
    expect(h.turn(b).phase).toBe("interrupted")
    expect(h.turn(b).notes).toContain(NOTES.credentialEnded)
    expect(h.state.notices.map((notice) => notice.text)).toContain(NOTES.credentialEnded)
  })

  test("paused turns do not count against the in-flight cap", () => {
    const h = new Harness("current")
    expect(localInFlightCap(h.state)).toBe(STATE_BOUNDS.maxLocalInFlight)
    const ids = Array.from({ length: 8 }, (_, index) => h.send(`turn ${index}`))
    expect(activeUserTurnCount(h.state)).toBe(8)
    expect(canStartTurn(h.state)).toBe(false)

    const [first, second] = ids as [TurnId, TurnId]
    h.evt({ kind: "turn.approval", turnId: first, approval: approval("req-1") })
    h.evt({ kind: "turn.finished", turnId: first, outcome: "paused" })
    expect(activeUserTurnCount(h.state)).toBe(7)
    expect(canStartTurn(h.state)).toBe(true)
    h.evt({ kind: "turn.finished", turnId: second, outcome: "detached" })
    expect(activeUserTurnCount(h.state)).toBe(6)

    // the server's own cap wins when it is lower
    const small = new Harness("replv3", { maxInFlight: 2 })
    expect(localInFlightCap(small.state)).toBe(2)
    small.send("a")
    small.send("b")
    expect(canStartTurn(small.state)).toBe(false)
  })
})

describe("legacy daemons: FIFO and control acks (rule 5)", () => {
  test("three queued turns seal FIFO without correlation", () => {
    const h = new Harness("legacy")
    const a = h.send("one")
    const b = h.send("two")
    const c = h.send("three")
    expect([h.turn(b).phase, h.turn(c).phase]).toEqual(["queued", "queued"])
    for (const text of ["reply one", "reply two", "reply three"]) h.evt({ kind: "turn.final", turnId: null, threadId: THREAD, text })
    expect([a, b, c].map((id) => [h.turn(id).phase, h.turn(id).finalText])).toEqual([
      ["done", "reply one"],
      ["done", "reply two"],
      ["done", "reply three"],
    ])
  })

  test("exact ids override FIFO", () => {
    const h = new Harness("current")
    const a = h.send("one")
    const b = h.send("two")
    // B is answered first; with exact ids it stays with B even though A is the oldest open turn
    h.evt({ kind: "turn.final", turnId: b, text: "reply two" })
    h.evt({ kind: "turn.finished", turnId: b, outcome: "replied" })
    expect(h.turn(b)).toMatchObject({ phase: "done", finalText: "reply two" })
    expect(h.turn(a)).toMatchObject({ phase: "sent", finalText: null })
    // on a correlated transport an id-less final is never FIFO-matched
    h.evt({ kind: "turn.final", turnId: null, text: "broadcast" })
    expect(h.turn(a).finalText).toBeNull()
  })

  test("interrupt ack seals the control and marks the running turn interrupted (legacy)", () => {
    const h = new Harness("legacy")
    const a = h.send("long task")
    h.evt({ kind: "turn.activity", turnId: null, activity: { kind: "thinking", text: "Processing..." } })
    h.evt({ kind: "turn.delta", turnId: null, text: "partial output" })
    expect(h.turn(a).phase).toBe("running")
    const stop = h.control("interrupt", "/interrupt")
    h.evt({ kind: "turn.final", turnId: null, text: "Interrupted." })
    expect(h.turn(stop)).toMatchObject({ phase: "done", control: { kind: "interrupt", ack: "Interrupted." } })
    expect(h.turn(a)).toMatchObject({ phase: "interrupted", deltas: "partial output", finalText: null })
    expect(h.state.strayInterruptAck).toEqual({ until: h.now + 5000 })
    expect(h.state.controls).toEqual([])
    expect(h.state.inflight).toEqual([])
  })

  test("interrupt ack does not close the follow-up", () => {
    const h = new Harness("legacy")
    const a = h.send("long task")
    h.evt({ kind: "turn.activity", turnId: null, activity: { kind: "thinking", text: "Processing..." } })
    // Ctrl+S: the interrupt, then the follow-up message
    const stop = h.control("interrupt", "/interrupt")
    const followUp = h.send("do this instead")
    h.evt({ kind: "turn.final", turnId: null, text: "Interrupted." }) // the control's ack
    h.evt({ kind: "turn.final", turnId: null, text: "Interrupted." }) // V1 also answers the aborted turn
    expect(h.turn(stop).phase).toBe("done")
    expect(h.turn(a).phase).toBe("interrupted")
    expect(h.turn(followUp).phase).not.toBe("done")
    expect(h.turn(followUp).finalText).toBeNull()
    h.evt({ kind: "turn.final", turnId: null, text: "Done, using the new plan." })
    expect(h.turn(followUp)).toMatchObject({ phase: "done", finalText: "Done, using the new plan." })

    // with nothing older open, the ack never ends the message sent after the interrupt (same millisecond)
    const solo = new Harness("legacy")
    solo.control("interrupt", "/interrupt")
    const only = solo.send("new message")
    solo.evt({ kind: "turn.final", turnId: null, text: "Interrupted." })
    expect(solo.turn(only).phase).toBe("sent")
    expect(solo.state.strayInterruptAck).toBeNull()
  })

  test("stray second Interrupted. swallowed once within 5 s (legacy V1)", () => {
    const h = new Harness("legacy")
    h.send("long task")
    h.evt({ kind: "turn.activity", turnId: null, activity: { kind: "thinking", text: "Processing..." } })
    h.control("interrupt", "/interrupt")
    h.evt({ kind: "turn.final", turnId: null, text: "Interrupted." })
    const next = h.send("next question")
    h.advance(4_000)
    h.evt({ kind: "turn.final", turnId: null, text: "Interrupted." })
    expect(h.state.strayInterruptAck).toBeNull()
    expect(h.turn(next).finalText).toBeNull()
    // only once: a further "Interrupted." is an ordinary reply again
    h.evt({ kind: "turn.final", turnId: null, text: "Interrupted." })
    expect(h.turn(next)).toMatchObject({ phase: "done", finalText: "Interrupted." })

    // after 5 s the window is closed (the tick clears it)
    const late = new Harness("legacy")
    late.send("x")
    late.evt({ kind: "turn.activity", turnId: null, activity: { kind: "thinking", text: "..." } })
    late.control("interrupt", "/interrupt")
    late.evt({ kind: "turn.final", turnId: null, text: "Interrupted." })
    late.advance(5_000)
    late.dispatch({ type: "notice/tick" })
    expect(late.state.strayInterruptAck).toBeNull()

    // V2 answers an interrupt once: no window
    const v2 = new Harness("legacy", { engine: "v2" })
    v2.send("x")
    v2.evt({ kind: "turn.activity", turnId: null, activity: { kind: "thinking", text: "..." } })
    v2.control("interrupt", "/interrupt")
    v2.evt({ kind: "turn.final", turnId: null, text: "Interrupted." })
    expect(v2.state.strayInterruptAck).toBeNull()
  })

  test("steer ack attaches to the steer record", () => {
    for (const mode of ["current", "legacy"] as const) {
      const h = new Harness(mode)
      const a = h.send("refactor the module")
      if (mode === "current") h.evt({ kind: "turn.started", turnId: a })
      else h.evt({ kind: "turn.activity", turnId: null, activity: { kind: "thinking", text: "Processing..." } })
      const steer = h.control("steer", "/steer use the staging database")
      expect(h.turn(a).steerNotes).toEqual([{ text: "use the staging database" }])
      const ack = "Steer note queued — the agent will see it after the current step."
      h.evt({ kind: "turn.final", turnId: mode === "current" ? steer : null, text: ack })
      expect(h.turn(steer)).toMatchObject({ phase: "done", control: { kind: "steer", ack } })
      expect(h.turn(a).steerNotes).toEqual([{ text: "use the staging database", ack }])
      expect(h.turn(a).phase).toBe("running")
      expect(h.turn(a).finalText).toBeNull()
    }
  })

  test("a message sent while a turn waits at an approval gets the daemon's immediate answer, not the paused turn", () => {
    // V2 answers a message on a gated thread at once (router.rs has_scoped_pending_gate); V1 says "Waiting for
    // approval" (thread_ops.rs). Neither may seal the paused turn; its continuation after the answer is its own.
    for (const engine of ["v2", "v1"] as const) {
      const h = new Harness("legacy")
      const a = h.send("clean up the temp files")
      h.sse({ type: "thinking", message: "Processing..." })
      h.sse({ type: "approval_needed", request_id: "req-1", tool_name: "shell", description: "Run", parameters: '{"command":"rm /tmp/x*"}', allow_always: false })
      expect(h.turn(a).phase).toBe("awaiting_approval")
      const b = h.send("what does that command do?")
      const rejection =
        engine === "v2"
          ? "This thread is waiting on a pending gate. Resolve it or use /interrupt before sending another message."
          : "Waiting for approval: shell — Run. Use /interrupt to cancel."
      h.sse({ type: "response", content: rejection })
      expect(h.turn(b)).toMatchObject({ phase: "done", finalText: rejection })
      expect(h.turn(a).phase).toBe("awaiting_approval")

      h.dispatch({ type: "approval/answering", approvalId: "req-1", decision: "approve" })
      h.dispatch({ type: "approval/answered", approvalId: "req-1", ok: true, expired: false })
      h.sse({ type: "tool_started", name: "shell" })
      h.sse({ type: "response", content: "Deleted 3 temp files." })
      expect(h.turn(a)).toMatchObject({ phase: "done", finalText: "Deleted 3 temp files." })
      expect(h.turn(a).activity.map((entry) => entry.kind)).toContain("tool_started")
      expect(h.turn(b).finalText).toBe(rejection)
      expect(h.records().filter((record) => record.origin === "background")).toHaveLength(0)
    }
  })

  test("a failed V2 model recovery fails only its own turn, not the follow-up queued behind it", () => {
    const h = new Harness("legacy")
    const a = h.send("summarize the logs")
    h.sse({ type: "thinking", message: "Processing..." })
    const b = h.send("and list the errors")
    const error = "The model returned invalid output after one repair attempt"
    h.sse({ type: "turn_recovery", recovery_state: "failed", failure_kind: "invalid_output", message: error, recovery_attempt: 1 })
    h.sse({ type: "thread_state_changed", from_state: "Running", to_state: "Failed" })
    h.sse({ type: "response", content: `Error: ${error}` })
    expect(h.turn(a)).toMatchObject({ phase: "failed", error: { message: error } })
    expect(h.turn(b).phase).toBe("queued")
    h.sse({ type: "response", content: "Errors: none found." })
    expect(h.turn(b)).toMatchObject({ phase: "done", finalText: "Errors: none found." })
    expect(h.records().filter((record) => record.origin === "background")).toHaveLength(0)
  })

  test("an approval answered with 404 (gate already gone) ends its paused turn, so the next reply is not misattributed", () => {
    const h = new Harness("legacy")
    const a = h.send("clean up")
    h.sse({ type: "approval_needed", request_id: "req-1", tool_name: "shell", description: "d", parameters: "{}", allow_always: false })
    expect(h.turn(a).phase).toBe("awaiting_approval")
    h.dispatch({ type: "approval/answering", approvalId: "req-1", decision: "approve" })
    h.dispatch({ type: "approval/answered", approvalId: "req-1", ok: false, expired: true })
    expect(h.turn(a)).toMatchObject({ phase: "interrupted" })
    expect(h.turn(a).notes).toContain(NOTES.approvalExpired)
    expect(h.state.inflight).not.toContain(a)

    const b = h.send("ok, then just show disk usage")
    h.sse({ type: "thinking", message: "Processing..." })
    h.sse({ type: "response", content: "/dev/sda1 42% used" })
    expect(h.turn(b)).toMatchObject({ phase: "done", finalText: "/dev/sda1 42% used" })
    expect(h.turn(a).finalText).toBeNull()
    // the sweep's gate_resolved(expired) arrives later and changes nothing
    h.sse({ type: "gate_resolved", request_id: "req-1", resolution: "expired" })
    expect(h.turn(a).phase).toBe("interrupted")
  })

  test("when every inflight turn is paused the oldest one still takes uncorrelated output", () => {
    const h = new Harness("legacy")
    const a = h.send("first")
    h.sse({ type: "approval_needed", request_id: "req-1", tool_name: "shell", description: "d", parameters: "{}", allow_always: false })
    h.sse({ type: "response", content: "unexpected" })
    expect(h.turn(a)).toMatchObject({ phase: "done", finalText: "unexpected" })
  })

  test("final with nothing inflight becomes background", () => {
    for (const mode of ["legacy", "current"] as const) {
      const h = new Harness(mode)
      h.evt({ kind: "turn.final", turnId: null, threadId: THREAD, text: "Good morning! Your 9:00 reminder." })
      const [record] = h.records()
      expect(record).toMatchObject({ origin: "background", phase: "done", finalText: "Good morning! Your 9:00 reminder." })
      expect(h.state.inflight).toEqual([])
    }
  })

  test("a second final for a sealed turn does not eat the next reply", () => {
    const h = new Harness("current")
    const a = h.send("first")
    h.evt({ kind: "turn.final", turnId: a, text: "answer A" })
    h.evt({ kind: "turn.finished", turnId: a, outcome: "replied" })
    const b = h.send("second")
    h.evt({ kind: "turn.final", turnId: a, text: "answer A" }) // duplicate
    h.evt({ kind: "turn.final", turnId: a, text: "addendum for A" }) // late text for A
    expect(h.turn(b).finalText).toBeNull()
    expect(h.turn(a).finalText).toBe("answer A\n\naddendum for A")
    h.evt({ kind: "turn.final", turnId: b, text: "answer B" })
    h.evt({ kind: "turn.finished", turnId: b, outcome: "replied" })
    expect(h.turn(b)).toMatchObject({ phase: "done", finalText: "answer B" })
  })

  test("uncorrelated response on a correlated gateway becomes background", () => {
    const h = new Harness("current")
    const a = h.send("question")
    h.evt({ kind: "turn.started", turnId: a })
    h.evt({ kind: "turn.final", turnId: null, threadId: THREAD, text: "routine finished" })
    expect(h.turn(a)).toMatchObject({ phase: "running", finalText: null })
    const background = h.records().find((record) => record.origin === "background")
    expect(background).toMatchObject({ phase: "done", finalText: "routine finished" })

    // another client's correlated turn on this thread gets a background record of its own
    const foreign = "web-client-turn-1" as TurnId
    h.evt({ kind: "turn.delta", turnId: foreign, threadId: THREAD, text: "streamed for the web UI" })
    h.evt({ kind: "turn.final", turnId: foreign, threadId: THREAD, text: "web UI answer" })
    expect(h.turn(foreign)).toMatchObject({ origin: "background", phase: "done", finalText: "web UI answer" })
    expect(h.turn(a).finalText).toBeNull()
  })

  test("legacy: 15 s without events marks unanswered", () => {
    const h = new Harness("legacy")
    const a = h.send("ok")
    h.advance(14_999)
    h.dispatch({ type: "notice/tick" })
    expect(h.turn(a).phase).toBe("sent")
    h.advance(1)
    h.dispatch({ type: "notice/tick" })
    expect(h.turn(a).phase).toBe("unanswered")
    expect(h.turn(a).notes).toContain(NOTES.unanswered)
    expect(h.state.inflight).toEqual([])

    // a turn that produced an event is never unanswered
    const b = h.send("real question")
    h.evt({ kind: "turn.activity", turnId: null, activity: { kind: "thinking", text: "Processing..." } })
    h.advance(60_000)
    h.dispatch({ type: "notice/tick" })
    expect(h.turn(b).phase).toBe("running")

    // correlated transports and the silent legacy socket do not use the rule
    for (const mode of ["current", "legacy-socket"] as const) {
      const other = new Harness(mode)
      const c = other.send("ok")
      other.advance(60_000)
      other.dispatch({ type: "notice/tick" })
      expect(other.turn(c).phase).toBe("sent")
    }
  })
})

describe("approvals, errors, threads and connection", () => {
  test("approvals deduplicate across live, history and pending-gates", () => {
    const h = new Harness("current")
    const a = h.send("clean up")
    h.evt({ kind: "turn.approval", turnId: a, threadId: THREAD, approval: approval("req-1") })
    h.evt({ kind: "turn.approval", turnId: a, threadId: THREAD, approval: approval("req-1") })
    h.dispatch({
      type: "resync/applied",
      snapshot: {
        threadId: THREAD,
        turns: [],
        approvals: [approval("req-1", { source: "history" }), approval("req-1", { source: "pending_gates" })],
      },
    })
    expect(Object.keys(h.state.approvals)).toEqual(["req-1"])
    expect(h.state.approvalQueue).toEqual(["req-1"])
    expect(h.state.approvals["req-1"]).toMatchObject({ source: "live", turnId: a, state: "pending", viewedFull: false })
    expect(h.turn(a).approvalIds).toEqual(["req-1"])
    expect(h.state.approvals["req-1"]?.focusedAt).toBe(h.now)

    // a prompt first seen without an owner takes the owner the live event names
    h.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, turns: [], approvals: [approval("req-2", { source: "pending_gates" })] } })
    expect(h.state.approvals["req-2"]?.turnId).toBeNull()
    h.evt({ kind: "turn.approval", turnId: a, approval: approval("req-2") })
    expect(h.state.approvals["req-2"]?.turnId).toBe(a)
    expect(h.state.approvalQueue).toEqual(["req-1", "req-2"])
  })

  test("a history copy (no allow_always) never hides a pending-gates copy that allows always", () => {
    const h = new Harness("legacy")
    h.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, turns: [], approvals: [approval("req-1", { source: "history", allowAlways: false })] } })
    expect(h.state.approvals["req-1"]?.allowAlways).toBe(false)
    h.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, turns: [], approvals: [approval("req-1", { source: "pending_gates", allowAlways: true })] } })
    expect(h.state.approvals["req-1"]).toMatchObject({ allowAlways: true, state: "pending" })
    // a later copy without it does not take it away again
    h.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, turns: [], approvals: [approval("req-1", { source: "history", allowAlways: false })] } })
    expect(h.state.approvals["req-1"]?.allowAlways).toBe(true)
  })

  test("a pending authentication gate in the snapshot restores credential mode; another active credential is kept", () => {
    const h = new Harness("legacy")
    const gate = { requestId: "gate-1", threadId: THREAD, extensionName: "github", instructions: "Paste a token" }
    h.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, turns: [], approvals: [], authGates: [gate] } })
    expect(h.state.composer.credential).toMatchObject({ extensionName: "github", requestId: "gate-1", threadId: THREAD, instructions: "Paste a token" })

    const other = new Harness("legacy")
    other.evt({ kind: "turn.auth_required", turnId: null, threadId: THREAD, extensionName: "linear" })
    other.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, turns: [], approvals: [], authGates: [gate] } })
    expect(other.state.composer.credential?.extensionName).toBe("linear")
    // the same extension learns the gate's ids
    other.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, turns: [], approvals: [], authGates: [{ ...gate, extensionName: "linear" }] } })
    expect(other.state.composer.credential).toMatchObject({ extensionName: "linear", requestId: "gate-1" })
    // later resyncs leave an active request alone (no new object: the masked buffer is never reset)
    const credential = other.state.composer.credential
    other.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, turns: [], approvals: [], authGates: [{ ...gate, extensionName: "linear" }] } })
    expect(other.state.composer.credential).toBe(credential)
  })

  test("approval 404 marks expired", () => {
    const h = new Harness("current")
    const a = h.send("x")
    h.evt({ kind: "turn.approval", turnId: a, approval: approval("req-1") })
    h.evt({ kind: "turn.approval", turnId: a, approval: approval("req-2", { toolName: "http" }) })
    h.dispatch({ type: "approval/answering", approvalId: "req-1", decision: "approve" })
    h.advance(500)
    h.dispatch({ type: "approval/answered", approvalId: "req-1", ok: false, expired: true })
    expect(h.state.approvals["req-1"]?.state).toBe("expired")
    expect(h.state.approvalQueue).toEqual(["req-2"])
    // the next prompt is focused now, so its 1 s freshness window starts now
    expect(h.state.approvals["req-2"]?.focusedAt).toBe(h.now)
    expect(h.turn(a).notes).toContain(NOTES.approvalExpired)
    expect(h.state.notices.at(-1)).toMatchObject({ level: "warn", text: "approval expired: shell" })

    // a transport failure leaves the prompt answerable
    h.dispatch({ type: "approval/answering", approvalId: "req-2", decision: "deny" })
    h.dispatch({ type: "approval/answered", approvalId: "req-2", ok: false, expired: false, message: "timeout" })
    expect(h.state.approvals["req-2"]?.state).toBe("pending")
    expect(h.state.approvalQueue).toEqual(["req-2"])
  })

  test("focus moves restart the 1 s window and the full view marks the prompt viewed", () => {
    const h = new Harness("current")
    const a = h.send("two tools")
    h.evt({ kind: "turn.approval", turnId: a, approval: approval("req-1") })
    h.advance(300)
    h.evt({ kind: "turn.approval", turnId: a, approval: approval("req-2") })
    expect(h.state.approvals["req-1"]?.focusedAt).toBe(BASE)
    expect(h.state.approvals["req-2"]).toMatchObject({ focusedAt: null, createdAt: BASE + 300 })
    h.advance(700)
    h.dispatch({ type: "approval/focus_next" })
    expect(h.state.approvalQueue).toEqual(["req-2", "req-1"])
    expect(focusedApproval(h.state)).toMatchObject({ id: "req-2", focusedAt: h.now })
    h.dispatch({ type: "approval/focused", approvalId: "req-1" })
    expect(h.state.approvalQueue).toEqual(["req-1", "req-2"])
    // focusing the prompt that is already focused does not restart its window
    const focusedAt = h.state.approvals["req-1"]?.focusedAt
    h.advance(50)
    h.dispatch({ type: "approval/focused", approvalId: "req-1" })
    expect(h.state.approvals["req-1"]?.focusedAt).toBe(focusedAt ?? -1)

    expect(h.state.approvals["req-1"]?.viewedFull).toBe(false)
    h.dispatch({ type: "panel/open", kind: "approval" })
    expect(h.state.panel).toEqual({ kind: "approval", scroll: 0, approvalId: "req-1" })
    expect(h.state.approvals["req-1"]?.viewedFull).toBe(true)
    expect(h.state.approvals["req-2"]?.viewedFull).toBe(false)
    h.dispatch({ type: "panel/scroll", delta: -5 })
    expect(h.state.panel?.scroll).toBe(0)
    h.dispatch({ type: "panel/scroll", delta: 7 })
    expect(h.state.panel?.scroll).toBe(7)
    h.dispatch({ type: "panel/close" })
    expect(h.state.panel).toBeNull()

    // a confirmation for a prompt that goes away is cleared with it
    h.dispatch({ type: "approval/confirm_requested", approvalId: "req-2", decision: "approve" })
    h.evt({ kind: "turn.approval_resolved", approvalId: "req-2", resolution: "approve" })
    expect(h.state.composer.confirm).toBeNull()
  })

  test("timed composer states end on the tick", () => {
    const h = new Harness("current")
    const a = h.send("connect")
    h.evt({ kind: "turn.approval", turnId: a, approval: approval("req-1") })
    h.dispatch({ type: "approval/confirm_requested", approvalId: "req-1", decision: "deny" })
    h.dispatch({ type: "composer/guard", text: "ok", reason: "approval_keyword", until: h.now + 10_000 })
    h.evt({ kind: "turn.auth_required", turnId: a, extensionName: "github" })
    h.dispatch({ type: "composer/credential_esc_armed", armed: true })
    h.advance(1_999)
    h.dispatch({ type: "notice/tick" })
    expect(h.state.composer.credential?.escArmedAt).toBe(h.now - 1_999)
    h.advance(1)
    h.dispatch({ type: "notice/tick" })
    expect(h.state.composer.credential?.escArmedAt).toBeNull()
    expect(h.state.composer.confirm).not.toBeNull()
    h.advance(8_000)
    h.dispatch({ type: "notice/tick" })
    expect(h.state.composer.confirm).toBeNull()
    expect(h.state.composer.guard).toBeNull()
  })

  test("error fails only its turn", () => {
    const h = new Harness("current")
    const a = h.send("one")
    const b = h.send("two")
    h.evt({ kind: "turn.error", turnId: a, code: "agent_error", message: "Error: tool crashed", retryable: false })
    expect(h.turn(a)).toMatchObject({ phase: "failed", error: { code: "agent_error", message: "Error: tool crashed" } })
    expect(h.turn(a).finalText).toBeNull() // never rendered as assistant text
    expect(h.turn(b).phase).toBe("queued")
    h.evt({ kind: "turn.finished", turnId: a, outcome: "replied" })
    expect(h.turn(a).phase).toBe("failed")

    // legacy: an id-less error fails the oldest open turn only; without one it is a notice
    const legacy = new Harness("legacy")
    const c = legacy.send("one")
    const d = legacy.send("two")
    legacy.evt({ kind: "turn.error", turnId: null, code: "agent_error", message: "boom", retryable: false })
    expect(legacy.turn(c).phase).toBe("failed")
    expect(legacy.turn(d).phase).toBe("queued")
    h.evt({ kind: "turn.error", turnId: null, code: "server", message: "stream broke", retryable: true })
    expect(h.state.notices.at(-1)).toMatchObject({ level: "error", sticky: true, text: "stream broke" })
  })

  test("other-thread events only bump the counter", () => {
    const h = new Harness("current")
    const a = h.send("mine")
    const before = h.records().length
    const other = "11111111-2222-4333-8444-555555555555"
    h.evt({ kind: "turn.delta", turnId: null, threadId: other, text: "chunk" })
    h.evt({ kind: "turn.final", turnId: null, threadId: other, text: "a reply elsewhere" })
    h.evt({ kind: "turn.final", turnId: a, threadId: other, text: "not for this thread" })
    h.evt({ kind: "turn.approval", turnId: null, threadId: other, approval: approval("req-9", { threadId: other }) })
    h.evt({ kind: "other_thread", threadId: other })
    expect(h.state.otherThreadsUnread).toBe(4)
    expect(h.records()).toHaveLength(before)
    expect(h.turn(a).finalText).toBeNull()
    expect(h.state.approvals).toEqual({})
    expect(h.state.notices.map((notice) => notice.text)).toEqual([NOTES.otherThreadApproval])

    // selecting another thread starts a fresh transcript and resets the counter
    h.dispatch({ type: "thread/selected", threadId: other })
    expect(h.state).toMatchObject({ threadId: other, otherThreadsUnread: 0, order: [], inflight: [] })
  })

  test("conn/lost marks legacy socket turns lost and leaves gateway turns open", () => {
    const socket = new Harness("legacy-socket")
    const a = socket.send("over the socket")
    const stop = socket.control("interrupt", "/interrupt")
    socket.dispatch({ type: "conn/lost", reason: "socket closed", attempt: 1, retryAt: socket.now + 500, resumable: false })
    expect(socket.turn(a).phase).toBe("lost")
    expect(socket.turn(a).notes).toContain(NOTES.lost)
    expect(socket.turn(stop).phase).toBe("lost")
    expect(socket.state.inflight).toEqual([])
    expect(socket.state.conn).toEqual({ kind: "reconnecting", attempt: 1, retryAt: socket.now + 500, lastError: "socket closed" })

    const gateway = new Harness("current")
    const b = gateway.send("over the gateway")
    gateway.evt({ kind: "turn.started", turnId: b })
    gateway.dispatch({ type: "conn/lost", reason: "stream ended", attempt: 1, retryAt: gateway.now + 500, resumable: true })
    expect(gateway.turn(b).phase).toBe("running")
    expect(gateway.state.inflight).toEqual([b])
    gateway.dispatch({ type: "conn/connecting", attempt: 1 })
    gateway.dispatch({ type: "conn/ready", info: serverInfo("current") })
    expect(gateway.state.conn.kind).toBe("ready")
    expect(gateway.turn(b).phase).toBe("running")
  })

  test("resync seals open turns from terminal history turns and keeps young Failed turns open", () => {
    const h = new Harness("current")
    const a = h.send("hello")
    h.advance(1_000)
    const b = h.send("second question")
    h.advance(1_000)
    const c = h.send("third question")
    h.advance(30_000)
    const snapshot = {
      threadId: THREAD,
      approvals: [],
      turns: [
        historyTurn("an older turn", "older answer", "Completed", BASE - 60_000),
        historyTurn("hello", "hi there", "Completed", BASE + 200),
        historyTurn("second question", null, "Failed", BASE + 1_200),
        historyTurn("third question", null, "Processing", BASE + 2_200),
      ],
    }
    const [phaseB, phaseC] = [h.turn(b).phase, h.turn(c).phase]
    expect([phaseB, phaseC]).toEqual(["queued", "queued"])
    h.dispatch({ type: "resync/applied", snapshot })
    expect(h.turn(a)).toMatchObject({ phase: "done", finalText: "hi there", recovered: true })
    expect(h.turn(b).phase).toBe(phaseB) // a young Failed turn may still be running (durable V2 history)
    expect(h.turn(c).phase).toBe(phaseC)
    expect(h.state.inflight).toEqual([b, c])
    expect(h.records().filter((record) => record.origin === "history")).toHaveLength(0) // no duplicates after the seed

    h.advance(610_000)
    h.dispatch({ type: "resync/applied", snapshot })
    expect(h.turn(b)).toMatchObject({ phase: "failed", recovered: true, error: { code: "failed" } })
    expect(h.turn(c).phase).toBe(phaseC) // still Processing on the server

    // a fresh window seeds sealed history records, oldest first, before anything live
    const fresh = new Harness("current")
    fresh.evt({ kind: "turn.final", turnId: null, threadId: THREAD, text: "proactive" })
    fresh.dispatch({ type: "resync/applied", snapshot: { ...snapshot, turns: snapshot.turns.slice(0, 2) } })
    expect(fresh.records().map((record) => [record.origin, record.phase, record.finalText])).toEqual([
      ["history", "done", "older answer"],
      ["history", "done", "hi there"],
      ["background", "done", "proactive"],
    ])
  })

  test("history times are converted to the Clock domain: a performance.now() clock never matches an older turn", () => {
    // Production's Clock counts from process start; history times are epoch milliseconds (wallAt converts them).
    const PERF_START = 75_000
    const h = new Harness("legacy", { start: PERF_START })
    const wallNow = (): number => BASE + (h.now - PERF_START)
    const yesterday = historyTurn("continue", "Step 3 done.", "Completed", BASE - 86_400_000)
    h.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, approvals: [], turns: [yesterday] }, wallAt: wallNow() })
    const seeded = h.records().find((record) => record.origin === "history")
    expect(seeded?.createdAt).toBe(PERF_START - 86_400_000)

    h.advance(1_000)
    const b = h.send("continue")
    h.evt({ kind: "turn.activity", turnId: null, threadId: THREAD, activity: { kind: "thinking", text: "Processing..." } })
    h.advance(3_000)
    const running = historyTurn("continue", null, "Processing", wallNow() - 2_500)
    h.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, approvals: [], turns: [yesterday, running] }, wallAt: wallNow() })
    expect(h.turn(b)).toMatchObject({ phase: "running", finalText: null })
    expect(h.turn(b).recovered).toBeUndefined()

    // the same history, once the server finished the turn, does seal it
    const done = { ...running, response: "Step 4 done.", state: "Completed", completedAt: iso(wallNow()) }
    h.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, approvals: [], turns: [yesterday, done] }, wallAt: wallNow() })
    expect(h.turn(b)).toMatchObject({ phase: "done", finalText: "Step 4 done.", recovered: true })
  })

  test("an image-only send is not sealed with the reply of an hour-old image-only turn (performance.now() clock)", () => {
    const PERF_START = 183
    const h = new Harness("legacy", { start: PERF_START })
    const wallNow = (): number => BASE + (h.now - PERF_START)
    const shot = historyTurn("\n\n<attachments><image/></attachments>", "a screenshot of a terminal", "Completed", BASE - 3_600_000)
    h.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, approvals: [], turns: [shot] }, wallAt: wallNow() })
    h.advance(2_000)
    const c = h.send("", [image("img-9")])
    h.evt({ kind: "turn.activity", turnId: null, threadId: THREAD, activity: { kind: "thinking", text: "Processing..." } })
    h.advance(10_000)
    const running = historyTurn("\n\n<attachments><image/></attachments>", null, "Processing", wallNow() - 9_000)
    h.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, approvals: [], turns: [shot, running] }, wallAt: wallNow() })
    expect(h.turn(c)).toMatchObject({ phase: "running", finalText: null })
  })

  test("a V2 turn still running when the history is seeded (reply-less Failed, young) stays open and takes its live reply", () => {
    const h = new Harness("legacy")
    const running = historyTurn("run the build", null, "Failed", BASE - 20_000)
    const old = historyTurn("an hour ago", null, "Failed", BASE - 3_600_000)
    h.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, approvals: [], turns: [old, running] } })
    const [stale, open] = h.records()
    expect(stale).toMatchObject({ origin: "history", phase: "failed", error: { message: NOTES.failedWithoutReply } })
    expect(open).toMatchObject({ origin: "history", phase: "running" })
    expect(open?.error).toBeUndefined()

    h.sse({ type: "stream_chunk", content: "Build " })
    h.sse({ type: "response", content: "Build finished: ok" })
    const sealed = h.state.turns[open?.id ?? ("" as TurnId)]
    expect(sealed).toMatchObject({ phase: "done", finalText: "Build finished: ok" })
    expect(h.records().filter((record) => record.origin === "background")).toHaveLength(0)

    // a later resync with the finished turn keeps the one record
    const finished = { ...running, response: "Build finished: ok", state: "Completed", completedAt: iso(BASE) }
    h.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, approvals: [], turns: [old, finished] } })
    expect(h.records().map((record) => [record.origin, record.phase])).toEqual([
      ["history", "failed"],
      ["history", "done"],
    ])
  })

  test("current gateway: a young reply-less Failed seed stays open until a resync shows it finished", () => {
    const h = new Harness("current")
    const running = historyTurn("run the build", null, "Failed", BASE - 20_000)
    h.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, approvals: [], turns: [running] } })
    expect(h.records()[0]?.phase).toBe("running")
    const finished = { ...running, response: "Build finished: ok", state: "Completed", completedAt: iso(BASE) }
    h.advance(5_000)
    h.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, approvals: [], turns: [finished] } })
    expect(h.records()[0]).toMatchObject({ phase: "done", finalText: "Build finished: ok", recovered: true })
  })

  test("auth_required enters credential mode and stores no text", () => {
    const h = new Harness("current")
    const a = h.send("connect github")
    h.evt({
      kind: "turn.auth_required",
      turnId: a,
      threadId: THREAD,
      extensionName: "github",
      requestId: "gate-1",
      instructions: "Paste a fine-grained token",
      authUrl: "https://github.example.invalid/settings/tokens",
    })
    expect(h.state.composer.credential).toEqual({
      extensionName: "github",
      threadId: THREAD,
      turnId: a,
      startedAt: h.now,
      chars: 0,
      escArmedAt: null,
      requestId: "gate-1",
      instructions: "Paste a fine-grained token",
      authUrl: "https://github.example.invalid/settings/tokens",
    })
    h.dispatch({ type: "composer/credential_chars", chars: 40 })
    expect(h.state.composer.credential?.chars).toBe(40)

    // even a controller that passed the secret along would not get it stored: a credential turn has no text
    const secret = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB"
    const sent = h.credentialTurn("github", secret)
    h.dispatch({ type: "composer/credential_done", outcome: "sent" })
    expect(h.turn(sent)).toMatchObject({ credential: { extensionName: "github" }, input: { text: "", images: [] } })
    expect(h.state.composer.credential).toBeNull()
    expect(JSON.stringify(h.state)).not.toContain(secret)
    expect(h.state.composer.history).toEqual([])
  })

  test("a V2 gate_resolved for the credential request leaves credential mode", () => {
    const h = new Harness("current")
    const a = h.send("connect github")
    h.evt({ kind: "turn.auth_required", turnId: a, extensionName: "github", requestId: "gate-1" })
    h.evt({ kind: "turn.finished", turnId: a, outcome: "paused" })
    h.evt({ kind: "turn.approval_resolved", approvalId: "gate-1", resolution: "cancelled" })
    expect(h.state.composer.credential).toBeNull()
    expect(h.turn(a).phase).toBe("interrupted")
    expect(h.state.notices.map((notice) => notice.text)).toContain(NOTES.credentialEnded)
  })
})

describe("composer, images, notices and bounds", () => {
  test("send failure restores the draft into an empty composer", () => {
    const h = new Harness("current")
    const picture = image("img-1")
    h.dispatch({ type: "composer/attach", image: picture })
    const turnId = h.create("hello with a picture", [picture])
    expect(h.state.composer.pendingImages).toEqual([])
    h.dispatch({ type: "turn/send_failed", turnId, code: "rate_limited", message: "429 rate limited", restore: true })
    expect(h.turn(turnId)).toMatchObject({ phase: "failed", error: { code: "rate_limited", message: "429 rate limited" } })
    expect(h.state.composer.restore?.text).toBe("hello with a picture")
    expect(h.state.composer.restore?.images.map((pending) => pending.id)).toEqual(["img-1"])
    expect(h.state.composer.pendingImages.map((pending) => pending.id)).toEqual(["img-1"])
    expect(h.state.composer.pendingImages[0]?.bytes).toEqual(picture.bytes)
    // the controller re-attaching the same image changes nothing
    h.dispatch({ type: "composer/attach", image: picture })
    expect(h.state.composer.pendingImages).toHaveLength(1)
    h.dispatch({ type: "composer/restore_consumed" })
    expect(h.state.composer.restore).toBeNull()
  })

  test("a failed send whose turn already has events is neither failed nor restored", () => {
    const h = new Harness("current")
    const turnId = h.create("already answered")
    h.evt({ kind: "turn.started", turnId })
    h.dispatch({ type: "turn/send_failed", turnId, code: "timeout", message: "POST timed out", restore: true })
    expect(h.turn(turnId).phase).toBe("running")
    expect(h.turn(turnId).notes).toContain("the daemon received this message; sending it reported: POST timed out")
    expect(h.state.composer.restore).toBeNull()
    expect(h.state.composer.history).toEqual([])
  })

  test("send failure with a new draft moves the text to history", () => {
    const h = new Harness("current")
    const turnId = h.create("first draft")
    h.dispatch({ type: "composer/started" })
    h.dispatch({ type: "turn/send_failed", turnId, code: "unavailable", message: "503", restore: false })
    expect(h.state.composer.restore).toBeNull()
    expect(h.state.composer.history).toEqual(["first draft"])
    expect(h.state.notices.at(-1)?.text).toBe(NOTES.sendFailedToHistory)
    expect(h.turn(turnId).phase).toBe("failed")
  })

  test("image tool without image event adds the note", () => {
    const h = new Harness("legacy") // generatedImages "unknown" before M2a
    const a = h.send("draw a cat")
    h.evt({ kind: "turn.activity", turnId: null, activity: { kind: "tool_completed", text: "image_generate (8123ms)", ok: true } })
    h.evt({ kind: "turn.final", turnId: null, text: "Here is your cat." })
    expect(h.turn(a).phase).toBe("done")
    expect(h.turn(a).notes).toContain(NOTES.imageNotDelivered)

    // with the image delivered there is no note, and the engine is now known to deliver images
    const b = h.send("draw a dog")
    h.evt({ kind: "turn.activity", turnId: null, activity: { kind: "tool_completed", text: "image_generate", ok: true } })
    h.evt({ kind: "turn.image", turnId: null, image: { mime: "image/png", bytes: new Uint8Array([137, 80, 78, 71]), caption: "dog.png" } })
    h.evt({ kind: "turn.final", turnId: null, text: "Here is your dog." })
    expect(h.turn(b).notes).not.toContain(NOTES.imageNotDelivered)
    expect(h.turn(b).images).toHaveLength(1)
    expect(h.turn(b).images[0]).toMatchObject({ mime: "image/png", byteLength: 4, caption: "dog.png" })
    expect(h.state.server?.capabilities.generatedImages).toBe(true)

    // a failed tool call is not an image that went missing
    const c = h.send("draw a bird")
    h.evt({ kind: "turn.activity", turnId: null, activity: { kind: "tool_completed", text: "image_generate", ok: false } })
    h.evt({ kind: "turn.final", turnId: null, text: "That did not work." })
    expect(h.turn(c).notes).not.toContain(NOTES.imageNotDelivered)
  })

  test("eviction keeps 500 records and 20 image payloads", () => {
    const h = new Harness("legacy")
    for (let index = 0; index < 40; index += 1) {
      h.send(`picture ${index}`, [image(`img-${index}`)])
      h.evt({ kind: "turn.final", turnId: null, text: `got picture ${index}` })
    }
    for (let index = 0; index < 470; index += 1) h.evt({ kind: "background", text: `notice ${index}` })
    expect(h.state.order).toHaveLength(STATE_BOUNDS.maxTurns)
    expect(Object.keys(h.state.turns)).toHaveLength(STATE_BOUNDS.maxTurns)
    const users = h.records().filter((record) => record.origin === "user")
    expect(users).toHaveLength(30) // the 10 oldest went first
    expect(users[0]?.input?.text).toBe("picture 10")
    const withBytes = users.filter((record) => record.input?.images[0]?.bytes !== null)
    expect(withBytes).toHaveLength(STATE_BOUNDS.maxImagePayloads)
    expect(withBytes[0]?.input?.text).toBe("picture 20")
    const evicted = users[0]?.input?.images[0]
    expect(evicted).toMatchObject({ bytes: null, byteLength: 64 })
  })

  test("[ui] max_turns_in_memory (ui.maxTurns) bounds the records; open turns are never evicted", () => {
    const h = new Harness("legacy")
    h.state = { ...h.state, ui: { ...h.state.ui, maxTurns: 10 } }
    const open = h.send("still running")
    for (let index = 0; index < 30; index += 1) h.evt({ kind: "background", text: `notice ${index}` })
    expect(h.state.order).toHaveLength(10)
    expect(h.state.order[0]).toBe(open)
    expect(h.turn(open).phase).toBe("sent")
    expect(h.records().at(-1)?.finalText).toBe("notice 29")
    expect(createInitialState({ profile: "p", tenant: null, view: { width: 80, height: 24 }, at: 0 }).ui.maxTurns).toBe(STATE_BOUNDS.maxTurns)
  })

  test("notice expiry by level, errors sticky", () => {
    const h = new Harness("current")
    h.dispatch({ type: "notice/push", notice: { level: "info", source: "client", text: "copied 12 characters" } })
    h.dispatch({ type: "notice/push", notice: { level: "warn", source: "voice", text: "LunarVoice busy, used hex-app" } })
    h.dispatch({ type: "notice/push", notice: { level: "error", source: "transport", text: "send failed" } })
    expect(h.state.notices.map((notice) => [notice.level, notice.sticky, notice.expiresAt])).toEqual([
      ["info", false, h.now + 6_000],
      ["warn", false, h.now + 12_000],
      ["error", true, null],
    ])
    const start = h.now
    h.advance(5_999)
    h.dispatch({ type: "notice/tick" })
    expect(h.state.notices).toHaveLength(3)
    h.now = start + 6_000
    h.dispatch({ type: "notice/tick" })
    expect(h.state.notices.map((notice) => notice.level)).toEqual(["warn", "error"])
    h.now = start + 12_000
    h.dispatch({ type: "notice/tick" })
    expect(h.state.notices.map((notice) => notice.level)).toEqual(["error"])
    h.now = start + 3_600_000
    h.dispatch({ type: "notice/tick" })
    expect(h.state.notices.map((notice) => notice.text)).toEqual(["send failed"])

    // a newer error replaces the sticky one; Esc dismisses the newest
    h.dispatch({ type: "notice/push", notice: { level: "error", source: "server", text: "approval failed" } })
    expect(h.state.notices.map((notice) => notice.text)).toEqual(["approval failed"])
    h.dispatch({ type: "notice/dismiss" })
    expect(h.state.notices).toEqual([])

    // the same id replaces in place instead of stacking; the list is capped
    h.dispatch({ type: "notice/push", notice: { level: "warn", source: "auth", text: "first", id: "w", sticky: true } })
    h.dispatch({ type: "notice/push", notice: { level: "warn", source: "auth", text: "second", id: "w", sticky: true } })
    expect(h.state.notices.map((notice) => notice.text)).toEqual(["second"])
    for (let index = 0; index < 30; index += 1) h.dispatch({ type: "notice/push", notice: { level: "info", source: "client", text: `n${index}` } })
    expect(h.state.notices).toHaveLength(STATE_BOUNDS.maxNotices)
  })

  test("server strings are sanitized before they are stored", () => {
    const h = new Harness("current")
    const a = h.send("x")
    h.evt({ kind: "turn.delta", turnId: a, text: "\u001b[31mred\u001b[0m" })
    h.evt({ kind: "turn.activity", turnId: a, activity: { kind: "tool_started", text: "shell\nrm -rf /\u202e" } })
    h.evt({ kind: "turn.final", turnId: a, text: "line one\r\nline two\u0007" })
    expect(h.turn(a).deltas).toBe("red")
    expect(h.turn(a).activity[0]?.text).toBe("shell rm -rf /")
    expect(h.turn(a).finalText).toBe("line one\nline two")
  })

  test("approval text is escaped, never stripped, so an escape sequence cannot hide parameters", () => {
    const h = new Harness("current")
    const a = h.send("x")
    const parameters = '{"command": "ls \u001b]0;title; rm -rf ~"}'
    h.evt({ kind: "turn.approval", turnId: a, approval: approval("req-1", { parameters, toolName: "shell\u001b[2K" }) })
    const prompt = h.state.approvals["req-1"]
    expect(prompt?.parameters).toBe('{"command": "ls \\u{1b}]0;title; rm -rf ~"}')
    expect(prompt?.toolName).toBe("shell\\u{1b}[2K")
    // a reply is stripped as usual
    h.evt({ kind: "turn.final", turnId: a, text: "ok \u001b]0;gone" })
    expect(h.turn(a).finalText).toBe("ok ")
  })

  test("reducer never reads Date.now (spy)", () => {
    const dateNow = spyOn(Date, "now")
    const perfNow = spyOn(performance, "now")
    const random = spyOn(Math, "random")
    const uuid = spyOn(crypto, "randomUUID")
    try {
      for (const mode of ["current", "legacy"] as const) {
        const h = new Harness(mode)
        h.dispatch({ type: "view/resize", width: 100, height: 30 })
        h.dispatch({ type: "composer/attach", image: image("img-1") })
        const a = h.send("hello", [image("img-1")])
        h.evt({ kind: "turn.started", turnId: a })
        h.evt({ kind: "turn.delta", turnId: a, text: "partial" })
        h.evt({ kind: "turn.activity", turnId: a, activity: { kind: "thinking", text: "Processing..." } })
        h.evt({ kind: "turn.image", turnId: a, image: { mime: "image/png", bytes: new Uint8Array([1, 2, 3]) } })
        h.evt({ kind: "turn.approval", turnId: a, approval: approval("req-1") })
        h.dispatch({ type: "approval/focus_next" })
        h.dispatch({ type: "panel/open", kind: "approval" })
        h.dispatch({ type: "approval/answering", approvalId: "req-1", decision: "deny" })
        h.dispatch({ type: "approval/answered", approvalId: "req-1", ok: true, expired: false })
        h.evt({ kind: "turn.final", turnId: a, text: "done" })
        h.evt({ kind: "turn.finished", turnId: a, outcome: "replied" })
        h.control("interrupt", "/interrupt")
        h.evt({ kind: "turn.final", turnId: null, text: "Interrupted." })
        h.evt({ kind: "notice", level: "info", source: "job", text: "job started" })
        h.dispatch({ type: "notice/push", notice: { level: "error", source: "client", text: "oops" } })
        h.dispatch({ type: "resync/applied", snapshot: { threadId: THREAD, turns: [historyTurn("hello", "done", "Completed", BASE)], approvals: [] } })
        h.dispatch({ type: "conn/lost", reason: "gone", attempt: 1, retryAt: h.now + 500, resumable: false })
        h.dispatch({ type: "notice/tick" })
        h.dispatch({ type: "voice/state", patch: { phase: "recording" } })
        h.dispatch({ type: "ui/blur" })
        h.dispatch({ type: "conn/offline", reason: "quit", fatal: false })
      }
      expect(dateNow).not.toHaveBeenCalled()
      expect(perfNow).not.toHaveBeenCalled()
      expect(random).not.toHaveBeenCalled()
      expect(uuid).not.toHaveBeenCalled()
    } finally {
      dateNow.mockRestore()
      perfNow.mockRestore()
      random.mockRestore()
      uuid.mockRestore()
    }
  })
})

describe("store and coalescer", () => {
  test("the store notifies once per dispatch or batch, and not for no-ops", () => {
    const initial = createInitialState({ profile: "test", tenant: null, view: { width: 80, height: 24 }, at: 0 })
    const store = createStore(initial)
    let calls = 0
    const unsubscribe = store.subscribe(() => {
      calls += 1
    })
    store.dispatch({ type: "view/resize", width: 80, height: 24, at: 1 }) // unchanged
    expect(calls).toBe(0)
    store.dispatch({ type: "view/resize", width: 100, height: 30, at: 2 })
    expect(calls).toBe(1)
    store.dispatchMany([
      { type: "ui/blur", at: 3 },
      { type: "ui/focus", at: 4 },
      { type: "view/resize", width: 120, height: 40, at: 5 },
    ])
    expect(calls).toBe(2)
    expect(store.getState().view).toEqual({ width: 120, height: 40 })
    unsubscribe()
    store.dispatch({ type: "ui/blur", at: 6 })
    expect(calls).toBe(2)
  })

  test("the coalescer merges consecutive deltas of one turn and flushes at most every 33 ms", () => {
    const clock = new ManualClock()
    clock.setTime(BASE)
    const batches: Action[][] = []
    const tapped: string[] = []
    const coalescer = createCoalescer({
      store: { dispatchMany: (actions) => batches.push([...actions]) },
      clock,
      tap: (event) => tapped.push(event.kind),
    })
    const a = "t-1" as TurnId
    const b = "t-2" as TurnId
    coalescer.push({ kind: "turn.delta", turnId: a, text: "Hel" })
    coalescer.push({ kind: "turn.delta", turnId: a, text: "lo" })
    coalescer.push({ kind: "turn.delta", turnId: b, text: "other" })
    coalescer.push({ kind: "turn.delta", turnId: a, text: " world" })
    expect(coalescer.pending).toBe(3)
    expect(batches).toHaveLength(0)
    clock.advance(0) // the first batch after a quiet period goes out at once
    expect(batches).toHaveLength(1)
    expect(batches[0]?.map((action) => (action.type === "evt" && action.event.kind === "turn.delta" ? action.event.text : ""))).toEqual([
      "Hello",
      "other",
      " world",
    ])
    expect(tapped).toHaveLength(4)

    // the next flush waits until 33 ms after the previous one
    clock.advance(5)
    coalescer.push({ kind: "turn.final", turnId: a, text: "Hello world" })
    clock.advance(COALESCE_INTERVAL_MS - 6)
    expect(batches).toHaveLength(1)
    clock.advance(1)
    expect(batches).toHaveLength(2)
    expect(batches[1]?.[0]).toMatchObject({ type: "evt", at: BASE + 5, event: { kind: "turn.final" } })

    // dispose flushes what is queued and ignores later events
    coalescer.push({ kind: "turn.finished", turnId: a, outcome: "replied" })
    coalescer.dispose()
    expect(batches).toHaveLength(3)
    coalescer.push({ kind: "turn.delta", turnId: a, text: "late" })
    clock.runAll()
    expect(batches).toHaveLength(3)
    expect(clock.pendingTimerCount).toBe(0)
  })
})
