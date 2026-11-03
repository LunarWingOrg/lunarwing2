/**
 * Scripted Bun.spawn stand-in (design 11.3.1) for parec, wl-paste, wl-copy, grim, slurp, notify-send, xdg-open
 * and hex. `fake.spawn` is a SpawnFn (src/config/types.ts): it records argv, env, options, stdin and kill
 * signals, and plays scripted stdout/stderr chunks, delays and an exit code on an injected Clock (pass the
 * test's ManualClock), so nothing real is ever started.
 *
 *   const fake = createFakeSpawn({ clock })
 *   fake.once(["wl-paste", "--list-types"], { stdout: ["image/png\ntext/plain\n"] })
 *   fake.on("grim", { stdout: [{ data: png, delayMs: 50 }], exitCode: 0 })
 *   fake.on("parec", { stayAlive: true, ignoreSignals: ["SIGINT"] })   // push chunks with proc.pushStdout()
 *   fake.on("hex", { stdout: [readyLine], stayAlive: true, exitOnStdinEnd: true })
 *
 * Unmatched commands throw ENOENT from spawn(), like a binary that is not installed. Bun semantics are kept:
 * a signal death resolves `exited` with 128 + n (SIGKILL -> 137), exitCode stays null, and `killed` becomes
 * true on any exit.
 */
import { constants } from "node:os"
import { basename } from "node:path"
import type { Clock, TimerHandle } from "@opentui/core"
import type { ChildProcess, ChildStdin, SpawnFn, SpawnOptions } from "../../src/config/types.ts"

export type FakeChunk = string | Uint8Array | { data: string | Uint8Array; delayMs: number }

export interface FakeProcessScript {
  /** Emitted in order; a chunk's delayMs is measured from the previous chunk of the same stream. */
  stdout?: readonly FakeChunk[]
  stderr?: readonly FakeChunk[]
  /** Exit code after the output (default 0); also used by exitOnStdinEnd. */
  exitCode?: number
  /** Wait after the last chunk before exiting. */
  exitDelayMs?: number
  /** Do not exit after the output: runs until killed, stdin ends (exitOnStdinEnd) or the spawn timeout fires. */
  stayAlive?: boolean
  /** Exit when stdin is ended or closed (the Hex embedded lease, 5.3.3). */
  exitOnStdinEnd?: boolean
  stdinEndExitDelayMs?: number
  /** Signals the child survives, e.g. ["SIGINT"] to exercise a SIGKILL escalation. */
  ignoreSignals?: readonly NodeJS.Signals[]
  /** spawn() throws this errno error, like a missing or non-executable binary. */
  spawnError?: "ENOENT" | "EACCES"
  pid?: number
}

/** A bare command name matches argv[0] or its basename; an array matches an argv prefix. */
export type ArgvMatcher = string | readonly string[] | RegExp | ((argv: readonly string[]) => boolean)

export interface FakeSpawnRequest {
  readonly argv: readonly string[]
  readonly env: Readonly<Record<string, string>>
  readonly options: Readonly<Omit<SpawnOptions, "env">>
}

/** A fixed script, or one chosen per request (e.g. by argv). */
export type ScriptSource = FakeProcessScript | ((request: FakeSpawnRequest) => FakeProcessScript)

export interface FakeSpawnCall extends FakeSpawnRequest {
  /** null when spawn() threw (spawnError or no matching script). */
  readonly process: FakeChildProcess | null
}

const encoder = new TextEncoder()
const decoder = new TextDecoder()

function toBytes(data: string | Uint8Array): Uint8Array {
  return typeof data === "string" ? encoder.encode(data) : data.slice()
}

function normalizeChunk(chunk: FakeChunk): { data: Uint8Array; delayMs: number } {
  if (typeof chunk === "string" || chunk instanceof Uint8Array) return { data: toBytes(chunk), delayMs: 0 }
  return { data: toBytes(chunk.data), delayMs: chunk.delayMs }
}

function signalName(signal: NodeJS.Signals | number): NodeJS.Signals | null {
  if (typeof signal === "string") return signal
  const entry = Object.entries(constants.signals).find(([, n]) => n === signal)
  return entry ? (entry[0] as NodeJS.Signals) : null
}

function signalNumber(name: NodeJS.Signals): number {
  return constants.signals[name] ?? 0
}

class OutputPipe {
  readonly stream: ReadableStream<Uint8Array>
  private controller: ReadableStreamDefaultController<Uint8Array> | null = null
  private open = true

  constructor() {
    this.stream = new ReadableStream<Uint8Array>({
      start: (controller) => {
        this.controller = controller
      },
      cancel: () => {
        this.open = false
      },
    })
  }

  push(data: Uint8Array): void {
    if (this.open) this.controller?.enqueue(data)
  }

  close(): void {
    if (!this.open) return
    this.open = false
    this.controller?.close()
  }
}

class FakeStdin implements ChildStdin {
  private readonly chunks: Uint8Array[] = []
  ended = false

  constructor(private readonly onEnd: () => void) {}

  write(chunk: string | Uint8Array): number {
    if (this.ended) return 0
    const bytes = toBytes(chunk)
    this.chunks.push(bytes)
    return bytes.length
  }

  flush(): number {
    return 0
  }

  end(): number {
    if (!this.ended) {
      this.ended = true
      this.onEnd()
    }
    return 0
  }

  record(bytes: Uint8Array): void {
    this.chunks.push(bytes.slice())
  }

  bytes(): Uint8Array {
    const total = this.chunks.reduce((n, c) => n + c.length, 0)
    const out = new Uint8Array(total)
    let offset = 0
    for (const c of this.chunks) {
      out.set(c, offset)
      offset += c.length
    }
    return out
  }
}

/** A scripted child. Implements ChildProcess; the extra methods drive it from a test. */
export class FakeChildProcess implements ChildProcess {
  readonly pid: number
  readonly stdin: ChildStdin | undefined
  readonly stdout: ReadableStream<Uint8Array> | undefined
  readonly stderr: ReadableStream<Uint8Array> | undefined
  readonly exited: Promise<number>
  /** Every signal passed to kill(), in order (also the ignored ones). */
  readonly kills: (NodeJS.Signals | number)[] = []

  private code: number | null = null
  private signal: NodeJS.Signals | null = null
  private done = false
  private resolveExit: (code: number) => void = () => undefined
  private readonly timers: TimerHandle[] = []
  private readonly out = new OutputPipe()
  private readonly err = new OutputPipe()
  private readonly input: FakeStdin

  constructor(
    readonly script: FakeProcessScript,
    private readonly options: SpawnOptions,
    private readonly clock: Clock,
    pid: number,
  ) {
    this.pid = script.pid ?? pid
    this.exited = new Promise<number>((resolve) => {
      this.resolveExit = resolve
    })
    this.input = new FakeStdin(() => this.onStdinEnd())
    this.stdin = options.stdin === "pipe" ? this.input : undefined
    this.stdout = options.stdout === "pipe" ? this.out.stream : undefined
    this.stderr = options.stderr === "pipe" ? this.err.stream : undefined
    this.start()
  }

  get exitCode(): number | null {
    return this.code
  }

  get signalCode(): NodeJS.Signals | null {
    return this.signal
  }

  get killed(): boolean {
    return this.done
  }

  /** True until the child exits. */
  get running(): boolean {
    return !this.done
  }

  kill(signal: NodeJS.Signals | number = "SIGTERM"): void {
    this.kills.push(signal)
    if (this.done || signal === 0) return
    const name = signalName(signal) ?? "SIGTERM"
    if (this.script.ignoreSignals?.includes(name)) return
    this.finish(null, name)
  }

  /** Emits stdout bytes now (a stayAlive child fed by the test, e.g. parec PCM chunks). */
  pushStdout(data: string | Uint8Array): void {
    if (!this.done) this.out.push(toBytes(data))
  }

  pushStderr(data: string | Uint8Array): void {
    if (!this.done) this.err.push(toBytes(data))
  }

  /** Exits now with `code` (default: the script's exitCode, else 0). */
  exit(code: number = this.script.exitCode ?? 0): void {
    this.finish(code, null)
  }

  /** Everything written to stdin, including a Uint8Array passed as `stdin`. */
  stdinBytes(): Uint8Array {
    return this.input.bytes()
  }

  stdinText(): string {
    return decoder.decode(this.input.bytes())
  }

  get stdinEnded(): boolean {
    return this.input.ended
  }

  private start(): void {
    const { options } = this
    if (options.stdin instanceof Uint8Array) {
      this.input.record(options.stdin)
      this.input.ended = true
    }
    if (options.timeout !== undefined) {
      this.timers.push(this.clock.setTimeout(() => this.kill(options.killSignal ?? "SIGTERM"), options.timeout))
    }
    if (options.signal) {
      const abort = () => this.kill(options.killSignal ?? "SIGTERM")
      if (options.signal.aborted) queueMicrotask(abort)
      else options.signal.addEventListener("abort", abort, { once: true })
    }
    let pending = 2
    const streamDone = () => {
      pending -= 1
      if (pending === 0) this.afterOutput()
    }
    this.play(this.script.stdout ?? [], this.out, streamDone)
    this.play(this.script.stderr ?? [], this.err, streamDone)
    if (options.stdin instanceof Uint8Array) this.onStdinEnd()
  }

  private play(chunks: readonly FakeChunk[], pipe: OutputPipe, onDone: () => void): void {
    let index = 0
    const next = (): void => {
      if (this.done) return
      const raw = chunks[index]
      if (raw === undefined) {
        onDone()
        return
      }
      index += 1
      const chunk = normalizeChunk(raw)
      this.after(chunk.delayMs, () => {
        pipe.push(chunk.data)
        next()
      })
    }
    next()
  }

  private afterOutput(): void {
    if (this.script.stayAlive) return
    this.after(this.script.exitDelayMs ?? 0, () => this.finish(this.script.exitCode ?? 0, null))
  }

  private onStdinEnd(): void {
    if (!this.script.exitOnStdinEnd) return
    this.after(this.script.stdinEndExitDelayMs ?? 0, () => this.finish(this.script.exitCode ?? 0, null))
  }

  /** Zero delays run as microtasks, so a ManualClock only has to advance for real delays. */
  private after(delayMs: number, run: () => void): void {
    const guarded = () => {
      if (!this.done) run()
    }
    if (delayMs > 0) this.timers.push(this.clock.setTimeout(guarded, delayMs))
    else queueMicrotask(guarded)
  }

  private finish(code: number | null, signal: NodeJS.Signals | null): void {
    if (this.done) return
    this.done = true
    this.code = code
    this.signal = signal
    for (const timer of this.timers) this.clock.clearTimeout(timer)
    this.timers.length = 0
    this.out.close()
    this.err.close()
    this.resolveExit(signal ? 128 + signalNumber(signal) : (code ?? 0))
  }
}

interface Rule {
  matcher: ArgvMatcher
  script: ScriptSource
  once: boolean
  used: boolean
}

function matches(matcher: ArgvMatcher, argv: readonly string[]): boolean {
  if (typeof matcher === "function") return matcher(argv)
  if (matcher instanceof RegExp) return matcher.test(argv.join(" "))
  const command = argv[0] ?? ""
  if (typeof matcher === "string") return command === matcher || basename(command) === matcher
  if (matcher.length === 0 || matcher.length > argv.length) return false
  return matcher.every((part, i) => {
    const arg = argv[i] ?? ""
    return i === 0 ? arg === part || basename(arg) === part : arg === part
  })
}

function errnoError(code: "ENOENT" | "EACCES", command: string): Error {
  const errno = code === "ENOENT" ? -2 : -13
  const text = code === "ENOENT" ? "no such file or directory" : "permission denied"
  return Object.assign(new Error(`${code}: ${text}, posix_spawn '${command}'`), {
    code,
    errno,
    syscall: "posix_spawn",
    path: command,
  })
}

/** Real timers, for tests that do not script delays; prefer passing the test's ManualClock. */
const realTimers: Clock = {
  now: () => Date.now(),
  setTimeout: (fn, ms) => setTimeout(fn, ms),
  clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
  setInterval: (fn, ms) => setInterval(fn, ms),
  clearInterval: (handle) => clearInterval(handle as ReturnType<typeof setInterval>),
}

export class FakeSpawn {
  readonly calls: FakeSpawnCall[] = []
  private readonly rules: Rule[] = []
  private fallback: ScriptSource = { spawnError: "ENOENT" }
  private nextPid = 41_000

  constructor(private readonly clock: Clock = realTimers) {}

  /** Uses `script` for every spawn whose argv matches (first registered rule wins). */
  on(matcher: ArgvMatcher, script: ScriptSource): this {
    this.rules.push({ matcher, script, once: false, used: false })
    return this
  }

  /** Like on(), but only for the first matching spawn; queue several to script successive runs. */
  once(matcher: ArgvMatcher, script: ScriptSource): this {
    this.rules.push({ matcher, script, once: true, used: false })
    return this
  }

  /** Script for unmatched spawns (default: throw ENOENT). */
  otherwise(script: ScriptSource): this {
    this.fallback = script
    return this
  }

  /** The SpawnFn to inject. */
  readonly spawn: SpawnFn = (argv, options) => this.run(argv, options)

  callsFor(matcher: ArgvMatcher): FakeSpawnCall[] {
    return this.calls.filter((call) => matches(matcher, call.argv))
  }

  get lastCall(): FakeSpawnCall | undefined {
    return this.calls.at(-1)
  }

  /** Children that have not exited (assert that nothing is orphaned). */
  running(): FakeChildProcess[] {
    return this.calls.flatMap((call) => (call.process?.running ? [call.process] : []))
  }

  /** SIGKILLs every running child. */
  killAll(): void {
    for (const proc of this.running()) proc.kill("SIGKILL")
  }

  private run(argv: readonly string[], options: SpawnOptions): ChildProcess {
    const frozenArgv = Object.freeze([...argv])
    const { env, ...rest } = options
    const record: FakeSpawnRequest = { argv: frozenArgv, env: Object.freeze({ ...env }), options: Object.freeze(rest) }
    const rule = this.rules.find((r) => !(r.once && r.used) && matches(r.matcher, frozenArgv))
    if (rule) rule.used = true
    const source = rule ? rule.script : this.fallback
    const script = typeof source === "function" ? source(record) : source
    if (script.spawnError) {
      this.calls.push({ ...record, process: null })
      throw errnoError(script.spawnError, frozenArgv[0] ?? "")
    }
    this.nextPid += 1
    const proc = new FakeChildProcess(script, options, this.clock, this.nextPid)
    this.calls.push({ ...record, process: proc })
    return proc
  }
}

export function createFakeSpawn(opts: { clock?: Clock } = {}): FakeSpawn {
  return new FakeSpawn(opts.clock)
}
