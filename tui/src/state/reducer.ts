/**
 * The root reducer (design 3.3): pure, synchronous, total. Every action carries `at` from the injected Clock, so
 * nothing here reads Date.now(), touches the network or spawns. Turn, control, approval, credential, resync and
 * connection-loss rules live in state/correlate.ts; this module dispatches and owns the simple slices
 * (connection, composer, voice, info panel, terminal, notices, view).
 *
 * A handler that changes nothing returns the state it was given, so the store notifies nobody.
 */
import type { Action, ActionOf, ActionType } from "./actions.ts"
import type { AppState, ComposerState, UiState } from "./model.ts"
import { sanitizeLine } from "../sanitize.ts"
import {
  applyEvent,
  applyResync,
  approvalAnswered,
  approvalAnswering,
  approvalFocusNext,
  approvalFocused,
  approvalViewedFull,
  closePanel,
  connectionLost,
  createControl,
  createUserTurn,
  credentialDone,
  failControl,
  failSend,
  markSent,
  noteDaemonStart,
  noteTurn,
  pushComposerHistory,
  pushNotice,
  selectThread,
  tickTurns,
} from "./correlate.ts"

export type Reducer = (state: AppState, action: Action) => AppState

/** A typed or dictated approval reply waits this long for its confirming Enter (3.10.3). */
export const CONFIRM_WINDOW_MS = 10_000
/** The second Esc must follow the first within this window to cancel a credential (3.6.4). */
export const CREDENTIAL_ESC_WINDOW_MS = 2_000

type Handlers = { readonly [T in ActionType]: (state: AppState, action: ActionOf<T>) => AppState }

const handlers: Handlers = {
  "conn/connecting": (state, action) => ({ ...state, conn: { kind: "connecting", attempt: action.attempt, since: action.at } }),
  "conn/ready": connReady,
  "conn/lost": connectionLost,
  "conn/offline": (state, action) => ({
    ...state,
    conn: { kind: "offline", reason: sanitizeLine(action.reason).trim(), fatal: action.fatal, since: action.at },
  }),
  "thread/selected": (state, action) => selectThread(state, action.threadId),

  "turn/created": createUserTurn,
  "turn/sent": markSent,
  "turn/send_failed": failSend,
  "turn/note": noteTurn,
  "control/created": createControl,
  "control/send_failed": failControl,

  evt: (state, action) => applyEvent(state, action.event, action.at),
  "resync/applied": (state, action) => applyResync(state, action.snapshot, action.at, action.wallAt),

  "approval/answering": approvalAnswering,
  "approval/answered": approvalAnswered,
  "approval/focus_next": (state, action) => approvalFocusNext(state, action.at),
  "approval/focused": (state, action) => approvalFocused(state, action.approvalId, action.at),
  "approval/viewed_full": (state, action) => approvalViewedFull(state, action.approvalId),
  "approval/confirm_requested": (state, action) =>
    patchComposer(state, { confirm: { approvalId: action.approvalId, decision: action.decision, at: action.at } }),
  "approval/confirm_cleared": (state) => (state.composer.confirm === null ? state : patchComposer(state, { confirm: null })),

  "composer/attach": attachImage,
  "composer/remove_last": (state) =>
    state.composer.pendingImages.length === 0
      ? state
      : patchComposer(state, { pendingImages: state.composer.pendingImages.slice(0, -1) }),
  "composer/clear_attachments": (state) =>
    state.composer.pendingImages.length === 0 ? state : patchComposer(state, { pendingImages: [] }),
  "composer/metrics": composerMetrics,
  "composer/started": (state, action) =>
    state.composer.startedAt !== null ? state : patchComposer(state, { startedAt: action.at }),
  "composer/cleared": (state) => (state.composer.startedAt === null ? state : patchComposer(state, { startedAt: null })),
  "composer/history_push": (state, action) => {
    const composer = pushComposerHistory(state.composer, action.text)
    return composer === state.composer ? state : { ...state, composer }
  },
  "composer/guard": (state, action) =>
    patchComposer(state, { guard: { text: action.text, reason: action.reason, until: action.until } }),
  "composer/guard_clear": (state) => (state.composer.guard === null ? state : patchComposer(state, { guard: null })),
  "composer/restore_consumed": (state) =>
    state.composer.restore === null ? state : patchComposer(state, { restore: null }),
  "composer/credential_chars": credentialChars,
  "composer/credential_esc_armed": credentialEscArmed,
  "composer/credential_done": (state, action) => credentialDone(state, action.outcome, action.at),

  "voice/state": voiceState,
  "voice/level": (state, action) => ({ ...state, voice: { ...state.voice, level: action.level } }),

  "panel/open": panelOpen,
  "panel/scroll": (state, action) =>
    state.panel === null
      ? state
      : { ...state, panel: { ...state.panel, scroll: Math.max(0, state.panel.scroll + Math.trunc(action.delta)) } },
  "panel/close": (state, action) => closePanel(state, action.at),

  "ui/focus": (state) => (state.ui.blurred ? { ...state, ui: { ...state.ui, blurred: false } } : state),
  "ui/blur": (state) => (state.ui.blurred ? state : { ...state, ui: { ...state.ui, blurred: true } }),
  "ui/patch": uiPatch,

  "notice/push": (state, action) => pushNotice(state, action.notice, action.at),
  "notice/dismiss": dismissNotice,
  "notice/tick": (state, action) => tickTurns(expireComposer(expireNotices(state, action.at), action.at), action.at),

  "view/resize": (state, action) => {
    const width = Math.max(1, Math.floor(action.width))
    const height = Math.max(1, Math.floor(action.height))
    return width === state.view.width && height === state.view.height ? state : { ...state, view: { width, height } }
  },
}

export function reduce(state: AppState, action: Action): AppState {
  const handler = (handlers as Partial<Record<string, (state: AppState, action: Action) => AppState>>)[action.type]
  return handler === undefined ? state : handler(state, action)
}

/** Alias used by createStore's default. */
export const rootReducer: Reducer = reduce

// ---------------------------------------------------------------------------------------------------------

/** A daemon start later than the known one marks a restart; the resync that follows applies it (3.5 rule 13). */
function connReady(state: AppState, action: ActionOf<"conn/ready">): AppState {
  const next = noteDaemonStart({ ...state, conn: { kind: "ready", since: action.at }, server: action.info }, action.info)
  return action.info.threadId === null ? next : selectThread(next, action.info.threadId)
}

/** Error texts are shown in the voice panel; they are sanitized like every other displayed string. */
function voiceState(state: AppState, action: ActionOf<"voice/state">): AppState {
  const { lastError } = action.patch
  const patch =
    lastError === undefined || lastError === null
      ? action.patch
      : { ...action.patch, lastError: { ...lastError, message: sanitizeLine(lastError.message).trim() } }
  return { ...state, voice: { ...state.voice, ...patch } }
}

function patchComposer(state: AppState, patch: Partial<ComposerState>): AppState {
  return { ...state, composer: { ...state.composer, ...patch } }
}

/** Idempotent per image id (a restored draft may be re-attached by the controller as well). */
function attachImage(state: AppState, action: ActionOf<"composer/attach">): AppState {
  if (state.composer.pendingImages.some((image) => image.id === action.image.id)) return state
  return patchComposer(state, { pendingImages: [...state.composer.pendingImages, action.image] })
}

function composerMetrics(state: AppState, action: ActionOf<"composer/metrics">): AppState {
  const lines = Number.isFinite(action.lines) ? Math.max(1, Math.floor(action.lines)) : 1
  const bytes = Number.isFinite(action.bytes) ? Math.max(0, Math.floor(action.bytes)) : 0
  if (lines === state.composer.lines && bytes === state.composer.bytes) return state
  return patchComposer(state, { lines, bytes })
}

function credentialChars(state: AppState, action: ActionOf<"composer/credential_chars">): AppState {
  const credential = state.composer.credential
  if (credential === null) return state
  const chars = Number.isFinite(action.chars) ? Math.max(0, Math.floor(action.chars)) : 0
  return chars === credential.chars ? state : patchComposer(state, { credential: { ...credential, chars } })
}

function credentialEscArmed(state: AppState, action: ActionOf<"composer/credential_esc_armed">): AppState {
  const credential = state.composer.credential
  if (credential === null) return state
  const escArmedAt = action.armed ? action.at : null
  return escArmedAt === credential.escArmedAt ? state : patchComposer(state, { credential: { ...credential, escArmedAt } })
}

/**
 * Opening the approval view counts as viewing the full parameters (3.6.2). Opened from another panel that
 * covered the prompt, the prompt only now comes on screen, so its 1 s window starts now (SEC-08).
 */
function panelOpen(state: AppState, action: ActionOf<"panel/open">): AppState {
  if (action.kind !== "approval") return { ...state, panel: { kind: action.kind, scroll: 0 } }
  const approvalId = action.approvalId ?? state.approvalQueue[0]
  if (approvalId === undefined || state.approvals[approvalId] === undefined) {
    return { ...state, panel: { kind: "approval", scroll: 0 } }
  }
  const viewed = approvalViewedFull(state, approvalId)
  const covered = state.panel !== null && !(state.panel.kind === "approval" && state.panel.approvalId === approvalId)
  const prompt = viewed.approvals[approvalId]
  const approvals = covered && prompt !== undefined ? { ...viewed.approvals, [approvalId]: { ...prompt, focusedAt: action.at } } : viewed.approvals
  return { ...viewed, approvals, panel: { kind: "approval", scroll: 0, approvalId } }
}

function uiPatch(state: AppState, action: ActionOf<"ui/patch">): AppState {
  const { blurred: _ignored, ...patch } = action.patch as Partial<UiState>
  const ui = { ...state.ui, ...patch }
  const changed = (Object.keys(ui) as (keyof UiState)[]).some((key) => ui[key] !== state.ui[key])
  return changed ? { ...state, ui } : state
}

function dismissNotice(state: AppState, action: ActionOf<"notice/dismiss">): AppState {
  if (state.notices.length === 0) return state
  const notices =
    action.id === undefined
      ? state.notices.slice(0, -1)
      : state.notices.filter((notice) => notice.id !== action.id)
  return notices.length === state.notices.length ? state : { ...state, notices }
}

function expireNotices(state: AppState, at: number): AppState {
  const notices = state.notices.filter((notice) => notice.expiresAt === null || notice.expiresAt > at)
  return notices.length === state.notices.length ? state : { ...state, notices }
}

/** Timed composer states end on the tick, so the footer never shows a stale hint. */
function expireComposer(state: AppState, at: number): AppState {
  const { guard, confirm, credential } = state.composer
  const patch: Partial<ComposerState> = {}
  if (guard !== null && guard.until <= at) patch.guard = null
  if (confirm !== null && confirm.at + CONFIRM_WINDOW_MS <= at) patch.confirm = null
  if (credential !== null && credential.escArmedAt !== null && credential.escArmedAt + CREDENTIAL_ESC_WINDOW_MS <= at) {
    patch.credential = { ...credential, escArmedAt: null }
  }
  return Object.keys(patch).length === 0 ? state : patchComposer(state, patch)
}

