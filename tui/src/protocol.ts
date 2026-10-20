/**
 * Wire protocol mirror of the daemon's
 * `ic/src/channels/unix_socket_repl.rs` `ReplMessage` enum.
 *
 * Newline-delimited JSON, one message per line, Rust serde externally-tagged
 * enum shape: `{"Connect":{...}}`, unit variants as bare strings ("Ping").
 *
 * Compatibility contract (must stay in sync with the Rust side):
 * - `features` and `attachments` are optional; legacy peers omit them.
 * - `Status` frames are only delivered to clients that advertised the
 *   `"status"` feature in their `Connect` handshake.
 */

export const STATUS_FEATURE = "status" as const

export type ReplStatusKind =
  | "thinking"
  | "tool_started"
  | "tool_completed"
  | "tool_result"
  | "stream_chunk"
  | "status"
  | "image_generated"

/** Attachment sent to the daemon, base64-encoded on the wire. */
export interface ReplAttachment {
  mime_type: string
  filename?: string | null
  data_base64: string
}

export interface ReplStatusData {
  success?: boolean
  error?: string | null
  parameters?: string | null
  name?: string
  data_url?: string
  path?: string | null
  [key: string]: unknown
}

export type ReplMessage =
  | { Connect: { version: string; client_info?: string | null; features?: string[] } }
  | { Message: { content: string; session_id?: string | null; attachments?: ReplAttachment[] } }
  | { Response: { content: string; session_id?: string | null; is_complete: boolean } }
  | { Status: { kind: ReplStatusKind; content: string; data?: ReplStatusData; session_id?: string | null } }
  | { Disconnect: { session_id?: string | null; reason?: string | null } }
  | "Ping"
  | "Pong"

/** An image held in memory, ready to render or attach. */
export interface PendingImage {
  /** Stable local id. */
  id: string
  mime_type: string
  filename: string | null
  bytes: Uint8Array
}

export function toWireAttachment(image: PendingImage): ReplAttachment {
  return {
    mime_type: image.mime_type,
    filename: image.filename,
    data_base64: Buffer.from(image.bytes).toString("base64"),
  }
}
