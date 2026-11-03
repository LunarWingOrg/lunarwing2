/**
 * Credential mode (design 3.6.4, SEC-03, UX-13, 11.3.4): the secret never reaches a frame, the store or a log;
 * Esc arms and a second Esc within 2 s cancels through auth-cancel; the masked buffer is zero-filled afterwards.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { NOTES } from "../../src/state/correlate.ts"
import { renderApp, type AppHarness } from "../helpers/render.tsx"
import { FakeTransport, turnId } from "../mocks/fake-transport.ts"

const SECRET = "ghp_SuperSecret1234567890"

let app: AppHarness | null = null

afterEach(async () => {
  await app?.dispose()
  app = null
})

async function inCredentialMode(transport: FakeTransport): Promise<AppHarness> {
  const harness = await renderApp({ transport })
  await harness.type("connect github")
  await harness.enter()
  const owner = turnId("t-1")
  await harness.emit(
    { kind: "turn.started", turnId: owner },
    {
      kind: "turn.auth_required",
      turnId: owner,
      extensionName: "github",
      instructions: "Paste a personal access token",
      authUrl: "https://github.com/settings/tokens",
    },
    { kind: "turn.finished", turnId: owner, outcome: "paused" },
  )
  return harness
}

describe("credential mode", () => {
  test("a pasted credential never appears in a frame or the store", async () => {
    const transport = new FakeTransport()
    app = await inCredentialMode(transport)
    let frame = await app.render()
    expect(frame).toContain("credential for github - Enter sends it, Esc Esc cancels")
    expect(frame).toContain("Paste a personal access token")
    expect(frame).toContain("Enter send credential - Esc Esc cancel")

    await app.paste(SECRET)
    frame = await app.render()
    expect(frame).toContain(`credential for github: **** (${SECRET.length} chars)`)
    expect(frame).not.toContain(SECRET)
    expect(frame).not.toContain("SuperSecret")
    expect(JSON.stringify(app.store.getState())).not.toContain("SuperSecret")
    expect(app.runtime.composer.getText()).toBe("")

    await app.enter()
    frame = await app.render()
    expect(frame).not.toContain("SuperSecret")
    const calls = transport.callsOf("authToken")
    expect(calls.map((call) => [call.req.extensionName, call.req.turnId, call.tokenLength])).toEqual([["github", "t-1", SECRET.length]])
    expect(transport.lastSecret?.disposed).toBe(true)
    expect(transport.callsOf("send")).toHaveLength(1)
    expect(app.store.getState().composer.credential).toBeNull()
    expect(app.runtime.credential.length).toBe(0)
    expect(JSON.stringify(app.store.getState())).not.toContain("SuperSecret")
  })

  test("typed characters go to the masked buffer, Backspace removes one", async () => {
    const transport = new FakeTransport()
    app = await inCredentialMode(transport)
    await app.type("abc")
    await app.press("\b")
    const frame = await app.render()
    expect(frame).toContain("credential for github: **** (2 chars)")
    expect(app.runtime.composer.getText()).toBe("")
  })

  test("Esc arms and a second Esc within 2 s cancels; the buffer is cleared", async () => {
    const transport = new FakeTransport()
    app = await inCredentialMode(transport)
    await app.type("partial")
    await app.escape()
    let frame = await app.render()
    expect(frame).toContain("press Esc again to cancel the credential")
    expect(transport.callsOf("authCancel")).toHaveLength(0)

    await app.escape()
    expect(transport.callsOf("authCancel").map((call) => [call.req.extensionName, call.req.turnId])).toEqual([["github", "t-1"]])
    const state = app.store.getState()
    expect(state.composer.credential).toBeNull()
    expect(app.runtime.credential.length).toBe(0)
    const owner = state.turns[turnId("t-1")]
    expect(owner?.phase).toBe("interrupted")
    expect(owner?.notes).toContain(NOTES.credentialCancelled)

    // The composer gets the keys again.
    await app.type("hello")
    frame = await app.render()
    expect(app.runtime.composer.getText()).toBe("hello")
    expect(frame).not.toContain("partial")
  })

  test("an Esc after the 2 s window arms again instead of cancelling", async () => {
    const transport = new FakeTransport()
    app = await inCredentialMode(transport)
    await app.escape()
    await app.advance(2_500)
    await app.escape()
    expect(transport.callsOf("authCancel")).toHaveLength(0)
    expect(app.store.getState().composer.credential?.escArmedAt).not.toBeNull()
  })

  test("on a V1 engine credential mode ends after 290 s without sending", async () => {
    const transport = new FakeTransport({ engine: "v1" })
    app = await inCredentialMode(transport)
    await app.type("half")
    await app.advance(291_000, 1_000)
    expect(app.store.getState().composer.credential).toBeNull()
    expect(app.runtime.credential.length).toBe(0)
    expect(transport.callsOf("authToken")).toHaveLength(0)
    expect(app.store.getState().notices.map((notice) => notice.text)).toContain(NOTES.credentialEnded)
  })
})
