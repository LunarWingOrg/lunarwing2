import { describe, expect, test } from "bun:test"
import {
  approvalFromHistory,
  approvalFromPendingGate,
  capabilitiesFromChat,
  decodeGatewayEvent,
  decodeGeneratedImage,
  GATE_RESOLUTIONS,
  legacyGatewayCapabilities,
  legacyGatewayLimits,
  limitsFromWire,
  normalizeResolution,
  parseChatCapabilities,
  parseGatewayEvent,
  parseHistoryResponse,
  parsePendingGates,
  sniffAllowedImage,
  type ChatCapabilities,
  type GatewayWireContext,
} from "../../src/transport/gateway-wire.ts"
import { TURN_OUTCOMES, type TransportEvent } from "../../src/transport/types.ts"

const T = "5f0e4a52-6a3e-4a8f-9a1e-2b9d6c1e7a10"
const C = "0193f2a1-7c3e-4d4a-b1e2-6f0c9a8b7d6e"
const M = "0b8a6c1e-1111-4222-8333-944455556666"
const R = "6c0f2a8e-1f9b-4d3e-a1c2-7b5e9d0f4a21"

const current: GatewayWireContext = { correlated: true }
const legacy: GatewayWireContext = { correlated: false }
const corr = { message_id: M, client_turn_id: C }

const PNG = Uint8Array.from(Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==", "base64"))
const dataUrl = (mime: string, bytes: Uint8Array) => `data:${mime};base64,${Buffer.from(bytes).toString("base64")}`

function map(event: Record<string, unknown>, ctx: GatewayWireContext = current): TransportEvent[] {
  return decodeGatewayEvent(event, ctx)
}

describe("gateway SSE → TransportEvent (3.4 table)", () => {
  test("turn_started → turn.started; ignored without a client id or with an unknown one", () => {
    expect(map({ type: "turn_started", thread_id: T, ...corr })).toEqual([{ kind: "turn.started", turnId: C as never, threadId: T }])
    expect(map({ type: "turn_started", thread_id: T, message_id: M })).toEqual([])
    expect(map({ type: "turn_started", thread_id: T, ...corr }, { correlated: true, resolveTurnId: () => null })).toEqual([])
  })

  test("turn_finished → turn.finished; outcome strings map one to one", () => {
    for (const outcome of TURN_OUTCOMES) {
      expect(map({ type: "turn_finished", thread_id: T, outcome, ...corr })).toEqual([{ kind: "turn.finished", turnId: C as never, outcome }])
    }
    expect(map({ type: "turn_finished", thread_id: T, outcome: "exploded", ...corr })).toEqual([
      { kind: "turn.finished", turnId: C as never, outcome: "unknown" },
    ])
    expect(map({ type: "turn_finished", thread_id: T, outcome: "replied" })).toEqual([])
  })

  test("response → turn.final with the client id", () => {
    expect(map({ type: "response", content: "hello", thread_id: T, ...corr })).toEqual([
      { kind: "turn.final", turnId: C as never, threadId: T, text: "hello" },
    ])
  })

  test("response starting with 'Error: ' → turn.error{agent_error}", () => {
    expect(map({ type: "response", content: "Error: tool failed", thread_id: T, ...corr })).toEqual([
      { kind: "turn.error", turnId: C as never, threadId: T, code: "agent_error", message: "tool failed", retryable: false },
    ])
    expect(map({ type: "response", content: "Error: boom", thread_id: T }, legacy)).toEqual([
      { kind: "turn.error", turnId: null, threadId: T, code: "agent_error", message: "boom", retryable: false },
    ])
  })

  test("V1's 'Error: Steer note queue full' stays a final so it can be matched as a steer ack", () => {
    const content = "Error: Steer note queue full (8 notes pending). Wait for the agent to pick them up."
    expect(map({ type: "response", content, thread_id: T }, legacy)).toEqual([{ kind: "turn.final", turnId: null, threadId: T, text: content }])
  })

  test("response without correlation: background on a correlated gateway, an uncorrelated final on a legacy one", () => {
    expect(map({ type: "response", content: "proactive", thread_id: T })).toEqual([{ kind: "background", threadId: T, text: "proactive" }])
    expect(map({ type: "response", content: "Error: proactive", thread_id: T })).toEqual([
      { kind: "background", threadId: T, text: "Error: proactive" },
    ])
    expect(map({ type: "response", content: "reply", thread_id: T }, legacy)).toEqual([
      { kind: "turn.final", turnId: null, threadId: T, text: "reply" },
    ])
  })

  test("stream_chunk → turn.delta", () => {
    expect(map({ type: "stream_chunk", content: "Let me", thread_id: T, ...corr })).toEqual([
      { kind: "turn.delta", turnId: C as never, threadId: T, text: "Let me" },
    ])
  })

  test("thinking → turn.activity{thinking}", () => {
    expect(map({ type: "thinking", message: "Processing...", thread_id: T, ...corr })).toEqual([
      { kind: "turn.activity", turnId: C as never, threadId: T, activity: { kind: "thinking", text: "Processing..." } },
    ])
  })

  test("tool_started / tool_completed / tool_result → turn.activity", () => {
    expect(map({ type: "tool_started", name: "http", thread_id: T })).toEqual([
      { kind: "turn.activity", turnId: null, threadId: T, activity: { kind: "tool_started", text: "http" } },
    ])
    expect(map({ type: "tool_completed", name: "image_generate", success: true, parameters: "8123ms", thread_id: T })).toEqual([
      { kind: "turn.activity", turnId: null, threadId: T, activity: { kind: "tool_completed", text: "image_generate (8123ms)", ok: true } },
    ])
    const failed = map({ type: "tool_completed", name: "http", success: false, error: "refused\nstack", parameters: '{\n "url": 1\n}' })
    expect(failed).toEqual([{ kind: "turn.activity", turnId: null, activity: { kind: "tool_completed", text: "http: refused", ok: false } }])
    const preview = "x".repeat(500)
    const [result] = map({ type: "tool_result", name: "shell", preview, thread_id: T })
    expect(result?.kind).toBe("turn.activity")
    if (result?.kind === "turn.activity") {
      expect(result.activity.kind).toBe("tool_result")
      expect(Array.from(result.activity.text).length).toBe(200)
      expect(result.activity.text.startsWith("shell: xxx")).toBe(true)
    }
  })

  test("status: empty ignored; activity; settle hints on legacy gateways only", () => {
    expect(map({ type: "status", message: "", thread_id: T }, legacy)).toEqual([])
    expect(map({ type: "status", message: "Tool call denied.", thread_id: T })).toEqual([
      { kind: "turn.activity", turnId: null, threadId: T, activity: { kind: "status", text: "Tool call denied." } },
    ])
    expect(map({ type: "status", message: "Done", thread_id: T })).toHaveLength(1)
    const signals = (message: string) =>
      map({ type: "status", message, thread_id: T }, legacy).flatMap((e) => (e.kind === "turn.settle_hint" ? [e.signal] : []))
    expect(signals("Done")).toEqual(["done"])
    expect(signals("Rejected")).toEqual(["rejected"])
    expect(signals("Awaiting approval")).toEqual(["awaiting_approval"])
    expect(signals("Processing...")).toEqual([])
  })

  test("approval_needed and gate_required{Approval} → turn.approval", () => {
    const fields = { request_id: R, tool_name: "shell", description: "Run", parameters: "{}", thread_id: T }
    const approval = { id: R, threadId: T, toolName: "shell", description: "Run", parameters: "{}", truncated: false, source: "live" as const }
    expect(map({ type: "approval_needed", ...fields, allow_always: true, ...corr })).toEqual([
      { kind: "turn.approval", turnId: C as never, threadId: T, approval: { ...approval, allowAlways: true } },
    ])
    expect(map({ type: "gate_required", ...fields, gate_name: "approval", resume_kind: { Approval: { allow_always: false } } })).toEqual([
      { kind: "turn.approval", turnId: null, threadId: T, approval: { ...approval, allowAlways: false } },
    ])
  })

  test("gate_required{Authentication} → turn.auth_required with requestId; External → notice{auth}", () => {
    const base = { request_id: R, gate_name: "auth", tool_name: "github", description: "", parameters: "{}", thread_id: T }
    const auth = { Authentication: { credential_name: "github", instructions: "Paste a token", auth_url: null } }
    expect(map({ type: "gate_required", ...base, resume_kind: auth, ...corr })).toEqual([
      { kind: "turn.auth_required", turnId: C as never, threadId: T, extensionName: "github", requestId: R, instructions: "Paste a token" },
    ])
    const [notice] = map({ type: "gate_required", ...base, gate_name: "webhook", resume_kind: { External: { callback_id: "cb" } } })
    expect(notice).toEqual({ kind: "notice", level: "info", source: "auth", text: "waiting for external confirmation (webhook)" })
  })

  test("every gate_resolved resolution string normalizes as in 3.4", () => {
    const expected: Record<string, string> = {
      approved: "approve",
      approved_always: "always",
      denied: "deny",
      credential_provided: "credential",
      external_callback: "external",
      cancelled: "cancelled",
      thread_terminated: "terminated",
      expired: "expired",
    }
    expect({ ...GATE_RESOLUTIONS }).toEqual(expected as never)
    for (const [wire, resolution] of Object.entries(expected)) {
      expect(
        map({ type: "gate_resolved", request_id: R, gate_name: "g", tool_name: "t", resolution: wire, message: "", thread_id: T }),
      ).toEqual([{ kind: "turn.approval_resolved", approvalId: R, resolution: resolution as never }])
    }
    for (const odd of ["approve", "APPROVED", "", "__proto__", "toString"]) expect(normalizeResolution(odd)).toBe("unknown")
  })

  test("auth_required → turn.auth_required; auth_completed → notice{auth}", () => {
    expect(map({ type: "auth_required", extension_name: "notion", instructions: "Paste", auth_url: "https://x", setup_url: "https://y" })).toEqual([
      { kind: "turn.auth_required", turnId: null, extensionName: "notion", instructions: "Paste", authUrl: "https://x", setupUrl: "https://y" },
    ])
    expect(map({ type: "auth_completed", extension_name: "notion", success: true, message: "Connected" })).toEqual([
      { kind: "notice", level: "info", source: "auth", text: "Connected" },
    ])
    expect(map({ type: "auth_completed", extension_name: "notion", success: false, message: "" })).toEqual([
      { kind: "notice", level: "warn", source: "auth", text: "notion: authentication failed" },
    ])
  })

  test("image_generated → turn.image after validation; a path is only a caption", () => {
    const [image] = map({ type: "image_generated", data_url: dataUrl("image/png", PNG), path: "/srv/x.png", thread_id: T, ...corr })
    expect(image?.kind).toBe("turn.image")
    if (image?.kind === "turn.image") {
      expect(image.turnId).toBe(C as never)
      expect(image.image.mime).toBe("image/png")
      expect(Array.from(image.image.bytes)).toEqual(Array.from(PNG))
      expect(image.image.caption).toBe("/srv/x.png")
    }
  })

  test("image_generated with an SVG data URL is rejected", () => {
    const svg = new TextEncoder().encode('<svg xmlns="http://www.w3.org/2000/svg"/>')
    const events = map({ type: "image_generated", data_url: dataUrl("image/svg+xml", svg), thread_id: T })
    expect(events).toEqual([
      { kind: "notice", level: "warn", source: "image", text: "a generated image was rejected: not a PNG, JPEG, GIF or WebP data URL" },
    ])
  })

  test("suggestions, turn_cost, reasoning_update, skill_activated", () => {
    expect(map({ type: "suggestions", suggestions: ["a", "b"], thread_id: T })).toEqual([
      { kind: "turn.suggestions", turnId: null, threadId: T, suggestions: ["a", "b"] },
    ])
    expect(map({ type: "turn_cost", input_tokens: 10, output_tokens: 5, cost_usd: "0.001", thread_id: T })).toEqual([
      { kind: "turn.cost", turnId: null, threadId: T, inputTokens: 10, outputTokens: 5, costUsd: "0.001" },
    ])
    expect(map({ type: "reasoning_update", narrative: "Checking the registry", decisions: [], thread_id: T })).toEqual([
      { kind: "turn.activity", turnId: null, threadId: T, activity: { kind: "reasoning", text: "Checking the registry" } },
    ])
    expect(map({ type: "reasoning_update", narrative: "", decisions: [{ tool_name: "http", rationale: "fetch" }] })).toEqual([
      { kind: "turn.activity", turnId: null, activity: { kind: "reasoning", text: "http: fetch" } },
    ])
    expect(map({ type: "skill_activated", skill_names: ["ops", "git"], thread_id: T })).toEqual([
      { kind: "turn.activity", turnId: null, threadId: T, activity: { kind: "skill", text: "skills: ops, git" } },
    ])
  })

  test("turn_recovery: failed → turn.error{recovery_failed}, else turn.activity{recovery}", () => {
    const base = { type: "turn_recovery", thread_id: T, failure_kind: "model", recovery_attempt: 1 }
    expect(map({ ...base, recovery_state: "failed", message: "gave up", ...corr })).toEqual([
      { kind: "turn.error", turnId: C as never, threadId: T, code: "recovery_failed", message: "gave up", retryable: false },
    ])
    expect(map({ ...base, recovery_state: "retrying", message: "attempt 1" })).toEqual([
      { kind: "turn.activity", turnId: null, threadId: T, activity: { kind: "recovery", text: "retrying: attempt 1" } },
    ])
  })

  test("legacy turn_recovery failed is progress: the following 'Error: ' response is the one terminal event", () => {
    const base = { type: "turn_recovery", thread_id: T, failure_kind: "model", recovery_attempt: 1 }
    expect(map({ ...base, recovery_state: "failed", message: "gave up" }, legacy)).toEqual([
      { kind: "turn.activity", turnId: null, threadId: T, activity: { kind: "recovery", text: "failed: gave up", ok: false } },
    ])
    expect(map({ type: "response", content: "Error: gave up", thread_id: T }, legacy)).toEqual([
      { kind: "turn.error", turnId: null, threadId: T, code: "agent_error", message: "gave up", retryable: false },
    ])
  })

  test("job_started, job_result, job_status → notice{job}; URLs are text", () => {
    expect(map({ type: "job_started", job_id: "j1", title: "Build", browse_url: "http://x/j1" })).toEqual([
      { kind: "notice", level: "info", source: "job", text: "job started: Build (http://x/j1)" },
    ])
    expect(map({ type: "job_status", job_id: "j1", message: "running" })).toEqual([{ kind: "notice", level: "info", source: "job", text: "job j1: running" }])
    expect(map({ type: "job_result", job_id: "j1", status: "completed" })).toEqual([
      { kind: "notice", level: "info", source: "job", text: "job j1 completed" },
    ])
  })

  test("stream_gap → resync.required{gap}", () => {
    expect(map({ type: "stream_gap", missed_events: 12 })).toEqual([{ kind: "resync.required", reason: "gap" }])
  })

  test("thread_state_changed to a terminal state → settle hint, only without lifecycle events", () => {
    for (const to_state of ["Completed", "Done", "Failed"]) {
      expect(map({ type: "thread_state_changed", thread_id: T, from_state: "Running", to_state }, legacy)).toEqual([
        { kind: "turn.settle_hint", threadId: T, signal: "thread_terminal" },
      ])
      expect(map({ type: "thread_state_changed", thread_id: T, from_state: "Running", to_state })).toEqual([])
    }
    expect(map({ type: "thread_state_changed", thread_id: T, from_state: "Created", to_state: "Running" }, legacy)).toEqual([])
  })

  test("error → turn.error{server}", () => {
    expect(map({ type: "error", message: "bad", thread_id: T })).toEqual([
      { kind: "turn.error", turnId: null, threadId: T, code: "server", message: "bad", retryable: false },
    ])
  })

  test("heartbeat and the ignored types produce nothing", () => {
    for (const type of ["heartbeat", "job_message", "job_tool_use", "job_tool_result", "child_thread_spawned", "extension_status", "brand_new"]) {
      expect(map({ type, job_id: "j", role: "x", content: "y", extension_name: "e", status: "s" })).toEqual([])
    }
  })

  test("malformed payloads of known types are ignored", () => {
    expect(parseGatewayEvent({ type: "response", content: 5, thread_id: T })).toBeNull()
    expect(parseGatewayEvent({ type: "response", content: "x" })).toBeNull()
    expect(parseGatewayEvent({ type: "approval_needed", request_id: R })).toBeNull()
    expect(parseGatewayEvent({ type: "thinking", message: "x", thread_id: 7 })).toBeNull()
    expect(parseGatewayEvent({ type: "thinking", message: "x", client_turn_id: 7 })).toBeNull()
    expect(parseGatewayEvent([{ type: "thinking" }])).toBeNull()
    expect(parseGatewayEvent(null)).toBeNull()
    expect(parseGatewayEvent({ type: "toString" })).toBeNull()
  })

  test("a client id that breaks the id rule is never used", () => {
    expect(map({ type: "response", content: "x", thread_id: T, client_turn_id: "has space" })).toEqual([
      { kind: "background", threadId: T, text: "x" },
    ])
  })
})

describe("generated image validation (6.4)", () => {
  test("the four allowed types decode; the declared type must match the bytes", () => {
    expect(decodeGeneratedImage(dataUrl("image/png", PNG))?.mime).toBe("image/png")
    const jpeg = Uint8Array.of(0xff, 0xd8, 0xff, 0xe0, 0, 0x10)
    expect(decodeGeneratedImage(dataUrl("image/jpeg", jpeg))?.mime).toBe("image/jpeg")
    const gif = new TextEncoder().encode("GIF89a\x01\x00\x01\x00")
    expect(decodeGeneratedImage(dataUrl("image/gif", gif))?.mime).toBe("image/gif")
    const webp = new TextEncoder().encode("RIFF\x10\x00\x00\x00WEBPVP8 ")
    expect(decodeGeneratedImage(dataUrl("image/webp", webp))?.mime).toBe("image/webp")
    expect(decodeGeneratedImage(dataUrl("image/jpeg", PNG))).toBeNull()
  })

  test("non-base64, empty, oversize and BMP data URLs are rejected", () => {
    expect(decodeGeneratedImage("data:image/png;base64,!!!!")).toBeNull()
    expect(decodeGeneratedImage("data:image/png;base64,")).toBeNull()
    expect(decodeGeneratedImage("data:image/png,rawbytes")).toBeNull()
    expect(decodeGeneratedImage(`data:image/png;base64,${"A".repeat(16 * 1024 * 1024 + 4)}`)).toBeNull()
    expect(decodeGeneratedImage(dataUrl("image/bmp", new TextEncoder().encode("BM\x00\x00")))).toBeNull()
    expect(sniffAllowedImage(new TextEncoder().encode("BM\x00\x00"))).toBeNull()
    expect(sniffAllowedImage(new Uint8Array(0))).toBeNull()
  })
})

describe("capabilities and limits (3.7 folding table)", () => {
  const caps: ChatCapabilities = {
    api_version: 1,
    server_version: "2.1.9",
    engine: "v2",
    features: ["status", "attachments", "transcribe", "stream"],
    limits: {
      max_content_bytes: 100000,
      max_attachments: 5,
      max_attachment_bytes: 5242880,
      max_total_attachment_bytes: 7340032,
      attachment_mime_types: ["image/png", "image/jpeg", "image/gif", "image/webp", "image/svg+xml"],
      max_transcribe_b64_bytes: 25165824,
      transcribe_formats: ["wav"],
      messages_per_minute: 30,
      transcribes_per_minute: 10,
      max_request_bytes: 10485760,
    },
    voice: { provider: "lunarvoice", stt: true, tts: true },
    credential: "client",
  }

  test("the 4.2.2 document parses; a broken one does not", () => {
    expect(parseChatCapabilities(caps)).toEqual(caps)
    expect(parseChatCapabilities({ ...caps, engine: "v3" })).toBeNull()
    expect(parseChatCapabilities({ ...caps, limits: { max_content_bytes: 1 } })).toBeNull()
    expect(parseChatCapabilities({ ...caps, credential: "root" })).toBeNull()
  })

  test("WireLimits fold onto Limits; 18 MiB of WAV for 24 MiB of base64; MIME types intersected", () => {
    expect(limitsFromWire(caps.limits)).toEqual({
      maxContentBytes: 100000,
      maxImages: 5,
      maxImageBytes: 5242880,
      maxTotalImageBytes: 7340032,
      maxRequestBytes: 10485760,
      imageMimeTypes: ["image/png", "image/jpeg", "image/gif", "image/webp"],
      maxTranscribeWavBytes: 18 * 1024 * 1024,
      transcribePerMinute: 10,
      chatPerMinute: 30,
      maxInFlight: 8,
    })
  })

  test("legacy limits: today's server values; no transcription without voice", () => {
    const voice = { configured: true, max_audio_b64_len: 25165824 }
    expect(legacyGatewayLimits(voice)).toEqual({
      maxContentBytes: 100000,
      maxImages: 5,
      maxImageBytes: 5 * 1024 * 1024,
      maxTotalImageBytes: 7 * 1024 * 1024,
      maxRequestBytes: 10 * 1024 * 1024,
      imageMimeTypes: ["image/png", "image/jpeg", "image/gif", "image/webp"],
      maxTranscribeWavBytes: 18 * 1024 * 1024,
      transcribePerMinute: 10,
      chatPerMinute: 30,
      maxInFlight: 8,
    })
    expect(legacyGatewayLimits({ configured: false, max_audio_b64_len: 25165824 }).maxTranscribeWavBytes).toBe(0)
  })

  test("capabilities: current from features; legacy defaults", () => {
    expect(capabilitiesFromChat(caps, { configured: true, max_audio_b64_len: 1 })).toEqual({
      streaming: true,
      activity: true,
      images: true,
      generatedImages: true,
      approvals: true,
      transcribe: true,
      correlation: true,
      lifecycle: true,
      interrupt: "text",
      steer: "text",
      threads: true,
      history: true,
      backgroundMessages: true,
      resume: true,
    })
    const v1 = capabilitiesFromChat({ ...caps, engine: "v1", features: ["status", "attachments"] }, null)
    expect(v1.streaming).toBe(false)
    expect(v1.transcribe).toBe(false)
    const old = legacyGatewayCapabilities({ configured: true, max_audio_b64_len: 1 })
    expect(old).toMatchObject({ streaming: "unknown", generatedImages: "unknown", correlation: false, lifecycle: false, transcribe: true })
    expect(legacyGatewayCapabilities(null).transcribe).toBe(false)
  })
})

describe("history and gate DTOs", () => {
  test("history with a pending approval; null responses", () => {
    const history = parseHistoryResponse({
      thread_id: T,
      turns: [{ turn_number: 0, user_input: "hi", response: null, state: "Processing", started_at: "2026-11-03T00:00:00Z", completed_at: null, tool_calls: [] }],
      has_more: false,
      pending_approval: { request_id: R, tool_name: "shell", description: "d", parameters: "{}" },
    })
    expect(history?.turns[0]?.response).toBeNull()
    expect(history?.pending_approval?.request_id).toBe(R)
    expect(approvalFromHistory(history?.pending_approval ?? { request_id: "", tool_name: "", description: "", parameters: "" }, T)).toEqual({
      id: R,
      threadId: T,
      toolName: "shell",
      description: "d",
      parameters: "{}",
      allowAlways: false,
      truncated: false,
      source: "history",
    })
    expect(parseHistoryResponse({ thread_id: T, turns: [{ user_input: 1 }] })).toBeNull()
  })

  test("pending gates: approval gates become prompts; others and malformed entries are skipped", () => {
    const gates = parsePendingGates({
      gates: [
        { request_id: R, thread_id: T, gate_name: "approval", tool_name: "shell", description: "d", parameters: "{}", resume_kind: { Approval: { allow_always: true } } },
        { request_id: R, thread_id: T, gate_name: "auth", tool_name: "gh", description: "", parameters: "{}", resume_kind: { Authentication: { credential_name: "gh", instructions: "", auth_url: null } } },
        { request_id: R },
      ],
    })
    expect(gates).toHaveLength(2)
    expect(gates.map(approvalFromPendingGate).map((a) => a?.allowAlways ?? null)).toEqual([true, null])
  })
})
