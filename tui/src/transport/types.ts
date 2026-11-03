/**
 * The common event model and the transport interface (design 3.4, 3.7, 4.1). Every adapter (gateway now,
 * REPLv3 and legacy socket in M4) maps its wire format onto TransportEvent and folds the server's
 * capabilities into ServerInfo; components read only ServerInfo.
 */
import type { Secret } from "../config/secret.ts"
import type {
  Activity,
  ApprovalDecision,
  ApprovalId,
  ApprovalPrompt,
  ImageMime,
  NoticeLevel,
  NoticeSource,
  ThreadId,
  TurnId,
} from "../state/model.ts"

// ---------------------------------------------------------------------------------------------------------
// Shared wire vocabulary (mirrors ic/crates/lunarwing_common/src/client_caps.rs, 4.1)
// ---------------------------------------------------------------------------------------------------------

/** lunarwing_common::TurnOutcome as serialized by producers (snake_case); identical on both transports. */
export type TurnOutcome =
  | "replied"
  | "no_reply"
  | "paused"
  | "interrupted"
  | "detached"
  | "failed"
  | "timed_out"
  | "cancelled"

/** TurnOutcome plus "unknown" for strings a newer daemon may add (the Rust `#[serde(other)] Unknown`). */
export type FinishOutcome = TurnOutcome | "unknown"

export const TURN_OUTCOMES: readonly TurnOutcome[] = [
  "replied",
  "no_reply",
  "paused",
  "interrupted",
  "detached",
  "failed",
  "timed_out",
  "cancelled",
]

/** Maps a wire `outcome` string one to one; anything else (including non-strings) is "unknown". */
export function toFinishOutcome(wire: unknown): FinishOutcome {
  return typeof wire === "string" && (TURN_OUTCOMES as readonly string[]).includes(wire)
    ? (wire as TurnOutcome)
    : "unknown"
}

/** Feature names advertised by GET /api/chat/capabilities `features` and REPLv3 Welcome.features (4.1). */
export type WireFeature = "status" | "attachments" | "transcribe" | "stream"

/** Canonical order used whenever features are listed (FEATURES_CANONICAL). */
export const WIRE_FEATURES: readonly WireFeature[] = ["status", "attachments", "transcribe", "stream"]

/**
 * lunarwing_common::WireLimits (4.1, 4.2.12), snake_case as on the wire. transport/gateway-wire.ts and
 * transport/replv3-wire.ts re-export this type rather than redeclaring it.
 */
export interface WireLimits {
  max_content_bytes: number
  max_attachments: number
  /** Decoded bytes per image. */
  max_attachment_bytes: number
  /** Decoded bytes per message. */
  max_total_attachment_bytes: number
  attachment_mime_types: string[]
  /** 0 when transcription is not available. */
  max_transcribe_b64_bytes: number
  transcribe_formats: string[]
  messages_per_minute: number
  transcribes_per_minute: number
  /** Gateway JSON body limit. */
  max_request_bytes?: number
  /** REPLv3 line limit. */
  max_frame_bytes?: number
  /** REPLv3 in-flight cap. */
  max_in_flight?: number
  /** REPLv3 idle timeout. */
  idle_timeout_secs?: number
}

// ---------------------------------------------------------------------------------------------------------
// Common event model (3.4)
// ---------------------------------------------------------------------------------------------------------

/** An approval prompt as delivered by a transport; the reducer adds turnId, state and the timing fields. */
export type WireApproval = Omit<ApprovalPrompt, "state" | "createdAt" | "turnId" | "focusedAt" | "viewedFull">

/** gateway gate_resolved strings and REPLv3 ApprovalResolved, normalized (3.4, 3.5 rule 3b). */
export type ApprovalResolution =
  | "approve"
  | "always"
  | "deny"
  | "credential"
  | "external"
  | "cancelled"
  | "terminated"
  | "expired"
  | "unknown"

/** `turnId` is null when the server echoed no client id; state/correlate.ts resolves it. */
export type TransportEvent =
  /** REPLv3 Ack; an Ack without TurnStarted means queued. */
  | { kind: "turn.accepted"; turnId: TurnId }
  /** gateway turn_started, REPLv3 TurnStarted (again after an approval or credential continuation). */
  | { kind: "turn.started"; turnId: TurnId | null; threadId?: ThreadId }
  | { kind: "turn.delta"; turnId: TurnId | null; threadId?: ThreadId; text: string }
  | { kind: "turn.activity"; turnId: TurnId | null; threadId?: ThreadId; activity: Omit<Activity, "at"> }
  /** Bytes already validated by images/data-url.ts and images/sniff.ts; a server path is only ever a caption. */
  | {
      kind: "turn.image"
      turnId: TurnId | null
      threadId?: ThreadId
      image: { mime: ImageMime; bytes: Uint8Array; caption?: string }
    }
  /** Deduplicated by approval.id (request_id) across live, history and pending-gates sources. */
  | { kind: "turn.approval"; turnId: TurnId | null; threadId?: ThreadId; approval: WireApproval }
  | { kind: "turn.approval_resolved"; approvalId: ApprovalId; resolution: ApprovalResolution }
  /** Instructions and URLs are shown as sanitized text and never opened. */
  | {
      kind: "turn.auth_required"
      turnId: TurnId | null
      threadId?: ThreadId
      extensionName: string
      requestId?: string
      instructions?: string
      authUrl?: string
      setupUrl?: string
    }
  | { kind: "turn.final"; turnId: TurnId | null; threadId?: ThreadId; text: string }
  /** Always correlated: adapters drop turn_finished without a known client id. */
  | { kind: "turn.finished"; turnId: TurnId; outcome: FinishOutcome; droppedStatus?: number }
  /** Legacy gateway only (no lifecycle events): status Done/Rejected/Awaiting approval, thread_state_changed. */
  | {
      kind: "turn.settle_hint"
      threadId?: ThreadId
      signal: "done" | "rejected" | "awaiting_approval" | "thread_terminal"
    }
  | {
      kind: "turn.error"
      turnId: TurnId | null
      threadId?: ThreadId
      code: string
      message: string
      retryable: boolean
    }
  | {
      kind: "turn.cost"
      turnId: TurnId | null
      threadId?: ThreadId
      inputTokens: number
      outputTokens: number
      costUsd: string
    }
  | { kind: "turn.suggestions"; turnId: TurnId | null; threadId?: ThreadId; suggestions: string[] }
  | { kind: "notice"; level: NoticeLevel; source: NoticeSource; text: string }
  /** Proactive or job message without a turn. */
  | { kind: "background"; threadId?: ThreadId; text: string }
  | { kind: "other_thread"; threadId: ThreadId }
  | { kind: "resync.required"; reason: "gap" | "reconnect" | "overflow" }

export type TransportEventKind = TransportEvent["kind"]
export type TransportEventOf<K extends TransportEventKind> = Extract<TransportEvent, { kind: K }>

// ---------------------------------------------------------------------------------------------------------
// Capability model (3.7)
// ---------------------------------------------------------------------------------------------------------

export type TransportKind = "gateway" | "socket"
export type ProtocolFlavor = "gateway-chat" | "gateway-legacy" | "replv3" | "repl-legacy"
export type Tri = boolean | "unknown"

/** Folded from WireLimits or the legacy defaults of the 3.7 table. */
export interface Limits {
  /** UTF-8 bytes of the message text, enforced at send (3.10.5). */
  maxContentBytes: number
  maxImages: number
  /** Decoded bytes per image. */
  maxImageBytes: number
  /** Decoded bytes per message. */
  maxTotalImageBytes: number
  /** Gateway JSON body, or socket frame including the "\n". */
  maxRequestBytes: number
  /** Server list intersected with the client allowlist (state/model.ts IMAGE_MIME_TYPES). */
  imageMimeTypes: readonly ImageMime[]
  /** Decoded WAV bytes: floor(max_transcribe_b64_bytes * 3 / 4); 0 = no server transcription. */
  maxTranscribeWavBytes: number
  transcribePerMinute: number | null
  chatPerMinute: number | null
  /** Server in-flight cap; the local cap is min(8, maxInFlight) (3.6.1). */
  maxInFlight: number
}

export interface Capabilities {
  /** Token deltas (stream_chunk); "unknown" on a legacy gateway until the first chunk. */
  streaming: Tri
  /** thinking / tool progress events. */
  activity: boolean
  /** Image input accepted. */
  images: boolean
  /** image_generated output possible on this engine. */
  generatedImages: Tri
  /** Prompts are delivered and approve() works. */
  approvals: boolean
  /** Transport.transcribe usable (LunarVoice). */
  transcribe: boolean
  /** The server echoes the TurnId on every event of a turn. */
  correlation: boolean
  /** turn_started / turn_finished (TurnStarted / TurnFinished) per message. */
  lifecycle: boolean
  interrupt: "native" | "text"
  steer: "native" | "text"
  threads: boolean
  /** resync() available. */
  history: boolean
  backgroundMessages: boolean
  /** In-flight turns survive a reconnect. */
  resume: boolean
}

export interface ServerInfo {
  transport: TransportKind
  protocol: ProtocolFlavor
  protocolVersion: number | null
  serverVersion: string | null
  agentName: string | null
  engine: "v1" | "v2" | "unknown"
  sessionId: string | null
  threadId: ThreadId | null
  /** Socket path or URL origin; never a token. */
  endpointLabel: string
  credential: "master" | "client" | "none" | "unknown"
  capabilities: Capabilities
  limits: Limits
  /** Sticky warnings, e.g. "listener owner not verified (remote URL)", "this TUI uses the tenant master token". */
  warnings: readonly string[]
}

// ---------------------------------------------------------------------------------------------------------
// Requests and results (3.7)
// ---------------------------------------------------------------------------------------------------------

export interface OutgoingImage {
  /** Always the sniffed type (6.2). */
  mime: ImageMime
  bytes: Uint8Array
  filename: string | null
}

export interface SendRequest {
  turnId: TurnId
  text: string
  images: readonly OutgoingImage[]
}

export interface SendResult {
  /** Gateway 202 message_id (informational). */
  serverMessageId?: string
}

/** Interrupt and steer: a control record of their own; `requestId` becomes client_turn_id / msg_id. */
export interface ControlRequest {
  requestId: TurnId
  text?: string
}

export interface ApproveRequest {
  approvalId: ApprovalId
  decision: ApprovalDecision
  threadId: ThreadId | null
  /** The owning (original) turn: the continuation is correlated to it (4.1 rule 4). */
  turnId: TurnId | null
}

/** Gateway POST /api/chat/auth-token (3.6.4); turnId is sent as client_turn_id. */
export interface AuthTokenRequest {
  extensionName: string
  token: Secret
  /** The owning turn of the auth_required; null when unknown. */
  turnId: TurnId | null
  requestId?: string
  threadId?: ThreadId
}

/** Gateway POST /api/chat/auth-cancel (3.6.4); turnId is sent as client_turn_id. */
export interface AuthCancelRequest {
  extensionName: string
  turnId: TurnId | null
  requestId?: string
  threadId?: ThreadId
}

export interface TranscribeRequest {
  /** "t-" + UUIDv4 (state/ids.ts newTranscribeRequestId). */
  requestId: string
  wav: Uint8Array
  signal: AbortSignal
}

export interface TranscribeResult {
  text: string
  language?: string
  model?: string
  elapsedMs?: number
}

export interface HistoryTurn {
  userInput: string
  response: string | null
  state: string
  startedAt: string
  completedAt: string | null
}

/**
 * A pending V2 authentication gate of the thread (GET /api/chat/pending-gates, resume_kind Authentication).
 * While it waits, the daemon takes the next chat message on the thread as the credential (3.6.4).
 */
export interface WireAuthGate {
  requestId: string
  threadId: ThreadId
  extensionName: string
  instructions?: string
  authUrl?: string
}

export interface ResyncSnapshot {
  threadId: ThreadId
  turns: readonly HistoryTurn[]
  approvals: readonly WireApproval[]
  /** Authentication gates of this thread; credential mode is restored from them (absent: none reported). */
  authGates?: readonly WireAuthGate[]
}

export type ThreadSpec = { kind: "last" } | { kind: "new" } | { kind: "assistant" } | { kind: "id"; id: ThreadId }

export interface Transport {
  readonly kind: TransportKind
  /** One connection; may be called again after events() ended. Rejects with TransportError. */
  connect(signal: AbortSignal): Promise<ServerInfo>
  /** Single consumer. Ends normally after close(); throws TransportError when the connection is lost. */
  events(): AsyncIterable<TransportEvent>
  send(req: SendRequest): Promise<SendResult>
  interrupt(req: ControlRequest): Promise<void>
  steer(req: ControlRequest & { text: string }): Promise<void>
  approve(req: ApproveRequest): Promise<void>
  /** Gateway only. */
  authToken?(req: AuthTokenRequest): Promise<void>
  /** Gateway only. */
  authCancel?(req: AuthCancelRequest): Promise<void>
  /** Check capabilities.transcribe first; otherwise rejects with TransportError("unsupported"). */
  transcribe?(req: TranscribeRequest): Promise<TranscribeResult>
  /** Gateway: authoritative state after a gap or reconnect. */
  resync?(): Promise<ResyncSnapshot>
  /** Gateway threads; REPLv3 rotates the thread token and reconnects. */
  selectThread?(spec: ThreadSpec): Promise<ThreadId>
  close(reason?: string): Promise<void>
}
