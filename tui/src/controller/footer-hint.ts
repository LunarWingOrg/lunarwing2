/**
 * The one-line footer (design 3.12.3, UX-04): the keys that apply right now. Pure; the first matching row wins.
 *
 * | offline                     | offline: <reason> - /transport reconnect - F1 help                    |
 * | credential mode             | Enter send credential - Esc Esc cancel                                |
 * | confirmation pending        | Enter confirm - Esc back                                              |
 * | approval pending            | Alt+Y approve - Alt+N deny - Alt+A always - Alt+V view - Alt+J next   |
 * | recording, toggle           | REC 0:07 - F9 stop - Esc cancel                                       |
 * | recording, hold             | REC 0:07 - release to stop - Esc cancel                               |
 * | transcribing                | transcribing - Esc cancel                                             |
 * | turn running                | Enter queue - Ctrl+S stop - /steer <note> - F1 help                   |
 * | idle                        | F9 dictate - Ctrl+V image - /shot - Enter send - F1 help              |
 * | idle, no voice (--no-voice) | Ctrl+V image - /shot - Enter send - F1 help                           |
 *
 * Refinements: an armed credential Esc reads "press Esc again to cancel the credential"; a send guard (approval
 * keyword, //restart) uses the confirmation row; "opening mic..." while the capture starts. Every key offered
 * arrives in the legacy encoding too (inside tmux): the KKP-only keys of KEYMAP (Ctrl+Enter, Shift+Enter, Ctrl+-,
 * Ctrl+.) never appear here, so the running-turn row offers Ctrl+S, not Ctrl+Enter. "next" reads Alt+] only where
 * that key arrives (KKP outside tmux; in the legacy encoding Alt+] is ESC ], which the input parser drops), else
 * Alt+J. While an info panel covers a pending prompt, or the full view shows a settled one, the
 * approval keys answer nothing (controller/session.ts), so the row says how to get the prompt back on screen.
 * While the focused prompt has hidden text (approving needs the full view, SEC-04) "Alt+V view" comes first.
 */
import type { AppState } from "../state/model.ts"
import { activeUserTurnCount, displayedApproval } from "../state/correlate.ts"
import { approvalCapsForRows, approvalNeedsFullView, approvalTextWidth } from "../state/approval-measure.ts"
import { layoutForState } from "../state/layout.ts"
import { CREDENTIAL_ESC_WINDOW_MS } from "../state/reducer.ts"
import { formatClock } from "../util.ts"

export interface FooterOptions {
  /** Clock time for the REC timer and the Esc window; default: the recording start (0:00). */
  now?: number
  /** Label of the first push-to-talk key, default "F9". */
  ptt?: string
  /**
   * False when dictation cannot start (--no-voice, or no recorder or voice source: the mic reads "(no mic)"),
   * so the idle row offers no push-to-talk key. Default true.
   */
  voice?: boolean
}

export function footerHint(state: AppState, opts: FooterOptions = {}): string {
  const now = opts.now ?? state.voice.startedAt ?? 0
  const ptt = opts.ptt ?? "F9"
  if (state.conn.kind === "offline") return `offline: ${state.conn.reason} - /transport reconnect - F1 help`
  const credential = state.composer.credential
  if (credential !== null) {
    const armed = credential.escArmedAt !== null && now - credential.escArmedAt < CREDENTIAL_ESC_WINDOW_MS
    return armed ? "press Esc again to cancel the credential" : "Enter send credential - Esc Esc cancel"
  }
  if (state.composer.confirm !== null || state.composer.guard !== null) return "Enter confirm - Esc back"
  if (state.approvalQueue.length > 0) {
    if (displayedApproval(state) !== null) return approvalHint(state)
    return state.panel?.kind === "approval" ? "Esc close the view - the next approval waits behind it" : "Esc close the panel - an approval waits behind it"
  }
  const voice = voiceHint(state, now, ptt)
  if (voice !== null) return voice
  if (activeUserTurnCount(state) > 0) return "Enter queue - Ctrl+S stop - /steer <note> - F1 help"
  const idle = "Ctrl+V image - /shot - Enter send - F1 help"
  return opts.voice === false ? idle : `${ptt} dictate - ${idle}`
}

function approvalHint(state: AppState): string {
  const prompt = displayedApproval(state)
  // A truncated REPLv3 prompt can only be denied (3.6.2).
  const parts = prompt?.truncated === true ? ["Alt+N deny"] : ["Alt+Y approve", "Alt+N deny"]
  if (prompt?.allowAlways === true && !prompt.truncated) parts.push("Alt+A always")
  // While text is hidden approving needs the full view first: then its key leads, so a narrow footer keeps it.
  const rows = layoutForState(state).approval
  const hidden = prompt !== null && approvalNeedsFullView(prompt, approvalTextWidth(state.view.width), approvalCapsForRows(rows))
  if (hidden) parts.unshift("Alt+V view")
  else parts.push("Alt+V view")
  if (state.approvalQueue.length > 1) parts.push(state.ui.kkpRelease && !state.ui.tmux ? "Alt+] next" : "Alt+J next")
  return parts.join(" - ")
}

function voiceHint(state: AppState, now: number, ptt: string): string | null {
  const voice = state.voice
  if (voice.phase === "arming") return "opening mic... - Esc cancel"
  if (voice.phase === "recording") {
    const rec = `REC ${formatClock(now - (voice.startedAt ?? now))}`
    if (voice.mode === "toggle") return `${rec} - ${ptt} stop - Esc cancel`
    if (voice.mode === "provisional") return `${rec} - Esc cancel`
    return `${rec} - release to stop - Esc cancel`
  }
  if (voice.phase === "finishing" || voice.queue.length > 0) return "transcribing - Esc cancel"
  return null
}
