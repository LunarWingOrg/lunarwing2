/**
 * Correlation and sealing (design 3.5): which turn record every transport event belongs to, and when a turn
 * ends. Also the record-level parts of the turn, control, approval, credential, resync and connection actions,
 * because they share these rules. Everything here is pure: `at` comes from the action, never from Date.now().
 *
 * Event order, applied to every `evt`:
 * 1. Thread filter: an event of another thread only bumps otherThreadsUnread (approvals and credential requests
 *    also raise a warn notice).
 * 2. Exact match: a turnId naming a known record applies to it; records exist before their request is written.
 * 3. Lifecycle sealing: turn.finished sets the phase (table in finishUserTurn). Unsealed awaiting_* and
 *    background_running turns return to running on turn.started or correlated progress. On a sealed record
 *    deltas and progress are dropped, a final is kept as late text, a second turn.finished is ignored.
 *    3b. Paused turns whose gate is discarded are sealed interrupted: gate_resolved cancelled, terminated or
 *    expired; an interrupt, /clear or /new control answered with anything but "Nothing to interrupt."; the
 *    credential flow ending locally (composer/credential_done).
 * 4. Correlated without lifecycle: a correlated final seals.
 * 5. Uncorrelated final (legacy): control acks seal the oldest control of their kind; a stray "Interrupted." is
 *    swallowed once; otherwise the oldest inflight turn is sealed done (FIFO), skipping turns paused at an open
 *    prompt or credential request (legacyTarget); with nothing inflight it is a background record.
 *    5a. The daemon answers an interrupt from its scheduler (ic/src/agent/agent_loop.rs handle_priority_control,
 *    dispatch.rs cancel_lane): with a turn of the thread running it stops that one ("Interrupted.") and queued
 *    follow-ups still run; with only queued messages it drops them all ("Interrupted." once); with nothing it
 *    says "Nothing to interrupt.". So "Interrupted." ends the oldest running user turn created before the
 *    interrupt (arming the V1 stray-ack window), or, when none of them saw an event yet, every one still waiting
 *    ("cancelled before it started"); "Nothing to interrupt." ends the ones still waiting as unanswered (rule 12).
 * 6. Correlated transport, final without id: a background record, never FIFO.
 * 7. Uncorrelated progress (legacy): the oldest inflight turn; with none, deltas and images open a background
 *    record and activity is dropped.
 * 8. Uncorrelated approval (legacy): owned by the oldest inflight turn.
 * 9. turn.error: with an id it fails that record; without one it fails the oldest inflight turn (legacy) or
 *    becomes an error notice.
 * 10. turn.auth_required: credential mode (3.6.4); the secret itself never enters the store.
 * 11. Settle hints are for the session controller (legacy resync grace); the store ignores them.
 * 12. Waiting (legacy gateway, tickTurns): a turn at the head of the queue with no event 15 s after acceptance
 *    gets the "no reply yet" note and stays first in line, because a daemon busy with another channel's turn
 *    holds the message in its MessageScheduler without a word, and its reply arrives later (review finding
 *    client-correctness-4). It is sealed unanswered only after the stale limit of history recovery (610 s), or
 *    when the user cancels it (rule 5a). The first event removes the note.
 * 13. Daemon restart (resync): a connect that sees a later daemon start than the one before (ServerInfo
 *    .daemonStart, GET /api/gateway/status uptime_secs) marks a restart; the next resync of the thread first
 *    recovers what history shows finished, then seals everything still open from before the new start "lost
 *    (daemon restarted)", except a turn paused at a gate the restarted daemon still lists (V2 keeps persisted
 *    gates). Such turns never take another reply (review finding client-correctness-6). Without a usable start
 *    (the probe failed) nothing changes: a young reply-less Failed turn stays open as before.
 *
 * Every server string is sanitized here (sanitize.ts) before it is stored, whatever the adapter did: replies,
 * activity, notices and captions are stripped (sanitizeText); approval text and credential URLs are escaped
 * instead (sanitizeVisible), so nothing the user decides on can be hidden by a swallowed escape sequence.
 */
import type { ActionOf, NoticeInput } from "./actions.ts"
import {
  STATE_BOUNDS,
  isImageMime,
  type Activity,
  type AppState,
  type ApprovalId,
  type ApprovalPrompt,
  type ComposerState,
  type ControlKind,
  type PendingImage,
  type ThreadId,
  type TranscriptImage,
  type TurnId,
  type TurnOrigin,
  type TurnPhase,
  type TurnRecord,
} from "./model.ts"
import type {
  ApprovalResolution,
  DaemonStart,
  FinishOutcome,
  HistoryTurn,
  ResyncSnapshot,
  ServerInfo,
  TransportEvent,
  TransportEventOf,
  WireApproval,
} from "../transport/types.ts"
import { NOTHING_TO_INTERRUPT_ACK, controlAckKind, isInterruptedAck } from "../transport/control-acks.ts"
import { sanitizeText, sanitizeVisible } from "../sanitize.ts"
import { appendDelta, applyFinal, isSealedPhase, tailToUtf8Bytes, turnTextOf } from "./turn-text.ts"

// ---------------------------------------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------------------------------------

/** Rule 5a: after a legacy "Interrupted." ack, V1's second "Interrupted." (the aborted turn) is swallowed. */
export const STRAY_INTERRUPT_WINDOW_MS = 5_000
/** Resync: durable V2 history shows in-flight turns as Failed; the late watcher may run another 600 s. */
export const YOUNG_FAILED_TURN_MS = 610_000
/** Rule 12: the "no reply yet" note. */
export const NO_REPLY_YET_MS = 15_000
/** Rule 12: unanswered only after the same stale limit as history recovery. */
export const UNANSWERED_AFTER_MS = YOUNG_FAILED_TURN_MS
/** Rule 13: a daemon start later than the known one by more than this (plus drift) is a restart. */
export const RESTART_SLACK_MS = 2_000
/** Rule 13: the client's and the daemon host's clocks may drift apart by up to 1 ms per second. */
export const RESTART_DRIFT_RATIO = 0.001
/** Resync: a history turn matches a local turn that was sent at most this long after the turn started. */
export const HISTORY_START_SKEW_MS = 5_000
/** Prompts that are no longer pending (kept for the transcript) beyond this count are forgotten, oldest first. */
export const MAX_SETTLED_APPROVALS = 200
const MAX_NOTES_PER_TURN = 20
const MAX_ACTIVITY_TEXT = 1_000
const MAX_LINE_TEXT = 2_000
const MAX_SUGGESTIONS = 10

/** Notes and notice texts the reducer writes; components may use them as labels too. */
export const NOTES = {
  noReply: "no reply",
  /** Label for background_running (outcome detached). */
  detached: "still running; the result will arrive here",
  cancelled: "cancelled before it started",
  late: "reply arrived after the turn finished",
  deltaCut: "streamed text over 1 MiB was cut; the final reply replaces it",
  imageNotDelivered: "image generated, but this daemon does not deliver images to clients (needs the M2a daemon)",
  /** Rule 12, while the turn waits; removed by its first event. */
  noReplyYet: "no reply yet: the daemon may be busy with another channel; a late reply still lands here (Ctrl+S cancels)",
  unanswered: "no reply within 10 min (consumed as a control input?)",
  /** Rule 5a: the interrupt found nothing running or queued for the thread, so no reply will come. */
  nothingToInterrupt: "cancelled: the daemon had nothing running or queued for it",
  lost: "connection lost; this turn cannot be resumed",
  /** Rule 13; also the status line of such a turn. */
  daemonRestarted: "lost (daemon restarted)",
  credentialCancelled: "credential cancelled",
  credentialEnded: "credential mode ended; nothing was sent",
  approvalExpired: "approval expired",
  approvalCancelled: "approval cancelled",
  threadTerminated: "thread terminated",
  failedWithoutReply: "the daemon recorded this turn as failed",
  sendFailedToHistory: "send failed: text moved to history (Up)",
  otherThreadApproval: "approval pending in another thread",
  otherThreadAuth: "authentication required in another thread",
} as const

/** Notice ids that replace themselves instead of stacking. */
export const NOTICE_IDS = {
  otherThreadApproval: "other-thread-approval",
  otherThreadAuth: "other-thread-auth",
  credentialEnded: "credential-ended",
} as const

export function droppedStatusNote(count: number): string {
  return `${count} progress events were dropped`
}

// ---------------------------------------------------------------------------------------------------------
// Selectors
// ---------------------------------------------------------------------------------------------------------

export function isSealed(record: Pick<TurnRecord, "phase">): boolean {
  return isSealedPhase(record.phase)
}

/** The server echoes client ids (current gateway, REPLv3). False on legacy daemons and before the first connect. */
export function isCorrelated(state: AppState): boolean {
  return state.server?.capabilities.correlation === true
}

/** turn_started / turn_finished per message. */
export function hasLifecycle(state: AppState): boolean {
  return state.server?.capabilities.lifecycle === true
}

const ACTIVE_PHASES: ReadonlySet<TurnPhase> = new Set<TurnPhase>(["pending", "sent", "queued", "running"])
const WAITING_PHASES: ReadonlySet<TurnPhase> = new Set<TurnPhase>(["pending", "sent", "queued"])
const PAUSED_PHASES: ReadonlySet<TurnPhase> = new Set<TurnPhase>([
  "awaiting_approval",
  "awaiting_input",
  "background_running",
])

/** User turns in pending, sent, queued or running: what the local in-flight cap counts (3.6.1). */
export function activeUserTurnCount(state: AppState): number {
  let count = 0
  for (const id of state.inflight) {
    const record = state.turns[id]
    if (record !== undefined && record.origin === "user" && ACTIVE_PHASES.has(record.phase)) count += 1
  }
  return count
}

/** min(8, limits.maxInFlight); paused and detached turns do not count (3.6.1, 3.5 rule 3b). */
export function localInFlightCap(state: AppState): number {
  const server = state.server?.limits.maxInFlight
  const cap = typeof server === "number" && Number.isFinite(server) && server >= 1 ? Math.floor(server) : Infinity
  return Math.min(STATE_BOUNDS.maxLocalInFlight, cap)
}

/** False when the next Enter must be refused with a notice. */
export function canStartTurn(state: AppState): boolean {
  return activeUserTurnCount(state) < localInFlightCap(state)
}

/** "queued #n": 1-based position among this thread's queued user turns, or null. */
export function queuePosition(state: AppState, turnId: TurnId): number | null {
  let position = 0
  for (const id of state.inflight) {
    const record = state.turns[id]
    if (record?.phase === "queued") position += 1
    if (id === turnId) return record?.phase === "queued" ? position : null
  }
  return null
}

/** The head of approvalQueue is the focused prompt. */
export function focusedApproval(state: AppState): ApprovalPrompt | null {
  const id = state.approvalQueue[0]
  return id === undefined ? null : (state.approvals[id] ?? null)
}

/**
 * The prompt that is on screen now, the only one a key, click, command or typed reply may answer (SEC-04,
 * SEC-08): the approval panel shows the queue head unless an info panel covers it (the overlay hides it
 * completely); the full view (Alt+V) shows its own prompt, which counts only while that prompt is still in the
 * queue. Null while another panel covers the approval panel, or while the full view shows a settled prompt.
 */
export function displayedApproval(state: AppState): ApprovalPrompt | null {
  const panel = state.panel
  if (panel === null) return focusedApproval(state)
  if (panel.kind !== "approval" || panel.approvalId === undefined) return null
  const prompt = state.approvals[panel.approvalId]
  return prompt !== undefined && state.approvalQueue.includes(prompt.id) ? prompt : null
}

/**
 * 1-based position of a pending prompt in arrival order (the panel's "2/3"), or null. The queue itself rotates
 * on Alt+J, so it cannot give the position; `approvals` keeps its keys in insertion order (a prompt is added once
 * and later updates keep its key), which is the arrival order.
 */
export function approvalPosition(state: AppState, approvalId: ApprovalId): number | null {
  if (!state.approvalQueue.includes(approvalId)) return null
  const pending = new Set(state.approvalQueue)
  const ordered = Object.keys(state.approvals).filter((id) => pending.has(id))
  const index = ordered.indexOf(approvalId)
  return index < 0 ? null : index + 1
}

export function pendingApprovalsOf(state: AppState, turnId: TurnId): ApprovalPrompt[] {
  const record = state.turns[turnId]
  if (record === undefined) return []
  return record.approvalIds.flatMap((id) => {
    const prompt = state.approvals[id]
    return prompt !== undefined && isOpenPrompt(prompt) ? [prompt] : []
  })
}

function isOpenPrompt(prompt: ApprovalPrompt): boolean {
  return prompt.state === "pending" || prompt.state === "answering" || prompt.state === "failed"
}

// ---------------------------------------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------------------------------------

function cleanText(text: string): string {
  return sanitizeText(text, { multiline: true })
}

function cleanLine(text: string, max = MAX_LINE_TEXT): string {
  const line = sanitizeText(text).trim()
  return line.length > max ? `${line.slice(0, max - 1)}…` : line
}

/**
 * Approval text and URLs the user acts on are escaped, never stripped: stripping an unterminated OSC or DCS
 * sequence also removes everything after it, which would hide parameters from the panel (SEC-04).
 */
function escapedText(text: string): string {
  return sanitizeVisible(text, { multiline: true })
}

function escapedLine(text: string, max = MAX_LINE_TEXT): string {
  const line = sanitizeVisible(text).trim()
  return line.length > max ? `${line.slice(0, max - 1)}…` : line
}

function firstLine(text: string): string {
  const newline = text.indexOf("\n")
  return cleanLine(newline < 0 ? text : text.slice(0, newline), 200)
}

// ---------------------------------------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------------------------------------

/** Ids of records the reducer creates itself contain ":", which a server-echoed id never can (4.1). */
const LOCAL_BACKGROUND_PREFIX = "bg:"
const HISTORY_PREFIX = "hist:"
const IMAGE_PREFIX = "img:"
const NOTICE_PREFIX = "n:"

function localId(prefix: string, seq: number): TurnId {
  return `${prefix}${seq}` as TurnId
}

function blankRecord(id: TurnId, seq: number, origin: TurnOrigin, threadId: ThreadId | null, at: number): TurnRecord {
  return {
    id,
    seq,
    origin,
    threadId,
    input: null,
    phase: "running",
    deltas: "",
    finalText: null,
    activity: [],
    images: [],
    approvalIds: [],
    steerNotes: [],
    notes: [],
    createdAt: at,
  }
}

function putTurn(state: AppState, record: TurnRecord): AppState {
  return { ...state, turns: { ...state.turns, [record.id]: record } }
}

function addRecords(state: AppState, records: readonly TurnRecord[], where: "start" | "end"): AppState {
  if (records.length === 0) return state
  const turns = { ...state.turns }
  for (const record of records) turns[record.id] = record
  const ids = records.map((record) => record.id)
  const order = where === "end" ? [...state.order, ...ids] : [...ids, ...state.order]
  return evictRecords({ ...state, turns, order })
}

function withNote(record: TurnRecord, note: string): TurnRecord {
  if (note.length === 0 || record.notes.includes(note)) return record
  return { ...record, notes: [...record.notes, note].slice(-MAX_NOTES_PER_TURN) }
}

/** An event arrived for the record; the first one ends rule 12's wait, so its "no reply yet" note goes. */
function touched(record: TurnRecord, at: number): TurnRecord {
  const waited = record.firstEventAt === undefined && record.notes.includes(NOTES.noReplyYet)
  const notes = waited ? record.notes.filter((note) => note !== NOTES.noReplyYet) : record.notes
  return { ...record, notes, firstEventAt: record.firstEventAt ?? at, lastEventAt: at }
}

/** Accepted by the daemon (sent or queued here) without any event yet: rule 12's waiting turn. */
function isWaitingSilently(record: TurnRecord): boolean {
  return record.origin === "user" && record.firstEventAt === undefined && (record.phase === "sent" || record.phase === "queued")
}

/** A correlated event arrived: waiting turns start running, paused ones resume unless still blocked. */
function progressed(state: AppState, record: TurnRecord, at: number, explicitStart = false): TurnRecord {
  const next = touched(record, at)
  if (WAITING_PHASES.has(next.phase)) return { ...next, phase: "running" }
  if (PAUSED_PHASES.has(next.phase) && (explicitStart || !isBlocked(state, next))) {
    return { ...next, phase: "running", finish: undefined }
  }
  return next
}

/** A paused turn stays paused while one of its prompts or its credential request is still open. */
function isBlocked(state: AppState, record: TurnRecord): boolean {
  if (state.composer.credential?.turnId === record.id) return true
  return record.approvalIds.some((id) => state.approvals[id]?.state === "pending")
}

function hasPendingPrompt(state: AppState, record: TurnRecord): boolean {
  return record.approvalIds.some((id) => {
    const prompt = state.approvals[id]
    return prompt !== undefined && isOpenPrompt(prompt)
  })
}

function removeFromFlight(state: AppState, id: TurnId): AppState {
  const inInflight = state.inflight.includes(id)
  const inControls = state.controls.includes(id)
  if (!inInflight && !inControls) return state
  return {
    ...state,
    inflight: inInflight ? state.inflight.filter((other) => other !== id) : state.inflight,
    controls: inControls ? state.controls.filter((other) => other !== id) : state.controls,
  }
}

/** Ends a record. Generated-image check (UX-03) included. */
function seal(state: AppState, record: TurnRecord, phase: TurnPhase, at: number, patch: Partial<TurnRecord> = {}): AppState {
  const sealed = withImageToolNote(state, { ...record, ...patch, phase, endedAt: record.endedAt ?? at })
  return removeFromFlight(putTurn(state, sealed), record.id)
}

const IMAGE_TOOL = /(^|[^A-Za-z0-9_])image_(generate|edit)([^A-Za-z0-9_]|$)/

/** 3.7 degradation: an image tool completed but no image arrived before the turn sealed. */
function withImageToolNote(state: AppState, record: TurnRecord): TurnRecord {
  if (state.server?.capabilities.generatedImages === true || record.images.length > 0) return record
  const imageTool = record.activity.some(
    (entry) => entry.kind === "tool_completed" && entry.ok === true && IMAGE_TOOL.test(entry.text),
  )
  return imageTool ? withNote(record, NOTES.imageNotDelivered) : record
}

/** Keeps the newest ui.maxTurns records, evicting the oldest sealed (or history) ones first. */
function evictRecords(state: AppState): AppState {
  const excess = state.order.length - state.ui.maxTurns
  if (excess <= 0) return state
  const evicted = new Set<TurnId>()
  for (const id of state.order) {
    if (evicted.size >= excess) break
    const record = state.turns[id]
    if (record === undefined || isSealed(record) || record.origin === "history") evicted.add(id)
  }
  if (evicted.size === 0) return state
  const turns = { ...state.turns }
  const approvals = { ...state.approvals }
  for (const id of evicted) {
    for (const approvalId of turns[id]?.approvalIds ?? []) {
      const prompt = approvals[approvalId]
      if (prompt !== undefined && !isOpenPrompt(prompt)) delete approvals[approvalId]
    }
    delete turns[id]
  }
  return {
    ...state,
    turns,
    approvals,
    order: state.order.filter((id) => !evicted.has(id)),
    inflight: state.inflight.filter((id) => !evicted.has(id)),
    controls: state.controls.filter((id) => !evicted.has(id)),
  }
}

/** Only the newest STATE_BOUNDS.maxImagePayloads images (input and generated) keep their bytes. */
function enforceImagePayloads(state: AppState): AppState {
  let kept = 0
  let turns: Record<TurnId, TurnRecord> | null = null
  for (let index = state.order.length - 1; index >= 0; index -= 1) {
    const id = state.order[index]
    const record = id === undefined ? undefined : state.turns[id]
    if (record === undefined) continue
    const [images, keptAfterGenerated] = trimImages(record.images, kept)
    const inputImages = record.input?.images ?? []
    const [input, keptAfterInput] = trimImages(inputImages, keptAfterGenerated)
    kept = keptAfterInput
    if (images !== record.images || input !== inputImages) {
      turns ??= { ...state.turns }
      turns[record.id] = { ...record, images, input: record.input === null ? null : { ...record.input, images: input } }
    }
  }
  return turns === null ? state : { ...state, turns }
}

/** Newest last within a list; returns the same array when nothing changes. */
function trimImages(images: TranscriptImage[], keptBefore: number): [TranscriptImage[], number] {
  let kept = keptBefore
  let changed = false
  const next = images.slice()
  for (let index = next.length - 1; index >= 0; index -= 1) {
    const image = next[index]
    if (image === undefined || image.bytes === null) continue
    if (kept < STATE_BOUNDS.maxImagePayloads) {
      kept += 1
    } else {
      next[index] = { ...image, bytes: null }
      changed = true
    }
  }
  return [changed ? next : images, kept]
}

// ---------------------------------------------------------------------------------------------------------
// Notices and composer history (shared with reducer.ts)
// ---------------------------------------------------------------------------------------------------------

/** info 6 s, warn 12 s, errors sticky until Esc or a newer error; at most 20 kept (3.6.3). */
export function pushNotice(state: AppState, input: NoticeInput, at: number): AppState {
  const text = cleanLine(input.text)
  if (text.length === 0) return state
  const seq = state.seq + 1
  const id = input.id ?? `${NOTICE_PREFIX}${seq}`
  const sticky = input.sticky ?? input.level === "error"
  const lifetime = input.level === "info" ? STATE_BOUNDS.noticeInfoMs : STATE_BOUNDS.noticeWarnMs
  const kept = state.notices.filter(
    (notice) => notice.id !== id && !(input.level === "error" && notice.level === "error" && notice.sticky),
  )
  const notice = { id, level: input.level, source: input.source, text, sticky, expiresAt: sticky ? null : at + lifetime, at }
  return { ...state, seq, notices: [...kept, notice].slice(-STATE_BOUNDS.maxNotices) }
}

/** Sent texts and cleared drafts: in memory only, newest last, at most 50, no consecutive duplicates. */
export function pushComposerHistory(composer: ComposerState, text: string): ComposerState {
  if (text.trim().length === 0 || composer.history[composer.history.length - 1] === text) return composer
  return { ...composer, history: [...composer.history, text].slice(-STATE_BOUNDS.maxComposerHistory) }
}

// ---------------------------------------------------------------------------------------------------------
// Transport events
// ---------------------------------------------------------------------------------------------------------

export function applyEvent(state: AppState, event: TransportEvent, at: number): AppState {
  if (isOtherThreadEvent(state, event)) return applyOtherThreadEvent(state, event, at)
  switch (event.kind) {
    case "turn.accepted":
      return onAccepted(state, event, at)
    case "turn.started":
      return onStarted(state, event, at)
    case "turn.delta":
      return onDelta(state, event, at)
    case "turn.reasoning":
      return onReasoning(state, event, at)
    case "turn.activity":
      return onActivity(state, event, at)
    case "turn.image":
      return onImage(state, event, at)
    case "turn.approval":
      return onApproval(state, event, at)
    case "turn.approval_resolved":
      return onApprovalResolved(state, event.approvalId, event.resolution, at)
    case "turn.auth_required":
      return onAuthRequired(state, event, at)
    case "turn.final":
      return onFinal(state, event, at)
    case "turn.finished":
      return onFinished(state, event, at)
    case "turn.error":
      return onError(state, event, at)
    case "turn.cost":
      return onCost(state, event)
    case "turn.suggestions":
      return onSuggestions(state, event)
    case "notice":
      return pushNotice(state, { level: event.level, source: event.source, text: event.text }, at)
    case "background":
      return addBackground(state, cleanText(event.text), event.threadId ?? null, at)
    case "other_thread":
      return event.threadId === state.threadId ? state : bumpOtherThreads(state)
    case "turn.settle_hint":
    case "resync.required":
      return state
    default:
      return assertNever(event)
  }
}

function assertNever(value: never): never {
  throw new Error(`unhandled transport event ${JSON.stringify(value)}`)
}

function isOtherThreadEvent(state: AppState, event: TransportEvent): boolean {
  if (event.kind === "other_thread" || state.threadId === null || !("threadId" in event)) return false
  return event.threadId !== undefined && event.threadId !== state.threadId
}

function bumpOtherThreads(state: AppState): AppState {
  return { ...state, otherThreadsUnread: state.otherThreadsUnread + 1 }
}

/** Rule 1: replies, errors, approvals and credential requests of other threads count; nothing else changes. */
function applyOtherThreadEvent(state: AppState, event: TransportEvent, at: number): AppState {
  switch (event.kind) {
    case "turn.approval": {
      const notice = {
        level: "warn",
        source: "server",
        text: NOTES.otherThreadApproval,
        id: NOTICE_IDS.otherThreadApproval,
      } as const
      return bumpOtherThreads(pushNotice(state, notice, at))
    }
    case "turn.auth_required": {
      const notice = { level: "warn", source: "auth", text: NOTES.otherThreadAuth, id: NOTICE_IDS.otherThreadAuth } as const
      return bumpOtherThreads(pushNotice(state, notice, at))
    }
    case "turn.final":
    case "turn.error":
    case "background":
      return bumpOtherThreads(state)
    default:
      return state
  }
}

function onAccepted(state: AppState, event: TransportEventOf<"turn.accepted">, at: number): AppState {
  const record = state.turns[event.turnId]
  if (record === undefined || (record.phase !== "pending" && record.phase !== "sent")) return state
  return putTurn(state, { ...record, phase: "queued", sentAt: record.sentAt ?? at })
}

function onStarted(state: AppState, event: TransportEventOf<"turn.started">, at: number): AppState {
  const record = event.turnId === null ? undefined : state.turns[event.turnId]
  if (record === undefined || isSealed(record)) return state
  return putTurn(state, progressed(state, record, at, true))
}

/**
 * Rules 5c, 7 and 9 (legacy, uncorrelated): the oldest inflight turn that can receive output. A turn paused at
 * an open prompt or at its credential request gets nothing until it is answered, while a message sent meanwhile
 * is answered at once (V2 "This thread is waiting on a pending gate...", V1 "Waiting for approval: ..."), so the
 * paused turn is skipped; when every inflight turn is paused the oldest one is used.
 */
function legacyTarget(state: AppState): TurnRecord | undefined {
  let oldest: TurnRecord | undefined
  for (const id of state.inflight) {
    const record = state.turns[id]
    if (record === undefined) continue
    oldest ??= record
    if (!isBlocked(state, record)) return record
  }
  return oldest ?? openHistoryRecord(state)
}

/**
 * With nothing inflight, the newest history record of this thread that was seeded while the server was still
 * running it (V1 Processing, a young V2 Failed): its live output belongs to it, not to a background record.
 */
function openHistoryRecord(state: AppState): TurnRecord | undefined {
  for (let index = state.order.length - 1; index >= 0; index -= 1) {
    const id = state.order[index]
    const record = id === undefined ? undefined : state.turns[id]
    if (record?.origin === "history" && !isSealed(record) && record.threadId === state.threadId) return record
  }
  return undefined
}

/** Rules 2 and 7 for progress without content (activity, cost, suggestions): never creates a record. */
function progressTarget(state: AppState, turnId: TurnId | null): TurnRecord | undefined {
  if (turnId !== null) return state.turns[turnId]
  if (isCorrelated(state)) return undefined
  return legacyTarget(state)
}

/**
 * Rules 2, 6 and 7 for content (deltas, images): an exact match; on legacy daemons the oldest inflight turn;
 * otherwise an open background record. An id this window never created (another client's turn) gets a
 * background record of its own, so its later events stay together.
 */
function contentTarget(
  state: AppState,
  turnId: TurnId | null,
  threadId: ThreadId | null,
  at: number,
): { state: AppState; record: TurnRecord } {
  if (turnId !== null) {
    const known = state.turns[turnId]
    if (known !== undefined) return { state, record: known }
    return openRecord(state, turnId, threadId, at)
  }
  if (!isCorrelated(state)) {
    const record = legacyTarget(state)
    if (record !== undefined) return { state, record }
  }
  const open = findOpenBackground(state)
  return open !== undefined ? { state, record: open } : openRecord(state, null, threadId, at)
}

function openRecord(
  state: AppState,
  turnId: TurnId | null,
  threadId: ThreadId | null,
  at: number,
): { state: AppState; record: TurnRecord } {
  const seq = state.seq + 1
  const id = turnId ?? localId(LOCAL_BACKGROUND_PREFIX, seq)
  const record = blankRecord(id, seq, "background", threadId ?? state.threadId, at)
  return { state: addRecords({ ...state, seq }, [record], "end"), record }
}

/** The newest unsealed background record created for uncorrelated content (rules 6 and 7). */
function findOpenBackground(state: AppState): TurnRecord | undefined {
  for (let index = state.order.length - 1; index >= 0; index -= 1) {
    const id = state.order[index]
    const record = id === undefined ? undefined : state.turns[id]
    if (record?.origin === "background" && !isSealed(record) && record.id.startsWith(LOCAL_BACKGROUND_PREFIX)) {
      return record
    }
  }
  return undefined
}

/** A legacy gateway reports streaming "unknown" until the first chunk (3.7). */
function learnStreaming(state: AppState): AppState {
  const server = state.server
  if (server === null || server.capabilities.streaming !== "unknown") return state
  return { ...state, server: { ...server, capabilities: { ...server.capabilities, streaming: true } } }
}

function learnGeneratedImages(state: AppState): AppState {
  const server = state.server
  if (server === null || server.capabilities.generatedImages !== "unknown") return state
  return { ...state, server: { ...server, capabilities: { ...server.capabilities, generatedImages: true } } }
}

function onDelta(state: AppState, event: TransportEventOf<"turn.delta">, at: number): AppState {
  const text = cleanText(event.text)
  if (text.length === 0) return state
  const learned = learnStreaming(state)
  const known = event.turnId === null ? undefined : learned.turns[event.turnId]
  if (known !== undefined && isSealed(known)) return learned // rule 3: late deltas are dropped
  const target = contentTarget(learned, event.turnId, event.threadId ?? null, at)
  const result = appendDelta(turnTextOf(target.record), text)
  if (result.status === "rejected" || result.text === undefined) return learned
  let record: TurnRecord = { ...target.record, deltas: result.text.deltas }
  if (result.status === "cut") record = withNote(record, NOTES.deltaCut)
  return putTurn(target.state, progressed(target.state, record, at))
}

function onReasoning(state: AppState, event: TransportEventOf<"turn.reasoning">, at: number): AppState {
  const record = progressTarget(state, event.turnId)
  if (record === undefined || isSealed(record)) return state
  // Sanitize before cutting: a control sequence must not be split into visible fragments by the byte cap.
  const reasoning = tailToUtf8Bytes(cleanText(event.text), STATE_BOUNDS.maxReasoningBytesPerTurn)
  if (reasoning.trim().length === 0) return state
  return putTurn(state, progressed(state, { ...record, reasoning }, at))
}

function onActivity(state: AppState, event: TransportEventOf<"turn.activity">, at: number): AppState {
  const record = progressTarget(state, event.turnId)
  if (record === undefined || isSealed(record)) return state
  const text = cleanLine(event.activity.text, MAX_ACTIVITY_TEXT)
  if (text.length === 0) return state
  const entry: Activity = {
    kind: event.activity.kind,
    text,
    at,
    ...(event.activity.ok === undefined ? {} : { ok: event.activity.ok }),
  }
  const activity = [...record.activity, entry].slice(-STATE_BOUNDS.maxActivityPerTurn)
  return putTurn(state, progressed(state, { ...record, activity }, at))
}

/** Images are content: a late one still attaches to its (sealed) turn rather than being lost. */
function onImage(state: AppState, event: TransportEventOf<"turn.image">, at: number): AppState {
  const { mime, bytes } = event.image
  if (!isImageMime(mime) || !(bytes instanceof Uint8Array) || bytes.byteLength === 0) return state
  const target = contentTarget(learnGeneratedImages(state), event.turnId, event.threadId ?? null, at)
  const seq = target.state.seq + 1
  const caption = event.image.caption === undefined ? "" : cleanLine(event.image.caption, 256)
  const image: TranscriptImage = {
    id: `${IMAGE_PREFIX}${seq}`,
    mime,
    bytes,
    byteLength: bytes.byteLength,
    ...(caption.length > 0 ? { caption } : {}),
  }
  const base: TurnRecord = {
    ...target.record,
    images: [...target.record.images, image],
    notes: target.record.notes.filter((note) => note !== NOTES.imageNotDelivered),
  }
  const next = { ...target.state, seq }
  const record = isSealed(base) ? touched(base, at) : progressed(next, base, at)
  return enforceImagePayloads(putTurn(next, record))
}

function onCost(state: AppState, event: TransportEventOf<"turn.cost">): AppState {
  const record = progressTarget(state, event.turnId)
  if (record === undefined) return state
  const cost = { inputTokens: event.inputTokens, outputTokens: event.outputTokens, costUsd: cleanLine(event.costUsd, 32) }
  return putTurn(state, { ...record, cost })
}

function onSuggestions(state: AppState, event: TransportEventOf<"turn.suggestions">): AppState {
  const record = progressTarget(state, event.turnId)
  if (record === undefined) return state
  const suggestions = event.suggestions
    .map((text) => cleanLine(text, 200))
    .filter((text) => text.length > 0)
    .slice(0, MAX_SUGGESTIONS)
  return putTurn(state, { ...record, suggestions })
}

// ---- finals ----

function onFinal(state: AppState, event: TransportEventOf<"turn.final">, at: number): AppState {
  const text = cleanText(event.text)
  if (event.turnId !== null) {
    const record = state.turns[event.turnId]
    if (record === undefined) return foreignFinal(state, event.turnId, event.threadId ?? null, text, at)
    if (record.origin === "control") return ackControl(state, record, text, at)
    if (record.origin === "background") return sealWithFinal(state, record, text, at)
    return userFinal(state, record, text, at)
  }
  if (!isCorrelated(state)) return legacyFinal(state, text, at)
  return backgroundFinal(state, text, event.threadId ?? null, at)
}

/** Rules 2-4 for a user (or history) record. */
function userFinal(state: AppState, record: TurnRecord, text: string, at: number): AppState {
  const result = applyFinal(turnTextOf(record), text)
  if (isSealed(record)) {
    if (!result.changed || result.text === undefined) return state
    return putTurn(state, withNote(touched({ ...record, finalText: result.text.finalText }, at), NOTES.late))
  }
  const updated = progressed(state, { ...record, finalText: result.text?.finalText ?? record.finalText }, at)
  return hasLifecycle(state) ? putTurn(state, updated) : seal(state, updated, "done", at)
}

function sealWithFinal(state: AppState, record: TurnRecord, text: string, at: number): AppState {
  const result = applyFinal(turnTextOf(record), text)
  if (isSealed(record)) {
    return result.changed && result.text !== undefined ? putTurn(state, { ...record, finalText: result.text.finalText }) : state
  }
  return seal(state, touched({ ...record, finalText: result.text?.finalText ?? record.finalText }, at), "done", at)
}

/** Another client's turn on this thread: shown as a background record under its own id. */
function foreignFinal(state: AppState, turnId: TurnId, threadId: ThreadId | null, text: string, at: number): AppState {
  if (text.length === 0) return state
  const opened = openRecord(state, turnId, threadId, at)
  return sealWithFinal(opened.state, opened.record, text, at)
}

/** Rules 5d and 6: seal the open background record, or add a new one. */
function backgroundFinal(state: AppState, text: string, threadId: ThreadId | null, at: number): AppState {
  const open = findOpenBackground(state)
  if (open !== undefined) return sealWithFinal(state, open, text, at)
  return addBackground(state, text, threadId, at)
}

function addBackground(state: AppState, text: string, threadId: ThreadId | null, at: number): AppState {
  if (text.length === 0) return state
  const seq = state.seq + 1
  const record: TurnRecord = {
    ...blankRecord(localId(LOCAL_BACKGROUND_PREFIX, seq), seq, "background", threadId ?? state.threadId, at),
    phase: "done",
    finalText: text,
    endedAt: at,
  }
  return addRecords({ ...state, seq }, [record], "end")
}

/** Rule 5 (legacy, uncorrelated). */
function legacyFinal(state: AppState, text: string, at: number): AppState {
  const kind = state.controls.length > 0 ? controlAckKind(text) : null
  const control = kind === null ? undefined : oldestControl(state, kind)
  if (control !== undefined) {
    let acked = state
    if (isInterruptedAck(text)) acked = interruptOldestTurn(state, control, at)
    else if (text.trim() === NOTHING_TO_INTERRUPT_ACK) acked = cancelWaitingTurns(state, control, at)
    return ackControl(acked, control, text, at)
  }
  const stray = state.strayInterruptAck
  if (stray !== null && stray.until > at && isInterruptedAck(text)) return { ...state, strayInterruptAck: null }
  const record = legacyTarget(state)
  if (record !== undefined) return sealWithFinal(state, record, text, at)
  return backgroundFinal(state, text, null, at)
}

/** The oldest unsealed control answered by an ack of `kind` (a pairing by kind, so a steer never eats an interrupt ack). */
function oldestControl(state: AppState, kind: ControlKind): TurnRecord | undefined {
  for (const id of state.controls) {
    const record = state.turns[id]
    if (record !== undefined && record.control?.kind === kind && !isSealed(record)) return record
  }
  return undefined
}

/** The unsealed user turns created before `control`, oldest first. */
function userTurnsBefore(state: AppState, control: TurnRecord): TurnRecord[] {
  return state.inflight.flatMap((id) => {
    const record = state.turns[id]
    return record !== undefined && record.origin === "user" && record.seq < control.seq ? [record] : []
  })
}

/**
 * Rule 5a, "Interrupted.": the daemon stopped the turn it was running, the oldest running user turn created before
 * the interrupt (else the oldest that saw any event), which ends interrupted with its deltas kept; a turn behind
 * it that is only queued still runs. When none of them saw an event, the daemon was only holding them and dropped
 * them all: each ends "cancelled before it started". Unless the engine is V2, V1 also answers a stopped turn with
 * "Interrupted.": arm the stray-ack window. A transport without progress events (legacy socket) cannot tell a
 * running turn from a waiting one, so there the oldest turn is the one stopped.
 */
function interruptOldestTurn(state: AppState, control: TurnRecord, at: number): AppState {
  const before = userTurnsBefore(state, control)
  const silent = state.server?.capabilities.activity !== true
  const target = silent
    ? before[0]
    : (before.find((record) => record.phase === "running") ?? before.find((record) => record.firstEventAt !== undefined))
  let next: AppState
  let armStray: boolean
  if (target !== undefined) {
    armStray = !PAUSED_PHASES.has(target.phase)
    next = seal(state, target, "interrupted", at)
  } else {
    const waiting = before.filter(isWaitingSilently)
    if (waiting.length === 0) return state
    armStray = true
    next = waiting.reduce((acc, record) => seal(acc, withNote(dropNote(record, NOTES.noReplyYet), NOTES.cancelled), "interrupted", at), state)
  }
  return armStray && state.server?.engine !== "v2" ? { ...next, strayInterruptAck: { until: at + STRAY_INTERRUPT_WINDOW_MS } } : next
}

/**
 * Rule 5a, "Nothing to interrupt." (legacy gateway): the daemon had nothing running or queued for the thread, so
 * the turns created before the interrupt that never saw an event will get no reply. They end unanswered: this is
 * how the user cancels a turn that rule 12 keeps waiting. Not on transports without progress events.
 */
function cancelWaitingTurns(state: AppState, control: TurnRecord, at: number): AppState {
  if (state.server?.capabilities.activity !== true) return state
  return userTurnsBefore(state, control)
    .filter(isWaitingSilently)
    .reduce((acc, record) => seal(acc, withNote(dropNote(record, NOTES.noReplyYet), NOTES.nothingToInterrupt), "unanswered", at), state)
}

function dropNote(record: TurnRecord, note: string): TurnRecord {
  return record.notes.includes(note) ? { ...record, notes: record.notes.filter((other) => other !== note) } : record
}

/** A control's reply: it becomes the ack, the control is sealed, and rule 3b may end paused turns. */
function ackControl(state: AppState, record: TurnRecord, text: string, at: number, failure?: { code: string }): AppState {
  if (record.control === undefined) return state
  if (record.control.ack !== undefined || (text.length === 0 && isSealed(record))) return state
  const ack = text.trim()
  const updated: TurnRecord = touched(
    {
      ...record,
      control: { ...record.control, ...(ack.length > 0 ? { ack } : {}) },
      finalText: record.finalText ?? (ack.length > 0 ? ack : null),
      ...(failure === undefined ? {} : { error: { code: failure.code, message: ack } }),
    },
    at,
  )
  let next = isSealed(record) ? putTurn(state, updated) : seal(state, updated, failure === undefined ? "done" : "failed", at)
  if (record.control.kind === "steer" && ack.length > 0) next = ackSteerNote(next, record, ack)
  if (failure === undefined && ack.length > 0) next = endPausedTurnsAfterControl(next, updated, ack, at)
  return next
}

/** Fills the ack of the steer note this control added to a running turn. */
function ackSteerNote(state: AppState, control: TurnRecord, ack: string): AppState {
  const note = steerNoteText(control.control?.text ?? "")
  for (let index = state.order.length - 1; index >= 0; index -= 1) {
    const id = state.order[index]
    const record = id === undefined ? undefined : state.turns[id]
    if (record === undefined || record.origin !== "user") continue
    const at = record.steerNotes.findIndex((entry) => entry.text === note && entry.ack === undefined)
    if (at < 0) continue
    const steerNotes = record.steerNotes.map((entry, index2) => (index2 === at ? { ...entry, ack } : entry))
    return putTurn(state, { ...record, steerNotes })
  }
  return state
}

export function steerNoteText(controlText: string): string {
  const text = controlText.trim()
  return /^\/steer\s/i.test(text) ? text.replace(/^\/steer\s+/i, "") : text
}

// ---- lifecycle ----

function onFinished(state: AppState, event: TransportEventOf<"turn.finished">, at: number): AppState {
  const record = state.turns[event.turnId]
  if (record === undefined || isSealed(record)) return state
  const dropped = event.droppedStatus ?? 0
  const finish = { outcome: event.outcome, at, ...(dropped > 0 ? { droppedStatus: dropped } : {}) }
  let updated = touched({ ...record, finish }, at)
  if (dropped > 0) updated = withNote(updated, droppedStatusNote(dropped))
  if (record.origin === "control" || record.origin === "background") {
    const failed = event.outcome === "failed" || event.outcome === "timed_out"
    return seal(state, updated, failed ? "failed" : "done", at)
  }
  return finishUserTurn(state, updated, event.outcome, at)
}

/** The 3.5 rule 3 table. */
function finishUserTurn(state: AppState, record: TurnRecord, outcome: FinishOutcome, at: number): AppState {
  const pending = hasPendingPrompt(state, record)
  switch (outcome) {
    case "replied":
    case "unknown":
      return seal(state, record, "done", at)
    case "no_reply":
      return pending
        ? putTurn(state, { ...record, phase: "awaiting_approval" })
        : seal(state, withNote(record, NOTES.noReply), "done", at)
    case "paused":
      return putTurn(state, { ...record, phase: pending ? "awaiting_approval" : "awaiting_input" })
    case "interrupted":
      return seal(state, record, "interrupted", at)
    case "detached":
      return putTurn(state, { ...record, phase: "background_running" })
    case "failed":
    case "timed_out":
      return seal(state, record, "failed", at, { error: record.error ?? failureOf(record, outcome) })
    case "cancelled":
      return seal(state, withNote(record, NOTES.cancelled), "interrupted", at)
  }
}

function failureOf(record: TurnRecord, outcome: "failed" | "timed_out"): { code: string; message: string } {
  const fallback = outcome === "timed_out" ? "timed out" : "failed"
  return { code: outcome, message: record.finalText ?? fallback }
}

// ---- errors ----

function onError(state: AppState, event: TransportEventOf<"turn.error">, at: number): AppState {
  const message = cleanText(event.message).trim() || "error"
  const code = cleanLine(event.code, 64) || "error"
  if (event.turnId !== null) {
    const record = state.turns[event.turnId]
    if (record === undefined) return state
    if (record.origin === "control") return failControlRecord(state, record, code, message, at)
    if (isSealed(record)) return putTurn(state, withNote(record, `error: ${firstLine(message)}`))
    return seal(state, touched(record, at), "failed", at, { error: { code, message } })
  }
  if (!isCorrelated(state)) {
    const kind = state.controls.length > 0 ? controlAckKind(message) : null
    const control = kind === null ? undefined : oldestControl(state, kind)
    if (control !== undefined) return ackControl(state, control, message, at, { code })
    const record = legacyTarget(state)
    if (record !== undefined) return seal(state, touched(record, at), "failed", at, { error: { code, message } })
  }
  return pushNotice(state, { level: "error", source: "server", text: firstLine(message) }, at)
}

function failControlRecord(state: AppState, record: TurnRecord, code: string, message: string, at: number): AppState {
  if (isSealed(record)) return putTurn(state, withNote(record, `error: ${firstLine(message)}`))
  return seal(state, touched(record, at), "failed", at, { error: { code, message } })
}

// ---------------------------------------------------------------------------------------------------------
// Approvals (3.6.2)
// ---------------------------------------------------------------------------------------------------------

function sanitizeApproval(wire: WireApproval): WireApproval {
  return {
    ...wire,
    toolName: escapedLine(wire.toolName, 256),
    description: escapedText(wire.description),
    parameters: escapedText(wire.parameters),
  }
}

/** Rule 8 and its correlated counterpart for prompts that arrive without a turn id. */
function approvalOwner(state: AppState, turnId: TurnId | null): TurnId | null {
  if (turnId !== null) return turnId
  if (!isCorrelated(state)) return state.inflight[0] ?? null
  return uniquePausedTurn(state)
}

/** The one turn paused at a gate without a prompt (not the credential owner); null when ambiguous. */
function uniquePausedTurn(state: AppState): TurnId | null {
  const candidates = state.inflight.filter((id) => {
    const record = state.turns[id]
    return (
      record !== undefined &&
      record.phase === "awaiting_input" &&
      state.composer.credential?.turnId !== id &&
      !hasPendingPrompt(state, record)
    )
  })
  return candidates.length === 1 ? (candidates[0] ?? null) : null
}

function onApproval(state: AppState, event: TransportEventOf<"turn.approval">, at: number): AppState {
  return upsertApproval(state, event.approval, event.turnId, event.threadId ?? state.threadId, at)
}

/** Deduplicated by request_id across live, history and pending-gates sources. */
function upsertApproval(
  state: AppState,
  raw: WireApproval,
  turnId: TurnId | null,
  threadId: ThreadId | null,
  at: number,
): AppState {
  if (typeof raw.id !== "string" || raw.id.length === 0) return state
  const wire = sanitizeApproval(raw)
  const owner = approvalOwner(state, turnId)
  const existing = state.approvals[wire.id]
  if (existing !== undefined) {
    if (!isOpenPrompt(existing)) return state
    // history.pending_approval carries no allow_always (false means unknown); a copy that has it wins.
    const upgraded = wire.allowAlways && !existing.allowAlways ? { ...existing, allowAlways: true } : existing
    const next = upgraded === existing ? state : { ...state, approvals: { ...state.approvals, [wire.id]: upgraded } }
    if (upgraded.turnId !== null || owner === null) return next
    const approvals = { ...next.approvals, [wire.id]: { ...upgraded, turnId: owner } }
    return linkPromptToTurn({ ...next, approvals }, wire.id, owner, at)
  }
  const prompt: ApprovalPrompt = {
    ...wire,
    turnId: owner,
    threadId,
    state: "pending",
    createdAt: at,
    focusedAt: state.approvalQueue.length === 0 ? at : null,
    viewedFull: false,
  }
  const next = pruneSettledApprovals({
    ...state,
    approvals: { ...state.approvals, [wire.id]: prompt },
    approvalQueue: [...state.approvalQueue, wire.id],
  })
  return owner === null ? next : linkPromptToTurn(next, wire.id, owner, at)
}

/** Adds the prompt to its turn; a paused turn (or any turn on a daemon without lifecycle) now awaits approval. */
function linkPromptToTurn(state: AppState, approvalId: ApprovalId, turnId: TurnId, at: number): AppState {
  const record = state.turns[turnId]
  if (record === undefined) return state
  const approvalIds = record.approvalIds.includes(approvalId) ? record.approvalIds : [...record.approvalIds, approvalId]
  let updated: TurnRecord = { ...record, approvalIds }
  const paused = record.phase === "awaiting_input" || record.phase === "background_running"
  if (!isSealed(record) && (paused || (!hasLifecycle(state) && record.origin === "user"))) {
    updated = { ...touched(updated, at), phase: "awaiting_approval" }
  }
  return putTurn(state, updated)
}

function pruneSettledApprovals(state: AppState): AppState {
  const ids = Object.keys(state.approvals)
  if (ids.length <= MAX_SETTLED_APPROVALS + state.approvalQueue.length) return state
  const settled = ids
    .map((id) => state.approvals[id])
    .filter((prompt): prompt is ApprovalPrompt => prompt !== undefined && !isOpenPrompt(prompt))
    .sort((a, b) => a.createdAt - b.createdAt)
  const excess = settled.length - MAX_SETTLED_APPROVALS
  if (excess <= 0) return state
  const approvals = { ...state.approvals }
  for (const prompt of settled.slice(0, excess)) delete approvals[prompt.id]
  return { ...state, approvals }
}

/** Takes a prompt out of the queue; the next head becomes focused now (its 1 s window starts). */
function settlePrompt(
  state: AppState,
  prompt: ApprovalPrompt,
  settled: ApprovalPrompt["state"],
  at: number,
  patch: Partial<ApprovalPrompt> = {},
): AppState {
  const wasHead = state.approvalQueue[0] === prompt.id
  const approvalQueue = state.approvalQueue.filter((id) => id !== prompt.id)
  const approvals: Record<ApprovalId, ApprovalPrompt> = {
    ...state.approvals,
    [prompt.id]: { ...prompt, ...patch, state: settled },
  }
  const head = approvalQueue[0]
  const headPrompt = head === undefined ? undefined : approvals[head]
  if (wasHead && head !== undefined && headPrompt !== undefined) approvals[head] = { ...headPrompt, focusedAt: at }
  const confirm = state.composer.confirm?.approvalId === prompt.id ? null : state.composer.confirm
  const composer = confirm === state.composer.confirm ? state.composer : { ...state.composer, confirm }
  return { ...state, approvals, approvalQueue, composer }
}

const RESUMING_RESOLUTIONS: ReadonlySet<ApprovalResolution> = new Set<ApprovalResolution>([
  "approve",
  "always",
  "deny",
  "credential",
  "external",
])

function resolutionNote(resolution: ApprovalResolution): string | null {
  if (resolution === "cancelled") return NOTES.approvalCancelled
  if (resolution === "terminated") return NOTES.threadTerminated
  if (resolution === "expired") return NOTES.approvalExpired
  return null
}

/** gate_resolved / ApprovalResolved: removes the prompt; rule 3b seals or resumes its paused turn. */
function onApprovalResolved(state: AppState, approvalId: ApprovalId, resolution: ApprovalResolution, at: number): AppState {
  const prompt = state.approvals[approvalId]
  if (prompt === undefined) return credentialGateResolved(state, approvalId, resolution, at)
  if (!isOpenPrompt(prompt)) return state
  const decision = resolution === "approve" || resolution === "always" || resolution === "deny" ? resolution : prompt.decision
  const settled = resolution === "expired" ? "expired" : "resolved"
  let next = settlePrompt(state, prompt, settled, at, decision === undefined ? {} : { decision })
  const record = prompt.turnId === null ? undefined : next.turns[prompt.turnId]
  if (record === undefined || isSealed(record)) return next
  const note = resolutionNote(resolution)
  if (note !== null) {
    const paused = record.phase === "awaiting_approval" || record.phase === "awaiting_input"
    return paused ? endPausedTurn(next, record, note, at) : putTurn(next, withNote(record, note))
  }
  if (RESUMING_RESOLUTIONS.has(resolution) && record.phase === "awaiting_approval" && !hasPendingPrompt(next, record)) {
    next = putTurn(next, { ...touched(record, at), phase: "running", finish: undefined })
  }
  return next
}

/** A V2 authentication gate resolved: credential mode ends without sending (3.6.4). */
function credentialGateResolved(state: AppState, requestId: string, resolution: ApprovalResolution, at: number): AppState {
  const credential = state.composer.credential
  if (credential === null || credential.requestId !== requestId) return state
  let next = leaveCredentialMode(state, at)
  const record = credential.turnId === null ? undefined : next.turns[credential.turnId]
  if (record === undefined || isSealed(record)) return next
  const note = resolutionNote(resolution)
  if (note !== null) return endPausedTurn(next, record, note, at)
  if (RESUMING_RESOLUTIONS.has(resolution) && PAUSED_PHASES.has(record.phase)) {
    next = putTurn(next, { ...touched(record, at), phase: "running", finish: undefined })
  }
  return next
}

function leaveCredentialMode(state: AppState, at: number): AppState {
  const cleared = { ...state, composer: { ...state.composer, credential: null } }
  return pushNotice(cleared, { level: "warn", source: "auth", text: NOTES.credentialEnded, id: NOTICE_IDS.credentialEnded }, at)
}

/** Rule 3b: a paused turn is sealed interrupted, its open prompts dropped, with a note naming the cause. */
function endPausedTurn(state: AppState, record: TurnRecord, note: string, at: number): AppState {
  let next = state
  for (const id of record.approvalIds) {
    const prompt = next.approvals[id]
    if (prompt !== undefined && isOpenPrompt(prompt)) next = settlePrompt(next, prompt, "expired", at)
  }
  const current = next.turns[record.id] ?? record
  return seal(next, withNote(current, note), "interrupted", at)
}

/** Rule 3b, second bullet (and the legacy rule 5a ack): interrupt, /clear or /new ended the thread's gates. */
function endPausedTurnsAfterControl(state: AppState, control: TurnRecord, ack: string, at: number): AppState {
  const kind = control.control?.kind
  if (kind !== "interrupt" && kind !== "clear" && kind !== "new") return state
  if (ack === NOTHING_TO_INTERRUPT_ACK) return state
  const thread = control.threadId
  let next = state
  for (const id of state.inflight) {
    const record = next.turns[id]
    if (record === undefined || record.threadId !== thread) continue
    if (record.phase !== "awaiting_approval" && record.phase !== "awaiting_input") continue
    const waitingFor = record.phase === "awaiting_approval" ? "approval" : "input"
    const cause = kind === "interrupt" ? "interrupted" : kind === "clear" ? "conversation cleared" : "new conversation started"
    next = endPausedTurn(next, record, `${cause} while waiting for ${waitingFor}`, at)
    if (state.composer.credential?.turnId === id && next.composer.credential !== null) next = leaveCredentialMode(next, at)
  }
  // The scope's gates are gone (V2 discards them without gate_resolved): drop the thread's remaining prompts.
  for (const id of next.approvalQueue) {
    const prompt = next.approvals[id]
    const inThread = prompt !== undefined && (prompt.threadId === thread || prompt.threadId === null)
    if (inThread) next = settlePrompt(next, prompt, "expired", at)
  }
  return next
}

// ---- approval actions ----

export function approvalAnswering(state: AppState, action: ActionOf<"approval/answering">): AppState {
  const prompt = state.approvals[action.approvalId]
  if (prompt === undefined || (prompt.state !== "pending" && prompt.state !== "failed")) return state
  const approvals = { ...state.approvals, [prompt.id]: { ...prompt, state: "answering" as const, decision: action.decision } }
  const confirm = state.composer.confirm?.approvalId === prompt.id ? null : state.composer.confirm
  return { ...state, approvals, composer: confirm === state.composer.confirm ? state.composer : { ...state.composer, confirm } }
}

/** 202 / Ack resolves; 404 or unknown_request expires; any other failure leaves the prompt answerable. */
export function approvalAnswered(state: AppState, action: ActionOf<"approval/answered">): AppState {
  const prompt = state.approvals[action.approvalId]
  if (prompt === undefined || !isOpenPrompt(prompt)) return state
  const { at } = action
  if (action.ok) return settlePrompt(state, prompt, "resolved", at)
  if (action.expired) {
    let next = settlePrompt(state, prompt, "expired", at)
    const record = prompt.turnId === null ? undefined : next.turns[prompt.turnId]
    // Rule 3b, as for gate_resolved expired: the gate is gone (404), so a turn paused only at it never resumes and
    // would otherwise take the next message's reply (legacy FIFO). A later gate_resolved finds the prompt settled.
    const paused = record !== undefined && (record.phase === "awaiting_approval" || record.phase === "awaiting_input")
    if (record !== undefined && paused && !hasPendingPrompt(next, record) && next.composer.credential?.turnId !== record.id) {
      next = endPausedTurn(next, record, NOTES.approvalExpired, at)
    } else if (record !== undefined) {
      next = putTurn(next, withNote(record, NOTES.approvalExpired))
    }
    return pushNotice(next, { level: "warn", source: "server", text: `approval expired: ${prompt.toolName}` }, at)
  }
  const approvals = { ...state.approvals, [prompt.id]: { ...prompt, state: "pending" as const } }
  const reason = action.message === undefined ? "" : `: ${cleanLine(action.message, 200)}`
  return pushNotice({ ...state, approvals }, { level: "error", source: "transport", text: `approval not sent${reason}` }, at)
}

/**
 * Alt+]: the next prompt becomes focused and its 1 s window restarts. Inside the full view the view follows the
 * focus, so the prompt on screen is always the one the keys answer (and it now counts as viewed in full).
 */
export function approvalFocusNext(state: AppState, at: number): AppState {
  const [head, ...rest] = state.approvalQueue
  if (head === undefined) return state
  const approvalQueue = rest.length === 0 ? [head] : [...rest, head]
  const focused = approvalQueue[0]
  const prompt = focused === undefined ? undefined : state.approvals[focused]
  if (prompt === undefined) return state
  const fullView = state.panel?.kind === "approval"
  const next: AppState = {
    ...state,
    approvalQueue,
    approvals: { ...state.approvals, [prompt.id]: { ...prompt, focusedAt: at, ...(fullView ? { viewedFull: true } : {}) } },
  }
  return fullView ? { ...next, panel: { kind: "approval", scroll: 0, approvalId: prompt.id } } : next
}

/**
 * panel/close: the approval panel is visible again. A head prompt the closed panel covered (another panel, or
 * the full view of a different prompt) was not on screen, so its 1 s window starts now (SEC-08).
 */
export function closePanel(state: AppState, at: number): AppState {
  const panel = state.panel
  if (panel === null) return state
  const closed: AppState = { ...state, panel: null }
  const head = focusedApproval(state)
  if (head === null || (panel.kind === "approval" && panel.approvalId === head.id)) return closed
  return { ...closed, approvals: { ...closed.approvals, [head.id]: { ...head, focusedAt: at } } }
}

export function approvalFocused(state: AppState, approvalId: ApprovalId, at: number): AppState {
  const prompt = state.approvals[approvalId]
  if (prompt === undefined || !state.approvalQueue.includes(approvalId) || state.approvalQueue[0] === approvalId) return state
  const approvalQueue = [approvalId, ...state.approvalQueue.filter((id) => id !== approvalId)]
  return { ...state, approvalQueue, approvals: { ...state.approvals, [approvalId]: { ...prompt, focusedAt: at } } }
}

export function approvalViewedFull(state: AppState, approvalId: ApprovalId): AppState {
  const prompt = state.approvals[approvalId]
  if (prompt === undefined || prompt.viewedFull) return state
  return { ...state, approvals: { ...state.approvals, [approvalId]: { ...prompt, viewedFull: true } } }
}

// ---------------------------------------------------------------------------------------------------------
// Credential mode (3.6.4)
// ---------------------------------------------------------------------------------------------------------

/** Rule 10. Instructions and URLs are kept as sanitized text and never opened. */
function onAuthRequired(state: AppState, event: TransportEventOf<"turn.auth_required">, at: number): AppState {
  const extensionName = cleanLine(event.extensionName, 128)
  if (extensionName.length === 0) return state
  const owner = event.turnId ?? (isCorrelated(state) ? null : (state.inflight[0] ?? null))
  const details = {
    ...(event.requestId === undefined ? {} : { requestId: cleanLine(event.requestId, 256) }),
    ...(event.instructions === undefined ? {} : { instructions: cleanText(event.instructions).slice(0, MAX_LINE_TEXT) }),
    ...(event.authUrl === undefined ? {} : { authUrl: escapedLine(event.authUrl, 2048) }),
    ...(event.setupUrl === undefined ? {} : { setupUrl: escapedLine(event.setupUrl, 2048) }),
  }
  const current = state.composer.credential
  const credential =
    current !== null && current.extensionName === extensionName
      ? { ...current, ...details, turnId: current.turnId ?? owner }
      : {
          extensionName,
          threadId: event.threadId ?? state.threadId,
          turnId: owner,
          startedAt: at,
          chars: 0,
          escArmedAt: null,
          ...details,
        }
  const next = { ...state, composer: { ...state.composer, credential } }
  const record = owner === null ? undefined : next.turns[owner]
  if (record === undefined || isSealed(record)) return next
  let updated = withNote(touched(record, at), `authentication required for ${extensionName}`)
  if (!hasLifecycle(state) && record.origin === "user") updated = { ...updated, phase: "awaiting_input" }
  return putTurn(next, updated)
}

/**
 * composer/credential_done. sent: the owning turn records the extension (the transcript shows
 * "[credential sent to <extension>]"). cancelled / ended: rule 3b seals the owning turn interrupted.
 */
export function credentialDone(state: AppState, outcome: "sent" | "cancelled" | "ended", at: number): AppState {
  const credential = state.composer.credential
  if (credential === null) return state
  let next: AppState = { ...state, composer: { ...state.composer, credential: null } }
  const record = credential.turnId === null ? undefined : next.turns[credential.turnId]
  if (outcome === "sent") {
    if (record === undefined || sentAsOwnTurn(next, credential.extensionName, credential.startedAt)) return next
    return putTurn(next, { ...record, credential: { extensionName: credential.extensionName } })
  }
  const note = outcome === "cancelled" ? NOTES.credentialCancelled : NOTES.credentialEnded
  if (outcome === "ended") {
    next = pushNotice(next, { level: "warn", source: "auth", text: NOTES.credentialEnded, id: NOTICE_IDS.credentialEnded }, at)
  }
  if (record === undefined) return next
  return isSealed(record) ? putTurn(next, withNote(record, note)) : endPausedTurn(next, record, note, at)
}

/** On the socket the credential travels as a turn of its own (turn/created with `credential`). */
function sentAsOwnTurn(state: AppState, extensionName: string, since: number): boolean {
  for (let index = state.order.length - 1; index >= Math.max(0, state.order.length - 20); index -= 1) {
    const id = state.order[index]
    const record = id === undefined ? undefined : state.turns[id]
    if (record?.credential?.extensionName === extensionName && record.createdAt >= since && record.origin === "user") return true
  }
  return false
}

// ---------------------------------------------------------------------------------------------------------
// Turn and control actions
// ---------------------------------------------------------------------------------------------------------

function toTranscriptImage(image: PendingImage): TranscriptImage {
  return {
    id: image.id,
    mime: image.mime,
    bytes: image.bytes,
    byteLength: image.bytes.byteLength,
    width: image.width,
    height: image.height,
    ...(image.filename === null ? {} : { caption: cleanLine(image.filename, 256) }),
  }
}

/** turn/created: the record exists before the request is written (rule 2). A credential's text is never stored. */
export function createUserTurn(state: AppState, action: ActionOf<"turn/created">): AppState {
  if (state.turns[action.turnId] !== undefined) return state
  const seq = state.seq + 1
  const credential =
    action.credential === undefined ? undefined : { extensionName: cleanLine(action.credential.extensionName, 128) }
  const record: TurnRecord = {
    ...blankRecord(action.turnId, seq, "user", state.threadId, action.at),
    phase: "pending",
    input: {
      text: credential === undefined ? cleanText(action.text) : "",
      images: credential === undefined ? action.images.map(toTranscriptImage) : [],
    },
    ...(credential === undefined ? {} : { credential }),
  }
  const sentIds = new Set(action.images.map((image) => image.id))
  const pendingImages = state.composer.pendingImages.filter((image) => !sentIds.has(image.id))
  const unchanged = pendingImages.length === state.composer.pendingImages.length
  const composer = unchanged ? state.composer : { ...state.composer, pendingImages }
  const added = addRecords({ ...state, seq, composer, inflight: [...state.inflight, record.id] }, [record], "end")
  return enforceImagePayloads(added)
}

/** turn/sent: "queued" while an earlier turn of the thread is still being handled (same-thread FIFO). */
export function markSent(state: AppState, action: ActionOf<"turn/sent">): AppState {
  const record = state.turns[action.turnId]
  if (record === undefined) return state
  const serverMessageId = action.serverMessageId === undefined ? {} : { serverMessageId: cleanLine(action.serverMessageId, 128) }
  if (record.phase !== "pending") return putTurn(state, { ...record, sentAt: record.sentAt ?? action.at, ...serverMessageId })
  const ahead = state.inflight.some((id) => {
    if (id === record.id) return false
    const other = state.turns[id]
    return other !== undefined && other.seq < record.seq && other.threadId === record.threadId && ACTIVE_PHASES.has(other.phase)
  })
  return putTurn(state, { ...record, phase: ahead ? "queued" : "sent", sentAt: action.at, ...serverMessageId })
}

/**
 * turn/send_failed: the turn fails; the draft returns (empty composer) or goes to history; images re-attach.
 * A turn the daemon already answered (its events arrived before the request failed) stays as it is with a
 * note: putting its text back would invite sending it twice.
 */
export function failSend(state: AppState, action: ActionOf<"turn/send_failed">): AppState {
  const record = state.turns[action.turnId]
  if (record === undefined) return state
  const message = cleanLine(action.message, 300) || "send failed"
  if (record.firstEventAt !== undefined) {
    return putTurn(state, withNote(record, `the daemon received this message; sending it reported: ${message}`))
  }
  const error = { code: cleanLine(action.code, 64) || "send_failed", message }
  let next = isSealed(record) ? state : seal(state, record, "failed", action.at, { error })
  if (record.credential !== undefined || record.input === null) return next
  const text = record.input.text
  const images = record.input.images.flatMap((image) => (image.bytes === null ? [] : [toPendingImage(image)]))
  if (action.restore) {
    next = { ...next, composer: { ...next.composer, restore: { text, images } } }
    next = pushNotice(next, { level: "warn", source: "transport", text: `send failed: ${message}` }, action.at)
  } else {
    next = { ...next, composer: pushComposerHistory(next.composer, text) }
    next = pushNotice(next, { level: "warn", source: "transport", text: NOTES.sendFailedToHistory }, action.at)
  }
  next = reattachImages(next, images, action.at)
  const lost = record.input.images.length - images.length
  if (lost <= 0) return next
  const lostText = `${lost} image(s) of the failed message are no longer in memory`
  return pushNotice(next, { level: "warn", source: "image", text: lostText }, action.at)
}

function toPendingImage(image: TranscriptImage): PendingImage {
  return {
    id: image.id,
    mime: image.mime,
    bytes: image.bytes ?? new Uint8Array(),
    filename: image.caption ?? null,
    width: image.width ?? 0,
    height: image.height ?? 0,
    source: image.caption === undefined ? "clipboard" : "file",
  }
}

/** Puts images back into the composer up to limits.maxImages; the rest is reported, never dropped silently. */
function reattachImages(state: AppState, images: readonly PendingImage[], at: number): AppState {
  if (images.length === 0) return state
  const max = state.server?.limits.maxImages ?? 5
  const pending = [...state.composer.pendingImages]
  let refused = 0
  for (const image of images) {
    if (pending.some((other) => other.id === image.id)) continue
    if (pending.length >= max) refused += 1
    else pending.push(image)
  }
  const next = { ...state, composer: { ...state.composer, pendingImages: pending } }
  if (refused === 0) return next
  const text = `${refused} image(s) did not fit back into the message (limit ${max})`
  return pushNotice(next, { level: "warn", source: "image", text }, at)
}

export function noteTurn(state: AppState, action: ActionOf<"turn/note">): AppState {
  const record = state.turns[action.turnId]
  const note = cleanLine(action.text, 300)
  return record === undefined || note.length === 0 ? state : putTurn(state, withNote(record, note))
}

/** control/created: its own record; a steer also shows under the running turn until its ack arrives. */
export function createControl(state: AppState, action: ActionOf<"control/created">): AppState {
  if (state.turns[action.turnId] !== undefined) return state
  const seq = state.seq + 1
  const text = cleanText(action.text)
  const record: TurnRecord = {
    ...blankRecord(action.turnId, seq, "control", state.threadId, action.at),
    phase: "pending",
    control: { kind: action.kind, text },
  }
  let next = addRecords({ ...state, seq, controls: [...state.controls, record.id] }, [record], "end")
  if (action.kind === "steer") next = addSteerNote(next, steerNoteText(text))
  return next
}

function addSteerNote(state: AppState, note: string): AppState {
  if (note.length === 0) return state
  const running = state.inflight
    .map((id) => state.turns[id])
    .find((record) => record?.origin === "user" && record.phase === "running")
  if (running === undefined) return state
  return putTurn(state, { ...running, steerNotes: [...running.steerNotes, { text: note }] })
}

export function failControl(state: AppState, action: ActionOf<"control/send_failed">): AppState {
  const record = state.turns[action.turnId]
  if (record === undefined || isSealed(record)) return state
  const message = cleanLine(action.message, 300) || "not sent"
  const next = seal(state, record, "failed", action.at, { error: { code: cleanLine(action.code, 64) || "send_failed", message } })
  const kind = record.control?.kind ?? "control"
  return pushNotice(next, { level: "warn", source: "transport", text: `${kind} not sent: ${message}` }, action.at)
}

// ---------------------------------------------------------------------------------------------------------
// Connection, threads, time
// ---------------------------------------------------------------------------------------------------------

/** conn/lost: a transport that cannot resume (legacy socket) turns every open turn and control into `lost`. */
export function connectionLost(state: AppState, action: ActionOf<"conn/lost">): AppState {
  const lastError = cleanLine(action.reason, 300)
  const conn = { kind: "reconnecting" as const, attempt: action.attempt, retryAt: action.retryAt, lastError }
  if (action.resumable) return { ...state, conn }
  let next: AppState = { ...state, conn, strayInterruptAck: null }
  for (const id of [...state.inflight, ...state.controls]) {
    const record = next.turns[id]
    if (record !== undefined && !isSealed(record)) next = seal(next, withNote(record, NOTES.lost), "lost", action.at)
  }
  return next
}

/** A different thread replaces the transcript (its turns, controls and prompts belong to the old thread). */
export function selectThread(state: AppState, threadId: ThreadId): AppState {
  if (threadId === state.threadId) return state
  if (state.threadId === null) return { ...state, threadId }
  return {
    ...state,
    threadId,
    turns: {},
    order: [],
    inflight: [],
    controls: [],
    strayInterruptAck: null,
    approvals: {},
    approvalQueue: [],
    otherThreadsUnread: 0,
    composer: { ...state.composer, confirm: null },
  }
}

/** notice/tick part for turns: rule 12 (legacy gateway) and the stray-ack window. */
export function tickTurns(state: AppState, at: number): AppState {
  let next = state
  if (next.strayInterruptAck !== null && next.strayInterruptAck.until <= at) next = { ...next, strayInterruptAck: null }
  return markUnanswered(next, at)
}

/**
 * Rule 12, only where silence means something: legacy gateways stream activity, so the turn at the head of the
 * queue with no event 15 s after it was accepted (or after the turn before it ended) is waiting: in the daemon's
 * scheduler behind another channel's turn, or consumed as a control input. It gets the "no reply yet" note and
 * stays first in line, so a late reply still lands on it; only after the stale limit is it sealed unanswered. A
 * legacy socket has no progress events at all; its turns get the 120 s stall notice instead (3.6.2).
 */
function markUnanswered(state: AppState, at: number): AppState {
  const caps = state.server?.capabilities
  if (caps === undefined || caps.correlation || !caps.activity) return state
  const head = state.inflight[0]
  const record = head === undefined ? undefined : state.turns[head]
  if (record === undefined || record.sentAt === undefined || !isWaitingSilently(record)) return state
  const silent = at - Math.max(record.sentAt, lastUserTurnEnd(state))
  if (silent >= UNANSWERED_AFTER_MS) return seal(state, withNote(dropNote(record, NOTES.noReplyYet), NOTES.unanswered), "unanswered", at)
  if (silent >= NO_REPLY_YET_MS && !record.notes.includes(NOTES.noReplyYet)) return putTurn(state, withNote(record, NOTES.noReplyYet))
  return state
}

function lastUserTurnEnd(state: AppState): number {
  let latest = -Infinity
  for (let index = state.order.length - 1; index >= Math.max(0, state.order.length - 20); index -= 1) {
    const id = state.order[index]
    const record = id === undefined ? undefined : state.turns[id]
    if (record?.origin === "user" && record.endedAt !== undefined && record.endedAt > latest) latest = record.endedAt
  }
  return latest
}

// ---------------------------------------------------------------------------------------------------------
// Resync (3.8 resync(), history seed)
// ---------------------------------------------------------------------------------------------------------

const TERMINAL_HISTORY_STATES = new Set(["Completed", "Failed", "Interrupted"])

function historyPhase(state: string): TurnPhase {
  if (state === "Failed") return "failed"
  if (state === "Interrupted") return "interrupted"
  if (state === "Processing") return "running"
  return "done"
}

function parseTime(text: string | null): number | null {
  if (text === null) return null
  const ms = Date.parse(text)
  return Number.isFinite(ms) ? ms : null
}

/** A server (wall-clock) time in the Clock domain of the action's `at`, or null when it does not parse. */
type HistoryClock = (text: string | null) => number | null

/**
 * History times are epoch milliseconds from the server; `at` comes from the injected Clock, which is
 * performance.now() (milliseconds since the process started) in production. `wallAt` (Date.now() read with
 * `at`) gives the offset between the two; without it they are taken to agree.
 */
function historyClock(at: number, wallAt: number | undefined): HistoryClock {
  const offset = wallAt !== undefined && Number.isFinite(wallAt) ? wallAt - at : 0
  return (text) => {
    const ms = parseTime(text)
    return ms === null ? null : ms - offset
  }
}

/**
 * resync/applied. With no records of this thread yet it seeds sealed history records (connect step 5);
 * otherwise it matches history turns to the open local turns and seals the terminal ones (recovered).
 * Prompts are added (deduplicated) either way. Every history time is converted into the Clock domain first.
 */
export function applyResync(state: AppState, snapshot: ResyncSnapshot, at: number, wallAt?: number): AppState {
  if (state.threadId !== null && snapshot.threadId !== state.threadId) return state
  let next: AppState = state.threadId === null ? { ...state, threadId: snapshot.threadId } : state
  const clock = historyClock(at, wallAt)
  const seeded = next.order.some((id) => {
    const record = next.turns[id]
    if (record === undefined || record.threadId !== snapshot.threadId) return false
    return record.origin === "user" || record.origin === "history"
  })
  next = seeded ? matchHistory(next, snapshot.turns, at, clock) : seedHistory(next, snapshot, at, clock)
  next = sealLostToRestart(next, snapshot, at)
  for (const approval of snapshot.approvals) next = upsertApproval(next, approval, null, snapshot.threadId, at)
  return restoreCredentialMode(next, snapshot, at)
}

// ---------------------------------------------------------------------------------------------------------
// Daemon restarts (rule 13)
// ---------------------------------------------------------------------------------------------------------

/**
 * True when `after` puts the daemon's start later than `before` allows: the daemon restarted in between. Both
 * starts are bounds in the Clock domain; the slack covers the probes' rounding and round trips, the drift term
 * the two hosts' clocks over the time between the probes. A missing start (probe failed) says nothing.
 */
export function daemonRestarted(before: DaemonStart | null, after: DaemonStart | null | undefined): boolean {
  if (before === null || after === null || after === undefined) return false
  const drift = Math.abs(after.measuredAt - before.measuredAt) * RESTART_DRIFT_RATIO
  return after.earliest > before.latest + RESTART_SLACK_MS + drift
}

/**
 * conn/ready: the newest daemon start replaces the known one; a later one marks a restart for the next resync. A
 * connect that could not tell keeps the known start, so a restart behind it is still seen by a later connect.
 */
export function noteDaemonStart(state: AppState, info: ServerInfo): AppState {
  const start = info.daemonStart ?? null
  if (start === null) return state
  if (!daemonRestarted(state.daemonStart, start)) return { ...state, daemonStart: start }
  return { ...state, daemonStart: start, daemonRestart: { bootedAfter: start.earliest } }
}

/**
 * Rule 13, after history was matched: everything still open from before the new process started went down with the
 * old one (V2 stamps its threads Failed, ic/src/bridge/router.rs; queued messages and V1 state are in memory). It
 * is sealed "lost (daemon restarted)", so no later reply attaches to it, except a turn paused at a gate the
 * restarted daemon still lists. Prompts and a credential request the snapshot no longer lists are gone too.
 */
function sealLostToRestart(state: AppState, snapshot: ResyncSnapshot, at: number): AppState {
  const restart = state.daemonRestart
  if (restart === null) return state
  const live = new Set([...snapshot.approvals.map((approval) => approval.id), ...(snapshot.authGates ?? []).map((gate) => gate.requestId)])
  let next: AppState = { ...state, daemonRestart: null, strayInterruptAck: null }
  const credential = next.composer.credential
  if (credential !== null && (credential.requestId === undefined || !live.has(credential.requestId))) next = leaveCredentialMode(next, at)
  let lost = 0
  for (const id of state.order) {
    const record = next.turns[id]
    if (record === undefined || isSealed(record) || record.createdAt >= restart.bootedAfter) continue
    if (pausedAtLiveGate(next, record, live)) continue
    next = loseToRestart(next, record, at)
    if (record.origin === "user") lost += 1
  }
  for (const approvalId of next.approvalQueue) {
    const prompt = next.approvals[approvalId]
    if (prompt !== undefined && !live.has(prompt.id) && prompt.createdAt < restart.bootedAfter) next = settlePrompt(next, prompt, "expired", at)
  }
  const text = lost === 0 ? "the daemon restarted" : `the daemon restarted: ${lost} unfinished turn${lost === 1 ? " was" : "s were"} lost`
  return pushNotice(next, { level: lost === 0 ? "info" : "warn", source: "transport", text }, at)
}

/** V2 keeps a thread paused at a gate across a restart: the turn lives on while the daemon still lists the gate. */
function pausedAtLiveGate(state: AppState, record: TurnRecord, live: ReadonlySet<string>): boolean {
  if (record.phase !== "awaiting_approval" && record.phase !== "awaiting_input") return false
  if (record.approvalIds.some((id) => live.has(id))) return true
  const credential = state.composer.credential
  return credential !== null && credential.turnId === record.id && credential.requestId !== undefined && live.has(credential.requestId)
}

function loseToRestart(state: AppState, record: TurnRecord, at: number): AppState {
  let next = state
  for (const id of record.approvalIds) {
    const prompt = next.approvals[id]
    if (prompt !== undefined && isOpenPrompt(prompt)) next = settlePrompt(next, prompt, "expired", at)
  }
  const current = next.turns[record.id] ?? record
  return seal(next, withNote(dropNote(current, NOTES.noReplyYet), NOTES.daemonRestarted), "lost", at)
}

/**
 * A pending authentication gate of the thread takes the next chat message as the credential (V2,
 * ic/src/bridge/router.rs handle_with_engine_inner), so credential mode is restored from the snapshot as the
 * live gate_required / auth_required would have entered it (the live events were missed: restart, reconnect,
 * stream gap, or the gate came from another client). An active credential for another extension is kept.
 */
function restoreCredentialMode(state: AppState, snapshot: ResyncSnapshot, at: number): AppState {
  const gate = snapshot.authGates?.find((candidate) => candidate.threadId === snapshot.threadId)
  if (gate === undefined) return state
  const current = state.composer.credential
  if (current !== null) {
    // Already in credential mode: an active request is never replaced (its masked buffer would be cleared); the
    // same extension only learns the gate's ids, which auth-token sends together.
    if (current.extensionName !== cleanLine(gate.extensionName, 128) || current.requestId !== undefined) return state
    const credential = { ...current, requestId: cleanLine(gate.requestId, 256), threadId: current.threadId ?? gate.threadId }
    return { ...state, composer: { ...state.composer, credential } }
  }
  const event: TransportEventOf<"turn.auth_required"> = {
    kind: "turn.auth_required",
    turnId: null,
    threadId: gate.threadId,
    extensionName: gate.extensionName,
    requestId: gate.requestId,
    ...(gate.instructions === undefined ? {} : { instructions: gate.instructions }),
    ...(gate.authUrl === undefined ? {} : { authUrl: gate.authUrl }),
  }
  return onAuthRequired(state, event, at)
}

function seedHistory(state: AppState, snapshot: ResyncSnapshot, at: number, clock: HistoryClock): AppState {
  let seq = state.seq
  const records = snapshot.turns.map((turn) => {
    seq += 1
    return historyRecord(turn, seq, snapshot.threadId, at, clock)
  })
  return addRecords({ ...state, seq }, records, "start")
}

function historyRecord(turn: HistoryTurn, seq: number, threadId: ThreadId, at: number, clock: HistoryClock): TurnRecord {
  const createdAt = clock(turn.startedAt) ?? at
  const endedAt = clock(turn.completedAt)
  const response = turn.response === null ? null : cleanText(turn.response)
  const replyless = response === null || response.length === 0
  // Durable V2 history shows a turn that is still running as Failed without a reply (ic/src/channels/web/util.rs):
  // a young one is seeded open, like a local turn (YOUNG_FAILED_TURN_MS), so its live reply can still complete it.
  const stated = historyPhase(turn.state)
  const phase = stated === "failed" && replyless && at - createdAt < YOUNG_FAILED_TURN_MS ? "running" : stated
  return {
    ...blankRecord(localId(HISTORY_PREFIX, seq), seq, "history", threadId, createdAt),
    input: { text: cleanText(turn.userInput), images: [] },
    historyStartedAt: turn.startedAt,
    phase,
    finalText: replyless ? null : response,
    ...(phase === "failed" && replyless ? { error: { code: "failed", message: NOTES.failedWithoutReply } } : {}),
    ...(endedAt === null || phase === "running" ? {} : { endedAt }),
  }
}

function matchHistory(state: AppState, turns: readonly HistoryTurn[], at: number, clock: HistoryClock): AppState {
  let next = state
  let cursor = 0
  for (const id of state.inflight) {
    const record = next.turns[id]
    if (record === undefined || record.origin !== "user" || isSealed(record)) continue
    const index = turns.findIndex((turn, at2) => at2 >= cursor && historyMatchesLocal(turn, record, clock))
    const turn = index < 0 ? undefined : turns[index]
    if (turn === undefined) continue
    cursor = index + 1
    next = recoverFromHistory(next, record, turn, at)
  }
  return refreshOpenHistoryRecords(next, turns, at)
}

/** started_at >= sentAt - 5 s and user_input starts with the sent text (it may carry an <attachment> block). */
function historyMatchesLocal(turn: HistoryTurn, record: TurnRecord, clock: HistoryClock): boolean {
  const started = clock(turn.startedAt)
  if (started === null || started < (record.sentAt ?? record.createdAt) - HISTORY_START_SKEW_MS) return false
  const sent = record.input?.text.trim() ?? ""
  const input = turn.userInput.trim()
  return sent.length > 0 ? input.startsWith(sent) : input.length === 0 || input.startsWith("<attachment")
}

function recoverFromHistory(state: AppState, record: TurnRecord, turn: HistoryTurn, at: number): AppState {
  if (!TERMINAL_HISTORY_STATES.has(turn.state)) return state
  const response = turn.response === null ? "" : cleanText(turn.response)
  const phase = historyPhase(turn.state)
  const finalText = record.finalText ?? (response.length > 0 ? response : null)
  const recovered: TurnRecord = { ...dropNote(record, NOTES.noReplyYet), recovered: true, finalText }
  if (response.length > 0) {
    const patch = phase === "failed" ? { error: record.error ?? { code: "failed", message: response } } : {}
    return seal(state, recovered, phase, at, patch)
  }
  if (phase === "failed" && at - (record.sentAt ?? record.createdAt) < YOUNG_FAILED_TURN_MS) return state
  if (phase === "failed") {
    return seal(state, recovered, "failed", at, { error: record.error ?? { code: "failed", message: NOTES.failedWithoutReply } })
  }
  return seal(state, phase === "done" ? withNote(recovered, NOTES.noReply) : recovered, phase, at)
}

/** History records seeded while the server was still processing them. */
function refreshOpenHistoryRecords(state: AppState, turns: readonly HistoryTurn[], at: number): AppState {
  let next = state
  for (const id of state.order) {
    const record = next.turns[id]
    if (record === undefined || record.origin !== "history" || isSealed(record)) continue
    // Identified by the server's own started_at text: the Clock-domain createdAt depends on each resync's offset.
    const turn = turns.find(
      (candidate) => candidate.startedAt === record.historyStartedAt && cleanText(candidate.userInput) === record.input?.text,
    )
    if (turn !== undefined) next = recoverFromHistory(next, record, turn, at)
  }
  return next
}
