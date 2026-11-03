/**
 * The push-to-talk state machine (design section 7). Pure: no clock, no I/O; every event carries its time and
 * the controller (voice/manager.ts) feeds it key and mouse input, a 50 ms tick while not idle, and the
 * recorder's capture events, then executes the effects. It covers capture only: transcription runs in the
 * controller's queue, so a new recording can start while earlier clips are transcribed.
 *
 * Inputs differ per terminal (7.1): kitty with KKP reports releases and flags repeats; kitty inside tmux never
 * reports releases and sends repeats as plain presses every 20 ms after the repeat delay. A press is fresh when
 * it is not flagged as a repeat and the previous PTT key event is at least `repeatQuietMs` old.
 */
import type { VoiceConfig } from "../config/types.ts"
import type { PttMode, PttPhase } from "./types.ts"

export type { PttMode, PttPhase } from "./types.ts"

export type PttTrigger = "key" | "mouse"

export interface PttConfig {
  /** 300: a shorter press or click is a tap (toggle). */
  holdThresholdMs: number
  /** 600 default; 300 matches Hyprland repeat_delay on this host. */
  repeatDelayMs: number
  /** 250: no repeat for this long means the key was released (repeat interval 20 ms). */
  repeatQuietMs: number
  /** 300 */
  minClipMs: number
  /** 120000 default, hard cap 290000. */
  maxDurationMs: number
  blurPolicy: "hold" | "always" | "never"
  /** Toggle modes only; default false (UX-06). */
  autoStopOnSilence: boolean
  /** 8000: a toggle recording with no frame above -50 dBFS is cancelled. */
  noSignalTimeoutMs: number
}

export interface PttState {
  phase: PttPhase
  mode: PttMode | null
  trigger: PttTrigger | null
  pressAt: number | null
  lastKeyAt: number | null
  repeats: number
  /** A KKP release for a PTT key was seen in this process. */
  releaseConfirmed: boolean
  startedAt: number | null
  warnedNearMax: boolean
}

export type PttEvent =
  | { type: "key_press"; repeated: boolean; at: number }
  | { type: "key_release"; at: number }
  | { type: "mouse_down"; at: number }
  | { type: "mouse_up"; at: number }
  | { type: "tick"; at: number }
  | { type: "escape"; at: number }
  | { type: "blur"; at: number }
  | { type: "capture_started"; at: number }
  | { type: "capture_failed"; at: number }
  | { type: "silence"; at: number }
  | { type: "no_signal_timeout"; at: number }
  | { type: "finished"; at: number }

export type PttEffect =
  | { type: "start_capture"; trigger: PttTrigger }
  | {
      type: "finish_capture"
      reason: "toggle" | "release" | "release_inferred" | "silence" | "max_duration" | "blur" | "stuck"
    }
  | { type: "cancel_capture"; reason: "escape" | "too_short" | "no_signal" | "capture_failed" }
  | { type: "notice"; level: "info" | "warn"; text: string }

export type PttFinishReason = Extract<PttEffect, { type: "finish_capture" }>["reason"]
export type PttCancelReason = Extract<PttEffect, { type: "cancel_capture" }>["reason"]

export interface PttResult {
  state: PttState
  effects: PttEffect[]
}

/** The "10 s left" warning comes this long before the maximum duration. */
export const NEAR_MAX_WARNING_MS = 10_000

export const DEFAULT_PTT_CONFIG: Readonly<PttConfig> = Object.freeze({
  holdThresholdMs: 300,
  repeatDelayMs: 600,
  repeatQuietMs: 250,
  minClipMs: 300,
  maxDurationMs: 120_000,
  blurPolicy: "hold",
  autoStopOnSilence: false,
  noSignalTimeoutMs: 8_000,
})

export const INITIAL_PTT_STATE: Readonly<PttState> = Object.freeze({
  phase: "idle",
  mode: null,
  trigger: null,
  pressAt: null,
  lastKeyAt: null,
  repeats: 0,
  releaseConfirmed: false,
  startedAt: null,
  warnedNearMax: false,
})

export function createPttState(): PttState {
  return { ...INITIAL_PTT_STATE }
}

/** PttConfig from [voice] (3.11.2); `maxDurationMs` is the effective cap of one recording (5.5). */
export function pttConfigFromVoice(
  voice: Pick<VoiceConfig, "repeatDelayMs" | "maxDurationMs" | "blurPolicy" | "autoStopOnSilence">,
  maxDurationMs: number = voice.maxDurationMs,
): PttConfig {
  return {
    ...DEFAULT_PTT_CONFIG,
    repeatDelayMs: voice.repeatDelayMs,
    maxDurationMs,
    blurPolicy: voice.blurPolicy,
    autoStopOnSilence: voice.autoStopOnSilence,
  }
}

export function pttReduce(s: PttState, e: PttEvent, cfg: PttConfig): PttResult {
  switch (s.phase) {
    case "idle":
      return reduceIdle(s, e, cfg)
    case "arming":
    case "recording":
      return reduceActive(s, e, cfg)
    case "finishing":
      return reduceFinishing(s, e)
  }
}

// ---- helpers ----

function keep(state: PttState): PttResult {
  return { state, effects: [] }
}

function isFresh(s: PttState, e: { repeated: boolean; at: number }, cfg: PttConfig): boolean {
  return !e.repeated && (s.lastKeyAt === null || e.at - s.lastKeyAt >= cfg.repeatQuietMs)
}

/** Back to idle; lastKeyAt and releaseConfirmed survive, so a key still held after Esc stays a repeat. */
function toIdle(s: PttState): PttState {
  return { ...s, phase: "idle", mode: null, trigger: null, pressAt: null, repeats: 0, startedAt: null, warnedNearMax: false }
}

function arm(s: PttState, trigger: PttTrigger, at: number): PttResult {
  const state: PttState = {
    ...s,
    phase: "arming",
    mode: "provisional",
    trigger,
    pressAt: at,
    lastKeyAt: trigger === "key" ? at : s.lastKeyAt,
    repeats: 0,
    startedAt: null,
    warnedNearMax: false,
  }
  return { state, effects: [{ type: "start_capture", trigger }] }
}

function cancel(s: PttState, reason: PttCancelReason, notices: PttEffect[] = []): PttResult {
  return { state: toIdle(s), effects: [{ type: "cancel_capture", reason }, ...notices] }
}

/** Every finish passes the minimum-length rule; a finish decided before the first chunk is always too short. */
function finish(s: PttState, at: number, reason: PttFinishReason, cfg: PttConfig, notices: PttEffect[] = []): PttResult {
  if (s.startedAt === null || at - s.startedAt < cfg.minClipMs) return cancel(s, "too_short")
  return { state: { ...s, phase: "finishing" }, effects: [{ type: "finish_capture", reason }, ...notices] }
}

function repeatWindow(cfg: PttConfig): number {
  return cfg.repeatDelayMs + cfg.repeatQuietMs
}

/**
 * Time-based mode changes of a provisional press, applied before every event so the result does not depend on
 * tick granularity: a key press without repeats becomes a toggle after the repeat window, a mouse press held for
 * the hold threshold becomes hold-mouse.
 */
function promote(s: PttState, at: number, cfg: PttConfig): PttState {
  if (s.mode !== "provisional" || s.pressAt === null) return s
  if (s.trigger === "key" && s.repeats === 0 && at - s.pressAt > repeatWindow(cfg)) return { ...s, mode: "toggle" }
  if (s.trigger === "mouse" && at - s.pressAt >= cfg.holdThresholdMs) return { ...s, mode: "hold-mouse" }
  return s
}

function formatDuration(ms: number): string {
  const total = Math.floor(ms / 1000)
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`
}

// ---- idle ----

function reduceIdle(s: PttState, e: PttEvent, cfg: PttConfig): PttResult {
  switch (e.type) {
    case "key_press":
      // A repeat in idle is a key still held after Esc (or after a refused start): tracked, never a start.
      return isFresh(s, e, cfg) ? arm(s, "key", e.at) : keep({ ...s, lastKeyAt: e.at })
    case "mouse_down":
      return arm(s, "mouse", e.at)
    case "key_release":
      return s.releaseConfirmed ? keep(s) : keep({ ...s, releaseConfirmed: true })
    default:
      return keep(s)
  }
}

// ---- arming and recording (shared input rules) ----

function reduceActive(s: PttState, e: PttEvent, cfg: PttConfig): PttResult {
  switch (e.type) {
    case "capture_started":
      return s.phase === "arming" ? keep({ ...s, phase: "recording", startedAt: e.at }) : keep(s)
    case "capture_failed":
      return cancel(s, "capture_failed")
    case "escape":
      return cancel(s, "escape", [{ type: "notice", level: "info", text: "dictation cancelled" }])
    case "tick":
      return onTick(promote(s, e.at, cfg), e.at, cfg)
    case "blur":
      return onBlur(promote(s, e.at, cfg), e.at, cfg)
    case "silence":
      return s.mode === "toggle" && cfg.autoStopOnSilence ? finish(s, e.at, "silence", cfg) : keep(s)
    case "no_signal_timeout":
      return s.mode === "toggle" && s.phase === "recording" ? cancel(s, "no_signal") : keep(s)
    case "key_press":
      return onKeyPress(promote(s, e.at, cfg), e, cfg)
    case "key_release":
      return onKeyRelease(promote(s, e.at, cfg), e.at, cfg)
    case "mouse_down":
      return onMouseDown(promote(s, e.at, cfg), e.at, cfg)
    case "mouse_up":
      return onMouseUp(promote(s, e.at, cfg), e.at, cfg)
    case "finished":
      return keep(s)
  }
}

function onKeyPress(s: PttState, e: { repeated: boolean; at: number }, cfg: PttConfig): PttResult {
  const fresh = isFresh(s, e, cfg)
  const tracked: PttState = { ...s, lastKeyAt: e.at }
  if (s.mode === "toggle") return fresh ? finish(s, e.at, "toggle", cfg) : keep(tracked)
  if (s.mode === "provisional" && s.trigger === "key") {
    // The first autorepeat arrives after the repeat delay as a plain press inside tmux: within the window it is a
    // repeat (a hold), never a stop tap.
    const withinWindow = s.pressAt !== null && e.at - s.pressAt <= repeatWindow(cfg)
    if (!e.repeated && !withinWindow) return finish(s, e.at, "toggle", cfg)
    return keep({ ...tracked, repeats: s.repeats + 1, mode: s.releaseConfirmed ? "hold-key" : "hold-inferred" })
  }
  // hold-inferred, hold-key, hold-mouse, and a mouse-provisional press: a fresh key press means a lost release.
  if (fresh) return finish(s, e.at, "stuck", cfg)
  return keep(s.trigger === "key" ? { ...tracked, repeats: s.repeats + 1 } : tracked)
}

function onKeyRelease(s: PttState, at: number, cfg: PttConfig): PttResult {
  const confirmed: PttState = { ...s, releaseConfirmed: true }
  if (s.trigger !== "key") return keep(confirmed)
  if (s.mode === "provisional") {
    const held = s.pressAt === null ? 0 : at - s.pressAt
    return held >= cfg.holdThresholdMs ? finish(confirmed, at, "release", cfg) : keep({ ...confirmed, mode: "toggle" })
  }
  if (s.mode === "hold-inferred" || s.mode === "hold-key") return finish(confirmed, at, "release", cfg)
  return keep(confirmed)
}

function onMouseDown(s: PttState, at: number, cfg: PttConfig): PttResult {
  // A toggle stops on a click; in any other mode a second press means a release was lost.
  return finish(s, at, s.mode === "toggle" ? "toggle" : "stuck", cfg)
}

function onMouseUp(s: PttState, at: number, cfg: PttConfig): PttResult {
  if (s.trigger !== "mouse") return keep(s)
  if (s.mode === "provisional") return keep({ ...s, mode: "toggle" })
  if (s.mode === "hold-mouse") return finish(s, at, "release", cfg)
  return keep(s)
}

function onTick(s: PttState, at: number, cfg: PttConfig): PttResult {
  if (s.mode === "hold-inferred" && s.lastKeyAt !== null && at - s.lastKeyAt > cfg.repeatQuietMs) {
    return finish(s, at, "release_inferred", cfg)
  }
  if (s.phase !== "recording" || s.startedAt === null) return keep(s)
  const elapsed = at - s.startedAt
  if (elapsed >= cfg.maxDurationMs) {
    const text = `maximum dictation length reached (${formatDuration(cfg.maxDurationMs)}); transcribing`
    return finish(s, at, "max_duration", cfg, [{ type: "notice", level: "info", text }])
  }
  if (!s.warnedNearMax && cfg.maxDurationMs > NEAR_MAX_WARNING_MS && elapsed >= cfg.maxDurationMs - NEAR_MAX_WARNING_MS) {
    return { state: { ...s, warnedNearMax: true }, effects: [{ type: "notice", level: "warn", text: "10 s left" }] }
  }
  return keep(s)
}

function onBlur(s: PttState, at: number, cfg: PttConfig): PttResult {
  if (cfg.blurPolicy === "never") return keep(s)
  if (s.mode === "toggle") return cfg.blurPolicy === "always" ? finish(s, at, "blur", cfg) : keep(s)
  return finish(s, at, "blur", cfg)
}

// ---- finishing: the recorder is flushing (well under 1 s) ----

function reduceFinishing(s: PttState, e: PttEvent): PttResult {
  switch (e.type) {
    case "finished":
      return keep(toIdle(s))
    case "escape":
      return cancel(s, "escape", [{ type: "notice", level: "info", text: "dictation cancelled" }])
    case "capture_failed":
      return cancel(s, "capture_failed")
    case "key_press":
      return keep({ ...s, lastKeyAt: e.at })
    case "key_release":
      return s.releaseConfirmed ? keep(s) : keep({ ...s, releaseConfirmed: true })
    default:
      return keep(s)
  }
}
