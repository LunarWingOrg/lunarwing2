/**
 * Bounded single-consumer event queue between a transport's reader and the supervisor (design 3.7).
 *
 * - At most `capacity` (10 000) queued events. On overflow the queued `turn.delta` items are dropped (the final
 *   text is canonical and history can be resynced) and one `resync.required{overflow}` is queued; if the queue is
 *   still full of other events the oldest one makes room for that marker and the newest event is dropped.
 * - `fail(error)`: the consumer still receives every queued event, then its next() throws `error` (the supervisor
 *   treats that as a connection loss). `close()`: the consumer drains and then ends normally.
 * - A consumer that stops iterating (break/return) ends the channel and runs `onReturn` (the transport aborts its
 *   reader).
 */
import type { TransportEvent } from "./types.ts"

export const EVENT_CHANNEL_CAPACITY = 10_000

export interface EventChannelOptions {
  capacity?: number
  /** Called once when the consumer stops iterating early. */
  onReturn?: () => void
}

type Waiter = {
  resolve: (result: IteratorResult<TransportEvent>) => void
  reject: (error: unknown) => void
}

export class EventChannel implements AsyncIterable<TransportEvent> {
  private readonly capacity: number
  private readonly onReturn: (() => void) | undefined
  private queue: TransportEvent[] = []
  private head = 0
  private waiter: Waiter | null = null
  private state: "open" | "closed" | "failed" | "done" = "open"
  private failure: unknown = null
  private overflowMarker: TransportEvent | null = null
  private deliveredCount = 0
  private droppedCount = 0
  private iterated = false

  constructor(opts: EventChannelOptions = {}) {
    this.capacity = Math.max(1, opts.capacity ?? EVENT_CHANNEL_CAPACITY)
    this.onReturn = opts.onReturn
  }

  /** Queues an event; false when the channel no longer accepts events or the event was dropped. */
  push(event: TransportEvent): boolean {
    if (this.state !== "open") return false
    if (this.waiter && this.size === 0) {
      const waiter = this.waiter
      this.waiter = null
      this.deliveredCount += 1
      waiter.resolve({ value: event, done: false })
      return true
    }
    if (this.size >= this.capacity && !this.makeRoom()) {
      this.droppedCount += 1
      return false
    }
    this.queue.push(event)
    return true
  }

  /** Ends the stream normally once the queued events are consumed. */
  close(): void {
    if (this.state !== "open") return
    this.state = "closed"
    this.settleWaiter()
  }

  /** Ends the stream with `error` once the queued events are consumed. */
  fail(error: unknown): void {
    if (this.state !== "open") return
    this.state = "failed"
    this.failure = error
    this.settleWaiter()
  }

  /** Removes and returns the first queued (undelivered) event matching `predicate`. */
  removeFirst(predicate: (event: TransportEvent) => boolean): TransportEvent | null {
    for (let i = this.head; i < this.queue.length; i += 1) {
      const event = this.queue[i]
      if (event !== undefined && predicate(event)) {
        this.queue.splice(i, 1)
        return event
      }
    }
    return null
  }

  /** Queued, undelivered events. */
  get size(): number {
    return this.queue.length - this.head
  }

  /** Events handed to the consumer so far. */
  get delivered(): number {
    return this.deliveredCount
  }

  /** Events dropped because the queue was full (deltas dropped on overflow included). */
  get dropped(): number {
    return this.droppedCount
  }

  /** False once close(), fail() or an early return ended the channel. */
  get open(): boolean {
    return this.state === "open"
  }

  [Symbol.asyncIterator](): AsyncIterator<TransportEvent> {
    if (this.iterated) throw new Error("EventChannel has a single consumer")
    this.iterated = true
    return {
      next: () => this.next(),
      return: (): Promise<IteratorResult<TransportEvent>> => {
        this.finishEarly()
        return Promise.resolve({ value: undefined, done: true })
      },
    }
  }

  private next(): Promise<IteratorResult<TransportEvent>> {
    const event = this.shift()
    if (event !== undefined) return Promise.resolve({ value: event, done: false })
    if (this.state === "failed") {
      this.state = "done"
      return Promise.reject(this.failure)
    }
    if (this.state !== "open") {
      this.state = "done"
      return Promise.resolve({ value: undefined, done: true })
    }
    if (this.waiter) return Promise.reject(new Error("EventChannel has a single consumer"))
    return new Promise((resolve, reject) => {
      this.waiter = { resolve, reject }
    })
  }

  private shift(): TransportEvent | undefined {
    if (this.head >= this.queue.length) return undefined
    const event = this.queue[this.head]
    this.head += 1
    if (this.head > 1024 && this.head * 2 > this.queue.length) {
      this.queue = this.queue.slice(this.head)
      this.head = 0
    }
    if (event === this.overflowMarker) this.overflowMarker = null
    if (event !== undefined) this.deliveredCount += 1
    return event
  }

  /** Overflow policy; true when the incoming event now fits. */
  private makeRoom(): boolean {
    const before = this.size
    this.queue = this.queue.slice(this.head).filter((event) => event.kind !== "turn.delta")
    this.head = 0
    this.droppedCount += before - this.queue.length
    if (this.overflowMarker === null) {
      if (this.size >= this.capacity) {
        this.queue.shift()
        this.droppedCount += 1
      }
      const marker: TransportEvent = { kind: "resync.required", reason: "overflow" }
      this.overflowMarker = marker
      this.queue.push(marker)
    }
    return this.size < this.capacity
  }

  private settleWaiter(): void {
    const waiter = this.waiter
    if (!waiter || this.size > 0) return
    this.waiter = null
    if (this.state === "failed") {
      this.state = "done"
      waiter.reject(this.failure)
    } else {
      this.state = "done"
      waiter.resolve({ value: undefined, done: true })
    }
  }

  private finishEarly(): void {
    const wasOpen = this.state === "open"
    this.state = "done"
    this.queue = []
    this.head = 0
    const waiter = this.waiter
    this.waiter = null
    waiter?.resolve({ value: undefined, done: true })
    if (wasOpen) this.onReturn?.()
  }
}
