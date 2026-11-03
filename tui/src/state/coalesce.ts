/**
 * Transport events reach the store through here (design 3.3, area-client 2.1, 2.11): they are batched and
 * flushed with one dispatchMany at most every 33 ms, so a burst of stream chunks costs one render per frame.
 * Consecutive turn.delta events of the same turn (and thread) are merged into one. Each `evt` action carries the
 * Clock time of its (latest) event. User intents never pass through here; they dispatch immediately.
 *
 * The timer comes from the injected Clock (ManualClock in tests). The optional `tap` sees every event as it
 * arrives, before batching, for controllers that react to events rather than state (legacy settle hints,
 * resync.required, the desktop notifier).
 */
import type { Clock, TimerHandle } from "@opentui/core"
import type { Action } from "./actions.ts"
import type { Store } from "./model.ts"
import type { TransportEvent } from "../transport/types.ts"

export const COALESCE_INTERVAL_MS = 33
/** A batch this large is flushed at once instead of waiting for the timer. */
export const COALESCE_MAX_BATCH = 1_000

export interface CoalescerOptions {
  store: Pick<Store, "dispatchMany">
  clock: Pick<Clock, "now" | "setTimeout" | "clearTimeout">
  intervalMs?: number
  maxBatch?: number
  tap?: (event: TransportEvent, at: number) => void
}

export interface Coalescer {
  push(event: TransportEvent): void
  /** Dispatches everything queued now. */
  flush(): void
  /** Flushes what is queued, cancels the timer and ignores later pushes. */
  dispose(): void
  /** Queued `evt` actions (after merging). */
  readonly pending: number
}

type EventAction = Extract<Action, { type: "evt" }>

export function createCoalescer(options: CoalescerOptions): Coalescer {
  const interval = Math.max(0, options.intervalMs ?? COALESCE_INTERVAL_MS)
  const maxBatch = Math.max(1, options.maxBatch ?? COALESCE_MAX_BATCH)
  let queue: EventAction[] = []
  let timer: TimerHandle | null = null
  let lastFlushAt = -Infinity
  let disposed = false

  const flush = (): void => {
    if (timer !== null) {
      options.clock.clearTimeout(timer)
      timer = null
    }
    lastFlushAt = options.clock.now()
    if (queue.length === 0) return
    const batch = queue
    queue = []
    options.store.dispatchMany(batch)
  }

  const schedule = (now: number): void => {
    if (timer !== null) return
    const delay = Math.max(0, lastFlushAt + interval - now)
    timer = options.clock.setTimeout(() => {
      timer = null
      flush()
    }, delay)
  }

  return {
    push(event) {
      if (disposed) return
      const at = options.clock.now()
      options.tap?.(event, at)
      queue = appendCoalesced(queue, { type: "evt", event, at })
      if (queue.length >= maxBatch) flush()
      else schedule(at)
    },
    flush,
    dispose() {
      if (disposed) return
      flush()
      disposed = true
    },
    get pending() {
      return queue.length
    },
  }
}

/** Appends `next`, merging it into the last queued action when both are deltas of the same turn and thread. */
export function appendCoalesced(queue: EventAction[], next: EventAction): EventAction[] {
  const last = queue[queue.length - 1]
  const merged = last === undefined ? null : mergeDeltas(last, next)
  if (merged === null) {
    queue.push(next)
    return queue
  }
  queue[queue.length - 1] = merged
  return queue
}

function mergeDeltas(previous: EventAction, next: EventAction): EventAction | null {
  const a = previous.event
  const b = next.event
  if (a.kind !== "turn.delta" || b.kind !== "turn.delta") return null
  if (a.turnId !== b.turnId || a.threadId !== b.threadId) return null
  return { type: "evt", at: next.at, event: { ...a, text: a.text + b.text } }
}
