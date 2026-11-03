/**
 * Listener ownership (design 3.8.1, SEC-01). Tenant gateways listen on shared high ports; while a daemon is down
 * any other uid can bind its port, and a reconnecting client would hand that process its bearer token. Before every
 * token-bearing request the client therefore finds the LISTEN sockets that a connection to the URL can reach in
 * /proc/net/tcp and /proc/net/tcp6 and requires all of them to belong to the expected uid. The Hex sources run the
 * same check against the user's own uid (5.3.2).
 *
 * Residual race (accepted, 3.8.1): the listener can change between this check and the connect.
 */
import { readFile } from "node:fs/promises"
import { endianness } from "node:os"
import { TransportError } from "./errors.ts"

export interface ListenerRow {
  family: 4 | 6
  /** Dotted IPv4, or RFC 5952 IPv6 ("::1", "::", "::ffff:127.0.0.1"). */
  address: string
  port: number
  uid: number
}

export type ProcReader = (path: "/proc/net/tcp" | "/proc/net/tcp6") => Promise<string>

export interface ExpectedListener {
  uid: number
  /** Shown in the refusal, e.g. the tenant name or "this user". */
  label: string
}

/** procfs reports size 0, so a reader that trusts stat must not be used; readFile reads to EOF. */
export const readProcNet: ProcReader = (path) => readFile(path, "utf8")

const LISTEN_STATE = "0A"
const HEX = /^[0-9A-Fa-f]+$/
/** The kernel prints each 32-bit word of the address in host byte order (%08X). */
const HOST_LITTLE_ENDIAN = endianness() === "LE"

// ---------------------------------------------------------------------------------------------------------
// /proc/net/tcp parsing
// ---------------------------------------------------------------------------------------------------------

function procWordBytes(word: string): number[] {
  const n = Number.parseInt(word, 16)
  const bigEndian = [(n >>> 24) & 0xff, (n >>> 16) & 0xff, (n >>> 8) & 0xff, n & 0xff]
  return HOST_LITTLE_ENDIAN ? bigEndian.reverse() : bigEndian
}

function procAddressBytes(hex: string, family: 4 | 6): Uint8Array | null {
  if (!HEX.test(hex) || hex.length !== (family === 4 ? 8 : 32)) return null
  const bytes: number[] = []
  for (let i = 0; i < hex.length; i += 8) bytes.push(...procWordBytes(hex.slice(i, i + 8)))
  return Uint8Array.from(bytes)
}

function parseRow(line: string, family: 4 | 6): ListenerRow | null {
  const fields = line.trim().split(/\s+/)
  // sl local_address rem_address st tx:rx tr:tm retrnsmt uid ...
  const local = fields[1]
  const state = fields[3]
  const uidField = fields[7]
  if (local === undefined || state === undefined || uidField === undefined) return null
  if (state.toUpperCase() !== LISTEN_STATE || !/^\d+$/.test(uidField)) return null
  const colon = local.lastIndexOf(":")
  const portHex = local.slice(colon + 1)
  if (colon < 0 || portHex.length !== 4 || !HEX.test(portHex)) return null
  const bytes = procAddressBytes(local.slice(0, colon), family)
  if (!bytes) return null
  return { family, address: formatAddress(bytes), port: Number.parseInt(portHex, 16), uid: Number(uidField) }
}

/**
 * Rows in state 0A (LISTEN) only. IPv4 addresses are 8 hex digits in little-endian byte order
 * (0100007F = 127.0.0.1); IPv6 addresses are four 32-bit little-endian words
 * (00000000000000000000000001000000 = ::1, 0000000000000000FFFF00000100007F = ::ffff:127.0.0.1).
 * The header and malformed lines are skipped.
 */
export function parseProcNetTcp(text: string, family: 4 | 6): ListenerRow[] {
  const rows: ListenerRow[] = []
  for (const line of text.split("\n")) {
    const row = parseRow(line, family)
    if (row) rows.push(row)
  }
  return rows
}

// ---------------------------------------------------------------------------------------------------------
// IP address helpers
// ---------------------------------------------------------------------------------------------------------

/** "a.b.c.d" with each part 0-255 (no leading-zero octal forms). */
export function parseIpv4(text: string): Uint8Array | null {
  const parts = text.split(".")
  if (parts.length !== 4) return null
  const bytes = parts.map((part) => (/^(0|[1-9]\d{0,2})$/.test(part) ? Number(part) : Number.NaN))
  if (bytes.some((b) => !Number.isInteger(b) || b > 255)) return null
  return Uint8Array.from(bytes)
}

function parseIpv6Groups(text: string): number[] | null {
  if (text === "") return []
  const groups: number[] = []
  const parts = text.split(":")
  for (let i = 0; i < parts.length; i += 1) {
    const part = parts[i] ?? ""
    if (i === parts.length - 1 && part.includes(".")) {
      const v4 = parseIpv4(part)
      if (!v4) return null
      groups.push(((v4[0] ?? 0) << 8) | (v4[1] ?? 0), ((v4[2] ?? 0) << 8) | (v4[3] ?? 0))
    } else if (/^[0-9A-Fa-f]{1,4}$/.test(part)) {
      groups.push(Number.parseInt(part, 16))
    } else {
      return null
    }
  }
  return groups
}

/** IPv6 text with optional brackets ("[::1]", "::ffff:7f00:1", "fe80::1"); zone ids are not accepted. */
export function parseIpv6(text: string): Uint8Array | null {
  const bare = text.startsWith("[") && text.endsWith("]") ? text.slice(1, -1) : text
  const halves = bare.split("::")
  if (halves.length > 2) return null
  const head = parseIpv6Groups(halves[0] ?? "")
  const tail = halves.length === 2 ? parseIpv6Groups(halves[1] ?? "") : []
  if (!head || !tail) return null
  const missing = 8 - head.length - tail.length
  if (halves.length === 2 ? missing < 1 : missing !== 0) return null
  const groups = [...head, ...new Array<number>(missing).fill(0), ...tail]
  const bytes = new Uint8Array(16)
  groups.forEach((g, i) => {
    bytes[i * 2] = g >> 8
    bytes[i * 2 + 1] = g & 0xff
  })
  return bytes
}

const V4_MAPPED_PREFIX = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff]

function isV4Mapped(bytes: Uint8Array): boolean {
  return bytes.length === 16 && V4_MAPPED_PREFIX.every((b, i) => bytes[i] === b)
}

function v4Mapped(v4: Uint8Array): Uint8Array {
  return Uint8Array.from([...V4_MAPPED_PREFIX, ...v4])
}

function formatIpv6(bytes: Uint8Array): string {
  if (isV4Mapped(bytes)) return `::ffff:${Array.from(bytes.subarray(12)).join(".")}`
  const groups: number[] = []
  for (let i = 0; i < 16; i += 2) groups.push(((bytes[i] ?? 0) << 8) | (bytes[i + 1] ?? 0))
  // RFC 5952: compress the longest run of two or more zero groups (the first one on a tie).
  let bestStart = -1
  let bestLength = 1
  for (let i = 0; i < 8; ) {
    let j = i
    while (j < 8 && groups[j] === 0) j += 1
    if (j - i > bestLength) {
      bestStart = i
      bestLength = j - i
    }
    i = j === i ? i + 1 : j
  }
  const hex = groups.map((g) => g.toString(16))
  if (bestStart < 0) return hex.join(":")
  return `${hex.slice(0, bestStart).join(":")}::${hex.slice(bestStart + bestLength).join(":")}`
}

function formatAddress(bytes: Uint8Array): string {
  return bytes.length === 4 ? Array.from(bytes).join(".") : formatIpv6(bytes)
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((value, i) => value === b[i])
}

const IPV4_ANY = new Uint8Array(4)
const IPV6_ANY = new Uint8Array(16)

/** 127.0.0.0/8, ::1, ::ffff:127.0.0.0/104 and "localhost" (URL.hostname form, brackets allowed). */
export function isLoopbackHost(hostname: string): boolean {
  const host = hostname.toLowerCase()
  if (host === "localhost") return true
  const v4 = parseIpv4(host)
  if (v4) return v4[0] === 127
  const v6 = parseIpv6(host)
  if (!v6) return false
  if (isV4Mapped(v6)) return v6[12] === 127
  return sameBytes(v6, Uint8Array.from([...new Array<number>(15).fill(0), 1]))
}

// ---------------------------------------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------------------------------------

type Target = { family: 4 | 6; bytes: Uint8Array }

/** The addresses a connection to `hostname` can reach; null for a DNS name (every row on the port matches). */
function targetsFor(hostname: string): Target[] | null {
  const host = hostname.toLowerCase()
  if (host === "localhost") {
    return [
      { family: 4, bytes: Uint8Array.from([127, 0, 0, 1]) },
      { family: 6, bytes: Uint8Array.from([...new Array<number>(15).fill(0), 1]) },
    ]
  }
  const v4 = parseIpv4(host)
  if (v4) return [{ family: 4, bytes: v4 }]
  const v6 = parseIpv6(host)
  if (!v6) return null
  // A v4-mapped destination is an IPv4 connection to the mapped address.
  return [isV4Mapped(v6) ? { family: 4, bytes: v6.subarray(12) } : { family: 6, bytes: v6 }]
}

function rowBytes(row: ListenerRow): Uint8Array | null {
  return row.family === 4 ? parseIpv4(row.address) : parseIpv6(row.address)
}

/**
 * IPv4 destinations reach IPv4 rows on that address or 0.0.0.0, and IPv6 rows on its v4-mapped form or on ::
 * (dual-stack sockets: bindv6only cannot be read from procfs, so :: is assumed to accept IPv4). IPv6 destinations
 * reach IPv6 rows on that address or ::.
 */
function rowReaches(row: ListenerRow, target: Target): boolean {
  const bytes = rowBytes(row)
  if (!bytes) return false
  if (target.family === 4) {
    if (row.family === 4) return sameBytes(bytes, target.bytes) || sameBytes(bytes, IPV4_ANY)
    return sameBytes(bytes, v4Mapped(target.bytes)) || sameBytes(bytes, IPV6_ANY)
  }
  return row.family === 6 && (sameBytes(bytes, target.bytes) || sameBytes(bytes, IPV6_ANY))
}

/** The URL's port, with the scheme default when the URL has none. */
export function urlPort(url: URL): number {
  if (url.port !== "") return Number(url.port)
  return url.protocol === "https:" ? 443 : 80
}

function isMissing(error: unknown): boolean {
  return typeof error === "object" && error !== null && (error as { code?: unknown }).code === "ENOENT"
}

async function readTables(read: ProcReader): Promise<{ tcp: string; tcp6: string }> {
  const [tcp, tcp6] = await Promise.allSettled([read("/proc/net/tcp"), read("/proc/net/tcp6")])
  const unverifiable = (detail: string) =>
    new TransportError("unsupported", `cannot verify the listener owner: ${detail}`, { fatal: true })
  // No IPv6 in the kernel (or in the namespace) means no tcp6 table: nothing listens there.
  const v6 = tcp6.status === "fulfilled" ? tcp6.value : isMissing(tcp6.reason) ? "" : null
  if (v6 === null) throw unverifiable("/proc/net/tcp6 is not readable")
  if (tcp.status === "rejected") {
    throw unverifiable(isMissing(tcp.reason) ? "/proc/net/tcp is not available" : "/proc/net/tcp is not readable")
  }
  return { tcp: tcp.value, tcp6: v6 }
}

/** Uids owning a LISTEN socket that a connection to `url` can reach (sorted, unique). */
export async function listenerUids(url: URL, read: ProcReader = readProcNet): Promise<number[]> {
  const { tcp, tcp6 } = await readTables(read)
  const port = urlPort(url)
  const rows = [...parseProcNetTcp(tcp, 4), ...parseProcNetTcp(tcp6, 6)].filter((row) => row.port === port)
  const targets = targetsFor(url.hostname)
  const matching = targets === null ? rows : rows.filter((row) => targets.some((t) => rowReaches(row, t)))
  return [...new Set(matching.map((row) => row.uid))].sort((a, b) => a - b)
}

/**
 * No matching listener: TransportError("unreachable", "nothing listens on 127.0.0.1:10000") (retried, nothing
 * sent). Any matching listener of another uid: TransportError("forbidden", "port 10000 is held by uid 1008,
 * expected 1010 (derpy)", {fatal: true}).
 */
export async function assertListenerOwner(
  url: URL,
  expected: ExpectedListener,
  read: ProcReader = readProcNet,
): Promise<void> {
  const uids = await listenerUids(url, read)
  const port = urlPort(url)
  if (uids.length === 0) {
    throw new TransportError("unreachable", `nothing listens on ${url.hostname}:${port}`)
  }
  const foreign = uids.filter((uid) => uid !== expected.uid)
  if (foreign.length > 0) {
    const holder = foreign.length === 1 ? `uid ${foreign[0]}` : `uids ${foreign.join(", ")}`
    throw new TransportError(
      "forbidden",
      `port ${port} is held by ${holder}, expected ${expected.uid} (${expected.label})`,
      { fatal: true },
    )
  }
}
