/**
 * Terminal-safe text (design 3.12.5, 11.3.2): escape sequences, C0/C1 controls and bidi controls never reach
 * the state; TAB survives; line feeds survive only in multiline mode.
 */
import { describe, expect, test } from "bun:test"
import { sanitizeLine, sanitizeText, sanitizeVisible } from "../src/sanitize.ts"

const ESC = "\u001b"
const BEL = "\u0007"
const RLO = "\u202e"

describe("sanitizeText (3.12.5)", () => {
  test("ANSI CSI sequences are removed (colours, cursor moves, erase, private modes)", () => {
    expect(sanitizeText(`${ESC}[31mred${ESC}[0m plain`)).toBe("red plain")
    expect(sanitizeText(`a${ESC}[2J${ESC}[H${ESC}[10;20Hb`)).toBe("ab")
    expect(sanitizeText(`x${ESC}[?1049h${ESC}[?25ly`)).toBe("xy")
    expect(sanitizeText("8-bit \u009b31mcsi")).toBe("8-bit csi")
  })

  test("OSC sequences are removed with their payload (titles, hyperlinks, clipboard writes)", () => {
    expect(sanitizeText(`${ESC}]0;evil title${BEL}after`)).toBe("after")
    expect(sanitizeText(`${ESC}]8;;https://example.invalid${ESC}\\link${ESC}]8;;${ESC}\\`)).toBe("link")
    expect(sanitizeText(`${ESC}]52;c;aGVsbG8=${BEL}text`)).toBe("text")
    expect(sanitizeText(`dcs ${ESC}Pq#0;2;0;0;0${ESC}\\done`)).toBe("dcs done")
  })

  test("C0 controls other than TAB and LF are removed, CR included", () => {
    const c0 = Array.from({ length: 0x20 }, (_, i) => String.fromCharCode(i)).join("")
    expect(sanitizeText(`[${c0}]`, { multiline: true })).toBe("[\t\n]")
    expect(sanitizeText("progress 10%\rprogress 99%")).toBe("progress 10%progress 99%")
    expect(sanitizeText("bell\u0007 backspace\u0008 nul\u0000")).toBe("bell backspace nul")
  })

  test("DEL and every C1 control are removed", () => {
    // 8-bit CSI (9B) and the string introducers DCS, SOS, OSC, PM, APC (90, 98, 9D-9F) start a sequence that
    // swallows what follows, as a terminal would; every other C1 control is just dropped.
    const introducers = new Set([0x90, 0x98, 0x9b, 0x9d, 0x9e, 0x9f])
    for (let code = 0x7f; code <= 0x9f; code += 1) {
      const out = sanitizeText(`a${String.fromCharCode(code)}b`)
      const expected = introducers.has(code) && code !== 0x9b ? "a […escape sequence removed]" : introducers.has(code) ? "a" : "ab"
      expect(out).toBe(expected)
    }
    expect(sanitizeText("next\u0085line")).toBe("nextline")
    expect(sanitizeText("osc \u009d0;title\u009cafter")).toBe("osc after")
  })

  test("an unterminated escape string leaves a visible removal marker", () => {
    const input = `before ${ESC}]0;title never-terminated after`
    const output = sanitizeText(input)
    expect(output).toBe("before […escape sequence removed]")
    expect(output).toContain("before")
    expect(output).toContain("escape sequence removed")
    expect(sanitizeText(`${ESC}]0;complete${BEL}after`)).toBe("after")
  })

  test("OSC and DCS terminator handling matches OpenTUI", () => {
    expect(sanitizeText(`${ESC}]0;title${ESC}\\after`)).toBe("after")
    expect(sanitizeText(`${ESC}]0;title\u009cafter`)).toBe("after")
    expect(sanitizeText(`before ${ESC}Ppayload${BEL}after`)).toBe("before after")
  })

  test("bidi embedding, override and isolate controls are removed", () => {
    const bidi = [0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2066, 0x2067, 0x2068, 0x2069].map((c) => String.fromCharCode(c)).join("")
    expect(sanitizeText(`ab${bidi}cd`)).toBe("abcd")
    // The "Trojan Source" shape: an override that would display "exe.txt" as "txt.exe".
    expect(sanitizeText(`report${RLO}txt.exe`)).toBe("reporttxt.exe")
  })

  test("printable text, TAB and non-ASCII letters are kept", () => {
    const text = "caf\u00e9 \u65e5\u672c\u8a9e \u{1f600}\tok"
    expect(sanitizeText(text)).toBe(text)
    expect(sanitizeText("")).toBe("")
  })

  test("newlines are kept only in multiline mode; CRLF becomes LF", () => {
    expect(sanitizeText("one\r\ntwo\nthree", { multiline: true })).toBe("one\ntwo\nthree")
    expect(sanitizeText("one\r\ntwo\n\nthree")).toBe("one two three")
    expect(sanitizeText("one\ntwo", { multiline: false })).toBe("one two")
    expect(sanitizeLine("a\nb")).toBe("a b")
  })

  test("no escape character survives, even from sequences split across chunks or nested in controls", () => {
    for (const input of [`${ESC}[3`, `1m${ESC}`, `${ESC}\u0000[31m`, `${ESC}${ESC}[[31m`, "\u009b", `${ESC}${RLO}[31m`]) {
      const out = sanitizeText(input, { multiline: true })
      expect(out.includes(ESC)).toBe(false)
      expect(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/.test(out)).toBe(false)
    }
  })
})

describe("sanitizeVisible (approval text, SEC-04)", () => {
  test("an OSC payload stays visible instead of disappearing with the sequence", () => {
    const hidden = `ls${ESC}]0; rm -rf ~${BEL}`
    expect(sanitizeText(hidden)).toBe("ls")
    expect(sanitizeVisible(hidden)).toBe("ls\\u{1b}]0; rm -rf ~\\u{7}")
  })

  test("controls and bidi characters become escapes; TAB is kept and LF only in multiline mode", () => {
    expect(sanitizeVisible(`a${RLO}b\tc\u0085d`)).toBe("a\\u{202e}b\tc\\u{85}d")
    expect(sanitizeVisible("one\r\ntwo", { multiline: true })).toBe("one\\u{d}\ntwo")
    expect(sanitizeVisible("one\ntwo")).toBe("one\\u{a}two")
    const out = sanitizeVisible(`x${ESC}[31my\u009bz`, { multiline: true })
    expect(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/.test(out)).toBe(false)
  })
})
