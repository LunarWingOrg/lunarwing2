/**
 * Replies the daemon gives to control inputs (design 3.5 "Known control acks", rule 5a). Used on legacy daemons,
 * whose replies carry no correlation id: an uncorrelated final whose text is a control ack belongs to the oldest
 * open control record, not to a user turn. Sources (V1 ic/src/agent/thread_ops.rs, V2 ic/src/bridge/router.rs):
 *
 * | ack                                        | control   | source                                   |
 * |--------------------------------------------|-----------|------------------------------------------|
 * | `Interrupted.`                             | interrupt | router.rs:55, thread_ops.rs:1146         |
 * | `Nothing to interrupt.`                    | interrupt | router.rs:57, thread_ops.rs:1148         |
 * | `Steer note queued ...`                    | steer     | thread_ops.rs:1171-1174, router.rs:3355  |
 * | `Steer note queue full ...`                | steer     | thread_ops.rs:1185 (V1 as "Error: ..."), router.rs:3376 |
 * | `Thread completed. Start a new thread ...` | steer     | thread_ops.rs:1178                       |
 * | `Thread cleared.`                          | clear     | thread_ops.rs:1258                       |
 * | `Conversation cleared.`                    | clear     | router.rs:3397                           |
 * | `New thread: <uuid>`                       | new       | thread_ops.rs:2129                       |
 * | `Started new conversation.`                | new       | router.rs:3388                           |
 */

/** The control a known ack answers (matches state/model.ts ControlKind). */
export type ControlAckKind = "interrupt" | "steer" | "clear" | "new"

export const INTERRUPTED_ACK = "Interrupted."
export const NOTHING_TO_INTERRUPT_ACK = "Nothing to interrupt."

/** V1 reports a full steer queue as an error reply (SubmissionResult::Error renders "Error: <message>"). */
const V1_ERROR_PREFIX = "Error: "

interface AckRule {
  readonly kind: ControlAckKind
  readonly exact?: string
  readonly prefix?: string
}

const ACK_RULES: readonly AckRule[] = [
  { kind: "interrupt", exact: INTERRUPTED_ACK },
  { kind: "interrupt", exact: NOTHING_TO_INTERRUPT_ACK },
  { kind: "steer", prefix: "Steer note queued" },
  { kind: "steer", prefix: "Steer note queue full" },
  { kind: "steer", prefix: "Thread completed. Start a new thread" },
  { kind: "clear", exact: "Thread cleared." },
  { kind: "clear", exact: "Conversation cleared." },
  { kind: "new", prefix: "New thread: " },
  { kind: "new", exact: "Started new conversation." },
]

/** Which control a reply acknowledges, or null for any other text. Tolerates surrounding whitespace and V1's "Error: ". */
export function controlAckKind(text: string): ControlAckKind | null {
  let body = text.trim()
  if (body.startsWith(V1_ERROR_PREFIX)) body = body.slice(V1_ERROR_PREFIX.length)
  for (const rule of ACK_RULES) {
    if (rule.exact !== undefined && body === rule.exact) return rule.kind
    if (rule.prefix !== undefined && body.startsWith(rule.prefix)) return rule.kind
  }
  return null
}

/** `isControlAck(text)` of design 3.5 rule 5a. */
export function isControlAck(text: string): boolean {
  return controlAckKind(text) !== null
}

/** "Interrupted." (rule 5a also marks the oldest running user turn interrupted; rule 5b swallows V1's second one). */
export function isInterruptedAck(text: string): boolean {
  return text.trim() === INTERRUPTED_ACK
}
