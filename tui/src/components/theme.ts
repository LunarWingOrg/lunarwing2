/**
 * Colours and text attributes (design 3.12.2, UX-15).
 *
 * resolveTheme({ color, noColor }): `auto` turns colour off when NO_COLOR is set and non-empty (OpenTUI does not
 * honour it itself), `never` always, `always` ignores NO_COLOR. The monochrome theme draws every role in the
 * terminal's own default foreground (RGBA.defaultForeground(), SGR 39) and marks states with bold, inverse and
 * underline only. Leaving `fg` out is not enough: OpenTUI then paints explicit white (RGB 255,255,255), which a
 * light terminal background makes unreadable. Every state also carries a text label (mic `( mic )`, `REC 0:07`,
 * `OFFLINE`, `APPROVAL`, phase words), so nothing depends on colour alone.
 */
import { RGBA, TextAttributes } from "@opentui/core"
import type { ColorMode } from "../config/types.ts"

/** A colour for OpenTUI props: a hex string, or an RGBA such as the terminal's default foreground. */
export type ThemeColor = string | RGBA

export type ThemeRole =
  | "text"
  | "dim"
  | "user"
  | "assistant"
  | "accent"
  | "info"
  | "warn"
  | "error"
  | "ok"
  | "approval"
  | "escape"
  | "rec"
  | "border"
  | "title"

export interface Theme {
  /** False for the monochrome theme. */
  readonly color: boolean
  /** Per role; the monochrome theme uses the terminal's default foreground for every role. */
  readonly fg: Readonly<Record<ThemeRole, ThemeColor>>
  /** Attribute bits per role (TextAttributes); 0 = none. */
  readonly attr: Readonly<Record<ThemeRole, number>>
  /** Placeholder text in the composer; the textarea needs a concrete colour. */
  readonly placeholder: string
  /** Text and track colours of the transcript scrollbar. */
  readonly scrollbar: { readonly thumb: string; readonly track: string }
  /** The fill of overlays (info panel): it must cover the transcript beneath. */
  readonly panelBackground: ThemeColor
}

const COLOR_FG: Record<ThemeRole, string> = {
  text: "#c0caf5",
  dim: "#7a82a8",
  user: "#7dcfff",
  assistant: "#c0caf5",
  accent: "#bb9af7",
  info: "#7dcfff",
  warn: "#e0af68",
  error: "#f7768e",
  ok: "#9ece6a",
  approval: "#e0af68",
  escape: "#ff9e64",
  rec: "#f7768e",
  border: "#565f89",
  title: "#a9b1d6",
}

const COLOR_ATTR: Record<ThemeRole, number> = {
  text: 0,
  dim: 0,
  user: TextAttributes.BOLD,
  assistant: TextAttributes.BOLD,
  accent: 0,
  info: 0,
  warn: TextAttributes.BOLD,
  error: TextAttributes.BOLD,
  ok: 0,
  approval: TextAttributes.BOLD,
  escape: TextAttributes.UNDERLINE,
  rec: TextAttributes.BOLD,
  border: 0,
  title: 0,
}

/** Monochrome: emphasis only through bold, inverse and underline. */
const MONO_ATTR: Record<ThemeRole, number> = {
  text: 0,
  dim: 0,
  user: TextAttributes.BOLD,
  assistant: TextAttributes.BOLD,
  accent: TextAttributes.UNDERLINE,
  info: 0,
  warn: TextAttributes.BOLD,
  error: TextAttributes.BOLD | TextAttributes.INVERSE,
  ok: 0,
  approval: TextAttributes.BOLD | TextAttributes.INVERSE,
  escape: TextAttributes.UNDERLINE | TextAttributes.BOLD,
  rec: TextAttributes.BOLD | TextAttributes.INVERSE,
  border: 0,
  title: TextAttributes.BOLD,
}

/** The terminal's default foreground for every role (intent "default": SGR 39, never an explicit colour). */
const DEFAULT_FG: Record<ThemeRole, RGBA> = Object.fromEntries(
  Object.keys(COLOR_FG).map((role) => [role, RGBA.defaultForeground()]),
) as Record<ThemeRole, RGBA>

export const COLOR_THEME: Theme = Object.freeze({
  color: true,
  fg: Object.freeze({ ...COLOR_FG }),
  attr: Object.freeze({ ...COLOR_ATTR }),
  placeholder: "#565f89",
  scrollbar: Object.freeze({ thumb: "#565f89", track: "#1f2335" }),
  panelBackground: "#1a1b26",
})

/**
 * The terminal's default colours for text, borders and overlays (they follow a light or a dark terminal theme);
 * greys only where a concrete colour is needed (placeholder, scrollbar), and grey carries no hue.
 */
export const MONO_THEME: Theme = Object.freeze({
  color: false,
  fg: Object.freeze({ ...DEFAULT_FG }),
  attr: Object.freeze({ ...MONO_ATTR }),
  placeholder: "#808080",
  scrollbar: Object.freeze({ thumb: "#808080", track: "#000000" }),
  panelBackground: RGBA.defaultBackground(),
})

export interface ThemeInput {
  color: ColorMode
  /** process.env.NO_COLOR */
  noColor?: string | undefined
}

/** True when colour is used for this configuration and environment. */
export function colorEnabled(input: ThemeInput): boolean {
  if (input.color === "never") return false
  if (input.color === "always") return true
  return input.noColor === undefined || input.noColor === ""
}

export function resolveTheme(input: ThemeInput): Theme {
  return colorEnabled(input) ? COLOR_THEME : MONO_THEME
}

/** `{ fg }` for a role (the terminal's default foreground in the monochrome theme). */
export function fgOf(theme: Theme, role: ThemeRole): { fg: ThemeColor } {
  return { fg: theme.fg[role] }
}

/** `{ fg, attributes }` for a role. */
export function styleOf(theme: Theme, role: ThemeRole): { fg: ThemeColor; attributes?: number } {
  const attributes = theme.attr[role]
  return { ...fgOf(theme, role), ...(attributes === 0 ? {} : { attributes }) }
}

/** `{ borderColor }` for a box (the terminal's default foreground in the monochrome theme). */
export function borderOf(theme: Theme, role: ThemeRole = "border"): { borderColor: ThemeColor } {
  return { borderColor: theme.fg[role] }
}

/**
 * The tenant picker's <select> colours: OpenTUI's own in colour; in monochrome the terminal's default foreground
 * for every text and an achromatic grey behind the selected row (its defaults are white text and a yellow
 * selection, #FFFF00 on #334455).
 */
export function selectColors(theme: Theme): Partial<Record<
  "textColor" | "focusedTextColor" | "focusedBackgroundColor" | "selectedBackgroundColor" | "selectedTextColor" | "descriptionColor" | "selectedDescriptionColor",
  ThemeColor
>> {
  if (theme.color) return {}
  const fg = theme.fg.text
  return {
    textColor: fg,
    focusedTextColor: fg,
    focusedBackgroundColor: "transparent",
    selectedBackgroundColor: theme.placeholder,
    selectedTextColor: fg,
    descriptionColor: fg,
    selectedDescriptionColor: fg,
  }
}

/** The textarea's text colours: the theme's in monochrome; OpenTUI's own in colour (unchanged look). */
export function textareaColors(theme: Theme): { textColor?: ThemeColor; focusedTextColor?: ThemeColor } {
  return theme.color ? {} : { textColor: theme.fg.text, focusedTextColor: theme.fg.text }
}
