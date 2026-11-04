/**
 * Tenant access files (design 3.11.3, 9.5): `<config dir>/tenants/<tenant>.json`, written by
 * `lunarwing-mt-admin.sh tui-access <tenant> --for-user <user>` (ic/scripts/mt-admin-lib/16-tui-access.sh):
 *
 *   {"schema":"wingtip/tenant-access","version":1,"tenant":"derpy","transport":"gateway",
 *    "url":"http://127.0.0.1:10000","tenant_uid":1010,"token":"<64 hex>","token_kind":"client","label":"tui:sun",
 *    "issued_at":"2026-11-03T00:15:00Z","expires_at":"2027-02-01T00:15:00Z"}
 *
 * expires_at is optional (tui-access has no default expiry, A.5). Before the token is read the file must be a
 * regular file (not a link) owned by the current uid, (mode & 0o077) === 0 and at most 4 KiB; then schema,
 * version 1, tenant (matching the file name), transport "gateway", a valid URL, tenant_uid an integer > 0
 * (required, SEC-01: the listener-owner check of 3.8.1 needs it), a token, token_kind client|master, and an
 * expires_at that has not passed. The token is the only source for a tenant profile; on a 401 the file is
 * re-read once (TokenSource.reload()).
 */
import { basename } from "node:path"
import { ConfigError, displayValue, isValidTenantName } from "./flags.ts"
import { displayPath, parseGatewayUrl } from "./paths.ts"
import { Secret } from "./secret.ts"
import { currentUid, isValidTokenText, readCheckedFile } from "./token-file.ts"
import type { TokenKind, TokenSource } from "./types.ts"

export const TENANT_ACCESS_SCHEMA = "wingtip/tenant-access"
export const TENANT_ACCESS_VERSION = 1
export const TENANT_ACCESS_MAX_BYTES = 4096

export type TenantTokenKind = "client" | "master"

/** Everything in the file except the token. Safe to show and to log as metadata. */
export interface TenantAccessInfo {
  readonly tenant: string
  /** Absolute path of the access file. */
  readonly path: string
  /** Normalized gateway base URL ("<origin>/"). */
  readonly url: URL
  /** True for 127.0.0.0/8, ::1 and localhost. */
  readonly loopback: boolean
  /** The tenant account's uid: the expected owner of the gateway listener (3.8.1). */
  readonly tenantUid: number
  readonly tokenKind: TenantTokenKind
  /** "tui:<user>" for client tokens; null when absent or malformed (display only). */
  readonly label: string | null
  readonly issuedAt: string | null
  /** ISO-8601; null when the grant does not expire. */
  readonly expiresAt: string | null
}

export interface TenantAccess extends TenantAccessInfo {
  readonly token: Secret
}

export interface TenantAccessReadOptions {
  /** Required file owner; default process.getuid(). */
  uid?: number
  /** Clock for the expiry check; default Date.now. */
  now?: () => number
  /** Plain http to a non-loopback host is refused unless the profile allows it (3.11.3). */
  allowInsecureHttp?: boolean
  /** For "~" in messages. */
  home?: string
  /** The desktop user, for the tui-access command in messages. */
  user?: string
}

const LABEL_PATTERN = /^[a-z0-9][a-z0-9:._-]{0,63}$/
const TIMESTAMP_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?Z$/
const MAX_UID = 4_294_967_294

/** The operator command that (re)creates a tenant access file. */
export function tuiAccessCommand(tenant: string, user: string, masterToken = false): string {
  const master = masterToken ? " --use-master-token" : ""
  return `sudo ic/scripts/lunarwing-mt-admin.sh tui-access ${tenant} --for-user ${user}${master}`
}

/** The tenant a file name stands for ("derpy.json" -> "derpy"); null when it is not a tenant name. */
export function tenantFromFileName(path: string): string | null {
  const name = basename(path)
  if (!name.endsWith(".json")) return null
  const tenant = name.slice(0, -".json".length)
  return isValidTenantName(tenant) ? tenant : null
}

function parseTimestamp(value: unknown): number | null {
  if (typeof value !== "string" || !TIMESTAMP_PATTERN.test(value)) return null
  const at = Date.parse(value)
  return Number.isFinite(at) ? at : null
}

/** What validateTenantAccess() checks the parsed JSON against. */
export interface ValidationContext {
  readonly path: string
  readonly label: string
  readonly tenant: string
  readonly now: number
  readonly allowInsecureHttp: boolean
  /** The re-run hint appended to most messages. */
  readonly rerun: string
}

function fail(ctx: ValidationContext, problem: string): never {
  throw new ConfigError(`${ctx.label} ${problem}`)
}

function checkHeader(record: Record<string, unknown>, ctx: ValidationContext): void {
  if (record.schema !== TENANT_ACCESS_SCHEMA) fail(ctx, `is not a tenant access file (schema is not ${TENANT_ACCESS_SCHEMA})`)
  if (record.version !== TENANT_ACCESS_VERSION) fail(ctx, `has an unsupported version (this client reads version 1); ${ctx.rerun}`)
  if (record.tenant !== ctx.tenant) fail(ctx, `names a different tenant than its file name; ${ctx.rerun}`)
  if (record.transport !== undefined && record.transport !== "gateway") fail(ctx, `has an unsupported transport (only gateway)`)
}

function checkUrl(record: Record<string, unknown>, ctx: ValidationContext): { url: URL; loopback: boolean } {
  if (typeof record.url !== "string") fail(ctx, `has no gateway url; ${ctx.rerun}`)
  const parsed = parseGatewayUrl(record.url)
  if (!parsed.ok) fail(ctx, `has a gateway url that ${parsed.reason}; ${ctx.rerun}`)
  if (parsed.url.protocol === "http:" && !parsed.loopback && !ctx.allowInsecureHttp) {
    fail(ctx, `points at plain http on a non-loopback host; use https, a loopback URL, or set allow_insecure_http = true`)
  }
  return { url: parsed.url, loopback: parsed.loopback }
}

function checkTenantUid(record: Record<string, unknown>, ctx: ValidationContext): number {
  const uid = record.tenant_uid
  if (uid === undefined) fail(ctx, `has no tenant_uid (written by an older tui-access); ${ctx.rerun}`)
  if (typeof uid !== "number" || !Number.isSafeInteger(uid) || uid <= 0 || uid > MAX_UID) {
    fail(ctx, `has an invalid tenant_uid (an integer > 0 is required); ${ctx.rerun}`)
  }
  return uid
}

function checkTokenKind(record: Record<string, unknown>, ctx: ValidationContext): TenantTokenKind {
  const kind = record.token_kind
  if (kind !== "client" && kind !== "master") fail(ctx, `has an unknown token_kind (client or master); ${ctx.rerun}`)
  return kind
}

function checkExpiry(record: Record<string, unknown>, ctx: ValidationContext): string | null {
  if (record.expires_at === undefined || record.expires_at === null) return null
  const at = parseTimestamp(record.expires_at)
  if (at === null) fail(ctx, `has an invalid expires_at; ${ctx.rerun}`)
  if (at <= ctx.now) fail(ctx, `expired at ${String(record.expires_at)}; ${ctx.rerun}`)
  return String(record.expires_at)
}

/** Validates parsed JSON against 9.5. Pure apart from the Secret it creates; the error never holds the token. */
export function validateTenantAccess(value: unknown, ctx: ValidationContext): TenantAccess {
  if (typeof value !== "object" || value === null || Array.isArray(value)) fail(ctx, "is not a JSON object")
  const record = value as Record<string, unknown>
  checkHeader(record, ctx)
  const { url, loopback } = checkUrl(record, ctx)
  const tenantUid = checkTenantUid(record, ctx)
  if (typeof record.token !== "string" || !isValidTokenText(record.token)) fail(ctx, `holds no valid token; ${ctx.rerun}`)
  const tokenKind = checkTokenKind(record, ctx)
  const expiresAt = checkExpiry(record, ctx)
  const label = typeof record.label === "string" && LABEL_PATTERN.test(record.label) ? record.label : null
  const issuedAt = parseTimestamp(record.issued_at) === null ? null : String(record.issued_at)
  return {
    tenant: ctx.tenant,
    path: ctx.path,
    url,
    loopback,
    tenantUid,
    tokenKind,
    label,
    issuedAt,
    expiresAt,
    token: new Secret(record.token),
  }
}

/** Reads and validates one tenant access file. Throws ConfigError naming the file and the failed check. */
export async function readTenantAccessFile(path: string, options: TenantAccessReadOptions = {}): Promise<TenantAccess> {
  const home = options.home ?? ""
  const label = `tenant access file ${displayValue(home === "" ? path : displayPath(path, home))}`
  const tenant = tenantFromFileName(path)
  if (tenant === null) throw new ConfigError(`${label} is not named <tenant>.json`)
  const ctx: ValidationContext = {
    path,
    label,
    tenant,
    now: (options.now ?? Date.now)(),
    allowInsecureHttp: options.allowInsecureHttp === true,
    rerun: `re-run tui-access (${tuiAccessCommand(tenant, options.user ?? "<user>")} [--use-master-token])`,
  }
  const bytes = await readCheckedFile(path, {
    uid: options.uid ?? currentUid(),
    maxBytes: TENANT_ACCESS_MAX_BYTES,
    label,
    forbiddenModeBits: 0o077,
  })
  let value: unknown
  try {
    value = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes))
  } catch {
    // The parser's message can quote the file (and so the token): never pass it on.
    fail(ctx, `is not valid JSON; ${ctx.rerun}`)
  } finally {
    bytes.fill(0)
  }
  return validateTenantAccess(value, ctx)
}

/** The file without its token: what the tenant picker and the header show. */
export function tenantAccessInfo(access: TenantAccess): TenantAccessInfo {
  const { token: _token, ...info } = access
  return Object.freeze(info)
}

/** Validates a file and keeps only its metadata; the token is dropped at once. */
export async function readTenantAccessInfo(path: string, options: TenantAccessReadOptions = {}): Promise<TenantAccessInfo> {
  const access = await readTenantAccessFile(path, options)
  access.token.dispose()
  return tenantAccessInfo(access)
}

/** The token of a tenant access file. reload() re-reads the file once; a changed url or tenant_uid is refused. */
class TenantTokenSource implements TokenSource {
  #access: TenantAccess
  #reloading: Promise<boolean> | null = null

  constructor(
    access: TenantAccess,
    private readonly options: TenantAccessReadOptions,
  ) {
    this.#access = access
  }

  get info(): TenantAccessInfo {
    return tenantAccessInfo(this.#access)
  }

  current(): Secret {
    return this.#access.token
  }

  reload(): Promise<boolean> {
    this.#reloading ??= this.#reread().finally(() => {
      this.#reloading = null
    })
    return this.#reloading
  }

  async #reread(): Promise<boolean> {
    let next: TenantAccess
    try {
      next = await readTenantAccessFile(this.#access.path, this.options)
    } catch {
      return false
    }
    // The transport keeps its base URL and expected listener uid: a re-provisioned endpoint needs a restart.
    const sameEndpoint = next.url.href === this.#access.url.href && next.tenantUid === this.#access.tenantUid
    if (!sameEndpoint || next.token.equals(this.#access.token)) {
      next.token.dispose()
      return false
    }
    this.#access = next
    return true
  }

  describe(): string {
    const access = this.#access
    return `tenants/${access.tenant}.json (${access.tokenKind} token${access.label === null ? "" : ` ${access.label}`})`
  }

  kind(): TokenKind {
    return this.#access.tokenKind
  }
}

export interface TenantTokenSession {
  readonly info: TenantAccessInfo
  readonly token: TokenSource
}

/** Validates the tenant access file and returns its metadata with a TokenSource for the gateway transport. */
export async function openTenantTokenSource(path: string, options: TenantAccessReadOptions = {}): Promise<TenantTokenSession> {
  const access = await readTenantAccessFile(path, options)
  const source = new TenantTokenSource(access, options)
  return { info: source.info, token: source }
}
