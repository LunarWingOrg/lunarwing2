/**
 * Desktop notifications (design 3.6.2, UX-12). When the terminal is blurred (useBlur / useFocus feed ui/blur and
 * ui/focus) and a new approval prompt arrives, notify-send is spawned through the injected SpawnFn:
 *
 *   ["notify-send", "-a", "wingtip", "--", "LunarWing <tenant>", "approval needed: <tool>"]
 *
 * argv only, with the allowlisted child environment; the tool name is sanitized and cut to 64 characters, and the
 * description and parameters are never included. At most one notification per 10 s. [ui] notify = "approvals"
 * (default), "all" (also when a turn of this window finishes while blurred) or "off". A missing notify-send
 * disables the notifier silently. Terminal bells are not used.
 */
import type { Clock } from "@opentui/core"
import { bunSpawn, type ChildEnv, type NotifyMode, type SpawnFn } from "../config/types.ts"
import { sanitizeLine } from "../sanitize.ts"
import type { AppState, Store, TurnRecord } from "../state/model.ts"
import { isSealed } from "../state/correlate.ts"

export const NOTIFY_MIN_INTERVAL_MS = 10_000
export const NOTIFY_TOOL_CHARS = 64
const NOTIFY_TIMEOUT_MS = 5_000

export interface NotifierDeps {
  store: Pick<Store, "getState" | "subscribe">
  clock: Pick<Clock, "now">
  env: ChildEnv
  mode: NotifyMode
  /** Shown as "LunarWing <name>": the tenant, else the profile. */
  name: string
  spawn?: SpawnFn
}

export interface Notifier {
  /** True until notify-send turned out to be missing (or mode is "off"). */
  readonly enabled: boolean
  dispose(): void
}

export function createNotifier(deps: NotifierDeps): Notifier {
  return new DesktopNotifier(deps)
}

/** The argv for one notification; exported for tests. */
export function notifyArgv(name: string, body: string): string[] {
  return ["notify-send", "-a", "wingtip", "--", `LunarWing ${sanitizeLine(name).trim()}`, body]
}

/** "approval needed: <tool>", the tool name sanitized and cut. */
export function approvalBody(toolName: string): string {
  const tool = Array.from(sanitizeLine(toolName).trim()).slice(0, NOTIFY_TOOL_CHARS).join("")
  return `approval needed: ${tool}`
}

class DesktopNotifier implements Notifier {
  private active: boolean
  private lastAt = Number.NEGATIVE_INFINITY
  private readonly seenApprovals = new Set<string>()
  private readonly unsubscribe: () => void
  private previous: AppState

  constructor(private readonly deps: NotifierDeps) {
    this.active = deps.mode !== "off"
    this.previous = deps.store.getState()
    for (const id of this.previous.approvalQueue) this.seenApprovals.add(id)
    this.unsubscribe = deps.store.subscribe(() => this.onState())
  }

  get enabled(): boolean {
    return this.active
  }

  dispose(): void {
    this.unsubscribe()
    this.active = false
  }

  private onState(): void {
    const state = this.deps.store.getState()
    const previous = this.previous
    this.previous = state
    if (!this.active) return
    const fresh = state.approvalQueue.filter((id) => !this.seenApprovals.has(id))
    for (const id of fresh) this.seenApprovals.add(id)
    if (!state.ui.blurred) return
    const prompt = fresh.map((id) => state.approvals[id]).find((candidate) => candidate !== undefined)
    if (prompt !== undefined) {
      this.notify(approvalBody(prompt.toolName))
      return
    }
    if (this.deps.mode === "all") {
      const finished = newlySealedUserTurn(previous, state)
      if (finished !== null) this.notify(finished.phase === "done" ? "reply ready" : `turn ${finished.phase}`)
    }
  }

  private notify(body: string): void {
    const now = this.deps.clock.now()
    if (now - this.lastAt < NOTIFY_MIN_INTERVAL_MS) return
    this.lastAt = now
    const spawn = this.deps.spawn ?? bunSpawn
    try {
      const child = spawn(notifyArgv(this.deps.name, body), {
        env: this.deps.env,
        stdin: "ignore",
        stdout: "ignore",
        stderr: "ignore",
        timeout: NOTIFY_TIMEOUT_MS,
      })
      void child.exited.catch(() => undefined)
    } catch {
      // notify-send is not installed: notifications are off for this session, silently.
      this.active = false
    }
  }
}

/** A user turn of this window that was open before and is sealed now. */
function newlySealedUserTurn(previous: AppState, state: AppState): TurnRecord | null {
  if (previous.turns === state.turns) return null
  for (const id of previous.inflight) {
    const before = previous.turns[id]
    const after = state.turns[id]
    if (before === undefined || after === undefined || after.origin !== "user") continue
    if (!isSealed(before) && isSealed(after)) return after
  }
  return null
}
