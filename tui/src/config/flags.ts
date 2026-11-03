/**
 * Command-line flags (design 3.11.1), the name rules shared by flags, environment and config.toml (R25), and
 * ConfigError, the one error every config module throws for a problem the user must fix (exit status 2).
 *
 * Tokens are never accepted on the command line: --token, --gateway-token and --token=<v> anywhere in argv
 * exit 2 before anything else is parsed. No message ever repeats an argument's value, because a mistyped flag
 * or a stray positional argument may be a token.
 */
import type { ThreadSpec } from "../transport/types.ts"
import { isVoiceSourceId, type VoiceSourceId } from "../voice/types.ts"
import type { ImageProtocolChoice, TransportChoice } from "./types.ts"

// ---------------------------------------------------------------------------------------------------------
// Errors and display
// ---------------------------------------------------------------------------------------------------------

/** A configuration problem the user must fix. main.tsx prints `message` to stderr and exits with `exitCode`. */
export class ConfigError extends Error {
  readonly exitCode: number

  constructor(message: string, options: { exitCode?: number } = {}) {
    super(message)
    this.name = "ConfigError"
    this.exitCode = options.exitCode ?? 2
  }
}

export function isConfigError(value: unknown): value is ConfigError {
  return value instanceof ConfigError
}

// Controls, bidi controls and invisible format characters: escaped so a hostile file or argument name cannot
// move the cursor or reorder the message on the user's terminal.
const UNSAFE_DISPLAY = /[\u0000-\u001f\u007f-\u009f\u00ad\u061c\u200b-\u200f\u2028-\u202e\u2060-\u206f\ufeff]/g

/** Makes an untrusted string (a path, a file name, a flag name) safe to print: escapes controls, truncates. */
export function displayValue(text: string, max = 160): string {
  const escaped = text.replace(UNSAFE_DISPLAY, (c) => `\\u{${(c.codePointAt(0) ?? 0).toString(16)}}`)
  return escaped.length > max ? `${escaped.slice(0, max - 3)}...` : escaped
}

// ---------------------------------------------------------------------------------------------------------
// Names and choices
// ---------------------------------------------------------------------------------------------------------

/** R25. Profile names become file names (<profile>.json, <profile>.lock, tokens/<profile>.token). */
export const PROFILE_NAME_PATTERN = /^[a-z0-9_-]{1,32}$/
/** mt-admin tenant names (sanitize_name: [a-z0-9-], no leading or trailing dash; Unix account names). */
export const TENANT_NAME_PATTERN = /^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$/

export function isValidProfileName(value: unknown): value is string {
  return typeof value === "string" && PROFILE_NAME_PATTERN.test(value)
}

export function isValidTenantName(value: unknown): value is string {
  return typeof value === "string" && TENANT_NAME_PATTERN.test(value)
}

/**
 * The value as quoted in a message: only for config.toml sources (`show`). Values from argv or the environment
 * are never repeated, because a mistyped one may be a token.
 */
function quoted(value: string, show: boolean): string {
  return show ? `"${displayValue(value, 40)}"` : "the value"
}

export function assertProfileName(value: string, source: string, show = false): string {
  if (!isValidProfileName(value)) {
    throw new ConfigError(`${source}: profile name ${quoted(value, show)} must match ^[a-z0-9_-]{1,32}$`)
  }
  return value
}

export function assertTenantName(value: string, source: string, show = false): string {
  if (!isValidTenantName(value)) {
    throw new ConfigError(`${source}: ${quoted(value, show)} is not a tenant name (lowercase letters, digits and -)`)
  }
  return value
}

export const TRANSPORT_CHOICES: readonly TransportChoice[] = ["gateway", "socket", "auto"]
export const IMAGE_PROTOCOL_CHOICES: readonly ImageProtocolChoice[] = ["auto", "kitty", "sixel", "blocks"]

export function parseChoice<T extends string>(raw: string, choices: readonly T[]): T | null {
  return choices.find((choice) => choice === raw) ?? null
}

function requireChoice<T extends string>(raw: string, choices: readonly T[], source: string): T {
  const value = parseChoice(raw, choices)
  if (value === null) throw new ConfigError(`${source} must be one of ${choices.join(", ")}`)
  return value
}

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i

/** "last" | "new" | "assistant" | a thread UUID (lowercased). Shared by --thread and config.toml thread. */
export function parseThreadSpec(raw: string, source: string): ThreadSpec {
  if (raw === "last" || raw === "new" || raw === "assistant") return { kind: raw }
  if (UUID_PATTERN.test(raw)) return { kind: "id", id: raw.toLowerCase() }
  throw new ConfigError(`${source} must be last, new, assistant or a thread UUID`)
}

/** "lunarvoice,hex-app" or a config array: known ids only, duplicates dropped, at least one. */
export function parseVoiceOrder(ids: readonly string[], source: string, show = false): VoiceSourceId[] {
  const order: VoiceSourceId[] = []
  for (const raw of ids) {
    const id = raw.trim()
    if (!isVoiceSourceId(id)) {
      throw new ConfigError(`${source}: ${quoted(id, show)} is not a voice source (lunarvoice, hex-app, hex-embedded)`)
    }
    if (!order.includes(id)) order.push(id)
  }
  if (order.length === 0) throw new ConfigError(`${source} must name at least one voice source`)
  return order
}

// "f9", "ctrl+space", "alt+shift+m": modifiers joined with "+", then one key name (keymap.ts interprets it).
const PTT_KEY_PATTERN = /^(?:(?:ctrl|alt|shift|meta|super|hyper)\+){0,4}[a-z0-9]{1,16}$/

/** Lowercases and validates one push-to-talk key name. */
export function parsePttKey(raw: string, source: string, show = false): string {
  const key = raw.trim().toLowerCase()
  if (!PTT_KEY_PATTERN.test(key)) {
    throw new ConfigError(`${source}: ${quoted(raw, show)} is not a key name (for example f9 or ctrl+space)`)
  }
  return key
}

// ---------------------------------------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------------------------------------

export interface ParsedFlags {
  help: boolean
  version: boolean
  tenant?: string
  profile?: string
  /** Raw paths as given (config.ts expands ~ and resolves them against the working directory). */
  config?: string
  transport?: TransportChoice
  gatewayUrl?: string
  tokenFile?: string
  socket?: string
  thread?: ThreadSpec
  imageProtocol?: ImageProtocolChoice
  voiceOrder?: readonly VoiceSourceId[]
  noVoice: boolean
  /** --ptt-key, repeatable; empty when not given. */
  pttKeys: readonly string[]
  /** Present when --debug-log was given; path null means the default <state dir>/debug.log. */
  debugLog?: { path: string | null }
}

export const TOKEN_ON_ARGV_MESSAGE =
  "tokens are not accepted on the command line (visible in ps and /proc); use a 0600 token file (--token-file) or LUNARWING_GATEWAY_TOKEN"

const TOKEN_FLAGS: ReadonlySet<string> = new Set(["--token", "--gateway-token"])

type FlagKind = "value" | "bool" | "optional"

const FLAG_KINDS: ReadonlyMap<string, FlagKind> = new Map<string, FlagKind>([
  ["--tenant", "value"],
  ["--profile", "value"],
  ["--config", "value"],
  ["--transport", "value"],
  ["--gateway-url", "value"],
  ["--token-file", "value"],
  ["--socket", "value"],
  ["--thread", "value"],
  ["--image-protocol", "value"],
  ["--voice-order", "value"],
  ["--ptt-key", "value"],
  ["--no-voice", "bool"],
  ["--help", "bool"],
  ["-h", "bool"],
  ["--version", "bool"],
  ["--debug-log", "optional"],
])

/** The part of an argument before "=": all that may ever be printed of it. */
function flagName(arg: string): string {
  const eq = arg.indexOf("=")
  return eq >= 0 ? arg.slice(0, eq) : arg
}

function rejectTokenFlags(argv: readonly string[]): void {
  if (argv.some((arg) => TOKEN_FLAGS.has(flagName(arg)))) throw new ConfigError(TOKEN_ON_ARGV_MESSAGE)
}

interface Cursor {
  readonly argv: readonly string[]
  index: number
}

/** The value of a value flag: inline (--x=v) or the next argument, which must not itself be an option. */
function takeValue(cursor: Cursor, name: string, inline: string | undefined): string {
  let value = inline
  if (value === undefined) {
    const next = cursor.argv[cursor.index + 1]
    if (next === undefined || next.startsWith("--")) throw new ConfigError(`${name} needs a value`)
    cursor.index += 1
    value = next
  }
  if (value === "") throw new ConfigError(`${name} needs a non-empty value`)
  return value
}

/** --debug-log [<path>]: a following argument is its path unless it is an option. */
function takeOptionalValue(cursor: Cursor, inline: string | undefined): string | null {
  if (inline !== undefined) return inline === "" ? null : inline
  const next = cursor.argv[cursor.index + 1]
  if (next === undefined || next.startsWith("-")) return null
  cursor.index += 1
  return next
}

function applyValue(flags: ParsedFlags, name: string, value: string): void {
  switch (name) {
    case "--tenant":
      flags.tenant = assertTenantName(value, "--tenant")
      return
    case "--profile":
      flags.profile = assertProfileName(value, "--profile")
      return
    case "--config":
      flags.config = value
      return
    case "--transport":
      flags.transport = requireChoice(value, TRANSPORT_CHOICES, "--transport")
      return
    case "--gateway-url":
      flags.gatewayUrl = value
      return
    case "--token-file":
      flags.tokenFile = value
      return
    case "--socket":
      flags.socket = value
      return
    default:
      applyVoiceOrDisplayValue(flags, name, value)
  }
}

function applyVoiceOrDisplayValue(flags: ParsedFlags, name: string, value: string): void {
  switch (name) {
    case "--thread":
      flags.thread = parseThreadSpec(value, "--thread")
      return
    case "--image-protocol":
      flags.imageProtocol = requireChoice(value.trim(), IMAGE_PROTOCOL_CHOICES, "--image-protocol")
      return
    case "--voice-order":
      flags.voiceOrder = parseVoiceOrder(value.split(","), "--voice-order")
      return
    case "--ptt-key":
      flags.pttKeys = [...flags.pttKeys, parsePttKey(value, "--ptt-key")]
      return
    default:
      throw new ConfigError(`internal: unhandled flag ${name}`)
  }
}

function applyBool(flags: ParsedFlags, name: string): void {
  if (name === "--no-voice") flags.noVoice = true
  else if (name === "--version") flags.version = true
  else flags.help = true
}

function consume(cursor: Cursor, flags: ParsedFlags): void {
  const arg = cursor.argv[cursor.index] ?? ""
  if (!arg.startsWith("-") || arg === "-" || arg === "--") {
    throw new ConfigError("unexpected argument (lunarwing-tui takes options only; see --help)")
  }
  const name = flagName(arg)
  const inline = name.length < arg.length ? arg.slice(name.length + 1) : undefined
  const kind = FLAG_KINDS.get(name)
  if (kind === undefined) throw new ConfigError(`unknown option ${displayValue(name, 40)} (see --help)`)
  if (kind === "bool") {
    if (inline !== undefined) throw new ConfigError(`${name} takes no value`)
    applyBool(flags, name)
  } else if (kind === "optional") {
    flags.debugLog = { path: takeOptionalValue(cursor, inline) }
  } else {
    applyValue(flags, name, takeValue(cursor, name, inline))
  }
}

/** Parses argv (process.argv.slice(2)). Throws ConfigError (exit 2) on any problem. Repeated flags: last wins. */
export function parseFlags(argv: readonly string[]): ParsedFlags {
  rejectTokenFlags(argv)
  const flags: ParsedFlags = { help: false, version: false, noVoice: false, pttKeys: [] }
  const cursor: Cursor = { argv, index: 0 }
  for (; cursor.index < argv.length; cursor.index += 1) consume(cursor, flags)
  return flags
}

export function usageText(): string {
  return [
    "Usage: lunarwing-tui [options]",
    "",
    "Connection:",
    "  --tenant <name>                 tenant gateway from tenants/<name>.json (mt-admin tui-access)",
    "  --profile <name>                profile from config.toml",
    "  --config <path>                 config file (default: <config dir>/config.toml)",
    "  --transport gateway|socket|auto",
    "  --gateway-url <url>             non-tenant gateway (plain http only to loopback)",
    "  --token-file <path>             0600 token file for a non-tenant gateway",
    "  --socket <path>                 Unix socket of a same-uid daemon",
    "  --thread last|new|assistant|<uuid>",
    "Display:",
    "  --image-protocol auto|kitty|sixel|blocks",
    "Voice:",
    "  --voice-order <id,...>          lunarvoice, hex-app, hex-embedded",
    "  --no-voice",
    "  --ptt-key <key>                 push-to-talk key, repeatable (default: f9, ctrl+space)",
    "Diagnostics:",
    "  --debug-log [<path>]            metadata-only 0600 debug log (default: <state dir>/debug.log)",
    "  --help, --version",
    "",
    "Tokens are never accepted on the command line: use a 0600 token file or LUNARWING_GATEWAY_TOKEN.",
  ].join("\n")
}
