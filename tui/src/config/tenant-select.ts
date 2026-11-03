/**
 * Tenant and transport selection (design 3.11.5, UX-10).
 *
 * 1. A requested target (--tenant, --gateway-url or --socket, LUNARWING_GATEWAY_URL, or a profile naming a
 *    tenant, gateway_url or socket) is used. A transport setting (--transport, LUNARWING_TUI_TRANSPORT, profile or
 *    global `transport`) that contradicts it is resolved by precedence: the higher layer wins, the same layer is
 *    an error. Without a requested target, transport "socket" selects the socket.
 * 2. Exactly one valid tenants/*.json file: it is used.
 * 3. Several: the tenant picker (components/tenant-picker.tsx, an OpenTUI <select>) is asked through the
 *    TenantPicker callback, preselecting lastTenant from state.json; the caller writes the pick back.
 * 4. None: a socket owned by the user at the resolved path is used (only once the socket transport exists, M4);
 *    otherwise exit 2 with the tui-access command. The client never reads the ports registry (R14).
 */
import { lstat, readdir } from "node:fs/promises"
import { join } from "node:path"
import { ConfigError, displayValue } from "./flags.ts"
import { tenantAccessPath, tenantsDir } from "./paths.ts"
import { readGlobalState } from "./state-file.ts"
import {
  readTenantAccessInfo,
  tenantFromFileName,
  tuiAccessCommand,
  type TenantAccessInfo,
  type TenantAccessReadOptions,
  type TenantTokenKind,
} from "./tenant-access.ts"
import type { TokenKind, TransportChoice } from "./types.ts"

/** At most this many files of tenants/ are read. */
export const MAX_TENANT_FILES = 64

// ---------------------------------------------------------------------------------------------------------
// Tenant files
// ---------------------------------------------------------------------------------------------------------

export interface TenantFileProblem {
  /** The file name inside tenants/, made printable. */
  readonly file: string
  readonly reason: string
}

export interface TenantListing {
  /** Sorted by tenant name. */
  readonly valid: readonly TenantAccessInfo[]
  readonly invalid: readonly TenantFileProblem[]
}

function errorCode(error: unknown): string | undefined {
  return typeof error === "object" && error !== null && "code" in error ? String(error.code) : undefined
}

/** tenants/ must be ours and not writable by others (tui-access creates it 0700). */
async function checkTenantsDir(dir: string, uid: number): Promise<string | null> {
  const stats = await lstat(dir)
  if (!stats.isDirectory()) return "tenants is not a directory"
  if (stats.uid !== uid) return `tenants is owned by uid ${stats.uid}, not by you (uid ${uid})`
  if ((stats.mode & 0o022) !== 0) return "tenants is writable by group or others (chmod 700)"
  return null
}

async function readTenantFile(dir: string, name: string, options: TenantAccessReadOptions): Promise<TenantAccessInfo | TenantFileProblem> {
  if (tenantFromFileName(name) === null) return { file: displayValue(name, 80), reason: "the file name is not <tenant>.json" }
  try {
    return await readTenantAccessInfo(join(dir, name), options)
  } catch (error) {
    if (error instanceof ConfigError) return { file: displayValue(name, 80), reason: error.message }
    throw error
  }
}

/**
 * The valid tenants/*.json files of `configDir` (validated as in 3.11.3) and the invalid ones with their reason.
 * A missing tenants/ directory lists nothing.
 */
export async function listTenantFiles(configDir: string, options: TenantAccessReadOptions & { uid: number }): Promise<TenantListing> {
  const dir = tenantsDir(configDir)
  let names: string[]
  try {
    const problem = await checkTenantsDir(dir, options.uid)
    if (problem !== null) return { valid: [], invalid: [{ file: "tenants/", reason: problem }] }
    names = (await readdir(dir)).filter((name) => name.endsWith(".json")).sort()
  } catch (error) {
    const code = errorCode(error)
    if (code === "ENOENT" || code === "ENOTDIR") return { valid: [], invalid: [] }
    return { valid: [], invalid: [{ file: "tenants/", reason: `cannot be read (${code ?? "I/O error"})` }] }
  }
  const valid: TenantAccessInfo[] = []
  const invalid: TenantFileProblem[] = []
  for (const name of names.slice(0, MAX_TENANT_FILES)) {
    const result = await readTenantFile(dir, name, options)
    if ("reason" in result) invalid.push(result)
    else valid.push(result)
  }
  if (names.length > MAX_TENANT_FILES) {
    invalid.push({ file: "tenants/", reason: `holds more than ${MAX_TENANT_FILES} files; the rest were not read` })
  }
  valid.sort((a, b) => a.tenant.localeCompare(b.tenant))
  return { valid, invalid }
}

// ---------------------------------------------------------------------------------------------------------
// Picker contract
// ---------------------------------------------------------------------------------------------------------

/** One row of the tenant picker; `name` and `description` map onto OpenTUI's SelectOption (ot081:Select.d.ts:9-12). */
export interface TenantChoice {
  readonly name: string
  readonly description: string
  readonly url: string
  readonly tokenKind: TenantTokenKind
  readonly expiresAt: string | null
}

/** Renders the picker; resolves with the chosen tenant name, or null when the user pressed Esc (quit). */
export type TenantPicker = (choices: readonly TenantChoice[], preselect: string | null) => Promise<string | null>

export function tenantChoices(tenants: readonly TenantAccessInfo[]): TenantChoice[] {
  return tenants.map((info) => {
    const url = info.url.origin
    const expiry = info.expiresAt === null ? "no expiry" : `expires ${info.expiresAt.slice(0, 10)}`
    return {
      name: info.tenant,
      description: `${url} - ${info.tokenKind} token - ${expiry}`,
      url,
      tokenKind: info.tokenKind,
      expiresAt: info.expiresAt,
    }
  })
}

// ---------------------------------------------------------------------------------------------------------
// Messages (UX-10)
// ---------------------------------------------------------------------------------------------------------

/** The exit-2 text when no tenant access is configured; lists unusable files and their reasons. */
export function noTenantAccessMessage(user: string, invalid: readonly TenantFileProblem[] = [], socketHint = false): string {
  const lines = [
    `no tenant access is configured for ${user}. An operator can grant it with: ${tuiAccessCommand("<tenant>", user, true)} (today's daemons; drop --use-master-token once the tenant runs M2b)`,
  ]
  if (socketHint) lines.push("A daemon running as you can be reached with --socket <path>.")
  for (const problem of invalid) lines.push(`  ${problem.file}: ${problem.reason}`)
  return lines.join("\n")
}

export interface AuthHintContext {
  /** Tenant name for client and master tokens. */
  readonly tenant?: string | null
  readonly user: string
  /** TokenSource.describe() of a file token. */
  readonly tokenSource?: string
}

/** The hint shown when the gateway still answers 401 after one token reload (3.8 step 2, UX-10). */
export function authFailureHint(kind: TokenKind, context: AuthHintContext): string {
  const tenant = context.tenant ?? "<tenant>"
  switch (kind) {
    case "client":
      return `this tenant may predate client tokens: re-run \`${tuiAccessCommand(tenant, context.user, true)}\`, or \`upgrade-tenant ${tenant}\``
    case "master":
      return `the master token changed: re-run \`${tuiAccessCommand(tenant, context.user, true)}\``
    case "file":
      return `the token in ${context.tokenSource ?? "the token file"} was rejected`
  }
}

export const SOCKET_UNAVAILABLE_MESSAGE =
  "the socket transport is not available in this build yet (milestone M4); connect through a gateway with --tenant <name> or --gateway-url <url>"

// ---------------------------------------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------------------------------------

/** Precedence layers: flags > environment > config-file profile > config-file globals > defaults. */
export type ConfigLayer = "flag" | "env" | "profile" | "global" | "default"

const LAYER_RANK: Readonly<Record<ConfigLayer, number>> = { flag: 4, env: 3, profile: 2, global: 1, default: 0 }

export type RequestedTarget =
  | { kind: "tenant"; tenant: string; layer: ConfigLayer; source: string }
  | { kind: "gateway"; url: string; layer: ConfigLayer; source: string }
  /** path undefined: resolved with resolveSocketPath(). */
  | { kind: "socket"; path?: string; layer: ConfigLayer; source: string }

export interface SelectionInput {
  readonly transport: { value: TransportChoice; layer: ConfigLayer; source: string }
  readonly requested: RequestedTarget | null
  readonly configDir: string
  readonly stateDir: string
  readonly user: string
  readonly uid: number
  readonly home: string
  readonly now: () => number
  /** The global allow_insecure_http, for tenant files found automatically. */
  readonly allowInsecureHttp: boolean
  /** False until the socket transport lands (M4). */
  readonly socketSupported: boolean
  readonly socketPath: () => string
  readonly pick?: TenantPicker
}

export type TargetSelection =
  | { kind: "tenant"; tenant: string; path: string; how: "requested" | "only" | "picked" }
  | { kind: "gateway"; url: string; source: string }
  | { kind: "socket"; path: string }
  | { kind: "cancelled" }

export interface SelectionResult {
  readonly selection: TargetSelection
  /** Non-fatal notes for the notice area (skipped files, overridden settings). */
  readonly warnings: readonly string[]
}

function compatible(transport: TransportChoice, target: RequestedTarget): boolean {
  if (transport === "auto") return true
  return target.kind === "socket" ? transport === "socket" : transport === "gateway"
}

/** Applies the layer rule to a transport setting that contradicts the requested target. */
function reconcile(input: SelectionInput, warnings: string[]): RequestedTarget | null {
  const { requested, transport } = input
  if (requested === null || compatible(transport.value, requested)) return requested
  const targetRank = LAYER_RANK[requested.layer]
  const transportRank = LAYER_RANK[transport.layer]
  if (targetRank === transportRank) {
    throw new ConfigError(`${transport.source} ${transport.value} conflicts with ${requested.source}`)
  }
  if (transportRank > targetRank) {
    warnings.push(`${transport.source} ${transport.value} overrides ${requested.source}`)
    return null
  }
  warnings.push(`${requested.source} overrides ${transport.source} ${transport.value}`)
  return requested
}

function socketSelection(input: SelectionInput, path?: string): TargetSelection {
  if (!input.socketSupported) throw new ConfigError(SOCKET_UNAVAILABLE_MESSAGE)
  return { kind: "socket", path: path ?? input.socketPath() }
}

async function isOwnSocket(path: string, uid: number): Promise<boolean> {
  try {
    const stats = await lstat(path)
    return stats.isSocket() && stats.uid === uid
  } catch {
    return false
  }
}

async function pickTenant(input: SelectionInput, tenants: readonly TenantAccessInfo[]): Promise<TargetSelection> {
  if (input.pick === undefined) {
    const names = tenants.map((t) => t.tenant).join(", ")
    throw new ConfigError(`several tenants are configured (${names}); choose one with --tenant <name>`)
  }
  const last = (await readGlobalState(input.stateDir, input.uid)).lastTenant ?? null
  const preselect = tenants.some((t) => t.tenant === last) ? last : null
  const choice = await input.pick(tenantChoices(tenants), preselect)
  if (choice === null) return { kind: "cancelled" }
  const chosen = tenants.find((t) => t.tenant === choice)
  if (chosen === undefined) throw new ConfigError("the tenant picker returned a tenant that is not configured")
  return { kind: "tenant", tenant: chosen.tenant, path: chosen.path, how: "picked" }
}

async function autoSelect(input: SelectionInput, warnings: string[]): Promise<TargetSelection> {
  const listing = await listTenantFiles(input.configDir, {
    uid: input.uid,
    now: input.now,
    allowInsecureHttp: input.allowInsecureHttp,
    home: input.home,
    user: input.user,
  })
  const skipped = listing.invalid.map((problem) => `skipped ${problem.file}: ${problem.reason}`)
  const [only, ...others] = listing.valid
  if (only !== undefined) {
    warnings.push(...skipped)
    if (others.length === 0) return { kind: "tenant", tenant: only.tenant, path: only.path, how: "only" }
    return pickTenant(input, listing.valid)
  }
  if (input.transport.value === "auto" && input.socketSupported) {
    const path = input.socketPath()
    if (await isOwnSocket(path, input.uid)) {
      warnings.push(...skipped)
      return { kind: "socket", path }
    }
  }
  throw new ConfigError(noTenantAccessMessage(input.user, listing.invalid, input.socketSupported))
}

/** Resolves the connection target (3.11.5). Throws ConfigError (exit 2) when nothing usable is configured. */
export async function selectTarget(input: SelectionInput): Promise<SelectionResult> {
  const warnings: string[] = []
  const requested = reconcile(input, warnings)
  let selection: TargetSelection
  if (requested?.kind === "tenant") {
    selection = { kind: "tenant", tenant: requested.tenant, path: tenantAccessPath(input.configDir, requested.tenant), how: "requested" }
  } else if (requested?.kind === "gateway") {
    selection = { kind: "gateway", url: requested.url, source: requested.source }
  } else if (requested?.kind === "socket" || input.transport.value === "socket") {
    selection = socketSelection(input, requested?.path)
  } else {
    selection = await autoSelect(input, warnings)
  }
  return { selection, warnings }
}
