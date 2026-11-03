/**
 * Private temp directories for tests (design 11.3.1, 11.3.5): mkdtemp under os.tmpdir() (mode 0700), never a
 * fixed /tmp path. test/setup.ts removes every directory from makeTempDir() after each test, and every
 * directory from makeRunTempDir() when the run ends.
 *
 * makeTempDir() inside beforeAll() would be removed after the first test of the file: use makeRunTempDir()
 * for fixtures shared by several tests.
 */
import { chmod, mkdtemp, readdir, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"

const perTest = new Set<string>()
const perRun = new Set<string>()

/** A fresh 0700 directory, removed after the current test. */
export async function makeTempDir(prefix = "lw-tui-test-"): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), prefix))
  perTest.add(dir)
  return dir
}

/** A fresh 0700 directory, removed when the whole test run ends. */
export async function makeRunTempDir(prefix = "lw-tui-run-"): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), prefix))
  perRun.add(dir)
  return dir
}

/**
 * Writes `data` to `path` with exactly `mode` (chmod after the write, so the umask cannot widen or narrow it).
 * For token files (0600 vs 0644 cases), Hex discovery files and other permission-sensitive fixtures.
 */
export async function writeFileWithMode(path: string, data: string | Uint8Array, mode: number): Promise<string> {
  await writeFile(path, data, { mode })
  await chmod(path, mode)
  return path
}

/** Creates `name` with `data` and `mode` (default 0600) inside a fresh per-test directory; returns its path. */
export async function makeTempFile(name: string, data: string | Uint8Array, mode = 0o600): Promise<string> {
  const dir = await makeTempDir()
  return writeFileWithMode(join(dir, name), data, mode)
}

/** Directories still registered for removal after the current test (for assertions). */
export function pendingTempDirs(): readonly string[] {
  return [...perTest]
}

/** Restores owner rwx on every directory so a test that chmodded one to 0500 or 0000 cannot block removal. */
async function makeWritable(dir: string): Promise<void> {
  await chmod(dir, 0o700).catch(() => undefined)
  const entries = await readdir(dir, { withFileTypes: true }).catch(() => [])
  await Promise.all(entries.filter((e) => e.isDirectory()).map((e) => makeWritable(join(dir, e.name))))
}

async function forceRemove(dir: string): Promise<void> {
  try {
    await rm(dir, { recursive: true, force: true })
  } catch {
    await makeWritable(dir)
    await rm(dir, { recursive: true, force: true })
  }
}

async function removeAll(dirs: Set<string>): Promise<void> {
  const list = [...dirs]
  dirs.clear()
  await Promise.all(list.map(forceRemove))
}

/** Called by test/setup.ts in a global afterEach. */
export function cleanupTempDirs(): Promise<void> {
  return removeAll(perTest)
}

/** Called by test/setup.ts in a global afterAll. */
export function cleanupRunTempDirs(): Promise<void> {
  return removeAll(perRun)
}
