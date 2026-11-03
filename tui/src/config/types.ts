/**
 * Shared configuration contracts (design 3.11, 3.8, 3.11.6). Declared by the scaffold task so the transport,
 * voice, image and controller modules compile against one definition before config/* is implemented (F6).
 *
 * Implemented by: config/token-file.ts and config/tenant-access.ts (TokenSource), config/state-file.ts
 * (StateFile), config/child-env.ts (ChildEnv), config/config.ts (UiConfig, VoiceConfig).
 */
import type { Secret } from "./secret.ts"
import type { PendingImage } from "../state/model.ts"
import type { VoiceSourceId } from "../voice/types.ts"

// ---------------------------------------------------------------------------------------------------------
// Tokens (3.8, 3.11.3)
// ---------------------------------------------------------------------------------------------------------

/**
 * Picks the 401 hint (UX-10): tenant access files report their token_kind ("client" or "master"); token files
 * and LUNARWING_GATEWAY_TOKEN report "file".
 */
export type TokenKind = "client" | "master" | "file"

export interface TokenSource {
  /** The current token. Callers expose() it only to build the Authorization header or the auth-token body. */
  current(): Secret
  /**
   * Re-reads the backing file once (401 rotation). True when a different valid token is now current; false when
   * unchanged or unreadable, in which case the previous token stays current.
   */
  reload(): Promise<boolean>
  /** Where the token comes from, e.g. "tenants/derpy.json (client token tui:sun)". Never the token. */
  describe(): string
  kind(): TokenKind
}

// ---------------------------------------------------------------------------------------------------------
// State files (3.11.5, 3.11.6)
// ---------------------------------------------------------------------------------------------------------

export type ImageSourceKind = PendingImage["source"]

/** `${XDG_STATE_HOME:-~/.local/state}/lunarwing-tui/<profile>.json`, 0600. Timestamps are ISO-8601 strings. */
export interface ProfileState {
  lastThreadId?: string
  /** "tui-<profile>-<16 hex>"; written only so --thread last or /thread resume can rejoin it (3.9). */
  lastReplThread?: string
  /** First-use privacy acknowledgement per voice backend (5.6). */
  voiceAck?: Partial<Record<VoiceSourceId, string>>
  /** First-use storage acknowledgement per image source (5.6, SEC-09). */
  imageAck?: Partial<Record<ImageSourceKind, string>>
  /** Written by /voice device <name>; wins over [voice] device (5.5). */
  voiceDevice?: string
}

/** `.../lunarwing-tui/state.json`, 0600. */
export interface GlobalState {
  lastTenant?: string
}

/** Keys a secondary instance (a live pid holds the profile lock) never writes (3.11.5, F3). */
export const PRIMARY_ONLY_STATE_KEYS = ["lastThreadId", "lastReplThread", "lastTenant"] as const

export interface StateFile {
  readonly profile: string
  /** From config/instance-lock.ts. When false, changes to PRIMARY_ONLY_STATE_KEYS are dropped silently. */
  readonly primary: boolean
  /** The profile state; {} when the file is missing. */
  read(): Promise<ProfileState>
  /**
   * Applies `change` to the current profile state and writes it atomically (`<file>.tmp` opened
   * O_CREAT|O_EXCL|O_NOFOLLOW 0600, then rename). Calls are serialized. Resolves with the state as written.
   */
  update(change: (current: ProfileState) => ProfileState): Promise<ProfileState>
  /** The shared state.json; {} when missing. */
  readGlobal(): Promise<GlobalState>
  updateGlobal(change: (current: GlobalState) => GlobalState): Promise<GlobalState>
}

// ---------------------------------------------------------------------------------------------------------
// Child processes (3.11.3, 10.6): async Bun.spawn, argv only, allowlisted environment
// ---------------------------------------------------------------------------------------------------------

/**
 * The allowlisted child environment built by config/child-env.ts (PATH, HOME, LANG, LC_*, XDG_*, WAYLAND_DISPLAY,
 * ...). Never process.env: no token, proxy or TRANSCRIBE_DUMP_DIR variable reaches a child.
 */
export type ChildEnv = Readonly<Record<string, string>>

export interface SpawnOptions {
  /** Required so no call site can inherit process.env. */
  env: ChildEnv
  /** Uint8Array: written to the child's stdin, then closed. */
  stdin: "pipe" | "ignore" | Uint8Array
  /** No "inherit": a child must never write to the terminal OpenTUI owns. */
  stdout: "pipe" | "ignore"
  stderr: "pipe" | "ignore"
  cwd?: string
  /** Aborting kills the child with killSignal. */
  signal?: AbortSignal
  /** Milliseconds; then the child is killed with killSignal. */
  timeout?: number
  /** Default SIGTERM. */
  killSignal?: NodeJS.Signals | number
}

/** Bun's FileSink subset for stdin "pipe". */
export interface ChildStdin {
  write(chunk: string | Uint8Array): number | Promise<number>
  flush(): number | Promise<number>
  end(error?: Error): number | Promise<number>
}

/** The Bun Subprocess subset children are driven through (real or test/helpers/fake-spawn.ts). */
export interface ChildProcess {
  readonly pid: number
  /** Present only for stdin "pipe". */
  readonly stdin: ChildStdin | undefined
  /** Present only for "pipe". */
  readonly stdout: ReadableStream<Uint8Array> | undefined
  readonly stderr: ReadableStream<Uint8Array> | undefined
  /** Resolves with the exit code, or 128 + n when killed by signal n (Bun semantics: SIGKILL -> 137). */
  readonly exited: Promise<number>
  /** null while running and when killed by a signal. */
  readonly exitCode: number | null
  readonly signalCode: NodeJS.Signals | null
  /** True once the process has exited (Bun semantics), whether by signal or not. */
  readonly killed: boolean
  kill(signal?: NodeJS.Signals | number): void
}

/**
 * The spawn-injection seam: every child-spawning module (images, voice recorder, notifier, copy, Hex) takes a
 * SpawnFn and defaults to bunSpawn; tests pass test/helpers/fake-spawn.ts. Throws synchronously when the
 * executable cannot be started (Bun: an Error with code "ENOENT" or "EACCES").
 */
export type SpawnFn = (argv: readonly string[], options: SpawnOptions) => ChildProcess

/** The production SpawnFn: Bun.spawn with an argv array (never a shell) and exactly the given environment. */
export const bunSpawn: SpawnFn = (argv, options) => {
  const [command, ...args] = argv
  if (command === undefined || command === "") throw new TypeError("spawn needs a non-empty argv")
  const proc = Bun.spawn([command, ...args], {
    env: { ...options.env },
    stdin: options.stdin,
    stdout: options.stdout,
    stderr: options.stderr,
    cwd: options.cwd,
    signal: options.signal,
    timeout: options.timeout,
    killSignal: options.killSignal,
  })
  const stdin = typeof proc.stdin === "object" ? proc.stdin : undefined
  return {
    pid: proc.pid,
    stdin,
    stdout: proc.stdout,
    stderr: proc.stderr,
    exited: proc.exited,
    get exitCode() {
      return proc.exitCode
    },
    get signalCode() {
      return proc.signalCode
    },
    get killed() {
      return proc.killed
    },
    kill: (signal) => proc.kill(signal),
  }
}

// ---------------------------------------------------------------------------------------------------------
// Resolved configuration sections (3.11.1, 3.11.2). config/config.ts merges flags > environment >
// profile > globals > these defaults.
// ---------------------------------------------------------------------------------------------------------

export type TransportChoice = "gateway" | "socket" | "auto"
export type ImageProtocolChoice = "auto" | "kitty" | "sixel" | "blocks"
/** auto honours NO_COLOR (3.12.2). */
export type ColorMode = "auto" | "always" | "never"
export type NotifyMode = "approvals" | "all" | "off"
export type VoiceBlurPolicy = "hold" | "always" | "never"

/** [ui] */
export interface UiConfig {
  /** --image-protocol > LUNARWING_TUI_IMAGE_PROTOCOL > image_protocol. */
  imageProtocol: ImageProtocolChoice
  maxTurnsInMemory: number
  color: ColorMode
  /** No spinner animation; VU meter at 4 Hz. */
  reduceMotion: boolean
  cursorBlink: boolean
  notify: NotifyMode
  /** Tenants where supervised test 16.5 passed (6.5). */
  visionVerifiedTenants: readonly string[]
}

/** [voice.hex] (M1b). */
export interface HexConfig {
  /** Argv prefix; default $LUNARWING_TUI_HEX_BIN, else ["hex"] resolved with Bun.which. */
  command: readonly string[]
  /** "" = the first verified model from GET /models. */
  model: string
}

/** [voice], with durations normalized to milliseconds. */
export interface VoiceConfig {
  /** False with --no-voice. */
  enabled: boolean
  /** --voice-order > order. */
  order: readonly VoiceSourceId[]
  /** --ptt-key (repeatable) > ptt_keys, e.g. "f9", "ctrl+space". */
  pttKeys: readonly string[]
  /** repeat_delay_ms; 300 matches Hyprland repeat_delay on this host. */
  repeatDelayMs: number
  /** max_duration_s * 1000, capped at VOICE_MAX_DURATION_CAP_MS. */
  maxDurationMs: number
  /** Toggle mode only, opt-in (7.2). */
  autoStopOnSilence: boolean
  silenceStopMs: number
  blurPolicy: VoiceBlurPolicy
  /** Case-insensitive substring of the device name; the profile state's voiceDevice wins (5.5). */
  device: string | null
  hex: HexConfig
}

/** The sidecar processes at most 300 s; the TUI stops below that (5.5). */
export const VOICE_MAX_DURATION_CAP_MS = 290_000

/** Built-in defaults, the lowest precedence layer (3.11.2). Frozen, arrays included. */
export const DEFAULT_UI_CONFIG: Readonly<UiConfig> = Object.freeze<UiConfig>({
  imageProtocol: "auto",
  maxTurnsInMemory: 500,
  color: "auto",
  reduceMotion: false,
  cursorBlink: true,
  notify: "approvals",
  visionVerifiedTenants: Object.freeze([]),
})

export const DEFAULT_HEX_CONFIG: Readonly<HexConfig> = Object.freeze<HexConfig>({
  command: Object.freeze(["hex"]),
  model: "",
})

export const DEFAULT_VOICE_CONFIG: Readonly<VoiceConfig> = Object.freeze<VoiceConfig>({
  enabled: true,
  order: Object.freeze<VoiceSourceId[]>(["lunarvoice", "hex-app", "hex-embedded"]),
  pttKeys: Object.freeze(["f9", "ctrl+space"]),
  repeatDelayMs: 600,
  maxDurationMs: 120_000,
  autoStopOnSilence: false,
  silenceStopMs: 3_500,
  blurPolicy: "hold",
  device: null,
  hex: DEFAULT_HEX_CONFIG,
})
