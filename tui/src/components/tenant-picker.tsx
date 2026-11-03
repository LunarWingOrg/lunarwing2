/**
 * The tenant picker (design 3.11.5, UX-10): when several valid tenants/*.json files exist, an OpenTUI <select>
 * (otr081:src/components/index.d.ts:11; SelectOption {name, description}, ot081:renderables/Select.d.ts:9-12)
 * lists tenant, URL, token kind and expiry, preselecting lastTenant. Enter picks, Esc quits (resolves null).
 * config/config.ts writes the pick back as lastTenant.
 *
 * pickTenantOn(renderer) implements config/tenant-select.ts TenantPicker by mounting the view on the renderer
 * before the app exists, and unmounting it once a choice was made.
 */
import type { CliRenderer, KeyEvent, SelectOption } from "@opentui/core"
import { createRoot, useKeyboard } from "@opentui/react"
import type { ReactNode } from "react"
import type { TenantChoice, TenantPicker } from "../config/tenant-select.ts"
import { truncate } from "../util.ts"
import { borderOf, selectColors, styleOf, type Theme } from "./theme.ts"

export interface TenantPickerViewProps {
  choices: readonly TenantChoice[]
  preselect: string | null
  theme: Theme
  onDone: (tenant: string | null) => void
  width?: number
}

export function tenantOptions(choices: readonly TenantChoice[]): SelectOption[] {
  return choices.map((choice) => ({ name: choice.name, description: choice.description, value: choice.name }))
}

export function TenantPickerView(props: TenantPickerViewProps): ReactNode {
  const options = tenantOptions(props.choices)
  const preselected = Math.max(0, options.findIndex((option) => option.value === props.preselect))
  useKeyboard((key: KeyEvent) => {
    if (key.name === "escape") {
      key.preventDefault()
      props.onDone(null)
    }
  })
  const select = (_index: number, option: SelectOption | null): void => {
    const value: unknown = option?.value
    props.onDone(typeof value === "string" ? value : null)
  }
  const width = props.width ?? 80
  return (
    <box style={{ flexDirection: "column", width, flexGrow: 1 }}>
      <text selectable={false} wrapMode="none" {...styleOf(props.theme, "title")}>
        {truncate("LunarWing: choose a tenant (Enter picks, Esc quits)", width)}
      </text>
      <box border {...borderOf(props.theme)} style={{ flexGrow: 1 }}>
        <select options={options} selectedIndex={preselected} focused showDescription onSelect={select} {...selectColors(props.theme)} style={{ flexGrow: 1 }} />
      </box>
    </box>
  )
}

/** A TenantPicker that renders on `renderer` (the one main.tsx later mounts the app on). */
export function pickTenantOn(renderer: CliRenderer, theme: Theme): TenantPicker {
  return (choices, preselect) =>
    new Promise<string | null>((resolve) => {
      const root = createRoot(renderer)
      let settled = false
      const done = (tenant: string | null): void => {
        if (settled) return
        settled = true
        // Unmount after the select's own key handling has returned.
        queueMicrotask(() => {
          root.unmount()
          resolve(tenant)
        })
      }
      root.render(<TenantPickerView choices={choices} preselect={preselect} theme={theme} onDone={done} width={renderer.width} />)
    })
}
