/**
 * Every store action (design 3.3, field lists from area-client 2.3 plus the 3.3 additions). Every action carries
 * `at`, the injected Clock's time, so the reducer stays pure. User intents are dispatched immediately; transport
 * events arrive as `evt` through state/coalesce.ts.
 */
import type {
  ApprovalDecision,
  ApprovalId,
  ControlKind,
  Notice,
  PanelKind,
  PendingImage,
  ThreadId,
  TurnId,
  UiState,
} from "./model.ts"
import type { ResyncSnapshot, ServerInfo, TransportEvent } from "../transport/types.ts"
import type { LevelMeter, VoiceUiState } from "../voice/types.ts"

/** notice/push input; the reducer assigns `at` and `expiresAt` (info 6 s, warn 12 s, errors sticky). */
export type NoticeInput = Omit<Notice, "id" | "at" | "expiresAt" | "sticky"> & {
  /** Default: true for errors, false otherwise. */
  sticky?: boolean
  /** A notice with the same id replaces the earlier one (sticky warnings never stack); default: a new id. */
  id?: string
}

export type Action =
  // ---- connection (controller/supervisor.ts, 3.6.5) ----
  /** Connect attempt `attempt` (1-based) started. */
  | { type: "conn/connecting"; attempt: number; at: number }
  | { type: "conn/ready"; info: ServerInfo; at: number }
  /**
   * The connection dropped; the next attempt (`attempt`) starts at `retryAt`. `resumable` is the lost
   * transport's capabilities.resume: false turns open records into `lost` (legacy socket).
   */
  | { type: "conn/lost"; reason: string; attempt: number; retryAt: number; resumable: boolean; at: number }
  /** Fatal refusal (second 401, 403, EACCES, foreign owner) or /quit ("quit"). */
  | { type: "conn/offline"; reason: string; fatal: boolean; at: number }
  | { type: "thread/selected"; threadId: ThreadId; at: number }

  // ---- user turns and controls (controller/session.ts) ----
  /**
   * Dispatched before Transport.send() so events that overtake the gateway's 202 still attach (3.5 rule 2).
   * `text` is what is sent; for a credential turn (socket path) it is "" and `credential` names the extension:
   * the secret never enters the store.
   */
  | {
      type: "turn/created"
      turnId: TurnId
      text: string
      images: readonly PendingImage[]
      credential?: { extensionName: string }
      at: number
    }
  | { type: "turn/sent"; turnId: TurnId; serverMessageId?: string; at: number }
  /**
   * The send failed (code: a TransportErrorCode or a local reason). `restore` is true when the composer was still
   * empty, so the draft goes to composer.restore; false pushes the text to history with the notice
   * "send failed: text moved to history (Up)" (3.10.5).
   */
  | { type: "turn/send_failed"; turnId: TurnId; code: string; message: string; restore: boolean; at: number }
  /** Appends a one-line note to the turn (TurnRecord.notes). */
  | { type: "turn/note"; turnId: TurnId; text: string; at: number }
  /** An interrupt, steer, /clear or /new request with its own id (3.6.1, 3.5 rule 3b). */
  | { type: "control/created"; turnId: TurnId; kind: ControlKind; text: string; at: number }
  | { type: "control/send_failed"; turnId: TurnId; code: string; message: string; at: number }

  // ---- transport (state/coalesce.ts, controller/supervisor.ts) ----
  | { type: "evt"; event: TransportEvent; at: number }
  /**
   * `wallAt`: the wall clock (epoch ms, Date.now) read together with `at`. History times are wall-clock server
   * times while `at` comes from the injected Clock (performance.now() in production), so the reducer converts
   * them with wallAt - at; without it both clocks are taken to agree (tests whose ManualClock is epoch-like).
   */
  | { type: "resync/applied"; snapshot: ResyncSnapshot; at: number; wallAt?: number }

  // ---- approvals (3.6.2, 3.10.3) ----
  | { type: "approval/answering"; approvalId: ApprovalId; decision: ApprovalDecision; at: number }
  /** `expired`: the gateway answered 404 or REPLv3 unknown_request / no_pending_approval. */
  | { type: "approval/answered"; approvalId: ApprovalId; ok: boolean; expired: boolean; message?: string; at: number }
  /** Alt+]: the next prompt becomes focused and its 1 s freshness window restarts. */
  | { type: "approval/focus_next"; at: number }
  /** A specific prompt became the focused one (mouse, or the panel showing it); sets focusedAt. */
  | { type: "approval/focused"; approvalId: ApprovalId; at: number }
  /** The full view (Alt+V, /approval view) was opened for this prompt; sets viewedFull. */
  | { type: "approval/viewed_full"; approvalId: ApprovalId; at: number }
  /** A typed or dictated reply waits for its confirming Enter (composer.confirm). */
  | { type: "approval/confirm_requested"; approvalId: ApprovalId; decision: "approve" | "deny"; at: number }
  | { type: "approval/confirm_cleared"; at: number }

  // ---- composer (3.10.5, 3.6.4) ----
  | { type: "composer/attach"; image: PendingImage; at: number }
  | { type: "composer/remove_last"; at: number }
  | { type: "composer/clear_attachments"; at: number }
  /** From ComposerHandle.reportMetrics(): visual lines and UTF-8 bytes of the textarea. */
  | { type: "composer/metrics"; lines: number; bytes: number; at: number }
  /** First edit or insert while the composer was empty (composer.startedAt = at). */
  | { type: "composer/started"; at: number }
  /** The composer became empty (composer.startedAt = null). */
  | { type: "composer/cleared"; at: number }
  /** A sent text or cleared draft; in memory only, at most 50 entries. */
  | { type: "composer/history_push"; text: string; at: number }
  | { type: "composer/guard"; text: string; reason: "approval_keyword" | "restart"; until: number; at: number }
  | { type: "composer/guard_clear"; at: number }
  | { type: "composer/restore_consumed"; at: number }
  /** Length of the masked credential buffer; never its content. */
  | { type: "composer/credential_chars"; chars: number; at: number }
  /** First Esc arms (escArmedAt = at), a timeout or other input disarms (null). */
  | { type: "composer/credential_esc_armed"; armed: boolean; at: number }
  /**
   * Credential mode ended. `sent`: the credential went to the daemon. `cancelled`: auth-cancel 2xx or the socket
   * Interrupt was sent (note "credential cancelled"). `ended`: left without sending (timeout, reconnect, thread
   * rotation; note "credential mode ended; nothing was sent"). Both non-sent outcomes seal the owning turn
   * `interrupted` (3.5 rule 3b).
   */
  | { type: "composer/credential_done"; outcome: "sent" | "cancelled" | "ended"; at: number }

  // ---- voice (voice/manager.ts) ----
  | { type: "voice/state"; patch: Partial<VoiceUiState>; at: number }
  /** Throttled meter updates (15 Hz, 4 Hz with reduce_motion); null hides the meter. */
  | { type: "voice/level"; level: LevelMeter | null; at: number }

  // ---- info panel (3.12.3) ----
  | { type: "panel/open"; kind: PanelKind; approvalId?: ApprovalId; at: number }
  /** PageUp/PageDown inside the panel; the reducer clamps at 0. */
  | { type: "panel/scroll"; delta: number; at: number }
  | { type: "panel/close"; at: number }

  // ---- terminal ----
  | { type: "ui/focus"; at: number }
  | { type: "ui/blur"; at: number }
  /** Terminal facts learned after startup (renderer capabilities: multiplexer, KKP, colour). */
  | { type: "ui/patch"; patch: Partial<Omit<UiState, "blurred">>; at: number }

  // ---- notices (3.6.3) ----
  | { type: "notice/push"; notice: NoticeInput; at: number }
  /** Dismisses the notice with `id`, or the newest one. */
  | { type: "notice/dismiss"; id?: string; at: number }
  /** Periodic clock tick: expires notices; other time-based reducer rules (legacy `unanswered`) may use it too. */
  | { type: "notice/tick"; at: number }

  // ---- view ----
  | { type: "view/resize"; width: number; height: number; at: number }

export type ActionType = Action["type"]
export type ActionOf<T extends ActionType> = Extract<Action, { type: T }>
