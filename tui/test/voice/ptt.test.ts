/**
 * The push-to-talk state machine (design 7.3, cases of 7.5). Pure: a ManualClock supplies the event times and,
 * like the controller, a 50 ms tick interval while the test advances time.
 */
import { describe, expect, test } from "bun:test"
import { ManualClock } from "@opentui/core/testing"
import { createPttState, DEFAULT_PTT_CONFIG, pttReduce, type PttConfig, type PttEffect, type PttEvent, type PttState } from "../../src/voice/ptt.ts"

/** The host setting (Hyprland repeat_delay 300). */
const HOST: PttConfig = { ...DEFAULT_PTT_CONFIG, repeatDelayMs: 300 }

class Driver {
  readonly clock = new ManualClock()
  state: PttState = createPttState()
  readonly effects: PttEffect[] = []

  constructor(
    readonly cfg: PttConfig = DEFAULT_PTT_CONFIG,
    ticks = true,
  ) {
    if (ticks) this.clock.setInterval(() => this.send({ type: "tick", at: this.clock.now() }), 50)
  }

  send(event: PttEvent): void {
    const result = pttReduce(this.state, event, this.cfg)
    this.state = result.state
    this.effects.push(...result.effects)
  }

  at(ms: number): this {
    this.clock.setTime(ms)
    return this
  }

  press(repeated = false): this {
    this.send({ type: "key_press", repeated, at: this.clock.now() })
    return this
  }

  release(): this {
    this.send({ type: "key_release", at: this.clock.now() })
    return this
  }

  mouseDown(): this {
    this.send({ type: "mouse_down", at: this.clock.now() })
    return this
  }

  mouseUp(): this {
    this.send({ type: "mouse_up", at: this.clock.now() })
    return this
  }

  started(): this {
    this.send({ type: "capture_started", at: this.clock.now() })
    return this
  }

  finished(): this {
    this.send({ type: "finished", at: this.clock.now() })
    return this
  }

  other(type: "escape" | "blur" | "silence" | "no_signal_timeout" | "capture_failed" | "tick"): this {
    this.send({ type, at: this.clock.now() })
    return this
  }

  /** Autorepeat until `untilMs`: a press every 20 ms; tmux sends them as plain presses, KKP flags them. */
  repeatUntil(untilMs: number, flagged = false): this {
    while (this.clock.now() + 20 <= untilMs) {
      this.clock.advance(20)
      this.press(flagged)
    }
    return this
  }

  /** Effects as compact strings: "start_capture:key", "finish_capture:toggle", "notice:dictation cancelled". */
  log(): string[] {
    return this.effects.map((e) => {
      if (e.type === "start_capture") return `start_capture:${e.trigger}`
      if (e.type === "notice") return `notice:${e.text}`
      return `${e.type}:${e.reason}`
    })
  }

  captureEffects(): string[] {
    return this.log().filter((entry) => !entry.startsWith("notice:"))
  }
}

describe("toggle (tap)", () => {
  test("tap F9 starts, second tap stops (toggle)", () => {
    const d = new Driver().press()
    expect(d.state.phase).toBe("arming")
    d.at(40).started()
    d.at(1_000)
    expect(d.state.mode).toBe("toggle")
    d.at(2_000).press()
    expect(d.captureEffects()).toEqual(["start_capture:key", "finish_capture:toggle"])
    expect(d.state.phase).toBe("finishing")
    d.at(2_100).finished()
    expect(d.state.phase).toBe("idle")
    expect(d.state.mode).toBeNull()
  })

  test("a 2 s pause does not stop a toggle recording by default", () => {
    const d = new Driver().press().at(30).started().at(1_000)
    expect(d.state.mode).toBe("toggle")
    for (let t = 1_020; t <= 3_000; t += 20) d.at(t)
    d.other("silence")
    d.at(3_500)
    expect(d.state.phase).toBe("recording")
    expect(d.captureEffects()).toEqual(["start_capture:key"])

    const auto = new Driver({ ...DEFAULT_PTT_CONFIG, autoStopOnSilence: true }).press().at(30).started().at(1_000)
    auto.at(4_500).other("silence")
    expect(auto.captureEffects()).toEqual(["start_capture:key", "finish_capture:silence"])
  })

  test("no_signal_timeout cancels a silent toggle recording", () => {
    const d = new Driver().press().at(30).started().at(1_000)
    d.at(8_030).other("no_signal_timeout")
    expect(d.captureEffects()).toEqual(["start_capture:key", "cancel_capture:no_signal"])
    expect(d.state.phase).toBe("idle")

    // Only toggle recordings: a hold is ended by its release.
    const hold = new Driver(HOST).press().at(30).started()
    hold.at(300).press().repeatUntil(1_000).other("no_signal_timeout")
    expect(hold.state.phase).toBe("recording")
  })
})

describe("hold", () => {
  test("held F9 under tmux: repeats after 300 ms then 250 ms quiet finishes (release_inferred)", () => {
    const d = new Driver(HOST).press().at(30).started()
    d.at(300).press().repeatUntil(2_000)
    expect(d.state.mode).toBe("hold-inferred")
    d.at(2_240)
    expect(d.state.phase).toBe("recording")
    d.at(2_300)
    expect(d.captureEffects()).toEqual(["start_capture:key", "finish_capture:release_inferred"])
  })

  test("first autorepeat at 300 ms is not taken as a stop tap", () => {
    const d = new Driver(HOST).press().at(30).started()
    d.at(300).press()
    expect(d.captureEffects()).toEqual(["start_capture:key"])
    expect(d.state.mode).toBe("hold-inferred")
    expect(d.state.repeats).toBe(1)

    // The same with the default 600 ms repeat delay.
    const slow = new Driver().press().at(30).started()
    slow.at(600).press()
    expect(slow.state.mode).toBe("hold-inferred")
  })

  test("KKP: press, repeats, release at 600 ms finishes with reason release and sets releaseConfirmed; the next hold uses hold-key", () => {
    const d = new Driver(HOST).press().at(30).started()
    d.at(300).press(true).repeatUntil(580, true)
    expect(d.state.mode).toBe("hold-inferred")
    d.at(600).release()
    expect(d.captureEffects()).toEqual(["start_capture:key", "finish_capture:release"])
    expect(d.state.releaseConfirmed).toBe(true)
    d.at(700).finished()

    d.at(2_000).press().at(2_030).started()
    d.at(2_300).press(true)
    expect(d.state.mode).toBe("hold-key")
    d.repeatUntil(2_800, true).at(2_850).release()
    expect(d.captureEffects().slice(2)).toEqual(["start_capture:key", "finish_capture:release"])
  })

  test("KKP: release within 300 ms becomes toggle", () => {
    const d = new Driver(HOST).press().at(30).started()
    d.at(150).release()
    expect(d.state.mode).toBe("toggle")
    expect(d.state.releaseConfirmed).toBe(true)
    expect(d.captureEffects()).toEqual(["start_capture:key"])
    d.at(1_500).press()
    expect(d.captureEffects()).toEqual(["start_capture:key", "finish_capture:toggle"])
  })

  test("fresh press during hold-inferred finishes as stuck", () => {
    const d = new Driver(HOST, false).press().at(30).started()
    d.at(300).press().at(320).press().at(340).press()
    expect(d.state.mode).toBe("hold-inferred")
    // 260 ms after the last repeat, before an inferred-release tick: a new press means the release was missed.
    d.at(600).press()
    expect(d.captureEffects()).toEqual(["start_capture:key", "finish_capture:stuck"])

    // A press from the other trigger ends a hold too.
    const mouse = new Driver(HOST, false).press().at(30).started()
    mouse.at(300).press().at(320).press().at(340).mouseDown()
    expect(mouse.captureEffects()).toEqual(["start_capture:key", "finish_capture:stuck"])
  })
})

describe("mouse", () => {
  test("mouse click toggles, mouse hold finishes on release", () => {
    const d = new Driver().mouseDown()
    expect(d.captureEffects()).toEqual(["start_capture:mouse"])
    d.at(30).started().at(120).mouseUp()
    expect(d.state.mode).toBe("toggle")
    d.at(1_500).mouseDown()
    expect(d.captureEffects()).toEqual(["start_capture:mouse", "finish_capture:toggle"])
    d.at(1_600).finished()

    d.at(3_000).mouseDown().at(3_030).started().at(3_400)
    expect(d.state.mode).toBe("hold-mouse")
    d.at(3_800).mouseUp()
    expect(d.captureEffects().slice(2)).toEqual(["start_capture:mouse", "finish_capture:release"])
  })

  test("mouse up is idempotent and ignored for key recordings", () => {
    const d = new Driver().press().at(30).started().at(1_000)
    d.mouseUp().mouseUp()
    expect(d.state.phase).toBe("recording")
    expect(d.captureEffects()).toEqual(["start_capture:key"])
  })
})

describe("cancel, blur and limits", () => {
  test("Esc cancels in arming, recording and finishing", () => {
    const arming = new Driver().press().other("escape")
    expect(arming.log()).toEqual(["start_capture:key", "cancel_capture:escape", "notice:dictation cancelled"])
    expect(arming.state.phase).toBe("idle")

    const recording = new Driver().press().at(30).started().at(500).other("escape")
    expect(recording.captureEffects()).toEqual(["start_capture:key", "cancel_capture:escape"])

    const finishing = new Driver().press().at(30).started().at(1_000).at(1_500).press()
    expect(finishing.state.phase).toBe("finishing")
    finishing.other("escape")
    expect(finishing.captureEffects()).toEqual(["start_capture:key", "finish_capture:toggle", "cancel_capture:escape"])
    expect(finishing.state.phase).toBe("idle")
  })

  test("blur finishes hold modes, not toggle (policy hold)", () => {
    const hold = new Driver(HOST).press().at(30).started()
    hold.at(300).press().repeatUntil(800).other("blur")
    expect(hold.captureEffects()).toEqual(["start_capture:key", "finish_capture:blur"])

    const mouse = new Driver().mouseDown().at(30).started().at(800).other("blur")
    expect(mouse.captureEffects()).toEqual(["start_capture:mouse", "finish_capture:blur"])

    const toggle = new Driver().press().at(30).started().at(1_000).other("blur")
    expect(toggle.state.phase).toBe("recording")

    const always = new Driver({ ...DEFAULT_PTT_CONFIG, blurPolicy: "always" }).press().at(30).started().at(1_000).other("blur")
    expect(always.captureEffects()).toEqual(["start_capture:key", "finish_capture:blur"])

    const never = new Driver({ ...HOST, blurPolicy: "never" }).press().at(30).started()
    never.at(300).press().repeatUntil(800).other("blur")
    expect(never.state.phase).toBe("recording")
  })

  test("max duration finishes with a notice; 10 s warning once", () => {
    const d = new Driver({ ...DEFAULT_PTT_CONFIG, maxDurationMs: 20_000 }).press().at(30).started()
    d.at(10_000)
    expect(d.log().filter((e) => e === "notice:10 s left")).toHaveLength(0)
    d.at(10_100)
    expect(d.log().filter((e) => e === "notice:10 s left")).toHaveLength(1)
    d.at(19_900)
    expect(d.log().filter((e) => e === "notice:10 s left")).toHaveLength(1)
    d.at(20_100)
    expect(d.captureEffects()).toEqual(["start_capture:key", "finish_capture:max_duration"])
    expect(d.log().at(-1)).toBe("notice:maximum dictation length reached (0:20); transcribing")
  })

  test("finish under 300 ms becomes cancel too_short", () => {
    const d = new Driver().press().at(100).started().at(350).release()
    expect(d.captureEffects()).toEqual(["start_capture:key", "cancel_capture:too_short"])
    expect(d.state.phase).toBe("idle")

    // A finish decided before the first chunk (during arming) has no audio at all.
    const early = new Driver().press().at(150).release().at(250).press()
    expect(early.captureEffects()).toEqual(["start_capture:key", "cancel_capture:too_short"])
  })

  test("repeat in idle after Esc is ignored", () => {
    const d = new Driver(HOST).press().at(30).started()
    d.at(300).press().repeatUntil(600).other("escape")
    d.repeatUntil(2_000)
    expect(d.captureEffects()).toEqual(["start_capture:key", "cancel_capture:escape"])
    expect(d.state.phase).toBe("idle")
    // Released (no repeat for a while): the next press is fresh.
    d.at(2_400).press()
    expect(d.captureEffects().at(-1)).toBe("start_capture:key")
  })

  test("REC appears only after capture_started", () => {
    const d = new Driver().press().at(500)
    expect(d.state.phase).toBe("arming")
    expect(d.state.startedAt).toBeNull()
    d.at(520).started()
    expect(d.state.phase).toBe("recording")
    expect(d.state.startedAt).toBe(520)
  })

  test("capture_failed returns to idle from arming and recording", () => {
    const arming = new Driver().press().other("capture_failed")
    expect(arming.captureEffects()).toEqual(["start_capture:key", "cancel_capture:capture_failed"])
    expect(arming.state.phase).toBe("idle")

    const recording = new Driver().press().at(30).started().at(800).other("capture_failed")
    expect(recording.captureEffects()).toEqual(["start_capture:key", "cancel_capture:capture_failed"])
  })

  test("presses while finishing are ignored", () => {
    const d = new Driver().press().at(30).started().at(1_000).at(1_500).press()
    d.at(1_800).press().mouseDown()
    expect(d.captureEffects()).toEqual(["start_capture:key", "finish_capture:toggle"])
    expect(d.state.phase).toBe("finishing")
  })
})
