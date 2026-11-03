import { describe, expect, test } from "bun:test"
import { isTransportError } from "../../src/transport/errors.ts"
import { assertSendableContent, isForbiddenContent, normalizeCommandText } from "../../src/transport/guards.ts"

describe("isForbiddenContent (3.10.2)", () => {
  test("catches the quit family after SubmissionParser's trim and lowercase", () => {
    for (const content of [" /EXIT ", "/Quit", "/shutdown", "/quit", "/exit", "\t/SHUTDOWN\n", "  /qUiT  "]) {
      expect(isForbiddenContent(content)).toBe(true)
    }
  })

  test("does not catch other text", () => {
    for (const content of ["/quitting", "quit", "exit", "/quit now", "/ quit", "//quit", "please /quit", "", "/shutdowns"]) {
      expect(isForbiddenContent(content)).toBe(false)
    }
  })

  test("trims what Rust's str::trim trims, including U+0085 (NEL), which String.prototype.trim keeps", () => {
    expect("\u0085/quit".trim()).not.toBe("/quit")
    expect(isForbiddenContent("\u0085/quit\u0085")).toBe(true)
    expect(isForbiddenContent("\u3000/exit ")).toBe(true)
    expect(isForbiddenContent("\u00A0/shutdown ")).toBe(true)
  })

  test("refuses a BOM-prefixed command too (the safe direction)", () => {
    expect(isForbiddenContent("\uFEFF/quit")).toBe(true)
  })

  test("long whitespace runs stay linear and are not forbidden", () => {
    const content = `/quit${" ".repeat(200_000)}x`
    expect(isForbiddenContent(content)).toBe(false)
    expect(isForbiddenContent(`${" ".repeat(200_000)}/quit${" ".repeat(200_000)}`)).toBe(true)
  })

  test("normalizeCommandText mirrors the daemon's normalization", () => {
    expect(normalizeCommandText("  /Interrupt \n")).toBe("/interrupt")
  })

  test("assertSendableContent throws bad_request and lets other text through", () => {
    expect(() => assertSendableContent("hello")).not.toThrow()
    let caught: unknown
    try {
      assertSendableContent(" /EXIT ")
    } catch (error) {
      caught = error
    }
    expect(isTransportError(caught)).toBe(true)
    if (isTransportError(caught)) {
      expect(caught.code).toBe("bad_request")
      expect(caught.fatal).toBe(false)
    }
  })
})
