/**
 * Minimal client for Hex's local API, API version "2" (design 5.3.2, 5.3.3; hex:src/local_api.rs:576-728,
 * :939-1080; hex:src/local_api/http.rs).
 *
 * - Only http://127.0.0.1:<port> is ever contacted; the URL is rebuilt from the port, never taken verbatim.
 * - Before every request: the proxy environment must be neutral (Bun's fetch would send loopback requests and the
 *   bearer token through HTTP_PROXY, 3.11.4; fail closed), then assertListenerOwner checks that the listening
 *   socket belongs to this user (3.8.1, SEC-01). A port another uid holds never sees the token or any audio.
 * - Every route takes `Authorization: Bearer hex_<64 hex>` (hex:src/local_api.rs:577-582). Bodies are complete
 *   Uint8Arrays, so fetch sends Content-Length; Hex refuses chunked bodies. Redirects are refused.
 * - Responses are read with byte caps; error bodies contribute only a sanitized `code`. Messages never contain the
 *   token, a query string, a transcript or audio.
 * - Timeouts run on the injected Clock (ManualClock in tests): /health 300 ms, /models 2 s, /transcriptions 90 s.
 */
import type { Clock } from "@opentui/core"
import type { EnvSource } from "../../config/paths.ts"
import { isTransportError } from "../../transport/errors.ts"
import { activeProxyVariable, type FetchLike } from "../../transport/gateway.ts"
import { assertListenerOwner, readProcNet, type ProcReader } from "../../transport/listener-owner.ts"
import { abortReason, cancelledError, isVoiceError, VoiceError, type VoiceErrorKind } from "../errors.ts"
import type { VoiceSourceId } from "../types.ts"
import type { HexEndpoint } from "./hex-locate.ts"

export const HEX_HOST = "127.0.0.1"
export const HEX_API_VERSION = "2"
/** Upload cap of /transcriptions (hex:src/local_api.rs:40). */
export const HEX_MAX_WAV_BYTES = 64 * 1024 * 1024
/** The TUI dictates English; Hex defaults to "en" too (hex:src/transcription/models.rs:164-171). */
export const HEX_LANGUAGE = "en"
export const HEX_DEFAULT_MODEL = "parakeet_unified_en"
export const HEX_TIMEOUTS = Object.freeze({ healthMs: 300, modelsMs: 2_000, transcribeMs: 90_000 })
/** Catalog ids (`parakeet_unified_en`, `custom-<16 hex>`); anything else never reaches a query string. */
export const HEX_MODEL_ID = /^[A-Za-z0-9_-]{1,64}$/

const MAX_JSON_BYTES = 1024 * 1024
const MAX_ERROR_BYTES = 4096
const ERROR_CODE = /^[a-z0-9][a-z0-9_-]{0,63}$/
const VERSION_TEXT = /^[0-9A-Za-z.+-]{1,40}$/
const BASE_URL = /^http:\/\/127\.0\.0\.1:([1-9]\d{0,4})\/?$/

export type HexSourceId = Extract<VoiceSourceId, "hex-app" | "hex-embedded">

export interface HexHttpDeps {
  clock: Clock
  /** Expected owner of the listening socket: the user's own uid (3.8.1). */
  uid: number
  /** Default globalThis.fetch. */
  fetch?: FetchLike
  /** Default readProcNet (/proc/net/tcp and tcp6). */
  readProc?: ProcReader
  /** Checked for proxy variables before every request; default process.env (read at request time). */
  env?: EnvSource
}

export interface HexHealth {
  version: string
  apiVersion: "2"
}

/** One entry of GET /models (hex:src/local_api.rs:1341-1356). */
export interface HexModelInfo {
  id: string
  name: string
  installed: boolean
  verified: boolean
  downloadBytes: number | null
  /** The model's languages plus "auto" when it detects the language; "*" means every language. */
  languages: readonly string[]
  supportsLanguageDetection: boolean
}

export interface HexTranscription {
  transcript: string
  durationMs: number
}

export interface HexTranscribeRequest {
  wav: Uint8Array
  model: string
  language: string
}

/** "http://127.0.0.1:<port>". */
export function hexBaseUrl(port: number): string {
  return `http://${HEX_HOST}:${port}`
}

/** The port of exactly "http://127.0.0.1:<port>" (optional trailing slash), else null. */
export function hexPortOf(baseUrl: string): number | null {
  const match = BASE_URL.exec(baseUrl)
  const port = match ? Number(match[1]) : Number.NaN
  return Number.isInteger(port) && port >= 1 && port <= 65_535 ? port : null
}

function sanitizeCode(value: unknown): string | null {
  return typeof value === "string" && ERROR_CODE.test(value) ? value : null
}

/** A version string safe for the voice panel. */
export function cleanHexVersion(value: unknown): string | null {
  return typeof value === "string" && VERSION_TEXT.test(value) ? value : null
}

function sourceOpts(source: HexSourceId | undefined): { source?: HexSourceId } {
  return source === undefined ? {} : { source }
}

/** Settles with `promise`, or rejects with the abort reason once `signal` aborts. */
export function raceSignal<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted) return Promise.reject(abortReason(signal))
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => reject(abortReason(signal))
    signal.addEventListener("abort", onAbort, { once: true })
    promise.then(
      (value) => {
        signal.removeEventListener("abort", onAbort)
        resolve(value)
      },
      (error: unknown) => {
        signal.removeEventListener("abort", onAbort)
        reject(error)
      },
    )
  })
}

// ---------------------------------------------------------------------------------------------------------
// Error mapping (5.3.3)
// ---------------------------------------------------------------------------------------------------------

function statusTag(status: number, code: string | null): string {
  return code === null ? `HTTP ${status}` : `${code}, HTTP ${status}`
}

const TRANSCRIPTION_ERRORS: ReadonlyMap<number, { kind: VoiceErrorKind; text: string }> = new Map([
  [429, { kind: "busy", text: "Hex is busy" }],
  [413, { kind: "too_large", text: "the recording is too large for Hex" }],
  [415, { kind: "unsupported_audio", text: "Hex refused the audio format" }],
  [400, { kind: "bad_request", text: "Hex rejected the request" }],
  [411, { kind: "bad_request", text: "Hex rejected the request" }],
  [431, { kind: "bad_request", text: "Hex rejected the request" }],
  [408, { kind: "timeout", text: "the upload to Hex timed out" }],
  [504, { kind: "timeout", text: "Hex timed out" }],
  [500, { kind: "processing_failed", text: "Hex could not transcribe the recording" }],
  [503, { kind: "unavailable", text: "the Hex service is going away" }],
  [401, { kind: "unavailable", text: "Hex rejected the token" }],
])

/**
 * A non-2xx answer of POST /transcriptions as a VoiceError: 409 model-not-ready -> model_not_ready; 429
 * queue-full -> busy; 413 -> too_large; 415 -> unsupported_audio; 400 invalid-audio, unknown-model,
 * unsupported-model, invalid-request -> bad_request; 408 -> timeout; 500 -> processing_failed; 503 service-gone
 * -> unavailable; 401 -> unavailable with status 401 (the source re-locates once first). Other statuses are
 * `unavailable`, so an unexpected answer moves on to the next source and keeps the clip.
 */
export function hexTranscriptionError(status: number, code: string | null, source?: HexSourceId): VoiceError {
  const opts = { status, ...(code === null ? {} : { code }), ...sourceOpts(source) }
  const tag = statusTag(status, code)
  if (status === 409) {
    return code === "model-not-ready" || code === null
      ? new VoiceError("model_not_ready", `the Hex model is not ready (${tag})`, opts)
      : new VoiceError("unavailable", `Hex cancelled the transcription (${tag})`, opts)
  }
  const known = TRANSCRIPTION_ERRORS.get(status)
  if (known !== undefined) return new VoiceError(known.kind, `${known.text} (${tag})`, opts)
  if (status >= 500) return new VoiceError("processing_failed", `Hex failed (${tag})`, opts)
  return new VoiceError("unavailable", `Hex answered ${tag}`, opts)
}

/**
 * Connection-level failures: nothing listens, the listener owner is wrong or unverifiable, or the connection was
 * refused. The endpoint is stale (or the embedded child broken), so callers forget it.
 */
export function isHexConnectionFailure(error: VoiceError): boolean {
  const code = error.opts.code ?? ""
  return code === "unreachable" || code.startsWith("listener_")
}

// ---------------------------------------------------------------------------------------------------------
// Model choice (5.3.2, 5.3.3)
// ---------------------------------------------------------------------------------------------------------

export function hexInstallCommand(model: string = HEX_DEFAULT_MODEL, language: string = HEX_LANGUAGE): string {
  return `hex model install ${model} --language ${language}`
}

function supportsLanguage(model: HexModelInfo, language: string): boolean {
  return model.languages.includes("*") || model.languages.includes(language)
}

/** Ready means installed && verified (5.3.2). */
export function isHexModelReady(model: HexModelInfo, language: string = HEX_LANGUAGE): boolean {
  return model.installed && model.verified && supportsLanguage(model, language)
}

export type HexModelChoice =
  | { ok: true; model: HexModelInfo }
  | { ok: false; kind: "unavailable" | "model_not_ready"; reason: string }

/**
 * `configured` (voice.hex.model) when set, else the first ready entry. Nothing is ever prepared or downloaded
 * (no POST /models/{id}/prepare): a missing model yields the install command instead. A configured model that is
 * unknown or wrong for the language is `unavailable`, so a configuration slip never discards a recording.
 */
export function pickHexModel(models: readonly HexModelInfo[], configured: string, language: string = HEX_LANGUAGE): HexModelChoice {
  const wanted = configured.trim()
  if (wanted === "") {
    const ready = models.find((model) => isHexModelReady(model, language))
    return ready
      ? { ok: true, model: ready }
      : { ok: false, kind: "unavailable", reason: `no verified Hex model: run \`${hexInstallCommand(HEX_DEFAULT_MODEL, language)}\`` }
  }
  if (!HEX_MODEL_ID.test(wanted)) return { ok: false, kind: "unavailable", reason: "voice.hex.model is not a Hex model id" }
  const model = models.find((entry) => entry.id === wanted)
  if (model === undefined) return { ok: false, kind: "unavailable", reason: `Hex does not offer the model ${wanted} (voice.hex.model)` }
  if (!supportsLanguage(model, language)) return { ok: false, kind: "unavailable", reason: `the Hex model ${wanted} does not support "${language}"` }
  if (!model.installed || !model.verified) {
    return { ok: false, kind: "model_not_ready", reason: `the Hex model ${wanted} is not installed: run \`${hexInstallCommand(wanted, language)}\`` }
  }
  return { ok: true, model }
}

// ---------------------------------------------------------------------------------------------------------
// Response validation
// ---------------------------------------------------------------------------------------------------------

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : null
}

export function parseHexHealth(value: unknown): HexHealth | null {
  const input = record(value)
  if (input === null || input.apiVersion !== HEX_API_VERSION) return null
  const version = cleanHexVersion(input.version)
  return version === null ? null : { version, apiVersion: HEX_API_VERSION }
}

function parseModel(value: unknown): HexModelInfo | null {
  const input = record(value)
  if (input === null || typeof input.id !== "string" || !HEX_MODEL_ID.test(input.id)) return null
  if (typeof input.installed !== "boolean" || typeof input.verified !== "boolean") return null
  const languages = Array.isArray(input.languages) ? input.languages.filter((l): l is string => typeof l === "string") : []
  const bytes = input.downloadBytes
  return {
    id: input.id,
    name: typeof input.name === "string" ? input.name.slice(0, 80) : input.id,
    installed: input.installed,
    verified: input.verified,
    downloadBytes: typeof bytes === "number" && Number.isFinite(bytes) && bytes >= 0 ? bytes : null,
    languages,
    supportsLanguageDetection: input.supportsLanguageDetection === true,
  }
}

/** The catalog; malformed entries are skipped. null when the answer is not an array. */
export function parseHexModels(value: unknown): HexModelInfo[] | null {
  if (!Array.isArray(value)) return null
  return value.map(parseModel).filter((model): model is HexModelInfo => model !== null)
}

export function parseHexTranscription(value: unknown): HexTranscription | null {
  const input = record(value)
  if (input === null || typeof input.transcript !== "string") return null
  const duration = input.durationMs
  return {
    transcript: input.transcript,
    durationMs: typeof duration === "number" && Number.isFinite(duration) && duration >= 0 ? duration : 0,
  }
}

// ---------------------------------------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------------------------------------

interface RequestSpec {
  method: "GET" | "POST"
  path: string
  query?: Record<string, string>
  body?: Uint8Array
  timeoutMs: number
  /** For messages, e.g. "GET /health". */
  label: string
}

interface HexReply {
  status: number
  /** Parsed JSON of a 2xx answer. */
  json: unknown
  /** Sanitized `code` of an error answer. */
  code: string | null
}

async function readCapped(res: Response, max: number): Promise<{ bytes: Uint8Array; overflow: boolean }> {
  const body = res.body
  if (!body) return { bytes: new Uint8Array(0), overflow: false }
  const reader = body.getReader()
  const chunks: Uint8Array[] = []
  let total = 0
  let overflow = false
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    chunks.push(value)
    total += value.byteLength
    if (total > max) {
      overflow = true
      await reader.cancel().catch(() => undefined)
      break
    }
  }
  const out = new Uint8Array(Math.min(total, max))
  let offset = 0
  for (const chunk of chunks) {
    const part = chunk.subarray(0, Math.max(0, out.length - offset))
    out.set(part, offset)
    offset += part.byteLength
  }
  return { bytes: out, overflow }
}

/** fetch takes ArrayBuffer-backed views only; the recorder's WAV always is one, anything else is copied. */
function bodyBytes(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  return bytes.buffer instanceof ArrayBuffer ? (bytes as Uint8Array<ArrayBuffer>) : new Uint8Array(bytes)
}

function errnoDetail(error: unknown): string {
  const code = typeof error === "object" && error !== null ? (error as { code?: unknown }).code : undefined
  return typeof code === "string" && /^[A-Za-z_]{1,40}$/.test(code) ? ` (${code})` : ""
}

/** HTTP client bound to one endpoint. Every method rejects with VoiceError only. */
export class HexClient {
  readonly port: number
  private readonly fetchImpl: FetchLike
  private readonly readProc: ProcReader

  constructor(
    private readonly endpoint: HexEndpoint,
    private readonly deps: HexHttpDeps,
    private readonly source?: HexSourceId,
  ) {
    const port = hexPortOf(endpoint.baseUrl)
    if (port === null) throw this.error("unavailable", "the Hex endpoint is not http://127.0.0.1:<port>; refused")
    this.port = port
    this.fetchImpl = deps.fetch ?? ((input, init) => fetch(input, init))
    this.readProc = deps.readProc ?? readProcNet
  }

  /** GET /health: {version, apiVersion: "2"}. */
  async health(signal?: AbortSignal): Promise<HexHealth> {
    const reply = await this.request({ method: "GET", path: "/health", timeoutMs: HEX_TIMEOUTS.healthMs, label: "GET /health" }, signal)
    if (reply.status === 401) throw this.error("unavailable", "Hex rejected the token (HTTP 401)", { status: 401 })
    if (reply.status !== 200) throw this.error("unavailable", `Hex /health answered ${statusTag(reply.status, reply.code)}`, { status: reply.status })
    const health = parseHexHealth(reply.json)
    if (health === null) throw this.error("unavailable", "Hex answered /health without API version 2")
    return health
  }

  /** GET /models?language=<language>: readiness metadata only; nothing is downloaded. */
  async models(language: string = HEX_LANGUAGE, signal?: AbortSignal): Promise<HexModelInfo[]> {
    const spec: RequestSpec = { method: "GET", path: "/models", query: { language }, timeoutMs: HEX_TIMEOUTS.modelsMs, label: "GET /models" }
    const reply = await this.request(spec, signal)
    if (reply.status === 401) throw this.error("unavailable", "Hex rejected the token (HTTP 401)", { status: 401 })
    if (reply.status !== 200) throw this.error("unavailable", `Hex /models answered ${statusTag(reply.status, reply.code)}`, { status: reply.status })
    const models = parseHexModels(reply.json)
    if (models === null) throw this.error("unavailable", "Hex sent an invalid model list")
    return models
  }

  /** POST /transcriptions?model=&language= with the WAV the TUI recorded (Content-Type audio/wav). */
  async transcribe(req: HexTranscribeRequest, signal?: AbortSignal): Promise<HexTranscription> {
    if (!HEX_MODEL_ID.test(req.model)) throw this.error("unavailable", "the Hex model id is invalid")
    if (req.wav.byteLength > HEX_MAX_WAV_BYTES) throw this.error("too_large", "the recording is larger than Hex's 64 MiB upload limit")
    const spec: RequestSpec = {
      method: "POST",
      path: "/transcriptions",
      query: { model: req.model, language: req.language },
      body: req.wav,
      timeoutMs: HEX_TIMEOUTS.transcribeMs,
      label: "POST /transcriptions",
    }
    const reply = await this.request(spec, signal)
    if (reply.status !== 200) throw hexTranscriptionError(reply.status, reply.code, this.source)
    const result = parseHexTranscription(reply.json)
    if (result === null) throw this.error("unavailable", "Hex sent an invalid transcription answer")
    return result
  }

  private error(kind: VoiceErrorKind, message: string, extra: { status?: number; code?: string } = {}): VoiceError {
    return new VoiceError(kind, message, { ...extra, ...sourceOpts(this.source) })
  }

  private url(spec: RequestSpec): string {
    const query = spec.query ? `?${new URLSearchParams(spec.query).toString()}` : ""
    return `${hexBaseUrl(this.port)}${spec.path}${query}`
  }

  /** Fails closed when a proxy variable is active, then checks the listener owner (SEC-01). */
  private async preflight(): Promise<void> {
    const proxy = activeProxyVariable(this.deps.env ?? process.env)
    if (proxy !== null) throw this.error("unavailable", `${proxy} is set: Hex requests would go through a proxy; refused`)
    try {
      await assertListenerOwner(new URL(hexBaseUrl(this.port)), { uid: this.deps.uid, label: "this user" }, this.readProc)
    } catch (error) {
      if (isTransportError(error)) {
        const message = error.code === "unreachable" ? `not running (${error.message})` : error.message
        throw this.error("unavailable", message, { code: `listener_${error.code}` })
      }
      throw this.error("unavailable", "cannot verify the owner of the Hex port")
    }
  }

  private async request(spec: RequestSpec, signal?: AbortSignal): Promise<HexReply> {
    if (signal?.aborted) throw abortReason(signal)
    await this.preflight()
    if (signal?.aborted) throw abortReason(signal)
    const timeout = new AbortController()
    const seconds = spec.timeoutMs >= 1000 ? `${spec.timeoutMs / 1000} s` : `${spec.timeoutMs} ms`
    const timer = this.deps.clock.setTimeout(
      () => timeout.abort(this.error("timeout", `Hex did not answer ${spec.label} within ${seconds}`)),
      spec.timeoutMs,
    )
    const combined = signal ? AbortSignal.any([timeout.signal, signal]) : timeout.signal
    try {
      const headers = new Headers({ Accept: "application/json", Authorization: `Bearer ${this.endpoint.token.expose()}` })
      if (spec.body) headers.set("Content-Type", "audio/wav")
      const body = spec.body === undefined ? undefined : bodyBytes(spec.body)
      const res = await this.fetchImpl(this.url(spec), { method: spec.method, headers, body, redirect: "error", signal: combined })
      return await this.reply(res, spec)
    } catch (error) {
      throw this.failure(error, spec, signal, timeout.signal)
    } finally {
      this.deps.clock.clearTimeout(timer)
    }
  }

  private async reply(res: Response, spec: RequestSpec): Promise<HexReply> {
    if (res.status >= 300 && res.status < 400) {
      await res.body?.cancel().catch(() => undefined)
      throw this.error("unavailable", `Hex answered ${spec.label} with a redirect; refused`, { status: res.status })
    }
    if (res.status >= 200 && res.status < 300) {
      const { bytes, overflow } = await readCapped(res, MAX_JSON_BYTES)
      if (overflow) throw this.error("unavailable", `Hex sent an oversized answer to ${spec.label}`)
      try {
        return { status: res.status, json: JSON.parse(new TextDecoder().decode(bytes)) as unknown, code: null }
      } catch {
        throw this.error("unavailable", `Hex sent invalid JSON for ${spec.label}`)
      }
    }
    const { bytes } = await readCapped(res, MAX_ERROR_BYTES)
    let code: string | null = null
    try {
      code = sanitizeCode(record(JSON.parse(new TextDecoder().decode(bytes)) as unknown)?.code)
    } catch {
      code = null
    }
    return { status: res.status, json: null, code }
  }

  private failure(error: unknown, spec: RequestSpec, caller: AbortSignal | undefined, timeout: AbortSignal): VoiceError {
    if (caller?.aborted) return isVoiceError(caller.reason) ? caller.reason : cancelledError(undefined, this.source)
    if (timeout.aborted && isVoiceError(timeout.reason)) return timeout.reason
    if (isVoiceError(error)) return error
    const code = typeof error === "object" && error !== null ? (error as { code?: unknown }).code : undefined
    if (code === "UnexpectedRedirect") return this.error("unavailable", `Hex answered ${spec.label} with a redirect; refused`)
    return this.error("unavailable", `cannot reach Hex on ${HEX_HOST}:${this.port}${errnoDetail(error)}`, { code: "unreachable" })
  }
}
