/**
 * Child process for test/config/proxy-env.test.ts (design 3.11.4). Run as
 *
 *   bun test/fixtures/proxy-child.ts neutralize|control <url>...
 *
 * with HTTP_PROXY, HTTPS_PROXY and ALL_PROXY pointing at a loopback recording proxy. "neutralize" calls
 * neutralizeProxyEnv() before its first fetch, exactly as main.tsx does; "control" does not, which shows that the
 * proxy would otherwise receive the requests. Prints one JSON line: {"results":[{url, status, body} | {url, error}]}.
 * It runs outside `bun test`, so the test preload (which blanks the proxy variables) does not apply here.
 */
import { neutralizeProxyEnv } from "../../src/config/proxy-env.ts"

type Result = { url: string; status: number; body: string } | { url: string; error: string }

const [mode, ...urls] = process.argv.slice(2)
if (mode === "neutralize") {
  neutralizeProxyEnv()
} else if (mode !== "control") {
  process.stderr.write("usage: proxy-child.ts neutralize|control <url>...\n")
  process.exit(2)
}

const results: Result[] = []
for (const url of urls) {
  try {
    const response = await fetch(url, { signal: AbortSignal.timeout(5_000) })
    results.push({ url, status: response.status, body: await response.text() })
  } catch (error) {
    results.push({ url, error: error instanceof Error ? error.name : "error" })
  }
}
process.stdout.write(`${JSON.stringify({ results })}\n`)
