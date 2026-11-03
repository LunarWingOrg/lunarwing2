/**
 * An in-memory Transport (design 11.3.1) for UI and controller tests. connect() resolves with a scripted
 * ServerInfo (current-gateway or legacy-gateway capabilities), events() is a real EventChannel that emit() feeds,
 * and every request is recorded in `calls` in order, so tests assert exactly what would reach the wire.
 *
 *   const transport = new FakeTransport({ mode: "current" })
 *   transport.emit({ kind: "turn.final", turnId, text: "hi" })   // the supervisor delivers it on the next tick
 *   transport.lose()                                              // the connection drops (TransportError)
 *   transport.sendImpl = () => Promise.reject(new TransportError("rate_limited", "429"))
 *
 * Nothing touches the network. Events reach the store through the supervisor's coalescer, so a test advances
 * the ManualClock by COALESCE_INTERVAL_MS (test/helpers/render.tsx does) instead of wrapping emit() in act().
 */
import type { Secret } from "../../src/config/secret.ts"
import type { ThreadId, TurnId } from "../../src/state/model.ts"
import { IMAGE_MIME_TYPES } from "../../src/state/model.ts"
import { TransportError } from "../../src/transport/errors.ts"
import { EventChannel } from "../../src/transport/event-channel.ts"
import { assertSendableContent } from "../../src/transport/guards.ts"
import type {
  ApproveRequest,
  AuthCancelRequest,
  AuthTokenRequest,
  Capabilities,
  ControlRequest,
  Limits,
  ResyncSnapshot,
  SendRequest,
  SendResult,
  ServerInfo,
  ThreadSpec,
  TranscribeRequest,
  TranscribeResult,
  Transport,
  TransportEvent,
} from "../../src/transport/types.ts"

const MIB = 1024 * 1024
export const FAKE_THREAD_ID = "00000000-0000-4000-8000-000000000001"

export const CURRENT_CAPABILITIES: Capabilities = Object.freeze({
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

export const LEGACY_CAPABILITIES: Capabilities = Object.freeze({
  ...CURRENT_CAPABILITIES,
  streaming: "unknown",
  generatedImages: "unknown",
  correlation: false,
  lifecycle: false,
})

export const FAKE_LIMITS: Limits = Object.freeze({
  maxContentBytes: 100_000,
  maxImages: 5,
  maxImageBytes: 5 * MIB,
  maxTotalImageBytes: 7 * MIB,
  maxRequestBytes: 10 * MIB,
  imageMimeTypes: IMAGE_MIME_TYPES,
  maxTranscribeWavBytes: 18 * MIB,
  transcribePerMinute: 10,
  chatPerMinute: 30,
  maxInFlight: 8,
})

export interface FakeServerInfoOptions {
  mode?: "current" | "legacy"
  capabilities?: Partial<Capabilities>
  limits?: Partial<Limits>
  engine?: ServerInfo["engine"]
  threadId?: ThreadId
  warnings?: string[]
}

export function fakeServerInfo(opts: FakeServerInfoOptions = {}): ServerInfo {
  const legacy = opts.mode === "legacy"
  return {
    transport: "gateway",
    protocol: legacy ? "gateway-legacy" : "gateway-chat",
    protocolVersion: legacy ? null : 1,
    serverVersion: legacy ? null : "test",
    agentName: null,
    engine: opts.engine ?? (legacy ? "unknown" : "v2"),
    sessionId: null,
    threadId: opts.threadId ?? FAKE_THREAD_ID,
    endpointLabel: "http://127.0.0.1:9",
    credential: "unknown",
    capabilities: { ...(legacy ? LEGACY_CAPABILITIES : CURRENT_CAPABILITIES), ...opts.capabilities },
    limits: { ...FAKE_LIMITS, ...opts.limits },
    warnings: opts.warnings ?? [],
  }
}

export type FakeCall =
  | { method: "connect" }
  | { method: "send"; req: SendRequest }
  | { method: "interrupt"; req: ControlRequest }
  | { method: "steer"; req: ControlRequest & { text: string } }
  | { method: "approve"; req: ApproveRequest }
  /** The token is recorded only as its length: tests assert it never leaks elsewhere. */
  | { method: "authToken"; req: Omit<AuthTokenRequest, "token">; tokenLength: number }
  | { method: "authCancel"; req: AuthCancelRequest }
  | { method: "transcribe"; req: Omit<TranscribeRequest, "signal"> }
  | { method: "resync" }
  | { method: "selectThread"; spec: ThreadSpec }
  | { method: "close" }

export type ConnectScript = ServerInfo | TransportError | "hang"

export class FakeTransport implements Transport {
  readonly kind = "gateway" as const
  info: ServerInfo
  readonly calls: FakeCall[] = []
  /** Outcomes of the next connect() calls, in order; when empty, connect() resolves with `info`. */
  readonly connectScript: ConnectScript[] = []
  snapshot: ResyncSnapshot
  sendImpl: ((req: SendRequest) => Promise<SendResult>) | null = null
  approveImpl: ((req: ApproveRequest) => Promise<void>) | null = null
  interruptImpl: ((req: ControlRequest) => Promise<void>) | null = null
  authTokenImpl: ((req: AuthTokenRequest) => Promise<void>) | null = null
  authCancelImpl: ((req: AuthCancelRequest) => Promise<void>) | null = null
  transcribeImpl: ((req: TranscribeRequest) => Promise<TranscribeResult>) | null = null
  /** A secret seen by authToken, kept only so a test can prove it was disposed. */
  lastSecret: Secret | null = null
  private channel: EventChannel | null = null
  private pendingConnect: { reject: (error: unknown) => void } | null = null

  constructor(opts: FakeServerInfoOptions = {}) {
    this.info = fakeServerInfo(opts)
    this.snapshot = { threadId: this.info.threadId ?? FAKE_THREAD_ID, turns: [], approvals: [] }
  }

  /** Method names in call order. */
  get log(): string[] {
    return this.calls.map((call) => call.method)
  }

  callsOf<M extends FakeCall["method"]>(method: M): Extract<FakeCall, { method: M }>[] {
    return this.calls.filter((call): call is Extract<FakeCall, { method: M }> => call.method === method)
  }

  /** Texts of every send(), in order. */
  get sentTexts(): string[] {
    return this.callsOf("send").map((call) => call.req.text)
  }

  async connect(signal: AbortSignal): Promise<ServerInfo> {
    this.calls.push({ method: "connect" })
    const next = this.connectScript.shift() ?? this.info
    if (next === "hang") {
      return new Promise<ServerInfo>((_resolve, reject) => {
        this.pendingConnect = { reject }
        signal.addEventListener("abort", () => reject(new TransportError("closed", "connect was cancelled")), { once: true })
      })
    }
    if (next instanceof TransportError) throw next
    this.channel?.close()
    this.channel = new EventChannel()
    this.info = next
    return next
  }

  events(): AsyncIterable<TransportEvent> {
    if (this.channel !== null) return this.channel
    const failed = new EventChannel()
    failed.fail(new TransportError("closed", "not connected"))
    return failed
  }

  /** Queues events on the open connection (no-op while disconnected). */
  emit(...events: TransportEvent[]): void {
    for (const event of events) this.channel?.push(event)
  }

  /** The connection drops: events() throws `error` after draining. */
  lose(error: TransportError = new TransportError("unreachable", "the event stream was interrupted")): void {
    const channel = this.channel
    this.channel = null
    channel?.fail(error)
  }

  /** Fails a connect() scripted as "hang". */
  failPendingConnect(error: TransportError): void {
    this.pendingConnect?.reject(error)
    this.pendingConnect = null
  }

  async send(req: SendRequest): Promise<SendResult> {
    assertSendableContent(req.text)
    this.calls.push({ method: "send", req })
    if (this.sendImpl !== null) return this.sendImpl(req)
    return { serverMessageId: `msg-${this.calls.length}` }
  }

  async interrupt(req: ControlRequest): Promise<void> {
    this.calls.push({ method: "interrupt", req })
    if (this.interruptImpl !== null) await this.interruptImpl(req)
  }

  async steer(req: ControlRequest & { text: string }): Promise<void> {
    assertSendableContent(req.text)
    this.calls.push({ method: "steer", req })
  }

  async approve(req: ApproveRequest): Promise<void> {
    this.calls.push({ method: "approve", req })
    if (this.approveImpl !== null) await this.approveImpl(req)
  }

  async authToken(req: AuthTokenRequest): Promise<void> {
    const { token, ...rest } = req
    this.lastSecret = token
    this.calls.push({ method: "authToken", req: rest, tokenLength: token.disposed ? -1 : token.expose().length })
    if (this.authTokenImpl !== null) await this.authTokenImpl(req)
  }

  async authCancel(req: AuthCancelRequest): Promise<void> {
    this.calls.push({ method: "authCancel", req })
    if (this.authCancelImpl !== null) await this.authCancelImpl(req)
  }

  async transcribe(req: TranscribeRequest): Promise<TranscribeResult> {
    this.calls.push({ method: "transcribe", req: { requestId: req.requestId, wav: req.wav } })
    if (this.transcribeImpl !== null) return this.transcribeImpl(req)
    return { text: "hello from the fake transport" }
  }

  async resync(): Promise<ResyncSnapshot> {
    this.calls.push({ method: "resync" })
    return this.snapshot
  }

  async selectThread(spec: ThreadSpec): Promise<ThreadId> {
    this.calls.push({ method: "selectThread", spec })
    const id = spec.kind === "id" ? spec.id : "00000000-0000-4000-8000-0000000000ff"
    this.snapshot = { threadId: id, turns: [], approvals: [] }
    return id
  }

  async close(): Promise<void> {
    this.calls.push({ method: "close" })
    const channel = this.channel
    this.channel = null
    channel?.close()
  }
}

/** A TurnId by its text, for events in tests. */
export function turnId(text: string): TurnId {
  return text as TurnId
}
