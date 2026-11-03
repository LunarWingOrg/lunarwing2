/**
 * /img and /attach (design 6.1, 11.3.2): a PNG from a temp dir; a FIFO rejected without blocking; a bounded read
 * when stat lies; quotes, "~" and file:// handled; the missing-file message. Also the shared PendingImage step and
 * the storage disclosure (5.6, DEC-3).
 */
import { describe, expect, test } from "bun:test"
import { existsSync } from "node:fs"
import { mkdir, symlink, writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { ProfileState } from "../../src/config/types.ts"
import {
  acknowledgeImageSource,
  createPendingImage,
  imageFileName,
  imageStorageCaption,
  imageStorageNotice,
  MAX_FILENAME_CHARS,
  readImageFile,
  resolveImagePath,
} from "../../src/images/attach.ts"
import { ImageInputError } from "../../src/images/limits.ts"
import { makeTempDir } from "../helpers/tmp.ts"

const MIB = 1024 * 1024
const PNG = new Uint8Array(
  Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR4nGM8IWfDwMDAxMDAwMDAAAAOTgEmR2hJIgAAAABJRU5ErkJggg==", "base64"),
)
const GIF = new Uint8Array(Buffer.from("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7", "base64"))

async function rejection(promise: Promise<unknown>): Promise<ImageInputError> {
  try {
    await promise
  } catch (error) {
    if (error instanceof ImageInputError) return error
    throw error
  }
  throw new Error("expected an ImageInputError")
}

async function fixture(name: string, data: Uint8Array = PNG): Promise<{ dir: string; path: string }> {
  const dir = await makeTempDir()
  const path = join(dir, name)
  await writeFile(path, data)
  return { dir, path }
}

describe("readImageFile", () => {
  test("PNG from a temp dir", async () => {
    const { dir, path } = await fixture("shot.png")
    const image = await readImageFile(path, { maxImageBytes: 5 * MIB, cwd: dir, home: dir })
    expect(image.mime).toBe("image/png")
    expect([image.width, image.height]).toEqual([2, 2])
    expect(image.filename).toBe("shot.png")
    expect(image.source).toBe("file")
    expect(image.id).toMatch(/^img-[0-9a-f-]{36}$/)
    expect(Array.from(image.bytes)).toEqual(Array.from(PNG))
    // Exactly the file: not a view into a larger read buffer.
    expect(image.bytes.buffer.byteLength).toBe(PNG.length)
  })

  test("the sniffed type wins over the extension; drops and copied files keep their source", async () => {
    const { dir, path } = await fixture("really-a-gif.png", GIF)
    const image = await readImageFile(path, { maxImageBytes: 5 * MIB, cwd: dir, source: "drop", newId: () => "img-1" })
    expect(image).toMatchObject({ id: "img-1", mime: "image/gif", source: "drop", width: 1, height: 1 })
  })

  test("FIFO rejected without blocking", async () => {
    const dir = await makeTempDir()
    const fifo = join(dir, "pipe.png")
    const mkfifo = Bun.spawn(["mkfifo", fifo], {
      env: { PATH: process.env.PATH ?? "/usr/bin:/bin" },
      stdin: "ignore",
      stdout: "ignore",
      stderr: "ignore",
    })
    expect(await mkfifo.exited).toBe(0)
    // Nothing ever writes to the FIFO: a blocking open or read would hang this test.
    const error = await rejection(readImageFile(fifo, { maxImageBytes: 5 * MIB, cwd: dir, home: dir }))
    expect(error.kind).toBe("not_a_file")
    expect(error.message).toBe("~/pipe.png is not a regular file")
  })

  test("bounded read when stat lies", async () => {
    // procfs reports size 0 for files that have content; only the read cap catches them.
    const lying = "/proc/self/status"
    if (!existsSync(lying)) return
    const error = await rejection(readImageFile(lying, { maxImageBytes: 64, cwd: "/" }))
    expect(error.kind).toBe("too_large")
    expect(error.message).toBe("status is larger than 64 B, the limit: select a smaller region (/shot) or compress it")
  })

  test("stat larger than the limit is rejected before reading", async () => {
    const { dir, path } = await fixture("big.png", new Uint8Array([...PNG, ...new Uint8Array(100)]))
    const error = await rejection(readImageFile(path, { maxImageBytes: PNG.length, cwd: dir }))
    expect(error.kind).toBe("too_large")
    expect(error.message).toBe(`big.png is ${PNG.length + 100} B; the limit is ${PNG.length} B: select a smaller region (/shot) or compress it`)
  })

  test("quotes, ~ and file:// handled", async () => {
    const dir = await makeTempDir()
    await mkdir(join(dir, "my pics"))
    const path = join(dir, "my pics", "café shot.png")
    await writeFile(path, PNG)
    const opts = { maxImageBytes: 5 * MIB, cwd: join(dir, "my pics"), home: dir }
    const inputs = [
      `"${path}"`,
      `'${path}'`,
      `  ${path}  `,
      "~/my pics/café shot.png",
      "café shot.png",
      `file://${path.split("/").map(encodeURIComponent).join("/")}`,
      `file://localhost${path.replaceAll(" ", "%20")}`,
    ]
    for (const input of inputs) {
      const image = await readImageFile(input, opts)
      expect(image.filename).toBe("café shot.png")
    }
  })

  test("missing file message", async () => {
    const dir = await makeTempDir()
    const error = await rejection(readImageFile(join(dir, "nope.png"), { maxImageBytes: 5 * MIB, cwd: dir, home: dir }))
    expect(error.kind).toBe("not_found")
    expect(error.message).toBe("no such file: ~/nope.png")
  })

  test("directories, non-images, remote URIs and empty input are refused", async () => {
    const { dir } = await fixture("notes.png", new TextEncoder().encode("just text"))
    const opts = { maxImageBytes: 5 * MIB, cwd: dir, home: dir }
    expect((await rejection(readImageFile(dir, opts))).kind).toBe("not_a_file")
    const text = await rejection(readImageFile("notes.png", opts))
    expect(text.kind).toBe("unsupported")
    expect(text.message).toBe("notes.png: unsupported image type (allowed: PNG, JPEG, GIF, WebP)")
    expect((await rejection(readImageFile("file://server/share/a.png", opts))).kind).toBe("bad_path")
    expect((await rejection(readImageFile("  ''  ", opts))).kind).toBe("bad_path")
  })

  test("an animated WebP (/img, a drop, a copied file) names the problem instead of a decoder code", async () => {
    // PIL 11.3: 3 lossless frames; OpenTUI 0.8.1 cannot decode it ("unsupported-feature").
    const animated = new Uint8Array(
      Buffer.from(
        "UklGRrQAAABXRUJQVlA4WAoAAAACAAAAPwAALwAAQU5JTQYAAAAAAAAAAABBTk1GKAAAAAAAAAAAAD8AAC8AAGQAAAJWUDhMDwAAAC8/wAsABxD9j/4HIqL/AQBBTk1GKAAAAAAAAAAAAD8AAC8AAGQAAABWUDhMDwAAAC8/wAsAB9D/iP4HIqL/AQBBTk1GKAAAAAAAAAAAAD8AAC8AAGQAAABWUDhMDwAAAC8/wAsABxDR//4HIqL/AQA=",
        "base64",
      ),
    )
    const { dir, path } = await fixture("sticker.webp", animated)
    const error = await rejection(readImageFile(path, { maxImageBytes: 5 * MIB, cwd: dir, home: dir }))
    expect([error.kind, error.message]).toEqual(["unsupported", "sticker.webp: animated WebP is not supported; convert it to PNG or GIF"])
    let clipboard: unknown
    try {
      createPendingImage(animated, { source: "clipboard", filename: null, maxImageBytes: 5 * MIB })
    } catch (caught) {
      clipboard = caught
    }
    expect((clipboard as ImageInputError).message).toBe("clipboard image: animated WebP is not supported; convert it to PNG or GIF")
  })

  test("symbolic links are followed: the user named the file", async () => {
    const { dir, path } = await fixture("real.png")
    await symlink(path, join(dir, "link.png"))
    const image = await readImageFile("link.png", { maxImageBytes: 5 * MIB, cwd: dir })
    expect(image.filename).toBe("link.png")
  })

  test("the server's accepted types apply", async () => {
    const { dir, path } = await fixture("a.gif", GIF)
    const error = await rejection(readImageFile(path, { maxImageBytes: 5 * MIB, cwd: dir, imageMimeTypes: ["image/png"] }))
    expect(error.message).toBe("a.gif: this daemon does not accept GIF images (allowed: PNG)")
  })
})

describe("resolveImagePath and file names", () => {
  test("relative, home and URI forms", () => {
    const opts = { cwd: "/work", home: "/home/sun" }
    expect(resolveImagePath("a.png", opts)).toBe("/work/a.png")
    expect(resolveImagePath("~/a.png", opts)).toBe("/home/sun/a.png")
    expect(resolveImagePath("'../b.png'", opts)).toBe("/b.png")
    expect(resolveImagePath("file:///tmp/a%20b.png", opts)).toBe("/tmp/a b.png")
  })

  test("names are sanitized and at most 128 characters", () => {
    expect(imageFileName("/tmp/a\u001b[31mred‮.png")).toBe("ared.png")
    expect(imageFileName(`/tmp/${"x".repeat(200)}.png`)).toHaveLength(MAX_FILENAME_CHARS)
    expect(imageFileName("/")).toBeNull()
    const image = createPendingImage(PNG, { source: "clipboard", filename: null, maxImageBytes: 5 * MIB })
    expect(image.filename).toBeNull()
    expect(image.source).toBe("clipboard")
  })

  test("validation errors name the source when there is no file name", () => {
    let error: unknown
    try {
      createPendingImage(new Uint8Array([1, 2, 3]), { source: "clipboard", filename: null, maxImageBytes: 5 * MIB })
    } catch (caught) {
      error = caught
    }
    expect(error).toBeInstanceOf(ImageInputError)
    expect((error as ImageInputError).message).toBe("clipboard image: unsupported image type (allowed: PNG, JPEG, GIF, WebP)")
  })
})

describe("storage disclosure (5.6, DEC-3)", () => {
  test("notice and caption texts", () => {
    expect(imageStorageNotice("derpy")).toBe(
      "images you send are stored in derpy's workspace (no expiry) and are readable from every channel",
    )
    expect(imageStorageCaption("derpy")).toBe("stored in derpy's workspace")
    expect(imageStorageCaption(null)).toBe("stored in the daemon's workspace")
  })

  test("the first image from each source is acknowledged once per profile", async () => {
    let state: ProfileState = {}
    const stateFile = {
      read: async () => state,
      update: async (change: (current: ProfileState) => ProfileState) => {
        state = change(state)
        return state
      },
    }
    const at = Date.UTC(2026, 10, 3, 4, 5, 6)
    expect(await acknowledgeImageSource(stateFile, "clipboard", at)).toBe(true)
    expect(await acknowledgeImageSource(stateFile, "clipboard", at + 1000)).toBe(false)
    expect(await acknowledgeImageSource(stateFile, "shot", at)).toBe(true)
    expect(state.imageAck).toEqual({ clipboard: "2026-11-03T04:05:06.000Z", shot: "2026-11-03T04:05:06.000Z" })
  })

  test("an unwritable state file still shows the notice", async () => {
    const broken = {
      read: async (): Promise<ProfileState> => {
        throw new Error("EACCES")
      },
      update: async (): Promise<ProfileState> => ({}),
    }
    expect(await acknowledgeImageSource(broken, "file", 0)).toBe(true)
  })
})
