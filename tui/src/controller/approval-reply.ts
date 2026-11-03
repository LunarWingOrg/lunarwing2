/**
 * Approval keywords, typed or dictated (design 3.10.3, 3.6.2, SEC-04, SEC-08, UX-07).
 *
 * While a prompt is pending, a bare keyword is never sent as chat: the daemon would take it as the decision for
 * whatever approval is pending on the thread (ic/src/agent/submission.rs:179-199, thread_ops.rs:1405-1422).
 * matchApprovalReply() normalizes generously (dictation returns "Yes." or "Okay!"), so the session controller
 * asks for a confirming Enter instead. With nothing pending the daemon's own exact rule applies
 * (isDaemonApprovalKeyword): "Yes." is not a keyword there, "ok" is.
 *
 * approvalGate() applies the 3.6.2 rules to the focused prompt; the panel and every approve path share it, with
 * the same measure (state/approval-measure.ts) as the panel, so "hidden" always means "not on screen".
 */
import type { ApprovalDecision, ApprovalPrompt, ComposerState } from "../state/model.ts"
import { APPROVAL_PANEL_CAPS, approvalNeedsFullView, approvalTextWidth, type ApprovalCaps } from "../state/approval-measure.ts"
import { normalizeCommandText } from "../transport/guards.ts"

/** A prompt must have been the focused one this long before a key or command answers it (SEC-08). */
export const APPROVAL_FRESHNESS_MS = 1_000

/** ic/src/agent/submission.rs:179-199, in the daemon's three arms. */
const APPROVE_WORDS: readonly string[] = ["yes", "y", "approve", "ok", "/approve", "/yes", "/y"]
const ALWAYS_WORDS: readonly string[] = ["always", "a", "yes always", "approve always", "/always", "/a"]
const DENY_WORDS: readonly string[] = ["no", "n", "deny", "reject", "cancel", "/deny", "/no", "/n"]
/** Client-only addition (3.10.3): dictation writes "Okay." for "ok". */
const CLIENT_APPROVE_WORDS: readonly string[] = ["okay"]

const DAEMON_KEYWORDS: ReadonlyMap<string, ApprovalDecision> = new Map<string, ApprovalDecision>([
  ...APPROVE_WORDS.map((word) => [word, "approve"] as const),
  ...ALWAYS_WORDS.map((word) => [word, "always"] as const),
  ...DENY_WORDS.map((word) => [word, "deny"] as const),
])

const REPLY_WORDS: ReadonlyMap<string, ApprovalDecision> = new Map<string, ApprovalDecision>([
  ...DAEMON_KEYWORDS,
  ...CLIENT_APPROVE_WORDS.map((word) => [word, "approve"] as const),
])

/** Leading and trailing characters dictation and typing add around a keyword (3.10.3). */
const EDGE_PUNCTUATION = /^[\s.,!?;:'"`]+|[\s.,!?;:'"`]+$/gu

/** NFKC, lower-case, trim, strip edge punctuation, collapse inner whitespace. */
export function normalizeApprovalReply(text: string): string {
  return text.normalize("NFKC").toLowerCase().trim().replace(EDGE_PUNCTUATION, "").replace(/\s+/gu, " ")
}

/**
 * The decision a typed or dictated reply stands for, or null when it is not a reply. Text that only starts
 * with a keyword ("Yes, but use staging.") is not a reply.
 */
export function matchApprovalReply(text: string): ApprovalDecision | null {
  const normalized = normalizeApprovalReply(text)
  if (normalized.length === 0) return null
  return REPLY_WORDS.get(normalized) ?? null
}

/** The daemon's own rule (trim, lower-case, exact list): what it would swallow as an approval answer. */
export function isDaemonApprovalKeyword(text: string): boolean {
  return DAEMON_KEYWORDS.has(normalizeCommandText(text))
}

/** The legacy text keyword for a decision (legacy socket, 3.6.2). */
export function legacyKeyword(decision: ApprovalDecision): string {
  return decision === "approve" ? "yes" : decision === "always" ? "always" : "no"
}

export type ApprovalGateResult = "ok" | "too_new" | "typed_before_prompt" | "hidden_unviewed" | "truncated"

export interface ApprovalGateOptions {
  /** Default "approve": the strictest rules. Deny skips the hidden-content and truncation checks. */
  decision?: ApprovalDecision
  /**
   * "key" (Alt+Y, Alt+A, Alt+N, panel clicks): the composer text is irrelevant. "text" (slash commands, typed or
   * dictated replies): the prompt must predate the first keystroke of the current composer text.
   */
  via?: "key" | "text"
  /** The panel's caps (approvalCapsForRows(layout.approval)); default the regular 8-row panel. */
  caps?: Readonly<ApprovalCaps>
}

/**
 * The 3.6.2 rules for the focused prompt. `width` is the terminal width (the panel text is
 * approvalTextWidth(width) columns, as in the panel).
 */
export function approvalGate(
  prompt: ApprovalPrompt,
  composer: Pick<ComposerState, "startedAt">,
  width: number,
  now: number,
  opts: ApprovalGateOptions = {},
): ApprovalGateResult {
  const decision = opts.decision ?? "approve"
  if (prompt.focusedAt === null || now - prompt.focusedAt < APPROVAL_FRESHNESS_MS) return "too_new"
  if (opts.via === "text" && composer.startedAt !== null && prompt.createdAt >= composer.startedAt) {
    return "typed_before_prompt"
  }
  if (decision === "deny") return "ok"
  if (prompt.truncated) return "truncated"
  if (approvalNeedsFullView(prompt, approvalTextWidth(width), opts.caps ?? APPROVAL_PANEL_CAPS)) return "hidden_unviewed"
  return "ok"
}

export const APPROVAL_PENDING_HINT = "approval pending: Alt+Y approve, Alt+N deny, Alt+A always (or /approve, /deny, /always)"
export const APPROVAL_TOO_NEW = "approval just arrived; press again"
/** Leads with the key: a notice row is cut to the terminal width. */
export const APPROVAL_HIDDEN = "Alt+V: open the full parameters first"
export const ALWAYS_NEEDS_KEY = "use Alt+A or /always to approve permanently"
/** A key or command while an info panel covers the approval panel (the prompt is not on screen). */
export const APPROVAL_COVERED = "the approval is behind this panel: Esc closes it, then answer"
/** A key or command while the full view shows a prompt that is no longer pending. */
export const APPROVAL_VIEW_SETTLED = "this approval is no longer pending: Esc closes the view"

/** Why no prompt can be answered while one is pending: none is on screen (controller/session.ts). */
export function approvalNotShown(panel: { kind: string } | null): string {
  return panel?.kind === "approval" ? APPROVAL_VIEW_SETTLED : APPROVAL_COVERED
}

/** The notice text for a refused gate result. */
export function gateRefusal(result: Exclude<ApprovalGateResult, "ok">, prompt: Pick<ApprovalPrompt, "parameters">): string {
  switch (result) {
    case "too_new":
    case "typed_before_prompt":
      return APPROVAL_TOO_NEW
    case "hidden_unviewed":
      return APPROVAL_HIDDEN
    case "truncated":
      return truncatedNotice(prompt)
  }
}

/** A REPLv3 prompt whose parameters did not fit the frame can only be denied (3.6.2). */
export function truncatedNotice(prompt: Pick<ApprovalPrompt, "parameters">): string {
  const bytes = Buffer.byteLength(prompt.parameters, "utf8")
  return `parameters too large to review here (${bytes} bytes); review in the web UI`
}

/** "'ok' is an approval keyword; ..." for the nothing-pending guard (3.10.3). */
export function keywordGuardNotice(text: string): string {
  const shown = text.trim().slice(0, 40)
  return `'${shown}' is an approval keyword; with nothing pending the agent swallows it. Press Enter again to send anyway.`
}
