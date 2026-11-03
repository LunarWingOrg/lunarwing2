/**
 * The voice sources of this build (design 5.4, 5.7). createVoiceSources() builds one instance per registered
 * factory; the manager orders them by voice.order or /voice use, and lists ids without an implementation here as
 * "not in this build". M1a registered lunarvoice; M1b adds hex-app and hex-embedded (5.3.2, 5.3.3).
 */
import type { Clock } from "@opentui/core"
import type { VoiceConfig } from "../../config/types.ts"
import type { Store } from "../../state/model.ts"
import type { Transport } from "../../transport/types.ts"
import type { VoiceSource } from "../types.ts"
import { createHexAppSource } from "./hex-app.ts"
import { createHexEmbeddedSource } from "./hex-embedded.ts"
import type { HexSourceOptions } from "./hex-locate.ts"
import { createLunarVoiceSource } from "./lunarvoice.ts"

export interface VoiceSourceDeps {
  store: Store
  clock: Clock
  /** The active transport; null before the first connection. */
  transport: () => Transport | null
  config: VoiceConfig
  /**
   * M1b: collaborators of the Hex sources (spawn, child environment, Bun.which, fetch, /proc reader, uid, a shared
   * locator). Omitted in production: every field has its default (bunSpawn, buildChildEnv(), process.getuid(), ...).
   */
  hex?: HexSourceOptions
}

/** Builds one source, or null when it does not apply to this configuration. */
export type VoiceSourceFactory = (deps: VoiceSourceDeps) => VoiceSource | null

/**
 * REGISTRATION POINT. Every voice source of this build, in the default voice.order (which decides the fallback
 * order). Both Hex sources share one HexLocator per deps object (hex-locate.ts hexLocatorFor) and implement
 * `onAvailabilityHint(listener): () => void` (voice/manager.ts AvailabilityHintSource): the manager re-probes them
 * when the 5 s discovery-file check sees a change.
 */
export const VOICE_SOURCE_FACTORIES: readonly VoiceSourceFactory[] = Object.freeze([
  createLunarVoiceSource,
  createHexAppSource,
  createHexEmbeddedSource,
])

/** One instance per factory; a duplicate id keeps the first. Sources are built even with --no-voice (the panel lists them). */
export function createVoiceSources(deps: VoiceSourceDeps): VoiceSource[] {
  const sources: VoiceSource[] = []
  const ids = new Set<string>()
  for (const factory of VOICE_SOURCE_FACTORIES) {
    const source = factory(deps)
    if (source === null || ids.has(source.id)) continue
    ids.add(source.id)
    sources.push(source)
  }
  return sources
}
