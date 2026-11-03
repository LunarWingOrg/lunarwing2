/**
 * The mic widget at the left of the composer row (design 7.4). Every label fits 8 columns, so nothing wraps:
 * idle "( mic )", arming "( ... )", recording "REC m:ss", transcribing (queue not empty, not recording)
 * "[ .... ]", no source or recorder available "(no mic)".
 *
 * onMouseDown calls preventDefault() so the composer keeps the focus, then voice.micDown(); releases on the
 * widget, at the end of a drag, or anywhere else (the root box's onMouseUp) call the idempotent micUp().
 */
import type { MouseEvent } from "@opentui/core"
import type { ReactNode } from "react"
import type { VoiceStatusReport, VoiceUiState } from "../voice/types.ts"
import { formatClock } from "../util.ts"
import { styleOf, type Theme } from "./theme.ts"

export const MIC_WIDGET_WIDTH = 10

/** True when no recording can start: voice off, no recorder, or every probed source unavailable. */
export function noMicAvailable(report: VoiceStatusReport | null): boolean {
  if (report === null || !report.enabled) return true
  if (report.recorder.kind === null && report.recorder.reason !== undefined && report.recorder.reason !== "not checked yet") return true
  const inBuild = report.sources.filter((source) => source.inBuild)
  if (inBuild.length === 0) return true
  const probed = inBuild.filter((source) => source.availability !== null)
  return probed.length === inBuild.length && probed.every((source) => source.availability?.available === false)
}

export function micLabel(voice: Pick<VoiceUiState, "phase" | "startedAt" | "queue">, opts: { noMic: boolean; now: number }): string {
  switch (voice.phase) {
    case "arming":
      return "( ... )"
    case "recording":
      return `REC ${formatClock(opts.now - (voice.startedAt ?? opts.now))}`
    case "finishing":
      return "[ .... ]"
    case "idle":
      if (voice.queue.length > 0) return "[ .... ]"
      return opts.noMic ? "(no mic)" : "( mic )"
  }
}

export interface MicWidgetProps {
  label: string
  theme: Theme
  recording: boolean
  onDown: () => void
  onUp: () => void
}

export function MicWidget(props: MicWidgetProps): ReactNode {
  const down = (event: MouseEvent): void => {
    event.preventDefault()
    props.onDown()
  }
  return (
    <box style={{ width: MIC_WIDGET_WIDTH, height: 1, flexShrink: 0 }} onMouseDown={down} onMouseUp={() => props.onUp()} onMouseDragEnd={() => props.onUp()}>
      <text selectable={false} wrapMode="none" {...styleOf(props.theme, props.recording ? "rec" : "accent")}>
        {props.label}
      </text>
    </box>
  )
}
