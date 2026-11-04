/**
 * GatewayTransport (design 3.8): HTTP POST + SSE against a LunarWing web gateway, in both of its modes.
 *
 * - "current" (M2a daemons, GET /api/chat/capabilities answers): events carry client_turn_id, every message gets
 *   turn_started / turn_finished, and replies without a client id are out-of-band.
 * - "legacy" (today's daemons, capabilities 404): no correlation, no lifecycle; the reducer correlates FIFO and
 *   the mapper adds settle hints (3.5 rules 5-12).
 *
 * Security invariants (3.8, 3.8.1, 3.11.3, 3.11.4, 10.2):
 * - Every token-bearing request is preceded by assertListenerOwner when an expected listener is configured, so a
 *   port held by another uid never sees the Authorization header.
 * - Requests use redirect "error" (and any 3xx is refused), carry the bearer token only in the Authorization
 *   header (never in a URL), and fail closed when an environment proxy would route them elsewhere.
 * - Plain http is accepted only to loopback unless allowInsecureHttp is set.
 * - Error messages are built from routes, HTTP statuses and short server reasons; the token is never part of
 *   one, and the auth-token credential is scrubbed from any server text.
 */
import type { Clock, TimerHandle } from "@opentui/core"
import type { Secret } from "../config/secret.ts"
import type { StateFile, TokenKind, TokenSource } from "../config/types.ts"
import { sanitizeLine } from "../sanitize.ts"
import type { ThreadId, TurnId } from "../state/model.ts"
import { asTurnId, isValidClientId } from "../state/ids.ts"
import { TransportError, codeForHttpStatus, isTransportError } from "./errors.ts"
import { EVENT_CHANNEL_CAPACITY, EventChannel } from "./event-channel.ts"
import { isFrameTooLargeError } from "./framing.ts"
import * as wire from "./gateway-wire.ts"
import { assertSendableContent } from "./guards.ts"
import { assertListenerOwner, isLoopbackHost, readProcNet, type ExpectedListener, type ProcReader } from "./listener-owner.ts"
import { SSE_MAX_EVENT_BYTES, SseParser, type SseMessage } from "./sse.ts"
import type {
  ApproveRequest,
  AuthCancelRequest,
  AuthTokenRequest,
  ControlRequest,
  DaemonStart,
  OutgoingImage,
  ResyncSnapshot,
  SendRequest,
  SendResult,
  ServerInfo,
  ThreadSpec,
  TranscribeRequest,
  TranscribeResult,
  Transport,
  TransportEvent,
  WireApproval,
  WireAuthGate,
} from "./types.ts"

// ---------------------------------------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------------------------------------

/** Request budgets (3.8). transcribe exceeds the gateway's own 120 + 30 s sidecar budget. */
export const GATEWAY_TIMEOUTS = Object.freeze({
  healthMs: 2_000,
  getMs: 10_000,
  postMs: 30_000,
  approvalMs: 15_000,
  transcribeMs: 150_000,
  /** Until the event-stream response headers arrive; afterwards the watchdog applies. */
  streamOpenMs: 10_000,
  /** The daemon-start probe (GET /api/gateway/status runs every channel's health check, Signal's over HTTP). */
  statusMs: 3_000,
})

/** Three missed 15 s keepalives (ic/src/channels/web/sse.rs). */
export const SSE_WATCHDOG_MS = 45_000
export const HISTORY_SEED_LIMIT = 30
const THREAD_PROBE_LIMIT = 1
/** 429 answers carry no Retry-After; the chat and voice windows are 60 s. */
const RATE_LIMIT_RETRY_MS = 60_000
/** 503 "Too many connections" on the event stream. */
const BUSY_RETRY_MS = 30_000
const MAX_JSON_BYTES = 16 * 1024 * 1024
const MAX_ERROR_BYTES = 2_048
const MAX_REASON_CHARS = 200
/** Body budget (3.8): sum(4*ceil(n/3)) + 6*contentBytes + 2 KiB <= maxRequestBytes - 64 KiB. */
const BODY_HEADROOM_BYTES = 64 * 1024
const BODY_OVERHEAD_BYTES = 2 * 1024
const KNOWN_TURN_CAPACITY = 4_096
const SEEN_APPROVAL_CAPACITY = 1_024
const AUTH_GATE_CAPACITY = 64

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i

/** Deltas and progress (no approvals, gates or images): the tail of a turn that history already completed (step 6). */
const COMPLETED_TURN_TAIL: ReadonlySet<string> = new Set([
  "stream_chunk",
  "thinking",
  "tool_started",
  "tool_completed",
  "tool_result",
  "status",
  "reasoning_update",
  "skill_activated",
  "turn_recovery",
  "turn_cost",
  "suggestions",
  "thread_state_changed",
])

/** The proxy variables Bun's fetch honours (3.11.4). */
export const GATEWAY_PROXY_ENV_NAMES = ["http_proxy", "HTTP_PROXY", "https_proxy", "HTTPS_PROXY", "all_proxy", "ALL_PROXY"] as const

// ---------------------------------------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------------------------------------

export type FetchLike = (input: string | URL | Request, init?: RequestInit) => Promise<Response>

export interface GatewayTransportOptions {
  baseUrl: URL
  token: TokenSource
  thread: ThreadSpec
  clock: Clock
  timezone: string
  stateFile: StateFile
  fetchImpl?: FetchLike
  /** 3.8.1; null only for non-loopback, non-tenant URLs (then a sticky warning is added). */
  expectedListener: ExpectedListener | null
  /** Injected in tests (transport/listener-owner.ts). */
  readProc?: ProcReader
  /** Tenant name for the 401 hints (UX-10); null for non-tenant profiles. */
  tenant?: string | null
  /** [profile] allow_insecure_http: plain http to a non-loopback host (sticky warning). */
  allowInsecureHttp?: boolean
  /** Environment checked for proxy and unsafe TLS variables before every request; default process.env. */
  env?: Readonly<Record<string, string | undefined>>
  /** image_generated validation; default gateway-wire decodeGeneratedImage. */
  decodeImage?: wire.ImageDecoder
  /** Extra sticky warnings from the configuration, reported in ServerInfo.warnings. */
  warnings?: readonly string[]
}

// ---------------------------------------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------------------------------------

/** The hint for a rejected token, by token kind (UX-10). Never contains the token. */
export function authFailureHint(kind: TokenKind, opts: { tenant: string | null; source: string }): string {
  const tenant = opts.tenant ?? "<tenant>"
  switch (kind) {
    case "client":
      return (
        "the client token was not accepted (revoked, or the daemon does not support client tokens): " +
        `ask for a new grant, \`sudo ic/scripts/lunarwing-mt-admin.sh tui-access ${tenant} --for-user $USER\` ` +
        "(with `--use-master-token` while the tenant predates client tokens)"
      )
    case "master":
      return `the master token changed: re-run \`tui-access ${tenant} --for-user $USER --use-master-token\``
    case "file":
      return `the token in ${opts.source} was rejected`
  }
}

/** The first proxy variable that would route a request through a proxy, or null (NO_PROXY="*" disables all). */
export function activeProxyVariable(env: Readonly<Record<string, string | undefined>>): string | null {
  if (env.NO_PROXY?.trim() === "*" || env.no_proxy?.trim() === "*") return null
  return GATEWAY_PROXY_ENV_NAMES.find((name) => (env[name] ?? "") !== "") ?? null
}

/** URL rules of 3.11.3; throws a fatal TransportError. */
export function validateGatewayUrl(url: URL, allowInsecureHttp = false): void {
  const refuse = (why: string) => new TransportError("bad_request", `gateway URL refused: ${why}`, { fatal: true })
  if (url.protocol !== "http:" && url.protocol !== "https:") throw refuse("only http and https are supported")
  if (url.username !== "" || url.password !== "") throw refuse("it must not contain credentials")
  if (url.search !== "" || url.hash !== "") throw refuse("it must not contain a query or fragment")
  if (url.protocol === "http:" && !allowInsecureHttp && !isLoopbackHost(url.hostname)) {
    throw refuse("plain http is allowed only to loopback (127.0.0.0/8, ::1, localhost); use https")
  }
}

function mib(bytes: number): string {
  return `${Math.round((bytes / (1024 * 1024)) * 10) / 10} MiB`
}

function base64(bytes: Uint8Array): string {
  return Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength).toString("base64")
}

function base64Length(bytes: number): number {
  return 4 * Math.ceil(bytes / 3)
}

function requireClientId(id: string, what: string): TurnId {
  const turnId = asTurnId(id)
  if (turnId === null) throw new TransportError("bad_request", `invalid ${what}: expected 1-64 characters of [A-Za-z0-9_-]`)
  return turnId
}

/** One line, no control characters, at most 200 characters. */
function cleanReason(text: string, secret?: Secret): string {
  let line = text.split("\n", 1)[0] ?? ""
  if (secret && !secret.disposed) {
    const value = secret.expose()
    if (value.length > 0) line = line.split(value).join("[redacted]")
  }
  line = line.replace(/[\u0000-\u001f\u007f-\u009f]/g, "").trim()
  return wire.truncateText(line, MAX_REASON_CHARS)
}

function discard(res: Response): void {
  res.body?.cancel().catch(() => undefined)
}

async function readCapped(res: Response, max: number, truncate: boolean): Promise<Uint8Array> {
  const body = res.body
  if (!body) return new Uint8Array(0)
  const reader = body.getReader()
  const chunks: Uint8Array[] = []
  let total = 0
  for (;;) {
    const { value, done } = await reader.read()
    if (done) break
    chunks.push(value)
    total += value.length
    if (total > max) {
      await reader.cancel().catch(() => undefined)
      if (!truncate) throw new TransportError("protocol", `response larger than ${mib(max)}`)
      break
    }
  }
  const out = new Uint8Array(Math.min(total, max))
  let offset = 0
  for (const chunk of chunks) {
    const part = chunk.subarray(0, Math.max(0, out.length - offset))
    out.set(part, offset)
    offset += part.length
  }
  return out
}

async function readJson(res: Response, label: string): Promise<unknown> {
  const bytes = await readCapped(res, MAX_JSON_BYTES, false)
  try {
    return JSON.parse(new TextDecoder().decode(bytes)) as unknown
  } catch {
    throw new TransportError("protocol", `${label}: the gateway answered with invalid JSON`)
  }
}

interface ErrorReason {
  text: string
  /** The `error` field of a JSON voice error. */
  code: string | null
}

async function readErrorReason(res: Response, secret?: Secret): Promise<ErrorReason> {
  let raw = ""
  try {
    raw = new TextDecoder().decode(await readCapped(res, MAX_ERROR_BYTES, true))
  } catch {
    return { text: "", code: null }
  }
  if ((res.headers.get("content-type") ?? "").includes("json")) {
    try {
      const parsed = wire.parseVoiceError(JSON.parse(raw) as unknown)
      if (parsed) return { text: cleanReason(parsed.message || parsed.error, secret), code: parsed.error }
    } catch {
      // A JSON content type with a plain body: fall through to the text.
    }
  }
  return { text: cleanReason(raw, secret), code: null }
}

function retryAfterMs(res: Response, fallback: number): number {
  const header = res.headers.get("retry-after")
  const seconds = header !== null && /^\d{1,6}$/.test(header.trim()) ? Number(header.trim()) : null
  return seconds === null ? fallback : seconds * 1000
}

function networkError(error: unknown, label: string, origin: string): TransportError {
  const code = typeof error === "object" && error !== null ? (error as { code?: unknown }).code : undefined
  if (code === "UnexpectedRedirect") {
    return new TransportError("protocol", `${label}: the gateway answered with a redirect; redirects are refused`)
  }
  const detail = typeof code === "string" && /^[A-Za-z_]{1,40}$/.test(code) ? ` (${code})` : ""
  return new TransportError("unreachable", `cannot reach the gateway at ${origin}${detail}`)
}

class BoundedSet {
  private readonly items = new Set<string>()

  constructor(private readonly capacity: number) {}

  add(item: string): void {
    this.items.delete(item)
    this.items.add(item)
    if (this.items.size > this.capacity) {
      const oldest = this.items.values().next()
      if (!oldest.done) this.items.delete(oldest.value)
    }
  }

  has(item: string): boolean {
    return this.items.has(item)
  }
}

// ---------------------------------------------------------------------------------------------------------
// Request plumbing
// ---------------------------------------------------------------------------------------------------------

/** Per-route status refinements (3.8, 4.2.7, 5.3.1). */
type RouteKind = "health" | "events" | "read" | "chat" | "approval" | "auth" | "voice"

interface CallSpec {
  method: "GET" | "POST"
  path: string
  query?: Readonly<Record<string, string>>
  /** Built per attempt, so a secret is exposed as late as possible. */
  body?: () => unknown
  timeoutMs: number
  route: RouteKind
  /** Default true: Authorization header plus the listener-owner check. */
  auth?: boolean
  accept?: string
  signal?: AbortSignal
  /** Removed from any server text quoted in an error. */
  secret?: Secret
}

function labelOf(spec: CallSpec): string {
  return `${spec.method} ${spec.path}`
}

interface Connection {
  readonly channel: EventChannel
  readonly abort: AbortController
  /** Raw events received before connect() finished (released at step 6). */
  buffered: wire.GatewayEvent[] | null
  bufferOverflow: boolean
  wire: wire.GatewayWireContext
  watchdog: TimerHandle | null
  ended: boolean
  failure: TransportError | null
  /** The history snapshot of this connect, handed out by the first resync() before any event is consumed. */
  seed: ResyncSnapshot | null
}

// ---------------------------------------------------------------------------------------------------------
// GatewayTransport
// ---------------------------------------------------------------------------------------------------------

export class GatewayTransport implements Transport {
  readonly kind = "gateway" as const

  private readonly opts: GatewayTransportOptions
  private readonly fetchImpl: FetchLike
  private readonly readProc: ProcReader
  private readonly env: Readonly<Record<string, string | undefined>>
  private readonly origin: string
  private readonly pathPrefix: string
  private lifetime = new AbortController()
  private conn: Connection | null = null
  private info: ServerInfo | null = null
  private threadId: ThreadId | null = null
  /** One-time warnings (thread fallbacks, capabilities not understood) for the next ServerInfo. */
  private readonly pendingWarnings: string[] = []
  private readonly knownTurns = new BoundedSet(KNOWN_TURN_CAPACITY)
  private readonly seenApprovals = new BoundedSet(SEEN_APPROVAL_CAPACITY)
  /** Pending V2 authentication gates by extension: auth_required arrives without the gate's ids. */
  private readonly authGates = new Map<string, { requestId: string; threadId: string | undefined }>()
  private reloading: Promise<boolean> | null = null

  constructor(opts: GatewayTransportOptions) {
    validateGatewayUrl(opts.baseUrl, opts.allowInsecureHttp === true)
    this.opts = opts
    this.fetchImpl = opts.fetchImpl ?? ((input, init) => fetch(input, init))
    this.readProc = opts.readProc ?? readProcNet
    this.env = opts.env ?? process.env
    this.origin = opts.baseUrl.origin
    this.pathPrefix = opts.baseUrl.pathname.replace(/\/+$/, "")
  }

  /** The ServerInfo of the last successful connect. */
  get serverInfo(): ServerInfo | null {
    return this.info
  }

  /** The thread every send, history and approval uses. */
  get currentThreadId(): ThreadId | null {
    return this.threadId
  }

  // ---- connect (3.8 steps 1-6) ----

  async connect(signal: AbortSignal): Promise<ServerInfo> {
    this.endConnection(this.conn, "replaced")
    if (this.lifetime.signal.aborted) this.lifetime = new AbortController()
    const conn = this.newConnection()
    this.conn = conn
    const cancelled = () => new TransportError("closed", "connect was cancelled")
    const onAbort = () => this.endConnection(conn, cancelled())
    signal.addEventListener("abort", onAbort, { once: true })
    try {
      if (signal.aborted) throw cancelled()
      await this.checkHealth(signal)
      await this.openEventStream(conn, signal)
      // Alongside the other steps: best effort, it never fails or holds up the connect for long.
      const daemon = this.probeDaemonStart(signal)
      const threadId = await this.ensureThread(signal)
      const caps = await this.fetchCapabilities(signal)
      const voice = await this.fetchVoiceStatus(signal)
      const seed = await this.fetchSnapshot(threadId, signal)
      const daemonStart = await daemon
      if (conn.ended) throw conn.failure ?? new TransportError("closed", "the connection ended while connecting")
      const info = this.buildServerInfo(caps, voice, threadId, daemonStart)
      this.release(conn, caps !== null, seed)
      conn.seed = seed
      this.info = info
      return info
    } catch (error) {
      const failure = signal.aborted ? cancelled() : this.asTransportError(error)
      this.endConnection(conn, failure)
      throw failure
    } finally {
      signal.removeEventListener("abort", onAbort)
    }
  }

  events(): AsyncIterable<TransportEvent> {
    if (this.conn) return this.conn.channel
    const channel = new EventChannel()
    channel.fail(new TransportError("closed", "not connected to the gateway"))
    return channel
  }

  async close(_reason?: string): Promise<void> {
    this.lifetime.abort(new TransportError("closed", "the gateway transport was closed"))
    this.endConnection(this.conn, "closed")
  }

  // ---- chat ----

  async send(req: SendRequest): Promise<SendResult> {
    assertSendableContent(req.text)
    const turnId = requireClientId(req.turnId, "turn id")
    const threadId = this.requireThread()
    const limits = this.limits()
    const checked = this.checkImages(req.images, limits)
    const contentBytes = this.checkContent(req.text)
    const imageB64 = checked.reduce((sum, image) => sum + base64Length(image.bytes.byteLength), 0)
    if (imageB64 + 6 * contentBytes + BODY_OVERHEAD_BYTES > limits.maxRequestBytes - BODY_HEADROOM_BYTES) {
      throw new TransportError("too_large", `the message and its images exceed the gateway's ${mib(limits.maxRequestBytes)} request limit`)
    }
    const images: wire.ImageData[] = checked.map((image) => ({ media_type: image.mime, data: base64(image.bytes) }))
    this.knownTurns.add(turnId)
    const body: wire.SendMessageRequest = {
      content: req.text,
      thread_id: threadId,
      ...this.timezoneField(),
      ...(images.length > 0 ? { images } : {}),
      client_turn_id: turnId,
    }
    const accepted = await this.jsonCall(this.chatSpec(body), [202, 200], wire.parseSendResponse)
    return { serverMessageId: accepted.message_id }
  }

  async interrupt(req: ControlRequest): Promise<void> {
    await this.postControl(req.requestId, "/interrupt")
  }

  async steer(req: ControlRequest & { text: string }): Promise<void> {
    const note = req.text.trim()
    // "/steer" without a note is not a steer for SubmissionParser; it would reach the model as chat.
    if (note === "") throw new TransportError("bad_request", "a steer note needs text")
    await this.postControl(req.requestId, `/steer ${note}`)
  }

  async approve(req: ApproveRequest): Promise<void> {
    const owning = req.turnId !== null && isValidClientId(req.turnId) ? req.turnId : null
    if (owning) this.knownTurns.add(owning)
    const body: wire.ApprovalRequestBody = {
      request_id: req.approvalId,
      action: req.decision,
      ...(req.threadId ? { thread_id: req.threadId } : {}),
      ...(owning ? { client_turn_id: owning } : {}),
    }
    const spec: CallSpec = {
      method: "POST",
      path: "/api/chat/approval",
      body: () => body,
      timeoutMs: GATEWAY_TIMEOUTS.approvalMs,
      route: "approval",
    }
    // 404 "No pending approval matches this request_id" surfaces as not_found: the prompt expired (3.6.2).
    await this.call(spec, async (res) => {
      if (res.status !== 202 && res.status !== 200) throw await this.statusError(res, spec)
      discard(res)
    })
  }

  async authToken(req: AuthTokenRequest): Promise<void> {
    const owning = this.owningTurn(req.turnId)
    const spec: CallSpec = {
      method: "POST",
      path: "/api/chat/auth-token",
      body: (): wire.AuthTokenRequestBody => ({
        extension_name: req.extensionName,
        token: req.token.expose(),
        ...gateIds(req.requestId, req.threadId),
        ...(owning ? { client_turn_id: owning } : {}),
      }),
      timeoutMs: GATEWAY_TIMEOUTS.postMs,
      route: "auth",
      secret: req.token,
    }
    const result = await this.jsonCall(spec, [200], wire.parseActionResponse)
    if (!result.success) {
      const reason = cleanReason(result.message, req.token)
      throw new TransportError("bad_request", `the credential for ${req.extensionName} was not accepted${reason ? `: ${reason}` : ""}`, {
        status: 200,
      })
    }
  }

  async authCancel(req: AuthCancelRequest): Promise<void> {
    const owning = this.owningTurn(req.turnId)
    const body: wire.AuthCancelRequestBody = {
      extension_name: req.extensionName,
      ...gateIds(req.requestId, req.threadId),
      ...(owning ? { client_turn_id: owning } : {}),
    }
    const spec: CallSpec = {
      method: "POST",
      path: "/api/chat/auth-cancel",
      body: () => body,
      timeoutMs: GATEWAY_TIMEOUTS.postMs,
      route: "auth",
    }
    const result = await this.jsonCall(spec, [200], wire.parseActionResponse)
    if (!result.success) {
      throw new TransportError("bad_request", `cancelling the credential for ${req.extensionName} failed`, { status: 200 })
    }
  }

  // ---- voice (3.8, 5.3.1) ----

  async transcribe(req: TranscribeRequest): Promise<TranscribeResult> {
    const info = this.info
    if (!info?.capabilities.transcribe) throw new TransportError("unsupported", "this gateway offers no server transcription")
    const max = info.limits.maxTranscribeWavBytes
    if (req.wav.byteLength === 0) throw new TransportError("bad_request", "the recording is empty")
    if (max > 0 && req.wav.byteLength > max) throw new TransportError("too_large", `the recording is larger than ${mib(max)}`)
    const started = this.opts.clock.now()
    // `language` is omitted: the sidecar treats the gateway's "auto" as its configured language (4.2.10).
    const spec: CallSpec = {
      method: "POST",
      path: "/api/voice/transcribe",
      body: () => ({ audio: base64(req.wav), format: "wav" }),
      timeoutMs: GATEWAY_TIMEOUTS.transcribeMs,
      route: "voice",
      signal: req.signal,
    }
    const result = await this.jsonCall(spec, [200], wire.parseTranscribeResponse)
    return {
      text: result.text,
      ...(result.language ? { language: result.language } : {}),
      ...(result.model ? { model: result.model } : {}),
      elapsedMs: this.opts.clock.now() - started,
    }
  }

  // ---- history and threads ----

  /**
   * History (limit 30) and pending gates of the current thread. Called right after connect() and before the
   * first event is consumed, it returns the snapshot connect() already fetched without another request.
   */
  async resync(): Promise<ResyncSnapshot> {
    const conn = this.conn
    const seed = conn?.seed ?? null
    if (conn) conn.seed = null
    const threadId = this.requireThread()
    const fresh = seed !== null && conn !== null && !conn.ended && conn.channel.delivered === 0
    if (fresh && seed.threadId === threadId) return seed
    return this.fetchSnapshot(threadId)
  }

  async selectThread(spec: ThreadSpec): Promise<ThreadId> {
    const id = await this.resolveThread(spec)
    this.threadId = id
    if (this.conn) this.conn.seed = null
    await this.rememberThread(id)
    return id
  }

  /**
   * GET /api/chat/threads for /thread list and /thread <id prefix> (controller/session.ts ThreadLister, design
   * 3.10 "gateway threads API"). The assistant thread comes first; entries without a UUID id are skipped, and the
   * server's title and state are sanitized here because the threads panel renders them as they are.
   */
  async listThreads(): Promise<GatewayThreadRow[]> {
    const list = await this.jsonCall(this.readSpec("/api/chat/threads"), [200], wire.parseThreadList)
    const rows: GatewayThreadRow[] = []
    if (list.assistant_thread) pushThreadRow(rows, list.assistant_thread, true)
    for (const info of list.threads) pushThreadRow(rows, info, false)
    return rows
  }

  // ---- connect steps ----

  private newConnection(): Connection {
    const conn: Connection = {
      channel: new EventChannel({ capacity: EVENT_CHANNEL_CAPACITY, onReturn: () => this.endConnection(conn, "closed") }),
      abort: new AbortController(),
      buffered: [],
      bufferOverflow: false,
      wire: {
        correlated: false,
        resolveTurnId: (id) => (id !== undefined && this.knownTurns.has(id) ? asTurnId(id) : null),
        ...(this.opts.decodeImage ? { decodeImage: this.opts.decodeImage } : {}),
      },
      watchdog: null,
      ended: false,
      failure: null,
      seed: null,
    }
    return conn
  }

  private async checkHealth(signal: AbortSignal): Promise<void> {
    const spec: CallSpec = {
      method: "GET",
      path: "/api/health",
      timeoutMs: GATEWAY_TIMEOUTS.healthMs,
      route: "health",
      auth: false,
      signal,
    }
    await this.call(spec, async (res) => {
      discard(res)
      if (res.status !== 200) {
        const code = res.status >= 500 ? "unavailable" : "protocol"
        throw new TransportError(code, `no LunarWing gateway at ${this.origin} (GET /api/health answered HTTP ${res.status})`, {
          status: res.status,
        })
      }
    })
  }

  /** Step 2: the stream is open (response headers received) before anything else is requested. */
  private async openEventStream(conn: Connection, signal: AbortSignal): Promise<void> {
    const spec: CallSpec = {
      method: "GET",
      path: "/api/chat/events",
      timeoutMs: GATEWAY_TIMEOUTS.streamOpenMs,
      route: "events",
      accept: "text/event-stream",
    }
    // The stream outlives connect(): it gets its own abort scope; the open phase has its own timer.
    const timer = this.opts.clock.setTimeout(
      () => conn.abort.abort(new TransportError("timeout", `${labelOf(spec)} timed out after ${spec.timeoutMs / 1000} s`)),
      spec.timeoutMs,
    )
    const streamSignal = AbortSignal.any([conn.abort.signal, this.lifetime.signal])
    try {
      const res = await this.exchange(spec, streamSignal)
      if (res.status !== 200) throw await this.statusError(res, spec)
      const type = res.headers.get("content-type") ?? ""
      if (!res.body || !type.includes("text/event-stream")) {
        discard(res)
        throw new TransportError("protocol", `${labelOf(spec)} did not answer with an event stream`)
      }
      this.armWatchdog(conn)
      void this.pump(conn, res.body)
    } catch (error) {
      throw signal.aborted ? new TransportError("closed", "connect was cancelled") : this.callError(error, spec)
    } finally {
      this.opts.clock.clearTimeout(timer)
    }
  }

  private async ensureThread(signal: AbortSignal): Promise<ThreadId> {
    if (this.threadId) return this.threadId
    const id = await this.resolveThread(this.opts.thread, signal)
    this.threadId = id
    await this.rememberThread(id)
    return id
  }

  /** Step 3: last/id are verified with history?limit=1 (400/404 → a new thread); assistant uses assistant_thread. */
  private async resolveThread(spec: ThreadSpec, signal?: AbortSignal): Promise<ThreadId> {
    switch (spec.kind) {
      case "new":
        return this.createThread(signal)
      case "last": {
        const stored = (await this.readState()).lastThreadId
        if (stored && UUID.test(stored) && (await this.threadExists(stored, signal))) return stored
        return this.createThread(signal)
      }
      case "id": {
        if (UUID.test(spec.id) && (await this.threadExists(spec.id, signal))) return spec.id
        this.pendingWarnings.push(`thread ${spec.id} was not found; started a new thread`)
        return this.createThread(signal)
      }
      case "assistant": {
        const list = await this.jsonCall(this.readSpec("/api/chat/threads", undefined, signal), [200], wire.parseThreadList)
        if (list.assistant_thread && UUID.test(list.assistant_thread.id)) return list.assistant_thread.id
        this.pendingWarnings.push("this gateway has no assistant thread; started a new thread")
        return this.createThread(signal)
      }
    }
  }

  private async readState(): Promise<{ lastThreadId?: string }> {
    try {
      return await this.opts.stateFile.read()
    } catch {
      return {}
    }
  }

  private async rememberThread(id: ThreadId): Promise<void> {
    try {
      await this.opts.stateFile.update((state) => (state.lastThreadId === id ? state : { ...state, lastThreadId: id }))
    } catch {
      this.pendingWarnings.push("could not save the thread id in the state file")
    }
  }

  private async threadExists(id: ThreadId, signal?: AbortSignal): Promise<boolean> {
    const spec = this.readSpec("/api/chat/history", { thread_id: id, limit: String(THREAD_PROBE_LIMIT) }, signal)
    return this.call(spec, async (res) => {
      if (res.status !== 200 && res.status !== 400 && res.status !== 404) throw await this.statusError(res, spec)
      discard(res)
      return res.status === 200
    })
  }

  private async createThread(signal?: AbortSignal): Promise<ThreadId> {
    const spec: CallSpec = {
      method: "POST",
      path: "/api/chat/thread/new",
      timeoutMs: GATEWAY_TIMEOUTS.postMs,
      route: "read",
      ...(signal ? { signal } : {}),
    }
    const info = await this.jsonCall(spec, [200, 201], wire.parseThreadInfo)
    if (!UUID.test(info.id)) throw new TransportError("protocol", "POST /api/chat/thread/new returned a thread id that is not a UUID")
    return info.id
  }

  /** Step 4: 404 means a daemon without M2a (legacy mode). */
  private async fetchCapabilities(signal: AbortSignal): Promise<wire.ChatCapabilities | null> {
    const spec = this.readSpec("/api/chat/capabilities", undefined, signal)
    return this.call(spec, async (res) => {
      if (res.status === 404) {
        discard(res)
        return null
      }
      if (res.status !== 200) throw await this.statusError(res, spec)
      const caps = wire.parseChatCapabilities(await readJson(res, labelOf(spec)))
      if (!caps) this.pendingWarnings.push("the capabilities document was not understood; using legacy mode")
      return caps
    })
  }

  /**
   * When this daemon process started, from GET /api/gateway/status `uptime_secs` (authenticated, owner-checked like
   * every token-bearing request; client tokens may read it, design 8.3.5). The status was computed between the two
   * Clock readings, from whole seconds, so the start lies in (sent - (uptime + 1) s, answered - uptime s]. A later
   * connect whose start is later talks to a restarted daemon (state/correlate.ts rule 13). Never rejects: null on any
   * failure, a non-200 or an answer without uptime_secs, and after GATEWAY_TIMEOUTS.statusMs.
   */
  private async probeDaemonStart(signal: AbortSignal): Promise<DaemonStart | null> {
    const spec: CallSpec = { ...this.readSpec("/api/gateway/status", undefined, signal), timeoutMs: GATEWAY_TIMEOUTS.statusMs }
    try {
      const sentAt = this.opts.clock.now()
      const uptime = await this.call(spec, async (res) => {
        if (res.status !== 200) {
          discard(res)
          return null
        }
        return wire.parseGatewayStatus(await readJson(res, labelOf(spec)))?.uptime_secs ?? null
      })
      if (uptime === null) return null
      const answeredAt = this.opts.clock.now()
      return { earliest: sentAt - (uptime + 1) * 1000, latest: answeredAt - uptime * 1000, measuredAt: answeredAt }
    } catch {
      return null
    }
  }

  /** Not rate-limited, and checked before any transcription: the voice limiter counts before its config check. */
  private async fetchVoiceStatus(signal: AbortSignal): Promise<wire.VoiceStatusResponse | null> {
    const spec = this.readSpec("/api/voice/status", undefined, signal)
    return this.call(spec, async (res) => {
      if (res.status !== 200) {
        discard(res)
        return null
      }
      return wire.parseVoiceStatus(await readJson(res, labelOf(spec)))
    })
  }

  /**
   * Step 5 and resync(): history (limit 30) plus the thread's gates. Approval gates become prompts; the
   * pending-gates copy goes first because it carries allow_always (history.pending_approval does not). An
   * Authentication gate means the daemon takes the next chat message as the credential, so it is reported for
   * credential mode (and remembered for the ids of a later auth_required).
   */
  private async fetchSnapshot(threadId: ThreadId, signal?: AbortSignal): Promise<ResyncSnapshot> {
    const [history, gates] = await Promise.all([this.fetchHistory(threadId, signal), this.fetchPendingGates(signal)])
    const approvals: WireApproval[] = []
    const authGates: WireAuthGate[] = []
    for (const gate of gates) {
      if (gate.thread_id !== threadId) continue
      const approval = wire.approvalFromPendingGate(gate)
      if (approval) approvals.push(approval)
      const auth = wire.authGateFromPendingGate(gate)
      if (auth) {
        authGates.push(auth)
        this.rememberAuthGate(auth.extensionName, auth.requestId, auth.threadId)
      }
    }
    const pending = history?.pending_approval
    if (pending && !approvals.some((approval) => approval.id === pending.request_id)) {
      approvals.push(wire.approvalFromHistory(pending, threadId))
    }
    return { threadId, turns: (history?.turns ?? []).map(wire.historyTurnFromWire), approvals, authGates }
  }

  private async fetchHistory(threadId: ThreadId, signal?: AbortSignal): Promise<wire.HistoryResponse | null> {
    const spec = this.readSpec("/api/chat/history", { thread_id: threadId, limit: String(HISTORY_SEED_LIMIT) }, signal)
    return this.call(spec, async (res) => {
      if (res.status === 400 || res.status === 404) {
        discard(res)
        return null
      }
      if (res.status !== 200) throw await this.statusError(res, spec)
      const history = wire.parseHistoryResponse(await readJson(res, labelOf(spec)))
      if (!history) throw new TransportError("protocol", `${labelOf(spec)}: unexpected response`)
      return history
    })
  }

  private async fetchPendingGates(signal?: AbortSignal): Promise<wire.PendingGateView[]> {
    const spec = this.readSpec("/api/chat/pending-gates", undefined, signal)
    // 503 "Failed to list pending gates": history.pending_approval still covers approvals.
    return this.call(spec, async (res) => {
      if (res.status !== 200) {
        discard(res)
        return []
      }
      return wire.parsePendingGates(await readJson(res, labelOf(spec)))
    })
  }

  private buildServerInfo(
    caps: wire.ChatCapabilities | null,
    voice: wire.VoiceStatusResponse | null,
    threadId: ThreadId,
    daemonStart: DaemonStart | null,
  ): ServerInfo {
    const credential = caps ? caps.credential : legacyCredential(this.opts.token.kind())
    const warnings = [...(this.opts.warnings ?? []), ...this.pendingWarnings.splice(0)]
    if (this.opts.expectedListener === null) warnings.push("listener owner not verified (remote URL)")
    if (this.opts.baseUrl.protocol === "http:" && !isLoopbackHost(this.opts.baseUrl.hostname)) {
      warnings.push("plain http to a non-loopback host (allow_insecure_http)")
    }
    if (credential === "master") warnings.push("this TUI uses the tenant master token")
    return {
      transport: "gateway",
      protocol: caps ? "gateway-chat" : "gateway-legacy",
      protocolVersion: caps?.api_version ?? null,
      serverVersion: caps?.server_version ?? null,
      agentName: null,
      engine: caps?.engine ?? "unknown",
      sessionId: null,
      threadId,
      endpointLabel: `${this.origin}${this.pathPrefix}`,
      credential,
      capabilities: caps ? wire.capabilitiesFromChat(caps, voice) : wire.legacyGatewayCapabilities(voice),
      limits: caps ? wire.limitsFromWire(caps.limits) : wire.legacyGatewayLimits(voice),
      warnings: [...new Set(warnings)],
      daemonStart,
    }
  }

  /**
   * Step 6: map the buffered events with the now-known mode; drop one response that history already holds. The
   * snapshot is dispatched before any of these events, so the deltas and progress of that same thread buffered
   * before the duplicate belong to the turn the snapshot already completed: delivered after it, they would open
   * a background record that never ends, or stream into the next turn. They are dropped with it.
   */
  private release(conn: Connection, correlated: boolean, seed: ResyncSnapshot): void {
    conn.wire = { ...conn.wire, correlated }
    const buffered = conn.buffered ?? []
    conn.buffered = null
    const newest = seed.turns.at(-1)?.response ?? null
    const duplicateAt =
      newest === null
        ? -1
        : buffered.findIndex((event) => event.type === "response" && event.content === newest && event.thread_id === seed.threadId)
    buffered.forEach((event, index) => {
      if (index === duplicateAt) return
      if (index < duplicateAt && COMPLETED_TURN_TAIL.has(event.type) && "thread_id" in event && event.thread_id === seed.threadId) return
      this.deliver(conn, event)
    })
    if (conn.bufferOverflow) conn.channel.push({ kind: "resync.required", reason: "overflow" })
  }

  // ---- the event stream ----

  private async pump(conn: Connection, body: ReadableStream<Uint8Array>): Promise<void> {
    const reader = body.getReader()
    const parser = new SseParser({ maxEventBytes: SSE_MAX_EVENT_BYTES })
    try {
      for (;;) {
        const { value, done } = await reader.read()
        if (done) throw new TransportError("closed", "the gateway closed the event stream")
        if (conn.ended) return
        this.armWatchdog(conn)
        for (const message of parser.push(value)) this.onSseMessage(conn, message)
      }
    } catch (error) {
      this.endConnection(conn, this.streamError(error))
    } finally {
      try {
        reader.releaseLock()
      } catch {
        // Already released by a cancelled or errored stream.
      }
    }
  }

  private streamError(error: unknown): TransportError {
    if (isTransportError(error)) return error
    if (isFrameTooLargeError(error)) {
      return new TransportError("protocol", `the gateway sent an event larger than ${mib(SSE_MAX_EVENT_BYTES)}`)
    }
    return new TransportError("unreachable", "the event stream was interrupted")
  }

  private onSseMessage(conn: Connection, message: SseMessage): void {
    let json: unknown
    try {
      json = JSON.parse(message.data) as unknown
    } catch {
      throw new TransportError("protocol", "the gateway sent an event that is not JSON")
    }
    const event = wire.parseGatewayEvent(json)
    if (!event) return
    this.trackAuthGates(event)
    if (conn.buffered) {
      this.buffer(conn, event)
      return
    }
    this.deliver(conn, event)
  }

  /** Same policy as EventChannel: on overflow drop the deltas, then the oldest event, and resync afterwards. */
  private buffer(conn: Connection, event: wire.GatewayEvent): void {
    let buffered = conn.buffered
    if (!buffered) return
    if (buffered.length >= EVENT_CHANNEL_CAPACITY) {
      conn.bufferOverflow = true
      buffered = buffered.filter((e) => e.type !== "stream_chunk")
      if (buffered.length >= EVENT_CHANNEL_CAPACITY) buffered.shift()
      conn.buffered = buffered
    }
    buffered.push(event)
  }

  private deliver(conn: Connection, event: wire.GatewayEvent): void {
    for (const mapped of wire.mapGatewayEvent(event, conn.wire)) {
      const out = this.postProcess(mapped)
      if (out) conn.channel.push(out)
    }
  }

  /** Transport-level state the pure mapper cannot hold: approval dedupe and V2 auth gate ids. */
  private postProcess(event: TransportEvent): TransportEvent | null {
    if (event.kind === "turn.approval") {
      // V2 announces a gate twice (gate_required, then approval_needed); one prompt per request_id.
      if (this.seenApprovals.has(event.approval.id)) return null
      this.seenApprovals.add(event.approval.id)
      return event
    }
    if (event.kind === "turn.auth_required" && event.requestId === undefined) {
      const gate = this.authGates.get(event.extensionName)
      if (!gate) return event
      return { ...event, requestId: gate.requestId, ...(gate.threadId && !event.threadId ? { threadId: gate.threadId } : {}) }
    }
    return event
  }

  private rememberAuthGate(name: string, requestId: string, threadId: string | undefined): void {
    this.authGates.delete(name)
    this.authGates.set(name, { requestId, threadId })
    if (this.authGates.size > AUTH_GATE_CAPACITY) {
      const oldest = this.authGates.keys().next()
      if (!oldest.done) this.authGates.delete(oldest.value)
    }
  }

  private trackAuthGates(event: wire.GatewayEvent): void {
    if (event.type === "gate_required" && "Authentication" in event.resume_kind) {
      this.rememberAuthGate(event.resume_kind.Authentication.credential_name, event.request_id, event.thread_id)
    } else if (event.type === "gate_resolved") {
      for (const [name, gate] of this.authGates) if (gate.requestId === event.request_id) this.authGates.delete(name)
    } else if (event.type === "auth_completed") {
      this.authGates.delete(event.extension_name)
    }
  }

  private armWatchdog(conn: Connection): void {
    if (conn.ended) return
    if (conn.watchdog !== null) this.opts.clock.clearTimeout(conn.watchdog)
    conn.watchdog = this.opts.clock.setTimeout(() => {
      this.endConnection(conn, new TransportError("timeout", `no data from the gateway for ${SSE_WATCHDOG_MS / 1000} s`))
    }, SSE_WATCHDOG_MS)
  }

  /** Ends a connection once: aborts its stream, clears the watchdog, ends (close) or fails its channel. */
  private endConnection(conn: Connection | null, outcome: TransportError | "closed" | "replaced"): void {
    if (!conn || conn.ended) return
    conn.ended = true
    if (conn.watchdog !== null) {
      this.opts.clock.clearTimeout(conn.watchdog)
      conn.watchdog = null
    }
    conn.buffered = null
    conn.seed = null
    if (outcome === "closed" || outcome === "replaced") {
      const why = outcome === "closed" ? "the gateway transport was closed" : "replaced by a new connection"
      conn.abort.abort(new TransportError("closed", why))
      conn.channel.close()
      return
    }
    conn.failure = outcome
    conn.abort.abort(outcome)
    conn.channel.fail(outcome)
  }

  // ---- request plumbing ----

  private url(path: string, query?: Readonly<Record<string, string>>): URL {
    const url = new URL(`${this.pathPrefix}${path}`, this.origin)
    for (const [key, value] of Object.entries(query ?? {})) url.searchParams.set(key, value)
    return url
  }

  private readSpec(path: string, query?: Readonly<Record<string, string>>, signal?: AbortSignal): CallSpec {
    return {
      method: "GET",
      path,
      ...(query ? { query } : {}),
      timeoutMs: GATEWAY_TIMEOUTS.getMs,
      route: "read",
      ...(signal ? { signal } : {}),
    }
  }

  private chatSpec(body: wire.SendMessageRequest): CallSpec {
    return { method: "POST", path: "/api/chat/send", body: () => body, timeoutMs: GATEWAY_TIMEOUTS.postMs, route: "chat" }
  }

  /** Runs one request (with the 401 retry) and `consume` inside one timeout and abort scope. */
  private async call<T>(spec: CallSpec, consume: (res: Response) => Promise<T>): Promise<T> {
    const timeout = new AbortController()
    const timer = this.opts.clock.setTimeout(
      () => timeout.abort(new TransportError("timeout", `${labelOf(spec)} timed out after ${spec.timeoutMs / 1000} s`)),
      spec.timeoutMs,
    )
    const signals = [timeout.signal, this.lifetime.signal]
    if (spec.signal) signals.push(spec.signal)
    try {
      const res = await this.exchange(spec, AbortSignal.any(signals))
      return await consume(res)
    } catch (error) {
      throw this.callError(error, spec)
    } finally {
      this.opts.clock.clearTimeout(timer)
    }
  }

  private async jsonCall<T>(spec: CallSpec, ok: readonly number[], parse: (value: unknown) => T | null): Promise<T> {
    return this.call(spec, async (res) => {
      if (!ok.includes(res.status)) throw await this.statusError(res, spec)
      const parsed = parse(await readJson(res, labelOf(spec)))
      if (parsed === null) throw new TransportError("protocol", `${labelOf(spec)}: unexpected response`)
      return parsed
    })
  }

  private callError(error: unknown, spec: CallSpec): unknown {
    // The caller's own abort (e.g. Esc during a transcription) propagates as its reason, like fetch.
    if (spec.signal?.aborted && !isTransportError(error)) return spec.signal.reason ?? error
    if (isTransportError(error)) return error
    if (this.lifetime.signal.aborted) return new TransportError("closed", "the gateway transport was closed")
    return networkError(error, labelOf(spec), this.origin)
  }

  /** One attempt; on 401 reloads the token once and retries; a second 401 is a fatal `auth` error. */
  private async exchange(spec: CallSpec, signal: AbortSignal): Promise<Response> {
    if (spec.auth === false) return this.refuseRedirect(await this.attempt(spec, signal, null), spec)
    const used = this.opts.token.current()
    const first = await this.attempt(spec, signal, used)
    if (first.status !== 401) return this.refuseRedirect(first, spec)
    discard(first)
    if (!(await this.refreshToken(used))) throw this.authError(spec)
    const second = await this.attempt(spec, signal, this.opts.token.current())
    if (second.status === 401) {
      discard(second)
      throw this.authError(spec)
    }
    return this.refuseRedirect(second, spec)
  }

  private async attempt(spec: CallSpec, signal: AbortSignal, token: Secret | null): Promise<Response> {
    this.assertNoProxy()
    if (token !== null && this.opts.expectedListener !== null) {
      await assertListenerOwner(this.opts.baseUrl, this.opts.expectedListener, this.readProc)
    }
    const headers = new Headers({ Accept: spec.accept ?? "application/json" })
    if (token !== null) headers.set("Authorization", `Bearer ${token.expose()}`)
    let body: string | undefined
    if (spec.body) {
      headers.set("Content-Type", "application/json")
      body = JSON.stringify(spec.body())
    }
    return this.fetchImpl(this.url(spec.path, spec.query), { method: spec.method, headers, body, redirect: "error", signal })
  }

  private refuseRedirect(res: Response, spec: CallSpec): Response {
    if (res.status < 300 || res.status >= 400) return res
    discard(res)
    throw new TransportError("protocol", `${labelOf(spec)}: the gateway answered with a redirect (HTTP ${res.status}); redirects are refused`, {
      status: res.status,
    })
  }

  /** True when a different token is current now (reloaded here or by a concurrent request). */
  private async refreshToken(used: Secret): Promise<boolean> {
    if (!this.opts.token.current().equals(used)) return true
    this.reloading ??= this.opts.token
      .reload()
      .catch(() => false)
      .finally(() => {
        this.reloading = null
      })
    const reloaded = await this.reloading
    return reloaded && !this.opts.token.current().equals(used)
  }

  private authError(spec: CallSpec): TransportError {
    const hint = authFailureHint(this.opts.token.kind(), { tenant: this.opts.tenant ?? null, source: this.opts.token.describe() })
    return new TransportError("auth", `the gateway rejected the token (HTTP 401 on ${labelOf(spec)}): ${hint}`, {
      status: 401,
      fatal: true,
    })
  }

  private assertNoProxy(): void {
    const name = activeProxyVariable(this.env)
    if (name !== null) {
      throw new TransportError("unsupported", `${name} is set: gateway requests are never sent through a proxy (3.11.4)`, {
        fatal: true,
      })
    }
    if (this.opts.baseUrl.protocol === "https:" && this.env.NODE_TLS_REJECT_UNAUTHORIZED === "0") {
      throw new TransportError(
        "unsupported",
        "NODE_TLS_REJECT_UNAUTHORIZED=0 disables certificate verification: refusing the HTTPS gateway request",
        { fatal: true },
      )
    }
  }

  private async statusError(res: Response, spec: CallSpec): Promise<TransportError> {
    const status = res.status
    const reason = await readErrorReason(res, spec.secret)
    const quoted = reason.text ? `: ${reason.text}` : ""
    const label = labelOf(spec)
    if (status === 403) {
      // On auth-token a 403 means "no pending authentication request" (SEC-07), not a token scope problem.
      const scope = spec.route === "auth" ? "" : "; client tokens reach only chat, voice and status routes"
      return new TransportError("forbidden", `${label} was refused (HTTP 403)${quoted}${scope}`, {
        status,
        fatal: spec.route !== "auth",
      })
    }
    if (status === 429) {
      return new TransportError("rate_limited", `${label}: rate limited (HTTP 429)${quoted}`, {
        status,
        retryAfterMs: retryAfterMs(res, RATE_LIMIT_RETRY_MS),
      })
    }
    if (status === 503) return this.unavailableError(spec, reason, res)
    if (status === 409 && spec.route === "auth") {
      return new TransportError("not_found", `${label}: the authentication request is stale or no longer available (HTTP 409)`, { status })
    }
    return new TransportError(codeForHttpStatus(status), `${label} failed (HTTP ${status})${quoted}`, { status })
  }

  private unavailableError(spec: CallSpec, reason: ErrorReason, res: Response): TransportError {
    const label = labelOf(spec)
    const quoted = reason.text ? `: ${reason.text}` : ""
    if (spec.route === "events" && /too many connections/i.test(reason.text)) {
      return new TransportError("busy", `${label}: the gateway has too many event streams open (HTTP 503)`, {
        status: 503,
        retryAfterMs: retryAfterMs(res, BUSY_RETRY_MS),
      })
    }
    // 503 "voice sidecar is unavailable" is a busy sidecar (retry, then fall back); "voice service is not
    // configured" disables the source for the session (5.3.1).
    if (spec.route === "voice" && /sidecar is unavailable/i.test(reason.text)) {
      return new TransportError("busy", `${label}: the voice sidecar is busy (HTTP 503)${quoted}`, { status: 503 })
    }
    return new TransportError("unavailable", `${label}: unavailable (HTTP 503)${quoted}`, { status: 503 })
  }

  private asTransportError(error: unknown): TransportError {
    if (isTransportError(error)) return error
    return new TransportError("protocol", "connecting to the gateway failed unexpectedly")
  }

  // ---- small helpers ----

  private requireThread(): ThreadId {
    if (this.threadId === null) throw new TransportError("closed", "not connected to the gateway")
    return this.threadId
  }

  private limits() {
    return this.info?.limits ?? wire.legacyGatewayLimits(null)
  }

  private timezoneField(): { timezone?: string } {
    return this.opts.timezone.trim() !== "" ? { timezone: this.opts.timezone } : {}
  }

  private owningTurn(turnId: TurnId | null): TurnId | null {
    if (turnId === null || !isValidClientId(turnId)) return null
    this.knownTurns.add(turnId)
    return turnId
  }

  private checkContent(text: string): number {
    const bytes = Buffer.byteLength(text, "utf8")
    const max = this.limits().maxContentBytes
    if (bytes > max) throw new TransportError("too_large", `the message is ${bytes} bytes; the gateway accepts ${max}`)
    return bytes
  }

  /** Count, type (sniffed from the bytes, never the declared type) and size checks, before any I/O. */
  private checkImages(images: readonly OutgoingImage[], limits: ReturnType<GatewayTransport["limits"]>) {
    if (images.length > limits.maxImages) {
      throw new TransportError("too_large", `at most ${limits.maxImages} images per message`)
    }
    let total = 0
    return images.map((image, index) => {
      const mime = wire.sniffAllowedImage(image.bytes)
      if (mime === null || !limits.imageMimeTypes.includes(mime)) {
        throw new TransportError("bad_request", `image ${index + 1} is not a PNG, JPEG, GIF or WebP image`)
      }
      if (image.bytes.byteLength > limits.maxImageBytes) {
        throw new TransportError("too_large", `image ${index + 1} is larger than ${mib(limits.maxImageBytes)}`)
      }
      total += image.bytes.byteLength
      if (total > limits.maxTotalImageBytes) {
        throw new TransportError("too_large", `the images exceed ${mib(limits.maxTotalImageBytes)} in total`)
      }
      return { mime, bytes: image.bytes }
    })
  }

  private async postControl(requestId: TurnId, content: string): Promise<void> {
    assertSendableContent(content)
    const id = requireClientId(requestId, "control id")
    const threadId = this.requireThread()
    this.checkContent(content)
    this.knownTurns.add(id)
    const body: wire.SendMessageRequest = { content, thread_id: threadId, ...this.timezoneField(), client_turn_id: id }
    await this.jsonCall(this.chatSpec(body), [202, 200], wire.parseSendResponse)
  }
}

function legacyCredential(kind: TokenKind): ServerInfo["credential"] {
  return kind === "client" ? "client" : kind === "master" ? "master" : "unknown"
}

/** request_id and thread_id go together or not at all (server.rs requested_engine_auth_gate), and must be UUIDs. */
function gateIds(requestId: string | undefined, threadId: string | undefined): { request_id?: string; thread_id?: string } {
  if (requestId === undefined || threadId === undefined || !UUID.test(requestId) || !UUID.test(threadId)) return {}
  return { request_id: requestId, thread_id: threadId }
}

/** One thread of GET /api/chat/threads; the shape of controller/panel-content.ts ThreadRow. */
export interface GatewayThreadRow {
  id: ThreadId
  title: string | null
  state: string
  turnCount: number
  updatedAt: string
  assistant: boolean
}

const THREAD_TITLE_MAX_CHARS = 200

function pushThreadRow(rows: GatewayThreadRow[], info: wire.ThreadInfo, assistant: boolean): void {
  if (!UUID.test(info.id)) return
  // Cut by code point, so a long title never ends in half a surrogate pair.
  const title = typeof info.title === "string" ? Array.from(sanitizeLine(info.title).trim()).slice(0, THREAD_TITLE_MAX_CHARS).join("") : ""
  const count = info.turn_count
  rows.push({
    id: info.id.toLowerCase(),
    title: title === "" ? null : title,
    state: typeof info.state === "string" ? sanitizeLine(info.state).slice(0, 32) : "unknown",
    turnCount: typeof count === "number" && Number.isSafeInteger(count) && count >= 0 ? count : 0,
    updatedAt: typeof info.updated_at === "string" ? sanitizeLine(info.updated_at).slice(0, 40) : "",
    assistant,
  })
}
