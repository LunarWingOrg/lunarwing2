/**
 * Gateway wire types and the SSE → TransportEvent mapper (design 3.4 gateway table, 4.2.12). The same mapper
 * decodes REPLv3 Status.event objects in M4 (pass a resolveTurnId that returns the frame's msg_id).
 *
 * Request DTOs mirror ic/src/channels/web/types.rs (none of them deny unknown fields, so `client_turn_id` is
 * ignored by daemons that predate M2a). Event payloads are validated field by field: an event of a known type with
 * a malformed payload is ignored rather than trusted.
 */
import { IMAGE_MIME_TYPES, type ImageMime, type ThreadId, type TurnId } from "../state/model.ts"
import { asTurnId } from "../state/ids.ts"
import { controlAckKind } from "./control-acks.ts"
import {
  toFinishOutcome,
  type ApprovalResolution,
  type Capabilities,
  type HistoryTurn,
  type Limits,
  type TransportEvent,
  type WireApproval,
  type WireAuthGate,
  type WireLimits,
} from "./types.ts"

export type { WireLimits } from "./types.ts"

// ---------------------------------------------------------------------------------------------------------
// Request and response DTOs (4.2.2-4.2.12; area-gateway 1.12 for history, threads, gates and voice)
// ---------------------------------------------------------------------------------------------------------

export interface ChatCapabilities {
  api_version: number
  server_version: string
  engine: "v1" | "v2"
  features: string[]
  limits: WireLimits
  voice: { provider: string | null; stt: boolean; tts: boolean }
  credential: "master" | "client"
}

export interface ImageData {
  media_type: ImageMime
  /** Base64 without a data: prefix. */
  data: string
}

export interface SendMessageRequest {
  content: string
  thread_id: string
  timezone?: string
  images?: ImageData[]
  client_turn_id?: string
}

export interface SendMessageResponse {
  message_id: string
  status: "accepted"
}

export interface ApprovalRequestBody {
  request_id: string
  action: "approve" | "always" | "deny"
  thread_id?: string
  client_turn_id?: string
}

export interface AuthTokenRequestBody {
  extension_name: string
  token: string
  request_id?: string
  thread_id?: string
  client_turn_id?: string
}

export interface AuthCancelRequestBody {
  extension_name: string
  request_id?: string
  thread_id?: string
  client_turn_id?: string
}

export type ResumeKind =
  | { Approval: { allow_always: boolean } }
  | { Authentication: { credential_name: string; instructions: string; auth_url: string | null } }
  | { External: { callback_id: string } }

export interface ThreadInfo {
  id: string
  state: string
  turn_count: number
  created_at: string
  updated_at: string
  title?: string
  thread_type?: string
  channel?: string
}

export interface ThreadListResponse {
  assistant_thread: ThreadInfo | null
  threads: ThreadInfo[]
  active_thread: string | null
}

export interface TurnInfo {
  turn_number: number
  user_input: string
  response: string | null
  state: string
  started_at: string
  completed_at: string | null
}

export interface PendingApprovalInfo {
  request_id: string
  tool_name: string
  description: string
  parameters: string
}

export interface HistoryResponse {
  thread_id: string
  turns: TurnInfo[]
  has_more: boolean
  oldest_timestamp?: string
  pending_approval?: PendingApprovalInfo
}

export interface PendingGateView {
  request_id: string
  thread_id: string
  gate_name: string
  tool_name: string
  description: string
  parameters: string
  resume_kind: ResumeKind
}

export interface PendingGatesResponse {
  gates: PendingGateView[]
}

export interface VoiceStatusResponse {
  configured: boolean
  model?: string
  max_audio_b64_len: number
  max_text_chars?: number
  max_synth_audio_secs?: number
  /** M2a additions. */
  stt?: boolean
  tts?: boolean
  provider?: string | null
}

export interface VoiceTranscribeResponse {
  text: string
  language?: string
  confidence?: number
  model?: string
}

/** Voice handlers answer errors as JSON {"error","message"} (4.2.10). */
export interface VoiceErrorBody {
  error: string
  message: string
}

/**
 * GET /api/gateway/status (server.rs GatewayStatusResponse), the one field the client reads: whole seconds since
 * the gateway channel of this daemon process was created (GatewayState.startup_time).
 */
export interface GatewayStatusResponse {
  uptime_secs: number
}

/** auth-token / auth-cancel answer 200 with ActionResponse (types.rs ActionResponse). */
export interface ActionResponse {
  success: boolean
  message: string
}

type Corr = { message_id?: string; client_turn_id?: string }

export type GatewayEvent = Corr &
  (
    | { type: "turn_started"; thread_id: string }
    | { type: "turn_finished"; thread_id: string; outcome: string }
    | { type: "response"; content: string; thread_id: string; emotes?: string[] }
    | { type: "thinking"; message: string; thread_id?: string }
    | { type: "stream_chunk"; content: string; thread_id?: string }
    | { type: "stream_gap"; missed_events: number }
    | { type: "tool_started"; name: string; thread_id?: string }
    | { type: "tool_result"; name: string; preview: string; thread_id?: string }
    | {
        type: "tool_completed"
        name: string
        success: boolean
        error?: string
        parameters?: string
        thread_id?: string
      }
    | {
        type: "reasoning_update"
        narrative: string
        decisions: { tool_name: string; rationale: string }[]
        thread_id?: string
      }
    | { type: "status"; message: string; thread_id?: string }
    | {
        type: "approval_needed"
        request_id: string
        tool_name: string
        description: string
        parameters: string
        thread_id?: string
        allow_always: boolean
      }
    | {
        type: "gate_required"
        request_id: string
        gate_name: string
        tool_name: string
        description: string
        parameters: string
        resume_kind: ResumeKind
        thread_id?: string
      }
    | {
        type: "gate_resolved"
        request_id: string
        gate_name: string
        tool_name: string
        resolution: string
        message: string
        thread_id?: string
      }
    | { type: "auth_required"; extension_name: string; instructions?: string; auth_url?: string; setup_url?: string }
    | { type: "auth_completed"; extension_name: string; success: boolean; message: string }
    | { type: "image_generated"; data_url: string; path?: string; thread_id?: string }
    | { type: "turn_cost"; input_tokens: number; output_tokens: number; cost_usd: string; thread_id?: string }
    | { type: "suggestions"; suggestions: string[]; thread_id?: string }
    | { type: "skill_activated"; skill_names: string[]; thread_id?: string }
    | { type: "thread_state_changed"; thread_id: string; from_state: string; to_state: string; reason?: string }
    | {
        type: "turn_recovery"
        thread_id: string
        failure_kind: string
        recovery_state: string
        recovery_attempt: number
        message: string
      }
    | { type: "job_started"; job_id: string; title: string; browse_url: string }
    | { type: "job_status"; job_id: string; message: string }
    | { type: "job_result"; job_id: string; status: string; session_id?: string }
    | { type: "error"; message: string; thread_id?: string }
    | { type: "heartbeat" }
  )

export type GatewayEventType = GatewayEvent["type"]
export type GatewayEventOf<T extends GatewayEventType> = Extract<GatewayEvent, { type: T }>

// ---------------------------------------------------------------------------------------------------------
// Field validation
// ---------------------------------------------------------------------------------------------------------

type Obj = Record<string, unknown>

function isObj(value: unknown): value is Obj {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

const isStr = (v: unknown): v is string => typeof v === "string"
const isBool = (v: unknown): v is boolean => typeof v === "boolean"
const isNum = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v)
const optStr = (v: unknown): boolean => v === undefined || v === null || isStr(v)
const isStrArray = (v: unknown): v is string[] => Array.isArray(v) && v.every(isStr)

function isResumeKind(value: unknown): value is ResumeKind {
  if (!isObj(value)) return false
  const approval = value.Approval
  if (isObj(approval)) return isBool(approval.allow_always)
  const auth = value.Authentication
  if (isObj(auth)) return isStr(auth.credential_name) && optStr(auth.instructions) && optStr(auth.auth_url)
  const external = value.External
  return isObj(external) && isStr(external.callback_id)
}

const isDecision = (v: unknown): boolean => isObj(v) && isStr(v.tool_name) && isStr(v.rationale)

/** Required-field checks per known event type; optional fields are checked where they are read. */
const EVENT_SHAPES: { readonly [K in GatewayEventType]: (o: Obj) => boolean } = {
  turn_started: (o) => isStr(o.thread_id),
  turn_finished: (o) => isStr(o.thread_id) && isStr(o.outcome),
  response: (o) => isStr(o.content) && isStr(o.thread_id),
  thinking: (o) => isStr(o.message),
  stream_chunk: (o) => isStr(o.content),
  stream_gap: (o) => isNum(o.missed_events),
  tool_started: (o) => isStr(o.name),
  tool_result: (o) => isStr(o.name) && isStr(o.preview),
  tool_completed: (o) => isStr(o.name) && isBool(o.success) && optStr(o.error) && optStr(o.parameters),
  reasoning_update: (o) => isStr(o.narrative) && Array.isArray(o.decisions) && o.decisions.every(isDecision),
  status: (o) => isStr(o.message),
  approval_needed: (o) =>
    isStr(o.request_id) && isStr(o.tool_name) && isStr(o.description) && isStr(o.parameters) && isBool(o.allow_always),
  gate_required: (o) =>
    isStr(o.request_id) &&
    isStr(o.gate_name) &&
    isStr(o.tool_name) &&
    isStr(o.description) &&
    isStr(o.parameters) &&
    isResumeKind(o.resume_kind),
  gate_resolved: (o) => isStr(o.request_id) && isStr(o.resolution),
  auth_required: (o) => isStr(o.extension_name) && optStr(o.instructions) && optStr(o.auth_url) && optStr(o.setup_url),
  auth_completed: (o) => isStr(o.extension_name) && isBool(o.success),
  image_generated: (o) => isStr(o.data_url) && optStr(o.path),
  turn_cost: (o) => isNum(o.input_tokens) && isNum(o.output_tokens) && isStr(o.cost_usd),
  suggestions: (o) => isStrArray(o.suggestions),
  skill_activated: (o) => isStrArray(o.skill_names),
  thread_state_changed: (o) => isStr(o.thread_id) && isStr(o.to_state),
  turn_recovery: (o) => isStr(o.recovery_state),
  job_started: (o) => isStr(o.job_id) && isStr(o.title),
  job_status: (o) => isStr(o.job_id) && isStr(o.message),
  job_result: (o) => isStr(o.job_id) && isStr(o.status),
  error: (o) => isStr(o.message),
  heartbeat: () => true,
}

function isKnownType(type: string): type is GatewayEventType {
  return Object.hasOwn(EVENT_SHAPES, type)
}

/** Every event type the client parses (the GatewayEvent union); the golden-fixture test checks it against the server. */
export const GATEWAY_EVENT_TYPES: readonly GatewayEventType[] = Object.freeze(Object.keys(EVENT_SHAPES) as GatewayEventType[])

/**
 * The type guard of design 3.8 ("JSON.parse → type guard → mapper"): a known event type with the fields the
 * mapper reads, else null (unknown types such as job_message, extension_status and child_thread_spawned
 * included). `thread_id`, `message_id` and `client_turn_id` must be strings when present.
 */
export function parseGatewayEvent(value: unknown): GatewayEvent | null {
  if (!isObj(value) || !isStr(value.type) || !isKnownType(value.type)) return null
  if (!optStr(value.thread_id) || !optStr(value.message_id) || !optStr(value.client_turn_id)) return null
  return EVENT_SHAPES[value.type](value) ? (value as GatewayEvent) : null
}

export function isGatewayEvent(value: unknown): value is GatewayEvent {
  return parseGatewayEvent(value) !== null
}

// ---------------------------------------------------------------------------------------------------------
// Images (design 6.2, 6.4): a strict data-URL decoder and the magic-byte allowlist
// ---------------------------------------------------------------------------------------------------------

export interface DecodedImage {
  mime: ImageMime
  bytes: Uint8Array
}

/** Validates an image_generated data URL; null rejects it. */
export type ImageDecoder = (dataUrl: string) => DecodedImage | null

/** 16 MiB of base64 (design 6.4). */
export const MAX_GENERATED_IMAGE_B64 = 16 * 1024 * 1024

const DATA_URL_PREFIX = /^data:(image\/(?:png|jpeg|gif|webp));base64,/
const BASE64_BODY = /^[A-Za-z0-9+/]*={0,2}$/

function startsWith(bytes: Uint8Array, prefix: readonly number[], offset = 0): boolean {
  return prefix.every((b, i) => bytes[offset + i] === b)
}

/** PNG, JPEG, GIF87a/GIF89a and WebP by magic bytes (BMP, SVG and everything else: null). */
export function sniffAllowedImage(bytes: Uint8Array): ImageMime | null {
  if (startsWith(bytes, [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])) return "image/png"
  if (startsWith(bytes, [0xff, 0xd8, 0xff])) return "image/jpeg"
  if (startsWith(bytes, [0x47, 0x49, 0x46, 0x38]) && (bytes[4] === 0x37 || bytes[4] === 0x39) && bytes[5] === 0x61) {
    return "image/gif"
  }
  if (startsWith(bytes, [0x52, 0x49, 0x46, 0x46]) && startsWith(bytes, [0x57, 0x45, 0x42, 0x50], 8)) {
    return "image/webp"
  }
  return null
}

/**
 * Only `data:image/(png|jpeg|gif|webp);base64,` URLs up to 16 MiB of base64 decode, and the bytes must sniff as
 * the declared type. The default for GatewayWireContext.decodeImage.
 */
export function decodeGeneratedImage(dataUrl: string): DecodedImage | null {
  const match = DATA_URL_PREFIX.exec(dataUrl)
  const declared = match?.[1]
  if (!match || declared === undefined) return null
  const body = dataUrl.slice(match[0].length)
  if (body.length === 0 || body.length > MAX_GENERATED_IMAGE_B64 || body.length % 4 !== 0) return null
  if (!BASE64_BODY.test(body)) return null
  const bytes = new Uint8Array(Buffer.from(body, "base64"))
  const sniffed = sniffAllowedImage(bytes)
  return sniffed !== null && sniffed === declared ? { mime: sniffed, bytes } : null
}

// ---------------------------------------------------------------------------------------------------------
// gate_resolved normalization (3.4)
// ---------------------------------------------------------------------------------------------------------

/** ic/src/bridge/router.rs:1236, :1346, :2428, :2945-2950, :2980, :3033; ic/src/bridge/router/auth.rs:115. */
export const GATE_RESOLUTIONS: Readonly<Record<string, ApprovalResolution>> = Object.freeze({
  approved: "approve",
  approved_always: "always",
  denied: "deny",
  credential_provided: "credential",
  external_callback: "external",
  cancelled: "cancelled",
  thread_terminated: "terminated",
  expired: "expired",
})

export function normalizeResolution(wire: string): ApprovalResolution {
  return Object.hasOwn(GATE_RESOLUTIONS, wire) ? (GATE_RESOLUTIONS[wire] ?? "unknown") : "unknown"
}

// ---------------------------------------------------------------------------------------------------------
// The mapper
// ---------------------------------------------------------------------------------------------------------

export interface GatewayWireContext {
  /**
   * True on gateways that serve GET /api/chat/capabilities (and on REPLv3): replies without a client id are
   * out-of-band (`background`) and settle hints are not produced. False on legacy gateways.
   */
  correlated: boolean
  /**
   * Maps the wire client_turn_id to a TurnId this client knows, else null. GatewayTransport resolves only ids it
   * sent itself; the default accepts any id that satisfies ^[A-Za-z0-9_-]{1,64}$.
   */
  resolveTurnId?: (wire: string | undefined) => TurnId | null
  /** image_generated validation; default decodeGeneratedImage. */
  decodeImage?: ImageDecoder
}

/** Tool result previews are cut to 200 characters (3.4). */
export const TOOL_PREVIEW_CHARS = 200

const AGENT_ERROR_PREFIX = "Error: "
const SETTLE_STATUS: Readonly<Record<string, "done" | "rejected" | "awaiting_approval">> = Object.freeze({
  Done: "done",
  Rejected: "rejected",
  "Awaiting approval": "awaiting_approval",
})
const TERMINAL_THREAD_STATES: ReadonlySet<string> = new Set(["Completed", "Done", "Failed"])

/** Cuts to `max` code points without splitting a surrogate pair. */
export function truncateText(text: string, max: number): string {
  if (text.length <= max) return text
  const points = Array.from(text.slice(0, max * 2))
  return points.length <= max ? points.join("") : `${points.slice(0, max - 1).join("")}…`
}

function firstLine(text: string): string {
  const newline = text.indexOf("\n")
  return (newline === -1 ? text : text.slice(0, newline)).trim()
}

interface MapScope {
  readonly turnId: TurnId | null
  readonly threadId: ThreadId | undefined
  readonly ctx: GatewayWireContext
}

type Mapper<T extends GatewayEventType> = (event: GatewayEventOf<T>, scope: MapScope) => TransportEvent[]

const NONE: TransportEvent[] = []

function activity(scope: MapScope, kind: Parameters<typeof activityEvent>[1], text: string, ok?: boolean) {
  return [activityEvent(scope, kind, text, ok)]
}

function activityEvent(
  scope: MapScope,
  kind: "thinking" | "tool_started" | "tool_completed" | "tool_result" | "status" | "reasoning" | "skill" | "recovery",
  text: string,
  ok?: boolean,
): TransportEvent {
  const act = ok === undefined ? { kind, text } : { kind, text, ok }
  return withThread({ kind: "turn.activity", turnId: scope.turnId, activity: act }, scope)
}

function withThread<E extends TransportEvent>(event: E, scope: MapScope): E {
  return scope.threadId === undefined ? event : { ...event, threadId: scope.threadId }
}

function approvalFrom(
  event: { request_id: string; tool_name: string; description: string; parameters: string; thread_id?: string },
  allowAlways: boolean,
): WireApproval {
  return {
    id: event.request_id,
    threadId: event.thread_id ?? null,
    toolName: event.tool_name,
    description: event.description,
    parameters: event.parameters,
    allowAlways,
    truncated: false,
    source: "live",
  }
}

function mapResponse(event: GatewayEventOf<"response">, scope: MapScope): TransportEvent[] {
  const text = event.content
  if (scope.turnId === null && scope.ctx.correlated) return [withThread({ kind: "background", text }, scope)]
  // V1 reports turn errors as "Error: <message>" replies (agent_loop.rs:2192). A full steer queue is reported the
  // same way but is the steer's acknowledgement, so it stays a final for control-ack matching (3.5 rule 5a).
  if (text.startsWith(AGENT_ERROR_PREFIX) && controlAckKind(text) === null) {
    const message = text.slice(AGENT_ERROR_PREFIX.length)
    return [withThread({ kind: "turn.error", turnId: scope.turnId, code: "agent_error", message, retryable: false }, scope)]
  }
  return [withThread({ kind: "turn.final", turnId: scope.turnId, text }, scope)]
}

function mapToolCompleted(event: GatewayEventOf<"tool_completed">, scope: MapScope): TransportEvent[] {
  // V2 puts the duration in `parameters` ("8123ms"); V1 puts redacted failure parameters there, never shown here.
  const duration = event.parameters && /^\d+ms$/.test(event.parameters) ? ` (${event.parameters})` : ""
  const error = !event.success && event.error ? `: ${firstLine(event.error)}` : ""
  return activity(scope, "tool_completed", truncateText(`${event.name}${duration}${error}`, TOOL_PREVIEW_CHARS), event.success)
}

function mapStatus(event: GatewayEventOf<"status">, scope: MapScope): TransportEvent[] {
  // Unmapped common events serialize as an empty status (types.rs); they carry nothing.
  if (event.message.trim() === "") return NONE
  const events = activity(scope, "status", event.message)
  const signal = SETTLE_STATUS[event.message.trim()]
  if (!scope.ctx.correlated && signal !== undefined) {
    events.push(withThread({ kind: "turn.settle_hint", signal }, scope))
  }
  return events
}

function mapGateRequired(event: GatewayEventOf<"gate_required">, scope: MapScope): TransportEvent[] {
  const kind = event.resume_kind
  if ("Approval" in kind) {
    return [withThread({ kind: "turn.approval", turnId: scope.turnId, approval: approvalFrom(event, kind.Approval.allow_always) }, scope)]
  }
  if ("Authentication" in kind) {
    const auth = kind.Authentication
    const required: TransportEvent = {
      kind: "turn.auth_required",
      turnId: scope.turnId,
      extensionName: auth.credential_name,
      requestId: event.request_id,
      ...(auth.instructions ? { instructions: auth.instructions } : {}),
      ...(auth.auth_url ? { authUrl: auth.auth_url } : {}),
    }
    return [withThread(required, scope)]
  }
  return [{ kind: "notice", level: "info", source: "auth", text: `waiting for external confirmation (${event.gate_name})` }]
}

function mapAuthRequired(event: GatewayEventOf<"auth_required">, scope: MapScope): TransportEvent[] {
  const required: TransportEvent = {
    kind: "turn.auth_required",
    turnId: scope.turnId,
    extensionName: event.extension_name,
    ...(event.instructions ? { instructions: event.instructions } : {}),
    ...(event.auth_url ? { authUrl: event.auth_url } : {}),
    ...(event.setup_url ? { setupUrl: event.setup_url } : {}),
  }
  return [withThread(required, scope)]
}

function mapImage(event: GatewayEventOf<"image_generated">, scope: MapScope): TransportEvent[] {
  const decoded = (scope.ctx.decodeImage ?? decodeGeneratedImage)(event.data_url)
  if (!decoded) {
    return [{ kind: "notice", level: "warn", source: "image", text: "a generated image was rejected: not a PNG, JPEG, GIF or WebP data URL" }]
  }
  const image = event.path ? { mime: decoded.mime, bytes: decoded.bytes, caption: event.path } : decoded
  return [withThread({ kind: "turn.image", turnId: scope.turnId, image }, scope)]
}

function mapReasoning(event: GatewayEventOf<"reasoning_update">, scope: MapScope): TransportEvent[] {
  const first = event.decisions[0]
  const text = event.narrative.trim() !== "" ? event.narrative : first ? `${first.tool_name}: ${first.rationale}` : ""
  return text === "" ? NONE : activity(scope, "reasoning", text)
}

function mapRecovery(event: GatewayEventOf<"turn_recovery">, scope: MapScope): TransportEvent[] {
  const message = isStr(event.message) ? event.message : ""
  if (event.recovery_state === "failed") {
    const text = message || (isStr(event.failure_kind) ? event.failure_kind : "model recovery failed")
    // Legacy (uncorrelated): the engine follows a failed recovery with the turn's "Error: ..." response
    // (orchestrator model_failure_result, router.rs thread_outcome_response), and that is the terminal event. A
    // terminal error here as well would fail the next queued turn with this turn's message (rule 9 takes the
    // oldest inflight turn), so the failure is progress until the response arrives.
    if (!scope.ctx.correlated && scope.turnId === null) return activity(scope, "recovery", `failed: ${text}`, false)
    return [withThread({ kind: "turn.error", turnId: scope.turnId, code: "recovery_failed", message: text, retryable: false }, scope)]
  }
  return activity(scope, "recovery", message ? `${event.recovery_state}: ${message}` : event.recovery_state)
}

function jobNotice(text: string): TransportEvent[] {
  return [{ kind: "notice", level: "info", source: "job", text }]
}

const MAPPERS: { readonly [K in GatewayEventType]: Mapper<K> } = {
  turn_started: (_e, s) => (s.turnId === null ? NONE : [withThread({ kind: "turn.started", turnId: s.turnId }, s)]),
  turn_finished: (e, s) => (s.turnId === null ? NONE : [{ kind: "turn.finished", turnId: s.turnId, outcome: toFinishOutcome(e.outcome) }]),
  response: mapResponse,
  stream_chunk: (e, s) => [withThread({ kind: "turn.delta", turnId: s.turnId, text: e.content }, s)],
  thinking: (e, s) => activity(s, "thinking", e.message),
  tool_started: (e, s) => activity(s, "tool_started", e.name),
  tool_completed: mapToolCompleted,
  tool_result: (e, s) => activity(s, "tool_result", truncateText(`${e.name}: ${e.preview}`, TOOL_PREVIEW_CHARS)),
  status: mapStatus,
  approval_needed: (e, s) => [withThread({ kind: "turn.approval", turnId: s.turnId, approval: approvalFrom(e, e.allow_always) }, s)],
  gate_required: mapGateRequired,
  gate_resolved: (e) => [{ kind: "turn.approval_resolved", approvalId: e.request_id, resolution: normalizeResolution(e.resolution) }],
  auth_required: mapAuthRequired,
  auth_completed: (e) => [
    {
      kind: "notice",
      level: e.success ? "info" : "warn",
      source: "auth",
      text: isStr(e.message) && e.message !== "" ? e.message : `${e.extension_name}: authentication ${e.success ? "completed" : "failed"}`,
    },
  ],
  image_generated: mapImage,
  suggestions: (e, s) => [withThread({ kind: "turn.suggestions", turnId: s.turnId, suggestions: e.suggestions }, s)],
  turn_cost: (e, s) => [
    withThread({ kind: "turn.cost", turnId: s.turnId, inputTokens: e.input_tokens, outputTokens: e.output_tokens, costUsd: e.cost_usd }, s),
  ],
  reasoning_update: mapReasoning,
  skill_activated: (e, s) => (e.skill_names.length === 0 ? NONE : activity(s, "skill", `skills: ${e.skill_names.join(", ")}`)),
  turn_recovery: mapRecovery,
  job_started: (e) => jobNotice(`job started: ${e.title}${isStr(e.browse_url) && e.browse_url ? ` (${e.browse_url})` : ""}`),
  job_status: (e) => jobNotice(`job ${e.job_id}: ${e.message}`),
  job_result: (e) => jobNotice(`job ${e.job_id} ${e.status}`),
  stream_gap: () => [{ kind: "resync.required", reason: "gap" }],
  thread_state_changed: (e, s) =>
    !s.ctx.correlated && TERMINAL_THREAD_STATES.has(e.to_state)
      ? [withThread({ kind: "turn.settle_hint", signal: "thread_terminal" }, s)]
      : NONE,
  error: (e, s) => [withThread({ kind: "turn.error", turnId: s.turnId, code: "server", message: e.message, retryable: false }, s)],
  heartbeat: () => NONE,
}

function defaultResolveTurnId(wire: string | undefined): TurnId | null {
  return wire === undefined ? null : asTurnId(wire)
}

/** One gateway event → zero or more TransportEvents (3.4 table). `turnId = client_turn_id ?? null`. */
export function mapGatewayEvent(event: GatewayEvent, ctx: GatewayWireContext): TransportEvent[] {
  const wireId = isStr(event.client_turn_id) ? event.client_turn_id : undefined
  const turnId = (ctx.resolveTurnId ?? defaultResolveTurnId)(wireId)
  const threadId = "thread_id" in event && isStr(event.thread_id) && event.thread_id !== "" ? event.thread_id : undefined
  const mapper = MAPPERS[event.type] as Mapper<GatewayEventType>
  return mapper(event, { turnId, threadId, ctx })
}

/** parseGatewayEvent + mapGatewayEvent; unknown or malformed events yield []. */
export function decodeGatewayEvent(value: unknown, ctx: GatewayWireContext): TransportEvent[] {
  const event = parseGatewayEvent(value)
  return event ? mapGatewayEvent(event, ctx) : NONE
}

// ---------------------------------------------------------------------------------------------------------
// Response DTO validation
// ---------------------------------------------------------------------------------------------------------

const isNullableStr = (v: unknown): boolean => v === null || v === undefined || isStr(v)

function isWireLimits(value: unknown): value is WireLimits {
  if (!isObj(value)) return false
  const required = [
    "max_content_bytes",
    "max_attachments",
    "max_attachment_bytes",
    "max_total_attachment_bytes",
    "max_transcribe_b64_bytes",
    "messages_per_minute",
    "transcribes_per_minute",
  ]
  const optional = ["max_request_bytes", "max_frame_bytes", "max_in_flight", "idle_timeout_secs"]
  return (
    required.every((key) => isNum(value[key])) &&
    optional.every((key) => value[key] === undefined || value[key] === null || isNum(value[key])) &&
    isStrArray(value.attachment_mime_types) &&
    isStrArray(value.transcribe_formats)
  )
}

export function parseChatCapabilities(value: unknown): ChatCapabilities | null {
  if (!isObj(value) || !isNum(value.api_version) || !isStr(value.server_version)) return null
  if ((value.engine !== "v1" && value.engine !== "v2") || !isStrArray(value.features)) return null
  if (!isWireLimits(value.limits) || (value.credential !== "master" && value.credential !== "client")) return null
  const voice = value.voice
  if (!isObj(voice) || !isNullableStr(voice.provider) || !isBool(voice.stt) || !isBool(voice.tts)) return null
  return value as unknown as ChatCapabilities
}

export function parseThreadInfo(value: unknown): ThreadInfo | null {
  return isObj(value) && isStr(value.id) ? (value as unknown as ThreadInfo) : null
}

export function parseThreadList(value: unknown): ThreadListResponse | null {
  if (!isObj(value) || !Array.isArray(value.threads)) return null
  const assistant = value.assistant_thread
  if (assistant !== null && assistant !== undefined && parseThreadInfo(assistant) === null) return null
  return { ...(value as unknown as ThreadListResponse), assistant_thread: assistant ? (assistant as ThreadInfo) : null }
}

function isTurnInfo(value: unknown): value is TurnInfo {
  return (
    isObj(value) &&
    isStr(value.user_input) &&
    isNullableStr(value.response) &&
    isStr(value.state) &&
    isStr(value.started_at) &&
    isNullableStr(value.completed_at)
  )
}

function isPendingApprovalInfo(value: unknown): value is PendingApprovalInfo {
  return isObj(value) && isStr(value.request_id) && isStr(value.tool_name) && isStr(value.description) && isStr(value.parameters)
}

export function parseHistoryResponse(value: unknown): HistoryResponse | null {
  if (!isObj(value) || !isStr(value.thread_id) || !Array.isArray(value.turns) || !value.turns.every(isTurnInfo)) return null
  const pending = value.pending_approval
  if (pending !== undefined && pending !== null && !isPendingApprovalInfo(pending)) return null
  return {
    thread_id: value.thread_id,
    turns: value.turns,
    has_more: value.has_more === true,
    ...(isStr(value.oldest_timestamp) ? { oldest_timestamp: value.oldest_timestamp } : {}),
    ...(isPendingApprovalInfo(pending) ? { pending_approval: pending } : {}),
  }
}

function isPendingGate(value: unknown): value is PendingGateView {
  return (
    isObj(value) &&
    isStr(value.request_id) &&
    isStr(value.thread_id) &&
    isStr(value.tool_name) &&
    isStr(value.description) &&
    isStr(value.parameters) &&
    isResumeKind(value.resume_kind)
  )
}

/** Malformed gates are skipped. */
export function parsePendingGates(value: unknown): PendingGateView[] {
  return isObj(value) && Array.isArray(value.gates) ? value.gates.filter(isPendingGate) : []
}

export function parseVoiceStatus(value: unknown): VoiceStatusResponse | null {
  if (!isObj(value) || !isBool(value.configured)) return null
  return {
    configured: value.configured,
    max_audio_b64_len: isNum(value.max_audio_b64_len) ? value.max_audio_b64_len : 0,
    ...(isStr(value.model) ? { model: value.model } : {}),
    ...(isBool(value.stt) ? { stt: value.stt } : {}),
    ...(isBool(value.tts) ? { tts: value.tts } : {}),
    ...(isStr(value.provider) || value.provider === null ? { provider: value.provider } : {}),
  }
}

export function parseTranscribeResponse(value: unknown): VoiceTranscribeResponse | null {
  if (!isObj(value) || !isStr(value.text)) return null
  return {
    text: value.text,
    ...(isStr(value.language) ? { language: value.language } : {}),
    ...(isStr(value.model) ? { model: value.model } : {}),
    ...(isNum(value.confidence) ? { confidence: value.confidence } : {}),
  }
}

export function parseVoiceError(value: unknown): VoiceErrorBody | null {
  return isObj(value) && isStr(value.error) ? { error: value.error, message: isStr(value.message) ? value.message : "" } : null
}

export function parseActionResponse(value: unknown): ActionResponse | null {
  return isObj(value) && isBool(value.success) ? { success: value.success, message: isStr(value.message) ? value.message : "" } : null
}

/** `uptime_secs` must be a non-negative integer (a Rust u64); anything else gives no daemon start. */
export function parseGatewayStatus(value: unknown): GatewayStatusResponse | null {
  if (!isObj(value)) return null
  const uptime = value.uptime_secs
  return typeof uptime === "number" && Number.isSafeInteger(uptime) && uptime >= 0 ? { uptime_secs: uptime } : null
}

export function parseSendResponse(value: unknown): SendMessageResponse | null {
  return isObj(value) && isStr(value.message_id) ? { message_id: value.message_id, status: "accepted" } : null
}

// ---------------------------------------------------------------------------------------------------------
// History and gates → snapshot pieces
// ---------------------------------------------------------------------------------------------------------

export function historyTurnFromWire(turn: TurnInfo): HistoryTurn {
  return {
    userInput: turn.user_input,
    response: turn.response ?? null,
    state: turn.state,
    startedAt: turn.started_at,
    completedAt: turn.completed_at ?? null,
  }
}

/** history.pending_approval carries no allow_always: "always" is not offered for it. */
export function approvalFromHistory(pending: PendingApprovalInfo, threadId: ThreadId): WireApproval {
  return {
    id: pending.request_id,
    threadId,
    toolName: pending.tool_name,
    description: pending.description,
    parameters: pending.parameters,
    allowAlways: false,
    truncated: false,
    source: "history",
  }
}

/** An authentication gate: the composer's credential mode must be restored from it (3.6.4). */
export function authGateFromPendingGate(gate: PendingGateView): WireAuthGate | null {
  if (!("Authentication" in gate.resume_kind)) return null
  const auth = gate.resume_kind.Authentication
  const extensionName = auth.credential_name.trim() !== "" ? auth.credential_name : gate.tool_name
  if (extensionName.trim() === "") return null
  return {
    requestId: gate.request_id,
    threadId: gate.thread_id,
    extensionName,
    ...(auth.instructions ? { instructions: auth.instructions } : {}),
    ...(auth.auth_url ? { authUrl: auth.auth_url } : {}),
  }
}

/** Approval gates only; authentication and external gates are not answerable as approvals. */
export function approvalFromPendingGate(gate: PendingGateView): WireApproval | null {
  if (!("Approval" in gate.resume_kind)) return null
  return {
    id: gate.request_id,
    threadId: gate.thread_id,
    toolName: gate.tool_name,
    description: gate.description,
    parameters: gate.parameters,
    allowAlways: gate.resume_kind.Approval.allow_always,
    truncated: false,
    source: "pending_gates",
  }
}

// ---------------------------------------------------------------------------------------------------------
// Capabilities and limits → ServerInfo pieces (3.7 folding table)
// ---------------------------------------------------------------------------------------------------------

const MIB = 1024 * 1024

/** The gateway's global body limit (ic/src/channels/web/server.rs DefaultBodyLimit 10 MiB). */
export const GATEWAY_MAX_REQUEST_BYTES = 10 * MIB

function wavBytesForBase64(b64Bytes: number): number {
  return Math.floor((Math.max(0, b64Bytes) * 3) / 4)
}

function allowedMimeTypes(wire: readonly string[]): ImageMime[] {
  return IMAGE_MIME_TYPES.filter((mime) => wire.includes(mime))
}

export function limitsFromWire(wire: WireLimits): Limits {
  return {
    maxContentBytes: wire.max_content_bytes,
    maxImages: wire.max_attachments,
    maxImageBytes: wire.max_attachment_bytes,
    maxTotalImageBytes: wire.max_total_attachment_bytes,
    maxRequestBytes: wire.max_request_bytes ?? GATEWAY_MAX_REQUEST_BYTES,
    imageMimeTypes: allowedMimeTypes(wire.attachment_mime_types),
    maxTranscribeWavBytes: wavBytesForBase64(wire.max_transcribe_b64_bytes),
    transcribePerMinute: wire.transcribes_per_minute,
    chatPerMinute: wire.messages_per_minute,
    maxInFlight: wire.max_in_flight ?? 8,
  }
}

/** Legacy gateways (capabilities 404): today's server values (3.7). */
export function legacyGatewayLimits(voice: VoiceStatusResponse | null): Limits {
  return {
    maxContentBytes: 100_000,
    maxImages: 5,
    maxImageBytes: 5 * MIB,
    maxTotalImageBytes: 7 * MIB,
    maxRequestBytes: GATEWAY_MAX_REQUEST_BYTES,
    imageMimeTypes: [...IMAGE_MIME_TYPES],
    maxTranscribeWavBytes: voice?.configured ? wavBytesForBase64(voice.max_audio_b64_len) : 0,
    transcribePerMinute: 10,
    chatPerMinute: 30,
    maxInFlight: 8,
  }
}

export function capabilitiesFromChat(caps: ChatCapabilities, voice: VoiceStatusResponse | null): Capabilities {
  const features = new Set(caps.features)
  return {
    streaming: features.has("stream"),
    activity: features.has("status"),
    images: features.has("attachments"),
    generatedImages: true,
    approvals: true,
    transcribe: features.has("transcribe") && voice?.configured !== false,
    correlation: true,
    lifecycle: true,
    interrupt: "text",
    steer: "text",
    threads: true,
    history: true,
    backgroundMessages: true,
    resume: true,
  }
}

export function legacyGatewayCapabilities(voice: VoiceStatusResponse | null): Capabilities {
  return {
    streaming: "unknown",
    activity: true,
    images: true,
    generatedImages: "unknown",
    approvals: true,
    transcribe: voice?.configured === true,
    correlation: false,
    lifecycle: false,
    interrupt: "text",
    steer: "text",
    threads: true,
    history: true,
    backgroundMessages: true,
    resume: true,
  }
}
