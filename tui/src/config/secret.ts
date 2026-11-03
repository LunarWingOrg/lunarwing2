/**
 * Secret wrapper for gateway tokens, Hex tokens and typed credentials (design 3.11.3, SEC-03).
 *
 * The value never lives on the object: it sits in a module-private WeakMap, so inspectors, serializers,
 * structuredClone and Object.keys cannot reach it. Every implicit conversion yields "[redacted]":
 * String(), template literals, string concatenation, toString(), JSON.stringify (toJSON), console.log,
 * Bun.inspect and node:util inspect (Bun.inspect.custom and util.inspect.custom are the same symbol).
 * The only way to read the value is an explicit expose() (or its alias reveal()), called only where the
 * Authorization header or the auth-token request body is built.
 */
import { timingSafeEqual } from "node:crypto"
import { inspect } from "node:util"

export const REDACTED = "[redacted]"

const values = new WeakMap<Secret, string>()
const utf8 = new TextEncoder()

export class Secret {
  constructor(value: string) {
    values.set(this, value)
    Object.freeze(this)
  }

  /** The secret value. Call only where it leaves the process (Authorization header, auth-token body). */
  expose(): string {
    const value = values.get(this)
    if (value === undefined) throw new Error("secret was disposed")
    return value
  }

  /** Alias of expose(). */
  reveal(): string {
    return this.expose()
  }

  /** False once dispose() ran. */
  get disposed(): boolean {
    return !values.has(this)
  }

  /** Drops the value; later expose() calls throw. JS strings cannot be zero-filled, so this only ends the reference. */
  dispose(): void {
    values.delete(this)
  }

  /** Constant-time comparison of two secrets (e.g. TokenSource.reload() detecting a rotated token). */
  equals(other: Secret): boolean {
    const a = values.get(this)
    const b = values.get(other)
    if (a === undefined || b === undefined) return false
    const left = utf8.encode(a)
    const right = utf8.encode(b)
    return left.length === right.length && timingSafeEqual(left, right)
  }

  toString(): string {
    return REDACTED
  }

  toJSON(): string {
    return REDACTED
  }

  [Symbol.toPrimitive](): string {
    return REDACTED
  }

  [inspect.custom](): string {
    return REDACTED
  }
}

export function isSecret(value: unknown): value is Secret {
  return value instanceof Secret
}
