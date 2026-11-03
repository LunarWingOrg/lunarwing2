/**
 * Hex discovery (design 5.3.2; cases of 11.3.2): the discovery file checks, pid liveness, the listener-owner
 * check before any token is sent, the support-dir override, the 5 s cache and poll, and the binary version gate.
 * The app is test/mocks/mock-hex.ts on 127.0.0.1:0; `hex --version` is a FakeSpawn script; /proc/net/tcp is an
 * injected reader. Nothing real is spawned and no real Hex is contacted.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { chmod, symlink } from "node:fs/promises"
import { join } from "node:path"
import { ManualClock } from "@opentui/core/testing"
import { REDACTED, isSecret } from "../../src/config/secret.ts"
import { DEFAULT_VOICE_CONFIG, type VoiceConfig } from "../../src/config/types.ts"
import { createHexEmbeddedSource } from "../../src/voice/sources/hex-embedded.ts"
import {
  createHexLocator,
  hexDiscoveryPath,
  hexLocatorFor,
  hexSupportDir,
  HEX_LOCATE_TIMING,
  parseHexDiscovery,
  parseHexVersion,
  pidState,
  supportsEmbedded,
  type HexEndpoint,
  type HexLocator,
  type HexLocatorOptions,
  type HexUnavailable,
} from "../../src/voice/sources/hex-locate.ts"
import { createFakeSpawn, type FakeSpawn } from "../helpers/fake-spawn.ts"
import { makeTempDir, writeFileWithMode } from "../helpers/tmp.ts"
import { procReaderFor } from "../mocks/mock-gateway.ts"
import { MockHex, randomHexToken } from "../mocks/mock-hex.ts"

const uid = process.getuid?.() ?? 0
const never = new AbortController().signal
const mocks: MockHex[] = []

afterEach(async () => {
  for (const mock of mocks.splice(0)) await mock.stop()
})

async function startHex(opts: Parameters<typeof MockHex.start>[0] = {}): Promise<MockHex> {
  const mock = await MockHex.start(opts)
  mocks.push(mock)
  return mock
}

function errno(code: string): Error {
  return Object.assign(new Error(`kill ${code}`), { code })
}

function locatorFor(mock: MockHex, overrides: Partial<HexLocatorOptions> = {}): { locator: HexLocator; clock: ManualClock } {
  const clock = new ManualClock()
  const locator = createHexLocator({
    clock,
    command: ["hex"],
    env: { HEX_APPLICATION_SUPPORT_DIR: mock.supportDir, HOME: "/nonexistent-home" },
    uid,
    readProc: procReaderFor(mock.port, uid),
    which: () => null,
    ...overrides,
  })
  return { locator, clock }
}

function expectEndpoint(result: HexEndpoint | HexUnavailable): HexEndpoint {
  if ("unavailable" in result) throw new Error(`expected an endpoint, got: ${result.unavailable}`)
  return result
}

function expectUnavailable(result: unknown): string {
  if (typeof result === "object" && result !== null && "unavailable" in result) return String((result as HexUnavailable).unavailable)
  throw new Error("expected { unavailable }")
}

describe("running-app discovery", () => {
  test("valid file accepted", async () => {
    const mock = await startHex()
    const { locator } = locatorFor(mock)
    const endpoint = expectEndpoint(await locator.locateApp(never))
    expect(endpoint).toMatchObject({ baseUrl: `http://127.0.0.1:${mock.port}`, pid: process.pid, origin: "app", version: "3.3.0" })
    expect(isSecret(endpoint.token)).toBe(true)
    expect(String(endpoint.token)).toBe(REDACTED)
    expect(JSON.stringify(endpoint)).not.toContain(mock.token)
    expect(endpoint.token.expose()).toBe(mock.token)
    const health = mock.requestsTo("/health")
    expect(health).toHaveLength(1)
    expect(health[0]?.headers.authorization).toBe(`Bearer ${mock.token}`)
    expect(health[0]?.headers.host).toBe(`127.0.0.1:${mock.port}`)
  })

  test("0640 refused", async () => {
    const mock = await startHex()
    await chmod(mock.discoveryPath, 0o640)
    const { locator } = locatorFor(mock)
    const reason = expectUnavailable(await locator.locateApp(never))
    expect(reason).toContain("mode 0640")
    expect(reason).not.toContain(mock.token)
    expect(mock.requests).toHaveLength(0)
  })

  test("symlink refused", async () => {
    const mock = await startHex()
    await mock.symlinkDiscovery()
    const { locator } = locatorFor(mock)
    expect(expectUnavailable(await locator.locateApp(never))).toContain("symbolic link")
    expect(mock.requests).toHaveLength(0)
  })

  test("a file owned by another uid or an oversized file is refused", async () => {
    const mock = await startHex()
    expect(expectUnavailable(await locatorFor(mock, { uid: uid + 1 }).locator.locateApp(never))).toContain(`owned by uid ${uid}`)
    await writeFileWithMode(mock.discoveryPath, `${JSON.stringify({ ...mock.discoveryDocument(), pad: "x".repeat(5000) })}\n`, 0o600)
    expect(expectUnavailable(await locatorFor(mock).locator.locateApp(never))).toContain("larger than 4096 bytes")
    expect(mock.requests).toHaveLength(0)
  })

  test("a missing file means not running", async () => {
    const mock = await startHex({ mode: "embedded" })
    expect(expectUnavailable(await locatorFor(mock).locator.locateApp(never))).toBe("not running")
  })

  test("dead pid is stale", async () => {
    const mock = await startHex()
    // Above pid_max: kill(pid, 0) answers ESRCH on every Linux host.
    await mock.writeDiscovery({ pid: 2_147_483_647 })
    const { locator } = locatorFor(mock)
    expect(expectUnavailable(await locator.locateApp(never))).toBe("not running (stale local-api.json)")
    expect(mock.requests).toHaveLength(0)
  })

  test("EPERM refused", async () => {
    const mock = await startHex()
    const { locator } = locatorFor(mock, {
      kill: () => {
        throw errno("EPERM")
      },
    })
    expect(expectUnavailable(await locator.locateApp(never))).toBe(`local-api.json names pid ${process.pid}, which is not this user's process; refused`)
    expect(mock.requests).toHaveLength(0)
  })

  test("pidState maps ESRCH and EPERM", () => {
    expect(pidState(process.pid)).toBe("alive")
    expect(pidState(42, () => { throw errno("ESRCH") })).toBe("dead")
    expect(pidState(42, () => { throw errno("EPERM") })).toBe("foreign")
    expect(pidState(42, () => { throw errno("EINVAL") })).toBe("foreign")
  })

  test("a port held by another uid is refused before any request", async () => {
    const mock = await startHex()
    const reader = procReaderFor(mock.port, uid + 7)
    const { locator } = locatorFor(mock, { readProc: reader })
    expect(expectUnavailable(await locator.locateApp(never))).toBe(`port ${mock.port} is held by uid ${uid + 7}, expected ${uid} (this user)`)
    expect(reader.reads).toBeGreaterThan(0)
    expect(mock.requests).toHaveLength(0)
  })

  test("nothing listening on the published port means not running", async () => {
    const mock = await startHex()
    const { locator } = locatorFor(mock, { readProc: procReaderFor(mock.port + 1 > 65_535 ? 1 : mock.port + 1, uid) })
    expect(expectUnavailable(await locator.locateApp(never))).toBe(`not running (nothing listens on 127.0.0.1:${mock.port})`)
    expect(mock.requests).toHaveLength(0)
  })

  test("token format and apiVersion validated", async () => {
    const mock = await startHex()
    const cases: [Record<string, unknown>, string][] = [
      [{ token: mock.token.toUpperCase().replace("HEX_", "hex_") }, "local-api.json holds no valid Hex token"],
      [{ token: `${mock.token}0` }, "local-api.json holds no valid Hex token"],
      [{ token: 42 }, "local-api.json holds no valid Hex token"],
      [{ apiVersion: "1" }, "Hex local API version 1 is not supported (this client speaks 2)"],
      [{ apiVersion: 2 }, "Hex local API version unknown is not supported (this client speaks 2)"],
      [{ port: 0 }, "local-api.json has an invalid port"],
      [{ port: 70_000 }, "local-api.json has an invalid port"],
      [{ pid: 0 }, "local-api.json has an invalid pid"],
      [{ pid: -1 }, "local-api.json has an invalid pid"],
    ]
    for (const [change, reason] of cases) {
      await mock.writeDiscovery(change)
      const result = await locatorFor(mock).locator.locateApp(never)
      expect(expectUnavailable(result)).toBe(reason)
    }
    await writeFileWithMode(mock.discoveryPath, "{not json", 0o600)
    expect(expectUnavailable(await locatorFor(mock).locator.locateApp(never))).toBe("local-api.json is not valid JSON")
    expect(mock.requests).toHaveLength(0)
    expect(expectUnavailable(parseHexDiscovery([]))).toBe("local-api.json is not a JSON object")
  })

  test("a token Hex rejects and a /health without API version 2 make the app unavailable", async () => {
    const mock = await startHex()
    await mock.writeDiscovery({ token: randomHexToken() })
    expect(expectUnavailable(await locatorFor(mock).locator.locateApp(never))).toBe("Hex rejected the token in local-api.json")
    await mock.writeDiscovery()
    mock.override("GET /health", () => Response.json({ version: "9.0.0", apiVersion: "3" }))
    expect(expectUnavailable(await locatorFor(mock).locator.locateApp(never))).toBe("Hex answered /health without API version 2")
  })

  test("HEX_APPLICATION_SUPPORT_DIR override", async () => {
    expect(hexSupportDir({ HEX_APPLICATION_SUPPORT_DIR: "/srv/hex-data", XDG_DATA_HOME: "/data", HOME: "/home/u" })).toBe("/srv/hex-data")
    expect(hexSupportDir({ HEX_APPLICATION_SUPPORT_DIR: "", XDG_DATA_HOME: "/data", HOME: "/home/u" })).toBe("/data/hex")
    expect(hexSupportDir({ XDG_DATA_HOME: "relative/data", HOME: "/home/u" })).toBe("/home/u/.local/share/hex")
    expect(hexSupportDir({ HOME: "/home/u" })).toBe("/home/u/.local/share/hex")
    expect(hexDiscoveryPath({ HOME: "/home/u" })).toBe("/home/u/.local/share/hex/local-api.json")

    // The same file under XDG_DATA_HOME/hex is found, and the override wins over it.
    const mock = await startHex()
    const data = await makeTempDir()
    const viaXdg = createHexLocator({ clock: new ManualClock(), command: ["hex"], env: { XDG_DATA_HOME: data, HOME: "/nonexistent" }, uid, readProc: procReaderFor(mock.port, uid) })
    expect(viaXdg.discoveryPath).toBe(join(data, "hex", "local-api.json"))
    expect(expectUnavailable(await viaXdg.locateApp(never))).toBe("not running")
    const viaOverride = createHexLocator({
      clock: new ManualClock(),
      command: ["hex"],
      env: { HEX_APPLICATION_SUPPORT_DIR: mock.supportDir, XDG_DATA_HOME: data, HOME: "/nonexistent" },
      uid,
      readProc: procReaderFor(mock.port, uid),
    })
    expect(viaOverride.discoveryPath).toBe(mock.discoveryPath)
    expectEndpoint(await viaOverride.locateApp(never))
  })

  test("results are cached for 5 s; invalidate() forgets them", async () => {
    const mock = await startHex()
    const { locator, clock } = locatorFor(mock)
    expectEndpoint(await locator.locateApp(never))
    expectEndpoint(await locator.locateApp(never))
    expect(mock.requestsTo("/health")).toHaveLength(1)
    clock.advance(HEX_LOCATE_TIMING.cacheMs)
    expectEndpoint(await locator.locateApp(never))
    expect(mock.requestsTo("/health")).toHaveLength(2)
    locator.invalidate()
    expectEndpoint(await locator.locateApp(never))
    expect(mock.requestsTo("/health")).toHaveLength(3)
  })

  test("a changed discovery file invalidates the cache and notifies; the poll runs every 5 s while someone listens", async () => {
    const mock = await startHex()
    const { locator, clock } = locatorFor(mock)
    expectEndpoint(await locator.locateApp(never))
    let hints = 0
    const off = locator.onDiscoveryChange(() => {
      hints += 1
    })
    expect(clock.pendingTimerCount).toBe(1)
    expect(await locator.checkDiscovery()).toBe(false)
    await mock.removeDiscovery()
    expect(await locator.checkDiscovery()).toBe(true)
    expect(hints).toBe(1)
    expect(expectUnavailable(await locator.locateApp(never))).toBe("not running")

    await mock.writeDiscovery()
    clock.advance(HEX_LOCATE_TIMING.pollMs)
    await locator.checkDiscovery()
    expect(hints).toBe(2)
    expectEndpoint(await locator.locateApp(never))
    off()
    expect(clock.pendingTimerCount).toBe(0)
  })

  test("one locator per createVoiceSources deps object", () => {
    const deps = { clock: new ManualClock(), config: DEFAULT_VOICE_CONFIG }
    expect(hexLocatorFor(deps)).toBe(hexLocatorFor(deps))
    expect(hexLocatorFor({ ...deps })).not.toBe(hexLocatorFor(deps))
    const given = hexLocatorFor({ ...deps })
    expect(hexLocatorFor({ ...deps, hex: { locator: given } })).toBe(given)
  })
})

describe("binary discovery", () => {
  interface BinaryRig {
    fake: FakeSpawn
    clock: ManualClock
    locator: HexLocator
    dir: string
  }

  async function binaryRig(version: string | null, command: readonly string[] = ["hex"]): Promise<BinaryRig> {
    const clock = new ManualClock()
    const fake = createFakeSpawn({ clock })
    if (version !== null) fake.on(["/opt/hex/bin/hex", "--version"], { stdout: [`${version}\n`] })
    const dir = await makeTempDir()
    const locator = createHexLocator({
      clock,
      command,
      env: { HEX_APPLICATION_SUPPORT_DIR: dir, HOME: "/nonexistent" },
      childEnv: { PATH: "/opt/hex/bin:/usr/bin" },
      uid,
      spawn: fake.spawn,
      which: (cmd, opts) => (cmd === "hex" && opts?.PATH?.includes("/opt/hex/bin") ? "/opt/hex/bin/hex" : cmd === "/opt/hex/bin/hex" ? cmd : null),
      readProc: procReaderFor(1, uid),
    })
    return { fake, clock, locator, dir }
  }

  function embeddedConfig(): VoiceConfig {
    return { ...DEFAULT_VOICE_CONFIG, hex: { command: ["hex"], model: "" } }
  }

  test("a binary older than 3.3.0 leaves hex-embedded unavailable", async () => {
    const rig = await binaryRig("hex 3.2.0")
    expect(await rig.locator.binary()).toEqual({ unavailable: "Hex 3.3.0 or later required (found 3.2.0)" })
    const source = createHexEmbeddedSource({ clock: rig.clock, config: embeddedConfig(), hex: { locator: rig.locator, spawn: rig.fake.spawn, uid } })
    const availability = await source.probe(never)
    expect(availability).toMatchObject({ available: false, reason: "Hex 3.3.0 or later required (found 3.2.0)" })
    expect(rig.fake.calls.map((c) => c.argv)).toEqual([["/opt/hex/bin/hex", "--version"]])
    expect(rig.fake.callsFor((argv) => argv.includes("--embedded"))).toHaveLength(0)
    await source.dispose()
  })

  test("3.3.0 and later enable hex-embedded; the version check runs with the child environment, once", async () => {
    const rig = await binaryRig("hex 3.3.0")
    expect(await rig.locator.binary()).toEqual({ command: ["/opt/hex/bin/hex"], version: "3.3.0" })
    expect(await rig.locator.binary()).toEqual({ command: ["/opt/hex/bin/hex"], version: "3.3.0" })
    expect(rig.fake.calls).toHaveLength(1)
    expect(rig.fake.calls[0]?.env).toEqual({ PATH: "/opt/hex/bin:/usr/bin" })
    expect(rig.fake.calls[0]?.options.stdin).toBe("ignore")
    const source = createHexEmbeddedSource({ clock: rig.clock, config: embeddedConfig(), hex: { locator: rig.locator, spawn: rig.fake.spawn, uid } })
    expect(await source.probe(never)).toMatchObject({ available: true, detail: "Hex 3.3.0; the embedded service starts at the first transcription" })
    expect(rig.fake.callsFor((argv) => argv.includes("--embedded"))).toHaveLength(0)
    await source.dispose()
  })

  test("an absolute command keeps its extra words", async () => {
    const rig = await binaryRig("hex 3.4.1-beta.2", ["/opt/hex/bin/hex", "--quiet"])
    rig.fake.on(["/opt/hex/bin/hex", "--quiet", "--version"], { stdout: ["hex 3.4.1-beta.2\n"] })
    expect(await rig.locator.binary()).toEqual({ command: ["/opt/hex/bin/hex", "--quiet"], version: "3.4.1-beta.2" })
  })

  test("a missing binary is not installed and is looked up again after 60 s", async () => {
    const clock = new ManualClock()
    let installed = false
    const fake = createFakeSpawn({ clock }).on("hex", { stdout: ["hex 3.3.0\n"] })
    const locator = createHexLocator({
      clock,
      command: ["hex"],
      env: { HOME: "/nonexistent" },
      childEnv: { PATH: "/usr/bin" },
      uid,
      spawn: fake.spawn,
      which: () => (installed ? "/usr/bin/hex" : null),
    })
    expect(await locator.binary()).toEqual({ unavailable: "not installed" })
    installed = true
    expect(await locator.binary()).toEqual({ unavailable: "not installed" })
    clock.advance(HEX_LOCATE_TIMING.binaryRetryMs)
    expect(await locator.binary()).toEqual({ command: ["/usr/bin/hex"], version: "3.3.0" })
    expect(fake.calls).toHaveLength(1)
  })

  test("garbage, a failing exit, a hang and a relative path are refused", async () => {
    const odd = await binaryRig("Hex version three")
    expect(await odd.locator.binary()).toEqual({ unavailable: "`hex --version` printed no version" })

    const failing = await binaryRig(null)
    failing.fake.on("hex", { stdout: ["hex 3.3.0\n"], exitCode: 1 })
    expect(await failing.locator.binary()).toEqual({ unavailable: "`hex --version` failed (exit 1)" })

    const hanging = await binaryRig(null)
    hanging.fake.on("hex", { stayAlive: true })
    const pending = hanging.locator.binary()
    await new Promise<void>((resolve) => setImmediate(resolve))
    hanging.clock.advance(HEX_LOCATE_TIMING.versionTimeoutMs)
    expect(await pending).toEqual({ unavailable: "`hex --version` did not answer within 2 s" })
    expect(hanging.fake.running()).toHaveLength(0)

    const relative = await binaryRig("hex 3.3.0", ["./bin/hex"])
    expect(await relative.locator.binary()).toEqual({ unavailable: "voice.hex.command must name a command or an absolute path" })
  })

  test("parseHexVersion reads clap's version line", () => {
    expect(parseHexVersion("hex 3.3.0\n")).toEqual({ major: 3, minor: 3, patch: 0, text: "3.3.0" })
    expect(parseHexVersion("hex 0.0.0-dev-local-1c70d22-dirty")).toMatchObject({ major: 0, text: "0.0.0-dev-local-1c70d22-dirty" })
    expect(parseHexVersion("hex 3.3")).toBeNull()
    expect(parseHexVersion("hexagon 3.3.0")).toBeNull()
    expect(parseHexVersion("")).toBeNull()
    const v = (text: string) => parseHexVersion(`hex ${text}`)
    expect([v("3.2.9"), v("3.3.0"), v("3.10.0"), v("4.0.0")].map((x) => (x === null ? null : supportsEmbedded(x)))).toEqual([false, true, true, true])
  })
})

test("only the file itself must not be a link: a linked support directory is Hex's own choice", async () => {
  const mock = await startHex()
  const linked = await makeTempDir()
  await symlink(mock.supportDir, join(linked, "hex"))
  const locator = createHexLocator({ clock: new ManualClock(), command: ["hex"], env: { XDG_DATA_HOME: linked, HOME: "/nonexistent" }, uid, readProc: procReaderFor(mock.port, uid) })
  expectEndpoint(await locator.locateApp(never))
})
