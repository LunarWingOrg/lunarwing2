/**
 * Info panel content (design 3.12.3, UX-04): buildPanel(kind, state, env) returns the panel's lines, wrapped to
 * env.width columns. Pure, so live panels (voice) follow the state and every panel is testable without a
 * renderer. Output longer than one line (help, /voice, /transport, /thread list, full approval text) goes here,
 * never to notices (3.6.3).
 *
 * Asynchronous data (capture devices, the thread list) is fetched by the command that opens the panel and passed
 * in through env; secrets never are (the endpoint label and token description never contain a token).
 */
import type { AppState, PanelKind } from "../state/model.ts"
import { approvalPosition, focusedApproval } from "../state/correlate.ts"
import { fullApprovalLines } from "../state/approval-measure.ts"
import type { Capabilities, Limits, ServerInfo } from "../transport/types.ts"
import type { CaptureDevice, VoiceSourceStatus, VoiceStatusReport } from "../voice/types.ts"
import type { ImageProtocolStatus } from "../images/protocol.ts"
import { formatBytes } from "../images/limits.ts"
import { imageStorageNotice } from "../images/attach.ts"
import { truncatedNotice } from "./approval-reply.ts"
import { KEYMAP, KKP_ONLY_LEGEND, KKP_ONLY_MARK, keyLabel, pttLabel, type KeymapEntry } from "./keymap.ts"
import { cellWidth, formatClock, formatSeconds, truncate, wrapText } from "../util.ts"

export interface ThreadRow {
  id: string
  title: string | null
  state: string
  turnCount: number
  updatedAt: string
  assistant: boolean
}

export interface PanelEnv {
  /** Text columns inside the panel. */
  width: number
  now: number
  /** Normalized voice.pttKeys ("f9", "ctrl+space"). */
  pttKeys: readonly string[]
  /** null with --no-voice or before the voice controller exists. */
  voice: VoiceStatusReport | null
  /** For the devices panel; null while loading or when the recorder cannot list devices. */
  devices?: readonly CaptureDevice[] | null
  /** For the threads panel; null when this transport cannot list threads. */
  threads?: readonly ThreadRow[] | null
  /** Header and /transport (images/protocol.ts describeImageProtocol). */
  imageProtocol?: ImageProtocolStatus | null
  /** How the listener owner is checked, e.g. "uid 1010 (derpy)" or null for a remote URL. */
  listenerOwner?: string | null
  /** Where the token comes from (TokenSource.describe()), never the token. */
  tokenSource?: string | null
  /** [ui] vision_verified_tenants contains this tenant (6.5). */
  visionVerified?: boolean
}

const PANEL_TITLES: Readonly<Record<PanelKind, string>> = {
  help: "Help",
  voice: "Voice",
  privacy: "Privacy",
  devices: "Capture devices",
  transport: "Transport",
  threads: "Threads",
  approval: "Approval",
}

export function panelTitle(kind: PanelKind): string {
  return PANEL_TITLES[kind]
}

export function buildPanel(kind: PanelKind, state: AppState, env: PanelEnv): string[] {
  const lines = rawPanel(kind, state, env)
  return lines.flatMap((line) => (line.length === 0 ? [""] : wrapIndented(line, env.width)))
}

function rawPanel(kind: PanelKind, state: AppState, env: PanelEnv): string[] {
  switch (kind) {
    case "help":
      return helpLines(state, env)
    case "voice":
      return voiceLines(env)
    case "privacy":
      return privacyLines(state, env)
    case "devices":
      return deviceLines(env)
    case "transport":
      return transportLines(state, env)
    case "threads":
      return threadLines(state, env)
    case "approval":
      return approvalLines(state, env)
  }
}

/** Wraps keeping a line's leading indentation on its continuation lines. */
function wrapIndented(line: string, width: number): string[] {
  if (cellWidth(line) <= width) return [line]
  const indent = /^ */u.exec(line)?.[0] ?? ""
  const body = line.slice(indent.length)
  const room = Math.max(8, width - indent.length)
  return wrapText(body, room).map((part) => indent + part)
}

// ---------------------------------------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------------------------------------

/** The push-to-talk line for the detected environment (3.12.3). */
/** The push-to-talk sentence when dictation cannot start (the mic reads "(no mic)"). */
export function pttUnavailableHelp(report: Pick<VoiceStatusReport, "enabled"> | null): string {
  if (report === null || !report.enabled) return "off (started with --no-voice)"
  return "no microphone or voice source is available (/voice shows why)"
}

export function pttHelp(state: Pick<AppState, "ui">, pttKeys: readonly string[]): string {
  const key = pttLabel(pttKeys)
  if (state.ui.tmux) return `tap ${key} to start and stop; holding ${key} also works (key repeat); or hold the mic with the mouse`
  if (state.ui.kkpRelease) return `hold ${key} to talk and release to stop; a short tap starts and stops; or hold the mic with the mouse`
  return `tap ${key} to start and stop; holding ${key} works through key repeat; or hold the mic with the mouse`
}

const COMMAND_HELP: readonly (readonly [string, string])[] = [
  ["/help", "this help (//help asks the daemon)"],
  ["/img <path>, /attach <path>", "attach an image file"],
  ["/shot [full]", "attach a screenshot of a region (or of one output)"],
  ["/image open [n], /image save [n] [path]", "open or save the newest (n-th newest) generated image"],
  ["/copy [n]", "copy the newest (n-th newest) reply"],
  ["/voice [status|use <id>|auto|order <ids>|device [name]|test|privacy|retry]", "voice sources and devices"],
  ["/transport [status|reconnect]", "connection details; reconnect now"],
  ["/interrupt, /stop, /steer <note>", "stop the running turn (or cancel a message that shows \"no reply yet\"); add a note to it"],
  ["/approve, /always, /deny, /approval view", "answer or view the focused approval"],
  ["/thread [list|new|<id prefix>]", "threads of this gateway"],
  ["/clear, /new", "forwarded to the daemon"],
  ["//<text>", "send /<text> to the daemon"],
  ["/quit, /exit", "quit the TUI (never sent; /shutdown and /restart are refused)"],
]

/** "  <label>  <text>" with the text wrapped under itself (a hanging indent), so long help keeps its columns. */
function hangingLines(label: string, text: string, labelWidth: number, width: number): string[] {
  const indent = 2 + labelWidth + 1
  const parts = wrapText(text, Math.max(12, width - indent))
  return parts.map((part, index) => (index === 0 ? `  ${label.padEnd(labelWidth)} ${part}` : `${" ".repeat(indent)}${part}`))
}

/** "Ctrl+S, Ctrl+Enter (KKP)": a key that needs the kitty keyboard protocol carries the mark. */
function keyLabels(entry: KeymapEntry, env: PanelEnv): string {
  if (entry.action === "ptt") return env.pttKeys.map(keyLabel).join(", ")
  const kkpOnly = entry.kkpOnly ?? []
  return entry.keys.map((key) => (kkpOnly.includes(key) ? `${key} ${KKP_ONLY_MARK}` : key)).join(", ")
}

function helpLines(state: AppState, env: PanelEnv): string[] {
  const labels = KEYMAP.map((entry) => keyLabels(entry, env))
  const labelWidth = Math.min(36, Math.max(...labels.map((label) => label.length)))
  const keys = KEYMAP.flatMap((entry, index) => hangingLines(labels[index] ?? "", entry.help, labelWidth, env.width))
  const commands = COMMAND_HELP.flatMap(([command, help]) => wrapIndented(`  ${command} - ${help}`, env.width))
  return [
    "Keys",
    ...keys,
    ...wrapIndented(`  ${KKP_ONLY_LEGEND}`, env.width),
    "",
    "Push-to-talk",
    ...wrapIndented(`  ${pttHelp(state, env.pttKeys)}`, env.width),
    "",
    "Commands",
    ...commands,
    "",
    "Copy: drag over transcript text to copy it (kitty: Shift+drag uses the terminal's own selection).",
    "PageUp/PageDown scroll this panel, Esc closes it.",
  ]
}

// ---------------------------------------------------------------------------------------------------------
// Voice, privacy, devices
// ---------------------------------------------------------------------------------------------------------

function sourceLine(source: VoiceSourceStatus, pinned: boolean): string {
  const mark = pinned ? "*" : " "
  if (!source.inBuild) return ` ${mark} ${source.id}: not in this build`
  const availability = source.availability
  const status =
    availability === null
      ? "not checked yet"
      : availability.available
        ? `available${availability.detail === undefined ? "" : ` (${availability.detail})`}`
        : `unavailable: ${availability.reason ?? "unknown reason"}`
  const limit = source.maxWavBytes === null || source.maxWavBytes === 0 ? "" : `, up to ${formatBytes(source.maxWavBytes)}`
  return ` ${mark} ${source.id} (${source.label}): ${status}${limit}`
}

function voiceLines(env: PanelEnv): string[] {
  const report = env.voice
  if (report === null) return ["Voice is off (--no-voice)."]
  const lines = [
    `Voice: ${report.enabled ? "on" : "off (--no-voice)"}`,
    `Order: ${report.order.join(", ")}${report.pinned === null ? " (automatic)" : ` (pinned: ${report.pinned}; /voice auto restores the order)`}`,
    "Sources:",
    ...report.sources.map((source) => sourceLine(source, source.id === report.pinned)),
    `Recorder: ${report.recorder.kind ?? "none"}${report.recorder.reason === undefined ? "" : ` (${report.recorder.reason})`}`,
    `Device: ${report.device.name ?? "default source"} (${report.device.origin})`,
    `Longest recording: ${formatClock(report.maxDurationMs)}`,
  ]
  const retained = report.retained
  if (retained !== null) {
    const wait = retained.retryAt !== null && retained.retryAt > env.now ? `, retry possible in ${formatSeconds(retained.retryAt - env.now)}` : ""
    lines.push(`Kept clip: ${formatSeconds(retained.durationMs)} (${retained.reason})${wait} - Alt+R retry, Esc discard`)
  }
  if (report.lastAttempts.length > 0) {
    const attempts = report.lastAttempts.map((attempt) => `${attempt.source} ${attempt.outcome} ${attempt.ms} ms`)
    lines.push(`Last attempts: ${attempts.join("; ")}`)
  }
  lines.push("", "/voice use <id> pins a source, /voice order <ids> sets the order, /voice device lists devices.")
  return lines
}

/** The source the next clip goes to first: the pinned one, else the first available in order. */
function activeSource(report: VoiceStatusReport | null): string | null {
  if (report === null) return null
  if (report.pinned !== null) return report.pinned
  const available = report.order.find((id) => report.sources.some((source) => source.id === id && source.availability?.available === true))
  return available ?? null
}

function privacyLines(state: AppState, env: PanelEnv): string[] {
  const active = activeSource(env.voice)
  const sources = (env.voice?.sources ?? [])
    .filter((source) => source.inBuild)
    .map((source) => `${source.id === active ? "*" : " "} ${source.label}: ${source.privacyNote}`)
  return [
    "What persists where (* = the voice backend in use):",
    "  TUI recorder: nothing on disk; audio exists only in memory and is zero-filled after use; one failed clip is kept for at most 10 min for Alt+R.",
    ...sources.map((line) => ` ${line}`),
    `  Images: ${imageStorageNotice(state.tenant)}; every image you send (clipboard, /img, drag-and-drop, /shot) counts.`,
    "  Clipboard: cliphist records image and text copies on this host, including the TUI's own copies.",
    "  Transcripts are inserted into the composer only; they never touch the clipboard or the debug log.",
  ]
}

function deviceLines(env: PanelEnv): string[] {
  const devices = env.devices
  if (devices === undefined || devices === null) return ["Capture devices are not available (no OpenTUI audio engine, or still loading)."]
  if (devices.length === 0) return ["No capture devices found."]
  const current = env.voice?.device.name ?? null
  return [
    ...devices.map((device) => {
      const mark = device.name === current ? "*" : " "
      return `${mark} ${device.index}: ${device.name}${device.isDefault ? " (default)" : ""}`
    }),
    "",
    "/voice device <name> selects a device for this profile; /voice device \"\" goes back to the default.",
  ]
}

// ---------------------------------------------------------------------------------------------------------
// Transport and threads
// ---------------------------------------------------------------------------------------------------------

function connLine(state: AppState, now: number): string {
  const conn = state.conn
  switch (conn.kind) {
    case "connecting":
      return `connecting (attempt ${conn.attempt})`
    case "ready":
      return `ready for ${formatSeconds(now - conn.since)}`
    case "reconnecting":
      return `reconnecting (attempt ${conn.attempt}) in ${formatSeconds(Math.max(0, conn.retryAt - now))}: ${conn.lastError}`
    case "offline":
      return `offline${conn.fatal ? " (needs action)" : ""}: ${conn.reason}`
  }
}

function yesNo(value: boolean | "unknown"): string {
  return value === "unknown" ? "unknown" : value ? "yes" : "no"
}

function capabilityLines(caps: Capabilities): string[] {
  return [
    `  streaming ${yesNo(caps.streaming)}, activity ${yesNo(caps.activity)}, images ${yesNo(caps.images)}, generated images ${yesNo(caps.generatedImages)}`,
    `  approvals ${yesNo(caps.approvals)}, transcription ${yesNo(caps.transcribe)}, correlation ${caps.correlation ? "exact" : "FIFO (legacy daemon)"}, lifecycle ${yesNo(caps.lifecycle)}`,
    `  interrupt ${caps.interrupt}, steer ${caps.steer}, threads ${yesNo(caps.threads)}, history ${yesNo(caps.history)}, resume ${yesNo(caps.resume)}`,
  ]
}

function limitLines(limits: Limits): string[] {
  const transcribe =
    limits.maxTranscribeWavBytes > 0
      ? `${formatBytes(limits.maxTranscribeWavBytes)} WAV${limits.transcribePerMinute === null ? "" : `, ${limits.transcribePerMinute}/min`}`
      : "none"
  return [
    `  message ${formatBytes(limits.maxContentBytes)}, images ${limits.maxImages} x ${formatBytes(limits.maxImageBytes)} (${formatBytes(limits.maxTotalImageBytes)} total), request ${formatBytes(limits.maxRequestBytes)}`,
    `  transcription ${transcribe}, chat ${limits.chatPerMinute === null ? "unlimited" : `${limits.chatPerMinute}/min`}, in flight ${limits.maxInFlight}`,
  ]
}

function serverLines(server: ServerInfo, state: AppState, env: PanelEnv): string[] {
  const credential = server.credential === "unknown" ? "unknown" : `${server.credential} token`
  const lines = [
    `Endpoint: ${server.endpointLabel}`,
    `Credential: ${credential}${env.tokenSource ? ` (${env.tokenSource})` : ""}`,
    `Listener owner: ${env.listenerOwner ?? "not verified (remote URL)"}`,
    `Protocol: ${server.protocol}${server.protocolVersion === null ? "" : ` v${server.protocolVersion}`}${server.serverVersion === null ? "" : `, server ${server.serverVersion}`}`,
    `Engine: ${server.engine}`,
    "Capabilities:",
    ...capabilityLines(server.capabilities),
    "Limits:",
    ...limitLines(server.limits),
  ]
  if (server.capabilities.generatedImages !== true) lines.push("generated images: not delivered on this engine")
  if (state.tenant !== null && env.visionVerified !== true) {
    lines.push(`vision: images go to the configured model; vision is not verified for ${state.tenant}`)
  }
  return lines
}

function transportLines(state: AppState, env: PanelEnv): string[] {
  const lines = [
    `Profile: ${state.profile}${state.tenant === null ? "" : `, tenant ${state.tenant}`}`,
    `Connection: ${connLine(state, env.now)}`,
    `Thread: ${state.threadId ?? "none yet"}${state.otherThreadsUnread > 0 ? ` (${state.otherThreadsUnread} events in other threads)` : ""}`,
  ]
  if (state.server !== null) lines.push(...serverLines(state.server, state, env))
  const protocol = env.imageProtocol
  if (protocol !== undefined && protocol !== null) {
    lines.push(`Images: ${protocol.label}`)
    for (const note of protocol.notes) lines.push(`  ${note}`)
  }
  const warnings = state.server?.warnings ?? []
  if (warnings.length > 0) lines.push("Warnings:", ...warnings.map((warning) => `  ${warning}`))
  lines.push("", "/transport reconnect connects again now.")
  return lines
}

function threadLines(state: AppState, env: PanelEnv): string[] {
  const threads = env.threads
  if (threads === undefined || threads === null) {
    return [`Current thread: ${state.threadId ?? "none yet"}`, "", "This transport cannot list threads; /thread new starts a new one."]
  }
  if (threads.length === 0) return ["No threads."]
  return [
    ...threads.map((thread) => {
      const mark = thread.id === state.threadId ? "*" : " "
      const title = thread.title === null ? "" : ` ${truncate(thread.title, 40)}`
      const kind = thread.assistant ? " [assistant]" : ""
      return `${mark} ${thread.id.slice(0, 8)}${title}${kind} - ${thread.state}, ${thread.turnCount} turns, ${thread.updatedAt.slice(0, 16).replace("T", " ")}`
    }),
    "",
    "/thread <id prefix> switches, /thread new starts a new thread.",
  ]
}

// ---------------------------------------------------------------------------------------------------------
// Approval (full view, Alt+V)
// ---------------------------------------------------------------------------------------------------------

function approvalLines(state: AppState, env: PanelEnv): string[] {
  const id = state.panel?.approvalId
  const prompt = (id === undefined ? undefined : state.approvals[id]) ?? focusedApproval(state)
  if (prompt === null || prompt === undefined) return ["No approval is pending."]
  const position = approvalPosition(state, prompt.id)
  const counter = position === null || state.approvalQueue.length < 2 ? "" : ` (${position}/${state.approvalQueue.length})`
  const full = fullApprovalLines(prompt, Math.max(10, env.width - 2))
  // Keys answer only a pending prompt on screen; a settled one (answered here or elsewhere) offers none.
  const pending = state.approvalQueue.includes(prompt.id)
  const actions = !pending
    ? "no longer pending - Esc closes this view"
    : prompt.truncated
      ? "[Alt+N] deny"
      : `[Alt+Y] approve${prompt.allowAlways ? "  [Alt+A] always" : ""}  [Alt+N] deny`
  const lines = [
    `Tool: ${prompt.toolName}${counter} - ${prompt.state}`,
    "Description:",
    ...full.description.map((line) => `  ${line}`),
    "Parameters:",
    ...full.parameters.map((line) => `  ${line}`),
    "",
  ]
  if (prompt.truncated) lines.push(truncatedNotice(prompt))
  lines.push(actions)
  return lines
}
