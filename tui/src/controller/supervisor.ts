/**
 * The connection supervisor (design 3.6.5, area-client 3.4).
 *
 *   connecting(attempt) --ok--> ready --loss--> reconnecting(attempt, retryAt) --timer--> connecting
 *        |                        +--/transport reconnect--> connecting
 *        +--fatal (second 401, 403, foreign listener owner, ...)--> offline(fatal)
 *   /quit from any state --> offline("quit")
 *
 * Per connection: info = await transport.connect(signal) -> conn/ready -> transport.resync() (on the gateway the
 * connect-time history seed, no extra request) -> resync/applied -> for await (ev of transport.events())
 * coalescer.push(ev). A normal end of events() means close() or a replaced connection; a thrown TransportError
 * is a loss: conn/lost, then wait max(backoff.next(), retryAfterMs) on the injected Clock. Nothing is ever
 * re-sent automatically.
 *
 * The coalescer is flushed before every action that must stay ordered after buffered events (conn/*,
 * resync/applied, thread/selected; the session controller calls flush() before turn/created and
 * control/created). Its tap sees every event: legacy settle hints start the 10 s resync grace (3.5 rule 11),
 * resync.required resyncs at once, and `onEvent` forwards to the notifier.
 *
 * The supervisor also runs the 1 s notice/tick (notice expiry, the legacy waiting rule, stray-ack window,
 * confirmation, guard and credential Esc expiries) and calls `onTick` for the session's own timers.
 */
import type { Clock, TimerHandle } from "@opentui/core"
import type { DebugLog } from "../log.ts"
import type { Store, ThreadId } from "../state/model.ts"
import { createCoalescer, type Coalescer } from "../state/coalesce.ts"
import { isSealed } from "../state/correlate.ts"
import { exponentialBackoff, type Backoff } from "../transport/backoff.ts"
import { isTransportError, TransportError } from "../transport/errors.ts"
import type { ServerInfo, ThreadSpec, Transport, TransportEvent } from "../transport/types.ts"

/** Legacy gateways: a settle hint without a reply triggers a resync after this grace (3.5 rule 11). */
export const SETTLE_GRACE_MS = 10_000
/** The notice/tick period. */
export const TICK_MS = 1_000

export interface SupervisorDeps {
  transport: Transport
  store: Store
  clock: Clock
  /**
   * The wall clock (epoch ms), default Date.now. History times are wall-clock server times while `clock` is
   * performance.now() in production, so resync/applied carries both readings (wallAt) for the conversion.
   */
  wallNow?: () => number
  /** Default exponentialBackoff on the same clock (500 ms doubling, cap 15 s, reset after 30 s ready). */
  backoff?: Backoff
  /** Sees every transport event when it arrives (before batching). */
  onEvent?: (event: TransportEvent, at: number) => void
  /** Called on every tick, after notice/tick. */
  onTick?: (at: number) => void
  log?: DebugLog
  settleGraceMs?: number
  tickMs?: number
}

export interface Supervisor {
  readonly transport: Transport
  readonly coalescer: Coalescer
  /** The ServerInfo of the newest connection, kept while reconnecting. */
  readonly serverInfo: ServerInfo | null
  start(): void
  /** /transport reconnect: connect again now (also from offline). */
  reconnectNow(): void
  /** Quit: no further attempts, the transport is closed, conn/offline("quit"). */
  stop(reason: string): Promise<void>
  /** Dispatches buffered events now. */
  flush(): void
  /** History resync (gateway); resolves false when the transport has none or it failed. */
  resync(): Promise<boolean>
  /** Gateway threads: switch, then seed the new thread's history. */
  selectThread(spec: ThreadSpec): Promise<ThreadId>
  /** A request outside connect/events failed fatally (second 401, 403 on a chat route): go offline. */
  reportFatal(error: TransportError): void
  /** Resolves when the connection loop has ended (stop, or offline). */
  idle(): Promise<void>
}

export function createSupervisor(deps: SupervisorDeps): Supervisor {
  return new ConnectionSupervisor(deps)
}

function asTransportError(error: unknown): TransportError {
  if (isTransportError(error)) return error
  return new TransportError("protocol", "the connection failed unexpectedly")
}

class ConnectionSupervisor implements Supervisor {
  readonly transport: Transport
  readonly coalescer: Coalescer
  private readonly backoff: Backoff
  private readonly settleGraceMs: number
  private loopPromise: Promise<void> | null = null
  private stopped = false
  private offline = false
  private reconnectRequested = false
  private connectAbort: AbortController | null = null
  private wake: (() => void) | null = null
  private waitTimer: TimerHandle | null = null
  private settleTimer: TimerHandle | null = null
  private tickTimer: TimerHandle | null = null
  private resyncing: Promise<boolean> | null = null
  private resyncAgain = false
  private info: ServerInfo | null = null

  constructor(private readonly deps: SupervisorDeps) {
    this.transport = deps.transport
    this.backoff = deps.backoff ?? exponentialBackoff({ clock: deps.clock })
    this.settleGraceMs = deps.settleGraceMs ?? SETTLE_GRACE_MS
    this.coalescer = createCoalescer({ store: deps.store, clock: deps.clock, tap: (event, at) => this.onEvent(event, at) })
  }

  get serverInfo(): ServerInfo | null {
    return this.info
  }

  start(): void {
    if (this.stopped || this.loopPromise !== null) return
    this.tickTimer ??= this.deps.clock.setInterval(() => this.tick(), this.deps.tickMs ?? TICK_MS)
    this.offline = false
    this.loopPromise = this.loop().finally(() => {
      this.loopPromise = null
    })
  }

  reconnectNow(): void {
    if (this.stopped) return
    this.backoff.reset()
    if (this.loopPromise === null) {
      this.start()
      return
    }
    this.reconnectRequested = true
    if (this.wake !== null) {
      this.wake()
      return
    }
    if (this.connectAbort !== null) {
      this.connectAbort.abort(new TransportError("closed", "reconnecting"))
      return
    }
    void this.transport.close("reconnect").catch(() => undefined)
  }

  async stop(reason: string): Promise<void> {
    if (this.stopped) return
    this.stopped = true
    this.clearTimer("tickTimer")
    this.clearTimer("settleTimer")
    this.wake?.()
    this.connectAbort?.abort(new TransportError("closed", reason))
    this.coalescer.dispose()
    try {
      await this.transport.close(reason)
    } catch {
      // Closing is best effort at quit.
    }
    this.deps.store.dispatch({ type: "conn/offline", reason: "quit", fatal: false, at: this.deps.clock.now() })
    await this.loopPromise
  }

  flush(): void {
    this.coalescer.flush()
  }

  idle(): Promise<void> {
    return this.loopPromise ?? Promise.resolve()
  }

  reportFatal(error: TransportError): void {
    if (this.stopped || this.offline) return
    this.goOffline(error)
    void this.transport.close("fatal").catch(() => undefined)
  }

  resync(): Promise<boolean> {
    if (this.resyncing !== null) {
      this.resyncAgain = true
      return this.resyncing
    }
    this.resyncing = this.runResync().finally(() => {
      this.resyncing = null
      if (this.resyncAgain) {
        this.resyncAgain = false
        void this.resync()
      }
    })
    return this.resyncing
  }

  async selectThread(spec: ThreadSpec): Promise<ThreadId> {
    const select = this.transport.selectThread?.bind(this.transport)
    if (select === undefined) throw new TransportError("unsupported", "this transport has no threads")
    const id = await select(spec)
    this.coalescer.flush()
    this.deps.store.dispatch({ type: "thread/selected", threadId: id, at: this.deps.clock.now() })
    await this.resync()
    return id
  }

  // ---- the loop ----

  private async loop(): Promise<void> {
    let attempt = 1
    while (!this.stopped && !this.offline) {
      const info = await this.connectOnce(attempt)
      if (info === "stop") return
      if (info === "retry") {
        attempt += 1
        continue
      }
      if (info === "again") {
        attempt = 1
        continue
      }
      attempt = 1
      const ended = await this.consume()
      if (ended === "stop") return
    }
  }

  /** One connect attempt; "retry" after a non-fatal failure (the backoff wait is done), "again" after reconnectNow. */
  private async connectOnce(attempt: number): Promise<ServerInfo | "retry" | "again" | "stop"> {
    const { store, clock } = this.deps
    const abort = new AbortController()
    this.connectAbort = abort
    store.dispatch({ type: "conn/connecting", attempt, at: clock.now() })
    try {
      const info = await this.transport.connect(abort.signal)
      if (this.stopped) return "stop"
      this.info = info
      this.coalescer.flush()
      store.dispatch({ type: "conn/ready", info, at: clock.now() })
      this.backoff.ready()
      this.deps.log?.event("conn.ready", { protocol: info.protocol, engine: info.engine })
      return info
    } catch (error) {
      if (this.stopped) return "stop"
      if (this.consumeReconnectRequest()) return "again"
      return (await this.afterFailure(asTransportError(error), attempt + 1)) ? "retry" : "stop"
    } finally {
      this.connectAbort = null
    }
  }

  /** Resync, then the event stream until it ends. "stop" when the loop must end. */
  private async consume(): Promise<"stop" | "again"> {
    await this.resync()
    try {
      for await (const event of this.transport.events()) this.coalescer.push(event)
      this.coalescer.flush()
      if (this.stopped || this.offline) return "stop"
      if (this.consumeReconnectRequest()) return "again"
      return (await this.afterFailure(new TransportError("closed", "the connection ended"), 1)) ? "again" : "stop"
    } catch (error) {
      this.coalescer.flush()
      if (this.stopped || this.offline) return "stop"
      if (this.consumeReconnectRequest()) return "again"
      return (await this.afterFailure(asTransportError(error), 1)) ? "again" : "stop"
    }
  }

  private consumeReconnectRequest(): boolean {
    if (!this.reconnectRequested) return false
    this.reconnectRequested = false
    return true
  }

  /** Fatal: offline (false). Otherwise conn/lost and the backoff wait (true unless stopped meanwhile). */
  private async afterFailure(error: TransportError, nextAttempt: number): Promise<boolean> {
    const { store, clock } = this.deps
    if (error.fatal) {
      this.goOffline(error)
      return false
    }
    const delay = Math.max(this.backoff.next(), error.retryAfterMs ?? 0)
    const resumable = this.info?.capabilities.resume ?? true
    store.dispatch({ type: "conn/lost", reason: error.message, attempt: nextAttempt, retryAt: clock.now() + delay, resumable, at: clock.now() })
    this.deps.log?.event("conn.lost", { code: error.code, status: error.status ?? null, delayMs: delay })
    await this.wait(delay)
    this.reconnectRequested = false
    return !this.stopped
  }

  private goOffline(error: TransportError): void {
    this.offline = true
    this.coalescer.flush()
    this.deps.store.dispatch({ type: "conn/offline", reason: error.message, fatal: true, at: this.deps.clock.now() })
    this.deps.log?.event("conn.offline", { code: error.code, status: error.status ?? null })
  }

  private wait(ms: number): Promise<void> {
    return new Promise<void>((resolve) => {
      const done = (): void => {
        this.clearTimer("waitTimer")
        this.wake = null
        resolve()
      }
      this.wake = done
      this.waitTimer = this.deps.clock.setTimeout(done, ms)
    })
  }

  private clearTimer(name: "waitTimer" | "settleTimer" | "tickTimer"): void {
    const handle = this[name]
    if (handle === null) return
    if (name === "tickTimer") this.deps.clock.clearInterval(handle)
    else this.deps.clock.clearTimeout(handle)
    this[name] = null
  }

  // ---- resync and the event tap ----

  private async runResync(): Promise<boolean> {
    const resync = this.transport.resync?.bind(this.transport)
    if (resync === undefined) return false
    try {
      const snapshot = await resync()
      if (this.stopped) return false
      this.coalescer.flush()
      const at = this.deps.clock.now()
      const wallAt = (this.deps.wallNow ?? Date.now)()
      this.deps.store.dispatch({ type: "resync/applied", snapshot, at, wallAt })
      return true
    } catch (error) {
      const failure = asTransportError(error)
      if (failure.fatal) this.reportFatal(failure)
      else if (failure.code !== "closed") this.notice("warn", `history resync failed: ${failure.message}`)
      return false
    }
  }

  private onEvent(event: TransportEvent, at: number): void {
    if (event.kind === "resync.required") void this.resync()
    if (event.kind === "turn.settle_hint" && event.signal !== "awaiting_approval") this.armSettleGrace()
    this.deps.onEvent?.(event, at)
  }

  private armSettleGrace(): void {
    if (this.settleTimer !== null || this.stopped) return
    this.settleTimer = this.deps.clock.setTimeout(() => {
      this.settleTimer = null
      const state = this.deps.store.getState()
      const head = state.inflight[0]
      const record = head === undefined ? undefined : state.turns[head]
      if (record !== undefined && !isSealed(record)) void this.resync()
    }, this.settleGraceMs)
  }

  private tick(): void {
    if (this.stopped) return
    const at = this.deps.clock.now()
    this.deps.store.dispatch({ type: "notice/tick", at })
    this.deps.onTick?.(at)
  }

  private notice(level: "info" | "warn" | "error", text: string): void {
    this.deps.store.dispatch({ type: "notice/push", notice: { level, source: "transport", text }, at: this.deps.clock.now() })
  }
}
