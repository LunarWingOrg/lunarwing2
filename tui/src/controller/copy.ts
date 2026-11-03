/**
 * Copy (design 3.12.4, UX-11). useMouse: true sends drags to the app and OpenTUI emits a selection event only
 * when a drag ends; inside tmux `set-clipboard external` makes tmux ignore OSC 52 from applications, so the
 * client copies by itself:
 *
 * 1. The host clipboard instance shared with Ctrl+V (images/clipboard.ts ClipboardReader.hostClipboard()):
 *    writeText(text, { selection: "clipboard" }). "written" -> notice "copied N characters (cliphist records
 *    copies)".
 * 2. unsupported, failed or timed-out: `wl-copy` with the text on stdin (argv ["wl-copy"], the allowlisted child
 *    environment, 3 s) when WAYLAND_DISPLAY is set;
 * 3. otherwise OSC 52 through renderer.copyToClipboardOSC52(text), outside tmux only.
 *
 * All chrome is selectable={false}, so a drag selects transcript text only. /copy [n] copies the final text of
 * the newest (or n-th newest) reply.
 */
import type { Clock, Selection } from "@opentui/core"
import { bunSpawn, type ChildEnv, type SpawnFn } from "../config/types.ts"
import type { ClipboardWriter } from "../images/clipboard.ts"
import type { AppState, Store, TurnRecord } from "../state/model.ts"
import { visibleText } from "../state/turn-text.ts"

export const WL_COPY_TIMEOUT_MS = 3_000

export interface CopyDeps {
  store: Pick<Store, "getState" | "dispatch">
  clock: Pick<Clock, "now">
  /** The shared host clipboard; null or a throw means unavailable. */
  writer: () => ClipboardWriter | null
  env: ChildEnv
  spawn?: SpawnFn
  /** renderer.copyToClipboardOSC52; omitted in tests without a renderer. */
  osc52?: (text: string) => boolean
}

export interface CopyController {
  /** Copies `text`; resolves true when one of the three paths reported success. */
  copyText(text: string): Promise<boolean>
  /** useSelectionHandler: a finished drag over transcript text. */
  onSelection(selection: Selection): void
  /** /copy [n]: the newest (n = 1) or n-th newest reply. */
  copyReply(n: number): Promise<boolean>
}

export function createCopyController(deps: CopyDeps): CopyController {
  return new Copier(deps)
}

/** Records whose text is a reply: everything but controls. */
function isReply(record: TurnRecord): boolean {
  return record.origin !== "control" && visibleText(record).trim().length > 0
}

/** The n-th newest reply text (n >= 1), or null. */
export function nthReply(state: Pick<AppState, "order" | "turns">, n: number): string | null {
  let seen = 0
  for (let index = state.order.length - 1; index >= 0; index -= 1) {
    const id = state.order[index]
    const record = id === undefined ? undefined : state.turns[id]
    if (record === undefined || !isReply(record)) continue
    seen += 1
    if (seen === n) return visibleText(record)
  }
  return null
}

class Copier implements CopyController {
  constructor(private readonly deps: CopyDeps) {}

  onSelection(selection: Selection): void {
    const text = selection.getSelectedText()
    if (text.trim().length === 0) return
    void this.copyText(text)
  }

  async copyReply(n: number): Promise<boolean> {
    const index = Number.isFinite(n) && n >= 1 ? Math.floor(n) : 1
    const text = nthReply(this.deps.store.getState(), index)
    if (text === null) {
      this.notice("warn", index === 1 ? "no reply to copy yet" : `there is no reply number ${index}`)
      return false
    }
    return this.copyText(text)
  }

  async copyText(text: string): Promise<boolean> {
    const chars = Array.from(text).length
    if (await this.viaHost(text)) return this.copied(chars, "")
    if (this.deps.env.WAYLAND_DISPLAY !== undefined && this.deps.env.WAYLAND_DISPLAY !== "") {
      if (await this.viaWlCopy(text)) return this.copied(chars, " via wl-copy")
    } else if (!this.inTmux() && this.deps.osc52?.(text) === true) {
      return this.copied(chars, " via the terminal (OSC 52)")
    }
    this.notice("warn", "copy failed: no clipboard is reachable from this terminal")
    return false
  }

  private async viaHost(text: string): Promise<boolean> {
    try {
      const writer = this.deps.writer()
      if (writer === null) return false
      const result = await writer.writeText(text, { selection: "clipboard" })
      return result.status === "written"
    } catch {
      return false
    }
  }

  private async viaWlCopy(text: string): Promise<boolean> {
    const spawn = this.deps.spawn ?? bunSpawn
    try {
      const child = spawn(["wl-copy"], {
        env: this.deps.env,
        stdin: new TextEncoder().encode(text),
        stdout: "ignore",
        stderr: "ignore",
        timeout: WL_COPY_TIMEOUT_MS,
        killSignal: "SIGKILL",
      })
      return (await child.exited) === 0
    } catch {
      return false
    }
  }

  private inTmux(): boolean {
    return this.deps.store.getState().ui.tmux
  }

  private copied(chars: number, how: string): true {
    this.notice("info", `copied ${chars} characters${how} (cliphist records copies)`)
    return true
  }

  private notice(level: "info" | "warn", text: string): void {
    this.deps.store.dispatch({ type: "notice/push", notice: { level, source: "client", text }, at: this.deps.clock.now() })
  }
}
