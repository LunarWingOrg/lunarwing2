/**
 * Capability-driven degradation (design 3.7 degradation table, C3, 11.3.4): a daemon without images, approvals or
 * progress events (the legacy socket's capabilities) refuses image gestures, shows no approval panel and raises
 * the 120 s stall notice; without streaming a running turn shows activity and an elapsed timer; without
 * server transcription and without other sources the mic shows "(no mic)" and the voice panel lists the reasons.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { STALL_NOTICE } from "../../src/controller/session.ts"
import { renderApp, type AppHarness } from "../helpers/render.tsx"
import { FakeTransport, turnId } from "../mocks/fake-transport.ts"

let app: AppHarness | null = null

afterEach(async () => {
  await app?.dispose()
  app = null
})

function legacySocketCapabilities(): FakeTransport {
  const transport = new FakeTransport({
    capabilities: { images: false, approvals: false, activity: false, streaming: false, correlation: false, lifecycle: false, generatedImages: false },
  })
  transport.info = { ...transport.info, protocol: "repl-legacy" }
  return transport
}

function notices(harness: AppHarness): string[] {
  return harness.store.getState().notices.map((notice) => notice.text)
}

describe("degradation", () => {
  test("legacy capabilities: image gestures refused, no approval panel, stall notice after 120 s", async () => {
    const transport = legacySocketCapabilities()
    app = await renderApp({ transport, width: 100, height: 24 })
    await app.type("/img ./cat.png")
    await app.enter()
    expect(notices(app)).toContain("this daemon does not accept images (legacy REPL)")
    expect(app.store.getState().composer.pendingImages).toHaveLength(0)

    await app.type("are you there")
    await app.enter()
    expect(transport.sentTexts).toEqual(["are you there"])
    let frame = await app.render()
    expect(frame).not.toContain("APPROVAL")
    expect(frame).toContain("legacy daemon")

    await app.advance(119_000, 1_000)
    expect(notices(app)).not.toContain(STALL_NOTICE)
    await app.advance(2_000, 1_000)
    expect(notices(app).filter((text) => text === STALL_NOTICE)).toHaveLength(1)
    await app.advance(5_000, 1_000)
    expect(notices(app).filter((text) => text === STALL_NOTICE)).toHaveLength(1)
    frame = await app.render()
    expect(frame).toContain("no reply for 120 s")
  })

  test("no streaming: a running turn shows its activity and an elapsed timer", async () => {
    const transport = new FakeTransport({ capabilities: { streaming: false } })
    app = await renderApp({ transport, width: 100, height: 24 })
    await app.type("check the disk")
    await app.enter()
    const id = turnId("t-1")
    await app.emit(
      { kind: "turn.started", turnId: id },
      { kind: "turn.activity", turnId: id, activity: { kind: "tool_started", text: "shell" } },
    )
    await app.advance(12_000, 1_000)
    const frame = await app.render()
    expect(frame).toContain(": shell")
    expect(frame).toContain("working, 12 s")
  })

  test("no transcription and no other source: (no mic), and the voice panel lists the reasons", async () => {
    const transport = new FakeTransport({ capabilities: { transcribe: false }, limits: { maxTranscribeWavBytes: 0 } })
    app = await renderApp({ transport, buildSources: true, width: 100, height: 40 })
    await app.advance(1_500)
    let frame = await app.render()
    expect(frame).toContain("(no mic)")
    await app.type("/voice")
    await app.enter()
    await app.advance(1_500)
    frame = await app.render()
    expect(app.store.getState().panel?.kind).toBe("voice")
    expect(frame).toMatch(/lunarvoice \(LunarVoice\): unavailable: \S/u)
    for (const id of ["hex-app", "hex-embedded"]) {
      expect(frame).toMatch(new RegExp(`${id}[^\\n]*(not in this build|unavailable)`, "u"))
    }
    expect(frame).toContain("Recorder: none")
  })
})
