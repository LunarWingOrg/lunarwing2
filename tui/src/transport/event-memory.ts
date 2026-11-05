/** Conservative retained-size estimate for the acyclic event records produced by transport decoders. */
export function eventMemoryBytes(value: unknown): number {
  if (typeof value === "string") return value.length * 2
  if (typeof value === "number" || typeof value === "boolean") return 8
  if (value instanceof Uint8Array) return value.byteLength
  if (value instanceof ArrayBuffer) return value.byteLength
  if (Array.isArray(value)) return value.reduce<number>((total, item) => total + eventMemoryBytes(item), 0)
  if (typeof value === "object" && value !== null) {
    return Object.entries(value).reduce<number>(
      (total, [key, item]) => total + key.length * 2 + eventMemoryBytes(item),
      0,
    )
  }
  return 0
}
