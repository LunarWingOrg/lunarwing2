/**
 * Where the client finds and keeps things (design 3.11.2, 3.11.6, 3.9): the config, state and runtime
 * directories, the socket path, and the gateway URL rules (3.11.3).
 *
 * Directories honour XDG_CONFIG_HOME, XDG_STATE_HOME and XDG_RUNTIME_DIR (relative values are ignored, as the
 * XDG base directory spec requires), so tests and smoke runs point the client at temp directories.
 */
import { stat } from "node:fs/promises"
import { homedir } from "node:os"
import { isAbsolute, join, resolve } from "node:path"

export type EnvSource = Readonly<Record<string, string | undefined>>

export const APP_DIR_NAME = "wingtip"

/** A set, non-empty variable. */
export function envValue(env: EnvSource, name: string): string | undefined {
  const value = env[name]
  return value !== undefined && value.length > 0 ? value : undefined
}

/** An XDG directory variable: only absolute values count. */
export function xdgDir(env: EnvSource, name: string): string | undefined {
  const value = envValue(env, name)
  return value !== undefined && isAbsolute(value) ? value : undefined
}

/** "~" and "~/x" expand to the home directory; "~user" forms are left alone. */
export function expandHome(path: string, home: string): string {
  if (path === "~") return home
  if (path.startsWith("~/")) return join(home, path.slice(2))
  return path
}

/** Expands "~" and resolves a relative path against `base`. */
export function resolvePath(path: string, base: string, home: string): string {
  return resolve(base, expandHome(path, home))
}

/** The path as shown to the user: the home directory abbreviated to "~". */
export function displayPath(path: string, home: string): string {
  if (home.length > 1 && (path === home || path.startsWith(`${home}/`))) return `~${path.slice(home.length)}`
  return path
}

// ---------------------------------------------------------------------------------------------------------
// Socket (3.9; M4 transport, M1a resolution)
// ---------------------------------------------------------------------------------------------------------

/**
 * Mirror of the Rust client's default socket resolution order.
 *
 * Moved verbatim from v4:tui/src/repl-client.ts:36-50 (it mirrors ic/src/main.rs:490-500); the only change is
 * that the environment and the home directory are parameters defaulting to process.env and homedir(), so a
 * call without arguments behaves exactly as before and tests need not mutate process.env.
 */
export function defaultSocketPath(source: EnvSource = process.env, home: string = homedir()): string {
  const env = (name: string): string | undefined => {
    const value = source[name]
    return value && value.length > 0 ? value : undefined
  }
  const runtimeDir = env("XDG_RUNTIME_DIR")
  const baseDir = env("LUNARWING_BASE_DIR") ?? env("IRONCLAW_BASE_DIR")
  return (
    env("LUNARWING_SOCKET") ??
    env("IRONCLAW_SOCKET") ??
    (runtimeDir ? join(runtimeDir, "lunarwing.sock") : undefined) ??
    (baseDir ? join(baseDir, "lunarwing.sock") : undefined) ??
    join(home, ".ironclaw", "lunarwing.sock")
  )
}

/**
 * The socket path with the full precedence of 3.9: `--socket`, LUNARWING_SOCKET, IRONCLAW_SOCKET, the profile's
 * `socket`, then defaultSocketPath() ($XDG_RUNTIME_DIR, the base dir, ~/.ironclaw).
 */
export function resolveSocketPath(options: {
  flag?: string
  profile?: string
  env: EnvSource
  home: string
  cwd: string
}): string {
  const explicit = options.flag ?? envValue(options.env, "LUNARWING_SOCKET") ?? envValue(options.env, "IRONCLAW_SOCKET")
  if (explicit !== undefined) return resolvePath(explicit, options.cwd, options.home)
  if (options.profile !== undefined) return options.profile
  return defaultSocketPath(options.env, options.home)
}

// ---------------------------------------------------------------------------------------------------------
// Directories and files (3.11.2, 3.11.6)
// ---------------------------------------------------------------------------------------------------------

/**
 * Config directory candidates in search order. An explicit LUNARWING_TUI_CONFIG_DIR is the only candidate (it is
 * used even before it exists, so a test or smoke run never falls through to the real ~/.config); otherwise
 * ${XDG_CONFIG_HOME:-$HOME/.config}/wingtip, then $HOME/.config/wingtip (where tui-access writes).
 */
export function configDirCandidates(env: EnvSource, home: string): string[] {
  const explicit = envValue(env, "LUNARWING_TUI_CONFIG_DIR")
  if (explicit !== undefined) return [resolve(expandHome(explicit, home))]
  const candidates = [join(xdgDir(env, "XDG_CONFIG_HOME") ?? join(home, ".config"), APP_DIR_NAME)]
  const fallback = join(home, ".config", APP_DIR_NAME)
  if (!candidates.includes(fallback)) candidates.push(fallback)
  return candidates
}

async function isDirectory(path: string): Promise<boolean> {
  try {
    return (await stat(path)).isDirectory()
  } catch {
    return false
  }
}

/** The first candidate that exists as a directory, else the first candidate. */
export async function resolveConfigDir(env: EnvSource, home: string): Promise<string> {
  const candidates = configDirCandidates(env, home)
  for (const dir of candidates) {
    if (await isDirectory(dir)) return dir
  }
  return candidates[0] ?? join(home, ".config", APP_DIR_NAME)
}

/** ${XDG_STATE_HOME:-~/.local/state}/wingtip (0700). */
export function resolveStateDir(env: EnvSource, home: string): string {
  return join(xdgDir(env, "XDG_STATE_HOME") ?? join(home, ".local", "state"), APP_DIR_NAME)
}

/** $XDG_RUNTIME_DIR/wingtip (0700, /image open exports), or null without a runtime directory. */
export function resolveRuntimeDir(env: EnvSource): string | null {
  const runtime = xdgDir(env, "XDG_RUNTIME_DIR")
  return runtime === undefined ? null : join(runtime, APP_DIR_NAME)
}

export function configFilePath(configDir: string): string {
  return join(configDir, "config.toml")
}

export function tenantsDir(configDir: string): string {
  return join(configDir, "tenants")
}

export function tenantAccessPath(configDir: string, tenant: string): string {
  return join(tenantsDir(configDir), `${tenant}.json`)
}

/** <config dir>/tokens/<profile>.token, the last token source of a non-tenant profile (3.11.3). */
export function defaultTokenFilePath(configDir: string, profile: string): string {
  return join(configDir, "tokens", `${profile}.token`)
}

export function profileStatePath(stateDir: string, profile: string): string {
  return join(stateDir, `${profile}.json`)
}

export function globalStatePath(stateDir: string): string {
  return join(stateDir, "state.json")
}

export function profileLockPath(stateDir: string, profile: string): string {
  return join(stateDir, `${profile}.lock`)
}

export function defaultDebugLogPath(stateDir: string): string {
  return join(stateDir, "debug.log")
}

// ---------------------------------------------------------------------------------------------------------
// Gateway URLs (3.11.3)
// ---------------------------------------------------------------------------------------------------------

const IPV4_LOOPBACK = /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/
/** WHATWG URL normalizes [::ffff:127.0.0.1] to [::ffff:7f00:1]. */
const IPV4_MAPPED_LOOPBACK = /^\[::ffff:7f[0-9a-f]{2}:[0-9a-f]{1,4}\]$/

/** 127.0.0.0/8, ::1 (and the v4-mapped 127/8 form) and exactly "localhost"; takes URL.hostname. */
export function isLoopbackHostname(hostname: string): boolean {
  const host = hostname.toLowerCase()
  return host === "localhost" || host === "[::1]" || IPV4_LOOPBACK.test(host) || IPV4_MAPPED_LOOPBACK.test(host)
}

export type GatewayUrlResult = { ok: true; url: URL; loopback: boolean } | { ok: false; reason: string }

/**
 * Syntax rules for a gateway base URL: http or https; no userinfo, query, fragment or path. Returns the URL
 * normalized to "<origin>/". The insecure-http policy (plain http only to loopback unless allow_insecure_http)
 * is applied by the caller, because it depends on the profile.
 */
export function parseGatewayUrl(raw: string): GatewayUrlResult {
  let url: URL
  try {
    url = new URL(raw.trim())
  } catch {
    return { ok: false, reason: "is not a valid URL" }
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return { ok: false, reason: "must use http or https" }
  if (url.username !== "" || url.password !== "") return { ok: false, reason: "must not contain a user name or password" }
  if (url.search !== "" || url.hash !== "" || /[?#]/.test(raw)) {
    return { ok: false, reason: "must not have a query or a fragment" }
  }
  if (url.pathname !== "/") return { ok: false, reason: "must not have a path (use the gateway's base URL)" }
  return { ok: true, url, loopback: isLoopbackHostname(url.hostname) }
}
