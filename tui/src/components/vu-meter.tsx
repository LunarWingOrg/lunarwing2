/**
 * The VU meter (design 5.5, 7.4): 12 cells to the right of the mic widget, only while recording. -60..0 dBFS,
 * 5 dB per cell. The level comes from voice/levels.ts LevelMeterModel (instant attack, about 25 dB/s release,
 * 1 s peak hold, clip marker at -1 dBFS or above, "no signal" after 1.5 s below -70 dBFS), throttled by the voice
 * controller to 15 Hz (4 Hz with reduce_motion). ASCII cells so the meter reads the same in every font:
 * "=" level, "|" held peak, "!" clipping, "." empty.
 */
import type { ReactNode } from "react"
import type { LevelMeter } from "../voice/types.ts"
import { styleOf, type Theme } from "./theme.ts"

export const VU_CELLS = 12
const FLOOR_DB = -60
const DB_PER_CELL = -FLOOR_DB / VU_CELLS

function cellOf(db: number): number {
  if (!Number.isFinite(db)) return 0
  return Math.max(0, Math.min(VU_CELLS, Math.round((db - FLOOR_DB) / DB_PER_CELL)))
}

/** The meter as exactly VU_CELLS characters. */
export function vuCells(level: LevelMeter | null): string {
  if (level === null) return ".".repeat(VU_CELLS)
  const filled = cellOf(level.rmsDb)
  const peak = cellOf(level.heldPeakDb)
  const cells: string[] = Array.from({ length: VU_CELLS }, (_, index) => (index < filled ? "=" : "."))
  if (peak > 0 && peak > filled) cells[peak - 1] = "|"
  if (level.clipping) cells[VU_CELLS - 1] = "!"
  return cells.join("")
}

export function VuMeter(props: { level: LevelMeter | null; theme: Theme }): ReactNode {
  const clipping = props.level?.clipping === true
  return (
    <box style={{ width: VU_CELLS, height: 1, flexShrink: 0 }}>
      <text selectable={false} wrapMode="none" {...styleOf(props.theme, clipping ? "rec" : "ok")}>
        {vuCells(props.level)}
      </text>
    </box>
  )
}
