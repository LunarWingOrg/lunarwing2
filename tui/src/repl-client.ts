/**
 * Unix-socket REPL client: NDJSON protocol, handshake with capability
 * negotiation, keepalive, and a callback surface for the UI.
 */

import { connect, type Socket } from "node:net"
import { homedir } from "node:os"
import { join } from "node:path"
import {
  STATUS_FEATURE,
  toWireAttachment,
  type PendingImage,
  type ReplMessage,
  type ReplStatusData,
  type ReplStatusKind,
} from "./protocol.ts"

const CLIENT_VERSION = "lunarwing-tui 0.1.0"
const KEEPALIVE_INTERVAL_MS = 30_000
const HANDSHAKE_TIMEOUT_MS = 10_000

export interface ReplClientHandlers {
  /** Welcome message after a successful handshake. */
  onWelcome?: (content: string, sessionId: string | null) => void
  /** Final assistant response for a turn (closes the stream). */
  onResponse?: (content: string) => void
  /** Live status frame (only delivered because we advertise "status"). */
  onStatus?: (kind: ReplStatusKind, content: string, data: ReplStatusData | undefined) => void
  /** Response with no open turn and no pending handshake (notices). */
  onInfo?: (content: string) => void
  /** Socket closed or handshake failed. */
  onDisconnect?: (reason: string) => void
}

/** Mirror of the Rust client's default socket resolution order. */
export function defaultSocketPath(): string {
  const env = (name: string): string | undefined => {
    const value = process.env[name]
    return value && value.length > 0 ? value : undefined
  }
  const runtimeDir = env("XDG_RUNTIME_DIR")
  const baseDir = env("LUNARWING_BASE_DIR") ?? env("IRONCLAW_BASE_DIR")
  return (
    env("LUNARWING_SOCKET") ??
    env("IRONCLAW_SOCKET") ??
    (runtimeDir ? join(runtimeDir, "lunarwing.sock") : undefined) ??
    (baseDir ? join(baseDir, "lunarwing.sock") : undefined) ??
    join(homedir(), ".ironclaw", "lunarwing.sock")
  )
}

export class ReplClient {
  private socket: Socket | null = null
  private lineBuffer = ""
  private keepalive: ReturnType<typeof setInterval> | null = null
  private closedByUs = false
  private handshake: {
    resolve: () => void
    reject: (error: Error) => void
  } | null = null
  sessionId: string | null = null

  constructor(
    private readonly socketPath: string,
    private readonly handlers: ReplClientHandlers,
  ) {}

  get connected(): boolean {
    return this.socket !== null && !this.socket.destroyed
  }

  async connect(): Promise<void> {
    const socket = await new Promise<Socket>((resolve, reject) => {
      const socket = connect(this.socketPath)
      socket.setNoDelay(true)
      socket.once("error", (error) => reject(new Error(error.message)))
      socket.once("connect", () => resolve(socket))
    })
    this.socket = socket

    socket.on("data", (chunk: Buffer) => this.onData(chunk))
    socket.on("error", (error: Error) => this.onSocketError(error))
    socket.once("close", () => this.onClose())

    const handshake = new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(() => {
        reject(new Error("handshake timed out waiting for welcome"))
      }, HANDSHAKE_TIMEOUT_MS)
      this.handshake = {
        resolve: () => {
          clearTimeout(timeout)
          resolve()
        },
        reject: (error) => {
          clearTimeout(timeout)
          reject(error)
        },
      }
    })

    this.sendRaw({
      Connect: {
        version: CLIENT_VERSION,
        client_info: "opentui",
        features: [STATUS_FEATURE],
      },
    })

    await handshake
    this.startKeepalive()
  }

  /** Send a user turn. Attachments are base64-encoded on the wire. */
  send(content: string, attachments: PendingImage[] = []): void {
    this.sendRaw({
      Message: {
        content,
        attachments: attachments.map(toWireAttachment),
      },
    })
  }

  /** Ask the daemon to abort the running turn (priority control). */
  interrupt(): void {
    this.sendRaw({ Message: { content: "/interrupt" } })
  }

  /** Graceful disconnect. */
  disconnect(reason = "client requested disconnect"): void {
    this.closedByUs = true
    this.stopKeepalive()
    if (this.connected) {
      this.sendRaw({ Disconnect: { reason } })
      this.socket?.end()
    }
  }

  // ── internals ──────────────────────────────────────────────────────

  private sendRaw(message: ReplMessage): void {
    this.socket?.write(`${JSON.stringify(message)}\n`)
  }

  private onData(chunk: Buffer): void {
    this.lineBuffer += chunk.toString("utf8")
    let newline = this.lineBuffer.indexOf("\n")
    while (newline !== -1) {
      const line = this.lineBuffer.slice(0, newline).trim()
      this.lineBuffer = this.lineBuffer.slice(newline + 1)
      if (line.length > 0) {
        this.handleLine(line)
      }
      newline = this.lineBuffer.indexOf("\n")
    }
  }

  private handleLine(line: string): void {
    let message: ReplMessage
    try {
      message = JSON.parse(line) as ReplMessage
    } catch {
      return // ignore malformed lines; the protocol is line-atomic
    }

    if (message === "Ping") {
      this.sendRaw("Pong")
      return
    }
    if (message === "Pong") {
      return
    }
    if ("Response" in message) {
      const { content, is_complete: isComplete } = message.Response
      const sessionId = message.Response.session_id ?? null
      if (sessionId !== null) {
        this.sessionId = sessionId
      }
      if (this.handshake !== null) {
        if (isComplete) {
          this.handlers.onWelcome?.(content, sessionId)
          const { resolve } = this.handshake
          this.handshake = null
          resolve()
        }
        return
      }
      if (isComplete) {
        this.handlers.onResponse?.(content)
      } else {
        this.handlers.onInfo?.(content)
      }
      return
    }
    if ("Status" in message) {
      this.handlers.onStatus?.(message.Status.kind, message.Status.content, message.Status.data)
      return
    }
    if ("Disconnect" in message) {
      this.handlers.onDisconnect?.(message.Disconnect.reason ?? "server disconnected")
      this.socket?.end()
    }
    // The server never sends Connect or Message frames.
  }

  private onSocketError(error: Error): void {
    if (this.handshake !== null) {
      const { reject } = this.handshake
      this.handshake = null
      reject(new Error(`connection failed: ${error.message}`))
      return
    }
    if (!this.closedByUs) {
      this.handlers.onDisconnect?.(`connection error: ${error.message}`)
    }
  }

  private onClose(): void {
    this.stopKeepalive()
    if (this.handshake !== null) {
      const { reject } = this.handshake
      this.handshake = null
      reject(new Error("connection closed during handshake"))
      return
    }
    if (!this.closedByUs) {
      this.handlers.onDisconnect?.("connection lost")
    }
  }

  private startKeepalive(): void {
    this.keepalive = setInterval(() => {
      if (this.connected) {
        this.send("Ping")
      }
    }, KEEPALIVE_INTERVAL_MS)
  }

  private stopKeepalive(): void {
    if (this.keepalive !== null) {
      clearInterval(this.keepalive)
      this.keepalive = null
    }
  }
}
