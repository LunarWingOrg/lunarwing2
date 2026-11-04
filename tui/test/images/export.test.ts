/**
 * /image open and /image save (design 6.4, UX-14, 11.3.2): open writes a 0600 file in a 0700 directory and spawns
 * xdg-open with argv only; save refuses an existing path (O_EXCL); an evicted image reports that it is no longer
 * in memory; exports are removed on dispose. xdg-open is a FakeSpawn script.
 */
import { describe, expect, test } from "bun:test"
import { chmod, lstat, mkdir, readFile, stat, symlink, writeFile } from "node:fs/promises"
import { join } from "node:path"
import { ManualClock } from "@opentui/core/testing"
import { buildChildEnv } from "../../src/config/child-env.ts"
import {
  createImageExporter,
  defaultSaveName,
  generatedImages,
  OPENER_WAIT_MS,
  parseImageCommand,
  pickGeneratedImage,
  type ImageExporterOptions,
} from "../../src/images/export.ts"
import { ImageInputError } from "../../src/images/limits.ts"
import { createInitialState, type AppState, type TranscriptImage, type TurnId, type TurnRecord } from "../../src/state/model.ts"
import { createFakeSpawn, type FakeSpawn } from "../helpers/fake-spawn.ts"
import { makeTempDir } from "../helpers/tmp.ts"

const PNG = new Uint8Array(
  Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR4nGM8IWfDwMDAxMDAwMDAAAAOTgEmR2hJIgAAAABJRU5ErkJggg==", "base64"),
)
const GIF = new Uint8Array(Buffer.from("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7", "base64"))
const ENV = buildChildEnv({ PATH: "/usr/bin:/bin", WAYLAND_DISPLAY: "wayland-1", LUNARWING_GATEWAY_TOKEN: "do-not-leak-0123456789abcdef" })
const UID = process.getuid?.() ?? 0

interface Rig {
  dir: string
  runtimeDir: string
  fake: FakeSpawn
  clock: ManualClock
  options: ImageExporterOptions
}

async function rig(overrides: Partial<ImageExporterOptions> = {}): Promise<Rig> {
  const dir = await makeTempDir()
  const runtimeDir = join(dir, "run", "wingtip")
  await mkdir(join(dir, "run"), { mode: 0o700 })
  await mkdir(join(dir, "work"))
  const clock = new ManualClock()
  const fake = createFakeSpawn({ clock })
  let n = 0
  const options: ImageExporterOptions = {
    runtimeDir,
    env: ENV,
    spawn: fake.spawn,
    clock,
    uid: UID,
    cwd: join(dir, "work"),
    home: dir,
    newId: () => `00000000-0000-4000-8000-00000000000${++n}`,
    ...overrides,
  }
  return { dir, runtimeDir, fake, clock, options }
}

async function rejection(promise: Promise<unknown>): Promise<ImageInputError> {
  try {
    await promise
  } catch (error) {
    if (error instanceof ImageInputError) return error
    throw error
  }
  throw new Error("expected an ImageInputError")
}

/** Yields to the event loop until `condition` holds (real file I/O completes there, not in microtasks). */
async function until(condition: () => boolean): Promise<void> {
  for (let i = 0; i < 5_000 && !condition(); i += 1) await new Promise((resolve) => setImmediate(resolve))
  expect(condition()).toBe(true)
}

const mode = async (path: string): Promise<number> => (await stat(path)).mode & 0o777
const generated = (bytes: Uint8Array | null, mime: TranscriptImage["mime"] = "image/png"): TranscriptImage => ({
  id: "gen",
  mime,
  bytes,
  byteLength: bytes?.byteLength ?? 1234,
})

describe("/image open", () => {
  test("open writes a 0600 file in a 0700 directory and spawns xdg-open with argv only", async () => {
    const { runtimeDir, fake, options } = await rig()
    fake.on("xdg-open", { exitCode: 0 })
    const exporter = createImageExporter(options)
    const file = await exporter.open(generated(PNG))
    expect(file).toBe(join(runtimeDir, "img-00000000-0000-4000-8000-000000000001.png"))
    expect(await mode(runtimeDir)).toBe(0o700)
    expect(await mode(file)).toBe(0o600)
    expect(new Uint8Array(await readFile(file))).toEqual(PNG)
    expect(fake.calls).toHaveLength(1)
    const call = fake.calls[0]
    expect(call?.argv).toEqual(["xdg-open", file])
    expect(call?.env).toEqual(ENV)
    expect(call?.env.LUNARWING_GATEWAY_TOKEN).toBeUndefined()
    expect(call?.options).toMatchObject({ stdin: "ignore", stdout: "ignore", stderr: "ignore" })
    expect(exporter.exported()).toEqual([file])
    await exporter.dispose()
  })

  test("exports are removed on dispose", async () => {
    const { runtimeDir, fake, options } = await rig()
    fake.on("xdg-open", { exitCode: 0 })
    const exporter = createImageExporter(options)
    const first = await exporter.open(generated(PNG))
    const second = await exporter.open(generated(GIF, "image/gif"))
    expect(second.endsWith(".gif")).toBe(true)
    await exporter.dispose()
    await expect(lstat(first)).rejects.toThrow()
    await expect(lstat(second)).rejects.toThrow()
    await expect(lstat(runtimeDir)).rejects.toThrow()
    expect((await rejection(exporter.open(generated(PNG)))).kind).toBe("failed")
  })

  test("a viewer that keeps running counts as opened; a quick failure is reported", async () => {
    const running = await rig()
    running.fake.on("xdg-open", { stayAlive: true })
    const exporter = createImageExporter(running.options)
    const pending = exporter.open(generated(PNG))
    await until(() => running.fake.calls.length === 1)
    running.clock.advance(OPENER_WAIT_MS)
    expect(await pending).toEndWith(".png")
    await exporter.dispose()

    const failing = await rig()
    failing.fake.on("xdg-open", { exitCode: 4 })
    const error = await rejection(createImageExporter(failing.options).open(generated(PNG)))
    expect(error.kind).toBe("failed")
    expect(error.message).toMatch(/^xdg-open failed \(exit 4\); the image is at ~\/run\/wingtip\/img-.*\.png until you quit$/)
  })

  test("a missing xdg-open keeps the file until quit and says where it is", async () => {
    const { fake, options } = await rig()
    const exporter = createImageExporter(options)
    const error = await rejection(exporter.open(generated(PNG)))
    expect(error.kind).toBe("unavailable")
    expect(error.message).toContain("xdg-open is not installed; the image is at ~/run/wingtip/img-")
    expect(fake.calls).toHaveLength(1)
    expect(exporter.exported()).toHaveLength(1)
    await exporter.dispose()
    expect(exporter.exported()).toHaveLength(0)
  })

  test("the runtime directory must be private, real and ours", async () => {
    const wide = await rig()
    await mkdir(wide.runtimeDir, { mode: 0o755 })
    await chmod(wide.runtimeDir, 0o755)
    const error = await rejection(createImageExporter(wide.options).open(generated(PNG)))
    expect(error.kind).toBe("unsafe_dir")
    expect(error.message).toContain("mode 0755")

    const linked = await rig()
    await mkdir(join(linked.dir, "elsewhere"), { mode: 0o700 })
    await symlink(join(linked.dir, "elsewhere"), linked.runtimeDir)
    expect((await rejection(createImageExporter(linked.options).open(generated(PNG)))).kind).toBe("unsafe_dir")

    const foreign = await rig({ uid: UID + 1 })
    expect((await rejection(createImageExporter(foreign.options).open(generated(PNG)))).message).toContain(`owned by uid ${UID}`)

    const none = await rig({ runtimeDir: null })
    expect((await rejection(createImageExporter(none.options).open(generated(PNG)))).kind).toBe("unavailable")
    expect(wide.fake.calls).toHaveLength(0)
  })

  test("an evicted image reports that it is no longer in memory", async () => {
    const { fake, options } = await rig()
    const exporter = createImageExporter(options)
    const open = await rejection(exporter.open(generated(null)))
    expect(open.kind).toBe("evicted")
    expect(open.message).toBe("image no longer in memory")
    const save = await rejection(exporter.save(generated(null), { path: null, n: 1, label: "derpy", at: new Date() }))
    expect(save.message).toBe("image no longer in memory")
    expect(fake.calls).toHaveLength(0)
  })
})

describe("/image save", () => {
  const at = new Date(2026, 10, 3, 4, 5, 6)

  test("the default name goes to the working directory as a 0600 file", async () => {
    const { dir, fake, options } = await rig()
    const exporter = createImageExporter(options)
    const path = await exporter.save(generated(PNG), { path: null, n: 2, label: "derpy", at })
    expect(path).toBe(join(dir, "work", "lunarwing-derpy-20261103-040506-2.png"))
    expect(await mode(path)).toBe(0o600)
    expect(new Uint8Array(await readFile(path))).toEqual(PNG)
    expect(fake.calls).toHaveLength(0)
    // save files are the user's: dispose leaves them alone.
    await exporter.dispose()
    expect(await mode(path)).toBe(0o600)
  })

  test("save refuses an existing path (O_EXCL)", async () => {
    const { dir, options } = await rig()
    const exporter = createImageExporter(options)
    const target = join(dir, "work", "cat.png")
    await writeFile(target, "keep me")
    const error = await rejection(exporter.save(generated(PNG), { path: "cat.png", n: 1, label: null, at }))
    expect(error.kind).toBe("exists")
    expect(error.message).toBe("~/work/cat.png already exists; choose another name")
    expect(await readFile(target, "utf8")).toBe("keep me")
    // A symbolic link, even a dangling one, is never followed or replaced.
    await symlink(join(dir, "victim.png"), join(dir, "work", "link.png"))
    expect((await rejection(exporter.save(generated(PNG), { path: "link.png", n: 1, label: null, at }))).kind).toBe("exists")
    await expect(lstat(join(dir, "victim.png"))).rejects.toThrow()
  })

  test("a directory or ~/ target gets the default name", async () => {
    const { dir, options } = await rig()
    await mkdir(join(dir, "Pictures"))
    const exporter = createImageExporter(options)
    expect(await exporter.save(generated(GIF, "image/gif"), { path: "~/Pictures", n: 1, label: "derpy", at })).toBe(
      join(dir, "Pictures", "lunarwing-derpy-20261103-040506-1.gif"),
    )
    expect(await exporter.save(generated(PNG), { path: "~/Pictures/", n: 3, label: null, at })).toBe(
      join(dir, "Pictures", "lunarwing-local-20261103-040506-3.png"),
    )
    const missing = await rejection(exporter.save(generated(PNG), { path: "~/nope/x.png", n: 1, label: null, at }))
    expect(missing.kind).toBe("not_found")
  })
})

describe("choosing the image and parsing the command", () => {
  function turn(id: string, images: TranscriptImage[]): TurnRecord {
    return {
      id: id as TurnId,
      seq: 0,
      origin: "user",
      threadId: null,
      input: { text: "draw", images: [] },
      phase: "done",
      deltas: "",
      finalText: "ok",
      activity: [],
      images,
      approvalIds: [],
      steerNotes: [],
      notes: [],
      createdAt: 0,
    }
  }

  function stateWith(turns: TurnRecord[]): AppState {
    const base = createInitialState({ profile: "derpy", tenant: "derpy", view: { width: 80, height: 24 }, at: 0 })
    return { ...base, turns: Object.fromEntries(turns.map((t) => [t.id, t])), order: turns.map((t) => t.id) }
  }

  test("newest first across turns; n picks the n-th newest", () => {
    const image = (id: string): TranscriptImage => ({ id, mime: "image/png", bytes: PNG, byteLength: PNG.length })
    const state = stateWith([turn("t1", [image("a"), image("b")]), turn("t2", []), turn("t3", [image("c")])])
    expect(generatedImages(state).map((i) => i.id)).toEqual(["c", "b", "a"])
    expect(pickGeneratedImage(state).id).toBe("c")
    expect(pickGeneratedImage(state, 3).id).toBe("a")
    expect(() => pickGeneratedImage(state, 4)).toThrow("there are 3 generated images")
    expect(() => pickGeneratedImage(stateWith([]))).toThrow("no generated image in this session")
  })

  test("parseImageCommand", () => {
    expect(parseImageCommand("open")).toEqual({ action: "open", n: 1 })
    expect(parseImageCommand("OPEN 2")).toEqual({ action: "open", n: 2 })
    expect(parseImageCommand("save")).toEqual({ action: "save", n: 1, path: null })
    expect(parseImageCommand("save 3")).toEqual({ action: "save", n: 3, path: null })
    expect(parseImageCommand("save ~/my cat.png")).toEqual({ action: "save", n: 1, path: "~/my cat.png" })
    expect(parseImageCommand("save 2 '/tmp/a b.png'")).toEqual({ action: "save", n: 2, path: "/tmp/a b.png" })
    for (const bad of ["", "view", "open x", "open 0", "save 1000"]) expect(parseImageCommand(bad).action).toBe("error")
  })

  test("defaultSaveName", () => {
    const at = new Date(2026, 0, 2, 3, 4, 5)
    expect(defaultSaveName("derpy", at, 1, "image/jpeg")).toBe("lunarwing-derpy-20260102-030405-1.jpg")
    expect(defaultSaveName("../../etc", at, 2, "image/webp")).toBe("lunarwing-etc-20260102-030405-2.webp")
    expect(defaultSaveName(null, at, 1, "image/png")).toBe("lunarwing-local-20260102-030405-1.png")
  })
})
