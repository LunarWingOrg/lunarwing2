/**
 * The client state model (design 3.3). A pure reducer (state/reducer.ts) over an external store
 * (state/store.ts); React reads it with useSyncExternalStore(store.subscribe, store.getState).
 *
 * Invariants every module relies on:
 * - The reducer never calls Date.now(), the network or a child process; every action carries `at` from the
 *   injected Clock (ManualClock in tests).
 * - Secrets (tokens, a credential being typed) and audio never enter the store.
 * - Every server string is sanitized (sanitize.ts) before it is stored.
 */
import type { Action } from "./actions.ts"
import type { DaemonStart, FinishOutcome, ServerInfo } from "../transport/types.ts"
import { createInitialVoiceUiState, type VoiceUiState } from "../voice/types.ts"

/** Client-generated correlation id: crypto.randomUUID() (state/ids.ts). */
export type TurnId = string & { readonly __brand: "TurnId" }
/** Server request_id (UUID text). */
export type ApprovalId = string
export type ThreadId = string
export type ImageMime = "image/png" | "image/jpeg" | "image/gif" | "image/webp"

/** The client image allowlist (6.2), in the order clipboard reads prefer them. */
export const IMAGE_MIME_TYPES: readonly ImageMime[] = ["image/png", "image/jpeg", "image/gif", "image/webp"]

export function isImageMime(value: unknown): value is ImageMime {
  return typeof value === "string" && (IMAGE_MIME_TYPES as readonly string[]).includes(value)
}

export type ConnState =
  | { kind: "connecting"; attempt: number; since: number }
  | { kind: "ready"; since: number }
  | { kind: "reconnecting"; attempt: number; retryAt: number; lastError: string }
  | { kind: "offline"; reason: string; fatal: boolean; since: number }

export type TurnPhase =
  /** created; send() in flight */
  | "pending"
  /** send() resolved; no server event yet */
  | "sent"
  /** REPLv3 Ack without TurnStarted, or another turn of this thread is running */
  | "queued"
  /** turn.started or the first correlated event */
  | "running"
  /** finished with outcome paused and an approval prompt pending */
  | "awaiting_approval"
  /** finished with outcome paused, credential or external gate */
  | "awaiting_input"
  /** finished with outcome detached; a late reply will follow */
  | "background_running"
  | "done"
  | "interrupted"
  | "failed"
  /** connection dropped and the transport cannot resume (legacy socket), or the daemon restarted meanwhile */
  | "lost"
  /** legacy daemons only: no event for 10 min after acceptance, or cancelled while still waiting */
  | "unanswered"

export type TurnOrigin = "user" | "control" | "background" | "history"
/** clear/new: forwarded /clear and /new (3.5 rule 3b). */
export type ControlKind = "interrupt" | "steer" | "clear" | "new"

export interface Activity {
  kind: "thinking" | "tool_started" | "tool_completed" | "tool_result" | "status" | "reasoning" | "skill" | "recovery" | "job"
  /** Sanitized to one line. */
  text: string
  ok?: boolean
  at: number
}

export interface TranscriptImage {
  id: string
  mime: ImageMime
  /** null once evicted from memory (only the newest 20 images keep their bytes). */
  bytes: Uint8Array | null
  byteLength: number
  width?: number
  height?: number
  /** A server `path` is shown as caption text, never loaded. */
  caption?: string
}

export interface TurnRecord {
  id: TurnId
  seq: number
  origin: TurnOrigin
  control?: { kind: ControlKind; text: string; ack?: string }
  threadId: ThreadId | null
  input: { text: string; images: TranscriptImage[] } | null
  phase: TurnPhase
  /** Streamed text (state/turn-text.ts), capped at 1 MiB. */
  deltas: string
  /** Canonical text; several finals are joined with a blank line. */
  finalText: string | null
  /** Ring buffer, last 50. */
  activity: Activity[]
  /** Generated images. */
  images: TranscriptImage[]
  approvalIds: ApprovalId[]
  steerNotes: { text: string; ack?: string }[]
  /** One-line annotations: "credential cancelled", "approval expired", the 3.7 image note. */
  notes: string[]
  error?: { code: string; message: string }
  cost?: { inputTokens: number; outputTokens: number; costUsd: string }
  suggestions?: string[]
  /** Gateway 202 message_id (informational). */
  serverMessageId?: string
  /** Filled from /api/chat/history after a gap. */
  recovered?: boolean
  /** History records: the server's started_at text, which identifies the turn across resyncs. */
  historyStartedAt?: string
  finish?: { outcome: FinishOutcome; at: number; droppedStatus?: number }
  /** This turn carried a credential; its text is never stored. */
  credential?: { extensionName: string }
  createdAt: number
  sentAt?: number
  firstEventAt?: number
  lastEventAt?: number
  endedAt?: number
}

export type ApprovalDecision = "approve" | "always" | "deny"

export interface ApprovalPrompt {
  id: ApprovalId
  turnId: TurnId | null
  threadId: ThreadId | null
  toolName: string
  description: string
  parameters: string
  allowAlways: boolean
  /** REPLv3 parameters_truncated: may only be denied (3.6.2). */
  truncated: boolean
  source: "live" | "history" | "pending_gates" | "legacy_text"
  state: "pending" | "answering" | "resolved" | "expired" | "failed"
  decision?: ApprovalDecision
  createdAt: number
  /** When it last became the focused prompt (1 s rule, 3.6.2). */
  focusedAt: number | null
  /** The full view (Alt+V) was opened for this request (3.6.2). */
  viewedFull: boolean
}

export type NoticeLevel = "info" | "warn" | "error"
export type NoticeSource = "client" | "transport" | "server" | "voice" | "image" | "job" | "auth"

export interface Notice {
  id: string
  level: NoticeLevel
  source: NoticeSource
  /** Sanitized, one line; longer output goes to the info panel (3.6.3). */
  text: string
  sticky: boolean
  /** info 6 s, warn 12 s, errors sticky (null). */
  expiresAt: number | null
  at: number
}

export interface PendingImage {
  id: string
  /** Sniffed, never the declared type. */
  mime: ImageMime
  bytes: Uint8Array
  filename: string | null
  width: number
  height: number
  source: "clipboard" | "file" | "drop" | "shot"
}

export interface ComposerState {
  pendingImages: readonly PendingImage[]
  /** First edit or insert since the composer was last empty (3.10.3). */
  startedAt: number | null
  /** Visual lines and UTF-8 bytes, reported by onContentChange (3.10.5). */
  lines: number
  bytes: number
  /** In memory only, newest last, at most 50 entries (3.10.5). */
  history: readonly string[]
  guard: { text: string; reason: "approval_keyword" | "restart"; until: number } | null
  /** A typed or dictated approval reply waiting for its second Enter (3.10.3). */
  confirm: { approvalId: ApprovalId; decision: "approve" | "deny"; at: number } | null
  /** Draft to put back after a failed send (3.10.5); consumed with composer/restore_consumed. */
  restore: { text: string; images: readonly PendingImage[] } | null
  /** Credential mode (3.6.4); the secret itself lives in controller/credential-input.ts. */
  credential: {
    extensionName: string
    threadId: ThreadId | null
    turnId: TurnId | null
    startedAt: number
    /** Length of the masked buffer in controller/credential-input.ts. */
    chars: number
    escArmedAt: number | null
    requestId?: string
    instructions?: string
    authUrl?: string
    setupUrl?: string
  } | null
}

export type PanelKind = "help" | "voice" | "privacy" | "devices" | "transport" | "threads" | "approval"

export interface PanelState {
  kind: PanelKind
  scroll: number
  approvalId?: ApprovalId
}

export interface UiState {
  tmux: boolean
  /** KKP release events are available (negotiated, or a release was seen). */
  kkpRelease: boolean
  blurred: boolean
  color: boolean
  reduceMotion: boolean
  /** Records kept in `order`: [ui] max_turns_in_memory (10-5000), default STATE_BOUNDS.maxTurns. */
  maxTurns: number
}

export interface AppState {
  profile: string
  tenant: string | null
  conn: ConnState
  /** Kept while reconnecting. */
  server: ServerInfo | null
  /** When the daemon process started, from the newest connect that could tell (ServerInfo.daemonStart). */
  daemonStart: DaemonStart | null
  /**
   * A daemon restart seen at conn/ready, applied by the next resync of the thread: what the old process was still
   * handling is sealed lost, after history had its chance to show finished turns (3.5, restart rule).
   */
  daemonRestart: { bootedAfter: number } | null
  threadId: ThreadId | null
  turns: Readonly<Record<TurnId, TurnRecord>>
  /** Render order, newest 500. */
  order: readonly TurnId[]
  /** Unsealed user turns, send order. */
  inflight: readonly TurnId[]
  /** Unsealed control records, send order. */
  controls: readonly TurnId[]
  /** Legacy only (3.5 rule 5a/5b). */
  strayInterruptAck: { until: number } | null
  approvals: Readonly<Record<ApprovalId, ApprovalPrompt>>
  /** Pending prompts, oldest first; the head is the focused prompt. */
  approvalQueue: readonly ApprovalId[]
  composer: ComposerState
  voice: VoiceUiState
  notices: readonly Notice[]
  panel: PanelState | null
  ui: UiState
  otherThreadsUnread: number
  view: { width: number; height: number }
  seq: number
}

/** Implemented by state/store.ts; declared here so controllers and voice compile before it exists (F6). */
export interface Store {
  getState(): AppState
  dispatch(action: Action): void
  dispatchMany(actions: readonly Action[]): void
  subscribe(listener: () => void): () => void
}

/** Bounds the reducer enforces (3.12.5, 3.6.3, 3.10.5). */
export const STATE_BOUNDS = {
  /** Default for UiState.maxTurns, the records kept in `order` (config [ui] max_turns_in_memory). */
  maxTurns: 500,
  /** Only the newest images keep their bytes; older ones become placeholders. */
  maxImagePayloads: 20,
  maxActivityPerTurn: 50,
  maxDeltaBytesPerTurn: 1024 * 1024,
  maxNotices: 20,
  visibleNotices: 2,
  noticeInfoMs: 6_000,
  noticeWarnMs: 12_000,
  maxComposerHistory: 50,
  /** User turns in pending/sent/queued/running: min(this, limits.maxInFlight) (3.6.1); paused/detached excluded. */
  maxLocalInFlight: 8,
} as const

export function createInitialComposerState(): ComposerState {
  return {
    pendingImages: [],
    startedAt: null,
    lines: 1,
    bytes: 0,
    history: [],
    guard: null,
    confirm: null,
    restore: null,
    credential: null,
  }
}

export interface InitialStateOptions {
  profile: string
  tenant: string | null
  view: { width: number; height: number }
  ui?: Partial<UiState>
  /** Clock time of creation; the connection starts as connecting (attempt 0) since then. */
  at: number
}

export function createInitialState(opts: InitialStateOptions): AppState {
  return {
    profile: opts.profile,
    tenant: opts.tenant,
    conn: { kind: "connecting", attempt: 0, since: opts.at },
    server: null,
    daemonStart: null,
    daemonRestart: null,
    threadId: null,
    turns: {},
    order: [],
    inflight: [],
    controls: [],
    strayInterruptAck: null,
    approvals: {},
    approvalQueue: [],
    composer: createInitialComposerState(),
    voice: createInitialVoiceUiState(),
    notices: [],
    panel: null,
    ui: { tmux: false, kkpRelease: false, blurred: false, color: true, reduceMotion: false, maxTurns: STATE_BOUNDS.maxTurns, ...opts.ui },
    otherThreadsUnread: 0,
    view: { width: opts.view.width, height: opts.view.height },
    seq: 0,
  }
}
