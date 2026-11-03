import { beforeAll, describe, expect, test } from "bun:test"
import { readFile } from "node:fs/promises"
import { join } from "node:path"
import { isTransportError, type TransportError } from "../../src/transport/errors.ts"
import {
  assertListenerOwner,
  isLoopbackHost,
  listenerUids,
  parseIpv6,
  parseProcNetTcp,
  type ProcReader,
} from "../../src/transport/listener-owner.ts"

const FIXTURES = join(import.meta.dir, "..", "fixtures", "proc-net")
const fixtures: Record<string, string> = {}

beforeAll(async () => {
  for (const name of ["tcp-tenants.txt", "tcp6-loopback.txt", "tcp-foreign.txt"]) {
    fixtures[name] = await readFile(join(FIXTURES, name), "utf8")
  }
})

function enoent(path: string): Error {
  return Object.assign(new Error(`ENOENT: no such file or directory, open '${path}'`), { code: "ENOENT" })
}

/** A ProcReader serving fixture files; a missing table rejects with ENOENT like readFile. */
function reader(tcp: string | null, tcp6: string | null): ProcReader {
  return async (path) => {
    const name = path === "/proc/net/tcp" ? tcp : tcp6
    if (name === null) throw enoent(path)
    const text = fixtures[name]
    if (text === undefined) throw new Error(`fixture ${name} not loaded`)
    return text
  }
}

async function refusal(promise: Promise<unknown>): Promise<TransportError> {
  try {
    await promise
  } catch (error) {
    if (isTransportError(error)) return error
    throw error
  }
  throw new Error("expected a TransportError")
}

const url = (text: string) => new URL(text)

describe("parseProcNetTcp (3.8.1)", () => {
  test("IPv4 rows: LISTEN only, little-endian addresses, hex ports, uids", () => {
    const rows = parseProcNetTcp(fixtures["tcp-tenants.txt"] ?? "", 4)
    expect(rows).toEqual([
      { family: 4, address: "0.0.0.0", port: 10000, uid: 1010 },
      { family: 4, address: "0.0.0.0", port: 10010, uid: 1009 },
      { family: 4, address: "0.0.0.0", port: 10020, uid: 1011 },
      { family: 4, address: "0.0.0.0", port: 10030, uid: 1017 },
      { family: 4, address: "0.0.0.0", port: 10050, uid: 1008 },
      { family: 4, address: "127.0.0.1", port: 8080, uid: 1000 },
    ])
  })

  test("IPv6 rows: ::1, the wildcard, v4-mapped and link-local addresses", () => {
    const rows = parseProcNetTcp(fixtures["tcp6-loopback.txt"] ?? "", 6)
    expect(rows).toEqual([
      { family: 6, address: "::1", port: 8081, uid: 1000 },
      { family: 6, address: "::", port: 8082, uid: 1000 },
      { family: 6, address: "::ffff:127.0.0.1", port: 8083, uid: 1000 },
      { family: 6, address: "::1", port: 10000, uid: 1010 },
      { family: 6, address: "fe80::1", port: 8085, uid: 1000 },
    ])
  })

  test("the header, blank and malformed lines are skipped", () => {
    const text = [
      "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode",
      "",
      "   0: XYZ:2710 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1010",
      "   1: 00000000:271 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1010",
      "   2: 00000000:2710 00000000:0000 0A 00000000:00000000 00:00000000 00000000  uid",
      "   3: 0100007F:0050 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 1 1",
      "garbage",
    ].join("\n")
    expect(parseProcNetTcp(text, 4)).toEqual([{ family: 4, address: "127.0.0.1", port: 80, uid: 1000 }])
  })
})

describe("listenerUids and assertListenerOwner (3.8.1)", () => {
  const tenants = () => reader("tcp-tenants.txt", "tcp6-loopback.txt")

  test("IPv4 wildcard rows match a loopback URL", async () => {
    expect(await listenerUids(url("http://127.0.0.1:10000"), tenants())).toEqual([1010])
    await assertListenerOwner(url("http://127.0.0.1:10000/"), { uid: 1010, label: "derpy" }, tenants())
  })

  test("an IPv4 loopback row matches 127.0.0.1 but not another loopback address", async () => {
    expect(await listenerUids(url("http://127.0.0.1:8080"), tenants())).toEqual([1000])
    expect(await listenerUids(url("http://127.0.0.2:8080"), tenants())).toEqual([])
  })

  test("only LISTEN rows count (an accepted connection on the port by another uid is ignored)", async () => {
    expect(await listenerUids(url("http://127.0.0.1:10000"), reader("tcp-tenants.txt", null))).toEqual([1010])
  })

  test("IPv6 ::1 matches [::1] and not an IPv4 destination", async () => {
    expect(await listenerUids(url("http://[::1]:8081"), tenants())).toEqual([1000])
    expect(await listenerUids(url("http://127.0.0.1:8081"), tenants())).toEqual([])
  })

  test("the IPv6 wildcard accepts IPv6 and (dual-stack) IPv4 destinations", async () => {
    expect(await listenerUids(url("http://[::1]:8082"), tenants())).toEqual([1000])
    expect(await listenerUids(url("http://127.0.0.1:8082"), tenants())).toEqual([1000])
  })

  test("a v4-mapped row matches the IPv4 URL and the v4-mapped URL", async () => {
    expect(await listenerUids(url("http://127.0.0.1:8083"), tenants())).toEqual([1000])
    expect(await listenerUids(url("http://[::ffff:127.0.0.1]:8083"), tenants())).toEqual([1000])
  })

  test("an IPv4 0.0.0.0 listener is not reachable through an IPv6 destination", async () => {
    expect(await listenerUids(url("http://[::1]:10020"), tenants())).toEqual([])
  })

  test("localhost matches both 127.0.0.1 and ::1", async () => {
    expect(await listenerUids(url("http://localhost:8081"), tenants())).toEqual([1000])
    expect(await listenerUids(url("http://localhost:8080"), tenants())).toEqual([1000])
    expect(await listenerUids(url("http://localhost:10000"), tenants())).toEqual([1010])
  })

  test("a DNS name matches every listener on the port", async () => {
    expect(await listenerUids(url("http://gateway.example:10000"), tenants())).toEqual([1010])
  })

  test("a foreign uid on the port is refused, fatal, with the uid message", async () => {
    const error = await refusal(
      assertListenerOwner(url("http://127.0.0.1:10000"), { uid: 1010, label: "derpy" }, reader("tcp-foreign.txt", null)),
    )
    expect(error.code).toBe("forbidden")
    expect(error.fatal).toBe(true)
    expect(error.message).toBe("port 10000 is held by uid 1008, expected 1010 (derpy)")
  })

  test("any matching row with another uid is refused, even next to the expected one", async () => {
    const error = await refusal(
      assertListenerOwner(url("http://127.0.0.1:10010"), { uid: 1009, label: "tenant-b" }, reader("tcp-foreign.txt", null)),
    )
    expect(error.code).toBe("forbidden")
    expect(error.message).toBe("port 10010 is held by uid 1666, expected 1009 (tenant-b)")
  })

  test("no listener: unreachable (retried), not fatal", async () => {
    const error = await refusal(assertListenerOwner(url("http://127.0.0.1:9999"), { uid: 1000, label: "this user" }, tenants()))
    expect(error.code).toBe("unreachable")
    expect(error.fatal).toBe(false)
    expect(error.message).toBe("nothing listens on 127.0.0.1:9999")
  })

  test("a URL without a port uses the scheme default", async () => {
    const error = await refusal(assertListenerOwner(url("http://127.0.0.1/"), { uid: 1000, label: "this user" }, tenants()))
    expect(error.message).toBe("nothing listens on 127.0.0.1:80")
  })

  test("a missing tcp6 table means no IPv6 listeners", async () => {
    expect(await listenerUids(url("http://127.0.0.1:8080"), reader("tcp-tenants.txt", null))).toEqual([1000])
  })

  test("an unreadable /proc/net/tcp cannot be verified: unsupported and fatal, never a silent pass", async () => {
    const error = await refusal(
      assertListenerOwner(url("http://127.0.0.1:8080"), { uid: 1000, label: "this user" }, reader(null, null)),
    )
    expect(error.code).toBe("unsupported")
    expect(error.fatal).toBe(true)
  })
})

describe("address helpers", () => {
  test("isLoopbackHost", () => {
    for (const host of ["127.0.0.1", "127.1.2.3", "[::1]", "localhost", "LOCALHOST", "[::ffff:7f00:1]"]) {
      expect(isLoopbackHost(host)).toBe(true)
    }
    for (const host of ["10.0.0.1", "[::2]", "example.com", "128.0.0.1", "[fe80::1]", "0.0.0.0"]) {
      expect(isLoopbackHost(host)).toBe(false)
    }
  })

  test("parseIpv6 handles compression, brackets and embedded IPv4", () => {
    expect(Array.from(parseIpv6("[::1]") ?? [])).toEqual([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1])
    expect(Array.from(parseIpv6("::ffff:127.0.0.1") ?? []).slice(10)).toEqual([0xff, 0xff, 127, 0, 0, 1])
    expect(parseIpv6("1::2::3")).toBeNull()
    expect(parseIpv6("12345::")).toBeNull()
  })
})
