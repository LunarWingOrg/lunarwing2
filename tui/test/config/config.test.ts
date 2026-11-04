/**
 * Configuration and secrets (design 3.11, 3.12.5 log rules, 11.3.2). Everything runs inside a temp HOME with
 * XDG_CONFIG_HOME and XDG_STATE_HOME pointing into it; tokens are fakes.
 */
import { describe, expect, test } from "bun:test"
import { mkdir, readFile, rename, stat, symlink, writeFile } from "node:fs/promises"
import { join } from "node:path"
import { buildChildEnv, CHILD_ENV_NAMES } from "../../src/config/child-env.ts"
import { loadConfig, parseConfigToml, readConfigFile, type LoadConfigOptions, type ResolvedConfig } from "../../src/config/config.ts"
import { ConfigError, parseFlags, TOKEN_ON_ARGV_MESSAGE } from "../../src/config/flags.ts"
import {
  configDirCandidates,
  defaultSocketPath,
  parseGatewayUrl,
  resolveRuntimeDir,
  resolveSocketPath,
  resolveStateDir,
} from "../../src/config/paths.ts"
import { REDACTED, Secret } from "../../src/config/secret.ts"
import { openTenantTokenSource, readTenantAccessFile } from "../../src/config/tenant-access.ts"
import { openTokenFileSource, readTokenFile, takeEnvToken } from "../../src/config/token-file.ts"
import { DEFAULT_UI_CONFIG, DEFAULT_VOICE_CONFIG } from "../../src/config/types.ts"
import { NULL_DEBUG_LOG, openDebugLog, safeTag } from "../../src/log.ts"
import { makeTempDir, writeFileWithMode } from "../helpers/tmp.ts"

const NOW = Date.parse("2026-11-03T12:00:00Z")
const USER = "tester"
const UID = process.getuid?.() ?? -1
// Fake values shaped like tokens; never real credentials.
const TOKEN = "lw-test-token-0123456789abcdef"
const SECRET = "s3cr3t-value-that-must-never-be-echoed-0123456789"

interface Sandbox {
  root: string
  home: string
  configDir: string
  options(argv: readonly string[], env?: Record<string, string>, extra?: Partial<LoadConfigOptions>): LoadConfigOptions
  writeConfig(text: string, mode?: number): Promise<string>
  writeToken(name: string, token: string, mode?: number): Promise<string>
}

async function sandbox(): Promise<Sandbox> {
  const root = await makeTempDir()
  const home = join(root, "home")
  const configDir = join(home, ".config", "wingtip")
  await mkdir(join(configDir, "tokens"), { recursive: true, mode: 0o700 })
  await mkdir(join(configDir, "tenants"), { recursive: true, mode: 0o700 })
  const base = { HOME: home, XDG_CONFIG_HOME: join(home, ".config"), XDG_STATE_HOME: join(root, "state") }
  return {
    root,
    home,
    configDir,
    options: (argv, env = {}, extra = {}) => ({ argv, env: { ...base, ...env }, home, user: USER, cwd: root, now: () => NOW, ...extra }),
    writeConfig: (text, mode = 0o600) => writeFileWithMode(join(configDir, "config.toml"), text, mode),
    writeToken: (name, token, mode = 0o600) => writeFileWithMode(join(configDir, "tokens", `${name}.token`), `${token}\n`, mode),
  }
}

async function ready(options: LoadConfigOptions): Promise<ResolvedConfig> {
  const result = await loadConfig(options)
  if (result.kind !== "ready") throw new Error(`expected a ready config, got exit ${result.code}`)
  await result.config.lock.release()
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

function flagError(argv: readonly string[]): ConfigError {
  try {
    parseFlags(argv)
  } catch (error) {
    if (error instanceof ConfigError) return error
    throw error
  }
  throw new Error("expected a ConfigError")
}

function gateway(config: ResolvedConfig) {
  if (config.target.transport !== "gateway") throw new Error("expected a gateway target")
  return config.target
}

function tenantJson(tenant: string, overrides: Record<string, unknown> = {}): string {
  const record: Record<string, unknown> = {
    schema: "wingtip/tenant-access",
    version: 1,
    tenant,
    transport: "gateway",
    url: "http://127.0.0.1:10000",
    tenant_uid: 1010,
    token: TOKEN,
    token_kind: "client",
    label: "tui:tester",
    issued_at: "2026-11-03T00:15:00Z",
    expires_at: "2027-02-01T00:15:00Z",
    ...overrides,
  }
  for (const [key, value] of Object.entries(record)) if (value === undefined) delete record[key]
  return `${JSON.stringify(record)}\n`
}

/** Replaces a file the way tui-access does: a new file renamed over the old one. */
async function replaceFile(path: string, data: string, mode = 0o600): Promise<void> {
  const tmp = `${path}.new`
  await writeFileWithMode(tmp, data, mode)
  await rename(tmp, path)
}

// ---------------------------------------------------------------------------------------------------------

describe("flags (3.11.1)", () => {
  test("--token exits 2: --token, --gateway-token and --token=<v> are refused anywhere and never echoed", async () => {
    for (const argv of [
      ["--token", SECRET],
      ["--gateway-token", SECRET],
      [`--token=${SECRET}`],
      [`--gateway-token=${SECRET}`],
      ["--help", "--profile", "derpy", "--token", SECRET],
    ]) {
      const error = flagError(argv)
      expect(error.exitCode).toBe(2)
      expect(error.message).toBe(TOKEN_ON_ARGV_MESSAGE)
      expect(error.message).toContain("tokens are not accepted on the command line (visible in ps and /proc)")
      expect(error.message).not.toContain(SECRET)
    }
    const sb = await sandbox()
    const options = sb.options(["--token", SECRET], { LUNARWING_GATEWAY_TOKEN: TOKEN })
    const error = await configError(loadConfig(options))
    expect(error.exitCode).toBe(2)
    expect(options.env?.LUNARWING_GATEWAY_TOKEN).toBe("")
  })

  test("unknown options and stray arguments are refused without echoing their values", () => {
    const typo = flagError([`--tokn=${SECRET}`])
    expect(typo.message).toBe("unknown option --tokn (see --help)")
    expect(flagError([SECRET]).message).not.toContain(SECRET)
    expect(flagError(["--profile"]).message).toBe("--profile needs a value")
    expect(flagError(["--profile", "--tenant"]).message).toBe("--profile needs a value")
    expect(flagError(["--no-voice=yes"]).message).toBe("--no-voice takes no value")
  })

  test("flag values are validated", () => {
    expect(flagError(["--transport", "carrier-pigeon"]).message).toBe("--transport must be one of gateway, socket, auto")
    expect(flagError(["--thread", "yesterday"]).message).toContain("--thread must be last, new, assistant or a thread UUID")
    expect(flagError(["--image-protocol", "ascii"]).message).toBe("--image-protocol must be one of auto, kitty, sixel, blocks")
    expect(flagError(["--voice-order", "lunarvoice,whisper"]).message).toBe("--voice-order: the value is not a voice source (lunarvoice, hex-app, hex-embedded)")
    expect(flagError(["--ptt-key", "ctrl+"]).message).toContain("is not a key name")
    expect(flagError(["--tenant", "Derpy"]).message).toBe("--tenant: the value is not a tenant name (lowercase letters, digits and -)")
    for (const flag of ["--tenant", "--profile", "--voice-order", "--ptt-key", "--thread", "--transport", "--image-protocol"]) {
      expect(flagError([flag, SECRET]).message).not.toContain(SECRET.slice(0, 8))
    }
  })

  test("parsed flags: inline values, the optional --debug-log path, repeatable --ptt-key, last value wins", () => {
    const flags = parseFlags([
      "--profile=work",
      "--profile",
      "home",
      "--thread",
      "4B0C7D0E-1D1A-4C49-9A5C-2A1F3F0F9E11",
      "--voice-order",
      "hex-app,lunarvoice,hex-app",
      "--ptt-key",
      "F9",
      "--ptt-key=ctrl+space",
      "--no-voice",
      "--debug-log",
      "--image-protocol",
      "kitty",
    ])
    expect(flags.profile).toBe("home")
    expect(flags.thread).toEqual({ kind: "id", id: "4b0c7d0e-1d1a-4c49-9a5c-2a1f3f0f9e11" })
    expect(flags.voiceOrder).toEqual(["hex-app", "lunarvoice"])
    expect(flags.pttKeys).toEqual(["f9", "ctrl+space"])
    expect(flags.noVoice).toBe(true)
    expect(flags.debugLog).toEqual({ path: null })
    expect(flags.imageProtocol).toBe("kitty")
    expect(parseFlags(["--debug-log", "/tmp/x.log"]).debugLog).toEqual({ path: "/tmp/x.log" })
    expect(parseFlags(["--debug-log=/tmp/y.log"]).debugLog).toEqual({ path: "/tmp/y.log" })
  })

  test("--help and --version exit 0 before any file is read", async () => {
    const help = await loadConfig({ argv: ["--help"], env: {}, home: "/nonexistent", user: USER })
    expect(help.kind).toBe("exit")
    if (help.kind === "exit") expect(help.output).toContain("--tenant <name>")
    const version = await loadConfig({ argv: ["--version"], env: {}, home: "/nonexistent", user: USER })
    expect(version).toEqual({ kind: "exit", code: 0, output: "wingtip 0.1.0" })
  })
})

describe("precedence: flags > environment > profile > globals > defaults (3.11)", () => {
  test("image_protocol through every layer", async () => {
    const bare = await sandbox()
    await bare.writeToken("default", TOKEN)
    expect((await ready(bare.options(["--gateway-url", "http://127.0.0.1:8765"]))).ui).toEqual(DEFAULT_UI_CONFIG)

    const sb = await sandbox()
    await sb.writeToken("p", TOKEN)
    await sb.writeToken("q", TOKEN)
    await sb.writeConfig(
      [
        "[ui]",
        'image_protocol = "blocks"',
        "reduce_motion = true",
        "[profiles.p]",
        'gateway_url = "http://127.0.0.1:8765"',
        "[profiles.p.ui]",
        'image_protocol = "sixel"',
        "[profiles.q]",
        'gateway_url = "http://127.0.0.1:8765"',
        "",
      ].join("\n"),
    )
    const protocol = async (argv: string[], env: Record<string, string> = {}) => (await ready(sb.options(argv, env))).ui.imageProtocol
    expect(await protocol(["--profile", "q"])).toBe("blocks")
    expect(await protocol(["--profile", "p"])).toBe("sixel")
    expect(await protocol(["--profile", "p"], { LUNARWING_TUI_IMAGE_PROTOCOL: " kitty " })).toBe("kitty")
    expect(await protocol(["--profile", "p", "--image-protocol", "auto"], { LUNARWING_TUI_IMAGE_PROTOCOL: "kitty" })).toBe("auto")
    const ignored = await ready(sb.options(["--profile", "p"], { LUNARWING_TUI_IMAGE_PROTOCOL: "ascii" }))
    expect(ignored.ui.imageProtocol).toBe("sixel")
    expect(ignored.ui.reduceMotion).toBe(true)
    expect(ignored.notices.map((n) => n.text).join("\n")).toContain("ignoring LUNARWING_TUI_IMAGE_PROTOCOL")
  })

  test("gateway URL and token sources keep their order", async () => {
    const sb = await sandbox()
    await sb.writeConfig('[profiles.p]\ngateway_url = "http://127.0.0.1:1111"\ntoken_file = "tokens/p-profile.token"\n[profiles.r]\ngateway_url = "http://127.0.0.1:1111"\n[profiles.s]\ntransport = "gateway"\n')
    await sb.writeToken("p-profile", "profile-token-0123456789")
    await sb.writeToken("r", "default-token-0123456789")
    const other = join(sb.root, "other")
    await mkdir(other, { mode: 0o700 })
    await writeFileWithMode(join(other, "flag.token"), "flag-token-0123456789ab\n", 0o600)
    await writeFileWithMode(join(other, "env.token"), "envfile-token-0123456789\n", 0o600)
    const tokenOf = (config: ResolvedConfig) => gateway(config).token.current().expose()

    const profile = await ready(sb.options(["--profile", "p"]))
    expect(gateway(profile).url.href).toBe("http://127.0.0.1:1111/")
    expect(tokenOf(profile)).toBe("profile-token-0123456789")
    expect(gateway(profile).expectedListener).toEqual({ uid: UID, label: USER })

    const fromEnvUrl = await ready(sb.options(["--profile", "p"], { LUNARWING_GATEWAY_URL: "http://127.0.0.1:2222" }))
    expect(gateway(fromEnvUrl).url.port).toBe("2222")
    expect(fromEnvUrl.notices.map((n) => n.text)).toContain("LUNARWING_GATEWAY_URL overrides [profiles.p] gateway_url")
    const fromFlagUrl = await ready(sb.options(["--profile", "p", "--gateway-url", "http://127.0.0.1:3333"], { LUNARWING_GATEWAY_URL: "http://127.0.0.1:2222" }))
    expect(gateway(fromFlagUrl).url.port).toBe("3333")

    const envAll = { LUNARWING_GATEWAY_TOKEN_FILE: join(other, "env.token"), LUNARWING_GATEWAY_TOKEN: "env-token-0123456789abcd" }
    expect(tokenOf(await ready(sb.options(["--profile", "p", "--token-file", join(other, "flag.token")], { ...envAll })))).toBe("flag-token-0123456789ab")
    expect(tokenOf(await ready(sb.options(["--profile", "p"], { ...envAll })))).toBe("envfile-token-0123456789")
    const envToken = await ready(sb.options(["--profile", "p"], { LUNARWING_GATEWAY_TOKEN: "env-token-0123456789abcd" }))
    expect(tokenOf(envToken)).toBe("env-token-0123456789abcd")
    expect(gateway(envToken).token.describe()).toBe("LUNARWING_GATEWAY_TOKEN")
    expect(tokenOf(await ready(sb.options(["--profile", "r"])))).toBe("default-token-0123456789")

    const none = await configError(loadConfig(sb.options(["--profile", "s", "--gateway-url", "http://127.0.0.1:1"])))
    expect(none.message).toContain("no gateway token for profile s")
    expect(none.message).toContain("~/.config/wingtip/tokens/s.token")
  })

  test("transport: --transport > LUNARWING_TUI_TRANSPORT > profile > global", async () => {
    const sb = await sandbox()
    await sb.writeConfig('transport = "socket"\n[profiles.p]\ngateway_url = "http://127.0.0.1:8765"\ntransport = "gateway"\n')
    await sb.writeToken("p", TOKEN)
    expect((await ready(sb.options(["--profile", "p"]))).target.transport).toBe("gateway")
    const fromEnv = await configError(loadConfig(sb.options(["--profile", "p"], { LUNARWING_TUI_TRANSPORT: "socket" })))
    expect(fromEnv.message).toContain("socket transport is not available")
    expect((await ready(sb.options(["--profile", "p", "--transport", "gateway"], { LUNARWING_TUI_TRANSPORT: "socket" }))).target.transport).toBe("gateway")
    const bad = await configError(loadConfig(sb.options(["--profile", "p"], { LUNARWING_TUI_TRANSPORT: "smoke-signals" })))
    expect(bad.message).toBe("LUNARWING_TUI_TRANSPORT must be one of gateway, socket, auto")
  })

  test("voice settings through every layer; max_duration_s is capped at 290 s", async () => {
    const sb = await sandbox()
    await sb.writeToken("p", TOKEN)
    await sb.writeConfig(
      [
        "[voice]",
        'order = ["hex-app", "lunarvoice"]',
        'ptt_keys = ["f8"]',
        "max_duration_s = 400",
        "repeat_delay_ms = 300",
        "[voice.hex]",
        'command = ["/usr/local/bin/hex"]',
        'model = "parakeet"',
        "[profiles.p]",
        'gateway_url = "http://127.0.0.1:8765"',
        "[profiles.p.voice]",
        'blur_policy = "never"',
        'ptt_keys = ["f10"]',
        'device = "C922"',
        "",
      ].join("\n"),
    )
    const layered = await ready(sb.options(["--profile", "p"]))
    expect(layered.voice).toEqual({
      ...DEFAULT_VOICE_CONFIG,
      order: ["hex-app", "lunarvoice"],
      pttKeys: ["f10"],
      maxDurationMs: 290_000,
      repeatDelayMs: 300,
      blurPolicy: "never",
      device: "C922",
      hex: { command: ["/usr/local/bin/hex"], model: "parakeet" },
    })
    expect(layered.notices.map((n) => n.text).join("\n")).toContain("max_duration_s 400 is above the 290 s cap")
    const flagged = await ready(
      sb.options(["--profile", "p", "--no-voice", "--voice-order", "lunarvoice", "--ptt-key", "F7", "--ptt-key", "ctrl+space"], {
        LUNARWING_TUI_HEX_BIN: "/opt/hex/bin/hex",
      }),
    )
    expect(flagged.voice.enabled).toBe(false)
    expect(flagged.voice.order).toEqual(["lunarvoice"])
    expect(flagged.voice.pttKeys).toEqual(["f7", "ctrl+space"])
    expect(flagged.voice.hex).toEqual({ command: ["/opt/hex/bin/hex"], model: "parakeet" })
    expect(Object.isFrozen(flagged.voice.order)).toBe(true)
  })
})

describe("tokens (3.11.3)", () => {
  test("token file 0644 refused, 0600 accepted, content never echoed", async () => {
    const sb = await sandbox()
    const path = await sb.writeToken("devgw", SECRET, 0o644)
    const refused = await configError(readTokenFile(path, { home: sb.home }))
    expect(refused.message).toBe("token file ~/.config/wingtip/tokens/devgw.token has mode 0644: group and others must have no access (chmod 600)")
    expect(refused.message).not.toContain(SECRET)
    const viaConfig = await configError(loadConfig(sb.options(["--profile", "devgw", "--gateway-url", "http://127.0.0.1:8765"])))
    expect(viaConfig.message).toContain("has mode 0644")

    await writeFileWithMode(path, `  ${SECRET}\n`, 0o600)
    expect((await readTokenFile(path, { home: sb.home })).expose()).toBe(SECRET)
    const config = await ready(sb.options(["--profile", "devgw", "--gateway-url", "http://127.0.0.1:8765"]))
    expect(gateway(config).token.current().expose()).toBe(SECRET)
    expect(gateway(config).token.kind()).toBe("file")
    expect(gateway(config).token.describe()).toBe("token file ~/.config/wingtip/tokens/devgw.token")
  })

  test("symlinked, foreign, oversized, non-regular and malformed token files are refused by name, never by content", async () => {
    const sb = await sandbox()
    const real = await sb.writeToken("real", SECRET)
    const link = join(sb.configDir, "tokens", "link.token")
    await symlink(real, link)
    expect((await configError(readTokenFile(link, { home: sb.home }))).message).toContain("is a symbolic link")
    expect((await configError(readTokenFile(real, { home: sb.home, uid: UID + 1 }))).message).toContain(`is owned by uid ${UID}, not by you`)
    const big = await sb.writeToken("big", "x".repeat(5000))
    expect((await configError(readTokenFile(big, { home: sb.home }))).message).toContain("is larger than 4096 bytes")
    const dir = join(sb.configDir, "tokens", "dir.token")
    await mkdir(dir)
    expect((await configError(readTokenFile(dir, { home: sb.home }))).message).toContain("is not a regular file")
    for (const content of [`${SECRET} with spaces`, "short", `${SECRET}\u0000`, `${"y".repeat(513)}`]) {
      const path = await sb.writeToken("bad", content)
      const error = await configError(readTokenFile(path, { home: sb.home }))
      expect(error.message).toContain("is not a valid token file")
      expect(error.message).not.toContain(SECRET)
      expect(error.message).not.toContain("short")
    }
    const missing = await configError(readTokenFile(join(sb.root, "nope.token"), { home: sb.home }))
    expect(missing.message).toContain("does not exist")
    // A token passed where the path belongs is not echoed back.
    const pasted = await configError(loadConfig(sb.options(["--gateway-url", "http://127.0.0.1:8765", "--token-file", SECRET])))
    expect(pasted.message).toContain("the name looks like a token")
    expect(pasted.message).not.toContain(SECRET)
  })

  test("a token file source re-reads its file once on reload()", async () => {
    const sb = await sandbox()
    const path = await sb.writeToken("rot", "first-token-0123456789")
    const source = await openTokenFileSource(path, { home: sb.home })
    expect(await source.reload()).toBe(false)
    await replaceFile(path, "second-token-0123456789\n")
    expect(await source.reload()).toBe(true)
    expect(source.current().expose()).toBe("second-token-0123456789")
    await replaceFile(path, "third-token-0123456789\n", 0o644)
    expect(await source.reload()).toBe(false)
    expect(source.current().expose()).toBe("second-token-0123456789")
  })

  test("LUNARWING_GATEWAY_TOKEN is blanked in process.env after it is read", async () => {
    process.env.LUNARWING_GATEWAY_TOKEN = SECRET
    try {
      const secret = takeEnvToken()
      expect(process.env.LUNARWING_GATEWAY_TOKEN).toBe("")
      expect(Object.hasOwn(process.env, "LUNARWING_GATEWAY_TOKEN")).toBe(true)
      expect(secret?.expose()).toBe(SECRET)
      expect(takeEnvToken()).toBeNull()
    } finally {
      delete process.env.LUNARWING_GATEWAY_TOKEN
    }
    const sb = await sandbox()
    const options = sb.options(["--gateway-url", "http://127.0.0.1:8765"], { LUNARWING_GATEWAY_TOKEN: SECRET })
    const config = await ready(options)
    expect(options.env?.LUNARWING_GATEWAY_TOKEN).toBe("")
    expect(gateway(config).token.current().expose()).toBe(SECRET)
    const invalid = sb.options(["--gateway-url", "http://127.0.0.1:8765"], { LUNARWING_GATEWAY_TOKEN: `${SECRET} x` })
    const error = await configError(loadConfig(invalid))
    expect(error.message).toContain("LUNARWING_GATEWAY_TOKEN is not a valid token")
    expect(error.message).not.toContain(SECRET)
    expect(invalid.env?.LUNARWING_GATEWAY_TOKEN).toBe("")
  })

  test("Secret is absent from JSON.stringify, String() and Bun.inspect of the resolved configuration", async () => {
    const sb = await sandbox()
    await writeFileWithMode(join(sb.configDir, "tenants", "derpy.json"), tenantJson("derpy", { token: SECRET }), 0o600)
    const config = await ready(sb.options([]))
    const target = gateway(config)
    for (const text of [
      JSON.stringify(config),
      JSON.stringify(target),
      String(target.token),
      String(target.token.current()),
      `${target.token.current()}`,
      Bun.inspect(config),
      Bun.inspect(target.token),
      Bun.inspect(target.token.current()),
      target.token.describe(),
    ]) {
      expect(text).not.toContain(SECRET)
    }
    const access = await readTenantAccessFile(target.tenant?.path ?? "", { now: () => NOW })
    expect(JSON.stringify(access)).toContain(`"token":"${REDACTED}"`)
    expect(Bun.inspect(access)).not.toContain(SECRET)
    expect(JSON.stringify(new Secret(SECRET))).toBe(JSON.stringify(REDACTED))
  })
})

describe("gateway URLs (3.11.3)", () => {
  test("plain http to a non-loopback host is refused unless allow_insecure_http is set (then a sticky warning)", async () => {
    const sb = await sandbox()
    await sb.writeToken("default", TOKEN)
    await sb.writeToken("lan", TOKEN)
    const refused = await configError(loadConfig(sb.options(["--gateway-url", "http://192.168.1.20:8765"])))
    expect(refused.message).toContain("plain http is allowed only to loopback")
    await sb.writeConfig('[profiles.lan]\ngateway_url = "http://192.168.1.20:8765"\nallow_insecure_http = true\n')
    const allowed = await ready(sb.options(["--profile", "lan"]))
    // The transport turns these into its sticky warnings (ServerInfo.warnings, 3.7).
    expect(gateway(allowed).allowInsecureHttp).toBe(true)
    expect(gateway(allowed).expectedListener).toBeNull()
    expect(gateway(allowed).url.href).toBe("http://192.168.1.20:8765/")

    const remote = await ready(sb.options(["--gateway-url", "https://gw.example.invalid"]))
    expect(gateway(remote).expectedListener).toBeNull()
    expect(gateway(remote).allowInsecureHttp).toBe(false)
    for (const loopback of ["http://127.0.0.1:8765", "http://127.9.9.9:1", "http://localhost:8765", "http://[::1]:8765"]) {
      const config = await ready(sb.options(["--gateway-url", loopback]))
      expect(gateway(config).expectedListener).toEqual({ uid: UID, label: USER })
    }
  })

  test("userinfo, query, fragment, path and other schemes are refused", () => {
    expect(parseGatewayUrl("http://127.0.0.1:8765")).toMatchObject({ ok: true, loopback: true })
    for (const [raw, reason] of [
      ["http://user:pw@127.0.0.1:8765", "user name or password"],
      ["http://127.0.0.1:8765/?token=x", "query or a fragment"],
      ["http://127.0.0.1:8765/#x", "query or a fragment"],
      ["http://127.0.0.1:8765/?", "query or a fragment"],
      ["http://127.0.0.1:8765/api", "must not have a path"],
      ["ws://127.0.0.1:8765", "http or https"],
      ["file:///etc/passwd", "http or https"],
      ["not a url", "not a valid URL"],
    ] as const) {
      const result = parseGatewayUrl(raw)
      expect(result.ok).toBe(false)
      if (!result.ok) expect(result.reason).toContain(reason)
    }
    expect(parseGatewayUrl("http://0.0.0.0:8765")).toMatchObject({ ok: true, loopback: false })
    expect(parseGatewayUrl("http://localhost.:8765")).toMatchObject({ ok: true, loopback: false })
  })
})

describe("tenant access files (3.11.3, 9.5)", () => {
  async function tenantFile(tenant: string, overrides: Record<string, unknown> = {}, mode = 0o600): Promise<string> {
    const dir = join(await makeTempDir(), "tenants")
    await mkdir(dir, { mode: 0o700 })
    return writeFileWithMode(join(dir, `${tenant}.json`), tenantJson(tenant, overrides), mode)
  }

  async function refusal(path: string, options: Parameters<typeof readTenantAccessFile>[1] = {}): Promise<string> {
    const error = await configError(readTenantAccessFile(path, { now: () => NOW, user: USER, ...options }))
    expect(error.message).not.toContain(TOKEN)
    return error.message
  }

  test("the exact tui-access format is accepted, with and without expires_at", async () => {
    const access = await readTenantAccessFile(await tenantFile("derpy"), { now: () => NOW })
    expect(access).toMatchObject({ tenant: "derpy", tenantUid: 1010, tokenKind: "client", label: "tui:tester", loopback: true, expiresAt: "2027-02-01T00:15:00Z" })
    expect(access.url.href).toBe("http://127.0.0.1:10000/")
    expect(access.token.expose()).toBe(TOKEN)
    const forever = await readTenantAccessFile(await tenantFile("derpy", { expires_at: undefined }), { now: () => NOW })
    expect(forever.expiresAt).toBeNull()
    const ipv6 = await readTenantAccessFile(await tenantFile("derpy", { url: "http://[::1]:10000" }), { now: () => NOW })
    expect(ipv6.loopback).toBe(true)
  })

  test("validated: schema, version, mode, owner, size, expiry and tenant_uid present", async () => {
    expect(await refusal(await tenantFile("derpy", { schema: "something-else" }))).toContain("is not a tenant access file")
    expect(await refusal(await tenantFile("derpy", { version: 2 }))).toContain("has an unsupported version")
    expect(await refusal(await tenantFile("derpy", {}, 0o640))).toContain("has mode 0640")
    expect(await refusal(await tenantFile("derpy", {}, 0o604))).toContain("has mode 0604")
    expect(await refusal(await tenantFile("derpy"), { uid: UID + 1 })).toContain(`is owned by uid ${UID}`)
    expect(await refusal(await tenantFile("derpy", { padding: "z".repeat(4200) }))).toContain("is larger than 4096 bytes")
    const expired = await refusal(await tenantFile("derpy", { expires_at: "2026-11-01T00:00:00Z" }))
    expect(expired).toContain("expired at 2026-11-01T00:00:00Z")
    expect(expired).toContain("re-run tui-access (sudo ic/scripts/lunarwing-mt-admin.sh tui-access derpy --for-user tester")
    expect(await refusal(await tenantFile("derpy", { expires_at: "next tuesday" }))).toContain("has an invalid expires_at")
    const noUid = await refusal(await tenantFile("derpy", { tenant_uid: undefined }))
    expect(noUid).toContain("has no tenant_uid")
    expect(noUid).toContain("re-run tui-access")
    for (const uid of [0, -5, 1.5, "1010", null]) {
      expect(await refusal(await tenantFile("derpy", { tenant_uid: uid }))).toContain("has an invalid tenant_uid")
    }
    expect(await refusal(await tenantFile("derpy", { tenant: "other" }))).toContain("names a different tenant")
    expect(await refusal(await tenantFile("derpy", { transport: "socket" }))).toContain("unsupported transport")
    expect(await refusal(await tenantFile("derpy", { token_kind: "root" }))).toContain("unknown token_kind")
    expect(await refusal(await tenantFile("derpy", { token: "short" }))).toContain("holds no valid token")
    expect(await refusal(await tenantFile("derpy", { url: "http://127.0.0.1:10000/api" }))).toContain("must not have a path")
    expect(await refusal(await tenantFile("derpy", { url: "http://10.0.0.5:10000" }))).toContain("plain http on a non-loopback host")
    const lan = await readTenantAccessFile(await tenantFile("derpy", { url: "http://10.0.0.5:10000" }), { now: () => NOW, allowInsecureHttp: true })
    expect(lan.loopback).toBe(false)

    const path = await tenantFile("derpy")
    const misnamed = await writeFileWithMode(join(path, "..", "Not_A_Tenant.json"), tenantJson("derpy"), 0o600)
    expect(await refusal(misnamed)).toContain("is not named <tenant>.json")
    const linkedTenant = join(path, "..", "twin.json")
    await symlink(path, linkedTenant)
    expect(await refusal(linkedTenant)).toContain("is a symbolic link")
    await writeFile(path, `{"token":"${TOKEN}"`)
    expect(await refusal(path)).toContain("is not valid JSON")
  })

  test("re-read once on 401: reload() takes a rotated token and keeps the current one when the file breaks", async () => {
    const path = await tenantFile("derpy")
    const { info, token } = await openTenantTokenSource(path, { now: () => NOW })
    expect(info.tenantUid).toBe(1010)
    expect(token.current().expose()).toBe(TOKEN)
    expect(await token.reload()).toBe(false)

    await replaceFile(path, tenantJson("derpy", { token: "rotated-token-0123456789abcdef" }))
    const first = token.reload()
    expect(token.reload()).toBe(first)
    expect(await first).toBe(true)
    expect(token.current().expose()).toBe("rotated-token-0123456789abcdef")

    await replaceFile(path, tenantJson("derpy", { token: "unreadable-token-0123456789" }), 0o644)
    expect(await token.reload()).toBe(false)
    await replaceFile(path, tenantJson("derpy", { token: "moved-token-0123456789", url: "http://127.0.0.1:10001" }))
    expect(await token.reload()).toBe(false)
    await replaceFile(path, tenantJson("derpy", { token: "master-token-0123456789", token_kind: "master" }))
    expect(await token.reload()).toBe(true)
    expect(token.kind()).toBe("master")
    expect(token.current().expose()).toBe("master-token-0123456789")
  })

  test("token_kind master is reported by the token source (the transport raises the sticky warning from it)", async () => {
    const sb = await sandbox()
    await writeFileWithMode(join(sb.configDir, "tenants", "derpy.json"), tenantJson("derpy", { token_kind: "master", label: undefined }), 0o600)
    const config = await ready(sb.options([]))
    expect(gateway(config).token.kind()).toBe("master")
    expect(gateway(config).tenant?.tokenKind).toBe("master")
    expect(gateway(config).token.describe()).toBe("tenants/derpy.json (master token)")
  })
})

describe("config.toml (3.11.2)", () => {
  test("group-writable config.toml refused; a foreign owner refused; symlinks to a safe file are followed", async () => {
    const sb = await sandbox()
    await sb.writeToken("default", TOKEN)
    const path = await sb.writeConfig("[ui]\nreduce_motion = true\n", 0o664)
    const error = await configError(loadConfig(sb.options(["--gateway-url", "http://127.0.0.1:8765"])))
    expect(error.message).toBe("config file ~/.config/wingtip/config.toml has mode 0664: it must not be writable by group or others (chmod go-w)")
    await writeFileWithMode(path, "[ui]\nreduce_motion = true\n", 0o602)
    expect((await configError(loadConfig(sb.options(["--gateway-url", "http://127.0.0.1:8765"])))).message).toContain("has mode 0602")
    await writeFileWithMode(path, "[ui]\nreduce_motion = true\n", 0o644)
    expect((await configError(readConfigFile(path, { uid: UID + 1, home: sb.home, required: true }))).message).toContain("is owned by uid")

    const dotfiles = join(sb.root, "dotfiles")
    await mkdir(dotfiles)
    await writeFileWithMode(join(dotfiles, "config.toml"), "[ui]\ncursor_blink = false\n", 0o644)
    await rename(path, `${path}.old`)
    await symlink(join(dotfiles, "config.toml"), path)
    expect((await ready(sb.options(["--gateway-url", "http://127.0.0.1:8765"]))).ui.cursorBlink).toBe(false)
  })

  test("a missing default config.toml is fine; an explicit --config or LUNARWING_TUI_CONFIG must exist", async () => {
    const sb = await sandbox()
    await sb.writeToken("default", TOKEN)
    const config = await ready(sb.options(["--gateway-url", "http://127.0.0.1:8765"]))
    expect(config.configFile).toBeNull()
    const explicit = await configError(loadConfig(sb.options(["--config", "nope.toml", "--gateway-url", "http://127.0.0.1:8765"])))
    expect(explicit.message).toContain("nope.toml does not exist")
    const fromEnv = await configError(loadConfig(sb.options(["--gateway-url", "http://127.0.0.1:8765"], { LUNARWING_TUI_CONFIG: join(sb.root, "gone.toml") })))
    expect(fromEnv.message).toContain("gone.toml does not exist")
    const elsewhere = await writeFileWithMode(join(sb.root, "alt.toml"), "[ui]\nnotify = \"off\"\n", 0o600)
    expect((await ready(sb.options(["--config", elsewhere, "--gateway-url", "http://127.0.0.1:8765"]))).ui.notify).toBe("off")
  })

  test("unknown keys warn; wrong types and conflicting profile keys fail with the key named", () => {
    const parse = (text: string) => parseConfigToml(text, { source: "config.toml", baseDir: "/cfg", home: "/home/t" })
    const file = parse('surprise = 1\n[ui]\nfoo = "bar"\n[voice.hex]\nraw = true\n[profiles.p]\ntoken_file = "~/t.token"\nsocket = "run/x.sock"\n')
    expect(file.warnings).toEqual([
      "config.toml: [ui] foo is not a known setting (ignored)",
      "config.toml: [voice.hex] raw is not a known setting (ignored)",
      "config.toml: surprise is not a known setting (ignored)",
    ])
    expect(file.profiles.get("p")).toMatchObject({ tokenFile: "/home/t/t.token", socket: "/cfg/run/x.sock" })
    const failures: Array<[string, string | RegExp]> = [
      ["[ui]\nreduce_motion = \"yes\"\n", "config.toml: [ui] reduce_motion must be true or false"],
      ["[ui]\nmax_turns_in_memory = 3\n", "config.toml: [ui] max_turns_in_memory must be an integer from 10 to 5000"],
      ["[ui]\ncolor = \"rainbow\"\n", "config.toml: [ui] color must be one of auto, always, never"],
      ["[voice]\norder = []\n", "must name at least one voice source"],
      ["[voice.hex]\ncommand = []\n", "config.toml: [voice.hex] command must be a non-empty array"],
      ["[profiles.p]\ntenant = \"a\"\ngateway_url = \"http://127.0.0.1:1\"\n", "config.toml: [profiles.p] sets more than one of tenant, gateway_url and socket"],
      ["[profiles.p]\ntenant = \"a\"\ntoken_file = \"x\"\n", "config.toml: [profiles.p] token_file cannot be used with tenant"],
      ["[profiles.p]\ntransport = \"socket\"\ntenant = \"a\"\n", "config.toml: [profiles.p] transport socket conflicts"],
      ["[profiles.p]\ngateway_url = \"http://127.0.0.1:1/x\"\n", "config.toml: [profiles.p] gateway_url must not have a path"],
      ["[profiles.p]\nthread = \"yesterday\"\n", "thread must be last, new, assistant or a thread UUID"],
      ["default_profile = \"ghost\"\n", 'default_profile "ghost" has no [profiles.ghost] table'],
      // A TOML syntax error names the file, plus the line where the runtime reports it: Bun 1.3 does
      // (BuildMessage.position); Bun 1.4 (the CI image) throws a SyntaxError without the TOML position.
      ["a = \n", /^config\.toml(?: line 1)?: \S/],
    ]
    for (const [text, message] of failures) expect(() => parse(text)).toThrow(message)
    if (Bun.version.startsWith("1.3.")) expect(() => parse("a = \n")).toThrow("config.toml line 1: ")
  })

  test("profile names outside ^[a-z0-9_-]{1,32}$ are refused (flag, environment, table name, default_profile)", async () => {
    expect(flagError(["--profile", "Derpy"]).message).toContain("must match ^[a-z0-9_-]{1,32}$")
    expect(flagError(["--profile", "a".repeat(33)]).message).toContain("must match")
    expect(flagError(["--profile", "../etc"]).message).toContain("must match")
    const sb = await sandbox()
    const env = await configError(loadConfig(sb.options([], { LUNARWING_TUI_PROFILE: "x/y" })))
    expect(env.message).toContain("LUNARWING_TUI_PROFILE: profile name")
    const parse = (text: string) => parseConfigToml(text, { source: "config.toml", baseDir: "/cfg", home: "/home/t" })
    expect(() => parse('[profiles."Work Laptop"]\ntransport = "gateway"\n')).toThrow("must match ^[a-z0-9_-]{1,32}$")
    expect(() => parse('default_profile = "x y"\n')).toThrow("must match")
    // Valid names that are also Object.prototype keys stay plain map keys.
    const tricky = parse('[profiles.constructor]\ntransport = "gateway"\n[profiles.__proto__]\nthread = "new"\n')
    expect(tricky.profiles.get("constructor")?.transport).toBe("gateway")
    expect(tricky.profiles.get("__proto__")?.thread).toEqual({ kind: "new" })
    expect(tricky.profiles.has("toString")).toBe(false)
    await sb.writeToken("hasownproperty", TOKEN)
    const config = await ready(sb.options(["--profile", "hasownproperty", "--gateway-url", "http://127.0.0.1:8765"]))
    expect(config.profile).toBe("hasownproperty")
  })
})

describe("child environment (3.11.3)", () => {
  test("child env holds no token variable, no proxy variable and no TRANSCRIBE_DUMP_DIR", () => {
    const source = {
      PATH: "/usr/bin:/bin",
      HOME: "/home/t",
      LANG: "en_US.UTF-8",
      LC_ALL: "C.UTF-8",
      LC_MESSAGES: "C",
      XDG_RUNTIME_DIR: "/run/user/1000",
      XDG_DATA_HOME: "",
      WAYLAND_DISPLAY: "wayland-1",
      DBUS_SESSION_BUS_ADDRESS: "unix:path=/run/user/1000/bus",
      PIPEWIRE_RUNTIME_DIR: "/run/user/1000",
      HEX_APPLICATION_SUPPORT_DIR: "/home/t/.local/share/hex",
      LUNARWING_GATEWAY_TOKEN: SECRET,
      LUNARWING_GATEWAY_TOKEN_FILE: "/home/t/x.token",
      GATEWAY_AUTH_TOKEN: SECRET,
      VOICE_AUTH_TOKEN: SECRET,
      HTTP_PROXY: "http://proxy.invalid:3128",
      https_proxy: "http://proxy.invalid:3128",
      ALL_PROXY: "socks5://proxy.invalid:1080",
      NO_PROXY: "*",
      TRANSCRIBE_DUMP_DIR: "/tmp/hex-dump",
      SSH_AUTH_SOCK: "/run/user/1000/ssh",
      TERM: "xterm-kitty",
    }
    const env = buildChildEnv(source)
    expect(Object.keys(env).sort()).toEqual(
      ["DBUS_SESSION_BUS_ADDRESS", "HEX_APPLICATION_SUPPORT_DIR", "HOME", "LANG", "LC_ALL", "LC_MESSAGES", "PATH", "PIPEWIRE_RUNTIME_DIR", "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR"].sort(),
    )
    expect(Object.values(env).join("\n")).not.toContain(SECRET)
    expect(Object.isFrozen(env)).toBe(true)
    expect(buildChildEnv(source, { HEX_APPLICATION_SUPPORT_DIR: "/override" }).HEX_APPLICATION_SUPPORT_DIR).toBe("/override")
    for (const name of ["HTTP_PROXY", "no_proxy", "TRANSCRIBE_DUMP_DIR", "LUNARWING_GATEWAY_TOKEN", "HEX_TOKEN", "MY_API_KEY", "bad name"]) {
      expect(() => buildChildEnv(source, { [name]: "x" })).toThrow(TypeError)
    }
    const fromProcess = buildChildEnv()
    for (const name of Object.keys(fromProcess)) {
      expect((CHILD_ENV_NAMES as readonly string[]).includes(name) || /^LC_[A-Z_]+$/.test(name)).toBe(true)
    }
  })
})

describe("paths (3.9, 3.11.2, 3.11.6)", () => {
  test("defaultSocketPath keeps the daemon's resolution order; --socket, LUNARWING_SOCKET and the profile come first", () => {
    const home = "/home/t"
    expect(defaultSocketPath({ LUNARWING_SOCKET: "/a.sock", IRONCLAW_SOCKET: "/b.sock", XDG_RUNTIME_DIR: "/run/user/1" }, home)).toBe("/a.sock")
    expect(defaultSocketPath({ LUNARWING_SOCKET: "", IRONCLAW_SOCKET: "/b.sock", XDG_RUNTIME_DIR: "/run/user/1" }, home)).toBe("/b.sock")
    expect(defaultSocketPath({ XDG_RUNTIME_DIR: "/run/user/1", LUNARWING_BASE_DIR: "/base" }, home)).toBe("/run/user/1/lunarwing.sock")
    expect(defaultSocketPath({ LUNARWING_BASE_DIR: "/base", IRONCLAW_BASE_DIR: "/iron" }, home)).toBe("/base/lunarwing.sock")
    expect(defaultSocketPath({ IRONCLAW_BASE_DIR: "/iron" }, home)).toBe("/iron/lunarwing.sock")
    expect(defaultSocketPath({}, home)).toBe("/home/t/.ironclaw/lunarwing.sock")
    const env = { LUNARWING_SOCKET: "/env.sock", XDG_RUNTIME_DIR: "/run/user/1" }
    expect(resolveSocketPath({ flag: "rel.sock", profile: "/p.sock", env, home, cwd: "/work" })).toBe("/work/rel.sock")
    expect(resolveSocketPath({ profile: "/p.sock", env, home, cwd: "/work" })).toBe("/env.sock")
    expect(resolveSocketPath({ profile: "/p.sock", env: { XDG_RUNTIME_DIR: "/run/user/1" }, home, cwd: "/work" })).toBe("/p.sock")
    expect(resolveSocketPath({ env: { XDG_RUNTIME_DIR: "/run/user/1" }, home, cwd: "/work" })).toBe("/run/user/1/lunarwing.sock")
  })

  test("config, state and runtime directories honour LUNARWING_TUI_CONFIG_DIR and the XDG variables", () => {
    const home = "/home/t"
    expect(configDirCandidates({ LUNARWING_TUI_CONFIG_DIR: "~/cfg", XDG_CONFIG_HOME: "/x" }, home)).toEqual(["/home/t/cfg"])
    expect(configDirCandidates({ XDG_CONFIG_HOME: "/x" }, home)).toEqual(["/x/wingtip", "/home/t/.config/wingtip"])
    expect(configDirCandidates({ XDG_CONFIG_HOME: "relative" }, home)).toEqual(["/home/t/.config/wingtip"])
    expect(resolveStateDir({ XDG_STATE_HOME: "/s" }, home)).toBe("/s/wingtip")
    expect(resolveStateDir({}, home)).toBe("/home/t/.local/state/wingtip")
    expect(resolveRuntimeDir({ XDG_RUNTIME_DIR: "/run/user/1" })).toBe("/run/user/1/wingtip")
    expect(resolveRuntimeDir({})).toBeNull()
  })

  test("an unwritable state directory is a ConfigError, not a crash", async () => {
    const sb = await sandbox()
    await sb.writeToken("default", TOKEN)
    const url = ["--gateway-url", "http://127.0.0.1:8765"]
    // A regular file where the directory must go fails for every uid, root included (the CI container runs as root).
    const blocker = await writeFileWithMode(join(sb.root, "not-a-dir"), "", 0o600)
    const notDir = await configError(loadConfig(sb.options(url, { XDG_STATE_HOME: blocker })))
    expect(notDir.message).toContain("cannot be created (ENOTDIR)")
    // A 0500 parent refuses the mkdir with EACCES, but root bypasses permission bits, so only a non-root run can show it.
    if (UID === 0) return
    const locked = join(sb.root, "readonly")
    await mkdir(locked, { mode: 0o500 })
    const error = await configError(loadConfig(sb.options(url, { XDG_STATE_HOME: locked })))
    expect(error.message).toContain("cannot be created (EACCES)")
  })

  test("the state directory is created 0700 and the config dir falls back to ~/.config/wingtip", async () => {
    const sb = await sandbox()
    await sb.writeToken("default", TOKEN)
    const custom = join(sb.root, "xdg-config")
    const config = await ready(sb.options(["--gateway-url", "http://127.0.0.1:8765"], { XDG_CONFIG_HOME: custom }))
    expect(config.dirs.config).toBe(sb.configDir)
    expect(config.dirs.state).toBe(join(sb.root, "state", "wingtip"))
    expect((await stat(config.dirs.state)).mode & 0o777).toBe(0o700)
  })
})

describe("debug log (3.11.6)", () => {
  test("opt-in, 0600, JSON lines of metadata only: free text and token-like strings are redacted", async () => {
    expect(await openDebugLog(null)).toBe(NULL_DEBUG_LOG)
    const dir = await makeTempDir()
    const path = join(dir, "debug.log")
    let now = 1_000
    const log = await openDebugLog(path, { now: () => now })
    now = 1_250
    log.event("turn.delta", {
      bytes: 120,
      status: 202,
      code: "rate_limited",
      route: "/api/chat/send",
      ok: true,
      text: "hello, this is the user's message",
      token: TOKEN,
      hex: "0123456789abcdef0123456789abcdef",
      secret: new Secret(SECRET) as unknown as string,
      nan: Number.NaN,
      "Bad-Key": 1,
    })
    log.event("not a type", { n: 1 })
    await log.close()
    log.event("after.close", {})
    expect((await stat(path)).mode & 0o777).toBe(0o600)
    const text = await readFile(path, "utf8")
    expect(text).not.toContain(TOKEN)
    expect(text).not.toContain(SECRET)
    expect(text).not.toContain("hello")
    const lines = text.trim().split("\n").map((line) => JSON.parse(line) as Record<string, unknown>)
    expect(lines).toEqual([
      { t: 1_000, type: "log.open", pid: process.pid },
      {
        t: 1_250,
        type: "turn.delta",
        bytes: 120,
        status: 202,
        code: "rate_limited",
        route: "/api/chat/send",
        ok: true,
        text: "[redacted]",
        token: "[redacted]",
        hex: "[redacted]",
        nan: null,
      },
      { t: 1_250, type: "invalid", n: 1 },
    ])
    expect(safeTag("lunarvoice")).toBe("lunarvoice")
    expect(safeTag("abcdefghijklmnopq")).toBe("[redacted]")
  })

  test("an existing wider log is tightened to 0600; a symlinked log path is refused", async () => {
    const dir = await makeTempDir()
    const path = await writeFileWithMode(join(dir, "debug.log"), "", 0o644)
    const log = await openDebugLog(path)
    await log.close()
    expect((await stat(path)).mode & 0o777).toBe(0o600)
    const target = join(dir, "target.log")
    await writeFile(target, "")
    await symlink(target, join(dir, "link.log"))
    await expect(openDebugLog(join(dir, "link.log"))).rejects.toThrow(ConfigError)
    await expect(openDebugLog(join(dir, "link.log"))).rejects.toThrow("symbolic link")
  })

  test("--debug-log and LUNARWING_TUI_DEBUG_LOG resolve the path; the default lives in the state directory", async () => {
    const sb = await sandbox()
    await sb.writeToken("default", TOKEN)
    const url = ["--gateway-url", "http://127.0.0.1:8765"]
    expect((await ready(sb.options(url))).debugLogPath).toBeNull()
    expect((await ready(sb.options([...url, "--debug-log"]))).debugLogPath).toBe(join(sb.root, "state", "wingtip", "debug.log"))
    expect((await ready(sb.options([...url, "--debug-log", "logs/x.log"]))).debugLogPath).toBe(join(sb.root, "logs", "x.log"))
    expect((await ready(sb.options(url, { LUNARWING_TUI_DEBUG_LOG: "~/tui.log" }))).debugLogPath).toBe(join(sb.home, "tui.log"))
  })
})
