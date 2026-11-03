/**
 * The per-profile instance lock and the secondary-instance rules (design 3.11.5, F3, 11.3.2).
 */
import { describe, expect, test } from "bun:test"
import { lstat, mkdir, readFile, readlink, stat, symlink, writeFile } from "node:fs/promises"
import { join } from "node:path"
import { loadConfig } from "../../src/config/config.ts"
import { ConfigError } from "../../src/config/flags.ts"
import { acquireProfileLock } from "../../src/config/instance-lock.ts"
import { ensureStateDir, openStateFile } from "../../src/config/state-file.ts"
import { makeTempDir, writeFileWithMode } from "../helpers/tmp.ts"

/** Above the kernel's pid_max (at most 2^22 on Linux), so process.kill(pid, 0) always fails with ESRCH. */
const DEAD_PID = 2_147_483_646

async function stateDir(): Promise<string> {
  const dir = join(await makeTempDir(), "state", "lunarwing-tui")
  await ensureStateDir(dir)
  return dir
}

describe("instance lock (3.11.5)", () => {
  test("the first instance is primary and holds a 0600 lock with its pid", async () => {
    const dir = await stateDir()
    const lock = await acquireProfileLock(dir, "derpy")
    expect(lock.primary).toBe(true)
    expect(await readFile(join(dir, "derpy.lock"), "utf8")).toBe(`${process.pid}\n`)
    expect((await stat(join(dir, "derpy.lock"))).mode & 0o777).toBe(0o600)
    await lock.release()
    await expect(lstat(join(dir, "derpy.lock"))).rejects.toThrow()
  })

  test("a second instance is secondary and never writes lastThreadId", async () => {
    const dir = await stateDir()
    const first = await acquireProfileLock(dir, "derpy")
    const second = await acquireProfileLock(dir, "derpy")
    expect(first.primary).toBe(true)
    expect(second.primary).toBe(false)

    const secondary = openStateFile({ stateDir: dir, profile: "derpy", primary: second.primary })
    const written = await secondary.update((s) => ({
      ...s,
      lastThreadId: "4b0c7d0e-1d1a-4c49-9a5c-2a1f3f0f9e11",
      lastReplThread: "tui-derpy-0123456789abcdef",
      voiceAck: { lunarvoice: "2026-11-03T10:00:00Z" },
    }))
    expect(written).toEqual({ voiceAck: { lunarvoice: "2026-11-03T10:00:00Z" } })
    await secondary.updateGlobal((g) => ({ ...g, lastTenant: "derpy" }))
    expect(JSON.parse(await readFile(join(dir, "derpy.json"), "utf8"))).toEqual({ voiceAck: { lunarvoice: "2026-11-03T10:00:00Z" } })
    await expect(lstat(join(dir, "state.json"))).rejects.toThrow()

    const primary = openStateFile({ stateDir: dir, profile: "derpy", primary: first.primary })
    await primary.update((s) => ({ ...s, lastThreadId: "4b0c7d0e-1d1a-4c49-9a5c-2a1f3f0f9e11" }))
    expect(await secondary.read()).toEqual({
      lastThreadId: "4b0c7d0e-1d1a-4c49-9a5c-2a1f3f0f9e11",
      voiceAck: { lunarvoice: "2026-11-03T10:00:00Z" },
    })
    // The secondary keeps the primary's value instead of erasing it.
    await secondary.update(() => ({}))
    expect((await primary.read()).lastThreadId).toBe("4b0c7d0e-1d1a-4c49-9a5c-2a1f3f0f9e11")

    await second.release()
    expect(await readFile(join(dir, "derpy.lock"), "utf8")).toBe(`${process.pid}\n`)
    await first.release()
    await first.release()
  })

  test("a stale lock with a dead pid is replaced", async () => {
    const dir = await stateDir()
    await writeFileWithMode(join(dir, "derpy.lock"), `${DEAD_PID}\n`, 0o600)
    const lock = await acquireProfileLock(dir, "derpy")
    expect(lock.primary).toBe(true)
    expect(await readFile(join(dir, "derpy.lock"), "utf8")).toBe(`${process.pid}\n`)
    await lock.release()
  })

  test("a lock without a readable pid counts as stale; a live pid (or EPERM) counts as held", async () => {
    const dir = await stateDir()
    await writeFileWithMode(join(dir, "a.lock"), "garbage\n", 0o600)
    const replaced = await acquireProfileLock(dir, "a")
    expect(replaced.primary).toBe(true)
    await replaced.release()

    await writeFileWithMode(join(dir, "c.lock"), "x".repeat(1000), 0o600)
    const oversized = await acquireProfileLock(dir, "c")
    expect(oversized.primary).toBe(true)
    await oversized.release()

    await writeFileWithMode(join(dir, "b.lock"), "1\n", 0o600)
    const held = await acquireProfileLock(dir, "b", { isAlive: (pid) => pid === 1 })
    expect(held.primary).toBe(false)
    expect(await readFile(join(dir, "b.lock"), "utf8")).toBe("1\n")
  })

  test("a symlinked lock path is refused and its target is left alone", async () => {
    const dir = await stateDir()
    const target = join(dir, "elsewhere")
    await writeFile(target, "keep me\n")
    await symlink(target, join(dir, "derpy.lock"))
    await expect(acquireProfileLock(dir, "derpy")).rejects.toThrow(ConfigError)
    await expect(acquireProfileLock(dir, "derpy")).rejects.toThrow(/symbolic link/)
    expect(await readFile(target, "utf8")).toBe("keep me\n")
    expect(await readlink(join(dir, "derpy.lock"))).toBe(target)

    await mkdir(join(dir, "dir.lock"))
    await expect(acquireProfileLock(dir, "dir")).rejects.toThrow(/not a regular file/)
  })

  test("profile names that are not ^[a-z0-9_-]{1,32}$ never become lock paths", async () => {
    const dir = await stateDir()
    for (const bad of ["../x", "Derpy", "", "a".repeat(33), "a/b"]) {
      await expect(acquireProfileLock(dir, bad)).rejects.toThrow(ConfigError)
    }
  })

  test("a second window of a profile uses a new gateway thread unless --thread was given", async () => {
    const root = await makeTempDir()
    const configDir = join(root, "config")
    await mkdir(join(configDir, "tokens"), { recursive: true, mode: 0o700 })
    await writeFileWithMode(join(configDir, "tokens", "devgw.token"), "lw-dev-token-0123456789abcdef\n", 0o600)
    const env = { HOME: join(root, "home"), LUNARWING_TUI_CONFIG_DIR: configDir, XDG_STATE_HOME: join(root, "state") }
    const argv = ["--profile", "devgw", "--gateway-url", "http://127.0.0.1:8765"]
    const options = { env, home: join(root, "home"), user: "tester", cwd: root }

    const first = await loadConfig({ ...options, argv })
    const second = await loadConfig({ ...options, argv })
    const third = await loadConfig({ ...options, argv: [...argv, "--thread", "last"] })
    if (first.kind !== "ready" || second.kind !== "ready" || third.kind !== "ready") throw new Error("expected ready")
    expect(first.config.primary).toBe(true)
    expect(first.config.thread).toEqual({ kind: "last" })
    expect(second.config.primary).toBe(false)
    expect(second.config.thread).toEqual({ kind: "new" })
    expect(third.config.primary).toBe(false)
    expect(third.config.thread).toEqual({ kind: "last" })
    expect(third.config.threadExplicit).toBe(true)
    await Promise.all([first.config.lock.release(), second.config.lock.release(), third.config.lock.release()])
  })
})
