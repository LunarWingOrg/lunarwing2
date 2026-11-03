/**
 * Proxy environment (design 3.11.4, SEC-14, 11.3.2). Bun's fetch routes even loopback requests through an
 * inherited HTTP_PROXY; neutralizeProxyEnv() must stop that. The proof runs test/fixtures/proxy-child.ts as a
 * real child process with the proxy variables set, against a recording proxy and a mock gateway on loopback
 * ports chosen by the kernel, so a Bun upgrade that changes the behaviour fails here.
 */
import { afterAll, beforeAll, describe, expect, test } from "bun:test"
import { join } from "node:path"
import { neutralizeProxyEnv, PROXY_ENV_NAMES } from "../../src/config/proxy-env.ts"
import { makeTempDir } from "../helpers/tmp.ts"

const TUI_ROOT = join(import.meta.dir, "..", "..")
const CHILD = join(TUI_ROOT, "test", "fixtures", "proxy-child.ts")
const HEALTH = JSON.stringify({ status: "healthy", channel: "gateway" })

type Server = ReturnType<typeof Bun.serve>

interface Peers {
  proxy: Server
  gateway4: Server
  /** Same port as gateway4 on ::1, so localhost reaches the gateway whichever address it resolves to. */
  gateway6: Server | null
  port: number
}

const proxyHits: string[] = []
const gatewayHits: string[] = []
let peers: Peers

function gatewayServer(hostname: string, port: number): Server {
  return Bun.serve({
    hostname,
    port,
    fetch(request) {
      gatewayHits.push(new URL(request.url).pathname)
      return new Response(HEALTH, { headers: { "content-type": "application/json" } })
    },
  })
}

function hasIpv6Loopback(): boolean {
  try {
    Bun.serve({ hostname: "::1", port: 0, fetch: () => new Response("") }).stop(true)
    return true
  } catch {
    return false
  }
}

function startPeers(): Peers {
  const proxy = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch(request) {
      proxyHits.push(request.url)
      return new Response("via-proxy", { status: 502 })
    },
  })
  const ipv6 = hasIpv6Loopback()
  for (let attempt = 0; attempt < 10; attempt += 1) {
    const gateway4 = gatewayServer("127.0.0.1", 0)
    const port = gateway4.port ?? 0
    if (!ipv6) return { proxy, gateway4, gateway6: null, port }
    try {
      return { proxy, gateway4, gateway6: gatewayServer("::1", port), port }
    } catch {
      gateway4.stop(true) // the kernel gave this port to someone else on ::1; try another
    }
  }
  throw new Error("could not bind the mock gateway on 127.0.0.1 and ::1 with one port")
}

function targetUrls(): string[] {
  const urls = [`http://127.0.0.1:${peers.port}/api/health`, `http://localhost:${peers.port}/api/health`]
  if (peers.gateway6 !== null) urls.push(`http://[::1]:${peers.port}/api/health`)
  return urls
}

interface ChildOutput {
  results: Array<{ url: string; status?: number; body?: string; error?: string }>
}

async function runChild(mode: "neutralize" | "control", urls: readonly string[]): Promise<ChildOutput> {
  const proxyUrl = `http://127.0.0.1:${peers.proxy.port}`
  const env: Record<string, string> = { PATH: process.env.PATH ?? "/usr/bin:/bin", HOME: await makeTempDir() }
  for (const name of PROXY_ENV_NAMES) env[name] = proxyUrl
  const child = Bun.spawn([process.execPath, CHILD, mode, ...urls], {
    cwd: TUI_ROOT,
    env,
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
    timeout: 20_000,
  })
  const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited])
  if (code !== 0) throw new Error(`proxy-child exited with ${code}: ${stderr.slice(0, 500)}`)
  return JSON.parse(stdout) as ChildOutput
}

beforeAll(() => {
  peers = startPeers()
})

afterAll(() => {
  peers.proxy.stop(true)
  peers.gateway4.stop(true)
  peers.gateway6?.stop(true)
})

describe("proxy environment (3.11.4)", () => {
  test(
    "with HTTP_PROXY pointing at a recording proxy, the child's requests to the mock gateway on 127.0.0.1, localhost and [::1] never reach the proxy",
    async () => {
      proxyHits.length = 0
      gatewayHits.length = 0
      const urls = targetUrls()
      const output = await runChild("neutralize", urls)
      expect(proxyHits).toEqual([])
      expect(output.results.map((r) => r.body)).toEqual(urls.map(() => HEALTH))
      expect(gatewayHits).toEqual(urls.map(() => "/api/health"))
    },
    30_000,
  )

  test(
    "control: without neutralizeProxyEnv() the same child sends its loopback requests to the proxy",
    async () => {
      proxyHits.length = 0
      gatewayHits.length = 0
      const output = await runChild("control", targetUrls())
      // If Bun ever stops proxying loopback by itself this fails too: the test above would then prove nothing.
      expect(proxyHits.length).toBeGreaterThan(0)
      expect(output.results.some((r) => r.body === "via-proxy")).toBe(true)
    },
    30_000,
  )

  test("neutralizeProxyEnv() assigns empty values instead of deleting, and sets NO_PROXY and no_proxy to *", () => {
    const env: Record<string, string | undefined> = { HTTP_PROXY: "http://proxy.invalid:3128", all_proxy: "socks5://x", PATH: "/bin" }
    neutralizeProxyEnv(env)
    for (const name of PROXY_ENV_NAMES) {
      expect(Object.hasOwn(env, name)).toBe(true)
      expect(env[name]).toBe("")
    }
    expect(env.NO_PROXY).toBe("*")
    expect(env.no_proxy).toBe("*")
    expect(env.PATH).toBe("/bin")
  })
})
