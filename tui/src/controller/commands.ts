/**
 * Local slash commands (design 3.10.1, R17). The composer text is trimmed; a leading "/" starts a command; the
 * first token matches case-insensitively. "//<text>" is not a command: it sends "/<text>" through the guards.
 * Any other "/cmd" is forwarded verbatim (/compact, /undo, /model, /status, ...), through the guards.
 *
 * Never sent: /quit and /exit (quit the TUI), /shutdown (refused), /restart (refused with the mt-admin verb;
 * //restart forwards after a second Enter within 5 s).
 *
 * parseSlash() is pure; runSlash() executes a parsed command against a CommandContext, which
 * controller/session.ts implements.
 */
import type { ApprovalDecision, PanelKind } from "../state/model.ts"
import { isVoiceSourceId, type VoiceController, type VoiceSourceId } from "../voice/types.ts"

export interface SlashCommand {
  /** Lower-cased first token without the slash. */
  readonly name: string
  /** The rest, trimmed. */
  readonly args: string
  /** The whole trimmed text. */
  readonly raw: string
}

/** null when `text` is not a local command (no leading "/", or the "//" escape). */
export function parseSlash(text: string): SlashCommand | null {
  const trimmed = text.trim()
  if (!trimmed.startsWith("/") || trimmed.startsWith("//")) return null
  const match = /^\/(\S+)(?:\s+([\s\S]*))?$/u.exec(trimmed)
  if (match === null) return null
  return { name: (match[1] ?? "").toLowerCase(), args: (match[2] ?? "").trim(), raw: trimmed }
}

/** "//<text>" -> "/<text>"; null when `text` is not the escape. */
export function unescapeSlash(text: string): string | null {
  const trimmed = text.trim()
  return trimmed.startsWith("//") ? trimmed.slice(1) : null
}

export const SHUTDOWN_REFUSAL =
  "not forwarded: /shutdown stops the daemon. For tenants use `lunarwing-mt-admin.sh stop-tenant <t>`."

export function restartRefusal(tenant: string | null): string {
  return `not forwarded: use \`lunarwing-mt-admin.sh restart-tenant ${tenant ?? "<t>"}\` (//restart sends it to the daemon after a second Enter)`
}

export type ControlCommandKind = "interrupt" | "steer" | "clear" | "new"

/** What commands drive; implemented by controller/session.ts. */
export interface CommandContext {
  readonly tenant: string | null
  readonly voice: VoiceController | null
  notice(level: "info" | "warn" | "error", text: string): void
  openPanel(kind: PanelKind): void
  quit(): void
  /**
   * Sends `text` as a chat message through the guards (forbidden content, keywords, //restart). "keep" when
   * nothing was sent and the composer text must stay (a guard waits for a second Enter, or not connected).
   */
  forward(text: string): Promise<CommandOutcome>
  control(kind: ControlCommandKind, text: string): Promise<CommandOutcome>
  /** /approve, /always, /deny: the 3.6.2 rules with via "text"; legacy daemons get the text keyword. */
  answerByCommand(decision: ApprovalDecision): Promise<CommandOutcome>
  viewApproval(): void
  attachFile(path: string): Promise<void>
  screenshot(full: boolean): Promise<void>
  imageCommand(args: string): Promise<void>
  copyReply(n: number): Promise<void>
  reconnect(): void
  thread(args: string): Promise<void>
  /** /voice device: loads the device list, then opens the devices panel. */
  showDevices(): Promise<void>
}

/** "done": the composer text was used and is cleared; "keep": it stays (usage error, guard, not connected). */
export type CommandOutcome = "done" | "keep"

/** Runs one parsed command. The caller clears the composer afterwards unless this returns "keep". */
export async function runSlash(command: SlashCommand, ctx: CommandContext): Promise<CommandOutcome> {
  const handler = HANDLERS[command.name]
  if (handler === undefined) return ctx.forward(command.raw)
  return (await handler(command, ctx)) ?? "done"
}

type Handler = (command: SlashCommand, ctx: CommandContext) => Promise<CommandOutcome | void> | CommandOutcome | void

const quit: Handler = (_command, ctx) => ctx.quit()

const HANDLERS: Readonly<Record<string, Handler>> = {
  quit,
  exit: quit,
  shutdown: (_command, ctx) => ctx.notice("warn", SHUTDOWN_REFUSAL),
  restart: (_command, ctx) => ctx.notice("warn", restartRefusal(ctx.tenant)),
  help: (_command, ctx) => ctx.openPanel("help"),
  img: (command, ctx) => attach(command, ctx),
  attach: (command, ctx) => attach(command, ctx),
  shot: (command, ctx) => ctx.screenshot(command.args.toLowerCase() === "full"),
  image: (command, ctx) => ctx.imageCommand(command.args),
  copy: (command, ctx) => copy(command, ctx),
  voice: (command, ctx) => voiceCommand(command.args, ctx),
  transport: (command, ctx) => transportCommand(command.args, ctx),
  interrupt: (_command, ctx) => ctx.control("interrupt", "/interrupt"),
  stop: (_command, ctx) => ctx.control("interrupt", "/interrupt"),
  steer: (command, ctx) => steer(command, ctx),
  approve: (_command, ctx) => ctx.answerByCommand("approve"),
  always: (_command, ctx) => ctx.answerByCommand("always"),
  deny: (_command, ctx) => ctx.answerByCommand("deny"),
  approval: (command, ctx) => approvalCommand(command, ctx),
  thread: (command, ctx) => ctx.thread(command.args),
  clear: (command, ctx) => (command.args === "" ? ctx.control("clear", "/clear") : ctx.forward(command.raw)),
  new: (command, ctx) => (command.args === "" ? ctx.control("new", "/new") : ctx.forward(command.raw)),
}

async function attach(command: SlashCommand, ctx: CommandContext): Promise<CommandOutcome> {
  if (command.args === "") {
    ctx.notice("warn", `usage: /${command.name} <path>`)
    return "keep"
  }
  await ctx.attachFile(command.args)
  return "done"
}

async function copy(command: SlashCommand, ctx: CommandContext): Promise<CommandOutcome> {
  const n = command.args === "" ? 1 : Number(command.args)
  if (!Number.isInteger(n) || n < 1) {
    ctx.notice("warn", "usage: /copy [n] (1 = the newest reply)")
    return "keep"
  }
  await ctx.copyReply(n)
  return "done"
}

async function steer(command: SlashCommand, ctx: CommandContext): Promise<CommandOutcome> {
  if (command.args === "") {
    ctx.notice("warn", "usage: /steer <note>")
    return "keep"
  }
  return ctx.control("steer", `/steer ${command.args}`)
}

function approvalCommand(command: SlashCommand, ctx: CommandContext): CommandOutcome {
  if (command.args.toLowerCase() === "view" || command.args === "") {
    ctx.viewApproval()
    return "done"
  }
  ctx.notice("warn", "usage: /approval view")
  return "keep"
}

async function transportCommand(args: string, ctx: CommandContext): Promise<CommandOutcome> {
  const sub = args.toLowerCase()
  if (sub === "" || sub === "status") {
    ctx.openPanel("transport")
    return "done"
  }
  if (sub === "reconnect") {
    ctx.reconnect()
    return "done"
  }
  ctx.notice("warn", "usage: /transport [status|reconnect]")
  return "keep"
}

/** "lunarvoice, hex-app" or "lunarvoice hex-app" -> ids; null when one is unknown. */
export function parseVoiceIds(text: string): VoiceSourceId[] | null {
  const ids = text
    .split(/[\s,]+/u)
    .map((id) => id.trim().toLowerCase())
    .filter((id) => id.length > 0)
  if (ids.length === 0 || !ids.every(isVoiceSourceId)) return null
  return [...new Set(ids)] as VoiceSourceId[]
}

async function voiceCommand(args: string, ctx: CommandContext): Promise<CommandOutcome> {
  const voice = ctx.voice
  const [rawSub = "", ...rest] = args.split(/\s+/u)
  const sub = rawSub.toLowerCase()
  const value = rest.join(" ").trim()
  if (sub === "" || sub === "status") {
    voice?.probeNow()
    ctx.openPanel("voice")
    return "done"
  }
  if (sub === "privacy") {
    ctx.openPanel("privacy")
    return "done"
  }
  if (voice === null) {
    ctx.notice("warn", "voice is off (--no-voice)")
    return "done"
  }
  return voiceSubcommand(sub, value, voice, ctx)
}

async function voiceSubcommand(sub: string, value: string, voice: VoiceController, ctx: CommandContext): Promise<CommandOutcome> {
  switch (sub) {
    case "use": {
      if (!isVoiceSourceId(value)) {
        ctx.notice("warn", "usage: /voice use lunarvoice|hex-app|hex-embedded")
        return "keep"
      }
      voice.use(value)
      ctx.notice("info", `voice: ${value} pinned for this session (/voice auto restores the order)`)
      return "done"
    }
    case "auto":
      voice.use("auto")
      ctx.notice("info", "voice: automatic source order")
      return "done"
    case "order": {
      const ids = parseVoiceIds(value)
      if (ids === null) {
        ctx.notice("warn", "usage: /voice order lunarvoice,hex-app,hex-embedded")
        return "keep"
      }
      voice.setOrder(ids)
      ctx.notice("info", `voice order: ${ids.join(", ")}`)
      return "done"
    }
    case "device":
      return deviceCommand(value, voice, ctx)
    case "retry":
      if (!voice.retry()) ctx.notice("info", "no voice clip is kept for a retry")
      return "done"
    case "test":
      ctx.notice("info", "voice test: press F9 and watch the meter next to the mic; nothing is transcribed until you stop")
      return "done"
    default:
      ctx.notice("warn", "usage: /voice [status|use <id>|auto|order <ids>|device [name]|test|privacy|retry]")
      return "keep"
  }
}

async function deviceCommand(value: string, voice: VoiceController, ctx: CommandContext): Promise<CommandOutcome> {
  if (value === "") {
    await ctx.showDevices()
    return "done"
  }
  const name = value === '""' || value === "''" ? "" : value
  const ok = await voice.selectDevice(name)
  if (name === "") ctx.notice("info", "voice device: back to the default source")
  else if (ok) ctx.notice("info", `voice device: ${name} (saved for this profile)`)
  else ctx.notice("warn", `no capture device matches "${name}" (/voice device lists them)`)
  return ok || name === "" ? "done" : "keep"
}
