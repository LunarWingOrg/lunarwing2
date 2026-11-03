/**
 * GatewayTransport against the mock gateway in legacy and current modes (design 11.3.3). Real HTTP and SSE on
 * 127.0.0.1 with an ephemeral port; time is a ManualClock (timeouts and the watchdog only fire when advanced);
 * the listener-owner check reads injected /proc tables, never the host's.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { ManualClock } from "@opentui/core/testing"
import { Secret } from "../../src/config/secret.ts"
import type { TokenKind } from "../../src/config/types.ts"
import type { TurnId } from "../../src/state/model.ts"
import { isTransportError, TransportError, type TransportErrorCode } from "../../src/transport/errors.ts"
import { EventChannel } from "../../src/transport/event-channel.ts"
import { GatewayTransport, type FetchLike, type GatewayTransportOptions } from "../../src/transport/gateway.ts"
import type { ProcReader } from "../../src/transport/listener-owner.ts"
import type { ServerInfo, TransportEvent } from "../../src/transport/types.ts"
import {
  fixtureToken,
  MemoryStateFile,
  MemoryTokenSource,
  MockGateway,
  procReaderFor,
  TINY_PNG,
  type MockGatewayOptions,
} from "../mocks/mock-gateway.ts"

const TOKEN = fixtureToken("test")
const UID = process.getuid?.() ?? 1000
const TURN = "turn-1" as TurnId
const MIB = 1024 * 1024

interface Rig {
  mock: MockGateway
  transport: GatewayTransport
  clock: ManualClock
  token: MemoryTokenSource
  state: MemoryStateFile
}

interface RigOptions {
  mock?: Partial<MockGatewayOptions>
  transport?: Partial<GatewayTransportOptions>
  tokenKind?: TokenKind
  tokenSource?: string
}

const cleanups: (() => Promise<unknown>)[] = []

afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
})

async function rig(mode: "legacy" | "current", opts: RigOptions = {}): Promise<Rig> {
  const mock = await MockGateway.start({ mode, token: TOKEN, ...opts.mock })
  cleanups.push(() => mock.stop())
  const clock = new ManualClock()
  const token = new MemoryTokenSource(TOKEN, opts.tokenKind ?? "file", opts.tokenSource ?? "tokens/test.token")
  const state = new MemoryStateFile()
  const transport = new GatewayTransport({
    baseUrl: mock.url,
    token,
    thread: { kind: "last" },
    clock,
    timezone: "Europe/Berlin",
    stateFile: state,
    expectedListener: { uid: UID, label: "test" },
    readProc: procReaderFor(mock.port, UID),
    env: {},
    ...opts.transport,
  })
  cleanups.push(() => transport.close())
  return { mock, transport, clock, token, state }
}

function connect(r: Rig): Promise<ServerInfo> {
  return r.transport.connect(new AbortController().signal)
}

function iterate(r: Rig): AsyncIterator<TransportEvent> {
  return r.transport.events()[Symbol.asyncIterator]()
}

async function next(it: AsyncIterator<TransportEvent>, match: (e: TransportEvent) => boolean = () => true): Promise<TransportEvent> {
  for (let i = 0; i < 200; i += 1) {
    const result = await it.next()
    if (result.done) throw new Error("the event stream ended")
    if (match(result.value)) return result.value
  }
  throw new Error("no matching event")
}

async function rejection(promise: Promise<unknown>): Promise<TransportError> {
  try {
    await promise
  } catch (error) {
    if (isTransportError(error)) return error
    throw error
  }
  throw new Error("expected a TransportError")
}

const text = (status: number, body: string) => new Response(body, { status, headers: { "content-type": "text/plain" } })
const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } })
const routes = (mock: MockGateway) => mock.requests.map((rec) => `${rec.method} ${rec.path}`)
const completedTurn = (user_input: string, response: string) => ({
  user_input,
  response,
  state: "Completed",
  started_at: "2026-11-03T00:10:02Z",
  completed_at: "2026-11-03T00:10:05Z",
})

describe("connect (3.8 steps 1-6)", () => {
  test("opens SSE before any send, resolves thread `last` from the state file and seeds history", async () => {
    const r = await rig("current")
    const thread = r.mock.addThread([completedTurn("hi", "hello"), completedTurn("how are you", "fine")])
    await r.state.update(() => ({ lastThreadId: thread.id }))

    const info = await connect(r)
    expect(info.threadId).toBe(thread.id)
    expect(routes(r.mock).slice(0, 2)).toEqual(["GET /api/health", "GET /api/chat/events"])
    const historyQueries = r.mock.requestsTo("GET /api/chat/history").map((rec) => rec.query)
    expect(historyQueries).toEqual([
      { thread_id: thread.id, limit: "1" },
      { thread_id: thread.id, limit: "30" },
    ])

    // The connect-time history seed is handed out without another request.
    const seed = await r.transport.resync()
    expect(seed.threadId).toBe(thread.id)
    expect(seed.turns.map((t) => [t.userInput, t.response, t.state])).toEqual([
      ["hi", "hello", "Completed"],
      ["how are you", "fine", "Completed"],
    ])
    expect(r.mock.requestsTo("GET /api/chat/history")).toHaveLength(2)

    await r.transport.send({ turnId: TURN, text: "ping", images: [] })
    const send = r.mock.requestsTo("POST /api/chat/send")[0]
    expect(send?.openStreams).toBe(1)
    expect(routes(r.mock).indexOf("GET /api/chat/events")).toBeLessThan(routes(r.mock).indexOf("POST /api/chat/send"))
    expect(r.mock.requestsTo("GET /api/health")[0]?.headers.authorization).toBeUndefined()
  })

  test("`last` without a stored thread, or with one the gateway no longer knows, creates a new thread", async () => {
    const r = await rig("legacy")
    await r.state.update(() => ({ lastThreadId: "5f0e4a52-6a3e-4a8f-9a1e-2b9d6c1e7a10" }))
    const info = await connect(r)
    expect(r.mock.requestsTo("POST /api/chat/thread/new")).toHaveLength(1)
    expect(info.threadId).not.toBe("5f0e4a52-6a3e-4a8f-9a1e-2b9d6c1e7a10")
    expect((await r.state.read()).lastThreadId).toBe(info.threadId ?? "")
  })

  test("`new` creates a thread and remembers it", async () => {
    const r = await rig("current", { transport: { thread: { kind: "new" } } })
    const info = await connect(r)
    expect(r.mock.requestsTo("POST /api/chat/thread/new")).toHaveLength(1)
    expect(r.mock.threads.has(info.threadId ?? "")).toBe(true)
    expect((await r.state.read()).lastThreadId).toBe(info.threadId ?? "")
  })

  test("`assistant` uses assistant_thread; without one a new thread is used with a warning", async () => {
    const r = await rig("current", { transport: { thread: { kind: "assistant" } } })
    const info = await connect(r)
    expect(info.threadId).toBe(r.mock.assistantThreadId)
    expect(r.mock.requestsTo("POST /api/chat/thread/new")).toHaveLength(0)

    const bare = await rig("current", { mock: { assistantThread: false }, transport: { thread: { kind: "assistant" } } })
    const fallback = await connect(bare)
    expect(bare.mock.requestsTo("POST /api/chat/thread/new")).toHaveLength(1)
    expect(fallback.warnings).toContain("this gateway has no assistant thread; started a new thread")
  })

  test("reconnecting keeps the thread instead of resolving it again", async () => {
    const r = await rig("current", { transport: { thread: { kind: "new" } } })
    const first = await connect(r)
    const second = await connect(r)
    expect(second.threadId).toBe(first.threadId ?? "")
    expect(r.mock.requestsTo("POST /api/chat/thread/new")).toHaveLength(1)
    expect(r.mock.requestsTo("GET /api/chat/events")).toHaveLength(2)
  })

  test("capabilities fold onto ServerInfo in current mode", async () => {
    const r = await rig("current")
    const info = await connect(r)
    expect(info).toMatchObject({
      transport: "gateway",
      protocol: "gateway-chat",
      protocolVersion: 1,
      serverVersion: "2.1.9-mock",
      engine: "v2",
      credential: "master",
      endpointLabel: r.mock.url.origin,
    })
    expect(info.capabilities).toMatchObject({ correlation: true, lifecycle: true, streaming: true, transcribe: true, generatedImages: true })
    expect(info.limits.maxTranscribeWavBytes).toBe(18 * MIB)
    expect(info.limits.maxRequestBytes).toBe(10 * MIB)
    expect(info.warnings).toContain("this TUI uses the tenant master token")
  })

  test("a 404 from /api/chat/capabilities selects legacy mode", async () => {
    const r = await rig("legacy")
    const info = await connect(r)
    expect(info).toMatchObject({ protocol: "gateway-legacy", protocolVersion: null, engine: "unknown", credential: "unknown" })
    expect(info.capabilities).toMatchObject({
      correlation: false,
      lifecycle: false,
      streaming: "unknown",
      generatedImages: "unknown",
      transcribe: true,
    })
    expect(info.limits).toMatchObject({ maxImages: 5, maxImageBytes: 5 * MIB, maxTotalImageBytes: 7 * MIB, maxTranscribeWavBytes: 18 * MIB })

    const mute = await rig("legacy", { mock: { voiceConfigured: false } })
    const muted = await connect(mute)
    expect(muted.capabilities.transcribe).toBe(false)
    expect(muted.limits.maxTranscribeWavBytes).toBe(0)
  })

  test("503 'Too many connections' on the event stream is busy, retried after 30 s", async () => {
    const r = await rig("current", { mock: { maxStreams: 0 } })
    const error = await rejection(connect(r))
    expect(error.code).toBe("busy")
    expect(error.fatal).toBe(false)
    expect(error.retryAfterMs).toBe(30_000)
  })

  test("a buffered response that history already holds is dropped once (step 6)", async () => {
    const mock = await MockGateway.start({ mode: "legacy", token: TOKEN })
    cleanups.push(() => mock.stop())
    const thread = mock.addThread([completedTurn("hi", "hello")])
    const encoder = new TextEncoder()
    let stream: ReadableStreamDefaultController<Uint8Array> | null = null
    const sse = (event: Record<string, unknown>) =>
      stream?.enqueue(encoder.encode(`event: ${String(event.type)}\ndata: ${JSON.stringify(event)}\n\n`))
    // A scripted event stream: its events are queued while connect() is still fetching capabilities and history.
    const fetchImpl: FetchLike = async (input, init) => {
      const url = new URL(input instanceof Request ? input.url : String(input))
      if (url.pathname === "/api/chat/events") {
        const body = new ReadableStream<Uint8Array>({ start: (c) => void (stream = c) })
        init?.signal?.addEventListener("abort", () => stream?.error(init.signal?.reason))
        return new Response(body, { headers: { "content-type": "text/event-stream" } })
      }
      if (url.pathname === "/api/chat/capabilities") {
        sse({ type: "response", content: "hello", thread_id: thread.id })
        sse({ type: "thinking", message: "Processing...", thread_id: thread.id })
        sse({ type: "response", content: "hello", thread_id: thread.id })
      }
      return fetch(input, init)
    }
    const transport = new GatewayTransport({
      baseUrl: mock.url,
      token: new MemoryTokenSource(TOKEN),
      thread: { kind: "id", id: thread.id },
      clock: new ManualClock(),
      timezone: "",
      stateFile: new MemoryStateFile(),
      fetchImpl,
      expectedListener: { uid: UID, label: "test" },
      readProc: procReaderFor(mock.port, UID),
      env: {},
    })
    cleanups.push(() => transport.close())
    await transport.connect(new AbortController().signal)
    const it = transport.events()[Symbol.asyncIterator]()
    expect((await next(it)).kind).toBe("turn.activity")
    expect(await next(it)).toEqual({ kind: "turn.final", turnId: null, threadId: thread.id, text: "hello" })
  })

  test("the buffered tail of the turn history already completed is dropped with its response (step 6)", async () => {
    // The seed is applied before buffered events, so this tail would open a background record that never ends.
    const mock = await MockGateway.start({ mode: "legacy", token: TOKEN })
    cleanups.push(() => mock.stop())
    const thread = mock.addThread([completedTurn("check the disk", "Disk check done: the disk is 91% full.")])
    const encoder = new TextEncoder()
    let stream: ReadableStreamDefaultController<Uint8Array> | null = null
    const sse = (event: Record<string, unknown>) =>
      stream?.enqueue(encoder.encode(`event: ${String(event.type)}\ndata: ${JSON.stringify(event)}\n\n`))
    const fetchImpl: FetchLike = async (input, init) => {
      const url = new URL(input instanceof Request ? input.url : String(input))
      if (url.pathname === "/api/chat/events") {
        const body = new ReadableStream<Uint8Array>({ start: (c) => void (stream = c) })
        init?.signal?.addEventListener("abort", () => stream?.error(init.signal?.reason))
        return new Response(body, { headers: { "content-type": "text/event-stream" } })
      }
      if (url.pathname === "/api/chat/capabilities") {
        sse({ type: "thinking", message: "Processing...", thread_id: thread.id })
        sse({ type: "stream_chunk", content: "...and the disk is 91% full.", thread_id: thread.id })
        sse({ type: "status", message: "Done", thread_id: thread.id })
        sse({ type: "stream_chunk", content: "another thread", thread_id: crypto.randomUUID() })
        sse({ type: "response", content: "Disk check done: the disk is 91% full.", thread_id: thread.id })
        sse({ type: "thinking", message: "next turn", thread_id: thread.id })
      }
      return fetch(input, init)
    }
    const transport = new GatewayTransport({
      baseUrl: mock.url,
      token: new MemoryTokenSource(TOKEN),
      thread: { kind: "id", id: thread.id },
      clock: new ManualClock(),
      timezone: "",
      stateFile: new MemoryStateFile(),
      fetchImpl,
      expectedListener: { uid: UID, label: "test" },
      readProc: procReaderFor(mock.port, UID),
      env: {},
    })
    cleanups.push(() => transport.close())
    await transport.connect(new AbortController().signal)
    const it = transport.events()[Symbol.asyncIterator]()
    // another thread's event is kept (it only bumps a counter), the completed turn's tail is gone
    expect(await next(it)).toMatchObject({ kind: "turn.delta", text: "another thread" })
    expect(await next(it)).toMatchObject({ kind: "turn.activity", threadId: thread.id, activity: { kind: "thinking", text: "next turn" } })
  })
})

describe("events", () => {
  test("a current-mode turn arrives correlated: started, progress, final, then finished", async () => {
    const r = await rig("current")
    await connect(r)
    const it = iterate(r)
    await r.transport.send({ turnId: TURN, text: "ping", images: [] })
    const kinds: string[] = []
    for (;;) {
      const event = await next(it)
      kinds.push(event.kind)
      if ("turnId" in event) expect(event.turnId).toBe(TURN)
      if (event.kind === "turn.final") expect(event.text).toBe("echo: ping")
      if (event.kind === "turn.finished") {
        expect(event.outcome).toBe("replied")
        break
      }
    }
    expect(kinds[0]).toBe("turn.started")
    expect(kinds).toContain("turn.delta")
    expect(kinds.indexOf("turn.final")).toBeLessThan(kinds.indexOf("turn.finished"))
  })

  test("events of client ids this transport did not send are not attributed to a turn", async () => {
    const r = await rig("current")
    const info = await connect(r)
    const it = iterate(r)
    const corr = { message_id: crypto.randomUUID(), client_turn_id: "web-ui-turn" }
    r.mock.emit({ type: "turn_started", thread_id: info.threadId ?? "" }, corr)
    r.mock.emit({ type: "response", content: "someone else's reply", thread_id: info.threadId ?? "" }, corr)
    r.mock.emit({ type: "turn_finished", thread_id: info.threadId ?? "", outcome: "replied" }, corr)
    r.mock.emit({ type: "thinking", message: "marker" })
    expect(await next(it)).toEqual({ kind: "background", threadId: info.threadId ?? "", text: "someone else's reply" })
    expect((await next(it)).kind).toBe("turn.activity")
  })

  test("a legacy V1 turn arrives uncorrelated, with the status Done settle hint", async () => {
    const r = await rig("legacy", { mock: { engine: "v1" } })
    await connect(r)
    const it = iterate(r)
    await r.transport.send({ turnId: TURN, text: "ping", images: [] })
    expect(r.mock.requestsTo("POST /api/chat/send")[0]?.body).toMatchObject({ client_turn_id: TURN })
    const hint = await next(it, (e) => e.kind === "turn.settle_hint")
    expect(hint).toMatchObject({ kind: "turn.settle_hint", signal: "done" })
    const final = await next(it, (e) => e.kind === "turn.final")
    expect(final).toMatchObject({ turnId: null, text: "echo: ping" })
  })

  test("a legacy V2 turn streams uncorrelated deltas and settles on the terminal thread state", async () => {
    const r = await rig("legacy")
    await connect(r)
    const it = iterate(r)
    await r.transport.send({ turnId: TURN, text: "ping", images: [] })
    const delta = await next(it, (e) => e.kind === "turn.delta")
    expect(delta).toMatchObject({ turnId: null })
    expect(await next(it, (e) => e.kind === "turn.settle_hint")).toMatchObject({ signal: "thread_terminal" })
    expect(await next(it, (e) => e.kind === "turn.final")).toMatchObject({ turnId: null, text: "echo: ping" })
  })

  test("stream_gap raises resync.required", async () => {
    const r = await rig("current")
    await connect(r)
    const it = iterate(r)
    r.mock.emit({ type: "stream_gap", missed_events: 12 })
    expect(await next(it)).toEqual({ kind: "resync.required", reason: "gap" })
  })

  test("a V2 gate announced twice (gate_required, approval_needed) yields one prompt", async () => {
    const r = await rig("current")
    await connect(r)
    const it = iterate(r)
    await r.transport.send({ turnId: TURN, text: "!approval", images: [] })
    const approval = await next(it, (e) => e.kind === "turn.approval")
    expect(approval).toMatchObject({ turnId: TURN, approval: { toolName: "shell", allowAlways: true, source: "live" } })
    const after = await next(it, (e) => e.kind === "turn.approval" || e.kind === "turn.finished")
    expect(after).toMatchObject({ kind: "turn.finished", outcome: "paused" })
  })

  test("auth_required after gate_required{Authentication} carries the gate's request and thread ids", async () => {
    const r = await rig("current")
    const info = await connect(r)
    const it = iterate(r)
    const requestId = crypto.randomUUID()
    const threadId = info.threadId ?? ""
    r.mock.emit({
      type: "gate_required",
      request_id: requestId,
      gate_name: "auth",
      tool_name: "github",
      description: "",
      parameters: "{}",
      resume_kind: { Authentication: { credential_name: "github", instructions: "Paste a token", auth_url: null } },
      thread_id: threadId,
    })
    r.mock.emit({ type: "auth_required", extension_name: "github", instructions: "Paste a token" })
    expect(await next(it)).toMatchObject({ kind: "turn.auth_required", requestId, threadId })
    expect(await next(it)).toMatchObject({ kind: "turn.auth_required", extensionName: "github", requestId, threadId })
  })

  test("the watchdog ends the stream after 45 s without a byte; any byte re-arms it", async () => {
    const r = await rig("current")
    await connect(r)
    const it = iterate(r)
    r.clock.advance(44_999)
    r.mock.emitComment()
    r.mock.emit({ type: "thinking", message: "still here" })
    expect((await next(it)).kind).toBe("turn.activity")
    r.clock.advance(44_999)
    r.mock.emit({ type: "thinking", message: "still here" })
    expect((await next(it)).kind).toBe("turn.activity")
    r.clock.advance(45_000)
    const error = await rejection(it.next())
    expect(error.code).toBe("timeout")
    expect(error.message).toBe("no data from the gateway for 45 s")
  })

  test("the server closing the stream ends events() with an error (a loss, not a normal end)", async () => {
    const r = await rig("current")
    await connect(r)
    const it = iterate(r)
    r.mock.dropStreams()
    const error = await rejection(it.next())
    expect(error.code).toBe("closed")
  })

  test("close() ends events() normally and later requests fail as closed", async () => {
    const r = await rig("current")
    await connect(r)
    const it = iterate(r)
    await r.transport.close()
    expect(await it.next()).toEqual({ value: undefined, done: true })
    expect((await rejection(r.transport.send({ turnId: TURN, text: "x", images: [] }))).code).toBe("closed")
  })

  test("resync() after events were consumed fetches history and gates again", async () => {
    const r = await rig("current")
    await connect(r)
    const it = iterate(r)
    r.mock.emit({ type: "thinking", message: "x" })
    await next(it)
    const before = r.mock.requestsTo("GET /api/chat/history").length
    await r.transport.resync()
    expect(r.mock.requestsTo("GET /api/chat/history").length).toBe(before + 1)
    expect(r.mock.requestsTo("GET /api/chat/pending-gates").length).toBe(2)
  })
})

describe("snapshot gates", () => {
  const threadId = "0c1d2e3f-4a5b-4c6d-8e7f-8091a2b3c4d5"

  test("a V2 approval listed by history and pending-gates keeps allow_always (the pending-gates copy wins)", async () => {
    const r = await rig("legacy", { transport: { thread: { kind: "id", id: threadId } } })
    r.mock.addThread([], threadId)
    r.mock.addApproval({ request_id: "a1b2c3d4-0000-4000-8000-00000000000a", thread_id: threadId, tool_name: "shell", description: "d", parameters: "{}", allow_always: true })
    await connect(r)
    const snapshot = await r.transport.resync()
    expect(snapshot.approvals.map((approval) => [approval.id, approval.source, approval.allowAlways])).toEqual([
      ["a1b2c3d4-0000-4000-8000-00000000000a", "pending_gates", true],
    ])
  })

  test("authentication gates of the thread are reported, other threads' are not", async () => {
    const r = await rig("legacy", { transport: { thread: { kind: "id", id: threadId } } })
    r.mock.addThread([], threadId)
    r.mock.addAuthGate({ request_id: "a1b2c3d4-0000-4000-8000-00000000000b", thread_id: threadId, extension_name: "github", instructions: "Paste a token" })
    r.mock.addAuthGate({ request_id: "a1b2c3d4-0000-4000-8000-00000000000c", thread_id: crypto.randomUUID(), extension_name: "linear", instructions: "" })
    await connect(r)
    const snapshot = await r.transport.resync()
    expect(snapshot.approvals).toEqual([])
    expect(snapshot.authGates).toEqual([
      { requestId: "a1b2c3d4-0000-4000-8000-00000000000b", threadId, extensionName: "github", instructions: "Paste a token" },
    ])
  })
})

describe("events() bounds (EventChannel, 3.7)", () => {
  const delta = (text: string): TransportEvent => ({ kind: "turn.delta", turnId: TURN, text })
  const activity = (text: string): TransportEvent => ({ kind: "turn.activity", turnId: TURN, activity: { kind: "status", text } })

  async function drain(channel: EventChannel): Promise<TransportEvent[]> {
    const out: TransportEvent[] = []
    for await (const event of channel) out.push(event)
    return out
  }

  test("delivers in order and ends normally after close(), draining first", async () => {
    const channel = new EventChannel()
    channel.push(activity("a"))
    channel.push(activity("b"))
    channel.close()
    expect(channel.push(activity("late"))).toBe(false)
    expect(await drain(channel)).toEqual([activity("a"), activity("b")])
  })

  test("fail() delivers the queued events, then throws", async () => {
    const channel = new EventChannel()
    channel.push(activity("a"))
    channel.fail(new TransportError("closed", "lost"))
    const it = channel[Symbol.asyncIterator]()
    expect((await it.next()).value).toEqual(activity("a"))
    expect((await rejection(it.next())).message).toBe("lost")
    expect(await it.next()).toEqual({ value: undefined, done: true })
  })

  test("overflow drops the queued deltas and queues one resync.required{overflow}", async () => {
    const channel = new EventChannel({ capacity: 5 })
    for (const event of [delta("1"), activity("a"), delta("2"), activity("b"), delta("3")]) channel.push(event)
    expect(channel.push(activity("c"))).toBe(true)
    expect(channel.push(delta("4"))).toBe(true)
    // Full again: the queued delta goes, the marker is already queued, and the new event fits.
    expect(channel.push(activity("d"))).toBe(true)
    channel.close()
    expect(await drain(channel)).toEqual([
      activity("a"),
      activity("b"),
      { kind: "resync.required", reason: "overflow" },
      activity("c"),
      activity("d"),
    ])
    expect(channel.dropped).toBe(4)
  })

  test("overflow without deltas makes room for the marker and drops the newest event", async () => {
    const channel = new EventChannel({ capacity: 3 })
    for (const name of ["a", "b", "c"]) channel.push(activity(name))
    expect(channel.push(activity("d"))).toBe(false)
    expect(channel.push(activity("e"))).toBe(false)
    channel.close()
    expect(await drain(channel)).toEqual([activity("b"), activity("c"), { kind: "resync.required", reason: "overflow" }])
  })

  test("a waiting consumer receives events directly; there is a single consumer; an early return runs onReturn", async () => {
    let returned = 0
    const channel = new EventChannel({ onReturn: () => (returned += 1) })
    const it = channel[Symbol.asyncIterator]()
    const pending = it.next()
    channel.push(activity("direct"))
    expect((await pending).value).toEqual(activity("direct"))
    expect(() => channel[Symbol.asyncIterator]()).toThrow()
    await it.return?.()
    expect(returned).toBe(1)
    expect(channel.push(activity("after"))).toBe(false)
  })

  test("removeFirst takes only undelivered events", async () => {
    const channel = new EventChannel()
    channel.push(activity("a"))
    channel.push(activity("b"))
    channel.push(activity("b"))
    expect(channel.removeFirst((e) => e.kind === "turn.activity" && e.activity.text === "b")).toEqual(activity("b"))
    channel.close()
    expect(await drain(channel)).toEqual([activity("a"), activity("b")])
  })

  test("an event that is not JSON is a protocol error that ends the stream", async () => {
    const r = await rig("current")
    await connect(r)
    const it = iterate(r)
    r.mock.emitRaw("event: response\ndata: {not json\n\n")
    const error = await rejection(it.next())
    expect(error.code).toBe("protocol")
  })

  test("an event larger than 24 MiB is a protocol error that ends the stream", async () => {
    const r = await rig("current")
    await connect(r)
    const it = iterate(r)
    r.mock.emitRaw(`event: image_generated\ndata: ${"A".repeat(24 * MIB + 1024)}\n\n`)
    const error = await rejection(it.next())
    expect(error.code).toBe("protocol")
    expect(error.message).toContain("larger than 24 MiB")
  })

  test("unknown and malformed events are ignored without ending the stream", async () => {
    const r = await rig("current")
    await connect(r)
    const it = iterate(r)
    r.mock.emit({ type: "job_message", job_id: "j", role: "x", content: "y" })
    r.mock.emit({ type: "response", content: 42, thread_id: "t" })
    r.mock.emit({ type: "thinking", message: "next" })
    expect(await next(it)).toMatchObject({ kind: "turn.activity", activity: { text: "next" } })
  })
})

describe("requests", () => {
  test("client_turn_id is sent on send, interrupt, steer, approval, auth-token and auth-cancel", async () => {
    const r = await rig("current")
    const info = await connect(r)
    const threadId = info.threadId ?? ""
    await r.transport.send({ turnId: TURN, text: "hello", images: [] })
    await r.transport.interrupt({ requestId: "ctl-1" as TurnId })
    await r.transport.steer({ requestId: "ctl-2" as TurnId, text: "  use staging  " })
    const sends = r.mock.requestsTo("POST /api/chat/send").map((rec) => rec.body)
    expect(sends).toEqual([
      { content: "hello", thread_id: threadId, timezone: "Europe/Berlin", client_turn_id: TURN },
      { content: "/interrupt", thread_id: threadId, timezone: "Europe/Berlin", client_turn_id: "ctl-1" },
      { content: "/steer use staging", thread_id: threadId, timezone: "Europe/Berlin", client_turn_id: "ctl-2" },
    ])

    const approval = r.mock.addApproval({
      request_id: crypto.randomUUID(),
      thread_id: threadId,
      tool_name: "shell",
      description: "d",
      parameters: "{}",
      allow_always: false,
    })
    await r.transport.approve({ approvalId: approval.request_id, decision: "always", threadId, turnId: TURN })
    expect(r.mock.requestsTo("POST /api/chat/approval")[0]?.body).toEqual({
      request_id: approval.request_id,
      action: "always",
      thread_id: threadId,
      client_turn_id: TURN,
    })

    const gate = r.mock.addAuthGate({ request_id: crypto.randomUUID(), thread_id: threadId, extension_name: "github", instructions: "token" })
    const credential = new Secret("ghp_credential_value_0001")
    await r.transport.authToken({ extensionName: "github", token: credential, turnId: TURN, requestId: gate.request_id, threadId })
    expect(r.mock.requestsTo("POST /api/chat/auth-token")[0]?.body).toEqual({
      extension_name: "github",
      token: "ghp_credential_value_0001",
      request_id: gate.request_id,
      thread_id: threadId,
      client_turn_id: TURN,
    })
    await r.transport.authCancel({ extensionName: "notion", turnId: TURN })
    expect(r.mock.requestsTo("POST /api/chat/auth-cancel")[0]?.body).toEqual({ extension_name: "notion", client_turn_id: TURN })
  })

  test("request_id and thread_id are sent together or not at all", async () => {
    const r = await rig("current")
    await connect(r)
    await r.transport.authCancel({ extensionName: "notion", turnId: null, requestId: crypto.randomUUID() })
    expect(r.mock.requestsTo("POST /api/chat/auth-cancel")[0]?.body).toEqual({ extension_name: "notion" })
  })

  test("image send carries the sniffed media_type", async () => {
    const r = await rig("current")
    await connect(r)
    await r.transport.send({ turnId: TURN, text: "", images: [{ mime: "image/jpeg", bytes: TINY_PNG, filename: "shot.jpg" }] })
    const body = r.mock.requestsTo("POST /api/chat/send")[0]?.body as { images?: { media_type: string; data: string }[] }
    expect(body.images).toEqual([{ media_type: "image/png", data: Buffer.from(TINY_PNG).toString("base64") }])
  })

  test("image checks and the body budget refuse without any request", async () => {
    const r = await rig("current", { mock: { limits: { max_request_bytes: 256 * 1024 } } })
    await connect(r)
    const png = (size: number) => {
      const bytes = new Uint8Array(size)
      bytes.set(TINY_PNG.subarray(0, 8))
      return bytes
    }
    const before = r.mock.requests.length
    const budget = await rejection(r.transport.send({ turnId: TURN, text: "x", images: [{ mime: "image/png", bytes: png(150 * 1024), filename: null }] }))
    expect(budget.code).toBe("too_large")
    const notImage = await rejection(
      r.transport.send({ turnId: TURN, text: "x", images: [{ mime: "image/png", bytes: new TextEncoder().encode("<svg/>"), filename: null }] }),
    )
    expect(notImage.code).toBe("bad_request")
    const many = Array.from({ length: 6 }, () => ({ mime: "image/png" as const, bytes: TINY_PNG, filename: null }))
    expect((await rejection(r.transport.send({ turnId: TURN, text: "x", images: many }))).code).toBe("too_large")
    const long = "é".repeat(60_000)
    expect((await rejection(r.transport.send({ turnId: TURN, text: long, images: [] }))).code).toBe("too_large")
    expect(r.mock.requests.length).toBe(before)

    // Just under the budget still goes out.
    await r.transport.send({ turnId: TURN, text: "x", images: [{ mime: "image/png", bytes: png(100 * 1024), filename: null }] })
    expect(r.mock.requestsTo("POST /api/chat/send")).toHaveLength(1)
  })

  test("/quit, /exit and /shutdown never reach the wire; an empty steer is refused", async () => {
    const r = await rig("legacy")
    await connect(r)
    for (const content of ["/quit", " /EXIT ", "/Shutdown"]) {
      expect((await rejection(r.transport.send({ turnId: TURN, text: content, images: [] }))).code).toBe("bad_request")
    }
    expect((await rejection(r.transport.steer({ requestId: TURN, text: "   " }))).code).toBe("bad_request")
    expect((await rejection(r.transport.send({ turnId: "bad id" as TurnId, text: "x", images: [] }))).code).toBe("bad_request")
    expect(r.mock.requestsTo("POST /api/chat/send")).toHaveLength(0)
  })

  test("transcribe posts base64 WAV without a language and returns the text", async () => {
    const r = await rig("current")
    await connect(r)
    const wav = new Uint8Array(64).fill(1)
    const result = await r.transport.transcribe({ requestId: "t-1", wav, signal: new AbortController().signal })
    expect(result).toEqual({ text: "hello from the mock", language: "en", model: "parakeet-tdt-0.6b-v2-int8", elapsedMs: 0 })
    expect(r.mock.requestsTo("POST /api/voice/transcribe")[0]?.body).toEqual({ audio: Buffer.from(wav).toString("base64"), format: "wav" })
  })

  test("transcribe is refused without server transcription and when aborted", async () => {
    const mute = await rig("legacy", { mock: { voiceConfigured: false } })
    await connect(mute)
    const wav = new Uint8Array(8)
    expect((await rejection(mute.transport.transcribe({ requestId: "t-1", wav, signal: new AbortController().signal }))).code).toBe("unsupported")
    expect(mute.mock.requestsTo("POST /api/voice/transcribe")).toHaveLength(0)

    const r = await rig("current")
    await connect(r)
    const aborted = new AbortController()
    aborted.abort(new Error("esc"))
    let reason: unknown
    try {
      await r.transport.transcribe({ requestId: "t-2", wav, signal: aborted.signal })
    } catch (error) {
      reason = error
    }
    expect((reason as Error).message).toBe("esc")
  })
})

describe("timeouts and cancellation (ManualClock)", () => {
  /** Never answers; settles once the client gives up, so the mock can stop. */
  const hang = (req: Request) =>
    new Promise<Response>((resolve) => req.signal.addEventListener("abort", () => resolve(new Response(null, { status: 499 }))))

  test("a POST that never answers times out after 30 s; a transcription after 150 s", async () => {
    const r = await rig("current")
    await connect(r)
    r.mock.override("POST /api/chat/send", hang)
    const sending = rejection(r.transport.send({ turnId: TURN, text: "x", images: [] }))
    await r.mock.waitForRequest("POST /api/chat/send")
    r.clock.advance(29_999)
    r.clock.advance(1)
    const sendError = await sending
    expect([sendError.code, sendError.message]).toEqual(["timeout", "POST /api/chat/send timed out after 30 s"])

    r.mock.override("POST /api/voice/transcribe", hang)
    const transcribing = rejection(r.transport.transcribe({ requestId: "t-1", wav: new Uint8Array(8), signal: new AbortController().signal }))
    await r.mock.waitForRequest("POST /api/voice/transcribe")
    r.clock.advance(149_999)
    r.clock.advance(1)
    expect((await transcribing).code).toBe("timeout")
  })

  test("aborting connect() cancels it and fails its event stream", async () => {
    const r = await rig("current")
    r.mock.override("GET /api/chat/capabilities", hang)
    const controller = new AbortController()
    const connecting = rejection(r.transport.connect(controller.signal))
    await r.mock.waitForRequest("GET /api/chat/capabilities")
    controller.abort()
    const error = await connecting
    expect([error.code, error.message]).toEqual(["closed", "connect was cancelled"])
    expect((await rejection(iterate(r).next())).code).toBe("closed")
  })

  test("selectThread switches the thread used by later sends and remembers it", async () => {
    const r = await rig("current")
    const first = await connect(r)
    const other = r.mock.addThread([completedTurn("q", "a")])
    expect(await r.transport.selectThread({ kind: "id", id: other.id })).toBe(other.id)
    expect((await r.state.read()).lastThreadId).toBe(other.id)
    await r.transport.send({ turnId: TURN, text: "x", images: [] })
    expect(r.mock.requestsTo("POST /api/chat/send")[0]?.body).toMatchObject({ thread_id: other.id })
    expect(other.id).not.toBe(first.threadId ?? "")
    const snapshot = await r.transport.resync()
    expect(snapshot.threadId).toBe(other.id)
  })
})

describe("HTTP status mapping", () => {
  async function mapped(r: Rig, route: string, response: () => Response, op: () => Promise<unknown>): Promise<TransportError | "ok"> {
    r.mock.override(route, () => response(), 1)
    try {
      await op()
      return "ok"
    } catch (error) {
      if (!isTransportError(error)) throw error
      return error
    } finally {
      r.mock.override(route, null)
    }
  }

  test("send", async () => {
    const r = await rig("current")
    await connect(r)
    const send = () => r.transport.send({ turnId: TURN, text: "x", images: [] })
    const cases: [number, string, TransportErrorCode, boolean][] = [
      [400, "thread_id is required for /api/chat/send", "bad_request", false],
      [403, "Client token not permitted for this route", "forbidden", true],
      [404, "", "not_found", false],
      [413, "image 1 rejected: larger than 5242880 bytes", "too_large", false],
      [422, "image 1 rejected: unsupported image type (allowed: png, jpeg, gif, webp)", "bad_request", false],
      [429, "Rate limit exceeded. Try again shortly.", "rate_limited", false],
      [503, "Channel not started", "unavailable", false],
      [500, "Channel closed", "server", false],
    ]
    for (const [status, body, code, fatal] of cases) {
      const error = await mapped(r, "POST /api/chat/send", () => text(status, body), send)
      if (error === "ok") throw new Error(`HTTP ${status} was accepted`)
      expect([status, error.code, error.fatal, error.status]).toEqual([status, code, fatal, status])
      if (body) expect(error.message).toContain(body)
    }
    const limited = await mapped(r, "POST /api/chat/send", () => text(429, "Rate limit exceeded."), send)
    expect(limited !== "ok" && limited.retryAfterMs).toBe(60_000)
    expect(await mapped(r, "POST /api/chat/send", () => json(202, { message_id: crypto.randomUUID(), status: "accepted" }), send)).toBe("ok")
  })

  test("send: 401 twice is a fatal auth error after one token reload", async () => {
    const r = await rig("current")
    await connect(r)
    r.mock.override("POST /api/chat/send", () => text(401, "Invalid or missing auth token"))
    const error = await rejection(r.transport.send({ turnId: TURN, text: "x", images: [] }))
    expect([error.code, error.fatal, error.status]).toEqual(["auth", true, 401])
    expect(r.token.reloads).toBe(1)
  })

  test("approval: 202 completes, 404 means expired, 400 is a bad request", async () => {
    const r = await rig("current")
    const info = await connect(r)
    const approve = () => r.transport.approve({ approvalId: crypto.randomUUID(), decision: "approve", threadId: info.threadId, turnId: TURN })
    expect(await mapped(r, "POST /api/chat/approval", () => json(202, { message_id: crypto.randomUUID(), status: "accepted" }), approve)).toBe("ok")
    const expired = await rejection(approve())
    expect([expired.code, expired.status]).toEqual(["not_found", 404])
    const bad = await mapped(r, "POST /api/chat/approval", () => text(400, "Unknown action: x"), approve)
    expect(bad !== "ok" && bad.code).toBe("bad_request")
  })

  test("auth-token and auth-cancel", async () => {
    const r = await rig("current")
    await connect(r)
    const token = new Secret("ghp_credential_value_0002")
    const authToken = () => r.transport.authToken({ extensionName: "github", token, turnId: TURN })
    const authCancel = () => r.transport.authCancel({ extensionName: "github", turnId: TURN })
    expect(await mapped(r, "POST /api/chat/auth-token", () => json(200, { success: true, message: "ok" }), authToken)).toBe("ok")
    const rejected = await mapped(r, "POST /api/chat/auth-token", () => json(200, { success: false, message: "bad token ghp_credential_value_0002" }), authToken)
    expect(rejected !== "ok" && rejected.code).toBe("bad_request")
    expect(rejected !== "ok" && rejected.message).not.toContain("ghp_credential_value_0002")
    const stale = await mapped(r, "POST /api/chat/auth-token", () => text(409, "Authentication request is stale or no longer available"), authToken)
    expect(stale !== "ok" && [stale.code, stale.status]).toEqual(["not_found", 409])
    const scoped = await mapped(r, "POST /api/chat/auth-token", () => text(403, "Client token may only answer a pending authentication request"), authToken)
    expect(scoped !== "ok" && [scoped.code, scoped.fatal]).toEqual(["forbidden", false])
    const bad = await mapped(r, "POST /api/chat/auth-token", () => text(400, "request_id and thread_id must be supplied together"), authToken)
    expect(bad !== "ok" && bad.code).toBe("bad_request")
    expect(await mapped(r, "POST /api/chat/auth-cancel", () => json(200, { success: true, message: "Auth cancelled" }), authCancel)).toBe("ok")
    const cancelStale = await mapped(r, "POST /api/chat/auth-cancel", () => text(409, "stale"), authCancel)
    expect(cancelStale !== "ok" && cancelStale.code).toBe("not_found")
  })

  test("transcribe: 200, 400, 401, 413, 422, 429, 503 busy versus not configured, 504, 500", async () => {
    const r = await rig("current")
    await connect(r)
    const wav = new Uint8Array(16).fill(2)
    const transcribe = () => r.transport.transcribe({ requestId: "t-1", wav, signal: new AbortController().signal })
    const voice = (status: number, error: string, message: string) => () => json(status, { error, message })
    expect(await mapped(r, "POST /api/voice/transcribe", () => json(200, { text: "ok", language: "en", model: "m" }), transcribe)).toBe("ok")
    const cases: [() => Response, TransportErrorCode, number][] = [
      [voice(400, "bad_request", "voice sidecar rejected the request"), "bad_request", 400],
      [voice(413, "payload_too_large", "audio exceeds 25165824 base64 bytes"), "too_large", 413],
      [voice(422, "processing_failed", "voice sidecar could not process the request"), "bad_request", 422],
      [voice(429, "rate_limited", "too many voice requests"), "rate_limited", 429],
      [voice(503, "unavailable", "voice sidecar is unavailable"), "busy", 503],
      [voice(503, "unavailable", "voice service is not configured"), "unavailable", 503],
      [voice(504, "upstream_timeout", "voice sidecar request timed out"), "timeout", 504],
      [voice(500, "upstream_error", "voice sidecar rejected the gateway credential"), "server", 500],
      [voice(502, "upstream_error", "failed to contact voice sidecar"), "server", 502],
    ]
    for (const [response, code, status] of cases) {
      const error = await mapped(r, "POST /api/voice/transcribe", response, transcribe)
      if (error === "ok") throw new Error(`HTTP ${status} was accepted`)
      expect([error.code, error.status]).toEqual([code, status])
    }
    const notConfigured = await mapped(r, "POST /api/voice/transcribe", voice(503, "unavailable", "voice service is not configured"), transcribe)
    expect(notConfigured !== "ok" && notConfigured.message).toContain("voice service is not configured")
    r.mock.override("POST /api/voice/transcribe", () => text(401, "Invalid or missing auth token"))
    const auth = await rejection(transcribe())
    expect([auth.code, auth.fatal]).toEqual(["auth", true])
  })
})

describe("tokens (3.8, UX-10)", () => {
  test("401 re-reads the access file once and retries with the new token", async () => {
    const r = await rig("current")
    const rotated = fixtureToken("rotated")
    r.mock.token = rotated
    r.token.rotateOnReload(rotated)
    const info = await connect(r)
    expect(info.protocol).toBe("gateway-chat")
    expect(r.token.reloads).toBe(1)
    expect(r.mock.requestsTo("GET /api/chat/events").map((rec) => rec.headers.authorization)).toEqual([
      `Bearer ${TOKEN}`,
      `Bearer ${rotated}`,
    ])
    await r.transport.send({ turnId: TURN, text: "x", images: [] })
    expect(r.mock.requestsTo("POST /api/chat/send")[0]?.headers.authorization).toBe(`Bearer ${rotated}`)
  })

  const hints: [TokenKind, string, string][] = [
    ["client", "tenants/derpy.json (client token tui:sun)", "tui-access derpy --for-user $USER --use-master-token"],
    ["master", "tenants/derpy.json (master token)", "the master token changed: re-run `tui-access derpy --for-user $USER --use-master-token`"],
    ["file", "tokens/devgw.token", "the token in tokens/devgw.token was rejected"],
  ]
  for (const [kind, source, hint] of hints) {
    test(`a second 401 is fatal with the ${kind} token hint`, async () => {
      const r = await rig("current", { tokenKind: kind, tokenSource: source, transport: { tenant: "derpy" } })
      r.mock.token = "some-other-token-0000"
      const error = await rejection(connect(r))
      expect([error.code, error.fatal, error.status]).toEqual(["auth", true, 401])
      expect(error.message).toContain(hint)
      expect(r.token.reloads).toBe(1)
    })
  }

  test("403 on the event stream is fatal (client tokens are route-scoped)", async () => {
    const r = await rig("current")
    r.mock.override("GET /api/chat/events", () => text(403, "Client token not permitted for this route"))
    const error = await rejection(connect(r))
    expect([error.code, error.fatal]).toEqual(["forbidden", true])
  })
})

describe("security", () => {
  test("a listener owned by another uid never receives an Authorization header", async () => {
    const r = await rig("current", { transport: { readProc: procReaderFor(0, UID) } })
    const foreign = procReaderFor(r.mock.port, UID + 4242)
    const t = new GatewayTransport({
      baseUrl: r.mock.url,
      token: r.token,
      thread: { kind: "last" },
      clock: r.clock,
      timezone: "",
      stateFile: r.state,
      expectedListener: { uid: UID, label: "derpy" },
      readProc: foreign,
      env: {},
    })
    cleanups.push(() => t.close())
    const error = await rejection(t.connect(new AbortController().signal))
    expect([error.code, error.fatal]).toEqual(["forbidden", true])
    expect(error.message).toBe(`port ${r.mock.port} is held by uid ${UID + 4242}, expected ${UID} (derpy)`)
    expect(routes(r.mock)).toEqual(["GET /api/health"])
    expect(r.mock.requests.every((rec) => rec.headers.authorization === undefined)).toBe(true)
  })

  test("the owner is checked again before every later token-bearing request", async () => {
    let owner = UID
    const r = await rig("current")
    const reader: ProcReader = (path) => procReaderFor(r.mock.port, owner)(path)
    const t = new GatewayTransport({
      baseUrl: r.mock.url,
      token: r.token,
      thread: { kind: "new" },
      clock: r.clock,
      timezone: "",
      stateFile: r.state,
      expectedListener: { uid: UID, label: "derpy" },
      readProc: reader,
      env: {},
    })
    cleanups.push(() => t.close())
    await t.connect(new AbortController().signal)
    owner = UID + 1
    const error = await rejection(t.send({ turnId: TURN, text: "secret plans", images: [] }))
    expect(error.code).toBe("forbidden")
    expect(r.mock.requestsTo("POST /api/chat/send")).toHaveLength(0)
  })

  test("no listener on the port: unreachable before any token-bearing request", async () => {
    const r = await rig("current", { transport: { readProc: procReaderFor(1, UID) } })
    const error = await rejection(connect(r))
    expect([error.code, error.fatal]).toEqual(["unreachable", false])
    expect(routes(r.mock)).toEqual(["GET /api/health"])
  })

  test("the token never appears in a URL, a log line or any error text", async () => {
    const r = await rig("current")
    const written: string[] = []
    const consoleMethods = ["log", "info", "warn", "error", "debug", "trace"] as const
    const saved = consoleMethods.map((name) => console[name])
    const savedStdout = process.stdout.write.bind(process.stdout)
    const savedStderr = process.stderr.write.bind(process.stderr)
    const capture = (...args: unknown[]) => {
      written.push(args.map(String).join(" "))
    }
    for (const name of consoleMethods) console[name] = capture
    process.stdout.write = ((chunk: unknown) => (capture(chunk), true)) as typeof process.stdout.write
    process.stderr.write = ((chunk: unknown) => (capture(chunk), true)) as typeof process.stderr.write
    const errors: unknown[] = []
    let info: ServerInfo | null = null
    try {
      info = await connect(r)
      r.mock.override("POST /api/chat/send", () => text(401, "Invalid or missing auth token"))
      errors.push(await rejection(r.transport.send({ turnId: TURN, text: "x", images: [] })))
      r.mock.override("POST /api/chat/send", null)
      r.mock.override("POST /api/chat/approval", () => text(404, "No pending approval matches this request_id"))
      errors.push(await rejection(r.transport.approve({ approvalId: crypto.randomUUID(), decision: "deny", threadId: null, turnId: null })))
      r.mock.override("POST /api/voice/transcribe", () => json(500, { error: "upstream_error", message: "x" }))
      errors.push(await rejection(r.transport.transcribe({ requestId: "t", wav: new Uint8Array(4), signal: new AbortController().signal })))
      const foreign = new GatewayTransport({
        baseUrl: r.mock.url,
        token: r.token,
        thread: { kind: "last" },
        clock: r.clock,
        timezone: "",
        stateFile: r.state,
        expectedListener: { uid: UID, label: "x" },
        readProc: procReaderFor(r.mock.port, UID + 1),
        env: {},
      })
      errors.push(await rejection(foreign.connect(new AbortController().signal)))
      await foreign.close()
    } finally {
      consoleMethods.forEach((name, i) => {
        const original = saved[i]
        if (original) console[name] = original
      })
      process.stdout.write = savedStdout
      process.stderr.write = savedStderr
    }
    const texts = [
      ...errors.map((e) => `${String(e)} ${(e as Error).message} ${JSON.stringify(e)}`),
      ...r.mock.requests.map((rec) => rec.url),
      JSON.stringify(info),
      ...written,
    ]
    expect(errors).toHaveLength(4)
    for (const piece of texts) expect(piece).not.toContain(TOKEN)
    expect(r.mock.requests.some((rec) => rec.headers.authorization === `Bearer ${TOKEN}`)).toBe(true)
  })

  test("redirects are refused and the token is never replayed to the target", async () => {
    const catcher = await MockGateway.start({ mode: "current", token: null })
    cleanups.push(() => catcher.stop())
    const r = await rig("current")
    const elsewhere = `${catcher.url.origin}/api/chat/events`
    r.mock.override("GET /api/chat/events", () => new Response(null, { status: 302, headers: { location: elsewhere } }))
    const error = await rejection(connect(r))
    expect(error.code).toBe("protocol")
    expect(error.message).toContain("redirect")

    r.mock.override("GET /api/chat/events", null)
    await connect(r)
    r.mock.override("POST /api/chat/send", () => new Response(null, { status: 307, headers: { location: `${catcher.url.origin}/api/chat/send` } }))
    expect((await rejection(r.transport.send({ turnId: TURN, text: "x", images: [] }))).code).toBe("protocol")
    expect(catcher.requests).toHaveLength(0)
  })

  test("an environment proxy fails closed before any request; NO_PROXY=* lets requests go direct", async () => {
    const r = await rig("current", { transport: { env: { HTTP_PROXY: "http://127.0.0.1:9" } } })
    const error = await rejection(connect(r))
    expect([error.code, error.fatal]).toEqual(["unsupported", true])
    expect(error.message).not.toContain("127.0.0.1:9")
    expect(r.mock.requests).toHaveLength(0)

    const direct = await rig("current", { transport: { env: { HTTP_PROXY: "http://127.0.0.1:9", NO_PROXY: "*" } } })
    expect((await connect(direct)).protocol).toBe("gateway-chat")
  })

  test("plain http is accepted only to loopback unless allowed; credentials, queries and fragments are refused", () => {
    const base = {
      token: new MemoryTokenSource(TOKEN),
      thread: { kind: "last" } as const,
      clock: new ManualClock(),
      timezone: "",
      stateFile: new MemoryStateFile(),
      expectedListener: null,
      env: {},
    }
    const build = (url: string, allowInsecureHttp = false) => () => new GatewayTransport({ ...base, baseUrl: new URL(url), allowInsecureHttp })
    for (const bad of ["http://10.1.2.3:8080", "http://user:pw@127.0.0.1:1", "http://127.0.0.1:1/?token=x", "http://127.0.0.1:1/#x", "ftp://127.0.0.1/"]) {
      let caught: unknown
      try {
        build(bad)()
      } catch (error) {
        caught = error
      }
      expect(isTransportError(caught) && caught.fatal).toBe(true)
    }
    expect(build("http://10.1.2.3:8080", true)).not.toThrow()
    expect(build("https://gateway.example:8443")).not.toThrow()
    expect(build("http://[::1]:8765")).not.toThrow()
    expect(build("http://localhost:8765")).not.toThrow()
  })

  test("a remote URL without an owner check is reported as a sticky warning", async () => {
    const r = await rig("current", { transport: { expectedListener: null } })
    const info = await connect(r)
    expect(info.warnings).toContain("listener owner not verified (remote URL)")
  })
})
