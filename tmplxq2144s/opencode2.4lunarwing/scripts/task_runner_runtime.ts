import { isRecord, truncateUtf8, utf8ByteLength } from "./lunarwing_runtime"

export function serializeFrameWithinLimit(
  frame: Record<string, unknown>,
  payloadKey: string,
  maxBytes: number,
  label = "frame",
): string {
  const nested = frame[payloadKey]
  const payload: Record<string, unknown> = isRecord(nested) ? { ...nested } : {}
  const limited: Record<string, unknown> = { ...frame, [payloadKey]: isRecord(nested) ? payload : nested }
  let serialized = JSON.stringify(limited)
  if (utf8ByteLength(serialized) <= maxBytes) return serialized
  if (!isRecord(nested)) throw new Error(`${label} exceeds ${maxBytes} bytes`)

  for (const field of ["output", "delta", "error"] as const) {
    const value = payload[field]
    if (typeof value !== "string") continue
    let low = 0
    let high = utf8ByteLength(value)
    let best = ""
    while (low <= high) {
      const middle = Math.floor((low + high) / 2)
      const candidate = truncateUtf8(value, middle)
      payload[field] = candidate
      const encoded = JSON.stringify(limited)
      if (utf8ByteLength(encoded) <= maxBytes) {
        best = candidate
        serialized = encoded
        low = middle + 1
      } else {
        high = middle - 1
      }
    }
    payload[field] = best
    serialized = JSON.stringify(limited)
    if (utf8ByteLength(serialized) <= maxBytes) return serialized
  }
  throw new Error(`${label} exceeds ${maxBytes} bytes`)
}

export function wsMaxPayloadBytes(): number {
  const raw = process.env.WS_MAX_PAYLOAD_BYTES
  if (raw === undefined || raw === "") return 1_048_576
  const value = /^\d+$/.test(raw) ? Number(raw) : Number.NaN
  if (!Number.isSafeInteger(value) || value < 1_024 || value > 16_777_216) {
    throw new Error("WS_MAX_PAYLOAD_BYTES must be an integer between 1024 and 16777216")
  }
  return value
}

export const RUNNER_LINE_SLACK_BYTES = 65_536

export function runnerLineCapBytes(): number {
  return wsMaxPayloadBytes() + RUNNER_LINE_SLACK_BYTES
}
