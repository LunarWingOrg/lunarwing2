/**
 * state/turn-text.ts: the nine vibetui4 TurnBuffer cases (v4:tui/test/turn-buffer.test.ts) re-targeted at the
 * immutable per-turn text (design 3.2, 11.3.2), plus the 1 MiB delta cap (3.12.5).
 */
import { describe, expect, test } from "bun:test"
import {
  FINAL_SEPARATOR,
  appendDelta,
  applyFinal,
  createTurnText,
  cutToUtf8Bytes,
  isSealedPhase,
  sealTurnText,
  turnTextOf,
  utf8Length,
  visibleText,
  type TurnText,
} from "../../src/state/turn-text.ts"

function append(text: TurnText | undefined, ...chunks: string[]): TurnText | undefined {
  let current = text
  for (const chunk of chunks) current = appendDelta(current, chunk).text
  return current
}

/** Applies a final and seals, the way a final closes a turn on a daemon without lifecycle events. */
function finalize(text: TurnText, final: string): TurnText {
  return sealTurnText(applyFinal(text, final).text ?? text)
}

describe("turn-text: the TurnBuffer invariants, per turn and immutable", () => {
  test("accumulate", () => {
    const start = createTurnText()
    const first = appendDelta(start, "Hello ")
    const second = appendDelta(first.text, "world")
    expect(first.status).toBe("appended")
    expect(second.status).toBe("appended")
    expect(second.text?.deltas).toBe("Hello world")
    expect(second.text?.sealed).toBe(false)
    expect(visibleText(second.text ?? start)).toBe("Hello world")
    // immutable: the earlier values are untouched
    expect(start.deltas).toBe("")
    expect(first.text?.deltas).toBe("Hello ")
  })

  test("empty final keeps deltas", () => {
    const streamed = append(createTurnText(), "partial answer that must survive") ?? createTurnText()
    const result = applyFinal(streamed, "")
    expect(result.changed).toBe(false)
    const sealed = sealTurnText(result.text ?? streamed)
    expect(sealed.sealed).toBe(true)
    expect(sealed.finalText).toBeNull()
    expect(visibleText(sealed)).toBe("partial answer that must survive")
  })

  test("non-empty final is canonical", () => {
    const streamed = append(createTurnText(), "streamed text") ?? createTurnText()
    const sealed = finalize(streamed, "final authoritative text")
    expect(visibleText(sealed)).toBe("final authoritative text")
    expect(sealed.deltas).toBe("streamed text") // kept, only hidden behind the canonical text
  })

  test("final without deltas", () => {
    const sealed = finalize(createTurnText(), "answer")
    expect(visibleText(sealed)).toBe("answer")
    expect(sealed.deltas).toBe("")
  })

  test("late deltas rejected", () => {
    const sealed = finalize(append(createTurnText(), "real content") ?? createTurnText(), "")
    const late = appendDelta(sealed, "zombie chunk")
    expect(late.status).toBe("rejected")
    expect(late.text).toBe(sealed)
    expect(visibleText(sealed)).toBe("real content")
  })

  test("duplicate final changes nothing", () => {
    const sealed = finalize(append(createTurnText(), "abc") ?? createTurnText(), "canonical")
    const emptyAgain = applyFinal(sealed, "")
    const sameAgain = applyFinal(sealed, "canonical")
    expect(emptyAgain.changed).toBe(false)
    expect(sameAgain.changed).toBe(false)
    expect(emptyAgain.text).toBe(sealed)
    expect(sameAgain.text).toBe(sealed)
    expect(visibleText(sealed)).toBe("canonical")
  })

  test("deltas before creation rejected", () => {
    const orphan = appendDelta(undefined, "orphan")
    expect(orphan.status).toBe("rejected")
    expect(orphan.text).toBeUndefined()
    expect(applyFinal(undefined, "orphan final")).toEqual({ text: undefined, changed: false, late: false })
  })

  test("sealed record never reopened", () => {
    const sealed = finalize(append(createTurnText(), "done") ?? createTurnText(), "")
    expect(sealTurnText(sealed)).toBe(sealed)
    // a different final after the seal is kept as late text, but the text stays sealed
    const late = applyFinal(sealed, "a late reply")
    expect(late.changed).toBe(true)
    expect(late.late).toBe(true)
    expect(late.text?.sealed).toBe(true)
    expect(late.text?.finalText).toBe("a late reply")
    expect(appendDelta(late.text, "more").status).toBe("rejected")
    // the phase decides sealing for records
    for (const phase of ["done", "interrupted", "failed", "lost", "unanswered"] as const) expect(isSealedPhase(phase)).toBe(true)
    for (const phase of ["pending", "sent", "queued", "running", "awaiting_approval", "awaiting_input", "background_running"] as const) {
      expect(isSealedPhase(phase)).toBe(false)
    }
    expect(turnTextOf({ deltas: "x", finalText: null, phase: "done" }).sealed).toBe(true)
  })

  test("new turn starts empty", () => {
    finalize(append(createTurnText(), "first turn") ?? createTurnText(), "")
    const next = createTurnText()
    expect(next).toEqual({ deltas: "", finalText: null, sealed: false })
    const second = appendDelta(next, "second")
    expect(second.status).toBe("appended")
    expect(visibleText(second.text ?? next)).toBe("second")
  })
})

describe("turn-text: several finals and the delta cap", () => {
  test("finals of one open turn are joined with a blank line", () => {
    const one = applyFinal(createTurnText(), "part one").text ?? createTurnText()
    const two = applyFinal(one, "part two")
    expect(two.late).toBe(false)
    expect(two.text?.finalText).toBe(`part one${FINAL_SEPARATOR}part two`)
  })

  test("deltas stop at the byte cap and report the cut on a code point boundary", () => {
    const max = 16
    const start = appendDelta(createTurnText(), "0123456789", max)
    expect(start.status).toBe("appended")
    // "é" is 2 bytes and "日" 3: 10 + 2 + 3 = 15, the next "日" no longer fits
    const cut = appendDelta(start.text, "é日日", max)
    expect(cut.status).toBe("cut")
    expect(cut.text?.deltas).toBe("0123456789é日")
    expect(utf8Length(cut.text?.deltas ?? "")).toBeLessThanOrEqual(max)
    // one byte of room is left: "x" fits exactly, then nothing more does
    const exact = appendDelta(cut.text, "x", max)
    expect(exact.status).toBe("appended")
    expect(utf8Length(exact.text?.deltas ?? "")).toBe(max)
    const full = appendDelta(exact.text, "y", max)
    expect(full.status).toBe("cut")
    expect(full.text).toBe(exact.text)
  })

  test("the default cap is 1 MiB of UTF-8", () => {
    const big = "x".repeat(1024 * 1024 - 2)
    const first = appendDelta(createTurnText(), big)
    expect(first.status).toBe("appended")
    const over = appendDelta(first.text, "日") // 3 bytes, only 2 left
    expect(over.status).toBe("cut")
    expect(utf8Length(over.text?.deltas ?? "")).toBe(1024 * 1024 - 2)
  })

  test("cutToUtf8Bytes never splits a surrogate pair", () => {
    expect(cutToUtf8Bytes("a😀b", 4)).toBe("a")
    expect(cutToUtf8Bytes("a😀b", 5)).toBe("a😀")
    expect(cutToUtf8Bytes("abc", 0)).toBe("")
  })
})
