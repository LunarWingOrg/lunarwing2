/**
 * The server's golden SSE fixture through the client's own pipeline (design 4.2.5, 8.3.2; the M2a contract
 * cross-check). ic/src/channels/web/tests/correlation.rs writes ic/src/channels/web/tests/fixtures/
 * sse_current.ndjson from the real gateway's SSE output: one {"event", "data"} object per frame, the data bytes as
 * sent on the `data:` line. Each frame is replayed as the wire bytes sse.rs writes and goes through exactly what
 * transport/gateway.ts does with them: SseParser.push, JSON.parse(message.data), parseGatewayEvent, then
 * mapGatewayEvent with a current-mode connection context (correlated, the client's own client_turn_ids, the
 * production image decoder).
 *
 * The fixture is read from the repository checkout (../ic from tui/); the CI tui-ts lane has the full checkout.
 * Nothing here depends on the position of a frame: the V2 continuation's turn_started may come before or after the
 * uncorrelated gate_resolved, and both orders are checked.
 */
import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { decodeDataUrl } from "../../src/images/data-url.ts"
import { asTurnId } from "../../src/state/ids.ts"
import {
  GATEWAY_EVENT_TYPES,
  mapGatewayEvent,
  parseGatewayEvent,
  type GatewayWireContext,
} from "../../src/transport/gateway-wire.ts"
import { SSE_MAX_EVENT_BYTES, SseParser, type SseMessage } from "../../src/transport/sse.ts"
import type { TransportEvent } from "../../src/transport/types.ts"

const FIXTURE = join(import.meta.dir, "..", "..", "..", "ic", "src", "channels", "web", "tests", "fixtures", "sse_current.ndjson")

/** Server events the client deliberately ignores (design 3.4: job transcripts, child threads, extension status). */
const IGNORED_TYPES: ReadonlySet<string> = new Set(["job_message", "job_tool_use", "job_tool_result", "child_thread_spawned", "extension_status"])
/** Known types that map to nothing on a correlated gateway: the keepalive (watchdog only) and the legacy settle hint. */
const SILENT_TYPES: ReadonlySet<string> = new Set(["heartbeat", "thread_state_changed"])
/** TransportEvent kinds that carry no turnId by definition; a correlated frame may map to these and nothing else. */
const KINDS_WITHOUT_TURN: ReadonlySet<string> = new Set(["notice", "turn.approval_resolved", "resync.required", "turn.settle_hint"])

interface Frame {
  /** 1-based line of the fixture. */
  line: number
  name: string
  /** The data bytes exactly as on the `data:` line. */
  raw: string
  data: Record<string, unknown>
}

interface Replayed {
  frame: Frame
  /** parseGatewayEvent accepted the payload. */
  accepted: boolean
  mapped: TransportEvent[]
}

/** The golden file's frames; throws (failing the file) when it is missing or a line is not {event, data}. */
function readFixture(): Frame[] {
  let text: string
  try {
    text = readFileSync(FIXTURE, "utf8")
  } catch {
    throw new Error(`${FIXTURE} is missing: this test needs the repository checkout (ic/ next to tui/)`)
  }
  return text
    .split("\n")
    .filter((line) => line.length > 0)
    .map((line, index) => {
      const record = JSON.parse(line) as { event: string; data: Record<string, unknown> }
      const prefix = `{"event":${JSON.stringify(record.event)},"data":`
      if (!line.startsWith(prefix) || !line.endsWith("}")) throw new Error(`fixture line ${index + 1} is not {"event", "data"}`)
      const raw = line.slice(prefix.length, -1)
      if (JSON.stringify(JSON.parse(raw)) !== JSON.stringify(record.data)) throw new Error(`fixture line ${index + 1}: data bytes differ`)
      return { line: index + 1, name: record.event, raw, data: record.data }
    })
}

const str = (value: unknown): string | undefined => (typeof value === "string" ? value : undefined)

/**
 * Where the V2 approval continuation sits: the paused message's turn_finished{paused}, then the continuation's
 * turn_started and the uncorrelated gate_resolved, in either order. Null when the fixture has no such pair.
 */
function continuationAnchors(frames: readonly Frame[]): { started: number; resolved: number } | null {
  const paused = frames.findIndex((frame) => frame.name === "turn_finished" && frame.data.outcome === "paused")
  if (paused < 0) return null
  const after = (match: (frame: Frame) => boolean) => frames.findIndex((frame, index) => index > paused && match(frame))
  const started = after((frame) => frame.name === "turn_started")
  const resolved = after((frame) => frame.name === "gate_resolved" && str(frame.data.client_turn_id) === undefined)
  return started < 0 || resolved < 0 ? null : { started, resolved }
}

/** The other legal order of the continuation: the gate_resolved moved to the other side of its turn_started. */
function otherContinuationOrder(frames: readonly Frame[]): Frame[] {
  const anchors = continuationAnchors(frames)
  if (anchors === null) return [...frames]
  const out = [...frames]
  const [gate] = out.splice(anchors.resolved, 1)
  if (gate === undefined) return [...frames]
  // Removing the gate shifts the turn_started left when the gate came first.
  const started = anchors.resolved < anchors.started ? anchors.started - 1 : anchors.started
  out.splice(anchors.resolved < anchors.started ? started + 1 : started, 0, gate)
  return out
}

/** The bytes sse.rs writes, split into small chunks, through one SseParser. */
function parseWire(frames: readonly Frame[]): SseMessage[] {
  const wire = new TextEncoder().encode(frames.map((frame) => `event: ${frame.name}\ndata: ${frame.raw}\n\n`).join(""))
  const parser = new SseParser({ maxEventBytes: SSE_MAX_EVENT_BYTES })
  const messages: SseMessage[] = []
  for (let offset = 0; offset < wire.length; offset += 13) messages.push(...parser.push(wire.subarray(offset, offset + 13)))
  return messages
}

/** parseWire, then what gateway.ts deliver() does with each message. */
function replay(frames: readonly Frame[]): Replayed[] {
  const sent = new Set(frames.flatMap((frame) => str(frame.data.client_turn_id) ?? []))
  const ctx: GatewayWireContext = {
    correlated: true,
    resolveTurnId: (id) => (id !== undefined && sent.has(id) ? asTurnId(id) : null),
    decodeImage: (url) => decodeDataUrl(url),
  }
  return parseWire(frames).map((message, index) => {
    const frame = frames[index] as Frame
    const event = parseGatewayEvent(JSON.parse(message.data) as unknown)
    return { frame, accepted: event !== null, mapped: event === null ? [] : mapGatewayEvent(event, ctx) }
  })
}

const fixture = readFixture()
const orders: [string, Frame[]][] = [
  ["as written", fixture],
  ["with the V2 continuation's turn_started and gate_resolved in the other order", otherContinuationOrder(fixture)],
]

test("the fixture has the V2 continuation whose turn_started and gate_resolved may come in either order", () => {
  expect(fixture.length).toBeGreaterThan(20)
  const anchors = continuationAnchors(fixture)
  expect(anchors).not.toBeNull()
  const other = otherContinuationOrder(fixture)
  const flipped = continuationAnchors(other)
  expect(flipped !== null && anchors !== null && flipped.started < flipped.resolved).toBe(anchors !== null && anchors.started > anchors.resolved)
  expect(other.map((frame) => frame.line).sort((a, b) => a - b)).toEqual(fixture.map((frame) => frame.line))
})

describe.each(orders)("sse_current.ndjson %s", (_label, frames) => {
  let cached: Replayed[] | null = null
  /** Computed on first use, so every test also runs alone (bun test -t). */
  const replayedFrames = (): Replayed[] => (cached ??= replay(frames))

  test("SseParser reads every frame's event name and data bytes unchanged", () => {
    expect(parseWire(frames).map((message) => [message.event, message.data])).toEqual(frames.map((frame) => [frame.name, frame.raw]))
    expect(replayedFrames()).toHaveLength(frames.length)
  })

  test("every known type is accepted and mapped; every other type is one the client ignores on purpose", () => {
    const known = new Set<string>(GATEWAY_EVENT_TYPES)
    for (const { frame, accepted, mapped } of replayedFrames()) {
      const type = frame.name
      expect(frame.data.type).toBe(type)
      if (IGNORED_TYPES.has(type)) {
        expect([type, accepted]).toEqual([type, false])
        continue
      }
      // A new server event type must be classified: mapped in gateway-wire.ts, or listed in IGNORED_TYPES.
      expect([type, known.has(type)]).toEqual([type, true])
      expect([type, accepted]).toEqual([type, true])
      expect([type, mapped.length > 0]).toEqual([type, !SILENT_TYPES.has(type)])
      for (const event of mapped) {
        if (event.kind === "turn.finished") expect(event.outcome).not.toBe("unknown")
        if (event.kind === "turn.approval_resolved") expect(event.resolution).not.toBe("unknown")
        // image_generated must decode, not turn into the "rejected" notice
        if (type === "image_generated") expect(event.kind).toBe("turn.image")
      }
    }
    // ... and the server's fixture samples every type the client parses.
    const seen = new Set(replayedFrames().map(({ frame }) => frame.name))
    expect(GATEWAY_EVENT_TYPES.filter((type) => !seen.has(type))).toEqual([])
  })

  test("correlated events resolve to their client_turn_id; uncorrelated replies are background", () => {
    let correlated = 0
    for (const { frame, mapped } of replayedFrames()) {
      const id = str(frame.data.client_turn_id)
      for (const event of mapped) {
        if (id === undefined) {
          if (frame.name === "response") expect(event.kind).toBe("background")
          continue
        }
        correlated += 1
        expect(event.kind).not.toBe("background")
        if ("turnId" in event) expect([frame.line, frame.name, event.turnId]).toEqual([frame.line, frame.name, id])
        else expect([frame.name, KINDS_WITHOUT_TURN.has(event.kind)]).toEqual([frame.name, true])
      }
    }
    expect(correlated).toBeGreaterThan(10)
  })

  test("each message: turn.started first, its final before the one turn.finished, nothing after it", () => {
    const byMessage = new Map<string, string[]>()
    const outcomes = new Map<string, string>()
    for (const { frame, mapped } of replayedFrames()) {
      const messageId = str(frame.data.message_id)
      if (messageId === undefined) continue
      const kinds = byMessage.get(messageId) ?? []
      for (const event of mapped) {
        kinds.push(event.kind)
        if (event.kind === "turn.finished") outcomes.set(messageId, event.outcome)
      }
      byMessage.set(messageId, kinds)
    }
    expect(byMessage.size).toBeGreaterThanOrEqual(3)
    for (const [messageId, kinds] of byMessage) {
      expect([messageId, kinds[0]]).toEqual([messageId, "turn.started"])
      expect([messageId, kinds.filter((kind) => kind === "turn.finished").length]).toEqual([messageId, 1])
      expect([messageId, kinds.at(-1)]).toEqual([messageId, "turn.finished"])
      const finals = kinds.flatMap((kind, index) => (kind === "turn.final" ? [index] : []))
      for (const index of finals) expect(index).toBeLessThan(kinds.length - 1)
      // a replied message has its reply before it finishes; a paused one has none
      if (outcomes.get(messageId) === "replied") expect([messageId, finals.length > 0]).toEqual([messageId, true])
    }
  })
})
