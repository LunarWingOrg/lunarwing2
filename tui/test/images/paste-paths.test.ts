/**
 * Drag-and-drop detection (design 6.1, 11.3.2): kitty newline-joined paths; percent-decoded file:// URIs;
 * backslash-escaped and single-quoted paths; prose is not a drop; non-image extensions are not a drop; more than
 * maxImages paths is not a drop.
 */
import { describe, expect, test } from "bun:test"
import {
  DROP_MAX_BYTES,
  fileUriToPath,
  hasImageExtension,
  looksLikeDrop,
  parseDroppedPaths,
  parseUriList,
  splitShellWords,
} from "../../src/images/paste-paths.ts"

const drop = (text: string, maxPaths = 5): string[] | null => parseDroppedPaths(text, { maxPaths })

describe("parseDroppedPaths", () => {
  test("kitty newline-joined paths, spaces included", () => {
    expect(drop("/home/sun/Pictures/a.png\n/home/sun/Pictures/my screenshot.JPG\n")).toEqual([
      "/home/sun/Pictures/a.png",
      "/home/sun/Pictures/my screenshot.JPG",
    ])
    expect(drop("/tmp/one.webp")).toEqual(["/tmp/one.webp"])
    expect(drop("\r\n/tmp/a.gif\r\n\r\n/tmp/b.jpeg\r\n")).toEqual(["/tmp/a.gif", "/tmp/b.jpeg"])
  })

  test("percent-decoded file:// URIs", () => {
    expect(drop("file:///home/sun/My%20Pictures/caf%C3%A9.png")).toEqual(["/home/sun/My Pictures/café.png"])
    expect(drop("file://localhost/tmp/a%2Bb.png\nfile:///tmp/c.png")).toEqual(["/tmp/a+b.png", "/tmp/c.png"])
    expect(drop("file://otherhost/tmp/a.png")).toBeNull()
    expect(drop("file:///tmp/a%00.png")).toBeNull()
    expect(drop("file:///tmp/a%ZZ.png")).toBeNull()
  })

  test("backslash-escaped paths (WezTerm) and single-quoted paths (VTE)", () => {
    expect(drop("/home/sun/my\\ shot.png")).toEqual(["/home/sun/my shot.png"])
    expect(drop("'/home/sun/my shot.png'")).toEqual(["/home/sun/my shot.png"])
    expect(drop("'/tmp/a b.png' '/tmp/c.png' ")).toEqual(["/tmp/a b.png", "/tmp/c.png"])
    expect(drop("'/tmp/it'\\''s.png'")).toEqual(["/tmp/it's.png"])
    expect(drop("/tmp/x\\ y.png /tmp/z.webp")).toEqual(["/tmp/x y.png", "/tmp/z.webp"])
    expect(drop("/tmp/a.png /tmp/b.png")).toEqual(["/tmp/a.png", "/tmp/b.png"])
  })

  test("kitty's unquoted paths keep quotes and backslashes that are part of the file name", () => {
    // kitty 0.44 Boss.on_drop joins the paths with "\n" and quotes nothing while an app owns the window.
    expect(drop("/home/sun/Pictures/Chris's shot.png")).toEqual(["/home/sun/Pictures/Chris's shot.png"])
    expect(drop("/home/sun/Pictures/it's here/a.png")).toEqual(["/home/sun/Pictures/it's here/a.png"])
    expect(drop('/home/sun/a"b.png')).toEqual(['/home/sun/a"b.png'])
    expect(drop("/home/sun/it's Chris's.png")).toEqual(["/home/sun/it's Chris's.png"])
    expect(drop("/home/sun/a\\b.png")).toEqual(["/home/sun/a\\b.png"])
    expect(drop("/home/sun/Chris's shot.png\n/home/sun/b.png")).toEqual(["/home/sun/Chris's shot.png", "/home/sun/b.png"])
    // WezTerm's escapes are still shell quoting
    expect(drop("/home/sun/it\\'s.png")).toEqual(["/home/sun/it's.png"])
  })

  test("looksLikeDrop: what a refused paste of image paths looks like", () => {
    expect(looksLikeDrop("/tmp/1.png\n/tmp/2.png\n/tmp/3.png")).toBe(true)
    expect(looksLikeDrop("'/tmp/a b.png' '/tmp/c.png'")).toBe(true)
    expect(looksLikeDrop("file:///tmp/a.png")).toBe(true)
    expect(looksLikeDrop("look at /tmp/a.png please")).toBe(false)
    expect(looksLikeDrop("/tmp/notes.txt")).toBe(false)
    expect(looksLikeDrop("")).toBe(false)
  })

  test("prose is not a drop", () => {
    expect(drop("look at /tmp/a.png please")).toBeNull()
    expect(drop("what is in this screenshot?")).toBeNull()
    expect(drop("/tmp/a.png\nand also this text")).toBeNull()
    expect(drop("it's '/tmp/a.png")).toBeNull()
    expect(drop("~/Pictures/a.png")).toBeNull()
    expect(drop("Pictures/a.png")).toBeNull()
    expect(drop("")).toBeNull()
    expect(drop("\n  \n")).toBeNull()
    expect(drop("/tmp/a\u001b[31m.png")).toBeNull()
  })

  test("non-image extensions are not a drop", () => {
    expect(drop("/tmp/notes.txt")).toBeNull()
    expect(drop("/tmp/a.png\n/tmp/b.svg")).toBeNull()
    expect(drop("/tmp/image.bmp")).toBeNull()
    expect(drop("/tmp/archive.png.tar")).toBeNull()
    expect(drop("https://example.invalid/a.png")).toBeNull()
  })

  test("more than maxImages paths is not a drop", () => {
    const three = "/tmp/1.png\n/tmp/2.png\n/tmp/3.png"
    expect(drop(three, 3)).toHaveLength(3)
    expect(drop(three, 2)).toBeNull()
    expect(drop("'/tmp/1.png' '/tmp/2.png' '/tmp/3.png'", 2)).toBeNull()
    expect(drop("/tmp/1.png", 0)).toBeNull()
  })

  test("pastes over 64 KiB are never drops", () => {
    const long = `/tmp/${"a".repeat(DROP_MAX_BYTES)}.png`
    expect(drop(long)).toBeNull()
    expect(drop(`/tmp/${"é".repeat(DROP_MAX_BYTES / 2)}.png`)).toBeNull()
  })
})

describe("helpers", () => {
  test("fileUriToPath accepts only local file URIs", () => {
    expect(fileUriToPath("file:///etc/hosts")).toBe("/etc/hosts")
    expect(fileUriToPath("FILE://LOCALHOST/tmp/x")).toBe("/tmp/x")
    expect(fileUriToPath("file:/tmp/x")).toBeNull()
    expect(fileUriToPath("file://host/tmp/x")).toBeNull()
    expect(fileUriToPath("file:///tmp/x?query")).toBeNull()
    expect(fileUriToPath("file:///tmp/x#frag")).toBeNull()
    expect(fileUriToPath("http:///tmp/x")).toBeNull()
  })

  test("parseUriList skips comments and non-file URIs", () => {
    expect(parseUriList("# copied by a file manager\r\nfile:///tmp/a.png\r\nhttps://example.invalid/\r\nfile:///tmp/b%20c.txt\r\n")).toEqual([
      "/tmp/a.png",
      "/tmp/b c.txt",
    ])
    expect(parseUriList("")).toEqual([])
  })

  test("splitShellWords handles quotes and escapes, rejects unbalanced input", () => {
    expect(splitShellWords(`a 'b c' "d \\"e\\"" f\\ g`)).toEqual(["a", "b c", 'd "e"', "f g"])
    expect(splitShellWords("'open")).toBeNull()
    expect(splitShellWords("trailing\\")).toBeNull()
  })

  test("hasImageExtension is case-insensitive and anchored at the end", () => {
    expect(["a.png", "a.JPG", "a.jpeg", "a.Gif", "a.webp"].every(hasImageExtension)).toBe(true)
    expect(["a.png.txt", "a.svg", "apng", "a.bmp"].some(hasImageExtension)).toBe(false)
  })
})
