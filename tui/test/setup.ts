/**
 * bun test preload (bunfig.toml [test].preload; design 11.3.1, 11.3.5).
 *
 * 1. Unhandled promise rejections fail the run. Bun 1.3.14 already fails the running test (or reports an
 *    "Unhandled error between tests"); the handler below keeps that policy on runtimes that do not: it records
 *    the reason, forces a failing exit code, and the global afterEach fails the test that leaked it.
 * 2. Temp directories from test/helpers/tmp.ts are removed after each test (makeTempDir) and at the end of the
 *    run (makeRunTempDir).
 * 3. Host safety: Bun.spawn and Bun.spawnSync refuse the real desktop helpers (recorders and pactl, clipboard,
 *    screenshot, notifier, opener, Hex) and privilege or service tools; tests inject test/helpers/fake-spawn.ts. An absolute
 *    path that resolves inside os.tmpdir() (a fake a test wrote) or inside tui/test/ (a committed fake, e.g. a
 *    fixture named `hex`) is allowed. Async node:child_process spawn/execFile route through Bun.spawn and are
 *    guarded too; its *Sync functions are bound before any preload runs and cannot be guarded, so tests must not
 *    use them (src/ never may: rule 4).
 * 4. Network: fetch() is refused for every host except loopback, and the proxy variables are blanked so Bun's
 *    fetch never routes loopback test traffic through an inherited proxy (3.11.4). NO_PROXY is left alone so the
 *    proxy-environment test still exercises config/proxy-env.ts in its child process.
 */
import { afterAll, afterEach } from "bun:test"
import { realpathSync } from "node:fs"
import { tmpdir } from "node:os"
import { basename, isAbsolute, sep } from "node:path"
import { cleanupRunTempDirs, cleanupTempDirs } from "./helpers/tmp.ts"

// ---- 1. unhandled rejections ----

const leaked: unknown[] = []

process.on("unhandledRejection", (reason) => {
  leaked.push(reason)
  process.exitCode = 1
})

function failOnLeaks(scope: string): void {
  if (leaked.length === 0) return
  const reasons = leaked.splice(0)
  throw new AggregateError(reasons, `${reasons.length} unhandled promise rejection(s) ${scope}`)
}

// ---- 2. temp directories ----

afterEach(async () => {
  await cleanupTempDirs()
  failOnLeaks("during this test")
})

afterAll(async () => {
  await cleanupRunTempDirs()
  failOnLeaks("after the last test")
})

// ---- 3. host binaries ----

const GUARDED_COMMANDS: ReadonlySet<string> = new Set([
  "parec",
  "pactl",
  "pw-record",
  "pw-cat",
  "arecord",
  "wl-paste",
  "wl-copy",
  "xclip",
  "xsel",
  "grim",
  "slurp",
  "notify-send",
  "xdg-open",
  "hex",
  "sudo",
  "su",
  "doas",
  "pkexec",
  "systemctl",
  "rc-service",
  "lunarwing-mt-admin.sh",
])

function realDir(path: string): string {
  try {
    return realpathSync(path)
  } catch {
    return path
  }
}

/** Where fake executables may live: test temp dirs and the committed test tree. */
const FAKE_ROOTS: readonly string[] = [realDir(tmpdir()), realDir(import.meta.dir)]

function isFakeExecutable(command: string): boolean {
  if (!isAbsolute(command)) return false
  let real: string
  try {
    real = realpathSync(command)
  } catch {
    return true // does not exist: Bun.spawn reports ENOENT itself
  }
  return FAKE_ROOTS.some((root) => real.startsWith(root + sep))
}

function commandOf(args: readonly unknown[]): string | undefined {
  const first = args[0]
  const argv = Array.isArray(first) ? first : (first as { cmd?: unknown } | undefined)?.cmd
  return Array.isArray(argv) && typeof argv[0] === "string" ? argv[0] : undefined
}

function assertSpawnAllowed(args: readonly unknown[]): void {
  const command = commandOf(args)
  if (command === undefined) return
  const name = basename(command)
  if (GUARDED_COMMANDS.has(name) && !isFakeExecutable(command)) {
    throw new Error(`test host-safety guard: refusing to run the real \`${name}\`; inject test/helpers/fake-spawn.ts`)
  }
}

function guardSpawn(name: "spawn" | "spawnSync"): void {
  const real: unknown = Reflect.get(Bun, name)
  if (typeof real !== "function") return
  const guarded = (...args: unknown[]): unknown => {
    assertSpawnAllowed(args)
    return Reflect.apply(real, Bun, args)
  }
  if (!Reflect.set(Bun, name, guarded)) throw new Error(`test setup: cannot guard Bun.${name}`)
}

guardSpawn("spawn")
guardSpawn("spawnSync")

// ---- 4. network ----

for (const name of ["http_proxy", "HTTP_PROXY", "https_proxy", "HTTPS_PROXY", "all_proxy", "ALL_PROXY"]) {
  process.env[name] = ""
}

const NETWORK_PROTOCOLS: ReadonlySet<string> = new Set(["http:", "https:", "ws:", "wss:"])

/** URL.hostname is normalized: "[::ffff:127.0.0.1]" arrives as "[::ffff:7f00:1]". */
function isLoopbackHost(hostname: string): boolean {
  return (
    hostname === "localhost" ||
    hostname === "[::1]" ||
    /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(hostname) ||
    /^\[::ffff:7f[0-9a-f]{2}:[0-9a-f]{1,4}\]$/.test(hostname)
  )
}

function requestUrl(input: unknown): URL | null {
  try {
    if (input instanceof Request) return new URL(input.url)
    if (input instanceof URL) return input
    return new URL(String(input))
  } catch {
    return null
  }
}

const realFetch = globalThis.fetch

const guardedFetch = (input: Parameters<typeof fetch>[0], init?: Parameters<typeof fetch>[1]): Promise<Response> => {
  const url = requestUrl(input)
  if (url && NETWORK_PROTOCOLS.has(url.protocol) && !isLoopbackHost(url.hostname)) {
    return Promise.reject(new TypeError(`test network guard: refusing fetch to ${url.origin} (loopback only)`))
  }
  return realFetch(input, init)
}

globalThis.fetch = Object.assign(guardedFetch, { preconnect: realFetch.preconnect.bind(realFetch) })
