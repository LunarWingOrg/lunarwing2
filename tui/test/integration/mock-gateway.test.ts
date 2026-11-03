/**
 * The mock gateway's "current" mode against the real server's contract (design 11.3.1): the image rejections of
 * ic/src/channels/image_mime.rs decode_images (check order) with server.rs image_rejection_response's bodies and
 * status codes, axum's 422 for request bodies that do not fit the request types (a non-string client_turn_id
 * included), the order of a V2 continuation (turn_started before gate_resolved), and GET /api/gateway/status.
 * The requests are raw HTTP: the client validates images itself, so GatewayTransport never sends these bodies.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { fixtureToken, MockGateway, TINY_PNG, type MockGatewayOptions } from "../mocks/mock-gateway.ts"

const TOKEN = fixtureToken("mock")
const MIB = 1024 * 1024
const THREAD = "5f0e4a52-6a3e-4a8f-9a1e-2b9d6c1e7a10"
const UNSUPPORTED = "unsupported image type (allowed: png, jpeg, gif, webp)"

const mocks: MockGateway[] = []

afterEach(async () => {
  for (const mock of mocks.splice(0)) await mock.stop()
})

async function start(opts: Partial<MockGatewayOptions> = {}): Promise<MockGateway> {
  const mock = await MockGateway.start({ mode: "current", token: TOKEN, ...opts })
  mocks.push(mock)
  return mock
}

async function post(mock: MockGateway, path: string, body: unknown): Promise<{ status: number; text: string }> {
  const res = await fetch(new URL(path, mock.url), {
    method: "POST",
    headers: { authorization: `Bearer ${TOKEN}`, "content-type": "application/json" },
    body: JSON.stringify(body),
  })
  return { status: res.status, text: await res.text() }
}

const b64 = (bytes: Uint8Array): string => Buffer.from(bytes).toString("base64")

/** PNG magic followed by filler, `len` bytes in total (image_mime.rs tests' png()). */
function png(len: number): Uint8Array {
  const bytes = new Uint8Array(len).fill(0x42)
  bytes.set(TINY_PNG.subarray(0, 8))
  return bytes
}

const image = (data: string, media_type = "image/png") => ({ media_type, data })
const send = (mock: MockGateway, images: unknown, extra: Record<string, unknown> = {}) =>
  post(mock, "/api/chat/send", { content: "", thread_id: THREAD, images, ...extra })

/** Resolves once the mock has emitted an event matching `match` (scripts play on a zero-delay timer). */
async function emittedUntil(mock: MockGateway, match: (event: Record<string, unknown>) => boolean): Promise<void> {
  for (let i = 0; i < 200 && !mock.emitted.some(match); i += 1) await new Promise((resolve) => setTimeout(resolve, 2))
  expect(mock.emitted.some(match)).toBe(true)
}

describe("current mode: image rejections as the server sends them", () => {
  test("bodies and status codes of image_rejection_response, in decode_images' check order", async () => {
    const mock = await start()
    const valid = b64(TINY_PNG)
    const svg = Buffer.from('<svg xmlns="http://www.w3.org/2000/svg"/>').toString("base64")
    // One base64 quad over the 5 MiB bound, of a character that is never base64: only the length check rejects it.
    const oversize = "!".repeat(4 * Math.ceil((5 * MIB) / 3) + 4)
    // Each image is under 5 MiB, together they exceed 7 MiB: refused before decoding.
    const half = "A".repeat(4_893_360)
    const cases: [unknown, number, string][] = [
      [Array.from({ length: 6 }, () => image(valid)), 422, "image 6 rejected: too many images (6, at most 5)"],
      [[image(half), image(half)], 413, "images rejected: the images exceed 7340032 bytes in total"],
      [[image(oversize)], 413, "image 1 rejected: larger than 5242880 bytes"],
      [[image(valid), image(oversize, "text/plain")], 413, "image 2 rejected: larger than 5242880 bytes"],
      [[image("not base64!!")], 422, "image 1 rejected: invalid base64"],
      // The Rust STANDARD engine wants canonical padding and no trailing bits; Buffer.from would take both.
      [[image("aGVsbG8")], 422, "image 1 rejected: invalid base64"],
      [[image("AB==")], 422, "image 1 rejected: invalid base64"],
      [[image("")], 422, "image 1 rejected: no image data"],
      [[image(valid, "text/plain")], 422, `image 1 rejected: ${UNSUPPORTED}`],
      [[image(valid), image(svg, "image/svg+xml")], 422, `image 2 rejected: ${UNSUPPORTED}`],
      [[image(svg)], 422, `image 1 rejected: ${UNSUPPORTED}`],
    ]
    for (const [images, status, body] of cases) {
      expect(await send(mock, images)).toEqual({ status, text: body })
    }
    // The sniffed type wins over the declared one; "IMAGE/" is still an image declaration.
    expect((await send(mock, [image(valid, "image/jpeg"), image(valid, "IMAGE/PNG")])).status).toBe(202)
    expect(mock.requestsTo("POST /api/chat/send")).toHaveLength(cases.length + 1)
  })

  test("decoded sizes are checked after decoding, against the limits the gateway advertises", async () => {
    const mock = await start({ limits: { max_attachment_bytes: 100, max_total_attachment_bytes: 151 } })
    // 101 bytes fit the base64 bound of 100 (136 characters) but not max_attachment_bytes.
    expect(await send(mock, [image(b64(png(101)))])).toEqual({ status: 413, text: "image 1 rejected: larger than 100 bytes" })
    // 104 + 104 base64 characters = 4 * ceil(151 / 3) pass the message bound; 75 + 77 decoded bytes exceed 151.
    expect(await send(mock, [image(b64(png(75))), image(b64(png(77)))])).toEqual({
      status: 413,
      text: "images rejected: the images exceed 151 bytes in total",
    })
    expect((await send(mock, [image(b64(png(75))), image(b64(png(76)))])).status).toBe(202)
  })

  test("a body that does not fit SendMessageRequest is axum's 422 before any handler check", async () => {
    const mock = await start()
    for (const body of [
      { thread_id: THREAD },
      { content: 7, thread_id: THREAD },
      { content: "x", thread_id: 5 },
      { content: "x", images: "png" },
      { content: "x", images: null },
      { content: "x", images: [{ media_type: "image/png" }] },
      { content: "x", images: [{ media_type: "image/png", data: 5 }] },
    ]) {
      const res = await post(mock, "/api/chat/send", body)
      expect([res.status, res.text.startsWith("Failed to deserialize the JSON body into the target type: ")]).toEqual([422, true])
    }
    // Without a thread the handler's own check runs: the shape was fine.
    expect(await post(mock, "/api/chat/send", { content: "x" })).toEqual({ status: 400, text: "thread_id is required for /api/chat/send" })
  })
})

describe("client_turn_id", () => {
  test("current mode: a non-string is axum's 422, an invalid string 400, on every route that takes it", async () => {
    const mock = await start()
    const sendWith = (id: unknown) => post(mock, "/api/chat/send", { content: "hi", thread_id: THREAD, client_turn_id: id })
    for (const id of [7, true, { id: "t-1" }, ["t-1"]]) {
      const res = await sendWith(id)
      expect([res.status, res.text]).toEqual([422, expect.stringContaining("client_turn_id: invalid type:")])
    }
    // A missing thread_id is not reached: the extractor refuses the body first.
    expect((await post(mock, "/api/chat/send", { content: "hi", client_turn_id: 7 })).status).toBe(422)
    for (const id of ["", "a.b", "a b", "x".repeat(65)]) expect(await sendWith(id)).toEqual({ status: 400, text: "invalid client_turn_id" })
    expect((await sendWith(null)).status).toBe(202)
    expect((await sendWith("t-0193f2a1_ok")).status).toBe(202)

    const approval = await post(mock, "/api/chat/approval", { request_id: crypto.randomUUID(), action: "approve", client_turn_id: 7 })
    expect(approval.status).toBe(422)
    const token = await post(mock, "/api/chat/auth-token", { extension_name: "github", token: "x", client_turn_id: 7 })
    expect(token.status).toBe(422)
    const cancel = await post(mock, "/api/chat/auth-cancel", { extension_name: "github", client_turn_id: false })
    expect(cancel.status).toBe(422)
    expect((await post(mock, "/api/chat/auth-cancel", { extension_name: "github", client_turn_id: "a.b" })).text).toBe("invalid client_turn_id")
  })

  test("legacy mode: daemons that predate M2a ignore the unknown field", async () => {
    const mock = await start({ mode: "legacy" })
    expect((await post(mock, "/api/chat/send", { content: "hi", thread_id: THREAD, client_turn_id: 7 })).status).toBe(202)
  })
})

describe("V2 continuations", () => {
  test("an approval continuation starts with turn_started under the owning id, then gate_resolved", async () => {
    const mock = await start()
    const accepted = await post(mock, "/api/chat/send", { content: "!approval", thread_id: THREAD, client_turn_id: "t-1" })
    expect(accepted.status).toBe(202)
    await emittedUntil(mock, (event) => event.type === "approval_needed")
    const requestId = [...mock.approvals.keys()][0] ?? ""
    const from = mock.emitted.length
    expect((await post(mock, "/api/chat/approval", { request_id: requestId, action: "approve", thread_id: THREAD, client_turn_id: "t-1" })).status).toBe(202)
    await emittedUntil(mock, (event) => event.type === "turn_finished" && event.outcome === "replied")
    const continuation = mock.emitted.slice(from)
    expect(continuation.map((event) => event.type)).toEqual(["turn_started", "gate_resolved", "tool_started", "tool_completed", "response", "turn_finished"])
    expect(continuation[0]).toMatchObject({ client_turn_id: "t-1" })
    expect(continuation[1]).not.toHaveProperty("client_turn_id")
  })

  test("a credential continuation (auth-token) starts with turn_started too", async () => {
    const mock = await start()
    const gate = mock.addAuthGate({ request_id: crypto.randomUUID(), thread_id: THREAD, extension_name: "github", instructions: "token" })
    const body = { extension_name: "github", token: "x", request_id: gate.request_id, thread_id: THREAD, client_turn_id: "t-2" }
    expect((await post(mock, "/api/chat/auth-token", body)).status).toBe(200)
    await emittedUntil(mock, (event) => event.type === "turn_finished")
    expect(mock.emitted.map((event) => event.type)).toEqual(["turn_started", "gate_resolved", "response", "turn_finished"])
  })
})

describe("GET /api/gateway/status", () => {
  test("uptime_secs counts whole seconds on the injected clock; restart() starts it again", async () => {
    let now = 1_000_000
    const mock = await start({ now: () => now })
    const status = async () => {
      const res = await fetch(new URL("/api/gateway/status", mock.url), { headers: { authorization: `Bearer ${TOKEN}` } })
      return { status: res.status, body: (await res.json()) as Record<string, unknown> }
    }
    now += 61_900
    expect(await status()).toMatchObject({ status: 200, body: { uptime_secs: 61, version: "2.1.9-mock" } })
    mock.restart()
    now += 4_000
    expect((await status()).body.uptime_secs).toBe(4)
    const unauthorized = await fetch(new URL("/api/gateway/status", mock.url))
    expect(unauthorized.status).toBe(401)
  })
})
