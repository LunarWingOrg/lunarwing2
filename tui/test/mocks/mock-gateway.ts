/**
 * Mock LunarWing web gateway (design 11.3.1) on Bun.serve, 127.0.0.1 and an ephemeral port.
 *
 * Routes: /api/health, /api/chat/{events,send,approval,history,threads,thread/new,pending-gates,capabilities,
 * auth-token,auth-cancel}, /api/gateway/status, /api/voice/{status,transcribe}. Request and response shapes,
 * status codes and the plain-text or JSON error bodies follow ic/src/channels/web/{server,sse,types,voice,auth}.rs.
 *
 * Modes:
 * - "legacy": today's daemons. No GET /api/chat/capabilities (404), no correlation fields, no turn_started /
 *   turn_finished, client_turn_id ignored, images accepted without validation (today's server.rs
 *   images_to_attachments drops invalid items silently instead of refusing the request).
 * - "current": the M2a contract (4.2). Capabilities document, `message_id` and `client_turn_id` merged into every
 *   event a message causes, turn_started before progress and turn_finished after the response (a V2 approval or
 *   credential continuation starts with turn_started, before its gate_resolved), client_turn_id validated (a
 *   non-string is axum's 422, an invalid string 400 "invalid client_turn_id"), G5 image validation in the order
 *   and with the bodies of ic/src/channels/image_mime.rs decode_images and server.rs image_rejection_response
 *   (count, then the base64 length bounds before anything is decoded; 413 for the size limits, 422 otherwise).
 *
 * Both modes reject request bodies whose shape does not fit the server's request type with axum's 422 before any
 * handler check (ic/src/channels/web/types.rs SendMessageRequest, ApprovalRequest, AuthTokenRequest, ...).
 *
 * GET /api/gateway/status reports `uptime_secs` (server.rs gateway_status_handler) from `uptimeSecs()`; restart()
 * plays a daemon restart: the streams drop, the uptime starts again from 0, in-flight history turns become Failed
 * without a reply (the V2 restart sweep, ic/src/bridge/router.rs) and, unless the gates were persisted (V2),
 * pending approvals and authentication gates are gone.
 *
 * Replies are scripted per send (`replies`, default `echoReply`): events are emitted to every open event stream
 * on a zero-delay timer after the 202 (or before it with `emitBeforeAccept`), correlated automatically in current
 * mode. Tests drive everything else with emit(), emitRaw(), emitComment(), dropStreams(), override() and onRequest();
 * every request is recorded with method, path, query, headers and body, and every emit() payload in `emitted`.
 *
 * Deviation from the real server: Bun.serve sends the response head only with the first body chunk, so each event
 * stream starts with one empty comment (":\n\n", the bytes of axum's 15 s keepalive).
 *
 * Standalone smoke server (never used by `bun test`):
 *   bun test/mocks/mock-gateway.ts --mode legacy|current [--port N] [--token-file PATH] [--engine v1|v2]
 * prints exactly one line "LISTENING <port>" and answers each send with a streamed "echo: <content>" until killed.
 * With --token-file the bearer token must equal the file's trimmed content; without it any bearer token is
 * accepted. Content "!approval" asks for an approval and "!image" returns a generated image.
 */
import { readFile } from "node:fs/promises"
import type { ProcReader } from "../../src/transport/listener-owner.ts"
import { Secret } from "../../src/config/secret.ts"
import type { GlobalState, ProfileState, StateFile, TokenKind, TokenSource } from "../../src/config/types.ts"
import type { WireLimits } from "../../src/transport/types.ts"

export type MockGatewayMode = "legacy" | "current"
export type MockEvent = { type: string } & Record<string, unknown>
export type MockCorr = { message_id?: string; client_turn_id?: string }

export interface MockTurn {
  user_input: string
  response: string | null
  state: string
  started_at: string
  completed_at: string | null
}

export interface MockThread {
  id: string
  turns: MockTurn[]
  created_at: string
  title?: string
}

export interface MockApproval {
  request_id: string
  thread_id: string
  tool_name: string
  description: string
  parameters: string
  allow_always: boolean
  /** Correlation of the turn that paused (current mode). */
  corr: MockCorr
}

export interface MockAuthGate {
  request_id: string
  thread_id: string
  extension_name: string
  instructions: string
  corr: MockCorr
}

export interface RecordedRequest {
  readonly method: string
  readonly path: string
  readonly query: Readonly<Record<string, string>>
  /** Path plus query string, as sent. */
  readonly url: string
  /** Lowercased header names. */
  readonly headers: Readonly<Record<string, string>>
  /** Parsed JSON body, or null. */
  readonly body: unknown
  readonly rawBody: string
  /** Event streams open when the request arrived. */
  readonly openStreams: number
}

export interface MockSendContext {
  readonly gateway: MockGateway
  readonly mode: MockGatewayMode
  readonly content: string
  readonly threadId: string
  readonly messageId: string
  readonly clientTurnId: string | undefined
  /** Merged into every scripted event in current mode; {} in legacy mode. */
  readonly corr: MockCorr
  readonly body: Readonly<Record<string, unknown>>
}

export type MockReplyScript = (ctx: MockSendContext) => readonly MockEvent[]

export type MockRouteHandler = (req: Request, rec: RecordedRequest) => Response | Promise<Response>

export interface MockGatewayOptions {
  mode: MockGatewayMode
  /** The accepted bearer token; null or undefined accepts any non-empty bearer token. */
  token?: string | null
  port?: number
  /** Engine (default "v2": stream_chunk deltas, thread states and gate events; "v1": status "Done", no streaming). */
  engine?: "v1" | "v2"
  /** LunarVoice configured (default true). */
  voiceConfigured?: boolean
  /** Overrides of the current-mode WireLimits. */
  limits?: Partial<WireLimits>
  /** Event-stream cap (100 in sse.rs); beyond it 503 "Too many connections". */
  maxStreams?: number
  /** Default echoReply. */
  replies?: MockReplyScript
  /** Emit scripted events before answering 202 (events overtaking the POST response). */
  emitBeforeAccept?: boolean
  /** Delay between scripted events (real timers; smoke runs only). Default 0: one batch. */
  stepDelayMs?: number
  /** Keepalive comment interval (real timers; smoke runs only). Default off. */
  keepaliveMs?: number | null
  /** GET /api/chat/threads has an assistant thread (a DB-backed daemon). Default true. */
  assistantThread?: boolean
  /** The clock uptime_secs is measured on (the test's ManualClock); default performance.now. */
  now?: () => number
}

const MIB = 1024 * 1024
const GLOBAL_BODY_LIMIT = 10 * MIB
const TRANSCRIBE_BODY_LIMIT = 32 * MIB
const MAX_AUDIO_B64_LEN = 24 * MIB
const AUDIO_FORMATS = ["wav", "ogg", "opus", "mp3", "mp4", "webm", "flac", "m4a", "aac"]
const CLIENT_ID = /^[A-Za-z0-9_-]{1,64}$/
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i
const encoder = new TextEncoder()

/**
 * A bearer token for tests, "tok-<label>-0123456789abcdef", built at run time: the repo's gitleaks scan
 * (.gitlab/scripts/scan-secrets.sh, rule generic-api-key) flags token-shaped string literals.
 */
export function fixtureToken(label: string): string {
  return ["tok", label, "0123456789abcdef"].join("-")
}

/** A 1x1 PNG for "!image" and tests. */
export const TINY_PNG = Uint8Array.from(
  Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==", "base64"),
)

export function defaultWireLimits(voiceConfigured: boolean): WireLimits {
  return {
    max_content_bytes: 100_000,
    max_attachments: 5,
    max_attachment_bytes: 5 * MIB,
    max_total_attachment_bytes: 7 * MIB,
    attachment_mime_types: ["image/png", "image/jpeg", "image/gif", "image/webp"],
    max_transcribe_b64_bytes: voiceConfigured ? MAX_AUDIO_B64_LEN : 0,
    transcribe_formats: voiceConfigured ? [...AUDIO_FORMATS] : [],
    messages_per_minute: 30,
    transcribes_per_minute: 10,
    max_request_bytes: GLOBAL_BODY_LIMIT,
  }
}

function text(status: number, body: string): Response {
  return new Response(body, { status, headers: { "content-type": "text/plain; charset=utf-8" } })
}

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } })
}

function voiceError(status: number, error: string, message: string): Response {
  return json(status, { error, message })
}

function nowIso(): string {
  return new Date().toISOString()
}

/** ic/src/channels/image_mime.rs sniff_image_mime: the full PNG magic, GIF87a/GIF89a, RIFF....WEBP. */
function sniff(bytes: Uint8Array): string | null {
  const starts = (prefix: readonly number[], offset = 0) => prefix.every((byte, i) => bytes[offset + i] === byte)
  if (starts([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])) return "image/png"
  if (starts([0xff, 0xd8, 0xff])) return "image/jpeg"
  if (starts([0x47, 0x49, 0x46, 0x38]) && (bytes[4] === 0x37 || bytes[4] === 0x39) && bytes[5] === 0x61) return "image/gif"
  if (starts([0x52, 0x49, 0x46, 0x46]) && starts([0x57, 0x45, 0x42, 0x50], 8)) return "image/webp"
  return null
}

/** image_mime.rs base64_len_bound: the padded base64 length of `decoded` bytes. */
function base64Bound(decoded: number): number {
  return 4 * Math.ceil(decoded / 3)
}

/**
 * The Rust base64 STANDARD engine: the standard alphabet, canonical padding, no trailing bits, no whitespace; null
 * where it fails. Buffer.from alone is lenient, so the result must encode back to the same text.
 */
function strictBase64(data: string): Uint8Array | null {
  if (data.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(data)) return null
  const bytes = Buffer.from(data, "base64")
  return bytes.toString("base64") === data ? new Uint8Array(bytes) : null
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

/** axum's Json extractor: a body that parses but does not fit the request type is a plain-text 422 (serde's reason). */
function shapeError(reason: string): Response {
  return text(422, `Failed to deserialize the JSON body into the target type: ${reason}`)
}

/** serde's name for the JSON value in an "invalid type" error. */
function serdeType(value: unknown): string {
  if (value === null) return "null"
  if (typeof value === "boolean") return `boolean \`${value}\``
  if (typeof value === "number") return Number.isInteger(value) ? `integer \`${value}\`` : `floating point \`${value}\``
  if (typeof value === "string") return `string ${JSON.stringify(value)}`
  return Array.isArray(value) ? "sequence" : "map"
}

/** The fields of a request type as serde sees them: `String` (required) and `Option<String>` (optional). */
interface BodyShape {
  required?: readonly string[]
  optional?: readonly string[]
}

/** serde's reason for the first field that does not deserialize; null when the object fits. */
function shapeReason(body: Record<string, unknown>, shape: BodyShape): string | null {
  for (const field of shape.required ?? []) {
    const value = body[field]
    if (value === undefined) return `missing field \`${field}\``
    if (typeof value !== "string") return `${field}: invalid type: ${serdeType(value)}, expected a string`
  }
  for (const field of shape.optional ?? []) {
    const value = body[field]
    if (value !== undefined && value !== null && typeof value !== "string") {
      return `${field}: invalid type: ${serdeType(value)}, expected a string`
    }
  }
  return null
}

/** The 422 for a body that does not fit `shape`; null when it fits. */
function checkShape(body: Record<string, unknown>, shape: BodyShape): Response | null {
  const reason = shapeReason(body, shape)
  return reason === null ? null : shapeError(reason)
}

/** One element of SendMessageRequest.images (types.rs ImageData: both fields required strings). */
interface WireImage {
  media_type: string
  data: string
}

/** `#[serde(default)] images: Vec<ImageData>`: absent is empty; null or anything but an array of ImageData is a 422. */
function checkImagesShape(images: unknown): WireImage[] | Response {
  if (images === undefined) return []
  if (!Array.isArray(images)) return shapeError(`images: invalid type: ${serdeType(images)}, expected a sequence`)
  for (const [i, image] of images.entries()) {
    if (!isRecord(image)) return shapeError(`images[${i}]: invalid type: ${serdeType(image)}, expected struct ImageData`)
    // serde_path_to_error paths: "images[0]: missing field `data`", "images[0].data: invalid type: ...".
    const reason = shapeReason(image, { required: ["media_type", "data"] })
    if (reason !== null) return shapeError(`images[${i}]${reason.startsWith("missing field") ? ": " : "."}${reason}`)
  }
  return images as WireImage[]
}

/** Default reply script: an echo, plus the daemon's control acks and two smoke-run magic words. */
export function echoReply(ctx: MockSendContext): MockEvent[] {
  const thread_id = ctx.threadId
  const lower = ctx.content.trim().toLowerCase()
  const current = ctx.mode === "current"
  const lifecycle = (outcome: string) => (current ? [{ type: "turn_finished", thread_id, outcome }] : [])
  const started = current ? [{ type: "turn_started", thread_id }] : []
  if (lower === "/interrupt" || lower === "/stop") {
    return [...started, { type: "response", content: "Nothing to interrupt.", thread_id }, ...lifecycle("replied")]
  }
  if (lower.startsWith("/steer ")) {
    const ack = "Steer note queued — the agent will see it when the next turn starts."
    return [...started, { type: "response", content: ack, thread_id }, ...lifecycle("replied")]
  }
  if (lower === "!approval") return ctx.gateway.scriptApproval(ctx)
  if (lower === "!image") {
    const data_url = `data:image/png;base64,${Buffer.from(TINY_PNG).toString("base64")}`
    return [
      ...started,
      { type: "tool_started", name: "image_generate", thread_id },
      { type: "image_generated", data_url, thread_id },
      { type: "tool_completed", name: "image_generate", success: true, thread_id },
      { type: "response", content: "Here is the image.", thread_id },
      ...lifecycle("replied"),
    ]
  }
  const reply = `echo: ${ctx.content}`
  // Streaming and thread states depend on the engine, not on M2a: today's V2 daemons stream uncorrelated chunks.
  if (ctx.gateway.engine === "v2") {
    const chunks = reply.split(/(?<= )/).map((content) => ({ type: "stream_chunk", content, thread_id }))
    return [
      ...started,
      { type: "thinking", message: "Processing...", thread_id },
      { type: "thread_state_changed", thread_id, from_state: "Created", to_state: "Running" },
      ...chunks,
      { type: "thread_state_changed", thread_id, from_state: "Running", to_state: "Completed" },
      { type: "response", content: reply, thread_id },
      ...lifecycle("replied"),
    ]
  }
  // V1 (thread_ops.rs) sends status "Done" before the reply is delivered.
  return [
    ...started,
    { type: "thinking", message: "Processing...", thread_id },
    { type: "status", message: "Done", thread_id },
    { type: "response", content: reply, thread_id },
    ...lifecycle("replied"),
  ]
}

export class MockGateway {
  readonly requests: RecordedRequest[] = []
  /** Every emit() payload (correlation merged), in emit order. */
  readonly emitted: MockEvent[] = []
  readonly threads = new Map<string, MockThread>()
  readonly approvals = new Map<string, MockApproval>()
  readonly authGates = new Map<string, MockAuthGate>()
  mode: MockGatewayMode
  token: string | null
  engine: "v1" | "v2"
  voiceConfigured: boolean
  limits: WireLimits
  replies: MockReplyScript
  emitBeforeAccept: boolean
  assistantThreadId: string | null
  /** Next transcription answer: the default is 200 {text: "hello from the mock", ...}. */
  transcribeReply: (() => Response) | null = null
  /** Next auth-token answer body (default {success: true}). */
  authTokenResult: { success: boolean; message: string } = { success: true, message: "Token saved" }
  /** GET /api/gateway/status uptime_secs; default: whole seconds on `now` since the start or the last restart(). */
  uptimeSecs: () => number

  private readonly now: () => number
  private bootedAt: number
  private readonly server: ReturnType<typeof Bun.serve>
  private readonly streams = new Set<ReadableStreamDefaultController<Uint8Array>>()
  private readonly overrides = new Map<string, { handler: MockRouteHandler; times: number }>()
  private readonly hooks = new Map<string, ((rec: RecordedRequest) => void | Promise<void>)[]>()
  private readonly waiters: { match: (rec: RecordedRequest) => boolean; resolve: (rec: RecordedRequest) => void }[] = []
  private readonly streamWaiters: { count: number; resolve: () => void }[] = []
  private readonly maxStreams: number
  private readonly stepDelayMs: number
  private keepalive: ReturnType<typeof setInterval> | null = null
  /** Settles every in-flight handler at stop(), so a scripted never-answering route cannot stall Bun's server.stop(). */
  private releaseHandlers: (response: Response) => void = () => undefined
  private readonly stopping = new Promise<Response>((resolve) => {
    this.releaseHandlers = resolve
  })

  private constructor(opts: MockGatewayOptions) {
    this.mode = opts.mode
    this.token = opts.token ?? null
    this.engine = opts.engine ?? "v2"
    this.voiceConfigured = opts.voiceConfigured ?? true
    this.limits = { ...defaultWireLimits(this.voiceConfigured), ...opts.limits }
    this.replies = opts.replies ?? echoReply
    this.emitBeforeAccept = opts.emitBeforeAccept ?? false
    this.maxStreams = opts.maxStreams ?? 100
    this.stepDelayMs = opts.stepDelayMs ?? 0
    this.assistantThreadId = opts.assistantThread === false ? null : crypto.randomUUID()
    this.now = opts.now ?? (() => performance.now())
    this.bootedAt = this.now()
    this.uptimeSecs = () => Math.max(0, Math.floor((this.now() - this.bootedAt) / 1000))
    this.server = Bun.serve({
      hostname: "127.0.0.1",
      port: opts.port ?? 0,
      // SSE streams stay idle for up to 15 s between keepalives; Bun's default idle timeout is 10 s.
      idleTimeout: 0,
      maxRequestBodySize: 64 * MIB,
      fetch: (req) => Promise.race([this.handle(req), this.stopping]),
    })
    if (opts.keepaliveMs) this.keepalive = setInterval(() => this.emitComment(), opts.keepaliveMs)
  }

  static async start(opts: MockGatewayOptions): Promise<MockGateway> {
    return new MockGateway(opts)
  }

  get port(): number {
    return this.server.port ?? 0
  }

  get url(): URL {
    return new URL(`http://127.0.0.1:${this.port}`)
  }

  get openStreams(): number {
    return this.streams.size
  }

  async stop(): Promise<void> {
    if (this.keepalive) clearInterval(this.keepalive)
    this.releaseHandlers(text(503, "mock gateway stopped"))
    this.dropStreams()
    // Let Bun finish the responses just released: stop(true) in the same turn can wait forever on one of them.
    await new Promise<void>((resolve) => setImmediate(resolve))
    await this.server.stop(true)
  }

  // ---- scripting ----

  /** Replaces the handler of "METHOD /path" for the next `times` requests (default every request); null removes. */
  override(route: string, handler: MockRouteHandler | null, times = Number.POSITIVE_INFINITY): void {
    if (handler === null) this.overrides.delete(route)
    else this.overrides.set(route, { handler, times })
  }

  /** Runs before the route handler of every matching request (e.g. emit events while the client connects). */
  onRequest(route: string, hook: (rec: RecordedRequest) => void | Promise<void>): void {
    this.hooks.set(route, [...(this.hooks.get(route) ?? []), hook])
  }

  /** Sends one event to every open stream; `corr` is merged at the top level (current-mode correlation). */
  emit(event: MockEvent, corr: MockCorr = {}): void {
    const payload = { ...event, ...corr }
    this.emitted.push(payload)
    this.emitRaw(`event: ${event.type}\ndata: ${JSON.stringify(payload)}\n\n`)
  }

  emitRaw(raw: string | Uint8Array): void {
    const bytes = typeof raw === "string" ? encoder.encode(raw) : raw
    for (const controller of this.streams) {
      try {
        controller.enqueue(bytes)
      } catch {
        this.streams.delete(controller)
      }
    }
  }

  /** The 15 s keepalive (an empty comment). */
  emitComment(): void {
    this.emitRaw(":\n\n")
  }

  /** Closes every event stream from the server side. */
  dropStreams(): void {
    for (const controller of this.streams) {
      try {
        controller.close()
      } catch {
        // already closed
      }
    }
    this.streams.clear()
  }

  /**
   * A daemon restart (upgrade-tenant, crash, OOM): every stream drops, uptime_secs starts again from 0, turns still
   * Processing become Failed without a reply (what durable history shows for a turn the restart killed), and the
   * pending approvals and authentication gates are gone unless `keepGates` (Engine V2 restores persisted gates).
   */
  restart(opts: { keepGates?: boolean } = {}): void {
    this.dropStreams()
    this.bootedAt = this.now()
    for (const thread of this.threads.values()) {
      for (const turn of thread.turns) if (turn.state === "Processing") turn.state = "Failed"
    }
    if (opts.keepGates !== true) {
      this.approvals.clear()
      this.authGates.clear()
    }
  }

  addThread(turns: MockTurn[] = [], id: string = crypto.randomUUID()): MockThread {
    const thread: MockThread = { id, turns, created_at: nowIso() }
    this.threads.set(id, thread)
    return thread
  }

  /** Registers a pending approval (shown by history, pending-gates, and answerable via POST /api/chat/approval). */
  addApproval(approval: Omit<MockApproval, "corr"> & { corr?: MockCorr }): MockApproval {
    const full = { ...approval, corr: approval.corr ?? {} }
    this.approvals.set(full.request_id, full)
    return full
  }

  addAuthGate(gate: Omit<MockAuthGate, "corr"> & { corr?: MockCorr }): MockAuthGate {
    const full = { ...gate, corr: gate.corr ?? {} }
    this.authGates.set(full.request_id, full)
    return full
  }

  /** Requests to "METHOD /path". */
  requestsTo(route: string): RecordedRequest[] {
    return this.requests.filter((rec) => `${rec.method} ${rec.path}` === route)
  }

  /** Resolves with the next (or an already recorded) request to "METHOD /path". */
  waitForRequest(route: string, alreadyRecorded = true): Promise<RecordedRequest> {
    const match = (rec: RecordedRequest) => `${rec.method} ${rec.path}` === route
    const existing = alreadyRecorded ? this.requests.find(match) : undefined
    if (existing) return Promise.resolve(existing)
    return new Promise((resolve) => this.waiters.push({ match, resolve }))
  }

  /** Resolves once `count` event streams are open. */
  waitForStreams(count = 1): Promise<void> {
    if (this.streams.size >= count) return Promise.resolve()
    return new Promise((resolve) => this.streamWaiters.push({ count, resolve }))
  }

  /** The script behind "!approval": a paused turn waiting for POST /api/chat/approval. */
  scriptApproval(ctx: MockSendContext): MockEvent[] {
    const request_id = crypto.randomUUID()
    const approval = this.addApproval({
      request_id,
      thread_id: ctx.threadId,
      tool_name: "shell",
      description: "Run a shell command",
      parameters: JSON.stringify({ command: "echo hello" }, null, 2),
      allow_always: true,
      corr: ctx.corr,
    })
    const started = ctx.mode === "current" ? [{ type: "turn_started", thread_id: ctx.threadId }] : []
    const gate =
      this.engine === "v2"
        ? [{ type: "gate_required", ...this.gateFields(approval), resume_kind: { Approval: { allow_always: true } } }]
        : []
    const paused = ctx.mode === "current" ? [{ type: "turn_finished", thread_id: ctx.threadId, outcome: "paused" }] : []
    return [
      ...started,
      { type: "thinking", message: "Processing...", thread_id: ctx.threadId },
      ...gate,
      { type: "approval_needed", ...this.gateFields(approval), allow_always: true },
      ...paused,
    ]
  }

  // ---- request handling ----

  private async handle(req: Request): Promise<Response> {
    const url = new URL(req.url)
    const route = `${req.method} ${url.pathname}`
    const limit = route === "POST /api/voice/transcribe" ? TRANSCRIBE_BODY_LIMIT : GLOBAL_BODY_LIMIT
    const rawBody = req.method === "GET" ? "" : await req.text()
    const rec = this.record(req, url, rawBody)
    for (const hook of this.hooks.get(route) ?? []) await hook(rec)
    if (Buffer.byteLength(rawBody) > limit) return text(413, "Failed to buffer the request body: length limit exceeded")
    const override = this.overrides.get(route)
    if (override) {
      override.times -= 1
      if (override.times <= 0) this.overrides.delete(route)
      return override.handler(req, rec)
    }
    const handler = this.routes[route]
    if (!handler) return new Response(null, { status: 404 })
    if (route !== "GET /api/health" && !this.authorized(req)) return text(401, "Invalid or missing auth token")
    return handler(rec)
  }

  private record(req: Request, url: URL, rawBody: string): RecordedRequest {
    let body: unknown = null
    if ((req.headers.get("content-type") ?? "").includes("application/json")) {
      try {
        body = JSON.parse(rawBody) as unknown
      } catch {
        body = null
      }
    }
    const headers: Record<string, string> = {}
    req.headers.forEach((value, key) => {
      headers[key.toLowerCase()] = value
    })
    const rec: RecordedRequest = {
      method: req.method,
      path: url.pathname,
      query: Object.fromEntries(url.searchParams),
      url: `${url.pathname}${url.search}`,
      headers,
      body,
      rawBody,
      openStreams: this.streams.size,
    }
    this.requests.push(rec)
    for (let i = this.waiters.length - 1; i >= 0; i -= 1) {
      const waiter = this.waiters[i]
      if (waiter?.match(rec)) {
        this.waiters.splice(i, 1)
        waiter.resolve(rec)
      }
    }
    return rec
  }

  /** auth.rs: "Bearer " (scheme case-insensitive) then the token; query tokens are never consulted. */
  private authorized(req: Request): boolean {
    const header = req.headers.get("authorization") ?? ""
    if (header.length <= 7 || header.slice(0, 7).toLowerCase() !== "bearer ") return false
    const presented = header.slice(7)
    return this.token === null ? presented.length > 0 : presented === this.token
  }

  private readonly routes: Readonly<Record<string, (rec: RecordedRequest) => Response | Promise<Response>>> = {
    "GET /api/health": () => json(200, { status: "healthy", channel: "gateway" }),
    "GET /api/chat/events": () => this.openStream(),
    "POST /api/chat/send": (rec) => this.send(rec),
    "POST /api/chat/approval": (rec) => this.approval(rec),
    "GET /api/chat/history": (rec) => this.history(rec),
    "GET /api/chat/threads": () => this.threadList(),
    "POST /api/chat/thread/new": () => this.newThread(),
    "GET /api/chat/pending-gates": () => this.pendingGates(),
    "GET /api/chat/capabilities": () => this.capabilities(),
    "POST /api/chat/auth-token": (rec) => this.authToken(rec),
    "POST /api/chat/auth-cancel": (rec) => this.authCancel(rec),
    "GET /api/gateway/status": () => this.gatewayStatus(),
    "GET /api/voice/status": () => this.voiceStatus(),
    "POST /api/voice/transcribe": (rec) => this.transcribe(rec),
  }

  private openStream(): Response {
    if (this.streams.size >= this.maxStreams) return text(503, "Too many connections")
    let self: ReadableStreamDefaultController<Uint8Array> | null = null
    const stream = new ReadableStream<Uint8Array>({
      start: (controller) => {
        self = controller
        this.streams.add(controller)
        controller.enqueue(encoder.encode(":\n\n"))
        for (let i = this.streamWaiters.length - 1; i >= 0; i -= 1) {
          const waiter = this.streamWaiters[i]
          if (waiter && this.streams.size >= waiter.count) {
            this.streamWaiters.splice(i, 1)
            waiter.resolve()
          }
        }
      },
      cancel: () => {
        if (self) this.streams.delete(self)
      },
    })
    return new Response(stream, {
      headers: { "content-type": "text/event-stream", "cache-control": "no-cache", "x-accel-buffering": "no" },
    })
  }

  /** axum's Json extractor: 415 without a JSON content type, 400 on a syntax error, 422 on a shape error. */
  private jsonBody(rec: RecordedRequest): Record<string, unknown> | Response {
    if (!(rec.headers["content-type"] ?? "").includes("application/json")) {
      return text(415, "Expected request with `Content-Type: application/json`")
    }
    if (!isRecord(rec.body)) return text(400, "Failed to parse the request body as JSON")
    return rec.body
  }

  /** `client_turn_id` is an `Option<String>` of the request types in current mode; legacy daemons do not know it. */
  private optionalIds(...fields: string[]): string[] {
    return this.mode === "current" ? [...fields, "client_turn_id"] : fields
  }

  /**
   * Legacy daemons ignore the field (serde skips unknown fields). M2a: a non-string is the extractor's 422 (the
   * shape check normally reports it first), an invalid string 400 "invalid client_turn_id" (8.3.1).
   */
  private clientTurnId(body: Record<string, unknown>): string | undefined | Response {
    const id = body.client_turn_id
    if (this.mode !== "current" || id === undefined || id === null) return undefined
    if (typeof id !== "string") return shapeError(`client_turn_id: invalid type: ${serdeType(id)}, expected a string`)
    return CLIENT_ID.test(id) ? id : text(400, "invalid client_turn_id")
  }

  private corrFor(messageId: string, clientTurnId: string | undefined): MockCorr {
    if (this.mode !== "current") return {}
    return clientTurnId ? { message_id: messageId, client_turn_id: clientTurnId } : { message_id: messageId }
  }

  /** server.rs chat_send_handler: the extractor's shape checks, thread_id, client_turn_id, then the images (G5). */
  private send(rec: RecordedRequest): Response {
    const body = this.jsonBody(rec)
    if (body instanceof Response) return body
    const shape = checkShape(body, { required: ["content"], optional: this.optionalIds("thread_id", "timezone") })
    if (shape) return shape
    const images = checkImagesShape(body.images)
    if (images instanceof Response) return images
    const content = body.content as string
    const threadId = typeof body.thread_id === "string" ? body.thread_id.trim() : ""
    if (threadId === "") return text(400, "thread_id is required for /api/chat/send")
    const clientTurnId = this.clientTurnId(body)
    if (clientTurnId instanceof Response) return clientTurnId
    const rejected = this.checkImages(images)
    if (rejected) return rejected
    const messageId = crypto.randomUUID()
    const thread = this.threads.get(threadId) ?? this.addThread([], threadId)
    const turn: MockTurn = { user_input: content, response: null, state: "Processing", started_at: nowIso(), completed_at: null }
    thread.turns.push(turn)
    const ctx: MockSendContext = {
      gateway: this,
      mode: this.mode,
      content,
      threadId,
      messageId,
      clientTurnId,
      corr: this.corrFor(messageId, clientTurnId),
      body,
    }
    const events = this.replies(ctx)
    if (this.emitBeforeAccept) this.play(events, ctx.corr, turn, 0)
    else setTimeout(() => this.play(events, ctx.corr, turn, 0), 0)
    return json(202, { message_id: messageId, status: "accepted" })
  }

  /**
   * G5 in current mode, in the order of ic/src/channels/image_mime.rs decode_images and with the bodies of
   * server.rs image_rejection_response (413 for the size limits, 422 otherwise, the 1-based image named): the
   * count, the message's base64 length bound, then per image its base64 length bound (both before anything is
   * decoded, so an oversize payload of invalid base64 is a 413), the declared `image/` type, strict base64, empty,
   * the decoded size, the decoded total and the sniffed type. Legacy daemons drop what they cannot decode silently.
   */
  private checkImages(images: readonly WireImage[]): Response | null {
    if (images.length === 0 || this.mode === "legacy") return null
    const maxCount = this.limits.max_attachments
    const maxBytes = this.limits.max_attachment_bytes
    const maxTotal = this.limits.max_total_attachment_bytes
    if (images.length > maxCount) return text(422, `image ${maxCount + 1} rejected: too many images (${images.length}, at most ${maxCount})`)
    const totalTooLarge = () => text(413, `images rejected: the images exceed ${maxTotal} bytes in total`)
    if (images.reduce((sum, image) => sum + image.data.length, 0) > base64Bound(maxTotal)) return totalTooLarge()
    const tooLarge = (n: number) => text(413, `image ${n} rejected: larger than ${maxBytes} bytes`)
    const unsupported = (n: number) => text(422, `image ${n} rejected: unsupported image type (allowed: png, jpeg, gif, webp)`)
    let total = 0
    for (const [i, image] of images.entries()) {
      const n = i + 1
      if (image.data.length > base64Bound(maxBytes)) return tooLarge(n)
      if (!/^image\//i.test(image.media_type)) return unsupported(n)
      const bytes = strictBase64(image.data)
      if (bytes === null) return text(422, `image ${n} rejected: invalid base64`)
      if (bytes.length === 0) return text(422, `image ${n} rejected: no image data`)
      if (bytes.length > maxBytes) return tooLarge(n)
      total += bytes.length
      if (total > maxTotal) return totalTooLarge()
      if (sniff(bytes) === null) return unsupported(n)
    }
    return null
  }

  /** Emits a script (correlated unless an event names its own ids); a `response` completes the history turn. */
  private play(events: readonly MockEvent[], corr: MockCorr, turn: MockTurn | null, index: number): void {
    for (let i = index; i < events.length; i += 1) {
      const event = events[i]
      if (event === undefined) continue
      const explicit = Object.hasOwn(event, "client_turn_id") || Object.hasOwn(event, "message_id")
      this.emit(event, explicit ? {} : corr)
      if (turn && event.type === "response" && typeof event.content === "string") {
        turn.response = event.content
        turn.state = "Completed"
        turn.completed_at = nowIso()
      }
      if (this.stepDelayMs > 0 && i + 1 < events.length) {
        setTimeout(() => this.play(events, corr, turn, i + 1), this.stepDelayMs)
        return
      }
    }
  }

  /** server.rs chat_approval_handler: shape, action, request_id, client_turn_id, then the owning thread and gate. */
  private approval(rec: RecordedRequest): Response {
    const body = this.jsonBody(rec)
    if (body instanceof Response) return body
    const shape = checkShape(body, { required: ["request_id", "action"], optional: this.optionalIds("thread_id") })
    if (shape) return shape
    const action = body.action as string
    if (!["approve", "always", "deny"].includes(action)) return text(400, `Unknown action: ${action}`)
    const requestId = body.request_id as string
    if (!UUID.test(requestId)) return text(400, "Invalid request_id (expected UUID)")
    const clientTurnId = this.clientTurnId(body)
    if (clientTurnId instanceof Response) return clientTurnId
    const requested = typeof body.thread_id === "string" ? body.thread_id.trim() : null
    if (body.thread_id !== undefined && body.thread_id !== null && !requested) {
      return text(400, "thread_id must not be blank when supplied")
    }
    const pending = this.approvals.get(requestId)
    if (!pending) return text(404, "No pending approval matches this request_id")
    if (requested && requested !== pending.thread_id) return text(400, "thread_id does not own this approval request")
    this.approvals.delete(requestId)
    const messageId = crypto.randomUUID()
    const corr = this.corrFor(messageId, clientTurnId)
    setTimeout(() => this.play(this.continuation(pending, action, corr), {}, null, 0), 0)
    return json(202, { message_id: messageId, status: "accepted" })
  }

  /**
   * After an approval, the resumed turn under the owning turn's client_turn_id. Its turn_started comes first: the
   * approval is a message of its own, so the agent loop reports Started before the V2 bridge's resolve_gate
   * broadcasts the (uncorrelated) gate_resolved (ic/src/agent/agent_loop.rs, ic/src/bridge/router.rs).
   */
  private continuation(pending: MockApproval, action: string, corr: MockCorr): MockEvent[] {
    const thread_id = pending.thread_id
    const resolution = action === "deny" ? "denied" : action === "always" ? "approved_always" : "approved"
    const resolved =
      this.engine === "v2"
        ? [{ type: "gate_resolved", ...this.gateFields(pending), resolution, message: "Gate resolved." }]
        : []
    const reply = action === "deny" ? "The command was not run." : "hello"
    const current = this.mode === "current"
    const tools =
      action === "deny"
        ? []
        : [
            { type: "tool_started", name: pending.tool_name, thread_id, ...corr },
            { type: "tool_completed", name: pending.tool_name, success: true, thread_id, ...corr },
          ]
    return [
      ...(current ? [{ type: "turn_started", thread_id, ...corr }] : []),
      ...resolved,
      ...tools,
      { type: "response", content: reply, thread_id, ...corr },
      ...(current ? [{ type: "turn_finished", thread_id, outcome: "replied", ...corr }] : []),
    ]
  }

  private gateFields(gate: MockApproval): Record<string, unknown> {
    return {
      request_id: gate.request_id,
      gate_name: "approval",
      tool_name: gate.tool_name,
      description: gate.description,
      parameters: gate.parameters,
      thread_id: gate.thread_id,
    }
  }

  private history(rec: RecordedRequest): Response {
    const threadId = rec.query.thread_id
    if (threadId === undefined) return text(404, "No active thread")
    if (!UUID.test(threadId)) return text(400, "Invalid thread_id")
    const thread = this.threads.get(threadId)
    if (!thread) return text(404, "Thread not found")
    const limit = Math.max(0, Number(rec.query.limit ?? 50) || 50)
    const turns = thread.turns.slice(-limit).map((turn, i) => ({ turn_number: i, ...turn, tool_calls: [] }))
    const pending = [...this.approvals.values()].find((a) => a.thread_id === threadId)
    return json(200, {
      thread_id: threadId,
      turns,
      has_more: thread.turns.length > limit,
      ...(pending
        ? {
            pending_approval: {
              request_id: pending.request_id,
              tool_name: pending.tool_name,
              description: pending.description,
              parameters: pending.parameters,
            },
          }
        : {}),
    })
  }

  private threadInfo(thread: MockThread, type: string): Record<string, unknown> {
    return {
      id: thread.id,
      state: "Idle",
      turn_count: thread.turns.length,
      created_at: thread.created_at,
      updated_at: thread.created_at,
      thread_type: type,
      channel: "gateway",
      ...(thread.title ? { title: thread.title } : {}),
    }
  }

  private threadList(): Response {
    const assistantId = this.assistantThreadId
    const assistant = assistantId ? (this.threads.get(assistantId) ?? this.addThread([], assistantId)) : null
    const threads = [...this.threads.values()].filter((t) => t.id !== assistantId).map((t) => this.threadInfo(t, "thread"))
    return json(200, {
      assistant_thread: assistant ? this.threadInfo(assistant, "assistant") : null,
      threads,
      active_thread: null,
    })
  }

  private newThread(): Response {
    return json(200, this.threadInfo(this.addThread(), "thread"))
  }

  private pendingGates(): Response {
    const approvals = [...this.approvals.values()].map((a) => ({
      ...this.gateFields(a),
      resume_kind: { Approval: { allow_always: a.allow_always } },
    }))
    const auth = [...this.authGates.values()].map((g) => ({
      request_id: g.request_id,
      thread_id: g.thread_id,
      gate_name: "authentication",
      tool_name: g.extension_name,
      description: g.instructions,
      parameters: "{}",
      resume_kind: { Authentication: { credential_name: g.extension_name, instructions: g.instructions, auth_url: null } },
    }))
    return json(200, { gates: [...approvals, ...auth] })
  }

  private capabilities(): Response {
    if (this.mode === "legacy") return new Response(null, { status: 404 })
    const features = ["status", "attachments", ...(this.voiceConfigured ? ["transcribe"] : []), ...(this.engine === "v2" ? ["stream"] : [])]
    return json(200, {
      api_version: 1,
      server_version: "2.1.9-mock",
      engine: this.engine,
      features,
      limits: this.limits,
      voice: { provider: this.voiceConfigured ? "lunarvoice" : null, stt: this.voiceConfigured, tts: this.voiceConfigured },
      credential: "master",
    })
  }

  /**
   * AuthTokenRequest / AuthCancelRequest: `request_id` and `thread_id` are `Option<Uuid>`, so anything but null or a
   * UUID string fails the extractor (422) before any handler check.
   */
  private authShape(body: Record<string, unknown>, required: readonly string[]): Response | null {
    const shape = checkShape(body, { required, optional: this.optionalIds("request_id", "thread_id") })
    if (shape) return shape
    for (const field of ["request_id", "thread_id"]) {
      const value = body[field]
      if (typeof value === "string" && !UUID.test(value)) return shapeError(`${field}: UUID parsing failed: invalid character`)
    }
    return null
  }

  /** server.rs requested_engine_auth_gate: both ids or neither; a named gate must still be pending (else 409). */
  private gateCheck(body: Record<string, unknown>): Response | MockAuthGate | null {
    const requestId = body.request_id
    const threadId = body.thread_id
    const has = (v: unknown) => v !== undefined && v !== null
    if (has(requestId) !== has(threadId)) return text(400, "request_id and thread_id must be supplied together")
    if (!has(requestId)) return [...this.authGates.values()].find((g) => g.extension_name === body.extension_name) ?? null
    const gate = this.authGates.get(requestId as string)
    if (!gate || gate.thread_id !== threadId || gate.extension_name !== body.extension_name) {
      return text(409, "Authentication request is stale or no longer available")
    }
    return gate
  }

  private authToken(rec: RecordedRequest): Response {
    const body = this.jsonBody(rec)
    if (body instanceof Response) return body
    const shape = this.authShape(body, ["extension_name", "token"])
    if (shape) return shape
    const clientTurnId = this.clientTurnId(body)
    if (clientTurnId instanceof Response) return clientTurnId
    const gate = this.gateCheck(body)
    if (gate instanceof Response) return gate
    const result = this.authTokenResult
    if (result.success) {
      if (gate) this.authGates.delete(gate.request_id)
      setTimeout(() => this.play(this.authContinuation(gate, body.extension_name as string, clientTurnId), {}, null, 0), 0)
    }
    return json(200, { success: result.success, message: result.message, activated: result.success })
  }

  /**
   * V2: the resume message queued by auth-token runs through the agent loop, so its turn_started comes before the
   * bridge's gate_resolved{credential_provided} (ic/src/bridge/router.rs resolve_gate).
   */
  private authContinuation(gate: MockAuthGate | null, extension: string, clientTurnId: string | undefined): MockEvent[] {
    if (!gate || this.engine !== "v2") {
      return [{ type: "auth_completed", extension_name: extension, success: true, message: `${extension} is ready` }]
    }
    const thread_id = gate.thread_id
    const corr = this.corrFor(crypto.randomUUID(), clientTurnId)
    const current = this.mode === "current"
    return [
      ...(current ? [{ type: "turn_started", thread_id, ...corr }] : []),
      { type: "gate_resolved", request_id: gate.request_id, gate_name: "authentication", tool_name: extension, resolution: "credential_provided", message: "Credential accepted.", thread_id },
      { type: "response", content: `${extension} is connected.`, thread_id, ...corr },
      ...(current ? [{ type: "turn_finished", thread_id, outcome: "replied", ...corr }] : []),
    ]
  }

  private authCancel(rec: RecordedRequest): Response {
    const body = this.jsonBody(rec)
    if (body instanceof Response) return body
    const shape = this.authShape(body, ["extension_name"])
    if (shape) return shape
    const clientTurnId = this.clientTurnId(body)
    if (clientTurnId instanceof Response) return clientTurnId
    const gate = this.gateCheck(body)
    if (gate instanceof Response) return gate
    if (gate) {
      this.authGates.delete(gate.request_id)
      if (this.engine === "v2") {
        const resolved = { type: "gate_resolved", request_id: gate.request_id, gate_name: "authentication", tool_name: gate.extension_name, resolution: "cancelled", message: "Authentication cancelled.", thread_id: gate.thread_id }
        setTimeout(() => this.emit(resolved), 0)
      }
    }
    return json(200, { success: true, message: "Auth cancelled" })
  }

  /** server.rs gateway_status_handler (the fields a client may read; no costs or channel health). */
  private gatewayStatus(): Response {
    return json(200, {
      version: "2.1.9-mock",
      sse_connections: this.streams.size,
      ws_connections: 0,
      total_connections: this.streams.size,
      uptime_secs: this.uptimeSecs(),
      restart_enabled: false,
      llm_backend: "openai_compatible",
      llm_model: "mock",
      enabled_channels: ["gateway"],
      skills_enabled: false,
      skills_max_active: 0,
      skills_max_context_tokens: 0,
    })
  }

  private voiceStatus(): Response {
    const extra =
      this.mode === "current"
        ? {
            stt: this.voiceConfigured,
            tts: this.voiceConfigured,
            provider: this.voiceConfigured ? "lunarvoice" : null,
            rate_limit: { requests: 10, window_secs: 60 },
          }
        : {}
    return json(200, {
      configured: this.voiceConfigured,
      ...(this.voiceConfigured ? { model: "parakeet-tdt-0.6b-v2-int8" } : {}),
      max_audio_b64_len: MAX_AUDIO_B64_LEN,
      max_text_chars: 10_000,
      max_synth_audio_secs: 300,
      ...extra,
    })
  }

  private transcribe(rec: RecordedRequest): Response {
    if (!this.voiceConfigured) return voiceError(503, "unavailable", "voice service is not configured")
    const body = this.jsonBody(rec)
    if (body instanceof Response) return body
    const audio = typeof body.audio === "string" ? body.audio : null
    if (audio === null) return text(422, "Failed to deserialize the JSON body into the target type: missing field `audio`")
    if (audio === "") return voiceError(400, "bad_request", "audio must not be empty")
    if (audio.length > MAX_AUDIO_B64_LEN) return voiceError(413, "payload_too_large", `audio exceeds ${MAX_AUDIO_B64_LEN} base64 bytes`)
    if (!/^[A-Za-z0-9+/]*={0,2}$/.test(audio)) return voiceError(400, "bad_request", "audio must be valid base64")
    const format = typeof body.format === "string" ? body.format : "wav"
    if (!AUDIO_FORMATS.includes(format)) return voiceError(400, "bad_request", `format must be one of: ${AUDIO_FORMATS.join(", ")}`)
    const scripted = this.transcribeReply
    if (scripted) {
      this.transcribeReply = null
      return scripted()
    }
    return json(200, { text: "hello from the mock", language: "en", confidence: 0.93, model: "parakeet-tdt-0.6b-v2-int8" })
  }
}

// ---------------------------------------------------------------------------------------------------------
// Test doubles for GatewayTransport's collaborators
// ---------------------------------------------------------------------------------------------------------

/** In-memory TokenSource: reload() switches to the next queued token (a rotated access file). */
export class MemoryTokenSource implements TokenSource {
  reloads = 0
  private token: Secret
  private readonly upcoming: string[] = []

  constructor(
    token: string,
    private readonly tokenKind: TokenKind = "file",
    private readonly source = "tokens/test.token",
  ) {
    this.token = new Secret(token)
  }

  /** The token the next reload() picks up (as if the access file was rewritten). */
  rotateOnReload(token: string): void {
    this.upcoming.push(token)
  }

  current(): Secret {
    return this.token
  }

  async reload(): Promise<boolean> {
    this.reloads += 1
    const next = this.upcoming.shift()
    if (next === undefined) return false
    const changed = next !== this.token.expose()
    this.token = new Secret(next)
    return changed
  }

  describe(): string {
    return this.source
  }

  kind(): TokenKind {
    return this.tokenKind
  }
}

/** In-memory StateFile (primary instance). */
export class MemoryStateFile implements StateFile {
  readonly profile: string
  readonly primary = true
  writes = 0
  private state: ProfileState
  private global: GlobalState = {}

  constructor(initial: ProfileState = {}, profile = "test") {
    this.state = { ...initial }
    this.profile = profile
  }

  async read(): Promise<ProfileState> {
    return { ...this.state }
  }

  async update(change: (current: ProfileState) => ProfileState): Promise<ProfileState> {
    this.state = change({ ...this.state })
    this.writes += 1
    return { ...this.state }
  }

  async readGlobal(): Promise<GlobalState> {
    return { ...this.global }
  }

  async updateGlobal(change: (current: GlobalState) => GlobalState): Promise<GlobalState> {
    this.global = change({ ...this.global })
    return { ...this.global }
  }
}

/** One /proc/net/tcp line in the kernel's format for a LISTEN socket on `address4hex:port`. */
export function procNetTcpLine(slot: number, addressHex: string, port: number, uid: number, state = "0A"): string {
  const portHex = port.toString(16).toUpperCase().padStart(4, "0")
  const remote = addressHex.length === 8 ? "00000000:0000" : `${"0".repeat(32)}:0000`
  return `${String(slot).padStart(4, " ")}: ${addressHex}:${portHex} ${remote} ${state} 00000000:00000000 00:00000000 00000000 ${String(uid).padStart(5, " ")}        0 ${100_000 + slot} 1 0000000000000000 100 0 0 10 0`
}

const TCP_HEADER = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode"
const TCP6_HEADER =
  "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode"

/** A ProcReader whose tables show one 0.0.0.0 LISTEN socket on `port` owned by `uid` (and no IPv6 sockets). */
export function procReaderFor(port: number, uid: number): ProcReader & { reads: number } {
  const reader = async (path: "/proc/net/tcp" | "/proc/net/tcp6"): Promise<string> => {
    reader.reads += 1
    return path === "/proc/net/tcp" ? `${TCP_HEADER}\n${procNetTcpLine(0, "00000000", port, uid)}\n` : `${TCP6_HEADER}\n`
  }
  reader.reads = 0
  return reader
}

// ---------------------------------------------------------------------------------------------------------
// Standalone smoke server
// ---------------------------------------------------------------------------------------------------------

interface CliOptions {
  mode: MockGatewayMode
  port: number
  tokenFile: string | null
  engine: "v1" | "v2"
}

function parseCli(argv: readonly string[]): CliOptions {
  const opts: CliOptions = { mode: "current", port: 0, tokenFile: null, engine: "v2" }
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i]
    const value = argv[i + 1]
    if (value === undefined) throw new Error(`${flag} needs a value`)
    if (flag === "--mode" && (value === "legacy" || value === "current")) opts.mode = value
    else if (flag === "--port" && /^\d{1,5}$/.test(value)) opts.port = Number(value)
    else if (flag === "--token-file") opts.tokenFile = value
    else if (flag === "--engine" && (value === "v1" || value === "v2")) opts.engine = value
    else throw new Error(`unknown argument ${flag} ${value}`)
    i += 1
  }
  return opts
}

async function runStandalone(argv: readonly string[]): Promise<void> {
  let opts: CliOptions
  try {
    opts = parseCli(argv)
  } catch (error) {
    process.stderr.write(`mock-gateway: ${(error as Error).message}\n`)
    process.stderr.write("usage: bun test/mocks/mock-gateway.ts --mode legacy|current [--port N] [--token-file PATH] [--engine v1|v2]\n")
    process.exit(2)
  }
  const token = opts.tokenFile ? (await readFile(opts.tokenFile, "utf8")).trim() : null
  const gateway = await MockGateway.start({
    mode: opts.mode,
    token,
    port: opts.port,
    engine: opts.engine,
    stepDelayMs: 40,
    keepaliveMs: 15_000,
  })
  process.stdout.write(`LISTENING ${gateway.port}\n`)
  const stop = () => {
    void gateway.stop().finally(() => process.exit(0))
  }
  process.on("SIGINT", stop)
  process.on("SIGTERM", stop)
}

if (import.meta.main) await runStandalone(process.argv.slice(2))
