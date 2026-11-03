/**
 * Client-generated ids (design 4.1 correlation rules, 3.9). One UUIDv4 per user turn and per control; "t-<uuid>"
 * per transcription; one REPLv3 thread token per process. Every id satisfies the servers' shared rule
 * ^[A-Za-z0-9_-]{1,64}$ (lunarwing_common::is_valid_client_id) and is opaque: never parsed, never a secret.
 */
import type { TurnId } from "./model.ts"

/** Max length of every client-chosen id: client_turn_id, msg_id, req_id and the REPLv3 thread token. */
export const CLIENT_ID_MAX_LEN = 64

const CLIENT_ID_PATTERN = /^[A-Za-z0-9_-]{1,64}$/

/** Mirrors lunarwing_common::is_valid_client_id. */
export function isValidClientId(id: unknown): id is string {
  return typeof id === "string" && CLIENT_ID_PATTERN.test(id)
}

/** A new UUIDv4 TurnId, created before the request is written (3.5 rule 2). */
export function newTurnId(): TurnId {
  return crypto.randomUUID() as TurnId
}

/** Brands an id echoed by a server (client_turn_id, msg_id); null when it breaks the id rule. */
export function asTurnId(id: unknown): TurnId | null {
  return isValidClientId(id) ? (id as TurnId) : null
}

/** Transcription request id: "t-" + UUIDv4 (38 characters). */
export function newTranscribeRequestId(): string {
  return `t-${crypto.randomUUID()}`
}

export type RandomFill = (bytes: Uint8Array) => Uint8Array

/**
 * The REPLv3 thread token "tui-<profile>-<16 hex>" (F3), minted once per process and reused only for this
 * process's reconnects. `fill` defaults to crypto.getRandomValues; tests inject a deterministic one.
 * Throws RangeError when the profile would make the token invalid (profiles are ^[a-z0-9_-]{1,32}$).
 */
export function newReplThreadToken(profile: string, fill: RandomFill = (bytes) => crypto.getRandomValues(bytes)): string {
  const bytes = fill(new Uint8Array(8))
  if (bytes.length !== 8) throw new RangeError("random fill must return 8 bytes")
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("")
  const token = `tui-${profile}-${hex}`
  if (!isValidClientId(token)) throw new RangeError("profile name is not usable in a REPLv3 thread token")
  return token
}

/** Controllers take a factory so tests can inject deterministic ids. */
export type TurnIdFactory = () => TurnId

/** Deterministic ids "<prefix>-1", "<prefix>-2", ... for tests. */
export function createTurnIdSequence(prefix = "turn"): TurnIdFactory {
  if (!isValidClientId(`${prefix}-1`)) throw new RangeError("invalid TurnId prefix")
  let n = 0
  return () => {
    n += 1
    const id = `${prefix}-${n}`
    if (!isValidClientId(id)) throw new RangeError("TurnId sequence exhausted")
    return id as TurnId
  }
}
