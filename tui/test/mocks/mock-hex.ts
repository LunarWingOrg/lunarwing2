/**
 * Mock Hex local API (design 11.3.1, M1b): Bun.serve on 127.0.0.1 and an ephemeral port, plus, in "app" mode, a
 * 0600 local-api.json in a temp HEX_APPLICATION_SUPPORT_DIR. Behaviour follows Hex dev 54a3a66
 * (hex:src/local_api.rs:576-728, :939-1080; hex:src/local_api/http.rs):
 *
 * - Every route requires `Authorization: Bearer <token>`; anything else is an empty 401.
 * - GET /health -> {version, apiVersion:"2"}; GET /capabilities; GET /models?language= -> the catalog with
 *   readiness for that language (400 unsupported-language; a repeated parameter is 400 invalid-request).
 * - POST /transcriptions?model=&language=: Content-Type audio/wav (415 unsupported-audio), Content-Length
 *   required (411 length-required), Transfer-Encoding refused (400), at most 64 MiB (413 resource-exhausted),
 *   `model` required (400 unknown-model), the model must support the language (400 unsupported-model) and be
 *   verified (409 model-not-ready), `queueFull` -> 429 queue-full; the body must be a PCM WAV (400 invalid-audio)
 *   -> 200 {transcript, durationMs}.
 * - POST /models/{id}/prepare is recorded (tests assert it never happens) and answers an SSE "ok".
 *
 * Every request is recorded (method, path, query, lowercased headers, body size, whether it was authorized).
 *
 * Standalone, it is the "fake hex script" of the integration test (never a real Hex binary):
 *   bun test/mocks/mock-hex.ts --version             prints "hex 3.3.0"
 *   bun test/mocks/mock-hex.ts service --embedded    serves on 127.0.0.1:0, writes the port to
 *                                                    $HEX_APPLICATION_SUPPORT_DIR/mock-embedded-port, prints the
 *                                                    ready line with its own pid, and exits when stdin closes
 */
import { mkdir, rename, rm, symlink, writeFile } from "node:fs/promises"
import { join } from "node:path"
import { makeTempDir, writeFileWithMode } from "../helpers/tmp.ts"
import type { ProcReader } from "../../src/transport/listener-owner.ts"
import { procNetTcpLine } from "./mock-gateway.ts"

export const MOCK_HEX_VERSION = "3.3.0"
export const MOCK_HEX_PORT_FILE = "mock-embedded-port"
const MAX_AUDIO_BYTES = 64 * 1024 * 1024
const KNOWN_LANGUAGES = new Set(["en", "de", "fr", "es", "it", "nl", "pt", "ru", "auto"])

export interface MockHexModel {
  id: string
  name: string
  installed: boolean
  verified: boolean
  managed?: boolean
  downloadBytes: number | null
  languages: string[]
  supportsLanguageDetection: boolean
  source?: string
}

/** The default catalog: Parakeet Unified English ready, Parakeet v3 and Whisper not downloaded. */
export function defaultMockModels(): MockHexModel[] {
  return [
    { id: "parakeet_v3", name: "Parakeet TDT v3", installed: false, verified: false, downloadBytes: 740_000_000, languages: ["en", "de", "fr"], supportsLanguageDetection: false },
    { id: "parakeet_unified_en", name: "Parakeet Unified English", installed: true, verified: true, downloadBytes: 731_000_000, languages: ["en"], supportsLanguageDetection: false },
    { id: "whisper_large_v3_turbo", name: "Whisper large-v3-turbo", installed: false, verified: false, downloadBytes: 886_000_000, languages: ["*", "auto"], supportsLanguageDetection: true },
  ]
}

export interface MockHexOptions {
  /** "app" (default) writes local-api.json; "embedded" writes nothing (the ready line carries the endpoint). */
  mode?: "app" | "embedded"
  /** Default: a fresh per-test temp dir. */
  supportDir?: string
  /** Default: a random hex_<64 hex>. */
  token?: string
  /** pid published in local-api.json (default process.pid: alive and ours). */
  pid?: number
  version?: string
  models?: MockHexModel[]
  /** Default "check the ports registry". */
  transcript?: string | ((wav: Uint8Array) => string)
}

export interface HexRecordedRequest {
  readonly method: string
  readonly path: string
  readonly query: Readonly<Record<string, string>>
  /** Lowercased header names. */
  readonly headers: Readonly<Record<string, string>>
  readonly bodyBytes: number
  readonly authorized: boolean
}

export type MockHexHandler = (req: Request, rec: HexRecordedRequest) => Response | Promise<Response>

export interface DiscoveryDocument {
  port: unknown
  token: unknown
  apiVersion: unknown
  pid: unknown
  [key: string]: unknown
}

export function randomHexToken(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32))
  return `hex_${Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("")}`
}

function code(status: number, value: string): Response {
  return Response.json({ code: value }, { status })
}

function empty(status: number): Response {
  return new Response(null, { status })
}

/** Minimal PCM WAV check (RIFF/WAVE, fmt PCM 16-bit, 8-192 kHz, 1-8 channels); duration in ms or null. */
export function wavDurationMs(bytes: Uint8Array): number | null {
  if (bytes.length < 44) return null
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  const ascii = (at: number) => String.fromCharCode(...bytes.subarray(at, at + 4))
  if (ascii(0) !== "RIFF" || ascii(8) !== "WAVE" || ascii(12) !== "fmt ") return null
  const format = view.getUint16(20, true)
  const channels = view.getUint16(22, true)
  const rate = view.getUint32(24, true)
  const bits = view.getUint16(34, true)
  if (format !== 1 || bits !== 16 || channels < 1 || channels > 8 || rate < 8_000 || rate > 192_000) return null
  if (ascii(36) !== "data") return null
  const dataBytes = Math.min(view.getUint32(40, true), bytes.length - 44)
  return Math.round((dataBytes / (channels * 2) / rate) * 1000)
}

export class MockHex {
  readonly requests: HexRecordedRequest[] = []
  readonly mode: "app" | "embedded"
  readonly supportDir: string
  /** The accepted bearer token (rotate it to simulate a Hex restart). */
  token: string
  version: string
  models: MockHexModel[]
  transcript: string | ((wav: Uint8Array) => string)
  /** 429 queue-full for every transcription while set. */
  queueFull = false
  /** The pid local-api.json publishes by default. */
  private readonly publishedPid: number
  private readonly server: ReturnType<typeof Bun.serve>
  private readonly overrides = new Map<string, { handler: MockHexHandler; times: number }>()
  private releaseHandlers: (response: Response) => void = () => undefined
  private readonly stopping = new Promise<Response>((resolve) => {
    this.releaseHandlers = resolve
  })

  private constructor(opts: Required<Pick<MockHexOptions, "mode" | "supportDir">> & MockHexOptions) {
    this.mode = opts.mode
    this.supportDir = opts.supportDir
    this.token = opts.token ?? randomHexToken()
    this.version = opts.version ?? MOCK_HEX_VERSION
    this.models = opts.models ?? defaultMockModels()
    this.transcript = opts.transcript ?? "check the ports registry"
    this.publishedPid = opts.pid ?? process.pid
    this.server = Bun.serve({
      hostname: "127.0.0.1",
      port: 0,
      maxRequestBodySize: MAX_AUDIO_BYTES + 1024,
      fetch: (req) => Promise.race([this.handle(req), this.stopping]),
    })
  }

  static async start(opts: MockHexOptions = {}): Promise<MockHex> {
    const mode = opts.mode ?? "app"
    const supportDir = opts.supportDir ?? (await makeTempDir("lw-tui-hex-"))
    const mock = new MockHex({ ...opts, mode, supportDir })
    if (mode === "app") await mock.writeDiscovery()
    return mock
  }

  get port(): number {
    return this.server.port ?? 0
  }

  get baseUrl(): string {
    return `http://127.0.0.1:${this.port}`
  }

  get discoveryPath(): string {
    return join(this.supportDir, "local-api.json")
  }

  /** {port, token, apiVersion:"2", pid} for this server, with `change` merged in. */
  discoveryDocument(change: Partial<DiscoveryDocument> = {}): DiscoveryDocument {
    return { port: this.port, token: this.token, apiVersion: "2", pid: this.publishedPid, ...change }
  }

  /** (Re)writes local-api.json atomically like Hex (temp file + rename), mode exactly `mode`. */
  async writeDiscovery(change: Partial<DiscoveryDocument> = {}, mode = 0o600): Promise<void> {
    await mkdir(this.supportDir, { recursive: true, mode: 0o700 })
    const temporary = `${this.discoveryPath}.${process.pid}.tmp`
    await writeFileWithMode(temporary, `${JSON.stringify(this.discoveryDocument(change))}\n`, mode)
    await rename(temporary, this.discoveryPath)
  }

  /** Replaces local-api.json with a symlink to a valid 0600 copy elsewhere in the support dir. */
  async symlinkDiscovery(): Promise<void> {
    const target = join(this.supportDir, "real-local-api.json")
    await writeFileWithMode(target, `${JSON.stringify(this.discoveryDocument())}\n`, 0o600)
    await rm(this.discoveryPath, { force: true })
    await symlink(target, this.discoveryPath)
  }

  async removeDiscovery(): Promise<void> {
    await rm(this.discoveryPath, { force: true })
  }

  /** The embedded ready line this server would print as `pid`. */
  readyLine(pid: number, change: Record<string, unknown> = {}): string {
    return `${JSON.stringify({ type: "ready", url: this.baseUrl, token: this.token, apiVersion: "2", pid, ...change })}\n`
  }

  /** Replaces "METHOD /path" for the next `times` requests (default all); null removes it. */
  override(route: string, handler: MockHexHandler | null, times = Number.POSITIVE_INFINITY): void {
    if (handler === null) this.overrides.delete(route)
    else this.overrides.set(route, { handler, times })
  }

  requestsTo(path: string, method?: string): HexRecordedRequest[] {
    return this.requests.filter((r) => r.path === path && (method === undefined || r.method === method))
  }

  async stop(): Promise<void> {
    this.releaseHandlers(empty(503))
    // Let Bun finish the responses just released: stop(true) in the same turn can wait forever on one of them.
    await new Promise<void>((resolve) => setImmediate(resolve))
    await this.server.stop(true)
  }

  private async handle(req: Request): Promise<Response> {
    const url = new URL(req.url)
    const headers: Record<string, string> = {}
    req.headers.forEach((value, name) => {
      headers[name.toLowerCase()] = value
    })
    const authorized = headers.authorization === `Bearer ${this.token}`
    const body = req.method === "POST" && authorized ? new Uint8Array(await req.arrayBuffer()) : new Uint8Array(0)
    const query: Record<string, string> = {}
    url.searchParams.forEach((value, name) => {
      if (!(name in query)) query[name] = value
    })
    const rec: HexRecordedRequest = { method: req.method, path: url.pathname, query, headers, bodyBytes: body.length, authorized }
    this.requests.push(rec)
    if (!authorized) return empty(401)
    const route = `${req.method} ${url.pathname}`
    const override = this.overrides.get(route)
    if (override !== undefined) {
      override.times -= 1
      if (override.times <= 0) this.overrides.delete(route)
      return override.handler(req, rec)
    }
    return this.route(req, url, rec, body)
  }

  private route(req: Request, url: URL, rec: HexRecordedRequest, body: Uint8Array): Response {
    if (req.method === "POST" && url.pathname === "/transcriptions") return this.transcribe(url, rec, body)
    if (req.method === "POST" && /^\/models\/[^/]+\/prepare$/.test(url.pathname)) {
      return new Response('data: {"type":"ok"}\n\n', { headers: { "Content-Type": "text/event-stream" } })
    }
    if (req.method === "GET" && url.pathname === "/health") return Response.json({ version: this.version, apiVersion: "2" })
    if (req.method === "GET" && url.pathname === "/capabilities") {
      return Response.json({ audioFormats: ["audio/wav"], partialTranscripts: false, serviceCapture: this.mode === "app", streams: true, developerControl: false, control: this.mode === "app" })
    }
    if (req.method === "GET" && url.pathname === "/models") return this.listModels(url)
    return code(404, "not_found")
  }

  private languageOf(url: URL): string | Response {
    const languages = url.searchParams.getAll("language")
    if (languages.length > 1 || url.searchParams.getAll("model").length > 1) return code(400, "invalid-request")
    return languages[0] ?? "en"
  }

  private supports(model: MockHexModel, language: string): boolean {
    return model.languages.includes("*") || model.languages.includes(language)
  }

  private listModels(url: URL): Response {
    const language = this.languageOf(url)
    if (language instanceof Response) return language
    if (!KNOWN_LANGUAGES.has(language)) return code(400, "unsupported-language")
    return Response.json(this.models.map((m) => ({ managed: false, source: m.installed ? "hex" : "download", ...m })))
  }

  private transcribe(url: URL, rec: HexRecordedRequest, body: Uint8Array): Response {
    const type = (rec.headers["content-type"] ?? "").split(";")[0]?.trim().toLowerCase()
    if (type !== "audio/wav") return code(415, "unsupported-audio")
    if (rec.headers["transfer-encoding"] !== undefined) return empty(400)
    const length = rec.headers["content-length"]
    if (length === undefined) return code(411, "length-required")
    if (Number(length) > MAX_AUDIO_BYTES) return code(413, "resource-exhausted")
    const language = this.languageOf(url)
    if (language instanceof Response) return language
    const model = this.models.find((m) => m.id === url.searchParams.get("model"))
    if (model === undefined) return code(400, "unknown-model")
    if (!KNOWN_LANGUAGES.has(language) || !this.supports(model, language)) return code(400, "unsupported-model")
    if (!model.verified) return code(409, "model-not-ready")
    if (this.queueFull) return code(429, "queue-full")
    const durationMs = wavDurationMs(body)
    if (durationMs === null) return code(400, "invalid-audio")
    const transcript = typeof this.transcript === "function" ? this.transcript(body) : this.transcript
    return Response.json({ transcript, durationMs })
  }
}

/**
 * A ProcReader whose /proc/net/tcp shows one 127.0.0.1 LISTEN socket owned by `uid` on the port written to
 * `portFile` by the standalone embedded mock (read at each call, so the port is known only once it started).
 */
export function portFileProcReader(portFile: string, uid: number): ProcReader & { reads: number } {
  const header = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode"
  const reader = async (path: "/proc/net/tcp" | "/proc/net/tcp6"): Promise<string> => {
    reader.reads += 1
    if (path === "/proc/net/tcp6") return `${header}\n`
    const port = Number((await Bun.file(portFile).text()).trim())
    return `${header}\n${procNetTcpLine(0, "0100007F", port, uid)}\n`
  }
  reader.reads = 0
  return reader
}

// ---------------------------------------------------------------------------------------------------------
// Standalone: the fake `hex` script
// ---------------------------------------------------------------------------------------------------------

async function waitForStdinEnd(): Promise<void> {
  const reader = Bun.stdin.stream().getReader()
  try {
    for (;;) {
      const { done } = await reader.read()
      if (done) return
    }
  } catch {
    // stdin closed with an error: the lease is gone either way
  }
}

async function runEmbedded(): Promise<void> {
  const supportDir = process.env.HEX_APPLICATION_SUPPORT_DIR
  if (supportDir === undefined || supportDir === "") {
    process.stderr.write("mock-hex: HEX_APPLICATION_SUPPORT_DIR must be set\n")
    process.exit(2)
  }
  const mock = await MockHex.start({ mode: "embedded", supportDir })
  await writeFile(join(supportDir, MOCK_HEX_PORT_FILE), `${mock.port}\n`, { mode: 0o600 })
  process.stdout.write(mock.readyLine(process.pid))
  await waitForStdinEnd()
  await mock.stop()
  process.exit(0)
}

if (import.meta.main) {
  const args = process.argv.slice(2)
  if (args.length === 1 && args[0] === "--version") {
    process.stdout.write(`hex ${MOCK_HEX_VERSION}\n`)
  } else if (args.length === 2 && args[0] === "service" && args[1] === "--embedded") {
    await runEmbedded()
  } else {
    process.stderr.write("usage: bun test/mocks/mock-hex.ts --version | service --embedded\n")
    process.exit(2)
  }
}
