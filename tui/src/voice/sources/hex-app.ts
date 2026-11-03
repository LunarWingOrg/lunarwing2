/**
 * The `hex-app` source (design 5.3.3, M1b): the TUI records, the running Hex app transcribes the in-memory WAV
 * through POST /transcriptions on its local API. Hex never records here (its /dictations and CLI are deferred,
 * 5.3.4) and no model is ever prepared or downloaded.
 *
 * - Probe: the app located by hex-locate.ts (discovery file, pid, listener owner, /health) and a ready model in
 *   GET /models?language=en (voice.hex.model, or the first installed && verified entry). Side-effect free.
 * - Transcribe: POST /transcriptions?model=<id>&language=en, Content-Type audio/wav, Content-Length, bearer
 *   token; client timeout 90 s. A 401 forgets the discovery cache and re-locates once (Hex may have restarted
 *   with a new token), then the source is `unavailable`. An empty transcript is no_speech.
 * - The discovery file's changes (5 s poll) hint the manager to re-probe (AvailabilityHintSource).
 */
import type { Clock } from "@opentui/core"
import type { VoiceConfig } from "../../config/types.ts"
import { cancelledError, isVoiceError, toVoiceError, VoiceError } from "../errors.ts"
import type { SourceAvailability, Transcript, VoiceSource, WavClip } from "../types.ts"
import {
  HEX_LANGUAGE,
  HEX_MAX_WAV_BYTES,
  HexClient,
  hexPortOf,
  isHexConnectionFailure,
  pickHexModel,
  raceSignal,
  type HexHttpDeps,
  type HexModelInfo,
} from "./hex-http.ts"
import {
  hexHttpDeps,
  hexLocatorFor,
  resolveHexOptions,
  type HexEndpoint,
  type HexLocator,
  type HexSourceDeps,
} from "./hex-locate.ts"

export const HEX_APP_ID = "hex-app" as const
export const HEX_APP_LABEL = "Hex (running app)"
export const HEX_APP_PRIVACY_NOTE =
  "audio goes to the Hex app on this machine (127.0.0.1 only); /transcriptions keeps no audio, history or transcript (Hex writes its process.log)"
/** GET /models answers are reused this long (one probe plus the transcription that follows). */
export const HEX_MODELS_CACHE_MS = 5_000

export interface HexAppSource extends VoiceSource {
  readonly id: typeof HEX_APP_ID
  /** AvailabilityHintSource (voice/manager.ts): fires when the discovery file changes. */
  onAvailabilityHint(listener: () => void): () => void
}

/** A GET /models answer of one endpoint. */
export class HexModelCache {
  private entry: { key: string; at: number; models: HexModelInfo[] } | null = null

  constructor(private readonly clock: Clock) {}

  async get(client: HexClient, endpoint: HexEndpoint, signal: AbortSignal): Promise<HexModelInfo[]> {
    const key = `${endpoint.baseUrl}#${endpoint.pid}`
    const now = this.clock.now()
    if (this.entry?.key === key && now - this.entry.at < HEX_MODELS_CACHE_MS) return this.entry.models
    const models = await client.models(HEX_LANGUAGE, signal)
    this.entry = { key, at: this.clock.now(), models }
    return models
  }

  clear(): void {
    this.entry = null
  }
}

function availabilityDetail(endpoint: HexEndpoint, model: string): string {
  const port = hexPortOf(endpoint.baseUrl) ?? 0
  const version = endpoint.version ? `Hex ${endpoint.version}` : "Hex"
  return `${version} on 127.0.0.1:${port} (pid ${endpoint.pid}), model ${model}`
}

class DefaultHexAppSource implements HexAppSource {
  readonly id = HEX_APP_ID
  readonly label = HEX_APP_LABEL
  readonly privacyNote = HEX_APP_PRIVACY_NOTE
  private readonly locator: HexLocator
  private readonly http: HexHttpDeps | null
  private readonly models: HexModelCache
  private readonly hints = new Set<() => void>()
  private disposed = false

  constructor(
    private readonly clock: Clock,
    private readonly config: VoiceConfig,
    deps: HexSourceDeps,
  ) {
    this.locator = hexLocatorFor(deps)
    this.http = hexHttpDeps(resolveHexOptions(clock, deps.hex))
    this.models = new HexModelCache(clock)
  }

  async probe(signal: AbortSignal): Promise<SourceAvailability> {
    const unavailable = (reason: string): SourceAvailability => ({ available: false, reason, checkedAt: this.clock.now() })
    if (this.disposed) return unavailable("disposed")
    try {
      const { endpoint, client } = await this.connect(signal)
      const choice = pickHexModel(await this.models.get(client, endpoint, signal), this.config.hex.model)
      if (!choice.ok) return unavailable(choice.reason)
      return { available: true, detail: availabilityDetail(endpoint, choice.model.id), checkedAt: this.clock.now() }
    } catch (error) {
      if (!isVoiceError(error)) return unavailable("the probe failed")
      this.forgetStale(error)
      return unavailable(error.message)
    }
  }

  maxWavBytes(): number {
    return HEX_MAX_WAV_BYTES
  }

  async transcribe(clip: WavClip, signal: AbortSignal): Promise<Transcript> {
    if (signal.aborted) throw cancelledError(undefined, HEX_APP_ID)
    if (this.disposed) throw new VoiceError("unavailable", "disposed", { source: HEX_APP_ID })
    const started = this.clock.now()
    try {
      return await this.attempt(clip, signal, started, true)
    } catch (error) {
      if (signal.aborted) throw cancelledError(undefined, HEX_APP_ID)
      throw toVoiceError(error, HEX_APP_ID, "unavailable")
    }
  }

  onAvailabilityHint(listener: () => void): () => void {
    if (!this.config.enabled || this.disposed) return () => undefined
    const off = this.locator.onDiscoveryChange(listener)
    this.hints.add(off)
    return () => {
      if (this.hints.delete(off)) off()
    }
  }

  async dispose(): Promise<void> {
    this.disposed = true
    for (const off of [...this.hints]) off()
    this.hints.clear()
    this.models.clear()
  }

  private async connect(signal: AbortSignal): Promise<{ endpoint: HexEndpoint; client: HexClient }> {
    if (this.http === null) throw new VoiceError("unavailable", "needs a POSIX system (no getuid)", { source: HEX_APP_ID })
    const located = await raceSignal(this.locator.locateApp(signal), signal)
    if ("unavailable" in located) throw new VoiceError("unavailable", located.unavailable, { source: HEX_APP_ID })
    return { endpoint: located, client: new HexClient(located, this.http, HEX_APP_ID) }
  }

  private async attempt(clip: WavClip, signal: AbortSignal, started: number, mayRelocate: boolean): Promise<Transcript> {
    const { endpoint, client } = await this.connect(signal)
    try {
      const choice = pickHexModel(await this.models.get(client, endpoint, signal), this.config.hex.model)
      if (!choice.ok) throw new VoiceError(choice.kind, choice.reason, { source: HEX_APP_ID })
      const result = await client.transcribe({ wav: clip.wav, model: choice.model.id, language: HEX_LANGUAGE }, signal)
      const text = result.transcript.trim()
      if (text === "") throw new VoiceError("no_speech", "no speech detected", { source: HEX_APP_ID })
      return {
        text,
        source: HEX_APP_ID,
        model: choice.model.id,
        language: HEX_LANGUAGE,
        audioMs: clip.durationMs,
        elapsedMs: this.clock.now() - started,
      }
    } catch (error) {
      if (!isVoiceError(error) || signal.aborted) throw error
      this.forgetStale(error)
      if (error.opts.status !== 401) throw error
      // Hex restarted with a new token, or the file is stale: read the discovery file again, once.
      if (mayRelocate) return this.attempt(clip, signal, started, false)
      throw new VoiceError("unavailable", "Hex rejected the token (HTTP 401)", { source: HEX_APP_ID, status: 401 })
    }
  }

  /** A 401 or a dead connection means the located endpoint is stale: the next use reads the discovery file again. */
  private forgetStale(error: VoiceError): void {
    if (error.opts.status !== 401 && !isHexConnectionFailure(error)) return
    this.locator.invalidate()
    this.models.clear()
  }
}

export function createHexAppSource(deps: HexSourceDeps): HexAppSource {
  return new DefaultHexAppSource(deps.clock, deps.config, deps)
}
