/**
 * Turn accumulation state machine for one assistant turn.
 *
 * HARD INVARIANT (lesson learned from earlier streaming clients): the done
 * frame CLOSES the stream, it never detonates it. Concretely:
 *
 * 1. `complete()` with an empty final payload must keep whatever
 *    `stream_chunk` deltas already accumulated — an empty done frame is a
 *    close, not a wipe.
 * 2. `complete()` with a non-empty payload adopts it as the canonical turn
 *    text (the daemon's final Response is authoritative).
 * 3. After `complete()`, the turn is sealed: late `stream_chunk` frames are
 *    discarded (return `false`) rather than resurrecting or mutating the
 *    sealed text.
 */

export type TurnPhase = "idle" | "streaming" | "done"

export class TurnBuffer {
  private accumulated = ""
  private sealedText: string | null = null
  private phase: TurnPhase = "idle"

  /** Open a turn (called when the user message is sent). Idempotent. */
  begin(): void {
    if (this.phase === "idle") {
      this.phase = "streaming"
      this.accumulated = ""
      this.sealedText = null
    }
  }

  /**
   * Append a streaming text delta. Returns true if accepted.
   * Rejected when no turn is open or the turn is already sealed.
   */
  appendChunk(text: string): boolean {
    if (this.phase === "done" || this.phase === "idle") {
      return false
    }
    this.accumulated += text
    return true
  }

  /**
   * Close the turn with the daemon's final payload.
   *
   * The final content is adopted as canonical only when non-empty; otherwise
   * the accumulated chunks ARE the turn text. Never wipes the buffer.
   */
  complete(finalContent: string): void {
    if (this.phase === "done") {
      // Already sealed — a duplicate done frame must not change anything.
      return
    }
    this.sealedText = finalContent.length > 0 ? finalContent : this.accumulated
    this.phase = "done"
  }

  /** Reset to idle for the next turn. */
  reset(): void {
    this.phase = "idle"
    this.accumulated = ""
    this.sealedText = null
  }

  /** Current visible text: sealed text when done, otherwise the live accumulation. */
  get text(): string {
    return this.sealedText ?? this.accumulated
  }

  get isStreaming(): boolean {
    return this.phase === "streaming"
  }

  get isDone(): boolean {
    return this.phase === "done"
  }

  get isIdle(): boolean {
    return this.phase === "idle"
  }
}
