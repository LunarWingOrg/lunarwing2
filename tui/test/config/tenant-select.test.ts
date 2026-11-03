/**
 * Tenant and transport selection (design 3.11.5, UX-10, 11.3.2). Every file lives in a temp HOME; the tenant
 * access files are fakes in the exact 9.5 format that `mt-admin tui-access` writes.
 */
import { describe, expect, test } from "bun:test"
import { mkdir, readFile } from "node:fs/promises"
import { join } from "node:path"
import { loadConfig, type LoadConfigOptions, type ResolvedConfig } from "../../src/config/config.ts"
import { ConfigError } from "../../src/config/flags.ts"
import { authFailureHint, listTenantFiles, noTenantAccessMessage, SOCKET_UNAVAILABLE_MESSAGE, type TenantChoice } from "../../src/config/tenant-select.ts"
import { makeTempDir, writeFileWithMode } from "../helpers/tmp.ts"

const NOW = Date.parse("2026-11-03T12:00:00Z")
const USER = "tester"
const GRANT = "sudo ic/scripts/lunarwing-mt-admin.sh tui-access <tenant> --for-user tester --use-master-token"

/** A fake 64-character token, unique per tenant. Never a real credential. */
function fakeToken(tenant: string): string {
  return `${tenant}-fake-token-`.padEnd(64, "0")
}

function tenantJson(tenant: string, overrides: Record<string, unknown> = {}): string {
  const record: Record<string, unknown> = {
    schema: "lunarwing-tui/tenant-access",
    version: 1,
    tenant,
    transport: "gateway",
    url: "http://127.0.0.1:10000",
    tenant_uid: 1010,
    token: fakeToken(tenant),
    token_kind: "client",
    label: `tui:${USER}`,
    issued_at: "2026-11-03T00:15:00Z",
    expires_at: "2027-02-01T00:15:00Z",
    ...overrides,
  }
  for (const [key, value] of Object.entries(record)) if (value === undefined) delete record[key]
  // tui-access commits the file through a here-string, so it ends with a newline.
  return `${JSON.stringify(record)}\n`
}

interface Sandbox {
  root: string
  home: string
  configDir: string
  stateDir: string
  options(argv: readonly string[], extra?: Partial<LoadConfigOptions>): LoadConfigOptions
  writeTenant(tenant: string, overrides?: Record<string, unknown>, mode?: number): Promise<string>
}

async function sandbox(): Promise<Sandbox> {
  const root = await makeTempDir()
  const home = join(root, "home")
  const configDir = join(home, ".config", "lunarwing-tui")
  await mkdir(join(configDir, "tenants"), { recursive: true, mode: 0o700 })
  const env = { HOME: home, XDG_CONFIG_HOME: join(home, ".config"), XDG_STATE_HOME: join(root, "state") }
  return {
    root,
    home,
    configDir,
    stateDir: join(root, "state", "lunarwing-tui"),
    options: (argv, extra = {}) => ({ argv, env: { ...env }, home, user: USER, cwd: root, now: () => NOW, ...extra }),
    writeTenant: (tenant, overrides = {}, mode = 0o600) =>
      writeFileWithMode(join(configDir, "tenants", `${tenant}.json`), tenantJson(tenant, overrides), mode),
  }
}

async function ready(options: LoadConfigOptions): Promise<ResolvedConfig> {
  const result = await loadConfig(options)
  if (result.kind !== "ready") throw new Error(`expected a ready config, got exit ${result.code}`)
  return result.config
}

async function configError(promise: Promise<unknown>): Promise<ConfigError> {
  try {
    await promise
  } catch (error) {
    if (error instanceof ConfigError) return error
    throw error
  }
  throw new Error("expected a ConfigError")
}

describe("tenant selection (3.11.5)", () => {
  test("one tenant file is used without flags", async () => {
    const sb = await sandbox()
    await sb.writeTenant("derpy")
    const config = await ready(sb.options([]))
    expect(config.profile).toBe("derpy")
    expect(config.tenantPicked).toBe(false)
    const target = config.target
    if (target.transport !== "gateway") throw new Error("expected a gateway target")
    expect(target.url.href).toBe("http://127.0.0.1:10000/")
    expect(target.expectedListener).toEqual({ uid: 1010, label: "derpy" })
    expect(target.tenant?.tenant).toBe("derpy")
    expect(target.token.kind()).toBe("client")
    expect(target.token.describe()).toBe("tenants/derpy.json (client token tui:tester)")
    expect(target.token.current().expose()).toBe(fakeToken("derpy"))
    expect(config.thread).toEqual({ kind: "last" })
    await config.lock.release()
  })

  test("five files open the picker and remember the choice", async () => {
    const sb = await sandbox()
    const names = ["echo", "alpha", "delta", "charlie", "bravo"]
    for (const [i, name] of names.entries()) await sb.writeTenant(name, { url: `http://127.0.0.1:${10000 + i}`, tenant_uid: 1010 + i })
    const calls: Array<{ choices: readonly TenantChoice[]; preselect: string | null }> = []
    const pickTenant = (choices: readonly TenantChoice[], preselect: string | null): Promise<string | null> => {
      calls.push({ choices, preselect })
      return Promise.resolve("charlie")
    }

    const first = await ready(sb.options([], { pickTenant }))
    expect(calls).toHaveLength(1)
    expect(calls[0]?.choices.map((c) => c.name)).toEqual(["alpha", "bravo", "charlie", "delta", "echo"])
    expect(calls[0]?.preselect).toBeNull()
    expect(calls[0]?.choices[2]?.description).toBe("http://127.0.0.1:10003 - client token - expires 2027-02-01")
    expect(first.profile).toBe("charlie")
    expect(first.tenantPicked).toBe(true)
    if (first.target.transport !== "gateway") throw new Error("expected a gateway target")
    expect(first.target.expectedListener).toEqual({ uid: 1013, label: "charlie" })
    expect(JSON.parse(await readFile(join(sb.stateDir, "state.json"), "utf8"))).toEqual({ lastTenant: "charlie" })
    await first.lock.release()

    const second = await ready(sb.options([], { pickTenant }))
    expect(calls[1]?.preselect).toBe("charlie")
    await second.lock.release()
  })

  test("the picker can be cancelled (Esc quits), and without a picker several tenants exit 2", async () => {
    const sb = await sandbox()
    await sb.writeTenant("alpha")
    await sb.writeTenant("bravo")
    expect(await loadConfig(sb.options([], { pickTenant: () => Promise.resolve(null) }))).toEqual({ kind: "exit", code: 0, output: "" })
    const error = await configError(loadConfig(sb.options([])))
    expect(error.exitCode).toBe(2)
    expect(error.message).toContain("several tenants are configured (alpha, bravo)")
    expect(error.message).toContain("--tenant <name>")
  })

  test("zero files exit 2 with the tui-access command", async () => {
    const sb = await sandbox()
    const error = await configError(loadConfig(sb.options([])))
    expect(error.exitCode).toBe(2)
    expect(error.message).toBe(
      `no tenant access is configured for tester. An operator can grant it with: ${GRANT} (today's daemons; drop --use-master-token once the tenant runs M2b)`,
    )
    expect(noTenantAccessMessage(USER)).toBe(error.message)
  })

  test("the 401 hint for a client token mentions --use-master-token", async () => {
    const client = authFailureHint("client", { tenant: "derpy", user: USER })
    expect(client).toContain("this tenant may predate client tokens")
    expect(client).toContain("sudo ic/scripts/lunarwing-mt-admin.sh tui-access derpy --for-user tester --use-master-token")
    expect(client).toContain("upgrade-tenant derpy")
    const master = authFailureHint("master", { tenant: "derpy", user: USER })
    expect(master).toContain("the master token changed")
    expect(master).toContain("--use-master-token")
    expect(authFailureHint("file", { user: USER, tokenSource: "token file ~/x.token" })).toBe("the token in token file ~/x.token was rejected")

    const sb = await sandbox()
    await sb.writeTenant("derpy")
    const config = await ready(sb.options([]))
    if (config.target.transport !== "gateway") throw new Error("expected a gateway target")
    expect(authFailureHint(config.target.token.kind(), { tenant: config.target.tenant?.tenant, user: USER })).toContain("--use-master-token")
    await config.lock.release()
  })

  test("invalid tenant files are skipped with a notice; with none valid the exit message gives each reason", async () => {
    const sb = await sandbox()
    await sb.writeTenant("derpy")
    await sb.writeTenant("old", { tenant_uid: undefined })
    await sb.writeTenant("loose", {}, 0o644)
    await writeFileWithMode(join(sb.configDir, "tenants", "broken.json"), `{"schema":"lunarwing-tui/tenant-access","token":"${fakeToken("broken")}`, 0o600)
    await writeFileWithMode(join(sb.configDir, "tenants", "notes.txt"), "not a tenant file", 0o600)

    const listing = await listTenantFiles(sb.configDir, { uid: process.getuid?.() ?? -1, now: () => NOW, home: sb.home, user: USER })
    expect(listing.valid.map((t) => t.tenant)).toEqual(["derpy"])
    expect(listing.invalid.map((p) => p.file)).toEqual(["broken.json", "loose.json", "old.json"])
    const reasons = listing.invalid.map((p) => p.reason).join("\n")
    expect(reasons).toContain("is not valid JSON")
    expect(reasons).toContain("has mode 0644")
    expect(reasons).toContain("has no tenant_uid")
    expect(reasons).not.toContain(fakeToken("broken"))

    const config = await ready(sb.options([]))
    expect(config.profile).toBe("derpy")
    expect(config.notices.filter((n) => n.text.startsWith("skipped "))).toHaveLength(3)
    await config.lock.release()

    const empty = await sandbox()
    await empty.writeTenant("old", { tenant_uid: undefined })
    const error = await configError(loadConfig(empty.options([])))
    expect(error.message).toContain(GRANT)
    expect(error.message).toContain("old.json: ")
    expect(error.message).toContain("re-run tui-access (sudo ic/scripts/lunarwing-mt-admin.sh tui-access old --for-user tester")
  })

  test("a profile naming a tenant wins over automatic selection, and --tenant wins over the profile", async () => {
    const sb = await sandbox()
    await sb.writeTenant("alpha", { tenant_uid: 1011 })
    await sb.writeTenant("bravo", { tenant_uid: 1012 })
    await writeFileWithMode(
      join(sb.configDir, "config.toml"),
      'default_profile = "work"\n[profiles.work]\ntenant = "bravo"\nthread = "new"\n[profiles.alpha]\nthread = "assistant"\n',
      0o600,
    )
    const work = await ready(sb.options([]))
    expect(work.profile).toBe("work")
    expect(work.thread).toEqual({ kind: "new" })
    if (work.target.transport !== "gateway") throw new Error("expected a gateway target")
    expect(work.target.tenant?.tenant).toBe("bravo")
    await work.lock.release()

    const alpha = await ready(sb.options(["--tenant", "alpha"]))
    expect(alpha.profile).toBe("alpha")
    expect(alpha.thread).toEqual({ kind: "assistant" })
    if (alpha.target.transport !== "gateway") throw new Error("expected a gateway target")
    expect(alpha.target.expectedListener).toEqual({ uid: 1011, label: "alpha" })
    await alpha.lock.release()

    const missing = await configError(loadConfig(sb.options(["--tenant", "zulu"])))
    expect(missing.message).toContain("no tenant access file for zulu (~/.config/lunarwing-tui/tenants/zulu.json)")
    expect(missing.message).toContain("sudo ic/scripts/lunarwing-mt-admin.sh tui-access zulu --for-user tester --use-master-token")
  })

  test("tenants are reached through the gateway only; transport conflicts follow the precedence layers", async () => {
    const sb = await sandbox()
    await sb.writeTenant("alpha")
    const same = await configError(loadConfig(sb.options(["--transport", "socket", "--tenant", "alpha"])))
    expect(same.message).toBe("--transport socket conflicts with --tenant")
    expect((await configError(loadConfig(sb.options(["--tenant", "alpha", "--gateway-url", "http://127.0.0.1:1"])))).message).toBe(
      "choose one of --tenant, --gateway-url and --socket",
    )
    // A higher layer wins: the flag's tenant over a profile's transport.
    await writeFileWithMode(join(sb.configDir, "config.toml"), '[profiles.alpha]\ntransport = "socket"\n', 0o600)
    const config = await ready(sb.options(["--tenant", "alpha"]))
    expect(config.target.transport).toBe("gateway")
    expect(config.notices.map((n) => n.text)).toContain("--tenant overrides [profiles.alpha] transport socket")
    await config.lock.release()
  })

  test("until the socket transport lands (M4) socket requests exit 2; with it, a socket of this user is the fallback", async () => {
    const sb = await sandbox()
    for (const argv of [["--socket", join(sb.root, "x.sock")], ["--transport", "socket"]]) {
      const error = await configError(loadConfig(sb.options(argv)))
      expect(error.message).toBe(SOCKET_UNAVAILABLE_MESSAGE)
    }
    const runtime = join(sb.root, "run")
    await mkdir(runtime, { mode: 0o700 })
    const path = join(runtime, "lunarwing.sock")
    const listener = Bun.listen({ unix: path, socket: { data: () => undefined } })
    try {
      const options = sb.options([], { socketTransport: true })
      const config = await ready({ ...options, env: { ...options.env, XDG_RUNTIME_DIR: runtime } })
      expect(config.target).toEqual({ transport: "socket", path })
      expect(config.thread).toEqual({ kind: "new" })
      await config.lock.release()
    } finally {
      listener.stop(true)
    }
  })
})
