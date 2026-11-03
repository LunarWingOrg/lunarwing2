/**
 * Pure label and panel builders (design 7.4, 3.12.3, 5.6): the VU meter cells, mic labels, the one voice status
 * line per state, the byte counter, and the transport and privacy panels (no token, active backend marked).
 */
import { describe, expect, test } from "bun:test"
import { byteCounter, composerTitle, voiceStatusLine } from "../../src/components/composer.tsx"
import { micLabel, noMicAvailable } from "../../src/components/mic-widget.tsx"
import { vuCells, VU_CELLS } from "../../src/components/vu-meter.tsx"
import { buildPanel, pttHelp, type PanelEnv } from "../../src/controller/panel-content.ts"
import type { Action } from "../../src/state/actions.ts"
import { createInitialState, type AppState } from "../../src/state/model.ts"
import { reduce } from "../../src/state/reducer.ts"
import type { VoiceQueueItem, VoiceStatusReport } from "../../src/voice/types.ts"
import { truncate, wrapText } from "../../src/util.ts"
import { fakeServerInfo } from "../mocks/fake-transport.ts"

const T0 = 5_000_000

function state(...actions: Action[]): AppState {
  const initial = createInitialState({ profile: "test", tenant: "derpy", view: { width: 100, height: 30 }, at: T0 })
  return [{ type: "conn/ready", info: fakeServerInfo(), at: T0 } as Action, ...actions].reduce(reduce, initial)
}

function voice(patch: Partial<AppState["voice"]>): Action {
  return { type: "voice/state", patch, at: T0 }
}

function report(patch: Partial<VoiceStatusReport> = {}): VoiceStatusReport {
  return {
    enabled: true,
    order: ["lunarvoice", "hex-app", "hex-embedded"],
    pinned: null,
    sources: [
      { id: "lunarvoice", label: "LunarVoice", privacyNote: "audio goes to the daemon's host", inBuild: true, availability: { available: true, checkedAt: T0 }, maxWavBytes: 18 * 1024 * 1024 },
      { id: "hex-app", label: "Hex (running app)", privacyNote: "local Hex app", inBuild: true, availability: { available: false, reason: "Hex not running", checkedAt: T0 }, maxWavBytes: 64 * 1024 * 1024 },
      { id: "hex-embedded", label: "Hex (embedded service)", privacyNote: "local Hex service", inBuild: false, availability: null, maxWavBytes: null },
    ],
    recorder: { kind: "opentui" },
    device: { name: null, origin: "default" },
    retained: null,
    lastAttempts: [],
    maxDurationMs: 120_000,
    ...patch,
  }
}

function env(patch: Partial<PanelEnv> = {}): PanelEnv {
  return { width: 90, now: T0, pttKeys: ["f9", "ctrl+space"], voice: report(), ...patch }
}

describe("VU meter and mic widget", () => {
  test("12 cells from -60 to 0 dBFS with the held peak and the clip marker", () => {
    expect(vuCells(null)).toBe(".".repeat(VU_CELLS))
    expect(vuCells({ rmsDb: -30, peakDb: -20, heldPeakDb: -10, clipping: false, noSignal: false })).toBe("======...|..")
    expect(vuCells({ rmsDb: 0, peakDb: 0, heldPeakDb: 0, clipping: true, noSignal: false })).toBe("===========!")
    expect(vuCells({ rmsDb: -95, peakDb: -95, heldPeakDb: -95, clipping: false, noSignal: true })).toBe(".".repeat(VU_CELLS))
  })

  test("every mic label fits 8 columns", () => {
    const labels = [
      micLabel({ phase: "idle", startedAt: null, queue: [] }, { noMic: false, now: T0 }),
      micLabel({ phase: "arming", startedAt: null, queue: [] }, { noMic: false, now: T0 }),
      micLabel({ phase: "recording", startedAt: T0, queue: [] }, { noMic: false, now: T0 + 7_400 }),
      micLabel({ phase: "idle", startedAt: null, queue: [{} as VoiceQueueItem] }, { noMic: false, now: T0 }),
      micLabel({ phase: "idle", startedAt: null, queue: [] }, { noMic: true, now: T0 }),
    ]
    expect(labels).toEqual(["( mic )", "( ... )", "REC 0:07", "[ .... ]", "(no mic)"])
    for (const label of labels) expect(label.length).toBeLessThanOrEqual(8)
  })

  test("(no mic) when voice is off, no recorder works, or every source is unavailable", () => {
    expect(noMicAvailable(null)).toBe(true)
    expect(noMicAvailable(report())).toBe(false)
    expect(noMicAvailable(report({ enabled: false }))).toBe(true)
    expect(noMicAvailable(report({ recorder: { kind: null, reason: "no PulseAudio" } }))).toBe(true)
    expect(noMicAvailable(report({ recorder: { kind: null, reason: "not checked yet" } }))).toBe(false)
    const down = report().sources.map((source) => ({ ...source, availability: { available: false, reason: "down", checkedAt: T0 } }))
    expect(noMicAvailable(report({ sources: down }))).toBe(true)
  })
})

describe("the voice status line (one at a time)", () => {
  test("arming, toggle, hold, transcribing, busy retry, waiting clips", () => {
    expect(voiceStatusLine(state(voice({ phase: "arming" })), T0, "F9")).toBe("opening mic...")
    expect(voiceStatusLine(state(voice({ phase: "recording", mode: "toggle", startedAt: T0 })), T0 + 7_000, "F9")).toBe("REC 0:07 - F9 stop - Esc cancel")
    expect(voiceStatusLine(state(voice({ phase: "recording", mode: "hold-key", startedAt: T0 })), T0 + 7_000, "F9")).toBe("REC 0:07 - release to stop - Esc cancel")
    const transcribing: VoiceQueueItem = { id: "a", durationMs: 4_000, state: "transcribing", source: "lunarvoice", startedAt: T0, attempt: 0 }
    expect(voiceStatusLine(state(voice({ queue: [transcribing] })), T0 + 3_000, "F9")).toBe("transcribing 0:03 - lunarvoice - Esc cancel")
    const retry: VoiceQueueItem = { id: "a", durationMs: 4_000, state: "waiting", source: "lunarvoice", startedAt: T0 + 2_000, attempt: 2 }
    expect(voiceStatusLine(state(voice({ queue: [retry] })), T0, "F9")).toBe("lunarvoice busy - retry 2/3 in 2 s")
    const waiting: VoiceQueueItem = { id: "b", durationMs: 1_000, state: "waiting", source: null, startedAt: null, attempt: 0 }
    expect(voiceStatusLine(state(voice({ queue: [waiting, { ...waiting, id: "c" }] })), T0, "F9")).toBe("2 clips waiting")
    expect(voiceStatusLine(state(), T0, "F9")).toBeNull()
  })

  test("the byte counter appears above 80 % and the credential title replaces it", () => {
    expect(byteCounter(state({ type: "composer/metrics", lines: 1, bytes: 80_000, at: T0 }))).toBeNull()
    expect(byteCounter(state({ type: "composer/metrics", lines: 1, bytes: 90_000, at: T0 }))).toBe("88 KiB / 98 KiB")
    const credential = state({ type: "evt", event: { kind: "turn.auth_required", turnId: null, extensionName: "github" }, at: T0 })
    expect(composerTitle(credential, T0, "F9")).toBe("credential for github - Enter sends it, Esc Esc cancels")
  })
})

describe("panels", () => {
  test("the transport panel shows the endpoint, owner and limits, never a token", () => {
    const lines = buildPanel("transport", state(), env({ listenerOwner: "uid 1010 (derpy)", tokenSource: "tenants/derpy.json (client token tui:sun)" }))
    const text = lines.join("\n")
    expect(text).toContain("Endpoint: http://127.0.0.1:9")
    expect(text).toContain("Listener owner: uid 1010 (derpy)")
    expect(text).toContain("Credential: unknown (tenants/derpy.json (client token tui:sun))")
    expect(text).toContain("message 98 KiB, images 5 x 5 MiB (7 MiB total)")
    expect(text).toContain("vision: images go to the configured model; vision is not verified for derpy")
    expect(text).not.toMatch(/tok-|Bearer|hex_[0-9a-f]{8}/u)
    for (const line of lines) expect(line.length).toBeLessThanOrEqual(90)
  })

  test("the privacy panel marks the backend in use", () => {
    const text = buildPanel("privacy", state(), env()).join("\n")
    expect(text).toContain("* LunarVoice: audio goes to the daemon's host")
    expect(text).toContain("  Hex (running app): local Hex app")
    expect(text).toContain("stored in derpy's workspace (no expiry)")
  })

  test("the push-to-talk help follows the environment", () => {
    const tmux = state({ type: "ui/patch", patch: { tmux: true }, at: T0 })
    expect(pttHelp(tmux, ["f9"])).toContain("holding F9 also works (key repeat)")
    const kkp = state({ type: "ui/patch", patch: { kkpRelease: true }, at: T0 })
    expect(pttHelp(kkp, ["ctrl+space"])).toContain("hold Ctrl+Space to talk and release to stop")
  })
})

describe("util", () => {
  test("wrapText splits long words instead of cutting them, and keeps lines that fit", () => {
    expect(wrapText("  indented line", 40)).toEqual(["  indented line"])
    expect(wrapText("aaaaaaaaaaaaaaaaaaaa", 8)).toEqual(["aaaaaaaa", "aaaaaaaa", "aaaa"])
    expect(wrapText("one two three", 8)).toEqual(["one two", "three"])
    expect(wrapText("a\n\nb", 8)).toEqual(["a", "", "b"])
  })

  test("truncate counts display cells and never splits a surrogate pair", () => {
    expect(truncate("hello world", 6)).toBe("hello…")
    expect(truncate("日本語テキスト", 7)).toBe("日本語…")
    expect(truncate("😀😀😀", 3)).toBe("😀…")
  })
})
