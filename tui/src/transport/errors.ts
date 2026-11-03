/**
 * Transport errors (design 3.7). Messages are built from HTTP status codes and server error codes only; they
 * never contain a token, a request body or message text. As a last line of defence the constructor also
 * masks bearer credentials, `token=` query values and Hex tokens should one slip into a message.
 */

export type TransportErrorCode =
  | "unreachable"
  | "auth"
  | "forbidden"
  | "permission_denied"
  | "not_found"
  | "bad_request"
  | "too_large"
  | "rate_limited"
  | "busy"
  | "unavailable"
  | "timeout"
  | "protocol"
  | "closed"
  | "unsupported"
  | "server"

/** Codes that are fatal unless the caller says otherwise: the supervisor goes offline instead of retrying. */
export const FATAL_TRANSPORT_CODES: ReadonlySet<TransportErrorCode> = new Set<TransportErrorCode>([
  "auth",
  "forbidden",
  "permission_denied",
])

export interface TransportErrorOptions {
  /** HTTP status, when the error came from a response. */
  status?: number
  /** Server- or client-chosen wait before a retry (429: 60 s; 503 "Too many connections": 30 s). */
  retryAfterMs?: number
  /** Overrides the default fatality of the code. */
  fatal?: boolean
}

export class TransportError extends Error {
  override readonly name = "TransportError"

  constructor(
    readonly code: TransportErrorCode,
    message: string,
    readonly opts: TransportErrorOptions = {},
  ) {
    super(maskCredentials(message))
  }

  get fatal(): boolean {
    return this.opts.fatal ?? FATAL_TRANSPORT_CODES.has(this.code)
  }

  get status(): number | undefined {
    return this.opts.status
  }

  get retryAfterMs(): number | undefined {
    return this.opts.retryAfterMs
  }
}

export function isTransportError(value: unknown): value is TransportError {
  return value instanceof TransportError
}

/**
 * Default code for an unexpected HTTP status. Adapters refine per route (3.8): an approval 404 means
 * "expired", a 503 on the event stream with "Too many connections" is `busy`, a 401 is retried once after
 * TokenSource.reload() before it becomes `auth`.
 */
export function codeForHttpStatus(status: number): TransportErrorCode {
  switch (status) {
    case 401:
      return "auth"
    case 403:
      return "forbidden"
    case 404:
      return "not_found"
    case 408:
    case 504:
      return "timeout"
    case 413:
      return "too_large"
    case 429:
      return "rate_limited"
    case 503:
      return "unavailable"
    default:
      if (status >= 500) return "server"
      if (status >= 400) return "bad_request"
      return "protocol"
  }
}

const BEARER = /\bBearer\s+[^\s"',;]+/gi
const TOKEN_QUERY = /([?&]token=)[^&\s"']+/gi
const HEX_TOKEN = /\bhex_[0-9a-f]{64}\b/gi

function maskCredentials(text: string): string {
  return text
    .replace(BEARER, "Bearer [redacted]")
    .replace(TOKEN_QUERY, "$1[redacted]")
    .replace(HEX_TOKEN, "hex_[redacted]")
}
