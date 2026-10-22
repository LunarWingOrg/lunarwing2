/**
 * Main application component: owns the ReplClient lifecycle, the turn
 * buffer (stream accumulation), pending attachments, and key handling.
 *
 * Input model:
 * - Enter            → send; the daemon queues mid-turn messages natively
 * - Ctrl+Enter       → interrupt the running turn, then send (hard steer)
 * - Ctrl+V           → paste image (or text) from the system clipboard
 * - /img <path>      → attach an image file from disk
 * - Ctrl+R / Ctrl+X  → remove last / clear pending attachments
 * - Ctrl+C           → quit
 *
 * HARD INVARIANT: the done frame (final Response) closes the turn, it never
 * wipes accumulated stream output. See turn-buffer.ts.
 */

import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react"
import { useKeyboard, useRenderer } from "@opentui/react"
import { ReplClient } from "./repl-client.ts"
import { TurnBuffer } from "./turn-buffer.ts"
import { attachFile, readClipboard, type ImageProtocol } from "./images.ts"
import { decodeDataUrl, truncate } from "./util.ts"
import type { PendingImage, ReplStatusKind } from "./protocol.ts"
import { AttachmentBar } from "./components/attachment-bar.tsx"
import { Transcript, type LiveTurnView, type TranscriptImage, type TranscriptItem } from "./components/transcript.tsx"

let itemCounter = 0
const nextId = (prefix: string): string => {
  itemCounter += 1
  return `${prefix}-${itemCounter}`
}

export function App(props: {
  socketPath: string
  agentName: string
  /** Requested image backend; `auto` lets the renderer negotiate. */
  imageProtocol?: ImageProtocol
}): ReactNode {
  const renderer = useRenderer()
  const requestedImageProtocol = props.imageProtocol ?? "auto"
  const clientRef = useRef<ReplClient | null>(null)
  const turnRef = useRef(new TurnBuffer())
  // Images generated during the current turn. Held in a ref (not state) so the
  // socket callbacks below, which close over the mount-time render, still see
  // the live list. Drained into the transcript item when the turn seals.
  const liveImagesRef = useRef<TranscriptImage[]>([])

  const [connected, setConnected] = useState(false)
  const [inputValue, setInputValue] = useState("")
  // The <input> is uncontrolled between our own mutations; `seed`/`epoch`
  // remount it only when WE change the text (paste insertion, submit clear)
  // so streaming re-renders never disturb typed text.
  const [inputSeed, setInputSeed] = useState("")
  const [inputEpoch, setInputEpoch] = useState(0)
  const [items, setItems] = useState<TranscriptItem[]>([])
  const [pending, setPending] = useState<PendingImage[]>([])
  const [live, setLive] = useState<LiveTurnView | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const busyRef = useRef(false)

  const setBusyState = (value: boolean): void => {
    busyRef.current = value
    setBusy(value)
  }

  const pushItem = useCallback((item: Omit<TranscriptItem, "id">): void => {
    setItems((previous) => [...previous, { ...item, id: nextId(item.kind) }])
  }, [])

  const reseedInput = useCallback((text: string): void => {
    setInputSeed(text)
    setInputValue(text)
    setInputEpoch((epoch) => epoch + 1)
  }, [])

  // ── Client lifecycle ────────────────────────────────────────────────
  useEffect(() => {
    const client = new ReplClient(props.socketPath, {
      onWelcome: (content) => {
        pushItem({ kind: "info", text: content })
        setConnected(true)
      },
      onResponse: (content) => {
        const turn = turnRef.current
        if (turn.isIdle) {
          // Spontaneous response outside a turn (proactive notices).
          pushItem({ kind: "assistant", text: content })
          return
        }
        // The done frame CLOSES the stream — never wipes it.
        turn.complete(content)
        // Generated images belong to the sealed turn: carry them into the
        // transcript item instead of letting setLive(null) discard them.
        const images = liveImagesRef.current
        liveImagesRef.current = []
        pushItem({
          kind: "assistant",
          text: turn.text,
          images: images.length > 0 ? images : undefined,
        })
        turn.reset()
        setLive(null)
        setBusyState(false)
      },
      onStatus: (kind, content, data) => {
        if (kind === "stream_chunk") {
          // Late chunks after the turn sealed are dropped by the buffer.
          turnRef.current.appendChunk(content)
          setLive((previous) =>
            previous === null
              ? { text: turnRef.current.text, statusKind: "idle", statusText: "", images: [] }
              : { ...previous, text: turnRef.current.text },
          )
          return
        }
        if (kind === "image_generated" && data?.data_url !== undefined) {
          const bytes = decodeDataUrl(String(data.data_url))
          if (bytes !== null) {
            const image: TranscriptImage = {
              id: nextId("gen"),
              bytes,
              // The daemon sends the file path (or "generated image") as the
              // frame content, so it doubles as the caption.
              caption: content.length > 0 ? content : undefined,
            }
            if (turnRef.current.isIdle) {
              // No turn open (e.g. a late frame after the seam): land it in the
              // transcript rather than dropping it on the floor.
              pushItem({ kind: "assistant", text: "", images: [image] })
            } else {
              liveImagesRef.current = [...liveImagesRef.current, image]
              const images = liveImagesRef.current
              setLive((previous) =>
                previous === null
                  ? { text: turnRef.current.text, statusKind: "idle", statusText: "", images }
                  : { ...previous, images },
              )
            }
          }
          return
        }
        setLive((previous) => ({
          text: turnRef.current.text,
          statusKind: kind,
          statusText: statusLine(kind, content, data),
          images: previous?.images ?? [],
        }))
      },
      onInfo: (content) => {
        pushItem({ kind: "info", text: content })
      },
      onDisconnect: (reason) => {
        setConnected(false)
        const turn = turnRef.current
        const images = liveImagesRef.current
        liveImagesRef.current = []
        if (turn.isStreaming && (turn.text.length > 0 || images.length > 0)) {
          // Connection dropped mid-stream: seal what we have, never wipe.
          turn.complete("")
          pushItem({
            kind: "assistant",
            text: turn.text,
            images: images.length > 0 ? images : undefined,
          })
          turn.reset()
        }
        setLive(null)
        setBusyState(false)
        pushItem({ kind: "error", text: `disconnected: ${reason}` })
      },
    })

    clientRef.current = client
    client.connect().catch((error: unknown) => {
      pushItem({
        kind: "error",
        text: `failed to connect: ${error instanceof Error ? error.message : String(error)}`,
      })
    })

    return () => {
      client.disconnect()
      clientRef.current = null
    }
  }, [props.socketPath, pushItem])

  // ── Submission ──────────────────────────────────────────────────────
  const submit = useCallback(
    (mode: { interrupt: boolean }): void => {
      const client = clientRef.current
      if (client === null || !client.connected) {
        setNotice("not connected — start the daemon (`lunarwing run`)")
        return
      }
      const text = inputValue.trim()
      if (text.length === 0 && pending.length === 0) {
        return
      }

      // /img <path>: attach instead of sending.
      if (text.startsWith("/img ") || text.startsWith("/attach ")) {
        const path = text.slice(text.indexOf(" ") + 1)
        const image = attachFile(path)
        if (image === null) {
          setNotice(`cannot read image: ${path}`)
        } else {
          setPending((previous) => [...previous, image])
          setNotice(null)
        }
        reseedInput("")
        return
      }

      const wasBusy = busyRef.current
      if (mode.interrupt && wasBusy) {
        client.interrupt()
        pushItem({ kind: "info", text: "— interrupted running turn —" })
      }

      client.send(text, pending)
      pushItem({
        kind: wasBusy && !mode.interrupt ? "queued" : "user",
        text: text.length > 0 ? text : "(image)",
        // Show what was actually sent: an image that vanishes behind a bare
        // "(image)" label is unhelpful when you want to review the turn later.
        images:
          pending.length > 0
            ? pending.map((image) => ({
                id: image.id,
                bytes: image.bytes,
                caption: image.filename ?? undefined,
              }))
            : undefined,
      })
      setPending([])
      reseedInput("")
      setNotice(null)

      liveImagesRef.current = []
      turnRef.current.begin()
      setBusyState(true)
      setLive({ text: "", statusKind: "idle", statusText: "", images: [] })
    },
    [inputValue, pending, pushItem, reseedInput],
  )

  // ── Key handling ────────────────────────────────────────────────────
  useKeyboard((key) => {
    const is = (name: string, ctrl = false): boolean =>
      key.name === name && Boolean(key.ctrl) === ctrl && !key.meta && !key.shift

    if (is("c", true)) {
      clientRef.current?.disconnect("quit")
      renderer.destroy()
      return
    }
    // Interrupt + send. Ctrl+Enter needs the kitty keyboard protocol to be
    // distinguishable from plain Enter; Ctrl+S works on every terminal.
    if (is("return", true) || is("s", true)) {
      submit({ interrupt: true })
      return
    }
    if (is("return")) {
      submit({ interrupt: false })
      return
    }
    if (is("v", true)) {
      const clip = readClipboard()
      if (clip.kind === "image") {
        setPending((previous) => [...previous, clip.image])
        setNotice(null)
      } else if (clip.kind === "text") {
        reseedInput(inputValue.length === 0 ? clip.text : `${inputValue} ${clip.text}`)
      } else {
        setNotice(clip.reason)
      }
      return
    }
    if (is("r", true)) {
      setPending((previous) => previous.slice(0, -1))
      return
    }
    if (is("x", true)) {
      setPending([])
    }
  })

  const liveWithText = useMemo<LiveTurnView | null>(
    () => (live === null ? null : { ...live, text: turnRef.current.text }),
    [live],
  )

  const statusColor = connected ? "#9ece6a" : "#f7768e"
  const stateLabel = !connected ? "offline" : busy ? "busy" : "ready"
  const historyHeight = renderer.height - 7 - (pending.length > 0 ? 9 : 0)
  // Which image protocol the renderer resolved for this terminal. OpenTUI
  // resolves "auto" itself, so surfacing the effective choice makes a silent
  // fallback to unicode blocks obvious instead of mysterious.
  const imageProtocol = resolvedImageProtocol(requestedImageProtocol, renderer.capabilities)
  const imageColor = imageProtocol === "kitty" ? "#9ece6a" : "#565f89"
  // The header is a single row: budget the fixed labels first and truncate the
  // socket path into whatever is left, so a narrow terminal never eats the
  // agent name or the connection state.
  const fixedHeaderWidth =
    props.agentName.length + 2 + 2 + stateLabel.length +
    (imageProtocol === null ? 0 : 5 + imageProtocol.length)
  const socketRoom = Math.max(renderer.width - fixedHeaderWidth - 2, 0)
  const socketLabel = socketRoom >= 4 ? truncate(props.socketPath, socketRoom) : ""

  return (
    <box style={{ flexDirection: "column", width: "100%", height: "100%" }}>
      <box style={{ flexDirection: "row", height: 1 }}>
        <text fg="#7aa2f7"> {props.agentName} </text>
        <text fg={statusColor}>● {stateLabel}</text>
        {imageProtocol !== null && <text fg={imageColor}> {`img:${imageProtocol}`}</text>}
        {socketLabel.length > 0 && <text fg="#565f89"> {socketLabel}</text>}
      </box>

      <Transcript
        items={items}
        live={liveWithText}
        height={historyHeight}
        width={renderer.width}
        protocol={requestedImageProtocol}
      />

      {notice !== null && <text fg="#e0af68"> {notice}</text>}

      <AttachmentBar images={pending} protocol={requestedImageProtocol} />

      <box
        title={busy ? "Message (Enter=queue · Ctrl+Enter/Ctrl+S=interrupt & send)" : "Message"}
        style={{ border: true, height: 3, flexDirection: "row" }}
      >
        <input
          key={inputEpoch}
          value={inputSeed}
          placeholder="Message LunarWing… (/img <path> to attach, Ctrl+V paste)"
          focused
          onInput={setInputValue}
        />
      </box>

      <box style={{ flexDirection: "row", height: 1 }}>
        <text fg="#565f89">
          {" Enter send/queue · Ctrl+Enter or Ctrl+S interrupt+send · Ctrl+V paste · Ctrl+R/Ctrl+X attachments · Ctrl+C quit"}
        </text>
      </box>
    </box>
  )
}

/**
 * Effective image protocol for this terminal.
 *
 * Mirrors OpenTUI's resolution: an explicit request wins, then a declared
 * `image_protocol` capability, and `auto` falls back through kitty → sixel →
 * unicode blocks. A multiplexer downgrades to blocks (graphics escapes can be
 * swallowed) unless a protocol is forced.
 */
function resolvedImageProtocol(
  requested: ImageProtocol,
  capabilities: {
    image_protocol?: string
    kitty_graphics: boolean
    sixel: boolean
    multiplexer: string
  } | null,
): string | null {
  if (requested !== "auto") {
    return requested
  }
  if (capabilities === null) {
    return null
  }
  const declared = capabilities.image_protocol
  if (declared !== undefined && declared !== "auto") {
    return declared
  }
  if (capabilities.multiplexer === "tmux") {
    return "blocks"
  }
  if (capabilities.kitty_graphics) {
    return "kitty"
  }
  if (capabilities.sixel) {
    return "sixel"
  }
  return "blocks"
}

/** Human-readable single-line rendering of a status frame. */
function statusLine(
  kind: ReplStatusKind,
  content: string,
  data: { success?: boolean; error?: string | null; name?: string } | undefined,
): string {
  switch (kind) {
    case "thinking":
      return `thinking: ${content}`
    case "tool_started":
      return `running ${content}…`
    case "tool_completed":
      return data?.success === false
        ? `${content} failed${data.error ? `: ${data.error}` : ""}`
        : `${content} done`
    case "tool_result":
      return `${data?.name ?? "tool"}: ${content}`
    case "status":
      return content
    default:
      return ""
  }
}
