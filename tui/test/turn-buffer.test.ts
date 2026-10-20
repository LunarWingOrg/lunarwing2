import { describe, expect, test } from "bun:test"
import { TurnBuffer } from "../src/turn-buffer.ts"

describe("TurnBuffer", () => {
  test("accumulates stream chunks", () => {
    const turn = new TurnBuffer()
    turn.begin()
    expect(turn.appendChunk("Hello ")).toBe(true)
    expect(turn.appendChunk("world")).toBe(true)
    expect(turn.text).toBe("Hello world")
    expect(turn.isStreaming).toBe(true)
  })

  test("REGRESSION: empty done frame never wipes accumulated output", () => {
    const turn = new TurnBuffer()
    turn.begin()
    turn.appendChunk("partial answer that must survive")
    turn.complete("") // empty final payload — close, not detonate
    expect(turn.text).toBe("partial answer that must survive")
    expect(turn.isDone).toBe(true)
  })

  test("non-empty done frame becomes canonical", () => {
    const turn = new TurnBuffer()
    turn.begin()
    turn.appendChunk("streamed text")
    turn.complete("final authoritative text")
    expect(turn.text).toBe("final authoritative text")
  })

  test("done frame with no chunks yields empty but sealed turn", () => {
    const turn = new TurnBuffer()
    turn.begin()
    turn.complete("answer")
    expect(turn.text).toBe("answer")
  })

  test("late chunks after done are rejected, sealed text unchanged", () => {
    const turn = new TurnBuffer()
    turn.begin()
    turn.appendChunk("real content")
    turn.complete("")
    expect(turn.appendChunk("zombie chunk")).toBe(false)
    expect(turn.text).toBe("real content")
  })

  test("duplicate done frame changes nothing", () => {
    const turn = new TurnBuffer()
    turn.begin()
    turn.appendChunk("abc")
    turn.complete("canonical")
    turn.complete("")
    expect(turn.text).toBe("canonical")
  })

  test("chunks before begin are rejected", () => {
    const turn = new TurnBuffer()
    expect(turn.appendChunk("orphan")).toBe(false)
    expect(turn.isIdle).toBe(true)
  })

  test("reset returns to idle for the next turn", () => {
    const turn = new TurnBuffer()
    turn.begin()
    turn.appendChunk("x")
    turn.complete("")
    turn.reset()
    expect(turn.isIdle).toBe(true)
    expect(turn.text).toBe("")
  })
})
