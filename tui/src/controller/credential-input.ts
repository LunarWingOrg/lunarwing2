/**
 * The masked credential buffer (design 3.6.4, SEC-03, UX-13).
 *
 * While the composer is in credential mode the global key and paste handlers call preventDefault() on everything
 * and feed this buffer instead of the textarea. The buffer lives outside the store: the store only ever sees its
 * length (composer/credential_chars), and the composer box shows `credential for <extension>: **** (N chars)`.
 *
 * The characters are kept as code points in a fixed Uint32Array that is zero-filled on clear(), on take() and
 * whenever credential mode ends or changes identity (bindCredentialInput). Only take() turns them into a string,
 * wrapped in a Secret right away; that is the one copy that leaves (the auth-token body). JS strings cannot be
 * zero-filled, so the Secret is disposed as soon as the request is built.
 */
import { Secret } from "../config/secret.ts"
import type { AppState, Store } from "../state/model.ts"

/** Generous for API tokens and passwords; anything longer is refused rather than silently cut. */
export const CREDENTIAL_MAX_CHARS = 4_096

export interface CredentialInput {
  /** Appends printable text; control characters and line breaks are dropped. False when it would not fit. */
  push(text: string): boolean
  /** Removes the last character; false when empty. */
  backspace(): boolean
  /** Zero-fills and empties the buffer. */
  clear(): void
  /** The buffer as a Secret; the buffer is zero-filled and empty afterwards. */
  take(): Secret
  readonly length: number
}

const DROPPED = /[\u0000-\u001f\u007f-\u009f\u2028\u2029]/u

export function createCredentialInput(capacity = CREDENTIAL_MAX_CHARS): CredentialInput {
  const buffer = new Uint32Array(Math.max(1, capacity))
  let length = 0

  const clear = (): void => {
    buffer.fill(0)
    length = 0
  }

  return {
    push(text) {
      const chars = Array.from(text).filter((char) => !DROPPED.test(char))
      if (length + chars.length > buffer.length) return false
      for (const char of chars) {
        buffer[length] = char.codePointAt(0) ?? 0
        length += 1
      }
      return true
    },
    backspace() {
      if (length === 0) return false
      length -= 1
      buffer[length] = 0
      return true
    },
    clear,
    take() {
      let value = ""
      for (let index = 0; index < length; index += 1) value += String.fromCodePoint(buffer[index] ?? 0)
      clear()
      return new Secret(value)
    },
    get length() {
      return length
    },
  }
}

type CredentialState = NonNullable<AppState["composer"]["credential"]>

function sameCredential(a: CredentialState | null, b: CredentialState | null): boolean {
  if (a === null || b === null) return a === b
  return a.extensionName === b.extensionName && a.turnId === b.turnId && a.startedAt === b.startedAt
}

/**
 * Zero-fills `input` whenever credential mode ends or a different request takes its place (T1.2: the reducer
 * also ends the mode on gate_resolved, an interrupt, /clear or /new). Returns the unsubscribe function.
 */
export function bindCredentialInput(store: Pick<Store, "getState" | "subscribe">, input: CredentialInput): () => void {
  let current = store.getState().composer.credential
  return store.subscribe(() => {
    const next = store.getState().composer.credential
    if (sameCredential(current, next)) {
      current = next
      return
    }
    current = next
    input.clear()
  })
}

/** What the composer box shows instead of the textarea. Never the secret. */
export function maskedCredentialLine(extensionName: string, chars: number): string {
  return chars === 0 ? `credential for ${extensionName}: (0 chars)` : `credential for ${extensionName}: **** (${chars} chars)`
}
