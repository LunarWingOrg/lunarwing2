/**
 * Configuration (design 3.11). Precedence: flags > environment > config-file profile > config-file globals >
 * defaults (DEFAULT_UI_CONFIG, DEFAULT_VOICE_CONFIG in config/types.ts).
 *
 * loadConfig() is the one call main.tsx makes: it parses argv (tokens on argv exit 2), takes
 * LUNARWING_GATEWAY_TOKEN out of the environment, reads config.toml (refused unless owned by the user and not
 * group- or world-writable, because it names token files and commands), picks the profile and the connection
 * target (config/tenant-select.ts, asking the tenant picker when several tenants are configured), builds the
 * TokenSource, the expected listener owner (3.8.1) and the UI and voice settings, and takes the per-profile
 * instance lock. Every problem the user must fix is a ConfigError (exit status 2).
 *
 * Profile name: --profile, then LUNARWING_TUI_PROFILE; otherwise --tenant <t> uses profile <t>; otherwise
 * default_profile, unless a flag or LUNARWING_GATEWAY_URL already names the target; otherwise a tenant picked
 * automatically names the profile (3.11), and anything else uses "default".
 */
import { userInfo } from "node:os"
import { dirname } from "node:path"
import type { ThreadSpec } from "../transport/types.ts"
import type { VoiceSourceId } from "../voice/types.ts"
import {
  assertProfileName,
  assertTenantName,
  ConfigError,
  displayValue,
  IMAGE_PROTOCOL_CHOICES,
  parseChoice,
  parseFlags,
  parsePttKey,
  parseThreadSpec,
  parseVoiceOrder,
  TRANSPORT_CHOICES,
  usageText,
  type ParsedFlags,
} from "./flags.ts"
import { acquireProfileLock, type ProfileLock } from "./instance-lock.ts"
import {
  configFilePath,
  defaultDebugLogPath,
  defaultTokenFilePath,
  displayPath,
  envValue,
  parseGatewayUrl,
  resolveConfigDir,
  resolvePath,
  resolveRuntimeDir,
  resolveSocketPath,
  resolveStateDir,
} from "./paths.ts"
import type { MutableEnv } from "./proxy-env.ts"
import type { Secret } from "./secret.ts"
import { ensureStateDir, openStateFile } from "./state-file.ts"
import { openTenantTokenSource, tenantFromFileName, tuiAccessCommand, type TenantAccessInfo } from "./tenant-access.ts"
import { selectTarget, type ConfigLayer, type RequestedTarget, type TenantPicker } from "./tenant-select.ts"
import {
  currentUid,
  envTokenSource,
  GATEWAY_TOKEN_FILE_ENV,
  MissingFileError,
  openTokenFileSource,
  readCheckedFile,
  takeEnvToken,
  tokenDirectoryWarning,
} from "./token-file.ts"
import {
  DEFAULT_HEX_CONFIG,
  DEFAULT_UI_CONFIG,
  DEFAULT_VOICE_CONFIG,
  VOICE_MAX_DURATION_CAP_MS,
  type ColorMode,
  type HexConfig,
  type ImageProtocolChoice,
  type NotifyMode,
  type StateFile,
  type TokenSource,
  type TransportChoice,
  type UiConfig,
  type VoiceBlurPolicy,
  type VoiceConfig,
} from "./types.ts"

export { ConfigError, isConfigError } from "./flags.ts"
export type { ParsedFlags } from "./flags.ts"
export type { TenantChoice, TenantPicker } from "./tenant-select.ts"
export type { ProfileLock } from "./instance-lock.ts"

export const CONFIG_FILE_MAX_BYTES = 256 * 1024
export const DEFAULT_PROFILE = "default"

// ---------------------------------------------------------------------------------------------------------
// config.toml (3.11.2)
// ---------------------------------------------------------------------------------------------------------

/** [voice] and [voice.hex] values from one layer of the file. */
export interface VoiceOverrides extends Partial<Omit<VoiceConfig, "enabled" | "hex">> {
  hex?: Partial<HexConfig>
}

export interface ProfileSection {
  readonly name: string
  readonly transport?: TransportChoice
  readonly tenant?: string
  readonly gatewayUrl?: string
  /** Absolute (relative paths resolve against the config file's directory). */
  readonly tokenFile?: string
  readonly socket?: string
  readonly thread?: ThreadSpec
  readonly expectedUid?: number
  readonly allowInsecureHttp?: boolean
  readonly ui: Partial<UiConfig>
  readonly voice: VoiceOverrides
}

export interface ConfigFile {
  /** null when no config file was read. */
  readonly path: string | null
  readonly defaultProfile?: string
  readonly transport?: TransportChoice
  readonly thread?: ThreadSpec
  readonly allowInsecureHttp?: boolean
  readonly ui: Partial<UiConfig>
  readonly voice: VoiceOverrides
  /** A Map, so profile names such as "constructor" or "__proto__" stay plain keys. */
  readonly profiles: ReadonlyMap<string, ProfileSection>
  /** Unknown keys and clamped values. */
  readonly warnings: readonly string[]
}

export const EMPTY_CONFIG_FILE: ConfigFile = Object.freeze({ path: null, ui: {}, voice: {}, profiles: new Map(), warnings: [] })

interface ParseContext {
  /** The file as named in messages. */
  readonly source: string
  /** For relative paths in the file. */
  readonly baseDir: string
  readonly home: string
  readonly warnings: string[]
}

type Table = Record<string, unknown>

function isTable(value: unknown): value is Table {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

const CONTROL_CHARACTERS = /[\u0000-\u001f\u007f-\u009f]/

/** Reads one TOML table; remembers the keys it was asked for and warns about the rest. */
class TableReader {
  readonly #seen = new Set<string>()

  constructor(
    private readonly values: Table,
    readonly where: string,
    private readonly ctx: ParseContext,
  ) {}

  #label(key: string): string {
    return `${this.ctx.source}: ${[this.where, key].filter((part) => part !== "").join(" ")}`
  }

  /** Throws for `key` ("" names the table itself). */
  fail(key: string, problem: string): never {
    throw new ConfigError(`${this.#label(key)} ${problem}`)
  }

  warn(key: string, problem: string): void {
    this.ctx.warnings.push(`${this.#label(key)} ${problem}`)
  }

  take(key: string): unknown {
    this.#seen.add(key)
    return Object.hasOwn(this.values, key) ? this.values[key] : undefined
  }

  string(key: string, max = 4096): string | undefined {
    const value = this.take(key)
    if (value === undefined) return undefined
    if (typeof value !== "string" || value.length > max || CONTROL_CHARACTERS.test(value)) {
      this.fail(key, `must be a string of at most ${max} characters without control characters`)
    }
    return value
  }

  bool(key: string): boolean | undefined {
    const value = this.take(key)
    if (value !== undefined && typeof value !== "boolean") this.fail(key, "must be true or false")
    return value as boolean | undefined
  }

  int(key: string, min: number, max: number): number | undefined {
    const value = this.take(key)
    if (value === undefined) return undefined
    if (typeof value !== "number" || !Number.isSafeInteger(value) || value < min || value > max) {
      this.fail(key, `must be an integer from ${min} to ${max}`)
    }
    return value
  }

  choice<T extends string>(key: string, choices: readonly T[]): T | undefined {
    const value = this.take(key)
    if (value === undefined) return undefined
    const parsed = typeof value === "string" ? parseChoice(value, choices) : null
    if (parsed === null) this.fail(key, `must be one of ${choices.join(", ")}`)
    return parsed
  }

  strings(key: string, maxItems: number): string[] | undefined {
    const value = this.take(key)
    if (value === undefined) return undefined
    if (!Array.isArray(value) || value.length > maxItems || !value.every((v) => typeof v === "string")) {
      this.fail(key, `must be an array of at most ${maxItems} strings`)
    }
    return value as string[]
  }

  table(key: string): Table | undefined {
    const value = this.take(key)
    if (value !== undefined && !isTable(value)) this.fail(key, "must be a table")
    return value as Table | undefined
  }

  path(key: string): string | undefined {
    const value = this.string(key)
    if (value === undefined) return undefined
    if (value === "") this.fail(key, "must not be empty")
    return resolvePath(value, this.ctx.baseDir, this.ctx.home)
  }

  /** Warns about keys nobody asked for (forward-compatible: they are ignored). */
  finish(): void {
    for (const key of Object.keys(this.values)) {
      if (!this.#seen.has(key)) this.warn(displayValue(key, 40), "is not a known setting (ignored)")
    }
  }
}

/** Drops undefined-valued keys, so a spread never overrides a lower layer with undefined. */
function defined<T extends object>(value: T): T {
  return Object.fromEntries(Object.entries(value).filter(([, v]) => v !== undefined)) as T
}

const COLOR_MODES: readonly ColorMode[] = ["auto", "always", "never"]
const NOTIFY_MODES: readonly NotifyMode[] = ["approvals", "all", "off"]
const BLUR_POLICIES: readonly VoiceBlurPolicy[] = ["hold", "always", "never"]

function parseUi(reader: TableReader): Partial<UiConfig> {
  const tenants = reader.strings("vision_verified_tenants", 256)
  const ui = defined<Partial<UiConfig>>({
    imageProtocol: reader.choice("image_protocol", IMAGE_PROTOCOL_CHOICES),
    maxTurnsInMemory: reader.int("max_turns_in_memory", 10, 5000),
    color: reader.choice("color", COLOR_MODES),
    reduceMotion: reader.bool("reduce_motion"),
    cursorBlink: reader.bool("cursor_blink"),
    notify: reader.choice("notify", NOTIFY_MODES),
    visionVerifiedTenants: tenants?.map((t) => assertTenantName(t, `${reader.where} vision_verified_tenants`, true)),
  })
  reader.finish()
  return ui
}

function parseHex(reader: TableReader): Partial<HexConfig> {
  const command = reader.strings("command", 32)
  if (command !== undefined && (command.length === 0 || command.some((part) => part === "" || CONTROL_CHARACTERS.test(part)))) {
    reader.fail("command", "must be a non-empty array of non-empty strings (argv, no shell)")
  }
  const hex = defined<Partial<HexConfig>>({ command, model: reader.string("model", 128) })
  reader.finish()
  return hex
}

function parseMaxDuration(reader: TableReader): number | undefined {
  const seconds = reader.int("max_duration_s", 1, 3600)
  if (seconds === undefined) return undefined
  const ms = seconds * 1000
  if (ms <= VOICE_MAX_DURATION_CAP_MS) return ms
  reader.warn("max_duration_s", `${seconds} is above the ${VOICE_MAX_DURATION_CAP_MS / 1000} s cap; using the cap`)
  return VOICE_MAX_DURATION_CAP_MS
}

function parseVoice(reader: TableReader, ctx: ParseContext): VoiceOverrides {
  const order = reader.strings("order", 16)
  const keys = reader.strings("ptt_keys", 8)
  const device = reader.string("device", 128)
  const hexTable = reader.table("hex")
  const voice: VoiceOverrides = defined<VoiceOverrides>({
    order: order === undefined ? undefined : parseVoiceOrder(order, `${ctx.source}: ${reader.where} order`, true),
    pttKeys: keys?.map((k) => parsePttKey(k, `${ctx.source}: ${reader.where} ptt_keys`, true)),
    repeatDelayMs: reader.int("repeat_delay_ms", 50, 5000),
    maxDurationMs: parseMaxDuration(reader),
    autoStopOnSilence: reader.bool("auto_stop_on_silence"),
    silenceStopMs: reader.int("silence_stop_ms", 300, 60_000),
    blurPolicy: reader.choice("blur_policy", BLUR_POLICIES),
    device: device === undefined ? undefined : device === "" ? null : device,
    hex: hexTable === undefined ? undefined : parseHex(new TableReader(hexTable, `${reader.where.slice(0, -1)}.hex]`, ctx)),
  })
  reader.finish()
  return voice
}

function checkProfileTargets(reader: TableReader, section: ProfileSection): void {
  const targets = [section.tenant, section.gatewayUrl, section.socket].filter((t) => t !== undefined)
  if (targets.length > 1) reader.fail("", "sets more than one of tenant, gateway_url and socket")
  if (section.tenant !== undefined && section.tokenFile !== undefined) {
    reader.fail("token_file", "cannot be used with tenant (the tenant access file holds the token)")
  }
  if (section.tenant !== undefined && section.expectedUid !== undefined) {
    reader.fail("expected_uid", "cannot be used with tenant (the tenant access file holds tenant_uid)")
  }
  const wantsSocket = section.socket !== undefined
  const wantsGateway = section.tenant !== undefined || section.gatewayUrl !== undefined
  if ((section.transport === "socket" && wantsGateway) || (section.transport === "gateway" && wantsSocket)) {
    reader.fail("transport", `${section.transport} conflicts with the profile's ${wantsSocket ? "socket" : "gateway"} target`)
  }
}

function parseGatewayUrlSetting(reader: TableReader): string | undefined {
  const raw = reader.string("gateway_url", 2048)
  if (raw === undefined) return undefined
  const parsed = parseGatewayUrl(raw)
  if (!parsed.ok) reader.fail("gateway_url", parsed.reason)
  return parsed.url.href
}

function parseProfile(name: string, table: Table, ctx: ParseContext): ProfileSection {
  assertProfileName(name, `${ctx.source}: [profiles]`, true)
  const where = `[profiles.${name}]`
  const reader = new TableReader(table, where, ctx)
  const tenant = reader.string("tenant", 64)
  const thread = reader.string("thread", 64)
  const uiTable = reader.table("ui")
  const voiceTable = reader.table("voice")
  const section: ProfileSection = defined<ProfileSection>({
    name,
    transport: reader.choice("transport", TRANSPORT_CHOICES),
    tenant: tenant === undefined ? undefined : assertTenantName(tenant, `${ctx.source}: ${where} tenant`, true),
    gatewayUrl: parseGatewayUrlSetting(reader),
    tokenFile: reader.path("token_file"),
    socket: reader.path("socket"),
    thread: thread === undefined ? undefined : parseThreadSpec(thread, `${ctx.source}: ${where} thread`),
    expectedUid: reader.int("expected_uid", 0, 4_294_967_294),
    allowInsecureHttp: reader.bool("allow_insecure_http"),
    ui: uiTable === undefined ? {} : parseUi(new TableReader(uiTable, `[profiles.${name}.ui]`, ctx)),
    voice: voiceTable === undefined ? {} : parseVoice(new TableReader(voiceTable, `[profiles.${name}.voice]`, ctx), ctx),
  })
  checkProfileTargets(reader, section)
  reader.finish()
  return section
}

function parseProfiles(table: Table | undefined, ctx: ParseContext): Map<string, ProfileSection> {
  const profiles = new Map<string, ProfileSection>()
  for (const [name, value] of Object.entries(table ?? {})) {
    if (!isTable(value)) throw new ConfigError(`${ctx.source}: [profiles.${displayValue(name, 40)}] must be a table`)
    profiles.set(name, parseProfile(name, value, ctx))
  }
  return profiles
}

/** Validates a parsed config.toml. Exported for tests; loadConfig() reads the file with readConfigFile(). */
export function parseConfigToml(text: string, options: { source: string; baseDir: string; home: string; path?: string }): ConfigFile {
  let raw: unknown
  try {
    raw = Bun.TOML.parse(text)
  } catch (error) {
    const line = typeof error === "object" && error !== null && "position" in error ? lineOf(error.position) : ""
    const message = error instanceof Error ? error.message : "parse error"
    throw new ConfigError(`${options.source}${line}: ${displayValue(message, 120)}`)
  }
  const ctx: ParseContext = { source: options.source, baseDir: options.baseDir, home: options.home, warnings: [] }
  const reader = new TableReader(isTable(raw) ? raw : {}, "", ctx)
  const defaultProfile = reader.string("default_profile", 32)
  const thread = reader.string("thread", 64)
  const uiTable = reader.table("ui")
  const voiceTable = reader.table("voice")
  const file: ConfigFile = defined<ConfigFile>({
    path: options.path ?? null,
    defaultProfile: defaultProfile === undefined ? undefined : assertProfileName(defaultProfile, `${options.source}: default_profile`, true),
    transport: reader.choice("transport", TRANSPORT_CHOICES),
    thread: thread === undefined ? undefined : parseThreadSpec(thread, `${options.source}: thread`),
    allowInsecureHttp: reader.bool("allow_insecure_http"),
    ui: uiTable === undefined ? {} : parseUi(new TableReader(uiTable, "[ui]", ctx)),
    voice: voiceTable === undefined ? {} : parseVoice(new TableReader(voiceTable, "[voice]", ctx), ctx),
    profiles: parseProfiles(reader.table("profiles"), ctx),
    warnings: ctx.warnings,
  })
  if (file.defaultProfile !== undefined && !file.profiles.has(file.defaultProfile)) {
    throw new ConfigError(`${options.source}: default_profile "${file.defaultProfile}" has no [profiles.${file.defaultProfile}] table`)
  }
  reader.finish()
  return file
}

function lineOf(position: unknown): string {
  return isTable(position) && typeof position.line === "number" ? ` line ${position.line}` : ""
}

/**
 * Reads config.toml: owned by the user, not group- or world-writable ((mode & 0o022) === 0), at most 256 KiB.
 * A symbolic link is followed (dotfile managers) and its target is checked. A missing file is an empty
 * configuration unless it was named explicitly (`required`).
 */
export async function readConfigFile(path: string, options: { uid: number; home: string; required: boolean }): Promise<ConfigFile> {
  const source = displayValue(displayPath(path, options.home))
  let bytes: Uint8Array
  try {
    bytes = await readCheckedFile(path, {
      uid: options.uid,
      maxBytes: CONFIG_FILE_MAX_BYTES,
      label: `config file ${source}`,
      forbiddenModeBits: 0o022,
      followSymlinks: true,
    })
  } catch (error) {
    if (error instanceof MissingFileError && !options.required) return EMPTY_CONFIG_FILE
    throw error
  }
  return parseConfigToml(new TextDecoder().decode(bytes), { source, baseDir: dirname(path), home: options.home, path })
}

// ---------------------------------------------------------------------------------------------------------
// UI and voice (flags > environment > profile > globals > defaults)
// ---------------------------------------------------------------------------------------------------------

export interface LayerInput {
  readonly flags: ParsedFlags
  readonly env: Readonly<Record<string, string | undefined>>
  readonly file: ConfigFile
  readonly profile: ProfileSection | undefined
  readonly warnings: ConfigNotice[]
}

function envImageProtocol(input: LayerInput): ImageProtocolChoice | undefined {
  const raw = envValue(input.env, "LUNARWING_TUI_IMAGE_PROTOCOL")
  if (raw === undefined) return undefined
  const parsed = parseChoice(raw.trim(), IMAGE_PROTOCOL_CHOICES)
  if (parsed === null) {
    input.warnings.push(info(`ignoring LUNARWING_TUI_IMAGE_PROTOCOL (expected one of ${IMAGE_PROTOCOL_CHOICES.join(", ")})`))
  }
  return parsed ?? undefined
}

export function resolveUiConfig(input: LayerInput): UiConfig {
  const fromEnv: Partial<UiConfig> = defined({ imageProtocol: envImageProtocol(input) })
  const fromFlags: Partial<UiConfig> = defined({ imageProtocol: input.flags.imageProtocol })
  const ui: UiConfig = { ...DEFAULT_UI_CONFIG, ...input.file.ui, ...(input.profile?.ui ?? {}), ...fromEnv, ...fromFlags }
  return Object.freeze({ ...ui, visionVerifiedTenants: Object.freeze([...ui.visionVerifiedTenants]) })
}

export function resolveVoiceConfig(input: LayerInput): VoiceConfig {
  const { hex: fileHex, ...fileVoice } = input.file.voice
  const { hex: profileHex, ...profileVoice } = input.profile?.voice ?? {}
  const hexBin = envValue(input.env, "LUNARWING_TUI_HEX_BIN")
  const hex: HexConfig = {
    ...DEFAULT_HEX_CONFIG,
    ...(fileHex ?? {}),
    ...(profileHex ?? {}),
    ...(hexBin === undefined ? {} : { command: [hexBin] }),
  }
  const flags = input.flags
  const fromFlags: Partial<VoiceConfig> = defined({
    enabled: flags.noVoice ? false : undefined,
    order: flags.voiceOrder,
    pttKeys: flags.pttKeys.length > 0 ? flags.pttKeys : undefined,
  })
  const voice: VoiceConfig = {
    ...DEFAULT_VOICE_CONFIG,
    ...fileVoice,
    ...profileVoice,
    ...fromFlags,
    hex: Object.freeze({ ...hex, command: Object.freeze([...hex.command]) }),
  }
  return Object.freeze({
    ...voice,
    order: Object.freeze([...voice.order]) as readonly VoiceSourceId[],
    pttKeys: Object.freeze([...voice.pttKeys]),
  })
}

// ---------------------------------------------------------------------------------------------------------
// Resolved configuration
// ---------------------------------------------------------------------------------------------------------

/**
 * A configuration note for the notice area (notice/push with source "client"): unknown config keys, ignored
 * variables, skipped tenant files, overridden settings, clamped values. Never a token.
 */
export interface ConfigNotice {
  readonly level: "info" | "warn"
  readonly text: string
  readonly sticky: boolean
}

function info(text: string): ConfigNotice {
  return { level: "info", text, sticky: false }
}

export interface ExpectedListener {
  readonly uid: number
  readonly label: string
}

/**
 * A gateway connection target. Maps onto GatewayTransportOptions as baseUrl = url, token, expectedListener,
 * tenant = tenant?.tenant ?? null and allowInsecureHttp. The transport itself reports the sticky warnings that
 * follow from these values (listener owner not verified, plain http, master token) in ServerInfo.warnings
 * (3.7), so they are not repeated in ResolvedConfig.notices.
 */
export interface GatewayTarget {
  readonly transport: "gateway"
  /** Base URL, normalized to "<origin>/". */
  readonly url: URL
  readonly token: TokenSource
  /** The listener-owner check of 3.8.1; null only for a non-tenant, non-loopback URL. */
  readonly expectedListener: ExpectedListener | null
  /** The tenant access file's metadata (never the token); null for non-tenant gateways. */
  readonly tenant: TenantAccessInfo | null
  /** Plain http to a non-loopback host was allowed by allow_insecure_http (and is in use when the URL is http). */
  readonly allowInsecureHttp: boolean
}

export interface SocketTarget {
  readonly transport: "socket"
  readonly path: string
}

export type ConnectionTarget = GatewayTarget | SocketTarget

export interface ResolvedConfig {
  readonly profile: string
  /** False when another live instance holds the profile lock ("second window", 3.11.5). */
  readonly primary: boolean
  /** Release on quit. */
  readonly lock: ProfileLock
  readonly stateFile: StateFile
  readonly target: ConnectionTarget
  readonly thread: ThreadSpec
  /** True when --thread named the thread. */
  readonly threadExplicit: boolean
  /** True when the tenant came from the picker: the caller has already stored it as lastTenant. */
  readonly tenantPicked: boolean
  readonly ui: UiConfig
  readonly voice: VoiceConfig
  /** Absolute path, or null when the debug log is off. Open it with log.ts openDebugLog(). */
  readonly debugLogPath: string | null
  readonly dirs: { readonly config: string; readonly state: string; readonly runtime: string | null }
  readonly configFile: string | null
  readonly user: string
  readonly notices: readonly ConfigNotice[]
}

export interface LoadConfigOptions {
  /** process.argv.slice(2). */
  readonly argv: readonly string[]
  /** Default process.env. LUNARWING_GATEWAY_TOKEN is blanked in it. */
  readonly env?: MutableEnv
  readonly home?: string
  readonly uid?: number
  readonly user?: string
  readonly cwd?: string
  /** Clock for the tenant-file expiry check. */
  readonly now?: () => number
  /** Asked when several tenant files are valid (components/tenant-picker.tsx). */
  readonly pickTenant?: TenantPicker
  /** The socket transport (M4). Until then socket requests exit 2 and the automatic socket fallback is off. */
  readonly socketTransport?: boolean
}

export type LoadConfigResult =
  | { readonly kind: "ready"; readonly config: ResolvedConfig }
  /** --help or --version (output for stdout), or the tenant picker was cancelled (empty output). */
  | { readonly kind: "exit"; readonly code: number; readonly output: string }

interface Context {
  readonly env: MutableEnv
  readonly home: string
  readonly uid: number
  readonly user: string
  readonly cwd: string
  readonly now: () => number
  readonly flags: ParsedFlags
  readonly envToken: Secret | null
  readonly notices: ConfigNotice[]
}

function desktopUser(uid: number, env: MutableEnv): string {
  try {
    return userInfo().username
  } catch {
    return envValue(env, "USER") ?? `uid-${uid}`
  }
}

function homeDirectory(env: MutableEnv): string {
  const home = envValue(env, "HOME")
  if (home !== undefined) return home
  try {
    return userInfo().homedir
  } catch {
    throw new ConfigError("cannot find the home directory (HOME is not set)")
  }
}

async function versionText(): Promise<string> {
  try {
    const pkg: unknown = await Bun.file(new URL("../../package.json", import.meta.url)).json()
    const version = isTable(pkg) && typeof pkg.version === "string" ? pkg.version : "unknown"
    return `wingtip ${version}`
  } catch {
    return "wingtip (version unknown)"
  }
}

async function loadConfigFile(ctx: Context, configDir: string): Promise<ConfigFile> {
  const explicit = ctx.flags.config ?? envValue(ctx.env, "LUNARWING_TUI_CONFIG")
  const path = explicit === undefined ? configFilePath(configDir) : resolvePath(explicit, ctx.cwd, ctx.home)
  const file = await readConfigFile(path, { uid: ctx.uid, home: ctx.home, required: explicit !== undefined })
  for (const warning of file.warnings) ctx.notices.push(info(warning))
  return file
}

interface ProfileChoice {
  /** undefined: decided after target selection (tenant name or "default"). */
  readonly name?: string
}

function chooseProfile(ctx: Context, file: ConfigFile): ProfileChoice {
  const fromEnv = envValue(ctx.env, "LUNARWING_TUI_PROFILE")
  const explicit = ctx.flags.profile ?? (fromEnv === undefined ? undefined : assertProfileName(fromEnv, "LUNARWING_TUI_PROFILE"))
  if (explicit !== undefined) {
    if (file.path !== null && !file.profiles.has(explicit)) {
      ctx.notices.push(info(`profile ${explicit} is not defined in ${displayPath(file.path, ctx.home)}; using defaults`))
    }
    return { name: explicit }
  }
  if (ctx.flags.tenant !== undefined) return { name: ctx.flags.tenant }
  const targetNamed = ctx.flags.gatewayUrl !== undefined || ctx.flags.socket !== undefined || envValue(ctx.env, "LUNARWING_GATEWAY_URL") !== undefined
  if (!targetNamed && file.defaultProfile !== undefined) return { name: file.defaultProfile }
  return {}
}

function flagTarget(flags: ParsedFlags): RequestedTarget | null {
  const named = [flags.tenant, flags.gatewayUrl, flags.socket].filter((v) => v !== undefined)
  if (named.length > 1) throw new ConfigError("choose one of --tenant, --gateway-url and --socket")
  if (flags.tenant !== undefined) {
    if (flags.tokenFile !== undefined) throw new ConfigError("--token-file cannot be used with --tenant (the tenant access file holds the token)")
    return { kind: "tenant", tenant: flags.tenant, layer: "flag", source: "--tenant" }
  }
  if (flags.gatewayUrl !== undefined) return { kind: "gateway", url: flags.gatewayUrl, layer: "flag", source: "--gateway-url" }
  if (flags.socket !== undefined) return { kind: "socket", layer: "flag", source: "--socket" }
  return null
}

function profileTarget(section: ProfileSection | undefined): RequestedTarget | null {
  if (section === undefined) return null
  const where = `[profiles.${section.name}]`
  if (section.tenant !== undefined) return { kind: "tenant", tenant: section.tenant, layer: "profile", source: `${where} tenant` }
  if (section.gatewayUrl !== undefined) return { kind: "gateway", url: section.gatewayUrl, layer: "profile", source: `${where} gateway_url` }
  if (section.socket !== undefined) return { kind: "socket", layer: "profile", source: `${where} socket` }
  return null
}

/** flags > LUNARWING_GATEWAY_URL > the profile's tenant, gateway_url or socket. */
function requestedTarget(ctx: Context, section: ProfileSection | undefined): RequestedTarget | null {
  const fromProfile = profileTarget(section)
  const envUrl = envValue(ctx.env, "LUNARWING_GATEWAY_URL")
  const fromEnv: RequestedTarget | null = envUrl === undefined ? null : { kind: "gateway", url: envUrl, layer: "env", source: "LUNARWING_GATEWAY_URL" }
  const chosen = flagTarget(ctx.flags) ?? fromEnv ?? fromProfile
  if (fromProfile !== null && chosen !== fromProfile) ctx.notices.push(info(`${chosen?.source ?? "a flag"} overrides ${fromProfile.source}`))
  return chosen
}

function transportSetting(ctx: Context, file: ConfigFile, section: ProfileSection | undefined): { value: TransportChoice; layer: ConfigLayer; source: string } {
  if (ctx.flags.transport !== undefined) return { value: ctx.flags.transport, layer: "flag", source: "--transport" }
  const fromEnv = envValue(ctx.env, "LUNARWING_TUI_TRANSPORT")
  if (fromEnv !== undefined) {
    const value = parseChoice(fromEnv.trim(), TRANSPORT_CHOICES)
    if (value === null) throw new ConfigError(`LUNARWING_TUI_TRANSPORT must be one of ${TRANSPORT_CHOICES.join(", ")}`)
    return { value, layer: "env", source: "LUNARWING_TUI_TRANSPORT" }
  }
  if (section?.transport !== undefined) return { value: section.transport, layer: "profile", source: `[profiles.${section.name}] transport` }
  if (file.transport !== undefined) return { value: file.transport, layer: "global", source: "transport in config.toml" }
  return { value: "auto", layer: "default", source: "the default transport" }
}

function allowInsecureHttp(file: ConfigFile, section: ProfileSection | undefined): boolean {
  return section?.allowInsecureHttp ?? file.allowInsecureHttp ?? false
}

// ---------------------------------------------------------------------------------------------------------
// Targets and tokens (3.11.3, 3.8.1)
// ---------------------------------------------------------------------------------------------------------

async function fileSource(ctx: Context, path: string): Promise<TokenSource> {
  const source = await openTokenFileSource(path, { uid: ctx.uid, home: ctx.home })
  const warning = await tokenDirectoryWarning(path, ctx.home)
  if (warning !== null) ctx.notices.push(info(warning))
  return source
}

/** Non-tenant token sources in order: --token-file, LUNARWING_GATEWAY_TOKEN_FILE, LUNARWING_GATEWAY_TOKEN, profile token_file, <config dir>/tokens/<profile>.token. */
async function gatewayTokenSource(ctx: Context, configDir: string, profile: string, section: ProfileSection | undefined): Promise<TokenSource> {
  if (ctx.flags.tokenFile !== undefined) return fileSource(ctx, resolvePath(ctx.flags.tokenFile, ctx.cwd, ctx.home))
  const envFile = envValue(ctx.env, GATEWAY_TOKEN_FILE_ENV)
  if (envFile !== undefined) return fileSource(ctx, resolvePath(envFile, ctx.cwd, ctx.home))
  if (ctx.envToken !== null) return envTokenSource(ctx.envToken)
  if (section?.tokenFile !== undefined) return fileSource(ctx, section.tokenFile)
  const fallback = defaultTokenFilePath(configDir, profile)
  try {
    return await fileSource(ctx, fallback)
  } catch (error) {
    if (!(error instanceof MissingFileError)) throw error
    throw new ConfigError(
      `no gateway token for profile ${profile}: put it in a 0600 file at ${displayPath(fallback, ctx.home)}, or use --token-file, ${GATEWAY_TOKEN_FILE_ENV} or LUNARWING_GATEWAY_TOKEN`,
    )
  }
}

async function tenantTarget(ctx: Context, path: string, allowHttp: boolean): Promise<GatewayTarget> {
  if (ctx.flags.tokenFile !== undefined) throw new ConfigError("--token-file cannot be used with a tenant (the tenant access file holds the token)")
  if (ctx.envToken !== null || envValue(ctx.env, GATEWAY_TOKEN_FILE_ENV) !== undefined) {
    ctx.notices.push(info("LUNARWING_GATEWAY_TOKEN and LUNARWING_GATEWAY_TOKEN_FILE are ignored for tenants"))
  }
  let session: Awaited<ReturnType<typeof openTenantTokenSource>>
  try {
    session = await openTenantTokenSource(path, { uid: ctx.uid, now: ctx.now, allowInsecureHttp: allowHttp, home: ctx.home, user: ctx.user })
  } catch (error) {
    if (!(error instanceof MissingFileError)) throw error
    const tenant = tenantFromFileName(path) ?? "<tenant>"
    throw new ConfigError(
      `no tenant access file for ${tenant} (${displayPath(path, ctx.home)}). An operator can grant it with: ${tuiAccessCommand(tenant, ctx.user, true)} (today's daemons; drop --use-master-token once the tenant runs M2b)`,
    )
  }
  const access = session.info
  // Tenant gateways always run on this host: the owner check applies whatever the URL host is (3.8.1).
  const expectedListener = { uid: access.tenantUid, label: access.tenant }
  return { transport: "gateway", url: access.url, token: session.token, expectedListener, tenant: access, allowInsecureHttp: allowHttp }
}

interface UrlTargetInput {
  readonly raw: string
  readonly source: string
  readonly configDir: string
  readonly profile: string
  readonly section: ProfileSection | undefined
  readonly allowHttp: boolean
}

async function urlTarget(ctx: Context, input: UrlTargetInput): Promise<GatewayTarget> {
  const parsed = parseGatewayUrl(input.raw)
  if (!parsed.ok) throw new ConfigError(`${input.source} ${parsed.reason}`)
  const { url, loopback } = parsed
  if (url.protocol === "http:" && !loopback && !input.allowHttp) {
    throw new ConfigError(
      `${input.source}: plain http is allowed only to loopback (127.0.0.0/8, ::1, localhost); use https or set allow_insecure_http = true in the profile`,
    )
  }
  const token = await gatewayTokenSource(ctx, input.configDir, input.profile, input.section)
  let expectedListener: ExpectedListener | null = null
  if (loopback) {
    const configured = input.section?.expectedUid
    expectedListener = configured === undefined ? { uid: ctx.uid, label: ctx.user } : { uid: configured, label: `expected_uid of profile ${input.profile}` }
  }
  return { transport: "gateway", url, token, expectedListener, tenant: null, allowInsecureHttp: input.allowHttp }
}

function resolveThread(ctx: Context, transport: "gateway" | "socket", primary: boolean, configured: ThreadSpec | undefined): { thread: ThreadSpec; explicit: boolean } {
  const explicit = ctx.flags.thread
  const thread: ThreadSpec = explicit ?? (primary ? (configured ?? { kind: transport === "gateway" ? "last" : "new" }) : { kind: "new" })
  if (transport === "socket" && thread.kind !== "last" && thread.kind !== "new") {
    throw new ConfigError("socket threads are last or new (assistant and thread ids are gateway threads)")
  }
  return { thread, explicit: explicit !== undefined }
}

function resolveDebugLogPath(ctx: Context, stateDir: string): string | null {
  const flag = ctx.flags.debugLog
  if (flag !== undefined) return flag.path === null ? defaultDebugLogPath(stateDir) : resolvePath(flag.path, ctx.cwd, ctx.home)
  const fromEnv = envValue(ctx.env, "LUNARWING_TUI_DEBUG_LOG")
  if (fromEnv === undefined) return null
  if (fromEnv === "1" || fromEnv === "true") return defaultDebugLogPath(stateDir)
  return resolvePath(fromEnv, ctx.cwd, ctx.home)
}

function makeContext(options: LoadConfigOptions, env: MutableEnv, flags: ParsedFlags, envToken: Secret | null): Context {
  const uid = options.uid ?? currentUid()
  return {
    env,
    home: options.home ?? homeDirectory(env),
    uid,
    user: options.user ?? desktopUser(uid, env),
    cwd: options.cwd ?? process.cwd(),
    now: options.now ?? Date.now,
    flags,
    envToken,
    notices: [],
  }
}

interface Selected {
  readonly profile: string
  readonly section: ProfileSection | undefined
  readonly target: ConnectionTarget
  readonly picked: boolean
}

async function selectAndBuild(ctx: Context, file: ConfigFile, dirs: { config: string; state: string }, options: LoadConfigOptions): Promise<Selected | null> {
  const choice = chooseProfile(ctx, file)
  let section = choice.name === undefined ? undefined : file.profiles.get(choice.name)
  const socketPath = (): string => resolveSocketPath({ flag: ctx.flags.socket, profile: section?.socket, env: ctx.env, home: ctx.home, cwd: ctx.cwd })
  const { selection, warnings } = await selectTarget({
    transport: transportSetting(ctx, file, section),
    requested: requestedTarget(ctx, section),
    configDir: dirs.config,
    stateDir: dirs.state,
    user: ctx.user,
    uid: ctx.uid,
    home: ctx.home,
    now: ctx.now,
    allowInsecureHttp: allowInsecureHttp(file, section),
    socketSupported: options.socketTransport === true,
    socketPath,
    pick: options.pickTenant,
  })
  for (const warning of warnings) ctx.notices.push(info(warning))
  if (selection.kind === "cancelled") return null
  const profile = choice.name ?? (selection.kind === "tenant" ? selection.tenant : DEFAULT_PROFILE)
  if (choice.name === undefined) section = file.profiles.get(profile)
  const allowHttp = allowInsecureHttp(file, section)
  let target: ConnectionTarget
  if (selection.kind === "tenant") target = await tenantTarget(ctx, selection.path, allowHttp)
  else if (selection.kind === "gateway") {
    target = await urlTarget(ctx, { raw: selection.url, source: selection.source, configDir: dirs.config, profile, section, allowHttp })
  } else target = { transport: "socket", path: selection.path }
  return { profile: assertProfileName(profile, "profile"), section, target, picked: selection.kind === "tenant" && selection.how === "picked" }
}

/** Writes the picker's choice back as lastTenant (dropped by a secondary instance); best effort. */
async function rememberTenant(ctx: Context, stateFile: StateFile, tenant: string): Promise<void> {
  try {
    await stateFile.updateGlobal((state) => ({ ...state, lastTenant: tenant }))
  } catch {
    ctx.notices.push(info("could not remember the tenant choice in state.json"))
  }
}

async function finish(ctx: Context, file: ConfigFile, dirs: { config: string; state: string }, selected: Selected): Promise<ResolvedConfig> {
  const layers: LayerInput = { flags: ctx.flags, env: ctx.env, file, profile: selected.section, warnings: ctx.notices }
  const ui = resolveUiConfig(layers)
  const voice = resolveVoiceConfig(layers)
  const debugLogPath = resolveDebugLogPath(ctx, dirs.state)
  const lock = await acquireProfileLock(dirs.state, selected.profile, { uid: ctx.uid })
  try {
    const stateFile = openStateFile({ stateDir: dirs.state, profile: selected.profile, primary: lock.primary, uid: ctx.uid })
    const tenant = selected.target.transport === "gateway" ? selected.target.tenant : null
    if (selected.picked && tenant !== null) await rememberTenant(ctx, stateFile, tenant.tenant)
    const { thread, explicit } = resolveThread(ctx, selected.target.transport, lock.primary, selected.section?.thread ?? file.thread)
    return Object.freeze({
      profile: selected.profile,
      primary: lock.primary,
      lock,
      stateFile,
      target: Object.freeze(selected.target),
      thread,
      threadExplicit: explicit,
      tenantPicked: selected.picked,
      ui,
      voice,
      debugLogPath,
      dirs: Object.freeze({ config: dirs.config, state: dirs.state, runtime: resolveRuntimeDir(ctx.env) }),
      configFile: file.path,
      user: ctx.user,
      notices: Object.freeze([...ctx.notices]),
    })
  } catch (error) {
    await lock.release()
    throw error
  }
}

/**
 * Resolves the whole session configuration (3.11). Throws ConfigError (exit 2) for every problem the user must
 * fix. On success the caller owns `config.lock` (release it on quit) and should push `config.notices`.
 */
export async function loadConfig(options: LoadConfigOptions): Promise<LoadConfigResult> {
  const env = options.env ?? process.env
  // Read once and blank it before anything can fail, whether or not the token is used.
  const envToken = takeEnvToken(env)
  const flags = parseFlags(options.argv)
  if (flags.help) return { kind: "exit", code: 0, output: usageText() }
  if (flags.version) return { kind: "exit", code: 0, output: await versionText() }
  const ctx = makeContext(options, env, flags, envToken)
  const configDir = await resolveConfigDir(env, ctx.home)
  const file = await loadConfigFile(ctx, configDir)
  const stateDir = resolveStateDir(env, ctx.home)
  await ensureStateDir(stateDir, ctx.uid)
  const dirs = { config: configDir, state: stateDir }
  const selected = await selectAndBuild(ctx, file, dirs, options)
  if (selected === null) return { kind: "exit", code: 0, output: "" }
  return { kind: "ready", config: await finish(ctx, file, dirs, selected) }
}
