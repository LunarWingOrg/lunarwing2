/**
 * The footer (design 3.12.3, UX-04, 11.3.4): one case per row of the table, from states built with the real
 * reducer, plus the rendered last row of the app.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { footerHint } from "../../src/controller/footer-hint.ts"
import type { Action } from "../../src/state/actions.ts"
import { createInitialState, type AppState, type TurnId } from "../../src/state/model.ts"
import { reduce } from "../../src/state/reducer.ts"
import type { WireApproval } from "../../src/transport/types.ts"
import { rows } from "../helpers/frames.ts"
import { renderApp, withMic, type AppHarness } from "../helpers/render.tsx"
import { fakeServerInfo } from "../mocks/fake-transport.ts"

const T0 = 1_000_000

function state(...actions: Action[]): AppState {
  const initial = createInitialState({ profile: "test", tenant: "derpy", view: { width: 100, height: 30 }, at: T0 })
  const ready: Action = { type: "conn/ready", info: fakeServerInfo(), at: T0 }
  return [ready, ...actions].reduce(reduce, initial)
}

function approval(id: string, allowAlways = true): Action {
  const wire: WireApproval = { id, threadId: null, toolName: "shell", description: "d", parameters: "{}", allowAlways, truncated: false, source: "live" }
  return { type: "evt", event: { kind: "turn.approval", turnId: null, approval: wire }, at: T0 }
}

describe("footerHint, one row each", () => {
  test("offline", () => {
    const s = state({ type: "conn/offline", reason: "port 10000 is held by uid 1008", fatal: true, at: T0 })
    expect(footerHint(s)).toBe("offline: port 10000 is held by uid 1008 - /transport reconnect - F1 help")
  })

  test("credential mode, and the armed Esc", () => {
    const s = state({ type: "evt", event: { kind: "turn.auth_required", turnId: null, extensionName: "github" }, at: T0 })
    expect(footerHint(s, { now: T0 })).toBe("Enter send credential - Esc Esc cancel")
    const armed = reduce(s, { type: "composer/credential_esc_armed", armed: true, at: T0 + 100 })
    expect(footerHint(armed, { now: T0 + 500 })).toBe("press Esc again to cancel the credential")
  })

  test("confirmation pending", () => {
    const s = state(approval("req-1"), { type: "approval/confirm_requested", approvalId: "req-1", decision: "approve", at: T0 })
    expect(footerHint(s)).toBe("Enter confirm - Esc back")
  })

  test("approval pending: Alt+A only with allowAlways, next only with more than one prompt", () => {
    expect(footerHint(state(approval("req-1")))).toBe("Alt+Y approve - Alt+N deny - Alt+A always - Alt+V view")
    expect(footerHint(state(approval("req-1", false)))).toBe("Alt+Y approve - Alt+N deny - Alt+V view")
    // Without KKP (and always inside tmux) Alt+] arrives as ESC ], which the input parser drops: Alt+J is offered.
    expect(footerHint(state(approval("req-1"), approval("req-2")))).toBe(
      "Alt+Y approve - Alt+N deny - Alt+A always - Alt+V view - Alt+J next",
    )
    const kkp: Action = { type: "ui/patch", patch: { kkpRelease: true }, at: T0 }
    expect(footerHint(state(kkp, approval("req-1"), approval("req-2")))).toBe(
      "Alt+Y approve - Alt+N deny - Alt+A always - Alt+V view - Alt+] next",
    )
    const tmux: Action = { type: "ui/patch", patch: { kkpRelease: true, tmux: true }, at: T0 }
    expect(footerHint(state(tmux, approval("req-1"), approval("req-2")))).toMatch(/ - Alt\+J next$/u)
  })

  test("approval pending behind a panel: the keys answer nothing, so the row says Esc", () => {
    const help: Action = { type: "panel/open", kind: "help", at: T0 }
    expect(footerHint(state(approval("req-1"), help))).toBe("Esc close the panel - an approval waits behind it")
    const view: Action = { type: "panel/open", kind: "approval", at: T0 }
    const answered: Action = { type: "approval/answered", approvalId: "req-1", ok: true, expired: false, at: T0 }
    expect(footerHint(state(approval("req-1"), view))).toBe("Alt+Y approve - Alt+N deny - Alt+A always - Alt+V view")
    expect(footerHint(state(approval("req-1"), view, answered, approval("req-2")))).toBe("Esc close the view - the next approval waits behind it")
  })

  test("recording, toggle and hold", () => {
    const toggle = state({ type: "voice/state", patch: { phase: "recording", mode: "toggle", startedAt: T0 }, at: T0 })
    expect(footerHint(toggle, { now: T0 + 7_000 })).toBe("REC 0:07 - F9 stop - Esc cancel")
    const hold = state({ type: "voice/state", patch: { phase: "recording", mode: "hold-inferred", startedAt: T0 }, at: T0 })
    expect(footerHint(hold, { now: T0 + 7_000 })).toBe("REC 0:07 - release to stop - Esc cancel")
  })

  test("transcribing", () => {
    const s = state({
      type: "voice/state",
      patch: { queue: [{ id: "c1", durationMs: 3_000, state: "transcribing", source: "lunarvoice", startedAt: T0, attempt: 0 }] },
      at: T0,
    })
    expect(footerHint(s)).toBe("transcribing - Esc cancel")
  })

  test("turn running", () => {
    const s = state({ type: "turn/created", turnId: "t-1" as TurnId, text: "hi", images: [], at: T0 })
    expect(footerHint(s)).toBe("Enter queue - Ctrl+S stop - /steer <note> - F1 help")
  })

  test("idle", () => {
    expect(footerHint(state())).toBe("F9 dictate - Ctrl+V image - /shot - Enter send - F1 help")
    expect(footerHint(state(), { ptt: "F8" })).toBe("F8 dictate - Ctrl+V image - /shot - Enter send - F1 help")
    // --no-voice or no mic: no push-to-talk key is offered
    expect(footerHint(state(), { voice: false })).toBe("Ctrl+V image - /shot - Enter send - F1 help")
  })
})

describe("the footer row", () => {
  let app: AppHarness | null = null

  afterEach(async () => {
    await app?.dispose()
    app = null
  })

  test("is the last row and follows the state", async () => {
    app = await renderApp({ ...withMic(), width: 100, height: 24 })
    let frame = await app.render()
    expect(rows(frame)[23]).toBe("F9 dictate - Ctrl+V image - /shot - Enter send - F1 help")
    await app.type("question")
    await app.enter()
    frame = await app.render()
    expect(rows(frame)[23]).toBe("Enter queue - Ctrl+S stop - /steer <note> - F1 help")
  })

  test("with --no-voice neither the footer nor the welcome offers push-to-talk", async () => {
    app = await renderApp({ ...withMic(), voice: { enabled: false }, width: 100, height: 24 })
    const frame = await app.render()
    expect(frame).toContain("(no mic)")
    expect(rows(frame)[23]).toBe("Ctrl+V image - /shot - Enter send - F1 help")
    expect(frame).toContain("push-to-talk: off (started with --no-voice)")
    expect(frame).not.toContain("F9 dictate")
    expect(frame).not.toContain("tap F9")
  })

  test("without any recorder or voice source the welcome says why instead of explaining F9", async () => {
    app = await renderApp({ width: 100, height: 24 })
    const frame = await app.render()
    expect(frame).toContain("(no mic)")
    expect(rows(frame)[23]).toBe("Ctrl+V image - /shot - Enter send - F1 help")
    expect(frame).toContain("push-to-talk: no microphone or voice source is available (/voice shows why)")
  })
})
