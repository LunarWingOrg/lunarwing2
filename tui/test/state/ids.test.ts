import { describe, expect, test } from "bun:test"
import {
  CLIENT_ID_MAX_LEN,
  asTurnId,
  createTurnIdSequence,
  isValidClientId,
  newReplThreadToken,
  newTranscribeRequestId,
  newTurnId,
} from "../../src/state/ids.ts"

const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

describe("isValidClientId mirrors ^[A-Za-z0-9_-]{1,64}$ (4.1)", () => {
  test("accepts the allowed charset up to 64 characters", () => {
    for (const id of ["a", "Z", "0", "_", "-", "turn-1", "A_b-9", "x".repeat(CLIENT_ID_MAX_LEN)]) {
      expect(isValidClientId(id)).toBe(true)
    }
  })

  test("rejects empty, too long, and any character outside the charset", () => {
    const bad = ["", "x".repeat(65), "a b", "a.b", "a/b", "a:b", "a+b", "\u00e9", "\u65e5\u672c", "a\u200bb", "a\u0000b"]
    for (const id of bad) expect(isValidClientId(id)).toBe(false)
  })

  test("a trailing newline is not accepted (the pattern is anchored to the end of input)", () => {
    expect(isValidClientId("abc\n")).toBe(false)
    expect(isValidClientId("\nabc")).toBe(false)
  })

  test("rejects non-strings", () => {
    for (const value of [undefined, null, 42, {}, ["a"], Symbol("a")]) expect(isValidClientId(value)).toBe(false)
  })
})

describe("newTurnId", () => {
  test("is a UUIDv4 that satisfies the id rule", () => {
    const id = newTurnId()
    expect(id).toMatch(UUID_V4)
    expect(isValidClientId(id)).toBe(true)
  })

  test("1000 ids are distinct", () => {
    const ids = new Set(Array.from({ length: 1000 }, () => newTurnId()))
    expect(ids.size).toBe(1000)
  })
})

describe("asTurnId", () => {
  test("brands a valid echoed id", () => {
    const id = asTurnId("0193f2a1-7c3e-4d4a-b1e2-6f0c9a8b7d6e")
    expect(id === null ? null : String(id)).toBe("0193f2a1-7c3e-4d4a-b1e2-6f0c9a8b7d6e")
  })

  test("returns null for ids that break the rule or are not strings", () => {
    expect(asTurnId("not valid!")).toBeNull()
    expect(asTurnId("")).toBeNull()
    expect(asTurnId(17)).toBeNull()
    expect(asTurnId(undefined)).toBeNull()
  })
})

describe("newTranscribeRequestId", () => {
  test("is t- plus a UUIDv4, 38 characters, valid", () => {
    const id = newTranscribeRequestId()
    expect(id.startsWith("t-")).toBe(true)
    expect(id.slice(2)).toMatch(UUID_V4)
    expect(id.length).toBe(38)
    expect(isValidClientId(id)).toBe(true)
  })
})

describe("newReplThreadToken (3.9, F3)", () => {
  test("formats tui-<profile>-<16 hex> from the injected random bytes", () => {
    const fill = (bytes: Uint8Array) => {
      bytes.set([0x00, 0x01, 0x0a, 0x10, 0x7f, 0x80, 0xab, 0xff])
      return bytes
    }
    expect(newReplThreadToken("derpy", fill)).toBe("tui-derpy-00010a107f80abff")
  })

  test("defaults to crypto.getRandomValues and stays a valid client id for the longest profile name", () => {
    const longest = "p".repeat(32)
    const token = newReplThreadToken(longest)
    expect(token).toMatch(new RegExp(`^tui-${longest}-[0-9a-f]{16}$`))
    expect(isValidClientId(token)).toBe(true)
    expect(newReplThreadToken(longest)).not.toBe(token)
  })

  test("refuses a profile that would make the token invalid", () => {
    expect(() => newReplThreadToken("bad.profile")).toThrow(RangeError)
    expect(() => newReplThreadToken("p".repeat(60))).toThrow(RangeError)
  })

  test("refuses a random fill that returns the wrong number of bytes", () => {
    expect(() => newReplThreadToken("derpy", () => new Uint8Array(4))).toThrow(RangeError)
  })
})

describe("createTurnIdSequence", () => {
  test("yields deterministic, valid ids", () => {
    const next = createTurnIdSequence()
    expect([next(), next(), next()].map(String)).toEqual(["turn-1", "turn-2", "turn-3"])
    const other = createTurnIdSequence("ctl")
    expect(String(other())).toBe("ctl-1")
  })

  test("sequences are independent", () => {
    const a = createTurnIdSequence("a")
    const b = createTurnIdSequence("a")
    a()
    a()
    expect(String(b())).toBe("a-1")
  })

  test("refuses a prefix that cannot form a valid id", () => {
    expect(() => createTurnIdSequence("has space")).toThrow(RangeError)
    expect(() => createTurnIdSequence("x".repeat(63))).toThrow(RangeError)
  })
})
