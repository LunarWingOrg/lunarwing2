/**
 * The allowlisted environment for child processes (design 3.11.3, 10.6).
 *
 * parec, wl-paste, wl-copy, grim, slurp, notify-send, xdg-open and hex get only the variables they need to find
 * the session (Wayland/X11, D-Bus, PulseAudio/PipeWire), the locale and Hex's discovery directory. Never
 * process.env: no token, no proxy variable (3.11.4) and no TRANSCRIBE_DUMP_DIR (Hex writes audio-derived
 * tensors there) ever reaches a child. Pass the result as SpawnOptions.env (config/types.ts).
 */
import { PROXY_ENV_NAMES } from "./proxy-env.ts"
import type { ChildEnv } from "./types.ts"

/** Copied from the parent environment when set and non-empty. */
export const CHILD_ENV_NAMES = [
  "PATH",
  "HOME",
  "LANG",
  "XDG_RUNTIME_DIR",
  "XDG_DATA_HOME",
  "XDG_CONFIG_HOME",
  "WAYLAND_DISPLAY",
  "DISPLAY",
  "DBUS_SESSION_BUS_ADDRESS",
  "PULSE_SERVER",
  "PIPEWIRE_RUNTIME_DIR",
  "HEX_APPLICATION_SUPPORT_DIR",
] as const

/** LC_ALL, LC_CTYPE, LC_MESSAGES, ... */
const LOCALE_NAME = /^LC_[A-Z_]+$/

/** Never passed, not even as an explicit extra. */
export const CHILD_ENV_DENIED: readonly string[] = [
  ...PROXY_ENV_NAMES,
  "NO_PROXY",
  "no_proxy",
  "TRANSCRIBE_DUMP_DIR",
  "LUNARWING_GATEWAY_TOKEN",
  "LUNARWING_GATEWAY_TOKEN_FILE",
]

const ENV_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/
/** Extras are for paths and display hand-offs; anything named like a credential is a programming error. */
const SECRET_LIKE_NAME = /TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|API_?KEY|AUTH/i

export type EnvSource = Readonly<Record<string, string | undefined>>

function isAllowedName(name: string): boolean {
  return (CHILD_ENV_NAMES as readonly string[]).includes(name) || LOCALE_NAME.test(name)
}

/**
 * Builds the child environment from `source` (default process.env). `extra` adds or overrides variables for one
 * child (for example HEX_APPLICATION_SUPPORT_DIR); a denied or credential-like name throws TypeError.
 */
export function buildChildEnv(source: EnvSource = process.env, extra: EnvSource = {}): ChildEnv {
  const env: Record<string, string> = {}
  for (const [name, value] of Object.entries(source)) {
    if (value !== undefined && value !== "" && isAllowedName(name)) env[name] = value
  }
  for (const [name, value] of Object.entries(extra)) {
    if (!ENV_NAME.test(name) || CHILD_ENV_DENIED.includes(name) || SECRET_LIKE_NAME.test(name)) {
      throw new TypeError(`child environment: ${name} may not be passed to a child process`)
    }
    if (value !== undefined && value !== "") env[name] = value
  }
  return Object.freeze(env)
}
